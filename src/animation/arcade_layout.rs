// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The arcade's layout: which views it shows, in which rows, and how wide.
//!
//! The layout comes from the `[arcade]` section of the config file (see
//! `docs/arcade-layout.md` and `crate::config`). Without one the arcade uses
//! [`default_rows`], which gives the layout the arcade has always had.
//!
//! # The screen
//!
//! From the top: the title row, Row 1 (the telemetry strip or the duel),
//! then each configured row as a rule (`╔══[ LABEL ]═══▓▒░`) followed by its
//! content rows, and one footer row at the bottom that the status bar draws
//! over. So `2 + rows + 1` rows are fixed, and the rest is content.
//!
//! # Weights and rounding
//!
//! Heights and widths are positive integer weights. The content rows (or a
//! row's columns) are cut at `floor(total * running_weight / weight_sum)`, in
//! integer arithmetic, so every row and column is used exactly once with no
//! gap and no overlap. Between two panels of a row the first column of the
//! right panel's share is a `│` divider. This reproduces the old fixed
//! layout exactly: weights 4, 3, 3 give the 40% / 30% / 30% rows, and widths
//! 11 and 9 give the castle its 55% of the columns, with the divider taken
//! from the defrag side.
//!
//! # Panels that do not fit
//!
//! Each view has a minimum size ([`View::min_size`]). A panel smaller than
//! its minimum is taken out of its row, and the row's other panels share the
//! columns. The four original views have a 1x1 minimum and are never taken
//! out: given no column or no content row they draw nothing and keep their
//! label, as the arcade did before the layout engine ([`View::fits`]). Its row's rule then names it with the size it needs, on one line
//! (`◌ DIE 3D needs 16x6`). When a row has no panel left, its content rows
//! go to the other rows; the rule stays, with the note. The panel taken out
//! is the rightmost one that does not fit, one at a time, so a removal that
//! makes room for another panel keeps that panel.
//!
//! The layout is worked out once, when the arcade is made (at start-up and
//! on a resize). The frame path only reads it.

use std::fmt;

/// A view the arcade can show in a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    Starfield,
    Castle,
    Flow,
    Defrag,
    Die3d,
    Training,
    Inference,
}

impl View {
    /// Every view, in the order the docs list them.
    pub const ALL: [View; 7] = [
        View::Starfield,
        View::Castle,
        View::Flow,
        View::Defrag,
        View::Die3d,
        View::Training,
        View::Inference,
    ];

    /// The name used in the config file.
    pub fn key(self) -> &'static str {
        match self {
            View::Starfield => "starfield",
            View::Castle => "castle",
            View::Flow => "flow",
            View::Defrag => "defrag",
            View::Die3d => "die3d",
            View::Training => "training",
            View::Inference => "inference",
        }
    }

    /// The view a config name stands for. `waffler` is accepted for `die3d`
    /// (the view's provisional display name). Letter case is ignored.
    pub fn from_key(key: &str) -> Option<View> {
        let k = key.trim().to_ascii_lowercase();
        if k == "waffler" {
            return Some(View::Die3d);
        }
        View::ALL.into_iter().find(|v| v.key() == k)
    }

    /// The label on the panel's rule. The first four are the labels the
    /// arcade has always printed.
    pub fn label(self) -> &'static str {
        match self {
            View::Starfield => "✧ STARFIELD",
            View::Castle => "🏰 MEMORY CASTLE",
            View::Flow => "🌊 MEMORY FLOW",
            View::Defrag => "▓ DEFRAG",
            View::Die3d => "◆ DIE 3D",
            View::Training => "∿ TRAINING",
            View::Inference => "≈ INFERENCE",
        }
    }

    /// The smallest panel (columns, rows) the view is drawn in.
    ///
    /// * The four original views: 1x1. They also draw (nothing) in an empty
    ///   panel ([`View::fits`]), so the default layout hides nothing at any
    ///   size, as the old layout did.
    /// * Die 3D: the view's own smallest scene (`die3d::SCENE_MIN_CELLS`).
    ///   Below it the full view shows a text summary, which is not the scene.
    /// * Training: the Training view's own floor (`TrainView::new` never
    ///   draws below 20 x 10).
    /// * Inference: the serving creature's dashboard needs about 24 x 8.
    pub fn min_size(self) -> (usize, usize) {
        match self {
            View::Starfield | View::Castle | View::Flow | View::Defrag => (1, 1),
            View::Die3d => crate::animation::die3d::SCENE_MIN_CELLS,
            View::Training => (20, 10),
            View::Inference => (24, 8),
        }
    }
}

impl View {
    /// Whether the view is drawn in a `w` x `h` panel. A panel at least
    /// [`View::min_size`] fits. So does an empty panel (no column or no row)
    /// of a view whose minimum is 1x1: the four original views draw nothing
    /// there and keep their row and label, as the arcade did before the
    /// layout engine. Die 3D, Training and the inference panel need their
    /// minimum.
    pub fn fits(self, w: usize, h: usize) -> bool {
        let (mw, mh) = self.min_size();
        let empty = w == 0 || h == 0;
        (w >= mw && h >= mh) || (empty && (mw, mh) == (1, 1))
    }
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// One panel of a configured row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelSpec {
    pub view: View,
    /// Width weight, at least 1. `2` is double wide, `3` triple.
    pub width: u32,
}

/// One configured row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSpec {
    /// Height weight, at least 1.
    pub height: u32,
    pub panels: Vec<PanelSpec>,
}

/// The arcade's settings from the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArcadeConfig {
    /// Whether the atmosphere backdrop (`animation::atmosphere`) is drawn
    /// behind the whole arcade. Off by default: the arcade has never drawn
    /// one, and the default must draw what it always drew.
    pub cloudfield: bool,
    pub rows: Vec<RowSpec>,
}

impl Default for ArcadeConfig {
    fn default() -> Self {
        ArcadeConfig {
            cloudfield: false,
            rows: default_rows(),
        }
    }
}

/// The layout the arcade has always had: the starfield over 40% of the
/// content rows, castle (55% of the columns) beside defrag over 30%, and the
/// memory flow over the last 30%.
pub fn default_rows() -> Vec<RowSpec> {
    let p = |view, width| PanelSpec { view, width };
    vec![
        RowSpec {
            height: 4,
            panels: vec![p(View::Starfield, 1)],
        },
        RowSpec {
            height: 3,
            panels: vec![p(View::Castle, 11), p(View::Defrag, 9)],
        },
        RowSpec {
            height: 3,
            panels: vec![p(View::Flow, 1)],
        },
    ]
}

/// The example layout in `docs/arcade-layout.md`: the starfield on top;
/// defrag triple wide beside the castle and Die 3D double wide; then flow,
/// training and the inference server. Every view, once. The bench and the
/// tests draw it.
pub fn example_rows() -> Vec<RowSpec> {
    let p = |view, width| PanelSpec { view, width };
    vec![
        RowSpec {
            height: 2,
            panels: vec![p(View::Starfield, 1)],
        },
        RowSpec {
            height: 3,
            panels: vec![p(View::Defrag, 3), p(View::Castle, 1), p(View::Die3d, 2)],
        },
        RowSpec {
            height: 2,
            panels: vec![
                p(View::Flow, 1),
                p(View::Training, 1),
                p(View::Inference, 1),
            ],
        },
    ]
}

/// Rows above the first rule: the title and Row 1.
pub const HEADER_ROWS: usize = 2;
/// Rows below the last content row: the footer the status bar covers.
pub const FOOTER_ROWS: usize = 1;
/// Between two panel labels on one rule.
pub const LABEL_SEP: &str = "  │  ";

/// A rectangle of cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    /// The columns the rectangle covers.
    pub fn cols(&self) -> std::ops::Range<usize> {
        self.x..self.x + self.w
    }
    /// The rows the rectangle covers.
    pub fn rows(&self) -> std::ops::Range<usize> {
        self.y..self.y + self.h
    }
}

/// A panel placed on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panel {
    pub view: View,
    pub rect: Rect,
}

/// A configured row placed on the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaidRow {
    /// The row of its rule.
    pub rule_y: usize,
    /// Its content rows, starting at `rule_y + 1`. 0 when no panel fits.
    pub height: usize,
    /// The panels shown, left to right.
    pub panels: Vec<Panel>,
    /// The panels taken out because they did not fit, with the size each
    /// needs, in config order.
    pub hidden: Vec<(View, (usize, usize))>,
}

impl LaidRow {
    /// The text in the row's rule: the shown panels' labels, then a note for
    /// each hidden one.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = self.panels.iter().map(|p| p.view.label().into()).collect();
        for (v, (w, h)) in &self.hidden {
            parts.push(hidden_note(*v, *w, *h));
        }
        parts.join(LABEL_SEP)
    }
}

/// The note, one line long, that a hidden panel leaves on its row's rule. The label's
/// symbol is replaced by `◌`.
pub fn hidden_note(view: View, w: usize, h: usize) -> String {
    let name = view
        .label()
        .split_once(' ')
        .map_or(view.label(), |(_, n)| n);
    format!("◌ {name} needs {w}x{h}")
}

/// Where everything goes for one terminal size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub rows: Vec<LaidRow>,
}

impl Layout {
    /// Every shown panel, top to bottom and left to right.
    pub fn panels(&self) -> impl Iterator<Item = &Panel> {
        self.rows.iter().flat_map(|r| r.panels.iter())
    }

    /// The first shown panel of `view`, top to bottom and left to right.
    pub fn find(&self, view: View) -> Option<&Panel> {
        self.panels().find(|p| p.view == view)
    }

    /// The row holding `panel`'s rule.
    pub fn row_of(&self, panel: &Panel) -> Option<&LaidRow> {
        self.rows.iter().find(|r| r.panels.contains(panel))
    }

    /// The panel the bus gauges and model labels go on: the topmost, then
    /// leftmost, of starfield, castle, defrag and Die 3D; with none of them,
    /// the first panel. `None` when no panel is shown.
    pub fn gauge_panel(&self) -> Option<&Panel> {
        const HOSTS: [View; 4] = [View::Starfield, View::Castle, View::Defrag, View::Die3d];
        self.panels()
            .find(|p| HOSTS.contains(&p.view))
            .or_else(|| self.panels().next())
    }

    /// The rows that show a panel, top to bottom, as the hero's bands:
    /// `rule_y .. rule_y + height` (the old `*_start .. *_end` fields). A
    /// row given no content rows still counts, as the old fields did.
    pub fn bands(&self) -> Vec<std::ops::Range<usize>> {
        self.rows
            .iter()
            .filter(|r| !r.panels.is_empty())
            .map(|r| r.rule_y..r.rule_y + r.height)
            .collect()
    }

    /// Lines the composed arcade has: the header, every rule, every content
    /// row. The footer row is not drawn.
    pub fn lines(&self) -> usize {
        HEADER_ROWS + self.rows.iter().map(|r| 1 + r.height).sum::<usize>()
    }
}

/// Cut `total` into parts by `weights`: part `i` ends at
/// `floor(total * running_weight_i / sum)`. Returns the end of each part.
/// Integer arithmetic, so the result never depends on float rounding.
pub fn cuts(total: usize, weights: &[u32]) -> Vec<usize> {
    let sum: u64 = weights.iter().map(|&w| u64::from(w)).sum();
    if sum == 0 {
        return vec![0; weights.len()];
    }
    let mut run = 0u64;
    weights
        .iter()
        .map(|&w| {
            run += u64::from(w);
            ((total as u64 * run) / sum) as usize
        })
        .collect()
}

/// The column spans `(x, w)` of panels with `weights` across `width`: each
/// panel after the first gives its first column to the divider.
pub fn split_cols(width: usize, weights: &[u32]) -> Vec<(usize, usize)> {
    let ends = cuts(width, weights);
    let mut start = 0;
    ends.iter()
        .enumerate()
        .map(|(i, &end)| {
            let x = if i == 0 { start } else { start + 1 };
            let span = (x.min(end), end.saturating_sub(x));
            start = end;
            span
        })
        .collect()
}

/// The layout of `rows` on a `width` x `height` terminal. See the module doc.
pub fn layout(rows: &[RowSpec], width: usize, height: usize) -> Layout {
    let content = height.saturating_sub(HEADER_ROWS + rows.len() + FOOTER_ROWS);
    // Rows still holding a panel. A row that loses all its panels gives its
    // height to the others, which may let a panel elsewhere fit, so this
    // repeats until nothing changes. Only removals happen, so it ends.
    let mut alive: Vec<bool> = rows.iter().map(|r| !r.panels.is_empty()).collect();
    loop {
        let heights = row_heights(rows, &alive, content);
        let mut changed = false;
        let mut placed: Vec<(Vec<(PanelSpec, (usize, usize))>, Vec<PanelSpec>)> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let (shown, hidden) = fit_row(row, width, heights[i]);
            if alive[i] && shown.is_empty() {
                alive[i] = false;
                changed = true;
            }
            placed.push((shown, hidden));
        }
        if changed {
            continue;
        }
        // Place the rows top to bottom.
        let mut y = HEADER_ROWS;
        let mut out = Vec::with_capacity(rows.len());
        for (i, (shown, hidden)) in placed.into_iter().enumerate() {
            let h = if alive[i] { heights[i] } else { 0 };
            let rule_y = y;
            let panels = shown
                .into_iter()
                .map(|(p, (x, w))| Panel {
                    view: p.view,
                    rect: Rect {
                        x,
                        y: rule_y + 1,
                        w,
                        h,
                    },
                })
                .collect();
            let hidden = hidden
                .into_iter()
                .map(|p| (p.view, p.view.min_size()))
                .collect();
            out.push(LaidRow {
                rule_y,
                height: h,
                panels,
                hidden,
            });
            y = rule_y + 1 + h;
        }
        return Layout {
            width,
            height,
            rows: out,
        };
    }
}

/// The content height of each row: the alive rows share `content` by weight,
/// the others get 0.
fn row_heights(rows: &[RowSpec], alive: &[bool], content: usize) -> Vec<usize> {
    let weights: Vec<u32> = rows
        .iter()
        .zip(alive)
        .filter(|(_, a)| **a)
        .map(|(r, _)| r.height)
        .collect();
    let ends = cuts(content, &weights);
    let mut out = Vec::with_capacity(rows.len());
    let (mut k, mut start) = (0, 0);
    for a in alive {
        if *a {
            out.push(ends[k] - start);
            start = ends[k];
            k += 1;
        } else {
            out.push(0);
        }
    }
    out
}

/// The panels of `row` that fit at `height` rows, with their column spans,
/// and the panels taken out. The rightmost panel under its minimum goes
/// first, then the columns are shared again.
type Fitted = (Vec<(PanelSpec, (usize, usize))>, Vec<PanelSpec>);
fn fit_row(row: &RowSpec, width: usize, height: usize) -> Fitted {
    let mut shown: Vec<PanelSpec> = row.panels.clone();
    let mut hidden: Vec<PanelSpec> = Vec::new();
    loop {
        let weights: Vec<u32> = shown.iter().map(|p| p.width).collect();
        let spans = split_cols(width, &weights);
        let bad = shown
            .iter()
            .zip(&spans)
            .rposition(|(p, (_, w))| !p.view.fits(*w, height));
        match bad {
            Some(i) => hidden.push(shown.remove(i)),
            None => {
                // Hidden panels are named in config order.
                hidden.sort_by_key(|h| row.panels.iter().position(|p| p == h));
                return (shown.into_iter().zip(spans).collect(), hidden);
            }
        }
    }
}

// ---- Commands ----------------------------------------------------------------

/// The command bar's `/arcade` command: `/arcade cloudfield on|off` (or
/// `toggle`, or no value to flip it). Sets `cloudfield` and returns the
/// message for the status line and whether it is an error.
pub fn command(arg: &str, cloudfield: &mut bool) -> (String, bool) {
    let mut words = arg.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("cloudfield"), value, None) => {
            let v = match value {
                None | Some("toggle") => !*cloudfield,
                Some("on") | Some("true") => true,
                Some("off") | Some("false") => false,
                Some(other) => {
                    return (
                        format!("/arcade cloudfield: expected on or off, got {other:?}"),
                        true,
                    )
                }
            };
            *cloudfield = v;
            (cloudfield_message(v), false)
        }
        _ => (USAGE.to_string(), true),
    }
}

/// The usage line for `/arcade`.
pub const USAGE: &str = "/arcade cloudfield on|off   the atmosphere behind the arcade";

/// The status-line message for a cloudfield change.
pub fn cloudfield_message(on: bool) -> String {
    format!("arcade cloudfield: {}", if on { "on" } else { "off" })
}

/// The key that flips the cloudfield while the arcade is on screen.
pub const CLOUDFIELD_KEY: char = 'c';

// ---- Legend and explain text --------------------------------------------------

/// The arcade legend's section on the layout and the cloudfield.
pub const LEGEND_TEXT: &[&str] = &[
    "The rows and panels come from [arcade] in",
    "~/.config/tt-toplike/config.toml (or ./.toplikerc);",
    "--check-config lists any problem in them.",
    "  │       between two panels of a row",
    "  ◌ NAME  a panel too small for its view, with",
    "          the size it needs",
    "  c       cloudfield on/off: the atmosphere of",
    "          the featured inference server behind",
    "          every blank cell (off by default)",
];

/// The arcade explain text's paragraph on the layout.
pub const EXPLAIN_TEXT: &[&str] = &[
    "",
    "Layout: the [arcade] section of the config",
    "file sets the rows and their panels (starfield,",
    "castle, flow, defrag, die3d, training,",
    "inference), each with a height or width",
    "weight. Without it the arcade is the layout",
    "above. A panel too small for its view is named",
    "on its row's rule and the others take its",
    "space. The gauges and labels go on the rule",
    "of the first starfield, castle, defrag or",
    "Die 3D panel. The cloudfield (c, or /arcade",
    "cloudfield on|off) lays the inference",
    "server's atmosphere behind blank cells.",
    "See docs/arcade-layout.md.",
];

/// The legend lines for [`LEGEND_TEXT`], with the overlay's left bar.
pub fn legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};
    LEGEND_TEXT
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled("║ ", Style::default().fg(bar).bg(bg)),
                Span::styled(*s, Style::default().fg(dim)),
            ])
        })
        .collect()
}

#[cfg(test)]
#[path = "arcade_layout_tests.rs"]
mod tests;
