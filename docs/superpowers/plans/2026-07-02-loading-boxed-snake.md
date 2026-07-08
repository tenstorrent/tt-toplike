# Loading Boxed-Snake Visualization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** When an inference service is Compiling or Loading, take over the `[i]` view with a boxed-snake progress visualization of the cold→Ready journey — honest % when a baseline exists, live momentum otherwise, red when stalled.

**Architecture:** A pure presentation layer over the existing `ServiceState` (no new probing). A `featured_loading` selector makes the `[i]` view a tri-state (cold→starfield, loading→snake, ready-only→list). `LoadSnake` (a new animation) holds the journey state — three boxes (compile/load/ready), dual-mode fill, alarm, elapsed, and a brief Ready burst — split into a tested logic core and a tested renderer. The tri-state wiring lives in the `DisplayMode::InferenceMonitor` render arm, mirroring how the starfield is wired.

**Tech Stack:** Rust, ratatui, `std::time::Instant` (wall-clock elapsed only). No new dependencies.

## Global Constraints

- **No new dependencies.**
- **Zero new probing** — read only the existing `ServiceState` fields; do NOT modify `probe.rs`, `state.rs`, or `monitor.rs` except to widen `CADENCE_SECS`'s visibility (Task 1).
- **Honest progress** — show a `%` only when `ServiceState.progress` is `Some`; in momentum mode never display or imply a percentage, and momentum fill never reaches 1.0 on its own (only a phase change locks a box full).
- **No panics** — guard zero dims (`new(0,0)`), saturating arithmetic on `rss_bytes`/`kernel_count`, char-safe truncation of model names, clamp `progress` to `[0,1]`.
- **Cross-platform** — `featured_loading`, `LoadSnake`, and `render_load_snake_view` are portable, NOT cfg-gated; they compile on macOS/Windows. Only the existing Linux-gated `inference_monitor.snapshot()` read stays gated (the `inference_monitor_rows` computation already handles this).
- **SPDX header** on every new file.
- **Verify each task** (real exit codes — NEVER pipe cargo/clippy through `tail`/`head`; append `; echo "exit: $?"`):
  - `cargo test --locked --lib --features tui <filter> ; echo "exit: $?"`
  - `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings ; echo "exit: $?"`
  - macOS cross-check: `cargo clippy --target aarch64-apple-darwin --no-default-features --features tui,json-backend -- -D warnings ; echo "exit: $?"` (skip with a note if the target isn't installed).

## Current-state anchors
- `[i]` render arm (`src/ui/tui/mod.rs`): `should_tick` arm `DisplayMode::InferenceMonitor =>` (~line 535) recreates+updates `model_starfield`; `inference_monitor_rows: Vec<ServiceState>` computed ~line 622 (Linux snapshot when in this mode / empty otherwise / empty on non-Linux); draw arm `DisplayMode::InferenceMonitor =>` (~line 675) currently: `if trail_is_cold(&inference_monitor_rows) { render_model_starfield_view(f,&model_starfield) } else { render_inference_monitor_view(f,&inference_monitor_rows, service_cursor) }`. Loop state `model_starfield`/`model_starfield_dims` ~lines 407-408. `render_model_starfield_view` defined ~line 2978.
- `src/ui/tui/inference_panel.rs`: `trail_is_cold(&[ServiceState]) -> bool` (built), a `base() -> ServiceState` test helper (~line 282), `GIB`/`MIB` constants, `format_service_row`, `service_rows`. `Phase`/`ServiceState` imported from `crate::workload::inference_server`.
- `ServiceState` fields: `key: String`, `label: String`, `phase: Phase`, `cpu_pct: f32`, `rss_bytes: u64`, `rss_delta: i64`, `kernel_count: usize`, `kernel_delta: i64`, `safetensors_fds: usize`, `readiness: Readiness`, `top_proc: Option<String>`, `last_log: Option<String>`, `progress: Option<f32>`, `flat_ticks: u32`.
- `Phase = Down | Compiling | Loading | Ready | Alarm`.
- `crate::workload::inference_server` re-exports `is_alarm`, `Phase`, `ServiceState`, `Readiness`. `CADENCE_SECS` is a private const in `monitor.rs:42` (`= TICK_INTERVAL.as_secs() as u32`, currently 2).
- Existing animation shape to mirror: `src/animation/model_starfield.rs` (`new`/`update`/`render(&self)->Vec<Line<'static>>`, canvas-of-cells render, zero-dim guards); `src/animation/mod.rs` declares+re-exports each animation.

## File Structure
- `src/workload/inference_server/monitor.rs` — widen `CADENCE_SECS` to `pub const` (Task 1).
- `src/workload/inference_server/mod.rs` — re-export `CADENCE_SECS` (Task 1).
- `src/ui/tui/inference_panel.rs` — add `featured_loading` (Task 1).
- `src/animation/inference_load.rs` — new: `LoadSnake` logic (Task 2) + `render` & formatters (Task 3).
- `src/animation/mod.rs` — declare + re-export `LoadSnake` (Task 2).
- `src/ui/tui/mod.rs` — tri-state wiring + `render_load_snake_view` (Task 4).

---

### Task 1: `featured_loading` selector + expose cadence

**Files:** Modify `src/ui/tui/inference_panel.rs`, `src/workload/inference_server/monitor.rs`, `src/workload/inference_server/mod.rs`.

**Interfaces:**
- Produces `inference_panel::featured_loading(snapshot: &[ServiceState]) -> Option<&ServiceState>` and `crate::workload::inference_server::CADENCE_SECS: u32` (public).

- [ ] **Step 1: Write the failing test** in `inference_panel.rs` (uses the existing `base()` helper):

```rust
    #[test]
    fn featured_loading_picks_first_compiling_or_loading() {
        let mut down = base(); down.phase = Phase::Down;
        let mut ready = base(); ready.phase = Phase::Ready;
        let mut loading = base(); loading.phase = Phase::Loading; loading.key = "b".into();
        let mut compiling = base(); compiling.phase = Phase::Compiling; compiling.key = "a".into();
        // Nothing loading → None.
        assert!(featured_loading(&[down.clone(), ready.clone()]).is_none());
        assert!(featured_loading(&[]).is_none());
        // First Compiling/Loading in slice order wins; Down/Ready skipped.
        let rows = vec![down, ready, compiling.clone(), loading];
        assert_eq!(featured_loading(&rows).map(|s| s.key.as_str()), Some("a"));
    }
```

- [ ] **Step 2: Run — fails** (`featured_loading` undefined): `cargo test --locked --lib --features tui featured_loading ; echo "exit: $?"` → FAIL.

- [ ] **Step 3: Implement `featured_loading`** in `inference_panel.rs` (next to `trail_is_cold`):

```rust
/// The service to feature in the full-screen loading view: the first one that
/// is Compiling or Loading, in snapshot order. `None` when nothing is loading
/// (the view then falls to the starfield when cold, else the live list).
pub fn featured_loading(snapshot: &[ServiceState]) -> Option<&ServiceState> {
    snapshot
        .iter()
        .find(|s| matches!(s.phase, Phase::Compiling | Phase::Loading))
}
```

- [ ] **Step 4: Expose the cadence.** In `src/workload/inference_server/monitor.rs:42` change `const CADENCE_SECS: u32` to `pub const CADENCE_SECS: u32` (leave the value expression unchanged). In `src/workload/inference_server/mod.rs`, add `CADENCE_SECS` to the `pub use monitor::{...}` re-export line (currently `pub use monitor::InferenceServerMonitor;` → `pub use monitor::{CADENCE_SECS, InferenceServerMonitor};`).

- [ ] **Step 5: Run tests + clippy + macOS cross-check (real exit codes) — pass. Commit.**

```bash
git add src/ui/tui/inference_panel.rs src/workload/inference_server/monitor.rs src/workload/inference_server/mod.rs
git commit -m "feat(inference): featured_loading selector + expose CADENCE_SECS"
```

---

### Task 2: `LoadSnake` journey logic

**Files:** Create `src/animation/inference_load.rs`; modify `src/animation/mod.rs`.

**Interfaces:**
- Consumes `crate::workload::inference_server::{ServiceState, Phase, is_alarm}`.
- Produces `LoadSnake::new(width: usize, height: usize) -> Self`, `update(&mut self, featured: &ServiceState, cadence_secs: u32)`, `finish_if_ready(&mut self, rows: &[ServiceState]) -> bool`, and accessors `active_phase(&self) -> Phase`, `compile_fill(&self) -> f32`, `load_fill(&self) -> f32`, `is_alarm(&self) -> bool`, `is_finishing(&self) -> bool`. Fields the renderer (Task 3) reads stay `pub(crate)` or accessed within the same file.

- [ ] **Step 1: Write the failing tests** in `src/animation/inference_load.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};

    fn svc(phase: Phase) -> ServiceState {
        ServiceState {
            key: "flux".into(), label: "FLUX".into(), phase,
            cpu_pct: 0.0, rss_bytes: 0, rss_delta: 0, kernel_count: 0, kernel_delta: 0,
            safetensors_fds: 0, readiness: Readiness::NotReady, top_proc: None,
            last_log: None, progress: None, flat_ticks: 0,
        }
    }

    #[test]
    fn compiling_fills_only_the_compile_box() {
        let mut s = LoadSnake::new(80, 20);
        let mut c = svc(Phase::Compiling); c.progress = Some(0.5);
        s.update(&c, 2);
        assert_eq!(s.active_phase(), Phase::Compiling);
        assert!((s.compile_fill() - 0.5).abs() < 1e-3, "determinate compile fill = progress");
        assert_eq!(s.load_fill(), 0.0, "load box untouched while compiling");
    }

    #[test]
    fn loading_locks_compile_full_and_fills_load() {
        let mut s = LoadSnake::new(80, 20);
        let mut l = svc(Phase::Loading); l.progress = Some(0.3);
        s.update(&l, 2);
        assert_eq!(s.compile_fill(), 1.0, "entering load locks compile box full");
        assert!((s.load_fill() - 0.3).abs() < 1e-3);
    }

    #[test]
    fn momentum_advances_with_delta_and_never_completes() {
        // No baseline (progress None) → momentum mode.
        let mut s = LoadSnake::new(80, 20);
        let mut c = svc(Phase::Compiling); c.kernel_delta = 50;
        s.update(&c, 2);
        let after_one = s.compile_fill();
        assert!(after_one > 0.0, "positive delta advances the head");
        // Zero delta holds position.
        c.kernel_delta = 0;
        s.update(&c, 2);
        assert_eq!(s.compile_fill(), after_one, "flat delta holds");
        // Momentum never reaches full on its own.
        for _ in 0..1000 { c.kernel_delta = 1000; s.update(&c, 2); }
        assert!(s.compile_fill() < 1.0, "momentum caps below full");
    }

    #[test]
    fn bigger_burst_is_a_bigger_step() {
        assert!(momentum_step(200, 50) > momentum_step(50, 50));
        assert_eq!(momentum_step(0, 50), 0.0);
        assert_eq!(momentum_step(-5, 50), 0.0);
    }

    #[test]
    fn new_featured_key_resets_the_journey() {
        let mut s = LoadSnake::new(80, 20);
        let mut a = svc(Phase::Loading); a.key = "a".into(); a.progress = Some(0.8);
        s.update(&a, 2);
        assert!(s.load_fill() > 0.0);
        let mut b = svc(Phase::Compiling); b.key = "b".into(); b.progress = Some(0.1);
        s.update(&b, 2);
        assert_eq!(s.load_fill(), 0.0, "switching service resets fills");
        assert_eq!(s.active_phase(), Phase::Compiling);
    }

    #[test]
    fn alarm_when_flat_ticks_exceed_five_minutes() {
        let mut s = LoadSnake::new(80, 20);
        let mut c = svc(Phase::Loading);
        c.flat_ticks = 200; // 200 * 2s = 400s >= 300s
        s.update(&c, 2);
        assert!(s.is_alarm());
        c.flat_ticks = 1;
        s.update(&c, 2);
        assert!(!s.is_alarm());
    }

    #[test]
    fn finish_if_ready_bursts_then_ends() {
        let mut s = LoadSnake::new(80, 20);
        s.update(&svc(Phase::Loading), 2); // active journey on key "flux"
        let mut ready = svc(Phase::Ready);
        // Bursts for a bounded number of frames, then ends.
        assert!(s.finish_if_ready(std::slice::from_ref(&ready)));
        assert!(s.is_finishing());
        let mut frames = 1;
        while s.finish_if_ready(std::slice::from_ref(&ready)) { frames += 1; assert!(frames < 1000); }
        assert!(!s.is_finishing());
        // With no active journey, returns false.
        assert!(!s.finish_if_ready(std::slice::from_ref(&ready)));
    }

    #[test]
    fn finish_if_ready_false_when_service_vanished() {
        let mut s = LoadSnake::new(80, 20);
        s.update(&svc(Phase::Loading), 2);
        assert!(!s.finish_if_ready(&[]), "vanished service → no burst, journey cleared");
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement the logic** in `src/animation/inference_load.rs` (SPDX header first). Design notes below the code:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Boxed-snake loading visualization for the `[i]` Inference Server Monitor.
//! Pure presentation over `ServiceState`: three boxes (compile → load → ready)
//! that fill as a model comes up — a true % when the monitor has a baseline,
//! else live momentum from the per-tick deltas; red when stalled (Alarm).

use crate::workload::inference_server::{is_alarm, Phase, ServiceState};
use std::time::Instant;

/// Momentum "briskness" references: a per-tick delta at/above this reads as a
/// strong surge. Compile counts `.o` kernels; load counts RSS bytes.
const COMPILE_REF: i64 = 50; // kernels/tick
const LOAD_REF: i64 = 200 * 1024 * 1024; // 200 MiB/tick
const BASE_STEP: f32 = 0.02; // momentum head advance per active tick (before scaling)
const FILL_CAP: f32 = 0.95; // momentum never claims a full box
/// Ready-burst duration in update frames (cosmetic gold celebration).
const BURST_FRAMES: u32 = 24;

/// Momentum increment for the active box given this tick's `delta` and the
/// phase's briskness `reference`. Zero for a non-positive delta; monotonically
/// larger for bigger bursts, saturating at 3×BASE_STEP.
pub fn momentum_step(delta: i64, reference: i64) -> f32 {
    if delta <= 0 || reference <= 0 {
        return 0.0;
    }
    let ratio = (delta as f32 / reference as f32).min(2.0); // 0..2
    BASE_STEP * (1.0 + ratio) // BASE_STEP .. 3*BASE_STEP
}

pub struct LoadSnake {
    width: usize,
    height: usize,
    frame: u64,
    /// The service whose journey we're rendering; `None` until first update.
    active_key: Option<String>,
    phase: Phase,
    compile_fill: f32,
    load_fill: f32,
    alarm: bool,
    burst_left: u32,
    started: Option<Instant>,
    // Footer stats snapshot (read by render, Task 3).
    pub(crate) kernel_count: usize,
    pub(crate) kernel_delta: i64,
    pub(crate) rss_bytes: u64,
    pub(crate) rss_delta: i64,
    pub(crate) progress: Option<f32>,
    pub(crate) cadence_secs: u32,
    pub(crate) label: String,
}

impl LoadSnake {
    pub fn new(width: usize, height: usize) -> Self {
        LoadSnake {
            width, height, frame: 0,
            active_key: None, phase: Phase::Down,
            compile_fill: 0.0, load_fill: 0.0, alarm: false, burst_left: 0,
            started: None,
            kernel_count: 0, kernel_delta: 0, rss_bytes: 0, rss_delta: 0,
            progress: None, cadence_secs: 2, label: String::new(),
        }
    }

    /// Advance the journey for the featured (Compiling/Loading) service.
    pub fn update(&mut self, featured: &ServiceState, cadence_secs: u32) {
        self.frame = self.frame.wrapping_add(1);
        // New service → reset the journey.
        if self.active_key.as_deref() != Some(featured.key.as_str()) {
            self.active_key = Some(featured.key.clone());
            self.compile_fill = 0.0;
            self.load_fill = 0.0;
            self.burst_left = 0;
            self.started = Some(Instant::now());
        }
        self.phase = featured.phase;
        self.cadence_secs = cadence_secs.max(1);
        self.label = featured.label.clone();
        self.kernel_count = featured.kernel_count;
        self.kernel_delta = featured.kernel_delta;
        self.rss_bytes = featured.rss_bytes;
        self.rss_delta = featured.rss_delta;
        self.progress = featured.progress;
        self.alarm = is_alarm(featured.flat_ticks, self.cadence_secs, &featured.readiness);

        match featured.phase {
            Phase::Compiling => {
                self.compile_fill = self.advance(self.compile_fill, featured.progress, featured.kernel_delta, COMPILE_REF);
            }
            Phase::Loading => {
                // Entering load locks the compile box full.
                self.compile_fill = 1.0;
                self.load_fill = self.advance(self.load_fill, featured.progress, featured.rss_delta, LOAD_REF);
            }
            // Ready/Down/Alarm aren't featured by `featured_loading`, but be safe.
            _ => {}
        }
    }

    /// Determinate (progress present, clamped) else momentum advance of `fill`.
    fn advance(&self, fill: f32, progress: Option<f32>, delta: i64, reference: i64) -> f32 {
        match progress {
            Some(p) => p.clamp(0.0, 1.0),
            None => {
                let step = momentum_step(delta, reference);
                (fill + step * (1.0 - fill)).min(FILL_CAP)
            }
        }
    }

    /// After the featured service leaves the loading phases, play a brief gold
    /// burst if it reached Ready. Returns true while the burst is in progress.
    /// Clears the journey when the burst ends or the service vanished.
    pub fn finish_if_ready(&mut self, rows: &[ServiceState]) -> bool {
        let Some(key) = self.active_key.clone() else { return false };
        let cur = rows.iter().find(|s| s.key == key);
        match cur.map(|s| s.phase) {
            Some(Phase::Ready) => {
                self.frame = self.frame.wrapping_add(1);
                if self.burst_left == 0 && self.phase != Phase::Ready {
                    // First frame of the burst: lock both boxes full.
                    self.burst_left = BURST_FRAMES;
                    self.compile_fill = 1.0;
                    self.load_fill = 1.0;
                    self.phase = Phase::Ready;
                }
                if self.burst_left > 0 {
                    self.burst_left -= 1;
                    return true;
                }
                self.active_key = None; // burst done
                false
            }
            // Still compiling/loading (someone else is featured), or vanished/Down.
            _ => {
                self.active_key = None;
                false
            }
        }
    }

    pub fn active_phase(&self) -> Phase { self.phase }
    pub fn compile_fill(&self) -> f32 { self.compile_fill }
    pub fn load_fill(&self) -> f32 { self.load_fill }
    pub fn is_alarm(&self) -> bool { self.alarm }
    pub fn is_finishing(&self) -> bool { self.burst_left > 0 }
}
```

Notes: `advance` is determinate when `progress` is `Some` (clamped), else momentum (`momentum_step`, capped at `FILL_CAP`). Entering `Loading` locks `compile_fill = 1.0`. `finish_if_ready` is only meaningful after an active journey exists; it plays a bounded burst when the tracked service is Ready, and clears the journey when the service vanished or the burst ends. `started`/`frame` are consumed by the renderer (Task 3).

- [ ] **Step 4: Wire `src/animation/mod.rs`** — add `pub mod inference_load;` and `pub use inference_load::LoadSnake;` (alongside the other animation modules; NOT cfg-gated).

- [ ] **Step 5: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/inference_load.rs src/animation/mod.rs
git commit -m "feat(animation): LoadSnake journey logic (boxes, dual-mode fill, alarm, burst)"
```

---

### Task 3: `LoadSnake` render + formatters

**Files:** Modify `src/animation/inference_load.rs`.

**Interfaces:**
- Consumes the Task 2 `LoadSnake` fields/accessors.
- Produces `LoadSnake::render(&self) -> Vec<ratatui::text::Line<'static>>`, and pure helpers `fmt_bytes(bytes: u64) -> String`, `fmt_rate_per_sec(delta: i64, cadence_secs: u32) -> String`, `fmt_elapsed(secs: u64) -> String`.

- [ ] **Step 1: Write the failing tests** (append to the `tests` mod in `inference_load.rs`):

```rust
    #[test]
    fn formatters_are_human_readable() {
        assert_eq!(fmt_bytes(0), "0B");
        assert_eq!(fmt_bytes(6_657_000_000), "6.2G");
        assert_eq!(fmt_bytes(700 * 1024 * 1024), "700M");
        // rate = delta / cadence, per second.
        assert_eq!(fmt_rate_per_sec(0, 2), "0/s");
        assert_eq!(fmt_rate_per_sec(84, 2), "42/s");
        assert_eq!(fmt_elapsed(0), "0:00");
        assert_eq!(fmt_elapsed(134), "2:14");
        assert_eq!(fmt_elapsed(3_661), "61:01");
    }

    #[test]
    fn render_determinate_shows_percent_and_label() {
        let mut s = LoadSnake::new(60, 16);
        let mut l = svc(Phase::Loading); l.label = "FLUX".into(); l.progress = Some(0.47);
        s.update(&l, 2);
        let text: String = s.render().iter()
            .flat_map(|ln| ln.spans.iter().map(|sp| sp.content.to_string())).collect();
        assert!(text.contains("FLUX"));
        assert!(text.contains("47%"), "determinate → shows percent");
        assert!(text.contains("load"));
    }

    #[test]
    fn render_momentum_shows_no_percent() {
        let mut s = LoadSnake::new(60, 16);
        let mut c = svc(Phase::Compiling); c.kernel_count = 1842; c.kernel_delta = 84; // progress None
        s.update(&c, 2);
        let text: String = s.render().iter()
            .flat_map(|ln| ln.spans.iter().map(|sp| sp.content.to_string())).collect();
        assert!(!text.contains('%'), "momentum → never shows a percent");
        assert!(text.contains("1,842") || text.contains("1842"), "shows raw kernel count");
    }

    #[test]
    fn render_zero_dims_does_not_panic() {
        let s = LoadSnake::new(0, 0);
        let _ = s.render(); // must not panic
    }
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `render` + the formatters.** Full code for the formatters (they are the tested/error-prone part); build the visual body against the `model_starfield.rs` canvas pattern.

```rust
// (add near the top-level fns in inference_load.rs)

/// Compact byte size: `0B`, `700M`, `6.2G`. Binary units (KiB base) but a short
/// single-letter suffix to keep the footer narrow.
pub fn fmt_bytes(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if bytes == 0 { "0B".into() }
    else if b < K * K { format!("{:.0}K", b / K) }
    else if b < K * K * K { format!("{:.0}M", b / (K * K)) }
    else { format!("{:.1}G", b / (K * K * K)) }
}

/// Per-second rate from a per-tick delta and the tick cadence. Negative → 0.
pub fn fmt_rate_per_sec(delta: i64, cadence_secs: u32) -> String {
    let cadence = cadence_secs.max(1) as i64;
    let per_sec = (delta / cadence).max(0);
    if per_sec >= 1024 * 1024 {
        format!("{}/s", fmt_bytes(per_sec as u64))
    } else {
        format!("{per_sec}/s")
    }
}

/// `M:SS` elapsed, minutes uncapped (`2:14`, `61:01`).
pub fn fmt_elapsed(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}
```

For `render(&self) -> Vec<Line<'static>>`, mirror `render_model_starfield_view`'s companion (`ModelStarfield::render`) and the canvas idiom in `model_starfield.rs`:
  - Guard: if `self.width == 0 || self.height == 0` return `vec![]`.
  - **Title line**: `format!("{}  {}  {}", self.label, phase_word, fmt_elapsed(elapsed))` where `phase_word` is `"compiling"`/`"loading"`/`"ready"` from `self.phase`; `elapsed = self.started.map(|t| t.elapsed().as_secs()).unwrap_or(0)`. Char-safe truncate to width.
  - **Boxes**: draw three labeled chambers (`compile` / `load` / `ready`) across the width, sized to leave room; fill each serpentine (row-major) up to its fill fraction using a body glyph (e.g. `●`) in the phase color (compile = `colors::SUCCESS`, load = `colors::WARNING`, ready = gold `Color::Rgb(246,188,66)`); a bright head glyph (`▶`) at the active box's fill frontier. Closed (not-yet-open) boxes render a dashed border and stay empty. When `self.is_alarm()`, tint the active box + head `colors::ERROR` and add a `stalled` marker. On `is_finishing()`, the ready box is open/gold.
  - **Footer line**: determinate (`self.progress.is_some()`) →
    `format!("kernels {}  rss {} {}  {:.0}%", group_thousands(self.kernel_count), fmt_bytes(self.rss_bytes), fmt_rate_per_sec(self.rss_delta, self.cadence_secs), self.progress.unwrap()*100.0)`;
    momentum (`progress` None) → the same but WITHOUT the `%`, and include the active signal's rate:
    `format!("kernels {} ↑{}  rss {} ↑{}", group_thousands(self.kernel_count), fmt_rate_per_sec(self.kernel_delta, self.cadence_secs), fmt_bytes(self.rss_bytes), fmt_rate_per_sec(self.rss_delta, self.cadence_secs))`.
  - Add a small private `group_thousands(n: usize) -> String` (e.g. `1842 -> "1,842"`) so the momentum test's `contains("1,842")` passes; keep it simple and pure.
  - Build `Vec<Line<'static>>` from owned `String`s (like `ModelStarfield::render`), never borrowing `self`.

- [ ] **Step 4: Run tests + clippy + macOS cross-check — pass. Commit.**

```bash
git add src/animation/inference_load.rs
git commit -m "feat(animation): LoadSnake render + human-readable formatters"
```

---

### Task 4: Tri-state wiring in the `[i]` view

**Files:** Modify `src/ui/tui/mod.rs`.

**Interfaces:**
- Consumes `inference_panel::{trail_is_cold, featured_loading}`, `crate::animation::LoadSnake`, `crate::workload::inference_server::CADENCE_SECS`.
- Produces `render_load_snake_view(f: &mut Frame, snake: &crate::animation::LoadSnake)`.

- [ ] **Step 1: Add loop state.** Next to `model_starfield`/`model_starfield_dims` (~line 407):

```rust
    let mut load_snake = crate::animation::LoadSnake::new(0, 0);
    let mut load_snake_dims: (usize, usize) = (0, 0);
```

- [ ] **Step 2: Update in the tick block.** In the `should_tick` arm `DisplayMode::InferenceMonitor =>` (~line 535), AFTER the existing starfield update, feed the snake when a service is loading. `inference_monitor.snapshot()` is Linux-only, so guard it:

```rust
                    // Loading takes over the view: feed the boxed-snake when a
                    // service is Compiling/Loading. Snapshot read is Linux-only.
                    #[cfg(target_os = "linux")]
                    {
                        let snap = inference_monitor.snapshot();
                        if let Some(svc) = inference_panel::featured_loading(&snap) {
                            let sdims = (size.width as usize, (size.height as usize).saturating_sub(1));
                            if sdims != load_snake_dims {
                                load_snake = crate::animation::LoadSnake::new(sdims.0, sdims.1);
                                load_snake_dims = sdims;
                            }
                            load_snake.update(svc, crate::workload::inference_server::CADENCE_SECS);
                        }
                    }
```

- [ ] **Step 3: Branch the render arm.** Replace the draw-closure `DisplayMode::InferenceMonitor =>` body (~line 675) — currently the `trail_is_cold` if/else — with the tri-state. `inference_monitor_rows` is already in scope (the raw snapshot on Linux / empty otherwise):

```rust
                        DisplayMode::InferenceMonitor => {
                            if inference_panel::featured_loading(&inference_monitor_rows).is_some() {
                                render_load_snake_view(f, &load_snake);
                            } else if load_snake.finish_if_ready(&inference_monitor_rows) {
                                // Brief gold burst after the featured model goes Ready.
                                render_load_snake_view(f, &load_snake);
                            } else if inference_panel::trail_is_cold(&inference_monitor_rows) {
                                render_model_starfield_view(f, &model_starfield);
                            } else {
                                render_inference_monitor_view(
                                    f,
                                    &inference_monitor_rows,
                                    service_cursor,
                                );
                            }
                        }
```

Note: `finish_if_ready` takes `&mut self`, and the draw closure captures `load_snake`. It's already `let mut load_snake`, and the closure is `FnMut` — if the borrow checker objects to mutating inside `terminal.draw(|f| …)`, call `let finishing = load_snake.finish_if_ready(&inference_monitor_rows);` BEFORE the `terminal.draw(` call (alongside where `inference_monitor_rows` is computed) and branch on the precomputed `finishing` bool inside the closure. Prefer the precompute form to keep the closure borrow simple.

- [ ] **Step 4: Add `render_load_snake_view`** right after `render_model_starfield_view` (~line 2978), mirroring it (1-line title + body Paragraph). The `LoadSnake::render()` already includes its own title/footer lines, so this wrapper just renders those lines into the full area on a black background:

```rust
/// Full-screen render of the boxed-snake loading view (see
/// `crate::animation::LoadSnake`), shown in `DisplayMode::InferenceMonitor`
/// while a service is Compiling/Loading (and briefly after it goes Ready).
fn render_load_snake_view(f: &mut Frame, snake: &crate::animation::LoadSnake) {
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(colors::rgb(0, 0, 0))),
        area,
    );
    f.render_widget(Paragraph::new(snake.render()), area);
}
```

- [ ] **Step 5: Verify + manual check + commit.** Run the full lib suite, `RUSTFLAGS="-D warnings"` build, clippy, macOS cross-check (real exit codes). Manual: `tt-toplike-tui`, press `i` while a model is compiling/loading → the boxed snake takes over (compile box fills teal, flows into the amber load box; momentum motion when no baseline; footer shows kernels/rss/rate); when it goes Ready → brief gold burst, then the live list. Nothing loading + cold → starfield still shows.

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(tui): tri-state [i] view — boxed-snake loading takeover"
```

---

## Self-Review

**Spec coverage:** tri-state mode model (cold→starfield / loading→snake / ready-only→list) → Task 1 selector + Task 4 wiring; featured-service selection (first Compiling/Loading, stable order) → Task 1 `featured_loading`; three boxes + phase-driven open/lock → Task 2 `update`; compile=teal/load=amber/ready=gold + pulsing head → Task 3 render; dual-mode fill (determinate `progress` vs momentum deltas, never a false %, momentum caps below full) → Task 2 `advance`/`momentum_step` + Task 3 footer branch; stall→Alarm via `is_alarm` → Task 2 + Task 3 red tint; zero new probing (reads existing `ServiceState`; only widens `CADENCE_SECS`) → Global Constraints + Task 1; elapsed tracked by the viz, reset on key change → Task 2 `started`/reset + Task 3 `fmt_elapsed`; Ready gold burst then handoff → Task 2 `finish_if_ready` + Task 4 branch; `+N more loading` pips → **see note**; error handling (vanished service, zero dims, saturating math, char-safe truncate, clamp progress) → Task 2 `finish_if_ready`/`advance` clamp + Task 3 zero-dim guard + Global Constraints; testing → each task's pure tests + render smoke. 

**Note (scope trim):** the spec's `+N more loading` pip for the multi-loader case is a minor cosmetic. It is NOT implemented as a separate step (the tests don't require it); the implementer MAY add a single footer/corner `+N more loading` string in Task 3 render when more than one service is Compiling/Loading, but the plan does not gate on it. Flagging so it isn't mistaken for a silent omission — feature one service is the v1 behavior; the pip is optional polish.

**Placeholder scan:** the pure/error-prone pieces (`featured_loading`, `momentum_step`, `advance`, `finish_if_ready`, `fmt_bytes`/`fmt_rate_per_sec`/`fmt_elapsed`) have complete code + tests. Task 3's `render` body and Task 4's box-drawing are described against exact anchors + the existing `model_starfield.rs`/`render_model_starfield_view` patterns rather than pasted verbatim (acceptable per "follow existing patterns"); `group_thousands` is specified with its required behavior and test hook. No TBD/TODO/"handle edge cases".

**Type consistency:** `featured_loading(&[ServiceState]) -> Option<&ServiceState>`, `LoadSnake::{new,update,finish_if_ready,active_phase,compile_fill,load_fill,is_alarm,is_finishing,render}`, `momentum_step(i64,i64)->f32`, `fmt_bytes(u64)`, `fmt_rate_per_sec(i64,u32)`, `fmt_elapsed(u64)`, `render_load_snake_view(&mut Frame,&LoadSnake)`, `CADENCE_SECS: u32` — consistent across Tasks 1–4. `update` takes `cadence_secs: u32` (Task 2) and the wiring passes `CADENCE_SECS` (Task 4). `finish_if_ready` `&mut self` borrow addressed in Task 4 Step 3 (precompute form).
