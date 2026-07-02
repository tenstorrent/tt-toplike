// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Boxed-snake loading visualization for the `[i]` Inference Server Monitor.
//! Pure presentation over `ServiceState`: three boxes (compile → load → ready)
//! that fill as a model comes up — a true % when the monitor has a baseline,
//! else live momentum from the per-tick deltas; red when stalled (Alarm).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::time::Instant;

use crate::ui::colors;
use crate::workload::inference_server::{is_alarm, Phase, ServiceState};

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

/// Compact byte size: `0B`, `700M`, `6.2G`. Binary units (KiB base) but a short
/// single-letter suffix to keep the footer narrow.
pub fn fmt_bytes(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if bytes == 0 {
        "0B".into()
    } else if b < K * K {
        format!("{:.0}K", b / K)
    } else if b < K * K * K {
        format!("{:.0}M", b / (K * K))
    } else {
        format!("{:.1}G", b / (K * K * K))
    }
}

/// Per-second rate from a per-tick delta and the tick cadence. Negative → 0.
pub fn fmt_rate_per_sec(delta: i64, cadence_secs: u32) -> String {
    let cadence = cadence_secs.max(1) as i64;
    let per_sec = (delta / cadence).max(0);
    if per_sec >= 1024 * 1024 {
        format!("{}/s", fmt_bytes(per_sec as u64))
    } else {
        format!("{per_sec}/s")
    }
}

/// `M:SS` elapsed, minutes uncapped (`2:14`, `61:01`).
pub fn fmt_elapsed(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Group an integer's digits into comma-separated thousands: `1842 -> "1,842"`.
/// Pure and ASCII-only (digits + commas), so it never byte-slices multibyte text.
fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub struct LoadSnake {
    // Layout for the boxed-snake render (read by `render`).
    width: usize,
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
        // New service → reset the journey. Reset `phase` too: the `Alarm` arm
        // deliberately preserves the working phase, so without this a switch to
        // an already-stalled service would inherit the prior service's phase and
        // redden the wrong box.
        if self.active_key.as_deref() != Some(featured.key.as_str()) {
            self.active_key = Some(featured.key.clone());
            self.compile_fill = 0.0;
            self.load_fill = 0.0;
            self.burst_left = 0;
            self.started = Some(Instant::now());
            self.phase = Phase::Down;
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

    /// Render the boxed-snake loading visualization to owned Ratatui lines:
    /// a title line, three labeled chambers (compile → load → ready) that fill
    /// serpentine as the model comes up, and a footer of live stats. Red when
    /// stalled (Alarm). Returns `vec![]` on a zero-sized view.
    pub fn render(&self) -> Vec<Line<'static>> {
        if self.width == 0 || self.height == 0 {
            return vec![];
        }

        let bg = colors::rgb(0, 0, 0);
        let gold = Color::Rgb(246, 188, 66);
        let dim = colors::rgb(90, 90, 90);
        let alarm = self.is_alarm();

        let mut lines: Vec<Line<'static>> = Vec::new();

        // --- Title: "{label}  {phase_word}  {elapsed}" ---
        // Down/Alarm fall back to the working sense ("compiling"): Task 2 keeps
        // the working phase through an Alarm, so `self.phase` still names the box.
        let phase_word = match self.phase {
            Phase::Loading => "loading",
            Phase::Ready => "ready",
            _ => "compiling",
        };
        let elapsed = self.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let title_full = format!("{}  {}  {}", self.label, phase_word, fmt_elapsed(elapsed));
        // Char-safe truncation — labels may be multibyte, never byte-slice.
        let title: String = title_full.chars().take(self.width).collect();
        let title_color = if alarm {
            colors::ERROR
        } else {
            colors::TEXT_PRIMARY
        };
        lines.push(Line::from(Span::styled(
            title,
            Style::default().fg(title_color).add_modifier(Modifier::BOLD),
        )));

        // --- Chambers canvas (title + footer reserve one row each) ---
        let box_h = self.height.saturating_sub(2);
        if box_h > 0 {
            let mut canvas: Vec<Vec<(char, Color)>> = vec![vec![(' ', bg); self.width]; box_h];

            // Which chamber is active (gets the head + alarm-red tint).
            let active = match self.phase {
                Phase::Loading => 1usize,
                Phase::Ready => 2usize,
                _ => 0usize, // Compiling / Down
            };

            let ready_fill = if self.is_finishing() { 1.0 } else { 0.0 };
            // (label, phase color, fill fraction, is_open)
            let chambers: [(&str, Color, f32, bool); 3] = [
                (
                    "compile",
                    colors::SUCCESS,
                    self.compile_fill,
                    self.compile_fill > 0.0
                        || matches!(self.phase, Phase::Compiling | Phase::Loading | Phase::Ready),
                ),
                (
                    "load",
                    colors::WARNING,
                    self.load_fill,
                    self.load_fill > 0.0 || matches!(self.phase, Phase::Loading | Phase::Ready),
                ),
                ("ready", gold, ready_fill, self.is_finishing()),
            ];

            let seg_w = self.width / 3;
            for (i, (label, color, fill, open)) in chambers.iter().enumerate() {
                let x0 = i * seg_w;
                let x1 = if i == 2 { self.width } else { (i + 1) * seg_w };
                let is_active = i == active;
                // Active box reddens on alarm; closed boxes read dim gray.
                let box_color = if is_active && alarm {
                    colors::ERROR
                } else if *open {
                    *color
                } else {
                    dim
                };
                draw_chamber(
                    &mut canvas, x0, x1, box_h, label, box_color, *fill, *open, is_active,
                );
            }

            for row in canvas {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut current_text = String::new();
                let mut current_color = bg;
                for (ch, color) in row {
                    if color != current_color {
                        if !current_text.is_empty() {
                            spans.push(Span::styled(
                                std::mem::take(&mut current_text),
                                Style::default().fg(current_color),
                            ));
                        }
                        current_color = color;
                    }
                    current_text.push(ch);
                }
                if !current_text.is_empty() {
                    spans.push(Span::styled(
                        current_text,
                        Style::default().fg(current_color),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }

        // --- Footer: determinate shows %, momentum shows signal rates instead ---
        let mut footer = if let Some(p) = self.progress {
            format!(
                "kernels {}  rss {} {}  {:.0}%",
                group_thousands(self.kernel_count),
                fmt_bytes(self.rss_bytes),
                fmt_rate_per_sec(self.rss_delta, self.cadence_secs),
                p * 100.0,
            )
        } else {
            format!(
                "kernels {} ↑{}  rss {} ↑{}",
                group_thousands(self.kernel_count),
                fmt_rate_per_sec(self.kernel_delta, self.cadence_secs),
                fmt_bytes(self.rss_bytes),
                fmt_rate_per_sec(self.rss_delta, self.cadence_secs),
            )
        };
        if alarm {
            footer.push_str("  stalled");
        }
        let footer: String = footer.chars().take(self.width).collect();
        let footer_color = if alarm {
            colors::ERROR
        } else {
            colors::TEXT_SECONDARY
        };
        lines.push(Line::from(Span::styled(
            footer,
            Style::default().fg(footer_color),
        )));

        lines
    }
}

/// Draw one chamber (border + serpentine fill + head) into the canvas region
/// spanning columns `[x0, x1)` and all `box_h` rows. Closed chambers get a
/// dashed border and no fill. All indices are clamped by the caller-supplied
/// bounds so this never panics on a narrow/short view.
#[allow(clippy::too_many_arguments)]
fn draw_chamber(
    canvas: &mut [Vec<(char, Color)>],
    x0: usize,
    x1: usize,
    box_h: usize,
    label: &str,
    color: Color,
    fill: f32,
    open: bool,
    is_active: bool,
) {
    if x1 <= x0 || box_h == 0 {
        return;
    }
    let w = x1 - x0;
    let last_row = box_h - 1;
    let last_col = x1 - 1;
    // Solid border when the chamber has opened; dashed while it's still closed.
    let (hz, vt) = if open { ('─', '│') } else { ('┄', '┆') };

    // Top and bottom borders (corners are always solid so the box reads).
    let corner = |x: usize, l: char, r: char| {
        if x == x0 {
            l
        } else if x == last_col {
            r
        } else {
            hz
        }
    };
    for (x, cell) in canvas[0].iter_mut().enumerate().take(x1).skip(x0) {
        *cell = (corner(x, '┌', '┐'), color);
    }
    for (x, cell) in canvas[last_row].iter_mut().enumerate().take(x1).skip(x0) {
        *cell = (corner(x, '└', '┘'), color);
    }
    // Left/right sides.
    for row in canvas.iter_mut().take(last_row).skip(1) {
        row[x0] = (vt, color);
        row[last_col] = (vt, color);
    }

    // Label along the top border, inside the corners.
    if w > 2 {
        let inner_w = w - 2;
        for (i, ch) in label.chars().take(inner_w).enumerate() {
            canvas[0][x0 + 1 + i] = (ch, color);
        }
    }

    // Interior serpentine fill (only meaningful once the chamber is open and it
    // has an inside). Even rows run left→right, odd rows right→left.
    if open && box_h > 2 && w > 2 {
        let inner_w = w - 2;
        let inner_h = box_h - 2;
        let total = inner_w * inner_h;
        let filled = ((fill.clamp(0.0, 1.0) * total as f32).round() as usize).min(total);
        for idx in 0..filled {
            let r = idx / inner_w;
            let col_in_row = idx % inner_w;
            let cx = if r % 2 == 0 {
                col_in_row
            } else {
                inner_w - 1 - col_in_row
            };
            let y = 1 + r;
            let x = x0 + 1 + cx;
            // Head glyph rides the frontier of the active chamber; body elsewhere.
            let glyph = if is_active && idx + 1 == filled {
                '▶'
            } else {
                '●'
            };
            canvas[y][x] = (glyph, color);
        }
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
    fn switching_to_an_already_alarmed_service_does_not_inherit_prior_phase() {
        let mut s = LoadSnake::new(80, 20);
        let mut a = svc(Phase::Loading);
        a.key = "a".into();
        s.update(&a, 2); // journey A ends at Loading
        // featured_loading jumps to a different, already-stalled service B.
        let mut b = svc(Phase::Alarm);
        b.key = "b".into();
        b.flat_ticks = 200;
        s.update(&b, 2);
        assert_eq!(
            s.active_phase(),
            Phase::Down,
            "new journey resets phase; Alarm arm must not inherit A's Loading"
        );
        assert_eq!(s.compile_fill(), 0.0, "new journey reset the fills");
        assert_eq!(s.load_fill(), 0.0);
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

    #[test]
    fn formatters_are_human_readable() {
        assert_eq!(fmt_bytes(0), "0B");
        assert_eq!(fmt_bytes(6_657_000_000), "6.2G");
        assert_eq!(fmt_bytes(700 * 1024 * 1024), "700M");
        // rate = delta / cadence, per second.
        assert_eq!(fmt_rate_per_sec(0, 2), "0/s");
        assert_eq!(fmt_rate_per_sec(84, 2), "42/s");
        assert_eq!(fmt_elapsed(0), "0:00");
        assert_eq!(fmt_elapsed(134), "2:14");
        assert_eq!(fmt_elapsed(3_661), "61:01");
    }

    #[test]
    fn render_determinate_shows_percent_and_label() {
        let mut s = LoadSnake::new(60, 16);
        let mut l = svc(Phase::Loading);
        l.label = "FLUX".into();
        l.progress = Some(0.47);
        s.update(&l, 2);
        let text: String = s
            .render()
            .iter()
            .flat_map(|ln| ln.spans.iter().map(|sp| sp.content.to_string()))
            .collect();
        assert!(text.contains("FLUX"));
        assert!(text.contains("47%"), "determinate → shows percent");
        assert!(text.contains("load"));
    }

    #[test]
    fn render_momentum_shows_no_percent() {
        let mut s = LoadSnake::new(60, 16);
        let mut c = svc(Phase::Compiling);
        c.kernel_count = 1842;
        c.kernel_delta = 84; // progress None
        s.update(&c, 2);
        let text: String = s
            .render()
            .iter()
            .flat_map(|ln| ln.spans.iter().map(|sp| sp.content.to_string()))
            .collect();
        assert!(!text.contains('%'), "momentum → never shows a percent");
        assert!(
            text.contains("1,842") || text.contains("1842"),
            "shows raw kernel count"
        );
    }

    #[test]
    fn render_zero_dims_does_not_panic() {
        let s = LoadSnake::new(0, 0);
        let _ = s.render(); // must not panic
    }
}
