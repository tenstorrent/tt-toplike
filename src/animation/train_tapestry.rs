// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure helpers behind the Training view's tapestry band.
//!
//! The band replaces a decorative node grid with layers drawn from signals we
//! really read: per-step wall time, per-chip power, hardware gauges, a
//! diagnosis, and loss-convergence numbers. Everything that can be computed
//! without a terminal lives here so each piece can be tested directly; the
//! renderer in `train_view.rs` only places what these functions return.

use crate::workload::train::{StepSample, STEP_HISTORY};
use std::collections::{BTreeMap, VecDeque};

/// Columns reserved at the left of every band row for its label.
pub const LABEL_W: usize = 6;
/// Chip lanes drawn at most (one row each).
pub const MAX_LANES: usize = 3;
/// A chip whose aiclk falls below this fraction of the highest it has shown
/// is marked as throttled in its lane.
pub const AICLK_DROP_FRAC: f32 = 0.90;
/// Tallest the step-time bars grow.
const MAX_BAR_ROWS: usize = 4;

/// Compute-bound: the busiest chip is at least this fraction of its TDP.
const COMPUTE_BOUND_TDP_FRAC: f32 = 0.50;
/// Host-bound: the busiest chip is below this fraction of TDP while the host
/// is busy (see [`HOST_BOUND_CPU_PCT`]).
const HOST_BOUND_TDP_FRAC: f32 = 0.30;
/// 100 is one saturated core, so this is "more than one core busy".
const HOST_BOUND_CPU_PCT: f32 = 100.0;
/// Loss slope (per 100 logged losses) smaller than this counts as flat.
const SLOPE_EPS: f32 = 0.002;
/// A pass of the grid cursor never runs faster than this, because a cursor
/// that crosses the band several times between frames is not visible.
const MIN_PASS_SECS: f32 = 0.2;

/// Median of `values`, `None` when empty.
pub fn median(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) / 2.0
    })
}

/// The glyph for one cell of a vertical bar `rows` cells tall, at eighth-cell
/// resolution. `row_from_bottom` is 0 for the bottom cell. Any positive
/// `frac` shows at least a sliver, so a small value is never invisible.
pub fn bar_cell(frac: f32, row_from_bottom: usize, rows: usize) -> char {
    const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    // `!(x > 0.0)` is also true for NaN, which draws nothing.
    if rows == 0 || frac.is_nan() || frac <= 0.0 {
        return ' ';
    }
    let total = ((frac.min(1.0) * rows as f32 * 8.0).round() as usize).max(1);
    let filled = total.saturating_sub(row_from_bottom * 8).min(8);
    if filled == 0 {
        ' '
    } else {
        GLYPHS[filled - 1]
    }
}

/// Why a step looks the way it does, as far as the log can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepCause {
    Normal,
    /// The program cache grew on this step.
    Compile,
    /// A checkpoint was written while this was the latest step.
    Checkpoint,
}

/// A compile outranks a checkpoint: growth in the program cache is measured
/// from the step line itself, while the checkpoint flag only means a save was
/// noticed while this was the newest step.
pub fn cause_of(s: &StepSample) -> StepCause {
    if s.cache_delta > 0 {
        StepCause::Compile
    } else if s.checkpoint {
        StepCause::Checkpoint
    } else {
        StepCause::Normal
    }
}

/// Least-squares slope of the last 100 logged losses, scaled to "per 100
/// logged losses". `None` under 8 samples. The unit is logged losses, not
/// steps: a trainer that prints every tenth step has ten steps per entry.
pub fn loss_slope_per_100(losses: &[f32]) -> Option<f32> {
    let w = &losses[losses.len().saturating_sub(100)..];
    if w.len() < 8 {
        return None;
    }
    let n = w.len() as f32;
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = w.iter().sum::<f32>() / n;
    let (mut cov, mut var) = (0.0f32, 0.0f32);
    for (i, y) in w.iter().enumerate() {
        let dx = i as f32 - mean_x;
        cov += dx * (y - mean_y);
        var += dx * dx;
    }
    Some(cov / var * 100.0)
}

/// Standard deviation of the step-to-step loss changes over the last 50
/// logged losses. A steady descent has near-zero noise even though the loss
/// moves, because every change is the same size. `None` under 9 samples.
pub fn loss_noise(losses: &[f32]) -> Option<f32> {
    let w = &losses[losses.len().saturating_sub(50)..];
    if w.len() < 9 {
        return None;
    }
    let d: Vec<f32> = w.windows(2).map(|p| p[1] - p[0]).collect();
    let mean = d.iter().sum::<f32>() / d.len() as f32;
    let var = d.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / d.len() as f32;
    Some(var.sqrt())
}

/// Logged losses since the lowest one (the latest, on ties). `Some(0)` means
/// the newest loss is the best so far. `None` under 2 samples.
pub fn logs_since_best(losses: &[f32]) -> Option<usize> {
    if losses.len() < 2 {
        return None;
    }
    let (mut best_i, mut best) = (0usize, f32::INFINITY);
    for (i, &l) in losses.iter().enumerate() {
        if l <= best {
            best = l;
            best_i = i;
        }
    }
    Some(losses.len() - 1 - best_i)
}

/// The convergence strip's pieces, in display order. Each is present only when
/// its input exists, so a trainer with no learning rate simply has no `lr`
/// piece. `lr` is the configured base rate. The schedule position is how far
/// through the run's step budget we are, which is not the live learning rate.
pub fn convergence_parts(
    losses: &[f32],
    lr: Option<f32>,
    scheduler: Option<&str>,
    step: u64,
    max_steps: u64,
) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(s) = loss_slope_per_100(losses) {
        let arrow = if s < -SLOPE_EPS {
            '↘'
        } else if s > SLOPE_EPS {
            '↗'
        } else {
            '→'
        };
        parts.push(format!("loss {arrow} {s:+.3}/100 logs"));
    }
    if let Some(n) = loss_noise(losses) {
        parts.push(format!("noise {n:.3}"));
    }
    match logs_since_best(losses) {
        Some(0) => parts.push("at best loss".to_string()),
        Some(n) => parts.push(format!("best {n} logs ago")),
        None => {}
    }
    if let Some(lr) = lr {
        parts.push(format!("base lr {lr:.1e}"));
    }
    match (scheduler, max_steps) {
        (Some(name), m) if m > 0 => parts.push(format!(
            "{name} {}% through",
            (step.saturating_mul(100) / m).min(100)
        )),
        (Some(name), _) => parts.push(name.to_string()),
        _ => {}
    }
    parts
}

/// The live readings the diagnosis looks at.
#[derive(Debug, Clone, Copy, Default)]
pub struct Readings {
    /// The newest step grew the program cache.
    pub compiled_last_step: bool,
    /// Power over TDP for the busiest chip. `None` when no chip reports a TDP.
    pub busiest_tdp_frac: Option<f32>,
    pub host_cpu_pct: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosisKind {
    Compiling,
    ComputeBound,
    HostBound,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnosis {
    pub kind: DiagnosisKind,
    /// Names the readings that triggered it, so the reason can be checked.
    pub text: String,
    /// `text` without the trailing `, host cpu N%` clause, for a band too
    /// narrow for the full line. Equal to `text` when there is no such clause
    /// to drop (a compile verdict has none, and a host-bound verdict is
    /// decided by the cpu reading so it is shown whole or not at all).
    pub short: String,
}

/// A one-line verdict from plain thresholds, or `None` when the readings are
/// inconclusive. A missing TDP gives no power-based verdict rather than a
/// guess from an assumed ceiling.
pub fn diagnose(r: &Readings) -> Option<Diagnosis> {
    if r.compiled_last_step {
        return Some(Diagnosis {
            kind: DiagnosisKind::Compiling,
            text: "compiling - the program cache grew on the latest step".to_string(),
            short: "compiling - the program cache grew on the latest step".to_string(),
        });
    }
    let frac = r.busiest_tdp_frac?;
    if frac >= COMPUTE_BOUND_TDP_FRAC {
        let cpu = r
            .host_cpu_pct
            .map(|c| format!(", host cpu {c:.0}%"))
            .unwrap_or_default();
        return Some(Diagnosis {
            kind: DiagnosisKind::ComputeBound,
            text: format!(
                "compute-bound - busiest chip at {:.0}% of TDP{cpu}",
                frac * 100.0
            ),
            short: format!(
                "compute-bound - busiest chip at {:.0}% of TDP",
                frac * 100.0
            ),
        });
    }
    let cpu = r.host_cpu_pct?;
    if frac < HOST_BOUND_TDP_FRAC && cpu > HOST_BOUND_CPU_PCT {
        let text = format!(
            "host-bound - busiest chip at {:.0}% of TDP, host cpu {cpu:.0}%",
            frac * 100.0
        );
        return Some(Diagnosis {
            kind: DiagnosisKind::HostBound,
            short: text.clone(),
            text,
        });
    }
    None
}

/// What the band would like to draw, before it is fitted to the height.
#[derive(Debug, Clone, Copy, Default)]
pub struct BandWants {
    pub lanes: usize,
    pub gauges: usize,
    pub verdict: bool,
    pub strip: bool,
}

/// Rows granted to each layer. Top to bottom the band draws: header, bars,
/// grid, lanes, gauges, verdict, strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandPlan {
    pub bar_rows: usize,
    pub grid_row: bool,
    pub lane_rows: usize,
    pub gauge_rows: usize,
    pub verdict_row: bool,
    pub strip_row: bool,
}

/// Fit the layers into a band `height` rows tall, header included. Rows are
/// handed out in priority order: one bar row, verdict, chip lanes, gauges,
/// strip, grid, then extra bar height. So a short terminal keeps the step
/// chart and loses the convergence strip first. The total never exceeds
/// `height - 1`.
pub fn plan_band(height: usize, w: BandWants) -> BandPlan {
    let mut rem = height.saturating_sub(1);
    let mut take = |want: usize| -> usize {
        let n = want.min(rem);
        rem -= n;
        n
    };
    let bar_min = take(1);
    let verdict_row = w.verdict && take(1) == 1;
    let lane_rows = take(w.lanes);
    let gauge_rows = take(w.gauges.div_ceil(2).min(2));
    let strip_row = w.strip && take(1) == 1;
    let grid_row = take(1) == 1;
    let bar_rows = bar_min + take(MAX_BAR_ROWS - 1);
    BandPlan {
        bar_rows,
        grid_row,
        lane_rows,
        gauge_rows,
        verdict_row,
        strip_row,
    }
}

/// Seconds for one pass of the grid cursor, and whether that was raised to the
/// visible minimum. The cursor makes one pass per measured step. `None` until
/// a step time is known.
pub fn pass_secs(step_ms: f32) -> Option<(f32, bool)> {
    if step_ms.is_nan() || step_ms <= 0.0 {
        return None;
    }
    let secs = step_ms / 1000.0;
    if secs < MIN_PASS_SECS {
        Some((MIN_PASS_SECS, true))
    } else {
        Some((secs, false))
    }
}

/// Position within the current pass, in `[0, 1)`.
pub fn pass_fraction(frame: u64, fps: f32, pass_secs: f32) -> f32 {
    if pass_secs.is_nan() || pass_secs <= 0.0 || fps.is_nan() || fps <= 0.0 {
        return 0.0;
    }
    ((frame as f32 / fps) / pass_secs).fract()
}

/// Advance a pulse phase in `[0, 1)` by `frames` frames of a pass lasting
/// `pass_secs`. Accumulating (rather than recomputing from absolute time)
/// keeps the cursor where it is when the pass length changes: only its speed
/// changes. An unusable `pass_secs` or `fps` leaves the phase alone.
pub fn advance_phase(phase: f32, frames: u64, fps: f32, pass_secs: f32) -> f32 {
    if pass_secs.is_nan() || pass_secs <= 0.0 || fps.is_nan() || fps <= 0.0 {
        return phase;
    }
    (phase + (frames as f32 / fps) / pass_secs).fract()
}

/// One chip's reading, taken when the view first saw a new step sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipSample {
    /// The run's sample sequence number (`TrainState::step_seq`) this
    /// reading belongs to, matched against `StepSample::seq`. A step number
    /// can repeat after a progress bar restarts, so it is not used as the key.
    pub seq: u64,
    pub power_w: f32,
    pub aiclk_mhz: u32,
}

/// Chip readings keyed by the run's sample sequence number, plus best-so-far
/// values for the gauges. A reading is taken when the view first sees each
/// sequence number, so it is close to the moment the step was recorded, but
/// not exactly at it.
#[derive(Debug, Default)]
pub struct ChipHistory {
    last_seq: Option<u64>,
    rings: BTreeMap<usize, VecDeque<ChipSample>>,
    aiclk_max: BTreeMap<usize, u32>,
    best_tps: f32,
    best_pcie_bps: f64,
}

impl ChipHistory {
    /// Record `(device index, power W, aiclk MHz)` for every chip at sample
    /// sequence number `seq` (`TrainState::step_seq`). The sequence number
    /// rises by one per recorded step sample and does not go down when a
    /// progress bar restarts. A repeat of the same number is ignored. A lower
    /// number can only come from a new run, so everything is cleared and old
    /// samples never appear as the new run's lanes. (The view also clears
    /// the history when the run's identity changes, which covers a new run
    /// whose number does not go down.)
    ///
    /// Call `record` before `note_tps`/`note_pcie` each frame, because a
    /// reset here clears the bests too. `record(seq, &[])` still marks `seq`
    /// as seen, so a later call with the same number and some chips is
    /// ignored.
    pub fn record(&mut self, seq: u64, chips: &[(usize, f32, u32)]) {
        if let Some(prev) = self.last_seq {
            if seq < prev {
                *self = Self::default();
            } else if seq == prev {
                return;
            }
        }
        for &(idx, power_w, aiclk_mhz) in chips {
            let ring = self.rings.entry(idx).or_default();
            ring.push_back(ChipSample {
                seq,
                power_w,
                aiclk_mhz,
            });
            while ring.len() > STEP_HISTORY {
                ring.pop_front();
            }
            let m = self.aiclk_max.entry(idx).or_insert(0);
            *m = (*m).max(aiclk_mhz);
        }
        self.last_seq = Some(seq);
    }

    /// The reading for chip `idx` at sample sequence number `seq`, if the
    /// view saw that number while the chip reported telemetry.
    pub fn sample_at(&self, idx: usize, seq: u64) -> Option<ChipSample> {
        self.rings
            .get(&idx)?
            .iter()
            .rev()
            .find(|s| s.seq == seq)
            .copied()
    }

    /// Highest aiclk this chip has shown since the run (or tool) started.
    pub fn aiclk_max(&self, idx: usize) -> u32 {
        self.aiclk_max.get(&idx).copied().unwrap_or(0)
    }

    /// Fold in a tokens/sec reading and return the best so far.
    pub fn note_tps(&mut self, tps: f32) -> f32 {
        if tps.is_finite() && tps > self.best_tps {
            self.best_tps = tps;
        }
        self.best_tps
    }

    pub fn best_tps(&self) -> f32 {
        self.best_tps
    }

    /// Fold in a summed PCIe bytes/sec reading and return the best so far.
    pub fn note_pcie(&mut self, bps: f64) -> f64 {
        if bps.is_finite() && bps > self.best_pcie_bps {
            self.best_pcie_bps = bps;
        }
        self.best_pcie_bps
    }

    pub fn best_pcie_bps(&self) -> f64 {
        self.best_pcie_bps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(step: u64, delta: u32, ckpt: bool) -> StepSample {
        StepSample {
            step,
            seq: step,
            ms: 400.0,
            cache_delta: delta,
            checkpoint: ckpt,
        }
    }

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }

    #[test]
    fn bar_cell_resolves_eighths_across_rows() {
        assert_eq!(bar_cell(1.0, 0, 1), '█');
        assert_eq!(bar_cell(0.5, 0, 1), '▄');
        assert_eq!(bar_cell(0.0, 0, 1), ' ');
        // Two rows, 0.75 full = 12 eighths: a full bottom cell, half a top one.
        assert_eq!(bar_cell(0.75, 0, 2), '█');
        assert_eq!(bar_cell(0.75, 1, 2), '▄');
        // A tiny positive value is still visible.
        assert_eq!(bar_cell(0.001, 0, 2), '▁');
        assert_eq!(bar_cell(f32::NAN, 0, 2), ' ');
        assert_eq!(bar_cell(1.0, 0, 0), ' ');
    }

    #[test]
    fn a_compile_outranks_a_checkpoint_which_outranks_normal() {
        assert_eq!(cause_of(&sample(1, 3, true)), StepCause::Compile);
        assert_eq!(cause_of(&sample(1, 0, true)), StepCause::Checkpoint);
        assert_eq!(cause_of(&sample(1, 0, false)), StepCause::Normal);
    }

    #[test]
    fn slope_follows_the_direction_and_rate_of_the_loss() {
        let down: Vec<f32> = (0..100).map(|i| 5.0 - 0.01 * i as f32).collect();
        assert!((loss_slope_per_100(&down).unwrap() + 1.0).abs() < 1e-3);
        let up: Vec<f32> = (0..100).map(|i| 1.0 + 0.01 * i as f32).collect();
        assert!(loss_slope_per_100(&up).unwrap() > 0.9);
        assert!(loss_slope_per_100(&[2.0; 100]).unwrap().abs() < 1e-4);
        assert_eq!(loss_slope_per_100(&[1.0; 7]), None);
    }

    #[test]
    fn noise_is_zero_for_a_steady_descent_and_high_for_a_jagged_one() {
        let steady: Vec<f32> = (0..30).map(|i| 5.0 - 0.01 * i as f32).collect();
        assert!(loss_noise(&steady).unwrap() < 1e-3);
        let jagged: Vec<f32> = (0..30)
            .map(|i| 2.0 + if i % 2 == 0 { 0.1 } else { -0.1 })
            .collect();
        assert!(loss_noise(&jagged).unwrap() > 0.1);
        assert_eq!(loss_noise(&[1.0; 8]), None);
    }

    #[test]
    fn logs_since_best_counts_back_from_the_latest_minimum() {
        assert_eq!(logs_since_best(&[3.0, 2.0, 2.5, 2.6]), Some(2));
        assert_eq!(logs_since_best(&[3.0, 2.0, 1.0]), Some(0));
        // Ties take the latest, so a flat run reads as "at best".
        assert_eq!(logs_since_best(&[1.0, 1.0, 1.0]), Some(0));
        assert_eq!(logs_since_best(&[1.0]), None);
    }

    #[test]
    fn the_strip_has_a_piece_per_available_input_and_none_otherwise() {
        let down: Vec<f32> = (0..120).map(|i| 5.0 - 0.01 * i as f32).collect();
        let parts = convergence_parts(&down, Some(3.0e-4), Some("cosine"), 41, 100);
        let joined = parts.join(" | ");
        assert!(joined.contains('↘'), "{joined}");
        assert!(joined.contains("noise"), "{joined}");
        assert!(joined.contains("at best loss"), "{joined}");
        assert!(joined.contains("base lr 3.0e-4"), "{joined}");
        assert!(joined.contains("cosine 41% through"), "{joined}");
        // No losses, no lr, no scheduler: nothing to show.
        assert!(convergence_parts(&[], None, None, 0, 0).is_empty());
        // A scheduler with no step budget names itself without a percentage.
        let p = convergence_parts(&[], None, Some("linear"), 5, 0);
        assert_eq!(p, vec!["linear".to_string()]);
    }

    #[test]
    fn the_diagnosis_uses_the_documented_thresholds() {
        let r = |compiled, frac, cpu| Readings {
            compiled_last_step: compiled,
            busiest_tdp_frac: frac,
            host_cpu_pct: cpu,
        };
        assert_eq!(
            diagnose(&r(true, Some(0.9), Some(400.0))).unwrap().kind,
            DiagnosisKind::Compiling
        );
        let d = diagnose(&r(false, Some(0.58), Some(140.0))).unwrap();
        assert_eq!(d.kind, DiagnosisKind::ComputeBound);
        assert!(
            d.text.contains("58%") && d.text.contains("140%"),
            "{}",
            d.text
        );
        let d = diagnose(&r(false, Some(0.2), Some(380.0))).unwrap();
        assert_eq!(d.kind, DiagnosisKind::HostBound);
        assert!(
            d.text.contains("20%") && d.text.contains("380%"),
            "{}",
            d.text
        );
        // Between the two thresholds, or idle with a quiet host: no verdict.
        assert_eq!(diagnose(&r(false, Some(0.4), Some(500.0))), None);
        assert_eq!(diagnose(&r(false, Some(0.2), Some(50.0))), None);
        // No TDP known: never guess from an assumed ceiling.
        assert_eq!(diagnose(&r(false, None, Some(500.0))), None);
        // Compute-bound does not need the host reading.
        assert_eq!(
            diagnose(&r(false, Some(0.7), None)).unwrap().kind,
            DiagnosisKind::ComputeBound
        );
    }

    #[test]
    fn the_band_never_hands_out_more_rows_than_it_has() {
        for height in 0..30usize {
            for lanes in 0..=4usize {
                for gauges in 0..=5usize {
                    for verdict in [false, true] {
                        for strip in [false, true] {
                            let p = plan_band(
                                height,
                                BandWants {
                                    lanes,
                                    gauges,
                                    verdict,
                                    strip,
                                },
                            );
                            let used = p.bar_rows
                                + usize::from(p.grid_row)
                                + p.lane_rows
                                + p.gauge_rows
                                + usize::from(p.verdict_row)
                                + usize::from(p.strip_row);
                            assert!(
                                used <= height.saturating_sub(1),
                                "h={height} lanes={lanes} gauges={gauges}: used {used}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_full_height_band_gives_every_layer_its_rows() {
        let all = BandWants {
            lanes: 3,
            gauges: 4,
            verdict: true,
            strip: true,
        };
        assert_eq!(
            plan_band(13, all),
            BandPlan {
                bar_rows: 4,
                grid_row: true,
                lane_rows: 3,
                gauge_rows: 2,
                verdict_row: true,
                strip_row: true,
            }
        );
    }

    #[test]
    fn short_bands_drop_layers_in_the_documented_order() {
        let all = BandWants {
            lanes: 3,
            gauges: 4,
            verdict: true,
            strip: true,
        };
        // Header plus one bar row is the floor.
        let p = plan_band(2, all);
        assert_eq!((p.bar_rows, p.lane_rows, p.gauge_rows), (1, 0, 0));
        assert!(!p.verdict_row && !p.strip_row && !p.grid_row);
        // The verdict is the first thing added, then lanes, then gauges.
        let p = plan_band(3, all);
        assert!(p.verdict_row && p.lane_rows == 0);
        let p = plan_band(4, all);
        assert!(p.verdict_row && p.lane_rows == 1 && p.gauge_rows == 0);
        // The strip and grid come last, before extra bar height.
        let p = plan_band(9, all);
        assert!(p.strip_row && !p.grid_row && p.bar_rows == 1);
        let p = plan_band(10, all);
        assert!(p.strip_row && p.grid_row && p.bar_rows == 1);
    }

    #[test]
    fn the_cursor_makes_one_pass_per_step_but_never_faster_than_visible() {
        assert_eq!(pass_secs(0.0), None);
        assert_eq!(pass_secs(f32::NAN), None);
        assert_eq!(pass_secs(412.0), Some((0.412, false)));
        assert_eq!(pass_secs(83.0), Some((MIN_PASS_SECS, true)));
        assert_eq!(pass_fraction(0, 60.0, 1.0), 0.0);
        assert!((pass_fraction(30, 60.0, 1.0) - 0.5).abs() < 1e-6);
        assert!(pass_fraction(61, 60.0, 1.0) < 0.05, "wraps after a pass");
        assert_eq!(pass_fraction(5, 60.0, 0.0), 0.0);
    }

    #[test]
    fn chip_samples_are_found_by_sequence_number_and_a_repeat_is_ignored() {
        let mut h = ChipHistory::default();
        h.record(1, &[(0, 50.0, 1000), (1, 60.0, 1000)]);
        h.record(2, &[(0, 55.0, 1000), (1, 65.0, 1000)]);
        h.record(2, &[(0, 99.0, 1000), (1, 99.0, 1000)]);
        assert_eq!(h.sample_at(0, 2).unwrap().power_w, 55.0);
        assert_eq!(h.sample_at(1, 1).unwrap().power_w, 60.0);
        assert_eq!(h.sample_at(0, 3), None);
        assert_eq!(h.sample_at(7, 1), None);
    }

    /// A lower sample sequence number can only come from a new run, so it
    /// clears the old samples, the aiclk maximum and the bests.
    #[test]
    fn a_lower_sequence_number_clears_old_samples_and_bests() {
        let mut h = ChipHistory::default();
        h.record(500, &[(0, 80.0, 1200)]);
        h.note_tps(9000.0);
        h.note_pcie(5e9);
        h.record(1, &[(0, 20.0, 800)]);
        assert_eq!(h.sample_at(0, 500), None);
        assert_eq!(h.sample_at(0, 1).unwrap().power_w, 20.0);
        assert_eq!(h.aiclk_max(0), 800);
        assert_eq!(h.best_tps(), 0.0);
        assert_eq!(h.best_pcie_bps(), 0.0);
    }

    #[test]
    fn chip_history_is_bounded_and_tracks_the_highest_aiclk() {
        let mut h = ChipHistory::default();
        for step in 1..=200u64 {
            h.record(step, &[(0, 50.0, 900 + (step % 3) as u32 * 100)]);
        }
        assert_eq!(h.sample_at(0, 200).unwrap().seq, 200);
        assert_eq!(h.sample_at(0, 100), None, "old samples are dropped");
        assert_eq!(h.aiclk_max(0), 1100);
    }

    #[test]
    fn best_so_far_values_only_rise() {
        let mut h = ChipHistory::default();
        assert_eq!(h.note_tps(100.0), 100.0);
        assert_eq!(h.note_tps(50.0), 100.0);
        assert_eq!(h.note_tps(f32::NAN), 100.0);
        assert_eq!(h.note_pcie(2e9), 2e9);
        assert_eq!(h.note_pcie(1e9), 2e9);
    }

    #[test]
    fn advance_phase_accumulates_wraps_and_ignores_bad_input() {
        // 30 frames at 60 fps of a 1 s pass is half a pass.
        assert!((advance_phase(0.0, 30, 60.0, 1.0) - 0.5).abs() < 1e-6);
        // Wraps past 1.
        assert!((advance_phase(0.75, 30, 60.0, 1.0) - 0.25).abs() < 1e-6);
        // Changing the pass length mid-way keeps the position.
        let p = advance_phase(0.0, 15, 60.0, 1.0);
        assert!((advance_phase(p, 0, 60.0, 0.3) - p).abs() < 1e-6);
        // Unusable inputs leave the phase alone.
        assert_eq!(advance_phase(0.4, 10, 60.0, 0.0), 0.4);
        assert_eq!(advance_phase(0.4, 10, 0.0, 1.0), 0.4);
    }
}
