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
use tt_toplike_bus::LocalView;

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
