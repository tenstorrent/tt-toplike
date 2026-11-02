//! Record types and the one-line JSON wire format.
//!
//! A [`Record`] is one thing that happened on the bus. The log stores one
//! record per line. Field names are short and stable; changing one is a
//! schema change and needs a new [`SCHEMA_VERSION`].
//!
//! Wire format, one JSON object per line:
//!
//! ```text
//! {"q":7,"m":123456789,"w":1700000000000,"s":"tt-smi","p":{"k":"sample",...}}
//! ```
//!
//! | field | meaning |
//! |-------|---------|
//! | `q`   | sequence number (`seq`) |
//! | `m`   | monotonic time in nanoseconds (`t_mono_ns`) |
//! | `w`   | wall-clock time in milliseconds since the Unix epoch (`t_wall_ms`) |
//! | `s`   | source name |
//! | `p`   | payload, tagged by `k` |
//!
//! Payload kinds (`k`):
//!
//! * `sample`: `e` entity, `n` signal, `v` value, `r` provenance.
//! * `sweep`: `e` entity, `n` signal names, `v` values (same length), `r` provenance.
//! * `event`: `t` event type, plus the event's own fields.
//! * `raw`: `s` source, `b` verbatim body.
//! * `gap`: `why` reason.
//!
//! A value is `{"f":1.5}`, `{"u":3}`, `{"i":-3}` or `{"t":"text"}`.
//! Provenance is `"measured"`, `"derived"`, `"estimated"` or `"decoration"`.
//!
//! A missing reading has no representation: the source emits no sample.
//! JSON cannot carry NaN or infinity, so a non-finite `F64` is not a valid
//! value. Sources must not emit one. [`crate::Bus::publish`] enforces it by
//! running every payload through [`Payload::sanitize`], which drops
//! non-finite readings and rejects malformed sweeps, so neither reaches the
//! log.
//!
//! `serde_json` escapes every control character inside strings, so a line
//! never contains a raw newline even when text or a raw body does.

use crate::entity::EntityKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Version of the wire format described above.
pub const SCHEMA_VERSION: u32 = 1;

/// How trustworthy a value is. Every moving thing in a view carries one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Read from hardware or a real service.
    Measured,
    /// Computed from measured values.
    Derived,
    /// A labelled guess.
    Estimated,
    /// Named decoration with no data behind it.
    Decoration,
}

/// A reading. There is no "none": a missing reading is no sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    #[serde(rename = "f")]
    F64(f64),
    #[serde(rename = "u")]
    U64(u64),
    #[serde(rename = "i")]
    I64(i64),
    #[serde(rename = "t")]
    Text(String),
}

impl Value {
    /// False only for an `F64` that is NaN or infinite.
    pub fn is_finite(&self) -> bool {
        match self {
            Value::F64(x) => x.is_finite(),
            _ => true,
        }
    }
}

/// One reading of one signal on one entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    #[serde(rename = "e")]
    pub entity: EntityKey,
    #[serde(rename = "n")]
    pub signal: String,
    #[serde(rename = "v")]
    pub value: Value,
    #[serde(rename = "r")]
    pub prov: Provenance,
}

/// Many readings of one entity in one record (one chip per tick, or one tile
/// sweep). `signals[i]` names `values[i]`, so both vectors must have the same
/// length. [`Payload::sanitize`] enforces that rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sweep {
    #[serde(rename = "e")]
    pub entity: EntityKey,
    #[serde(rename = "n")]
    pub signals: Vec<String>,
    #[serde(rename = "v")]
    pub values: Vec<Value>,
    #[serde(rename = "r")]
    pub prov: Provenance,
}

/// A discrete thing that happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Event {
    ModelLoading {
        model: EntityKey,
        chips: Vec<EntityKey>,
        progress: Option<f32>,
    },
    ModelReady {
        model: EntityKey,
        chips: Vec<EntityKey>,
    },
    ModelGone {
        model: EntityKey,
    },
    RequestsCompleted {
        model: EntityKey,
        n: u32,
    },
    KernelCompiled {
        chip: Option<EntityKey>,
        source: String,
    },
    Reset {
        chips: Vec<EntityKey>,
    },
    LinkChanged {
        chip: EntityKey,
        mask: u16,
    },
    LeaseGranted {
        lease: EntityKey,
        chips: Vec<EntityKey>,
    },
    LeaseReleased {
        lease: EntityKey,
        chips: Vec<EntityKey>,
    },
    EccTick {
        chip: EntityKey,
        channel: u8,
    },
    /// The name of the view now on screen.
    ViewChanged {
        view: String,
    },
    Spark {
        chip: Option<EntityKey>,
        kind: String,
    },
}

/// A verbatim snapshot from a source (the tt-smi JSON), kept so a replay can
/// feed it back through the same parser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawFrame {
    #[serde(rename = "s")]
    pub source: String,
    #[serde(rename = "b")]
    pub body: String,
}

/// A marker that readings are missing for a stretch, with the reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    #[serde(rename = "why")]
    pub reason: String,
}

/// The content of a record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "lowercase")]
pub enum Payload {
    Sample(Sample),
    Sweep(Sweep),
    Event(Event),
    Raw(RawFrame),
    Gap(Gap),
}

impl Payload {
    /// Make a payload safe to log. A sample with a non-finite value, a sweep
    /// whose `signals` and `values` differ in length, and a sweep with no
    /// finite pair left all return `None`. A sweep with some non-finite
    /// values keeps only its finite (signal, value) pairs. Everything else is
    /// returned unchanged.
    pub fn sanitize(self) -> Option<Payload> {
        match self {
            Payload::Sample(s) => s.value.is_finite().then_some(Payload::Sample(s)),
            Payload::Sweep(mut s) => {
                if s.signals.len() != s.values.len() {
                    return None;
                }
                if s.values.iter().all(Value::is_finite) {
                    return (!s.values.is_empty()).then_some(Payload::Sweep(s));
                }
                let (signals, values): (Vec<_>, Vec<_>) = s
                    .signals
                    .drain(..)
                    .zip(s.values.drain(..))
                    .filter(|(_, v)| v.is_finite())
                    .unzip();
                if values.is_empty() {
                    return None;
                }
                s.signals = signals;
                s.values = values;
                Some(Payload::Sweep(s))
            }
            other => Some(other),
        }
    }
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(rename = "q")]
    pub seq: u64,
    #[serde(rename = "m")]
    pub t_mono_ns: u64,
    #[serde(rename = "w")]
    pub t_wall_ms: u64,
    #[serde(rename = "s")]
    pub source: String,
    #[serde(rename = "p")]
    pub payload: Payload,
}

/// Why a line did not decode.
#[derive(Debug)]
pub struct DecodeError(String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot decode record: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

impl Record {
    /// Encode as one JSON line with no trailing newline and no newline inside.
    pub fn to_line(&self) -> String {
        // Every field is plain data with string keys, so encoding cannot fail.
        serde_json::to_string(self).expect("record encodes as JSON")
    }

    /// Decode one line. A truncated or malformed line is an error.
    pub fn from_line(line: &str) -> Result<Record, DecodeError> {
        serde_json::from_str(line).map_err(|e| DecodeError(e.to_string()))
    }
}

/// Whether a log came from a live run or is a replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeaderKind {
    Live,
    Replay,
}

/// First line of a log: the schema version, whether it is live or a replay, and
/// the `tool_versions` map. The map holds this crate's version and the tags the
/// host adds: `backend` (`hybrid`, `sysfs`, `mock`, ...) and `mode` (`live`,
/// `mock`, `replay`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub schema: u32,
    pub kind: HeaderKind,
    pub tool_versions: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntityKey;

    fn chip() -> EntityKey {
        EntityKey::chip("0000:03:00.0")
    }

    /// A float read back from a line is the float that was written, to the
    /// bit, in a sample and in a sweep. The values are f32 readings widened
    /// to f64, as the backend source writes them; serde_json's default
    /// parser reads some of them one ulp off once they pass through the
    /// buffered (internally tagged) payload, which a recorded mock run hit.
    #[test]
    fn f64_values_round_trip_bit_for_bit() {
        let mut bad = Vec::new();
        for i in 0..20_000u32 {
            let x = (i as f32 * 0.012_345_6 + 0.5) as f64;
            for p in [
                Payload::Sample(Sample {
                    entity: chip(),
                    signal: "s".into(),
                    value: Value::F64(x),
                    prov: Provenance::Measured,
                }),
                Payload::Sweep(Sweep {
                    entity: chip(),
                    signals: vec!["s".into()],
                    values: vec![Value::F64(x)],
                    prov: Provenance::Measured,
                }),
            ] {
                let back = Record::from_line(&rec(p.clone()).to_line()).unwrap();
                if back.payload != p {
                    bad.push(x);
                }
            }
        }
        assert!(
            bad.is_empty(),
            "{} values changed, first {:?}",
            bad.len(),
            &bad[..bad.len().min(3)]
        );
    }

    fn rec(payload: Payload) -> Record {
        Record {
            seq: 7,
            t_mono_ns: 123_456_789,
            t_wall_ms: 1_700_000_000_000,
            source: "test".into(),
            payload,
        }
    }

    fn all_payloads() -> Vec<Payload> {
        let c = chip();
        let m = EntityKey::model("llama");
        vec![
            Payload::Sample(Sample {
                entity: c.clone(),
                signal: "tt_asic_temperature_celsius".into(),
                value: Value::F64(61.5),
                prov: Provenance::Measured,
            }),
            Payload::Sample(Sample {
                entity: c.clone(),
                signal: "phase".into(),
                value: Value::Text("ready".into()),
                prov: Provenance::Derived,
            }),
            Payload::Sample(Sample {
                entity: c.clone(),
                signal: "n".into(),
                value: Value::U64(u64::MAX),
                prov: Provenance::Estimated,
            }),
            Payload::Sample(Sample {
                entity: c.clone(),
                signal: "d".into(),
                value: Value::I64(i64::MIN),
                prov: Provenance::Decoration,
            }),
            Payload::Sweep(Sweep {
                entity: c.clone(),
                signals: vec!["a".into(), "b".into()],
                values: vec![Value::U64(1), Value::F64(2.5)],
                prov: Provenance::Measured,
            }),
            Payload::Event(Event::ModelLoading {
                model: m.clone(),
                chips: vec![c.clone()],
                progress: Some(0.5),
            }),
            Payload::Event(Event::ModelLoading {
                model: m.clone(),
                chips: vec![],
                progress: None,
            }),
            Payload::Event(Event::ModelReady {
                model: m.clone(),
                chips: vec![c.clone()],
            }),
            Payload::Event(Event::ModelGone { model: m.clone() }),
            Payload::Event(Event::RequestsCompleted {
                model: m.clone(),
                n: 3,
            }),
            Payload::Event(Event::KernelCompiled {
                chip: Some(c.clone()),
                source: "x".into(),
            }),
            Payload::Event(Event::KernelCompiled {
                chip: None,
                source: "x".into(),
            }),
            Payload::Event(Event::Reset {
                chips: vec![c.clone()],
            }),
            Payload::Event(Event::LinkChanged {
                chip: c.clone(),
                mask: 0xBEEF,
            }),
            Payload::Event(Event::LeaseGranted {
                lease: EntityKey::lease("L1"),
                chips: vec![c.clone()],
            }),
            Payload::Event(Event::LeaseReleased {
                lease: EntityKey::lease("L1"),
                chips: vec![c.clone()],
            }),
            Payload::Event(Event::EccTick {
                chip: c.clone(),
                channel: 2,
            }),
            Payload::Event(Event::ViewChanged {
                view: "die3d".into(),
            }),
            Payload::Event(Event::Spark {
                chip: None,
                kind: "boot".into(),
            }),
            Payload::Raw(RawFrame {
                source: "tt-smi".into(),
                body: "{\"a\":1}".into(),
            }),
            Payload::Gap(Gap {
                reason: "reset".into(),
            }),
        ]
    }

    #[test]
    fn every_payload_round_trips() {
        for p in all_payloads() {
            let r = rec(p);
            let line = r.to_line();
            let back = Record::from_line(&line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(r, back, "line: {line}");
        }
    }

    #[test]
    fn line_never_contains_a_newline() {
        let nasty = "a\nb\r\nc\u{2028}d";
        let r1 = rec(Payload::Sample(Sample {
            entity: chip(),
            signal: "s".into(),
            value: Value::Text(nasty.into()),
            prov: Provenance::Measured,
        }));
        let r2 = rec(Payload::Raw(RawFrame {
            source: "tt-smi".into(),
            body: format!("{nasty}\n{{\n}}\n"),
        }));
        for r in [r1, r2] {
            let line = r.to_line();
            assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
            assert_eq!(Record::from_line(&line).unwrap(), r);
        }
    }

    /// Raw bodies with an embedded NUL byte and with a lone-surrogate JSON
    /// escape (the six characters `\ud800`, which is valid text but not a
    /// valid character once decoded) survive the on-disk line format
    /// unchanged, and the line holds no raw NUL.
    #[test]
    fn awkward_raw_bodies_round_trip_through_a_line() {
        for body in [
            "{\"a\":\"x\0y\"}\0tail".to_string(),
            r#"{"s":"\ud800","t":"\udfff\ud83d"}"#.to_string(),
            "\0".to_string(),
            "\\u0000 and \0 together".to_string(),
        ] {
            let r = rec(Payload::Raw(RawFrame {
                source: "tt-smi".into(),
                body: body.clone(),
            }));
            let line = r.to_line();
            assert!(!line.contains('\0'), "{line:?}");
            let back = Record::from_line(&line).unwrap();
            match back.payload {
                Payload::Raw(f) => assert_eq!(f.body, body),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn truncated_line_is_an_error() {
        let line = rec(Payload::Gap(Gap { reason: "x".into() })).to_line();
        for cut in [1, line.len() / 2, line.len() - 1] {
            assert!(Record::from_line(&line[..cut]).is_err(), "cut {cut}");
        }
        assert!(Record::from_line("").is_err());
    }

    #[test]
    fn tile_key_includes_chip_and_both_coordinates() {
        let t = EntityKey::tile(&chip(), 18, 25);
        let s = t.as_str();
        assert!(s.contains(chip().as_str()), "{s}");
        assert!(s.contains("18") && s.contains("25"), "{s}");
        assert_ne!(t, EntityKey::tile(&chip(), 25, 18));
        assert_ne!(t, EntityKey::tile(&EntityKey::chip("0000:04:00.0"), 18, 25));
    }

    #[test]
    fn missing_reading_has_no_representation() {
        // Value has exactly four variants; this match fails to compile if a
        // "none" variant is ever added.
        fn exhaustive(v: &Value) -> u8 {
            match v {
                Value::F64(_) => 0,
                Value::U64(_) => 1,
                Value::I64(_) => 2,
                Value::Text(_) => 3,
            }
        }
        assert_eq!(exhaustive(&Value::U64(0)), 1);
        // And a JSON null is not a value.
        assert!(serde_json::from_str::<Value>("null").is_err());
    }

    #[test]
    fn header_round_trips_and_schema_is_one() {
        assert_eq!(SCHEMA_VERSION, 1);
        let mut v = std::collections::BTreeMap::new();
        v.insert("tt-smi".to_string(), "6.3.0".to_string());
        let h = Header {
            schema: SCHEMA_VERSION,
            kind: HeaderKind::Replay,
            tool_versions: v,
        };
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<Header>(&s).unwrap(), h);
    }

    #[test]
    fn sanitize_drops_non_finite_sample() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let p = Payload::Sample(Sample {
                entity: chip(),
                signal: "tt_tdp_w".into(),
                value: Value::F64(bad),
                prov: Provenance::Measured,
            });
            assert_eq!(p.sanitize(), None);
        }
    }

    #[test]
    fn sanitize_keeps_finite_pairs_aligned() {
        let p = Payload::Sweep(Sweep {
            entity: chip(),
            signals: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            values: vec![
                Value::F64(1.0),
                Value::F64(f64::INFINITY),
                Value::U64(3),
                Value::F64(f64::NAN),
            ],
            prov: Provenance::Measured,
        });
        match p.sanitize() {
            Some(Payload::Sweep(s)) => {
                assert_eq!(s.signals, vec!["a".to_string(), "c".to_string()]);
                assert_eq!(s.values, vec![Value::F64(1.0), Value::U64(3)]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sanitize_rejects_mismatched_and_all_bad_sweeps() {
        let mismatched = Payload::Sweep(Sweep {
            entity: chip(),
            signals: vec!["a".into(), "b".into()],
            values: vec![Value::U64(1)],
            prov: Provenance::Measured,
        });
        assert_eq!(mismatched.sanitize(), None);
        let all_bad = Payload::Sweep(Sweep {
            entity: chip(),
            signals: vec!["a".into()],
            values: vec![Value::F64(f64::NAN)],
            prov: Provenance::Measured,
        });
        assert_eq!(all_bad.sanitize(), None);
    }

    #[test]
    fn sanitize_leaves_finite_payloads_unchanged() {
        for p in all_payloads() {
            assert_eq!(p.clone().sanitize(), Some(p));
        }
    }
}
