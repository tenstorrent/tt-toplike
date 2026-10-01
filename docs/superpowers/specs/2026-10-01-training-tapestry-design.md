# Training View tapestry: replace the node-grid scanner with data-driven layers

Date: 2026-10-01
Status: implemented in v0.13.6
Files: `src/animation/train_view.rs`, `src/workload/train/monitor.rs`, `src/workload/train/mock.rs`

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

The band stays between the MODEL card and the LIVE panel. At full height
(13 rows) it holds, top to bottom:

1. Header: `STEP ANATOMY  last N steps · median M ms`.
2. Step-time bars, one column per step, newest at the right (Layer A).
3. Node-grid backdrop with a cursor (Layer D).
4. One power lane per chip on the same columns (Layer A).
5. Gauge rows (Layer B).
6. Verdict line (Layer B).
7. Convergence strip (Layer C).

### Layer A: step anatomy

- One bar per step, height proportional to step time, scaled to the window's
  maximum.
- Bar colour by cause:
  - teal: normal step;
  - purple: the program-cache count rose on this step (a compile);
  - amber: a checkpoint was written within one tick of this step.
- A dotted line marks the window median.
- Chip lanes use the same columns. Each shows power as a fraction of the chip's
  TDP. They are sampled when each step line arrived.
- Lane power is scaled to the chip's TDP, or to the window maximum when no TDP
  is known. A column with no chip sample shows `⋅`. A lane cell is coral where
  aiclk is below 90% of that chip's highest. Chip samples are taken for all
  chips the backend reports. Lanes show the first three.

### Layer B: roofline gauges and verdict

Four bars:

- tok/s against the run's own best so far;
- power against TDP;
- aiclk against the highest value observed for that chip;
- PCIe throughput in MB/s, with no ceiling.

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

"Chip" in the gauges and the verdict means the busiest chip (most power),
because the trainer's own chips are not identified. The tok/s and PCIe bars are
relative to the best value seen. The aiclk bar is relative to the highest aiclk
seen on that chip. The power gauge and the power-based verdicts are absent when
no chip reports a TDP.

Narrow-width rule: a clause that is shown is whole. The strip keeps only the
leading parts that fit. The diagnosis drops its `, host cpu N%` suffix before it
drops entirely. A gauge is drawn only when its whole reading fits.

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

### Layer D: node-grid backdrop

- The grid stays as a dim backdrop under the bars.
- The cursor makes one pass per measured `step_ms`, so the pulse speed is the
  real step rate.
- The backward-pass hue is removed. We do not measure a forward/backward split.
- The cursor never runs faster than one pass per 0.2 s. The pulse clause is
  atomic. When the cursor is slowed, the header says
  `pulse = 1 step (max 5/s)`.
- Grid dimensions no longer imply a topology. The `N blocks x M heads` text
  moves to the MODEL card, which already shows blocks and heads.

## New state

- `TrainState.step_history`: ring of 64 `StepSample { seq, step, ms,
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
- `TrainState.chunked_bar` marks a bar that restarts per chunk. The first real
  run found that the harness prints one tqdm bar per 3195-step chunk.
- The monitor's per-run anchors (`last_step_seen`, `saw_reported_step_time`,
  `last_cpu`) reset at attach and at detach.
- The derived step cadence used to freeze after a bar restart. That is fixed,
  so derived step rate and tokens/sec keep updating.
- `TrainView` chip samples: per chip power, aiclk and PCIe throughput, recorded
  once per new step. They live in a `RefCell` ring in the same style as
  `cache_last`, and are described as sampled when the step line arrived.
- `--mock` emits step history, loss, config and chip telemetry. It has no PCIe
  gauge, because the mock backend has no PCIe counters and adding them would
  change the Insights sidebar that shares it.

## Missing signals

- A chip with no telemetry has no lane.
- A backend with no PCIe counters has no PCIe bar.
- No `step_ms` means no bars, and the band says it is waiting for step timing.
- Nothing is drawn from a placeholder value.

## Shrinking

When the band is shorter than 13 rows, layers drop in this order:

1. Bars and header (minimum 2 rows, always kept).
2. Verdict.
3. Chip lanes.
4. Gauges.
5. Convergence strip.

Side-panel narrow-width rules (`panel_fit`) are unchanged.

## Testing

- Each layer has a test that changes its input and asserts the output cells
  change: `cache_delta` recolours a bar, chip power changes a lane, loss slope
  changes the strip, `step_ms` changes the cursor period.
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
