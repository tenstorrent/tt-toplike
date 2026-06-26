// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Headless render-cost benchmark types + pure helpers.
//!
//! The harness that actually renders each screen lives in `super`
//! (`run_render_bench`); this module holds the result type and the pure math so
//! it can be unit-tested without a backend or a terminal.

/// One screen's measured render cost.
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub mode: &'static str,
    pub frames: usize,
    pub avg_ms: f64,
    pub p95_ms: f64,
    pub target_fps: u32,
    pub est_cpu_pct: f64,
}

/// Estimated CPU as a percent of one core: ms-per-frame * frames-per-second,
/// expressed as a percentage (ms*fps / 1000 * 100 == ms*fps/10).
pub fn est_cpu_pct(avg_ms: f64, target_fps: u32) -> f64 {
    avg_ms * target_fps as f64 / 10.0
}

/// Linear-interpolation-free percentile: nearest-rank on a sorted copy.
pub fn percentile_ms(samples: &mut [f64], pct: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((pct / 100.0) * (samples.len() as f64 - 1.0)).round() as usize;
    samples[rank.min(samples.len() - 1)]
}

/// Render a fixed-width table of results for stdout.
pub fn format_table(results: &[BenchResult]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<16}{:>8}{:>14}{:>10}{:>12}{:>18}\n",
        "mode", "frames", "avg ms/frame", "p95 ms", "target fps", "est cpu (1 core)"
    ));
    for r in results {
        out.push_str(&format!(
            "{:<16}{:>8}{:>14.3}{:>10.3}{:>12}{:>17.1}%\n",
            r.mode, r.frames, r.avg_ms, r.p95_ms, r.target_fps, r.est_cpu_pct
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn est_cpu_is_avg_times_fps_over_ten() {
        // 1.0 ms/frame at 60 fps = 60 ms of work per 1000 ms = 6% of one core.
        assert!((est_cpu_pct(1.0, 60) - 6.0).abs() < 1e-9);
        // 0.4 ms at 10 fps = 0.4% .
        assert!((est_cpu_pct(0.4, 10) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn percentile_picks_high_sample() {
        let mut s = vec![1.0, 2.0, 3.0, 4.0, 100.0];
        let p95 = percentile_ms(&mut s, 95.0);
        assert!(p95 >= 4.0, "p95 should be near the top: {p95}");
    }

    #[test]
    fn table_has_a_row_per_result() {
        let rows = vec![
            BenchResult { mode: "insights", frames: 10, avg_ms: 0.4, p95_ms: 0.6, target_fps: 10, est_cpu_pct: 0.4 },
            BenchResult { mode: "arcade", frames: 10, avg_ms: 3.6, p95_ms: 5.0, target_fps: 60, est_cpu_pct: 21.6 },
        ];
        let t = format_table(&rows);
        assert!(t.contains("insights"));
        assert!(t.contains("arcade"));
        assert!(t.lines().count() >= 3); // header + 2 rows
    }
}
