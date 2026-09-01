// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Synthetic inference services for `--mock`.
//!
//! Companion to `workload::train::mock`, for the same reason: `--mock`
//! fabricates chip telemetry, but this view probes real Docker containers and
//! real `/metrics` endpoints, so on a mock backend it only ever showed the
//! cold "no server" state. That left the snake's three other behaviours —
//! compiling, loading, serving — impossible to see without standing up an
//! actual model server.
//!
//! Everything is a closed-form function of elapsed time, so a screenshot at
//! `t` seconds is reproducible and a test can inspect any point in the
//! lifecycle without sleeping to reach it.
//!
//! **Every service is labelled `(mock)`.** Synthetic serving numbers that
//! read as real ones would undermine the guarantee the rest of this tool
//! makes about its own display.

use super::metrics::{ServingStats, VllmCounters};
use super::probe::Readiness;
use super::state::{Phase, ServiceState};

/// Seconds for one full pass through the lifecycle before it loops.
const CYCLE_SECS: f32 = 150.0;

/// Marker appended to every synthetic service's label.
pub const MOCK_SUFFIX: &str = " (mock)";

/// Whether a label was produced by this module.
pub fn is_mock_label(label: &str) -> bool {
    label.ends_with(MOCK_SUFFIX)
}

fn serving_stats(t: f32) -> ServingStats {
    // A plausible decode cadence that breathes rather than sitting flat.
    let wave = (t * 0.7).sin() * 0.5 + 0.5;
    ServingStats {
        generation_tps: 380.0 + wave * 260.0,
        prompt_tps: 1_900.0 + (t * 0.31).sin() * 700.0,
        completed_delta: 2 + ((t * 0.5) as u32 % 4),
        errored_delta: 0,
        requests_running: 1 + ((t * 0.4) as u32 % 5),
        requests_waiting: (t * 0.23) as u32 % 3,
        kv_cache_usage: (0.18 + wave * 0.55).clamp(0.0, 1.0),
        ttft_avg_s: 0.09 + wave * 0.06,
        queue_avg_s: 0.01 + wave * 0.03,
        prefill_avg_s: 0.05 + wave * 0.04,
        decode_avg_s: 0.011 + wave * 0.004,
        tpot_avg_s: 0.012 + wave * 0.005,
        prefix_hit_rate: 0.34 + wave * 0.2,
        preemptions_delta: 0,
        counters: VllmCounters::default(),
    }
}

/// One synthetic service, `phase_offset` seconds into the shared cycle — so
/// several services can sit in different phases at the same instant.
fn service_at(key: &str, model: &str, t: f32, phase_offset: f32) -> ServiceState {
    let c = (t + phase_offset).rem_euclid(CYCLE_SECS);

    // Cold → compiling → loading → ready, then hold in ready for the rest of
    // the cycle, which is roughly how a real cold start unfolds.
    let (phase, readiness, progress) = if c < 10.0 {
        (Phase::Down, Readiness::Down, None)
    } else if c < 45.0 {
        (
            Phase::Compiling,
            Readiness::NotReady,
            Some((c - 10.0) / 35.0),
        )
    } else if c < 80.0 {
        (Phase::Loading, Readiness::NotReady, Some((c - 45.0) / 35.0))
    } else {
        (
            Phase::Ready,
            Readiness::Ready {
                runner: Some(model.to_string()),
            },
            None,
        )
    };

    let kernels = match phase {
        Phase::Compiling => ((c - 10.0) * 34.0) as usize,
        Phase::Down => 0,
        _ => 1_190,
    };
    let loaded = match phase {
        Phase::Loading => ((c - 45.0) * 8.0) as usize,
        Phase::Ready | Phase::Alarm => 291,
        _ => 0,
    };
    let rss = match phase {
        Phase::Down => 0,
        Phase::Compiling => 2_800_000_000 + (c as u64 - 10) * 40_000_000,
        Phase::Loading => 4_200_000_000 + (c as u64 - 45) * 900_000_000,
        _ => 37_400_000_000,
    };

    ServiceState {
        key: key.to_string(),
        // The suffix is what keeps synthetic serving numbers from reading as
        // real ones; `is_mock_label` is how the UI finds it.
        label: format!("{model}{MOCK_SUFFIX}"),
        phase,
        cpu_pct: match phase {
            Phase::Down => 0.0,
            Phase::Compiling => 780.0,
            Phase::Loading => 240.0,
            _ => 120.0 + (t * 0.6).sin() * 60.0,
        },
        rss_bytes: rss,
        rss_delta: if phase == Phase::Loading {
            900_000_000
        } else {
            0
        },
        kernel_count: kernels,
        kernel_delta: if phase == Phase::Compiling { 34 } else { 0 },
        loaded_count: loaded,
        loaded_delta: if phase == Phase::Loading { 8 } else { 0 },
        safetensors_fds: if phase == Phase::Loading { 2 } else { 0 },
        readiness,
        top_proc: match phase {
            Phase::Down => None,
            Phase::Compiling => Some("g++".into()),
            _ => Some("python3".into()),
        },
        last_log: Some(
            match phase {
                Phase::Down => "waiting for container",
                Phase::Compiling => "compiling TTNN kernels",
                Phase::Loading => "loading weight shards",
                _ => "Avg generation throughput steady",
            }
            .to_string(),
        ),
        progress,
        flat_ticks: 0,
        serving: (phase == Phase::Ready).then(|| serving_stats(t)),
        media: None,
    }
}

/// The synthetic roster at `elapsed` seconds.
///
/// Two services, deliberately offset so the view usually has one serving and
/// one mid-bringup — which is the interesting case, and the one a single
/// service can never show.
pub fn mock_services(elapsed: f32) -> Vec<ServiceState> {
    vec![
        service_at("mock-llama", "Llama-3.3-70B", elapsed, 95.0),
        service_at("mock-qwen", "Qwen3-32B", elapsed, 0.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of mocking this view is seeing the states a cold start
    /// passes through, so every one of them has to be reachable.
    #[test]
    fn the_cycle_reaches_every_phase() {
        // A Vec rather than a set: `Phase` is deliberately not `Hash`, and
        // widening a production derive to suit a test is the wrong trade.
        let mut seen: Vec<Phase> = Vec::new();
        for i in 0..600 {
            let t = CYCLE_SECS * i as f32 / 600.0;
            for s in mock_services(t) {
                if !seen.contains(&s.phase) {
                    seen.push(s.phase);
                }
            }
        }
        for want in [Phase::Down, Phase::Compiling, Phase::Loading, Phase::Ready] {
            assert!(seen.contains(&want), "{want:?} is never reached");
        }
    }

    /// Synthetic serving numbers that look real would undermine the one
    /// guarantee this tool makes about its display.
    #[test]
    fn every_mock_service_is_labelled_as_mock() {
        for s in mock_services(100.0) {
            assert!(
                is_mock_label(&s.label),
                "service {:?} is not marked as mock",
                s.label
            );
        }
        // ...and a real label is not, or the marker means nothing.
        assert!(!is_mock_label("Llama-3.3-70B"));
    }

    /// Serving stats belong to a serving model. Publishing them in any other
    /// phase would draw a throughput readout for a server still compiling.
    #[test]
    fn serving_stats_appear_only_while_ready() {
        for i in 0..400 {
            let t = CYCLE_SECS * i as f32 / 400.0;
            for s in mock_services(t) {
                assert_eq!(
                    s.serving.is_some(),
                    s.phase == Phase::Ready,
                    "phase {:?} must not carry serving stats",
                    s.phase
                );
            }
        }
    }

    /// The offset exists so the roster shows two different phases at once —
    /// the case a single service can't produce.
    #[test]
    fn the_two_services_are_usually_in_different_phases() {
        let differing = (0..200)
            .filter(|i| {
                let t = CYCLE_SECS * *i as f32 / 200.0;
                let r = mock_services(t);
                r[0].phase != r[1].phase
            })
            .count();
        assert!(
            differing > 100,
            "expected the two services to differ most of the time, got {differing}/200"
        );
    }

    #[test]
    fn the_same_instant_always_yields_the_same_roster() {
        let a = mock_services(42.0);
        let b = mock_services(42.0);
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].phase, b[0].phase);
        assert_eq!(a[0].kernel_count, b[0].kernel_count);
    }
}
