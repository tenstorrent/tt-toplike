// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests of the shared intake against the views that use it.
//!
//! Each test feeds one scripted sequence of worlds to more than one reader
//! and asserts they decide alike. A change to the rule in one place that is
//! not made everywhere fails here.

use super::*;
use crate::animation::arcade_bus::{gauge_of, ArcadeBusState, ArcadeFx, Gauge};
use crate::animation::bus_test_support::{chip, Bus, SEC, TICK};
use crate::animation::die3d::overlay::{Fill, Overlay, OverlayState};
use tt_toplike_bus::{LocalView, Provenance};

/// The arcade and the shared pieces, as the Starfield and MemoryFlow use
/// them, over one sequence with a reconnect and a stale reading.
mod same_rules_as_the_arcade {
    use super::*;

    /// What one update decided: whether it was live, the requests it handed
    /// out for models on the drawn chip, chip 0's gauge (from the phase
    /// change memo), and the smoothed tokens per second.
    type Decision = (bool, u32, Option<Gauge>, Option<f64>);

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

/// The arcade, Die 3D and the shared intake over one sequence.
mod arcade_die3d_and_the_shared_intake {
    use super::*;

    /// What every reader can show of one update: whether it drew a request
    /// for a model on chip 0, and since when chip 0's model has been ready
    /// (the time of the phase change, while the settle lasts).
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Shared {
        pulsed: bool,
        settled_since: Option<u64>,
    }

    /// One update of the arcade: the shared part, whether it was live, the
    /// requests drawn and the smoothed tokens per second.
    type ArcadeDecision = (Shared, bool, u32, Option<f64>);

    /// One update of Die 3D: the shared part and whether channel 0 of chip
    /// 0 flashed for its ECC counters.
    type Die3dDecision = (Shared, bool);

    /// One update of the shared intake: the arcade's decision, then whether
    /// channel 0 flashed under each ECC policy (Die 3D's, then the others').
    type IntakeDecision = (ArcadeDecision, bool, bool);

    /// The chip the three readers draw.
    fn chips() -> [(usize, EntityKey); 1] {
        [(0usize, chip(0))]
    }

    /// The arcade's decisions over `worlds`.
    fn arcade(worlds: &[LocalView]) -> Vec<ArcadeDecision> {
        let mut prev = ArcadeBusState::default();
        worlds
            .iter()
            .map(|w| {
                let now = w.now_mono_ns();
                let fx = ArcadeFx::from_world(w, &chips(), 1, now, &mut prev);
                let co = fx.chips.iter().find(|c| c.slot == 0);
                let shared = Shared {
                    pulsed: co.is_some_and(|c| c.hits.iter().any(|h| h.start_ns == now)),
                    settled_since: co.and_then(|c| match c.gauge {
                        Some(Gauge::Settle { since_ns }) => Some(since_ns),
                        _ => None,
                    }),
                };
                (shared, fx.fresh, fx.new_requests, fx.tokens_per_s)
            })
            .collect()
    }

    /// Die 3D's decisions over `worlds`.
    fn die3d(worlds: &[LocalView]) -> Vec<Die3dDecision> {
        let mut prev = OverlayState::default();
        worlds
            .iter()
            .map(|w| {
                let now = w.now_mono_ns();
                let o = Overlay::from_world(w, &chips(), now, &mut prev);
                let co = o.chips.iter().find(|c| c.index == 0);
                let shared = Shared {
                    pulsed: co.is_some_and(|c| c.pulses.iter().any(|p| p.start_ns == now)),
                    settled_since: co.and_then(|c| match c.fill {
                        Some(Fill::Settle { since_ns }) => Some(since_ns),
                        _ => None,
                    }),
                };
                let flashed = co.is_some_and(|c| {
                    c.gddr
                        .iter()
                        .any(|r| r.channel == 0 && r.flash_ns == Some(now))
                });
                (shared, flashed)
            })
            .collect()
    }

    /// The shared intake's decisions over `worlds`, made the way the
    /// Starfield uses it, with one intake per ECC policy.
    fn intake(worlds: &[LocalView]) -> Vec<IntakeDecision> {
        let key = chip(0);
        let dk = EntityKey::dram_channel(&key, 0);
        let mut events = EventIntake::default();
        let mut any_rise = EventIntake::with_ecc(EccPolicy::FlashAnyRise);
        let mut book = PhaseBook::default();
        let mut ecc_after_away: HashMap<EntityKey, u64> = HashMap::new();
        let mut ecc_any_rise: HashMap<EntityKey, u64> = HashMap::new();
        let mut tps: Option<f64> = None;
        worlds
            .iter()
            .map(|w| {
                let now = w.now_mono_ns();
                let models = w.models();
                let ours = |m: &ModelView| m.chips.contains(&key);
                let it = events.take(w, now);
                let it_any = any_rise.take(w, now);
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
                let settled_since = w
                    .model_on_chip(&key)
                    .filter(|m| !m.chips.is_empty())
                    .and_then(|m| match gauge_of(&m, book.get(&m.key), now) {
                        Some(Gauge::Settle { since_ns }) => Some(since_ns),
                        _ => None,
                    });
                let raw = models
                    .iter()
                    .filter(|m| ours(m))
                    .filter_map(|m| fresh_tps(w, m, now))
                    .fold(None, |s: Option<f64>, t| Some(s.unwrap_or(0.0) + t));
                tps = smooth_tps(raw, tps, it.gap);
                let shared = Shared {
                    pulsed: n > 0,
                    settled_since,
                };
                let flash_any = it_any.ecc_flashes(w, &dk, &mut ecc_any_rise);
                let flash_after_away = it.ecc_flashes(w, &dk, &mut ecc_after_away);
                ((shared, it.live, n, tps), flash_any, flash_after_away)
            })
            .collect()
    }

    /// Set the corrected-read ECC count of channel 0 of chip 0.
    fn ecc(bus: &mut Bus, v: u64) {
        bus.sample(
            &EntityKey::dram_channel(&chip(0), 0),
            crate::bus::signals::DRAM_ECC_CORR_RD,
            Value::U64(v),
            Provenance::Measured,
        );
    }

    #[test]
    fn arcade_die3d_and_the_shared_intake_decide_alike_on_the_shared_cases() {
        let t0 = 5 * SEC;
        // 0. History at first read: a load, a request and an ECC count are
        //    already in the world. Nothing is drawn.
        let mut a = Bus::new();
        a.loading("m", &[0], None).requests("m", 2);
        ecc(&mut a, 5);
        // 1. Live: a request and an ECC rise.
        let mut b = a.clone();
        b.requests("m", 3);
        ecc(&mut b, 6);
        // 2. A phase change (loading to ready, with tokens per second) and a
        //    request.
        let mut c = b.clone();
        c.ready("m", &[0], Some(40.0)).requests("m", 1);
        // 3. A re-announcement with an unchanged phase: the settle keeps the
        //    time of the change.
        let mut d = c.clone();
        d.ready("m", &[0], Some(80.0));
        // 4. A gap over 1 s (more than four usual gaps): a request and an
        //    ECC rise that arrived while away. No request is drawn; the ECC
        //    rise is where the policies differ.
        let mut e = d.clone();
        e.requests("m", 5).ready("m", &[0], Some(60.0));
        ecc(&mut e, 9);
        // 5. Live again a tick later: a request, and an ECC counter reset,
        //    which only rebaselines.
        let mut f = e.clone();
        f.requests("m", 4);
        ecc(&mut f, 2);
        // 6. Live: an ECC rise from the new baseline.
        let mut g = f.clone();
        ecc(&mut g, 3);
        let after_away = t0 + 3 * TICK + 1_500_000_000;
        let times = [
            t0,
            t0 + TICK,
            t0 + 2 * TICK,
            t0 + 3 * TICK,
            after_away,
            after_away + TICK,
            after_away + 2 * TICK,
        ];
        let worlds: Vec<LocalView> = [&a, &b, &c, &d, &e, &f, &g]
            .iter()
            .zip(times)
            .map(|(bus, t)| bus.view(t))
            .collect();

        let arc = arcade(&worlds);
        let die = die3d(&worlds);
        let shared = intake(&worlds);

        // The sequence reaches every case it names.
        let live: Vec<bool> = arc.iter().map(|d| d.1).collect();
        assert_eq!(live, [false, true, true, true, false, true, true]);
        let pulsed: Vec<bool> = arc.iter().map(|d| d.0.pulsed).collect();
        assert_eq!(pulsed, [false, true, true, false, false, true, false]);
        let change = Some(t0 + 2 * TICK);
        let settled: Vec<Option<u64>> = arc.iter().map(|d| d.0.settled_since).collect();
        assert_eq!(
            settled,
            [None, None, change, change, change, change, change],
            "the re-announcements keep the time of the change"
        );
        // The smoothing runs across the gap: the update after it moves from
        // the last smoothed value toward the new reading of 60 by
        // 1 - e^(-1.5 / 2), the share a 1.5 s gap covers with a 2 s time
        // constant.
        let tps3 = arc[3].3.expect("a fresh reading before the gap");
        let tps4 = arc[4].3.expect("a fresh reading after the gap");
        let want4 = tps3 + (60.0 - tps3) * (1.0 - (-1.5f64 / 2.0).exp());
        assert!((tps4 - want4).abs() < 1e-9, "{tps4} against {want4}");

        // The arcade and the shared intake agree on everything they share.
        let shared_arcade: Vec<ArcadeDecision> = shared.iter().map(|d| d.0).collect();
        assert_eq!(shared_arcade, arc);
        // Die 3D agrees with both on the requests and the phase.
        let die_shared: Vec<Shared> = die.iter().map(|d| d.0).collect();
        let arc_shared: Vec<Shared> = arc.iter().map(|d| d.0).collect();
        assert_eq!(die_shared, arc_shared);

        // ECC. Die 3D flashes any rise, including the one first seen after
        // being away (update 4); the shared intake under `FlashAnyRise`
        // makes the same decisions. Under `SuppressAfterAway`, the policy of
        // every other view, the rise after being away is recorded and
        // draws nothing. A counter reset (update 5) flashes under neither.
        let die_ecc: Vec<bool> = die.iter().map(|d| d.1).collect();
        let flash_any: Vec<bool> = shared.iter().map(|d| d.1).collect();
        let after_away_ecc: Vec<bool> = shared.iter().map(|d| d.2).collect();
        assert_eq!(die_ecc, [false, true, false, false, true, false, true]);
        assert_eq!(flash_any, die_ecc);
        assert_eq!(
            after_away_ecc,
            [false, true, false, false, false, false, true]
        );
    }
}
