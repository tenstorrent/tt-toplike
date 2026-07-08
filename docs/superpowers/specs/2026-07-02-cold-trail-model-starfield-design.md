# Cold-trail model starfield for the `[i]` view (design)

**Status:** approved design, pending spec review.
**Date:** 2026-07-02
**Sequence:** a new cold-state visualization for the Inference Server Monitor view
created in `2026-07-02-tt-process-filter-inference-monitor-view-design.md`.
Independent of the deferred spec #2 (logstalgia, hot) and #3 (snake, load) — its
own spec→plan→build cycle.

## Problem

The `[i]` Inference Server Monitor lists every known service as `Down` when
nothing is running. A wall of `Down` isn't useful. When the trail is cold we can
instead show *what's possible* on this box — an After-Dark-style screensaver of
the Tenstorrent model catalog, with the models that run on the detected hardware
brought to the fore.

## Goal

When the `[i]` view is open and no inference server is running, render a **model
starfield** screensaver: models drift/fly toward the viewer as labeled stars;
models compatible with the detected TT hardware are bright + status-colored, the
rest dim; a footer reads "N of M models run on your `<arch>`". The instant a
service is detected/loading, the view switches to the live monitor.

## Data

Source: the public Tenstorrent compatibility catalog,
`https://d1oi7xemha0dsy.cloudfront.net/data/compatibility.json` (today: 96
models, ~420 KB). Schema (verified 2026-07-02):

```
{ "metadata": {...},
  "models": [ {
    "id", "display_name", "family", "tasks": [...],
    "model_size", "model_size_num", "model_description",
    "compatibility": [ { "chip_set": "Blackhole"|"Wormhole",
                         "hardware": "p150"|"n300"|"Quietbox"|...,
                         "hardware_family": "Card"|"Quietbox"|...,
                         "software": ["tt-inference-server"|...],
                         "status": "Supported"|"Experimental"|"Not Supported" } ] } ] }
```

### Layered acquisition (instant + fresh + offline-safe)
1. **Bundled snapshot** — commit today's `compatibility.json` into the repo (e.g.
   `assets/compatibility.snapshot.json`) and `include_str!` it; parse at startup.
   Instant, offline, compile-checked — the always-available floor.
2. **Background refresh** — the catalog is **HTTPS** and the crate has no TLS
   client, and we add no TLS dependency; instead a background thread shells out to
   **`curl -fsS --max-time <n> <URL>`** once at startup (then ~every 24 h), writes
   `~/.cache/tt-toplike/compatibility.json`, and publishes the newly-parsed catalog
   via `arc-swap`. All off the render path. The `.deb` gains `Recommends: curl`.
3. **Precedence:** use the freshest valid catalog available — cache (if present and
   parseable) else bundled. A failed/absent `curl`, offline, or malformed download
   simply keeps the last good catalog (bundled never fails).

## Components

### `src/workload/model_catalog.rs`
- `CatalogModel { display_name, family, model_size, tasks: Vec<String>, compatibility: Vec<Compat> }`, `Compat { chip_set, hardware, status }` (drop fields we don't render).
- `parse_catalog(json: &str) -> Vec<CatalogModel>` (serde_json; tolerant — skip malformed models, never panic).
- `pub fn bundled() -> &'static str` = `include_str!("../../assets/compatibility.snapshot.json")`.
- `Support` = `Supported | Experimental | No` and `model_support(model, arch) -> Support` — matches `compatibility[].chip_set` against the detected arch (Blackhole/Wormhole/Grayskull) and returns the best status.
- `CatalogRefresher` — background thread + `arc-swap` cache of `Arc<Vec<CatalogModel>>`, mirroring `InferenceServerMonitor`: seed from bundled at construction, then `curl`-refresh; `snapshot() -> Arc<Vec<CatalogModel>>` lock-free read. Pure `parse_catalog`/`model_support` unit-tested; the `curl` shell is a thin, isolated function.

### `src/animation/model_starfield.rs`
- A frame-driven screensaver: each `CatalogModel` is a "star" with a position, depth/velocity (flying toward the viewer, wrapping), and a label (`display_name` + `model_size`). Compatible models (Supported/Experimental for the current arch) render bright with a status color (Supported → green, Experimental → amber) and twinkle; incompatible models render dim/gray. A footer line: "N of M models run on your `<arch>`" (N = compatible count).
- `ModelStarfield::new(width, height)`, `update(&mut self, models: &[CatalogModel], arch)`, `render(&self) -> Vec<Line<'static>>`. Reuses the existing starfield color/movement idioms where natural. Pure layout — unit-testable given a synthetic catalog (star count == model count, compatible ones flagged, footer text).

### Activation — `src/ui/tui/mod.rs` / `inference_panel.rs`
- In the `InferenceMonitor` render arm: the trail is **cold** when no service in the live snapshot is detected/active (no service in a running phase — Compiling/Loading/Ready). When cold, render the `ModelStarfield` (fed the `CatalogRefresher` snapshot + detected arch); otherwise render the live service list (current behavior).
- Spawn the `CatalogRefresher` once (like the other background monitors); read its `snapshot()` in the render arm. Non-Linux: the screensaver still renders from the bundled catalog (it's pure data + a `curl` shell that simply no-ops if curl/network absent); no platform gating needed beyond what the fetch shell already tolerates.

## Error handling / degradation
- No `curl` / offline / HTTP error / malformed download → keep the last good catalog (bundled floor never fails; it's compile-checked).
- Empty/failed parse of even the bundled snapshot (shouldn't happen — compile-checked) → the view shows a single "model catalog unavailable" line rather than panicking.
- Arch undetectable (e.g. `--host`, mock) → still show the starfield with nothing highlighted (or highlight none), footer "0 of M run on your <unknown>"; never panic.
- All fetch/cache I/O on the background thread; the render path only reads the `arc-swap` snapshot.

## Testing
Pure/host-runnable:
- `parse_catalog` over the bundled snapshot: model count > 0, a known model's fields (e.g. "FLUX.1 [schnell]" present with a Blackhole compat entry), and that a malformed model entry is skipped not panicked.
- `model_support(model, arch)` for each chip_set (Blackhole/Wormhole) → correct Supported/Experimental/No.
- `ModelStarfield`: given a synthetic catalog + arch, star count == model count, compatible stars flagged bright, footer "N of M" correct.
- Activation predicate: cold (no running service) → starfield; any Compiling/Loading/Ready service → live list.
- Render smoke via the existing `TestBackend`/buffer harness.

## Out of scope
- Logstalgia (spec #2) and snake load-progress (spec #3).
- Clicking/selecting a model to launch it (this is a *screensaver*, read-only).
- Board-level `hardware` matching beyond `chip_set` (start with chip_set; refine to specific board `hardware` later if useful).
- Adding a TLS HTTP client (deliberately using `curl` + bundled snapshot instead).
