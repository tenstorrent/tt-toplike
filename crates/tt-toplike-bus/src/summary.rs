//! The world summary: what the socket's `snapshot` op sends, and what a
//! remote reader keeps in sync.
//!
//! [`World`] is the full reducer state and is not serialisable. A
//! [`WorldSummary`] is the part of it another program needs, as plain
//! `Serialize + Deserialize` types. The server builds one with
//! [`WorldSummary::from_world`]; the client deserialises the same types and
//! keeps them current with [`WorldSummary::apply`], which folds a record in
//! the way [`World::apply`] does for the fields the summary holds.
//!
//! These types build on every platform. Only the socket that carries them
//! is Unix-only.
//!
//! # Wire shape
//!
//! ```text
//! {"seq":N,                         // the snapshot is current to record N
//!  "instance":I,                    // the server bus's random id (Bus::instance)
//!  "kind":"Live",                   // the server bus's header kind: Live or Replay
//!  "host":"box/1a2b...",            // the server machine's identity (crate::ident)
//!  "backend":"hybrid","mode":"live",// the server bus's header tags
//!  "world":{
//!    "now_mono_ns":T,               // server monotonic time the ages are taken at
//!    "entities":["chip:0000:03:00.0", ...],        // every entity seen, key order
//!    "readings":[{"entity":"chip:...","signal":"tt_asic_temperature_celsius",
//!                 "value":{"f":51.5},"prov":"measured",
//!                 "seq":41,"t_mono_ns":123,"age_ms":250}, ...],
//!    "models":[{"key":"model:llama","label":"llama","phase":"ready",
//!               "chips":["chip:..."],"progress":null,"tokens_per_s":12.5,
//!               "running":1,"waiting":0,"kv_cache":0.4}, ...],
//!    "events":[{"seq":40,"event":{"t":"Reset","chips":["chip:..."]}}, ...],
//!    "resets":[{"chip":"chip:...","seq":40}, ...],
//!    "raw":[{"source":"tt-smi","bytes":18342}, ...]}}
//! ```
//!
//! * `value` uses the record format's value encoding (`{"f":..}`,
//!   `{"u":..}`, `{"i":..}`, `{"t":".."}`); `prov` is `measured`, `derived`,
//!   `estimated` or `decoration`; `event` is the record format's event.
//! * `age_ms` is `now_mono_ns - t_mono_ns` in milliseconds. A reading is
//!   never zeroed when its source goes quiet; its age grows.
//! * `events` holds the last [`SUMMARY_EVENTS`] events, oldest first.
//! * `raw` lists the newest raw frame of each source by size only. The
//!   socket's `raw` op returns a body.
//! * `kind` says whether the server's bus is live or a replay
//!   ([`crate::Bus::header`]). A server that predates the field sends none,
//!   and a reader takes it as `Live`.
//! * `host` is the server machine's identity ([`crate::ident`]): its host
//!   name and a hash of its machine id, never the raw id. `backend` and
//!   `mode` are the bus header's tags of those names (`mode` is `live`,
//!   `mock` or `replay`). A reader uses them to decide whether a remote
//!   world describes its own chips. Older servers send none of the three;
//!   they read as empty strings.
//!
//! # Pruning
//!
//! `World` drops churning entities (processes, leases, runs, models that
//! were never announced) after [`crate::world::ENTITY_TTL_NS`] of silence.
//! `apply` alone does not prune. A reader that keeps a summary current for
//! a long time (an attached TUI) folds records in through a
//! [`SummaryPruner`], which applies the same rule at the same cadence
//! (every [`crate::world::PRUNE_EVERY_NS`] of record time), so the copy
//! stays about as large as the server's world.

use crate::entity::EntityKey;
use crate::record::{Event, HeaderKind, Payload, Provenance, Record, Value};
use crate::world::{
    expired, is_churning, mentions, update_view, ModelPhase, ModelView, World, PRUNE_EVERY_NS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// How many of the newest events a summary carries.
pub const SUMMARY_EVENTS: usize = 64;

/// The latest value of one signal on one entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingSummary {
    pub entity: EntityKey,
    pub signal: String,
    pub value: Value,
    pub prov: Provenance,
    /// Sequence number of the record that set it.
    pub seq: u64,
    /// Server monotonic time (ns) of that record.
    pub t_mono_ns: u64,
    /// Age at [`WorldSummary::now_mono_ns`], in milliseconds.
    pub age_ms: u64,
}

/// One event and its sequence number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventSummary {
    pub seq: u64,
    pub event: Event,
}

/// A chip's latest `Reset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetSummary {
    pub chip: EntityKey,
    pub seq: u64,
}

/// The newest raw frame of one source, by size. The body is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSummary {
    /// The record's source name (the key `World::latest_raw` takes).
    pub source: String,
    /// Length of the frame body in bytes.
    pub bytes: u64,
}

/// The part of [`World`] another program reads. See the module doc.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WorldSummary {
    pub now_mono_ns: u64,
    pub entities: Vec<EntityKey>,
    pub readings: Vec<ReadingSummary>,
    pub models: Vec<ModelView>,
    pub events: Vec<EventSummary>,
    pub resets: Vec<ResetSummary>,
    pub raw: Vec<RawSummary>,
}

/// A snapshot reply: the summary and the sequence number it is current to.
/// The server sends exactly this as the `snapshot` reply line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSnapshot {
    pub seq: u64,
    /// The server bus's [`crate::Bus::instance`].
    pub instance: u64,
    /// The server bus's header kind: `Live`, or `Replay` for a bus that
    /// republishes a recorded log. Absent from older servers; read as
    /// `Live` then.
    #[serde(default = "live")]
    pub kind: HeaderKind,
    /// The server machine's identity ([`crate::ident::host_identity`]).
    /// Empty from older servers.
    #[serde(default)]
    pub host: String,
    /// The server bus's `backend` header tag. Empty when absent.
    #[serde(default)]
    pub backend: String,
    /// The server bus's `mode` header tag (`live`, `mock`, `replay`).
    /// Empty when absent.
    #[serde(default)]
    pub mode: String,
    /// Every header tag of the server bus ([`crate::BusConfig::header_tags`]
    /// plus the `tt-toplike-bus` version), so a client can read a tag the
    /// fields above do not name (the saga's season length, for one).
    /// Empty from older servers.
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    pub world: WorldSummary,
}

/// The header kind a snapshot without a `kind` field is read as.
fn live() -> HeaderKind {
    HeaderKind::Live
}

impl WorldSummary {
    /// Summarise `w`, taking ages at `now_mono_ns` (the bus clock's now).
    pub fn from_world(w: &World, now_mono_ns: u64) -> WorldSummary {
        let skip = w.events().len().saturating_sub(SUMMARY_EVENTS);
        WorldSummary {
            now_mono_ns,
            entities: w.all_entities().cloned().collect(),
            readings: w
                .readings()
                .map(|(e, s, r)| ReadingSummary {
                    entity: e.clone(),
                    signal: s.to_string(),
                    value: r.value.clone(),
                    prov: r.prov,
                    seq: r.seq,
                    t_mono_ns: r.t_mono_ns,
                    age_ms: w.age_ms(r, now_mono_ns),
                })
                .collect(),
            models: w.models(),
            events: w
                .events()
                .iter()
                .skip(skip)
                .map(|(q, e)| EventSummary {
                    seq: *q,
                    event: e.clone(),
                })
                .collect(),
            resets: w
                .resets()
                .map(|(c, q)| ResetSummary {
                    chip: c.clone(),
                    seq: q,
                })
                .collect(),
            raw: w
                .raw_frames()
                .map(|(s, f)| RawSummary {
                    source: s.to_string(),
                    bytes: f.body.len() as u64,
                })
                .collect(),
        }
    }

    /// The latest reading of `signal` on `entity`.
    pub fn reading(&self, entity: &EntityKey, signal: &str) -> Option<&ReadingSummary> {
        self.find_reading(entity, signal)
            .ok()
            .map(|i| &self.readings[i])
    }

    /// Recompute every `age_ms` for a new `now_mono_ns`.
    pub fn set_now(&mut self, now_mono_ns: u64) {
        self.now_mono_ns = now_mono_ns;
        for r in &mut self.readings {
            r.age_ms = now_mono_ns.saturating_sub(r.t_mono_ns) / 1_000_000;
        }
    }

    /// Fold one record in, as [`World::apply`] does for the fields a summary
    /// holds (see the module doc for what is not copied). Records must
    /// arrive in log order. `now_mono_ns` moves forward to the record's time.
    pub fn apply(&mut self, rec: &Record) {
        if rec.t_mono_ns > self.now_mono_ns {
            self.now_mono_ns = rec.t_mono_ns;
        }
        match &rec.payload {
            Payload::Sample(s) => self.set(rec, &s.entity, &s.signal, &s.value, s.prov),
            Payload::Sweep(s) => {
                for (sig, v) in s.signals.iter().zip(&s.values) {
                    self.set(rec, &s.entity, sig, v, s.prov);
                }
            }
            Payload::Event(e) => {
                self.apply_event(rec.seq, e);
                if self.events.len() >= SUMMARY_EVENTS {
                    let extra = self.events.len() + 1 - SUMMARY_EVENTS;
                    self.events.drain(..extra);
                }
                self.events.push(EventSummary {
                    seq: rec.seq,
                    event: e.clone(),
                });
            }
            Payload::Raw(f) => {
                let bytes = f.body.len() as u64;
                match self.raw.binary_search_by(|r| r.source.cmp(&rec.source)) {
                    Ok(i) => self.raw[i].bytes = bytes,
                    Err(i) => self.raw.insert(
                        i,
                        RawSummary {
                            source: rec.source.clone(),
                            bytes,
                        },
                    ),
                }
            }
            Payload::Gap(_) => {}
        }
    }

    fn find_reading(&self, entity: &EntityKey, signal: &str) -> Result<usize, usize> {
        self.readings
            .binary_search_by(|r| (&r.entity, r.signal.as_str()).cmp(&(entity, signal)))
    }

    fn know(&mut self, e: &EntityKey) {
        if let Err(i) = self.entities.binary_search(e) {
            self.entities.insert(i, e.clone());
        }
    }

    fn set(&mut self, rec: &Record, entity: &EntityKey, signal: &str, v: &Value, prov: Provenance) {
        self.know(entity);
        if let Some(m) = self.models.iter_mut().find(|m| &m.key == entity) {
            update_view(m, signal, v);
        }
        let r = ReadingSummary {
            entity: entity.clone(),
            signal: signal.to_string(),
            value: v.clone(),
            prov,
            seq: rec.seq,
            t_mono_ns: rec.t_mono_ns,
            age_ms: self.now_mono_ns.saturating_sub(rec.t_mono_ns) / 1_000_000,
        };
        match self.find_reading(entity, signal) {
            Ok(i) => self.readings[i] = r,
            Err(i) => self.readings.insert(i, r),
        }
    }

    fn apply_event(&mut self, seq: u64, e: &Event) {
        match e {
            Event::ModelLoading {
                model,
                chips,
                progress,
            } => {
                let m = self.announce(model, chips);
                m.phase = ModelPhase::Loading;
                m.progress = *progress;
            }
            Event::ModelReady { model, chips } => {
                let m = self.announce(model, chips);
                m.phase = ModelPhase::Ready;
                m.progress = None;
            }
            Event::ModelGone { model } => {
                self.models.retain(|m| &m.key != model);
                self.readings.retain(|r| &r.entity != model);
            }
            Event::Reset { chips } => {
                for c in chips {
                    self.know(c);
                    match self.resets.binary_search_by(|r| r.chip.cmp(c)) {
                        Ok(i) => self.resets[i].seq = seq,
                        Err(i) => self.resets.insert(
                            i,
                            ResetSummary {
                                chip: c.clone(),
                                seq,
                            },
                        ),
                    }
                }
            }
            _ => {}
        }
    }

    /// The model's view, created and filled from held readings if new.
    fn announce(&mut self, model: &EntityKey, chips: &[EntityKey]) -> &mut ModelView {
        self.know(model);
        for c in chips {
            self.know(c);
        }
        let i = match self.models.binary_search_by(|m| m.key.cmp(model)) {
            Ok(i) => i,
            Err(i) => {
                let mut v = ModelView {
                    key: model.clone(),
                    label: model
                        .as_str()
                        .strip_prefix("model:")
                        .unwrap_or(model.as_str())
                        .to_string(),
                    phase: ModelPhase::Down,
                    chips: Vec::new(),
                    progress: None,
                    tokens_per_s: None,
                    running: None,
                    waiting: None,
                    kv_cache: None,
                };
                for r in self.readings.iter().filter(|r| &r.entity == model) {
                    update_view(&mut v, &r.signal, &r.value);
                }
                self.models.insert(i, v);
                i
            }
        };
        let m = &mut self.models[i];
        m.chips = chips.to_vec();
        m
    }
}

/// Keeps a [`WorldSummary`] pruned the way [`World`] prunes itself (see the
/// module doc). It remembers, for each churning entity, the record time it
/// was last seen ([`crate::world::mentions`]), and every
/// [`PRUNE_EVERY_NS`] of record time drops the entities the shared rule
/// ([`crate::world::expired`]) calls stale: their readings and their entry
/// in `entities`. Chips, boards, DRAM channels, Ethernet ports, tiles and
/// hosts are never dropped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryPruner {
    touched: BTreeMap<EntityKey, u64>,
    last_prune_ns: u64,
}

impl SummaryPruner {
    /// A pruner for a summary that came from a snapshot. Each churning
    /// entity counts as last seen at its newest reading's time, or at the
    /// snapshot's `now_mono_ns` when it has no reading (the server knew it
    /// recently enough to keep it). The first prune runs one
    /// [`PRUNE_EVERY_NS`] after the snapshot's time.
    pub fn for_snapshot(w: &WorldSummary) -> SummaryPruner {
        let mut touched: BTreeMap<EntityKey, u64> = BTreeMap::new();
        for r in &w.readings {
            if is_churning(r.entity.kind()) {
                let t = touched.entry(r.entity.clone()).or_insert(0);
                *t = (*t).max(r.t_mono_ns);
            }
        }
        for e in &w.entities {
            if is_churning(e.kind()) {
                touched.entry(e.clone()).or_insert(w.now_mono_ns);
            }
        }
        SummaryPruner {
            touched,
            last_prune_ns: w.now_mono_ns,
        }
    }

    /// Fold `rec` into `w` ([`WorldSummary::apply`]), note the churning
    /// entities it mentions, and prune when a sweep is due.
    pub fn apply(&mut self, w: &mut WorldSummary, rec: &Record) {
        let t = rec.t_mono_ns;
        mentions(rec, |k| {
            if is_churning(k.kind()) {
                self.touched.insert(k.clone(), t);
            }
        });
        w.apply(rec);
        if t.saturating_sub(self.last_prune_ns) >= PRUNE_EVERY_NS {
            self.prune(w, t);
        }
    }

    /// Entities with remembered last-seen times (for tests and sizing).
    pub fn tracked(&self) -> usize {
        self.touched.len()
    }

    fn prune(&mut self, w: &mut WorldSummary, now: u64) {
        self.last_prune_ns = now;
        let announced: BTreeSet<&EntityKey> = w.models.iter().map(|m| &m.key).collect();
        let stale: BTreeSet<EntityKey> = self
            .touched
            .iter()
            .filter(|(k, t)| expired(k, **t, now, announced.contains(k)))
            .map(|(k, _)| k.clone())
            .collect();
        if stale.is_empty() {
            return;
        }
        self.touched.retain(|k, _| !stale.contains(k));
        w.readings.retain(|r| !stale.contains(&r.entity));
        w.entities.retain(|e| !stale.contains(e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BusConfig, ManualClock};
    use crate::log::Bus;
    use crate::record::{RawFrame, Sample, Sweep};
    use crate::signals;
    use std::time::Duration;

    fn chip(n: u8) -> EntityKey {
        EntityKey::chip(&format!("0000:0{n}:00.0"))
    }

    fn sample(e: &EntityKey, sig: &str, v: Value) -> Payload {
        Payload::Sample(Sample {
            entity: e.clone(),
            signal: sig.into(),
            value: v,
            prov: Provenance::Measured,
        })
    }

    /// A record stream that touches every branch of `apply`: samples and
    /// sweeps before and after a model is announced, loading and ready,
    /// gone, resets, raw frames and more events than the summary keeps.
    fn varied(bus: &Bus, clock: &ManualClock, from: u64) {
        let m = EntityKey::model("llama");
        let step = |p: Payload| {
            clock.advance(Duration::from_millis(7));
            bus.publish("t", p);
        };
        step(sample(
            &m,
            signals::MODEL_TOKENS_PER_S,
            Value::F64(3.0 + from as f64),
        ));
        step(Payload::Event(Event::ModelLoading {
            model: m.clone(),
            chips: vec![chip(1)],
            progress: Some(0.5),
        }));
        step(sample(&m, signals::MODEL_RUNNING, Value::U64(2)));
        step(Payload::Event(Event::ModelReady {
            model: m.clone(),
            chips: vec![chip(1), chip(2)],
        }));
        step(Payload::Sweep(Sweep {
            entity: chip(1),
            signals: vec!["a".into(), "b".into()],
            values: vec![Value::F64(1.0), Value::I64(-1)],
            prov: Provenance::Derived,
        }));
        step(Payload::Raw(RawFrame {
            source: "tt-smi".into(),
            body: "x".repeat(10 + from as usize),
        }));
        for i in 0..70 {
            step(Payload::Event(Event::Reset {
                chips: vec![chip(1 + (i % 3) as u8)],
            }));
        }
        let gone = EntityKey::model("gone");
        step(Payload::Event(Event::ModelReady {
            model: gone.clone(),
            chips: vec![chip(3)],
        }));
        step(sample(&gone, signals::MODEL_KV_CACHE, Value::F64(0.1)));
        step(Payload::Event(Event::ModelGone { model: gone }));
    }

    /// A summary kept current by `apply` equals a fresh summary of the
    /// world the bus folded from the same records.
    #[test]
    fn apply_matches_a_fresh_summary() {
        let clock = ManualClock::new(1_000);
        let bus = Bus::open_with_world(BusConfig {
            clock: clock.clone(),
            ..BusConfig::default()
        })
        .unwrap();
        varied(&bus, &clock, 0);
        let snap = bus.snapshot().unwrap();
        let mut kept = WorldSummary::from_world(&snap.world, bus.now_mono_ns());
        varied(&bus, &clock, 5);
        for r in bus.read_from(snap.seq + 1, usize::MAX).unwrap() {
            kept.apply(&r);
        }
        let fresh = WorldSummary::from_world(&bus.snapshot().unwrap().world, bus.now_mono_ns());
        kept.set_now(fresh.now_mono_ns);
        assert_eq!(kept, fresh);
        assert_eq!(fresh.events.len(), SUMMARY_EVENTS);
        assert!(fresh.models.iter().all(|m| m.label != "gone"));
        assert_eq!(
            fresh.raw,
            vec![RawSummary {
                source: "t".into(),
                bytes: 15
            }]
        );
    }

    #[test]
    fn summary_round_trips_through_json() {
        let clock = ManualClock::new(1_000);
        let bus = Bus::open_with_world(BusConfig {
            clock: clock.clone(),
            ..BusConfig::default()
        })
        .unwrap();
        varied(&bus, &clock, 0);
        let snap = bus.snapshot().unwrap();
        let s = RemoteSnapshot {
            seq: snap.seq,
            instance: bus.instance(),
            kind: HeaderKind::Replay,
            host: "box/0011223344556677".into(),
            backend: "mock".into(),
            mode: "replay".into(),
            tags: [("saga.season_ticks".to_string(), "200".to_string())].into(),
            world: WorldSummary::from_world(&snap.world, bus.now_mono_ns()),
        };
        let line = serde_json::to_string(&s).unwrap();
        assert!(line.contains("\"phase\":\"ready\""), "{line}");
        assert_eq!(serde_json::from_str::<RemoteSnapshot>(&line).unwrap(), s);
        assert!(line.contains("\"kind\":\"Replay\""), "{line}");
        assert!(line.contains("\"host\":\"box/0011223344556677\""), "{line}");
        assert!(line.contains("\"mode\":\"replay\""), "{line}");
        assert!(line.contains("\"backend\":\"mock\""), "{line}");
        assert!(
            line.contains("\"tags\":{\"saga.season_ticks\":\"200\"}"),
            "{line}"
        );
        // An older server sends no tags: the map is empty.
        let older = line.replace(",\"tags\":{\"saga.season_ticks\":\"200\"}", "");
        assert!(serde_json::from_str::<RemoteSnapshot>(&older)
            .unwrap()
            .tags
            .is_empty());
    }

    /// A snapshot line from a server that sends no `kind` is read as live.
    #[test]
    fn a_snapshot_without_a_kind_is_live() {
        let line = r#"{"seq":0,"instance":1,"world":{"now_mono_ns":0,"entities":[],"readings":[],"models":[],"events":[],"resets":[],"raw":[]}}"#;
        let s: RemoteSnapshot = serde_json::from_str(line).unwrap();
        assert_eq!(s.kind, HeaderKind::Live);
        assert_eq!(
            (s.host.as_str(), s.backend.as_str(), s.mode.as_str()),
            ("", "", "")
        );
    }

    #[test]
    fn ages_grow_with_now() {
        let mut s = WorldSummary::default();
        let rec = Record {
            seq: 1,
            t_mono_ns: 1_000_000_000,
            t_wall_ms: 0,
            source: "t".into(),
            payload: sample(&chip(1), "x", Value::U64(1)),
        };
        s.apply(&rec);
        assert_eq!(s.reading(&chip(1), "x").unwrap().age_ms, 0);
        s.set_now(3_000_000_000);
        assert_eq!(s.reading(&chip(1), "x").unwrap().age_ms, 2_000);
        assert_eq!(s.reading(&chip(1), "x").unwrap().value, Value::U64(1));
    }

    // ---- Pruning an applied summary (final fix wave, I2) -------------------

    const SEC: u64 = 1_000_000_000;

    fn at(seq: u64, t: u64, p: Payload) -> Record {
        Record {
            seq,
            t_mono_ns: t,
            t_wall_ms: 0,
            source: "t".into(),
            payload: p,
        }
    }

    /// 10,000 processes, each seen once, one per second (about 2.8 hours of
    /// record time). The client's copy holds at most the last 15 minutes of
    /// them plus one prune interval: under 1,000 entities and readings.
    #[test]
    fn ten_thousand_short_lived_processes_keep_the_summary_bounded() {
        let mut w = WorldSummary::default();
        let mut p = SummaryPruner::for_snapshot(&w);
        let mut seq = 0;
        for i in 0..10_000u64 {
            seq += 1;
            let e = EntityKey::process(i as u32 + 1, 7);
            p.apply(
                &mut w,
                &at(seq, i * SEC, sample(&e, "cpu", Value::F64(1.0))),
            );
        }
        // 900 s of live entities plus up to 60 s since the last sweep.
        let bound = 1_000;
        assert!(w.entities.len() < bound, "{} entities", w.entities.len());
        assert!(w.readings.len() < bound, "{} readings", w.readings.len());
        assert!(p.tracked() < bound, "{} tracked", p.tracked());
    }

    /// A chip that went quiet is never pruned: it stays and its age grows.
    /// A process updated every minute stays too.
    #[test]
    fn a_quiet_chip_and_a_process_seen_every_minute_stay() {
        let mut w = WorldSummary::default();
        let mut p = SummaryPruner::for_snapshot(&w);
        let proc_key = EntityKey::process(42, 1);
        let gone = EntityKey::process(43, 1);
        let mut seq = 1;
        p.apply(
            &mut w,
            &at(seq, 0, sample(&chip(1), "temp", Value::F64(50.0))),
        );
        seq += 1;
        p.apply(&mut w, &at(seq, 0, sample(&gone, "cpu", Value::F64(1.0))));
        for minute in 1..=60u64 {
            seq += 1;
            let r = at(
                seq,
                minute * 60 * SEC,
                sample(&proc_key, "cpu", Value::F64(2.0)),
            );
            p.apply(&mut w, &r);
        }
        assert!(w.reading(&chip(1), "temp").is_some(), "the chip stays");
        assert!(w.entities.contains(&chip(1)));
        w.set_now(w.now_mono_ns);
        assert_eq!(w.reading(&chip(1), "temp").unwrap().age_ms, 3_600_000);
        assert!(
            w.reading(&proc_key, "cpu").is_some(),
            "the busy process stays"
        );
        assert!(w.reading(&gone, "cpu").is_none(), "the quiet process went");
        assert!(!w.entities.contains(&gone));
    }

    /// The client's pruned copy keeps exactly the entities and readings a
    /// fresh snapshot of the server's world would show at the same record.
    #[test]
    fn a_pruned_summary_matches_a_fresh_snapshot_at_the_same_cut() {
        let mut world = World::new();
        let mut w = WorldSummary::default();
        let mut p = SummaryPruner::for_snapshot(&w);
        let m = EntityKey::model("llama");
        let unannounced = EntityKey::model("ghost");
        let lease = EntityKey::lease("L1");
        let mut seq = 0;
        let mut push = |t: u64, pl: Payload, world: &mut World, w: &mut WorldSummary| {
            seq += 1;
            let r = at(seq, t, pl);
            world.apply(&r);
            p.apply(w, &r);
        };
        push(
            0,
            sample(&unannounced, "x", Value::U64(1)),
            &mut world,
            &mut w,
        );
        push(
            SEC,
            Payload::Event(Event::ModelReady {
                model: m.clone(),
                chips: vec![chip(1)],
            }),
            &mut world,
            &mut w,
        );
        push(
            2 * SEC,
            Payload::Event(Event::LeaseGranted {
                lease: lease.clone(),
                chips: vec![chip(1)],
            }),
            &mut world,
            &mut w,
        );
        for i in 0..3_000u64 {
            let t = 3 * SEC + i * SEC;
            let e = EntityKey::process((i % 1_500) as u32 + 1, 0);
            push(
                t,
                sample(&e, "cpu", Value::F64(i as f64)),
                &mut world,
                &mut w,
            );
            if i % 120 == 0 {
                push(
                    t,
                    sample(&chip(2), "temp", Value::F64(40.0)),
                    &mut world,
                    &mut w,
                );
                push(t, sample(&m, "tps", Value::F64(5.0)), &mut world, &mut w);
            }
        }
        let fresh = WorldSummary::from_world(&world, w.now_mono_ns);
        assert_eq!(w.entities, fresh.entities);
        let keys = |s: &WorldSummary| -> Vec<(EntityKey, String)> {
            s.readings
                .iter()
                .map(|r| (r.entity.clone(), r.signal.clone()))
                .collect()
        };
        assert_eq!(keys(&w), keys(&fresh));
        // The run did prune: the unannounced model is gone and the
        // announced model stays.
        assert!(!w.entities.contains(&unannounced));
        assert!(w.entities.contains(&m));
        assert!(w.entities.len() < 1_100, "{}", w.entities.len());
    }
}
