//! The world: the current state, folded from the log.
//!
//! [`World`] is a [`Reducer`]. The bus applies every record to it in log
//! order under the publish lock, so a live world and a world rebuilt by
//! replaying the log are equal.
//!
//! What it holds:
//!
//! * the latest [`Reading`] of every signal on every entity, with the
//!   provenance, sequence number and monotonic time of the record that set it;
//! * the models that events have announced, as [`ModelView`]s;
//! * the newest raw frame per source;
//! * the last [`EVENT_WINDOW`] events;
//! * for each chip, the sequence number of its latest `Reset`.
//!
//! A reading is never zeroed or removed because its source went quiet. Its
//! age grows, and [`World::age_ms`] reports it. A `Reset` does not delete
//! readings either. It records [`World::reset_seq`], so a view can tell a
//! reading from before the reset from one after it.

use crate::entity::{EntityKey, EntityKind};
use crate::log::{Bus, Reducer};
use crate::record::{Event, Payload, Provenance, RawFrame, Record, Value};
use crate::signals;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;

/// How many events [`World::events`] keeps.
pub const EVENT_WINDOW: usize = 512;

/// How old (record time, ns) the newest reading or event mention of a
/// churning entity must be before [`World::apply`] drops it. Churning kinds
/// are processes, leases, training runs and models that are not announced.
/// Chips, boards, tiles, DRAM channels, Ethernet ports and hosts are bounded
/// by hardware and are never dropped: a quiet one stays visible and ages.
pub const ENTITY_TTL_NS: u64 = 15 * 60 * 1_000_000_000;

/// The sweep for stale churning entities runs when a record's monotonic time
/// is at least this far (ns) past the previous sweep. It uses record time
/// only, so replaying the same records prunes at the same points.
pub const PRUNE_EVERY_NS: u64 = 60 * 1_000_000_000;

/// The latest value of one signal and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub value: Value,
    pub prov: Provenance,
    /// Sequence number of the record that set this value.
    pub seq: u64,
    /// Monotonic time (ns) of the record that set this value.
    pub t_mono_ns: u64,
}

/// Where a model is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPhase {
    Down,
    Compiling,
    Loading,
    Ready,
    Alarm,
}

impl ModelPhase {
    fn parse(s: &str) -> Option<ModelPhase> {
        match s {
            "down" => Some(ModelPhase::Down),
            "compiling" => Some(ModelPhase::Compiling),
            "loading" => Some(ModelPhase::Loading),
            "ready" => Some(ModelPhase::Ready),
            "alarm" => Some(ModelPhase::Alarm),
            _ => None,
        }
    }
}

/// What a view shows about one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelView {
    pub key: EntityKey,
    pub label: String,
    pub phase: ModelPhase,
    pub chips: Vec<EntityKey>,
    pub progress: Option<f32>,
    pub tokens_per_s: Option<f64>,
    pub running: Option<u32>,
    pub waiting: Option<u32>,
    pub kv_cache: Option<f64>,
}

/// The current state of everything on the bus.
///
/// `PartialEq` compares every field, including the pruning bookkeeping
/// (`touched`, `last_prune_ns`). Both are functions of the record stream, so
/// two worlds built from the same records are equal.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct World {
    seq: u64,
    readings: BTreeMap<EntityKey, BTreeMap<String, Reading>>,
    known: BTreeSet<EntityKey>,
    models: BTreeMap<EntityKey, ModelView>,
    resets: BTreeMap<EntityKey, u64>,
    raw: BTreeMap<String, RawFrame>,
    events: VecDeque<(u64, Event)>,
    /// Record time (ns) of the newest reading or event mention of each
    /// churning entity. Holds no hardware-bounded kinds.
    touched: BTreeMap<EntityKey, u64>,
    /// Record time (ns) of the last prune sweep.
    last_prune_ns: u64,
}

impl World {
    /// An empty world at sequence 0.
    pub fn new() -> World {
        World::default()
    }

    /// Sequence number of the last record applied.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Fold one record in. Records must arrive in log order.
    pub fn apply(&mut self, rec: &Record) {
        self.seq = rec.seq;
        match &rec.payload {
            Payload::Sample(s) => self.set(rec, &s.entity, &s.signal, &s.value, s.prov),
            Payload::Sweep(s) => {
                // `zip` stops at the shorter side; `sanitize` already
                // rejects a sweep whose lengths differ.
                for (sig, v) in s.signals.iter().zip(&s.values) {
                    self.set(rec, &s.entity, sig, v, s.prov);
                }
            }
            Payload::Event(e) => {
                self.apply_event(rec.seq, rec.t_mono_ns, e);
                if self.events.len() == EVENT_WINDOW {
                    self.events.pop_front();
                }
                self.events.push_back((rec.seq, e.clone()));
            }
            Payload::Raw(f) => {
                self.raw.insert(rec.source.clone(), f.clone());
            }
            Payload::Gap(_) => {}
        }
        if rec.t_mono_ns.saturating_sub(self.last_prune_ns) >= PRUNE_EVERY_NS {
            self.prune(rec.t_mono_ns);
        }
    }

    /// Note that a churning entity was seen at `t`. Other kinds are not
    /// tracked.
    fn touch(&mut self, key: &EntityKey, t: u64) {
        if is_churning(key.kind()) {
            self.touched.insert(key.clone(), t);
        }
    }

    /// Drop churning entities not seen for [`ENTITY_TTL_NS`]. Announced
    /// models are kept. `readings` and `known` lose the same keys.
    fn prune(&mut self, now: u64) {
        self.last_prune_ns = now;
        let models = &self.models;
        let stale: Vec<EntityKey> = self
            .touched
            .iter()
            .filter(|(k, t)| now.saturating_sub(**t) > ENTITY_TTL_NS && !models.contains_key(*k))
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            self.touched.remove(&k);
            self.readings.remove(&k);
            self.known.remove(&k);
        }
    }

    /// Store one reading and, for a model signal, update the model's view.
    fn set(&mut self, rec: &Record, entity: &EntityKey, signal: &str, v: &Value, prov: Provenance) {
        self.known.insert(entity.clone());
        self.touch(entity, rec.t_mono_ns);
        let r = Reading {
            value: v.clone(),
            prov,
            seq: rec.seq,
            t_mono_ns: rec.t_mono_ns,
        };
        if let Some(view) = self.models.get_mut(entity) {
            update_view(view, signal, &r.value);
        }
        self.readings
            .entry(entity.clone())
            .or_default()
            .insert(signal.to_string(), r);
    }

    fn apply_event(&mut self, seq: u64, t: u64, e: &Event) {
        match e {
            Event::ModelLoading { model, .. }
            | Event::ModelReady { model, .. }
            | Event::ModelGone { model }
            | Event::RequestsCompleted { model, .. } => self.touch(model, t),
            Event::LeaseGranted { lease, .. } | Event::LeaseReleased { lease, .. } => {
                self.touch(lease, t)
            }
            _ => {}
        }
        match e {
            Event::ModelLoading {
                model,
                chips,
                progress,
            } => {
                let v = self.announce(model, chips);
                v.phase = ModelPhase::Loading;
                v.progress = *progress;
            }
            Event::ModelReady { model, chips } => {
                let v = self.announce(model, chips);
                v.phase = ModelPhase::Ready;
                v.progress = None;
            }
            Event::ModelGone { model } => {
                self.models.remove(model);
                // The next run starts without the last run's numbers.
                self.readings.remove(model);
            }
            Event::Reset { chips } => {
                for c in chips {
                    self.known.insert(c.clone());
                    self.resets.insert(c.clone(), seq);
                }
            }
            _ => {}
        }
    }

    /// Create the model's view if it is new, fill it from readings already
    /// held, and set its chips.
    fn announce(&mut self, model: &EntityKey, chips: &[EntityKey]) -> &mut ModelView {
        self.known.insert(model.clone());
        self.known.extend(chips.iter().cloned());
        let readings = &self.readings;
        let v = self.models.entry(model.clone()).or_insert_with(|| {
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
            if let Some(rs) = readings.get(model) {
                for (sig, r) in rs {
                    update_view(&mut v, sig, &r.value);
                }
            }
            v
        });
        v.chips = chips.to_vec();
        v
    }

    /// The latest reading of `signal` on `entity`, if any was published.
    pub fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading> {
        self.readings.get(entity)?.get(signal).cloned()
    }

    /// How old a reading is at `now_mono_ns`, in milliseconds. Grows while
    /// the source is quiet. A `now` before the reading gives 0.
    pub fn age_ms(&self, r: &Reading, now_mono_ns: u64) -> u64 {
        now_mono_ns.saturating_sub(r.t_mono_ns) / 1_000_000
    }

    /// Every entity the world has seen, of one kind, in key order.
    pub fn entities(&self, kind: EntityKind) -> impl Iterator<Item = &EntityKey> {
        self.known.iter().filter(move |k| k.kind() == kind)
    }

    /// The announced models, in key order.
    pub fn models(&self) -> Vec<ModelView> {
        self.models.values().cloned().collect()
    }

    /// The model announced on `chip`. With two, the first by key.
    pub fn model_on_chip(&self, chip: &EntityKey) -> Option<&ModelView> {
        self.models.values().find(|m| m.chips.contains(chip))
    }

    /// The last [`EVENT_WINDOW`] events with their sequence numbers.
    pub fn events(&self) -> &VecDeque<(u64, Event)> {
        &self.events
    }

    /// Events with a sequence number above `seq`.
    pub fn events_since(&self, seq: u64) -> impl Iterator<Item = &(u64, Event)> {
        self.events.iter().filter(move |(q, _)| *q > seq)
    }

    /// Sequence number of the latest `Reset` that named `chip`. A reading
    /// with a lower `seq` was taken before it.
    pub fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.resets.get(chip).copied()
    }

    /// The newest raw frame published by `source` (the record's source name).
    pub fn latest_raw(&self, source: &str) -> Option<&RawFrame> {
        self.raw.get(source)
    }
}

/// Kinds that come and go while the monitor runs.
fn is_churning(k: EntityKind) -> bool {
    matches!(
        k,
        EntityKind::Process | EntityKind::Lease | EntityKind::TrainingRun | EntityKind::Model
    )
}

/// Number-like values as `f64`. Text is not a number.
fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) => Some(*x),
        Value::U64(x) => Some(*x as f64),
        Value::I64(x) => Some(*x as f64),
        Value::Text(_) => None,
    }
}

/// Non-negative whole values as `u32`, saturating. Anything else is `None`.
fn as_u32(v: &Value) -> Option<u32> {
    match v {
        Value::U64(x) => Some((*x).min(u32::MAX as u64) as u32),
        Value::I64(x) if *x >= 0 => Some((*x).min(u32::MAX as i64) as u32),
        Value::F64(x) if *x >= 0.0 => Some(*x as u32),
        _ => None,
    }
}

/// Copy one model signal into the view. Other signals are ignored.
fn update_view(view: &mut ModelView, signal: &str, v: &Value) {
    match signal {
        signals::MODEL_LABEL => {
            if let Value::Text(t) = v {
                view.label = t.clone();
            }
        }
        signals::MODEL_PHASE => {
            if let Some(p) = ModelPhase::parse(match v {
                Value::Text(t) => t,
                _ => "",
            }) {
                view.phase = p;
            }
        }
        signals::MODEL_TOKENS_PER_S => view.tokens_per_s = as_f64(v),
        signals::MODEL_RUNNING => view.running = as_u32(v),
        signals::MODEL_WAITING => view.waiting = as_u32(v),
        signals::MODEL_KV_CACHE => view.kv_cache = as_f64(v),
        signals::MODEL_PROGRESS => view.progress = as_f64(v).map(|x| x as f32),
        _ => {}
    }
}

impl Reducer for World {
    fn apply(&mut self, rec: &Record) {
        World::apply(self, rec)
    }
}

/// A copy of the world at one sequence number (see [`Bus::snapshot`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub seq: u64,
    pub world: World,
}

impl Bus {
    /// Open a bus whose reducer is a fresh [`World`].
    pub fn open_with_world(cfg: crate::BusConfig) -> io::Result<Bus> {
        Bus::open_with(cfg, Box::new(World::new()))
    }

    /// A copy of the world and the sequence number it is current to.
    ///
    /// The bus keeps the live world up to date under the publish lock, so a
    /// snapshot is one clone and never reads the segments. Returns `None`
    /// when the bus was not opened with a [`World`] reducer.
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.with_reducer(|w: &World, seq| Snapshot {
            seq,
            world: w.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BusConfig, ManualClock};
    use crate::record::{Sample, Sweep};

    fn chip(n: u8) -> EntityKey {
        EntityKey::chip(&format!("0000:0{n}:00.0"))
    }
    fn model() -> EntityKey {
        EntityKey::model("llama")
    }
    fn rec(seq: u64, mono: u64, source: &str, payload: Payload) -> Record {
        Record {
            seq,
            t_mono_ns: mono,
            t_wall_ms: 0,
            source: source.into(),
            payload,
        }
    }
    fn sample(e: &EntityKey, sig: &str, v: Value, prov: Provenance) -> Payload {
        Payload::Sample(Sample {
            entity: e.clone(),
            signal: sig.into(),
            value: v,
            prov,
        })
    }
    fn ev(e: Event) -> Payload {
        Payload::Event(e)
    }

    #[test]
    fn sample_sets_latest_reading_with_provenance() {
        let mut w = World::new();
        let c = chip(1);
        w.apply(&rec(
            1,
            10,
            "s",
            sample(&c, "tdp", Value::F64(10.0), Provenance::Measured),
        ));
        w.apply(&rec(
            2,
            20,
            "s",
            sample(&c, "tdp", Value::F64(12.0), Provenance::Derived),
        ));
        let r = w.reading(&c, "tdp").unwrap();
        assert_eq!(r.value, Value::F64(12.0));
        assert_eq!(r.prov, Provenance::Derived);
        assert_eq!((r.seq, r.t_mono_ns), (2, 20));
        assert_eq!(w.seq(), 2);
    }

    #[test]
    fn sweep_applies_every_value() {
        let mut w = World::new();
        let c = chip(1);
        w.apply(&rec(
            1,
            5,
            "s",
            Payload::Sweep(Sweep {
                entity: c.clone(),
                signals: vec!["a".into(), "b".into(), "c".into()],
                values: vec![Value::U64(1), Value::I64(-2), Value::Text("x".into())],
                prov: Provenance::Estimated,
            }),
        ));
        assert_eq!(w.reading(&c, "a").unwrap().value, Value::U64(1));
        assert_eq!(w.reading(&c, "b").unwrap().value, Value::I64(-2));
        let r = w.reading(&c, "c").unwrap();
        assert_eq!(r.value, Value::Text("x".into()));
        assert_eq!(r.prov, Provenance::Estimated);
        assert_eq!(r.seq, 1);
    }

    #[test]
    fn unpublished_signal_is_none() {
        let mut w = World::new();
        assert!(w.reading(&chip(1), "tdp").is_none());
        w.apply(&rec(
            1,
            1,
            "s",
            sample(&chip(1), "tdp", Value::U64(1), Provenance::Measured),
        ));
        assert!(w.reading(&chip(1), "other").is_none());
        assert!(w.reading(&chip(2), "tdp").is_none());
    }

    #[test]
    fn quiet_source_keeps_value_and_age_grows() {
        let mut w = World::new();
        let c = chip(1);
        w.apply(&rec(
            1,
            1_000_000_000,
            "s",
            sample(&c, "tdp", Value::F64(55.5), Provenance::Measured),
        ));
        // Another source keeps talking; "s" is silent.
        w.apply(&rec(
            2,
            90_000_000_000,
            "other",
            sample(&chip(2), "tdp", Value::F64(1.0), Provenance::Measured),
        ));
        let r = w.reading(&c, "tdp").unwrap();
        assert_eq!(r.value, Value::F64(55.5), "value must not be zeroed");
        assert_eq!(w.age_ms(&r, 1_000_000_000), 0);
        assert_eq!(w.age_ms(&r, 2_000_000_000), 1_000);
        assert_eq!(w.age_ms(&r, 61_000_000_000), 60_000);
        assert!(w.age_ms(&r, 61_000_000_000) > w.age_ms(&r, 2_000_000_000));
        // A clock reading older than the sample never underflows.
        assert_eq!(w.age_ms(&r, 0), 0);
    }

    #[test]
    fn model_lifecycle_and_chip_association() {
        let mut w = World::new();
        let (m, c1, c2) = (model(), chip(1), chip(2));
        assert!(w.models().is_empty());
        w.apply(&rec(
            1,
            1,
            "s",
            ev(Event::ModelLoading {
                model: m.clone(),
                chips: vec![c1.clone(), c2.clone()],
                progress: Some(0.25),
            }),
        ));
        let v = w.models().pop().unwrap();
        assert_eq!(v.phase, ModelPhase::Loading);
        assert_eq!(v.progress, Some(0.25));
        assert_eq!(v.chips, vec![c1.clone(), c2.clone()]);
        assert_eq!(w.model_on_chip(&c2).unwrap().key, m);
        w.apply(&rec(
            2,
            2,
            "s",
            ev(Event::ModelReady {
                model: m.clone(),
                chips: vec![c1.clone(), c2.clone()],
            }),
        ));
        assert_eq!(w.model_on_chip(&c1).unwrap().phase, ModelPhase::Ready);
        w.apply(&rec(3, 3, "s", ev(Event::ModelGone { model: m.clone() })));
        assert!(w.models().is_empty());
        assert!(w.model_on_chip(&c1).is_none());
        assert!(w.model_on_chip(&c2).is_none());
    }

    #[test]
    fn model_fields_come_from_samples_and_last_phase_wins() {
        let mut w = World::new();
        let (m, c) = (model(), chip(1));
        let s = |q, sig, v| rec(q, q, "vllm", sample(&model(), sig, v, Provenance::Measured));
        // Samples before the model is announced are kept as readings only.
        w.apply(&s(1, signals::MODEL_TOKENS_PER_S, Value::F64(7.0)));
        assert!(w.models().is_empty());
        w.apply(&rec(
            2,
            2,
            "s",
            ev(Event::ModelReady {
                model: m.clone(),
                chips: vec![c.clone()],
            }),
        ));
        let v = w.models().pop().unwrap();
        assert_eq!(
            v.tokens_per_s,
            Some(7.0),
            "earlier reading shows once announced"
        );
        assert_eq!(v.label, "llama");
        w.apply(&s(3, signals::MODEL_TOKENS_PER_S, Value::F64(42.5)));
        w.apply(&s(4, signals::MODEL_RUNNING, Value::U64(3)));
        w.apply(&s(5, signals::MODEL_WAITING, Value::U64(9)));
        w.apply(&s(6, signals::MODEL_KV_CACHE, Value::F64(0.5)));
        w.apply(&s(7, signals::MODEL_PROGRESS, Value::F64(0.75)));
        w.apply(&s(8, signals::MODEL_LABEL, Value::Text("Llama 3".into())));
        let v = w.models().pop().unwrap();
        assert_eq!(v.tokens_per_s, Some(42.5));
        assert_eq!((v.running, v.waiting), (Some(3), Some(9)));
        assert_eq!(v.kv_cache, Some(0.5));
        assert_eq!(v.progress, Some(0.75));
        assert_eq!(v.label, "Llama 3");
        // Phase: sample after event wins, then event after sample wins.
        w.apply(&s(9, signals::MODEL_PHASE, Value::Text("alarm".into())));
        assert_eq!(w.models()[0].phase, ModelPhase::Alarm);
        w.apply(&rec(
            10,
            10,
            "s",
            ev(Event::ModelReady {
                model: m.clone(),
                chips: vec![c.clone()],
            }),
        ));
        assert_eq!(w.models()[0].phase, ModelPhase::Ready);
        w.apply(&s(
            11,
            signals::MODEL_PHASE,
            Value::Text("compiling".into()),
        ));
        assert_eq!(w.models()[0].phase, ModelPhase::Compiling);
        // An unknown phase text changes nothing.
        w.apply(&s(12, signals::MODEL_PHASE, Value::Text("???".into())));
        assert_eq!(w.models()[0].phase, ModelPhase::Compiling);
    }

    #[test]
    fn gone_model_does_not_come_back_from_a_late_sample_or_keep_old_numbers() {
        let mut w = World::new();
        let (m, c) = (model(), chip(1));
        w.apply(&rec(
            1,
            1,
            "s",
            ev(Event::ModelReady {
                model: m.clone(),
                chips: vec![c.clone()],
            }),
        ));
        w.apply(&rec(
            2,
            2,
            "v",
            sample(
                &m,
                signals::MODEL_TOKENS_PER_S,
                Value::F64(9.0),
                Provenance::Measured,
            ),
        ));
        w.apply(&rec(3, 3, "s", ev(Event::ModelGone { model: m.clone() })));
        w.apply(&rec(
            4,
            4,
            "v",
            sample(
                &m,
                signals::MODEL_RUNNING,
                Value::U64(1),
                Provenance::Measured,
            ),
        ));
        assert!(w.models().is_empty());
        w.apply(&rec(
            5,
            5,
            "s",
            ev(Event::ModelLoading {
                model: m.clone(),
                chips: vec![c.clone()],
                progress: None,
            }),
        ));
        let v = &w.models()[0];
        assert_eq!(v.tokens_per_s, None, "old run's numbers are gone");
        assert_eq!(
            v.running,
            Some(1),
            "samples after Gone but before the next load are kept"
        );
    }

    #[test]
    fn reset_records_seq_and_keeps_readings() {
        let mut w = World::new();
        let (c1, c2) = (chip(1), chip(2));
        w.apply(&rec(
            1,
            1,
            "s",
            sample(&c1, "tdp", Value::F64(30.0), Provenance::Measured),
        ));
        w.apply(&rec(
            2,
            2,
            "s",
            sample(&c2, "tdp", Value::F64(31.0), Provenance::Measured),
        ));
        assert_eq!(w.reset_seq(&c1), None);
        w.apply(&rec(
            3,
            3,
            "s",
            ev(Event::Reset {
                chips: vec![c1.clone()],
            }),
        ));
        assert_eq!(w.reset_seq(&c1), Some(3));
        assert_eq!(w.reset_seq(&c2), None);
        let r = w.reading(&c1, "tdp").unwrap();
        assert!(
            r.seq < w.reset_seq(&c1).unwrap(),
            "reading predates the reset"
        );
        w.apply(&rec(
            4,
            4,
            "s",
            sample(&c1, "tdp", Value::F64(5.0), Provenance::Measured),
        ));
        let r = w.reading(&c1, "tdp").unwrap();
        assert!(
            r.seq > w.reset_seq(&c1).unwrap(),
            "new reading follows the reset"
        );
        assert_eq!(w.reading(&c2, "tdp").unwrap().value, Value::F64(31.0));
    }

    #[test]
    fn events_are_bounded_and_events_since_filters() {
        let mut w = World::new();
        for q in 1..=(EVENT_WINDOW as u64 + 100) {
            w.apply(&rec(
                q,
                q,
                "s",
                ev(Event::ViewChanged {
                    view: format!("v{q}"),
                }),
            ));
        }
        assert_eq!(w.events().len(), EVENT_WINDOW);
        assert_eq!(w.events().front().unwrap().0, 101);
        let head = EVENT_WINDOW as u64 + 100;
        assert_eq!(
            w.events_since(head - 2).map(|e| e.0).collect::<Vec<_>>(),
            vec![head - 1, head]
        );
        assert_eq!(w.events_since(head).count(), 0);
    }

    #[test]
    fn latest_raw_is_newest_per_source() {
        let mut w = World::new();
        let raw = |q, src: &str, body: &str| {
            rec(
                q,
                q,
                src,
                Payload::Raw(RawFrame {
                    source: src.into(),
                    body: body.into(),
                }),
            )
        };
        assert!(w.latest_raw("tt-smi").is_none());
        w.apply(&raw(1, "tt-smi", "one"));
        w.apply(&raw(2, "other", "x"));
        w.apply(&raw(3, "tt-smi", "two"));
        assert_eq!(w.latest_raw("tt-smi").unwrap().body, "two");
        assert_eq!(w.latest_raw("other").unwrap().body, "x");
    }

    #[test]
    fn entities_lists_by_kind() {
        let mut w = World::new();
        let c = chip(1);
        w.apply(&rec(
            1,
            1,
            "s",
            sample(&c, "a", Value::U64(1), Provenance::Measured),
        ));
        w.apply(&rec(
            2,
            2,
            "s",
            sample(
                &EntityKey::tile(&c, 1, 2),
                "a",
                Value::U64(1),
                Provenance::Measured,
            ),
        ));
        w.apply(&rec(
            3,
            3,
            "s",
            ev(Event::ModelReady {
                model: model(),
                chips: vec![chip(2)],
            }),
        ));
        let chips: Vec<_> = w.entities(EntityKind::Chip).cloned().collect();
        assert_eq!(chips, vec![c.clone(), chip(2)]);
        assert_eq!(w.entities(EntityKind::Tile).count(), 1);
        assert_eq!(w.entities(EntityKind::Model).count(), 1);
        assert_eq!(w.entities(EntityKind::Host).count(), 0);
    }

    #[test]
    fn replayed_log_gives_an_equal_world_and_snapshot_needs_no_segments() {
        let clock = ManualClock::new(1_700_000_000_000);
        let bus = Bus::open_with_world(BusConfig {
            clock: clock.clone(),
            ..BusConfig::default()
        })
        .unwrap();
        let (m, c) = (model(), chip(1));
        bus.publish(
            "tt-smi",
            sample(&c, "tdp", Value::F64(20.0), Provenance::Measured),
        );
        clock.advance_secs(1);
        bus.publish(
            "tt-smi",
            Payload::Raw(RawFrame {
                source: "tt-smi".into(),
                body: "{}".into(),
            }),
        );
        bus.publish(
            "vllm",
            ev(Event::ModelLoading {
                model: m.clone(),
                chips: vec![c.clone()],
                progress: Some(0.5),
            }),
        );
        bus.publish(
            "vllm",
            sample(
                &m,
                signals::MODEL_RUNNING,
                Value::U64(2),
                Provenance::Measured,
            ),
        );
        bus.publish(
            "gozer",
            ev(Event::Reset {
                chips: vec![c.clone()],
            }),
        );
        let snap = bus.snapshot().unwrap();
        assert_eq!(snap.seq, bus.head());
        assert_eq!(snap.world.seq(), snap.seq);
        let mut replay = World::new();
        for r in bus.read_from(1, 1000).unwrap() {
            replay.apply(&r);
        }
        assert_eq!(replay, snap.world);
        // The snapshot is a copy: later records do not change it.
        bus.publish(
            "tt-smi",
            sample(&c, "tdp", Value::F64(99.0), Provenance::Measured),
        );
        assert_eq!(
            snap.world.reading(&c, "tdp").unwrap().value,
            Value::F64(20.0)
        );
        assert_eq!(
            bus.snapshot()
                .unwrap()
                .world
                .reading(&c, "tdp")
                .unwrap()
                .value,
            Value::F64(99.0)
        );
    }

    const SEC: u64 = 1_000_000_000;

    #[test]
    fn churning_entities_are_pruned_and_the_maps_stay_bounded() {
        let mut w = World::new();
        let c = chip(1);
        w.apply(&rec(
            1,
            SEC,
            "s",
            sample(&c, "tdp", Value::F64(1.0), Provenance::Measured),
        ));
        // One new process per second of record time, 10,000 in all.
        for i in 0..10_000u64 {
            let p = EntityKey::process(i as u32, i);
            w.apply(&rec(
                2 + i,
                (2 + i) * SEC,
                "ps",
                sample(&p, "cpu", Value::F64(1.0), Provenance::Measured),
            ));
        }
        // 15 minutes of TTL is 900 s; the sweep runs at most every 60 s.
        let bound = 1_000;
        assert!(w.readings.len() <= bound, "readings {}", w.readings.len());
        assert!(w.known.len() <= bound, "known {}", w.known.len());
        assert_eq!(
            w.readings.len() - 1,
            w.entities(EntityKind::Process).count()
        );
        assert_eq!(w.known.len(), w.readings.len(), "maps agree after a prune");
        assert!(w.touched.len() <= bound);
        // The newest process is still there; an early one is gone.
        assert!(w
            .reading(&EntityKey::process(9_999, 9_999), "cpu")
            .is_some());
        assert!(w.reading(&EntityKey::process(0, 0), "cpu").is_none());
    }

    #[test]
    fn quiet_chip_survives_an_hour_with_growing_age() {
        let mut w = World::new();
        let c = chip(1);
        w.apply(&rec(
            1,
            SEC,
            "s",
            sample(&c, "tdp", Value::F64(42.0), Provenance::Measured),
        ));
        w.apply(&rec(
            2,
            SEC,
            "s",
            sample(
                &EntityKey::host("h"),
                "load",
                Value::F64(1.0),
                Provenance::Measured,
            ),
        ));
        w.apply(&rec(
            3,
            SEC,
            "s",
            sample(
                &EntityKey::tile(&c, 1, 1),
                "a",
                Value::U64(1),
                Provenance::Measured,
            ),
        ));
        // Another source talks for an hour.
        for i in 0..60u64 {
            let p = EntityKey::process(1, 1);
            w.apply(&rec(
                4 + i,
                (60 + i * 60) * SEC,
                "ps",
                sample(&p, "cpu", Value::F64(1.0), Provenance::Measured),
            ));
        }
        let r = w.reading(&c, "tdp").unwrap();
        assert_eq!(r.value, Value::F64(42.0));
        assert!(w.age_ms(&r, 3_600 * SEC) >= 3_500_000);
        assert!(w.reading(&EntityKey::host("h"), "load").is_some());
        assert_eq!(w.entities(EntityKind::Tile).count(), 1);
    }

    #[test]
    fn process_updated_every_minute_is_never_pruned() {
        let mut w = World::new();
        let p = EntityKey::process(7, 7);
        for i in 0..240u64 {
            w.apply(&rec(
                1 + i,
                (i + 1) * 60 * SEC,
                "ps",
                sample(&p, "cpu", Value::U64(i), Provenance::Measured),
            ));
            assert!(w.reading(&p, "cpu").is_some(), "minute {i}");
        }
    }

    #[test]
    fn models_in_view_and_event_mentions_keep_entities_alive() {
        let mut w = World::new();
        let (m, l) = (model(), EntityKey::lease("L1"));
        w.apply(&rec(
            1,
            SEC,
            "s",
            ev(Event::ModelReady {
                model: m.clone(),
                chips: vec![chip(1)],
            }),
        ));
        w.apply(&rec(
            2,
            SEC,
            "v",
            sample(
                &m,
                signals::MODEL_RUNNING,
                Value::U64(2),
                Provenance::Measured,
            ),
        ));
        w.apply(&rec(
            3,
            SEC,
            "g",
            sample(&l, "chips", Value::U64(1), Provenance::Measured),
        ));
        // The lease is mentioned by an event 10 minutes in, then an hour passes.
        w.apply(&rec(
            4,
            600 * SEC,
            "g",
            ev(Event::LeaseGranted {
                lease: l.clone(),
                chips: vec![chip(1)],
            }),
        ));
        w.apply(&rec(
            5,
            1_000 * SEC,
            "x",
            sample(&chip(2), "t", Value::U64(1), Provenance::Measured),
        ));
        assert!(
            w.reading(&l, "chips").is_some(),
            "event mention at 600 s keeps it at 1000 s"
        );
        w.apply(&rec(
            6,
            4_000 * SEC,
            "x",
            sample(&chip(2), "t", Value::U64(2), Provenance::Measured),
        ));
        assert!(w.reading(&l, "chips").is_none());
        // The announced model keeps its view and readings however quiet.
        assert_eq!(w.models()[0].running, Some(2));
        assert!(w.reading(&m, signals::MODEL_RUNNING).is_some());
    }

    #[test]
    fn replay_including_pruning_gives_an_equal_world() {
        let mut recs = Vec::new();
        for i in 0..3_000u64 {
            let p = EntityKey::process((i % 500) as u32, i / 500);
            recs.push(rec(
                1 + i,
                (1 + i) * 2 * SEC,
                "ps",
                sample(&p, "cpu", Value::U64(i), Provenance::Measured),
            ));
        }
        let (mut a, mut b) = (World::new(), World::new());
        for r in &recs {
            a.apply(r);
        }
        for r in &recs {
            b.apply(r);
        }
        assert_eq!(a, b);
        assert!(
            a.readings.len() < 500 * 6,
            "some pruning happened: {}",
            a.readings.len()
        );
    }

    #[test]
    fn bus_without_a_world_has_no_snapshot() {
        let bus = Bus::open(BusConfig::default()).unwrap();
        assert!(bus.snapshot().is_none());
    }
}
