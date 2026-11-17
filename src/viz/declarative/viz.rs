// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! [`DeclarativeViz`]: draws a [`VizDef`] as a [`Viz`].
//!
//! The renderer does the layout and leaves the drawing to
//! [`widgets`](super::widgets):
//!
//! 1. **Expand rows.** A row with `each: chip` becomes one row per chip. At
//!    most [`MAX_INSTANCES`] widget instances (cells, counted after the
//!    expansion) are kept per frame. The validator caps the cells in the
//!    file, not the instances, so 16 chips times a 6-cell row would
//!    otherwise be 96 widgets; the cap lives here, where the chip count is
//!    known.
//! 2. **Give rows heights.** A fixed height is in terminal lines and is cut
//!    to what is left of the area. `fill` rows share what the fixed rows
//!    leave, the first ones getting the odd line. A row left with no line is
//!    not drawn.
//! 3. **Say what was cut.** When rows were dropped (by the cap or for lack
//!    of lines) the last visible line becomes `+N more`, where N counts the
//!    rows not drawn, including the row that gave up its line.
//! 4. **Split rows into cells** by `weight`, using integer arithmetic on the
//!    cumulative weights so the cells always add up to the row width.
//!
//! With no chips, a definition that reads chips draws one line, `no chips`.
//! Nothing here writes outside the `area` it is given, and a zero-size
//! area draws nothing.

use super::schema::{RowDef, VizDef, WidgetDef};
use super::widgets::{self, EntityCache, Inst};
use crate::ui::colors;
use crate::viz::{SignalSpec, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tt_toplike_bus::EntityKey;

/// Most widget instances drawn in one frame, over all rows.
pub const MAX_INSTANCES: usize = 64;

/// The smallest terminal used when the file gives no `min_size`.
const DEFAULT_MIN_SIZE: (u16, u16) = (40, 10);

/// A viz described by a definition file.
pub struct DeclarativeViz {
    def: VizDef,
}

/// One row to draw: the definition row and the chip it is drawn for.
struct RowInst<'a> {
    row: &'a RowDef,
    inst: Inst,
}

impl DeclarativeViz {
    /// Wrap a validated definition (from `declarative::load`).
    pub fn new(def: VizDef) -> DeclarativeViz {
        DeclarativeViz { def }
    }

    /// Does any widget read chips? True for an `each` row or a widget with
    /// no `kind` (the default is chip). A definition that reads only hosts,
    /// say, has no use for the `no chips` placeholder.
    fn uses_chips(&self) -> bool {
        self.def.layout.iter().any(|r| {
            r.each.is_some()
                || r.cells.iter().any(|c| {
                    // `log` reads events, never chips, whatever its `kind`.
                    !matches!(c.widget, WidgetDef::Log(_))
                        && c.widget
                            .body()
                            .kind
                            .is_none_or(|k| k == super::schema::KindName::Chip)
                })
        })
    }

    /// Step 1: the rows to draw and how many row instances the cap dropped.
    fn expand<'a>(&'a self, chips: &[EntityKey]) -> (Vec<RowInst<'a>>, usize) {
        let mut out = Vec::new();
        let mut budget = MAX_INSTANCES;
        let mut dropped = 0;
        for row in &self.def.layout {
            let copies: Vec<Inst> = if row.each.is_some() {
                chips
                    .iter()
                    .map(|c| Inst {
                        chip: Some(c.clone()),
                    })
                    .collect()
            } else {
                vec![Inst::default()]
            };
            for inst in copies {
                // Once one instance does not fit, none after it is drawn,
                // so the rows that remain are a prefix of the layout.
                if dropped == 0 && row.cells.len() <= budget {
                    budget -= row.cells.len();
                    out.push(RowInst { row, inst });
                } else {
                    dropped += 1;
                }
            }
        }
        (out, dropped)
    }
}

/// Step 2: `(index into rows, y offset, height)` for each row that gets at
/// least one line, and how many rows got none.
fn allocate(rows: &[RowInst], height: u16) -> (Vec<(usize, u16, u16)>, usize) {
    use super::schema::Height;
    let fixed: u32 = rows
        .iter()
        .map(|r| match r.row.height {
            Height::Rows(n) => n as u32,
            Height::Fill => 0,
        })
        .sum();
    let fills = rows.iter().filter(|r| r.row.height == Height::Fill).count() as u32;
    let left = (height as u32).saturating_sub(fixed);
    let (per, extra) = if fills > 0 {
        (left / fills, left % fills)
    } else {
        (0, 0)
    };
    let (mut y, mut fill_seen) = (0u16, 0u32);
    let mut placed = Vec::new();
    let mut dropped = 0;
    for (i, r) in rows.iter().enumerate() {
        let want = match r.row.height {
            Height::Rows(n) => n as u32,
            Height::Fill => {
                let h = per + u32::from(fill_seen < extra);
                fill_seen += 1;
                h
            }
        };
        let h = want.min((height - y) as u32) as u16;
        if h == 0 {
            dropped += 1;
        } else {
            placed.push((i, y, h));
            y += h;
        }
    }
    (placed, dropped)
}

/// Step 4: the x offset and width of each cell of a row `width` columns wide.
fn split_cells(row: &RowDef, width: u16) -> Vec<(u16, u16)> {
    let total: u32 = row.cells.iter().map(|c| c.weight as u32).sum();
    let mut before = 0u32;
    row.cells
        .iter()
        .map(|c| {
            let x0 = (width as u32 * before / total.max(1)) as u16;
            before += c.weight as u32;
            let x1 = (width as u32 * before / total.max(1)) as u16;
            (x0, x1 - x0)
        })
        .collect()
}

/// The lower-case file name of a serde-named enum value, for descriptions.
fn name_of<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// One widget in words: `gauge of power_w (max 300; yellow from 200, red
/// from 260)`. Used by the generated legend and explain text.
fn describe(w: &WidgetDef) -> String {
    let b = w.body();
    let mut s = format!("{} of {}", widget_name(w), b.signal);
    if let Some(k) = b.kind.filter(|k| *k != super::schema::KindName::Chip) {
        s.push_str(&format!(" on {}", name_of(&k)));
    }
    let mut notes = Vec::new();
    if let Some(m) = b.max {
        notes.push(format!("max {m}"));
    }
    let mut th: Vec<_> = b.thresholds.iter().collect();
    th.sort_by(|a, b| a.at.total_cmp(&b.at));
    let steps: Vec<String> = th
        .iter()
        .map(|t| format!("{} from {}", name_of(&t.color), t.at))
        .collect();
    if !steps.is_empty() {
        notes.push(steps.join(", "));
    }
    if !notes.is_empty() {
        s.push_str(&format!(" ({})", notes.join("; ")));
    }
    s
}

/// The widget's name as the file spells it (`heat_strip`).
fn widget_name(w: &WidgetDef) -> String {
    serde_json::to_value(w)
        .ok()
        .and_then(|v| v.get("widget").and_then(|n| n.as_str().map(str::to_string)))
        .unwrap_or_default()
}

impl Viz for DeclarativeViz {
    fn id(&self) -> &str {
        &self.def.id
    }

    fn title(&self) -> &str {
        &self.def.title
    }

    /// The file's `min_size`, or 40 x 10. Not used to size anything here:
    /// the host shows the "needs W x H" message below it, and `render`
    /// copes with any area regardless.
    fn min_size(&self) -> (u16, u16) {
        self.def.min_size.unwrap_or(DEFAULT_MIN_SIZE)
    }

    /// Every (kind, signal) the widgets read, each once, in layout order.
    /// `log` widgets name events, not signals, and are left out.
    fn signals(&self) -> Vec<SignalSpec> {
        let mut out: Vec<SignalSpec> = Vec::new();
        for cell in self.def.layout.iter().flat_map(|r| &r.cells) {
            if matches!(cell.widget, WidgetDef::Log(_)) {
                continue;
            }
            let b = cell.widget.body();
            let spec = SignalSpec {
                kind: b
                    .kind
                    .unwrap_or(super::schema::KindName::Chip)
                    .entity_kind(),
                name: b.signal.clone(),
            };
            if !out.contains(&spec) {
                out.push(spec);
            }
        }
        out
    }

    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &VizCtx) {
        // Never draw outside the buffer, whatever area the caller passed.
        let area = area.intersection(*buf.area());
        if area.width == 0 || area.height == 0 {
            return;
        }
        // One entity cache per frame: every widget that reads entities
        // shares the lists it fetches (`widgets::EntityCache`).
        let cache = EntityCache::new();
        // The chip list is fetched only when the definition reads chips.
        let uses_chips = self.uses_chips();
        let chips = if uses_chips {
            cache.get(ctx, tt_toplike_bus::EntityKind::Chip)
        } else {
            Default::default()
        };
        if chips.is_empty() && uses_chips {
            let note = "no chips - waiting for the first reading";
            buf.set_stringn(
                area.x,
                area.y,
                note,
                area.width as usize,
                Style::default().fg(colors::text_secondary()),
            );
            return;
        }

        let (rows, capped) = self.expand(&chips);
        let (mut placed, unplaced) = allocate(&rows, area.height);
        let mut hidden = capped + unplaced;

        // Step 3: the last visible line carries the `+N more` note.
        let mut more_y = None;
        if hidden > 0 {
            match placed.last_mut() {
                Some((_, y, h)) if *h > 1 => {
                    *h -= 1;
                    more_y = Some(*y + *h);
                }
                Some(_) => {
                    let (_, y, _) = placed.pop().expect("just matched Some");
                    hidden += 1;
                    more_y = Some(y);
                }
                None => more_y = Some(0),
            }
        }

        for (i, y, h) in placed {
            let r = &rows[i];
            let row_area = Rect::new(area.x, area.y + y, area.width, h);
            for (cell, (dx, w)) in r.row.cells.iter().zip(split_cells(r.row, area.width)) {
                let cell_area = Rect::new(row_area.x + dx, row_area.y, w, row_area.height);
                widgets::draw(buf, cell_area, &cell.widget, &r.inst, ctx, &cache);
            }
        }
        if let Some(y) = more_y {
            buf.set_stringn(
                area.x,
                area.y + y,
                format!("+{hidden} more"),
                area.width as usize,
                Style::default().fg(colors::text_secondary()),
            );
        }
    }

    /// The file's legend lines, or one line per widget describing it with
    /// its signal and thresholds.
    fn legend(&self) -> Vec<String> {
        if !self.def.legend.is_empty() {
            return self.def.legend.clone();
        }
        self.legend_from_widgets()
    }

    /// The file's `explain`, or a sentence built from the widgets.
    fn explain(&self) -> String {
        if let Some(e) = &self.def.explain {
            return e.clone();
        }
        let parts = self.legend_from_widgets();
        format!("{} shows {}.", self.def.title, parts.join("; "))
    }

    fn help(&self) -> String {
        "This view has no keys of its own.".to_string()
    }
}

impl DeclarativeViz {
    /// The generated widget descriptions, whatever the file's own legend says.
    fn legend_from_widgets(&self) -> Vec<String> {
        self.def
            .layout
            .iter()
            .flat_map(|r| &r.cells)
            .map(|c| describe(&c.widget))
            .collect()
    }
}
