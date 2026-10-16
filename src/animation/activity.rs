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
    /// Stable identity of the thing running (`train:<pid>` or `svc:<key>`), so
    /// a consumer can follow one activity while the lanes reorder around it.
    pub id: String,
    pub kind: ActivityKind,
    pub label: String,
    pub stalled: bool,
    pub fields: Vec<String>,
    /// Increases by one per finished training step or completed request. A
    /// consumer treats a *decrease* as a reset (new run, restarted server).
    /// `None` when the count is unavailable right now (a scrape with no
    /// stats): that is not a zero, and a consumer keeps its last baseline.
    pub pulse_seq: Option<u64>,
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
        id: format!("train:{}", proc.pid),
        kind: ActivityKind::Train,
        label: proc.binary.clone(),
        stalled: false,
        fields,
        pulse_seq: Some(state.step_seq),
        key_hint: 't',
    })
}

/// One inference service as an activity. `elapsed_secs` is how long it has been
/// loading (the caller tracks it); `pulse_seq` its completed-request count.
pub fn from_service(
    row: &ServiceState,
    elapsed_secs: Option<f32>,
    pulse_seq: Option<u64>,
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
        id: format!("svc:{}", row.key),
        kind,
        label: row.label.clone(),
        stalled,
        fields,
        pulse_seq,
        key_hint: 'i',
    })
}

/// Cumulative count of finished requests for a service, from the scrape's
/// monotonic counters (vLLM: succeeded plus errored; a media server: completed
/// generations). It only ever rises, so reading the same snapshot again on the
/// next frame adds nothing. A drop means the server restarted. `None` when the
/// service has no stats this tick, which is not the same as a count of zero.
pub fn pulse_total(row: &ServiceState) -> Option<u64> {
    if let Some(sv) = &row.serving {
        Some(sv.counters.requests_succeeded_total + sv.counters.requests_errored_total)
    } else {
        row.media.as_ref().map(|m| m.counters.requests_total)
    }
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

/// Keeps the per-service state the pure builders need: when each service
/// started its current load, for the load clock. The clock runs only while a
/// service is compiling, loading or stalled. It is cleared when the service is
/// Ready or Down, so a server that served for an hour and then reloads starts
/// from zero. A service that disappears is forgotten too. The pulse count needs
/// no state: it is the scrape's cumulative counter (see [`pulse_total`]).
#[derive(Debug, Default)]
pub struct ActivityTracker {
    loading_since: HashMap<String, f32>,
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
        self.loading_since.retain(|k, _| live.contains(&k.as_str()));

        let mut out: Vec<Activity> = Vec::new();
        if let Some(a) = train.and_then(from_train) {
            out.push(a);
        }
        for row in rows {
            let loading = matches!(row.phase, Phase::Compiling | Phase::Loading | Phase::Alarm);
            let elapsed = if loading {
                let started = *self
                    .loading_since
                    .entry(row.key.clone())
                    .or_insert(now_secs);
                Some(now_secs - started)
            } else {
                self.loading_since.remove(&row.key);
                None
            };
            if let Some(a) = from_service(row, elapsed, pulse_total(row)) {
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
            start_time: None,
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
            from_service(&svc("m", Phase::Compiling), Some(7.0), Some(0))
                .unwrap()
                .kind,
            ActivityKind::Load
        );
        assert_eq!(
            from_service(&svc("m", Phase::Loading), Some(7.0), Some(0))
                .unwrap()
                .kind,
            ActivityKind::Load
        );
        assert_eq!(
            from_service(&svc("m", Phase::Ready), None, Some(0))
                .unwrap()
                .kind,
            ActivityKind::Serve
        );
        let stalled = from_service(&svc("m", Phase::Alarm), Some(300.0), Some(0)).unwrap();
        assert!(stalled.stalled);
        assert!(from_service(&svc("m", Phase::Down), None, Some(0)).is_none());
    }

    #[test]
    fn a_load_shows_placement_and_elapsed_but_no_invented_percent() {
        let a = from_service(&svc("rag-gate-27b", Phase::Loading), Some(7.0), Some(0)).unwrap();
        assert_eq!(a.label, "rag-gate-27b");
        assert_eq!(a.fields[0], ":8100 · chips 0,1");
        assert_eq!(a.fields[1], "loading 0:07");
        assert!(a.fields.iter().all(|f| !f.contains('%')), "{:?}", a.fields);
        // A real progress value is shown.
        let mut with = svc("m", Phase::Loading);
        with.progress = Some(0.63);
        let a = from_service(&with, Some(7.0), Some(0)).unwrap();
        assert!(a.fields.iter().any(|f| f.contains("63%")), "{:?}", a.fields);
    }

    #[test]
    fn urgency_order_is_stalled_then_load_then_train_then_serve() {
        let mk = |kind, stalled, label: &str| Activity {
            id: label.into(),
            kind,
            label: label.into(),
            stalled,
            fields: vec![],
            pulse_seq: Some(0),
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

    /// The monitor snapshot repeats for several seconds while the render loop
    /// reads it every frame, so a pulse count must come from the cumulative
    /// counters: the same snapshot seen again adds nothing.
    #[test]
    fn the_pulse_count_is_cumulative_and_a_repeated_snapshot_adds_nothing() {
        let mut serving = crate::workload::inference_server::metrics::ServingStats::zeroed();
        serving.completed_delta = 3; // a per-tick delta, which must NOT be summed
        serving.counters.requests_succeeded_total = 3;
        serving.counters.requests_errored_total = 1;
        let mut ready = svc("m", Phase::Ready);
        ready.serving = Some(serving);
        let mut t = ActivityTracker::new();
        let a = t.update(None, &[ready.clone()], 1.0);
        assert_eq!(a[0].pulse_seq, Some(4), "succeeded plus errored");
        // The same snapshot, seen on later frames.
        for frame in 2..60 {
            let a = t.update(None, &[ready.clone()], frame as f32 * 0.016);
            assert_eq!(a[0].pulse_seq, Some(4), "frame {frame}");
        }
        // A new scrape with more finished requests.
        let mut more = ready.clone();
        more.serving
            .as_mut()
            .unwrap()
            .counters
            .requests_succeeded_total = 9;
        let a = t.update(None, &[more], 2.0);
        assert_eq!(a[0].pulse_seq, Some(10));
    }

    #[test]
    fn a_media_servers_pulse_count_is_its_completed_generations() {
        let mut m = svc("flux", Phase::Ready);
        let mut media = crate::workload::inference_server::metrics::MediaStats {
            generations_per_min: 0.0,
            jobs_in_progress: 1,
            completed_delta: 0,
            errored_delta: 0,
            duration_avg_s: 0.0,
            post_avg_s: 0.0,
            pre_avg_s: 0.0,
            inference_avg_s: 0.0,
            warmup_avg_s: 0.0,
            counters: Default::default(),
        };
        media.counters.requests_total = 7;
        m.media = Some(media);
        let a = from_service(&m, None, pulse_total(&m)).unwrap();
        assert_eq!(a.pulse_seq, Some(7));
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

    /// A service that served for an hour and then reloads under the same key
    /// starts its load clock at zero, not at an hour. The clock runs only while
    /// it is compiling, loading or stalled.
    #[test]
    fn the_load_clock_runs_only_during_a_load_and_restarts_after_serving() {
        let mut t = ActivityTracker::new();
        let loading = svc("m", Phase::Loading);
        let ready = svc("m", Phase::Ready);
        let a = t.update(None, &[loading.clone()], 0.0);
        assert!(a[0].fields.iter().any(|f| f == "loading 0:00"));
        // An hour of serving: no load clock on a Ready row.
        let a = t.update(None, &[ready.clone()], 30.0);
        assert!(a[0].fields.iter().all(|f| !f.starts_with("loading")));
        let a = t.update(None, &[ready], 3600.0);
        assert!(a[0].fields.iter().all(|f| !f.starts_with("loading")));
        // The same key reloads: zero, not an hour.
        let a = t.update(None, &[loading.clone()], 3605.0);
        assert!(
            a[0].fields.iter().any(|f| f == "loading 0:00"),
            "{:?}",
            a[0].fields
        );
        // A Down row clears it too.
        let _ = t.update(None, &[svc("m", Phase::Down)], 3700.0);
        let a = t.update(None, &[loading], 3800.0);
        assert!(a[0].fields.iter().any(|f| f == "loading 0:00"));
    }

    #[test]
    fn a_service_with_no_stats_has_no_pulse_count_rather_than_zero() {
        let bare = svc("m", Phase::Ready);
        assert_eq!(pulse_total(&bare), None);
        assert_eq!(
            from_service(&bare, None, pulse_total(&bare))
                .unwrap()
                .pulse_seq,
            None
        );
    }

    #[test]
    fn activities_carry_a_stable_identity() {
        let st = running();
        assert_eq!(from_train(&st).unwrap().id, "train:7");
        assert_eq!(
            from_service(&svc("m", Phase::Ready), None, None)
                .unwrap()
                .id,
            "svc:m"
        );
    }
}
