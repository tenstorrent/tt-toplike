// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The bookkeeping every bus view shares: which events are new, which phase
//! each model was last seen in, how the ECC counters of a DRAM channel add
//! up, and the smoothing of tokens per second.
//!
//! The arcade (`arcade_bus`, which Memory Castle and Defrag also read
//! through), Die 3D (`die3d::overlay`), the Starfield and MemoryFlow take
//! these rules from here, through [`EventIntake::take`] and [`PhaseBook`].
//! Three readers still carry their own copy of the away test: the Grid
//! (`ui::tui::grid_bus`, also its own phase memo), Training (`train_bus`,
//! which reads no event sequence) and the hivemind (`ui::tui::hivemind_bus`,
//! which marks away events as history and keeps them). The tests in `bus_intake_tests.rs` feed one sequence of
//! worlds to the arcade, to Die 3D and to these pieces and assert the same
//! decisions. The rules:
//!
//! * The first world, a world whose head went backwards, and an update after
//!   the view was away hand out no event. "Away" is an update more than
//!   [`AWAY_MIN_NS`] after the last one and more than [`AWAY_USUAL_GAPS`]
//!   times the usual gap between updates, so a slow data tick
//!   (`--interval 2000`) is not away. The newest event is recorded, so the
//!   next update starts after it.
//! * A phase change is a change against the phase last seen. The first sight
//!   of a model is not a change, and a re-announcement with the same phase
//!   changes nothing.
//! * A tokens-per-second reading older than [`TPS_STALE_MS`] is ignored, and
//!   fresh readings are smoothed over [`TPS_TAU_S`].
//!
//! The views apply the ECC rule differently, on purpose, and the intake
//! carries the choice as an [`EccPolicy`]. Die 3D flashes any rise of
//! a channel's ECC total since its last read, including the first read after
//! being away ([`EccPolicy::FlashAnyRise`]). The other views record a rise
//! first seen after being away and draw nothing for it
//! ([`EccPolicy::SuppressAfterAway`], the default). The first sight of a
//! channel never flashes under either. See [`Intake::ecc_flashes`].
//!
//! All of this runs at the data rate, in each view's `set_bus` or `update`.

use crate::animation::arcade_bus::{PhaseMemo, TPS_STALE_MS, TPS_TAU_S};
use crate::animation::die3d::overlay::PULSE_NS;
use crate::bus::signals as sig;
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, Event, ModelPhase, ModelView, Value, WorldRead};

const NS_PER_S: u64 = 1_000_000_000;

/// An update at least this long after the last one may be away: Die 3D's
/// pulse lifetime, so a pulse drawn for an event that arrived while away
/// would already have ended.
pub(crate) const AWAY_MIN_NS: u64 = PULSE_NS;

/// An update more than this many usual gaps after the last one (and more
/// than [`AWAY_MIN_NS`]) is away.
pub(crate) const AWAY_USUAL_GAPS: u64 = 4;

/// How a view treats a rise of a DRAM channel's ECC total. See the module
/// doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum EccPolicy {
    /// Any rise since the last read flashes, also on the first read after
    /// being away. Die 3D's policy.
    FlashAnyRise,
    /// A rise first seen on an update that is not live (after being away,
    /// or after the head went backwards) is recorded and draws nothing.
    /// Every view but Die 3D.
    #[default]
    SuppressAfterAway,
}

/// What one update may draw from the events.
#[derive(Debug, Clone, Default)]
pub(crate) struct Intake {
    /// The events since the last update, oldest first, with their sequence
    /// numbers. Empty on a first sight and after being away.
    pub(crate) events: Vec<(u64, Event)>,
    /// Whether this update may draw a change at all: false on a first
    /// sight, after being away, and when the head went backwards. Counter
    /// rises and resets are only recorded then.
    pub(crate) live: bool,
    /// Time since the last update, `None` on the first.
    pub(crate) gap: Option<u64>,
    /// The ECC policy of the intake that handed this out.
    ecc: EccPolicy,
}

impl Intake {
    /// Record the ECC total of DRAM channel `dk` in `book`, and whether this
    /// update draws a flash for it: a rise since the last read
    /// ([`ecc_rose`]), drawn on a live update, and under
    /// [`EccPolicy::FlashAnyRise`] also on one that is not.
    pub(crate) fn ecc_flashes(
        &self,
        world: &dyn WorldRead,
        dk: &EntityKey,
        book: &mut HashMap<EntityKey, u64>,
    ) -> bool {
        // The total is recorded whatever the policy decides, so the next
        // update compares against it.
        let rose = ecc_rose(world, dk, book);
        rose && (self.live || self.ecc == EccPolicy::FlashAnyRise)
    }
}

/// Where the last update left off. Start with the default; set a new default
/// when the view stops reading a world, so the next world is a first sight.
#[derive(Debug, Clone, Default)]
pub(crate) struct EventIntake {
    /// The highest event sequence number handled. `None` before the first
    /// world.
    seen_seq: Option<u64>,
    /// Bus time of the last update.
    last_now: Option<u64>,
    /// The time between the last two updates: the usual spacing (the data
    /// tick, 100 ms by default).
    last_gap: Option<u64>,
    /// How the [`Intake`]s handed out treat an ECC rise.
    ecc: EccPolicy,
}

impl EventIntake {
    /// A fresh intake whose updates treat an ECC rise under `ecc`. The
    /// default is [`EccPolicy::SuppressAfterAway`]; Die 3D starts its intake
    /// with [`EccPolicy::FlashAnyRise`].
    pub(crate) fn with_ecc(ecc: EccPolicy) -> EventIntake {
        EventIntake {
            ecc,
            ..EventIntake::default()
        }
    }

    /// The highest event sequence number handled, `None` before the first
    /// world.
    #[cfg(test)]
    pub(crate) fn seen_seq(&self) -> Option<u64> {
        self.seen_seq
    }

    /// Set the highest event sequence number handled, as if a world with
    /// that head had been read. A test uses it to put the head ahead of the
    /// next world's.
    #[cfg(test)]
    pub(crate) fn set_seen_seq(&mut self, seq: Option<u64>) {
        self.seen_seq = seq;
    }

    /// The events of `world` this update at bus time `now` may draw.
    pub(crate) fn take(&mut self, world: &dyn WorldRead, now: u64) -> Intake {
        let head = world.seq();
        let gap = self.last_now.map(|t| now.saturating_sub(t));
        let away = gap.is_some_and(|g| {
            g > AWAY_MIN_NS
                && self
                    .last_gap
                    .is_none_or(|usual| g > usual.saturating_mul(AWAY_USUAL_GAPS))
        });
        self.last_now = Some(now);
        if gap.is_some() {
            self.last_gap = gap;
        }
        let since = self.seen_seq.filter(|s| *s <= head && !away);
        let mut seen = head;
        let mut events = Vec::new();
        if let Some(s) = since {
            events = world.events_since(s);
            if let Some((q, _)) = events.last() {
                seen = seen.max(*q);
            }
        }
        self.seen_seq = Some(seen);
        Intake {
            events,
            live: since.is_some(),
            gap,
            ecc: self.ecc,
        }
    }
}

/// The phase each model was last seen in, and when it changed to it.
#[derive(Debug, Clone, Default)]
pub(crate) struct PhaseBook {
    phases: HashMap<EntityKey, PhaseMemo>,
}

impl PhaseBook {
    /// Record the phases of `models` at `now`. A model no longer announced is
    /// forgotten.
    pub(crate) fn update(&mut self, models: &[ModelView], now: u64) {
        self.phases
            .retain(|k, _| models.iter().any(|m| m.key == *k));
        for m in models {
            let memo = self.phases.entry(m.key.clone()).or_insert(PhaseMemo {
                phase: m.phase,
                since_ns: None,
            });
            if memo.phase != m.phase {
                *memo = PhaseMemo {
                    phase: m.phase,
                    since_ns: Some(now),
                };
            }
        }
    }

    /// The memo of model `key`.
    pub(crate) fn get(&self, key: &EntityKey) -> Option<&PhaseMemo> {
        self.phases.get(key)
    }
}

/// A reading's value as a number (`Text` is not one).
pub(crate) fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) => Some(*x).filter(|x| x.is_finite()),
        Value::U64(x) => Some(*x as f64),
        Value::I64(x) => Some(*x as f64),
        Value::Text(_) => None,
    }
}

/// The sum of the four ECC counters of DRAM channel `dk`, as Die 3D adds
/// them. `None` when the channel has none of them.
pub(crate) fn ecc_total(world: &dyn WorldRead, dk: &EntityKey) -> Option<u64> {
    [
        sig::DRAM_ECC_CORR_RD,
        sig::DRAM_ECC_CORR_WR,
        sig::DRAM_ECC_UNCORR_RD,
        sig::DRAM_ECC_UNCORR_WR,
    ]
    .iter()
    .filter_map(|s| world.reading(dk, s).and_then(|r| number(&r.value)))
    .fold(None, |sum: Option<u64>, v| {
        Some(sum.unwrap_or(0).saturating_add(v as u64))
    })
}

/// Record the ECC total of `dk` in `book`. Whether it rose since the last
/// update: never on the first sight of the channel, and a total that went
/// down (a counter reset) is only recorded. Views call it through
/// [`Intake::ecc_flashes`], which applies their [`EccPolicy`].
pub(crate) fn ecc_rose(
    world: &dyn WorldRead,
    dk: &EntityKey,
    book: &mut HashMap<EntityKey, u64>,
) -> bool {
    let Some(total) = ecc_total(world, dk) else {
        return false;
    };
    book.insert(dk.clone(), total)
        .is_some_and(|old| total > old)
}

/// The tokens per second of model `m` when it is ready and its reading is
/// at most [`TPS_STALE_MS`] old at `now`.
pub(crate) fn fresh_tps(world: &dyn WorldRead, m: &ModelView, now: u64) -> Option<f64> {
    if m.phase != ModelPhase::Ready {
        return None;
    }
    let r = world.reading(&m.key, tt_toplike_bus::signals::MODEL_TOKENS_PER_S)?;
    if world.age_ms(&r, now) > TPS_STALE_MS {
        return None;
    }
    m.tokens_per_s.filter(|t| t.is_finite() && *t >= 0.0)
}

/// `prev` moved toward `raw` over `gap`, with time constant [`TPS_TAU_S`].
/// No fresh reading drops the smoothing.
pub(crate) fn smooth_tps(raw: Option<f64>, prev: Option<f64>, gap: Option<u64>) -> Option<f64> {
    match (raw, prev, gap) {
        (None, _, _) => None,
        (Some(r), Some(s), Some(g)) => {
            let alpha = 1.0 - (-(g as f64 / NS_PER_S as f64) / TPS_TAU_S).exp();
            Some(s + (r - s) * alpha)
        }
        (Some(r), _, _) => Some(r),
    }
}

/// A model label made safe for a grid of one character per cell: a
/// character that is not one column wide becomes `?`.
pub(crate) fn one_column(text: &str) -> String {
    use unicode_width::UnicodeWidthChar;
    text.chars()
        .map(|c| if c.width() == Some(1) { c } else { '?' })
        .collect()
}

#[cfg(test)]
#[path = "bus_intake_tests.rs"]
mod tests;
