# Inference atmosphere

Date: 2026-10-14. Branch: `updates/reality`. Status: written and built without a
review gate, at the request "keep working through all of it to show off to me
later". The decisions below are mine; each can be reversed.

## Why

The Inference view (`i`) is a calm snake. It encodes concurrency, tokens per
second, KV-cache heat, a completion pulse and queued pellets. The things that make
inference interesting happen out of sight: weights streaming in, kernels
compiling, a prompt being read, tokens being written, the cache filling, a model
stalling. The view should feel like something is being thought, generated or made.
The Training view's aurora and stars (`train_sky`) show the idea: a living sky
whose motion means something.

## What the viewer sees

A backdrop drawn *behind* whatever the Inference view already draws. Only blank
cells show it, so the snake, the dashboards and the roster keep priority. The
featured service picks the scene (loading wins, else the first Ready service, the
same rule the snake uses):

| Scene | When | What it looks like |
|---|---|---|
| Forge | compiling or loading | A scaffold of beams rises from the floor, growing as kernels compile and weight shards land; sparks fly off the beam tops when kernels compile; the floor glows with host CPU load. |
| Storm | ready, requests running or waiting, no tokens flowing yet (prefill: the model is reading) | A cloud band builds across the top, with a slow heartbeat of three dots; lightning flickers when requests are running. |
| Aurora | tokens are flowing | The Training view's aurora, brightness following tokens per second and hue following KV-cache use. Gold glyphs rise from the floor at the token rate ("writing"); dim glyphs fall in proportion to prompt tokens per second ("reading"). Each running request is a comet orbiting the centre; each waiting request a dim star on the horizon; each completion a flare. |
| Forming | a diffusion or video server has a job in flight | A canvas in the middle resolves from noise toward structure in a slow loop. It is indeterminate on purpose. |
| Dream | ready and idle | A quiet sky: stars and a faint, slow aurora. |
| Stalled | no progress for 5 minutes | Dim red embers, beams stopped, no sparks. |

Ambient layers on the Aurora, Storm and Dream scenes: a **tide** along the bottom
whose height is KV-cache use (blue, then amber, then red), splashing when requests
are preempted; **static** across a row for a moment after an error.

No service featured (the cold state): nothing changes. The existing catalog
starfield and roaming snake stay as they are.

## Honest signals

Every effect maps to a number the monitor collects. Nothing runs without its
signal.

| Effect | Signal (`ServiceState`) |
|---|---|
| Forge beam growth | cumulative positive `kernel_delta` + `loaded_delta` this session. Not a percentage and never labelled as one. |
| Forge sparks | `kernel_delta > 0` this tick |
| Forge floor glow | `cpu_pct` |
| Aurora energy | `serving.generation_tps`, normalised by the peak seen (slowly decaying), so it is sensitive on any hardware |
| Aurora hue, tide height | `serving.kv_cache_usage` |
| Falling glyphs | `serving.prompt_tps` (normalised) |
| Rising glyphs | `serving.generation_tps` |
| Comets | `serving.requests_running`, at most 6 |
| Horizon stars | `serving.requests_waiting`, at most 12 |
| Completion flares | `serving.completed_delta`, at most 3 per tick |
| Storm | `requests_running > 0` or `requests_waiting > 0` while tokens per second is under 1 |
| Splash | `serving.preemptions_delta` |
| Static | `serving.errored_delta` |
| Forming | `media.jobs_in_progress > 0` |

The view never shows generated text. The monitor reads only counters, never
content, and this design does not change that. The rising glyphs are abstract.

## Components

- `src/animation/atmosphere.rs` (new): `Scene`, `scene_of`, `Atmosphere`
  (`observe(rows)`, `active()`, `render_backdrop(w, h)`), and the pure
  `composite(canvas, lines, width)`. Layers are small private functions that paint
  into a canvas, bottom to top: sky, tide, rain, storm, forge, forming, comets,
  flares, static.
- `src/animation/train_sky.rs`: the aurora and star hash are reused; `aurora` and
  `star_hash` become `pub(crate)`.
- `src/animation/snake.rs`: `Snake` owns an `Atmosphere`, calls `observe` in
  `update`, and composites in `render` when the atmosphere is active. A
  `render_foreground` split keeps the existing output reachable for tests.
- Inference `l` and `!` text, with a guard test.

## Behaviour rules

- Deterministic: the backdrop is a pure function of the state and a frame
  counter. No randomness. Positions come from the existing spatial hash.
- Bounded: at most 45% of cells lit at full signal (so text stays readable); at
  most 6 comets, 12 horizon stars, 8 flares; no layer is drawn at a size under 4
  columns by 3 rows.
- Foreground wins: a cell the foreground draws (any non-space character, wide
  characters included) is never replaced.
- Cost: a 160 by 50 backdrop plus composite is well under 2 ms in a debug build.

## Testing

Each behaviour has a test that was seen to fail first: scene selection for every
phase and stats combination; each effect absent without its signal (no rain at zero
rates, no forge growth from a negative or sentinel delta, no forming without a job
in flight); determinism; every size from 0 by 0 up to 160 by 50; the density cap;
`composite` never overwriting foreground cells; `Snake::render` identical to its
foreground when the atmosphere is inactive; flares expire and are capped; the cost
bound; the legend and explain guard.

## Out of scope

Arcade (its lanes are separate); the Training view; any new key or setting; a
percentage or ETA for loads; a per-job progress figure for diffusion.

## Risks

- **Readability.** A busy backdrop could make the dashboards harder to read. The
  density cap and the foreground-wins rule bound this; the view is checked on
  screen at 80 and 150 columns.
- **Terminals without true colour.** Colours go through `colors::rgb`, as the
  Training sky does.
- **Frame rate.** `Snake::update` may run per frame; the per-update work is a few
  float operations and the backdrop is only built in `render`.
