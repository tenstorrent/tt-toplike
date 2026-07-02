// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Boxed-snake loading visualization for the `[i]` Inference Server Monitor.
//! Pure presentation over `ServiceState`: three boxes (compile → load → ready)
//! that fill as a model comes up — a true % when the monitor has a baseline,
//! else live momentum from the per-tick deltas; red when stalled (Alarm).

use crate::workload::inference_server::{is_alarm, Phase, ServiceState};
use std::time::Instant;

/// Momentum "briskness" references: a per-tick delta at/above this reads as a
/// strong surge. Compile counts `.o` kernels; load counts RSS bytes.
const COMPILE_REF: i64 = 50; // kernels/tick
const LOAD_REF: i64 = 200 * 1024 * 1024; // 200 MiB/tick
const BASE_STEP: f32 = 0.02; // momentum head advance per active tick (before scaling)
const FILL_CAP: f32 = 0.95; // momentum never claims a full box
/// Ready-burst duration in update frames (cosmetic gold celebration).
const BURST_FRAMES: u32 = 24;

/// Momentum increment for the active box given this tick's `delta` and the
/// phase's briskness `reference`. Zero for a non-positive delta; monotonically
/// larger for bigger bursts, saturating at 3×BASE_STEP.
pub fn momentum_step(delta: i64, reference: i64) -> f32 {
    if delta <= 0 || reference <= 0 {
        return 0.0;
    }
    let ratio = (delta as f32 / reference as f32).min(2.0); // 0..2
    BASE_STEP * (1.0 + ratio) // BASE_STEP .. 3*BASE_STEP
}

pub struct LoadSnake {
    // Layout for the boxed-snake render; consumed by Task 3's `render`, unread until then.
    #[allow(dead_code)]
    width: usize,
    #[allow(dead_code)]
    height: usize,
    frame: u64,
    /// The service whose journey we're rendering; `None` until first update.
    active_key: Option<String>,
    phase: Phase,
    compile_fill: f32,
    load_fill: f32,
    alarm: bool,
    burst_left: u32,
    started: Option<Instant>,
    // Footer stats snapshot (read by render, Task 3).
    pub(crate) kernel_count: usize,
    pub(crate) kernel_delta: i64,
    pub(crate) rss_bytes: u64,
    pub(crate) rss_delta: i64,
    pub(crate) progress: Option<f32>,
    pub(crate) cadence_secs: u32,
    pub(crate) label: String,
}

impl LoadSnake {
    pub fn new(width: usize, height: usize) -> Self {
        LoadSnake {
            width,
            height,
            frame: 0,
            active_key: None,
            phase: Phase::Down,
            compile_fill: 0.0,
            load_fill: 0.0,
            alarm: false,
            burst_left: 0,
            started: None,
            kernel_count: 0,
            kernel_delta: 0,
            rss_bytes: 0,
            rss_delta: 0,
            progress: None,
            cadence_secs: 2,
            label: String::new(),
        }
    }

    /// Advance the journey for the featured (Compiling/Loading) service.
    pub fn update(&mut self, featured: &ServiceState, cadence_secs: u32) {
        self.frame = self.frame.wrapping_add(1);
        // New service → reset the journey.
        if self.active_key.as_deref() != Some(featured.key.as_str()) {
            self.active_key = Some(featured.key.clone());
            self.compile_fill = 0.0;
            self.load_fill = 0.0;
            self.burst_left = 0;
            self.started = Some(Instant::now());
        }
        self.cadence_secs = cadence_secs.max(1);
        self.label = featured.label.clone();
        self.kernel_count = featured.kernel_count;
        self.kernel_delta = featured.kernel_delta;
        self.rss_bytes = featured.rss_bytes;
        self.rss_delta = featured.rss_delta;
        self.progress = featured.progress;
        self.alarm = featured.phase == Phase::Alarm
            || is_alarm(featured.flat_ticks, self.cadence_secs, &featured.readiness);

        match featured.phase {
            Phase::Compiling => {
                self.phase = Phase::Compiling;
                self.compile_fill = self
                    .advance(
                        self.compile_fill,
                        featured.progress,
                        featured.kernel_delta,
                        COMPILE_REF,
                    )
                    .max(self.compile_fill);
            }
            Phase::Loading => {
                self.phase = Phase::Loading;
                // Entering load locks the compile box full.
                self.compile_fill = 1.0;
                self.load_fill = self
                    .advance(
                        self.load_fill,
                        featured.progress,
                        featured.rss_delta,
                        LOAD_REF,
                    )
                    .max(self.load_fill);
            }
            Phase::Ready => {
                self.phase = Phase::Ready;
            }
            Phase::Down => {
                self.phase = Phase::Down;
            }
            Phase::Alarm => {
                // Keep the last working phase (Compiling/Loading) and freeze
                // both fills — the frozen snake + `self.alarm`'s red tint is
                // the stall indicator, not further fill movement.
            }
        }
    }

    /// Determinate (progress present, clamped) else momentum advance of `fill`.
    fn advance(&self, fill: f32, progress: Option<f32>, delta: i64, reference: i64) -> f32 {
        match progress {
            Some(p) => p.clamp(0.0, 1.0),
            None => {
                let step = momentum_step(delta, reference);
                (fill + step * (1.0 - fill)).min(FILL_CAP)
            }
        }
    }

    /// After the featured service leaves the loading phases, play a brief gold
    /// burst if it reached Ready. Returns true while the burst is in progress.
    /// Clears the journey when the burst ends or the service vanished.
    pub fn finish_if_ready(&mut self, rows: &[ServiceState]) -> bool {
        let Some(key) = self.active_key.clone() else {
            return false;
        };
        let cur = rows.iter().find(|s| s.key == key);
        match cur.map(|s| s.phase) {
            Some(Phase::Ready) => {
                self.frame = self.frame.wrapping_add(1);
                if self.burst_left == 0 && self.phase != Phase::Ready {
                    // First frame of the burst: lock both boxes full.
                    self.burst_left = BURST_FRAMES;
                    self.compile_fill = 1.0;
                    self.load_fill = 1.0;
                    self.phase = Phase::Ready;
                }
                if self.burst_left > 0 {
                    self.burst_left -= 1;
                    return true;
                }
                self.active_key = None; // burst done
                false
            }
            // Still compiling/loading (someone else is featured), or vanished/Down.
            _ => {
                self.active_key = None;
                false
            }
        }
    }

    pub fn active_phase(&self) -> Phase {
        self.phase
    }
    pub fn compile_fill(&self) -> f32 {
        self.compile_fill
    }
    pub fn load_fill(&self) -> f32 {
        self.load_fill
    }
    pub fn is_alarm(&self) -> bool {
        self.alarm
    }
    pub fn is_finishing(&self) -> bool {
        self.burst_left > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};

    fn svc(phase: Phase) -> ServiceState {
        ServiceState {
            key: "flux".into(),
            label: "FLUX".into(),
            phase,
            cpu_pct: 0.0,
            rss_bytes: 0,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::NotReady,
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
        }
    }

    #[test]
    fn compiling_fills_only_the_compile_box() {
        let mut s = LoadSnake::new(80, 20);
        let mut c = svc(Phase::Compiling);
        c.progress = Some(0.5);
        s.update(&c, 2);
        assert_eq!(s.active_phase(), Phase::Compiling);
        assert!(
            (s.compile_fill() - 0.5).abs() < 1e-3,
            "determinate compile fill = progress"
        );
        assert_eq!(s.load_fill(), 0.0, "load box untouched while compiling");
    }

    #[test]
    fn loading_locks_compile_full_and_fills_load() {
        let mut s = LoadSnake::new(80, 20);
        let mut l = svc(Phase::Loading);
        l.progress = Some(0.3);
        s.update(&l, 2);
        assert_eq!(
            s.compile_fill(),
            1.0,
            "entering load locks compile box full"
        );
        assert!((s.load_fill() - 0.3).abs() < 1e-3);
    }

    #[test]
    fn momentum_advances_with_delta_and_never_completes() {
        // No baseline (progress None) → momentum mode.
        let mut s = LoadSnake::new(80, 20);
        let mut c = svc(Phase::Compiling);
        c.kernel_delta = 50;
        s.update(&c, 2);
        let after_one = s.compile_fill();
        assert!(after_one > 0.0, "positive delta advances the head");
        // Zero delta holds position.
        c.kernel_delta = 0;
        s.update(&c, 2);
        assert_eq!(s.compile_fill(), after_one, "flat delta holds");
        // Momentum never reaches full on its own.
        for _ in 0..1000 {
            c.kernel_delta = 1000;
            s.update(&c, 2);
        }
        assert!(s.compile_fill() < 1.0, "momentum caps below full");
    }

    #[test]
    fn bigger_burst_is_a_bigger_step() {
        assert!(momentum_step(200, 50) > momentum_step(50, 50));
        assert_eq!(momentum_step(0, 50), 0.0);
        assert_eq!(momentum_step(-5, 50), 0.0);
    }

    #[test]
    fn new_featured_key_resets_the_journey() {
        let mut s = LoadSnake::new(80, 20);
        let mut a = svc(Phase::Loading);
        a.key = "a".into();
        a.progress = Some(0.8);
        s.update(&a, 2);
        assert!(s.load_fill() > 0.0);
        let mut b = svc(Phase::Compiling);
        b.key = "b".into();
        b.progress = Some(0.1);
        s.update(&b, 2);
        assert_eq!(s.load_fill(), 0.0, "switching service resets fills");
        assert_eq!(s.active_phase(), Phase::Compiling);
    }

    #[test]
    fn loading_then_compiling_bounce_does_not_deflate_compile_box() {
        let mut s = LoadSnake::new(80, 20);
        let mut l = svc(Phase::Loading);
        l.progress = Some(0.2);
        s.update(&l, 2);
        assert_eq!(s.compile_fill(), 1.0);
        // JIT recompile mid-load bounces phase back to Compiling.
        let mut c = svc(Phase::Compiling);
        c.progress = Some(0.4);
        s.update(&c, 2);
        assert_eq!(
            s.compile_fill(),
            1.0,
            "compile box must not deflate once locked"
        );
    }

    #[test]
    fn alarm_keeps_working_phase_and_freezes_fills() {
        let mut s = LoadSnake::new(80, 20);
        let mut l = svc(Phase::Loading);
        l.progress = Some(0.3);
        s.update(&l, 2);
        let load_before = s.load_fill();
        let mut a = svc(Phase::Alarm);
        a.flat_ticks = 200;
        a.progress = Some(0.9);
        s.update(&a, 2);
        assert!(s.is_alarm(), "alarm phase sets the alarm flag");
        assert_eq!(
            s.active_phase(),
            Phase::Loading,
            "keeps last working phase so renderer knows the active box"
        );
        assert_eq!(s.load_fill(), load_before, "fills freeze during alarm");
    }

    #[test]
    fn alarm_when_flat_ticks_exceed_five_minutes() {
        let mut s = LoadSnake::new(80, 20);
        let mut c = svc(Phase::Loading);
        c.flat_ticks = 200; // 200 * 2s = 400s >= 300s
        s.update(&c, 2);
        assert!(s.is_alarm());
        c.flat_ticks = 1;
        s.update(&c, 2);
        assert!(!s.is_alarm());
    }

    #[test]
    fn finish_if_ready_bursts_then_ends() {
        let mut s = LoadSnake::new(80, 20);
        s.update(&svc(Phase::Loading), 2); // active journey on key "flux"
        let mut ready = svc(Phase::Ready);
        // Bursts for a bounded number of frames, then ends.
        assert!(s.finish_if_ready(std::slice::from_ref(&ready)));
        assert!(s.is_finishing());
        let mut frames = 1;
        while s.finish_if_ready(std::slice::from_ref(&ready)) {
            frames += 1;
            assert!(frames < 1000);
        }
        assert!(!s.is_finishing());
        // With no active journey, returns false.
        assert!(!s.finish_if_ready(std::slice::from_ref(&ready)));
    }

    #[test]
    fn finish_if_ready_false_when_service_vanished() {
        let mut s = LoadSnake::new(80, 20);
        s.update(&svc(Phase::Loading), 2);
        assert!(
            !s.finish_if_ready(&[]),
            "vanished service → no burst, journey cleared"
        );
    }
}
