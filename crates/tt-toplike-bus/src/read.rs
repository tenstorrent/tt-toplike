//! [`WorldRead`]: the read interface a view uses, whether the world is local
//! or read from another process's socket.
//!
//! A view (Die 3D, the `i` inference monitor) takes a `&dyn WorldRead` and
//! does not care where the world lives. Two types implement it:
//!
//! * [`LocalView`]: a copy of a local [`World`] with the bus's clock reading
//!   and header kind. A `World` alone has no clock (ages need "now") and no
//!   header (live or replay), so the trait is implemented on this pairing.
//! * [`crate::RemoteView`] (Unix only): the summary a socket client keeps
//!   current ([`crate::RemoteWorld`]).
//!
//! Both give the same answers for the same records. The one built-in limit
//! is the event window: a summary carries the newest
//! [`SUMMARY_EVENTS`](crate::SUMMARY_EVENTS) events, so
//! [`WorldRead::events_since`] returns at most that many on BOTH
//! implementations. A local world keeps more ([`crate::world::EVENT_WINDOW`]),
//! and code that needs them reads the `World` directly.
//!
//! The equivalence is tested on one fixture: a `World` folded from records
//! and a `RemoteView` decoded from the JSON snapshot of the same world must
//! answer every method alike (see the tests below). When they differ, the
//! summary is missing something, and the fix is to add it to the summary.

use crate::entity::EntityKey;
use crate::record::{Event, HeaderKind};
use crate::summary::SUMMARY_EVENTS;
use crate::world::{ModelView, Reading, World};

/// What a view reads from the world. See the module doc.
///
/// Every method is cheap enough to call a few times per frame on a world of
/// the size the bus holds (dozens of entities); none of them takes a lock.
pub trait WorldRead: Send + Sync {
    /// Sequence number of the last record this world holds.
    fn seq(&self) -> u64;

    /// The announced models, in key order.
    fn models(&self) -> Vec<ModelView>;

    /// The model announced on `chip`. With two, the first by key.
    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView>;

    /// The latest reading of `signal` on `entity`, with its provenance,
    /// sequence number and record time.
    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading>;

    /// How old `r` is at `now_mono_ns`, in milliseconds. A `now` before the
    /// reading gives 0.
    fn age_ms(&self, r: &Reading, now_mono_ns: u64) -> u64 {
        now_mono_ns.saturating_sub(r.t_mono_ns) / 1_000_000
    }

    /// Events with a sequence number above `seq`, oldest first, from the
    /// newest [`SUMMARY_EVENTS`] events.
    fn events_since(&self, seq: u64) -> Vec<(u64, Event)>;

    /// Sequence number of the latest `Reset` that named `chip`.
    fn reset_seq(&self, chip: &EntityKey) -> Option<u64>;

    /// `Live`, or `Replay` when the bus republishes a recorded log.
    fn header_kind(&self) -> HeaderKind;

    /// The bus clock's monotonic time (ns) to take ages at: the scale every
    /// reading's `t_mono_ns` is on.
    fn now_mono_ns(&self) -> u64;
}

/// A local world with the clock reading and header kind a view needs.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalView {
    /// The sequence number the world is current to (the bus head when the
    /// copy was taken).
    pub seq: u64,
    pub world: World,
    pub now_mono_ns: u64,
    pub header_kind: HeaderKind,
}

impl WorldRead for LocalView {
    fn seq(&self) -> u64 {
        self.seq
    }

    fn models(&self) -> Vec<ModelView> {
        self.world.models()
    }

    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView> {
        self.world.model_on_chip(chip).cloned()
    }

    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading> {
        self.world.reading(entity, signal)
    }

    fn events_since(&self, seq: u64) -> Vec<(u64, Event)> {
        // The same window a summary carries, so local and attached views
        // see the same events.
        let all = self.world.events();
        let skip = all.len().saturating_sub(SUMMARY_EVENTS);
        all.iter()
            .skip(skip)
            .filter(|(q, _)| *q > seq)
            .cloned()
            .collect()
    }

    fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.world.reset_seq(chip)
    }

    fn header_kind(&self) -> HeaderKind {
        self.header_kind
    }

    fn now_mono_ns(&self) -> u64 {
        self.now_mono_ns
    }
}

#[cfg(unix)]
impl WorldRead for crate::client::RemoteView {
    fn seq(&self) -> u64 {
        self.seq
    }

    fn models(&self) -> Vec<ModelView> {
        self.world.models.clone()
    }

    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView> {
        // `models` is in key order, as `World`'s map is.
        self.world
            .models
            .iter()
            .find(|m| m.chips.contains(chip))
            .cloned()
    }

    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading> {
        self.world.reading(entity, signal).map(|r| Reading {
            value: r.value.clone(),
            prov: r.prov,
            seq: r.seq,
            t_mono_ns: r.t_mono_ns,
        })
    }

    fn events_since(&self, seq: u64) -> Vec<(u64, Event)> {
        let all = &self.world.events;
        let skip = all.len().saturating_sub(SUMMARY_EVENTS);
        all.iter()
            .skip(skip)
            .filter(|e| e.seq > seq)
            .map(|e| (e.seq, e.event.clone()))
            .collect()
    }

    fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.world
            .resets
            .binary_search_by(|r| r.chip.cmp(chip))
            .ok()
            .map(|i| self.world.resets[i].seq)
    }

    fn header_kind(&self) -> HeaderKind {
        self.header_kind
    }

    fn now_mono_ns(&self) -> u64 {
        self.world.now_mono_ns
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::RemoteView;
    use crate::record::{Payload, Provenance, Record, Sample, Sweep, Value};
    use crate::signals;
    use crate::summary::{RemoteSnapshot, WorldSummary};

    const FIXTURE: &str = include_str!("../tests/fixtures/bringup.jsonl");

    fn chip(n: u8) -> EntityKey {
        EntityKey::chip(&format!("0000:0{n}:00.0"))
    }

    /// The bring-up fixture, then records that reach the branches the
    /// fixture does not: a sweep, a model that goes away, readings of every
    /// provenance, and more events than a summary keeps.
    fn fixture_records() -> Vec<Record> {
        let mut recs: Vec<Record> = FIXTURE
            .lines()
            .filter_map(|l| Record::from_line(l).ok())
            .collect();
        let mut seq = recs.last().map_or(0, |r| r.seq);
        let mut t = recs.last().map_or(0, |r| r.t_mono_ns);
        let mut push = |payload: Payload| {
            seq += 1;
            t += 7_000_000;
            recs.push(Record {
                seq,
                t_mono_ns: t,
                t_wall_ms: 1_790_000_100_000 + seq,
                source: "extra".into(),
                payload,
            });
        };
        push(Payload::Sweep(Sweep {
            entity: chip(3),
            signals: vec!["chip.temp_c".into(), "chip.aiclk_mhz".into()],
            values: vec![Value::F64(61.25), Value::U64(1350)],
            prov: Provenance::Derived,
        }));
        for (i, prov) in [
            Provenance::Measured,
            Provenance::Derived,
            Provenance::Estimated,
            Provenance::Decoration,
        ]
        .into_iter()
        .enumerate()
        {
            push(Payload::Sample(Sample {
                entity: chip(1),
                signal: format!("x.{i}"),
                value: Value::I64(-(i as i64)),
                prov,
            }));
        }
        let gone = EntityKey::model("gone");
        push(Payload::Event(Event::ModelReady {
            model: gone.clone(),
            chips: vec![chip(3)],
        }));
        push(Payload::Sample(Sample {
            entity: gone.clone(),
            signal: signals::MODEL_KV_CACHE.into(),
            value: Value::F64(0.1),
            prov: Provenance::Measured,
        }));
        push(Payload::Event(Event::ModelGone { model: gone }));
        for i in 0..70u8 {
            push(Payload::Event(Event::Reset {
                chips: vec![chip(1 + i % 3)],
            }));
        }
        push(Payload::Sample(Sample {
            entity: chip(2),
            signal: "chip.temp_c".into(),
            value: Value::F64(0.1 + 0.2),
            prov: Provenance::Measured,
        }));
        recs
    }

    /// The fixture as a local view and as a remote view decoded from the
    /// JSON snapshot line a server would send.
    fn both() -> (LocalView, RemoteView, Vec<Record>) {
        let recs = fixture_records();
        let mut world = World::new();
        for r in &recs {
            world.apply(r);
        }
        let now = recs.last().unwrap().t_mono_ns + 2_500_000_000;
        let local = LocalView {
            seq: world.seq(),
            world: world.clone(),
            now_mono_ns: now,
            header_kind: HeaderKind::Replay,
        };
        let line = serde_json::to_string(&RemoteSnapshot {
            seq: world.seq(),
            instance: 7,
            kind: HeaderKind::Replay,
            world: WorldSummary::from_world(&world, now),
        })
        .unwrap();
        let remote = RemoteView::from(serde_json::from_str::<RemoteSnapshot>(&line).unwrap());
        (local, remote, recs)
    }

    /// Every method answers alike on a local and a remote view of the same
    /// world, for every entity and signal in the fixture.
    #[test]
    fn local_and_remote_views_answer_alike() {
        let (local, remote, recs) = both();
        let views: [&dyn WorldRead; 2] = [&local, &remote];
        let ask = |v: &dyn WorldRead| {
            let mut out = Vec::new();
            out.push(format!("seq {}", v.seq()));
            out.push(format!("kind {:?}", v.header_kind()));
            out.push(format!("now {}", v.now_mono_ns()));
            out.push(format!("models {:?}", v.models()));
            let mut entities: Vec<EntityKey> = Vec::new();
            let mut signals: Vec<String> = Vec::new();
            for r in &recs {
                match &r.payload {
                    Payload::Sample(s) => {
                        entities.push(s.entity.clone());
                        signals.push(s.signal.clone());
                    }
                    Payload::Sweep(s) => {
                        entities.push(s.entity.clone());
                        signals.extend(s.signals.iter().cloned());
                    }
                    _ => {}
                }
            }
            entities.extend((1..=4).map(chip));
            entities.push(EntityKey::model("nowhere"));
            entities.sort();
            entities.dedup();
            signals.push("no.such.signal".into());
            signals.sort();
            signals.dedup();
            for e in &entities {
                out.push(format!("on {e}: {:?}", v.model_on_chip(e)));
                out.push(format!("reset {e}: {:?}", v.reset_seq(e)));
                for s in &signals {
                    let r = v.reading(e, s);
                    let age = r.as_ref().map(|r| v.age_ms(r, v.now_mono_ns()));
                    out.push(format!("{e} {s}: {r:?} age {age:?}"));
                }
            }
            for q in [0, 1, 20, 60, 100, v.seq().saturating_sub(3), v.seq()] {
                out.push(format!("since {q}: {:?}", v.events_since(q)));
            }
            out
        };
        let a = ask(views[0]);
        let b = ask(views[1]);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x, y);
        }
        // The fixture reaches what it is meant to.
        assert!(local.world.events().len() > SUMMARY_EVENTS);
        assert_eq!(local.events_since(0).len(), SUMMARY_EVENTS);
        assert!(local.models().iter().any(|m| m.label == "Llama 3.1 8B"));
        assert!(local.model_on_chip(&chip(1)).is_some());
        assert_eq!(
            local.reading(&chip(2), "chip.temp_c").unwrap().value,
            Value::F64(0.1 + 0.2)
        );
        assert_eq!(
            local.reading(&chip(1), "x.3").unwrap().prov,
            Provenance::Decoration
        );
        assert_eq!(remote.header_kind(), HeaderKind::Replay);
    }

    /// A reading's age grows with `now` and never goes below 0.
    #[test]
    fn age_is_taken_at_the_given_now() {
        let (local, remote, _) = both();
        for v in [&local as &dyn WorldRead, &remote] {
            let r = v.reading(&chip(2), "chip.temp_c").unwrap();
            assert_eq!(v.age_ms(&r, r.t_mono_ns + 1_500_000_000), 1_500);
            assert_eq!(v.age_ms(&r, 0), 0);
        }
    }
}
