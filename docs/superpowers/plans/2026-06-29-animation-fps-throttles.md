# Animation FPS Throttles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two opt-in animation throttles — adaptive load throttle (`--throttle` / `/throttle`) and focus-aware idle (`--idle-on-blur` / `/idle-on-blur`) — that reduce animated-mode FPS only when enabled; the default 60 FPS experience is unchanged.

**Architecture:** A pure `ThrottleState` module decides the animated frame interval by *modulating* the existing `ui_poll_rate_anim` base (so the existing `/fps` command keeps working). The run loop owns one `ThrottleState`, updates it from focus events and the already-sampled `host_cpu_pct`, and uses `state.effective_anim_interval(base)` as the animated render cadence. Slash commands and CLI flags toggle the two flags 1:1.

**Tech Stack:** Rust, existing TUI run loop (`src/ui/tui/mod.rs`), `host_cpu_pct` (system CPU, already sampled), `execute_command` slash dispatcher, crossterm focus events.

## Global Constraints

- **Default 60 FPS animated is unchanged.** Both throttles are OFF by default. Data screens stay 10 FPS.
- **1:1 surfaces:** `--throttle` ↔ `/throttle`; `--idle-on-blur` ↔ `/idle-on-blur` (CLI flag sets initial state; slash command toggles live).
- **Adaptive throttle:** when on, animated FPS = 30 while load-throttling, else base. Hysteresis: engage at host CPU ≥ 85%, disengage at < 80%; the 80–85% band holds prior state.
- **Focus-idle:** when on, animated FPS = 2 while unfocused; restore on focus. Focus-idle takes priority over load-throttle.
- `ThrottleState` *modulates* the existing `ui_poll_rate_anim` base — never below it (if `/fps` set a slower base, throttle won't speed it up): `effective ≥ base`.
- No new dependencies; cross-platform (uses the already cross-platform `host_cpu_pct`).
- TDD for the pure module; zero new warnings; frequent commits.
- Repo facts (`src/ui/tui/mod.rs`): animated render cadence chosen at ~line 351 (`render_interval = if is_anim_mode { ui_poll_rate_anim } else { ui_poll_rate_data }`); focus events handled at ~line 602 (`Event::FocusGained | Event::FocusLost => {}` — currently a no-op); `host_cpu_pct` refreshed at ~line 988 inside the `last_proc_rows_update.elapsed() >= Duration::from_secs(2)` block; `execute_command(cmd, display_mode, anim_poll, data_poll, sensitivity, overlay) -> (String /*msg*/, bool /*is_error*/, bool /*quit*/)` at ~line 2282 (already handles `/fps`, `/datafps`, `/mode`, `/quit`); `render_global_statusbar(f, backend, mode, perf: Option<&str>)` at ~line 1342. The `Cli` struct is in `src/cli.rs`; literals to update with the new fields exist at cli.rs lines ~355, 421, 452, 478, 504, 534, 557, 583, 609, 632, 657 and factory.rs ~271. Build: `cargo build --bin tt-toplike-tui --features tui`. Tests: `cargo test --lib --tests --features tui`.

---

### Task 1: `ThrottleState` pure module

**Files:**
- Create: `src/ui/tui/throttle.rs`
- Modify: `src/ui/tui/mod.rs` (`mod throttle; pub use throttle::ThrottleState;`, matching the module gating used by `bench`/`perf`)
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct ThrottleState { pub throttle_on: bool, pub idle_on_blur: bool, pub focused: bool, load_throttling: bool }`
  - `ThrottleState::new(throttle_on: bool, idle_on_blur: bool) -> Self` (focused defaults true, load_throttling false)
  - `ThrottleState::update_load(&mut self, host_cpu_pct: f32)` (85/80 hysteresis)
  - `ThrottleState::effective_anim_interval(&self, base: std::time::Duration) -> std::time::Duration`
  - `ThrottleState::status_hint(&self) -> Option<&'static str>` (e.g. `"⏸2"` blur-idle, `"⏷30"` load-throttled, else `None`)

- [ ] **Step 1: Write the failing test**

Create `src/ui/tui/throttle.rs` with the test module first:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const BASE: Duration = Duration::from_millis(16); // ~60 FPS

    #[test]
    fn full_speed_when_off_and_focused() {
        let s = ThrottleState::new(false, false);
        assert_eq!(s.effective_anim_interval(BASE), BASE);
        assert_eq!(s.status_hint(), None);
    }

    #[test]
    fn blur_idle_only_when_enabled_and_unfocused() {
        let mut s = ThrottleState::new(false, true);
        s.focused = true;
        assert_eq!(s.effective_anim_interval(BASE), BASE); // focused → full
        s.focused = false;
        assert_eq!(s.effective_anim_interval(BASE), Duration::from_millis(500)); // 2 FPS
        assert_eq!(s.status_hint(), Some("⏸2"));
    }

    #[test]
    fn load_throttle_drops_to_30_with_hysteresis() {
        let mut s = ThrottleState::new(true, false);
        s.update_load(90.0); // ≥85 engages
        assert_eq!(s.effective_anim_interval(BASE), Duration::from_millis(33)); // ~30 FPS
        assert_eq!(s.status_hint(), Some("⏷30"));
        s.update_load(82.0); // 80–85 band holds prior (still throttling)
        assert_eq!(s.effective_anim_interval(BASE), Duration::from_millis(33));
        s.update_load(78.0); // <80 disengages
        assert_eq!(s.effective_anim_interval(BASE), BASE);
    }

    #[test]
    fn load_throttle_ignored_when_toggle_off() {
        let mut s = ThrottleState::new(false, false);
        s.update_load(99.0);
        assert_eq!(s.effective_anim_interval(BASE), BASE);
    }

    #[test]
    fn blur_idle_beats_load_throttle() {
        let mut s = ThrottleState::new(true, true);
        s.update_load(99.0);
        s.focused = false;
        assert_eq!(s.effective_anim_interval(BASE), Duration::from_millis(500)); // blur wins
    }

    #[test]
    fn never_faster_than_base() {
        // If /fps set a slow base (e.g. 10 FPS = 100ms), throttle's 30 FPS must not speed it up.
        let mut s = ThrottleState::new(true, false);
        s.update_load(99.0);
        let slow = Duration::from_millis(100);
        assert_eq!(s.effective_anim_interval(slow), slow);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui throttle`
Expected: FAIL — `ThrottleState` not found.

- [ ] **Step 3: Implement the module**

Prepend to `src/ui/tui/throttle.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Opt-in animation throttling decisions (pure logic).
//!
//! Modulates the animated render interval: full speed by default, dropped to
//! 30 FPS under heavy host load (when `--throttle`/`/throttle` is on, with
//! hysteresis), or 2 FPS when the terminal is unfocused (when
//! `--idle-on-blur`/`/idle-on-blur` is on). Focus-idle beats load-throttle.
//! Never returns an interval faster than the supplied base.

use std::time::Duration;

const IDLE_FPS: u64 = 2;
const LOAD_FPS: u64 = 30;
const LOAD_HI: f32 = 85.0; // engage load throttle at/above this host CPU %
const LOAD_LO: f32 = 80.0; // disengage below this (hysteresis band 80–85)

fn fps_interval(fps: u64) -> Duration {
    Duration::from_secs_f64(1.0 / fps as f64)
}

/// Runtime throttle flags + derived load state. Cheap to copy semantics; lives
/// in the run loop.
#[derive(Debug, Clone)]
pub struct ThrottleState {
    pub throttle_on: bool,
    pub idle_on_blur: bool,
    pub focused: bool,
    load_throttling: bool,
}

impl ThrottleState {
    pub fn new(throttle_on: bool, idle_on_blur: bool) -> Self {
        ThrottleState {
            throttle_on,
            idle_on_blur,
            focused: true,
            load_throttling: false,
        }
    }

    /// Update the load-throttle latch from a host CPU% sample, with hysteresis.
    pub fn update_load(&mut self, host_cpu_pct: f32) {
        if host_cpu_pct >= LOAD_HI {
            self.load_throttling = true;
        } else if host_cpu_pct < LOAD_LO {
            self.load_throttling = false;
        }
        // 80–85% band: leave self.load_throttling unchanged.
    }

    /// Animated render interval given the user's base (e.g. from `/fps`).
    /// Result is never faster (smaller) than `base`.
    pub fn effective_anim_interval(&self, base: Duration) -> Duration {
        let chosen = if self.idle_on_blur && !self.focused {
            fps_interval(IDLE_FPS)
        } else if self.throttle_on && self.load_throttling {
            fps_interval(LOAD_FPS)
        } else {
            base
        };
        chosen.max(base)
    }

    /// Short status-bar hint when actively throttling, else `None`.
    pub fn status_hint(&self) -> Option<&'static str> {
        if self.idle_on_blur && !self.focused {
            Some("⏸2")
        } else if self.throttle_on && self.load_throttling {
            Some("⏷30")
        } else {
            None
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui throttle`
Expected: PASS (6 tests). Note: `fps_interval(30)` = 33.33 ms; the test asserts `Duration::from_millis(33)` — adjust the assertion to `from_secs_f64(1.0/30.0)` if the millis rounding mismatches. (Use the exact `fps_interval(30)`/`fps_interval(2)` in assertions to avoid rounding ambiguity.)

- [ ] **Step 5: Commit**
```bash
git add src/ui/tui/throttle.rs src/ui/tui/mod.rs
git commit -m "feat(throttle): ThrottleState — pure animation FPS throttle logic"
```

---

### Task 2: CLI flags `--throttle` / `--idle-on-blur`

**Files:**
- Modify: `src/cli.rs` (two bool flags + every `Cli { .. }` literal)
- Modify: `src/backend/factory.rs` (its test `Cli { .. }` literal)

**Interfaces:**
- Produces: `cli.throttle: bool`, `cli.idle_on_blur: bool` (clap `#[arg(long)]`, default false).

- [ ] **Step 1: Add the flags**

In `src/cli.rs`, in the `Cli` struct after `pub bench: bool`:
```rust
    /// Throttle animation FPS when the host is under heavy load (opt-in).
    ///
    /// When set, animated modes drop from 60 to 30 FPS while system CPU is high
    /// (≥85%, restored below 80%), yielding CPU to the monitored workload.
    /// Toggle live with the `/throttle` command. Off by default.
    #[arg(long)]
    pub throttle: bool,

    /// Drop animation to ~2 FPS while the terminal is unfocused (opt-in).
    ///
    /// Toggle live with the `/idle-on-blur` command. Off by default.
    #[arg(long)]
    pub idle_on_blur: bool,
```

- [ ] **Step 2: Update every `Cli { .. }` literal**

`grep -rn "Cli {" src/` and add `throttle: false,` and `idle_on_blur: false,` to each literal (there are ~11 in `src/cli.rs` tests + the `bench_default()` constructor + 1 in `src/backend/factory.rs` tests). A missed literal is a compile error.

- [ ] **Step 3: Verify it builds + tests pass**

Run: `cargo build --bin tt-toplike-tui --features tui` (zero warnings) and `cargo test --lib --features tui cli` (CLI tests pass).
Expected: builds; the flags parse (clap derive).

- [ ] **Step 4: Commit**
```bash
git add src/cli.rs src/backend/factory.rs
git commit -m "feat(cli): --throttle and --idle-on-blur flags (default off)"
```

---

### Task 3: Wire `ThrottleState` into the run loop + slash commands + status hint

**Files:**
- Modify: `src/ui/tui/mod.rs` (run loop state, focus events, load sample, render cadence, `execute_command`, status bar)

**Interfaces:**
- Consumes: `ThrottleState` (Task 1), `cli.throttle`/`cli.idle_on_blur` (Task 2).
- Produces: the throttles working end-to-end; `execute_command` gains a `&mut ThrottleState` parameter.

- [ ] **Step 1: Own a `ThrottleState`, initialized from the flags**

In `run_app`, near the other run-loop state (by `ui_poll_rate_anim`):
```rust
    let mut throttle_state = ThrottleState::new(cli.throttle, cli.idle_on_blur);
```
Ensure `ThrottleState` is in scope (`use` is via the `pub use throttle::ThrottleState` from Task 1; reference as `crate::ui::tui::ThrottleState` or add a local `use` — match how `PerfMeter` is referenced).

- [ ] **Step 2: Track focus**

Replace the no-op focus arm (~line 602):
```rust
                Event::FocusGained => {
                    throttle_state.focused = true;
                    force_redraw = true;
                }
                Event::FocusLost => {
                    throttle_state.focused = false;
                    force_redraw = true;
                }
```

- [ ] **Step 3: Feed host load into the throttle**

In the `~2 s` refresh block where `host_cpu_pct` is set (after `host_cpu_pct = sys_monitor.global_cpu_usage();`, ~line 988):
```rust
            throttle_state.update_load(host_cpu_pct);
```

- [ ] **Step 4: Use the throttle for the animated render cadence**

At the cadence decision (~line 351), replace:
```rust
        let render_interval = if is_anim_mode {
            ui_poll_rate_anim
        } else {
            ui_poll_rate_data
        };
```
with:
```rust
        let render_interval = if is_anim_mode {
            throttle_state.effective_anim_interval(ui_poll_rate_anim)
        } else {
            ui_poll_rate_data
        };
```
(`ui_poll_rate_anim` remains the base set by `/fps`; the throttle only ever slows it.)

- [ ] **Step 5: Add `/throttle` and `/idle-on-blur` slash commands**

Change `execute_command`'s signature to take the throttle state — add a parameter `throttle_state: &mut ThrottleState` (e.g. after `overlay`). Inside its `match verb.as_str()`, add arms (mirroring the existing `/fps` style):
```rust
        "throttle" => {
            throttle_state.throttle_on = !throttle_state.throttle_on;
            (
                format!(
                    "load throttle: {}",
                    if throttle_state.throttle_on { "on" } else { "off" }
                ),
                false,
                false,
            )
        }
        "idle-on-blur" | "idleonblur" => {
            throttle_state.idle_on_blur = !throttle_state.idle_on_blur;
            (
                format!(
                    "idle-on-blur: {}",
                    if throttle_state.idle_on_blur { "on" } else { "off" }
                ),
                false,
                false,
            )
        }
```
Update the single `execute_command(...)` call site (~line 624) to pass `&mut throttle_state`.

- [ ] **Step 6: Show the status hint**

`render_global_statusbar` already takes `perf: Option<&str>`. Add a second optional `throttle_hint: Option<&str>` parameter; render it (dim) next to the perf readout when `Some`. At the draw call site, compute `let throttle_hint = throttle_state.status_hint();` before the closure and pass it. Update the `render_global_statusbar` signature and its single call site.

- [ ] **Step 7: Build, test, and exercise**

Run: `cargo build --bin tt-toplike-tui --features tui` (zero warnings); `cargo test --lib --tests --features tui` (all pass).
Manual (real terminal): `tt-toplike-tui --host --mode arcade --throttle`; load the machine (e.g. `yes > /dev/null &` ×N) and confirm the status bar shows `⏷30` and animation eases; kill the load → returns to full. `tt-toplike-tui --host --mode arcade --idle-on-blur` then switch terminal focus away → `⏸2`. Run `/throttle` and `/idle-on-blur` in the command bar to toggle live.

- [ ] **Step 8: Commit**
```bash
git add src/ui/tui/mod.rs
git commit -m "feat(throttle): wire load + focus throttles, /throttle + /idle-on-blur, status hint"
```

---

## Self-Review

**1. Spec coverage:**
- Default 60 unchanged; both off by default → Task 1 (`new(false,false)` → base) + Task 2 (flags default false) ✅
- Adaptive load throttle 60→30 with 85/80 hysteresis → Task 1 `update_load` + `effective_anim_interval`; Task 3 Steps 3–4 ✅
- Focus-idle ~2 FPS, priority over load → Task 1 (`blur_idle_beats_load_throttle`); Task 3 Step 2 ✅
- 1:1 surfaces (`--throttle`/`/throttle`, `--idle-on-blur`/`/idle-on-blur`) → Task 2 flags + Task 3 Step 5 ✅
- Modulate base, never faster → Task 1 (`chosen.max(base)`, `never_faster_than_base` test) ✅
- Status hint → Task 1 `status_hint` + Task 3 Step 6 ✅
- No new deps; cross-platform (`host_cpu_pct`) → reuses existing sampling ✅
- Testing (resolver + hysteresis) → Task 1 tests ✅

**2. Placeholder scan:** No TBD/TODO. The Step-2 "grep and update every literal" is a concrete enumerated action (lines listed in Global Constraints), not a deferred decision.

**3. Type consistency:** `ThrottleState::{new,update_load,effective_anim_interval,status_hint}` and fields (`throttle_on`/`idle_on_blur`/`focused`) defined in Task 1 are used identically in Task 3. `cli.throttle`/`cli.idle_on_blur` (Task 2) feed `ThrottleState::new` (Task 3 Step 1). `execute_command`'s new `&mut ThrottleState` param (Task 3 Step 5) matches the call-site update.

**Known risk (flagged for execution):** Task 3 modifies the run loop and `execute_command`'s signature — the implementer must update the single `execute_command` call site and the single `render_global_statusbar` call site (both inside the draw closure) and pass the precomputed `throttle_hint`/state correctly (borrow: compute `status_hint()` before the closure, like `perf_status`). The `from_millis(33)` vs `from_secs_f64(1/30)` assertion rounding (Task 1 Step 4) — use `fps_interval(..)` in assertions to avoid it.
