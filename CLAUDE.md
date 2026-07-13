# tt-toplike — project notes

Rust TUI/GUI "top-like" telemetry monitor for Tenstorrent devices. Binaries:
`tt-toplike` (dispatcher), `tt-toplike-tui`, `tt-toplike-app` (egui GUI).

## Build gotchas
* A `build-deb.sh` run leaves an **untracked** `.cargo/config.toml` + partial
  `vendor/` in the tree that redirects all crate lookups to `vendor/` for
  network-free dpkg builds. That vendor set is incomplete (missing
  `all-smi-luwen-*` and most deps), so a normal `cargo build` fails with
  *"no matching package named `all-smi-luwen-core`"*. Move `.cargo/config.toml`
  aside to build/test against the real registry cache, then restore it.
* Per global CLAUDE.md: after `cargo build --release`, copy every shipped binary
  into `~/.local/bin` (`cp target/release/tt-toplike{,-tui} ~/.local/bin/`) or a
  stale one on `$PATH` shadows it.
* `all-smi-luwen-*` crates are **optional**, gated behind the `luwen-backend`
  feature — default builds don't need them.

## Session log

### 2026-07-11 — legend/sidebar truncation + SkyReels metrics finding (v0.7.31)
Prompt: "the text in our legends (and other little sidebars) keeps getting
truncated!" — plus a follow-up that tt-inference-server mode shows nothing for a
SkyReels model.

* **Fix (shipped):** `render_overlay_panel` (legend / help / explain overlay in
  `src/ui/tui/mod.rs`) hardcoded `PANEL_W = 42`, but content lines run 50–66
  display columns. `Paragraph` clips (doesn't wrap), so every long line lost its
  tail (e.g. the `/serve` help line dropped "(plaintext, trusted-LAN)"). Now the
  panel measures its widest content line in real display columns (new `line_cols`
  helper via `unicode-width`, matching arcade.rs) and sizes itself to fit,
  clamped to the terminal. Regression test:
  `overlay_panel_does_not_truncate_wide_content`. The kill-confirm dialog
  (`render_kill_dialog`, also 42-wide) is fine — it deliberately truncates the
  process name to 14 chars.

* **Media metric names corrected against a LIVE server (v0.7.33).** The 0.7.32
  parser used doc-derived names that don't exist on the real
  `tt-media-inference-server:0.15.0` — so a live SkyReels box showed nothing when
  load was sent. Curled the real `:8000/metrics` and fixed against ground truth:
  - Real signals: `tt_media_server_requests_base_total` (completed),
    **`tt_media_server_jobs_in_progress`** (in-flight gauge — the "acking" signal
    the user was missing), `tt_media_server_requests_base_duration_seconds_total`
    histogram (end-to-end per-gen wall time), `tt_media_server_post_processing_*`.
    The doc names `model_inference_total`/`pre_processing`/`device_warmup` are
    NOT emitted (parsed best-effort, shown only if non-zero).
  - **Duplicate-series gotcha:** that build (prometheus multiprocess) emits every
    series *twice*, byte-identical. The summing parser doubled everything → now
    dedupes by `name{labels}` identity while still summing distinct label sets.
  - `jobs_in_progress` drives the snake body length + keeps it animated while
    work is in flight (rate/completions are rare — a clip takes minutes). Panel
    leads with "in flight N". Roster shows "N in flight · M done".
  - Live readiness: `/health`→200 `{}`, `/tt-liveness`→`model_ready:true` +
    `queue_size` + `runner_in_use`; generic tracking uses `/health` (200→Ready).
  - Verified end-to-end: real full `/metrics` → `requests_total=1,
    jobs_in_progress=2, duration=612s` (no doubling).
  - ⚠️ Metric shape is version-specific; re-curl `/metrics` when the server image
    changes. Reference scrape saved under the session scratchpad during the fix.

* **SkyReels / media-server metrics (initial cut, v0.7.32):** the metrics layer only
  parsed the `vllm:` namespace, so diffusion/video servers
  (**tt-media-inference-server**: SkyReels, SDXL, z-image) showed a blank [i]
  panel — they expose a *different* namespace, `tt_media_server_*`, on the same
  `/metrics` endpoint the monitor already scrapes, and tokens/sec is meaningless
  for diffusion anyway. Added:
  - `parse_media_metrics` + `MediaCounters`/`MediaStats` (+ `fold`) in
    `metrics.rs` — parses `requests_base_total`, `model_inference_total` (status
    label → done/errored), and the stage-duration histograms
    (`model_inference`/`pre_processing`/`post_processing`/`device_warmup`
    `_duration_seconds`), summing across `device_id`/`model_type` labels.
  - `ServiceState.media: Option<MediaStats>`, folded in `monitor.rs` alongside
    `serving` (mutually exclusive — a scrape carries one namespace or the other).
  - Reused the Feeding snake (`serving_creature.rs`) via a workload-agnostic
    `CreatureDrive`: headline = generations/min (+ seconds-per-gen), snake motion
    from generation rate, a diffusion panel (throughput / generations /
    warmup→pre→infer→post stage times / container) instead of the token panel.
    LLM path is byte-for-byte unchanged. Roster line shows `gen/min · done`.
  - Remote wire: `RemoteMedia` on `RemoteInference` (`#[serde(default)]` for
    back-comat), mapped both directions.
  - Design decisions (asked): reuse-the-snake (not a separate panel), and show
    BOTH gen/min and s/gen. No native per-step/per-frame progress exists (would
    need log scraping) — deferred.
  - Tests: media parser/fold, mutual exclusivity, creature media-panel render
    (asserts "gen/min"/"stages" present, "tok/s"/"KV cache" absent), history.
