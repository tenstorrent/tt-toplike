# Unified serving snake for the `[i]` view (design)

**Status:** approved design, pending spec review.
**Date:** 2026-07-03
**Sequence:** unifies the three `[i]` Inference Server Monitor states behind one
persistent creature. Absorbs the cold-trail model starfield
(`2026-07-02-cold-trail-model-starfield-design.md`) and the loading boxed-snake
(`2026-07-02-loading-boxed-snake-design.md`), and adds the previously-deferred
serving/"logstalgia" state as the third behavior. Supersedes the curated
Down-roster list that the Ready state showed.

## Problem

The `[i]` view is a tri-state: cold → model starfield, loading → boxed snake,
ready → a list. Two issues: (1) the ready/serving state shows a wall of curated
`SERVERS` media models as `Down` plus the one running server buried at the
bottom, with an inert selection cursor — not useful; (2) the three states are
disconnected visuals with nothing tying them together. Meanwhile a running
tt-inference-server exposes rich live telemetry (vLLM Prometheus `/metrics`) we
don't surface at all.

## Goal

Make the `[i]` view **one persistent snake** — a single creature with a
continuous identity that plays a distinct role in each state, so cold, loading,
and serving read as three phases of one world rather than three screens:

- **Cold** (nothing running) → **Roaming**: a small, hungry snake drifts through
  the model-star field (the catalog) waiting for something to load.
- **Loading** (compiling/loading/alarm) → **Growing**: the snake lengthens along
  the compile→load journey (the built boxed-snake behavior).
- **Serving** (ready) → **Feeding**: a calm living creature whose form encodes
  live vLLM signals directly — near-still when idle, animating only in
  proportion to real serving activity.

## Continuity model

Continuity is by **identity and carried state**, not frame-perfect morphing. One
`Snake` object lives for the whole session; on each frame it's told the current
world-state + telemetry and re-targets its behavior, carrying its head, color
language, and body length across transitions. We do NOT tween a literal slither
between the three (very different) layouts — the creature *persists* and changes
what it's doing. This is called out explicitly so implementers don't attempt
(and reviewers don't expect) a physically continuous path between screens.

## State machine

Same detection as today, on the raw `InferenceServerMonitor` snapshot, picking
the snake's behavior each frame (precedence top-to-bottom):

1. `featured_loading(&snapshot).is_some()` (Compiling/Loading/Alarm) → **Growing**
2. a service is `Ready` (nothing loading) → **Feeding** on that service
3. otherwise (`trail_is_cold`) → **Roaming**

The brief Ready gold-burst (built) still plays on the loading→ready edge, then
hands to Feeding.

## Behaviors

### Roaming (cold)

The existing `ModelStarfield` remains the cold **food field** renderer (catalog
stars, compatible models glow by support level, footer "N of M models run on
your `<arch>`"). The snake is drawn *over* it: a short (hungry) body that drifts,
unable to eat (no model loaded). Conveys "waiting." No new data — reads the
bundled/refreshed catalog + detected arch, as today.

### Growing (loading)

Unchanged from the built boxed-snake: compile→load chambers, determinate `%`
when a `ModelProfile` baseline exists else momentum from `kernel_delta`/
`rss_delta`, red on Alarm, gold burst at Ready. Reframed as the snake growing.

### Feeding (serving)

A calm living creature in an arena, driven **directly** by the serving signals;
near-still at rest. Signal → motion mapping (each is a real vLLM metric):

| Signal (vLLM `/metrics`)                         | Snake expression                    |
|--------------------------------------------------|-------------------------------------|
| `num_requests_running`                           | body length (in-flight) + base      |
| `generation_tokens_total` rate (tok/s)           | slither wave amplitude / speed      |
| `kv_cache_usage_perc`                            | color-heat (cool→hot)               |
| `request_success_total{stop\|length}` delta      | a single bright pulse per completion|
| `request_success_total{error\|abort}` delta      | tail-fray / flinch                  |
| `num_requests_waiting`                           | dim queued pellets alongside        |
| idle (0 tok/s, 0 in-flight)                       | gentle breathing only (non-distracting) |

A compact stats line underneath carries the exact numbers:
`{tps} tok/s · {running} running · {waiting} queued · KV {kv}% · TTFT {ttft}s · {served} served`.

## Data layer — serving telemetry

The vLLM server exposes a Prometheus text endpoint at
`http://<host>:<port>/metrics` (verified live: `vllm:num_requests_running`,
`vllm:num_requests_waiting`, `vllm:generation_tokens_total`,
`vllm:prompt_tokens_total`, `vllm:request_success_total{finished_reason}`,
`vllm:kv_cache_usage_perc`, `vllm:time_to_first_token_seconds_{sum,count}`).

### Acquisition — reuse the existing monitor

`InferenceServerMonitor` already probes each container every `CADENCE_SECS` (5s)
via `ContainerProbe::http(port, path)`. `build_sample` will make a second
`http(port, "/metrics")` call and put the raw text on `TickSample`. All I/O
stays on the background thread; the render path only reads the published
snapshot.

### `src/workload/inference_server/metrics.rs` (new)

- `VllmCounters` — the raw values parsed from one `/metrics` scrape:
  `generation_tokens_total: u64`, `prompt_tokens_total: u64`,
  `requests_succeeded_total: u64` (sum of `stop`+`length`),
  `requests_errored_total: u64` (sum of `error`+`abort`),
  `requests_running: u32`, `requests_waiting: u32`, `kv_cache_usage: f32`,
  `ttft_sum: f64`, `ttft_count: u64`.
- `parse_vllm_metrics(text: &str) -> Option<VllmCounters>` — tolerant Prometheus
  line parser (ignores `#` comments, matches the `vllm:<name>{labels} value`
  lines we care about, sums the labelled `request_success_total` variants).
  `None` when the text has no `vllm:` metrics (non-vLLM server).
- `ServingStats` — display-ready, folded per tick:
  rates `generation_tps: f32`, `prompt_tps: f32`;
  edges `completed_delta: u32`, `errored_delta: u32`;
  gauges `requests_running: u32`, `requests_waiting: u32`, `kv_cache_usage: f32`,
  `ttft_avg_s: f32`; and the carried cumulative counters (a `VllmCounters`) so
  the next tick can diff.
- `ServingStats::fold(prev: Option<&VllmCounters>, cur: &VllmCounters, cadence_secs: u32) -> ServingStats`
  — rates = `(cur - prev) / cadence`; a **counter reset** (cur < prev, e.g. a
  server restart) clamps that rate/delta to 0; `ttft_avg_s = ttft_sum / ttft_count`
  (0 when count 0).

### `ServiceState` gains `serving: Option<ServingStats>`

`fold_tick` parses the tick's `/metrics` text (`parse_vllm_metrics`) and, when
`Some`, computes `ServingStats::fold` against the previous tick's carried
counters. `None` (no metrics / not vLLM) leaves `serving = None`. The Feeding
snake reads the featured Ready service's `serving`; `None` → a calm "alive, no
token telemetry" creature (still shows readiness + uptime, no rates).

## Architecture / files

- `src/animation/snake.rs` (new) — the unified `Snake { behavior, body/length,
  head, color, frame }` with `Behavior { Roaming, Growing, Feeding }`,
  `update(&mut self, world: SnakeWorld, ...)`, `render() -> Vec<Line<'static>>`.
  Absorbs the `LoadSnake` growing logic (moved/kept behind the Growing behavior)
  and overlays a roaming body on the starfield. `ModelStarfield` stays as the
  cold food-field renderer the Roaming behavior draws over.
- `src/workload/inference_server/metrics.rs` (new) — `parse_vllm_metrics`,
  `VllmCounters`, `ServingStats`.
- `src/workload/inference_server/state.rs` — add `serving: Option<ServingStats>`
  to `ServiceState`; update every struct literal (fresh_state, down_placeholder,
  test `base()`, etc.).
- `src/workload/inference_server/monitor.rs` — `build_sample` fetches `/metrics`;
  `fold_tick` folds `ServingStats`; `TickSample` gains the raw metrics text.
- `src/ui/tui/mod.rs` — the `[i]` render arm drives the single `Snake` from the
  world-state + featured/ready service; replaces the three separate render
  branches with one snake render (still choosing behavior via the state machine).
  Removes the ready-state `service_rows` list path.

## Error handling / degradation

- `/metrics` missing / non-200 / unparseable → `serving = None` → calm alive
  creature (readiness + uptime only). Never an error screen, never a panic.
- Counter reset (server restart) → negative delta clamped to 0 (no negative tps).
- Non-vLLM / media server (no `vllm:` metrics) → `serving = None`, same calm state.
- Non-Linux / no container → Roaming from the bundled catalog (as today).
- Zero terminal dims, multibyte model names → guarded (as the existing anims).
- All `/metrics` I/O on the background thread; render reads the snapshot only.

## Testing (pure / host-runnable)

- `parse_vllm_metrics` over a captured real sample: correct counters; sums the
  labelled `request_success_total` variants; `None` on text with no `vllm:`.
- `ServingStats::fold`: rates from deltas over cadence; counter-reset → 0;
  `ttft_avg` = sum/count (and 0 when count 0).
- `fold_tick`: a Ready tick with metrics text → `serving = Some(..)`; no/empty
  metrics → `None`; carried counters enable the next tick's rates.
- Snake behaviors: Roaming (body drifts, hungry/short); Growing (the existing
  boxed-snake tests carry over); Feeding (length ∝ `requests_running`; near-still
  when idle — no motion at 0 tps/0 running; a completed-delta triggers a pulse;
  an error-delta triggers a fray); state machine picks the right behavior and the
  snake's identity/length persists across a transition.
- Render smoke per state via the existing `TestBackend`/buffer harness; zero-dim
  no-panic guard.

## Out of scope

- Per-request tracing or a scrollback of individual prompts (aggregate metrics
  only).
- Retrofitting the existing `ServingMetrics` (serving.rs) — this feature adds
  focused `vllm:` parsing; the Insights-screen `ServingMetrics` path is left
  as-is.
- Media/diffusion serving metrics (they don't expose `vllm:` counters) — those
  servers get the calm "alive, no token telemetry" Feeding state; a variant-aware
  metrics source is a later effort.
- A literal playable snake-game (chosen: abstract signal-driven creature).
