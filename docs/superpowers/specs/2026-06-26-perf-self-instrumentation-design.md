# Perf self-instrumentation (measure render cost + live self-CPU)

**Status:** Design — awaiting review
**Branch:** `macos-cpu-mode`
**Date:** 2026-06-26

## Problem

We suspect the animated screens cost meaningful CPU (the Insights screen was
redrawing at ~60 FPS until just fixed), and on `--host`/TT the tool can starve
the very workload it monitors. But we have **no measured per-screen numbers** —
and the interactive TUI can't be measured headlessly (it needs a live terminal,
so it exits without interactive stdin in CI/sandbox). We're guessing.

## Goal

Replace guesses with measurement, via two complementary pieces:

- **A — headless render benchmark:** render each display mode's visualization
  into an off-screen buffer for N frames and report avg/p95 frame time + an
  estimated CPU cost. No TTY required → runs in CI/sandbox and on any machine.
- **B — live self-CPU/FPS readout:** show the tool's actual FPS and its own
  process CPU% in the status bar (toggleable), so the real observer-effect cost
  is visible under real conditions.

Build A first (it unblocks measurement/tuning); then B.

## Component A — headless render benchmark

### Behavior
A new run mode (CLI flag `--bench`) that does NOT enter raw mode / the TUI. For
each display mode (or a `--mode`-selected one), it:
1. Builds the chosen backend (`--mock N` default, or `--host`) and a fresh
   visualization sized to a fixed `TestBackend` (default 120×40).
2. Runs N frames (default 120): each frame advances the viz's `update`/tick with
   the backend, then renders via the existing `ui_*` / `render_insights` /
   `render_grid_mode` fn into the `TestBackend`'s `Terminal`, timing the
   render+tick with `std::time::Instant`.
3. Reports a table to stdout: `mode, frames, avg_ms, p95_ms, target_fps,
   est_cpu_pct` where `est_cpu_pct = avg_ms * target_fps / 10.0` (% of one core).

Target FPS per mode matches the run loop: data modes (Insights, Grid) = 10;
animated modes (Starfield, Memory Castle, Memory Flow, Arcade) = 60; Defrag = 60.

### Why TestBackend
`ratatui::backend::TestBackend` renders into an in-memory buffer with no terminal
— deterministic, no raw mode, no TTY. The same harness the isolation tests
already use. This is the crux that makes headless measurement possible.

### Placement
A `run_bench(cli)` fn invoked from `src/bin/tui.rs` when `cli.bench` is set,
before any TUI/backend-TTY path. It reuses the public viz constructors and the
existing `ui_*`/`render_*` fns from `src/ui/tui`. To call those render fns from
the bench, expose a small `pub(crate)` entry in `src/ui/tui/mod.rs`
(`pub(crate) fn bench_render_frame(mode, f, &viz_state, backend)`) that maps a
mode to its render call — avoids duplicating the match arm.

### Output is advisory, not asserted
Frame times are machine- and load-dependent. The bench prints numbers for humans
to read; tests assert the harness runs N frames per mode without panicking and
emits a row per mode — not specific timings.

## Component B — live self-CPU / FPS readout

### Behavior
A `PerfMeter` owned by the TUI run loop:
- **FPS:** counts actual `terminal.draw` calls and divides by elapsed wall-time
  over a sliding ~1s window → displayed FPS reflects the real (post-throttle)
  draw rate.
- **Self CPU%:** a `sysinfo::System` refreshing only our own PID
  (`sysinfo::get_current_pid()`) ~once/sec; reports `process.cpu_usage()` (% of
  one core, sysinfo's convention).
- **Frame time:** the most recent render+tick duration (already available if we
  time the gated draw).

### Display
Rendered compactly at the right of the global status bar, e.g.
`9.8fps · 0.4ms · self 1.3%`. **Off by default**, toggled by the `P` hotkey
(mnemonic: perf). When off, zero added cost (don't sample sysinfo).

### Self-CPU caveat
`sysinfo` process CPU% needs two samples separated in time; the first reading
after enabling is `0.0` until the second refresh. Acceptable — it settles within
~1–2s. The number is the tool's own usage, the honest observer-effect cost.

## Data flow

```
A (bench):  --bench → run_bench → for each mode { build viz+backend;
            loop N: tick+render into TestBackend, time it } → print table
B (live):   run loop → time the gated draw; PerfMeter.tick(frame_dur);
            every ~1s refresh own-PID cpu% → status bar (when toggled on)
```

## Error handling / cost
- A: a mode that errors/panics is caught per-mode (report "n/a") so one bad mode
  doesn't abort the whole table. No raw mode, no TTY.
- B: if `get_current_pid()` or the refresh fails, show `self —` (no CPU figure);
  never panic. When toggled off, the sysinfo system isn't refreshed → no cost.

## Testing
- A: unit/integration test that `run_bench`-equivalent renders ≥1 frame per mode
  into a `TestBackend` without panicking and produces one result row per mode.
- B: unit-test `PerfMeter`'s FPS math (frames/elapsed) and the formatting with
  injected frame counts/durations and a stubbed CPU value.
- Existing suite stays green; new code is cross-platform (sysinfo + TestBackend).

## Out of scope
- Per-mode auto-tuning / adaptive FPS (that's items #1–#4 from the perf
  discussion — this spec only *measures*).
- GPU/accelerator-side cost (this measures the monitor's own host cost).
- Historical perf logging / export beyond the one-shot bench table.

## Effort & risk
~1.5–3 days. Low risk: A is additive and self-contained (TestBackend harness, no
TUI changes beyond exposing one render entry point); B adds a small meter + a
status-bar string + a hotkey. No new dependencies (ratatui TestBackend + sysinfo
both already present).
