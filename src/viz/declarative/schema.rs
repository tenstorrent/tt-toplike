// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The data model of a declarative viz: what a YAML or JSON file holds.
//!
//! These types are only data. Parsing and the limits live in `mod.rs` and
//! `validate.rs`; drawing comes later. The same structs read both YAML and
//! JSON, so a file converts between the two without loss.
//!
//! Every struct that is read from a file has `deny_unknown_fields`: a
//! misspelt key is an error that names the key, not a silent default.
//! (`CellDef` cannot use it, because serde does not allow it together with
//! `flatten`. Its flattened widget variants do deny unknown keys, so a
//! typo in a cell is still caught, and `weight` is the only other key.)
//!
//! The file `viz/schema/viz-v1.schema.json` describes the same shape. It is
//! written by hand, and the test
//! `schema_file_accepts_the_reference_and_rejects_the_unknown_widget_fixture`
//! fails when the two drift apart on the reference fixture. When you add a
//! field here, add it to the schema and to `fixtures/reference.yaml`.

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

/// One viz definition, the root of a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VizDef {
    /// Format version. Only 1 exists; `validate` rejects anything else.
    pub version: u32,
    /// Unique kebab-case id (see `validate::check_id`).
    pub id: String,
    /// Name shown in the view cycle.
    pub title: String,
    /// Smallest terminal (columns, rows) the view draws well in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<(u16, u16)>,
    /// Lines for the legend strip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legend: Vec<String>,
    /// A paragraph for the explain key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain: Option<String>,
    /// Rows, top to bottom.
    pub layout: Vec<RowDef>,
}

/// One row of the layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowDef {
    /// Terminal lines, or `fill` for what is left. Defaults to `fill`.
    #[serde(default)]
    pub height: Height,
    /// When set, the row repeats once per entity of that kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<EachChip>,
    /// Cells, left to right.
    pub cells: Vec<CellDef>,
}

/// Row height: a fixed number of lines, or the remaining space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Height {
    /// This many terminal lines.
    Rows(u16),
    /// Share what the fixed rows leave over.
    #[default]
    Fill,
}

// Written by hand because the file form is a bare number or the word
// `fill`, which no derive attribute expresses.
impl Serialize for Height {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Height::Rows(n) => s.serialize_u16(*n),
            Height::Fill => s.serialize_str("fill"),
        }
    }
}

impl<'de> Deserialize<'de> for Height {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Height;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number of lines (1 or more) or the word `fill`")
            }
            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Height, E> {
                match u16::try_from(n) {
                    Ok(n) if n >= 1 => Ok(Height::Rows(n)),
                    _ => Err(E::custom(format!(
                        "height {n} is out of range (1 to 65535)"
                    ))),
                }
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Height, E> {
                u64::try_from(n)
                    .map_err(|_| E::custom(format!("height {n} is out of range (1 to 65535)")))
                    .and_then(|n| self.visit_u64(n))
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Height, E> {
                if s == "fill" {
                    Ok(Height::Fill)
                } else {
                    Err(E::custom(format!(
                        "unknown height `{s}`, expected a number or `fill`"
                    )))
                }
            }
        }
        d.deserialize_any(V)
    }
}

/// The value of `each:`. Only `chip` exists for now; it is an enum so more
/// kinds can be added without changing the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EachChip {
    /// Repeat the row once per chip.
    Chip,
}

/// One cell: a share of the row width and the widget drawn in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellDef {
    /// Share of the row width, 1 to 100 (`validate`). Defaults to 1.
    #[serde(default = "default_weight")]
    pub weight: u16,
    /// The widget; its kind is chosen by the `widget:` key in the file.
    #[serde(flatten)]
    pub widget: WidgetDef,
}

fn default_weight() -> u16 {
    1
}

/// A widget, chosen by the `widget:` key. Every variant takes the same
/// fields ([`WidgetBody`]); the renderer decides what each one does with
/// them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "widget", rename_all = "snake_case")]
pub enum WidgetDef {
    /// A bar that fills toward `max`.
    Gauge(WidgetBody),
    /// A line of recent values.
    Sparkline(WidgetBody),
    /// Current values, one row per entity.
    Table(WidgetBody),
    /// Recent events, newest last.
    Log(WidgetBody),
    /// One cell per entity, coloured by value.
    Grid(WidgetBody),
    /// A single strip coloured by value over time.
    HeatStrip(WidgetBody),
    /// Plain text with the value filled in.
    Text(WidgetBody),
}

impl WidgetDef {
    /// The fields common to every widget.
    pub fn body(&self) -> &WidgetBody {
        match self {
            WidgetDef::Gauge(b)
            | WidgetDef::Sparkline(b)
            | WidgetDef::Table(b)
            | WidgetDef::Log(b)
            | WidgetDef::Grid(b)
            | WidgetDef::HeatStrip(b)
            | WidgetDef::Text(b) => b,
        }
    }
}

/// The fields every widget takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidgetBody {
    /// Name of the bus signal to show. Any non-empty string; the host
    /// decides what exists, so the list is not fixed here.
    pub signal: String,
    /// The entity kind the signal belongs to. Defaults to `chip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<KindName>,
    /// Caption drawn with the widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The value that fills the widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Recolour once the value reaches `at`; the highest reached wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<Threshold>,
}

/// A value and the colour to use from that value up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Threshold {
    /// The value at which the colour starts.
    pub at: f64,
    /// The colour.
    pub color: ColorName,
}

/// Entity kinds a widget may read, spelt as in the bus (`EntityKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindName {
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
}

impl KindName {
    /// The bus kind this name stands for.
    pub fn entity_kind(self) -> tt_toplike_bus::EntityKind {
        use tt_toplike_bus::EntityKind as K;
        match self {
            KindName::Chip => K::Chip,
            KindName::Board => K::Board,
            KindName::Tile => K::Tile,
            KindName::Model => K::Model,
            KindName::Process => K::Process,
            KindName::Lease => K::Lease,
            KindName::Host => K::Host,
            KindName::DramChannel => K::DramChannel,
            KindName::EthPort => K::EthPort,
            KindName::TrainingRun => K::TrainingRun,
        }
    }
}

/// A colour a file may name. The first group is the TUI palette in
/// `crate::ui::colors` (so a view follows the theme); the second is plain
/// terminal colours. An unknown name fails to parse, and the message names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorName {
    // Palette colours, from crate::ui::colors.
    Primary,
    Secondary,
    Success,
    Error,
    Warning,
    Info,
    /// `colors::text_primary()`.
    Text,
    /// `colors::text_secondary()`.
    Muted,
    Border,
    // Plain colours.
    Red,
    Yellow,
    Green,
    Cyan,
    Blue,
    Magenta,
    White,
    Gray,
}

impl ColorName {
    /// The terminal colour. Palette names go through `crate::ui::colors`, so
    /// the theme applies; plain names are the standard ANSI colours.
    pub fn color(self) -> ratatui::style::Color {
        use crate::ui::colors;
        use ratatui::style::Color;
        match self {
            ColorName::Primary => colors::primary(),
            ColorName::Secondary => colors::SECONDARY,
            ColorName::Success => colors::success(),
            ColorName::Error => colors::error(),
            ColorName::Warning => colors::warning(),
            ColorName::Info => colors::info(),
            ColorName::Text => colors::text_primary(),
            ColorName::Muted => colors::text_secondary(),
            ColorName::Border => colors::BORDER,
            ColorName::Red => Color::Red,
            ColorName::Yellow => Color::Yellow,
            ColorName::Green => Color::Green,
            ColorName::Cyan => Color::Cyan,
            ColorName::Blue => Color::Blue,
            ColorName::Magenta => Color::Magenta,
            ColorName::White => Color::White,
            ColorName::Gray => Color::Gray,
        }
    }
}
