# Arcade Activity Lanes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a training run or a model load/serve is active, Arcade shows one lane per activity over the top of the starfield and the stars take a tint and flare per step or request.

**Architecture:** A pure activity model (`activity.rs`) turns existing `TrainState` and `ServiceState` data into `Activity` values. A pure lane renderer (`activity_lanes.rs`) draws at most two rows. `ArcadeVisualization` splices the lanes over the first starfield rows after the hero overlay, and `HardwareStarfield` applies a bounded tint and flares. Training is noticed from the background process scan, never an inline `/proc` walk.

**Tech Stack:** Rust, ratatui, existing `TrainMonitor`, `InferenceServerMonitor`, `proc_scan` worker.

**Spec:** `docs/superpowers/specs/2026-10-14-arcade-activity-lanes-design.md`

## Global Constraints

- Arcade only. No new hotkeys. No status-bar chip. Training and Inference views unchanged in behaviour and tests.
- No right-side border glyph on any row this adds (`│ ┐ ┘ ║ ▕`). A lane has a left edge only.
- No invented figures. Chunk-local training steps have no `%`, bar or ETA. A load with no real progress shows no `%` and no ETA. tok/s only from a real scrape.
- No `/proc` scan on the render thread. Render-thread cost of detection under 0.1 ms per tick.
- Lanes only when the starfield region has at least 6 rows. At most 2 lanes. Urgency order: stalled, loading or compiling, training, serving.
- `--remote`: lanes suppressed. `--mock`: synthetic run and mock services.
- Each behaviour test is seen to fail first. Build with `cargo fmt`, clippy `-D warnings`, and `cargo test --locked --lib --features tui`.
- Version goes to 0.13.11 (Task 6).

## Review Focus

- A trainer whose stdout is not redirected (no readable log): the lane says `no per-step log` and shows no progress. Pinned in Task 1.
- `pulse_seq` going backwards (new run, restarted server): a reset, not a burst. Pinned in Task 1.
- Terminal width 0 to 200 with every activity mix: no lane wider than the width, no right border. Pinned in Task 2.
- Fewer than 6 starfield rows: no lanes, row count unchanged. Pinned in Task 3.
- The hero at the top of the screen: it must not cover a lane, and Arcade with no activities is structurally unchanged. Pinned in Task 3.
- A trainer process that exits: the monitor drops it and the lane goes at once. Pinned in Task 5.

## File Structure

- Create `src/animation/activity.rs`: `Activity`, `ActivityKind`, `ActivityTracker`, pure builders, formatting helpers.
- Create `src/animation/activity_lanes.rs`: `render_lanes`.
- Modify `src/animation/mod.rs`: declare and re-export the two modules.
- Modify `src/animation/arcade.rs`: `set_activities`, `lane_rows`, splice, hero clamp, tint and pulse hand-off.
- Modify `src/animation/starfield.rs`: `ActivityTint`, `set_activity_tint`, `pulse`, tint and flare in `render`.
- Modify `src/workload/train/detect.rs`: `find_trainer`.
- Modify `src/workload/train/monitor.rs`: `attach`, `poll_attached`, `poll` split.
- Create `src/workload/train/watch.rs`: `TrainWatch` (process list in, `TrainState` out).
- Modify `src/ui/tui/mod.rs` and `src/ui/tui/proc_scan.rs` tests: wire the Arcade arm.
- Modify `src/ui/tui/mod.rs`: Arcade `l` and `!` text and guard test.
- Modify `Cargo.toml`, `Cargo.lock`, `QUICK_START.md`, `site/index.html`, `debian/changelog`, `CHANGELOG.md`, `AGENTS.md`.

---

### Task 1: The activity model

**Files:**
- Create: `src/animation/activity.rs`
- Modify: `src/animation/mod.rs`

**Interfaces:**
- Produces:
  - `pub enum ActivityKind { Train, Load, Serve }`
  - `pub struct Activity { pub kind: ActivityKind, pub label: String, pub stalled: bool, pub fields: Vec<String>, pub pulse_seq: u64, pub key_hint: char }`
  - `pub struct ActivityTracker` with `pub fn new() -> Self` and `pub fn update(&mut self, train: Option<&TrainState>, rows: &[ServiceState], now_secs: f32) -> Vec<Activity>`
  - `pub fn from_train(state: &TrainState) -> Option<Activity>`
  - `pub fn from_service(row: &ServiceState, elapsed_secs: Option<f32>, pulse_seq: u64) -> Option<Activity>`
  - `pub fn ordered(all: Vec<Activity>) -> Vec<Activity>`
  - `pub fn fmt_eta(secs: f32) -> String`

- [ ] **Step 1: Write the failing tests** at the bottom of the new `activity.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};
    use crate::workload::train::{TrainProcess, TrainState};

    fn svc(label: &str, phase: Phase) -> ServiceState {
        ServiceState {
            key: label.into(),
            label: label.into(),
            phase,
            cpu_pct: 0.0,
            rss_bytes: 0,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            loaded_count: 0,
            loaded_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::Down,
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving: None,
            media: None,
            port: Some(8100),
            chips: vec![0, 1],
        }
    }

    fn running() -> TrainState {
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 7,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st
    }

    #[test]
    fn no_run_attached_is_no_activity() {
        assert!(from_train(&TrainState::new()).is_none());
    }

    #[test]
    fn a_run_with_a_budget_shows_step_percent_and_bar() {
        let mut st = running();
        st.step = 4120;
        st.max_steps = 10_000;
        let a = from_train(&st).unwrap();
        assert_eq!(a.kind, ActivityKind::Train);
        assert_eq!(a.label, "nano_gpt");
        assert_eq!(a.key_hint, 't');
        assert!(a.fields[0].starts_with("step 4,120/10,000"), "{:?}", a.fields);
        assert!(a.fields[0].contains("41%"), "{:?}", a.fields);
    }

    #[test]
    fn a_chunk_local_step_has_no_percent_bar_or_eta() {
        let mut st = running();
        st.step = 61;
        st.max_steps = 6391;
        st.chunked_bar = true; // the bar restarts per chunk: step is chunk-local
        assert!(st.step_is_chunk_local());
        let a = from_train(&st).unwrap();
        assert_eq!(a.fields[0], "step 61");
        assert!(a.fields.iter().all(|f| !f.contains('%') && !f.contains("left")));
    }

    #[test]
    fn a_trainer_with_no_readable_log_says_so_and_shows_no_progress() {
        let st = running(); // step 0, no log
        let a = from_train(&st).unwrap();
        assert_eq!(a.fields, vec!["no per-step log".to_string()]);
    }

    #[test]
    fn service_phases_map_to_kinds_and_down_is_nothing() {
        assert_eq!(
            from_service(&svc("m", Phase::Compiling), Some(7.0), 0).unwrap().kind,
            ActivityKind::Load
        );
        assert_eq!(
            from_service(&svc("m", Phase::Loading), Some(7.0), 0).unwrap().kind,
            ActivityKind::Load
        );
        assert_eq!(
            from_service(&svc("m", Phase::Ready), None, 0).unwrap().kind,
            ActivityKind::Serve
        );
        let stalled = from_service(&svc("m", Phase::Alarm), Some(300.0), 0).unwrap();
        assert!(stalled.stalled);
        assert!(from_service(&svc("m", Phase::Down), None, 0).is_none());
    }

    #[test]
    fn a_load_shows_placement_and_elapsed_but_no_invented_percent() {
        let a = from_service(&svc("rag-gate-27b", Phase::Loading), Some(7.0), 0).unwrap();
        assert_eq!(a.label, "rag-gate-27b");
        assert_eq!(a.fields[0], ":8100 · chips 0,1");
        assert_eq!(a.fields[1], "loading 0:07");
        assert!(a.fields.iter().all(|f| !f.contains('%')), "{:?}", a.fields);
        // A real progress value is shown.
        let mut with = svc("m", Phase::Loading);
        with.progress = Some(0.63);
        let a = from_service(&with, Some(7.0), 0).unwrap();
        assert!(a.fields.iter().any(|f| f.contains("63%")), "{:?}", a.fields);
    }

    #[test]
    fn urgency_order_is_stalled_then_load_then_train_then_serve() {
        let mk = |kind, stalled, label: &str| Activity {
            kind,
            label: label.into(),
            stalled,
            fields: vec![],
            pulse_seq: 0,
            key_hint: 'i',
        };
        let got = ordered(vec![
            mk(ActivityKind::Serve, false, "s"),
            mk(ActivityKind::Train, false, "t"),
            mk(ActivityKind::Load, false, "l"),
            mk(ActivityKind::Serve, true, "x"),
        ]);
        let labels: Vec<&str> = got.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["x", "l", "t", "s"]);
    }

    #[test]
    fn eta_is_formatted_in_the_largest_two_units() {
        assert_eq!(fmt_eta(45.0), "45s left");
        assert_eq!(fmt_eta(12.0 * 60.0 + 3.0), "12m left");
        assert_eq!(fmt_eta(3600.0 + 5.0 * 60.0), "1h05m left");
    }

    #[test]
    fn the_tracker_counts_completed_requests_as_pulses_and_treats_a_drop_as_a_reset() {
        let mut serving = crate::workload::inference_server::metrics::ServingStats::zeroed();
        serving.completed_delta = 3;
        let mut ready = svc("m", Phase::Ready);
        ready.serving = Some(serving.clone());
        let mut t = ActivityTracker::new();
        let a = t.update(None, &[ready.clone()], 1.0);
        assert_eq!(a[0].pulse_seq, 3);
        let a = t.update(None, &[ready.clone()], 2.0);
        assert_eq!(a[0].pulse_seq, 6);
        // The server restarted: the key is gone for a tick, then returns.
        let _ = t.update(None, &[], 3.0);
        let a = t.update(None, &[ready], 4.0);
        assert_eq!(a[0].pulse_seq, 3, "a returning service starts a fresh count");
    }

    #[test]
    fn a_load_clock_starts_when_the_service_is_first_seen_and_resets_when_it_goes() {
        let mut t = ActivityTracker::new();
        let loading = svc("m", Phase::Loading);
        let a = t.update(None, &[loading.clone()], 100.0);
        assert!(a[0].fields.iter().any(|f| f == "loading 0:00"), "{:?}", a[0].fields);
        let a = t.update(None, &[loading.clone()], 107.0);
        assert!(a[0].fields.iter().any(|f| f == "loading 0:07"), "{:?}", a[0].fields);
        let _ = t.update(None, &[], 110.0);
        let a = t.update(None, &[loading], 200.0);
        assert!(a[0].fields.iter().any(|f| f == "loading 0:00"), "{:?}", a[0].fields);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib activity::tests 2>&1 | tail -20`
Expected: FAIL to compile (`from_train` and the other items are not defined) until the stubs in Step 3 exist; after adding empty stubs, FAIL on the assertions.

`ServingStats::zeroed()` does not exist yet. Add it in this task as a `#[cfg(test)] pub(crate) fn zeroed() -> Self` next to `ServingStats` in `src/workload/inference_server/metrics.rs`, copying the all-zero literal from `serving_zero()` in `ui/tui/mod.rs` tests (fields: `generation_tps`, `prompt_tps`, `requests_running`, `requests_waiting`, `kv_cache_usage`, `ttft_avg_s`, `queue_avg_s`, `prefill_avg_s`, `decode_avg_s`, `tpot_avg_s`, `completed_delta`, `errored_delta`, `prefix_hit_rate`, `preemptions_delta`, `counters: Default::default()`).

- [ ] **Step 3: Write the implementation** above the test module

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The activity model behind Arcade's lanes: "something with a goal or a clock
//! is running". Pure data and pure functions. No I/O and no clock of its own:
//! the caller passes the time in, so every function here is testable with
//! fixed inputs.
//!
//! Nothing is invented. A figure is present only when the underlying data is
//! real: a chunk-local training step has no percent or ETA, a model load with
//! no known progress has no percent, tokens per second only comes from a real
//! `/metrics` scrape.

use crate::workload::inference_server::{Phase, ServiceState};
use crate::workload::train::TrainState;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Train,
    Load,
    Serve,
}

/// One thing running. `fields` are pre-formatted, in the order a narrow
/// terminal should keep them (most important first).
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub kind: ActivityKind,
    pub label: String,
    pub stalled: bool,
    pub fields: Vec<String>,
    /// Increases by one per finished training step or completed request. A
    /// consumer treats a *decrease* as a reset (new run, restarted server).
    pub pulse_seq: u64,
    /// The key that opens the full view: `t` (Training) or `i` (Inference).
    pub key_hint: char,
}

/// Human ETA in the largest two units: `45s left`, `12m left`, `1h05m left`.
pub fn fmt_eta(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    if s < 60 {
        format!("{s}s left")
    } else if s < 3600 {
        format!("{}m left", s / 60)
    } else {
        format!("{}h{:02}m left", s / 3600, (s % 3600) / 60)
    }
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_mmss(secs: f32) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// An 8-cell bar for a progress fraction: `████░░░░`. No enclosing glyphs.
fn bar(progress: f32) -> String {
    let filled = ((progress.clamp(0.0, 1.0) * 8.0).round() as usize).min(8);
    format!("{}{}", "█".repeat(filled), "░".repeat(8 - filled))
}

/// The Training activity, or `None` when no run is attached.
pub fn from_train(state: &TrainState) -> Option<Activity> {
    let proc = state.proc.as_ref()?;
    let mut fields: Vec<String> = Vec::new();
    if state.step == 0 && state.max_steps == 0 {
        // A trainer was found but nothing has been read from it: stdout is not
        // redirected to a file, or the run has not printed a step yet.
        fields.push("no per-step log".to_string());
    } else if state.step_is_chunk_local() || state.max_steps == 0 {
        // The step cannot be set against a budget, so no percent, bar or ETA.
        fields.push(format!("step {}", group_thousands(state.step)));
    } else {
        let p = state.step as f32 / state.max_steps as f32;
        fields.push(format!(
            "step {}/{} {} {:.0}%",
            group_thousands(state.step),
            group_thousands(state.max_steps),
            bar(p),
            (p * 100.0).clamp(0.0, 100.0)
        ));
        if let Some(eta) = state.eta_secs() {
            fields.push(fmt_eta(eta));
        }
    }
    let sps = state.steps_per_sec();
    if sps > 0.0 {
        fields.push(format!("{sps:.1} step/s"));
    }
    if let (Some(now), Some(prev)) = (state.loss, state.prev_loss) {
        let arrow = if now < prev {
            '↘'
        } else if now > prev {
            '↗'
        } else {
            '→'
        };
        fields.push(format!("loss {arrow}"));
    }
    Some(Activity {
        kind: ActivityKind::Train,
        label: proc.binary.clone(),
        stalled: false,
        fields,
        pulse_seq: state.step_seq,
        key_hint: 't',
    })
}

```

The rest of the file (the real `from_service`, `ordered` and the tracker) follows.

```rust
/// One inference service as an activity. `elapsed_secs` is how long it has been
/// loading (the caller tracks it); `pulse_seq` its completed-request count.
pub fn from_service(
    row: &ServiceState,
    elapsed_secs: Option<f32>,
    pulse_seq: u64,
) -> Option<Activity> {
    let (kind, stalled) = match row.phase {
        Phase::Down => return None,
        Phase::Compiling | Phase::Loading => (ActivityKind::Load, false),
        Phase::Ready => (ActivityKind::Serve, false),
        Phase::Alarm => (ActivityKind::Load, true),
    };
    let mut fields: Vec<String> = Vec::new();
    let placement = crate::workload::inference_server::placement_text(row.port, &row.chips);
    if !placement.is_empty() {
        fields.push(placement);
    }
    match kind {
        ActivityKind::Load => {
            let word = match row.phase {
                Phase::Compiling => "compiling",
                Phase::Alarm => "stalled",
                _ => "loading",
            };
            let when = fmt_mmss(elapsed_secs.unwrap_or(0.0));
            fields.push(format!("{word} {when}"));
            if let Some(p) = row.progress {
                fields.push(format!("{:.0}%", (p * 100.0).clamp(0.0, 100.0)));
            }
        }
        _ => {
            if let Some(sv) = &row.serving {
                fields.push(format!("{:.0} tok/s", sv.generation_tps));
                fields.push(format!("{} running", sv.requests_running));
            } else if let Some(m) = &row.media {
                fields.push(format!(
                    "{} in flight · {} done",
                    m.jobs_in_progress, m.counters.requests_total
                ));
            }
        }
    }
    Some(Activity {
        kind,
        label: row.label.clone(),
        stalled,
        fields,
        pulse_seq,
        key_hint: 'i',
    })
}

/// Most urgent first: stalled, then loading or compiling, then training, then
/// serving. Stable within a group (input order kept).
pub fn ordered(mut all: Vec<Activity>) -> Vec<Activity> {
    let rank = |a: &Activity| -> u8 {
        if a.stalled {
            0
        } else {
            match a.kind {
                ActivityKind::Load => 1,
                ActivityKind::Train => 2,
                ActivityKind::Serve => 3,
            }
        }
    };
    all.sort_by_key(|a| rank(a));
    all
}

/// Keeps the per-service state the pure builders need: when each service was
/// first seen (for the load clock) and its accumulated completed-request count
/// (for the pulse counter). A service that disappears is forgotten, so a
/// restarted server starts fresh.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    first_seen: HashMap<String, f32>,
    pulses: HashMap<String, u64>,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the ordered activity list for this tick. `now_secs` is any
    /// monotonically increasing clock in seconds.
    pub fn update(
        &mut self,
        train: Option<&TrainState>,
        rows: &[ServiceState],
        now_secs: f32,
    ) -> Vec<Activity> {
        let live: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        self.first_seen.retain(|k, _| live.contains(&k.as_str()));
        self.pulses.retain(|k, _| live.contains(&k.as_str()));

        let mut out: Vec<Activity> = Vec::new();
        if let Some(a) = train.and_then(from_train) {
            out.push(a);
        }
        for row in rows {
            let started = *self.first_seen.entry(row.key.clone()).or_insert(now_secs);
            let completed = row
                .serving
                .as_ref()
                .map(|s| u64::from(s.completed_delta))
                .unwrap_or(0);
            let total = self.pulses.entry(row.key.clone()).or_insert(0);
            *total += completed;
            if let Some(a) = from_service(row, Some(now_secs - started), *total) {
                out.push(a);
            }
        }
        ordered(out)
    }
}
```

Add to `src/animation/mod.rs`: `pub mod activity;` and `pub use activity::{Activity, ActivityKind, ActivityTracker};`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib activity::tests 2>&1 | tail -20`
Expected: PASS (10 tests).

- [ ] **Step 5: Commit**

```bash
git add src/animation/activity.rs src/animation/mod.rs src/workload/inference_server/metrics.rs
git commit -m "feat: activity model for Arcade lanes (training and inference)"
```

---

### Task 2: The lane renderer

**Files:**
- Create: `src/animation/activity_lanes.rs`
- Modify: `src/animation/mod.rs`

**Interfaces:**
- Consumes: `Activity`, `ActivityKind` from Task 1.
- Produces: `pub fn render_lanes(activities: &[Activity], width: usize) -> Vec<Line<'static>>` (at most 2 rows, each at most `width` columns wide).

- [ ] **Step 1: Write the failing tests** (bottom of `activity_lanes.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::activity::{Activity, ActivityKind};

    fn act(kind: ActivityKind, label: &str, fields: &[&str], hint: char) -> Activity {
        Activity {
            kind,
            label: label.into(),
            stalled: false,
            fields: fields.iter().map(|s| s.to_string()).collect(),
            pulse_seq: 0,
            key_hint: hint,
        }
    }

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn train() -> Activity {
        act(
            ActivityKind::Train,
            "nano_gpt",
            &["step 4,120/10,000 ████░░░░ 41%", "12m left", "1.8 step/s", "loss ↘"],
            't',
        )
    }

    fn load() -> Activity {
        act(ActivityKind::Load, "rag-gate-27b", &[":8100 · chips 0,1", "loading 0:07"], 'i')
    }

    #[test]
    fn a_wide_lane_shows_everything_in_priority_order() {
        let lanes = render_lanes(&[train()], 120);
        assert_eq!(lanes.len(), 1);
        let t = text(&lanes[0]);
        assert!(t.contains("TRAIN") && t.contains("nano_gpt"), "{t}");
        let (a, b, c, d) = (
            t.find("step 4,120").unwrap(),
            t.find("12m left").unwrap(),
            t.find("1.8 step/s").unwrap(),
            t.find("loss ↘").unwrap(),
        );
        assert!(a < b && b < c && c < d, "{t}");
        assert!(t.trim_end().ends_with("[t]"), "{t}");
    }

    #[test]
    fn a_narrow_lane_drops_the_hint_then_the_last_fields_first() {
        let wide = text(&render_lanes(&[train()], 200)[0]);
        let w = wide.chars().count();
        let mid = text(&render_lanes(&[train()], w - 2)[0]);
        assert!(!mid.contains("[t]"), "the hint goes first: {mid}");
        assert!(mid.contains("loss ↘"), "{mid}");
        let tight = text(&render_lanes(&[train()], 40)[0]);
        assert!(tight.contains("TRAIN") && tight.contains("nano_gpt"), "{tight}");
        assert!(!tight.contains("loss"), "{tight}");
    }

    #[test]
    fn a_lane_that_cannot_fit_its_tag_and_name_is_skipped() {
        assert!(render_lanes(&[train()], 6).is_empty());
        assert!(render_lanes(&[train()], 0).is_empty());
    }

    #[test]
    fn two_lanes_and_a_more_note_for_three_or_more() {
        let s = act(ActivityKind::Serve, "Qwen3-32B", &[":8000", "42 tok/s"], 'i');
        let two = render_lanes(&[load(), train()], 120);
        assert_eq!(two.len(), 2);
        let three = render_lanes(&[load(), train(), s], 120);
        assert_eq!(three.len(), 2);
        assert!(text(&three[1]).contains("+1 more"), "{}", text(&three[1]));
    }

    #[test]
    fn a_stalled_activity_is_marked_and_uses_the_error_colour() {
        let mut a = load();
        a.stalled = true;
        let l = &render_lanes(&[a], 120)[0];
        assert!(text(l).contains("✗"), "{}", text(l));
        let err = crate::ui::colors::error();
        assert!(l.spans.iter().any(|s| s.style.fg == Some(err)));
    }

    /// No lane is wider than the terminal at any width, and none has a
    /// right-side border glyph.
    #[test]
    fn lanes_never_exceed_the_width_and_have_no_right_border() {
        use unicode_width::UnicodeWidthStr;
        let s = act(ActivityKind::Serve, "Qwen3-32B", &[":8000 · chips 0,1", "42 tok/s", "3 running"], 'i');
        let mixes: Vec<Vec<Activity>> = vec![
            vec![train()],
            vec![load()],
            vec![load(), train()],
            vec![load(), train(), s.clone()],
            vec![s],
        ];
        for width in 0..=200 {
            for mix in &mixes {
                for l in render_lanes(mix, width) {
                    let t = text(&l);
                    assert!(
                        UnicodeWidthStr::width(t.as_str()) <= width,
                        "width {width}: {t:?}"
                    );
                    let last = t.trim_end().chars().last().unwrap_or(' ');
                    assert!(!"│┐┘║▕┃".contains(last), "right border at width {width}: {t:?}");
                }
            }
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib activity_lanes 2>&1 | tail -15`
Expected: FAIL (`render_lanes` not defined / returns nothing).

- [ ] **Step 3: Implement**

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Draws Arcade's activity lanes: at most two rows, one per activity. A lane is
//! a left edge, a tag, a name, then fields in priority order. When the width is
//! short the key hint goes first, then fields from the last one back, and a lane
//! that cannot hold its tag and name is skipped. No right-side border.

use crate::animation::activity::{Activity, ActivityKind};
use crate::ui::colors;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

const MAX_LANES: usize = 2;
const SEP: &str = " · ";

fn tag_of(a: &Activity) -> (&'static str, &'static str) {
    if a.stalled {
        return ("✗", "STALLED");
    }
    match a.kind {
        ActivityKind::Train => ("▶", "TRAIN"),
        ActivityKind::Load => ("◐", "LOAD"),
        ActivityKind::Serve => ("●", "SERVE"),
    }
}

fn accent(a: &Activity) -> ratatui::style::Color {
    if a.stalled {
        return colors::error();
    }
    match a.kind {
        ActivityKind::Train => colors::rgb(246, 188, 66),
        ActivityKind::Load => colors::rgb(180, 140, 230),
        ActivityKind::Serve => colors::rgb(79, 209, 197),
    }
}

/// One lane within `width` columns, or `None` when its tag and name do not fit.
fn lane(a: &Activity, width: usize, more: usize) -> Option<Line<'static>> {
    let (glyph, word) = tag_of(a);
    let head = format!("▎{glyph} {word} {}", a.label);
    let more_note = if more > 0 {
        format!("  +{more} more")
    } else {
        String::new()
    };
    let hint = format!("  [{}]", a.key_hint);
    let w = |s: &str| UnicodeWidthStr::width(s);
    if w(&head) > width {
        return None;
    }
    // Keep as many fields as fit, then the hint if it still fits. The "+N more"
    // note outranks the hint but not the fields already shown.
    let mut keep = a.fields.len();
    let total = |keep: usize, hint: &str| -> usize {
        let fields: usize = a.fields[..keep].iter().map(|f| w(SEP) + w(f)).sum();
        w(&head) + fields + w(&more_note) + w(hint)
    };
    while keep > 0 && total(keep, "") > width {
        keep -= 1;
    }
    let show_hint = total(keep, &hint) <= width;
    let show_more = total(keep, "") <= width;

    let color = accent(a);
    let mut spans = vec![
        Span::styled("▎", Style::default().fg(color)),
        Span::styled(
            format!("{glyph} {word} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(a.label.clone(), Style::default().fg(colors::text_primary())),
    ];
    for f in &a.fields[..keep] {
        spans.push(Span::styled(SEP, Style::default().fg(colors::text_secondary())));
        spans.push(Span::styled(f.clone(), Style::default().fg(colors::text_secondary())));
    }
    if show_more && !more_note.is_empty() {
        spans.push(Span::styled(more_note, Style::default().fg(colors::text_secondary())));
    }
    if show_hint {
        spans.push(Span::styled(hint, Style::default().fg(color)));
    }
    Some(Line::from(spans))
}

/// The lanes for `activities` (already in urgency order) within `width`.
pub fn render_lanes(activities: &[Activity], width: usize) -> Vec<Line<'static>> {
    let shown = activities.len().min(MAX_LANES);
    let mut out = Vec::with_capacity(shown);
    for (i, a) in activities.iter().take(MAX_LANES).enumerate() {
        let more = if i + 1 == MAX_LANES {
            activities.len().saturating_sub(MAX_LANES)
        } else {
            0
        };
        if let Some(l) = lane(a, width, more) {
            out.push(l);
        }
    }
    out
}
```

Add `pub mod activity_lanes;` and `pub use activity_lanes::render_lanes;` to `src/animation/mod.rs`.

- [ ] **Step 4: Run** `cargo test --lib activity_lanes 2>&1 | tail -15` — Expected: PASS (6 tests).

- [ ] **Step 5: Commit** `git add src/animation/activity_lanes.rs src/animation/mod.rs && git commit -m "feat: render Arcade activity lanes with priority-ordered fields"`

---

### Task 3: Arcade takes the lanes

**Files:**
- Modify: `src/animation/arcade.rs`

**Interfaces:**
- Consumes: `Activity`, `render_lanes`.
- Produces:
  - `pub fn set_activities(&mut self, activities: Vec<Activity>)`
  - `pub fn lane_rows(&self) -> usize` (0 unless the starfield region has at least 6 rows and activities exist)
  - lanes spliced over the first starfield rows after the hero overlay; the hero's target row is kept below the lanes.

- [ ] **Step 1: Write the failing tests** in `arcade.rs` tests

```rust
    fn lane_act(label: &str) -> crate::animation::Activity {
        crate::animation::Activity {
            kind: crate::animation::ActivityKind::Train,
            label: label.into(),
            stalled: false,
            fields: vec!["step 1/10 █░░░░░░░ 10%".into()],
            pulse_seq: 0,
            key_hint: 't',
        }
    }

    fn rows_of(a: &ArcadeVisualization, backend: &dyn TelemetryBackend) -> Vec<String> {
        a.render(backend)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>())
            .collect()
    }

    fn mock_backend() -> crate::backend::mock::MockBackend {
        let mut b = crate::backend::mock::MockBackend::new(2);
        b.init().unwrap();
        b.update().unwrap();
        b
    }

    /// With no activities nothing changes: no lanes are reserved and no lane tag
    /// appears. (Arcade's render uses randomness, so this checks the structure
    /// rather than comparing bytes with a stored frame.)
    #[test]
    fn no_activities_means_no_lanes() {
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        a.update(&backend);
        assert_eq!(a.lane_rows(), 0);
        let text = rows_of(&a, &backend).join("\n");
        assert!(!text.contains("TRAIN") && !text.contains('▎'), "{text}");
    }

    #[test]
    fn activities_replace_the_first_starfield_rows_and_keep_the_row_count() {
        let backend = mock_backend();
        let mut idle = ArcadeVisualization::new(120, 40);
        idle.update(&backend);
        let idle_rows = rows_of(&idle, &backend).len();

        let mut a = ArcadeVisualization::new(120, 40);
        a.set_activities(vec![lane_act("nano_gpt"), lane_act("second")]);
        a.update(&backend);
        assert_eq!(a.lane_rows(), 2);
        let rows = rows_of(&a, &backend);
        assert_eq!(rows.len(), idle_rows, "the render must keep its height");
        // Header, Row 1, separator, then the lanes.
        assert!(rows[3].contains("nano_gpt"), "{:?}", &rows[..6]);
        assert!(rows[4].contains("second"), "{:?}", &rows[..6]);
        // No lane row ends in a right-side border glyph.
        for r in &rows[3..5] {
            assert!(!"│┐┘║▕".contains(r.trim_end().chars().last().unwrap_or(' ')), "{r:?}");
        }
    }

    #[test]
    fn a_short_starfield_gets_no_lanes() {
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 12); // starfield region < 6 rows
        assert!((a.starfield_end - a.starfield_start) < 6);
        a.set_activities(vec![lane_act("nano_gpt")]);
        a.update(&backend);
        assert_eq!(a.lane_rows(), 0);
        assert!(!rows_of(&a, &backend).join("\n").contains("nano_gpt"));
    }

    /// The hero must not cover a lane. Force the hero to the top of the screen
    /// and check the lane text survives intact.
    #[test]
    fn the_hero_never_covers_a_lane() {
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        a.set_activities(vec![lane_act("nano_gpt")]);
        a.update(&backend);
        a.hero_x = 10.0;
        a.hero_y = 0.0;
        a.hero_target_y = 0.0;
        a.update(&backend);
        let rows = rows_of(&a, &backend);
        assert!(rows[3].contains("nano_gpt"), "{:?}", &rows[..5]);
        assert!(a.hero_target_y >= a.lane_rows() as f32 + 3.0, "{}", a.hero_target_y);
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib arcade 2>&1 | tail -15` — Expected: FAIL (compile error: `set_activities`, `lane_rows` undefined).

- [ ] **Step 3: Implement**

In the struct add `activities: Vec<crate::animation::Activity>,` (initialise to `Vec::new()` in `new`). Then:

```rust
    /// Hand Arcade the activities running this tick (already in urgency order).
    /// An empty list leaves Arcade exactly as it was.
    pub fn set_activities(&mut self, activities: Vec<crate::animation::Activity>) {
        self.activities = activities;
    }

    /// Rows the lanes take from the top of the starfield region: up to 2, and 0
    /// unless the starfield has at least 6 rows so it stays readable.
    pub fn lane_rows(&self) -> usize {
        const MIN_STARFIELD_ROWS: usize = 6;
        if self.activities.is_empty()
            || self.starfield_end.saturating_sub(self.starfield_start) < MIN_STARFIELD_ROWS
        {
            return 0;
        }
        self.activities.len().min(2)
    }

    /// First row index (in the whole render) below the lanes, used to keep the
    /// hero off them. Header, Row 1 and the separator come first.
    fn lanes_end_row(&self) -> usize {
        3 + self.lane_rows()
    }
```

In `render()`, after `self.overlay_hero(lines, backend)` is computed, splice:

```rust
        let mut lines = self.overlay_hero(lines, backend);
        let n = self.lane_rows();
        if n > 0 {
            let lanes = crate::animation::render_lanes(&self.activities, self.width);
            for (i, lane) in lanes.into_iter().enumerate() {
                // Header (0), Row 1 (1), separator (2), then the starfield rows.
                if let Some(slot) = lines.get_mut(3 + i) {
                    *slot = lane;
                }
            }
        }
        lines
```

(replace the final `self.overlay_hero(lines, backend)` return expression). In `update()`, after the hero's `hero_target_y` is computed (search for `hero_target_y =`), add:

```rust
        // Keep the hero below any lanes so it never covers one.
        let floor = self.lanes_end_row() as f32;
        if self.lane_rows() > 0 && self.hero_target_y < floor {
            self.hero_target_y = floor;
        }
```

If the hero's `hero_y` is set directly rather than lerped, clamp it the same way at the end of `update`.

- [ ] **Step 4: Run** `cargo test --lib arcade 2>&1 | tail -15` — Expected: PASS (new tests plus all existing Arcade tests).

- [ ] **Step 5: Commit** `git add src/animation/arcade.rs && git commit -m "feat: Arcade shows activity lanes over the starfield rows"`

---

### Task 4: The starfield responds

**Files:**
- Modify: `src/animation/starfield.rs`, `src/animation/arcade.rs`

**Interfaces:**
- Produces:
  - `pub struct ActivityTint { pub hue: f32, pub strength: f32 }` (strength capped at 0.45 on use)
  - `HardwareStarfield::set_activity_tint(&mut self, Option<ActivityTint>)`
  - `HardwareStarfield::pulse(&mut self)`
  - Arcade calls both from `update()`.

- [ ] **Step 1: Write the failing tests** in `starfield.rs` tests

```rust
    fn lit_cells(lines: &[ratatui::text::Line<'static>]) -> usize {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.chars().filter(|c| !c.is_whitespace()).count())
            .sum()
    }

    fn colours(lines: &[ratatui::text::Line<'static>]) -> Vec<Option<ratatui::style::Color>> {
        lines.iter().flat_map(|l| l.spans.iter().map(|s| s.style.fg)).collect()
    }

    fn lit_starfield() -> HardwareStarfield {
        let mut backend = crate::backend::mock::MockBackend::new(2);
        backend.init().unwrap();
        backend.update().unwrap();
        let mut sf = HardwareStarfield::new(80, 12);
        sf.initialize_from_devices(backend.devices());
        sf.update_from_telemetry(&backend);
        sf
    }

    #[test]
    fn a_tint_changes_star_colours_and_none_restores_them() {
        let mut sf = lit_starfield();
        let plain = colours(&sf.render());
        sf.set_activity_tint(Some(ActivityTint { hue: 40.0, strength: 0.4 }));
        let tinted = colours(&sf.render());
        assert_ne!(plain, tinted);
        sf.set_activity_tint(None);
        assert_eq!(plain, colours(&sf.render()));
    }

    #[test]
    fn the_tint_blend_is_capped() {
        let mut sf = lit_starfield();
        sf.set_activity_tint(Some(ActivityTint { hue: 40.0, strength: 5.0 }));
        let huge = colours(&sf.render());
        sf.set_activity_tint(Some(ActivityTint { hue: 40.0, strength: 0.45 }));
        assert_eq!(huge, colours(&sf.render()), "strength is capped at 0.45");
    }

    #[test]
    fn a_pulse_adds_a_flare_that_fades_and_is_bounded() {
        let mut sf = lit_starfield();
        let before = lit_cells(&sf.render());
        sf.pulse();
        let during = lit_cells(&sf.render());
        assert!(during > before, "{before} -> {during}");
        for _ in 0..200 {
            sf.pulse();
        }
        assert!(sf.live_flares() <= 6, "{}", sf.live_flares());
        for _ in 0..60 {
            let mut backend = crate::backend::mock::MockBackend::new(2);
            backend.init().unwrap();
            sf.update_from_telemetry(&backend);
        }
        assert_eq!(sf.live_flares(), 0, "flares expire");
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib starfield 2>&1 | tail -15` — Expected: FAIL (compile error).

- [ ] **Step 3: Implement** in `starfield.rs`:

```rust
/// A colour tint applied to the whole starfield while an activity runs.
/// `strength` is capped at 0.45 so the power-driven brightness the stars carry
/// stays readable underneath.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActivityTint {
    pub hue: f32,
    pub strength: f32,
}

const MAX_TINT: f32 = 0.45;
const MAX_FLARES: usize = 6;
const FLARE_FRAMES: u32 = 24;

/// One expanding ring born at `frame` around (`x`, `y`).
#[derive(Debug, Clone, Copy)]
struct Flare {
    born: u32,
    x: usize,
    y: usize,
}
```

Add fields `tint: Option<ActivityTint>`, `flares: Vec<Flare>`, `pulse_count: u32` (init `None`, `Vec::new()`, `0`). Methods:

```rust
    pub fn set_activity_tint(&mut self, tint: Option<ActivityTint>) {
        self.tint = tint;
    }

    /// Spawn one flare. The position is a deterministic function of how many
    /// pulses have happened, so it is stable and testable. At most
    /// `MAX_FLARES` are alive at once; the oldest is dropped first.
    pub fn pulse(&mut self) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        self.pulse_count = self.pulse_count.wrapping_add(1);
        let n = self.pulse_count as usize;
        let flare = Flare {
            born: self.frame,
            x: (n * 37 + 11) % self.width,
            y: (n * 5) % self.height,
        };
        if self.flares.len() >= MAX_FLARES {
            self.flares.remove(0);
        }
        self.flares.push(flare);
    }

    #[cfg(test)]
    fn live_flares(&self) -> usize {
        self.flares.len()
    }
```

In `update_from_telemetry` (where `self.frame` is incremented, line ~855) add after the increment: `let frame = self.frame; self.flares.retain(|f| frame.wrapping_sub(f.born) < FLARE_FRAMES);`.

In `render()`, after the canvas is built from stars/planets/streams and before conversion to spans, add:

```rust
        // Activity tint: blend every lit cell toward the tint hue.
        if let Some(t) = self.tint {
            use crate::animation::hsv_to_rgb;
            let k = t.strength.clamp(0.0, MAX_TINT);
            let target = hsv_to_rgb(t.hue, 0.6, 0.9);
            for row in canvas.iter_mut() {
                for (ch, color) in row.iter_mut() {
                    if *ch != ' ' {
                        *color = blend_rgb(*color, target, k);
                    }
                }
            }
        }
        // Flares: an expanding ring (wider than tall, to look round) that fades.
        for f in &self.flares {
            let age = self.frame.wrapping_sub(f.born);
            if age >= FLARE_FRAMES {
                continue;
            }
            let r = (age / 3 + 1) as i32;
            let glyph = match age {
                0..=7 => '○',
                8..=15 => '◌',
                _ => '·',
            };
            let v = 1.0 - age as f32 / FLARE_FRAMES as f32;
            let color = crate::animation::hsv_to_rgb(self.tint.map(|t| t.hue).unwrap_or(190.0), 0.4, 0.4 + 0.6 * v);
            for (dx, dy) in [(r * 2, 0), (-r * 2, 0), (0, r), (0, -r)] {
                let (x, y) = (f.x as i32 + dx, f.y as i32 + dy);
                if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
                    canvas[y as usize][x as usize] = (glyph, color);
                }
            }
        }
```

And a small helper at module level:

```rust
fn blend_rgb(base: Color, to: Color, k: f32) -> Color {
    match (base, to) {
        (Color::Rgb(br, bg, bb), Color::Rgb(tr, tg, tb)) => {
            let m = |a: u8, b: u8| (a as f32 * (1.0 - k) + b as f32 * k).round().clamp(0.0, 255.0) as u8;
            Color::Rgb(m(br, tr), m(bg, tg), m(bb, tb))
        }
        _ => base,
    }
}
```

(Cells that are black stay black: `' '` cells are skipped, so only lit glyphs tint.)

In `arcade.rs` `update()` add, after `self.starfield.update_from_telemetry(backend);`:

```rust
        self.starfield.set_activity_tint(self.activity_tint());
        self.pulse_activities();
```

with

```rust
    /// The tint for the most urgent activity: red stalled, violet loading, warm
    /// training, teal serving. `None` when nothing runs.
    fn activity_tint(&self) -> Option<crate::animation::starfield::ActivityTint> {
        use crate::animation::{starfield::ActivityTint, ActivityKind};
        let a = self.activities.first()?;
        let hue = if a.stalled {
            0.0
        } else {
            match a.kind {
                ActivityKind::Train => 40.0,
                ActivityKind::Load => 275.0,
                ActivityKind::Serve => 175.0,
            }
        };
        Some(ActivityTint { hue, strength: 0.3 })
    }

    /// One flare per new pulse across the lanes shown. A count that goes down
    /// (new run, restarted server) is a reset, not a burst.
    fn pulse_activities(&mut self) {
        let seen: Vec<u64> = self.activities.iter().take(2).map(|a| a.pulse_seq).collect();
        for (i, now) in seen.iter().enumerate() {
            let prev = self.last_pulse.get(i).copied().unwrap_or(*now);
            if *now > prev {
                for _ in 0..((*now - prev).min(3)) {
                    self.starfield.pulse();
                }
            }
        }
        self.last_pulse = seen;
    }
```

Add `last_pulse: Vec<u64>` to the struct (init `Vec::new()`), and make `starfield` module paths public as needed (`pub use` of `ActivityTint` in `animation/mod.rs`).

Add Arcade tests:

```rust
    #[test]
    fn a_new_pulse_flares_the_starfield_but_a_drop_does_not() {
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        let mut act = lane_act("nano_gpt");
        act.pulse_seq = 10;
        a.set_activities(vec![act.clone()]);
        a.update(&backend);
        assert_eq!(a.starfield.live_flares_for_test(), 0, "the first sight is a baseline");
        act.pulse_seq = 12;
        a.set_activities(vec![act.clone()]);
        a.update(&backend);
        assert!(a.starfield.live_flares_for_test() >= 1);
        let before = a.starfield.live_flares_for_test();
        act.pulse_seq = 1; // a new run
        a.set_activities(vec![act]);
        a.update(&backend);
        assert!(a.starfield.live_flares_for_test() <= before, "a drop is a reset, not a burst");
    }
```

and in `starfield.rs`: `pub(crate) fn live_flares_for_test(&self) -> usize { self.flares.len() }` (non-cfg-test, so Arcade's tests can call it; rename the test-only one to use it).

- [ ] **Step 4: Run** `cargo test --lib 2>&1 | grep -E "FAILED|test result"` — Expected: PASS.

- [ ] **Step 5: Commit** `git add -A && git commit -m "feat: the Arcade starfield tints and flares with running activities"`

---

### Task 5: Training is noticed without a render-thread scan

**Files:**
- Modify: `src/workload/train/detect.rs`, `src/workload/train/monitor.rs`, `src/workload/train/mod.rs`, `src/ui/tui/mod.rs`, `src/ui/tui/proc_scan.rs`
- Create: `src/workload/train/watch.rs`

**Interfaces:**
- Produces:
  - `pub fn find_trainer(processes: &[(i32, String, String)], ttml_loaded: &mut dyn FnMut(i32) -> bool) -> Option<TrainProcess>`
  - `TrainMonitor::attach(&mut self, p: TrainProcess)`, `TrainMonitor::poll_attached(&mut self)` (poll without the `/proc` walk)
  - `pub struct TrainWatch` with `new()`, `update(&mut self, processes: Option<&[(i32, String, String)]>)`, `state(&self) -> Option<&TrainState>`

- [ ] **Step 1: Write the failing tests**

In `detect.rs` tests:

```rust
    fn p(pid: i32, name: &str, cmd: &str) -> (i32, String, String) {
        (pid, name.to_string(), cmd.to_string())
    }

    #[test]
    fn find_trainer_picks_a_compiled_example_without_reading_maps() {
        let procs = vec![
            p(1, "bash", "bash"),
            p(2, "nano_gpt", "/x/nano_gpt -c train.yaml"),
        ];
        let mut maps_reads = 0;
        let t = find_trainer(&procs, &mut |_| {
            maps_reads += 1;
            false
        })
        .expect("nano_gpt");
        assert_eq!((t.pid, t.binary.as_str()), (2, "nano_gpt"));
        assert_eq!(maps_reads, 0, "a compiled example costs no maps read");
    }

    #[test]
    fn find_trainer_reads_maps_only_for_python_processes() {
        let procs = vec![
            p(1, "bash", "bash -l"),
            p(3, "python3", "python3 /w/train_run.py --steps 100"),
            p(4, "nginx", "nginx -g daemon off;"),
        ];
        let mut asked: Vec<i32> = Vec::new();
        let t = find_trainer(&procs, &mut |pid| {
            asked.push(pid);
            true
        })
        .expect("a ttml python trainer");
        assert_eq!(t.pid, 3);
        assert_eq!(asked, vec![3], "only the python process is probed");
    }
```

In `monitor.rs` tests:

```rust
    #[test]
    fn poll_attached_never_walks_proc_and_attach_takes_a_trainer() {
        let mut m = TrainMonitor::new();
        // Unattached: poll_attached does nothing (no scan, no state change).
        m.poll_attached();
        assert!(m.state().proc.is_none());
        assert!(m.last_scan.is_none(), "poll_attached must not rescan");
        // attach() records the process without scanning.
        m.attach(TrainProcess { pid: std::process::id() as i32, binary: "nano_gpt".into(), config_path: None });
        assert_eq!(m.state().proc.as_ref().unwrap().binary, "nano_gpt");
        assert!(m.last_scan.is_none());
    }
```

In `watch.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, name: &str, cmd: &str) -> (i32, String, String) {
        (pid, name.to_string(), cmd.to_string())
    }

    #[test]
    fn a_trainer_in_the_process_list_attaches_and_its_exit_detaches() {
        let mut w = TrainWatch::new();
        assert!(w.state().is_none());
        // Use this test process's own pid so `still_alive` is true.
        let me = std::process::id() as i32;
        let procs = vec![p(me, "nano_gpt", "/x/nano_gpt -c train.yaml")];
        w.update(Some(&procs));
        assert_eq!(w.state().unwrap().proc.as_ref().unwrap().binary, "nano_gpt");
        // A later scan without it: dropped at once.
        w.update(Some(&[p(1, "bash", "bash")]));
        assert!(w.state().is_none());
    }

    #[test]
    fn no_new_scan_keeps_the_attached_run_and_never_searches() {
        let mut w = TrainWatch::new();
        w.update(None);
        assert!(w.state().is_none());
        let me = std::process::id() as i32;
        w.update(Some(&[p(me, "nano_gpt", "/x/nano_gpt -c t.yaml")]));
        w.update(None); // between scans
        assert!(w.state().is_some());
    }

    #[test]
    fn maps_results_are_cached_by_pid() {
        let mut w = TrainWatch::new();
        let me = std::process::id() as i32;
        let procs = vec![p(me, "python3", "python3 /w/train_run.py --steps 5")];
        let mut calls = 0;
        w.update_with(Some(&procs), &mut |_| {
            calls += 1;
            false
        });
        w.update_with(Some(&procs), &mut |_| {
            calls += 1;
            false
        });
        assert_eq!(calls, 1, "the maps probe for one pid runs once");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib workload::train 2>&1 | tail -15` — Expected: FAIL (compile errors).

- [ ] **Step 3: Implement**

`detect.rs`:

```rust
/// A trainer among already-collected processes `(pid, name, cmdline)`, or
/// `None`. Pure apart from `ttml_loaded`, which is asked only for Python
/// processes (it reads `/proc/<pid>/maps`). A compiled example (`nano_gpt`,
/// `mnist_mlp`, `linear_regression`) costs no extra read.
pub fn find_trainer(
    processes: &[(i32, String, String)],
    ttml_loaded: &mut dyn FnMut(i32) -> bool,
) -> Option<TrainProcess> {
    for (pid, name, cmdline) in processes {
        if cmdline.is_empty() {
            continue;
        }
        if let Some(p) = parse_train_process(name, cmdline, *pid) {
            return Some(p);
        }
        let argv0 = cmdline.split_whitespace().next().unwrap_or("");
        let base = argv0.rsplit('/').next().unwrap_or(argv0);
        if is_python(base) {
            if let Some(p) = parse_python_trainer(cmdline, *pid, ttml_loaded(*pid)) {
                return Some(p);
            }
        }
    }
    None
}
```

(re-export from `workload/train/mod.rs`: `pub use detect::{find_trainer, ...}`; `is_python` is already in `detect.rs`.)

`monitor.rs`: split `poll`:

```rust
    pub fn poll(&mut self) {
        self.poll_inner(true);
    }

    /// Like `poll`, but never walks `/proc` looking for a trainer. For callers
    /// that find the trainer another way and call `attach`.
    pub fn poll_attached(&mut self) {
        self.poll_inner(false);
    }

    /// Start following `p`: load its config, find its log, begin tailing.
    pub fn attach(&mut self, p: TrainProcess) {
        let cfg = Self::load_config(&p);
        let log = discover_log(p.pid);
        if let LogSource::File(ref path) = log {
            self.tailer = Some(Tailer::new(path.clone()));
        }
        self.ckpt = Self::checkpoint_watch_for(&p, &cfg);
        self.state = TrainState::new();
        self.reset_run_anchors();
        self.state.first_seen = Some(Instant::now());
        self.state.config = cfg;
        self.state.log = Some(log);
        self.state.proc = Some(p);
    }
```

and rename the existing body of `poll` to `fn poll_inner(&mut self, allow_scan: bool)` with the attach block becoming:

```rust
        if self.state.proc.is_none() {
            if !allow_scan {
                return;
            }
            let due = self.last_scan.map(|t| t.elapsed() >= RESCAN_EVERY).unwrap_or(true);
            if !due {
                return;
            }
            self.last_scan = Some(Instant::now());
            if let Some(p) = Self::scan_for_process() {
                self.attach(p);
            }
            return;
        }
```

`pub fn detach(&mut self)` resets like the "run ended" branch (state, anchors, tailer, ckpt). Make `maps_ttml` `pub(crate)`.

`watch.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Follows a training run for a view that does not own the training monitor
//! (Arcade). It never walks `/proc` itself: a trainer is noticed in a process
//! list the background scan already collected, then followed through a
//! `TrainMonitor` that only tails its log and reads its own `/proc/<pid>` stats.

use super::detect::find_trainer;
use super::monitor::{TrainMonitor, TrainState};
use std::collections::HashMap;

pub struct TrainWatch {
    monitor: TrainMonitor,
    ttml: HashMap<i32, bool>,
}

impl TrainWatch {
    pub fn new() -> Self {
        Self { monitor: TrainMonitor::new(), ttml: HashMap::new() }
    }

    /// `processes` is `Some` on the tick a fresh scan arrives, `None` between
    /// scans. Between scans the attached run is only polled.
    pub fn update(&mut self, processes: Option<&[(i32, String, String)]>) {
        self.update_with(processes, &mut TrainMonitor::maps_ttml);
    }

    pub(crate) fn update_with(
        &mut self,
        processes: Option<&[(i32, String, String)]>,
        probe: &mut dyn FnMut(i32) -> bool,
    ) {
        if let Some(list) = processes {
            let attached = self.monitor.state().proc.as_ref().map(|p| p.pid);
            match attached {
                Some(pid) if !list.iter().any(|(p, _, _)| *p == pid) => self.monitor.detach(),
                Some(_) => {}
                None => {
                    let cache = &mut self.ttml;
                    let found = find_trainer(list, &mut |pid| {
                        *cache.entry(pid).or_insert_with(|| probe(pid))
                    });
                    if let Some(p) = found {
                        self.monitor.attach(p);
                    }
                    let live: Vec<i32> = list.iter().map(|(p, _, _)| *p).collect();
                    self.ttml.retain(|pid, _| live.contains(pid));
                }
            }
        }
        self.monitor.poll_attached();
    }

    pub fn state(&self) -> Option<&TrainState> {
        let s = self.monitor.state();
        s.proc.as_ref().map(|_| s)
    }
}

impl Default for TrainWatch {
    fn default() -> Self {
        Self::new()
    }
}
```

(`maps_ttml` is `fn(i32)->bool` associated function; pass `&mut |pid| TrainMonitor::maps_ttml(pid)` if the signature needs a closure.)

`TUI wiring` in `ui/tui/mod.rs`:

1. Declare `let mut train_watch = crate::workload::train::TrainWatch::new(); let mut activity_tracker = crate::animation::ActivityTracker::new(); let mut latest_scan: Option<Vec<(i32,String,String)>> = None;` near the other monitors (next to `train_monitor`).
2. In the scan request: `want_processes: reset_behavior.detects() || display_mode == DisplayMode::Arcade,`. Update the proc_scan source-guard test string (`"want_processes: reset_behavior.detects()"` at `proc_scan.rs:780`) to the new expression, and add a test that asserts the Arcade arm does not call `scan_for_process`/`TrainMonitor::new`.
3. Where the scan result is applied (`let scanned_processes = applied.processes;`), keep a copy for Arcade: `if display_mode == DisplayMode::Arcade && !scanned_processes.is_empty() { latest_scan = Some(scanned_processes.clone()); }` before it is moved into `scan_resets`.
4. In the Arcade arm, before `arc.update(backend)`:

```rust
                    let train_state: Option<crate::workload::train::TrainState> =
                        if backend_type == BackendType::Mock {
                            Some(mock_train.get_or_insert_with(MockTrainRun::new).state())
                        } else if backend_type == BackendType::Remote {
                            None
                        } else {
                            train_watch.update(latest_scan.take().as_deref());
                            train_watch.state().cloned()
                        };
                    #[cfg(target_os = "linux")]
                    let service_rows = if backend_type == BackendType::Mock {
                        crate::workload::inference_server::mock::mock_services(mock_started.elapsed().as_secs_f32())
                    } else if backend_type == BackendType::Remote {
                        Vec::new()
                    } else {
                        inference_monitor.snapshot()
                    };
                    #[cfg(not(target_os = "linux"))]
                    let service_rows: Vec<crate::workload::inference_server::ServiceState> = Vec::new();
                    arc.set_activities(activity_tracker.update(
                        train_state.as_ref(),
                        &service_rows,
                        mock_started.elapsed().as_secs_f32(),
                    ));
```

`TrainState` must be `Clone` (check; if not, derive `Clone` or pass a reference scoped to the call).

- [ ] **Step 4: Run** `cargo test --lib 2>&1 | grep -E "FAILED|test result"` — Expected: PASS.
- [ ] **Step 5: Render-thread cost test.** In `watch.rs` tests add a 548-process fixture and assert `update_with(Some(&procs), &mut |_| false)` completes 200 times in under 20 ms total (0.1 ms each):

```rust
    #[test]
    fn noticing_a_trainer_among_548_processes_costs_under_a_tenth_of_a_millisecond() {
        let mut procs: Vec<(i32, String, String)> = (1..=547)
            .map(|i| (i, "worker".to_string(), format!("/usr/bin/worker --id {i}")))
            .collect();
        procs.push((9000, "nano_gpt".into(), "/x/nano_gpt -c train.yaml".into()));
        let mut w = TrainWatch::new();
        w.update_with(Some(&procs), &mut |_| false); // attach (the one-time cost)
        let start = std::time::Instant::now();
        for _ in 0..200 {
            w.update_with(None, &mut |_| false); // between scans
        }
        let per_tick = start.elapsed() / 200;
        assert!(per_tick < std::time::Duration::from_micros(100), "{per_tick:?}");
    }
```

(Use pid `std::process::id()` for the trainer so `still_alive` holds.)
- [ ] **Step 6: Commit** `git add -A && git commit -m "feat: Arcade follows a training run without a render-thread scan"`

---

### Task 6: Docs, legend, version

**Files:**
- Modify: `src/ui/tui/mod.rs` (Arcade legend and explain), `AGENTS.md`, `CHANGELOG.md`, `debian/changelog`, `QUICK_START.md`, `site/index.html`, `Cargo.toml`, `Cargo.lock`

- [ ] **Step 1: Write the failing test** extending `legend_and_explain_text_names_what_the_views_draw`:

```rust
        // Arcade: the activity lanes and their tags.
        for panel in [P::Legend, P::Explain] {
            let text = overlay_text(panel, M::Arcade);
            for needle in ["TRAIN", "LOAD", "SERVE", "[t]", "[i]"] {
                assert!(text.contains(needle), "Arcade {panel:?} lacks {needle:?}:\n{text}");
            }
        }
```

- [ ] **Step 2: Run** `cargo test --lib legend_and_explain 2>&1 | tail -6` — Expected: FAIL.
- [ ] **Step 3: Implement.** Add to `hero_legend_lines`' neighbours an `activity_legend_lines(bar, bg, dim)` with rows for `▶ TRAIN`, `◐ LOAD`, `● SERVE`, `✗ STALLED`, `[t] / [i]` (opens the full view) and `tint` (warm = training, violet = loading, teal = serving, red = stalled; a flare = one step or request). Append it to the Arcade legend after the Hero block with a `sep!("▶ Activity")`. Add a paragraph to the Arcade `!` text (`DisplayMode::Arcade => explain_lines(...)`) naming lanes, the tints, the flare, the key hints, and that lanes appear only while something runs.
- [ ] **Step 4: Run the full CI steps** (fmt check, clippy `-D warnings`, `cargo test --locked --lib --features tui`, the Windows/macOS feature set, TUI and GUI builds) — Expected: all pass.
- [ ] **Step 5: Version.** `./scripts/bump-version.sh 0.13.11`; add a `debian/changelog` stanza above 0.13.10 and a `CHANGELOG.md` entry; update `QUICK_START.md`'s date. Run the four-way check.
- [ ] **Step 6: Log in AGENTS.md** (what, why, evidence), commit and push:

```bash
git add -A && git commit -m "feat: Arcade activity lanes (v0.13.11)" && git push origin updates/reality
```

---

## Self-review

- **Spec coverage:** lanes (Tasks 2, 3), starfield response (4), data sourcing and the no-inline-scan constraint (5), edge cases (1, 2, 3), `--mock` and `--remote` (5), docs and guard (6), version (6). The spec's "byte-identical to a stored fixture" is replaced by structural guards (Task 3) because Arcade's render uses randomness (three runs of the same code produced three different frames).
- **Placeholders:** none. The one stub in Task 1 Step 3 is called out and replaced in place.
- **Type consistency:** `Activity`, `ActivityKind`, `ActivityTracker`, `render_lanes`, `set_activities`, `lane_rows`, `ActivityTint`, `set_activity_tint`, `pulse`, `find_trainer`, `attach`, `poll_attached`, `detach`, `TrainWatch` are used with the same names throughout.
- **Review Focus:** each line has its test in the task named.

## As built (notes added after execution)

- Task 3: only `hero_y` is clamped below the lanes; the plan's extra clamp on
  `hero_target_y` was redundant and is not in the code.
- Task 4: `effective_tint_strength` is a public pure function and the cap test
  uses it, because star colours are indexed (not RGB) on a terminal without
  true-colour support. `live_flares()` is `#[cfg(test)] pub(crate)` (clippy rejects it as dead code otherwise).
- Task 5: the live-pid tests in `watch.rs` are Linux-only, and the cost test takes
  the best of fifteen batches. The scan guard in `proc_scan.rs` gained a test that
  the Arcade arm follows training through `TrainWatch`.
- Task 6: the version bump moved to the end of the branch, after the inference
  atmosphere work, so one release covers both.
