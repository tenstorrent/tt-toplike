# Loading boxed-snake visualization for the `[i]` view (design)

**Status:** approved design, pending spec review.
**Date:** 2026-07-02
**Sequence:** the third `[i]` Inference Server Monitor visualization. Follows the
cold-trail model starfield (`2026-07-02-cold-trail-model-starfield-design.md`,
built) and completes the view's mode trilogy. Independent of the deferred
logstalgia spec (endpoint hits, hot/serving). Its own spec → plan → build cycle.

## Problem

The `[i]` view now shows a starfield when the trail is cold and a live service
list otherwise. But the most interesting moment — a model actually coming up —
currently renders as a plain text row (`Loading`) with a couple of stats. There
is no sense of motion or progress while you wait the minutes it takes to compile
kernels and stream weights. We want a visualization of the cold→Ready journey.

## Goal

When a service is Compiling or Loading, take over the whole `[i]` view with a
**boxed-snake** progress visualization: a snake confined to chambers ("boxes")
that flows into the next box as the model advances along its journey
(compile → load → ready). The snake is **honest**: it shows a true percentage
only when we have a per-model baseline, otherwise it conveys live *activity*
(momentum) without claiming a false number, and it turns red when progress
stalls (the existing Alarm condition). The instant the service goes Ready, a
brief gold burst plays and the view hands off to the live list.

## Mode model — the `[i]` view becomes a clean tri-state

Selected fresh each frame from the live `InferenceServerMonitor` snapshot:

1. **cold** — `trail_is_cold(&snapshot)` (no service Compiling/Loading/Ready) →
   **model starfield** (existing).
2. **loading** — any service in `Phase::Compiling | Phase::Loading` →
   **boxed snake**, full-screen, featured on that service.
3. **ready-only** — something Ready but nothing loading → the **live service
   list** (existing `render_inference_monitor_view`).

Precedence is 1 → 2 → 3: loading wins over a co-present Ready service (you want
to watch the one still coming up). Off Linux the snapshot is always empty →
always cold → starfield; the snake never appears there (no inference servers to
observe), which is correct.

### Featured-service selection

`inference_panel::featured_loading(snapshot: &[ServiceState]) -> Option<&ServiceState>`
— returns the first service whose phase is `Compiling` or `Loading`, in the
snapshot's existing (stable, key-sorted) order. `None` when nothing is loading.
When more than one is loading (rare on a single box), feature the first and let
the viz show `+N more loading` as small pips; no split screen in v1.

## The snake mechanics

The journey is three boxes drawn left-to-right, each a rectangular chamber the
snake fills serpentine (row-major) up to its head:

- **compile** box — teal (`colors::SUCCESS`)
- **load** box — amber (`colors::WARNING`)
- **ready** box — gold; closed (dashed border) until Ready, then bursts open

Box state by phase:
- `Compiling` → snake in the compile box; load & ready boxes closed/dashed.
- `Loading` → compile box locked full; wall to load box opened; snake filling
  the load box; ready box still closed.
- `Ready` → ready box bursts gold; a brief (~1–2 s) celebratory fill plays, then
  the tri-state selector (which now sees no loader) drops the view to the list.

The **head** is a bright leading glyph (`▶`) that pulses (twinkle on frame +
phase), sitting at the current fill frontier of the active box.

### Dual-mode fill

The active box fills by one of two drivers, chosen per phase by whether a
baseline exists:

- **Determinate** — `ServiceState.progress` is `Some(f)` (the monitor already
  computes compile = `kernel_count / o_baseline`, load = `rss / footprint` when
  a `ModelProfile` exists). The active box fills to fraction `f`; the readout
  shows `{f*100}%`.
- **Momentum** — `progress` is `None` (the common case). The head advances a
  small step each tick that the phase's signal moved (`kernel_delta > 0` while
  Compiling, `rss_delta > 0` while Loading), with step size scaled by the delta
  magnitude (bigger burst → longer surge), clamped so the box fills gradually
  and never "completes" on its own (only a phase change locks a box full). No
  percentage is shown; the readout shows the raw signal + rate instead.

The two modes share one visual language — same boxes, same snake, same head —
they differ only in what advances the head.

### Stall → Alarm

When the active phase's signal is flat (`kernel_delta`/`rss_delta` == 0 for the
active phase), the head holds position and dims. Once
`is_alarm(flat_ticks, cadence_secs, readiness)` trips (the existing ≥5-min flat
rule), the head and the active box turn red (`colors::ERROR`) and the footer
reads `stalled`. The snake *is* the alarm indicator — no separate widget.

## Data flow — zero new probing

The visualization is **pure presentation over the existing `ServiceState`**. No
changes to `probe.rs`, `state.rs`, or `monitor.rs`. It reads: `label`, `phase`,
`progress`, `kernel_count`, `kernel_delta`, `rss_bytes`, `rss_delta`,
`flat_ticks`, `readiness`. Rates (`↑N/s`) are `delta / cadence_secs`. Elapsed is
tracked by the viz itself (accumulated across frames, reset when the featured
service `key` changes or leaves the loading phases), so no timestamp is added to
`ServiceState`.

### Component — `src/animation/inference_load.rs`

- `LoadSnake::new(width: usize, height: usize) -> Self` — dims fixed at
  construction; the view recreates it on terminal resize (same pattern as
  `ModelStarfield`).
- `update(&mut self, featured: &ServiceState, cadence_secs: u32)` — advances the
  head (determinate from `progress`, else momentum from deltas), tracks elapsed,
  detects featured-key change (reset), computes the alarm flag.
- `render(&self) -> Vec<ratatui::text::Line<'static>>` — the three boxes, the
  snake fill, the head, a title line (`label`, phase, elapsed) and a footer
  (`kernels 1,842 ↑42/s   rss 6.2G ↑180M/s   47%`, or the momentum variant
  without the `%`). Char-safe truncation for model names.

### Wiring — `src/ui/tui/mod.rs`

The `DisplayMode::InferenceMonitor` render arm becomes tri-state: if
`featured_loading(&rows)` is `Some(svc)` → recreate/update a `LoadSnake` and
render it (via a `render_load_snake_view` mirroring `render_model_starfield_view`);
else if `trail_is_cold(&rows)` → starfield; else → the live list. The
`LoadSnake` update happens in the `should_tick` arm (like the starfield), fed
the featured service + the monitor's cadence.

## Error handling / degradation

- Featured service disappears mid-load → next frame the tri-state selector
  simply returns list or starfield; no special-casing, no panic.
- Zero terminal dims (`new(0,0)` before first resize) → render yields empty
  lines, never indexes out of bounds (same guard as `ModelStarfield`).
- Huge `rss_bytes`/`kernel_count` → saturating arithmetic; no overflow panic.
- Multibyte model names in the title → char-based truncation.
- `progress` present but slightly >1.0 or <0 → clamped to [0,1].
- Multiple loaders → feature one, `+N more loading` pip; never split.

## Testing (pure / host-runnable)

- `featured_loading`: picks the first Compiling/Loading, skips Ready/Down,
  stable order; `None` when nothing loads.
- Box-open state per phase: Compiling → compile box active/others closed;
  Loading → compile locked full + load active; Ready → ready burst.
- Determinate fill: `progress = Some(0.5)` → active box half full; readout
  `50%`. Clamp for out-of-range progress.
- Momentum advance: positive `kernel_delta`/`rss_delta` moves the head; zero
  delta holds; bigger delta → larger step (monotonic).
- Alarm coloring: `flat_ticks`/`cadence` past the 5-min rule → red head/box,
  footer `stalled`.
- Rate + elapsed formatting; footer text (determinate vs momentum variants).
- Render smoke via the existing `TestBackend`/buffer harness; zero-dim and
  featured-key-change (elapsed reset) no-panic guards.

## Out of scope

- Logstalgia / serving-request visualization (the Ready/hot state) — its own
  deferred spec.
- Adding new probes or per-model baselines (`ModelProfile` stays as-is; the viz
  degrades to momentum mode when no baseline exists — expanding the baseline
  table is a separate effort).
- Historical/rewind of a completed load; multi-service split-screen.
- Persisting elapsed across app restarts.
