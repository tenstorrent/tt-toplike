# Arcade Duel + Dedup + Castle Compression + BBS Chrome Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Stage a telemetry-true hero-vs-snake duel in Arcade, de-duplicate the per-device telemetry Arcade prints three times, add a compact Memory-Castle tier before the fleet-grid fallback, and give the shared chrome a 1990s BBS/demoscene ANSI identity.

**Architecture:** A shared `bbs_rule` chrome helper in `animation/common.rs` restyles separators app-wide. `MemoryCastle::render` gains a three-tier dispatch (full/compact/fleet). The three embedded vizzes gain a `chrome` knob so Arcade renders ONE shared telemetry strip. The duel is a pure `duel.rs` module (balance + lunge state) rendered as a 1-row strip in Arcade's header, fed `ServingStats` threaded from the `[i]` monitor snapshot the TUI already holds.

**Tech Stack:** Rust, ratatui, existing `colors::rgb`/`hsv_to_rgb` (grayskull themes everything automatically). No new dependencies.

## Global Constraints

- **No new dependencies. Zero new probing** — the duel reads the existing `InferenceServerMonitor` snapshot + backend telemetry.
- **Telemetry-true**: every duel motion maps to a real signal (power-vs-baseline, tps, completed_delta, kv, throttle flag). Idle → resting, never scripted.
- **Standalone modes unchanged** by the dedup: `chrome` defaults to today's behavior; only Arcade passes the suppressed variant.
- **No right-side border characters** (project TUI rule): rules/frames end in a dither fade (`▓▒░`), never a right wall.
- **No panics**: char-safe truncation, width-0 guards, clamped/NaN-safe math, saturating arithmetic.
- **Cross-platform**: duel plumbing follows the existing pattern — snapshot read Linux-gated, everything else portable; off-Linux the duel strip is simply absent.
- **SPDX header** on new files.
- **Verify each task (real exit codes — NEVER pipe cargo through tail/head; append `; echo "exit: $?"`):** `cargo test --locked --lib --features tui <filter>`; `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings`; macOS cross-check `cargo clippy --target aarch64-apple-darwin --no-default-features --features tui,json-backend -- -D warnings` (skip w/ note if target absent). `cargo fmt` before each commit.

## Current-state anchors
- `animation/common.rs`: `BLOCK_CHARS = ['·','░','▒','▓','█']`, `value_to_char_intensity`, `hsv_to_rgb`; re-exported via `crate::animation::*`.
- `animation/arcade.rs`: layout budget comment at ~93-115 (Row 0 title, Row 1 topology, separators via `render_separator(label, w)`); `pub fn update(&mut self, backend: &dyn TelemetryBackend)` (:390); `pub fn render(&self, backend) -> Vec<Line<'static>>` (:520); hero expression flags `hero_throttled` etc. (:66-71); `AdaptiveBaseline` already imported (starfield uses it for spark flashes).
- `animation/memory_castle.rs`: `MIN_CHIP_COL_WIDTH: usize = 15` (:493); `if devices.len() > max_side_by_side { return self.render_fleet_grid(backend); }` (:495-497); full column render at :500-630 (device_info `" Dev{:<2} {:>3.0}W {:>3.0}°C "`, col_width math).
- Per-device telemetry duplicated in embedded sections — implementer MUST re-grep to locate exactly: `grep -n '°C' src/animation/{starfield,memory_castle,memory_flow}.rs` (known: castle device_info line; starfield per-device header ~:908; flow `🌡`/`⚡` header lines).
- `[i]` snapshot in `ui/tui/mod.rs`: the `should_tick` `DisplayMode::InferenceMonitor` arm reads `inference_monitor.snapshot()` (Linux-gated). Arcade's tick arm is `DisplayMode::Arcade => { ... arcade.update(backend) ... }` (grep `DisplayMode::Arcade =>`). `ServingStats` at `workload/inference_server/metrics.rs` (`generation_tps, completed_delta, requests_running, requests_waiting, kv_cache_usage`); `ServiceState.serving`; `Phase::Ready`.
- `AdaptiveBaseline` API: grep `impl AdaptiveBaseline` in `animation/baseline.rs` for the observe/deviation methods before using.

## File Structure
- `src/animation/common.rs` — `bbs_rule`, `bbs_title` chrome helpers (Task 1).
- `src/animation/arcade.rs` — chrome adoption (Task 1), shared telemetry strip (Task 3), duel strip render + state (Task 4).
- `src/animation/memory_castle.rs` — three-tier dispatch + compact column (Task 2), chrome knob (Task 3).
- `src/animation/starfield.rs`, `src/animation/memory_flow.rs` — chrome knob (Task 3).
- `src/animation/duel.rs` (new) — pure duel logic (Task 4).
- `src/ui/tui/mod.rs` — thread serving snapshot into Arcade update (Task 4).

---

### Task 1: BBS chrome helpers + adoption

**Files:** Modify `src/animation/common.rs`, `src/animation/arcade.rs`, `src/animation/serving_creature.rs` (region headers), `src/ui/tui/mod.rs` (render_snake_view title untouched — creature owns its title).

**Interfaces:** Produces `pub fn bbs_rule(label: &str, width: usize) -> String` and `pub fn bbs_title(text: &str, width: usize) -> String` in `common.rs` (re-exported via `crate::animation::*`).

- [ ] **Step 1: Failing tests** in `common.rs`:

```rust
    #[test]
    fn bbs_rule_boxes_label_and_fades_right() {
        let r = bbs_rule("STARFIELD", 40);
        assert_eq!(r.chars().count(), 40, "exact width");
        assert!(r.starts_with("╔══[ "), "double-line lead-in + label box");
        assert!(r.contains("STARFIELD ]"));
        assert!(r.ends_with("▓▒░"), "dither fade, never a right wall");
        // Degenerate widths: char-safe, exact width, no panic.
        assert_eq!(bbs_rule("STARFIELD", 6).chars().count(), 6);
        assert_eq!(bbs_rule("X", 0), "");
    }

    #[test]
    fn bbs_title_bookends_and_clamps() {
        let t = bbs_title("tt-toplike", 30);
        assert_eq!(t.chars().count(), 30);
        assert!(t.contains("▓ tt-toplike ▓") || t.contains("█ tt-toplike █"));
        assert_eq!(bbs_title("tt-toplike", 4).chars().count(), 4);
    }
```

- [ ] **Step 2: Run — fails.** `cargo test --locked --lib --features tui bbs_ ; echo "exit: $?"`

- [ ] **Step 3: Implement** in `common.rs` (near the char-set constants):

```rust
/// One 1990s-BBS-style horizontal rule: `╔══[ LABEL ]══════▓▒░` — double-line
/// lead, boxed label, rule body, and a dither fade on the right (this project
/// never draws right-side walls). Char-exact `width`; label truncated char-safe
/// when it doesn't fit; empty at width 0.
pub fn bbs_rule(label: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let fade = "▓▒░";
    let lead = "╔══[ ";
    let close = " ]";
    let fixed = lead.chars().count() + close.chars().count() + fade.chars().count();
    let label_room = width.saturating_sub(fixed);
    let label_cut: String = label.chars().take(label_room).collect();
    let mut s = String::new();
    s.push_str(lead);
    s.push_str(&label_cut);
    s.push_str(close);
    let used = s.chars().count() + fade.chars().count();
    for _ in used..width {
        s.push('═');
    }
    s.push_str(fade);
    // Degenerate widths: hard-clamp to exactly `width` chars.
    s.chars().take(width).collect()
}

/// BBS-style title bookends: `░▒▓ TEXT ▓▒░` centered-ish, padded/clamped to
/// exactly `width` chars.
pub fn bbs_title(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let core = format!("░▒▓ {text} ▓▒░");
    let mut s: String = core.chars().take(width).collect();
    let pad = width.saturating_sub(s.chars().count());
    for _ in 0..pad {
        s.push('▄');
    }
    s
}
```

- [ ] **Step 4: Adopt.** In `arcade.rs`, route `render_separator(label, ..)` output through `bbs_rule(label, self.width)` (keep the section colors). In `serving_creature.rs`, the swimlane header `"requests · N served"` and timeline label rows stay as-is EXCEPT prefix region headers via a dim `bbs_rule` only if a full row is already dedicated to them (do NOT add rows). Keep changes chrome-only — content untouched.

- [ ] **Step 5: Tests + clippy + macOS cross-check + fmt — pass. Commit** `feat(chrome): 1990s BBS ANSI rules + title bookends across arcade/[i] chrome`.

---

### Task 2: Memory Castle compact tier

**Files:** Modify `src/animation/memory_castle.rs`.

**Interfaces:** Produces `pub(crate) fn castle_tier(width: usize, devices: usize) -> CastleTier` with `#[derive(Debug, PartialEq)] pub(crate) enum CastleTier { Full, Compact, FleetGrid }`.

- [ ] **Step 1: Failing tests** (in `memory_castle.rs` tests):

```rust
    #[test]
    fn tier_dispatch_thresholds() {
        // 120 usable cols: 7 devices fit Full (>=15 each), 8..=14 Compact (>=8), 15+ FleetGrid.
        assert_eq!(castle_tier(122, 7), CastleTier::Full);
        assert_eq!(castle_tier(122, 8), CastleTier::Compact);
        assert_eq!(castle_tier(122, 14), CastleTier::Compact);
        assert_eq!(castle_tier(122, 16), CastleTier::FleetGrid);
        assert_eq!(castle_tier(0, 4), CastleTier::FleetGrid);
        assert_eq!(castle_tier(40, 1), CastleTier::Full);
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement.**

```rust
/// Per-device column budget for the full castle rendering (existing look).
const MIN_CHIP_COL_WIDTH: usize = 15;
/// Per-device budget for the compact tier: single-glyph walls, short label.
const MIN_COMPACT_COL_WIDTH: usize = 8;

/// Which rendering tier fits `devices` side-by-side in `width` columns.
/// Ladder: Full (>=15 cols each) → Compact (>=8) → FleetGrid.
pub(crate) fn castle_tier(width: usize, devices: usize) -> CastleTier {
    let usable = width.saturating_sub(2);
    if devices == 0 || usable == 0 {
        return CastleTier::FleetGrid;
    }
    let per = usable / devices;
    if per >= MIN_CHIP_COL_WIDTH {
        CastleTier::Full
    } else if per >= MIN_COMPACT_COL_WIDTH {
        CastleTier::Compact
    } else {
        CastleTier::FleetGrid
    }
}
```

  Replace the `:493-497` dispatch with a match on `castle_tier(self.width, devices.len())`: `Full` → existing path; `FleetGrid` → `render_fleet_grid`; `Compact` → a new `render_compact(backend)` clone of the full column loop with: device-info line `format!("D{} {:>3.0}W", idx, power)` (temp stays encoded in tower color), tower walls 1 glyph, DDR gate row 1 glyph per 2 channels (`(channels + 1) / 2`), per-device particle budget halved (never 0 — `.max(1)`). Bounds identical in style to the full path (all writes via the existing clamped col math). Add a render smoke test: 10 devices at width 122 renders non-empty and does not panic; 4 devices at width 30 (per=7) → fleet grid still works.

- [ ] **Step 4: Tests + clippy + macOS + fmt — pass. Commit** `feat(castle): compact tier before fleet-grid fallback (8-col towers)`.

---

### Task 3: Arcade telemetry dedup

**Files:** Modify `src/animation/{starfield,memory_castle,memory_flow,arcade}.rs`.

**Interfaces:** Each embedded viz gains `pub fn set_chrome(&mut self, chrome: bool)` (default `true` = today's standalone look; `false` suppresses its per-device power/temp header lines). Arcade produces one `telemetry_strip(backend, width) -> String` (private helper): `Dev0 92W 78°C 1.35GHz · Dev1 …`, char-safe, `…`-truncated.

- [ ] **Step 1: Locate every duplicate.** `grep -n '°C' src/animation/{starfield,memory_castle,memory_flow}.rs` — enumerate the per-device W/°C header lines in each (castle device_info; starfield ~:908; flow 🌡/⚡). List them in the report.

- [ ] **Step 2: Failing test** in `arcade.rs`:

```rust
    #[test]
    fn arcade_prints_each_device_telemetry_once() {
        let mut backend = crate::backend::mock::MockBackend::new(2);
        backend.init().unwrap();
        backend.update().unwrap();
        let mut a = ArcadeVisualization::new(120, 40);
        a.update(&backend);
        let text: String = a
            .render(&backend)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        // Device 0's wattage readout appears exactly once (the shared strip),
        // not once per embedded section (was 3x).
        let w_readouts = text.matches("°C").count();
        assert!(
            w_readouts <= backend.devices().len(),
            "expected ≤1 °C readout per device in arcade, got {w_readouts}"
        );
    }
```
  (Adjust MockBackend constructor to the real API — grep `impl MockBackend`.)

- [ ] **Step 3: Implement.** Add `chrome: bool` field (default `true`) + `set_chrome` to the three vizzes; wrap ONLY the per-device W/°C header emission in `if self.chrome`. Arcade's `new()` calls `set_chrome(false)` on its embedded instances and renders the shared `telemetry_strip` on Row 1 alongside/instead of the topology line (topology stays if it fits: strip left, topology right, or strip replaces blank). Standalone snapshot guard: add a test that a standalone `MemoryCastle` render still contains `°C`.

- [ ] **Step 4: Tests + clippy + macOS + fmt — pass. Commit** `feat(arcade): single shared telemetry strip; embedded sections drop duplicate W/°C headers`.

---

### Task 4: The duel — hero ⚔ snake

**Files:** Create `src/animation/duel.rs`; modify `src/animation/{mod,arcade}.rs`, `src/ui/tui/mod.rs`.

**Interfaces:** Produces `pub struct DuelState`, `DuelState::new()`, `pub fn duel_balance(hw: f32, serve: f32) -> f32`, `DuelState::update(&mut self, hw: f32, serve: f32, hero_lunge: bool, snake_lunge: bool, kv: f32)`, `DuelState::render_strip(&self, width: usize) -> String` (glyph row; Arcade colors it). Arcade gains `pub fn set_serving(&mut self, serving: Option<ServingStats>)`; TUI threads the first Ready service's `serving` in the `DisplayMode::Arcade` tick arm.

- [ ] **Step 1: Failing tests** in `duel.rs`:

```rust
    #[test]
    fn balance_is_clamped_and_signed() {
        assert!(duel_balance(1.0, 0.0) > 0.5, "hardware dominance → hero side (+)");
        assert!(duel_balance(0.0, 1.0) < -0.5, "serving demand → snake side (-)");
        assert_eq!(duel_balance(0.5, 0.5), 0.0);
        assert!(duel_balance(f32::NAN, 0.5).is_finite(), "NaN-safe");
        assert!((-1.0..=1.0).contains(&duel_balance(99.0, -5.0)), "clamped");
    }

    #[test]
    fn strip_stages_hero_snake_and_marker() {
        let mut d = DuelState::new();
        d.update(0.8, 0.3, true, false, 0.1);
        let s = d.render_strip(60);
        assert_eq!(s.chars().count(), 60);
        assert!(s.contains('@'), "hero present");
        assert!(s.contains('⚔'), "balance marker present");
        assert!(s.contains('▶') || s.contains('●'), "snake present");
        assert_eq!(d.render_strip(0), "");
        // Idle both sides → resting glyphs, no lunge offsets.
        let mut idle = DuelState::new();
        idle.update(0.0, 0.0, false, false, 0.0);
        let _ = idle.render_strip(40); // no panic; visual rest is manual-checked
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `duel.rs`** (SPDX header):

```rust
/// Signed tug-of-war balance in [-1, +1]: positive = hardware headroom is
/// winning (hero side), negative = serving demand dominates (snake side).
/// NaN-safe: non-finite inputs read as 0.
pub fn duel_balance(hw: f32, serve: f32) -> f32 {
    let h = if hw.is_finite() { hw.clamp(0.0, 1.0) } else { 0.0 };
    let s = if serve.is_finite() { serve.clamp(0.0, 1.0) } else { 0.0 };
    (h - s).clamp(-1.0, 1.0)
}
```
  `DuelState` fields: `frame: u64`, `balance_smooth: f32` (EMA toward `duel_balance`, factor 0.15), `hero_lunge_left: u32` / `snake_lunge_left: u32` (frame countdowns, ~12 frames, re-armed by the lunge flags), `kv: f32`. `render_strip(width)`: hero `@` at x = 2 (+2 during lunge, toward center); snake body `●●●▶` tail-anchored at the right edge, head advancing left during its lunge; `⚔` at `center + balance_smooth * (span/2)`, clamped between the two; idle (both 0) → hero `@` bobs via frame parity, snake shows a coiled `◉~` at rest, marker centered dim. All positions clamped to `[0, width)`; width < 12 → hero+marker+head only; width 0 → empty. Return a `String` of exactly `width` chars.

- [ ] **Step 4: Wire.** `animation/mod.rs`: `pub mod duel; pub use duel::{duel_balance, DuelState};`. In `arcade.rs`: store `serving: Option<ServingStats>` + `duel: DuelState`; in `update`, compute `hw` = mean over devices of `power/max_power_seen` (track a running max, `.max(1.0)` floor), `serve` = `min(1.0, tps/200.0)` blended `0.7/0.3` with `min(1.0, (running+waiting) as f32/8.0)`; `hero_lunge` = existing power-spike signal (reuse the baseline-deviation used for sparkle — grep it; if awkward, `power > 1.15 * baseline_mean`), `snake_lunge` = `completed_delta > 0`; call `self.duel.update(..)`. In `render`, when `self.serving.is_some()`, Row 1 becomes: duel strip (left half) + telemetry strip (right half, from Task 3) — else exactly the Task 3 layout. In `ui/tui/mod.rs` `DisplayMode::Arcade` tick arm: Linux-gated `inference_monitor.snapshot()`, find first `Phase::Ready` service, `arcade.set_serving(svc.serving)`; off-Linux `set_serving(None)`.

- [ ] **Step 5: Full suite + clippy + macOS + fmt + manual (arcade with serving model: duel strip live; without: unchanged) — pass. Commit** `feat(arcade): hero ⚔ snake duel strip — telemetry-true tug-of-war`.

---

## Self-Review

**Spec coverage:** duel signal table → Task 4 (balance/lunges/kv/idle/throttle-flag reuse noted); 1-row strip in header → Task 4 render wiring; zero new probing → snapshot threading; dedup one strip → Task 3 (chrome knob + strip + regression test ≤1 °C/device); castle ladder full→compact→fleet → Task 2 (`castle_tier` + compact render); BBS chrome (`bbs_rule` double-line + dither fade, `bbs_title` bookends, no right walls) → Task 1; grayskull compatibility → all colors via `colors::rgb`/`hsv_to_rgb`; error handling (absent serving → no strip; width guards; NaN) → Tasks 1/4 tests + Global Constraints. ✓

**Placeholder scan:** pure pieces (`bbs_rule`, `bbs_title`, `castle_tier`, `duel_balance`) fully coded + tested; compact-column, dedup gating, and duel strip internals specified by exact behavior + anchors with implementer discretion on canvas math (matches existing per-file idioms). MockBackend constructor + baseline-deviation API flagged for grep (unknown exact signatures — deliberate, with fallback given). No TBD.

**Type consistency:** `bbs_rule/bbs_title(&str, usize) -> String`; `CastleTier` + `castle_tier(usize, usize)`; `set_chrome(bool)`; `DuelState::{new,update,render_strip}` + `duel_balance(f32,f32)->f32`; `set_serving(Option<ServingStats>)` (ServingStats is Copy) consistent across Tasks 3/4.
