# Reset Takeover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When someone runs `tt-smi -r` on a box being watched by `tt-toplike-tui --tt-smi-reset-behavior dazzle`, react with a full-screen takeover animation (or, in the HivemindSweeper debug view, a real feed event) that honestly tracks the real reset's lifetime rather than a fixed fake timer.

**Architecture:** A pure, unit-testable detector (`src/workload/reset_detect.rs`) recognizes a `tt-smi -r`/`--reset` process from the TUI's existing per-tick `sysinfo` process scan and resolves its real target chip indices. A small enum-based animation framework (`src/animation/takeover/`, one concrete struct per variant — matching this codebase's existing enum+match convention rather than `dyn Trait`) renders one of six full-screen variants, weighted-random-selected by whether the reset is full-box or a subset. The main TUI loop (`src/ui/tui/mod.rs`) wires the detector into its existing 2-second process-refresh cadence, renders the active takeover as the last step of the existing draw closure, and special-cases the HivemindSweeper view to inject a real `SniffEvent` instead of showing a takeover.

**Tech Stack:** Rust, ratatui 0.30, sysinfo (already a dependency via `HostProcessMonitor`), rand 0.9 (`rand::rng()` / `.random_range(..)`).

**Spec:** `docs/superpowers/specs/2026-09-28-reset-takeover-design.md`

## Global Constraints

- The feature is controlled by `--tt-smi-reset-behavior <ignore|inform|dazzle|demo>` (config key `tt_smi_reset_behavior`, default `inform`). `ignore` never scans. `inform` scans and shows a status-bar segment only. Takeover animations stay inert unless the behavior is `dazzle` or `demo`. (This replaces the original opt-in `--reset-takeover` flag, which was never released; the task text below still shows that original flag.)
- Detection reuses the TUI's existing per-tick/per-2s `sysinfo` process scan — no new polling thread.
- Demo exception: `demo` plays all seven takeovers in a fixed order (8 s slots, each told its reset finished at 5 s) at boot and on every real reset, and ignores the real reset's finish so the sequence always runs to the end. This is a deliberate exception to the lifecycle rule below. The box title carries `DEMO - ` (boot) or `DEMO (real reset) - ` so a staged animation is never mistaken for a real reset. In a demo, Esc and q/Q end it and any other key skips one animation.
- Lifecycle honesty: "in progress" / "done" are driven by the real `tt-smi -r` pid's liveness, never a fixed fake timer. An animation may loop/hold its "in progress" beats until the real reset finishes.
- The screen is tinted full-screen, and every takeover variant draws its animation in one fixed 72x24 box centered on it (it shrinks to fit a smaller terminal). The box is the terminal's default background. Full vs. subset scope changes *content* (which chips are shown as targeted, chip-row density), never the box. (Updated 2026-10-01; this originally said every variant renders full-screen.)
- When `display_mode == DisplayMode::HivemindSweeper`, no takeover is created — the detected reset is injected as a real `SniffEvent` into the live `Hivemind` engine instead.
- Any keypress while a takeover is active calls `skip()` (during a `demo`, Esc and q/Q end the whole demo instead) and is fully consumed that keypress — it must never also reach normal key dispatch (mode switch, quit, etc.) in the same event.
- A second reset detected while one is already being tracked (`ResetDetector` has an active entry) is dropped — no queueing.
- Out of scope: Galaxy-tray resets (`-glx_reset`/`-glx_reset_auto`), the egui GUI binary, any mid-session disable toggle beyond per-animation skip.
- **Descoped from the spec, flagged for explicit sign-off:** the spec's "optional kmsg enrichment" (tailing `/dev/kmsg` for real tt-kmd per-chip reset lines when readable) is not built by this plan. Every variant (Tasks 4–9) only implements the spec's *fallback* path — generic, honestly-labeled per-chip pacing driven by `chip_count` and real process-liveness, never a real per-chip completion claim. This still satisfies the spec's "never claim a specific chip completed when that isn't actually known" requirement; it just means no variant currently shows literal captured tt-kmd log text. Real kmsg-line enrichment is a clean follow-up (its own task, added on top of the existing per-variant pacing) once the base feature is verified end-to-end — not built here to keep this plan's scope to what's needed for a correct, honest first version.

## Review Focus

- A `tt-smi -r` invocation targeting device *paths* (`/dev/tenstorrent/3`) or PCI BDFs, not bare integers — must resolve to the right device index, not silently drop that chip from Missile Command's targeting or the heat-grid emphasis. (Task 1)
- A tracked `tt-smi -r` process that never cleanly disappears in a later snapshot in the way expected (killed, or PID reused fast) — `is_finished`/`clear` must not wedge `ResetDetector` so it can never detect a later reset again. (Task 2)
- A terminal too small for a full-screen takeover — `render_takeover_frame` must not panic on a near-zero-sized `Rect` (matches the existing `render_overlay_panel` minimum-size guard elsewhere in this file). (Task 3)
- A `-r` invocation whose target tokens are partly unresolvable (typo'd BDF, unsupported syntax) — `chip_count`/`device_indices` must stay internally consistent (never `is_full == false` with an empty `device_indices` driving a blank-looking animation). (Task 1)
- The trickiest `is_full` boundary: an explicit target list that happens to enumerate every known device (e.g. `tt-smi -r 0 1` on a 2-chip box) — the design spec calls this out explicitly as counting as a full reset, and it is the one condition most likely to be missed by an implementer who only checks for empty/`all`. (Task 1)

---

### Task 1: `ResetEvent` + pure `tt-smi -r` cmdline parser

**Files:**
- Create: `src/workload/reset_detect.rs`
- Modify: `src/workload/mod.rs` (add `pub mod reset_detect;` and re-export)

**Interfaces:**
- Consumes: `crate::models::device::Device` (`pub index: usize`, `pub bus_id: String` — already exist).
- Produces:
  - `pub struct ResetEvent { pub pid: i32, pub is_full: bool, pub chip_count: usize, pub total_devices: usize, pub device_indices: Vec<u8>, pub raw_targets: Vec<String> }` (derives `Debug, Clone`)
  - `pub fn parse_reset_process(pid: i32, name: &str, cmdline: &str, devices: &[Device]) -> Option<ResetEvent>`

- [ ] **Step 1: Write the failing tests**

Create `src/workload/reset_detect.rs` with just the type stub and this test module (the tests reference `parse_reset_process`, which doesn't exist yet):

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Detects a `tt-smi -r`/`--reset` invocation from the host process list and
//! classifies it as a full-box or subset reset. See
//! docs/superpowers/specs/2026-09-28-reset-takeover-design.md.

use crate::models::device::Device;

/// A detected `tt-smi -r` invocation, still in progress or just finished.
#[derive(Debug, Clone)]
pub struct ResetEvent {
    pub pid: i32,
    /// True if this targets every device currently known (omitted targets,
    /// literal `all`, or an explicit list that happens to enumerate every
    /// known device).
    pub is_full: bool,
    /// Number of chips actually targeted (== `total_devices` when `is_full`).
    pub chip_count: usize,
    /// Total devices known on this box right now (from the active backend).
    pub total_devices: usize,
    /// Resolved real device indices for the targeted chip(s). Equals
    /// `0..total_devices` when `is_full`. A target token that couldn't be
    /// resolved (unknown BDF, junk) is dropped from this list but still
    /// counted in `raw_targets`/`chip_count` — never guessed.
    pub device_indices: Vec<u8>,
    /// The original TARGETS tokens, for display/log text.
    pub raw_targets: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_devices(n: usize) -> Vec<Device> {
        (0..n)
            .map(|i| {
                Device::new(
                    i,
                    "wormhole".to_string(),
                    format!("0000:{i:02x}:00.0"),
                    format!("({i})"),
                )
            })
            .collect()
    }

    #[test]
    fn ignores_non_tt_smi_process() {
        let devices = fixture_devices(4);
        assert!(parse_reset_process(100, "bash", "bash -c ls", &devices).is_none());
    }

    #[test]
    fn ignores_tt_smi_snapshot_invocation() {
        let devices = fixture_devices(4);
        assert!(parse_reset_process(100, "tt-smi", "tt-smi -s", &devices).is_none());
    }

    #[test]
    fn omitted_targets_is_full_reset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r", &devices).unwrap();
        assert!(ev.is_full);
        assert_eq!(ev.chip_count, 4);
        assert_eq!(ev.device_indices, vec![0, 1, 2, 3]);
        assert_eq!(ev.pid, 100);
    }

    #[test]
    fn literal_all_is_full_reset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi --reset all", &devices).unwrap();
        assert!(ev.is_full);
        assert_eq!(ev.total_devices, 4);
    }

    #[test]
    fn single_umd_id_is_subset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 2", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.chip_count, 1);
        assert_eq!(ev.device_indices, vec![2]);
    }

    #[test]
    fn space_separated_targets_are_subset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 2", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![0, 2]);
    }

    #[test]
    fn explicit_enumeration_of_every_device_counts_as_full() {
        // The trickiest boundary: not `all`, not omitted, but happens to name
        // every known device — the design spec calls this out explicitly.
        let devices = fixture_devices(2);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 1", &devices).unwrap();
        assert!(ev.is_full);
        assert_eq!(ev.chip_count, 2);
    }

    #[test]
    fn bdf_target_resolves_to_device_index() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0000:02:00.0", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![2]);
    }

    #[test]
    fn dev_tenstorrent_path_target_resolves_to_device_index() {
        let devices = fixture_devices(4);
        let ev =
            parse_reset_process(100, "tt-smi", "tt-smi -r /dev/tenstorrent/3", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![3]);
    }

    #[test]
    fn comma_separated_targets_are_split() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0,1", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![0, 1]);
        assert_eq!(ev.raw_targets, vec!["0".to_string(), "1".to_string()]);
    }

    #[test]
    fn unresolvable_target_is_dropped_from_device_indices_but_still_counted() {
        let devices = fixture_devices(4);
        let ev =
            parse_reset_process(100, "tt-smi", "tt-smi -r 0000:ff:00.0", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.chip_count, 1); // one token was asked for...
        assert!(ev.device_indices.is_empty()); // ...but it couldn't be resolved, never guessed
    }

    #[test]
    fn trailing_flag_after_targets_does_not_get_consumed_as_a_target() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 1 --use_luwen", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![0, 1]);
    }

    #[test]
    fn absolute_path_process_name_is_recognized() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "/usr/local/bin/tt-smi -r", &devices).unwrap();
        assert!(ev.is_full);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib reset_detect:: -- --nocapture` (after adding `pub mod reset_detect;` to `src/workload/mod.rs`, per Step 3)
Expected: FAIL to compile — `parse_reset_process` and `Device::new` 4-arg signature not found yet (check `Device::new`'s real signature in `src/models/device.rs` if this specific error appears; it already exists as `pub fn new(index: usize, board_type: String, bus_id: String, coords: String) -> Self`, so this should only fail on the missing `parse_reset_process`).

- [ ] **Step 3: Implement the minimal parser**

Add to `src/workload/mod.rs` (near the other `pub mod` declarations):

```rust
pub mod reset_detect;
```

Add to `src/workload/reset_detect.rs` (below the `ResetEvent` struct, above `#[cfg(test)]`):

```rust
/// Recognizes a `tt-smi -r [TARGETS...]` / `--reset [TARGETS...]` invocation
/// from a process's name + full cmdline, per the real `tt-smi --help`
/// grammar: `-r [TARGETS ...]` — omitted or literal `all` means every
/// device; otherwise whitespace- or comma-separated UMD logical IDs, PCI
/// BDFs (e.g. `0000:0a:00.0`), or `/dev/tenstorrent/<id>`. Returns `None`
/// for any other `tt-smi` invocation (`-s`, `-ls`, etc.) or a non-`tt-smi`
/// process. `devices` is the active backend's current device list, used to
/// know the true device count and to resolve each target token to a real
/// device index.
pub fn parse_reset_process(
    pid: i32,
    name: &str,
    cmdline: &str,
    devices: &[Device],
) -> Option<ResetEvent> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let first_tok = cmdline.split_whitespace().next().unwrap_or("");
    let first_base = first_tok.rsplit('/').next().unwrap_or(first_tok);
    if base != "tt-smi" && first_base != "tt-smi" {
        return None;
    }

    let tokens: Vec<&str> = cmdline.split_whitespace().collect();
    let flag_idx = tokens.iter().position(|t| *t == "-r" || *t == "--reset")?;

    let mut raw_targets: Vec<String> = Vec::new();
    for tok in &tokens[flag_idx + 1..] {
        if tok.starts_with('-') {
            break;
        }
        for piece in tok.split(',') {
            if !piece.is_empty() {
                raw_targets.push(piece.to_string());
            }
        }
    }

    let total_devices = devices.len();
    let is_all_literal =
        raw_targets.is_empty() || raw_targets.iter().any(|t| t.eq_ignore_ascii_case("all"));

    let device_indices: Vec<u8> = if is_all_literal {
        (0..total_devices).filter_map(|i| u8::try_from(i).ok()).collect()
    } else {
        resolve_target_indices(&raw_targets, devices)
    };

    let is_full =
        is_all_literal || (total_devices > 0 && device_indices.len() >= total_devices);

    let chip_count = if is_all_literal {
        total_devices
    } else {
        raw_targets.len()
    };

    Some(ResetEvent {
        pid,
        is_full,
        chip_count,
        total_devices,
        device_indices,
        raw_targets,
    })
}

/// Resolve each raw TARGETS token to a real device index: a bare integer or
/// `/dev/tenstorrent/<n>` is taken as the UMD logical id directly; anything
/// else is matched against each device's real PCI bus id. Unresolvable
/// tokens are silently dropped — never guessed.
fn resolve_target_indices(targets: &[String], devices: &[Device]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in targets {
        let idx = if let Some(rest) = t.strip_prefix("/dev/tenstorrent/") {
            rest.parse::<usize>().ok()
        } else if let Ok(n) = t.parse::<usize>() {
            Some(n)
        } else {
            devices.iter().find(|d| d.bus_id.eq_ignore_ascii_case(t)).map(|d| d.index)
        };
        if let Some(i) = idx {
            if let Ok(b) = u8::try_from(i) {
                out.push(b);
            }
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib reset_detect:: -- --nocapture`
Expected: PASS, all 13 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/workload/reset_detect.rs src/workload/mod.rs
git commit -m "feat: add pure tt-smi -r cmdline parser (ResetEvent)"
```

---

### Task 2: `ResetDetector` lifecycle wrapper

**Files:**
- Modify: `src/workload/reset_detect.rs`

**Interfaces:**
- Consumes: `ResetEvent`, `parse_reset_process` (Task 1); `Device` (as before).
- Produces:
  - `pub struct ResetDetector { .. }` with `pub fn new() -> Self`, `impl Default`
  - `pub fn observe(&mut self, processes: &[(i32, String, String)], devices: &[Device]) -> Option<&ResetEvent>`
  - `pub fn is_finished(&self, processes: &[(i32, String, String)]) -> bool`
  - `pub fn clear(&mut self)`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `src/workload/reset_detect.rs`:

```rust
    fn procs(entries: &[(i32, &str, &str)]) -> Vec<(i32, String, String)> {
        entries
            .iter()
            .map(|(pid, name, cmd)| (*pid, name.to_string(), cmd.to_string()))
            .collect()
    }

    #[test]
    fn detector_observes_a_new_reset() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let live = procs(&[(1, "bash", "bash"), (500, "tt-smi", "tt-smi -r 1")]);
        let ev = det.observe(&live, &devices).expect("should detect reset");
        assert_eq!(ev.pid, 500);
        assert_eq!(ev.device_indices, vec![1]);
    }

    #[test]
    fn detector_ignores_overlapping_second_reset() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let first = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        assert!(det.observe(&first, &devices).is_some());

        let second = procs(&[(500, "tt-smi", "tt-smi -r 0"), (600, "tt-smi", "tt-smi -r 1")]);
        assert!(det.observe(&second, &devices).is_none());
    }

    #[test]
    fn is_finished_false_while_pid_present() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let live = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&live, &devices);
        assert!(!det.is_finished(&live));
    }

    #[test]
    fn is_finished_true_once_pid_gone() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let live = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&live, &devices);
        let gone = procs(&[(1, "bash", "bash")]);
        assert!(det.is_finished(&gone));
    }

    #[test]
    fn is_finished_false_with_nothing_tracked() {
        let det = ResetDetector::new();
        let any = procs(&[(1, "bash", "bash")]);
        assert!(!det.is_finished(&any));
    }

    #[test]
    fn clear_allows_a_later_reset_to_be_tracked() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let first = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&first, &devices);
        det.clear();
        let second = procs(&[(600, "tt-smi", "tt-smi -r 1")]);
        let ev = det.observe(&second, &devices).expect("should detect after clear");
        assert_eq!(ev.pid, 600);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib reset_detect:: -- --nocapture`
Expected: FAIL to compile — `ResetDetector` not found.

- [ ] **Step 3: Implement `ResetDetector`**

Add above the `#[cfg(test)]` block in `src/workload/reset_detect.rs`:

```rust
/// Tracks at most one active `tt-smi -r` invocation at a time. A second
/// reset detected while one is already tracked is dropped (see design
/// spec's "overlapping resets" non-goal) — call `clear()` once its visual
/// consequence (a takeover animation, or a HivemindSweeper injection) is
/// also done, not merely once the process itself exits.
pub struct ResetDetector {
    active: Option<ResetEvent>,
}

impl ResetDetector {
    pub fn new() -> Self {
        Self { active: None }
    }

    /// Scans `processes` — `(pid, name, cmdline)` triples for every process
    /// currently visible, e.g. from
    /// `HostProcessMonitor::processes_snapshot()` — for a new `tt-smi -r`
    /// invocation. Returns `None` if one is already tracked or none is
    /// found.
    pub fn observe(
        &mut self,
        processes: &[(i32, String, String)],
        devices: &[Device],
    ) -> Option<&ResetEvent> {
        if self.active.is_some() {
            return None;
        }
        for (pid, name, cmdline) in processes {
            if let Some(ev) = parse_reset_process(*pid, name, cmdline, devices) {
                self.active = Some(ev);
                return self.active.as_ref();
            }
        }
        None
    }

    /// True once the tracked pid is no longer present in `processes`.
    /// `false` if nothing is being tracked.
    pub fn is_finished(&self, processes: &[(i32, String, String)]) -> bool {
        match &self.active {
            Some(ev) => !processes.iter().any(|(pid, _, _)| *pid == ev.pid),
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.active = None;
    }
}

impl Default for ResetDetector {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib reset_detect:: -- --nocapture`
Expected: PASS, all tests green (13 from Task 1 + 6 new).

- [ ] **Step 5: Commit**

```bash
git add src/workload/reset_detect.rs
git commit -m "feat: add ResetDetector lifecycle wrapper"
```

---

### Task 3: Takeover framework — `TakeoverClock` + `render_takeover_frame`

**Files:**
- Create: `src/animation/takeover/mod.rs`
- Modify: `src/animation/mod.rs` (add `pub mod takeover;`)

**Interfaces:**
- Consumes: nothing yet (variant structs and `ResetEvent` come in later tasks).
- Produces:
  - `pub(crate) struct TakeoverClock` with `pub(crate) fn new() -> Self`, `tick(&mut self, dt: Duration)`, `note_reset_finished(&mut self)`, `skip(&mut self)`, `is_done(&self, done_tail: Duration) -> bool`, `elapsed(&self) -> Duration`, `in_progress(&self) -> bool`
  - `pub(crate) fn render_takeover_frame(f: &mut Frame, area: Rect, title: &str, border_color: Color, lines: Vec<Line<'static>>)`

- [ ] **Step 1: Write the failing tests**

Create `src/animation/takeover/mod.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Full-screen "reset takeover" animations, triggered by
//! `crate::workload::reset_detect` when someone else runs `tt-smi -r`. Each
//! variant is a concrete struct (not a trait object — matching this
//! codebase's `DisplayMode`/`EventKind` convention of enum + match rather
//! than `dyn Trait`), driven by the *real* reset lifecycle via
//! [`TakeoverClock`]: `note_reset_finished` is called the tick the real
//! process exits, and a variant is only done once both the real reset has
//! finished AND its own short "done" resolution beat has played — never a
//! fixed fake timer running independent of the real reset.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use std::time::Duration;

/// Shared lifecycle bookkeeping every takeover variant embeds.
pub(crate) struct TakeoverClock {
    elapsed: Duration,
    finished_at: Option<Duration>,
    skipped: bool,
}

impl TakeoverClock {
    pub(crate) fn new() -> Self {
        Self {
            elapsed: Duration::ZERO,
            finished_at: None,
            skipped: false,
        }
    }

    pub(crate) fn tick(&mut self, dt: Duration) {
        self.elapsed += dt;
    }

    /// Call the tick `ResetDetector::is_finished()` first goes true.
    /// Idempotent — a later call does not push the recorded time forward.
    pub(crate) fn note_reset_finished(&mut self) {
        if self.finished_at.is_none() {
            self.finished_at = Some(self.elapsed);
        }
    }

    pub(crate) fn skip(&mut self) {
        self.skipped = true;
    }

    /// True once skipped, or once the real reset has finished AND
    /// `done_tail` has elapsed since then.
    pub(crate) fn is_done(&self, done_tail: Duration) -> bool {
        self.skipped
            || self
                .finished_at
                .map(|f| self.elapsed.saturating_sub(f) >= done_tail)
                .unwrap_or(false)
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// True until the real reset process has been observed to finish.
    pub(crate) fn in_progress(&self) -> bool {
        self.finished_at.is_none()
    }
}

/// Shared full-screen frame: clears the terminal cells, paints a bordered
/// block (left/bottom borders only, per this project's no-right-border-glyph
/// convention) with `title`, and renders `lines` as a centered paragraph.
/// Every variant's `render` calls this so a takeover always reads as one
/// consistent "something big just happened" moment. No-ops on a terminal too
/// small to safely draw into (matches `render_overlay_panel`'s guard).
pub(crate) fn render_takeover_frame(
    f: &mut Frame,
    area: Rect,
    title: &str,
    border_color: Color,
    lines: Vec<Line<'static>>,
) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::LEFT | Borders::BOTTOM)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let para = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_not_done_while_in_progress() {
        let mut c = TakeoverClock::new();
        c.tick(Duration::from_secs(10));
        assert!(!c.is_done(Duration::from_millis(500)));
        assert!(c.in_progress());
    }

    #[test]
    fn clock_done_after_finish_plus_tail() {
        let mut c = TakeoverClock::new();
        c.tick(Duration::from_millis(100));
        c.note_reset_finished();
        assert!(!c.in_progress());
        assert!(!c.is_done(Duration::from_millis(500))); // tail hasn't elapsed yet
        c.tick(Duration::from_millis(500));
        assert!(c.is_done(Duration::from_millis(500)));
    }

    #[test]
    fn clock_skip_is_immediately_done() {
        let mut c = TakeoverClock::new();
        c.skip();
        assert!(c.is_done(Duration::from_secs(999)));
    }

    #[test]
    fn second_note_reset_finished_does_not_reset_the_tail() {
        let mut c = TakeoverClock::new();
        c.tick(Duration::from_millis(100));
        c.note_reset_finished();
        c.tick(Duration::from_millis(300));
        c.note_reset_finished(); // should be a no-op
        c.tick(Duration::from_millis(300));
        // total elapsed since first finish = 600ms >= 500ms tail
        assert!(c.is_done(Duration::from_millis(500)));
    }

    #[test]
    fn render_takeover_frame_does_not_panic_on_tiny_area() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(3, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_takeover_frame(f, f.area(), "X", Color::White, vec![Line::raw("hi")]);
            })
            .unwrap();
    }

    #[test]
    fn render_takeover_frame_does_not_panic_on_normal_area() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_takeover_frame(
                    f,
                    f.area(),
                    "RESET",
                    Color::Red,
                    vec![Line::raw("line one"), Line::raw("line two")],
                );
            })
            .unwrap();
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod takeover;` to `src/animation/mod.rs` (alongside the other `pub mod` lines), then run:
Run: `cargo test --lib animation::takeover:: -- --nocapture`
Expected: at this point the module compiles as-is (there's no separate "before" state to fail against) — instead run it once with a deliberate typo (e.g. temporarily assert `false` in `clock_not_done_while_in_progress`) to confirm the harness actually executes these tests, then remove the typo. This module doesn't have a pre-existing "missing type" failure mode like Tasks 1–2 since it's new from scratch — the meaningful check is Step 4 passing for real.

- [ ] **Step 3: (already implemented in Step 1)**

No separate implementation step — the code above is the full implementation.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib animation::takeover:: -- --nocapture`
Expected: PASS, all 6 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/mod.rs src/animation/mod.rs
git commit -m "feat: add takeover animation framework (TakeoverClock + shared frame)"
```

---

### Task 4: `QuietNoticeTakeover` variant

**Files:**
- Create: `src/animation/takeover/quiet_notice.rs`
- Modify: `src/animation/takeover/mod.rs` (add `mod quiet_notice; pub use quiet_notice::QuietNoticeTakeover;`)

**Interfaces:**
- Consumes: `ResetEvent` (Task 1: `is_full: bool`, `chip_count: usize` fields), `TakeoverClock`/`render_takeover_frame` (Task 3).
- Produces: `pub struct QuietNoticeTakeover` with `pub fn new(ev: &ResetEvent) -> Self`, `pub fn tick(&mut self, dt: Duration)`, `pub fn render(&self, f: &mut Frame, area: Rect)`, `pub fn is_done(&self) -> bool`, `pub fn note_reset_finished(&mut self)`, `pub fn skip(&mut self)`.

- [ ] **Step 1: Write the failing test**

Create `src/animation/takeover/quiet_notice.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Minimal full-screen status readout — "the tool doing its thing."

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(600);

pub struct QuietNoticeTakeover {
    clock: TakeoverClock,
    is_full: bool,
    chip_count: usize,
}

impl QuietNoticeTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            is_full: ev.is_full,
            chip_count: ev.chip_count,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let status = if self.clock.in_progress() {
            "resetting…"
        } else {
            "reset complete"
        };
        let scope = if self.is_full {
            "all chips".to_string()
        } else {
            format!("{} chip(s)", self.chip_count)
        };
        let lines = vec![
            Line::from(Span::raw(format!("tt-smi -r — {scope}"))),
            Line::from(Span::styled(
                status,
                Style::default().fg(colors::rgb(180, 180, 190)),
            )),
        ];
        render_takeover_frame(f, area, "RESET", colors::rgb(140, 140, 150), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(is_full: bool, chip_count: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full,
            chip_count,
            total_devices: 4,
            device_indices: vec![0],
            raw_targets: vec!["0".to_string()],
        }
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = QuietNoticeTakeover::new(&ev(true, 4));
        t.tick(Duration::from_secs(5));
        assert!(!t.is_done());
        t.note_reset_finished();
        assert!(!t.is_done());
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = QuietNoticeTakeover::new(&ev(false, 2));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = QuietNoticeTakeover::new(&ev(false, 2));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Add to `src/animation/takeover/mod.rs`:
```rust
mod quiet_notice;
pub use quiet_notice::QuietNoticeTakeover;
```
Run: `cargo test --lib takeover::quiet_notice:: -- --nocapture`
Expected: compiles and passes immediately (code is complete from Step 1) — confirm by temporarily breaking `is_done`'s tail comparison (`>=` → `>`) to see `not_done_until_finished_and_tail_elapsed` go red, then revert.

- [ ] **Step 3: (implemented in Step 1)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib takeover::quiet_notice:: -- --nocapture`
Expected: PASS, 3 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/quiet_notice.rs src/animation/takeover/mod.rs
git commit -m "feat: add QuietNotice takeover variant"
```

---

### Task 5: `MissileCommandTakeover` variant

**Files:**
- Create: `src/animation/takeover/missile_command.rs`
- Modify: `src/animation/takeover/mod.rs` (add `mod missile_command; pub use missile_command::MissileCommandTakeover;`)

**Interfaces:**
- Consumes: `ResetEvent` (`device_indices: Vec<u8>`, `total_devices: usize`), `TakeoverClock`/`render_takeover_frame`.
- Produces: `pub struct MissileCommandTakeover` with the same five-method surface as Task 4.

- [ ] **Step 1: Write the failing test**

Create `src/animation/takeover/missile_command.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Missile Command — crosshairs/explosions only at the actual affected
//! chip(s), so a subset reset visibly fires at just those chips.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(800);
/// How long each `[*]`/`[ ]` blink half-cycle lasts while in progress.
const BLINK_MS: u128 = 250;

pub struct MissileCommandTakeover {
    clock: TakeoverClock,
    device_indices: Vec<u8>,
    total_devices: usize,
}

impl MissileCommandTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            device_indices: ev.device_indices.clone(),
            total_devices: ev.total_devices,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let frame_on = (self.clock.elapsed().as_millis() / BLINK_MS) % 2 == 0;
        let mut cells = String::new();
        let total = self.total_devices.max(1);
        for i in 0..total {
            let targeted = self.device_indices.contains(&(i as u8));
            let glyph = if !targeted {
                "[ ]"
            } else if self.clock.in_progress() {
                if frame_on {
                    "[*]"
                } else {
                    "[ ]"
                }
            } else {
                "[X]"
            };
            cells.push_str(glyph);
        }
        let lines = vec![
            Line::from(Span::raw("INCOMING RESET")),
            Line::from(Span::styled(
                cells,
                Style::default().fg(colors::rgb(255, 120, 90)),
            )),
        ];
        render_takeover_frame(f, area, "MISSILE COMMAND", colors::rgb(255, 90, 60), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(device_indices: Vec<u8>, total_devices: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: device_indices.len() == total_devices,
            chip_count: device_indices.len(),
            total_devices,
            device_indices,
            raw_targets: vec![],
        }
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = MissileCommandTakeover::new(&ev(vec![1], 4));
        t.tick(Duration::from_secs(3));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = MissileCommandTakeover::new(&ev(vec![0], 4));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_with_subset_targets() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = MissileCommandTakeover::new(&ev(vec![1, 2], 4));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_zero_total_devices() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = MissileCommandTakeover::new(&ev(vec![], 0));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Add to `src/animation/takeover/mod.rs`:
```rust
mod missile_command;
pub use missile_command::MissileCommandTakeover;
```
Run: `cargo test --lib takeover::missile_command:: -- --nocapture`
Expected: compiles as written; verify the harness truly exercises it by temporarily changing `>=` to `>` in `is_done`'s tail check reasoning (or asserting `false` in one test), observe red, then revert.

- [ ] **Step 3: (implemented in Step 1)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib takeover::missile_command:: -- --nocapture`
Expected: PASS, 4 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/missile_command.rs src/animation/takeover/mod.rs
git commit -m "feat: add MissileCommand takeover variant"
```

---

### Task 6: `BbsTakeover` variant

**Files:**
- Create: `src/animation/takeover/bbs.rs`
- Modify: `src/animation/takeover/mod.rs` (add `mod bbs; pub use bbs::BbsTakeover;`)

**Interfaces:**
- Consumes: `ResetEvent` (`is_full: bool`, `chip_count: usize`), `TakeoverClock`/`render_takeover_frame`.
- Produces: `pub struct BbsTakeover` with the same five-method surface.

- [ ] **Step 1: Write the failing test**

Create `src/animation/takeover/bbs.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! BBS sysop takeover — "the sysop wants to chat." Per-chip log lines
//! scroll in one at a time, paced by `chip_count`, looping while the real
//! reset is still in progress rather than racing ahead of it.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
const BEAT_MS: u128 = 400;

pub struct BbsTakeover {
    clock: TakeoverClock,
    chip_count: usize,
    is_full: bool,
}

impl BbsTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            chip_count: ev.chip_count.max(1),
            is_full: ev.is_full,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    /// How many "CHIP N: reset ack" lines should be visible right now,
    /// capped at `chip_count` and looping (mod `chip_count`) while the real
    /// reset is still in progress, so a longer real reset doesn't run out of
    /// choreography.
    fn visible_lines(&self) -> usize {
        let beat = (self.clock.elapsed().as_millis() / BEAT_MS) as usize;
        if self.clock.in_progress() {
            (beat % self.chip_count) + 1
        } else {
            self.chip_count
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![
            Line::from(Span::styled(
                "** SYSOP HAS TAKEN OVER THIS TERMINAL **",
                Style::default().fg(colors::rgb(0, 255, 0)),
            )),
            Line::from(Span::raw("")),
        ];
        let visible = self.visible_lines();
        for chip in 0..visible.min(self.chip_count) {
            lines.push(Line::from(Span::raw(format!(
                "CHIP {chip}: reset ack received..."
            ))));
        }
        if !self.clock.in_progress() {
            lines.push(Line::from(Span::styled(
                "CONNECTION RESTORED.",
                Style::default().fg(colors::rgb(0, 255, 0)),
            )));
        }
        let title = if self.is_full {
            "SYSTEM-WIDE RESET"
        } else {
            "PARTIAL RESET"
        };
        render_takeover_frame(f, area, title, colors::rgb(0, 220, 0), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(is_full: bool, chip_count: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full,
            chip_count,
            total_devices: 4,
            device_indices: vec![0],
            raw_targets: vec![],
        }
    }

    #[test]
    fn visible_lines_loops_while_in_progress() {
        let mut t = BbsTakeover::new(&ev(true, 2));
        t.tick(Duration::from_millis(BEAT_MS as u64 * 3)); // beat=3, 3%2=1 → 2 lines visible
        assert_eq!(t.visible_lines(), 2);
    }

    #[test]
    fn visible_lines_is_full_chip_count_once_finished() {
        let mut t = BbsTakeover::new(&ev(true, 5));
        t.note_reset_finished();
        assert_eq!(t.visible_lines(), 5);
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = BbsTakeover::new(&ev(false, 1));
        t.tick(Duration::from_secs(2));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = BbsTakeover::new(&ev(true, 8));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Add to `src/animation/takeover/mod.rs`:
```rust
mod bbs;
pub use bbs::BbsTakeover;
```
Run: `cargo test --lib takeover::bbs:: -- --nocapture`
Expected: compiles as written; sanity-check by temporarily changing the `% self.chip_count` to `% (self.chip_count + 1)` in `visible_lines`, observe `visible_lines_loops_while_in_progress` go red, then revert.

- [ ] **Step 3: (implemented in Step 1)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib takeover::bbs:: -- --nocapture`
Expected: PASS, 4 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/bbs.rs src/animation/takeover/mod.rs
git commit -m "feat: add BBS sysop takeover variant"
```

---

### Task 7: `BlackholeSwarmTakeover` variant

**Files:**
- Create: `src/animation/takeover/blackhole_swarm.rs`
- Modify: `src/animation/takeover/mod.rs` (add `mod blackhole_swarm; pub use blackhole_swarm::BlackholeSwarmTakeover;`)

**Interfaces:**
- Consumes: `ResetEvent` (`chip_count: usize`), `TakeoverClock`/`render_takeover_frame`.
- Produces: `pub struct BlackholeSwarmTakeover` with the same five-method surface.

- [ ] **Step 1: Write the failing test**

Create `src/animation/takeover/blackhole_swarm.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! "What would 1024 Blackholes look like" — a dense synthetic glyph swarm
//! whose density scales with the real `chip_count`, capped to what the
//! terminal can actually show.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
/// Synthetic glyphs per real targeted chip — a stylized multiplier, not a
/// claim about real telemetry (see design spec: content should *scale with*
/// chip_count, not literally count real per-chip signals we don't have).
const GLYPHS_PER_CHIP: usize = 32;
const MAX_GLYPHS: usize = 1024;

pub struct BlackholeSwarmTakeover {
    clock: TakeoverClock,
    density: usize,
}

impl BlackholeSwarmTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        let density = (ev.chip_count.max(1) * GLYPHS_PER_CHIP).min(MAX_GLYPHS);
        Self {
            clock: TakeoverClock::new(),
            density,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let cols = area.width.saturating_sub(4).max(1) as usize;
        let rows = area.height.saturating_sub(4).max(1) as usize;
        let cap = cols * rows;
        let filled = self.density.min(cap);
        let mut lines = Vec::with_capacity(rows);
        let mut remaining = filled;
        for _ in 0..rows {
            let take = remaining.min(cols);
            remaining -= take;
            let mut row = String::with_capacity(cols);
            for c in 0..cols {
                row.push(if c < take { '¤' } else { ' ' });
            }
            lines.push(Line::from(Span::styled(
                row,
                Style::default().fg(colors::rgb(160, 100, 255)),
            )));
        }
        render_takeover_frame(
            f,
            area,
            &format!("{} BLACKHOLES", self.density),
            colors::rgb(160, 100, 255),
            lines,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(chip_count: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: true,
            chip_count,
            total_devices: chip_count,
            device_indices: (0..chip_count as u8).collect(),
            raw_targets: vec![],
        }
    }

    #[test]
    fn density_scales_with_chip_count_and_caps() {
        assert_eq!(BlackholeSwarmTakeover::new(&ev(1)).density, 32);
        assert_eq!(BlackholeSwarmTakeover::new(&ev(4)).density, 128);
        assert_eq!(BlackholeSwarmTakeover::new(&ev(64)).density, 1024); // capped
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = BlackholeSwarmTakeover::new(&ev(4));
        t.tick(Duration::from_secs(2));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_on_small_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(10, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = BlackholeSwarmTakeover::new(&ev(32));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Add to `src/animation/takeover/mod.rs`:
```rust
mod blackhole_swarm;
pub use blackhole_swarm::BlackholeSwarmTakeover;
```
Run: `cargo test --lib takeover::blackhole_swarm:: -- --nocapture`
Expected: compiles as written; sanity-check `density_scales_with_chip_count_and_caps` by temporarily changing `GLYPHS_PER_CHIP` to a wrong value, observe red, then revert.

- [ ] **Step 3: (implemented in Step 1)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib takeover::blackhole_swarm:: -- --nocapture`
Expected: PASS, 3 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/blackhole_swarm.rs src/animation/takeover/mod.rs
git commit -m "feat: add 1024-Blackholes-swarm takeover variant"
```

---

### Task 8: `HatchCountdownTakeover` variant

**Files:**
- Create: `src/animation/takeover/hatch_countdown.rs`
- Modify: `src/animation/takeover/mod.rs` (add `mod hatch_countdown; pub use hatch_countdown::HatchCountdownTakeover;`)

**Interfaces:**
- Consumes: `ResetEvent` (only used for construction symmetry with the other variants; no fields read), `TakeoverClock`/`render_takeover_frame`.
- Produces: `pub struct HatchCountdownTakeover` with the same five-method surface.

- [ ] **Step 1: Write the failing test**

Create `src/animation/takeover/hatch_countdown.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Lost-hatch-style repeating countdown, paced by real elapsed/in-progress
//! time rather than a single fixed guess at how long the real reset takes.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
const CYCLE_MS: u128 = 30_000;

pub struct HatchCountdownTakeover {
    clock: TakeoverClock,
}

impl HatchCountdownTakeover {
    pub fn new(_ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    /// Seconds remaining in the current repeating countdown cycle, or `0`
    /// once the real reset has finished.
    fn remaining_secs(&self) -> u64 {
        if !self.clock.in_progress() {
            return 0;
        }
        let pos = self.clock.elapsed().as_millis() % CYCLE_MS;
        ((CYCLE_MS - pos) / 1000) as u64
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(Span::raw("THE HATCH")),
            Line::from(Span::styled(
                format!("{:04}", self.remaining_secs()),
                Style::default().fg(colors::rgb(255, 80, 0)),
            )),
            Line::from(Span::raw(if self.clock.in_progress() {
                "EXECUTE"
            } else {
                "SYSTEM RESET"
            })),
        ];
        render_takeover_frame(f, area, "DHARMA INITIATIVE", colors::rgb(255, 140, 0), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: true,
            chip_count: 4,
            total_devices: 4,
            device_indices: vec![0, 1, 2, 3],
            raw_targets: vec![],
        }
    }

    #[test]
    fn remaining_secs_counts_down_within_a_cycle() {
        let mut t = HatchCountdownTakeover::new(&ev());
        let start = t.remaining_secs();
        t.tick(Duration::from_secs(5));
        assert_eq!(t.remaining_secs(), start.saturating_sub(5));
    }

    #[test]
    fn remaining_secs_zero_once_finished() {
        let mut t = HatchCountdownTakeover::new(&ev());
        t.note_reset_finished();
        assert_eq!(t.remaining_secs(), 0);
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = HatchCountdownTakeover::new(&ev());
        t.tick(Duration::from_secs(40)); // past one full cycle, still "in progress"
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = HatchCountdownTakeover::new(&ev());
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Add to `src/animation/takeover/mod.rs`:
```rust
mod hatch_countdown;
pub use hatch_countdown::HatchCountdownTakeover;
```
Run: `cargo test --lib takeover::hatch_countdown:: -- --nocapture`
Expected: compiles as written; sanity-check `remaining_secs_counts_down_within_a_cycle` by temporarily flipping the subtraction, observe red, then revert.

- [ ] **Step 3: (implemented in Step 1)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib takeover::hatch_countdown:: -- --nocapture`
Expected: PASS, 4 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/hatch_countdown.rs src/animation/takeover/mod.rs
git commit -m "feat: add Lost-hatch-countdown takeover variant"
```

---

### Task 9: `TrekResetTakeover` variant

**Files:**
- Create: `src/animation/takeover/trek_reset.rs`
- Modify: `src/animation/takeover/mod.rs` (add `mod trek_reset; pub use trek_reset::TrekResetTakeover;`)

**Interfaces:**
- Consumes: `ResetEvent` (`device_indices: Vec<u8>`, `total_devices: usize`), `TakeoverClock`/`render_takeover_frame`.
- Produces: `pub struct TrekResetTakeover` with the same five-method surface.

- [ ] **Step 1: Write the failing test**

Create `src/animation/takeover/trek_reset.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Classic 1970s-BASIC-style "Star Trek" reset screen: a STARDATE header, a
//! short-range sensor scan grid (one cell per known chip, targeted ones
//! shown as Klingons under fire), and a torpedo/destroyed status line.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);

pub struct TrekResetTakeover {
    clock: TakeoverClock,
    device_indices: Vec<u8>,
    total_devices: usize,
    /// Cosmetic flavor text only — derived from real wall-clock time so
    /// repeated runs aren't identical, never used for any real pacing
    /// decision (pacing is entirely `TakeoverClock`-driven).
    stardate: String,
}

impl TrekResetTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let stardate = format!("{:.1}", (now.as_secs() as f64 / 86_400.0) % 10_000.0);
        Self {
            clock: TakeoverClock::new(),
            device_indices: ev.device_indices.clone(),
            total_devices: ev.total_devices,
            stardate,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![
            Line::from(Span::styled(
                format!("STARDATE {}", self.stardate),
                Style::default().fg(colors::rgb(255, 204, 0)),
            )),
            Line::from(Span::raw("SHORT RANGE SENSOR SCAN")),
            Line::from(Span::raw("")),
        ];
        let mut row = String::new();
        let total = self.total_devices.max(1);
        for i in 0..total {
            let targeted = self.device_indices.contains(&(i as u8));
            row.push(if !targeted {
                '.'
            } else if self.clock.in_progress() {
                'K'
            } else {
                '*'
            });
            row.push(' ');
        }
        lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(colors::rgb(0, 255, 120)),
        )));
        lines.push(Line::from(Span::raw(if self.clock.in_progress() {
            "PHOTON TORPEDOES AWAY..."
        } else {
            "KLINGON BATTLE CRUISER DESTROYED"
        })));
        render_takeover_frame(f, area, "USS ENTERPRISE", colors::rgb(0, 180, 255), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(device_indices: Vec<u8>, total_devices: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: device_indices.len() == total_devices,
            chip_count: device_indices.len(),
            total_devices,
            device_indices,
            raw_targets: vec![],
        }
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = TrekResetTakeover::new(&ev(vec![0], 4));
        t.tick(Duration::from_secs(2));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = TrekResetTakeover::new(&ev(vec![0], 4));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_with_full_and_subset_targets() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let full = TrekResetTakeover::new(&ev(vec![0, 1, 2, 3], 4));
        terminal.draw(|f| full.render(f, f.area())).unwrap();
        let subset = TrekResetTakeover::new(&ev(vec![2], 4));
        terminal.draw(|f| subset.render(f, f.area())).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Add to `src/animation/takeover/mod.rs`:
```rust
mod trek_reset;
pub use trek_reset::TrekResetTakeover;
```
Run: `cargo test --lib takeover::trek_reset:: -- --nocapture`
Expected: compiles as written; sanity-check by temporarily flipping the `'K'`/`'*'` targeted-glyph branch condition and confirming a render-content assertion would catch it if one existed — since this variant has no content-assertion test (rendering is a `does_not_panic` smoke test only, matching this codebase's established convention, e.g. `render_snake_view_with_band_does_not_panic`), instead sanity-check by temporarily breaking `is_done`'s tail comparison and observing `not_done_until_finished_and_tail_elapsed` go red, then revert.

- [ ] **Step 3: (implemented in Step 1)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib takeover::trek_reset:: -- --nocapture`
Expected: PASS, 3 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/trek_reset.rs src/animation/takeover/mod.rs
git commit -m "feat: add Star Trek reset-screen takeover variant"
```

---

### Task 10: `Takeover` enum + weighted `pick_takeover`

**Files:**
- Modify: `src/animation/takeover/mod.rs`

**Interfaces:**
- Consumes: all six variant structs (Tasks 4–9), `ResetEvent` (Task 1).
- Produces:
  - `pub enum Takeover { Bbs(BbsTakeover), BlackholeSwarm(BlackholeSwarmTakeover), HatchCountdown(HatchCountdownTakeover), MissileCommand(MissileCommandTakeover), TrekReset(TrekResetTakeover), QuietNotice(QuietNoticeTakeover) }` with inherent `tick`, `render`, `is_done`, `note_reset_finished`, `skip` methods matching the six-variant surface.
  - `pub fn pick_takeover(ev: &ResetEvent) -> Takeover` (uses real randomness)
  - `fn pick_takeover_from_roll(ev: &ResetEvent, roll: u8) -> Takeover` (pure, testable — `pick_takeover` is a one-line wrapper around this)

- [ ] **Step 1: Write the failing tests**

Append to `src/animation/takeover/mod.rs`, above the existing `#[cfg(test)] mod tests` block:

```rust
use crate::workload::reset_detect::ResetEvent;

/// One active full-screen takeover animation.
pub enum Takeover {
    Bbs(BbsTakeover),
    BlackholeSwarm(BlackholeSwarmTakeover),
    HatchCountdown(HatchCountdownTakeover),
    MissileCommand(MissileCommandTakeover),
    TrekReset(TrekResetTakeover),
    QuietNotice(QuietNoticeTakeover),
}

impl Takeover {
    pub fn tick(&mut self, elapsed: Duration) {
        match self {
            Takeover::Bbs(v) => v.tick(elapsed),
            Takeover::BlackholeSwarm(v) => v.tick(elapsed),
            Takeover::HatchCountdown(v) => v.tick(elapsed),
            Takeover::MissileCommand(v) => v.tick(elapsed),
            Takeover::TrekReset(v) => v.tick(elapsed),
            Takeover::QuietNotice(v) => v.tick(elapsed),
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        match self {
            Takeover::Bbs(v) => v.render(f, area),
            Takeover::BlackholeSwarm(v) => v.render(f, area),
            Takeover::HatchCountdown(v) => v.render(f, area),
            Takeover::MissileCommand(v) => v.render(f, area),
            Takeover::TrekReset(v) => v.render(f, area),
            Takeover::QuietNotice(v) => v.render(f, area),
        }
    }

    pub fn is_done(&self) -> bool {
        match self {
            Takeover::Bbs(v) => v.is_done(),
            Takeover::BlackholeSwarm(v) => v.is_done(),
            Takeover::HatchCountdown(v) => v.is_done(),
            Takeover::MissileCommand(v) => v.is_done(),
            Takeover::TrekReset(v) => v.is_done(),
            Takeover::QuietNotice(v) => v.is_done(),
        }
    }

    pub fn note_reset_finished(&mut self) {
        match self {
            Takeover::Bbs(v) => v.note_reset_finished(),
            Takeover::BlackholeSwarm(v) => v.note_reset_finished(),
            Takeover::HatchCountdown(v) => v.note_reset_finished(),
            Takeover::MissileCommand(v) => v.note_reset_finished(),
            Takeover::TrekReset(v) => v.note_reset_finished(),
            Takeover::QuietNotice(v) => v.note_reset_finished(),
        }
    }

    pub fn skip(&mut self) {
        match self {
            Takeover::Bbs(v) => v.skip(),
            Takeover::BlackholeSwarm(v) => v.skip(),
            Takeover::HatchCountdown(v) => v.skip(),
            Takeover::MissileCommand(v) => v.skip(),
            Takeover::TrekReset(v) => v.skip(),
            Takeover::QuietNotice(v) => v.skip(),
        }
    }
}

/// Weighted-random pick of a variant for a detected reset (real entropy —
/// see `pick_takeover_from_roll` for the deterministic, testable core).
/// `is_full` weights toward the four spectacle variants; a subset reset
/// weights toward `MissileCommand` (scoped to the real targets) and
/// `QuietNotice`.
pub fn pick_takeover(ev: &ResetEvent) -> Takeover {
    use rand::Rng;
    let roll: u8 = rand::rng().random_range(0..100);
    pick_takeover_from_roll(ev, roll)
}

/// Pure selection core: `roll` in `0..100` maps to a variant. Full-reset
/// weights: Bbs 25, BlackholeSwarm 25, HatchCountdown 20, TrekReset 20,
/// MissileCommand 5, QuietNotice 5. Subset weights: MissileCommand 45,
/// QuietNotice 35, TrekReset 10, Bbs 5, BlackholeSwarm 3, HatchCountdown 2.
fn pick_takeover_from_roll(ev: &ResetEvent, roll: u8) -> Takeover {
    if ev.is_full {
        match roll {
            0..=24 => Takeover::Bbs(BbsTakeover::new(ev)),
            25..=49 => Takeover::BlackholeSwarm(BlackholeSwarmTakeover::new(ev)),
            50..=69 => Takeover::HatchCountdown(HatchCountdownTakeover::new(ev)),
            70..=89 => Takeover::TrekReset(TrekResetTakeover::new(ev)),
            90..=94 => Takeover::MissileCommand(MissileCommandTakeover::new(ev)),
            _ => Takeover::QuietNotice(QuietNoticeTakeover::new(ev)),
        }
    } else {
        match roll {
            0..=44 => Takeover::MissileCommand(MissileCommandTakeover::new(ev)),
            45..=79 => Takeover::QuietNotice(QuietNoticeTakeover::new(ev)),
            80..=89 => Takeover::TrekReset(TrekResetTakeover::new(ev)),
            90..=94 => Takeover::Bbs(BbsTakeover::new(ev)),
            95..=97 => Takeover::BlackholeSwarm(BlackholeSwarmTakeover::new(ev)),
            _ => Takeover::HatchCountdown(HatchCountdownTakeover::new(ev)),
        }
    }
}
```

Add these tests to the existing `#[cfg(test)] mod tests` block at the bottom of the file:

```rust
    fn full_ev() -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: true,
            chip_count: 4,
            total_devices: 4,
            device_indices: vec![0, 1, 2, 3],
            raw_targets: vec![],
        }
    }

    fn subset_ev() -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: false,
            chip_count: 1,
            total_devices: 4,
            device_indices: vec![2],
            raw_targets: vec!["2".to_string()],
        }
    }

    #[test]
    fn full_reset_roll_boundaries_pick_expected_variant() {
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 0),
            Takeover::Bbs(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 24),
            Takeover::Bbs(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 25),
            Takeover::BlackholeSwarm(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 90),
            Takeover::MissileCommand(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 99),
            Takeover::QuietNotice(_)
        ));
    }

    #[test]
    fn subset_reset_roll_boundaries_pick_expected_variant() {
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 0),
            Takeover::MissileCommand(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 45),
            Takeover::QuietNotice(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 99),
            Takeover::HatchCountdown(_)
        ));
    }

    #[test]
    fn every_roll_value_produces_some_variant() {
        // Exhaustiveness check: no roll value should ever fail to match
        // (the match is total via `_`, but this pins that every branch is
        // actually reachable without panicking across the full range).
        for roll in 0..=255u8 {
            let _ = pick_takeover_from_roll(&full_ev(), roll);
            let _ = pick_takeover_from_roll(&subset_ev(), roll);
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib takeover:: -- --nocapture`
Expected: FAIL to compile — `Takeover`/`pick_takeover_from_roll` not found (they were just added in Step 1 above the test block, so this should actually compile; if so, verify the tests are meaningful by temporarily shifting one boundary, e.g. change `0..=24` to `0..=23`, observe `full_reset_roll_boundaries_pick_expected_variant` go red at `roll = 24`, then revert).

- [ ] **Step 3: (implemented in Step 1)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib takeover:: -- --nocapture`
Expected: PASS — full test suite for the `takeover` module (Tasks 3–10 combined) green.
Also run: `cargo clippy --lib --features tui -- -D warnings` (per this project's CI gotcha — code that only compiles under `cfg(test)` can dangle as "unused" under clippy's exact flags) and fix any warning before proceeding.

- [ ] **Step 5: Commit**

```bash
git add src/animation/takeover/mod.rs
git commit -m "feat: add Takeover enum + weighted variant selection"
```

---

### Task 11: `EventKind::Reset` + `Hivemind::inject_reset`

**Files:**
- Modify: `src/workload/hivemind/event.rs`
- Modify: `src/workload/hivemind/mod.rs`

**Interfaces:**
- Consumes: `Source::TtSmi` (already exists in `src/workload/hivemind/event.rs`), `HeatGrid::bump_at`, `FeedAgg::ingest` (already exist, used by `Hivemind::poll`).
- Produces:
  - New enum variant `EventKind::Reset`
  - `pub fn Hivemind::inject_reset(&mut self, text: String, device_indices: &[u8])`

- [ ] **Step 1: Write the failing test**

Add to `src/workload/hivemind/event.rs`'s `EventKind` enum (in the existing `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum EventKind { ... }` block):

```rust
    /// A detected `tt-smi -r` reset, injected by the main loop's own
    /// detector rather than a collector thread. Distinct from `DriverMsg`
    /// so the feed pane can give it its own visual treatment.
    Reset,
```

This project's existing convention in this file (see `mod tests` and `mod engine_tests` already in `src/workload/hivemind/mod.rs`) is one `#[cfg(test)]` module per logical group of tests, each with its own `use super::*;`. Add a third one at the bottom of the file, following `engine_tests`'s pattern of importing `Column` for grid assertions:

```rust
#[cfg(test)]
mod inject_reset_tests {
    use super::*;
    use crate::workload::hivemind::grid::Column;

    #[test]
    fn inject_reset_lands_in_events_and_bumps_grid_per_chip() {
        let mut hive = Hivemind::new();
        hive.inject_reset("tt-smi -r: 2 chip(s) targeted".to_string(), &[0, 2]);

        assert_eq!(hive.events().len(), 1);
        let ev = hive.events().back().unwrap();
        assert_eq!(ev.source, Source::TtSmi);
        assert_eq!(ev.kind, EventKind::Reset);
        assert_eq!(ev.severity, Severity::Warn);
        assert!(ev.text.contains("2 chip(s)"));

        // Both targeted chips got a real heat-grid bump — not just device 0.
        assert!(hive.grid().heat(Source::TtSmi, Column::Device(0)) > 0.0);
        assert!(hive.grid().heat(Source::TtSmi, Column::Device(2)) > 0.0);
    }

    #[test]
    fn inject_reset_with_no_resolved_chips_still_bumps_the_host_column() {
        let mut hive = Hivemind::new();
        hive.inject_reset("tt-smi -r: unresolved target".to_string(), &[]);
        assert_eq!(hive.events().len(), 1);
        assert!(hive.grid().heat(Source::TtSmi, Column::Host) > 0.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib hivemind::inject_reset_tests:: -- --nocapture`
Expected: FAIL to compile — `EventKind::Reset` and `Hivemind::inject_reset` not found.

- [ ] **Step 3: Implement `inject_reset`**

Add to `impl Hivemind` in `src/workload/hivemind/mod.rs`, near `push_for_test`:

```rust
    /// Inject a detected `tt-smi -r` reset as a real event, from outside the
    /// collector-thread pipeline (the main loop's own reset detector, which
    /// runs regardless of whether this engine's collector threads are
    /// started). Goes through the same grid/feed/ring update path `poll()`
    /// uses for a collector-sourced event, plus one heat-grid bump per
    /// actually-affected chip — not a fake amplitude multiplier — so a
    /// full-box reset visibly lights up wider than a single-chip one.
    pub fn inject_reset(&mut self, text: String, device_indices: &[u8]) {
        let now = Instant::now();
        let device = device_indices.first().copied();
        let ev = SniffEvent {
            ts: now,
            source: Source::TtSmi,
            device,
            severity: Severity::Warn,
            kind: EventKind::Reset,
            text,
            origin: "reset-detect".to_string(),
        };
        if device_indices.is_empty() {
            self.grid.bump_at(Source::TtSmi, None, now);
        } else {
            for &d in device_indices {
                self.grid.bump_at(Source::TtSmi, Some(d), now);
            }
        }
        self.feed.ingest(&ev, now);
        self.ingest(ev);
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib hivemind:: -- --nocapture`
Expected: PASS — the two new tests, plus the existing hivemind test suite unaffected.

- [ ] **Step 5: Commit**

```bash
git add src/workload/hivemind/event.rs src/workload/hivemind/mod.rs
git commit -m "feat: add EventKind::Reset + Hivemind::inject_reset"
```

---

### Task 12: CLI/config flag + `processes_snapshot` + main-loop wiring

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/config.rs`
- Modify: `src/workload/host_processes.rs`
- Modify: `src/ui/tui/mod.rs`

**Interfaces:**
- Consumes: `ResetDetector`/`ResetEvent` (Tasks 1–2), `Takeover`/`pick_takeover` (Tasks 3–10), `Hivemind::inject_reset` (Task 11), `DisplayMode::HivemindSweeper` (existing), `HostProcessMonitor` (existing).
- Produces: `Cli.reset_takeover: bool`, `AnimConfigOverrides.reset_takeover: Option<bool>`, `HostProcessMonitor::processes_snapshot(&self) -> Vec<(i32, String, String)>`, and the wired-up behavior in `run_app`.

- [ ] **Step 1: Write the failing tests**

Add to `src/workload/host_processes.rs`'s existing `#[cfg(test)]` module (search for it near the bottom of the file):

```rust
    #[test]
    fn processes_snapshot_includes_pid_name_and_cmdline() {
        let mut mon = HostProcessMonitor::new();
        mon.update();
        let snap = mon.processes_snapshot();
        // Every running test process (at minimum this test binary itself)
        // should show up with a non-empty name.
        assert!(!snap.is_empty());
        assert!(snap.iter().all(|(_, name, _)| !name.is_empty()));
    }
```

Add to `src/cli.rs`'s existing `mod tests` block (at line 583), matching the file's established convention of `Cli::try_parse_from(...)` (see the `--mode` spelling tests already there):

```rust
    #[test]
    fn reset_takeover_flag_defaults_to_false() {
        let cli = Cli::try_parse_from(["tt-toplike"]).unwrap();
        assert!(!cli.reset_takeover);
    }

    #[test]
    fn reset_takeover_flag_can_be_set() {
        let cli = Cli::try_parse_from(["tt-toplike", "--reset-takeover"]).unwrap();
        assert!(cli.reset_takeover);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib host_processes::tests::processes_snapshot_includes_pid_name_and_cmdline cli::tests::reset_takeover -- --nocapture`
Expected: FAIL to compile — `processes_snapshot` and `Cli.reset_takeover` not found.

- [ ] **Step 3: Implement the flag, the snapshot method, and the wiring**

Add to `src/cli.rs`'s `Cli` struct, near the other standalone bool flags (e.g. right after the `idle_on_blur` field around line 235):

```rust
    /// Opt-in: react to a detected `tt-smi -r` reset with a full-screen
    /// takeover animation (or, in HivemindSweeper, a real feed event). Off
    /// by default.
    #[arg(long)]
    pub reset_takeover: bool,
```

Add to `src/config.rs`'s `AnimConfigOverrides` struct:

```rust
    /// Equivalent of `--reset-takeover`, settable via config file instead of
    /// the CLI flag. Not an animation-sensitivity value like the other
    /// fields here — reuses this struct because it's the only config file
    /// this project has.
    pub reset_takeover: Option<bool>,
```

Add to `src/workload/host_processes.rs`, in `impl HostProcessMonitor` (near `detected_runtimes`):

```rust
    /// Cheap `(pid, name, full cmdline)` snapshot of every process seen in
    /// the current refresh — no TT-specific filtering. Consumed by
    /// `crate::workload::reset_detect::ResetDetector` so it doesn't need its
    /// own `sysinfo` refresh cadence.
    pub fn processes_snapshot(&self) -> Vec<(i32, String, String)> {
        self.sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let name = p.name().to_string_lossy().to_string();
                let cmdline = p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                (i32::try_from(pid.as_u32()).unwrap_or(i32::MAX), name, cmdline)
            })
            .collect()
    }
```

Now wire it into `src/ui/tui/mod.rs`. Four separate, precisely located edits:

**(a) State initialization** — near the other per-run state declared just before the `loop {` at line 676 (alongside `let mut snake = crate::animation::Snake::new();`):

```rust
    let reset_takeover_enabled =
        cli.reset_takeover || crate::config::load_config_overrides().reset_takeover.unwrap_or(false);
    let mut reset_detector = crate::workload::reset_detect::ResetDetector::new();
    let mut takeover: Option<crate::animation::takeover::Takeover> = None;
```

**(b) Detection, on the existing 2-second process-refresh cadence** — insert immediately after `host_proc_monitor.update();` at line 2338 (inside the existing `if last_proc_rows_update.elapsed() >= Duration::from_secs(2) { ... }` block):

```rust
            // ── Reset takeover detection (opt-in, `--reset-takeover`) ──────
            if reset_takeover_enabled {
                let reset_procs = host_proc_monitor.processes_snapshot();
                if let Some(ev) = reset_detector.observe(&reset_procs, backend.devices()) {
                    if display_mode == DisplayMode::HivemindSweeper {
                        if let Some(h) = hivemind.as_mut() {
                            h.inject_reset(
                                format!(
                                    "tt-smi -r: {} chip(s) targeted{}",
                                    ev.chip_count,
                                    if ev.is_full { " (all)" } else { "" }
                                ),
                                &ev.device_indices,
                            );
                        }
                        reset_detector.clear(); // no takeover to wait for
                    } else {
                        takeover = Some(crate::animation::takeover::pick_takeover(ev));
                    }
                }
                if reset_detector.is_finished(&reset_procs) {
                    if let Some(t) = takeover.as_mut() {
                        t.note_reset_finished();
                    }
                }
            }
```

**(c) Tick + cleanup + render** — insert right before the `terminal.draw(|f| {` call at line 1230 (so `takeover` state is settled before this frame decides what to render):

```rust
        if let Some(t) = takeover.as_mut() {
            t.tick(draw_start.elapsed()); // see note below on `draw_start` ordering
            if t.is_done() {
                takeover = None;
                reset_detector.clear();
            }
        }
```

*(Note: `draw_start` is declared as `let draw_start = Instant::now();` immediately before the `terminal.draw(...)` call at the existing line 1228 — moving that declaration a few lines earlier, above this new block, so it can be reused here as "time since last frame" is a reasonable adjustment; alternatively introduce a small dedicated `last_takeover_tick: Instant` variable next to the other `last_*` timers near line 676–680 and use `last_takeover_tick.elapsed()` / update it here instead, if reusing `draw_start` for this purpose reads as confusing when reviewing the diff. Either is acceptable — pick whichever keeps the diff smaller once you're looking at the real surrounding code.)*

Then insert the render call as the last statement inside the draw closure, right after the existing overlay-panel block (after line 1346's closing `}` for `if let Some(kind) = overlay { ... }`, before line 1347's closing `})`):

```rust
                    // ── Reset takeover (full-screen, drawn over everything) ──
                    if let Some(t) = &takeover {
                        t.render(f, f.area());
                    }
```

**(d) Key-handling skip arm** — insert immediately before the existing `Event::Key(key) if key.kind == KeyEventKind::Press => {` arm at line 1376, as a new preceding match arm on the same `match event::read()...` expression (so it's tried first and the existing 850-line arm is never touched):

```rust
                Event::Key(key) if key.kind == KeyEventKind::Press && takeover.is_some() => {
                    if let Some(t) = takeover.as_mut() {
                        t.skip();
                    }
                }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib host_processes:: cli:: -- --nocapture`
Expected: PASS.

Then run the full suite to catch any wiring regressions:
Run: `cargo test --lib --features tui`
Expected: PASS, no regressions in the existing TUI test suite (in particular the render-smoke tests like `render_snake_view_with_band_does_not_panic` and the `HivemindSweeper`-related tests).

Also run: `cargo clippy --lib --features tui -- -D warnings` (the literal CI invocation — differs from `cargo test`, per this project's established gotcha) and fix anything it flags before proceeding.

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/config.rs src/workload/host_processes.rs src/ui/tui/mod.rs
git commit -m "feat: wire reset takeover detection into the TUI main loop"
```

---

### Task 13: Hardware verification pass (manual)

**Files:** none (verification only; may produce small follow-up fixes to Tasks 1–12's files if real behavior differs from the assumed `tt-smi --help` grammar).

**Interfaces:** N/A.

This project's established practice (AGENTS.md Phase 27/28/33 entries) is: don't ship an assumption about real hardware/CLI behavior without checking it against the real thing. This plan's parsing logic (Task 1) was written against `tt-smi --help`'s documented grammar, not observed real invocations.

- [ ] **Step 1: Lease a chip** per this project's hardware-safety convention (see the "Tenstorrent hardware: always lease first" guidance) before running any reset.

- [ ] **Step 2: Build and install the binary**

```bash
cargo build --release --features tui
cp target/release/tt-toplike-tui ~/.local/bin/
```

- [ ] **Step 3: Run the TUI with the feature enabled in one terminal**

```bash
tt-toplike-tui --tt-smi-reset-behavior dazzle
```

- [ ] **Step 4: In a second terminal, run a real single-chip reset**

```bash
gozer run --chips 1 --who "claude:reset-takeover-verify" --reason "verify reset-takeover detection" -- tt-smi -r 0
```

Observe: does a takeover appear promptly (within the ~2s detection cadence)? Does it correctly show a subset-scoped animation? Does it end shortly after the real `tt-smi -r` process exits, not before?

- [ ] **Step 5: Run a full-box reset** (all chips leased)

```bash
gozer run --chips <all available> --who "claude:reset-takeover-verify" --reason "verify full reset-takeover detection" -- tt-smi -r
```

Observe: does it correctly classify as full and weight toward a spectacle variant? Run it a handful of times to sample different variants.

- [ ] **Step 6: Verify the skip key** — press any key mid-animation during one of the runs above; confirm it dismisses immediately and does not also trigger a mode switch or quit.

- [ ] **Step 7: Verify the HivemindSweeper special case** — enter `~` (HivemindSweeper) before triggering a reset, run `tt-smi -r 0` again, and confirm no takeover appears but a `Reset`-kind event shows up in the feed pane with the affected chip visibly lit in the heat grid.

- [ ] **Step 8: Record findings and fix any real-vs-assumed grammar mismatch**

If real `tt-smi -r` output/behavior differs from what Task 1 assumed (e.g. a target syntax not covered, timing much faster/slower than the 2-second detection cadence tolerates well), fix the specific function in `src/workload/reset_detect.rs` with a new regression test capturing the real observed shape, per this project's TDD convention — do not adjust behavior without a test pinning it first.

- [ ] **Step 9: Commit any fixes**

```bash
git add -A
git commit -m "fix: correct reset-takeover detection against real tt-smi -r behavior"
```

If no fixes were needed, note that in place of a commit (nothing to commit is fine — do not create an empty commit).
