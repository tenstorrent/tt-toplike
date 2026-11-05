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
