// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the declarative widgets and the `DeclarativeViz` renderer.
//!
//! Worlds are built with `testkit::WorldBuilder`. Most tests render a small
//! YAML definition to text (`testkit::render_to_text`) and read the lines;
//! the ones that care about colour render into a `Buffer` and read the cells.

use super::{load, DeclarativeViz, VizDef};
use crate::viz::testkit::{assert_golden, render_to_text, WorldBuilder};
use crate::viz::{History, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect, style::Color};
use std::path::Path;
use tt_toplike_bus::{EntityKey, EntityKind, Event, LocalView};

const REFERENCE_YAML: &str = include_str!("fixtures/reference.yaml");

const CHIP1: &str = "0000:01:00.0";
const CHIP2: &str = "0000:02:00.0";
const CHIP3: &str = "0000:03:00.0";

/// A definition with the given layout (YAML text for the `layout:` items,
/// already indented two spaces).
fn def_with(layout: &str) -> VizDef {
    let text = format!("version: 1\nid: t\ntitle: T\nlayout:\n{layout}");
    load(Path::new("t.yaml"), &text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

/// Render `def` to text in a `w` x `h` frame.
fn text_of(def: VizDef, w: u16, h: u16, view: &LocalView, hist: &History) -> String {
    render_to_text(&mut DeclarativeViz::new(def), w, h, view, hist)
}

/// Render into a buffer so a test can read colours.
fn buffer_of(def: VizDef, w: u16, h: u16, view: &LocalView, hist: &History) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let ctx = VizCtx {
        world: view,
        history: hist,
        tick: 0,
        now_mono_ns: 1_000,
    };
    DeclarativeViz::new(def).render(&mut buf, area, &ctx);
    buf
}

/// The cells of row `y` whose symbol is `sym`, as their foreground colours.
fn colours_of(buf: &Buffer, y: u16, sym: &str) -> Vec<Color> {
    (0..buf.area.width)
        .filter(|&x| buf[(x, y)].symbol() == sym)
        .map(|x| buf[(x, y)].fg)
        .collect()
}

/// A one-gauge, one-line layout reading `power_w` with the given extra YAML
/// lines (indented eight spaces by the caller).
fn gauge_layout(extra: &str) -> VizDef {
    def_with(&format!(
        "  - height: 1\n    cells:\n      - widget: gauge\n        signal: power_w\n        label: P\n{extra}"
    ))
}

#[test]
fn gauge_fills_in_proportion_and_takes_threshold_colour() {
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 90.0).build();
    let def = gauge_layout(
        "        max: 110\n        thresholds:\n          - at: 85\n            color: yellow\n          - at: 100\n            color: red\n",
    );
    let buf = buffer_of(def, 40, 1, &view, &History::default());
    let filled = colours_of(&buf, 0, "█");
    let empty = colours_of(&buf, 0, "░");
    let bar = filled.len() + empty.len();
    assert!(bar > 10, "a bar was drawn: {bar}");
    // 90 has reached 85 but not 100: every filled cell is yellow.
    assert!(filled.iter().all(|c| *c == Color::Yellow), "{filled:?}");
    // Within one cell of 90/110 of the bar.
    let want = bar as f64 * 90.0 / 110.0;
    assert!(
        (filled.len() as f64 - want).abs() <= 1.0,
        "filled {} of {bar}, want about {want:.1}",
        filled.len()
    );
}

#[test]
fn gauge_without_max_scales_to_its_history_maximum_or_to_100() {
    // History 10, 50, 25: the gauge scales to 50, so a reading of 25 is half.
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 25.0).build();
    let mut hist = History::default();
    for v in [10.0, 50.0, 25.0] {
        hist.record(&EntityKey::chip(CHIP1), "power_w", v);
    }
    let buf = buffer_of(gauge_layout(""), 40, 1, &view, &hist);
    let (f, e) = (
        colours_of(&buf, 0, "█").len(),
        colours_of(&buf, 0, "░").len(),
    );
    assert!(f + e > 10, "a bar was drawn");
    assert!(
        ((f * 2) as i64 - (f + e) as i64).abs() <= 1,
        "{f} of {}",
        f + e
    );

    // No history: scale to 100, so a reading of 50 is half and 25 a quarter.
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 50.0).build();
    let buf = buffer_of(gauge_layout(""), 40, 1, &view, &History::default());
    let (f, e) = (
        colours_of(&buf, 0, "█").len(),
        colours_of(&buf, 0, "░").len(),
    );
    assert!(f + e > 10, "a bar was drawn");
    assert!(
        ((f * 2) as i64 - (f + e) as i64).abs() <= 1,
        "{f} of {}",
        f + e
    );
}

#[test]
fn unsorted_thresholds_use_the_highest_one_reached() {
    // Listed 100 red, 50 yellow, 75 green. The highest `at` reached wins,
    // whatever the order in the file.
    let th = "        max: 200\n        thresholds:\n          - at: 100\n            color: red\n          - at: 50\n            color: yellow\n          - at: 75\n            color: green\n";
    for (v, want) in [
        (80.0, Color::Green),
        (60.0, Color::Yellow),
        (120.0, Color::Red),
    ] {
        let view = WorldBuilder::new().sample(CHIP1, "power_w", v).build();
        let buf = buffer_of(gauge_layout(th), 40, 1, &view, &History::default());
        let filled = colours_of(&buf, 0, "█");
        assert!(!filled.is_empty());
        assert!(filled.iter().all(|c| *c == want), "value {v}: {filled:?}");
    }
}

#[test]
fn sparkline_uses_history_series() {
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 7.0).build();
    let mut hist = History::default();
    for v in 0..8 {
        hist.record(&EntityKey::chip(CHIP1), "power_w", v as f64);
    }
    let def =
        def_with("  - height: 1\n    cells:\n      - widget: sparkline\n        signal: power_w\n");
    let text = text_of(def, 30, 1, &view, &hist);
    // 0..7 scaled to its own maximum climbs through all eight block heights.
    assert!(text.contains("▁▂▃▄▅▆▇█"), "{text}");
}

#[test]
fn table_sorts_rows_by_the_named_signal_descending() {
    let view = WorldBuilder::new()
        .sample(CHIP1, "temp_c", 40.0)
        .sample(CHIP2, "temp_c", 70.0)
        .sample(CHIP3, "temp_c", 55.0)
        .build();
    let def = def_with(
        "  - height: 6\n    cells:\n      - widget: table\n        signal: temp_c\n        label: Temp\n",
    );
    let text = text_of(def, 30, 6, &view, &History::default());
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in\n{text}"));
    assert!(at("02:00.0") < at("03:00.0"), "{text}");
    assert!(at("03:00.0") < at("01:00.0"), "{text}");
    assert!(text.contains("70") && text.contains("55") && text.contains("40"));
}

#[test]
fn missing_reading_draws_dashes() {
    // A chip exists (it has another signal) but has no `power_w`.
    let view = WorldBuilder::new().sample(CHIP1, "temp_c", 40.0).build();
    for widget in ["gauge", "text", "sparkline", "heat_strip", "grid", "table"] {
        let def = def_with(&format!(
            "  - height: 3\n    cells:\n      - widget: {widget}\n        signal: power_w\n        label: P\n"
        ));
        let text = text_of(def, 40, 3, &view, &History::default());
        assert!(text.contains("--"), "{widget}:\n{text}");
    }
}

#[test]
fn each_chip_repeats_the_row_per_chip() {
    let view = WorldBuilder::new()
        .sample(CHIP1, "power_w", 1.0)
        .sample(CHIP2, "power_w", 2.0)
        .sample(CHIP3, "power_w", 3.0)
        .build();
    let def = def_with(
        "  - height: 1\n    each: chip\n    cells:\n      - widget: text\n        signal: power_w\n        label: \"chip {chip}\"\n",
    );
    let text = text_of(def, 30, 6, &view, &History::default());
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains("chip 01:00.0"), "{text}");
    assert!(lines[1].contains("chip 02:00.0"), "{text}");
    assert!(lines[2].contains("chip 03:00.0"), "{text}");
    assert!(lines[3].is_empty(), "exactly three rows:\n{text}");
    // Each row reads its own chip.
    assert!(lines[0].contains('1') && lines[2].contains('3'));
}

#[test]
fn empty_world_renders_a_placeholder_line() {
    let view = WorldBuilder::new().build();
    let def = DeclarativeViz::new(load(Path::new("r.yaml"), REFERENCE_YAML).unwrap());
    let mut def = def;
    let text = render_to_text(&mut def, 80, 24, &view, &History::default());
    assert!(text.contains("no chips"), "{text}");
}

#[test]
fn each_with_a_non_chip_widget_kind_draws_a_note() {
    // `each: chip` repeats the row per chip. A cell reading another kind has
    // no defined entity per chip, so it draws a visible note.
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 1.0).build();
    let def = def_with(
        "  - height: 1\n    each: chip\n    cells:\n      - widget: gauge\n        signal: load\n        kind: host\n",
    );
    let text = text_of(def, 40, 2, &view, &History::default());
    assert!(text.contains("each: needs chip"), "{text}");
}

/// A chip-per-row layout over `n` chips, for the cap and clipping tests.
fn many_chips(n: usize) -> LocalView {
    let mut b = WorldBuilder::new();
    for i in 0..n {
        b = b.sample(&format!("0000:{i:02x}:00.0"), "power_w", i as f64);
    }
    b.build()
}

const ONE_LINE_PER_CHIP: &str =
    "  - height: 1\n    each: chip\n    cells:\n      - widget: text\n        signal: power_w\n        label: \"{chip}\"\n";

#[test]
fn per_chip_expansion_is_capped_at_64_instances_with_a_more_line() {
    let text = text_of(
        def_with(ONE_LINE_PER_CHIP),
        40,
        100,
        &many_chips(70),
        &History::default(),
    );
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    // 64 rows were allowed; the last one holds the note, so it counts the
    // row it replaced: 70 chips - 63 shown = 7.
    assert_eq!(lines.len(), 64, "{text}");
    assert!(lines[63].contains("+7 more"), "{}", lines[63]);
    assert!(!lines[62].contains("more"));
}

#[test]
fn rows_cut_by_a_short_area_say_so() {
    let text = text_of(
        def_with(ONE_LINE_PER_CHIP),
        40,
        3,
        &many_chips(5),
        &History::default(),
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[2].contains("+3 more"), "{text}");
    assert!(!lines[0].contains("more") && !lines[1].contains("more"));
}

#[test]
fn oversize_heights_and_tiny_or_zero_areas_do_not_panic() {
    let view = many_chips(3);
    let hist = History::default();
    let reference = load(Path::new("r.yaml"), REFERENCE_YAML).unwrap();
    let huge = def_with(
        "  - height: 65535\n    cells:\n      - widget: table\n        signal: power_w\n  - height: 65535\n    each: chip\n    cells:\n      - widget: gauge\n        signal: power_w\n      - widget: sparkline\n        signal: power_w\n",
    );
    for def in [&reference, &huge] {
        for w in 0..=7u16 {
            for h in 0..=7u16 {
                // The text helper builds a w x h buffer; zero sizes are legal.
                text_of(def.clone(), w, h, &view, &hist);
            }
        }
        // A frame larger than the area, drawn at an offset.
        text_of(def.clone(), 200, 60, &view, &hist);
    }
    // A zero-size area draws nothing even inside a bigger buffer.
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
    let before = buf.clone();
    let ctx = VizCtx {
        world: &view,
        history: &hist,
        tick: 0,
        now_mono_ns: 1,
    };
    let mut viz = DeclarativeViz::new(reference);
    viz.render(&mut buf, Rect::new(3, 1, 0, 4), &ctx);
    viz.render(&mut buf, Rect::new(3, 1, 4, 0), &ctx);
    assert_eq!(buf, before);
}

#[test]
fn nothing_is_drawn_outside_the_area() {
    let view = many_chips(4);
    let hist = History::default();
    let def = load(Path::new("r.yaml"), REFERENCE_YAML).unwrap();
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
    let blank = buf.clone();
    let area = Rect::new(5, 3, 20, 8);
    let ctx = VizCtx {
        world: &view,
        history: &hist,
        tick: 0,
        now_mono_ns: 1,
    };
    DeclarativeViz::new(def).render(&mut buf, area, &ctx);
    for y in 0..20u16 {
        for x in 0..40u16 {
            if !(area.x..area.right()).contains(&x) || !(area.y..area.bottom()).contains(&y) {
                assert_eq!(buf[(x, y)], blank[(x, y)], "cell {x},{y} was touched");
            }
        }
    }
    // And something was drawn inside it.
    assert_ne!(buf, blank);
}

#[test]
fn log_shows_recent_events_filtered_by_the_signal_text() {
    let view = WorldBuilder::new()
        .sample(CHIP1, "power_w", 1.0)
        .event(Event::KernelCompiled {
            chip: None,
            source: "matmul".into(),
        })
        .event(Event::Reset { chips: vec![] })
        .build();
    let layout = |sig: &str| {
        def_with(&format!(
            "  - height: 4\n    cells:\n      - widget: log\n        signal: \"{sig}\"\n"
        ))
    };
    let hist = History::default();
    let only = text_of(layout("kernel"), 60, 4, &view, &hist);
    assert!(
        only.contains("KernelCompiled") && !only.contains("Reset"),
        "{only}"
    );
    // `*` shows everything, oldest first, so the newest is last.
    let all = text_of(layout("*"), 60, 4, &view, &hist);
    assert!(
        all.find("KernelCompiled").unwrap() < all.find("Reset").unwrap(),
        "{all}"
    );
}

#[test]
fn grid_colours_one_cell_per_chip() {
    let view = WorldBuilder::new()
        .sample(CHIP1, "temp_c", 50.0)
        .sample(CHIP2, "temp_c", 90.0)
        .sample(CHIP3, "temp_c", 60.0)
        .build();
    let def = def_with(
        "  - height: 1\n    cells:\n      - widget: grid\n        signal: temp_c\n        max: 100\n        thresholds:\n          - at: 80\n            color: red\n",
    );
    let buf = buffer_of(def, 20, 1, &view, &History::default());
    // Each cell is two columns wide.
    let cells = colours_of(&buf, 0, "█");
    assert_eq!(cells.len(), 6, "{cells:?}");
    assert_eq!(cells.iter().filter(|c| **c == Color::Red).count(), 2);
}

#[test]
fn heat_strip_colours_each_history_sample() {
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 10.0).build();
    let mut hist = History::default();
    for v in [10.0, 10.0, 90.0, 90.0, 10.0] {
        hist.record(&EntityKey::chip(CHIP1), "power_w", v);
    }
    let def = def_with(
        "  - height: 1\n    cells:\n      - widget: heat_strip\n        signal: power_w\n        max: 100\n        thresholds:\n          - at: 50\n            color: red\n",
    );
    let buf = buffer_of(def, 30, 1, &view, &hist);
    let cells = colours_of(&buf, 0, "█");
    assert_eq!(cells.len(), 5, "{cells:?}");
    assert_eq!(cells.iter().filter(|c| **c == Color::Red).count(), 2);
}

#[test]
fn text_widget_shows_label_and_value_in_the_threshold_colour() {
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 42.5).build();
    let def = def_with(
        "  - height: 1\n    cells:\n      - widget: text\n        signal: power_w\n        label: Power\n        thresholds:\n          - at: 40\n            color: red\n",
    );
    let text = text_of(def.clone(), 30, 1, &view, &History::default());
    assert!(text.starts_with("Power: 42.5"), "{text}");
    let buf = buffer_of(def, 30, 1, &view, &History::default());
    assert_eq!(buf[(7, 0)].fg, Color::Red); // the `4` of 42.5
}

#[test]
fn weights_split_the_row_width() {
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 7.0).build();
    let def = def_with(
        "  - height: 1\n    cells:\n      - weight: 1\n        widget: text\n        signal: power_w\n        label: A\n      - weight: 3\n        widget: text\n        signal: power_w\n        label: B\n",
    );
    let text = text_of(def, 40, 1, &view, &History::default());
    // Width 40 split 1:3 puts the second cell at column 10.
    assert_eq!(text.find('B'), Some(10), "{text}");
}

#[test]
fn trait_metadata_comes_from_the_definition_with_generated_fallbacks() {
    let reference = DeclarativeViz::new(load(Path::new("r.yaml"), REFERENCE_YAML).unwrap());
    assert_eq!(reference.id(), "reference-board");
    assert_eq!(reference.title(), "Reference Board");
    assert_eq!(reference.min_size(), (60, 12));
    assert_eq!(reference.legend().len(), 3);
    assert!(reference.explain().starts_with("A small board"));
    // Signals are the union, each once.
    let sigs: Vec<(EntityKind, String)> = reference
        .signals()
        .into_iter()
        .map(|s| (s.kind, s.name))
        .collect();
    assert_eq!(
        sigs,
        vec![
            (EntityKind::Chip, "power_w".to_string()),
            (EntityKind::Chip, "temp_c".to_string())
        ]
    );

    // No min_size, legend or explain in the file: defaults and fallbacks.
    let bare = DeclarativeViz::new(gauge_layout(
        "        max: 300\n        thresholds:\n          - at: 200\n            color: yellow\n          - at: 260\n            color: red\n",
    ));
    assert_eq!(bare.min_size(), (40, 10));
    let legend = bare.legend().join("\n");
    assert!(
        legend.contains("power_w") && legend.contains("200") && legend.contains("260"),
        "{legend}"
    );
    let explain = bare.explain();
    assert!(
        explain.contains("power_w") && explain.contains("yellow"),
        "{explain}"
    );
}

#[test]
fn reference_fixture_golden_80x24() {
    let mut b = WorldBuilder::new();
    for (i, id) in [CHIP1, CHIP2, CHIP3].iter().enumerate() {
        b = b.sample(id, "power_w", [210.0, 95.0, 270.0][i]).sample(
            id,
            "temp_c",
            [61.0, 48.0, 83.0][i],
        );
    }
    let view = b.build();
    let mut hist = History::default();
    for (i, id) in [CHIP1, CHIP2, CHIP3].iter().enumerate() {
        for step in 0..40 {
            let wave = ((step as f64) / 4.0 + i as f64).sin() * 40.0;
            hist.record(
                &EntityKey::chip(id),
                "power_w",
                160.0 + 20.0 * i as f64 + wave,
            );
        }
    }
    let mut viz = DeclarativeViz::new(load(Path::new("r.yaml"), REFERENCE_YAML).unwrap());
    let text = render_to_text(&mut viz, 80, 24, &view, &hist);
    assert_golden(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/viz/declarative/golden/reference_80x24.txt"
        )),
        &text,
    );
}
