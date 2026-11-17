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
        // A frame much larger than the content.
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
        only.contains("KernelCompiled: matmul") && !only.contains("Reset"),
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
fn log_lines_are_short_readable_text_not_debug_output() {
    let chip = EntityKey::chip(CHIP2);
    let view = WorldBuilder::new()
        .event(Event::KernelCompiled {
            chip: Some(chip.clone()),
            source: "matmul".into(),
        })
        .event(Event::LinkChanged {
            chip: chip.clone(),
            mask: 0b1011,
        })
        .event(Event::RequestsCompleted {
            model: EntityKey::model("llama"),
            n: 7,
        })
        .event(Event::Reset {
            chips: vec![chip.clone(), EntityKey::chip(CHIP1)],
        })
        // No hand-written line: the generic fallback names the event.
        .event(Event::EccTick { chip, channel: 3 })
        .build();
    let def = def_with("  - height: 5\n    cells:\n      - widget: log\n        signal: \"*\"\n");
    let text = text_of(def, 70, 5, &view, &History::default());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "KernelCompiled: matmul on 02:00.0");
    assert_eq!(lines[1], "LinkChanged: 02:00.0 links 0x000b (3 up)");
    assert_eq!(lines[2], "RequestsCompleted: 7 on llama");
    assert_eq!(lines[3], "Reset: 2 chips");
    assert!(lines[4].starts_with("EccTick"), "{}", lines[4]);
    // None of the hand-written lines show struct syntax.
    assert!(!lines[..4]
        .iter()
        .any(|l| l.contains('{') || l.contains("Some(")));
}

#[test]
fn log_shows_only_the_newest_matches_that_fit() {
    let mut b = WorldBuilder::new();
    for i in 0..30 {
        b = b.event(Event::KernelCompiled {
            chip: None,
            source: format!("k{i}"),
        });
        b = b.event(Event::Reset { chips: vec![] });
    }
    let def = def_with("  - height: 2\n    cells:\n      - widget: log\n        signal: kernel\n");
    let text = text_of(def, 40, 2, &b.build(), &History::default());
    assert_eq!(text, "KernelCompiled: k28\nKernelCompiled: k29");
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

/// Three chips with power and temperature, and a power history, for the
/// reference fixture's golden and colour tests.
fn reference_world() -> (LocalView, History) {
    let mut b = WorldBuilder::new();
    for (i, id) in [CHIP1, CHIP2, CHIP3].iter().enumerate() {
        b = b.sample(id, "power_w", [210.0, 95.0, 270.0][i]).sample(
            id,
            "temp_c",
            [61.0, 48.0, 83.0][i],
        );
    }
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
    (b.build(), hist)
}

#[test]
fn reference_fixture_golden_80x24() {
    let (view, hist) = reference_world();
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

/// The golden is text only; this pins the colours on the same frame. The
/// gauge reads chip 1 (210 W: past the yellow threshold at 200, short of
/// the red one at 260). The three sparkline rows read 210, 95 and 270 W:
/// yellow, no threshold (the primary colour) and red.
#[test]
fn reference_fixture_colours_follow_the_thresholds() {
    use crate::ui::colors;
    let (view, hist) = reference_world();
    let def = load(Path::new("r.yaml"), REFERENCE_YAML).unwrap();
    let buf = buffer_of(def, 80, 24, &view, &hist);
    // Gauge: height 3, so the bar is on line 1.
    let filled = colours_of(&buf, 1, "█");
    assert!(!filled.is_empty());
    assert!(filled.iter().all(|c| *c == Color::Yellow), "{filled:?}");
    // Sparklines: lines 3, 4, 5. Collect the colours of their block cells.
    let blocks = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let row_colours =
        |y: u16| -> Vec<Color> { blocks.iter().flat_map(|b| colours_of(&buf, y, b)).collect() };
    for (y, want) in [(3, Color::Yellow), (4, colors::primary()), (5, Color::Red)] {
        let c = row_colours(y);
        assert!(!c.is_empty(), "line {y} has a sparkline");
        assert!(
            c.iter().all(|x| *x == want),
            "line {y}: {c:?}, want {want:?}"
        );
    }
}

// ---------------------------------------------------------------------
// Fix round 1: cost, colour defaults, edge values
// ---------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::Mutex;
use tt_toplike_bus::{HeaderKind, ModelView, Reading, Value, WorldRead};

/// A world that counts how often `entities` is called, per kind.
struct Counting {
    inner: LocalView,
    calls: Mutex<HashMap<EntityKind, usize>>,
}

impl Counting {
    fn new(inner: LocalView) -> Self {
        Self {
            inner,
            calls: Mutex::new(HashMap::new()),
        }
    }
    fn calls(&self, kind: EntityKind) -> usize {
        *self.calls.lock().unwrap().get(&kind).unwrap_or(&0)
    }
}

impl WorldRead for Counting {
    fn seq(&self) -> u64 {
        self.inner.seq()
    }
    fn models(&self) -> Vec<ModelView> {
        self.inner.models()
    }
    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView> {
        self.inner.model_on_chip(chip)
    }
    fn entities(&self, kind: EntityKind) -> Vec<EntityKey> {
        *self.calls.lock().unwrap().entry(kind).or_insert(0) += 1;
        self.inner.entities(kind)
    }
    fn reading(&self, e: &EntityKey, s: &str) -> Option<Reading> {
        self.inner.reading(e, s)
    }
    fn events_since(&self, seq: u64) -> Vec<(u64, Event)> {
        self.inner.events_since(seq)
    }
    fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.inner.reset_seq(chip)
    }
    fn header_kind(&self) -> HeaderKind {
        self.inner.header_kind()
    }
    fn now_mono_ns(&self) -> u64 {
        self.inner.now_mono_ns()
    }
}

/// Render `def` against `world` (any `WorldRead`) at 60 x 40.
fn render_on(def: VizDef, world: &dyn WorldRead) {
    let area = Rect::new(0, 0, 60, 40);
    let mut buf = Buffer::empty(area);
    let hist = History::default();
    let ctx = VizCtx {
        world,
        history: &hist,
        tick: 0,
        now_mono_ns: 1,
    };
    DeclarativeViz::new(def).render(&mut buf, area, &ctx);
}

#[test]
fn entity_lists_are_fetched_once_per_kind_per_frame() {
    // Ten chip widgets and a table, an `each` row of three, and a host gauge.
    let mut layout = String::new();
    for _ in 0..10 {
        layout.push_str(
            "  - height: 1\n    cells:\n      - widget: gauge\n        signal: power_w\n",
        );
    }
    layout.push_str("  - height: 4\n    cells:\n      - widget: table\n        signal: power_w\n");
    layout.push_str("  - height: 1\n    each: chip\n    cells:\n      - widget: text\n        signal: power_w\n");
    layout.push_str("  - height: 1\n    cells:\n      - widget: gauge\n        signal: load\n        kind: host\n");
    let w = Counting::new(many_chips(3));
    render_on(def_with(&layout), &w);
    assert_eq!(w.calls(EntityKind::Chip), 1, "one chip list for the frame");
    assert_eq!(w.calls(EntityKind::Host), 1);
}

#[test]
fn a_log_only_definition_never_lists_entities() {
    let def = def_with("  - height: 3\n    cells:\n      - widget: log\n        signal: \"*\"\n");
    let w = Counting::new(many_chips(3));
    render_on(def, &w);
    assert_eq!(w.calls(EntityKind::Chip), 0);
    assert_eq!(w.calls(EntityKind::Host), 0);
}

#[test]
fn a_widget_that_needs_a_chip_note_does_not_fetch_entities() {
    // In an `each` row a host-kind widget draws the note; it needs no list.
    let def = def_with("  - height: 1\n    each: chip\n    cells:\n      - widget: gauge\n        signal: x\n        kind: host\n");
    let w = Counting::new(many_chips(2));
    render_on(def, &w);
    assert_eq!(w.calls(EntityKind::Host), 0);
}

/// Cells of `sym` in row 0, as colours, for a one-line widget over `view`.
fn one_line(widget: &str, extra: &str, view: &LocalView, hist: &History) -> Buffer {
    buffer_of(
        def_with(&format!(
            "  - height: 1\n    cells:\n      - widget: {widget}\n        signal: power_w\n{extra}"
        )),
        30,
        1,
        view,
        hist,
    )
}

#[test]
fn grid_and_heat_strip_without_max_use_one_neutral_colour() {
    use crate::ui::colors;
    // A peak reading: with no `max` it would be 100 percent of its own
    // series and used to come out red.
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 300.0).build();
    let mut hist = History::default();
    for v in [10.0, 150.0, 300.0] {
        hist.record(&EntityKey::chip(CHIP1), "power_w", v);
    }
    for widget in ["grid", "heat_strip"] {
        let buf = one_line(widget, "", &view, &hist);
        let c = colours_of(&buf, 0, "█");
        assert!(!c.is_empty(), "{widget}");
        assert!(c.iter().all(|x| *x == colors::primary()), "{widget}: {c:?}");
    }
    // Thresholds alone recolour only what reaches them; the rest stays
    // neutral (no ramp without a `max`).
    let th = "        thresholds:\n          - at: 200\n            color: red\n";
    let c = colours_of(&one_line("heat_strip", th, &view, &hist), 0, "█");
    assert_eq!(c.len(), 3);
    assert_eq!(c.iter().filter(|x| **x == Color::Red).count(), 1);
    assert_eq!(c.iter().filter(|x| **x == colors::primary()).count(), 2);
}

#[test]
fn grid_and_heat_strip_with_max_use_the_heat_ramp() {
    use crate::ui::colors;
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 90.0).build();
    let mut hist = History::default();
    for v in [10.0, 60.0, 90.0] {
        hist.record(&EntityKey::chip(CHIP1), "power_w", v);
    }
    // Grid: 90 of 100 is over four fifths: the error colour.
    let g = colours_of(
        &one_line("grid", "        max: 100\n", &view, &hist),
        0,
        "█",
    );
    assert!(g.iter().all(|c| *c == colors::error()), "{g:?}");
    // Heat strip: 10 -> success, 60 -> warning, 90 -> error.
    let h = colours_of(
        &one_line("heat_strip", "        max: 100\n", &view, &hist),
        0,
        "█",
    );
    assert_eq!(
        h,
        vec![colors::success(), colors::warning(), colors::error()]
    );
}

/// A 40 x 10 buffer with a 20 x 3 area at (5, 2). Renders `def` over `view`
/// and `hist` into the area and asserts no cell outside it changed.
fn assert_contained(def: VizDef, view: &LocalView, hist: &History) {
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
    let blank = buf.clone();
    let area = Rect::new(5, 2, 20, 3);
    let ctx = VizCtx {
        world: view,
        history: hist,
        tick: 0,
        now_mono_ns: 1,
    };
    DeclarativeViz::new(def).render(&mut buf, area, &ctx);
    for y in 0..10u16 {
        for x in 0..40u16 {
            let inside = (5..25).contains(&x) && (2..5).contains(&y);
            if !inside {
                assert_eq!(buf[(x, y)], blank[(x, y)], "cell {x},{y} was touched");
            }
        }
    }
}

#[test]
fn huge_text_values_do_not_wrap_widths_or_spill() {
    // 70,000 columns of text is more than a u16 holds.
    let huge = "x".repeat(70_000);
    let view = WorldBuilder::new()
        .entity_sample(EntityKey::chip(CHIP1), "power_w", Value::Text(huge))
        .build();
    let hist = History::default();
    for widget in ["gauge", "text", "sparkline", "heat_strip", "table", "grid"] {
        let def = def_with(&format!(
            "  - height: 3\n    cells:\n      - widget: {widget}\n        signal: power_w\n        label: L\n"
        ));
        assert_contained(def, &view, &hist);
    }
    // And it still draws something in a plain frame.
    let def =
        def_with("  - height: 1\n    cells:\n      - widget: text\n        signal: power_w\n");
    let text = text_of(def, 20, 1, &view, &hist);
    assert!(text.starts_with("power_w: xxxx"), "{text}");
}

#[test]
fn long_and_wide_glyph_labels_stay_inside_their_cell() {
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 5.0).build();
    let mut hist = History::default();
    hist.record(&EntityKey::chip(CHIP1), "power_w", 5.0);
    let long = "L".repeat(300);
    // Full-width glyphs are two columns each; an odd cut must not leave half
    // a glyph past the edge.
    let wide = "漢".repeat(100);
    for label in [long, wide] {
        for widget in ["gauge", "text", "sparkline", "heat_strip", "table", "grid"] {
            let def = def_with(&format!(
                "  - height: 3\n    cells:\n      - widget: {widget}\n        signal: power_w\n        label: \"{label}\"\n"
            ));
            assert_contained(def, &view, &hist);
        }
    }
}

#[test]
fn negative_zero_prints_as_zero() {
    let def =
        def_with("  - height: 1\n    cells:\n      - widget: text\n        signal: power_w\n");
    for (v, want) in [
        (-0.0, "power_w: 0"),
        (-0.04, "power_w: 0.0"),
        (-3.0, "power_w: -3"),
    ] {
        let view = WorldBuilder::new().sample(CHIP1, "power_w", v).build();
        let text = text_of(def.clone(), 30, 1, &view, &History::default());
        assert_eq!(text, want);
    }
}

#[test]
fn flat_zero_and_single_sample_series_draw_without_nan() {
    let spark = |series: &[f64], now: f64| {
        let view = WorldBuilder::new().sample(CHIP1, "power_w", now).build();
        let mut hist = History::default();
        for v in series {
            hist.record(&EntityKey::chip(CHIP1), "power_w", *v);
        }
        let def = def_with(
            "  - height: 1\n    cells:\n      - widget: sparkline\n        signal: power_w\n",
        );
        text_of(def, 20, 1, &view, &hist)
    };
    // Flat at 5: its own maximum, so every sample is full height.
    assert_eq!(spark(&[5.0, 5.0, 5.0], 5.0), "███ 5");
    // All zero: no scale at all, so the lowest block, not a blank or a panic.
    assert_eq!(spark(&[0.0, 0.0, 0.0], 0.0), "▁▁▁ 0");
    // One sample.
    assert_eq!(spark(&[7.0], 7.0), "█ 7");
}

#[test]
fn gauge_scale_falls_back_to_100_when_history_is_all_zero() {
    // No `max`, and a history whose largest value is 0: the scale is 100.
    let view = WorldBuilder::new().sample(CHIP1, "power_w", 50.0).build();
    let mut hist = History::default();
    for _ in 0..3 {
        hist.record(&EntityKey::chip(CHIP1), "power_w", 0.0);
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
}

#[test]
fn a_host_only_definition_on_an_empty_world_has_no_chips_placeholder() {
    let view = WorldBuilder::new().build();
    let def = def_with(
        "  - height: 1\n    cells:\n      - widget: gauge\n        signal: load\n        kind: host\n        label: Load\n",
    );
    let text = text_of(def, 40, 2, &view, &History::default());
    assert!(!text.contains("no chips"), "{text}");
    assert!(text.contains("--"), "{text}");
}

#[test]
fn an_area_larger_than_the_buffer_is_cut_to_the_buffer() {
    let view = many_chips(3);
    let hist = History::default();
    let ctx = VizCtx {
        world: &view,
        history: &hist,
        tick: 0,
        now_mono_ns: 1,
    };
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
    let def = load(Path::new("r.yaml"), REFERENCE_YAML).unwrap();
    // The area reaches far past the buffer on every side but the top left.
    DeclarativeViz::new(def.clone()).render(&mut buf, Rect::new(0, 0, 500, 200), &ctx);
    // An area that starts outside the buffer draws nothing.
    let before = buf.clone();
    DeclarativeViz::new(def).render(&mut buf, Rect::new(100, 100, 20, 5), &ctx);
    assert_eq!(buf, before);
}
