// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The bookkeeping the Starfield and MemoryFlow bus ports share: which
//! events are new, which phase each model was last seen in, how the ECC
//! counters of a DRAM channel add up, and the smoothing of tokens per
//! second.
//!
//! These rules are copied from the arcade's (`arcade_bus`, which Memory
//! Castle and Defrag also use), which in turn copies Die 3D's
//! (`die3d::overlay`), with the same constants. The code is a copy: the
//! arcade and Die 3D keep their own. A test at the end of this file feeds
//! one sequence of worlds to this module and to the arcade and asserts the
//! same decisions, so a change to one copy alone fails it. The rules:
//!
//! * The first world, a world whose head went backwards, and an update after
//!   the view was away hand out no event. "Away" is an update more than
//!   [`PULSE_NS`] after the last one and more than four times the usual gap
//!   between updates. The newest event is recorded, so the next update
//!   starts after it.
//! * A phase change is a change against the phase last seen. The first sight
//!   of a model is not a change.
//! * A tokens-per-second reading older than [`TPS_STALE_MS`] is ignored, and
//!   fresh readings are smoothed over [`TPS_TAU_S`].
//!
//! ECC is where the copies differ. Die 3D flashes any rise of a channel's
//! ECC total since its last read, including the first read after being
//! away. The arcade draws no counter rises. These modes record a rise first
//! seen after being away (or on the first sight of the channel) and draw
//! nothing for it ([`ecc_rose`], gated on [`Intake::live`] by the callers).
//!
//! All of this runs at the data rate, in each view's `set_bus`.

use crate::animation::arcade_bus::{PhaseMemo, TPS_STALE_MS, TPS_TAU_S};
use crate::animation::die3d::overlay::PULSE_NS;
use crate::bus::signals as sig;
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, Event, ModelPhase, ModelView, Value, WorldRead};

const NS_PER_S: u64 = 1_000_000_000;

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
    /// The time between the last two updates.
    last_gap: Option<u64>,
}

impl EventIntake {
    /// The events of `world` this update at bus time `now` may draw.
    pub(crate) fn take(&mut self, world: &dyn WorldRead, now: u64) -> Intake {
        let head = world.seq();
        let gap = self.last_now.map(|t| now.saturating_sub(t));
        let away = gap.is_some_and(|g| {
            g > PULSE_NS
                && self
                    .last_gap
                    .is_none_or(|usual| g > usual.saturating_mul(4))
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
/// down (a counter reset) is only recorded.
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

/// The rules here are a copy of the arcade's. This test feeds one sequence
/// of worlds to both and asserts they make the same decisions, so a change
/// to one copy that is not made to the other fails here.
#[cfg(test)]
mod same_rules_as_the_arcade {
    use super::*;
    use crate::animation::arcade_bus::{gauge_of, ArcadeBusState, ArcadeFx};
    use crate::animation::bus_test_support::{chip, Bus, SEC, TICK};
    use tt_toplike_bus::LocalView;

    /// What one update decided: whether it was live, the requests it handed
    /// out for models on the drawn chip, chip 0's gauge (from the phase
    /// change memo), and the smoothed tokens per second.
    type Decision = (
        bool,
        u32,
        Option<crate::animation::arcade_bus::Gauge>,
        Option<f64>,
    );

    /// The arcade's decisions over `worlds`.
    fn arcade(worlds: &[LocalView]) -> Vec<Decision> {
        let chips = [(0usize, chip(0))];
        let mut prev = ArcadeBusState::default();
        worlds
            .iter()
            .map(|w| {
                let fx = ArcadeFx::from_world(w, &chips, 1, w.now_mono_ns(), &mut prev);
                let gauge = fx.chips.iter().find(|c| c.slot == 0).and_then(|c| c.gauge);
                (fx.fresh, fx.new_requests, gauge, fx.tokens_per_s)
            })
            .collect()
    }

    /// The same decisions made with this module's pieces, the way the
    /// Starfield and MemoryFlow use them.
    fn intake(worlds: &[LocalView]) -> Vec<Decision> {
        let key = chip(0);
        let mut events = EventIntake::default();
        let mut book = PhaseBook::default();
        let mut tps: Option<f64> = None;
        worlds
            .iter()
            .map(|w| {
                let now = w.now_mono_ns();
                let models = w.models();
                let ours = |m: &ModelView| m.chips.contains(&key);
                let it = events.take(w, now);
                let n: u32 = it
                    .events
                    .iter()
                    .filter_map(|(_, e)| match e {
                        Event::RequestsCompleted { model, n } if *n > 0 => models
                            .iter()
                            .any(|m| m.key == *model && ours(m))
                            .then_some(*n),
                        _ => None,
                    })
                    .sum();
                book.update(&models, now);
                let gauge = w
                    .model_on_chip(&key)
                    .filter(|m| !m.chips.is_empty())
                    .and_then(|m| gauge_of(&m, book.get(&m.key), now));
                let raw = models
                    .iter()
                    .filter(|m| ours(m))
                    .filter_map(|m| fresh_tps(w, m, now))
                    .fold(None, |s: Option<f64>, t| Some(s.unwrap_or(0.0) + t));
                tps = smooth_tps(raw, tps, it.gap);
                (it.live, n, gauge, tps)
            })
            .collect()
    }

    #[test]
    fn the_intake_and_the_arcade_decide_alike() {
        let t0 = 5 * SEC;
        // History: a load and a request already in the first world.
        let mut a = Bus::new();
        a.loading("m", &[0], Some(0.3)).requests("m", 2);
        // A new request while loading.
        let mut b = a.clone();
        b.requests("m", 3);
        // Ready, with tokens per second: a phase change.
        let mut c = b.clone();
        c.ready("m", &[0], Some(40.0)).requests("m", 1);
        let mut d = c.clone();
        d.ready("m", &[0], Some(80.0));
        // A request while the view is away (a long gap).
        let mut e = d.clone();
        e.requests("m", 5);
        // A request a tick later: live again.
        let mut f = e.clone();
        f.requests("m", 4);
        // A reconnect: a new world whose head is behind, with the model down.
        let mut g = Bus::new();
        g.ready("m", &[0], Some(10.0)).phase("m", "down");
        let mut h = g.clone();
        h.requests("m", 6);
        let worlds = [
            a.view(t0),
            b.view(t0 + TICK),
            c.view(t0 + 2 * TICK),
            d.view(t0 + 3 * TICK),
            e.view(t0 + 3 * TICK + 10 * SEC),
            f.view(t0 + 4 * TICK + 10 * SEC),
            g.view(t0 + 5 * TICK + 10 * SEC),
            h.view(t0 + 6 * TICK + 10 * SEC),
            // The tokens-per-second reading goes stale.
            h.view(t0 + 6 * TICK + 70 * SEC),
        ];
        let want = arcade(&worlds);
        // The sequence reaches every rule: history, live, a change, away,
        // a reconnect, smoothing and a stale reading.
        let live: Vec<bool> = want.iter().map(|d| d.0).collect();
        assert_eq!(
            live,
            [false, true, true, true, false, true, false, true, false]
        );
        assert!(want.iter().any(|d| d.1 > 0));
        assert!(want.iter().any(|d| d.2.is_some()));
        assert!(want.iter().any(|d| d.3.is_some()));
        assert_eq!(intake(&worlds), want);
    }
}
