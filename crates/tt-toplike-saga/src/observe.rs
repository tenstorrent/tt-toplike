//! Observations: what the saga core sees of the bus, and its clock.
//!
//! The core does not read bus records directly. A [`Feed`] turns each record
//! into zero or more [`Observation`]s. It looks only at the record's source
//! and payload. `seq`, `t_mono_ns` and `t_wall_ms` are never read, so the same
//! ordered stream of record contents gives the same observations however the
//! records were numbered or timed (a live run and its replay agree).
//!
//! The clock is the pacemaker sweep. The first sweep entity the feed ever sees
//! is the pacemaker, and each of its sweeps ends with one
//! [`Observation::Tick`], after that sweep's samples. If the pacemaker is quiet
//! for [`PACEMAKER_TIMEOUT`] records, any sweep ticks until the pacemaker
//! sweeps again.

use tt_toplike_bus::{EntityKey, Event, Payload, Record, Value};

/// Source name the saga core uses for the records it publishes itself.
/// The feed skips them so the core never reacts to its own output.
const SAGA_SOURCE: &str = "saga";

/// Number of records that may pass without a pacemaker sweep before any sweep
/// counts as a tick.
pub const PACEMAKER_TIMEOUT: u64 = 2000;

/// One thing the core observes.
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// One numeric reading, from a sample or from one pair of a sweep.
    Sample {
        entity: EntityKey,
        signal: String,
        value: f64,
    },
    /// A bus event, passed through unchanged.
    Event(Event),
    /// One step of the core's clock.
    Tick,
}

/// Turns records into observations and keeps the sweep clock.
#[derive(Debug, Clone, Default)]
pub struct Feed {
    /// Records pushed so far, not counting records from source "saga".
    ordinal: u64,
    /// The first sweep entity ever seen.
    pacemaker: Option<EntityKey>,
    /// The ordinal of the pacemaker's latest sweep.
    last_pacemaker_sweep: u64,
}

/// A number from a bus value, or `None` for text and non-finite floats.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::F64(x) if x.is_finite() => Some(*x),
        Value::F64(_) => None,
        Value::U64(x) => Some(*x as f64),
        Value::I64(x) => Some(*x as f64),
        Value::Text(_) => None,
    }
}

impl Feed {
    /// A feed that has seen nothing.
    pub fn new() -> Feed {
        Feed::default()
    }

    /// The number of records pushed so far, not counting records from source
    /// "saga". A replayed log that carries the saga's own events therefore
    /// gives the same ordinals the live run had. Every payload kind counts,
    /// including raw frames and gaps.
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Appends the observations for `rec` to `out`.
    pub fn push(&mut self, rec: &Record, out: &mut Vec<Observation>) {
        if rec.source == SAGA_SOURCE {
            return;
        }
        self.ordinal += 1;
        match &rec.payload {
            Payload::Sample(s) => {
                if let Some(value) = number(&s.value) {
                    out.push(Observation::Sample {
                        entity: s.entity.clone(),
                        signal: s.signal.clone(),
                        value,
                    });
                }
            }
            Payload::Sweep(sweep) => {
                for (signal, value) in sweep.signals.iter().zip(&sweep.values) {
                    if let Some(value) = number(value) {
                        out.push(Observation::Sample {
                            entity: sweep.entity.clone(),
                            signal: signal.clone(),
                            value,
                        });
                    }
                }
                if self.sweep_ticks(&sweep.entity) {
                    out.push(Observation::Tick);
                }
            }
            Payload::Event(e) => out.push(Observation::Event(e.clone())),
            Payload::Raw(_) | Payload::Gap(_) => {}
        }
    }

    /// Decides whether a sweep from `entity` is a tick, and updates the
    /// pacemaker state. The first sweep entity becomes the pacemaker.
    fn sweep_ticks(&mut self, entity: &EntityKey) -> bool {
        let pacemaker = self.pacemaker.get_or_insert_with(|| entity.clone());
        if pacemaker == entity {
            self.last_pacemaker_sweep = self.ordinal;
            return true;
        }
        // Records that passed since the pacemaker's last sweep, not counting
        // this one.
        let quiet = self.ordinal - self.last_pacemaker_sweep - 1;
        quiet >= PACEMAKER_TIMEOUT
    }
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod observe_tests;
