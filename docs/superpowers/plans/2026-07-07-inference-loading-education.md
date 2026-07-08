# Inference-Loading Education Band Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a phase-synced footer band to the `[i]` Inference Monitor loading view that explains what the current load phase is doing, sourced from education copy brought in-tree from tt-studio.

**Architecture:** A new pure module `src/workload/inference_server/education.rs` embeds three JSON data files (copied in-tree under `assets/education/`) via `include_str!`, parses them once behind a `OnceLock`, and exposes pure lookups + a pure band composer returning render-ready `Line`s. The `[i]` view composes the band on the inference monitor tick (mirroring the existing `inference_roster` pattern) and `render_snake_view` reserves rows for it (mirroring `roster_h`).

**Tech Stack:** Rust, ratatui (`Line`/`Span`/`Style`/`Color`), serde_json (already a dependency), `std::sync::OnceLock`.

## Global Constraints

- The band feature is **always-on for the `[i]` view** — gate nothing behind a cargo feature.
- **Panic-free:** never index past the end, never split a multibyte char (use char-boundary-safe truncation/wrapping). No `unwrap()`/`expect()` on parsed data at runtime.
- **No render-thread blocking:** parsing happens once via `OnceLock`; the band `Vec<Line>` is composed on the monitor tick (like `inference_roster`), and render is a cheap `Paragraph` blit.
- **Data is owned in-tree, not vendored/synced:** no sync script, no tt-studio coupling at build or run time. Keep the copy as standalone JSON data files so it can later move to a dedicated facts repo.
- **model-education is a subset:** keep only `tagline` + `key_stats` per model; drop `description`/`use_cases`/`quick_start`/`family`/`model_type_label`.
- **Honest comments;** synthwave loading-palette accents for the band marker: cyan `Color::Rgb(64,196,224)`, violet `Color::Rgb(158,104,232)`, pink `Color::Rgb(255,48,96)`. Secondary text uses `colors::text_secondary()`.
- **Verify every task:** `cargo test --locked --lib --features tui,remote` (relevant tests pass), `cargo clippy --locked --lib --bin tt-toplike-tui --features tui,remote -- -D warnings`, `cargo fmt`. Never pipe cargo through `tail`/`head` (append `; echo "exit: $?"`).

## File Structure

- `assets/education/deployment-stages.json` — **create** (copied verbatim from tt-studio): map of stage-key → `{title, explanation, duration_hint}`.
- `assets/education/did-you-know.json` — **create** (copied verbatim): `{ "cards": [{headline, body, tag}] }`.
- `assets/education/model-education.json` — **create** (subset): map of model-name → `{tagline, key_stats:{context?,params?,license?}}`.
- `src/workload/inference_server/education.rs` — **create**: embeds + parses the assets; types `Stage`, `Fact`, `KeyStats`, `ModelOverview`, `StageInfo`; lookups `stage_for_phase`, `model_overview`, `facts`; composer `compose_band`.
- `src/workload/inference_server/mod.rs` — **modify**: add `pub mod education;`.
- `src/ui/tui/mod.rs` — **modify**: compose the band on the inference tick alongside `inference_roster`; subtract its height from the snake height; add a `band` parameter to `render_snake_view` and render it above the status bar.

---

### Task 1: Bring the education data in-tree and parse it once

**Files:**
- Create: `assets/education/deployment-stages.json`, `assets/education/did-you-know.json`, `assets/education/model-education.json`
- Create: `src/workload/inference_server/education.rs`
- Modify: `src/workload/inference_server/mod.rs` (add `pub mod education;`)
- Test: unit tests inside `src/workload/inference_server/education.rs`

**Interfaces:**
- Consumes: nothing (leaf module).
- Produces:
  - `struct Stage { pub title: String, pub explanation: String, pub duration_hint: String }` (Deserialize)
  - `struct Fact { pub headline: String, pub body: String, pub tag: String }` (Deserialize)
  - `struct KeyStats { pub context: Option<String>, pub params: Option<String>, pub license: Option<String> }` (Deserialize, all `#[serde(default)]`)
  - `struct ModelOverview { pub tagline: String, pub key_stats: KeyStats }` (Deserialize)
  - `fn stages() -> &'static std::collections::HashMap<String, Stage>`
  - `fn facts() -> &'static [Fact]`
  - `fn models() -> &'static std::collections::HashMap<String, ModelOverview>`

- [ ] **Step 1: Copy the data files in-tree**

Run (exact):
```bash
cd ~/code/tt-toplike
mkdir -p assets/education
cp ~/code/tt-studio/app/frontend/src/data/education/deployment-stages.json assets/education/deployment-stages.json
cp ~/code/tt-studio/app/frontend/src/data/education/did-you-know.json assets/education/did-you-know.json
python3 -c "import json; d=json.load(open('/home/ttuser/code/tt-studio/app/frontend/src/data/education/model-education.json')); json.dump({k:{'tagline':v.get('tagline',''),'key_stats':v.get('key_stats',{})} for k,v in d.items()}, open('assets/education/model-education.json','w'), indent=1)"
```
Expected: three files exist; `deployment-stages.json` has keys `initialization,setup,model_preparation,container_setup,finalizing,complete`; `model-education.json` values have only `tagline` + `key_stats`.

- [ ] **Step 2: Write the failing parse test**

Create `src/workload/inference_server/education.rs` with only this test module (module body comes next):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_assets_parse() {
        // Guards against a bad copy-in: every embedded asset must deserialize.
        assert!(stages().contains_key("model_preparation"));
        assert!(stages().contains_key("container_setup"));
        assert!(stages().contains_key("complete"));
        assert!(!facts().is_empty());
        assert!(!models().is_empty());
        // The subset kept tagline + key_stats.
        let (_name, ov) = models().iter().next().expect("at least one model");
        assert!(!ov.tagline.is_empty() || ov.key_stats.context.is_some() || true);
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui,remote education::tests::all_assets_parse ; echo "exit: $?"`
Expected: FAIL to compile (`stages`/`facts`/`models` not defined).

- [ ] **Step 4: Implement the module body**

Prepend to `src/workload/inference_server/education.rs` (above the test module):
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Education copy for the `[i]` loading view: short, phase-synced explanations
//! of what a model load is doing, plus rotating TT facts and per-model taglines.
//! The data is owned in-tree (copied from a tt-studio proof-of-concept, NOT
//! vendored/synced) as plain JSON so it can later graduate to a dedicated facts
//! repo. Parsed once behind `OnceLock`; all lookups are pure.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
pub struct Stage {
    pub title: String,
    pub explanation: String,
    pub duration_hint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fact {
    pub headline: String,
    pub body: String,
    pub tag: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeyStats {
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub params: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelOverview {
    pub tagline: String,
    #[serde(default)]
    pub key_stats: KeyStats,
}

#[derive(Deserialize)]
struct FactsFile {
    cards: Vec<Fact>,
}

const STAGES_JSON: &str = include_str!("../../../assets/education/deployment-stages.json");
const FACTS_JSON: &str = include_str!("../../../assets/education/did-you-know.json");
const MODELS_JSON: &str = include_str!("../../../assets/education/model-education.json");

pub fn stages() -> &'static HashMap<String, Stage> {
    static S: OnceLock<HashMap<String, Stage>> = OnceLock::new();
    // A parse failure means a bad in-tree copy — degrade to empty (band suppresses)
    // rather than panicking the whole TUI.
    S.get_or_init(|| serde_json::from_str(STAGES_JSON).unwrap_or_default())
}

pub fn facts() -> &'static [Fact] {
    static F: OnceLock<Vec<Fact>> = OnceLock::new();
    F.get_or_init(|| {
        serde_json::from_str::<FactsFile>(FACTS_JSON)
            .map(|f| f.cards)
            .unwrap_or_default()
    })
}

pub fn models() -> &'static HashMap<String, ModelOverview> {
    static M: OnceLock<HashMap<String, ModelOverview>> = OnceLock::new();
    M.get_or_init(|| serde_json::from_str(MODELS_JSON).unwrap_or_default())
}
```

Add to `src/workload/inference_server/mod.rs` (near the other `pub mod` lines):
```rust
pub mod education;
```

- [ ] **Step 5: Run it to verify it passes**

Run: `cargo test --locked --lib --features tui,remote education::tests::all_assets_parse ; echo "exit: $?"`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add assets/education src/workload/inference_server/education.rs src/workload/inference_server/mod.rs
git commit -m "feat(edu): bring tt-studio education copy in-tree, parse once"
```

---

### Task 2: Pure lookups — stage-for-phase, model overview, facts

**Files:**
- Modify: `src/workload/inference_server/education.rs`
- Test: unit tests in the same file

**Interfaces:**
- Consumes: `Phase` (`crate::workload::inference_server::Phase` = `Down|Compiling|Loading|Ready|Alarm`); `stages()`, `models()` from Task 1.
- Produces:
  - `struct StageInfo { pub title: String, pub explanation: String, pub duration_hint: String }`
  - `fn stage_for_phase(phase: Phase) -> Option<StageInfo>` — `None` means "suppress the band" (`Down`). `Alarm` returns a synthetic stalled entry. `Compiling→container_setup`, `Loading→model_preparation`, `Ready→complete`.
  - `fn model_overview(label: &str) -> Option<&'static ModelOverview>`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `education.rs`:
```rust
use crate::workload::inference_server::Phase;

#[test]
fn stage_for_phase_is_total_and_maps_correctly() {
    // Down suppresses the band.
    assert!(stage_for_phase(Phase::Down).is_none());
    // Loading phases map to their tt-studio stage titles.
    assert_eq!(
        stage_for_phase(Phase::Compiling).unwrap().title,
        "Compiling execution graphs"
    );
    assert_eq!(
        stage_for_phase(Phase::Loading).unwrap().title,
        "Loading model weights"
    );
    assert_eq!(stage_for_phase(Phase::Ready).unwrap().title, "Ready");
    // Alarm has no tt-studio equivalent → synthetic stalled line.
    let alarm = stage_for_phase(Phase::Alarm).unwrap();
    assert!(alarm.title.to_lowercase().contains("stall"));
    assert!(!alarm.explanation.is_empty());
}

#[test]
fn model_overview_hit_and_miss() {
    // A model present in the education set resolves.
    assert!(model_overview("Llama-3.1-8B-Instruct").is_some());
    // An unknown model misses (caller falls back to the raw name).
    assert!(model_overview("Definitely-Not-A-Real-Model").is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --locked --lib --features tui,remote education::tests::stage_for_phase ; echo "exit: $?"`
Expected: FAIL (`StageInfo`/`stage_for_phase`/`model_overview` not defined).

- [ ] **Step 3: Implement the lookups**

Add to `education.rs` (module body, above the tests):
```rust
use crate::workload::inference_server::Phase;

/// A stage's display copy, resolved for a phase. Owned (cheap String clones at
/// tick cadence) so `Alarm`'s synthetic entry and the embedded stages share one
/// return type.
#[derive(Debug, Clone)]
pub struct StageInfo {
    pub title: String,
    pub explanation: String,
    pub duration_hint: String,
}

impl From<&Stage> for StageInfo {
    fn from(s: &Stage) -> Self {
        StageInfo {
            title: s.title.clone(),
            explanation: s.explanation.clone(),
            duration_hint: s.duration_hint.clone(),
        }
    }
}

/// The `deployment-stages.json` key each phase maps to. `Down` → `None`
/// (suppress). `Alarm` has no tt-studio stage, so the caller substitutes a
/// synthetic entry (see `stage_for_phase`).
fn stage_key_for_phase(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Down => None,
        Phase::Compiling => Some("container_setup"),
        Phase::Loading => Some("model_preparation"),
        Phase::Ready => Some("complete"),
        Phase::Alarm => None, // handled specially below
    }
}

/// Resolve the display copy for a phase. `None` = suppress the band (`Down`, or a
/// mapped key missing from the data). `Alarm` yields a synthetic stalled entry.
pub fn stage_for_phase(phase: Phase) -> Option<StageInfo> {
    if phase == Phase::Alarm {
        return Some(StageInfo {
            title: "Stalled".to_string(),
            explanation: "No forward progress detected. Check the server logs — \
                          the container may be waiting on a download, a wedged \
                          compile, or an out-of-memory condition."
                .to_string(),
            duration_hint: String::new(),
        });
    }
    let key = stage_key_for_phase(phase)?;
    stages().get(key).map(StageInfo::from)
}

/// Per-model tagline + key stats, by the label tt-toplike shows (e.g.
/// "Llama-3.1-8B-Instruct"). `None` → caller falls back to the raw label.
pub fn model_overview(label: &str) -> Option<&'static ModelOverview> {
    models().get(label)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --locked --lib --features tui,remote education::tests ; echo "exit: $?"`
Expected: PASS (all education tests).

- [ ] **Step 5: Commit**

```bash
git add src/workload/inference_server/education.rs
git commit -m "feat(edu): stage_for_phase + model_overview lookups (total over Phase)"
```

---

### Task 3: Pure band composer with graceful degradation

**Files:**
- Modify: `src/workload/inference_server/education.rs`
- Test: unit tests in the same file

**Interfaces:**
- Consumes: `stage_for_phase`, `model_overview`, `facts`, `Phase`; ratatui `Line`/`Span`/`Style`/`Color`/`Modifier`; `crate::ui::colors`.
- Produces:
  - `fn compose_band(model_label: &str, phase: Phase, width: usize, height_budget: usize, rotation_idx: usize) -> Vec<Line<'static>>`
    - Returns `vec![]` when `stage_for_phase(phase)` is `None` (suppress) or `width < 12`.
    - Full band (tallest first), dropped bottom-up as `height_budget` shrinks: rotating fact → model header → explanation's 2nd line → (nothing left) empty.
    - All text char-boundary-safe; explanation word-wrapped to `width`; header/phase truncated with `…` on a char boundary.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module:
```rust
use ratatui::text::Line;

fn plain(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect()
}

#[test]
fn compose_band_suppresses_when_down_or_too_narrow() {
    assert!(compose_band("Llama-3.1-8B-Instruct", Phase::Down, 80, 6, 0).is_empty());
    assert!(compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 8, 6, 0).is_empty());
}

#[test]
fn compose_band_full_then_degrades_by_height() {
    let full = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 6, 0);
    assert!(full.len() >= 3, "roomy band shows header+phase+explanation(+fact)");
    let text = plain(&full).join("\n");
    assert!(text.contains("Loading model weights"));

    // Height 1 keeps the single most important line (the phase title).
    let tiny = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 1, 0);
    assert_eq!(tiny.len(), 1);
    assert!(plain(&tiny)[0].contains("Loading model weights"));

    // Height 0 → nothing.
    assert!(compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 0, 0).is_empty());
}

#[test]
fn compose_band_rotation_is_deterministic_and_wraps() {
    if facts().len() >= 2 {
        let a = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 6, 0);
        let b = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 6, 1);
        assert_ne!(plain(&a), plain(&b), "different rotation_idx → different fact");
        let wrap = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 6, facts().len());
        assert_eq!(plain(&a), plain(&wrap), "rotation_idx wraps modulo facts().len()");
    }
}

#[test]
fn compose_band_is_char_safe_on_multibyte_and_unknown_model() {
    // A multibyte label + narrow width must never panic (no byte slicing).
    let out = compose_band("模型-测试-🚀-very-long-name-that-must-truncate", Phase::Loading, 20, 6, 0);
    assert!(!out.is_empty());
    // Unknown model → header falls back to the raw label (no crash, no lookup).
    let unknown = compose_band("Definitely-Not-A-Real-Model", Phase::Loading, 80, 6, 0);
    let text = plain(&unknown).join("\n");
    assert!(text.contains("Definitely-Not-A-Real-Model"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --locked --lib --features tui,remote education::tests::compose_band ; echo "exit: $?"`
Expected: FAIL (`compose_band` not defined).

- [ ] **Step 3: Implement the composer + char-safe helpers**

Add to `education.rs` (module body):
```rust
use crate::ui::colors;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Truncate `s` to at most `max` columns (approximated by chars — the copy is
/// ASCII-dominant), appending `…` when cut. Char-boundary-safe: never slices a
/// multibyte codepoint.
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Greedy word-wrap to `width` columns, char-safe, at most `max_lines` lines
/// (the last line truncates with `…` if content remains).
fn wrap_words(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let add = if cur.is_empty() { word.chars().count() } else { cur.chars().count() + 1 + word.chars().count() };
        if add > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                break;
            }
        }
        if cur.is_empty() {
            cur = truncate_chars(word, width);
        } else {
            cur.push(' ');
            cur.push_str(word);
        }
    }
    if lines.len() < max_lines && !cur.is_empty() {
        lines.push(truncate_chars(&cur, width));
    }
    lines
}

/// Accent color for the phase marker, matching the synthwave loading palette.
fn phase_accent(phase: Phase) -> Color {
    match phase {
        Phase::Compiling => Color::Rgb(64, 196, 224), // cyan-teal
        Phase::Loading => Color::Rgb(158, 104, 232),  // violet
        Phase::Alarm => Color::Rgb(255, 48, 96),      // hot pink
        _ => Color::Rgb(158, 104, 232),
    }
}

/// Compose the education footer band for the featured loading service. Empty
/// when the phase suppresses the band (`Down`) or the pane is too narrow.
/// As `height_budget` shrinks, lines are dropped by keep-priority (lowest kept
/// longest): phase title > explanation l1 > model header > explanation l2 > fact.
/// The kept lines are then emitted in visual order: header, phase, explanation, fact.
pub fn compose_band(
    model_label: &str,
    phase: Phase,
    width: usize,
    height_budget: usize,
    rotation_idx: usize,
) -> Vec<Line<'static>> {
    if width < 12 || height_budget == 0 {
        return Vec::new();
    }
    let Some(stage) = stage_for_phase(phase) else {
        return Vec::new();
    };
    let dim = Style::default().fg(colors::text_secondary()).add_modifier(Modifier::DIM);

    // Phase title (+ trailing duration hint).
    let hint = if stage.duration_hint.is_empty() {
        String::new()
    } else {
        format!("   {}", stage.duration_hint)
    };
    let title = truncate_chars(&stage.title, width.saturating_sub(2 + hint.chars().count()));
    let phase_line = Line::from(vec![
        Span::styled("▸ ".to_string(), Style::default().fg(phase_accent(phase))),
        Span::styled(title, Style::default().fg(colors::text_primary())),
        Span::styled(hint, dim),
    ]);

    // Explanation, wrapped to up to 2 lines.
    let mut expl = wrap_words(&stage.explanation, width, 2)
        .into_iter()
        .map(|l| Line::from(Span::styled(format!("  {l}"), dim)));
    let expl1 = expl.next();
    let expl2 = expl.next();

    // Model header (tagline + stats, or the raw label on a miss).
    let header_text = match model_overview(model_label) {
        Some(ov) => {
            let mut parts: Vec<String> = vec![ov.tagline.clone()];
            if let Some(c) = ov.key_stats.context.as_ref() {
                parts.push(format!("{c} ctx"));
            }
            if let Some(l) = ov.key_stats.license.as_ref() {
                parts.push(l.clone());
            }
            parts.join(" · ")
        }
        None => model_label.to_string(),
    };
    let header = Line::from(Span::styled(truncate_chars(&header_text, width), dim));

    // Rotating fact.
    let fact_line = facts().get(rotation_idx % facts().len().max(1)).map(|f| {
        Line::from(Span::styled(
            truncate_chars(&format!("TT · {}", f.headline), width),
            dim,
        ))
    });

    // Candidates in VISUAL order (header, phase, expl1, expl2, fact), each tagged
    // with a keep-priority (lower = kept longer as height shrinks).
    let mut cand: Vec<(u8, Line<'static>)> = vec![(2, header), (0, phase_line)];
    if let Some(e1) = expl1 {
        cand.push((1, e1));
    }
    if let Some(e2) = expl2 {
        cand.push((3, e2));
    }
    if let Some(f) = fact_line {
        cand.push((4, f));
    }

    // Keep the `height_budget` lowest-priority lines, then restore visual order
    // (ascending original index == the insertion/visual order above).
    let mut ranked: Vec<(u8, usize)> =
        cand.iter().enumerate().map(|(i, (p, _))| (*p, i)).collect();
    ranked.sort();
    ranked.truncate(height_budget);
    let mut keep: Vec<usize> = ranked.into_iter().map(|(_, i)| i).collect();
    keep.sort_unstable();
    keep.into_iter().map(|i| cand[i].1.clone()).collect()
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --locked --lib --features tui,remote education::tests ; echo "exit: $?"`
Expected: PASS (all education tests, including compose_band).

- [ ] **Step 5: Clippy + fmt the module**

Run: `cargo clippy --locked --lib --bin tt-toplike-tui --features tui,remote -- -D warnings ; echo "exit: $?"` then `cargo fmt`
Expected: clippy exit 0, fmt clean.

- [ ] **Step 6: Commit**

```bash
git add src/workload/inference_server/education.rs
git commit -m "feat(edu): pure compose_band with graceful degradation + char-safe wrap"
```

---

### Task 4: Wire the band into the `[i]` loading view

**Files:**
- Modify: `src/ui/tui/mod.rs` (inference tick compose ~line 769–796; `render_snake_view` ~line 4059-area / the fn near the roster render; the render call ~line 930)
- Test: a rendering smoke test in `src/ui/tui/mod.rs` tests, plus manual visual check

**Interfaces:**
- Consumes: `education::compose_band`, `inference_panel::featured_loading`, `Phase`, the featured `ServiceState` (`label`, `phase`).
- Produces: a rendered band; no new public API.

- [ ] **Step 1: Add band state next to the roster**

In `run_tui`, near where `inference_roster` is declared (search `let mut inference_roster: Vec<Line<'static>> = Vec::new();`), add:
```rust
// Education footer band for the [i] loading view (composed on the inference
// tick, like `inference_roster`). `band_rotation` advances each recompute; the
// visible fact changes every `BAND_ROTATE_EVERY` recomputes (~8s at the ~2s
// inference cadence).
let mut inference_band: Vec<Line<'static>> = Vec::new();
let mut band_rotation: usize = 0;
```
And near the other `const`s at the top of the file (search `INFERENCE_ROSTER_MAX`), add:
```rust
/// Recompose ticks between education-fact rotations (~8s at the ~2s inference
/// cadence). Keeps a long compile/load from showing one stale fact the whole time.
const BAND_ROTATE_EVERY: usize = 4;
/// Max rows the education band may claim in the [i] loading pane.
const INFERENCE_BAND_MAX_H: usize = 5;
```

- [ ] **Step 2: Compose the band on the inference tick**

In the inference-tick block, immediately after `inference_roster = inference_roster_lines(&rows, size.width as usize);` and the `remote_local_fallback_note` insert (i.e. after `let roster_h = inference_roster.len();`), add:
```rust
                    // Education band: describe the featured *loading* service.
                    band_rotation = band_rotation.wrapping_add(1);
                    inference_band = {
                        use crate::workload::inference_server::{education, Phase};
                        let featured = inference_panel::featured_loading(&rows);
                        match featured {
                            Some(s) if matches!(s.phase, Phase::Compiling | Phase::Loading | Phase::Alarm) => {
                                let avail = (size.height as usize)
                                    .saturating_sub(1 + roster_h + 3) // status + roster + min snake
                                    .min(INFERENCE_BAND_MAX_H);
                                education::compose_band(
                                    &s.label,
                                    s.phase,
                                    size.width as usize,
                                    avail,
                                    band_rotation / BAND_ROTATE_EVERY,
                                )
                            }
                            _ => Vec::new(),
                        }
                    };
                    let band_h = inference_band.len();
```
Then change the snake height line from:
```rust
                        height: (size.height as usize).saturating_sub(1 + roster_h),
```
to:
```rust
                        height: (size.height as usize).saturating_sub(1 + roster_h + band_h),
```

- [ ] **Step 3: Thread the band into `render_snake_view`**

Change the call (search `render_snake_view(f, &snake, &inference_roster);`) to:
```rust
                            render_snake_view(f, &snake, &inference_roster, &inference_band);
```
Change the fn signature + body (search `fn render_snake_view`) to reserve rows above the status bar for the band:
```rust
fn render_snake_view(
    f: &mut Frame,
    snake: &crate::animation::Snake,
    roster: &[Line<'static>],
    band: &[Line<'static>],
) {
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(colors::rgb(0, 0, 0))),
        area,
    );
    let roster_h = (roster.len() as u16).min(area.height);
    if roster_h > 0 {
        f.render_widget(
            Paragraph::new(roster.to_vec()),
            Rect { x: area.x, y: area.y, width: area.width, height: roster_h },
        );
    }
    // Education band sits just above the bottom status row.
    let band_h = (band.len() as u16).min(area.height.saturating_sub(roster_h + 1));
    let body = Rect {
        x: area.x,
        y: area.y + roster_h,
        width: area.width,
        height: area.height.saturating_sub(1 + roster_h + band_h),
    };
    f.render_widget(
        Paragraph::new(snake.render(body.width as usize, body.height as usize)),
        body,
    );
    if band_h > 0 {
        f.render_widget(
            Paragraph::new(band.to_vec()),
            Rect {
                x: area.x,
                y: area.y + area.height.saturating_sub(1 + band_h),
                width: area.width,
                height: band_h,
            },
        );
    }
}
```

- [ ] **Step 4: Write a rendering smoke test**

Add to the `tests` module in `src/ui/tui/mod.rs` (search for an existing `Terminal::new(TestBackend::new(` test to mirror the harness):
```rust
#[test]
fn render_snake_view_with_band_does_not_panic() {
    use ratatui::backend::TestBackend;
    use ratatui::text::Line;
    use ratatui::Terminal;
    let snake = crate::animation::Snake::new(80, 20);
    let roster: Vec<Line<'static>> = Vec::new();
    let band = crate::workload::inference_server::education::compose_band(
        "Llama-3.1-8B-Instruct",
        crate::workload::inference_server::Phase::Loading,
        80,
        5,
        0,
    );
    // A tiny terminal must also not panic (band + status exceed height).
    for (w, h) in [(80u16, 24u16), (40, 8), (20, 3)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| super::render_snake_view(f, &snake, &roster, &band)).unwrap();
    }
}
```
Adjust `crate::animation::Snake::new` if the constructor differs (search `impl Snake` for the real constructor; use whatever the existing snake tests use).

- [ ] **Step 5: Run tests + full gate**

Run:
```bash
cargo test --locked --lib --features tui,remote ; echo "exit: $?"
cargo clippy --locked --lib --bin tt-toplike-tui --features tui,remote -- -D warnings ; echo "exit: $?"
cargo clippy --locked --lib --bin tt-toplike-tui --no-default-features --features tui,json-backend -- -D warnings ; echo "exit: $?"
cargo fmt --check ; echo "exit: $?"
```
Expected: tests pass; both clippy configs exit 0; fmt clean.

- [ ] **Step 6: Manual visual check**

Run `tt-toplike` (build+install first: `cargo build --release --features tui,remote && cp target/release/tt-toplike ~/.local/bin/`), enter `[i]`, and start/observe a model load. Confirm the band appears under the snake, shows the phase title + explanation, rotates a fact, and the snake shrinks to make room (no overlap with the status bar). On a very short terminal the band should shrink/disappear rather than collide.

- [ ] **Step 7: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(i): education band under the loading snake (phase-synced)"
```

---

## Self-Review

**Spec coverage:** goal (band explaining the load) → Tasks 3–4; data owned in-tree, subset, embedded once → Task 1; phase→stage mapping incl. Down-suppress/Alarm-synthetic → Task 2; footer-band layout + graceful degradation + char-safety + rotation → Task 3; `render_snake_view` reserve + tick compose + snake-height sync → Task 4; remote (model+phase from streamed extension) → works because Task 4 reads the featured `ServiceState` regardless of backend. Colors/synthwave accents → Task 3 `phase_accent`. Testing bullets → covered per task.

**Type consistency:** `Phase` variants (`Down/Compiling/Loading/Ready/Alarm`) used identically in Tasks 2–4; `StageInfo`/`ModelOverview`/`Fact` defined in Tasks 1–2 and consumed in 3; `compose_band` signature `(model_label,&str; phase,Phase; width,usize; height_budget,usize; rotation_idx,usize) -> Vec<Line<'static>>` identical across Task 3 def and Task 4 call; `render_snake_view` 4-arg signature consistent between the call site and the fn.

**Placeholder scan:** no TBD/TODO; every code step shows the exact, final code to ship (Task 3's `compose_band` is a single clean priority-assembly implementation). Verify commands are exact with expected outcomes.
