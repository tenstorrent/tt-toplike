// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! MemoryFlow frames with and without the signal bus.
//!
//! The golden hashes pin the frames MemoryFlow drew before it read the bus.
//! They were taken by running `golden_hashes` against the commit that added
//! this file, which holds only these tests, so a frame drawn from an empty
//! world must hash to exactly these values.

use crate::animation::starfield_bus::tests::{frame_hash, tc, Fixed, HASHED_AT, SIZES};
use crate::animation::MemoryFlowVis;

/// Hashes of the frames of a MemoryFlow view at `w` x `h` over two
/// [`Fixed`] chips, after each update in [`HASHED_AT`]. `before_update`
/// runs before every update (the bus tests hand the view a world there).
pub(super) fn hashes_with(
    w: usize,
    h: usize,
    mut before_update: impl FnMut(&mut MemoryFlowVis, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(2);
        let mut mf = MemoryFlowVis::new(w, h);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut mf, &backend, i);
            mf.update(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&mf.render(&backend)));
            }
        }
        out
    })
}

/// The frames as MemoryFlow drew them before the bus port, at each of
/// [`SIZES`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 3]; 2] = [
    [
        16119804873617930179,
        2949916930769929029,
        9339828239280807536,
    ],
    [
        9868299539168236138,
        16623803713248559811,
        10993199208549131537,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui memory_flow_bus::tests::golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn golden_hashes() {
    for (w, h) in SIZES {
        println!("{w}x{h}: {:?}", hashes_with(w, h, |_, _, _| {}));
    }
}

#[test]
fn the_frames_with_no_bus_are_the_pinned_frames() {
    for (i, (w, h)) in SIZES.into_iter().enumerate() {
        assert_eq!(
            hashes_with(w, h, |_, _, _| {}),
            GOLDEN[i].to_vec(),
            "{w}x{h}"
        );
    }
}

// ---- The bus ---------------------------------------------------------------

use crate::animation::die3d::overlay::{ALARM_RED, FLASH_NS, LOAD_PURPLE, PULSE_NS};
use crate::animation::memory_flow_bus::{EXPLAIN_TEXT, LANE_LABEL_COLS, LEGEND_TEXT};
use crate::animation::starfield_bus::tests::{chip, row_text, Bus, Clocked, SEC, TICK};
use crate::backend::TelemetryBackend;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use tt_toplike_bus::{EntityKey, Event, Payload, Provenance, Sample, Value, WorldRead};

/// The view size the bus tests draw at: two slots of 60 columns, twelve
/// DDR segments of eight columns.
const W: usize = 120;
const H: usize = 30;
const CW: usize = 8;
/// Rows of the rendered lines: DDR IN, the model row, DDR OUT.
const LANE_IN: usize = 0;
const MODEL_ROW: usize = 1;
const LANE_OUT: usize = 27;

fn view() -> (MemoryFlowVis, Fixed) {
    (MemoryFlowVis::new(W, H), Fixed::new(2))
}

fn step(mf: &mut MemoryFlowVis, b: &Fixed, world: &dyn WorldRead) {
    mf.set_bus(Some(world), b.devices());
    mf.update(b);
}

/// Each row as (character, style) cells.
fn cells(lines: &[Line<'static>]) -> Vec<Vec<(char, Style)>> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                .collect()
        })
        .collect()
}

/// The columns of DDR segment `ch`.
fn seg(ch: usize) -> std::ops::Range<usize> {
    LANE_LABEL_COLS + ch * CW..LANE_LABEL_COLS + (ch + 1) * CW
}

/// The text of `row` within `cols`.
fn text_at(lines: &[Line<'static>], row: usize, cols: std::ops::Range<usize>) -> String {
    cells(lines)[row]
        .iter()
        .enumerate()
        .filter(|(i, _)| cols.contains(i))
        .map(|(_, c)| c.0)
        .collect()
}

/// A view fed `worlds` and a twin fed empty worlds at the same times; their
/// frames after the last update.
fn pair(worlds: &[(&Bus, u64)]) -> (Vec<Line<'static>>, Vec<Line<'static>>, MemoryFlowVis) {
    let (mut a, b) = view();
    let (mut q, _) = view();
    for (w, t) in worlds {
        step(&mut a, &b, &w.view(*t));
        step(&mut q, &b, &Bus::new().view(*t));
    }
    (a.render(&b), q.render(&b), a)
}

/// The rows where `a` and `b` differ.
fn rows_differing(a: &[Line<'static>], b: &[Line<'static>]) -> Vec<usize> {
    let (ca, cb) = (cells(a), cells(b));
    (0..ca.len().max(cb.len()))
        .filter(|r| ca.get(*r) != cb.get(*r))
        .collect()
}

fn dram(f: &mut Bus, i: usize, ch: u8, signal: &str, v: Value) {
    f.sample(
        &EntityKey::dram_channel(&chip(i), ch),
        signal,
        v,
        Provenance::Measured,
    );
}

#[test]
fn the_render_has_the_rows_the_tests_assume() {
    let (a, q, _) = pair(&[(&Bus::new(), 5 * SEC)]);
    assert_eq!(a, q);
    assert_eq!(a.len(), H - 1);
    assert!(row_text(&a, LANE_IN).starts_with("DDR IN:  "));
    assert!(row_text(&a, LANE_OUT).starts_with("DDR OUT: "));
    assert_eq!(row_text(&a, MODEL_ROW), "");
}

#[test]
fn an_empty_world_draws_the_pinned_frames() {
    let mut quiet = Bus::new();
    quiet.sample(
        &chip(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    for world in [Bus::new(), quiet] {
        for (i, (w, h)) in SIZES.into_iter().enumerate() {
            let got = hashes_with(w, h, |mf, b, n| {
                let v = world.view(5 * SEC + n as u64 * TICK);
                mf.set_bus(Some(&v), b.devices());
            });
            assert_eq!(got, GOLDEN[i].to_vec(), "{w}x{h}");
        }
    }
}

#[test]
fn dram_readings_set_a_lanes_state() {
    use crate::bus::signals as sig;
    tc(|| {
        let mut f = Bus::new();
        // Channel 2 of chip 0 trained; channel 3 harvested; channel 4
        // failed its BIST. Channel 5 of chip 1 trained: the bars show the
        // first device's channels, so it changes nothing.
        dram(&mut f, 0, 2, sig::DRAM_TRAINING_PASS, Value::U64(1));
        dram(&mut f, 0, 2, sig::DRAM_BIST_PASS, Value::U64(1));
        dram(&mut f, 0, 3, sig::DRAM_HARVESTED, Value::U64(1));
        dram(&mut f, 0, 3, sig::DRAM_TRAINING_PASS, Value::U64(1));
        dram(&mut f, 0, 4, sig::DRAM_BIST_PASS, Value::U64(0));
        dram(&mut f, 1, 5, sig::DRAM_TRAINING_PASS, Value::U64(1));
        let (a, q, _) = pair(&[(&f, 5 * SEC)]);
        // The fixed backend has no GDDR block, so with no bus every segment
        // is the idle `─`.
        assert_eq!(text_at(&q, LANE_IN, seg(2)), "─".repeat(CW));
        // Trained: a utilisation bar of `═` and `·`.
        let t = text_at(&a, LANE_IN, seg(2));
        assert!(
            t.chars().all(|c| c == '═' || c == '·') && t.contains('═'),
            "{t}"
        );
        let t = text_at(&a, LANE_OUT, seg(2));
        assert!(t.chars().all(|c| c == '═' || c == '·'), "{t}");
        // Harvested: idle, as before.
        assert_eq!(text_at(&a, LANE_IN, seg(3)), text_at(&q, LANE_IN, seg(3)));
        // BIST failed: the error colour.
        let err = crate::ui::colors::rgb(180, 60, 60);
        assert!(cells(&a)[LANE_IN][seg(4).start].1.fg == Some(err));
        assert_ne!(cells(&q)[LANE_IN][seg(4).start].1.fg, Some(err));
        // Chip 1's channel 5 and every other channel stay as they were.
        for ch in [0, 1, 5, 6, 11] {
            assert_eq!(
                text_at(&a, LANE_IN, seg(ch)),
                text_at(&q, LANE_IN, seg(ch)),
                "{ch}"
            );
        }
    })
}

#[test]
fn a_hot_channel_tints_its_segments_red() {
    use crate::bus::signals as sig;
    tc(|| {
        let at = |c: f64| {
            let mut f = Bus::new();
            dram(&mut f, 0, 1, sig::DRAM_TEMP_TOP_C, Value::F64(c));
            let (a, _, _) = pair(&[(&f, 5 * SEC)]);
            cells(&a)[LANE_IN][seg(1).start].1.fg
        };
        // Distance to the alarm red.
        let off = |c: Option<ratatui::style::Color>| match c {
            Some(ratatui::style::Color::Rgb(r, g, b)) => {
                (r as i32 - ALARM_RED[0] as i32).abs()
                    + (g as i32 - ALARM_RED[1] as i32).abs()
                    + (b as i32 - ALARM_RED[2] as i32).abs()
            }
            other => panic!("{other:?}"),
        };
        assert!(
            off(at(90.0)) < off(at(40.0)),
            "{:?} {:?}",
            at(90.0),
            at(40.0)
        );
    })
}

#[test]
fn a_loading_model_streams_into_ddr_in_and_shows_on_the_model_row() {
    tc(|| {
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.5));
        let (a, q, sf) = pair(&[(&f, 5 * SEC)]);
        assert!(sf.bus_fx().stream.is_some());
        let purple = Some(crate::ui::colors::rgb(
            LOAD_PURPLE[0],
            LOAD_PURPLE[1],
            LOAD_PURPLE[2],
        ));
        for ch in 0..12 {
            let t = text_at(&a, LANE_IN, seg(ch));
            assert!(
                t.starts_with("▸▸▸▸") && !t.starts_with("▸▸▸▸▸"),
                "{ch}: {t}"
            );
            assert_eq!(cells(&a)[LANE_IN][seg(ch).start].1.fg, purple);
        }
        assert_eq!(
            row_text(&a, LANE_OUT),
            row_text(&q, LANE_OUT),
            "DDR OUT has no stream"
        );
        let row = row_text(&a, MODEL_ROW);
        assert!(row.contains("m loading 50%"), "{row}");
        // The gauge: half of chip 0's 60 columns, none in chip 1's.
        assert_eq!(text_at(&a, MODEL_ROW, 0..30), "▸".repeat(30));
        assert_eq!(text_at(&a, MODEL_ROW, 60..120).trim(), "");

        // Compiling shows on the model row and streams nothing.
        let mut c = Bus::new();
        c.loading("m", &[0], Some(0.5));
        c.sample(
            &EntityKey::model("m"),
            tt_toplike_bus::signals::MODEL_PHASE,
            Value::Text("compiling".into()),
            Provenance::Measured,
        );
        let (a, q, sf) = pair(&[(&c, 5 * SEC)]);
        assert!(sf.bus_fx().stream.is_none(), "{:?}", sf.bus_fx());
        assert_eq!(row_text(&a, LANE_IN), row_text(&q, LANE_IN));
        assert!(row_text(&a, MODEL_ROW).contains("compiling"));
    })
}

#[test]
fn loading_with_no_progress_runs_along_the_lane() {
    tc(|| {
        let mut f = Bus::new();
        f.loading("m", &[0], None);
        let (a1, _, _) = pair(&[(&f, 5 * SEC)]);
        let (a2, _, _) = pair(&[(&f, 5 * SEC), (&f, 5 * SEC + SEC)]);
        let r1 = text_at(&a1, LANE_IN, seg(0));
        let r2 = text_at(&a2, LANE_IN, seg(0));
        assert!(r1.contains('▸') || r2.contains('▸'), "{r1} {r2}");
        assert_ne!(r1, r2, "the run moves");
        assert!(
            !r1.starts_with(&"▸".repeat(CW)),
            "the run is shorter than the segment"
        );
    })
}

#[test]
fn a_request_pulses_both_lanes_and_history_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut base = Bus::new();
        base.ready("m", &[1], None);
        let mut with = base.clone();
        with.requests("m", 3);

        let (a, q, sf) = pair(&[(&base, t0), (&with, t0 + TICK)]);
        assert_eq!(sf.bus_fx().pulses.len(), 1);
        let d = rows_differing(&a, &q);
        assert!(d.contains(&LANE_IN) && d.contains(&LANE_OUT), "{d:?}");
        assert!(cells(&a)[LANE_IN][seg(0).start]
            .1
            .add_modifier
            .contains(Modifier::BOLD));

        // History.
        let (_, _, sf) = pair(&[(&with, t0), (&with, t0 + TICK)]);
        assert!(sf.bus_fx().pulses.is_empty());
        // Away.
        let mut ws: Vec<(&Bus, u64)> = (0..5).map(|i| (&base, t0 + i * TICK)).collect();
        ws.push((&with, t0 + 10 * SEC));
        let (_, _, sf) = pair(&ws);
        assert!(sf.bus_fx().pulses.is_empty(), "an away event draws nothing");
        // It fades out.
        let (_, _, sf) = pair(&[
            (&base, t0),
            (&with, t0 + TICK),
            (&with, t0 + TICK + PULSE_NS),
        ]);
        assert!(sf.bus_fx().pulses.is_empty());
        // A model on no chip of this view pulses nothing.
        let mut other = Bus::new();
        other.event(Event::ModelReady {
            model: EntityKey::model("x"),
            chips: vec![EntityKey::chip("0000:ee:00.0")],
        });
        let mut other2 = other.clone();
        other2.requests("x", 5);
        let (_, _, sf) = pair(&[(&other, t0), (&other2, t0 + TICK)]);
        assert!(sf.bus_fx().pulses.is_empty());
    })
}

#[test]
fn an_ecc_rise_flashes_its_channel_and_the_first_sight_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut e1 = Bus::new();
        e1.ecc(1, 6, 5);
        let mut e2 = e1.clone();
        e2.ecc(1, 6, 9);
        let (a, _, _) = pair(&[(&e1, t0), (&e2, t0 + TICK)]);
        let red = Some(crate::ui::colors::rgb(
            ALARM_RED[0],
            ALARM_RED[1],
            ALARM_RED[2],
        ));
        for row in [LANE_IN, LANE_OUT] {
            let c = &cells(&a)[row];
            for x in seg(6) {
                assert!(
                    c[x].1.add_modifier.contains(Modifier::REVERSED),
                    "{row} {x}"
                );
                assert_eq!(c[x].1.fg, red);
            }
            assert!(!c[seg(5).start].1.add_modifier.contains(Modifier::REVERSED));
        }
        // First sight.
        let (a, q, _) = pair(&[(&e2, t0), (&e2, t0 + TICK)]);
        assert_eq!(a, q);
        // A count that first appears after the first update.
        let (a, q, _) = pair(&[(&Bus::new(), t0), (&e2, t0 + TICK)]);
        assert_eq!(a, q);
        // Over after the flash.
        let (a, q, _) = pair(&[(&e1, t0), (&e2, t0 + TICK), (&e2, t0 + TICK + FLASH_NS)]);
        assert_eq!(a, q);
        // An `EccTick` flashes too.
        let mut t = Bus::new();
        t.ecc(0, 0, 0);
        let mut t2 = t.clone();
        t2.event(Event::EccTick {
            chip: chip(0),
            channel: 2,
        });
        let (a, _, _) = pair(&[(&t, t0), (&t2, t0 + TICK)]);
        assert!(cells(&a)[LANE_IN][seg(2).start]
            .1
            .add_modifier
            .contains(Modifier::REVERSED));
    })
}

#[test]
fn a_reset_flashes_the_chips_share_of_the_model_row() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut r1 = Bus::new();
        r1.event(Event::Reset {
            chips: vec![chip(1)],
        });
        let mut r2 = r1.clone();
        r2.event(Event::Reset {
            chips: vec![chip(1)],
        });
        let (a, q, _) = pair(&[(&r1, t0), (&r2, t0 + TICK)]);
        assert_eq!(rows_differing(&a, &q), vec![MODEL_ROW]);
        let c = &cells(&a)[MODEL_ROW];
        assert!(c[60].1.add_modifier.contains(Modifier::REVERSED));
        assert!(c[119].1.add_modifier.contains(Modifier::REVERSED));
        assert!(!c[59].1.add_modifier.contains(Modifier::REVERSED));
        let (a, q, _) = pair(&[(&r2, t0), (&r2, t0 + TICK)]);
        assert_eq!(a, q, "a reset already there is history");
    })
}

#[test]
fn a_spark_lands_in_its_chips_share_of_the_grid() {
    tc(|| {
        let t0 = 5 * SEC;
        let base = Bus::new();
        let mut s = base.clone();
        s.event(Event::Spark {
            chip: Some(chip(1)),
            kind: "vLLM.log".into(),
        });
        let (a, _, _) = pair(&[(&base, t0), (&s, t0 + TICK)]);
        let found: Vec<(usize, usize)> = cells(&a)
            .iter()
            .enumerate()
            .flat_map(|(y, r)| {
                r.iter()
                    .enumerate()
                    .filter(|(_, c)| c.0 == '✦')
                    .map(move |(x, _)| (x, y))
            })
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        let (x, y) = found[0];
        assert!(
            (60..120).contains(&x) && (2..LANE_OUT - 1).contains(&y),
            "{x},{y}"
        );
    })
}

#[test]
fn an_estimated_model_has_a_tilde() {
    tc(|| {
        let mut e = Bus::new();
        e.sample(
            &EntityKey::model("m"),
            tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
            Value::F64(12.0),
            Provenance::Estimated,
        )
        .event(Event::ModelReady {
            model: EntityKey::model("m"),
            chips: vec![chip(0)],
        });
        let mut m = Bus::new();
        m.ready("m", &[0], Some(12.0));
        let (a, _, _) = pair(&[(&e, 5 * SEC)]);
        let (b, _, _) = pair(&[(&m, 5 * SEC)]);
        assert!(
            row_text(&a, MODEL_ROW).contains("~m ready 12t/s"),
            "{}",
            row_text(&a, MODEL_ROW)
        );
        assert!(
            row_text(&b, MODEL_ROW).contains(" m ready 12t/s"),
            "{}",
            row_text(&b, MODEL_ROW)
        );
    })
}

/// Contract rule 4 through the read path an attached TUI uses: a real
/// `ReadView` limited to this TUI's two chips. A model, requests, a spark,
/// a reset and DRAM readings on the host's own chip draw nothing.
#[test]
fn attached_through_only_chips_a_hosts_chip_draws_nothing() {
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    tc(|| {
        let (mut a, b) = view();
        let (mut q, _) = view();
        let host = BusHost::new(tt_toplike_bus::BusConfig::default(), &b).unwrap();
        let bus = host.bus().clone();
        let hv = BusView::Local(Box::new(host));
        let cache = ReadCache::new();
        let local: Arc<BTreeSet<EntityKey>> = Arc::new([chip(0), chip(1)].into_iter().collect());
        let read = || hv.read(&cache).only_chips(local.clone());
        let p = |e: Event| {
            bus.publish("test", Payload::Event(e));
        };
        let foreign = EntityKey::chip("0000:ee:00.0");
        let ecc = |v: u64| {
            bus.publish(
                "test",
                Payload::Sample(Sample {
                    entity: EntityKey::dram_channel(&foreign, 1),
                    signal: crate::bus::signals::DRAM_ECC_CORR_RD.into(),
                    value: Value::U64(v),
                    prov: Provenance::Measured,
                }),
            );
        };
        let theirs = EntityKey::model("theirs");
        ecc(1);
        let r = read();
        let w = Clocked(&r, 5 * SEC + 1 * TICK);
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));

        p(Event::ModelLoading {
            model: theirs.clone(),
            chips: vec![foreign.clone()],
            progress: Some(0.5),
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 4,
        });
        p(Event::Spark {
            chip: Some(foreign.clone()),
            kind: "vLLM.log".into(),
        });
        p(Event::Reset {
            chips: vec![foreign.clone()],
        });
        p(Event::EccTick {
            chip: foreign.clone(),
            channel: 1,
        });
        ecc(2);
        let r = read();
        let w = Clocked(&r, 5 * SEC + 2 * TICK);
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));
        assert!(a.bus_fx().is_empty(), "{:?}", a.bus_fx());
        assert_eq!(a.render(&b), q.render(&b));

        p(Event::ModelLoading {
            model: theirs.clone(),
            chips: vec![foreign.clone(), chip(0)],
            progress: Some(0.5),
        });
        let r = read();
        let w = Clocked(&r, 5 * SEC + 3 * TICK);
        step(&mut a, &b, &w);
        assert!(a.bus_fx().stream.is_some());
        assert!(row_text(&a.render(&b), MODEL_ROW).contains("theirs loading 50%"));
    })
}

#[test]
fn set_bus_none_clears_the_effects() {
    tc(|| {
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.5));
        let (mut a, b) = view();
        let (mut q, _) = view();
        step(&mut a, &b, &f.view(5 * SEC));
        q.update(&b);
        a.set_bus(None, b.devices());
        assert!(a.bus_fx().is_empty());
        assert_eq!(a.render(&b), q.render(&b));
    })
}

#[test]
fn the_legend_and_explain_text_have_no_right_border() {
    for s in LEGEND_TEXT.iter().chain(EXPLAIN_TEXT) {
        assert!(!s.ends_with('║') && !s.ends_with('│'), "{s}");
    }
}
