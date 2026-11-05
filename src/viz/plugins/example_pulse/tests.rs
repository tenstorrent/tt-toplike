// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for `example-pulse`: registration, golden frames, colours, edge cases.

use super::{make, sparkline, ENABLED_BY_DEFAULT, POWER, TEMP};
use crate::ui::colors;
use crate::viz::testkit::{assert_golden, render_to_text, WorldBuilder};
use crate::viz::{registry, History, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect, style::Color};
use std::path::PathBuf;
use tt_toplike_bus::{EntityKey, LocalView};

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/viz/plugins/example_pulse/golden")
        .join(name)
}

const A: &str = "0000:01:00.0";
const B: &str = "0000:02:00.0";
const C: &str = "0000:03:00.0";

/// Three chips (cool, warm, hot) with a rising power history each.
fn three_chips() -> (LocalView, History) {
    let mut w = WorldBuilder::new();
    let mut h = History::default();
    for (bus, power, temp) in [(A, 60.0, 50.0), (B, 95.5, 75.0), (C, 120.0, 90.0)] {
        w = w.sample(bus, POWER, power).sample(bus, TEMP, temp);
        for i in 0..30 {
            h.record(&EntityKey::chip(bus), POWER, power - 20.0 + (i % 10) as f64 * 3.0);
        }
    }
    (w.build(), h)
}

/// Render into a buffer and return it, so a test can read cell colours.
fn render_buf(viz: &mut dyn Viz, w: u16, h: u16, view: &LocalView, hist: &History) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let ctx = VizCtx { world: view, history: hist, tick: 0, now_mono_ns: 1 };
    viz.render(&mut buf, area, &ctx);
    buf
}

/// The colour of the first sparkline block on row `y`.
fn spark_colour(buf: &Buffer, y: u16) -> Color {
    (0..buf.area.width)
        .map(|x| &buf[(x, y)])
        .find(|c| "▁▂▃▄▅▆▇█".contains(c.symbol()))
        .expect("a sparkline block on the row")
        .fg
}

#[test]
fn registers_under_its_folder_id() {
    let _ = crate::viz::init();
    assert_eq!(make().id(), "example-pulse");
    const { assert!(!ENABLED_BY_DEFAULT) };
    let handle = registry::handle_of("example-pulse").expect("registered");
    assert!(registry::entries().iter().any(|e| e.handle == handle));
}

#[test]
fn golden_80x24_with_three_chips() {
    let (view, hist) = three_chips();
    let text = render_to_text(&mut *make(), 80, 24, &view, &hist);
    assert_golden(&golden("80x24.txt"), &text);
}

#[test]
fn golden_40x10_small() {
    let (view, hist) = three_chips();
    let text = render_to_text(&mut *make(), 40, 10, &view, &hist);
    assert_golden(&golden("40x10.txt"), &text);
}

#[test]
fn empty_world_shows_a_placeholder() {
    let view = WorldBuilder::new().build();
    let text = render_to_text(&mut *make(), 80, 24, &view, &History::default());
    assert!(text.contains("no chips"), "{text}");
    assert_golden(&golden("empty-80x24.txt"), &text);
}

#[test]
fn colours_follow_the_default_thresholds() {
    let (view, hist) = three_chips();
    let buf = render_buf(&mut *make(), 80, 24, &view, &hist);
    assert_eq!(spark_colour(&buf, 0), colors::success()); // 50 C
    assert_eq!(spark_colour(&buf, 1), colors::warning()); // 75 C
    assert_eq!(spark_colour(&buf, 2), colors::error()); // 90 C
}

#[test]
fn hot_c_setting_moves_the_colour_threshold() {
    let view = WorldBuilder::new().sample(A, TEMP, 65.0).build();
    let mut hist = History::default();
    for i in 0..10 {
        hist.record(&EntityKey::chip(A), POWER, i as f64);
    }
    let mut viz = make();
    // Default hot_c is 85, so yellow starts at 70: 65 C is green.
    let before = spark_colour(&render_buf(&mut *viz, 80, 6, &view, &hist), 0);
    assert_eq!(before, colors::success());
    let cfg: toml_edit::DocumentMut = "hot_c = 60".parse().unwrap();
    viz.configure(cfg.as_table());
    let after = spark_colour(&render_buf(&mut *viz, 80, 6, &view, &hist), 0);
    assert_eq!(after, colors::error());
}

#[test]
fn wrong_typed_hot_c_leaves_the_default() {
    for bad in ["hot_c = \"60\"", "hot_c = -5", "hot_c = nan", "hot_c = inf", "hot_c = true", "hot_c = 0"] {
        let mut viz = make();
        let cfg: toml_edit::DocumentMut = bad.parse().unwrap();
        viz.configure(cfg.as_table());
        // help() reports the value in force.
        assert!(viz.help().contains("now 85"), "{bad}: {}", viz.help());
    }
    // An integer is a valid value.
    let mut viz = make();
    let cfg: toml_edit::DocumentMut = "hot_c = 70".parse().unwrap();
    viz.configure(cfg.as_table());
    assert!(viz.help().contains("now 70"));
}

#[test]
fn zero_and_small_areas_do_not_panic_or_overflow() {
    let (view, hist) = three_chips();
    let ctx = VizCtx { world: &view, history: &hist, tick: 0, now_mono_ns: 1 };
    // Zero-size areas draw nothing.
    for area in [Rect::new(0, 0, 0, 0), Rect::new(0, 0, 10, 0), Rect::new(0, 0, 0, 5)] {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        make().render(&mut buf, area, &ctx);
        assert_eq!(buf, Buffer::empty(Rect::new(0, 0, 80, 24)));
    }
    // An area smaller than the buffer, offset, and narrower than a row:
    // nothing outside it changes.
    for (w, h) in [(1, 1), (5, 2), (20, 3), (39, 1)] {
        let full = Rect::new(0, 0, 80, 24);
        let area = Rect::new(3, 2, w, h);
        let mut buf = Buffer::empty(full);
        make().render(&mut buf, area, &ctx);
        for y in 0..24 {
            for x in 0..80 {
                if !area.contains(ratatui::layout::Position::new(x, y)) {
                    assert_eq!(buf[(x, y)].symbol(), " ", "wrote outside at {x},{y}");
                }
            }
        }
    }
}

#[test]
fn a_missing_reading_draws_dashes() {
    let view = WorldBuilder::new()
        .sample(A, TEMP, 55.0) // no power
        .sample(B, POWER, 80.0) // no temperature
        .build();
    let text = render_to_text(&mut *make(), 80, 6, &view, &History::default());
    let rows: Vec<&str> = text.lines().collect();
    assert!(rows[0].contains("--") && rows[0].contains("55 C"), "{}", rows[0]);
    assert!(rows[1].contains("80.0 W") && rows[1].contains("--"), "{}", rows[1]);
}

/// A chip with a power history but no temperature reading draws its
/// sparkline in the primary colour, and the legend and explain text say so.
#[test]
fn a_chip_with_no_temperature_uses_the_primary_colour() {
    let view = WorldBuilder::new().sample(A, POWER, 80.0).build();
    let mut hist = History::default();
    for i in 0..10 {
        hist.record(&EntityKey::chip(A), POWER, i as f64);
    }
    let mut viz = make();
    let buf = render_buf(&mut *viz, 80, 6, &view, &hist);
    assert_eq!(spark_colour(&buf, 0), colors::primary());
    assert!(
        viz.legend().iter().any(|l| l.contains("primary colour")),
        "legend: {:?}",
        viz.legend()
    );
    assert!(viz.explain().contains("primary colour"), "{}", viz.explain());
}

/// A flat series draws the middle block, padded on the left.
#[test]
fn sparkline_of_a_flat_series_sits_in_the_middle() {
    assert_eq!(sparkline(&[7.0, 7.0, 7.0], 5), "  \u{2584}\u{2584}\u{2584}");
}

/// One sample is flat too, and sits at the right edge.
#[test]
fn sparkline_of_a_single_sample() {
    assert_eq!(sparkline(&[5.0], 4), "   \u{2584}");
}

/// No samples give blanks, and a zero width gives nothing.
#[test]
fn sparkline_of_nothing_is_blank() {
    assert_eq!(sparkline(&[], 3), "   ");
    assert_eq!(sparkline(&[1.0, 2.0], 0), "");
}

/// A NaN sample does not panic or move the scale: it draws the lowest block
/// and the finite samples keep their places.
#[test]
fn sparkline_survives_a_nan_sample() {
    assert_eq!(sparkline(&[1.0, f64::NAN, 3.0], 3), "\u{2581}\u{2581}\u{2588}");
    assert_eq!(sparkline(&[f64::NAN, f64::NAN], 2).chars().count(), 2);
}
