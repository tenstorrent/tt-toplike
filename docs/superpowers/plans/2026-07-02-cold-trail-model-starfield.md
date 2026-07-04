# Cold-trail Model Starfield — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** When the `[i]` Inference Server Monitor is open and nothing is running, show an After-Dark-style starfield of the Tenstorrent model catalog — models compatible with the detected hardware bright/colored, the rest dim, with a "N of M models run on your `<arch>`" footer.

**Architecture:** A bundled `compatibility.json` snapshot (`include_str!`) is the instant/offline floor; a background thread `curl`s the live CloudFront catalog to refresh an `arc-swap` snapshot. A `ModelStarfield` animation renders the catalog as drifting stars. The `[i]` render arm shows the starfield when the trail is cold (no Compiling/Loading/Ready service) and the live list otherwise.

**Tech Stack:** Rust, `serde_json` (dep), `arc-swap` (dep), `std::process::Command` (curl), `std::thread`, ratatui. No new dependencies.

## Global Constraints

- **No new dependencies.** HTTPS fetch is via `curl` (the `.deb` gains `Recommends: curl`); no TLS crate.
- **No panics** on catalog data (untrusted download): tolerant parse, keep last-good; the bundled snapshot is the never-fail floor.
- **Off the hot path:** all curl/cache I/O on the background thread; render reads the `arc-swap` snapshot only.
- **Cross-platform:** `model_catalog` + `model_starfield` are pure/portable (serde + a curl shell that no-ops if curl absent). No cfg-gating needed; must compile on macOS/Windows (verify cross-checks with REAL exit codes — do NOT pipe through tail/head).
- **SPDX header** on every new file.
- **Verify each task:** `cargo test --locked --lib --features tui`, `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings`, macOS cross-check `cargo clippy --target aarch64-apple-darwin --no-default-features --features tui,json-backend -- -D warnings`.

## Current-state anchors
- `src/ui/tui/mod.rs`: `DisplayMode::InferenceMonitor` render arm (~line 653) computes `inference_monitor_rows = inference_panel::service_rows(&inference_monitor.snapshot())` when in that mode and renders `render_inference_monitor(rows, service_cursor)`. `inference_monitor.snapshot()` returns the raw `Vec<ServiceState>` (detected services only). Background monitors are spawned near line 265 (`InferenceServerMonitor::spawn()`), Linux-gated.
- `ServiceState.phase: Phase` (`Down|Compiling|Loading|Ready|Alarm`). "Cold" = none of the raw snapshot's services is `Compiling|Loading|Ready`.
- Arch: `backend.devices()` → `Device.architecture: Architecture` (`Grayskull|Wormhole|Blackhole|Unknown`); `Architecture` has a name string ("Blackhole" etc., src/models/device.rs:33-35).
- Existing animation shape (to mirror): `HardwareStarfield::new(w,h)` / `update…` / `render(&self) -> Vec<ratatui::text::Line<'static>>` (src/animation/starfield.rs).
- `arc-swap`, `serde`/`serde_json`, `rand` are all existing deps.

## File Structure
- `assets/compatibility.snapshot.json` — bundled catalog snapshot (committed).
- `src/workload/model_catalog.rs` — catalog types, `parse_catalog`, `bundled()`, `model_support`, `CatalogRefresher`.
- `src/animation/model_starfield.rs` — the `ModelStarfield` screensaver.
- `src/workload/mod.rs`, `src/animation/mod.rs` — module decls + re-exports.
- `src/ui/tui/mod.rs` — cold-trail activation in the `InferenceMonitor` arm; spawn `CatalogRefresher`.
- `debian/control` — add `curl` to `Recommends`.

---

### Task 1: Bundle the catalog + parse + hardware-support matcher

**Files:** Create `assets/compatibility.snapshot.json`, `src/workload/model_catalog.rs`; modify `src/workload/mod.rs`.

**Interfaces:** Produces `CatalogModel { display_name, family, model_size, tasks: Vec<String>, compatibility: Vec<Compat> }`, `Compat { chip_set: String, hardware: String, status: String }`, `parse_catalog(&str) -> Vec<CatalogModel>`, `bundled() -> &'static str`, `Support { Supported, Experimental, No }`, `model_support(&CatalogModel, chip_set: &str) -> Support`.

- [ ] **Step 1: Fetch today's snapshot into assets/** (the bundled floor):

```bash
mkdir -p assets
curl -fsS --max-time 30 "https://d1oi7xemha0dsy.cloudfront.net/data/compatibility.json" -o assets/compatibility.snapshot.json
python3 -c "import json;d=json.load(open('assets/compatibility.snapshot.json'));print('models:',len(d['models']))"   # expect ~96
```
(If offline, a copy was saved this session at the scratchpad `compatibility.json` — copy that instead.)

- [ ] **Step 2: Write the failing test** in `src/workload/model_catalog.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_bundled_catalog_and_matches_hardware() {
        let models = parse_catalog(bundled());
        assert!(models.len() > 20, "bundled catalog should have many models");
        // A known model with a Blackhole compatibility entry.
        let flux = models.iter().find(|m| m.display_name.contains("FLUX.1 [schnell]"))
            .expect("FLUX.1 [schnell] present in snapshot");
        assert!(matches!(model_support(flux, "Blackhole"), Support::Supported | Support::Experimental));
        // Malformed JSON → empty, never panics.
        assert!(parse_catalog("{ not json").is_empty());
        assert!(parse_catalog("{\"models\":[{}]}").len() == 1); // tolerant of missing fields
    }
}
```

- [ ] **Step 3: Run — fails** (`parse_catalog` undefined): `cargo test --locked --lib --features tui model_catalog` → FAIL.

- [ ] **Step 4: Implement `model_catalog.rs`** (SPDX header first):

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tenstorrent model-compatibility catalog: a bundled snapshot (offline floor)
//! plus a background curl-refreshed live copy. Drives the cold-trail starfield
//! screensaver in the Inference Server Monitor.

use serde::Deserialize;

/// One hardware-compatibility entry for a model.
#[derive(Debug, Clone, Deserialize)]
pub struct Compat {
    #[serde(default)]
    pub chip_set: String, // "Blackhole" | "Wormhole" | ...
    #[serde(default)]
    pub hardware: String, // "p150" | "n300" | "Quietbox" | ...
    #[serde(default)]
    pub status: String, // "Supported" | "Experimental" | "Not Supported"
}

/// One catalog model (only the fields the screensaver renders).
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogModel {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub model_size: String,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub compatibility: Vec<Compat>,
}

#[derive(Deserialize)]
struct Catalog {
    #[serde(default)]
    models: Vec<CatalogModel>,
}

/// Parse `compatibility.json`. Tolerant: any parse failure → empty vec (never panics).
pub fn parse_catalog(json: &str) -> Vec<CatalogModel> {
    serde_json::from_str::<Catalog>(json)
        .map(|c| c.models)
        .unwrap_or_default()
}

/// The compiled-in snapshot — the always-available offline floor.
pub fn bundled() -> &'static str {
    include_str!("../../assets/compatibility.snapshot.json")
}

/// Support level of a model on a given chip_set (best across its entries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Supported,
    Experimental,
    No,
}

pub fn model_support(m: &CatalogModel, chip_set: &str) -> Support {
    let mut best = Support::No;
    for c in &m.compatibility {
        if c.chip_set.eq_ignore_ascii_case(chip_set) {
            match c.status.as_str() {
                "Supported" => return Support::Supported,
                "Experimental" => best = Support::Experimental,
                _ => {}
            }
        }
    }
    best
}
```

- [ ] **Step 5: Wire `mod.rs`** — add `pub mod model_catalog;` and `pub use model_catalog::{parse_catalog, bundled, model_support, CatalogModel, Support};` to `src/workload/mod.rs` (cross-platform — NOT cfg-gated).

- [ ] **Step 6: Run tests + clippy + macOS cross-check (real exit codes) — pass. Commit.**

```bash
git add assets/compatibility.snapshot.json src/workload/model_catalog.rs src/workload/mod.rs
git commit -m "feat(workload): bundled model-compatibility catalog + parse + hardware matcher"
```

---

### Task 2: CatalogRefresher (background curl → arc-swap)

**Files:** Modify `src/workload/model_catalog.rs`, `src/workload/mod.rs`.

**Interfaces:** Consumes `parse_catalog`, `bundled`. Produces `CatalogRefresher::spawn() -> Self`, `CatalogRefresher::snapshot(&self) -> std::sync::Arc<Vec<CatalogModel>>`, and a pure `catalog_cache_path() -> std::path::PathBuf`.

- [ ] **Step 1: Failing test** for the seed + cache path (in `model_catalog.rs` tests):

```rust
    #[test]
    fn refresher_seeds_from_bundled_immediately() {
        let r = CatalogRefresher::spawn();
        // The bundled catalog is available synchronously at construction —
        // no waiting on the background curl.
        assert!(!r.snapshot().is_empty());
    }
    #[test]
    fn cache_path_is_under_tt_toplike() {
        assert!(catalog_cache_path().ends_with("tt-toplike/compatibility.json"));
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement** (append to `model_catalog.rs`), mirroring `InferenceServerMonitor`'s thread + `ArcSwap` pattern:

```rust
use arc_swap::ArcSwap;
use std::process::Command;
use std::sync::Arc;

const CATALOG_URL: &str = "https://d1oi7xemha0dsy.cloudfront.net/data/compatibility.json";
/// Re-fetch at most this often (catalog changes slowly).
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

pub fn catalog_cache_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tt-toplike")
        .join("compatibility.json")
}

/// Background-refreshed model catalog. Seeds from the bundled snapshot (and the
/// on-disk cache if newer/valid), then curls the live URL on a slow cadence.
pub struct CatalogRefresher {
    cache: Arc<ArcSwap<Vec<CatalogModel>>>,
}

impl CatalogRefresher {
    pub fn spawn() -> Self {
        // Seed synchronously: prefer a valid on-disk cache, else the bundled floor.
        let seed = std::fs::read_to_string(catalog_cache_path())
            .ok()
            .map(|s| parse_catalog(&s))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| parse_catalog(bundled()));
        let cache = Arc::new(ArcSwap::from_pointee(seed));
        let cache_bg = Arc::clone(&cache);
        let _ = std::thread::Builder::new()
            .name("tt-catalog-refresh".into())
            .spawn(move || loop {
                if let Some(json) = curl_fetch(CATALOG_URL) {
                    let models = parse_catalog(&json);
                    if !models.is_empty() {
                        // Best-effort cache write (ignore errors).
                        let path = catalog_cache_path();
                        if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        let _ = std::fs::write(&path, &json);
                        cache_bg.store(Arc::new(models));
                    }
                }
                std::thread::sleep(REFRESH_INTERVAL);
            });
        CatalogRefresher { cache }
    }

    pub fn snapshot(&self) -> Arc<Vec<CatalogModel>> {
        self.cache.load_full()
    }
}

/// Fetch a URL via `curl` (dep-free HTTPS). `None` on missing curl / error / non-2xx.
fn curl_fetch(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args(["-fsS", "--max-time", "20", url])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}
```
(`dirs` is already a dependency — used by `AnimConfig`.) Add `pub use model_catalog::CatalogRefresher;` to `src/workload/mod.rs`.

- [ ] **Step 4: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/workload/model_catalog.rs src/workload/mod.rs
git commit -m "feat(workload): CatalogRefresher — background curl refresh + arc-swap (bundled seed)"
```

---

### Task 3: ModelStarfield animation

**Files:** Create `src/animation/model_starfield.rs`; modify `src/animation/mod.rs`.

**Interfaces:** Consumes `CatalogModel`, `Support`, `model_support`. Produces `ModelStarfield::new(width: usize, height: usize)`, `update(&mut self, models: &[CatalogModel], chip_set: &str)`, `render(&self) -> Vec<ratatui::text::Line<'static>>`, and a pure `compatible_count(models: &[CatalogModel], chip_set: &str) -> usize`.

- [ ] **Step 1: Failing test** in `model_starfield.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::model_catalog::{CatalogModel, Compat};

    fn model(name: &str, chip: &str, status: &str) -> CatalogModel {
        CatalogModel {
            display_name: name.into(), family: String::new(), model_size: String::new(),
            tasks: vec![], compatibility: vec![Compat { chip_set: chip.into(), hardware: String::new(), status: status.into() }],
        }
    }
    #[test]
    fn footer_counts_compatible_and_stars_track_models() {
        let models = vec![
            model("A", "Blackhole", "Supported"),
            model("B", "Blackhole", "Experimental"),
            model("C", "Wormhole", "Supported"),  // not Blackhole
        ];
        assert_eq!(compatible_count(&models, "Blackhole"), 2);
        let mut sf = ModelStarfield::new(80, 24);
        sf.update(&models, "Blackhole");
        assert_eq!(sf.star_count(), 3, "one star per model");
        let text: String = sf.render().iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        assert!(text.contains("2 of 3"), "footer shows compatible/total");
        assert!(text.contains("Blackhole"));
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `model_starfield.rs`** (SPDX header). A star per model with a drift position (seed via `rand`, move each `update`, wrap at edges); render places the model's `display_name` (+ `model_size` if present) at its cell; `model_support` picks the color (Supported → green, Experimental → amber, No → dim gray) and whether it "twinkles" (brightness from frame + per-star phase). Reuse `crate::animation::common` color helpers (`hsv_to_rgb`) where natural. The last rendered line is the footer: `"{compatible} of {total} models run on your {chip_set}"`. Expose `star_count(&self) -> usize` and the pure `compatible_count`. Keep positions frame/`rand`-driven but assert only counts + footer in tests (not exact coords). Mirror the file shape of `src/animation/starfield.rs` (new/update/render + a private `Star` struct); do NOT assert on random positions.

- [ ] **Step 4: Wire `src/animation/mod.rs`** — `pub mod model_starfield;` + `pub use model_starfield::ModelStarfield;`.

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/model_starfield.rs src/animation/mod.rs
git commit -m "feat(animation): ModelStarfield — cold-trail model catalog screensaver"
```

---

### Task 4: Cold-trail activation in the `[i]` view

**Files:** Modify `src/ui/tui/mod.rs`; add a pure `trail_is_cold` to `src/ui/tui/inference_panel.rs`; add `curl` to `debian/control` Recommends.

**Interfaces:** Consumes `CatalogRefresher`, `ModelStarfield`, `trail_is_cold`. Produces `inference_panel::trail_is_cold(snapshot: &[ServiceState]) -> bool`.

- [ ] **Step 1: Failing test** for `trail_is_cold` (in `inference_panel.rs`, using the `base()` helper):

```rust
    #[test]
    fn trail_is_cold_only_when_nothing_running() {
        let mut down = base(); down.phase = Phase::Down;
        assert!(trail_is_cold(&[down.clone()]));
        assert!(trail_is_cold(&[]));
        let mut loading = base(); loading.phase = Phase::Loading;
        assert!(!trail_is_cold(&[down, loading]));
        let mut ready = base(); ready.phase = Phase::Ready;
        assert!(!trail_is_cold(&[ready]));
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `trail_is_cold`** in `inference_panel.rs`:

```rust
/// The inference "trail" is cold when no service is doing anything — used to
/// swap the [i] view to the model-starfield screensaver. Compiling/Loading/Ready
/// means a model is present, so the live list is shown instead.
pub fn trail_is_cold(snapshot: &[ServiceState]) -> bool {
    !snapshot.iter().any(|s| matches!(s.phase, Phase::Compiling | Phase::Loading | Phase::Ready))
}
```

- [ ] **Step 4: Wire the view** in `src/ui/tui/mod.rs`:
  - Near the other background monitors (~line 265): `let catalog_refresher = crate::workload::CatalogRefresher::spawn();` (cross-platform — not cfg-gated).
  - Add loop state: `let mut model_starfield = crate::animation::ModelStarfield::new(0, 0);` (resized on use).
  - In the `DisplayMode::InferenceMonitor` render arm: obtain the raw snapshot (Linux: `inference_monitor.snapshot()`; non-Linux: empty). If `inference_panel::trail_is_cold(&snapshot)`, resize + `model_starfield.update(&catalog_refresher.snapshot(), arch_str)` and render it (where `arch_str` = `backend.devices().first().map(|d| d.architecture.name()).unwrap_or("unknown")` — use the existing arch name accessor); otherwise render the live `render_inference_monitor(rows, service_cursor)` as today. Update the starfield each frame only while cold + in this view (mirror how other animations are updated per-frame in their mode arm).

- [ ] **Step 5:** `debian/control` — append `curl` to the `Recommends:` line of the `tt-toplike` package.

- [ ] **Step 6:** Run tests + `RUSTFLAGS="-D warnings"` build + clippy + macOS cross-check (real exit codes). Manual: `tt-toplike`, press `i` with nothing running → starfield with your hardware's models bright + "N of M run on your Blackhole"; start a model → switches to the live list. **Commit.**

---

## Self-Review

**Spec coverage:** bundled snapshot floor → Task 1; background curl refresh + arc-swap → Task 2; starfield visual (per-model stars, hw-compatible bright/colored, footer) → Task 3; cold-trail activation (screensaver when no Compiling/Loading/Ready, live list otherwise) → Task 4; hardware match via chip_set → Task 1 `model_support` + Task 4 arch; curl dep → Task 4 Recommends; no-TLS-dep/no-new-deps → Global Constraints + curl shell; error handling (offline/malformed → last-good, bundled floor) → Tasks 1/2; testing → each task's pure tests. ✓

**Placeholder scan:** Task 3 Step 3 and Task 4 Step 4 describe the starfield rendering internals and the render-arm wiring against exact anchors + the existing `starfield.rs`/animation-arm patterns rather than pasting a full ratatui render body; the pure/error-prone pieces (`parse_catalog`, `model_support`, `compatible_count`, `trail_is_cold`, `catalog_cache_path`, refresher seeding) have complete code + tests. Acceptable per "follow existing patterns."

**Type consistency:** `CatalogModel`/`Compat`/`parse_catalog`/`bundled`/`Support`/`model_support`, `CatalogRefresher::{spawn,snapshot}`/`catalog_cache_path`/`curl_fetch`, `ModelStarfield::{new,update,render,star_count}`/`compatible_count`, `trail_is_cold` — consistent across Tasks 1–4. `Architecture::name()` accessor: confirm the exact method name in `src/models/device.rs` (the strings "Blackhole"/"Wormhole"/"Grayskull" are there) and use it in Task 4.
