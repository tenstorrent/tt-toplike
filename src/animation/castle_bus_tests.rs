// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Memory Castle frames with and without the signal bus.
//!
//! The golden hashes pin the frames the castle drew before it read the bus.
//! They were taken by running `golden_hashes` against the commit before the
//! bus port (the commit that added this file holds only these tests), so a
//! frame drawn from an empty world must hash to exactly these values.

use super::*;
use crate::animation::bus_test_support::{frame_hash, tc, Fixed};

/// The layouts the golden hashes pin: (chips, width, height). One chip; two
/// chips in the full, compact and fleet-grid tiers.
const LAYOUTS: [(usize, usize, usize); 4] = [(1, 100, 30), (2, 120, 40), (2, 30, 30), (2, 14, 20)];
/// The updates after which a frame is hashed.
const HASHED_AT: [u32; 3] = [1, 10, 30];

/// Hashes of the frames of a castle in `layout` over [`Fixed`], after each
/// update in [`HASHED_AT`]. `before_update` runs before every update.
pub(super) fn hashes_with(
    (n, w, h): (usize, usize, usize),
    mut before_update: impl FnMut(&mut MemoryCastle, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(n);
        let mut c = MemoryCastle::new(w, h);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut c, &backend, i);
            c.update(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&c.render(&backend)));
            }
        }
        out
    })
}

/// The frames as the castle drew them before the bus port, for each of
/// [`LAYOUTS`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 3]; 4] = [
    [
        3227054172396918020,
        7077085263160370286,
        6484174506412191991,
    ],
    [
        630315234471104202,
        10500481099453121099,
        6438625630473264859,
    ],
    [
        15622758398212693181,
        306673472684778459,
        1072057634338145074,
    ],
    [
        18019069934949340965,
        18019069934949340965,
        18019069934949340965,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui castle_golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn castle_golden_hashes() {
    for l in LAYOUTS {
        println!("{l:?}: {:?}", hashes_with(l, |_, _, _| {}));
    }
}

#[test]
fn the_castle_frames_with_no_bus_are_the_pinned_frames() {
    for (i, l) in LAYOUTS.into_iter().enumerate() {
        assert_eq!(hashes_with(l, |_, _, _| {}), GOLDEN[i].to_vec(), "{l:?}");
    }
}

// ---- The bus ---------------------------------------------------------------

use crate::animation::arcade_bus::MAX_HIT_GLYPHS;
use crate::animation::bus_test_support::{cells, chip, fg, row_text, Bus, SEC, TICK};
use crate::animation::die3d::overlay::{
    FLASH_NS, LOAD_PURPLE, PULSE_NS, SERVE_TEAL, SETTLE_NS, SPARK_NS,
};
use tt_toplike_bus::{EntityKey, Event, Provenance, Value, WorldRead};

/// The castle the bus tests draw: two chips in the full tier at 120 x 40.
/// Each tower is 59 columns; tower `d` starts at column `2 + d * 60`.
const W: usize = 120;
const H: usize = 40;
const TOWER_W: usize = 59;
/// The first canvas row (after the top rule, the header and its rule) and
/// the canvas height (`H - 6`).
const CANVAS_TOP: usize = 3;
const CANVAS_H: usize = H - 6;

fn tower(d: usize) -> std::ops::Range<usize> {
    let a = 2 + d * (TOWER_W + 1);
    a..a + TOWER_W
}

fn castle() -> (MemoryCastle, Fixed) {
    (MemoryCastle::new(W, H), Fixed::new(2))
}

/// One castle update with `world`.
fn step(c: &mut MemoryCastle, b: &Fixed, world: &dyn WorldRead) {
    c.set_bus(Some(world), b);
    c.update(b);
}

/// Two castles given the same updates, `a` with `worlds` and `q` with an
/// empty world at the same times. Returns their frames.
fn pair(worlds: &[(u64, &Bus)]) -> (MemoryCastle, MemoryCastle, Fixed) {
    let (mut a, b) = castle();
    let (mut q, _) = castle();
    for (t, w) in worlds {
        step(&mut a, &b, &w.view(*t));
        step(&mut q, &b, &Bus::new().view(*t));
    }
    (a, q, b)
}

/// The canvas rows of tower `d` whose cells differ from the same rows of
/// `quiet`: (row in the canvas, number of differing cells).
fn changed_rows(frame: &[Line<'static>], quiet: &[Line<'static>], d: usize) -> Vec<(usize, usize)> {
    let (f, q) = (cells(frame), cells(quiet));
    (0..CANVAS_H)
        .filter_map(|r| {
            let row = CANVAS_TOP + r;
            let n = tower(d).filter(|&c| f[row].get(c) != q[row].get(c)).count();
            (n > 0).then_some((r, n))
        })
        .collect()
}

/// How many cells of `frame` in tower `d` hold `ch`.
fn count_in_tower(frame: &[Line<'static>], d: usize, ch: char) -> usize {
    let f = cells(frame);
    (CANVAS_TOP..CANVAS_TOP + CANVAS_H)
        .map(|r| {
            tower(d)
                .filter(|&c| f[r].get(c).map(|x| x.0) == Some(ch))
                .count()
        })
        .sum()
}

#[test]
fn an_empty_world_draws_the_pinned_castle_frames() {
    // An empty world, and one holding only a reading the castle does not
    // draw, give exactly the frames the castle drew before the bus.
    let mut quiet = Bus::new();
    quiet.sample(
        &chip(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    for world in [Bus::new(), quiet] {
        for (i, l) in LAYOUTS.into_iter().enumerate() {
            let got = hashes_with(l, |c, b, n| {
                c.set_bus(Some(&world.view(5 * SEC + n as u64 * TICK)), b)
            });
            assert_eq!(got, GOLDEN[i].to_vec(), "{l:?}");
        }
    }
}

#[test]
fn loading_at_half_raises_the_wing_half_way_and_no_progress_climbs() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.5));
        let (a, q, b) = pair(&[(t0, &f)]);
        let (fa, fq) = (a.render(&b), q.render(&b));
        // The banner is on row 0; the wing is the bottom half.
        let rows: Vec<usize> = changed_rows(&fa, &fq, 0)
            .into_iter()
            .map(|(r, _)| r)
            .filter(|r| *r > 0)
            .collect();
        assert_eq!(
            rows,
            (CANVAS_H / 2..CANVAS_H).collect::<Vec<_>>(),
            "{:?}",
            changed_rows(&fa, &fq, 0)
        );
        // The raised rows are purple and have no blank cells left.
        let row = &cells(&fa)[CANVAS_TOP + CANVAS_H - 1];
        assert!(tower(0).all(|c| row[c].0 != ' '));
        assert!(tower(0).any(|c| row[c].1.fg == fg(LOAD_PURPLE)));
        // Tower 1 has no model and is unchanged.
        assert!(changed_rows(&fa, &fq, 1).is_empty());

        // No progress figure: a band of rows, which climbs.
        let mut f = Bus::new();
        f.loading("m", &[0], None);
        let (mut a, mut q, b) = pair(&[(t0, &f)]);
        let first = changed_rows(&a.render(&b), &q.render(&b), 0);
        let band: Vec<usize> = first.iter().map(|(r, _)| *r).filter(|r| *r > 0).collect();
        assert!(!band.is_empty() && band.len() < CANVAS_H / 2, "{band:?}");
        step(&mut a, &b, &f.view(t0 + SEC * 3 / 10));
        step(&mut q, &b, &Bus::new().view(t0 + SEC * 3 / 10));
        let later: Vec<usize> = changed_rows(&a.render(&b), &q.render(&b), 0)
            .iter()
            .map(|(r, _)| *r)
            .filter(|r| *r > 0)
            .collect();
        assert!(
            !later.is_empty() && later.iter().max() < band.iter().max(),
            "the band climbs: {band:?} then {later:?}"
        );
    })
}

#[test]
fn ready_settles_the_tower_and_flies_a_banner() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.loading("m", &[1], Some(0.9));
        let (mut a, b) = castle();
        step(&mut a, &b, &f.view(t0));
        f.ready("m", &[1], Some(20.0));
        step(&mut a, &b, &f.view(t0 + TICK));
        let teal = |c: &MemoryCastle| {
            let f = cells(&c.render(&b));
            tower(1)
                .filter(|&x| f[CANVAS_TOP + 10][x].1.fg == fg(SERVE_TEAL))
                .count()
        };
        assert_eq!(teal(&a), TOWER_W, "the change to ready lights the tower");
        let banner = row_text(&a.render(&b), CANVAS_TOP);
        assert!(banner.contains("⚑ m ready 20t/s"), "{banner}");
        step(&mut a, &b, &f.view(t0 + TICK + SETTLE_NS));
        assert!(teal(&a) < 3, "and settles");
        assert!(row_text(&a.render(&b), CANVAS_TOP).contains("⚑ m ready 20t/s"));

        // A model first seen ready gets no settle.
        let (mut fresh, _) = castle();
        step(&mut fresh, &b, &f.view(t0));
        assert!(teal(&fresh) < 3);
    })
}

#[test]
fn an_estimated_model_banner_starts_with_a_tilde() {
    tc(|| {
        let m = EntityKey::model("mock");
        let mut f = Bus::new();
        f.sample(
            &m,
            tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
            Value::F64(20.0),
            Provenance::Estimated,
        )
        .event(Event::ModelReady {
            model: m,
            chips: vec![chip(0)],
        });
        let (mut a, b) = castle();
        step(&mut a, &b, &f.view(5 * SEC));
        let row = row_text(&a.render(&b), CANVAS_TOP);
        assert!(row.contains("⚑ ~mock ready 20t/s"), "{row}");

        // Measured: no tilde.
        let mut f = Bus::new();
        f.ready("real", &[0], Some(20.0));
        let (mut a, b) = castle();
        step(&mut a, &b, &f.view(5 * SEC));
        let row = row_text(&a.render(&b), CANVAS_TOP);
        assert!(
            row.contains("⚑ real ready 20t/s") && !row.contains('~'),
            "{row}"
        );
    })
}

/// A tower too narrow for any form of the banner still marks an estimate.
#[test]
fn a_tower_too_narrow_for_the_banner_still_marks_an_estimate() {
    tc(|| {
        // Two chips across 30 columns: the compact tier, towers of 14.
        let b = Fixed::new(2);
        let m = EntityKey::model("a-long-model-name");
        let mut f = Bus::new();
        f.sample(
            &m,
            tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
            Value::F64(20.0),
            Provenance::Estimated,
        )
        .event(Event::ModelReady {
            model: m,
            chips: vec![chip(0)],
        });
        let mut a = MemoryCastle::new(30, 30);
        step(&mut a, &b, &f.view(5 * SEC));
        // The compact tier: top rule, header, rule, gate row, then canvas.
        let frame = a.render(&b);
        let text = row_text(&frame, 4);
        assert!(!text.contains("a-long"), "{text}");
        assert_eq!(text.matches('~').count(), 1, "{text}");
    })
}

#[test]
fn a_request_sends_messengers_up_the_tower_and_history_does_not() {
    tc(|| {
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 3);
        let t0 = 5 * SEC;

        // Seen arriving: three messengers in tower 0, none in tower 1.
        let (a, _, b) = pair(&[(t0, &base), (t0 + TICK, &with)]);
        assert_eq!(count_in_tower(&a.render(&b), 0, '♞'), 3);
        assert_eq!(count_in_tower(&a.render(&b), 1, '♞'), 0);

        // History: the event was already there at the first read.
        let (h, q, b) = pair(&[(t0, &with)]);
        assert_eq!(h.render(&b).len(), q.render(&b).len());
        assert_eq!(count_in_tower(&h.render(&b), 0, '♞'), 0, "history");

        // Away: the event landed while the castle was not updating.
        let mut worlds: Vec<(u64, &Bus)> = (0..5).map(|i| (t0 + i * TICK, &base)).collect();
        worlds.push((t0 + 10 * SEC, &with));
        let (w, _, b) = pair(&worlds);
        assert_eq!(count_in_tower(&w.render(&b), 0, '♞'), 0, "away");
    })
}

#[test]
fn messengers_climb_and_are_gone_after_a_pulse() {
    tc(|| {
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 50);
        let t0 = 5 * SEC;
        let (mut a, b) = castle();
        step(&mut a, &b, &base.view(t0));
        step(&mut a, &b, &with.view(t0 + TICK));
        let row_of = |c: &MemoryCastle| {
            let f = cells(&c.render(&b));
            (CANVAS_TOP..CANVAS_TOP + CANVAS_H).find(|&r| tower(0).any(|x| f[r][x].0 == '♞'))
        };
        assert_eq!(
            count_in_tower(&a.render(&b), 0, '♞'),
            MAX_HIT_GLYPHS as usize,
            "one per request, up to the cap"
        );
        let low = row_of(&a).unwrap();
        step(&mut a, &b, &with.view(t0 + TICK + PULSE_NS / 2));
        let high = row_of(&a).unwrap();
        assert!(high < low, "the messengers climb: {low} then {high}");
        step(&mut a, &b, &with.view(t0 + TICK + PULSE_NS));
        assert_eq!(row_of(&a), None);
    })
}

#[test]
fn sparks_and_compiles_light_torches_in_their_chips_tower() {
    tc(|| {
        let base = Bus::new();
        let mut with = base.clone();
        with.spark(1, "vLLM.log").compiled(1);
        let t0 = 5 * SEC;
        let (mut a, _, b) = pair(&[(t0, &base), (t0 + TICK, &with)]);
        let torches = |c: &MemoryCastle, d: usize| {
            count_in_tower(&c.render(&b), d, '☼') + count_in_tower(&c.render(&b), d, '✶')
        };
        assert_eq!(torches(&a, 1), 2);
        assert_eq!(torches(&a, 0), 0);
        let f = cells(&a.render(&b));
        let colours: Vec<_> = (CANVAS_TOP..CANVAS_TOP + CANVAS_H)
            .flat_map(|r| tower(1).map(move |x| (r, x)))
            .filter(|&(r, x)| matches!(f[r][x].0, '☼' | '✶'))
            .map(|(r, x)| f[r][x].1.fg)
            .collect();
        assert!(colours.contains(&fg(SERVE_TEAL)) && colours.contains(&fg(LOAD_PURPLE)));
        // The torches burn down.
        step(&mut a, &b, &with.view(t0 + TICK + SPARK_NS));
        assert_eq!(torches(&a, 1), 0);
    })
}

#[test]
fn an_ecc_tick_opens_a_crack_that_heals() {
    tc(|| {
        let base = Bus::new();
        let mut with = base.clone();
        with.ecc(0, 7);
        let t0 = 5 * SEC;
        let (mut a, mut q, b) = pair(&[(t0, &base), (t0 + TICK, &with)]);
        let bottom = CANVAS_TOP + CANVAS_H - 1;
        let f = cells(&a.render(&b));
        let at: Vec<usize> = tower(0).filter(|&x| f[bottom][x].0 == '╳').collect();
        // Channel 7 of 8: in the last eighth of the tower.
        assert_eq!(at.len(), 1, "{at:?}");
        assert!(at[0] >= tower(0).start + TOWER_W * 7 / 8, "{at:?}");
        assert_eq!(count_in_tower(&a.render(&b), 1, '╳'), 0);
        step(&mut a, &b, &with.view(t0 + TICK + SETTLE_NS * 3 / 5));
        step(&mut q, &b, &Bus::new().view(t0 + TICK + SETTLE_NS * 3 / 5));
        assert_eq!(cells(&a.render(&b))[bottom][at[0]].0, '╱', "healing");
        // Healed: the cell is what a castle with no bus draws there.
        step(&mut a, &b, &with.view(t0 + TICK + SETTLE_NS));
        step(&mut q, &b, &Bus::new().view(t0 + TICK + SETTLE_NS));
        assert!(a.bus_fx.chips.is_empty(), "{:?}", a.bus_fx);
        assert_eq!(cells(&a.render(&b)), cells(&q.render(&b)), "healed");
    })
}

#[test]
fn a_reset_flashes_its_tower() {
    tc(|| {
        let mut base = Bus::new();
        base.reset(0);
        let mut again = base.clone();
        again.reset(0);
        let t0 = 5 * SEC;
        let reversed = |c: &MemoryCastle, b: &Fixed, d: usize| {
            let f = cells(&c.render(b));
            tower(d)
                .filter(|&x| {
                    f[CANVAS_TOP + 5][x]
                        .1
                        .add_modifier
                        .contains(Modifier::REVERSED)
                })
                .count()
        };
        // The first sight of a reset is history.
        let (mut a, b) = castle();
        step(&mut a, &b, &base.view(t0));
        assert_eq!(reversed(&a, &b, 0), 0);
        step(&mut a, &b, &again.view(t0 + TICK));
        assert_eq!(reversed(&a, &b, 0), TOWER_W);
        assert_eq!(reversed(&a, &b, 1), 0);
        step(&mut a, &b, &again.view(t0 + TICK + FLASH_NS));
        assert_eq!(reversed(&a, &b, 0), 0);
    })
}

#[test]
fn a_world_naming_a_chip_this_tui_lacks_draws_nothing_for_it() {
    tc(|| {
        // Chip 5 is not one of the two fixed chips.
        let mut f = Bus::new();
        f.ready("far", &[5], Some(120.0));
        let t0 = 5 * SEC;
        let mut g = f.clone();
        g.requests("far", 4)
            .spark(5, "vLLM.log")
            .compiled(5)
            .ecc(5, 1)
            .reset(5);
        let (a, q, b) = pair(&[(t0, &f), (t0 + TICK, &g)]);
        assert_eq!(a.render(&b), q.render(&b));
        assert!(a.bus_fx.chips.is_empty(), "{:?}", a.bus_fx);
    })
}

/// Contract rule 4 through the read path an attached TUI uses: a real
/// `ReadView` limited to this TUI's two chips. A model and its events on a
/// host's chip draw nothing; once the model also runs on chip 1, its banner
/// and messengers show in tower 1.
#[test]
fn attached_through_only_chips_a_hosts_chip_draws_nothing() {
    use crate::animation::bus_test_support::Clocked;
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tt_toplike_bus::{Payload, Sample};

    tc(|| {
        let (mut a, b) = castle();
        let (mut q, _) = castle();
        let host = BusHost::new(tt_toplike_bus::BusConfig::default(), &b).unwrap();
        let bus = host.bus().clone();
        let view = BusView::Local(Box::new(host));
        let cache = ReadCache::new();
        let local: Arc<BTreeSet<EntityKey>> = Arc::new([chip(0), chip(1)].into_iter().collect());
        let read = || view.read(&cache).only_chips(local.clone());
        // Each read gets a fixed bus time a tick after the last, so a stall
        // between two reads on a loaded machine cannot count as being away.
        let mut t = 5 * SEC;
        let mut at = || {
            t += TICK;
            t
        };
        let p = |e: Event| {
            bus.publish("test", Payload::Event(e));
        };
        let theirs = EntityKey::model("theirs");
        let foreign = EntityKey::chip("0000:ee:00.0");

        let r = read();
        let w = Clocked(&r, at());
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));

        bus.publish(
            "test",
            Payload::Sample(Sample {
                entity: theirs.clone(),
                signal: tt_toplike_bus::signals::MODEL_TOKENS_PER_S.into(),
                value: Value::F64(120.0),
                prov: Provenance::Measured,
            }),
        );
        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone()],
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 4,
        });
        p(Event::Spark {
            chip: Some(foreign.clone()),
            kind: "vLLM.log".into(),
        });
        p(Event::EccTick {
            chip: foreign.clone(),
            channel: 2,
        });
        p(Event::Reset {
            chips: vec![foreign.clone()],
        });
        let r = read();
        let w = Clocked(&r, at());
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));
        assert!(a.bus_fx.chips.is_empty(), "{:?}", a.bus_fx);
        assert_eq!(a.render(&b), q.render(&b));

        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone(), chip(1)],
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 2,
        });
        let r = read();
        let w = Clocked(&r, at());
        step(&mut a, &b, &w);
        let frame = a.render(&b);
        assert_eq!(count_in_tower(&frame, 1, '♞'), 2);
        assert_eq!(count_in_tower(&frame, 0, '♞'), 0);
        let banner = row_text(&frame, CANVAS_TOP);
        assert!(banner.contains("theirs ready"), "{banner}");
    })
}

/// Group B M4: the fleet grid's `~` goes in the label's pad column. A label
/// as wide as its five columns (`BH100`) has no pad, so the `~` goes on the
/// bar's first column and the label keeps its last character.
#[test]
fn the_estimate_mark_never_covers_a_label_character() {
    tc(|| {
        let flag = crate::animation::die3d::overlay::ModelFlag {
            label: "mock".into(),
            phase: tt_toplike_bus::ModelPhase::Loading,
            colour: [200, 100, 255],
            tokens_per_s: None,
            waiting: None,
            progress: None,
            estimated: true,
        };
        let co = crate::animation::arcade_bus::ChipFx {
            flag: Some(flag),
            ..Default::default()
        };
        for (label, want) in [("BH0  ", "BH0 ~████"), ("BH100", "BH100~███")] {
            let mut spans = vec![
                ratatui::text::Span::raw("x"),
                ratatui::text::Span::raw(label),
                ratatui::text::Span::raw("████"),
            ];
            let w = label.chars().count();
            crate::animation::castle_bus::draw_cell(&co, 5 * SEC, w..w + 4, &mut spans, 1);
            let text: String = spans[1..].iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(text, want, "{label}");
            assert_eq!(spans[0].content, "x", "spans before the cell are kept");
        }
    })
}

#[test]
fn the_fleet_grid_shows_the_gauge_and_the_estimate_mark() {
    tc(|| {
        // Two chips in 14 columns: the fleet grid. Row 3 is chip 0's cell;
        // its label is columns 2..7 and its bar 7..19.
        let b = Fixed::new(2);
        let m = EntityKey::model("mock");
        let mut f = Bus::new();
        f.sample(
            &m,
            tt_toplike_bus::signals::MODEL_PROGRESS,
            Value::F64(0.5),
            Provenance::Estimated,
        )
        .loading("mock", &[0], Some(0.5));
        let mut a = MemoryCastle::new(14, 20);
        let mut q = MemoryCastle::new(14, 20);
        step(&mut a, &b, &f.view(5 * SEC));
        step(&mut q, &b, &Bus::new().view(5 * SEC));
        let (fa, fq) = (cells(&a.render(&b)), cells(&q.render(&b)));
        let purple = (7..19)
            .filter(|&x| fa[3][x].1.fg == fg(LOAD_PURPLE))
            .count();
        assert_eq!(purple, 6, "half of the 12-column bar");
        assert_eq!(fa[3][6].0, '~');
        assert_eq!(fa[4], fq[4], "chip 1's cell is unchanged");
    })
}
