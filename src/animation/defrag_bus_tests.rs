// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Defrag frames with and without the signal bus.
//!
//! The golden hashes pin the frames Defrag drew before it read the bus.
//! They were taken by running `golden_hashes` against the commit before the
//! bus port (the commit that added this file holds only these tests), so a
//! frame drawn from an empty world must hash to exactly these values.

use super::*;
use crate::animation::bus_test_support::{frame_hash, tc, Fixed};

/// The layouts the golden hashes pin: (chips, width, height).
const LAYOUTS: [(usize, usize, usize); 3] = [(1, 60, 18), (2, 60, 18), (2, 160, 50)];
/// The updates after which a frame is hashed. Chip 0 loads for a while,
/// so the later frames show its fill grown.
const HASHED_AT: [u32; 4] = [1, 10, 60, 400];

/// Hashes of the frames of a Defrag view in `layout` over [`Fixed`], after
/// each update in [`HASHED_AT`]. `before_update` runs before every update.
pub(super) fn hashes_with(
    (n, w, h): (usize, usize, usize),
    mut before_update: impl FnMut(&mut DefragVis, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(n);
        let mut d = DefragVis::new(w, h);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut d, &backend, i);
            d.update(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&d.render(&backend)));
            }
        }
        out
    })
}

/// The frames as Defrag drew them before the bus port, for each of
/// [`LAYOUTS`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 4]; 3] = [
    [
        6378651105407557510,
        1473458929850560376,
        2672466645569721241,
        6670050726542021795,
    ],
    [
        17050773165888384831,
        11425570284173348045,
        5660999393278833181,
        3652699994344175700,
    ],
    [
        14788702879550107618,
        6915677417392135091,
        15995812354245785336,
        16781590828447983459,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui defrag_golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn defrag_golden_hashes() {
    for l in LAYOUTS {
        println!("{l:?}: {:?}", hashes_with(l, |_, _, _| {}));
    }
}

#[test]
fn the_defrag_frames_with_no_bus_are_the_pinned_frames() {
    for (i, l) in LAYOUTS.into_iter().enumerate() {
        assert_eq!(hashes_with(l, |_, _, _| {}), GOLDEN[i].to_vec(), "{l:?}");
    }
}

// ---- The bus ---------------------------------------------------------------

use crate::animation::bus_test_support::{cells, chip, fg, row_text, Bus, SEC, TICK};
use crate::animation::die3d::overlay::{FLASH_NS, LOAD_PURPLE, SERVE_TEAL, SETTLE_NS, SPARK_NS};
use ratatui::style::Modifier;
use tt_toplike_bus::{EntityKey, Event, Provenance, Value, WorldRead};

/// The Defrag view the bus tests draw: two chips at 160 x 50. Each device's
/// share is 79 columns; device `d` starts at column `d * 80`. Line 2 is the
/// label row, line 3 the power bar, and the channel rows start at line 4,
/// five rows per channel.
const W: usize = 160;
const H: usize = 50;
const GRID_W: usize = 79;
const LABEL_ROW: usize = 2;
const POWER_ROW: usize = 3;
const FIRST_CH_ROW: usize = 4;
const ROWS_PER_CH: usize = 5;

fn share(d: usize) -> std::ops::Range<usize> {
    let a = d * (GRID_W + 1);
    a..a + GRID_W
}

fn defrag() -> (DefragVis, Fixed) {
    (DefragVis::new(W, H), Fixed::new(2))
}

/// One Defrag update with `world` on a local bus.
fn step(d: &mut DefragVis, b: &Fixed, world: &dyn WorldRead) {
    d.set_bus(Some(world), b, true);
    d.update(b);
}

fn phase(d: &DefragVis, idx: usize) -> Phase {
    d.devices[&idx].phase
}

fn fills(d: &DefragVis, idx: usize) -> Vec<f32> {
    d.devices[&idx].channels.iter().map(|c| c.fill).collect()
}

/// Cells of the channel rows of device `dev` whose foreground is `c`.
fn count_fg(frame: &[Line<'static>], dev: usize, c: [u8; 3]) -> usize {
    let f = cells(frame);
    (FIRST_CH_ROW..FIRST_CH_ROW + 8 * ROWS_PER_CH)
        .map(|r| {
            share(dev)
                .filter(|&x| f[r].get(x).map(|s| s.1.fg) == Some(fg(c)))
                .count()
        })
        .sum()
}

/// A few updates with an empty world first, so the chips have state.
fn warmed(b: &Fixed, t0: u64) -> DefragVis {
    let (mut d, _) = defrag();
    for i in 0..3 {
        step(&mut d, b, &Bus::new().view(t0 + i * TICK));
    }
    d
}

#[test]
fn an_empty_world_draws_the_pinned_defrag_frames() {
    // An empty world, and one holding only a reading Defrag does not draw,
    // give exactly the frames Defrag drew before the bus.
    let mut quiet = Bus::new();
    quiet.sample(
        &chip(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    for world in [Bus::new(), quiet] {
        for (i, l) in LAYOUTS.into_iter().enumerate() {
            let got = hashes_with(l, |d, b, n| {
                d.set_bus(Some(&world.view(5 * SEC + n as u64 * TICK)), b, true)
            });
            assert_eq!(got, GOLDEN[i].to_vec(), "{l:?}");
        }
    }
}

#[test]
fn loading_with_progress_fills_every_channel_to_that_share() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        // Chip 1 is running by its power readings; the bus says it loads.
        let mut d = warmed(&b, t0);
        assert_eq!(phase(&d, 1), Phase::Running);
        let mut f = Bus::new();
        f.loading("m", &[1], Some(0.5));
        step(&mut d, &b, &f.view(t0 + SEC));
        assert_eq!(phase(&d, 1), Phase::Dma);
        assert!(
            fills(&d, 1).iter().all(|x| (*x - 0.5).abs() < 1e-6),
            "{:?}",
            fills(&d, 1)
        );
        let label = row_text(&d.render(&b), LABEL_ROW);
        assert!(label.contains(" 50%"), "{label}");
        assert!(label.contains("DMA"), "{label}");
        // Further on, the fill follows the figure.
        let mut g = Bus::new();
        g.loading("m", &[1], Some(0.8));
        step(&mut d, &b, &g.view(t0 + 2 * SEC));
        assert!(fills(&d, 1).iter().all(|x| (*x - 0.8).abs() < 1e-6));
        // Chip 0 has no model and keeps its own phase.
        assert_eq!(phase(&d, 0), Phase::Dma);
        assert!(fills(&d, 0).iter().all(|x| *x < 0.5));
    })
}

#[test]
fn compiling_holds_the_blocks_under_a_moving_scan() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut d = warmed(&b, t0);
        let before = fills(&d, 1);
        let mut f = Bus::new();
        f.loading("m", &[1], None).phase("m", "compiling");
        step(&mut d, &b, &f.view(t0 + SEC));
        assert_eq!(phase(&d, 1), Phase::Compile);
        assert_eq!(fills(&d, 1), before, "the blocks hold");
        let frame = d.render(&b);
        assert!(row_text(&frame, LABEL_ROW).contains("COMPILE"));
        let scan = |frame: &[Line<'static>]| {
            let f = cells(frame);
            share(1)
                .filter(|&x| f[FIRST_CH_ROW][x].1.fg == fg(LOAD_PURPLE))
                .collect::<Vec<_>>()
        };
        let first = scan(&frame);
        assert_eq!(first.len(), 2, "{first:?}");
        assert_eq!(count_fg(&frame, 0, LOAD_PURPLE), 0);
        step(&mut d, &b, &f.view(t0 + SEC + SEC / 2));
        let later = scan(&d.render(&b));
        assert_eq!(later.len(), 2);
        assert_ne!(first, later, "the scan moves");
    })
}

#[test]
fn ready_fills_the_blocks_and_washes_them_teal_on_the_change() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut d = warmed(&b, t0);
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.4));
        step(&mut d, &b, &f.view(t0 + SEC));
        f.ready("m", &[0], Some(20.0));
        step(&mut d, &b, &f.view(t0 + SEC + TICK));
        assert_eq!(phase(&d, 0), Phase::Running);
        assert!(fills(&d, 0).iter().all(|x| *x >= 0.999));
        let washed = count_fg(&d.render(&b), 0, SERVE_TEAL);
        let (mut q, _) = defrag();
        for i in 0..3 {
            step(&mut q, &b, &Bus::new().view(t0 + i * TICK));
        }
        step(&mut q, &b, &f.view(t0 + SEC + TICK));
        // A model first seen ready fills the blocks with no wash.
        assert_eq!(phase(&q, 0), Phase::Running);
        let unwashed = count_fg(&q.render(&b), 0, SERVE_TEAL);
        assert!(washed > 0 && unwashed == 0, "{washed} {unwashed}");
        // The label sits on the power bar row.
        let power = row_text(&d.render(&b), POWER_ROW);
        assert!(power.contains("m ready 20t/s"), "{power}");
        step(&mut d, &b, &f.view(t0 + SEC + TICK + SETTLE_NS));
        assert_eq!(count_fg(&d.render(&b), 0, SERVE_TEAL), 0, "the wash fades");
    })
}

#[test]
fn a_model_that_goes_down_or_away_evicts_its_chip_only() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut up = Bus::new();
        up.ready("m", &[0], None);
        let mut down = up.clone();
        down.phase("m", "down");
        let mut gone = up.clone();
        gone.gone("m");
        for after in [&down, &gone] {
            let mut d = warmed(&b, t0);
            step(&mut d, &b, &up.view(t0 + SEC));
            step(&mut d, &b, &after.view(t0 + SEC + TICK));
            assert_eq!(phase(&d, 0), Phase::Deconstructing);
            assert_ne!(phase(&d, 1), Phase::Deconstructing);
            assert!(row_text(&d.render(&b), LABEL_ROW).contains("EVICT"));
        }
        // First sight of a down model, and an unload while away: no evict.
        let mut d = warmed(&b, t0);
        step(&mut d, &b, &down.view(t0 + SEC));
        assert_ne!(phase(&d, 0), Phase::Deconstructing, "first sight");
        let mut d = warmed(&b, t0);
        step(&mut d, &b, &up.view(t0 + 4 * TICK));
        step(&mut d, &b, &down.view(t0 + 20 * SEC));
        assert_ne!(phase(&d, 0), Phase::Deconstructing, "away");
    })
}

#[test]
fn a_model_that_goes_down_while_loading_drains_what_it_had() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut f = Bus::new();
        f.loading("m", &[1], Some(0.3));
        let mut down = f.clone();
        down.phase("m", "down");
        let mut d = warmed(&b, t0);
        step(&mut d, &b, &f.view(t0 + SEC));
        step(&mut d, &b, &down.view(t0 + SEC + TICK));
        assert_eq!(phase(&d, 1), Phase::Deconstructing);
        assert!(fills(&d, 1).iter().all(|x| *x <= 0.3), "{:?}", fills(&d, 1));
    })
}

#[test]
fn a_ready_model_with_no_chips_evicts_every_chip_only_on_a_local_bus() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut up = Bus::new();
        up.ready("m", &[], None);
        let mut gone = up.clone();
        gone.gone("m");
        for (local, want) in [(true, true), (false, false)] {
            let (mut d, _) = defrag();
            for (i, w) in [&Bus::new(), &up, &gone].into_iter().enumerate() {
                d.set_bus(Some(&w.view(t0 + i as u64 * TICK)), &b, local);
                d.update(&b);
            }
            let evicted = [0, 1].map(|i| phase(&d, i) == Phase::Deconstructing);
            assert_eq!(evicted, [want, want], "local {local}");
        }
    })
}

#[test]
fn requests_on_a_running_chip_throw_a_burst_and_history_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut base = Bus::new();
        base.ready("m", &[1], None);
        let mut with = base.clone();
        with.requests("m", 5);
        let mut d = warmed(&b, t0);
        step(&mut d, &b, &base.view(t0 + SEC));
        // Let the power-spike bursts (if any) expire.
        for i in 1..40 {
            step(&mut d, &b, &base.view(t0 + SEC + i * TICK));
        }
        let quiet = d.devices[&1].scatter_bursts.len();
        d.set_bus(Some(&with.view(t0 + SEC + 40 * TICK)), &b, true);
        assert!(d.devices[&1].scatter_bursts.len() > quiet, "a burst");
        assert!(d.devices[&0].scatter_bursts.is_empty());

        // History: the requests were there at the first read.
        let mut h = warmed(&b, t0);
        h.set_bus(None, &b, true);
        h.set_bus(Some(&with.view(t0 + SEC)), &b, true);
        assert!(h.devices[&1].scatter_bursts.is_empty(), "history");
    })
}

#[test]
fn a_spark_lights_one_block_in_its_chip() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut with = Bus::new();
        with.ready("m", &[1], None);
        let base = with.clone();
        with.spark(1, "vLLM.log");
        let mut d = warmed(&b, t0);
        step(&mut d, &b, &base.view(t0 + SEC));
        step(&mut d, &b, &with.view(t0 + SEC + TICK));
        let f = cells(&d.render(&b));
        let lit: Vec<(usize, usize)> = (FIRST_CH_ROW..FIRST_CH_ROW + 8 * ROWS_PER_CH)
            .flat_map(|r| (0..W).map(move |x| (r, x)))
            .filter(|&(r, x)| {
                f[r].get(x)
                    .is_some_and(|c| c.0 == '█' && c.1.fg == fg(SERVE_TEAL))
            })
            .collect();
        assert_eq!(lit.len(), 1, "{lit:?}");
        assert!(share(1).contains(&lit[0].1));
        step(&mut d, &b, &with.view(t0 + SEC + TICK + SPARK_NS));
        let f = cells(&d.render(&b));
        assert!(f[lit[0].0][lit[0].1].0 != '█' || f[lit[0].0][lit[0].1].1.fg != fg(SERVE_TEAL));
    })
}

#[test]
fn an_estimated_model_label_starts_with_a_tilde() {
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
        let (mut d, b) = defrag();
        step(&mut d, &b, &f.view(5 * SEC));
        let row = row_text(&d.render(&b), POWER_ROW);
        assert!(row.contains("~mock ready 20t/s"), "{row}");

        let mut f = Bus::new();
        f.ready("real", &[0], Some(20.0));
        let (mut d, b) = defrag();
        step(&mut d, &b, &f.view(5 * SEC));
        let row = row_text(&d.render(&b), POWER_ROW);
        assert!(
            row.contains("real ready 20t/s") && !row.contains('~'),
            "{row}"
        );
    })
}

#[test]
fn a_reset_flashes_the_chips_label_row() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut base = Bus::new();
        base.reset(1);
        let mut again = base.clone();
        again.reset(1);
        let (_, b) = defrag();
        let reversed = |d: &DefragVis| {
            cells(&d.render(&b))[LABEL_ROW]
                .iter()
                .filter(|c| c.1.add_modifier.contains(Modifier::REVERSED))
                .count()
        };
        // A reset already in the world at the first read is history.
        let (mut h, _) = defrag();
        step(&mut h, &b, &base.view(t0));
        assert_eq!(reversed(&h), 0, "history");
        step(&mut h, &b, &base.view(t0 + TICK));
        assert_eq!(reversed(&h), 0, "history, read again");

        let mut d = warmed(&b, t0);
        step(&mut d, &b, &base.view(t0 + SEC));
        step(&mut d, &b, &again.view(t0 + SEC + TICK));
        let f = cells(&d.render(&b));
        let cols: Vec<usize> = (0..f[LABEL_ROW].len())
            .filter(|&x| f[LABEL_ROW][x].1.add_modifier.contains(Modifier::REVERSED))
            .collect();
        // The label row is not padded to the device's share, so the flash
        // is found by its text: chip 1's label, and none of chip 0's.
        let lit: String = cols.iter().map(|&x| f[LABEL_ROW][x].0).collect();
        assert!(lit.contains("BH1") && !lit.contains("BH0"), "{lit}");
        step(&mut d, &b, &again.view(t0 + SEC + TICK + FLASH_NS));
        assert_eq!(reversed(&d), 0);
    })
}

#[test]
fn a_world_naming_a_chip_this_tui_lacks_draws_nothing_for_it() {
    tc(|| {
        let t0 = 5 * SEC;
        let (_, b) = defrag();
        let mut f = Bus::new();
        f.ready("far", &[5], Some(120.0));
        let mut g = f.clone();
        g.requests("far", 4)
            .spark(5, "vLLM.log")
            .reset(5)
            .gone("far");
        let mut d = warmed(&b, t0);
        let mut q = warmed(&b, t0);
        for (i, w) in [&f, &g].into_iter().enumerate() {
            let t = t0 + SEC + i as u64 * TICK;
            step(&mut d, &b, &w.view(t));
            step(&mut q, &b, &Bus::new().view(t));
        }
        assert!(d.bus_fx.chips.is_empty(), "{:?}", d.bus_fx);
        assert_eq!(d.render(&b), q.render(&b));
    })
}

/// Contract rule 4 through the read path an attached TUI uses: a real
/// `ReadView` limited to this TUI's two chips, with `set_bus` told the bus
/// is attached. A host model on its own chip, and its unload, change
/// nothing here; the same model on chip 1 holds chip 1 and evicts it on
/// unload.
#[test]
fn attached_through_only_chips_a_hosts_chip_draws_nothing() {
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tt_toplike_bus::Payload;

    tc(|| {
        let (_, b) = defrag();
        let host = BusHost::new(tt_toplike_bus::BusConfig::default(), &b).unwrap();
        let bus = host.bus().clone();
        let view = BusView::Local(Box::new(host));
        let cache = ReadCache::new();
        let local: Arc<BTreeSet<EntityKey>> = Arc::new([chip(0), chip(1)].into_iter().collect());
        let read = || view.read(&cache).only_chips(local.clone());
        let p = |e: Event| {
            bus.publish("test", Payload::Event(e));
        };
        let theirs = EntityKey::model("theirs");
        let foreign = EntityKey::chip("0000:ee:00.0");
        let (mut d, _) = defrag();
        let (mut q, _) = defrag();
        let att = |d: &mut DefragVis, w: &dyn WorldRead| {
            d.set_bus(Some(w), &b, false);
            d.update(&b);
        };
        for _ in 0..3 {
            let w = read();
            att(&mut d, &w);
            att(&mut q, &Bus::new().view(w.now_mono_ns()));
        }
        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone()],
        });
        let w = read();
        att(&mut d, &w);
        att(&mut q, &Bus::new().view(w.now_mono_ns()));
        p(Event::ModelGone {
            model: theirs.clone(),
        });
        let w = read();
        att(&mut d, &w);
        att(&mut q, &Bus::new().view(w.now_mono_ns()));
        assert_eq!(d.render(&b), q.render(&b));

        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone(), chip(1)],
        });
        att(&mut d, &read());
        assert_eq!(phase(&d, 1), Phase::Running);
        assert!(row_text(&d.render(&b), POWER_ROW).contains("theirs ready"));
        p(Event::ModelGone {
            model: theirs.clone(),
        });
        att(&mut d, &read());
        assert_eq!(phase(&d, 1), Phase::Deconstructing);
        assert_ne!(phase(&d, 0), Phase::Deconstructing);
    })
}
