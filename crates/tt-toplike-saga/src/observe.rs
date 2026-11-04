//! Observations: what the saga core sees of the bus, and its clock.
//!
//! The core does not read bus records directly. A [`Feed`] turns each record
//! into zero or more [`Observation`]s. It looks only at the record's source
//! and payload. It skips the saga's own records and the records that
//! describe the host process (`ViewChanged` events and gaps). `seq`, `t_mono_ns` and `t_wall_ms` are never read, so the same
//! ordered stream of record contents gives the same observations however the
//! records were numbered or timed (a live run and its replay agree).
//!
//! The clock is the pacemaker sweep. The first chip whose sweep the feed
//! sees is the pacemaker, and each of its sweeps ends with one
//! [`Observation::Tick`], after that sweep's samples. Only a chip can be the
//! pacemaker: the backend sweeps every chip once per poll, while a process
//! scan sweeps each process only every 2 s, and a process comes and goes.
//!
//! The timeout is counted in rounds, never in time. A round without the
//! pacemaker is noticed when some other chip sweeps a second time since the
//! pacemaker last swept (a backend poll sweeps each chip once). After
//! [`PACEMAKER_TIMEOUT_ROUNDS`] such rounds, the chip whose sweep completes
//! the last of them becomes the pacemaker and ticks. So when the pacemaker's
//! chip goes away, or a backend switch changes every chip key, the clock
//! moves to another chip and still ticks once per poll. Before the final fix
//! wave the first sweep of any kind became the pacemaker for good, and once
//! it had been quiet for 2,000 records every sweep ticked (final review
//! finding I2).

use std::collections::BTreeSet;

use tt_toplike_bus::{EntityKey, EntityKind, Event, Payload, Record, Value};

/// Source name the saga core uses for the records it publishes itself.
/// The feed skips them so the core never reacts to its own output.
const SAGA_SOURCE: &str = "saga";

/// Rounds of chip sweeps that may pass without the pacemaker before another
/// chip takes over the clock (see the module comment). Three rounds are
/// 0.3 s at the default 10 Hz poll.
pub const PACEMAKER_TIMEOUT_ROUNDS: u64 = 3;

/// The most chips the feed remembers within one round. A round that reaches
/// it counts as missed, as a repeated chip does, so the set stays small
/// whatever a local writer sends. It is the saga's own chip cap
/// ([`crate::world::MAX_CHIPS`]); on a box with more chips than three times
/// this, a pacemaker that still sweeps can lose the clock to another chip.
/// The clock then still ticks about once per poll.
const MAX_ROUND_CHIPS: usize = 64;

/// The longest text, in UTF-8 bytes, that a record may carry into the saga
/// (ledger ruling R21). It applies to every entity key (the whole key, kind
/// prefix included), every signal name, and every string field of an event
/// (`KernelCompiled.source`, `Spark.kind`, `ViewChanged.view`, and every key
/// an event names) and a gap's reason. A record with a longer item is
/// dropped whole and counted in [`Feed::oversized`].
///
/// The caps on the saga's collections count items; this caps each item's
/// size. A bus line may be 16 MB, and the saga stores keys and names in
/// several places (the ready list, trails, beat causes), so without it a
/// few hostile records could hold gigabytes.
///
/// Not checked: a text sample value (the feed reads no text value), a raw
/// frame (the feed reads only that it is there), and the record's source
/// (compared and never stored).
pub const MAX_ITEM_BYTES: usize = 256;

/// The prefix of the saga's own derived series (`saga.heartbeat_alive`,
/// `saga.temperature_baseline`). A sample or sweep from another source that
/// carries a signal with this prefix is dropped whole and counted in
/// [`Feed::rejected`], so no outside writer can set the hero's vitality
/// inputs directly. The saga pushes its derived series itself.
pub const SAGA_SIGNAL_PREFIX: &str = "saga.";

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
    /// The chip whose sweeps tick (see the module comment).
    pacemaker: Option<EntityKey>,
    /// Other chips that have swept since the pacemaker last swept, in the
    /// current round. At most [`MAX_ROUND_CHIPS`].
    round: BTreeSet<EntityKey>,
    /// Rounds that have passed without a pacemaker sweep.
    missed_rounds: u64,
    /// Records dropped for an item longer than [`MAX_ITEM_BYTES`].
    oversized: u64,
    /// Records dropped for a signal named with [`SAGA_SIGNAL_PREFIX`].
    rejected: u64,
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

/// Whether `s` is at most [`MAX_ITEM_BYTES`] long.
fn short(s: &str) -> bool {
    s.len() <= MAX_ITEM_BYTES
}

/// Whether a key is at most [`MAX_ITEM_BYTES`] long, kind prefix included.
fn short_key(k: &EntityKey) -> bool {
    short(k.as_str())
}

/// Whether every key in a list is short.
fn short_keys(ks: &[EntityKey]) -> bool {
    ks.iter().all(short_key)
}

/// Whether every key, name and string a payload carries into the saga is at
/// most [`MAX_ITEM_BYTES`] long. Each event variant is listed, so a new one
/// does not compile until its fields are checked.
fn fits(payload: &Payload) -> bool {
    match payload {
        Payload::Sample(s) => short_key(&s.entity) && short(&s.signal),
        Payload::Sweep(s) => short_key(&s.entity) && s.signals.iter().all(|n| short(n)),
        Payload::Event(e) => match e {
            Event::ModelLoading { model, chips, .. } => short_key(model) && short_keys(chips),
            Event::ModelReady { model, chips } => short_key(model) && short_keys(chips),
            Event::ModelGone { model } => short_key(model),
            Event::RequestsCompleted { model, .. } => short_key(model),
            Event::KernelCompiled { chip, source } => {
                chip.as_ref().is_none_or(short_key) && short(source)
            }
            Event::Reset { chips } => short_keys(chips),
            Event::LinkChanged { chip, .. } => short_key(chip),
            Event::LinkPeer { port, peer } => short_key(port) && short_key(peer),
            Event::LeaseGranted { lease, chips } => short_key(lease) && short_keys(chips),
            Event::LeaseReleased { lease, chips } => short_key(lease) && short_keys(chips),
            Event::EccTick { chip, .. } => short_key(chip),
            Event::ViewChanged { view } => short(view),
            Event::Spark { chip, kind } => chip.as_ref().is_none_or(short_key) && short(kind),
        },
        // The feed reads only that a raw frame is there.
        Payload::Raw(_) => true,
        Payload::Gap(g) => short(&g.reason),
    }
}

/// Whether a sample or sweep carries a signal named with
/// [`SAGA_SIGNAL_PREFIX`]. The caller has already skipped records from the
/// saga itself.
fn spoofed(payload: &Payload) -> bool {
    let saga_name = |n: &String| n.starts_with(SAGA_SIGNAL_PREFIX);
    match payload {
        Payload::Sample(s) => saga_name(&s.signal),
        Payload::Sweep(s) => s.signals.iter().any(saga_name),
        _ => false,
    }
}

/// Whether a record describes the process that hosts the bus and not the
/// machine: a `ViewChanged` event, or a gap.
///
/// A TUI that hosts its bus publishes `ViewChanged` on its first frame and
/// on every view key. A replay host publishes a gap at each hole it finds
/// in the log it plays, and any bus publishes one when its disk fails.
/// None of these is in the recorded log at the same place, or at all: a
/// replaying TUI adds them at moments set by thread timing and key presses,
/// and `--saga-digest` reads the log file and sees none of the replay
/// host's. If they took ordinals, the ordinal in every later world seed and
/// beat cause would move, and two replays of one log would tell two
/// stories (final review finding C1). So the feed skips them the way it
/// skips the saga's own records: no ordinal and no observation.
///
/// The world lifecycle already ignored `ViewChanged` as activity, so the
/// saga loses nothing by not seeing it.
fn about_the_host(payload: &Payload) -> bool {
    matches!(
        payload,
        Payload::Event(Event::ViewChanged { .. }) | Payload::Gap(_)
    )
}

impl Feed {
    /// A feed that has seen nothing.
    pub fn new() -> Feed {
        Feed::default()
    }

    /// The number of records the feed has accepted. Records from source
    /// "saga" are not counted, so a replayed log that carries the saga's own
    /// events gives the same ordinals the live run had. Nor are `ViewChanged`
    /// events and gaps, which describe the host process (see
    /// `about_the_host`). Every other payload kind counts, raw frames
    /// included.
    ///
    /// A record dropped as oversized or rejected is not counted either. The
    /// ordinal feeds world seeds and every beat's cause, so a dropped record
    /// is invisible: inserting one anywhere in a stream leaves the saga and
    /// its digest as they were.
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Records dropped because they carry an item longer than
    /// [`MAX_ITEM_BYTES`].
    pub fn oversized(&self) -> u64 {
        self.oversized
    }

    /// Records dropped because a source other than "saga" sent a signal named
    /// with [`SAGA_SIGNAL_PREFIX`].
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    /// Appends the observations for `rec` to `out`.
    pub fn push(&mut self, rec: &Record, out: &mut Vec<Observation>) {
        if rec.source == SAGA_SOURCE {
            return;
        }
        // Dropped records take no ordinal (see `ordinal`). A record that is
        // both oversized and spoofed counts as oversized.
        if !fits(&rec.payload) {
            self.oversized += 1;
            return;
        }
        if spoofed(&rec.payload) {
            self.rejected += 1;
            return;
        }
        // Records about the host process carry no machine signal and take
        // no ordinal (see `about_the_host`).
        if about_the_host(&rec.payload) {
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
            // A gap never gets here (`about_the_host`).
            Payload::Raw(_) | Payload::Gap(_) => {}
        }
    }

    /// Decides whether a sweep from `entity` is a tick, and updates the
    /// pacemaker state (see the module comment). A sweep of anything but a
    /// chip never ticks and does not count toward the timeout.
    fn sweep_ticks(&mut self, entity: &EntityKey) -> bool {
        if entity.kind() != EntityKind::Chip {
            return false;
        }
        match &self.pacemaker {
            None => {
                self.elect(entity);
                return true;
            }
            Some(p) if p == entity => {
                self.round.clear();
                self.missed_rounds = 0;
                return true;
            }
            Some(_) => {}
        }
        // Another chip. Its second sweep since the pacemaker's last one
        // means a whole round passed without the pacemaker.
        if self.round.contains(entity) || self.round.len() >= MAX_ROUND_CHIPS {
            self.missed_rounds += 1;
            self.round.clear();
            if self.missed_rounds >= PACEMAKER_TIMEOUT_ROUNDS {
                self.elect(entity);
                return true;
            }
        }
        self.round.insert(entity.clone());
        false
    }

    /// Makes `entity` the pacemaker with a fresh round.
    fn elect(&mut self, entity: &EntityKey) {
        self.pacemaker = Some(entity.clone());
        self.round.clear();
        self.missed_rounds = 0;
    }
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod observe_tests;
