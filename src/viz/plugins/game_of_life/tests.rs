// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for `game-of-life`: the rules, the feeding from power, colours,
//! sizes and the edge cases (empty world, tiny area, a reading that is missing).

use super::{make, Life, ENABLED_BY_DEFAULT, POWER, STEP_NS, TEMP};
use crate::ui::colors;
use crate::viz::testkit::{render_to_text, WorldBuilder};
use crate::viz::{registry, History, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect};
use tt_toplike_bus::{EntityKey, LocalView};

const A: &str = "0000:01:00.0";
const B: &str = "0000:02:00.0";

fn ctx<'a>(view: &'a LocalView, hist: &'a History, now: u64) -> VizCtx<'a> {
    VizCtx { world: view, history: hist, tick: 0, now_mono_ns: now }
}

#[test]
fn registers_under_its_folder_id() {
    let _ = crate::viz::init();
    assert_eq!(make().id(), "game-of-life");
    const { assert!(!ENABLED_BY_DEFAULT) };
    assert!(registry::handle_of("game-of-life").is_some());
}

#[test]
fn a_blinker_flips_and_a_block_stays() {
    let mut l = Life::new();
    l.resize(10, 10);
    for (x, y) in [(1, 2), (2, 2), (3, 2)] {
        l.set(x, y); // blinker
    }
    for (x, y) in [(6, 6), (7, 6), (6, 7), (7, 7)] {
        l.set(x, y); // block
    }
    l.step();
    assert!(l.alive(2, 1) && l.alive(2, 2) && l.alive(2, 3));
    assert!(!l.alive(1, 2) && !l.alive(3, 2));
    for (x, y) in [(6, 6), (7, 6), (6, 7), (7, 7)] {
        assert!(l.alive(x, y));
    }
    assert_eq!(l.population(), 7);
}

#[test]
fn a_glider_wraps_round_the_torus() {
    let mut l = Life::new();
    l.resize(8, 8);
    l.glider(0, 0);
    for _ in 0..32 {
        l.step(); // four generations move it one cell diagonally: 8 cells in 32
    }
    // After 32 generations on an 8 x 8 torus the glider is back where it began.
    for (dx, dy) in super::GLIDER {
        assert!(l.alive(dx, dy));
    }
    assert_eq!(l.population(), 5);
}

#[test]
fn resizing_keeps_the_cells_that_fit() {
    let mut l = Life::new();
    l.resize(6, 6);
    l.set(1, 1);
    l.set(5, 5);
    l.resize(4, 4);
    assert!(l.alive(1, 1));
    assert_eq!(l.population(), 1);
}

#[test]
fn a_busy_chip_feeds_its_own_band_and_an_idle_one_does_not() {
    let mut l = Life::new();
    l.resize(40, 20);
    for _ in 0..200 {
        l.feed(0, 2, 1.0, false); // left band at full activity
        l.feed(1, 2, 0.0, false); // right band idle
    }
    let (left, right): (usize, usize) = (0..20)
        .map(|y| {
            (
                (0..20).filter(|&x| l.alive(x, y)).count(),
                (20..40).filter(|&x| l.alive(x, y)).count(),
            )
        })
        .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    assert!(left > 100, "left band got {left}");
    assert_eq!(right, 0);
}

#[test]
fn a_surge_drops_a_glider_in_the_chips_band() {
    let mut l = Life::new();
    l.resize(40, 20);
    l.feed(1, 2, 0.0, true);
    assert_eq!(l.population(), 5);
    assert!((0..20).all(|x| (0..20).all(|y| !l.alive(x, y))), "nothing in the left band");
}

#[test]
fn generations_follow_the_bus_clock_and_never_run_twice_for_one_time() {
    let view = WorldBuilder::new().sample(A, POWER, 50.0).sample(A, TEMP, 40.0).build();
    let hist = History::default();
    let mut viz = Life::new();
    let area = Rect::new(0, 0, 30, 10);
    let mut buf = Buffer::empty(area);
    viz.render(&mut buf, area, &ctx(&view, &hist, 1_000_000_000));
    assert_eq!(viz.generation(), 0);
    viz.render(&mut buf, area, &ctx(&view, &hist, 1_000_000_000 + STEP_NS));
    assert_eq!(viz.generation(), 1);
    viz.render(&mut buf, area, &ctx(&view, &hist, 1_000_000_000 + STEP_NS)); // same time
    assert_eq!(viz.generation(), 1);
    // A long pause runs at most four generations, then the clock restarts.
    viz.render(&mut buf, area, &ctx(&view, &hist, 100_000_000_000));
    assert_eq!(viz.generation(), 5);
    viz.render(&mut buf, area, &ctx(&view, &hist, 100_000_000_000 + STEP_NS));
    assert_eq!(viz.generation(), 6);
}

#[test]
fn the_same_readings_at_the_same_times_draw_the_same_board() {
    let run = || {
        let view = WorldBuilder::new()
            .sample(A, POWER, 120.0)
            .sample(A, TEMP, 50.0)
            .sample(B, POWER, 30.0)
            .sample(B, TEMP, 50.0)
            .build();
        let hist = History::default();
        let mut viz = Life::new();
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        for i in 1..=40u64 {
            viz.render(&mut buf, area, &ctx(&view, &hist, i * STEP_NS));
        }
        (0..12).flat_map(|y| (0..40).map(move |x| (x, y))).map(|(x, y)| buf[(x, y)].symbol().to_string()).collect::<String>()
    };
    assert_eq!(run(), run());
}

#[test]
fn a_power_rise_over_a_known_range_adds_cells_to_the_band() {
    // Two samples of the same chip: low, then high. The second frame sees a
    // surge and a high activity, so the band gains cells.
    let low = WorldBuilder::new().sample(A, POWER, 20.0).sample(A, TEMP, 40.0).build();
    let high = WorldBuilder::new().sample(A, POWER, 120.0).sample(A, TEMP, 40.0).build();
    let hist = History::default();
    let mut viz = Life::new();
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    viz.render(&mut buf, area, &ctx(&low, &hist, STEP_NS));
    // Establish a range: the first frame only starts the clock.
    viz.render(&mut buf, area, &ctx(&low, &hist, 2 * STEP_NS));
    let before = viz.population();
    viz.render(&mut buf, area, &ctx(&high, &hist, 3 * STEP_NS));
    assert!(viz.population() > before, "{} then {}", before, viz.population());
}

#[test]
fn colours_follow_the_temperature_thresholds() {
    let l = Life::new();
    assert_eq!(l.colour(Some(50.0)), colors::success());
    assert_eq!(l.colour(Some(75.0)), colors::warning());
    assert_eq!(l.colour(Some(90.0)), colors::error());
    assert_eq!(l.colour(None), colors::primary());
}

#[test]
fn hot_c_setting_applies_and_a_bad_value_is_ignored() {
    let mut viz = make();
    let cfg: toml_edit::DocumentMut = "hot_c = 60".parse().unwrap();
    viz.configure(cfg.as_table());
    assert!(viz.help().contains("now 60"), "{}", viz.help());
    for bad in ["hot_c = \"x\"", "hot_c = -1", "hot_c = nan", "hot_c = true", "hot_c = 0"] {
        let cfg: toml_edit::DocumentMut = bad.parse().unwrap();
        viz.configure(cfg.as_table());
        assert!(viz.help().contains("now 60"), "{bad}");
    }
}

#[test]
fn an_empty_world_says_so_and_does_not_panic() {
    let view = WorldBuilder::new().build();
    let text = render_to_text(&mut *make(), 60, 12, &view, &History::default());
    assert!(text.contains("no chips"), "{text}");
}

#[test]
fn tiny_and_empty_areas_do_not_panic() {
    let view = WorldBuilder::new().sample(A, POWER, 50.0).build();
    let hist = History::default();
    for (w, h) in [(0, 0), (1, 1), (2, 2), (3, 1), (200, 1), (1, 80)] {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let mut viz = make();
        for i in 1..6u64 {
            viz.render(&mut buf, area, &ctx(&view, &hist, i * STEP_NS));
        }
    }
}

#[test]
fn a_missing_power_reading_feeds_nothing() {
    let view = WorldBuilder::new().sample(A, TEMP, 50.0).build();
    let hist = History::default();
    let mut viz = Life::new();
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    for i in 1..30u64 {
        viz.render(&mut buf, area, &ctx(&view, &hist, i * STEP_NS));
    }
    assert_eq!(viz.population(), 0);
}

#[test]
fn many_chips_get_bands_that_cover_the_board_without_gaps() {
    let mut l = Life::new();
    l.resize(37, 10);
    let n = 5;
    let mut next = 0;
    for i in 0..n {
        let (a, b) = l.band(i, n);
        assert_eq!(a, next);
        assert!(b >= a);
        next = b;
    }
    assert_eq!(next, 37);
    let _ = EntityKey::chip(A);
}
