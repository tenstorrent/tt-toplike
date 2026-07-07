# Inference-Loading Education Band — Design

**Status:** approved (brainstorming), pending spec review
**Date:** 2026-07-07
**Author:** tt-toplike session (for Taylor Singletary)

## Goal

While a model is loading in the `[i]` Inference Monitor view, show a short,
phase-synced footer band that explains *what is actually happening right now* —
so a multi-minute compile/weight-load reads as purposeful progress, not a hang.
The copy comes from a proof-of-concept education set written in `~/code/tt-studio`
(`app/frontend/src/data/education/`), brought into tt-toplike as its own data.

## What changes for the user

Today the `[i]` loading view is the full-pane LoadSnake: a phase title, the
3-chamber coil animation, live footer stats, and an optional multi-model roster.
This adds a **footer band** beneath the snake that reads, for the featured
(loading) service:

```
 Llama-3.1-8B-Instruct · loading · 1:24
      [ snake coil animation ]

 Llama-3.1-8B · 128K ctx · Apache 2.0            ← model header (dim)
 ▸ Loading model weights                 ~30s+   ← phase title + duration hint (accent)
   Parameters stream from disk to SRAM over PCIe;
   a 70B model is ~35 GB in 4-bit — that's why.   ← explanation, word-wrapped
 TT · Tensix packs 108 compute cores per chip     ← rotating did-you-know fact (dim)
```

## Data — owned by tt-toplike, not vendored

The three education JSON sets are **copied into tt-toplike as its own data** and
embedded at build time. This is a deliberate departure from a vendor/sync model:
there is **no sync script and no upstream coupling to tt-studio**. tt-toplike owns
this copy now; the content is expected to graduate to a dedicated "facts" repo
later, so it is kept as standalone data files (not hand-written Rust literals) to
make that extraction a move, not a rewrite.

**Files** (new): `assets/education/` at the repo root, embedded via `include_str!`
and parsed once through `serde_json` behind a `OnceLock`:

- `deployment-stages.json` — copied in full (6 entries: `initialization`, `setup`,
  `model_preparation`, `container_setup`, `finalizing`, `complete`; each has
  `title`, `explanation`, `duration_hint`).
- `did-you-know.json` — copied in full (`cards[]` of `headline`, `body`, `tag`).
- `model-education.json` — copied as a **subset**: keep `tagline` + `key_stats`
  (`context`/`params`/`license`) per model, which is all the band renders in v1.
  The unused fields (`description`, `use_cases`, `quick_start`, `family`,
  `model_type_label`) are dropped for now (YAGNI) but the file keeps the same
  top-level shape (keyed by model name) so re-adding them is additive.

If a copyright/license header is required on source-tree assets (see
`check_copyright_config` conventions), add it as a sidecar or in the module that
embeds them — the JSON itself stays plain data.

## Phase → stage mapping

tt-toplike's `Phase` enum (`Compiling`, `Loading`, `Ready`, `Down`, `Alarm`) maps
to `deployment-stages.json` keys. The mapping is by what each `Phase` *means* in
`Phase::derive` (verified during implementation), not by chamber order:

| `Phase` | Stage key shown | Title |
|---|---|---|
| `Compiling` | `container_setup` | "Compiling execution graphs" |
| `Loading` | `model_preparation` | "Loading model weights" |
| `Ready` | `complete` | "Ready" (shown briefly; band fades with the ready-burst) |
| `Down` | `initialization` | "Starting up" — or the band is suppressed |
| `Alarm` | *(custom)* | "Stalled — check the server logs" (no tt-studio equivalent) |

`stage_for_phase(Phase) -> Stage` is total (every `Phase` yields a stage or an
explicit "suppress the band" signal for `Down`).

## Layout — the footer band

Rendered inside the `[i]` loading pane, mirroring the existing `roster_h` reserve
in `render_snake_view`: the band claims up to ~5 rows above the bottom status bar,
and the snake body shrinks to fit. Composition, top to bottom:

1. **Model header** (1 line, dim): `"{model short-name} · {context} · {license}"`
   from the model-education subset. Falls back to just the model name when the
   model isn't in the set.
2. **Phase line** (1 line, accent): `"▸ {stage title}"` left, `"{duration_hint}"`
   right-aligned.
3. **Explanation** (up to 2 lines, dim): the stage `explanation`, word-wrapped to
   the pane width, char-boundary-safe.
4. **Rotating fact** (1 line, dim): `"TT · {did-you-know headline}"`, cycling to
   the next card every ~8 s while the phase persists.

**Graceful degradation** (by available height, then width):
- Not enough rows for all 5 → drop the rotating fact first, then the model header,
  then the explanation's second line, then the whole band (snake reclaims the pane).
- Narrow width → word-wrap the explanation; truncate the header/phase lines on a
  char boundary with an ellipsis. Never panic, never split a multibyte char.

**Colors:** reuse the synthwave loading accents (cyan/violet/pink) for the phase
marker; dim slate for secondary lines. Consistent with the current loading palette.

**Remote:** works under `--remote` — the model label and phase already arrive via
the streamed `tt_toplike` inference extension, so the band renders for a watched
box's load exactly as for a local one.

## Structure & data flow

- New module `src/workload/inference_server/education.rs`:
  - Embeds the three JSON assets (`include_str!`) and parses them once (`OnceLock`)
    into typed structs (`Stage`, `Fact`, `ModelOverview`).
  - Pure lookups: `stage_for_phase(Phase) -> StageView` (with a suppress variant
    for `Down`), `model_overview(model_label: &str) -> Option<&ModelOverview>`,
    `facts() -> &[Fact]`.
  - A pure band composer:
    `compose_band(model_label, phase, elapsed, width, height, rotation_idx) -> Vec<Line<'static>>`
    that applies the layout + degradation rules above and returns render-ready lines.
- The band lines are composed on the inference **monitor tick** (like the
  multi-model roster today), stored alongside `inference_roster`, so the render
  path stays a cheap `Paragraph` blit. The rotation index advances on a frame
  timer (like the other animations).
- `render_snake_view` gains a `footer_h` reserve symmetric to `roster_h`: roster on
  top, snake in the middle, education band above the status bar.

## Testing

Pure unit tests (no TTY, no hardware):
- All three JSON assets parse into their structs (guards against a bad copy-in).
- `stage_for_phase` returns a stage (or the suppress signal) for **every** `Phase`
  variant, including `Alarm`'s custom line.
- `model_overview` hit (a known model) and miss (unknown → `None` → name fallback).
- `compose_band` at representative sizes: full 5-line band; each degradation step
  (drop fact → header → explanation line → whole band); a narrow width wraps the
  explanation and truncates headers **char-safely** (assert no panic on multibyte
  model names / copy).
- Rotation is deterministic: given a rotation index, the same fact comes out.

## Non-goals (v1)

- No live fetch from or sync with tt-studio; the copy is owned in-tree.
- No new dependency on the multi-model roster behavior — the band describes the
  **featured** (loading) service only.
- The dropped model-education fields (`description`/`use_cases`/`quick_start`) are
  not rendered yet; re-adding them is a later, additive change.
- Eventual migration of the copy to a dedicated "facts" repo is out of scope here;
  the data-file structure is chosen to make that a move rather than a rewrite.
