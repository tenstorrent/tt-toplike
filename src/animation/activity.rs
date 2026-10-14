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
        assert!(
            a.fields[0].starts_with("step 4,120/10,000"),
            "{:?}",
            a.fields
        );
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
        assert!(a
            .fields
            .iter()
            .all(|f| !f.contains('%') && !f.contains("left")));
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
            from_service(&svc("m", Phase::Compiling), Some(7.0), 0)
                .unwrap()
                .kind,
            ActivityKind::Load
        );
        assert_eq!(
            from_service(&svc("m", Phase::Loading), Some(7.0), 0)
                .unwrap()
                .kind,
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
        assert_eq!(
            a[0].pulse_seq, 3,
            "a returning service starts a fresh count"
        );
    }

    #[test]
    fn a_load_clock_starts_when_the_service_is_first_seen_and_resets_when_it_goes() {
        let mut t = ActivityTracker::new();
        let loading = svc("m", Phase::Loading);
        let a = t.update(None, &[loading.clone()], 100.0);
        assert!(
            a[0].fields.iter().any(|f| f == "loading 0:00"),
            "{:?}",
            a[0].fields
        );
        let a = t.update(None, &[loading.clone()], 107.0);
        assert!(
            a[0].fields.iter().any(|f| f == "loading 0:07"),
            "{:?}",
            a[0].fields
        );
        let _ = t.update(None, &[], 110.0);
        let a = t.update(None, &[loading], 200.0);
        assert!(
            a[0].fields.iter().any(|f| f == "loading 0:00"),
            "{:?}",
            a[0].fields
        );
    }
}
