// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the declarative viz definition: parsing, validation, limits and
//! the JSON Schema file.

use super::parse;
use super::schema::{CellDef, Height, VizDef, WidgetDef};
use super::validate::{validate, MAX_FILE_BYTES, MAX_ROWS, MAX_WEIGHT, MAX_WIDGETS};
use std::path::Path;

const REFERENCE_YAML: &str = include_str!("fixtures/reference.yaml");
const REFERENCE_JSON: &str = include_str!("fixtures/reference.json");
const UNKNOWN_WIDGET: &str = include_str!("fixtures/unknown_widget.yaml");
const SCHEMA: &str = include_str!("../../../viz/schema/viz-v1.schema.json");

fn reference() -> VizDef {
    parse(Path::new("reference.yaml"), REFERENCE_YAML).expect("reference parses")
}

/// A minimal valid YAML definition with `cells` gauge widgets in one row.
fn yaml_with_widgets(n: usize) -> String {
    let mut s =
        String::from("version: 1\nid: many\ntitle: Many\nlayout:\n  - height: 1\n    cells:\n");
    for _ in 0..n {
        s.push_str("      - widget: gauge\n        signal: power_w\n");
    }
    s
}

#[test]
fn parses_the_reference_yaml() {
    let def = reference();
    assert_eq!(def.id, "reference-board");
    assert_eq!(def.legend.len(), 3);
    assert_eq!(def.min_size, Some((60, 12)));
    assert_eq!(def.layout.len(), 3);
    assert_eq!(def.layout[0].height, Height::Rows(3));
    assert!(matches!(def.layout[0].cells[0].widget, WidgetDef::Gauge(_)));
    assert_eq!(def.layout[1].height, Height::Fill);
    assert!(def.layout[1].each.is_some());
    assert!(matches!(
        def.layout[1].cells[0].widget,
        WidgetDef::Sparkline(_)
    ));
    assert!(matches!(def.layout[2].cells[0].widget, WidgetDef::Table(_)));
    validate(&def).expect("reference validates");
}

#[test]
fn same_def_from_json_matches_yaml() {
    let from_json = parse(Path::new("reference.json"), REFERENCE_JSON).expect("json parses");
    assert_eq!(from_json, reference());
}

#[test]
fn unknown_widget_is_an_error_naming_the_widget() {
    let err = parse(Path::new("bad.yaml"), UNKNOWN_WIDGET)
        .unwrap_err()
        .to_string();
    assert!(err.contains("pie_chart"), "{err}");
    assert!(err.contains("bad.yaml"), "{err}");
}

#[test]
fn bad_id_is_an_error() {
    for bad in [
        "Bad_Id",
        "has space",
        "-lead",
        "trail-",
        "dou--ble",
        "",
        "1abc",
    ] {
        let text = format!("version: 1\nid: \"{bad}\"\ntitle: T\nlayout: []\n");
        let def = parse(Path::new("x.yaml"), &text).expect("parses");
        let err = validate(&def).expect_err(bad).to_string();
        assert!(err.contains("id"), "{bad}: {err}");
    }
}

#[test]
fn version_two_is_rejected_with_a_message_naming_version() {
    let text = "version: 2\nid: v-two\ntitle: T\nlayout: []\n";
    let def = parse(Path::new("x.yaml"), text).expect("parses");
    let err = validate(&def).unwrap_err().to_string();
    assert!(err.contains("version"), "{err}");
}

#[test]
fn oversize_file_is_rejected_before_parsing() {
    // 257 KiB of text that is not valid YAML: if the size check ran after
    // parsing, the message would be a syntax error, not a size error.
    let text = "{".repeat(257 * 1024);
    assert!(text.len() > MAX_FILE_BYTES);
    let err = parse(Path::new("big.yaml"), &text).unwrap_err().to_string();
    assert!(err.contains("too large"), "{err}");
    assert!(err.contains("big.yaml"), "{err}");
}

#[test]
fn too_many_widgets_is_rejected() {
    let ok = parse(Path::new("x.yaml"), &yaml_with_widgets(MAX_WIDGETS)).unwrap();
    validate(&ok).expect("at the limit is fine");
    let over = parse(Path::new("x.yaml"), &yaml_with_widgets(MAX_WIDGETS + 1)).unwrap();
    let err = validate(&over).unwrap_err().to_string();
    assert!(err.contains("widgets"), "{err}");
}

#[test]
fn too_many_rows_and_bad_weight_are_rejected() {
    let mut def = reference();
    def.layout = vec![def.layout[0].clone(); MAX_ROWS + 1];
    assert!(validate(&def).unwrap_err().to_string().contains("rows"));

    let mut def = reference();
    for w in [0u16, MAX_WEIGHT + 1] {
        let cell: &mut CellDef = &mut def.layout[0].cells[0];
        cell.weight = w;
        assert!(
            validate(&def).unwrap_err().to_string().contains("weight"),
            "{w}"
        );
    }
}

#[test]
fn typos_and_bad_values_are_errors_with_a_line() {
    // A misspelt key (deny_unknown_fields), on line 2.
    let err = parse(Path::new("t.yaml"), "version: 1\nidd: x\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("idd"), "{err}");
    assert!(err.contains("t.yaml:2"), "{err}");
    // An unknown colour is named.
    let text = yaml_with_widgets(1)
        + "        thresholds:\n          - at: 1.0\n            color: chartreuse\n";
    let err = parse(Path::new("t.yaml"), &text).unwrap_err().to_string();
    assert!(err.contains("chartreuse"), "{err}");
    // An empty signal name is a validation error.
    let text = "version: 1\nid: e\ntitle: T\nlayout:\n  - height: 1\n    cells:\n      - widget: gauge\n        signal: \"\"\n";
    let def = parse(Path::new("t.yaml"), text).unwrap();
    assert!(validate(&def).unwrap_err().to_string().contains("signal"));
}

/// Check `value` against the schema file; returns the error text if invalid.
fn schema_errors(value: &serde_json::Value) -> Option<String> {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
    validator.validate(value).err().map(|e| e.to_string())
}

#[test]
fn schema_file_accepts_the_reference_and_rejects_the_unknown_widget_fixture() {
    let good: serde_json::Value = serde_yaml_ng::from_str(REFERENCE_YAML).unwrap();
    assert_eq!(
        schema_errors(&good),
        None,
        "reference must satisfy the schema"
    );
    let good_json: serde_json::Value = serde_json::from_str(REFERENCE_JSON).unwrap();
    assert_eq!(schema_errors(&good_json), None);
    let bad: serde_json::Value = serde_yaml_ng::from_str(UNKNOWN_WIDGET).unwrap();
    assert!(
        schema_errors(&bad).is_some(),
        "unknown widget must fail the schema"
    );
    // A typo'd key fails too, as it does in the Rust types.
    let mut typo = good.clone();
    typo["idd"] = serde_json::json!("x");
    assert!(schema_errors(&typo).is_some());
}

#[test]
fn a_typo_inside_a_cell_is_an_error_naming_the_key() {
    // CellDef flattens its widget, where serde's deny_unknown_fields does not
    // apply; this pins that the widget variants still reject unknown keys.
    let text = yaml_with_widgets(1) + "        lable: oops\n";
    let err = parse(Path::new("t.yaml"), &text).unwrap_err().to_string();
    assert!(err.contains("lable"), "{err}");
}

// ---------------------------------------------------------------------------
// Fix round 1: schema drift, hostile input, load() and error format.
// ---------------------------------------------------------------------------

use super::schema::{ColorName, KindName, Threshold, WidgetBody};
use super::{load, LoadError};
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::Duration;

/// Run `f` on a thread and fail the test if it has not finished in 10 s, so
/// a parser that loops on hostile text fails the test instead of hanging CI.
/// The thread gets a large stack so a deep-nesting test fails on the
/// parser's own limit, not on ours.
fn with_timeout<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(f());
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(10))
        .expect("parser did not finish in 10 s")
}

/// `load` as a one-line message; panics if it succeeds.
fn load_err(name: &str, text: &str) -> LoadError {
    let (name, text) = (name.to_string(), text.to_string());
    with_timeout(move || load(Path::new(&name), &text)).expect_err("expected an error")
}

fn assert_one_line(e: &LoadError) {
    let s = e.to_string();
    assert!(!s.is_empty());
    assert!(!s.chars().any(|c| c.is_control()), "control char in {s:?}");
}

// ---- 1a. Exhaustive-match-driven schema coverage --------------------------
//
// The `match`es below have no wildcard arm. Adding a variant to WidgetDef,
// KindName or ColorName stops this file compiling until the arm (and so the
// list) is updated; the test then fails until viz-v1.schema.json lists the
// same name.

fn widget_name(w: &WidgetDef) -> &'static str {
    match w {
        WidgetDef::Gauge(_) => "gauge",
        WidgetDef::Sparkline(_) => "sparkline",
        WidgetDef::Table(_) => "table",
        WidgetDef::Log(_) => "log",
        WidgetDef::Grid(_) => "grid",
        WidgetDef::HeatStrip(_) => "heat_strip",
        WidgetDef::Text(_) => "text",
    }
}

fn kind_name(k: KindName) -> &'static str {
    match k {
        KindName::Chip => "chip",
        KindName::Board => "board",
        KindName::Tile => "tile",
        KindName::Model => "model",
        KindName::Process => "process",
        KindName::Lease => "lease",
        KindName::Host => "host",
        KindName::DramChannel => "dram_channel",
        KindName::EthPort => "eth_port",
        KindName::TrainingRun => "training_run",
    }
}

fn color_name(c: ColorName) -> &'static str {
    match c {
        ColorName::Primary => "primary",
        ColorName::Secondary => "secondary",
        ColorName::Success => "success",
        ColorName::Error => "error",
        ColorName::Warning => "warning",
        ColorName::Info => "info",
        ColorName::Text => "text",
        ColorName::Muted => "muted",
        ColorName::Border => "border",
        ColorName::Red => "red",
        ColorName::Yellow => "yellow",
        ColorName::Green => "green",
        ColorName::Cyan => "cyan",
        ColorName::Blue => "blue",
        ColorName::Magenta => "magenta",
        ColorName::White => "white",
        ColorName::Gray => "gray",
    }
}

fn body() -> WidgetBody {
    WidgetBody {
        signal: "power_w".into(),
        kind: None,
        label: None,
        max: Some(100.0),
        thresholds: vec![],
    }
}

fn all_widgets() -> Vec<WidgetDef> {
    let b = body();
    vec![
        WidgetDef::Gauge(b.clone()),
        WidgetDef::Sparkline(b.clone()),
        WidgetDef::Table(b.clone()),
        WidgetDef::Log(b.clone()),
        WidgetDef::Grid(b.clone()),
        WidgetDef::HeatStrip(b.clone()),
        WidgetDef::Text(b),
    ]
}

fn all_kinds() -> Vec<KindName> {
    use KindName::*;
    vec![
        Chip,
        Board,
        Tile,
        Model,
        Process,
        Lease,
        Host,
        DramChannel,
        EthPort,
        TrainingRun,
    ]
}

fn all_colors() -> Vec<ColorName> {
    use ColorName::*;
    vec![
        Primary, Secondary, Success, Error, Warning, Info, Text, Muted, Border, Red, Yellow, Green,
        Cyan, Blue, Magenta, White, Gray,
    ]
}

/// The `enum` list at `pointer` in the schema, as a sorted vector.
fn schema_enum(pointer: &str) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let mut v: Vec<String> = schema
        .pointer(pointer)
        .and_then(|e| e.as_array())
        .unwrap_or_else(|| panic!("no enum at {pointer}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

fn sorted(mut v: Vec<&str>) -> Vec<String> {
    v.sort();
    v.into_iter().map(String::from).collect()
}

/// A document whose only cell is `widget`, built from the reference.
fn doc_with(widget: WidgetDef) -> VizDef {
    let mut def = reference();
    def.layout = vec![def.layout[0].clone()];
    def.layout[0].cells = vec![CellDef { weight: 1, widget }];
    def
}

/// Serialise `def` with the Rust types, require the schema to accept it, and
/// require the Rust loader to read it back equal.
fn assert_schema_and_rust_accept(def: &VizDef) {
    let value = serde_json::to_value(def).unwrap();
    assert_eq!(schema_errors(&value), None, "schema rejects {value}");
    let text = serde_json::to_string(&value).unwrap();
    assert_eq!(&load(Path::new("x.json"), &text).unwrap(), def);
}

#[test]
fn every_widget_kind_and_color_variant_satisfies_the_schema() {
    let widgets = all_widgets();
    // The schema lists exactly the widgets the Rust enum has.
    assert_eq!(
        schema_enum("/$defs/cell/properties/widget/enum"),
        sorted(widgets.iter().map(widget_name).collect())
    );
    assert_eq!(
        schema_enum("/$defs/cell/properties/kind/enum"),
        sorted(all_kinds().into_iter().map(kind_name).collect())
    );
    assert_eq!(
        schema_enum("/$defs/threshold/properties/color/enum"),
        sorted(all_colors().into_iter().map(color_name).collect())
    );
    // The serde name of each variant is the name the schema and the match use.
    for w in &widgets {
        let v = serde_json::to_value(doc_with(w.clone())).unwrap();
        assert_eq!(v["layout"][0]["cells"][0]["widget"], widget_name(w));
        assert_schema_and_rust_accept(&doc_with(w.clone()));
    }
    for k in all_kinds() {
        let mut b = body();
        b.kind = Some(k);
        let def = doc_with(WidgetDef::Gauge(b));
        let v = serde_json::to_value(&def).unwrap();
        assert_eq!(v["layout"][0]["cells"][0]["kind"], kind_name(k));
        assert_schema_and_rust_accept(&def);
    }
    for c in all_colors() {
        let mut b = body();
        b.thresholds = vec![Threshold { at: 1.0, color: c }];
        let def = doc_with(WidgetDef::Gauge(b));
        let v = serde_json::to_value(&def).unwrap();
        assert_eq!(
            v["layout"][0]["cells"][0]["thresholds"][0]["color"],
            color_name(c)
        );
        assert_schema_and_rust_accept(&def);
    }
}

// ---- 1b. Differential table ------------------------------------------------

/// Mutations of the reference JSON that the schema and the Rust side must
/// both reject. Each is `(name, edit, text the Rust error must contain,
/// text the schema error must contain)`.
///
/// Rules only one side can express are pinned on the Rust side in
/// `rust_only_rules_are_rejected` and are NOT in this table:
///   * the 64-widget limit counts cells over all rows (the schema can only
///     cap one row's `cells`);
///   * whitespace-only `title` and `signal` (the schema says minLength 1);
///   * the 256 KiB file limit (a property of the file, not the document);
///   * non-finite `max` (JSON cannot write one; YAML can, see hostile tests).
#[test]
fn schema_and_rust_reject_the_same_mutations() {
    type Edit = fn(&mut Value);
    let cases: Vec<(&str, Edit, &str, &str)> = vec![
        (
            "weight 0",
            |v| v["layout"][0]["cells"][0]["weight"] = json!(0),
            "weight 0",
            "minimum",
        ),
        (
            "weight 101",
            |v| v["layout"][0]["cells"][0]["weight"] = json!(101),
            "weight 101",
            "maximum",
        ),
        (
            "max 0",
            |v| v["layout"][0]["cells"][0]["max"] = json!(0),
            "max 0",
            "minimum",
        ),
        (
            "height 0",
            |v| v["layout"][0]["height"] = json!(0),
            "height 0",
            "oneOf",
        ),
        (
            "height word",
            |v| v["layout"][0]["height"] = json!("tall"),
            "tall",
            "oneOf",
        ),
        (
            "unknown row key",
            |v| v["layout"][0]["colour"] = json!("x"),
            "colour",
            "colour",
        ),
        (
            "unknown cell key",
            |v| v["layout"][0]["cells"][0]["lable"] = json!("x"),
            "lable",
            "lable",
        ),
        (
            "unknown threshold key",
            |v| v["layout"][0]["cells"][0]["thresholds"][0]["colour"] = json!("red"),
            "colour",
            "colour",
        ),
        (
            "bad kind",
            |v| v["layout"][0]["cells"][0]["kind"] = json!("rack"),
            "rack",
            "is not one of",
        ),
        (
            "bad colour",
            |v| v["layout"][0]["cells"][0]["thresholds"][0]["color"] = json!("chartreuse"),
            "chartreuse",
            "is not one of",
        ),
        (
            "unknown widget",
            |v| v["layout"][0]["cells"][0]["widget"] = json!("pie_chart"),
            "unknown variant `pie_chart`",
            "\"pie_chart\" is not one of",
        ),
        (
            "version 2",
            |v| v["version"] = json!(2),
            "version 2",
            "1 was expected",
        ),
        (
            "bad id",
            |v| v["id"] = json!("Bad_Id"),
            "bad id",
            "does not match",
        ),
        (
            "empty layout",
            |v| v["layout"] = json!([]),
            "no rows",
            "less than 1 item",
        ),
        (
            "empty cells",
            |v| v["layout"][0]["cells"] = json!([]),
            "no cells",
            "less than 1 item",
        ),
        (
            "empty signal",
            |v| v["layout"][0]["cells"][0]["signal"] = json!(""),
            "signal is empty",
            "shorter than 1",
        ),
        (
            "extra top key",
            |v| v["colour_scheme"] = json!("x"),
            "colour_scheme",
            "colour_scheme",
        ),
    ];
    for (name, edit, rust_says, schema_says) in cases {
        let mut v: Value = serde_json::from_str(REFERENCE_JSON).unwrap();
        // The unedited reference must pass both, so a failure below is the edit's.
        assert_eq!(schema_errors(&v), None);
        edit(&mut v);
        let schema_err = schema_errors(&v).unwrap_or_else(|| panic!("{name}: schema accepted it"));
        assert!(
            schema_err.contains(schema_says),
            "{name}: schema said {schema_err}"
        );
        let e = load_err("m.json", &v.to_string());
        assert!(e.to_string().contains(rust_says), "{name}: rust said {e}");
        assert_one_line(&e);
    }
}

#[test]
fn rust_only_rules_are_rejected() {
    // See the doc comment on schema_and_rust_reject_the_same_mutations: the
    // schema cannot express these, so only the Rust side is pinned.
    let e = load_err("a.yaml", &yaml_with_widgets(MAX_WIDGETS + 1));
    assert!(e.to_string().contains("too many widgets"), "{e}");
    let many_rows = (0..MAX_ROWS + 1)
        .map(|_| "  - height: 1\n    cells:\n      - widget: gauge\n        signal: s\n")
        .collect::<String>();
    let e = load_err(
        "a.yaml",
        &format!("version: 1\nid: r\ntitle: T\nlayout:\n{many_rows}"),
    );
    assert!(e.to_string().contains("too many rows"), "{e}");
    let e = load_err("a.yaml", "version: 1\nid: r\ntitle: \"  \"\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n");
    assert!(e.to_string().contains("title is empty"), "{e}");
    let e = load_err("a.yaml", "version: 1\nid: r\ntitle: T\nlayout:\n  - cells:\n      - widget: gauge\n        signal: \"  \"\n");
    assert!(e.to_string().contains("signal is empty"), "{e}");
}

#[test]
fn empty_layout_and_empty_cells_fail_on_their_own() {
    // Version and id are valid, so only the layout rule can fail.
    let e = load_err("a.yaml", "version: 1\nid: ok\ntitle: T\nlayout: []\n");
    assert!(e.to_string().contains("layout has no rows"), "{e}");
    let e = load_err(
        "a.yaml",
        "version: 1\nid: ok\ntitle: T\nlayout:\n  - height: 1\n    cells: []\n",
    );
    assert!(e.to_string().contains("row 1 has no cells"), "{e}");
}

// ---- 2. Hostile input ------------------------------------------------------

#[test]
fn hostile_yaml_is_an_error_not_a_panic_or_a_hang() {
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    let deep_map = "a: ".repeat(10_000) + "1";
    // Billion-laughs style: each anchor holds nine copies of the one before.
    let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x]\n");
    for i in 1..12 {
        bomb.push_str(&format!(
            "a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n",
            p = i - 1
        ));
    }
    let valid_cell = "layout:\n  - cells:\n      - widget: gauge\n        signal: s\n";
    let cases: Vec<(&str, String)> = vec![
        ("deep flow seq", deep),
        ("deep map", deep_map),
        ("alias bomb", format!("version: 1\nid: b\ntitle: T\n{bomb}{valid_cell}")),
        ("alias bomb as title", format!("version: 1\nid: b\n{bomb}title: *a11\n{valid_cell}")),
        ("merge key", format!("base: &b {{version: 1}}\n<<: *b\nid: m\ntitle: T\n{valid_cell}")),
        ("tag on value", format!("version: !evil 1\nid: t\ntitle: T\n{valid_cell}")),
        ("custom tag", format!("version: 1\nid: t\ntitle: !evil x\n{valid_cell}")),
        (
            "two documents",
            format!("version: 1\nid: a\ntitle: T\n{valid_cell}---\nversion: 1\nid: b\ntitle: U\n{valid_cell}"),
        ),
        ("empty", String::new()),
        ("whitespace only", " \n\t \n".into()),
        ("comment only", "# nothing\n".into()),
        ("null document", "~\n".into()),
        ("max inf", format!("version: 1\nid: t\ntitle: T\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n        max: .inf\n")),
        ("max nan", format!("version: 1\nid: t\ntitle: T\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n        max: .nan\n")),
        ("max 1e999", format!("version: 1\nid: t\ntitle: T\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n        max: 1e999\n")),
        ("threshold inf", format!("version: 1\nid: t\ntitle: T\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n        thresholds: [{{at: .inf, color: red}}]\n")),
    ];
    for (name, text) in cases {
        eprintln!("hostile yaml case: {name}");
        let e = load_err("h.yaml", &text);
        assert!(e.to_string().starts_with("h.yaml"), "{name}: {e}");
        assert_one_line(&e);
        assert!(e.to_string().len() < 600, "{name}: message too long: {e}");
    }
}

#[test]
fn hostile_json_is_an_error_not_a_panic_or_a_hang() {
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    let deep_obj = r#"{"a":"#.repeat(10_000) + "1" + &"}".repeat(10_000);
    let cases: Vec<(&str, String)> = vec![
        ("deep array", deep),
        ("deep object", deep_obj),
        ("empty", String::new()),
        ("whitespace only", " \n\t ".into()),
        ("two values", format!("{REFERENCE_JSON}{REFERENCE_JSON}")),
        ("max 1e999", REFERENCE_JSON.replace("300.0", "1e999")),
        (
            "duplicate key",
            REFERENCE_JSON.replacen("\"id\"", "\"id\": \"x\", \"id\"", 1),
        ),
    ];
    for (name, text) in cases {
        let e = load_err("h.json", &text);
        assert_one_line(&e);
        assert!(e.to_string().starts_with("h.json"), "{name}: {e}");
    }
}

#[test]
fn oversize_json_is_rejected_before_parsing() {
    // Not valid JSON either, so a late size check would give a syntax error.
    let text = "[".repeat(257 * 1024);
    let e = load_err("big.json", &text);
    assert_eq!(e.line, None);
    assert!(e.to_string().starts_with("big.json: file too large"), "{e}");
}

#[test]
fn control_characters_in_echoed_text_are_stripped() {
    // The widget name goes into the error; an ESC and a newline in it must
    // not reach the log line.
    let text = "version: 1\nid: e\ntitle: T\nlayout:\n  - cells:\n      - widget: \"bad\\e[31m\\nname\"\n        signal: s\n";
    let e = load_err("e.yaml", text);
    assert_one_line(&e);
    assert!(e.to_string().contains("bad[31mname"), "{e}");
    // The same for a bad id, which validate echoes.
    let text = "version: 1\nid: \"x\\e[2J\\ny\"\ntitle: T\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n";
    let e = load_err("e.yaml", text);
    assert_one_line(&e);
    assert!(e.to_string().contains("bad id"), "{e}");
}

// ---- 3. load() and the error format ---------------------------------------

#[test]
fn load_error_display_format_is_exact() {
    // Parse error with a line: `<path>:<line>: <message>`.
    let e = load_err("dir/t.yaml", "version: 1\nidd: x\n");
    assert_eq!(e.line, Some(2));
    assert!(e.to_string().starts_with("dir/t.yaml:2: "), "{e}");
    assert!(e.to_string().contains("unknown field `idd`"), "{e}");
    // Validation error: no line, path added by load.
    let e = load_err("dir/v.yaml", "version: 2\nid: v\ntitle: T\nlayout: []\n");
    assert_eq!(
        e.to_string(),
        "dir/v.yaml: unsupported version 2 (this build reads version 1)"
    );
    // Oversize error: no line.
    let e = load_err("dir/big.yaml", &"x".repeat(MAX_FILE_BYTES + 1));
    assert_eq!(
        e.to_string(),
        format!(
            "dir/big.yaml: file too large: {} bytes (the limit is {MAX_FILE_BYTES})",
            MAX_FILE_BYTES + 1
        )
    );
    // Unknown extension.
    let e = load_err("dir/x.txt", "version: 1");
    assert_eq!(
        e.to_string(),
        "dir/x.txt: unknown file type (expected .yaml, .yml or .json)"
    );
    // A JSON syntax error also carries a line.
    let e = load_err("dir/j.json", "{\n  \"version\": 1,\n  oops\n}");
    assert!(e.to_string().starts_with("dir/j.json:3: "), "{e}");
}

#[test]
fn errors_inside_a_flattened_cell_name_the_file_and_the_value() {
    // The widget and its keys are read through serde's flatten buffering,
    // which may lose the position. Record what is reported.
    let unknown = "version: 1\nid: f\ntitle: T\nlayout:\n  - cells:\n      - widget: pie_chart\n        signal: s\n";
    let e = load_err("f.yaml", unknown);
    assert!(e.to_string().starts_with("f.yaml"), "{e}");
    assert!(e.to_string().contains("pie_chart"), "{e}");
    let typo = "version: 1\nid: f\ntitle: T\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n        lable: x\n";
    let e = load_err("f.yaml", typo);
    assert!(e.to_string().starts_with("f.yaml"), "{e}");
    assert!(e.to_string().contains("lable"), "{e}");
    println!(
        "flattened-cell lines: unknown widget {:?}, typo {:?}",
        load_err("f.yaml", unknown).line,
        e.line
    );
}

#[test]
fn load_succeeds_on_the_reference_in_both_formats() {
    assert_eq!(
        load(Path::new("a.yaml"), REFERENCE_YAML).unwrap(),
        reference()
    );
    assert_eq!(
        load(Path::new("a.json"), REFERENCE_JSON).unwrap(),
        reference()
    );
}

/// Plain `!` text in a title or a legend line is not a YAML tag. Tag
/// rejection (`has_tag`) looks at the parsed tree, so text that merely holds
/// or starts with `!` inside a quoted string must still load.
#[test]
fn bang_in_quoted_text_is_not_a_yaml_tag() {
    let text = "version: 1\nid: hot\ntitle: \"Hot!\"\nlegend: [\"!! careful\"]\nlayout:\n  - cells:\n      - widget: gauge\n        signal: s\n";
    let def = load(Path::new("hot.yaml"), text).expect("plain ! text loads");
    assert_eq!(def.title, "Hot!");
    assert_eq!(def.legend, vec!["!! careful".to_string()]);
}
