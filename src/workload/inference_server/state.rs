// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

use crate::workload::inference_server::metrics::{MediaStats, ServingStats};
use crate::workload::inference_server::probe::Readiness;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Down,
    Compiling,
    Loading,
    Ready,
    Alarm,
}

/// Per-model baselines for the optional % estimate (compile kernels / weight
/// footprint). No model→profile table is wired yet, so the monitor always
/// passes `Default` (all `None`) and the `%` estimate stays dormant — the
/// mechanism is here for when per-model baselines are measured and populated.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelProfile {
    pub o_baseline: Option<usize>,
    pub footprint_bytes: Option<u64>,
}

/// Derive phase from the tick's deltas + readiness. `Alarm` is layered on top by
/// the reducer (needs the flat-tick history), so this returns the momentary phase.
impl Phase {
    pub fn derive(
        kernel_delta: i64,
        python_alive: bool,
        rss_delta: i64,
        loaded_delta: i64,
        readiness: &Readiness,
    ) -> Phase {
        if let Readiness::Ready { .. } = readiness {
            return Phase::Ready;
        }
        if let Readiness::Down = readiness {
            if !python_alive {
                return Phase::Down;
            }
        }
        if kernel_delta > 0 {
            return Phase::Compiling;
        }
        // Weight-load progress shows as either host RSS growth or new
        // `.tensorbin` shards being written (device-loaded weights may not grow
        // host RSS), so either climbing means Loading.
        if rss_delta > 0 || loaded_delta > 0 {
            return Phase::Loading;
        }
        // not ready, nothing moving, but process alive → provisional Loading;
        // the reducer escalates to Alarm after enough flat ticks.
        if python_alive {
            Phase::Loading
        } else {
            Phase::Down
        }
    }
}

/// True when nothing has progressed for >= 5 minutes and the service isn't Ready.
pub fn is_alarm(flat_ticks: u32, cadence_secs: u32, readiness: &Readiness) -> bool {
    !matches!(readiness, Readiness::Ready { .. }) && flat_ticks.saturating_mul(cadence_secs) >= 300
}

/// Best-effort % (compile: kernel_count/baseline; load: rss/footprint). None w/o profile.
pub fn estimate_progress(
    phase: Phase,
    kernel_count: usize,
    rss: u64,
    profile: &ModelProfile,
) -> Option<f32> {
    match phase {
        Phase::Compiling => profile
            .o_baseline
            .filter(|b| *b > 0)
            .map(|b| (kernel_count as f32 / b as f32).clamp(0.0, 1.0)),
        Phase::Loading => profile
            .footprint_bytes
            .filter(|b| *b > 0)
            .map(|b| (rss as f32 / b as f32).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// Full per-service state published to the render path.
#[derive(Debug, Clone)]
pub struct ServiceState {
    pub key: String,
    pub label: String,
    pub phase: Phase,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub rss_delta: i64,
    pub kernel_count: usize,
    pub kernel_delta: i64,
    /// Loaded weight shards (`.tensorbin`) and per-tick change — the weight-load
    /// counterpart to `kernel_count`/`kernel_delta`'s compile signal.
    pub loaded_count: usize,
    pub loaded_delta: i64,
    pub safetensors_fds: usize,
    pub readiness: Readiness,
    pub top_proc: Option<String>,
    pub last_log: Option<String>,
    pub progress: Option<f32>,
    pub flat_ticks: u32,
    /// Folded vLLM `/metrics` telemetry (rates/gauges), `None` when the
    /// service doesn't serve a `/metrics` endpoint or it hasn't parsed yet.
    pub serving: Option<ServingStats>,
    /// Folded tt-media-inference-server `/metrics` telemetry (diffusion/video
    /// models like SkyReels), `None` for vLLM/non-media servers. Mutually
    /// exclusive with `serving` — a server exposes one namespace or the other.
    pub media: Option<MediaStats>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::probe::Readiness;
    #[test]
    fn phase_derivation() {
        assert_eq!(Phase::derive(0, false, 0, 0, &Readiness::Down), Phase::Down);
        assert_eq!(
            Phase::derive(12, true, 0, 0, &Readiness::NotReady),
            Phase::Compiling
        ); // kernels growing
        assert_eq!(
            Phase::derive(0, true, 700_000_000, 0, &Readiness::NotReady),
            Phase::Loading
        ); // RSS growing
           // Weight shards being written (`.tensorbin`) also reads as Loading, even
           // when host RSS is flat (weights land on the device, not host memory).
        assert_eq!(
            Phase::derive(0, true, 0, 40, &Readiness::NotReady),
            Phase::Loading
        );
        assert_eq!(
            Phase::derive(0, true, 0, 0, &Readiness::Ready { runner: None }),
            Phase::Ready
        );
    }
    #[test]
    fn all_flat_not_ready_is_alarm_after_5min() {
        // 5s cadence → 60 flat ticks = 5 min.
        assert!(is_alarm(60, 5, &Readiness::NotReady));
        assert!(!is_alarm(59, 5, &Readiness::NotReady));
        assert!(!is_alarm(120, 5, &Readiness::Ready { runner: None }));
    }
    #[test]
    fn progress_uses_profile_when_present() {
        let prof = ModelProfile {
            o_baseline: Some(1000),
            footprint_bytes: None,
        };
        assert_eq!(
            estimate_progress(Phase::Compiling, 500, 0, &prof),
            Some(0.5)
        );
        let none = ModelProfile {
            o_baseline: None,
            footprint_bytes: None,
        };
        assert_eq!(estimate_progress(Phase::Compiling, 500, 0, &none), None);
    }
}
