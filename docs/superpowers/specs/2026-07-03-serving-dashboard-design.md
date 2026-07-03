# Serving dashboard + ANSI-shaded loading (design)

**Status:** approved design, pending spec review.
**Date:** 2026-07-03
**Sequence:** builds on the unified serving snake
(`2026-07-03-unified-serving-snake-design.md`). Turns the Feeding state from a
snake+panel into a full **serving dashboard** driven by everything
tt-inference-server exposes, and reworks the Growing (loading) render with
classic ANSI-art shading.

## Problem

The Feeding view shows a calm creature + a key/value panel — good, but it uses a
fraction of the screen and a fraction of the telemetry. tt-inference-server (via
vLLM `/metrics`) gives us far more (per-stage latency, prefix cache, preemptions,
throughput history), and tt-toplike *already* reads the TT chips' live
power/temp/clock — which nothing else can put on the same screen as serving
activity. The loading (Growing) render is also visually flat.

## Goal

When serving, fill the screen with a composed, adaptive dashboard that is both
**entertaining and informing**, all driven by real data:

1. **Throughput timeline** — a top-band scrolling sparkline of tok/s + active
   requests over recent history.
2. **Token-exhaust snake** — the creature exhales generated tokens as a particle
   stream; emission scales with `generation_tps`.
3. **Request swimlanes** — lanes for in-flight requests, split into
   queue→prefill→decode by the average per-stage times (aggregate-driven; see
   Limitations).
4. **Silicon strip** ⭐ — the box's TT chips (power/temp/AICLK, from tt-toplike's
   own device telemetry) pulsing beside the serving activity.

And make the **loading** sequence look like demoscene art: block-ramp gradients,
a compile→load→ready hue sweep, dithered edges, a bright animated leading edge.

## Adaptive layout

Full screen (below the title row, above the global status bar). Regions collapse
by available height/width — the snake + a compact stats line always survive:

```
Qwen3-32B   serving · READY · up 24m · p300x2                         [title]
 tok/s ▁▂▃▅▇█▇▅▃▂▁▂▅█ 842   requests ▁▁▂▃▃▂▁ 1 running · 2 queued      [timeline band, 1-2 rows]
────────────────────────────────────┬────────────────────────────────
 ~●▓▓▓▓▓▒▒░░ »  »   »   »             │ requests                        [center-left: snake+exhaust]
        (snake exhaling tokens →)    │  #1 ▓queue│░prefill│▒decode▒    [center-right: swimlanes]
                                     │  #2       │░prefill│▒decode     
                                     │ decode 842 t/s · KV 4% · TTFT.. [stats fold-in]
────────────────────────────────────┴────────────────────────────────
 silicon  BH0 ▓ 78°C 92W 1.35GHz   BH1 ▒ 74°C 85W 1.35GHz             [chip strip, 1 row]
```

**Collapse order** as the terminal shrinks: drop the timeline band → drop
swimlanes (snake takes full width) → drop the chip strip → finally just
snake + stats. Width < ~44 → snake-only (existing fallback). Every region has a
minimum size below which it's omitted rather than squeezed.

## Data plumbing

### `metrics.rs` — richer parse + `ServingStats` fields

Extend `VllmCounters` and the parser to also read the per-stage latency
histograms' running sums/counts (Prometheus `_sum`/`_count`), and prefix cache:
- `queue_time_sum: f64`, `queue_time_count: u64` (`vllm:request_queue_time_seconds`)
- `prefill_time_sum: f64`, `prefill_time_count: u64` (`vllm:request_prefill_time_seconds`)
- `decode_time_sum: f64`, `decode_time_count: u64` (`vllm:request_decode_time_seconds`)
- `tpot_sum: f64`, `tpot_count: u64` (`vllm:time_per_output_token_seconds`)
- `prefix_queries_total: u64`, `prefix_hits_total: u64` (`vllm:prefix_cache_queries_total` / `_hits_total`)
- `preemptions_total: u64` (`vllm:num_preemptions_total`)

`ServingStats` gains (all folded like the existing `ttft_avg_s`):
`queue_avg_s`, `prefill_avg_s`, `decode_avg_s`, `tpot_avg_s: f32`;
`prefix_hit_rate: f32` (hits/queries, 0 when no queries);
`preemptions_delta: u32` (per-tick, for a spill flash later). All degrade to 0
when the metric is absent (older vLLM / media servers), so the dashboard never
shows garbage — panels that depend on a missing metric simply flatten.

### `SnakeWorld` — chip telemetry

Add `chips: &'a [ChipReading]` where
`ChipReading { index: usize, arch: &'static str, power_w: Option<f32>, temp_c: Option<f32>, aiclk_mhz: Option<u32> }`.
The `[i]` tick wiring builds this each frame from `backend.devices()` +
`backend.telemetry(idx)` (fields already exist: `power`, `asic_temperature`,
`aiclk`). It's the whole box's TT devices — mapping a server to *specific* chips
isn't exposed, so the strip is box-level and labeled as such.

### Rolling history

The dashboard keeps a bounded ring buffer (e.g. last 64 samples) of
`(generation_tps, requests_running)` pushed once per `update`, for the timeline
sparkline. Lives in the dashboard state, capped so it never grows.

## Components / files

The Feeding renderer grows into a small module so each panel stays focused and
testable:

- `src/animation/serving/mod.rs` — `ServingDashboard` (was `ServingCreature`):
  owns state (frame, label, uptime, cpu/rss, `serving`, chips snapshot, history
  ring, pulse/fray), `update(&mut self, svc, uptime, chips)`, and `render(w,h)`
  that lays out the regions adaptively and composes the panels onto one canvas.
- `src/animation/serving/creature.rs` — the snake body + **token exhaust**
  (particles emitted right of the head, count/speed from `generation_tps`).
- `src/animation/serving/timeline.rs` — pure sparkline over the history ring
  (`sparkline(&[f32], width) -> String` using `▁▂▃▄▅▆▇█`).
- `src/animation/serving/swimlanes.rs` — pure: given `running`, `queue/prefill/
  decode` avg times, produce per-lane stage-segment widths + glyphs.
- `src/animation/serving/chips.rs` — pure: format a `ChipReading` into a shaded
  cell (`BH0 ▓ 78°C 92W 1.35GHz`), heat block from temp, dim when idle.
- `src/animation/inference_load.rs` — ANSI-art shading for the Growing render
  (see below); logic unchanged.
- `src/workload/inference_server/metrics.rs` — the parser/stats extensions.
- `src/animation/snake.rs`, `src/ui/tui/mod.rs` — `SnakeWorld.chips` + wiring.

Shared shading helpers (block ramps, dither, hue ramp) go in
`src/animation/common.rs` (already re-exported as `crate::animation::*`) so both
the loading render and the dashboard panels use one vocabulary.

## Token exhaust

Right of the snake's head, emit drifting token glyphs (`»`/`·`/fading dim) whose
spawn rate tracks `generation_tps` and whose per-tick burst tracks the tick's
generation-token delta. Deterministic drift (frame-indexed), capped particle
count, cleared when tps → 0 (calm at rest). Pure enough to test emission count
vs tps.

## Swimlanes (Limitations)

vLLM `/metrics` is **aggregate** — no per-request IDs — so lanes cannot track
individual requests. We render `requests_running` lanes (capped to the region
height), each split into queue/prefill/decode **segments proportional to the
average per-stage times** (`queue_avg_s : prefill_avg_s : decode_avg_s`), shaded
by stage. A per-tick `request_success` delta emits a brief "completion" marker.
This is a faithful *aggregate* picture, explicitly not per-request tracing. When
the stage-time metrics are absent, lanes fall back to a single "active" bar.

## ANSI-shaded loading

Rework `LoadSnake::render` (Growing) so a load reads like ASCII/ANSI art:
- **Block ramp** for the snake body and box fill: `█▓▒░` (+ `▀▄` half-blocks) by
  local intensity (fill fraction + a shimmer), instead of a single glyph.
- **Hue sweep** across the journey: compile teal → load amber → ready gold, as a
  smooth ramp via `hsv_to_rgb`, not three flat colors.
- **Dithered edges** at the fill frontier (alternating `▓▒░`) for a gradient feel.
- **Bright animated leading edge** (the head) that shimmers along the frontier.
- Alarm still overrides to red; determinate/momentum fill logic unchanged.

## Error handling / degradation

- Missing/old vLLM metrics → the new `ServingStats` fields are 0 → dependent
  panels flatten (swimlanes → single bar, latency → "—"); never a panic.
- No chip telemetry (host/mock/off-Linux, or `backend.telemetry` None) → the
  silicon strip shows "no device telemetry" or is omitted; never a panic.
- Tiny terminals → regions omitted per the collapse order; snake+stats survive;
  `vec![]` on zero dims.
- History ring is bounded; multibyte labels char-safe; all casts saturating.
- All I/O stays on the monitor thread; the dashboard reads snapshots only.

## Testing (pure / host-runnable)

- Parser: the new stage/prefix/preemption metrics parsed (real sample); absent →
  0; `_sum`/`_count` → averages via fold; counter-reset clamp holds.
- `sparkline`: monotone/empty/one-sample inputs → correct glyph ramp, exact width.
- Swimlanes: proportions from stage-time ratios sum to the lane width; 0 running
  → no lanes; missing stage times → single-bar fallback.
- Chip strip: formats power/temp/AICLK; `None` fields → placeholders; heat block
  from temp bucket.
- Token exhaust: emission count scales with tps; 0 tps → none (calm).
- Loading shading: block-ramp glyph selection is a pure `fn(intensity) -> char`
  tested across the range; hue ramp endpoints (teal/amber/gold).
- Dashboard compose: at a large size all regions present; shrinking drops them in
  the specified order; zero-dim no-panic; narrow → snake-only.
- Render smoke via the existing `TestBackend`/buffer harness.

## Out of scope

- True per-request tracing / individual request latency (aggregate only).
- Mapping a server to specific chips (box-level silicon strip).
- Persisting history across restarts; historical scrollback.
- Media/diffusion serving metrics (no `vllm:` counters → panels flatten).
- Retrofitting the Insights-screen `ServingMetrics` path.
