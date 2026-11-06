// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the YAML example `viz/examples/thermal-board.yaml`.
//!
//! The example is also the template `viz new <name> --yaml` copies, so a broken file
//! must fail CI. Every test reads the real file from the repository, not a
//! copy of its text.

use super::{load, ColorName, DeclarativeViz, VizDef};
use crate::viz::declarative::loader::Loader;
use crate::viz::registry::Registry;
use crate::viz::testkit::{assert_golden, render_to_text, WorldBuilder};
use crate::viz::{History, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect, style::Color};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const TEMP: &str = "tt_asic_temperature_celsius";
const POWER: &str = "tt_tdp_w";

/// The repository's `viz/` folder.
fn viz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("viz")
}

fn example_path() -> PathBuf {
    viz_dir().join("examples").join("thermal-board.yaml")
}

fn golden(name: &str) -> PathBuf {
    viz_dir().join("examples").join("golden").join(name)
}

/// The example, loaded through the same entry point the loader uses.
fn thermal_board() -> VizDef {
    let path = example_path();
    let text = std::fs::read_to_string(&path).expect("example file is readable");
    load(&path, &text).unwrap_or_else(|e| panic!("{e}"))
}

/// A world with one chip per `(bus id, temperature, power)`.
fn world(chips: &[(&str, f64, f64)]) -> tt_toplike_bus::LocalView {
    let mut b = WorldBuilder::new();
    for (id, t, p) in chips {
        b = b.sample(id, TEMP, *t).sample(id, POWER, *p);
    }
    b.build()
}

const THREE: [(&str, f64, f64); 3] = [
    ("0000:01:00.0", 61.0, 120.0),
    ("0000:02:00.0", 48.0, 95.0),
    ("0000:03:00.0", 83.0, 270.0),
];

fn render_text(chips: &[(&str, f64, f64)], w: u16, h: u16) -> String {
    let view = world(chips);
    let mut viz = DeclarativeViz::new(thermal_board());
    render_to_text(&mut viz, w, h, &view, &History::new(8))
}

fn render_buffer(chips: &[(&str, f64, f64)], w: u16, h: u16) -> Buffer {
    let view = world(chips);
    let hist = History::new(8);
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let ctx = VizCtx {
        world: &view,
        history: &hist,
        tick: 0,
        now_mono_ns: view_now(&view),
    };
    DeclarativeViz::new(thermal_board()).render(&mut buf, area, &ctx);
    buf
}

fn view_now(v: &tt_toplike_bus::LocalView) -> u64 {
    use tt_toplike_bus::WorldRead;
    v.now_mono_ns()
}

#[test]
fn thermal_board_loads_and_validates() {
    let def = thermal_board();
    assert_eq!(def.id, "thermal-board");
    assert_eq!(def.title, "Thermal Board");
    assert!(def.min_size.is_some());
    assert!(!def.legend.is_empty());
    assert!(def.explain.is_some());

    // The file also satisfies the published JSON Schema, the way the schema
    // tests check the reference definition.
    let text = std::fs::read_to_string(example_path()).unwrap();
    let value: serde_json::Value = serde_yaml_ng::from_str(&text).unwrap();
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(viz_dir().join("schema/viz-v1.schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
    if let Err(e) = validator.validate(&value) {
        panic!("thermal-board.yaml fails the JSON Schema: {e}");
    }

    // Only the two signals confirmed on the bus are read.
    let names: Vec<String> = DeclarativeViz::new(def)
        .signals()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec![TEMP.to_string(), POWER.to_string()]);
}

#[test]
fn thermal_board_golden_80x24() {
    assert_golden(
        &golden("thermal-board-80x24.txt"),
        &render_text(&THREE, 80, 24),
    );
}

#[test]
fn thermal_board_empty_world_placeholder() {
    let text = render_text(&[], 80, 24);
    assert!(text.starts_with("no chips"), "{text}");
    assert_golden(&golden("thermal-board-empty-80x24.txt"), &text);
}

/// Documents the host's rule (`viz_host::cycle_list`): an entry joins the view
/// cycle when it is enabled by default or its id is in the enabled set.
/// `viz_host` is private to the TUI module, so the rule is restated here and
/// is not tested against the real function. What this file pins is the
/// loader's `enabled_by_default = false` flag for `viz/examples/`.
fn in_cycle(e: &crate::viz::registry::VizEntry, enabled: &BTreeSet<String>) -> bool {
    e.enabled_by_default || enabled.contains(&e.id)
}

#[test]
fn thermal_board_is_hidden_until_enabled() {
    // A private registry, so the global one is untouched.
    let mut reg = Registry::new();
    let mut loader = Loader::new(vec![viz_dir()]);
    loader.scan_with(&mut reg);
    let e = reg
        .entries()
        .into_iter()
        .find(|e| e.id == "thermal-board")
        .expect("the loader finds viz/examples/thermal-board.yaml");
    assert!(!e.enabled_by_default);
    assert_eq!(e.status, crate::viz::registry::EntryStatus::Ok);

    // An empty `[viz]` leaves it out; `enable = ["thermal-board"]` puts it in.
    let none = BTreeSet::new();
    assert!(!in_cycle(&e, &none));
    let enabled: BTreeSet<String> = ["thermal-board".to_string()].into();
    assert!(in_cycle(&e, &enabled));

    // Everything the loader found in the repo's `viz/`: exactly one entry,
    // the example, from `viz/examples`, and nothing failed.
    let all = reg.entries();
    assert!(
        all.iter()
            .all(|e| e.status == crate::viz::registry::EntryStatus::Ok),
        "{all:?}"
    );
    let from_examples: Vec<&str> = all
        .iter()
        .filter(|e| {
            matches!(&e.source, crate::viz::registry::Source::File(p)
                if p.starts_with(viz_dir().join("examples")))
        })
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(from_examples, ["thermal-board"]);
}

/// The cells of the bar line of chip `i` (chip rows are two lines high, the
/// bar on the second), left half only (the temperature gauge), as the
/// foreground colours of the filled `█` blocks.
fn temp_bar_colours(buf: &Buffer, chip: u16) -> Vec<Color> {
    let y = chip * 2 + 1;
    (0..40)
        .filter(|&x| buf[(x, y)].symbol() == "█")
        .map(|x| buf[(x, y)].fg)
        .collect()
}

#[test]
fn thermal_board_colours_follow_the_thresholds() {
    let buf = render_buffer(
        &[
            ("0000:01:00.0", 90.0, 100.0),
            ("0000:02:00.0", 75.0, 100.0),
            ("0000:03:00.0", 50.0, 100.0),
        ],
        80,
        24,
    );
    let hot = temp_bar_colours(&buf, 0);
    let warm = temp_bar_colours(&buf, 1);
    let cool = temp_bar_colours(&buf, 2);
    assert!(!hot.is_empty() && !warm.is_empty() && !cool.is_empty());
    assert!(hot.iter().all(|c| *c == ColorName::Red.color()), "{hot:?}");
    assert!(
        warm.iter().all(|c| *c == ColorName::Yellow.color()),
        "{warm:?}"
    );
    // Below every threshold the gauge keeps its normal colour.
    assert!(
        cool.iter().all(|c| *c == ColorName::Primary.color()),
        "{cool:?}"
    );
    // The three colours differ, so the checks above cannot all pass on a
    // theme where red, yellow and primary happen to match.
    assert_ne!(hot[0], warm[0]);
    assert_ne!(warm[0], cool[0]);
}

#[test]
fn thermal_board_table_lists_the_hottest_chip_first() {
    let text = render_text(
        &[
            ("0000:01:00.0", 52.0, 250.0),
            ("0000:02:00.0", 88.0, 90.0),
            ("0000:03:00.0", 64.0, 150.0),
        ],
        80,
        24,
    );
    let lines: Vec<&str> = text.lines().collect();
    let header = lines
        .iter()
        .position(|l| l.contains("Hottest first"))
        .expect("table header");
    let order: Vec<&str> = (1..=3)
        .map(|i| lines[header + i].split_whitespace().next().unwrap())
        .collect();
    assert_eq!(order, ["02:00.0", "03:00.0", "01:00.0"], "{text}");
    assert!(lines[header + 1].contains("88"));
    // The power table has its own order: 01 (250), 03 (150), 02 (90).
    let power: Vec<&str> = (1..=3)
        .map(|i| {
            let right = &lines[header + i][40..];
            right.split_whitespace().next().unwrap()
        })
        .collect();
    assert_eq!(power, ["01:00.0", "03:00.0", "02:00.0"], "{text}");
}

#[test]
fn thermal_board_with_many_chips_says_how_many_are_cut() {
    // 12 chips at two lines each need 24 lines; the tables get none, so the
    // last line becomes `+N more`. No panic, nothing outside the frame.
    let chips: Vec<(String, f64, f64)> = (1..=12)
        .map(|i| (format!("0000:{i:02x}:00.0"), 40.0 + i as f64, 100.0))
        .collect();
    let refs: Vec<(&str, f64, f64)> = chips.iter().map(|(s, t, p)| (s.as_str(), *t, *p)).collect();
    let text = render_text(&refs, 80, 24);
    assert_eq!(text.lines().count(), 24);
    let last = text.lines().last().unwrap();
    // All 12 chip rows want 24 lines, so the table row gets none (1 row not
    // drawn). The last chip row gives up its second line to the note, which
    // still draws its label line, so N is 1.
    assert_eq!(last, "+1 more", "{text}");

    // Five chips fit with the tables: no "+N more", all five in the table.
    let five = &refs[..5];
    let text = render_text(five, 80, 24);
    assert!(!text.contains("more"), "{text}");
    for (id, _, _) in five {
        let short = &id[5..];
        // Once as a gauge label, once in each of the two tables.
        assert!(text.matches(short).count() >= 3, "{short} missing\n{text}");
    }
}
