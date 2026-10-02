# Training View tapestry: replace the node-grid scanner with data-driven layers

Date: 2026-10-01 (revised 2026-10-02)
Status: implemented in v0.13.6; starfield and signal weave in v0.13.7
Files: `src/animation/train_view.rs`, `src/animation/train_tapestry.rs`, `src/animation/train_canvas.rs`, `src/workload/train/monitor.rs`, `src/workload/train/mock.rs`

## Revision 2026-10-02: starfield over a signal weave

The user on v0.13.6: "the pulse doesn't make efficient use of the space the
way the old knots / stars did. rethink that area of the viz again". They chose
"Starfield over a signal weave". The step bars, the pulse row and the four
gauge rows are replaced:

- The step bars became a starfield (Layer A below). A bar took a whole
  column per step. Braille dots put two steps in each column, so the same
  width shows twice the steps, and the starfield grows to six rows where the
  bars had four.
- The pulse row (Layer D) is removed. It spent a full row on a cursor whose
  only information was the step rate. The step rate now drives a swell on the
  newest three stars, which uses no row of its own.
- The gauge rows (Layer B) are removed. Each gauge showed one current value.
  The weave rows show the same signals over the same steps as the stars, and
  each still ends in its current value. tok/s stays in the LIVE panel.

The sections below describe the band as it is now.

## Problem

The top-left band of the Training View draws a 6x6 node grid with a sweeping
pulse (`draw_network`). None of it comes from data. The sweep position depends
only on the frame counter. Every node takes the same colour, the current loss.
tt-train logs no per-block or per-head signal, so mapping data onto individual
nodes would be invented. This tool's rule is that every pixel maps to a real
signal, and the grid breaks it. It also uses up to 13 rows of the most visible
space to say little about how the run is going.

## Goal

Show how and why a step took the time it did, how hard the hardware is working,
and whether the run is still learning. Every cell is drawn from a signal we
already read. A signal that is missing is left out of the picture.

## Approved layout

The band stays between the MODEL card and the LIVE panel. It is at most 13
rows tall (half the content area, capped, so the river keeps the larger
share). Top to bottom it holds:

1. Header: `STEP ANATOMY  last N steps · median M ms`.
2. Starfield of step times, up to 6 rows, newest at the right (Layer A).
3. One power row per chip on the same columns (Layer A).
4. Aux weave rows: aiclk, PCIe, host CPU (Layer B).
5. Verdict line (Layer B).
6. Convergence strip (Layer C).

This frame is copied from a 134x40 render of `--mock` (two mock chips, after
consecutive steps). The mock backend has no PCIe counters, so it has no
`pcie` row. A backend with counters adds one between `aiclk` and `host`,
ending in its throughput (`310 MB/s`).

```
STEP ANATOMY  last 112 steps · median 83 ms
  173           ◆  ◆  ◆              ◆  ◆  ◆
             ◆           ◆        ◆
       ◆  ◆                 ◆  ◆

      ⢄┈⡠⠢⡀⢀⠔⠄┈⡠⠂⡀⢀⠄⢄┈⡀⠢⡀┈⠔⠄┈⡠⠢┈⢀⠒⠄┈⡐⠂┈⢀⠂⠄┈⡀⠢┈⢀⠒⠄┈⡐⠢┈⢀⠒⠄┈⡐⠢┈⢀⠒ median
   64  ⠂   ⠂  ⠒  ⠐⠂  ⠒  ⠐⠂ ⠈⠂  ⠁⠂  ⠒  ⠑⠂ ⠈⠒  ⠑⠂ ⠈⠒  ⠑⠂ ✺⠒  ⠑⠂
chip0 ⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅▃▃▃▃▃▃▃▃▃▃▃▃▃▃▃▃▃▃▃▂▂▂▂▂▂▂▂▂▂▃▃▃▃▃▃▃▃ 37% TDP
chip1 ⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▃▃▃▃▃▃▃▃▃▃▃▃▃▄▄▄▄▄▄ 52% TDP
aiclk ⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅███████▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇████████████ 1003 MHz
host  ⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅⋅███████▇▇▇▇▇▇▇▇▇▇▇▆▆▆▆▆▆▇▇▇▆▆▆▆▆▇▇▇▇▇ cpu 136%
▸ compute-bound - busiest chip at 52% of TDP, host cpu 136%
loss ↘ -0.367/100 logs  noise 0.029  best 6 logs ago  base lr 3.0e-4
```

The `⋅` cells at the left of the weave rows are steps taken before the view
first saw them, so no chip reading exists for them. The `┈` horizon shows
on the median's row wherever no star is drawn. The fourth star row is blank
in this frame because no shown step fell in its time range.

With three chips and all three aux rows, a 13-row band leaves 4 star rows
when a verdict is shown and 5 without one. The starfield reaches 6 rows when
fewer weave rows are present. The band is capped at 13 rows so the river
keeps the larger share of the screen. At a 30-row terminal the band is 10
rows: with three chips, two aux rows and a verdict the starfield gets 3 rows
and the strip is left out.

Columns: the first 6 hold row labels and the starfield's axis values. One
column is left free at the right. When at least 20 data columns remain
beside it, a 10-column value label (after a one-column gap) ends each weave
row. Otherwise the value labels are left out whole and the data takes their
columns. A value that does not fit whole is left out.

### Layer A: step anatomy

- Starfield: one star per step on a braille canvas. A cell holds 2 dots across
  and 4 down, so two steps share a column. Steps are right-aligned, so the
  newest is in the last data column; steps older than the canvas are dropped.
  The header's `last N steps` counts the steps shown.
- y is the step time, auto-ranged over the shown steps (`y_range`: 10% of the
  span below the smallest, 5% above the largest). The top and bottom of the
  range are printed in the label column (`{:>5.0}` ms).
- Star glyph by cause:
  - teal braille dot: normal step;
  - purple `◆`: the program-cache count rose on this step (a compile);
  - amber `✺`: a checkpoint was written within one tick of this step.
  A cell holding both a compile and a checkpoint draws `◆`.
- A dotted `┈` horizon marks the median of the shown steps. It is drawn on
  every column of its row that has no star in it, and the value column says
  `median`. The axis range, the horizon and the header's median come from
  the same shown steps.
- The newest three stars swell once per measured step: their colour lifts
  towards white and back. Only the brightness changes. The swell's phase
  accumulates frame by frame, so a change in step time changes its speed and
  never makes it jump. A cycle never runs faster than 0.2 s. With no step
  time nothing swells.
- Chip rows use the same columns, with the larger of a column's two readings.
  Each shows power as a fraction of the chip's TDP, ending in the current
  value (`58% TDP`, or watts with no TDP). Readings are sampled when each
  step line arrived.
- Chip power is scaled to the chip's TDP, or to the window maximum when no TDP
  is known. A column with no chip sample shows `⋅`. A reading with no height
  (a zero scale or a zero reading) shows `▁`. A chip cell is coral where aiclk
  is below 90% of that chip's highest. Chip samples are taken for all chips
  the backend reports. The band shows the first three.

### Layer B: aux weave rows and verdict

Up to three rows on the starfield's columns, each ending in its current value:

- `aiclk`: the busiest chip's aiclk against the highest value observed for
  that chip, coral where it dropped below 90% (`1086 MHz`);
- `pcie`: summed PCIe throughput against the best seen, only when PCIe
  counters exist (`310 MB/s`);
- `host`: host CPU of the trainer process against the window maximum, only
  when a host reading exists (`cpu 140%`).

With no step samples the aux rows have no cells. They still draw their label
and current value when the value column is shown.

The verdict line applies plain thresholds and names the readings that
triggered it:

| Condition | Verdict |
|---|---|
| Cache count rose on the latest step | `compiling` |
| Chip power above 50% of TDP | `compute-bound` |
| Chip power below 30% of TDP and host CPU above 100% | `host-bound` |
| Anything else | no verdict line |

The 50% and 30% values are first guesses. They are constants to tune on a real
run.

"Chip" in the aiclk row and the verdict means the busiest chip (most power),
because the trainer's own chips are not identified. The power-based verdicts
are absent when no chip reports a TDP.

Narrow-width rule: a clause that is shown is whole. The header and the strip
keep only the leading parts that fit. The diagnosis drops its `, host cpu N%`
suffix before it drops entirely. A value label is drawn only when it fits
whole.

### Layer C: convergence strip

One row from `loss_history`, `lr` and `scheduler`:

- loss slope per 100 logged losses (arrow and number);
- noise (standard deviation of recent loss deltas);
- steps since the best loss;
- learning-rate value and schedule position.

The unit is logged losses, because a trainer that prints every tenth step has
ten steps per entry. `lr` is the configured base rate. The schedule position is
the fraction of the step budget completed.

It uses only data every trainer provides, including bar-only trainers. When the
progress bar restarts per chunk (`TrainState.chunked_bar`), the strip names the
scheduler without a percentage.

For bar-only and chunk-jump trainers the step time is derived from log
cadence, and the first step seen after attach is only a baseline. The first
increase after it measures nothing, so a trainer that prints one line per
chunk shows no tok/s until its second chunk end after attach. The value then
spans train, checkpoint and validation time for a chunk.

### Layer D: removed

v0.13.6 kept the node grid as a dim backdrop with a cursor that made one pass
per measured step, and the header said `pulse = 1 step (max 5/s)` when the
cursor was slowed. v0.13.7 removes the row, its cursor and the header clause
(see the revision note at the top). The step rate drives the starfield's
swell instead. The `N blocks x M heads` text stays on the MODEL card.

## New state

- `TrainState.step_history`: ring of 160 (`STEP_HISTORY`) `StepSample { seq, step, ms,
  cache_delta, checkpoint }`, pushed when a step time is known. `seq` is the
  per-run sample sequence number (`TrainState.step_seq`). It keys the chip
  lanes.
- Trainer-reported step times become samples. A trainer that prints no
  per-step time and gives no bar has no history, and the band header reads
  `STEP ANATOMY  no per-step times reported`.
- Bar-observed path: a trainer that prints only a progress bar (the tt-tnt
  Python harness) gets step times from the bar. A step time is the gap between
  the polls that saw consecutive steps. It is recorded only when exactly one
  step was seen, so it is accurate to within one poll interval. It is never
  recorded for the gap across a bar restart, and never from an unparsed step 0.
  `cache_delta` is unknown (0). The title reads `STEP ANATOMY (from bar)`.
- A step line with a time and no cache count (`Step: N, Loss: L, Time: T ms`)
  is a trainer-reported time with unknown cache growth. It parses as
  `StepAndMs`, records a sample with `cache_delta` 0, leaves `cache_entries`
  as it was, and turns off the derived and bar-observed paths. The LIVE cache
  row appears only when a cache count has been reported.
- `TrainState.chunked_bar` marks a bar that restarts per chunk. The first real
  run found that the harness prints one tqdm bar per 3195-step chunk.
- The monitor's per-run anchors (`last_step_seen`, `saw_reported_step_time`,
  `last_cpu`) reset at attach and at detach.
- A resumed run appends to the same log. Its header states `steps=` for this
  process only, and its val lines carry absolute steps. The line
  `resumed from <ckpt> at step S ...; running M more steps to step E` parses as
  `Resumed` and sets the absolute budget (`max_steps` and `stated_max_steps` =
  E, `resume_start` = S). A run header that arrives when the state already
  holds run data calls `TrainState::begin_new_run`, which clears the per-run
  data and keeps the attachment, config and host cost. The monitor also clears
  `last_step_seen` and `saw_reported_step_time` then, and leaves `last_cpu`.
  The header draws `step N / M` only when N is at most M; otherwise it draws
  `step N`, and the strip makes no schedule claim.
- The derived step cadence used to freeze after a bar restart. That is fixed,
  so derived step rate and tokens/sec keep updating.
- `TrainView` chip samples: per chip power and aiclk, plus summed PCIe
  throughput and host CPU, recorded once per new step (`ChipHistory`). They
  live in a `RefCell` in the same style as `cache_last`, and are described as
  sampled when the step line arrived. The view clears them when the run's
  identity (pid and attach time) changes.
- `--mock` emits step history, loss, config, chip telemetry and a closed-form
  host CPU and RSS, so the host row appears and a screenshot at t seconds is
  reproducible. It has no PCIe row, because the mock backend has no PCIe
  counters and adding them would change the Insights sidebar that shares it.

## Missing signals

- A chip with no telemetry has no row.
- A backend with no PCIe counters has no PCIe row. A run with no host reading
  has no host row.
- With no step history the starfield and the chip rows are left out and the
  header reads `STEP ANATOMY  no per-step times reported`. The aux rows still
  draw their current values when their signals exist.
- Nothing is drawn from a placeholder value.

## Shrinking

`plan_band` hands rows out in this order, and a shorter band loses them in
reverse:

1. Header and one star row (minimum 2 rows, always kept).
2. Verdict.
3. Chip rows.
4. Star rows, up to 3 in all.
5. Aux rows.
6. Convergence strip.
7. Star rows, up to 6 in all.

The starfield is the band's main layer, so it gets 3 rows before any aux
row. With no step samples there is no star row and no chip row. When steps
exist but none has a time the canvas can place (every time is 0, negative or
not finite), there is no star row and the chip rows still draw.

Side-panel narrow-width rules (`panel_fit`) are unchanged.

## Testing

- Each layer has a test that changes its input and asserts the output cells
  change: `cache_delta` turns a star into `◆`, chip power changes a chip row,
  loss slope changes the strip, `step_ms` changes the swell's speed.
- Each new test is run against a deliberately broken implementation to confirm
  it fails, then the fix is restored.
- Existing tests stay green, including `never_emits_a_right_side_border_and_fits_the_width`
  and the side-panel tests.
- A test asserts that a missing chip, missing PCIe counter or missing step time
  leaves the matching layer out.
- The old test `network_header_never_fabricates_topology_it_cannot_source` is
  rewritten for the MODEL card, where the topology text now lives.

## Housekeeping

- Bump the version.
- Log the prompt, decisions and this design in the repo `AGENTS.md`.
- After `cargo build --release`, copy `tt-toplike` and `tt-toplike-tui` to
  `~/.local/bin`.

## Out of scope

- Per-block or per-head data. tt-train does not emit it.
- A forward/backward time split.
- A PCIe ceiling by link generation.
