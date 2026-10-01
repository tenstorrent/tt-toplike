# Training Tapestry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Training View's decorative 6x6 node-grid scanner with a band built from real signals: per-step timing, per-chip power, hardware gauges with a verdict, and loss-convergence instruments.

**Architecture:** `TrainState` gains a 64-entry per-step history. A new pure module (`src/animation/train_tapestry.rs`) holds every calculation (medians, loss slope, verdict, row planning, chip sample ring) so each can be tested without rendering. `TrainView::draw_tapestry` replaces `draw_network` and only draws what those helpers return.

**Tech Stack:** Rust, ratatui (character-grid `Cell` buffer in `train_view.rs`), existing `TelemetryBackend` trait.

**Spec:** `docs/superpowers/specs/2026-10-01-training-tapestry-design.md`

Commit steps run only after the user has agreed to commits when choosing the execution method. The checkout's git user is `github-actions[bot]` and the branch is `dazzle-me/tt-smi-reset`, which is unrelated to this work. Start from a new branch or worktree.

## Global Constraints

- Every cell is drawn from a real signal. A missing signal leaves its layer out. Nothing is drawn from a placeholder value.
- No right-side border glyphs (`╗`, `╝`). `║` may appear only at column 0. Every rendered line fits the terminal width.
- Default branch is `main`. Never commit to `master`.
- Comments are thorough and honest, matching the surrounding code. Update any comment the change makes false.
- UI strings use a plain hyphen, never an em dash.
- Brand palette values: teal `#74C5DF` = `Rgb(116,197,223)`, yellow `#F6BC42` = `Rgb(246,188,66)`, red `#FF9E8A` = `Rgb(255,158,138)`, green `#6FABA0` = `Rgb(111,171,160)`. The compile colour is the existing LIVE cache colour `Rgb(180,140,230)`.
- Tests assert on wiring, not just arithmetic. Each new test is run against a deliberately broken implementation to see it fail, then the fix is restored.
- Do not edit `MockBackend`. Its telemetry feeds the Insights sidebar and that sidebar's fit tests.
- Never `import ttnn` or `ttml`, and never open `/dev/tenstorrent/*`. This work needs no hardware, so no gozer lease.
- After `cargo build --release`, copy both binaries: `cp target/release/tt-toplike{,-tui} ~/.local/bin/`.

## Review Focus

1. A trainer that reports no per-step times (cadence is derived from log timing): the band shows `no per-step times reported`, keeps the pulse and gauges, and does not panic.
2. A backend with zero devices or no telemetry: no chip lanes, no power/aiclk/pcie gauges, no panic.
3. Attaching mid-run to a trainer whose program cache is already full: the first step sample must not show as a compile bar.
4. Very small and very large terminals (width 20 to 134, height 10 to 40): no panic, no right border, lines fit the width, side panels keep their narrow-width rules.
5. A new run restarting the step counter at 1: old chip samples must not show up as the new run's lanes, and the run's best-so-far values reset.

## File Structure

| File | Responsibility |
|---|---|
| `src/workload/train/monitor.rs` | `StepSample`, `STEP_HISTORY`, `TrainState.step_history`, `TrainState::mark_checkpoint` |
| `src/workload/train/mock.rs` | Deterministic step history for `--mock` |
| `src/workload/train/mod.rs` | Re-export `StepSample`, `STEP_HISTORY` |
| `src/animation/train_tapestry.rs` (new) | Pure helpers: median, bar glyphs, step cause, loss analytics, diagnosis, band row plan, pass cursor, `ChipHistory` |
| `src/animation/mod.rs` | Register the new module |
| `src/animation/train_view.rs` | `sample`, `draw_tapestry`, `draw_gauge`; remove `draw_network`, `BWD_HUE`, `sweep_head`; legend and test updates |
| `Cargo.toml`, `debian/changelog`, `README.md`, `AGENTS.md`, spec | Version, docs, spec amendments |

---

### Task 1: Per-step history in `TrainState`

**Files:**
- Modify: `src/workload/train/monitor.rs` (constants near line 21, `TrainState` struct near line 65, `apply_event` `StepTime` arm near line 86, `poll` checkpoint block near line 802, new tests appended at end of file)
- Modify: `src/workload/train/mod.rs` (re-export line)
- Modify: `src/workload/train/mock.rs` (`state_at`, new tests)

**Interfaces:**
- Consumes: `TrainEvent::StepTime`, existing `CKPT_PULSE_TICKS`.
- Produces:
  - `pub const STEP_HISTORY: usize = 64`
  - `pub struct StepSample { pub step: u64, pub ms: f32, pub cache_delta: u32, pub checkpoint: bool }` (`Debug, Clone, Copy, PartialEq`)
  - `TrainState.step_history: Vec<StepSample>`
  - `TrainState::mark_checkpoint(&mut self)`

- [ ] **Step 1: Write the failing tests**

Append to the end of `src/workload/train/monitor.rs`:

```rust
#[cfg(test)]
mod step_history_tests {
    use super::*;

    fn timed(step: u64, ms: f32, cache: u32) -> TrainEvent {
        TrainEvent::StepAndTime {
            step,
            loss: 2.0,
            ms,
            cache_entries: cache,
        }
    }

    #[test]
    fn step_time_events_build_a_per_step_history() {
        let mut st = TrainState::new();
        st.apply_event(timed(1, 410.0, 8));
        st.apply_event(timed(2, 395.0, 8));
        st.apply_event(timed(3, 402.0, 8));
        let got: Vec<(u64, f32)> = st.step_history.iter().map(|s| (s.step, s.ms)).collect();
        assert_eq!(got, vec![(1, 410.0), (2, 395.0), (3, 402.0)]);
    }

    /// Review Focus 3. Attaching mid-run to a trainer whose program cache is
    /// already full must not paint the first sample as a compile: there is no
    /// earlier sample to measure growth against.
    #[test]
    fn the_first_sample_has_no_cache_baseline_so_it_is_never_a_compile() {
        let mut st = TrainState::new();
        st.apply_event(timed(900, 400.0, 64));
        st.apply_event(timed(901, 400.0, 70));
        st.apply_event(timed(902, 400.0, 70));
        let deltas: Vec<u32> = st.step_history.iter().map(|s| s.cache_delta).collect();
        assert_eq!(deltas, vec![0, 6, 0]);
    }

    #[test]
    fn the_history_is_bounded() {
        let mut st = TrainState::new();
        for i in 1..=200u64 {
            st.apply_event(timed(i, 400.0, 8));
        }
        assert_eq!(st.step_history.len(), STEP_HISTORY);
        assert_eq!(st.step_history.last().unwrap().step, 200);
        assert_eq!(st.step_history.first().unwrap().step, 200 - STEP_HISTORY as u64 + 1);
    }

    #[test]
    fn a_second_time_line_for_the_same_step_replaces_rather_than_duplicates() {
        let mut st = TrainState::new();
        st.apply_event(timed(5, 400.0, 8));
        st.apply_event(TrainEvent::StepTime {
            ms: 450.0,
            cache_entries: 8,
        });
        assert_eq!(st.step_history.len(), 1);
        assert_eq!(st.step_history[0].ms, 450.0);
    }

    /// Review Focus 1. A trainer that prints only loss (bar-only harnesses,
    /// or cadence derived from log timing) has no per-step times. The history
    /// must stay empty rather than be filled from the derived average.
    #[test]
    fn loss_only_events_leave_the_history_empty() {
        let mut st = TrainState::new();
        for i in 1..=10u64 {
            st.apply_event(TrainEvent::Step { step: i, loss: 2.0 });
        }
        assert!(st.step_history.is_empty());
    }

    #[test]
    fn a_checkpoint_flags_the_latest_sample_and_starts_the_pulse() {
        let mut st = TrainState::new();
        st.apply_event(timed(7, 400.0, 8));
        st.apply_event(timed(8, 400.0, 8));
        st.mark_checkpoint();
        assert!(!st.step_history[0].checkpoint);
        assert!(st.step_history[1].checkpoint);
        assert_eq!(st.checkpoint_step, 8);
        assert_eq!(st.checkpoint_pulse, CKPT_PULSE_TICKS);
    }

    #[test]
    fn a_checkpoint_with_no_history_still_pulses() {
        let mut st = TrainState::new();
        st.mark_checkpoint();
        assert_eq!(st.checkpoint_pulse, CKPT_PULSE_TICKS);
    }
}
```

- [ ] **Step 2: Run to confirm they fail**

Run: `cargo test --lib step_history_tests 2>&1 | tail -20`
Expected: compile errors (`STEP_HISTORY`, `step_history`, `mark_checkpoint` not found).

- [ ] **Step 3: Implement**

In `monitor.rs`, after `pub const LOSS_HISTORY: usize = 512;` add:

```rust
/// Per-step samples retained for the Training view's step-anatomy bars. One
/// bar per column, so this is also the widest the bar chart can ever be.
pub const STEP_HISTORY: usize = 64;

/// One step as the trainer itself reported it.
///
/// Only trainer-reported step times become samples. A trainer that prints no
/// time has its cadence derived from log timing (`note_step_progress`), which
/// is an average over however many steps one poll happened to read. Storing
/// that as a per-step bar would invent resolution, so such runs have no
/// history at all and the view says so.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepSample {
    pub step: u64,
    /// Wall time the trainer printed for this step, in milliseconds.
    pub ms: f32,
    /// How much the program cache grew on this step. Growth means kernels were
    /// compiled, which is the usual reason one step is far slower than the rest.
    pub cache_delta: u32,
    /// A checkpoint was written while this was the latest step.
    pub checkpoint: bool,
}
```

Add to `TrainState` (after `loss_history`):

```rust
    /// The last [`STEP_HISTORY`] trainer-reported step times, oldest first.
    /// Empty for trainers that print no per-step time.
    pub step_history: Vec<StepSample>,
```

Replace the `TrainEvent::StepTime { ms, cache_entries } => { ... }` arm in `apply_event` with:

```rust
            TrainEvent::StepTime { ms, cache_entries } => {
                // Growth is measured against the previous sample, so the first
                // sample has no baseline. Without this guard, attaching to a
                // run whose cache is already full would paint its first bar
                // as a compile.
                let cache_delta = if self.step_history.is_empty() {
                    0
                } else {
                    cache_entries.saturating_sub(self.cache_entries)
                };
                self.step_ms = ms;
                self.cache_entries = cache_entries;
                let sample = StepSample {
                    step: self.step,
                    ms,
                    cache_delta,
                    checkpoint: false,
                };
                match self.step_history.last_mut() {
                    // A second time line for the same step replaces the first
                    // and keeps what the first already established.
                    Some(last) if last.step == sample.step => {
                        *last = StepSample {
                            cache_delta: last.cache_delta.max(sample.cache_delta),
                            checkpoint: last.checkpoint,
                            ..sample
                        };
                    }
                    _ => {
                        self.step_history.push(sample);
                        if self.step_history.len() > STEP_HISTORY {
                            self.step_history.remove(0);
                        }
                    }
                }
            }
```

Add this method to `impl TrainState` (after `eta_secs`):

```rust
    /// A checkpoint was just written: start the pulse, remember the step, and
    /// flag the latest step bar so the chart can mark it.
    pub fn mark_checkpoint(&mut self) {
        self.checkpoint_pulse = CKPT_PULSE_TICKS;
        self.checkpoint_step = self.step;
        if let Some(last) = self.step_history.last_mut() {
            last.checkpoint = true;
        }
    }
```

In `TrainMonitor::poll`, replace

```rust
            if w.poll() {
                self.state.checkpoint_pulse = CKPT_PULSE_TICKS;
                self.state.checkpoint_step = self.state.step;
            }
```

with

```rust
            if w.poll() {
                self.state.mark_checkpoint();
            }
```

In `src/workload/train/mod.rs` change the monitor re-export to:

```rust
pub use monitor::{
    CheckpointWatch, StepSample, Tailer, TrainMonitor, TrainState, LOSS_HISTORY, STEP_HISTORY,
};
```

- [ ] **Step 4: Run to confirm they pass**

Run: `cargo test --lib step_history_tests 2>&1 | tail -15`
Expected: 7 passed.

- [ ] **Step 5: Add the mock history, test first**

Append inside `mod tests` of `src/workload/train/mock.rs`:

```rust
    /// `--mock` has to carry every signal the step-anatomy chart draws:
    /// compiles while the cache fills, a checkpoint, and per-step variation.
    #[test]
    fn a_mock_run_has_a_step_history_with_compiles_and_a_checkpoint() {
        // 31 s is about step 371 (f32 rounding makes it 371, not 372): the
        // 64-step window holds the cache still filling (growth stops at step
        // 336) and the step-360 save.
        let st = MockTrainRun::new().state_at(31.0);
        assert_eq!(st.step_history.len(), crate::workload::train::STEP_HISTORY);
        assert_eq!(st.step_history.last().unwrap().step, st.step);
        assert!(st.step_history.iter().any(|s| s.cache_delta > 0));
        assert!(st.step_history.iter().any(|s| s.checkpoint));
        let ms: Vec<f32> = st.step_history.iter().map(|s| s.ms).collect();
        let (lo, hi) = ms
            .iter()
            .fold((f32::MAX, 0.0f32), |(l, h), m| (l.min(*m), h.max(*m)));
        assert!(hi > lo * 1.3, "bars need visible variation: {lo}..{hi}");
    }
```

Run: `cargo test --lib a_mock_run_has_a_step_history 2>&1 | tail -8`
Expected: FAIL (`step_history` is empty, length assertion).

Implement. In `mock.rs`, after `loss_at` add:

```rust
/// Program-cache entries after `step` steps: fills during the opening steps,
/// then holds. Shared by the cache counter and the per-step compile marks so
/// the two can never disagree.
fn cache_at(step: u64) -> u32 {
    64.min(8 + step as u32 / 6)
}

/// Wall time of one simulated step, in milliseconds. A smooth wobble gives the
/// bars texture, and a step that grew the cache is slower, as a compile is.
fn step_ms_at(step: u64) -> f32 {
    let base = STEP_SECS * 1000.0;
    let wobble = 1.0 + 0.12 * (step as f32 * 0.9).sin();
    let compiled = step > 1 && cache_at(step) > cache_at(step - 1);
    base * wobble * if compiled { 1.8 } else { 1.0 }
}
```

Change `st.cache_entries = 64.min(8 + step as u32 / 6);` to `st.cache_entries = cache_at(step);`, and add the import `use super::monitor::{StepSample, TrainState, LOSS_HISTORY, STEP_HISTORY};` (replacing the existing `use super::monitor::{TrainState, LOSS_HISTORY};`). After `st.prev_loss = ...;` add:

```rust
        // Per-step times for the step-anatomy chart, derived from the same
        // closed-form functions as everything else so a screenshot at t
        // seconds is reproducible.
        let first_step = step.saturating_sub(STEP_HISTORY as u64 - 1).max(1);
        st.step_history = (first_step..=step)
            .map(|s| StepSample {
                step: s,
                ms: step_ms_at(s),
                cache_delta: if s > 1 { cache_at(s) - cache_at(s - 1) } else { 0 },
                checkpoint: s >= SAVE_EVERY && s % SAVE_EVERY == 0,
            })
            .collect();
```

- [ ] **Step 6: Run the whole train suite**

Run: `cargo test --lib workload::train 2>&1 | tail -15`
Expected: all pass, including the new mock test.

- [ ] **Step 7: Make the new tests prove themselves**

Temporarily change `if self.step_history.is_empty() { 0 }` to `if false { 0 }` in `apply_event`. Run `cargo test --lib the_first_sample_has_no_cache_baseline`. Expected: FAIL. Restore it. Temporarily delete the `last.checkpoint = true;` line in `mark_checkpoint`. Run `cargo test --lib a_checkpoint_flags_the_latest_sample`. Expected: FAIL. Restore it. Re-run `cargo test --lib workload::train` and confirm green.

- [ ] **Step 8: Commit**

```bash
git add src/workload/train/
git commit -m "feat: keep a per-step history in TrainState (step time, cache growth, checkpoint)"
```

---

### Task 2: The pure tapestry module

**Files:**
- Create: `src/animation/train_tapestry.rs`
- Modify: `src/animation/mod.rs:35` (add `pub mod train_tapestry;` after `pub mod train_sky;`)

**Interfaces:**
- Consumes: `crate::workload::train::{StepSample, STEP_HISTORY}`.
- Produces (all `pub` in `crate::animation::train_tapestry`):
  - consts `LABEL_W: usize = 6`, `MAX_LANES: usize = 3`, `AICLK_DROP_FRAC: f32 = 0.90`
  - `median(&[f32]) -> Option<f32>`
  - `bar_cell(frac: f32, row_from_bottom: usize, rows: usize) -> char`
  - `enum StepCause { Normal, Compile, Checkpoint }`, `cause_of(&StepSample) -> StepCause`
  - `loss_slope_per_100(&[f32]) -> Option<f32>`, `loss_noise(&[f32]) -> Option<f32>`, `logs_since_best(&[f32]) -> Option<usize>`
  - `convergence_parts(losses: &[f32], lr: Option<f32>, scheduler: Option<&str>, step: u64, max_steps: u64) -> Vec<String>`
  - `struct Readings { compiled_last_step: bool, busiest_tdp_frac: Option<f32>, host_cpu_pct: Option<f32> }`
  - `enum DiagnosisKind { Compiling, ComputeBound, HostBound }`, `struct Diagnosis { kind, text }`, `diagnose(&Readings) -> Option<Diagnosis>`
  - `struct BandWants { lanes, gauges, verdict, strip }`, `struct BandPlan { bar_rows, grid_row, lane_rows, gauge_rows, verdict_row, strip_row }`, `plan_band(height: usize, BandWants) -> BandPlan`
  - `pass_secs(step_ms: f32) -> Option<(f32, bool)>`, `pass_fraction(frame: u64, fps: f32, pass_secs: f32) -> f32`
  - `struct ChipSample { step, power_w, aiclk_mhz }`, `struct ChipHistory` with `record(step, &[(usize, f32, u32)])`, `sample_at(idx, step) -> Option<ChipSample>`, `aiclk_max(idx) -> u32`, `note_tps(f32) -> f32`, `best_tps() -> f32`, `note_pcie(f64) -> f64`, `best_pcie_bps() -> f64`

- [ ] **Step 1: Register the module**

In `src/animation/mod.rs`, after `pub mod train_sky;` add `pub mod train_tapestry;`.

- [ ] **Step 2: Write the module with its tests**

Create `src/animation/train_tapestry.rs`:

```rust
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
    if rows == 0 || !(frac > 0.0) {
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
        (Some(name), m) if m > 0 => {
            parts.push(format!("{name} {}% through", (step.saturating_mul(100) / m).min(100)))
        }
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
}

/// A one-line verdict from plain thresholds, or `None` when the readings are
/// inconclusive. A missing TDP gives no power-based verdict rather than a
/// guess from an assumed ceiling.
pub fn diagnose(r: &Readings) -> Option<Diagnosis> {
    if r.compiled_last_step {
        return Some(Diagnosis {
            kind: DiagnosisKind::Compiling,
            text: "compiling - the program cache grew on the latest step".to_string(),
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
            text: format!("compute-bound - busiest chip at {:.0}% of TDP{cpu}", frac * 100.0),
        });
    }
    let cpu = r.host_cpu_pct?;
    if frac < HOST_BOUND_TDP_FRAC && cpu > HOST_BOUND_CPU_PCT {
        return Some(Diagnosis {
            kind: DiagnosisKind::HostBound,
            text: format!(
                "host-bound - busiest chip at {:.0}% of TDP, host cpu {cpu:.0}%",
                frac * 100.0
            ),
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
    if !(step_ms > 0.0) {
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
    if !(pass_secs > 0.0) || !(fps > 0.0) {
        return 0.0;
    }
    ((frame as f32 / fps) / pass_secs).fract()
}

/// One chip's reading, taken when a new step was first seen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipSample {
    pub step: u64,
    pub power_w: f32,
    pub aiclk_mhz: u32,
}

/// Chip readings keyed by training step, plus best-so-far values for the
/// gauges. Samples are taken when the view first sees each step, so they are
/// close to, not exactly at, the moment the step line was written.
#[derive(Debug, Default)]
pub struct ChipHistory {
    last_step: Option<u64>,
    rings: BTreeMap<usize, VecDeque<ChipSample>>,
    aiclk_max: BTreeMap<usize, u32>,
    best_tps: f32,
    best_pcie_bps: f64,
}

impl ChipHistory {
    /// Record `(device index, power W, aiclk MHz)` for every chip at `step`.
    /// A repeat of the same step is ignored. A step lower than the last one
    /// means a new run restarted the counter, so everything is cleared:
    /// old samples must not appear as the new run's lanes.
    pub fn record(&mut self, step: u64, chips: &[(usize, f32, u32)]) {
        if let Some(prev) = self.last_step {
            if step < prev {
                *self = Self::default();
            } else if step == prev {
                return;
            }
        }
        for &(idx, power_w, aiclk_mhz) in chips {
            let ring = self.rings.entry(idx).or_default();
            ring.push_back(ChipSample {
                step,
                power_w,
                aiclk_mhz,
            });
            while ring.len() > STEP_HISTORY {
                ring.pop_front();
            }
            let m = self.aiclk_max.entry(idx).or_insert(0);
            *m = (*m).max(aiclk_mhz);
        }
        self.last_step = Some(step);
    }

    pub fn sample_at(&self, idx: usize, step: u64) -> Option<ChipSample> {
        self.rings
            .get(&idx)?
            .iter()
            .rev()
            .find(|s| s.step == step)
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
        assert!(d.text.contains("58%") && d.text.contains("140%"), "{}", d.text);
        let d = diagnose(&r(false, Some(0.2), Some(380.0))).unwrap();
        assert_eq!(d.kind, DiagnosisKind::HostBound);
        assert!(d.text.contains("20%") && d.text.contains("380%"), "{}", d.text);
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
    fn chip_samples_are_found_by_step_and_a_repeat_step_is_ignored() {
        let mut h = ChipHistory::default();
        h.record(1, &[(0, 50.0, 1000), (1, 60.0, 1000)]);
        h.record(2, &[(0, 55.0, 1000), (1, 65.0, 1000)]);
        h.record(2, &[(0, 99.0, 1000), (1, 99.0, 1000)]);
        assert_eq!(h.sample_at(0, 2).unwrap().power_w, 55.0);
        assert_eq!(h.sample_at(1, 1).unwrap().power_w, 60.0);
        assert_eq!(h.sample_at(0, 3), None);
        assert_eq!(h.sample_at(7, 1), None);
    }

    /// Review Focus 5. A restarted step counter is a new run.
    #[test]
    fn a_restarted_step_counter_clears_old_samples_and_bests() {
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
        assert_eq!(h.sample_at(0, 200).unwrap().step, 200);
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
}
```

- [ ] **Step 3: Run**

Run: `cargo test --lib train_tapestry 2>&1 | tail -25`
Expected: 15 passed.

- [ ] **Step 4: Make the key tests prove themselves**

One at a time, apply each change, run the named test, confirm it fails, restore:
1. In `plan_band` swap the `verdict_row` and `lane_rows` lines. Run `cargo test --lib short_bands_drop_layers`. Expected: FAIL.
2. In `diagnose` change `frac < HOST_BOUND_TDP_FRAC && cpu > HOST_BOUND_CPU_PCT` to `cpu > HOST_BOUND_CPU_PCT`. Run `cargo test --lib the_diagnosis_uses_the_documented_thresholds`. Expected: FAIL (0.4 with 500% cpu).
3. In `ChipHistory::record` delete the `*self = Self::default();` line. Run `cargo test --lib a_restarted_step_counter`. Expected: FAIL.
4. In `bar_cell` remove `.max(1)`. Run `cargo test --lib bar_cell_resolves_eighths`. Expected: FAIL.

Re-run `cargo test --lib train_tapestry` and confirm 15 passed.

- [ ] **Step 5: Commit**

```bash
git add src/animation/mod.rs src/animation/train_tapestry.rs
git commit -m "feat: pure helpers for the training tapestry (step bars, loss analytics, diagnosis, band plan)"
```

---

### Task 3: Draw the step bars, grid backdrop and chip lanes

Replaces `draw_network`. The band at this point has a header, bars, grid and lanes. Gauges, verdict and strip arrive in Task 4, so `plan_band` is called with those wants off.

**Files:**
- Modify: `src/animation/train_view.rs`

**Interfaces:**
- Consumes: everything from Task 2; `TrainState.step_history`; `backend.devices()`, `backend.telemetry(idx)`, `backend.smbus_telemetry(idx)`, `backend.pcie_bandwidth(idx)`.
- Produces: `TrainView::draw_tapestry(&self, buf, st, backend)`, `TrainView::sample(&self, st, backend)`, `TrainView::chip_tdp(backend, &Device) -> Option<f32>`, field `history: RefCell<ChipHistory>`.

- [ ] **Step 1: Imports, constants and field**

Replace `use std::cell::Cell as StdCell;` with `use std::cell::{Cell as StdCell, RefCell};` and add after the existing `use crate::animation::train_sky::sky_cell;` line:

```rust
use crate::animation::train_tapestry::{
    bar_cell, cause_of, median, pass_fraction, pass_secs, plan_band, BandWants, ChipHistory,
    StepCause, AICLK_DROP_FRAC, LABEL_W, MAX_LANES,
};
use crate::models::Device;
```

Delete `const BWD_HUE: f32 = 268.0;` and the `SWEEP_SUBCOLS_PER_SEC` constant with its doc comment (5 lines starting `/// Sub-columns the forward/backward pass advances per second`). Delete the whole `fn sweep_head` and its doc comment (starts `/// Current sweep-head position`). Keep `FWD_HUE`, `SWEEP_TAIL`, `SWEEP_LEAD`, `smoothstep` and `sweep_at`. In the `sweep_at` doc comment, change "makes the motion read directionally" wording only if it mentions backward passes. It does not, so leave it.

Add after the `BORDER` constant:

```rust
/// Step bar colours. Teal, yellow and red are the docs-site brand tints; the
/// compile purple is the LIVE panel's cache colour so one meaning has one hue.
const BAR_NORMAL: Color = Color::Rgb(116, 197, 223);
const BAR_COMPILE: Color = Color::Rgb(180, 140, 230);
const BAR_CHECKPOINT: Color = Color::Rgb(246, 188, 66);
/// A chip lane cell where aiclk had dropped well below that chip's best.
const AICLK_DROP: Color = Color::Rgb(255, 158, 138);
/// Resting colour of the grid backdrop, median line and empty gauge cells.
const WIRE_REST: Color = Color::Rgb(80, 90, 115);
/// Placeholder for a lane column with no chip sample. A glyph used nowhere
/// else, so a test can count missing samples exactly.
const NO_SAMPLE: char = '⋅';
/// Columns between nodes in the grid backdrop.
const GRID_STRIDE: usize = 4;
```

Add to the `TrainView` struct, after `cache_steady_ticks`:

```rust
    /// Chip readings per step and best-so-far values for the gauges. Filled
    /// by `sample` during `render(&self, ...)`, so it is a `RefCell` for the
    /// same reason `cache_last` is a `Cell`.
    history: RefCell<ChipHistory>,
```

and in `TrainView::new` add `history: RefCell::new(ChipHistory::default()),`.

- [ ] **Step 2: Write the failing tests**

Run this script to replace the old grid and sweep tests and add the new ones. Save it to the scratchpad directory as `edit_tests.py` and run `python3 <scratchpad>/edit_tests.py` from the repo root. It first writes the new test text, then applies three marker-based slice edits to `src/animation/train_view.rs` and fails loudly if any marker is missing.

```python
import re, sys
p = "src/animation/train_view.rs"
s = open(p).read()

def cut(s, start, end):
    i = s.index(start)
    j = s.index(end, i)
    return i, j

SWEEP_TESTS = '''    /// Fluidity guard. The cursor head is continuous with a lead-in and a
    /// trailing falloff, so a cell's brightness changes by small increments
    /// between consecutive frames. A regression to on/off would produce a 1.0
    /// jump, and dropping the lead-in ramp measured 0.95.
    #[test]
    fn sweep_intensity_changes_smoothly_between_consecutive_frames() {
        let period = 50.0;
        let head = |f: u64| pass_fraction(f, crate::animation::train_sky::ANIM_FPS, 1.0) * period;
        let mut worst: f32 = 0.0;
        for at in [0.0f32, 7.5, 15.0, 33.0, 49.0] {
            let mut prev = sweep_at(head(0), at, period);
            for f in 1..240u64 {
                let cur = sweep_at(head(f), at, period);
                worst = worst.max((cur - prev).abs());
                prev = cur;
            }
        }
        assert!(
            worst < 0.25,
            "sweep steps by {worst:.3} between frames - motion should be continuous"
        );
        let peak = (0..240u64)
            .map(|f| sweep_at(head(f), 15.0, period))
            .fold(0.0f32, f32::max);
        assert!(peak > 0.85, "sweep never reaches full brightness: {peak:.3}");
    }

    /// The pulse must travel in one direction and complete one pass per
    /// measured step: at a 1 s step it wraps once per 60 frames.
    #[test]
    fn the_cursor_advances_monotonically_and_wraps_once_per_pass() {
        let fps = crate::animation::train_sky::ANIM_FPS;
        let mut last = pass_fraction(0, fps, 1.0);
        let mut wraps = 0;
        for f in 1..=180u64 {
            let h = pass_fraction(f, fps, 1.0);
            if h < last {
                wraps += 1;
            } else {
                assert!(h - last < 0.05, "cursor jumped {:.3} in one frame", h - last);
            }
            last = h;
        }
        assert_eq!(wraps, 3, "three one-second passes in 180 frames at {fps} fps");
    }

'''

NEW_TESTS = '''    // ---- tapestry band -------------------------------------------------

    fn live_state() -> TrainState {
        use crate::workload::train::{LogSource, TrainProcess};
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(crate::workload::train::TrainEvent::Step { step: 1, loss: 2.0 });
        st
    }

    fn sample_at(step: u64, ms: f32, delta: u32) -> crate::workload::train::StepSample {
        crate::workload::train::StepSample {
            step,
            ms,
            cache_delta: delta,
            checkpoint: false,
        }
    }

    fn rows_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn step_bars_are_drawn_from_the_step_history() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        // Thirty normal steps and one four times slower: the bar scale is the
        // window maximum, so only the slow step reaches the top row.
        st.step_history = (1..=31u64)
            .map(|i| sample_at(i, if i == 20 { 400.0 } else { 100.0 }, 0))
            .collect();
        st.step = 31;
        st.step_ms = 100.0;
        let rows = rows_of(&TrainView::new(134, 40).render(&st, &b));
        let header = rows
            .iter()
            .position(|r| r.contains("STEP ANATOMY"))
            .expect("the band header should render");
        assert!(rows[header].contains("last 31 steps"), "{}", rows[header]);
        assert!(rows[header].contains("median 100 ms"), "{}", rows[header]);
        let top = &rows[header + 1];
        assert_eq!(
            top.matches('█').count(),
            1,
            "only the slow step reaches the top bar row: {top:?}"
        );
        // The normal steps are a quarter of the scale: one full bottom cell.
        let bottom = &rows[header + 4];
        assert!(bottom.matches('█').count() >= 31, "{bottom:?}");
    }

    /// Review Focus 1.
    #[test]
    fn a_trainer_with_no_per_step_times_gets_no_bars_but_keeps_the_pulse() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_ms = 400.0; // derived from log cadence; no per-step history
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY"), "{out}");
        assert!(out.contains("no per-step times reported"), "{out}");
        assert!(out.contains("pulse"), "the grid backdrop still renders:\n{out}");
    }

    #[test]
    fn the_header_says_when_the_pulse_is_slowed_to_stay_visible() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 83.0, 0)).collect();
        st.step = 10;
        st.step_ms = 83.0;
        let out = text_of(&TrainView::new(150, 40).render(&st, &b));
        assert!(out.contains("pulse = 1 step (drawn at 5/s max)"), "{out}");
        st.step_ms = 400.0;
        let out = text_of(&TrainView::new(150, 40).render(&st, &b));
        assert!(out.contains("pulse = 1 step"), "{out}");
        assert!(!out.contains("5/s max"), "{out}");
    }

    /// Review Focus 2.
    #[test]
    fn a_backend_with_no_chips_draws_no_lanes_and_does_not_panic() {
        // Not `init()`ed: `MockBackend::new(0)` refuses to initialise, and an
        // un-initialised backend reports no devices, which is the case.
        let b = MockBackend::new(0);
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step = 10;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY"), "{out}");
        assert!(!out.contains("chip0"), "no chip, no lane:\n{out}");
    }

    /// The lane columns share the bars' time axis, and a column with no chip
    /// sample says so rather than drawing a value.
    #[test]
    fn chip_lanes_line_up_with_the_step_bars_and_mark_missing_samples() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=8u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step_ms = 100.0;
        let v = TrainView::new(134, 40);
        let lane_row = |v: &TrainView, st: &TrainState| -> String {
            rows_of(&v.render(st, &b))
                .into_iter()
                .find(|r| r.contains("chip0"))
                .expect("a lane for chip 0")
        };
        // The view only sees steps 1..=4: the other four columns are missing.
        for step in 1..=4u64 {
            st.step = step;
            v.render(&st, &b);
        }
        let row = lane_row(&v, &st);
        assert_eq!(row.matches(NO_SAMPLE).count(), 4, "{row:?}");
        // Seeing the rest fills every column.
        for step in 5..=8u64 {
            st.step = step;
            v.render(&st, &b);
        }
        let row = lane_row(&v, &st);
        assert_eq!(row.matches(NO_SAMPLE).count(), 0, "{row:?}");
    }

    /// Review Focus 4.
    #[test]
    fn the_tapestry_fits_every_terminal_size() {
        let mut b = MockBackend::new(3);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=64u64).map(|i| sample_at(i, 100.0 + i as f32, (i % 9 == 0) as u32)).collect();
        st.step = 64;
        st.step_ms = 150.0;
        for w in [20usize, 40, 60, 80, 100, 134] {
            for h in [10usize, 14, 20, 30, 40] {
                let v = TrainView::new(w, h);
                v.render(&st, &b);
                for line in v.render(&st, &b) {
                    let s: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
                    assert!(
                        !s.contains('╗') && !s.contains('╝'),
                        "right-side corner (w={w} h={h}): {s:?}"
                    );
                    for (pos, _) in s.match_indices('║') {
                        assert_eq!(pos, 0, "`║` only at column 0 (w={w} h={h}): {s:?}");
                    }
                    let cols = unicode_width::UnicodeWidthStr::width(s.as_str());
                    assert!(cols <= w, "line is {cols} cols at width {w}x{h}: {s:?}");
                }
            }
        }
    }

    #[test]
    fn the_legend_lists_step_symbols_only_when_there_are_step_samples() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let legend = |st: &TrainState| -> String {
            rows_of(&TrainView::new(150, 40).render(st, &b))
                .into_iter()
                .find(|r| r.contains("chip temp"))
                .unwrap_or_default()
        };
        let mut st = live_state();
        let without = legend(&st);
        assert!(!without.contains("step time") && !without.contains("compile"), "{without}");
        st.step_history = vec![sample_at(1, 100.0, 0)];
        let with = legend(&st);
        assert!(with.contains("step time") && with.contains("compile"), "{with}");
    }

    /// The topology text left the band, so it has to be findable on the MODEL
    /// card, and still must not state a count nobody sourced.
    #[test]
    fn the_model_card_states_known_topology_and_nothing_unknown() {
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let v = TrainView::new(100, 30);

        let st = live_state();
        assert_eq!(st.config.num_blocks, None);
        assert_eq!(st.config.num_heads, None);
        let out = text_of(&v.render(&st, &b));
        assert!(
            !out.contains("blocks") && !out.contains("heads"),
            "must not assert a block/head count when the config is unknown:\\n{out}"
        );

        let mut st2 = live_state();
        st2.config.num_blocks = Some(12);
        st2.config.num_heads = Some(8);
        let out2 = text_of(&v.render(&st2, &b));
        assert!(out2.contains("blocks 12"), "{out2}");
        assert!(out2.contains("heads 8"), "{out2}");
    }
}
'''

# 1. replace both sweep tests
i, j = cut(s, "    /// Fluidity guard. The sweep used to be a binary", "    /// The grid should breathe on a normal terminal")
s = s[:i] + SWEEP_TESTS + s[j:]
# 2. delete the roomy-grid test
i, j = cut(s, "    /// The grid should breathe on a normal terminal", "    #[test]\n    fn network_header_never_fabricates_topology_it_cannot_source")
s = s[:i] + s[j:]
# 3. replace the old topology test through end of the module
i = s.index("    #[test]\n    fn network_header_never_fabricates_topology_it_cannot_source")
s = s[:i] + NEW_TESTS
open(p, "w").write(s)
print("ok")
```

In the same file, fix the two old legend assertions: in `the_legend_only_lists_symbols_that_can_appear`, delete the line `"gradients",` from the first `for present in [...]` list, and change `for present in ["loss ↓", "checkpoint", "gradients"] {` to `for present in ["loss ↓", "checkpoint"] {`.

Run: `cargo test --lib animation::train_view 2>&1 | tail -30`
Expected: compile errors (`draw_tapestry` and the removed items are still referenced) or test failures. That is the red state.

- [ ] **Step 3: Replace `draw_network`**

Delete `fn draw_network` entirely with this script (saved and run the same way as the one above):

```python
p = "src/animation/train_view.rs"
s = open(p).read()
i = s.index("    fn draw_network(&self, buf: &mut [Vec<Cell>], st: &TrainState) {")
j = s.index("    #[allow(unused_assignments)]\n    fn draw_live_stats")
s = s[:i] + "    // __TAPESTRY__\n\n" + s[j:]
open(p, "w").write(s)
```

Then replace the `    // __TAPESTRY__` line with the code below.

```rust
    /// Record this frame's chip readings against the current step, and fold
    /// tokens/sec and PCIe throughput into their best-so-far values.
    fn sample(&self, st: &TrainState, backend: &dyn TelemetryBackend) {
        // A chip with no telemetry is not sampled, so it never gets a lane.
        let chips: Vec<(usize, f32, u32)> = backend
            .devices()
            .iter()
            .filter_map(|d| {
                let t = backend.telemetry(d.index)?;
                Some((d.index, t.power_w(), t.aiclk_mhz()))
            })
            .collect();
        let mut h = self.history.borrow_mut();
        h.record(st.step, &chips);
        if let Some(tps) = st.tokens_per_sec() {
            h.note_tps(tps);
        }
        if let Some(bps) = Self::pcie_total(backend) {
            h.note_pcie(bps);
        }
    }

    /// Summed in+out PCIe bytes/sec across chips that report it, `None` when
    /// no chip does (only the sysfs/hybrid backends have these counters).
    fn pcie_total(backend: &dyn TelemetryBackend) -> Option<f64> {
        let mut any = false;
        let mut total = 0.0f64;
        for d in backend.devices() {
            if let Some(bw) = backend.pcie_bandwidth(d.index) {
                any = true;
                total += bw.rx_bytes_per_sec + bw.tx_bytes_per_sec;
            }
        }
        any.then_some(total)
    }

    /// A chip's TDP in watts, from SMBUS telemetry first and the tt-smi
    /// `limits` block second. `None` when neither reports a positive value.
    fn chip_tdp(backend: &dyn TelemetryBackend, device: &Device) -> Option<f32> {
        backend
            .smbus_telemetry(device.index)
            .and_then(|s| s.tdp_watts())
            .or_else(|| device.limits.as_ref().and_then(|l| l.tdp_limit))
            .filter(|v| *v > 0.0)
    }

    /// The tapestry band: step bars, grid backdrop and chip lanes, drawn only
    /// from `st.step_history`, the measured step time and chip telemetry. A
    /// layer whose signal is missing is left out.
    fn draw_tapestry(
        &self,
        buf: &mut [Vec<Cell>],
        st: &TrainState,
        backend: &dyn TelemetryBackend,
    ) {
        let (x0, w) = self.network_bounds();
        let Layout {
            network_top,
            network_h,
            ..
        } = self.layout();
        let label = Color::Rgb(150, 200, 255);
        let dim = Color::Rgb(120, 130, 155);

        let lane_devices: Vec<&Device> = backend
            .devices()
            .iter()
            .filter(|d| backend.telemetry(d.index).is_some())
            .take(MAX_LANES)
            .collect();
        let plan = plan_band(
            network_h,
            BandWants {
                lanes: lane_devices.len(),
                ..BandWants::default()
            },
        );

        // The newest `n` steps, right-aligned so the latest step is always at
        // the band's right edge and the lanes below share the same columns.
        let hist = &st.step_history;
        let bar_w = w.saturating_sub(LABEL_W + 1);
        let n = hist.len().min(bar_w);
        let shown = &hist[hist.len() - n..];
        let x_bars = x0 + LABEL_W;
        let col0 = x_bars + (bar_w - n);
        let pass = pass_secs(st.step_ms);

        // ── header ───────────────────────────────────────────────────
        let header = if shown.is_empty() {
            "STEP ANATOMY  no per-step times reported".to_string()
        } else {
            let ms: Vec<f32> = shown.iter().map(|s| s.ms).collect();
            let med = median(&ms).unwrap_or(0.0);
            let mut h = format!("STEP ANATOMY  last {n} steps · median {med:.0} ms");
            match pass {
                Some((_, true)) => h.push_str(" · pulse = 1 step (drawn at 5/s max)"),
                Some((_, false)) => h.push_str(" · pulse = 1 step"),
                None => {}
            }
            h
        };
        self.text(buf, x0, network_top, &Self::clip(&header, w), label, false);

        // ── step bars ────────────────────────────────────────────────
        let y_bars = network_top + 1;
        let max_ms = shown.iter().map(|s| s.ms).fold(0.0f32, f32::max);
        if plan.bar_rows > 0 && max_ms > 0.0 {
            // The scale is printed, not implied: window maximum on the top
            // row and zero on the bottom one.
            self.text(buf, x0, y_bars, &format!("{max_ms:>5.0}"), dim, false);
            if plan.bar_rows > 1 {
                let y_last = y_bars + plan.bar_rows - 1;
                self.text(buf, x0, y_last, &format!("{:>5}", 0), dim, false);
            }
            for (i, s) in shown.iter().enumerate() {
                let frac = s.ms / max_ms;
                let color = match cause_of(s) {
                    StepCause::Normal => BAR_NORMAL,
                    StepCause::Compile => BAR_COMPILE,
                    StepCause::Checkpoint => BAR_CHECKPOINT,
                };
                for r in 0..plan.bar_rows {
                    let from_bottom = plan.bar_rows - 1 - r;
                    let ch = bar_cell(frac, from_bottom, plan.bar_rows);
                    self.put(buf, col0 + i, y_bars + r, ch, color, false);
                }
            }
            // Median line: a dotted rule across every column whose bar is
            // shorter than the median, so it shows through gaps and never
            // overwrites a bar.
            let ms: Vec<f32> = shown.iter().map(|s| s.ms).collect();
            let med = median(&ms).unwrap_or(0.0);
            let eighths = ((med / max_ms * plan.bar_rows as f32 * 8.0).round() as usize).max(1);
            let med_from_bottom = ((eighths - 1) / 8).min(plan.bar_rows - 1);
            let y_med = y_bars + plan.bar_rows - 1 - med_from_bottom;
            for (i, s) in shown.iter().enumerate() {
                if bar_cell(s.ms / max_ms, med_from_bottom, plan.bar_rows) == ' ' {
                    self.put(buf, col0 + i, y_med, '┈', WIRE_REST, false);
                }
            }
        }

        // ── grid backdrop with the step-rate cursor ──────────────────
        let mut y = y_bars + plan.bar_rows;
        if plan.grid_row {
            self.text(buf, x0, y, "pulse", dim, false);
            let gw = bar_w;
            let period = gw as f32 + SWEEP_TAIL + SWEEP_LEAD;
            let head = pass.map(|(secs, _)| {
                pass_fraction(self.frame, crate::animation::train_sky::ANIM_FPS, secs) * period
            });
            let node_glyphs = ['●', '◉', '○', '◇', '·'];
            for c in 0..gw {
                let lit = head.map(|h| sweep_at(h, c as f32, period)).unwrap_or(0.0);
                let ch = if c % GRID_STRIDE == 0 {
                    node_glyphs[((1.0 - lit) * (node_glyphs.len() - 1) as f32).round() as usize
                        % node_glyphs.len()]
                } else {
                    '─'
                };
                let (col, bold) = if lit > 0.04 {
                    (hsv_to_rgb(FWD_HUE, 0.45 + lit * 0.4, 0.35 + lit * 0.6), lit > 0.55)
                } else {
                    (WIRE_REST, false)
                };
                self.put(buf, x_bars + c, y, ch, col, bold);
            }
            y += 1;
        }

        // ── chip lanes ───────────────────────────────────────────────
        let hist_chips = self.history.borrow();
        for dev in lane_devices.iter().take(plan.lane_rows) {
            self.text(
                buf,
                x0,
                y,
                &Self::clip(&format!("chip{}", dev.index), LABEL_W),
                dim,
                false,
            );
            let temp = backend
                .telemetry(dev.index)
                .map(|t| t.temp_c())
                .unwrap_or(0.0);
            let tcolor = colors::temp_color(temp);
            let samples: Vec<_> = shown
                .iter()
                .map(|s| hist_chips.sample_at(dev.index, s.step))
                .collect();
            // Scale to the chip's TDP. With no TDP known, to the highest power
            // in the window, so the lane still shows its own shape.
            let scale = Self::chip_tdp(backend, dev).unwrap_or_else(|| {
                samples
                    .iter()
                    .flatten()
                    .map(|c| c.power_w)
                    .fold(0.0, f32::max)
            });
            let aimax = hist_chips.aiclk_max(dev.index);
            for (i, smp) in samples.iter().enumerate() {
                let (ch, col) = match smp {
                    Some(c) if scale > 0.0 => {
                        let dropped = c.aiclk_mhz > 0
                            && aimax > 0
                            && (c.aiclk_mhz as f32) < AICLK_DROP_FRAC * aimax as f32;
                        (
                            bar_cell(c.power_w / scale, 0, 1),
                            if dropped { AICLK_DROP } else { tcolor },
                        )
                    }
                    _ => (NO_SAMPLE, WIRE_REST),
                };
                self.put(buf, col0 + i, y, ch, col, false);
            }
            y += 1;
        }
    }
```

In `render`, in both match arms replace `self.draw_network(&mut buf, st);` with `self.draw_tapestry(&mut buf, st, backend);`. Immediately after `Some(_) => {` (the arm that begins with `self.draw_header(&mut buf, st);`) add as the first line of that arm:

```rust
                self.sample(st, backend);
```

Update the comment in `layout()` that says "(see `draw_network`'s `row_stride`)" to read "(see `plan_band` in `train_tapestry`)". Update the comments in `render` that say "the network grid and the river" to "the tapestry band and the river".

- [ ] **Step 4: Legend**

In `draw_legend`, replace the two entries `('─', "forward", ...)` and `('∙', "gradients", ...)` with:

```rust
            ('▇', "step time", BAR_NORMAL),
            ('▇', "compile", BAR_COMPILE),
```

and change the filter so these appear only when step samples exist:

```rust
        let has_steps = !st.step_history.is_empty();
        let entries: Vec<(char, &str, Color)> = all
            .into_iter()
            .filter(|(_, label, _)| has_stream || !matches!(*label, "loss" | "loss ↓" | "loss ↑"))
            .filter(|(_, label, _)| has_steps || !matches!(*label, "step time" | "compile"))
            .collect();
```

Update the comment above `entries` to add: "`step time` and `compile` need step samples, so a trainer that prints no per-step time does not advertise them."

Update the module doc table at the top of the file: replace the rows `amber sweep left→right | forward pass` and `violet sweep right→left | backward pass / gradients` with:

```
//! | bar height (step band) | trainer-reported wall time per step |
//! | bar colour teal / purple / amber | normal step / program cache grew / checkpoint written |
//! | pulse cursor speed | one pass per measured step (never faster than 5/s) |
//! | chip lane height | power as a fraction of that chip's TDP (coral = aiclk dropped) |
```

and change the first paragraph "a live tt-train run drawn as a character-grid network being fed tokens" to "a live tt-train run drawn as a step-time chart over chip power".

- [ ] **Step 5: Run the view tests**

Run: `cargo test --lib animation::train_view 2>&1 | tail -30`
Expected: all pass, including the 8 new tests and the existing width/side-panel tests. If an older test fails because it asserted on grid output, report which one and why before changing it.

- [ ] **Step 6: Make the key tests prove themselves**

1. In `draw_tapestry` change `let col0 = x_bars + (bar_w - n);` to `let col0 = x_bars;`. Run `cargo test --lib chip_lanes_line_up_with_the_step_bars`. Expected: FAIL (columns no longer right-aligned, placeholder count wrong). Restore.
2. Change `cause_of(s)` in the bar colour match to `StepCause::Normal`. This is invisible to text tests, so also confirm `a_compile_outranks_a_checkpoint` still guards the logic, and note in the commit message that colours are covered at the helper layer because span colours depend on terminal colour support.
3. Change `h.record(st.step, &chips);` in `sample` to do nothing (comment it out). Run `cargo test --lib chip_lanes_line_up`. Expected: FAIL. Restore.

Re-run `cargo test --lib animation::train_view` and confirm green.

- [ ] **Step 7: Commit**

```bash
git add src/animation/train_view.rs
git commit -m "feat: replace the decorative node grid with step bars, a step-rate pulse and chip lanes"
```

---

### Task 4: Gauges, diagnosis and convergence strip

**Files:**
- Modify: `src/animation/train_view.rs`

**Interfaces:**
- Consumes: `diagnose`, `Readings`, `Diagnosis`, `DiagnosisKind`, `convergence_parts` from Task 2; `ChipHistory::{best_tps, aiclk_max, best_pcie_bps}`.
- Produces: `struct Gauge { label: &'static str, frac: f32, text: String }`, `TrainView::gauges`, `TrainView::busiest_chip`, `TrainView::draw_gauge`.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests`, before its closing brace:

```rust
    #[test]
    fn gauges_show_the_busiest_chip_and_name_their_readings() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step_ms = 100.0;
        st.step = 10;
        let v = TrainView::new(134, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(out.contains("power") && out.contains("% TDP"), "{out}");
        assert!(out.contains("aiclk") && out.contains("MHz"), "{out}");
        // The mock backend has no PCIe counters, so there is no PCIe gauge.
        assert!(!out.contains("MB/s"), "{out}");
    }

    #[test]
    fn the_tokens_per_second_gauge_is_relative_to_the_runs_own_best() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.config.max_sequence_length = Some(256);
        st.batch_size = 8;
        st.step_history = vec![sample_at(1, 100.0, 0)];
        let v = TrainView::new(134, 40);
        st.step = 1;
        st.step_ms = 100.0;
        v.render(&st, &b); // best so far is set by this step rate
        st.step = 2;
        st.step_ms = 200.0; // half the rate
        let out = text_of(&v.render(&st, &b));
        assert!(out.contains("50% of best"), "{out}");
    }

    #[test]
    fn the_diagnosis_line_appears_with_its_readings() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = vec![sample_at(1, 100.0, 0), sample_at(2, 300.0, 6)];
        st.step = 2;
        st.step_ms = 300.0;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(
            out.contains("compiling - the program cache grew on the latest step"),
            "{out}"
        );
        // No compile and no readings that decide it: no verdict line at all.
        st.step_history = vec![sample_at(1, 100.0, 0), sample_at(2, 100.0, 0)];
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(!out.contains("compiling -"), "{out}");
    }

    #[test]
    fn the_convergence_strip_reads_the_loss_history_and_config() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.loss_history = (0..120).map(|i| 5.0 - 0.01 * i as f32).collect();
        st.config.learning_rate = Some(3.0e-4);
        st.scheduler = Some("cosine".into());
        st.max_steps = 100;
        st.step = 41;
        // 160 columns leaves the band about 96 wide; the full strip is 85 to 90.
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("↘"), "{out}");
        assert!(out.contains("base lr 3.0e-4"), "{out}");
        assert!(out.contains("cosine 41% through"), "{out}");
        // A rising loss flips the arrow.
        st.loss_history = (0..120).map(|i| 1.0 + 0.01 * i as f32).collect();
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("↗") && !out.contains("↘"), "{out}");
    }
```

Run: `cargo test --lib animation::train_view 2>&1 | tail -20`
Expected: the four new tests FAIL (nothing draws gauges, verdict or strip yet).

- [ ] **Step 2: Implement**

Extend the `train_tapestry` import list in `train_view.rs` with `convergence_parts, diagnose, DiagnosisKind, Readings`.

Add above `impl TrainView` (next to the other top-level items):

```rust
/// One gauge row cell: a label, a fill fraction and the reading in words.
struct Gauge {
    label: &'static str,
    frac: f32,
    text: String,
}

/// The chip drawing the most power right now, standing in for "the chip the
/// training run is on". Every chip the backend can see is a candidate because
/// the trainer's chips are not identified, so an idle neighbour never dilutes
/// the reading the way an average would.
struct Busiest {
    index: usize,
    power_w: f32,
    aiclk_mhz: u32,
    tdp: Option<f32>,
}
```

Add these methods to `impl TrainView` (after `chip_tdp`):

```rust
    fn busiest_chip(&self, backend: &dyn TelemetryBackend) -> Option<Busiest> {
        backend
            .devices()
            .iter()
            .filter_map(|d| {
                let t = backend.telemetry(d.index)?;
                Some(Busiest {
                    index: d.index,
                    power_w: t.power_w(),
                    aiclk_mhz: t.aiclk_mhz(),
                    tdp: Self::chip_tdp(backend, d),
                })
            })
            .max_by(|a, b| a.power_w.total_cmp(&b.power_w))
    }

    /// The gauges that have a source, in display order. A gauge with no
    /// source is absent, not drawn empty.
    fn gauges(&self, st: &TrainState, backend: &dyn TelemetryBackend) -> Vec<Gauge> {
        let hist = self.history.borrow();
        let mut out = Vec::new();
        if let Some(tps) = st.tokens_per_sec() {
            let best = hist.best_tps().max(tps);
            if best > 0.0 {
                out.push(Gauge {
                    label: "tok/s",
                    frac: tps / best,
                    text: format!("{:.0}% of best", tps / best * 100.0),
                });
            }
        }
        if let Some(chip) = self.busiest_chip(backend) {
            if let Some(tdp) = chip.tdp {
                out.push(Gauge {
                    label: "power",
                    frac: chip.power_w / tdp,
                    text: format!("{:.0}% TDP", chip.power_w / tdp * 100.0),
                });
            }
            let amax = hist.aiclk_max(chip.index);
            if chip.aiclk_mhz > 0 && amax > 0 {
                out.push(Gauge {
                    label: "aiclk",
                    frac: chip.aiclk_mhz as f32 / amax as f32,
                    text: format!("{} MHz", chip.aiclk_mhz),
                });
            }
        }
        if let Some(bps) = Self::pcie_total(backend) {
            // The bar is relative to the highest throughput seen, because no
            // single ceiling is right for every link generation and width.
            let best = hist.best_pcie_bps().max(bps);
            out.push(Gauge {
                label: "pcie",
                frac: if best > 0.0 { (bps / best) as f32 } else { 0.0 },
                text: format!("{:.0} MB/s", bps / 1e6),
            });
        }
        out
    }

    /// One gauge in a cell `w` columns wide: label, bar (when there is room
    /// for at least three cells) and the reading.
    fn draw_gauge(&self, buf: &mut [Vec<Cell>], x: usize, y: usize, w: usize, g: &Gauge) {
        let label = format!("{:<width$}", g.label, width = LABEL_W);
        self.text(buf, x, y, &Self::clip(&label, w), Color::Rgb(150, 200, 255), false);
        let val_len = g.text.chars().count();
        let bar_w = w.saturating_sub(LABEL_W + 1 + val_len).min(10);
        let mut cx = x + LABEL_W;
        if bar_w >= 3 {
            let filled = (g.frac.clamp(0.0, 1.0) * bar_w as f32).round() as usize;
            for i in 0..bar_w {
                let (ch, col) = if i < filled {
                    ('█', BAR_NORMAL)
                } else {
                    ('░', WIRE_REST)
                };
                self.put(buf, cx + i, y, ch, col, false);
            }
            cx += bar_w + 1;
        }
        let room = (x + w).saturating_sub(cx);
        self.text(buf, cx, y, &Self::clip(&g.text, room), Color::Rgb(210, 230, 220), false);
    }
```

In `draw_tapestry`, replace the block that builds `plan` (the `let plan = plan_band(...)` statement) with:

```rust
        let gauges = self.gauges(st, backend);
        let parts = convergence_parts(
            &st.loss_history,
            st.config.learning_rate,
            st.scheduler.as_deref(),
            st.step,
            st.max_steps,
        );
        let busiest = self.busiest_chip(backend);
        let diagnosis = diagnose(&Readings {
            compiled_last_step: st.step_history.last().map(|s| s.cache_delta > 0).unwrap_or(false),
            busiest_tdp_frac: busiest
                .as_ref()
                .and_then(|b| b.tdp.map(|t| b.power_w / t)),
            host_cpu_pct: st.host_cpu_pct,
        });
        let plan = plan_band(
            network_h,
            BandWants {
                lanes: lane_devices.len(),
                gauges: gauges.len(),
                verdict: diagnosis.is_some(),
                strip: !parts.is_empty(),
            },
        );
```

Append at the end of `draw_tapestry`, after the lanes loop (inside the function; `hist_chips` is still borrowed there, so drop it first):

```rust
        drop(hist_chips);

        // ── gauges: two per row, half the band each ──────────────────
        let half = w / 2;
        for (i, g) in gauges.iter().enumerate() {
            let row = i / 2;
            if row >= plan.gauge_rows {
                break;
            }
            let (gx, gw) = if i % 2 == 0 {
                (x0, half)
            } else {
                (x0 + half, w - half)
            };
            self.draw_gauge(buf, gx, y + row, gw.saturating_sub(1), g);
        }
        y += plan.gauge_rows;

        // ── diagnosis ────────────────────────────────────────────────
        if let (true, Some(d)) = (plan.verdict_row, diagnosis.as_ref()) {
            let color = match d.kind {
                DiagnosisKind::Compiling => BAR_COMPILE,
                DiagnosisKind::ComputeBound => Color::Rgb(111, 171, 160),
                DiagnosisKind::HostBound => BAR_CHECKPOINT,
            };
            self.text(buf, x0, y, &Self::clip(&format!("▸ {}", d.text), w), color, false);
            y += 1;
        }

        // ── convergence strip ────────────────────────────────────────
        if plan.strip_row {
            self.text(
                buf,
                x0,
                y,
                &Self::clip(&parts.join("  "), w),
                Color::Rgb(190, 200, 220),
                false,
            );
        }
```

In the lanes section, the `let hist_chips = self.history.borrow();` line stays as is. Because `gauges` is computed before it, the shared borrows do not conflict.

- [ ] **Step 3: Run**

Run: `cargo test --lib animation::train_view 2>&1 | tail -25`
Expected: all pass, including `the_tapestry_fits_every_terminal_size` (now with gauges, verdict and strip in play; the history there has a compile every ninth step, so the verdict can appear).

If `the_tapestry_fits_every_terminal_size` fails on a width, the failing assertion names the row. Fix the clipping in the draw code, not the test.

- [ ] **Step 4: Make the new tests prove themselves**

1. In `gauges`, change `let best = hist.best_tps().max(tps);` to `let best = tps;`. Run `cargo test --lib the_tokens_per_second_gauge`. Expected: FAIL (reads 100%). Restore.
2. In `convergence_parts` (Task 2 file) swap `'↘'` and `'↗'`. Run `cargo test --lib the_convergence_strip_reads`. Expected: FAIL. Restore.
3. In `draw_tapestry` change `verdict: diagnosis.is_some(),` to `verdict: false,`. Run `cargo test --lib the_diagnosis_line_appears`. Expected: FAIL. Restore.

Re-run `cargo test --lib animation::train_view` and confirm green.

- [ ] **Step 5: Look at it**

Run the mock view and check the band by eye at a roomy size and a small one:

```bash
cargo run --release -- --mock --mode training
```

Confirm: bars and median line render, the pulse crosses at a visible rate, purple bars appear while the cache fills in the first seconds, an amber bar appears about every 10 s, lanes fill in from the right as steps are seen, gauges show power and aiclk, and no PCIe gauge appears in mock mode. If `--mode training` is not the flag, run `cargo run --release -- --help` and use the flag it names for the Training view.

- [ ] **Step 6: Commit**

```bash
git add src/animation/train_view.rs
git commit -m "feat: add hardware gauges, a one-line diagnosis and a convergence strip to the training band"
```

---

### Task 5: Step times observed from a progress bar

Added after the first real run. The tt-tnt harness (`python train/run.py`, ttml `train()`) prints a tqdm bar and no per-step time line. Its bar also restarts at step 1 for every chunk (630/3195, then 1/3195 after each validation boundary). Three consequences, all fixed here:

1. The step chart stays empty (`no per-step times reported`). The user chose to time steps from bar updates.
2. `ChipHistory` keys on `st.step`, so every bar restart looks like a new run and clears the lanes and best-so-far values.
3. `note_step_progress` only re-anchors when the step rises, so after the first restart the derived step time and tokens/sec freeze until the bar passes its old position (an existing bug).

**Files:**
- Modify: `src/workload/train/monitor.rs` (`StepSample`, `TrainState`, `apply_event` `StepTime` arm, `note_step_progress`, tests)
- Modify: `src/workload/train/mod.rs` (re-export `StepTimeSource`)
- Modify: `src/workload/train/mock.rs` (`seq`, `step_seq`, `step_time_source`)
- Modify: `src/animation/train_view.rs` (`sample`, lanes lookup, header title, strip budget, existing tests that set `st.step` to drive sampling, new tests)

**Interfaces:**
- Consumes: Task 1's `StepSample`, `TrainState.step_history`, `apply_event`; Task 3's `sample()`/lane code; Task 2's `convergence_parts(.., step, max_steps)`.
- Produces:
  - `StepSample.seq: u64` (new field). The run's own sample sequence number: it only rises within a run, whatever the trainer's step counter does. `step` stays the trainer's own number (informational).
  - `pub enum StepTimeSource { Unknown (default), Reported, Observed }` and `TrainState.step_time_source`
  - `TrainState.step_seq: u64` (count of samples recorded this run), `TrainState.chunked_bar: bool` (a step regression was seen: the bar is per chunk)
  - `TrainState::record_observed_step(&mut self, step: u64, ms: f32)`

**Design rules (from the user's choice and the honesty rule):**
- An observed sample is recorded only when exactly one step was seen since the previous poll. A poll that saw several steps measures an average, so it records no sample. A trainer faster than one step per poll therefore has no history, and the header says so.
- The time is the gap between the polls that saw step n-1 and step n, so it is accurate to within one poll interval. The header title says `STEP ANATOMY (from bar)` so it is never taken for a trainer-reported time.
- The gap across a step regression (a new chunk) includes validation and checkpoint work. It is not a step time, so no sample is recorded for it.
- Observed samples carry `cache_delta: 0` (unknown, not zero growth), so they are never coloured as compiles.
- Samples are keyed by `seq` for chip lanes. `ChipHistory` is keyed by `st.step_seq`, not `st.step`, so a bar restart does not clear it.
- Once a regression has been seen (`chunked_bar`), the bar's total is a chunk size and not the run's budget, so the strip's schedule position drops its percentage (name only) and the schedule claim is never made from a chunk.

- [ ] **Step 1: Write the failing tests**

Append to `src/workload/train/monitor.rs` (a new test module at the end, like `step_history_tests`):

```rust
#[cfg(test)]
mod observed_step_tests {
    use super::*;
    use std::time::Duration;

    fn bar_state_at(m: &mut TrainMonitor, step: u64, now: Instant) {
        m.state.step = step;
        m.note_step_progress(now);
    }

    #[test]
    fn one_step_between_polls_is_timed_from_the_gap() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        bar_state_at(&mut m, 11, t0 + Duration::from_millis(300));
        assert_eq!(m.state.step_history.len(), 1);
        let s = m.state.step_history[0];
        assert!((s.ms - 300.0).abs() < 1.0, "{}", s.ms);
        assert_eq!(s.step, 11);
        assert_eq!(s.seq, 1);
        assert_eq!(s.cache_delta, 0, "unknown growth is not a compile");
        assert_eq!(m.state.step_time_source, StepTimeSource::Observed);
    }

    /// A poll that read several steps measures an average, which would give
    /// the chart resolution it does not have.
    #[test]
    fn several_steps_in_one_poll_record_no_sample_but_still_update_the_rate() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        bar_state_at(&mut m, 13, t0 + Duration::from_millis(900));
        assert!(m.state.step_history.is_empty());
        assert!((m.state.step_ms - 300.0).abs() < 1.0, "{}", m.state.step_ms);
    }

    /// The existing freeze bug: after a bar restart the cadence derivation
    /// must resume. The gap across the restart is validation work, not a step.
    #[test]
    fn a_bar_restart_records_no_sample_flags_the_bar_and_resumes_timing() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 3194, t0);
        bar_state_at(&mut m, 3195, t0 + Duration::from_millis(300));
        assert_eq!(m.state.step_history.len(), 1);
        // Validation and a checkpoint take 40 s, then the bar restarts at 1.
        bar_state_at(&mut m, 1, t0 + Duration::from_millis(40_300));
        assert_eq!(m.state.step_history.len(), 1, "no sample for the restart gap");
        assert!(m.state.chunked_bar);
        // Timing resumes straight away, far below the old step number.
        let before = m.state.step_ms;
        bar_state_at(&mut m, 2, t0 + Duration::from_millis(40_550));
        assert_eq!(m.state.step_history.len(), 2);
        assert!((m.state.step_history[1].ms - 250.0).abs() < 1.0);
        assert_ne!(m.state.step_ms, before, "the derived rate must update again");
        // seq keeps rising across the restart even though step went back.
        assert_eq!(m.state.step_history[1].seq, 2);
        assert!(m.state.step_history[1].step < m.state.step_history[0].step);
    }

    #[test]
    fn a_trainer_that_reports_its_own_step_time_is_left_alone() {
        let mut m = TrainMonitor::new();
        m.saw_reported_step_time = true;
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        bar_state_at(&mut m, 11, t0 + Duration::from_millis(300));
        assert!(m.state.step_history.is_empty());
        assert_eq!(m.state.step_time_source, StepTimeSource::Unknown);
    }

    #[test]
    fn an_implausible_gap_records_no_sample() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        bar_state_at(&mut m, 11, t0 + Duration::from_secs(300));
        assert!(m.state.step_history.is_empty());
    }

    #[test]
    fn reported_samples_carry_a_rising_seq_and_say_they_are_reported() {
        let mut st = TrainState::new();
        for i in [900u64, 901, 902] {
            st.apply_event(TrainEvent::StepAndTime {
                step: i,
                loss: 2.0,
                ms: 400.0,
                cache_entries: 8,
            });
        }
        let seqs: Vec<u64> = st.step_history.iter().map(|s| s.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(st.step_time_source, StepTimeSource::Reported);
        assert_eq!(st.step_seq, 3);
    }

    #[test]
    fn a_second_time_line_for_the_same_step_keeps_the_first_seq() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::StepAndTime { step: 5, loss: 2.0, ms: 400.0, cache_entries: 8 });
        st.apply_event(TrainEvent::StepTime { ms: 450.0, cache_entries: 8 });
        assert_eq!(st.step_history.len(), 1);
        assert_eq!(st.step_history[0].seq, 1);
        assert_eq!(st.step_seq, 1);
    }
}
```

Add `seq` to every `StepSample` literal already in the repo (`seq: step` where the literal has a `step` value that is also the intended key; in `monitor.rs`'s `StepTime` arm it is the new `self.step_seq`): `mock.rs` `state_at`, the `sample()` helper in `src/animation/train_tapestry.rs` tests, and the `sample_at` helper in `src/animation/train_view.rs` tests (`seq: step`).

In `src/animation/train_view.rs` tests add, and update the existing tests named below:

```rust
    #[test]
    fn the_title_says_when_step_times_were_observed_from_the_bar() {
        use crate::workload::train::StepTimeSource;
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 285.0, 0)).collect();
        st.step_seq = 10;
        st.step_ms = 285.0;
        st.step_time_source = StepTimeSource::Reported;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY  last 10 steps"), "{out}");
        assert!(!out.contains("from bar"), "{out}");
        st.step_time_source = StepTimeSource::Observed;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY (from bar)  last 10 steps"), "{out}");
    }

    /// A bar restart sends `st.step` back to 1. It must not look like a new
    /// run: lanes and best-so-far values keep their history.
    #[test]
    fn a_bar_restart_does_not_clear_the_chip_lanes() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=6u64).map(|i| sample_at(i, 285.0, 0)).collect();
        st.step_ms = 285.0;
        let v = TrainView::new(134, 40);
        // seq 1..=3 while the bar runs 3193..=3195, then the bar restarts.
        for (seq, bar_step) in [(1u64, 3193u64), (2, 3194), (3, 3195), (4, 1), (5, 2), (6, 3)] {
            st.step_seq = seq;
            st.step = bar_step;
            v.render(&st, &b);
        }
        let row = rows_of(&v.render(&st, &b))
            .into_iter()
            .find(|r| r.contains("chip0"))
            .expect("a lane for chip 0");
        assert_eq!(row.matches(NO_SAMPLE).count(), 0, "all six columns sampled: {row:?}");
    }

    #[test]
    fn once_the_bar_is_known_to_be_chunked_the_strip_makes_no_schedule_claim() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.loss_history = (0..120).map(|i| 5.0 - 0.01 * i as f32).collect();
        st.scheduler = Some("cosine".into());
        st.max_steps = 3195; // a chunk size, not the run's budget
        st.step = 630;
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("cosine 19% through"), "{out}");
        st.chunked_bar = true;
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("cosine"), "{out}");
        assert!(!out.contains("% through"), "{out}");
    }
```

Update the existing view tests that drive chip sampling by setting `st.step` (`chip_lanes_line_up_with_the_step_bars_and_mark_missing_samples`, `the_tokens_per_second_gauge_is_relative_to_the_runs_own_best`, and any other test that does `st.step = n` to feed `sample()`) so they set `st.step_seq` as well as, or instead of, `st.step`. The lane test's `sample_at(i, ..)` entries already carry `seq: i`.

- [ ] **Step 2: Run to confirm they fail**

Run: `cargo test --lib observed_step_tests 2>&1 | tail -20`
Expected: compile errors (`seq`, `StepTimeSource`, `step_seq`, `chunked_bar`, `record_observed_step` not found).

- [ ] **Step 3: Implement**

In `monitor.rs`:

```rust
/// Where the step history's times came from. The chart title discloses
/// `Observed`, so a time measured by polling a progress bar is never taken
/// for one the trainer printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepTimeSource {
    /// No per-step times yet.
    #[default]
    Unknown,
    /// The trainer printed the time (`Time: N ms`).
    Reported,
    /// Timed from the gap between progress-bar updates, to within one poll
    /// interval.
    Observed,
}
```

Add `pub seq: u64,` to `StepSample` after `step`, with this doc: "The run's own sequence number for this sample. It only rises within a run, whatever the trainer's step counter does (a progress-bar harness restarts its bar every chunk), so it is the key chip samples are joined on. `step` stays the trainer's own number."

Add to `TrainState` (after `step_history`):

```rust
    /// Samples recorded this run. Rises by one per new sample, so it never goes
    /// backwards when a progress bar restarts. Chip lanes are keyed on it.
    pub step_seq: u64,
    /// Where `step_history`'s times came from.
    pub step_time_source: StepTimeSource,
    /// A step regression was seen, so the bar counts a chunk and its total is
    /// a chunk size, not the run's step budget.
    pub chunked_bar: bool,
```

In the `StepTime` arm of `apply_event`, set `self.step_time_source = StepTimeSource::Reported;`, and when a new sample is pushed (not on the same-step replace) do `self.step_seq += 1;` and put `seq: self.step_seq` in the pushed sample (the replace branch keeps `last.seq`; extend its struct update to `seq: last.seq`). Make the literal `StepSample { step: self.step, seq: self.step_seq + 1, ... }` consistent with that rule.

Add:

```rust
    /// Record a step timed by watching a progress bar. `ms` is the gap between
    /// the two polls that saw `step - 1` and `step`.
    pub fn record_observed_step(&mut self, step: u64, ms: f32) {
        self.step_seq += 1;
        self.step_time_source = StepTimeSource::Observed;
        self.step_history.push(StepSample {
            step,
            seq: self.step_seq,
            ms,
            cache_delta: 0,
            checkpoint: false,
        });
        if self.step_history.len() > STEP_HISTORY {
            self.step_history.remove(0);
        }
    }
```

Rewrite `note_step_progress`, keeping its existing doc comment and extending it with the three behaviours above:

```rust
    fn note_step_progress(&mut self, now: Instant) {
        if self.saw_reported_step_time {
            return;
        }
        let step = self.state.step;
        match self.last_step_seen {
            // The counter went backwards: a progress-bar harness starts a new
            // bar for every chunk. Re-anchor without measuring. The gap to this
            // step spans the validation and checkpoint work between chunks, and
            // is not a step. Without this the derivation froze until the bar
            // passed its old position.
            Some((prev_step, _)) if step < prev_step => {
                self.state.chunked_bar = true;
                self.last_step_seen = Some((step, now));
                return;
            }
            Some((prev_step, prev_at)) if step > prev_step => {
                let dt_ms = now.duration_since(prev_at).as_secs_f32() * 1000.0;
                let dsteps = (step - prev_step) as f32;
                let per_step = dt_ms / dsteps;
                // (keep the existing comment on the plausible range here)
                if (1.0..=120_000.0).contains(&per_step) {
                    // (keep the existing smoothing code and its comment)
                    // Exactly one step since the last poll: the gap is that
                    // step's own time, to within one poll interval. More than
                    // one is an average, which is not recorded as a sample.
                    if step - prev_step == 1 {
                        self.state.record_observed_step(step, per_step);
                    }
                }
            }
            _ => {}
        }
        if self.last_step_seen.map(|(s, _)| step > s).unwrap_or(true) {
            self.last_step_seen = Some((step, now));
        }
    }
```

Re-export `StepTimeSource` from `src/workload/train/mod.rs`.

In `mock.rs` `state_at`: set `st.step_seq = step;`, `st.step_time_source = StepTimeSource::Reported;` and `seq: s` in each sample (the mock's step numbers are already monotonic, so `seq` equals `step`).

In `train_view.rs`:
- `sample()`: `h.record(st.step_seq, &chips);`
- lanes: `hist_chips.sample_at(dev.index, s.seq)`.
- header: the title is `STEP ANATOMY (from bar)` when `st.step_time_source == StepTimeSource::Observed`, and `STEP ANATOMY` otherwise. The title is never dropped. Keep the pulse clause atomic as it is now; with the longer title it may be omitted entirely at 134 columns when it does not fit, which is acceptable (an omitted clause states nothing false). The no-history header text is unchanged.
- strip: pass `if st.chunked_bar { 0 } else { st.max_steps }` as the `max_steps` argument of `convergence_parts`.

- [ ] **Step 4: Run**

Run: `cargo test --lib workload::train 2>&1 | tail -15` then `cargo test --lib animation 2>&1 | tail -15`
Expected: all pass.

- [ ] **Step 5: Make the new tests prove themselves**

1. In `note_step_progress`, delete the `Some((prev_step, _)) if step < prev_step => {...}` arm. Run `cargo test --lib a_bar_restart_records_no_sample`. Expected: FAIL (the freeze bug returns). Restore.
2. Change `if step - prev_step == 1 {` to `if step > prev_step {`. Run `cargo test --lib several_steps_in_one_poll_record_no_sample`. Expected: FAIL. Restore.
3. Change `h.record(st.step_seq, &chips);` back to `h.record(st.step, &chips);` in `sample()`. Run `cargo test --lib a_bar_restart_does_not_clear_the_chip_lanes`. Expected: FAIL. Restore.
4. Change the strip's `if st.chunked_bar { 0 } else { st.max_steps }` to `st.max_steps`. Run `cargo test --lib once_the_bar_is_known_to_be_chunked`. Expected: FAIL. Restore.

Re-run `cargo test --lib animation` and `cargo test --lib workload::train` and confirm green.

- [ ] **Step 6: Commit**

```bash
git add src/workload/train/ src/animation/train_view.rs src/animation/train_tapestry.rs
git commit -m "feat: time steps from progress-bar updates and survive per-chunk bar restarts"
```

---

### Task 6: Docs, spec amendments, version and build

**Files:**
- Modify: `docs/superpowers/specs/2026-10-01-training-tapestry-design.md`
- Modify: `Cargo.toml` (line 3), `debian/changelog` (new top entry), `README.md` (line 441), `AGENTS.md` (append a dated entry)

- [ ] **Step 1: Amend the spec to match what was built**

Apply these edits to the spec so it states what the code does:

1. In "New state", rename `checkpoint_near` to `checkpoint`, and state that only trainer-reported step times become samples. A trainer that prints no per-step time has no history and the band header reads `STEP ANATOMY  no per-step times reported`.
2. In "Layer C", change "loss slope per 100 steps" to "loss slope per 100 logged losses", and add: the unit is logged losses, because a trainer that prints every tenth step has ten steps per entry. State that `lr` is the configured base rate and the schedule position is the fraction of the step budget completed.
3. In "Layer A", add: lane power is scaled to the chip's TDP, or to the window maximum when no TDP is known. A column with no chip sample shows `⋅`. A lane cell is coral where aiclk is below 90% of that chip's highest. Chip samples are taken for all chips the backend reports. Lanes show the first three.
4. In "Layer B", add: "chip" in the gauges and verdict means the busiest chip (most power), because the trainer's own chips are not identified. The tok/s and PCIe bars are relative to the best value seen. The aiclk bar is relative to the highest aiclk seen on that chip. The power gauge and the power-based verdicts are absent when no chip reports a TDP.
5. In "Layer D", add: the cursor never runs faster than one pass per 0.2 s. When it is slowed, the header says `pulse = 1 step (drawn at 5/s max)`.
6. In "New state", replace "`--mock` emits every signal above" with: `--mock` emits step history, loss, config and chip telemetry. It has no PCIe gauge, because the mock backend has no PCIe counters and adding them would change the Insights sidebar that shares it.
7. In "New state", add the bar-observed path: for trainers that print only a progress bar, step times are the gap between the polls that saw consecutive steps, recorded only when exactly one step was seen (accurate to within one poll interval), never for the gap across a bar restart, with `cache_delta` unknown (0). The title reads `STEP ANATOMY (from bar)`. `StepSample.seq` is the sample sequence number and keys the chip lanes. `TrainState.chunked_bar` marks a bar that restarts per chunk, and the strip then names the scheduler without a percentage.
8. Set the spec's `Status:` line to `implemented in v0.13.6`.

- [ ] **Step 2: Bump the version and log the release**

Change `version = "0.13.5"` to `version = "0.13.6"` in `Cargo.toml`.

Add this entry at the top of `debian/changelog`. Reuse the maintainer line from the existing top entry by running `grep -m1 '^ -- ' debian/changelog` and copying its name and email, with the date `Thu, 01 Oct 2026 12:00:00 +0000`:

```
tt-toplike (0.13.6) noble; urgency=medium

  * Training view: the top-left band no longer draws a decorative 6x6
    node grid. It now shows the step-time chart (bars coloured by cause:
    normal, program-cache compile, checkpoint), per-chip power lanes on
    the same time axis, hardware gauges with a one-line diagnosis
    (compiling, compute-bound, host-bound), and a loss-convergence strip.
    The pulse cursor makes one pass per measured step. Layers whose
    signal is missing are left out. The MODEL card keeps the block and
    head counts.

 -- <name and email copied from the previous entry>  Thu, 01 Oct 2026 12:00:00 +0000
```

If `CHANGELOG.md` lists the most recent releases in a summary table or list, add a matching `0.13.6` line in the same format.

- [ ] **Step 3: Update the README description**

Replace the sentence at `README.md:441` that begins "The model is drawn as the network it is" through "with amber sweeps for the forward pass and violet sweeps for backward/gradients." with:

```
The top-left band shows how each step went and what the hardware did about it: a bar per step (teal normal, purple when the program cache grew, amber at a checkpoint), a power lane per chip on the same time axis, gauges for tokens/sec, power, aiclk and PCIe with a one-line diagnosis, and a strip of loss slope, noise and schedule position. The pulse across the grid makes one pass per measured step.
```

Keep the rest of that paragraph (the mountains, the aurora and the comet).

- [ ] **Step 4: Log the session in `AGENTS.md`**

This repo has no `CLAUDE.md`; its session log is `AGENTS.md`. Run `grep -n '^## Phase' AGENTS.md | tail -3` to find the last phase heading, then append a new section after the end of the file in the same style:

```
## Phase NN: Training tapestry (Oct 1, 2026, v0.13.6)

Prompt: the Training View's top-left block-head scanner was "not the right
usage of space or signal"; asked for three alternatives rooted in hardware
and training performance data. Chosen: all of them together, with the old
grid kept as a backdrop.

Finding that drove the design: the grid's sweep was driven by the frame
counter and every node took the same loss hue, so none of it was data, and
tt-train logs no per-block or per-head signal. Any per-node mapping would
have been invented.

Decisions: per-step history only from trainer-reported times (derived
cadence is an average and would fake resolution); "chip" in gauges and the
verdict means the busiest chip because the trainer's chips are not
identified; MockBackend left alone (its telemetry feeds the Insights
sidebar tests), so --mock has no PCIe gauge; slope is per 100 logged
losses, not steps; pulse capped at 5 passes/s so it stays visible.

Process: brainstorming -> spec (docs/superpowers/specs/2026-10-01-
training-tapestry-design.md) -> plan (docs/superpowers/plans/2026-10-01-
training-tapestry.md).
```

Replace `NN` with the next phase number.

- [ ] **Step 5: Full verification**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings 2>&1 | tail -20
cargo test 2>&1 | tail -15
cargo build --release 2>&1 | tail -5
```

Expected: formatting clean, no clippy warnings, the full suite green, a release build. If `cargo fmt --check` reports differences in the files this plan touched, run `cargo fmt` and re-check. If clippy reports an existing warning outside the touched files, report it and do not fix it here.

- [ ] **Step 6: Install the binaries**

```bash
cp target/release/tt-toplike{,-tui} ~/.local/bin/
which -a tt-toplike
```

Expected: `~/.local/bin/tt-toplike` is first on the path.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock debian/changelog CHANGELOG.md README.md AGENTS.md docs/superpowers/
git commit -m "v0.13.6: Training view tapestry band (step bars, chip lanes, gauges, diagnosis, convergence)"
```

---

## Self-review

**Spec coverage**

| Spec section | Task |
|---|---|
| Layer A step bars, causes, median line, chip lanes | 2 (`bar_cell`, `cause_of`, `median`, `ChipHistory`), 3 (drawing) |
| Layer B gauges and diagnosis | 2 (`diagnose`), 4 |
| Layer C convergence strip | 2 (`convergence_parts`), 4 |
| Layer D grid backdrop and cursor | 2 (`pass_secs`, `pass_fraction`), 3 |
| New state (`step_history`, chip samples) | 1, 2 |
| Mock coverage | 1 |
| Missing signals | 3 (no lanes), 4 (no gauges), tests for Review Focus 1 and 2 |
| Shrinking order | 2 (`plan_band` and its tests) |
| Testing, including red-run proofs and the rewritten topology test | every task, 3 |
| Housekeeping (version, log, install) | 5 |

**Type consistency.** `StepSample { step, ms, cache_delta, checkpoint }` is defined in Task 1 and used with those fields in Tasks 2, 3 and 4. `plan_band(height, BandWants) -> BandPlan` and `BandWants::default()` match between Task 2 and Tasks 3 and 4. `ChipHistory::{record, sample_at, aiclk_max, note_tps, best_tps, note_pcie, best_pcie_bps}` are defined in Task 2 and used in Tasks 3 and 4 under the same names. `diagnose`, `Readings`, `DiagnosisKind` replace the spec's "verdict" naming consistently in code.

**Known differences from the spec, all recorded in Task 5 Step 1:** the `checkpoint` field name, slope unit, TDP fallback for lane scale, busiest-chip meaning, pulse speed cap, and no PCIe gauge in `--mock`.

---

### Task 7: Read a step time that has no cache count

Added after the tt-tnt follow-up. The tt-tnt harness (branch `dazzle-me/tt-train` in the tt-tnt repo) now writes one `Step: {absolute step}, Loss: {loss:.4f}, Time: {ms:.1f} ms` line per step when its output is a log. It has no program-cache count to report, and must not invent one. `parse_train_line` only returns a time when the line also carries `cache entries: N`, so today that line parses as a plain `Step` and the time is dropped.

**Files:**
- Modify: `src/workload/train/parse.rs` (`TrainEvent`, the `Step:` branch of `parse_train_line`, tests)
- Modify: `src/workload/train/monitor.rs` (`apply_event`, the reported-time check in `poll`, tests)
- Modify: `src/animation/train_view.rs` (`draw_live_stats` cache row, tests)
- Modify: any other `match` on `TrainEvent` the compiler flags (adding a variant breaks exhaustive matches, including in tests)

**Interfaces:**
- Consumes: Task 1/5 `TrainState::apply_event`, the `StepTime` arm, `StepSample`, `StepTimeSource`, `step_seq`.
- Produces:
  - `TrainEvent::StepAndMs { step: u64, loss: f32, ms: f32 }`: a step line with a time and no cache count.
  - `TrainState` records it as a trainer-reported sample with `cache_delta: 0` (growth unknown) and leaves `cache_entries` untouched.
  - `fn is_reported_step_time(ev: &TrainEvent) -> bool` in `monitor.rs`: true for `StepTime`, `StepAndTime` and `StepAndMs`; `poll` uses it to set `saw_reported_step_time`.

**Design rules:**
- A trainer-printed time is reported, whether or not a cache count came with it: `step_time_source` is `Reported`, the title has no `(from bar)`, and `saw_reported_step_time` turns off the derived and bar-observed paths.
- No cache count means growth is unknown. The sample carries `cache_delta: 0` and `cache_entries` keeps its value, so no bar is coloured as a compile and the LIVE panel is not told the cache holds 0 entries.
- The LIVE panel shows its cache row only when a cache count has been reported (`cache_entries > 0`). Before this task a bar-only or Time-only trainer showed `cache 0 steady`, which states a count nobody reported.

- [ ] **Step 1: Write the failing tests**

In `src/workload/train/parse.rs` tests:

```rust
    #[test]
    fn a_step_line_with_a_time_and_no_cache_count_carries_the_time() {
        let ev = parse_train_line("Step: 25565, Loss: 3.1367, Time: 285.0 ms").unwrap();
        assert_eq!(
            ev,
            TrainEvent::StepAndMs { step: 25565, loss: 3.1367, ms: 285.0 }
        );
    }

    #[test]
    fn the_other_step_shapes_are_unchanged() {
        assert!(matches!(
            parse_train_line("Step: 2431, Loss: 1.8342, Time: 1124.5 ms, cache entries: 21"),
            Some(TrainEvent::StepAndTime { step: 2431, cache_entries: 21, .. })
        ));
        assert!(matches!(
            parse_train_line("Step: 7 Loss: 0.4213"),
            Some(TrainEvent::Step { step: 7, .. })
        ));
        // A time that does not parse is dropped, not guessed.
        assert!(matches!(
            parse_train_line("Step: 8, Loss: 0.5, Time: soon ms"),
            Some(TrainEvent::Step { step: 8, .. })
        ));
        // A cache count with no time is still a plain step.
        assert!(matches!(
            parse_train_line("Step: 9, Loss: 0.5, cache entries: 4"),
            Some(TrainEvent::Step { step: 9, .. })
        ));
    }
```

In `src/workload/train/monitor.rs` add a test module `step_and_ms_tests`:

```rust
#[cfg(test)]
mod step_and_ms_tests {
    use super::*;

    fn line(step: u64, ms: f32) -> TrainEvent {
        TrainEvent::StepAndMs { step, loss: 2.0, ms }
    }

    #[test]
    fn a_time_with_no_cache_count_is_a_reported_sample_with_unknown_growth() {
        let mut st = TrainState::new();
        st.cache_entries = 7; // an earlier reading must survive
        st.apply_event(line(100, 285.0));
        st.apply_event(line(101, 290.0));
        assert_eq!(st.step, 101);
        assert_eq!(st.step_ms, 290.0);
        assert_eq!(st.cache_entries, 7, "no cache count was reported");
        assert_eq!(st.step_time_source, StepTimeSource::Reported);
        let got: Vec<(u64, u64, f32, u32)> = st
            .step_history
            .iter()
            .map(|s| (s.step, s.seq, s.ms, s.cache_delta))
            .collect();
        assert_eq!(got, vec![(100, 1, 285.0, 0), (101, 2, 290.0, 0)]);
    }

    #[test]
    fn a_trainer_that_prints_time_and_cache_still_measures_growth() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::StepAndTime { step: 1, loss: 2.0, ms: 300.0, cache_entries: 8 });
        st.apply_event(TrainEvent::StepAndTime { step: 2, loss: 2.0, ms: 300.0, cache_entries: 12 });
        assert_eq!(st.step_history[1].cache_delta, 4);
    }

    #[test]
    fn every_step_time_shape_counts_as_a_reported_time() {
        assert!(is_reported_step_time(&line(1, 1.0)));
        assert!(is_reported_step_time(&TrainEvent::StepTime { ms: 1.0, cache_entries: 1 }));
        assert!(is_reported_step_time(&TrainEvent::StepAndTime {
            step: 1, loss: 1.0, ms: 1.0, cache_entries: 1
        }));
        assert!(!is_reported_step_time(&TrainEvent::Step { step: 1, loss: 1.0 }));
        assert!(!is_reported_step_time(&TrainEvent::MaxSteps(5)));
    }
}
```

In `src/animation/train_view.rs` tests:

```rust
    #[test]
    fn the_live_panel_shows_a_cache_row_only_when_a_cache_count_was_reported() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step = 50;
        st.cache_entries = 0;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(!out.contains("cache "), "no count reported, no row:\n{out}");
        st.cache_entries = 21;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("cache   21"), "{out}");
    }
```

Run: `cargo test --lib parse:: 2>&1 | tail -15`
Expected: compile errors (`StepAndMs`, `is_reported_step_time` not found).

- [ ] **Step 2: Implement**

`parse.rs`: add to `TrainEvent`:

```rust
    /// A step line with a wall time and no program-cache count. A trainer
    /// that times its own steps but has no cache to report (the tt-tnt
    /// harness, which drives ttml from Python) prints this shape.
    StepAndMs {
        step: u64,
        loss: f32,
        ms: f32,
    },
```
and in `parse_train_line` change the final match to:

```rust
        return Some(match (ms, cache) {
            (Some(ms), Some(cache_entries)) => TrainEvent::StepAndTime { step, loss, ms, cache_entries },
            (Some(ms), None) => TrainEvent::StepAndMs { step, loss, ms },
            _ => TrainEvent::Step { step, loss },
        });
```
Update the doc comment above it to list the new shape (`Step: 25565, Loss: 3.1367, Time: 285.0 ms`) and the module header comment if it enumerates shapes.

`monitor.rs`: refactor the `StepTime` arm body into a helper `fn note_reported_time(&mut self, ms: f32, cache_entries: Option<u32>)`:
- with `Some(n)`: exactly today's behaviour (growth against the previous count, then update `cache_entries`);
- with `None`: `cache_delta = 0`, `cache_entries` untouched;
- both: set `step_ms`, `step_time_source = Reported`, push or replace the sample with the existing same-step and `seq` rules.
`StepTime { ms, cache_entries }` calls `note_reported_time(ms, Some(cache_entries))`. Add the arm `TrainEvent::StepAndMs { step, loss, ms } => { self.apply_event(TrainEvent::Step { step, loss }); self.note_reported_time(ms, None); }`. Add `fn is_reported_step_time(ev: &TrainEvent) -> bool` (module level, `matches!` on the three variants) and use it in `poll` where the `StepTime | StepAndTime` arm sets `saw_reported_step_time`, so that arm covers `StepAndMs` too.

`train_view.rs` `draw_live_stats`: wrap the cache row in `if st.cache_entries > 0 { ... }`. Keep the climbing/steady tracking (`cache_last`, `cache_steady_ticks`) updating as before so a count that appears later starts from the right baseline: update the cells whether or not the row is drawn. Update the comment to say why the row needs a reported count.

Update every other `match` on `TrainEvent` the compiler flags (grep for `TrainEvent::StepAndTime` to find them) and the module docs in `parse.rs` and the `monitor.rs` doc for `saw_reported_step_time` if they list the shapes.

- [ ] **Step 3: Run**

Run: `cargo test --lib workload::train 2>&1 | tail -15` then `cargo test --lib animation 2>&1 | tail -15`
Expected: all pass. If an existing test asserted `cache 0 steady` or a cache row with no count, update it minimally and list it in the report.

- [ ] **Step 4: Make the new tests prove themselves**

1. In `parse_train_line` change `(Some(ms), None) => TrainEvent::StepAndMs { .. }` to fall through to `Step`. Run `cargo test --lib a_step_line_with_a_time_and_no_cache_count`. Expected: FAIL. Restore.
2. In `note_reported_time` make the `None` case set `cache_entries = 0`. Run `cargo test --lib a_time_with_no_cache_count_is_a_reported_sample`. Expected: FAIL. Restore.
3. Remove `StepAndMs` from `is_reported_step_time`. Run `cargo test --lib every_step_time_shape_counts`. Expected: FAIL. Restore.
4. Remove the `if st.cache_entries > 0` guard. Run `cargo test --lib the_live_panel_shows_a_cache_row_only`. Expected: FAIL. Restore.

Re-run both suites and confirm green.

- [ ] **Step 5: Docs**

Update `docs/superpowers/specs/2026-10-01-training-tapestry-design.md`: in the bar-observed section add that a step line with a time and no cache count (`Step: N, Loss: L, Time: T ms`) is a trainer-reported time with unknown cache growth, and that the LIVE cache row needs a reported count. Add a short line to the `AGENTS.md` Phase entry for this release: the tt-tnt harness change on branch `dazzle-me/tt-train` in the tt-tnt repo now prints that line, and tt-toplike reads it. Update the `debian/changelog` 0.13.6 entry with one bullet (do not change the version). Apply the CLAUDE.md prose rules.

- [ ] **Step 6: Commit**

```bash
git add src/workload/train/ src/animation/train_view.rs docs/superpowers/specs/ AGENTS.md debian/changelog
git commit -m "feat: read a trainer-reported step time that has no cache count"
```
