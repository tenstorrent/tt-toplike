//! `inspect`: print a summary of a recorded log.
//!
//! The summary lists, in order: the header, the record count by payload
//! kind, the time span, the entities seen, the events, and for every signal
//! its sample count with minimum, maximum and last value.
//!
//! A [`Filter`] narrows the entity, event and signal sections to one entity
//! and/or one signal name (exact match on the key or name as written in the
//! log). The totals at the top always cover the whole log. A signal filter
//! leaves out the event section, because an event is not a signal. An entity
//! filter keeps the events that name that entity.
//!
//! Holes (see [`crate::replay::Hole`]) are listed under `holes:`. Records
//! lost from the end of a log (a deleted last segment) cannot be seen from
//! sequence jumps, so only an unreadable last segment is reported.
//!
//! The reader is [`Replay`], so the input can be a plain segment, a
//! compressed segment or a directory, and damaged lines are skipped and
//! counted in the output.

use crate::entity::EntityKey;
use crate::record::{Event, Payload, Record, Value};
use crate::replay::Replay;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

/// How many events the event section lists before it says "and N more".
const EVENT_LIST_MAX: usize = 50;

/// Narrow the entity, event and signal sections. `None` keeps everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Keep only this entity, for example `model:llama-8b`.
    pub entity: Option<String>,
    /// Keep only this signal, for example `chip.temp_c`.
    pub signal: Option<String>,
}

impl Filter {
    fn entity_ok(&self, e: &EntityKey) -> bool {
        self.entity.as_deref().is_none_or(|k| k == e.as_str())
    }

    fn signal_ok(&self, s: &str) -> bool {
        self.signal.as_deref().is_none_or(|k| k == s)
    }
}

/// Running numbers for one signal of one entity.
#[derive(Default)]
struct Stat {
    count: u64,
    min: Option<f64>,
    max: Option<f64>,
    last: Option<Value>,
}

impl Stat {
    fn add(&mut self, v: &Value) {
        self.count += 1;
        let x = match v {
            Value::F64(x) => Some(*x),
            Value::U64(x) => Some(*x as f64),
            Value::I64(x) => Some(*x as f64),
            Value::Text(_) => None,
        };
        if let Some(x) = x {
            self.min = Some(self.min.map_or(x, |m| m.min(x)));
            self.max = Some(self.max.map_or(x, |m| m.max(x)));
        }
        self.last = Some(v.clone());
    }
}

/// The entities an event names.
pub(crate) fn event_entities(e: &Event) -> Vec<&EntityKey> {
    match e {
        Event::ModelLoading { model, chips, .. } | Event::ModelReady { model, chips } => {
            std::iter::once(model).chain(chips).collect()
        }
        Event::ModelGone { model } | Event::RequestsCompleted { model, .. } => vec![model],
        Event::KernelCompiled { chip, .. } | Event::Spark { chip, .. } => chip.iter().collect(),
        Event::Reset { chips } => chips.iter().collect(),
        Event::LinkChanged { chip, .. } | Event::EccTick { chip, .. } => vec![chip],
        Event::LeaseGranted { lease, chips } | Event::LeaseReleased { lease, chips } => {
            std::iter::once(lease).chain(chips).collect()
        }
        Event::ViewChanged { .. } => Vec::new(),
    }
}

/// The event's type name as written in the log (`Reset`, `ModelReady`, ...).
fn event_name(e: &Event) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| v.get("t").and_then(|t| t.as_str().map(str::to_string)))
        .unwrap_or_else(|| "Event".to_string())
}

/// A number with up to three decimals and no trailing zeros.
fn num(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn show(v: &Value) -> String {
    match v {
        Value::F64(x) => num(*x),
        Value::U64(x) => x.to_string(),
        Value::I64(x) => x.to_string(),
        Value::Text(t) => format!("{t:?}"),
    }
}

fn secs(ns: u64) -> String {
    format!("{:.3}s", ns as f64 / 1e9)
}

/// Write the summary of the log at `path` to `out`.
///
/// Errors only when the path cannot be opened or `out` fails.
pub fn inspect(path: &Path, filter: &Filter, out: &mut impl Write) -> io::Result<()> {
    let mut replay = Replay::open(path)?;
    let header = replay.header().cloned();

    let mut total = 0u64;
    let (mut samples, mut sweeps, mut events_n, mut raws, mut gaps) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut span: Option<(u64, u64)> = None;
    let mut wall: Option<(u64, u64)> = None;
    let mut seqs: Option<(u64, u64)> = None;
    let mut entities: BTreeMap<EntityKey, u64> = BTreeMap::new();
    let mut stats: BTreeMap<(EntityKey, String), Stat> = BTreeMap::new();
    let mut by_event: BTreeMap<String, u64> = BTreeMap::new();
    let mut event_rows: Vec<(Record, String, Vec<String>)> = Vec::new();
    let mut event_total = 0u64;

    for rec in replay.records() {
        total += 1;
        let widen = |r: &mut Option<(u64, u64)>, x: u64| {
            *r = Some(r.map_or((x, x), |(lo, hi)| (lo.min(x), hi.max(x))));
        };
        widen(&mut span, rec.t_mono_ns);
        widen(&mut wall, rec.t_wall_ms);
        widen(&mut seqs, rec.seq);
        match &rec.payload {
            Payload::Sample(s) => {
                samples += 1;
                if filter.entity_ok(&s.entity) && filter.signal_ok(&s.signal) {
                    *entities.entry(s.entity.clone()).or_default() += 1;
                    stats
                        .entry((s.entity.clone(), s.signal.clone()))
                        .or_default()
                        .add(&s.value);
                }
            }
            Payload::Sweep(s) => {
                sweeps += 1;
                if filter.entity_ok(&s.entity) {
                    for (name, v) in s.signals.iter().zip(&s.values) {
                        if filter.signal_ok(name) {
                            *entities.entry(s.entity.clone()).or_default() += 1;
                            stats
                                .entry((s.entity.clone(), name.clone()))
                                .or_default()
                                .add(v);
                        }
                    }
                }
            }
            Payload::Event(e) => {
                events_n += 1;
                *by_event.entry(event_name(e)).or_default() += 1;
                let named = event_entities(e);
                let keep = filter.signal.is_none()
                    && (filter.entity.is_none() || named.iter().any(|k| filter.entity_ok(k)));
                if keep {
                    event_total += 1;
                    for k in &named {
                        if filter.entity_ok(k) {
                            *entities.entry((*k).clone()).or_default() += 1;
                        }
                    }
                    if event_rows.len() < EVENT_LIST_MAX {
                        let who = named.iter().map(|k| k.to_string()).collect();
                        event_rows.push((rec.clone(), event_name(e), who));
                    }
                }
            }
            Payload::Raw(_) => raws += 1,
            Payload::Gap(_) => gaps += 1,
        }
    }

    match &header {
        Some(h) => {
            // `backend` and `mode` are what the host recorded the log from
            // (see `BusConfig::header_tags`). They are printed on their own line.
            let tools: Vec<String> = h
                .tool_versions
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "backend" | "mode"))
                .map(|(k, v)| format!("{k} {v}"))
                .collect();
            writeln!(
                out,
                "header: schema {}, {:?}, written by {}",
                h.schema,
                h.kind,
                tools.join(", ")
            )?;
            let tag = |k: &str| h.tool_versions.get(k).map_or("unknown", String::as_str);
            writeln!(
                out,
                "recorded by: backend {}, mode {}",
                tag("backend"),
                tag("mode")
            )?;
        }
        None => writeln!(out, "header: none (the first line was not a header)")?,
    }
    writeln!(out, "log: {}", path.display())?;
    writeln!(
        out,
        "records: {total} (skipped {} bad lines)",
        replay.bad_lines()
    )?;
    if replay.hole_count() == 0 {
        writeln!(out, "holes: 0 holes")?;
    }
    for h in replay.holes() {
        let after = h.after_seq.map_or("start".to_string(), |n| format!("#{n}"));
        let next = h.next_seq.map_or("none".to_string(), |n| format!("#{n}"));
        write!(out, "holes: after {after}, next {next}: {}", h.describe())?;
        if let Some(d) = &h.detail {
            write!(out, " ({d})")?;
        }
        writeln!(out)?;
    }
    writeln!(
        out,
        "by kind: sample {samples}, sweep {sweeps}, event {events_n}, raw {raws}, gap {gaps}"
    )?;
    if let (Some((lo, hi)), Some((wlo, whi)), Some((slo, shi))) = (span, wall, seqs) {
        writeln!(
            out,
            "time span: {} (monotonic {} to {}), wall clock {wlo} to {whi} ms since the epoch, sequence {slo} to {shi}",
            secs(hi - lo),
            secs(lo),
            secs(hi)
        )?;
    }
    if filter.entity.is_some() || filter.signal.is_some() {
        writeln!(
            out,
            "filter: entity {}, signal {} (sections below only)",
            filter.entity.as_deref().unwrap_or("any"),
            filter.signal.as_deref().unwrap_or("any")
        )?;
    }

    writeln!(out, "\nentities ({}):", entities.len())?;
    for (k, n) in &entities {
        writeln!(out, "  {k}  ({n} records)")?;
    }

    if filter.signal.is_none() {
        writeln!(out, "\nevents ({event_total}):")?;
        if filter.entity.is_none() {
            for (name, n) in &by_event {
                writeln!(out, "  {name}: {n}")?;
            }
        }
        for (rec, name, who) in &event_rows {
            writeln!(
                out,
                "  #{} at {} {} {}",
                rec.seq,
                secs(rec.t_mono_ns),
                name,
                who.join(" ")
            )?;
        }
        if event_total > EVENT_LIST_MAX as u64 {
            writeln!(out, "  and {} more", event_total - EVENT_LIST_MAX as u64)?;
        }
    }

    writeln!(out, "\nsignals ({}):", stats.len())?;
    for ((e, s), st) in &stats {
        let range = match (st.min, st.max) {
            (Some(lo), Some(hi)) => format!("min {} max {}", num(lo), num(hi)),
            _ => "text".to_string(),
        };
        let last = st.last.as_ref().map_or_else(String::new, show);
        writeln!(out, "  {e} {s}: {} samples, {range}, last {last}", st.count)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bringup.jsonl");

    fn run(filter: &Filter) -> String {
        let mut out = Vec::new();
        inspect(Path::new(FIXTURE), filter, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The backend and mode a host recorded in the header are printed on
    /// their own line; a log without them says so.
    #[test]
    fn prints_the_recording_backend_and_mode() {
        let dir = crate::testutil::TempDir::new("inspect-tags");
        let mut cfg = crate::BusConfig {
            dir: Some(dir.path().to_path_buf()),
            ..crate::BusConfig::default()
        };
        cfg.header_tags.insert("backend".into(), "mock".into());
        cfg.header_tags.insert("mode".into(), "mock".into());
        let bus = crate::Bus::open(cfg).unwrap();
        bus.publish("t", crate::Payload::Gap(crate::Gap { reason: "x".into() }));
        drop(bus);
        let mut out = Vec::new();
        inspect(dir.path(), &Filter::default(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("recorded by: backend mock, mode mock"),
            "{text}"
        );
        assert!(!text.contains("written by backend"), "{text}");
        assert!(run(&Filter::default()).contains("recorded by: backend unknown, mode unknown"));
    }

    #[test]
    fn lists_the_model_entity_and_the_reset_event() {
        let text = run(&Filter::default());
        assert!(text.contains("model:llama-8b"), "{text}");
        assert!(text.contains("Reset"), "{text}");
    }

    #[test]
    fn summary_has_header_counts_span_and_signal_stats() {
        let text = run(&Filter::default());
        assert!(text.contains("header: schema 1, Live"), "{text}");
        assert!(
            text.contains("records: 114 (skipped 0 bad lines)"),
            "{text}"
        );
        assert!(text.contains("event 6"), "{text}");
        assert!(text.contains("time span: 20.000s"), "{text}");
        assert!(
            text.contains("model:llama-8b model.progress: 13 samples, min 0 max 1, last 1"),
            "{text}"
        );
        assert!(text.contains("RequestsCompleted: 3"), "{text}");
        assert!(text.contains("Reset chip:0000:01:00.0"), "{text}");
    }

    #[test]
    fn entity_filter_keeps_that_entity_and_its_events() {
        let text = run(&Filter {
            entity: Some("chip:0000:02:00.0".into()),
            signal: None,
        });
        assert!(text.contains("chip:0000:02:00.0 chip.temp_c"), "{text}");
        assert!(!text.contains("chip:0000:01:00.0 chip.temp_c"), "{text}");
        assert!(!text.contains("model.tokens_per_s"), "{text}");
        // The reset names both chips, so it stays; request events do not.
        assert!(text.contains("Reset"), "{text}");
        assert!(!text.contains("RequestsCompleted"), "{text}");
    }

    #[test]
    fn signal_filter_keeps_that_signal_and_drops_events() {
        let text = run(&Filter {
            entity: None,
            signal: Some("chip.temp_c".into()),
        });
        assert!(text.contains("signals (2):"), "{text}");
        assert!(!text.contains("events ("), "{text}");
        assert!(!text.contains("chip.power_w"), "{text}");
    }

    #[test]
    fn a_missing_log_is_an_error() {
        let mut out = Vec::new();
        assert!(inspect(Path::new("/nonexistent/bus"), &Filter::default(), &mut out).is_err());
    }

    #[test]
    fn a_hole_in_the_log_is_printed() {
        let dir = crate::testutil::TempDir::new("inspect-hole");
        std::fs::create_dir_all(dir.path()).unwrap();
        let text = std::fs::read_to_string(FIXTURE).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Header + records 1..=10 in one segment, records 21.. in another.
        std::fs::write(
            dir.path().join("bus-1.jsonl"),
            lines[..11].join("\n") + "\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("bus-21.jsonl"),
            lines[21..].join("\n") + "\n",
        )
        .unwrap();
        let mut out = Vec::new();
        inspect(dir.path(), &Filter::default(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("holes: after #10, next #21"), "{text}");
        assert!(text.contains("missing records 11..=20"), "{text}");
        assert!(!text.contains("0 holes"), "{text}");
    }

    #[test]
    fn a_head_hole_is_printed_with_its_detail() {
        let dir = crate::testutil::TempDir::new("inspect-head");
        std::fs::create_dir_all(dir.path()).unwrap();
        let text = std::fs::read_to_string(FIXTURE).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(dir.path().join("bus-1.jsonl"), "").unwrap();
        std::fs::write(
            dir.path().join("bus-21.jsonl"),
            lines[21..].join("\n") + "\n",
        )
        .unwrap();
        let mut out = Vec::new();
        inspect(dir.path(), &Filter::default(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("holes: after start, next #21"), "{text}");
        assert!(text.contains("segment unreadable"), "{text}");
        assert!(text.contains("bus-1.jsonl is empty"), "{text}");
    }

    #[test]
    fn an_intact_log_says_zero_holes() {
        assert!(run(&Filter::default()).contains("holes: 0 holes"));
    }
}
