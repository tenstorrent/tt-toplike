// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What Defrag draws from the signal bus. Defrag is the model-loading view,
//! so the bus drives its blocks directly: a model's phase and progress on a
//! chip decide that chip's phase and how full its blocks are.
//!
//! # Two steps
//!
//! Defrag reads the world the way the arcade and Memory Castle do, through
//! [`ArcadeFx::from_world`] (see `arcade_bus`), so the modes share the
//! phase, history and away rules. `DefragVis::set_bus` runs that step once
//! per Defrag update and acts on what changed (an eviction, a burst for
//! completed requests). `DefragVis::update` then lets [`hold_of`] set the
//! phase of each chip that has a model. The frame path calls [`draw`] for
//! each device's share of a row; it does arithmetic on the chip's
//! [`ChipFx`] and never reads the world.
//!
//! A chip with nothing from the bus has no `ChipFx`: its phase comes from
//! the power and clock readings as before, and [`draw`] is not called for
//! it. An empty world draws exactly what Defrag drew before the bus.
//!
//! # What drives what
//!
//! * **`Compiling`**: the chip shows `COMPILE`, its blocks hold, and a
//!   purple scan sweeps each channel row (decoration: the scan says work is
//!   being prepared and does not say how far).
//! * **`Loading` with a progress figure**: the chip shows `DMA` and every
//!   trained channel is filled to that share (derived from the reported
//!   progress). Without a figure the write head advances at Defrag's own
//!   rate (decoration).
//! * **`Ready`**: the chip shows `RUN` with every trained channel full. A
//!   change to `Ready` washes the blocks teal and fades over [`SETTLE_NS`].
//! * **`Down`, or the model gone**: a chip whose model was up and is now
//!   down or gone is evicted: its blocks drain right to left. This replaces
//!   the local inference monitor's unload signal, so an attached TUI sees
//!   the host's unloads too. Locally, a ready model that names no chip
//!   evicts every chip when it unloads, as the monitor's signal did.
//! * **Requests**: completed requests for a model on a running chip throw a
//!   scatter burst across its blocks, brighter for more requests (the size
//!   follows the count; the cells it lights are decoration).
//! * **Sparks and kernel compiles**: one bright block in a channel row of
//!   the chip for [`SPARK_NS`], in the spark colours or the load purple
//!   (where the block is: decoration).
//! * **The label**: the model's name, phase and tokens per second at the
//!   right of the chip's power bar, in the phase colour, `~` in front for an
//!   estimate (`model_text_is_estimated`).
//! * **Reset flash**: the chip's label row shows reversed for [`FLASH_NS`].
//!
//! The model's `kernel_count` reading is not used: Defrag's block count is
//! set by the screen width.

use crate::animation::arcade_bus::{
    left, restyle, splice_str, stepped_secs, tint, ChipFx, Gauge, Particle,
};
use crate::animation::die3d::overlay::{
    FLASH_NS, GDDR_CHANNELS, LOAD_PURPLE, SERVE_TEAL, SETTLE_NS, SPARK_NS, SWEEP_PERIOD_NS,
};
use crate::ui::colors;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tt_toplike_bus::ModelPhase;
use unicode_width::UnicodeWidthStr;

const NS_PER_S: f32 = 1_000_000_000.0;

/// What the bus says a chip's blocks do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    /// Compiling: the blocks hold and a scan sweeps them.
    Compile,
    /// Loading, to this share when the source reports one.
    Load(Option<f32>),
    /// Ready: every trained channel full.
    Serve,
}

/// The hold of the chip `co`, from its model's phase and gauge. `None` for a
/// chip with no model, or one down or in alarm: the readings decide.
pub fn hold_of(co: &ChipFx) -> Option<Hold> {
    let flag = co.flag.as_ref()?;
    match flag.phase {
        ModelPhase::Compiling => Some(Hold::Compile),
        ModelPhase::Loading => Some(Hold::Load(match co.gauge {
            Some(Gauge::Progress(p)) => Some(p),
            _ => None,
        })),
        ModelPhase::Ready => Some(Hold::Serve),
        ModelPhase::Down | ModelPhase::Alarm => None,
    }
}

/// Which row of a device's share of the screen is being drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowKind {
    /// The label row (name, fill, power, clock, phase).
    Label,
    /// The power bar row.
    Power,
    /// A channel's row: `ch` is the channel, `first` is its first sub-row,
    /// `label_w` the columns its label takes, and `live` whether it has
    /// blocks (enabled and not a bad sector).
    Channel {
        ch: usize,
        first: bool,
        label_w: usize,
        live: bool,
    },
}

/// Run `f` on `spans[start..]` as one line (row 0), then put the result
/// back. Defrag builds each row device by device, so the spans from `start`
/// on are one device's share.
fn on_segment(
    spans: &mut Vec<Span<'static>>,
    start: usize,
    f: impl FnOnce(&mut [Line<'static>], usize),
) {
    if start > spans.len() {
        return;
    }
    let tail = spans.split_off(start);
    let width = tail.iter().map(|s| s.width()).sum();
    let mut one = [Line::from(tail)];
    f(&mut one, width);
    let [line] = one;
    spans.extend(line.spans);
}

fn bold(rgb: [u8; 3]) -> Style {
    Style::default()
        .fg(colors::rgb(rgb[0], rgb[1], rgb[2]))
        .add_modifier(Modifier::BOLD)
}

fn put(lines: &mut [Line<'static>], col: usize, ch: char, style: Style) {
    let mut buf = [0u8; 4];
    splice_str(lines, 0, col, ch.encode_utf8(&mut buf), style);
}

/// Draw `co` on one device's share of a row (`spans[start..]`) at bus time
/// `now`. `compiling` is whether the chip shows `COMPILE`. Arithmetic on
/// `co` only.
pub fn draw(
    co: &ChipFx,
    now: u64,
    kind: RowKind,
    compiling: bool,
    spans: &mut Vec<Span<'static>>,
    start: usize,
) {
    on_segment(spans, start, |lines, width| match kind {
        RowKind::Label => {
            if co.flash_ns.is_some_and(|t| left(t, now, FLASH_NS) > 0.0) {
                restyle(lines, 0, 0..width, |s| s.add_modifier(Modifier::REVERSED));
            }
        }
        RowKind::Power => {
            if let Some(flag) = &co.flag {
                draw_label(flag, &co.label_texts, width, lines);
            }
        }
        RowKind::Channel {
            ch,
            first,
            label_w,
            live,
        } => {
            if !live || width <= label_w {
                return;
            }
            let cells = label_w..width;
            if compiling {
                draw_scan(ch, now, cells.clone(), lines);
            }
            if let Some(Gauge::Settle { since_ns }) = co.gauge {
                let w = left(since_ns, now, SETTLE_NS);
                if w > 0.0 {
                    tint(lines, 0, cells.clone(), SERVE_TEAL, w);
                }
            }
            if first {
                for p in &co.particles {
                    draw_spark(p, now, ch, cells.clone(), lines);
                }
            }
        }
    });
}

/// The compile scan on one channel row: two purple blocks sweeping across,
/// each channel a little behind the one above. Decoration.
fn draw_scan(ch: usize, now: u64, cells: std::ops::Range<usize>, lines: &mut [Line<'static>]) {
    let period = SWEEP_PERIOD_NS as f32 / NS_PER_S;
    let phase = (stepped_secs(0, now) / period + ch as f32 * 0.11).fract();
    let at = cells.start + (phase * cells.len() as f32) as usize;
    put(lines, at, '▓', bold(LOAD_PURPLE));
    if at + 1 < cells.end {
        put(lines, at + 1, '▒', bold(LOAD_PURPLE));
    }
}

/// One bright block for a spark or compile, on the channel its sequence
/// number picks (decoration), dimming as it ages.
fn draw_spark(
    p: &Particle,
    now: u64,
    ch: usize,
    cells: std::ops::Range<usize>,
    lines: &mut [Line<'static>],
) {
    let w = left(p.start_ns, now, SPARK_NS);
    if w <= 0.0 || (p.seq % GDDR_CHANNELS as u64) as usize != ch {
        return;
    }
    let col = cells.start + (p.seq.wrapping_mul(7919) % cells.len() as u64) as usize;
    let glyph = if w > 0.5 { '█' } else { '▓' };
    put(lines, col, glyph, bold(p.rgb));
}

/// The model's label, right-aligned in the power bar row with one column
/// spare, clear of the bar's 11-column prefix. The longest of `texts` that
/// fits; with none, a bare `~` for an estimate.
fn draw_label(
    flag: &crate::animation::die3d::overlay::ModelFlag,
    texts: &[String],
    width: usize,
    lines: &mut [Line<'static>],
) {
    const PREFIX: usize = 11;
    let end = width.saturating_sub(1);
    let style = bold(flag.colour);
    let Some(text) = texts.iter().find(|t| {
        let w = UnicodeWidthStr::width(t.as_str());
        end >= w && end - w >= PREFIX
    }) else {
        if flag.estimated && end > PREFIX {
            put(lines, end - 1, '~', style);
        }
        return;
    };
    let col = end - UnicodeWidthStr::width(text.as_str());
    splice_str(lines, 0, col, text, style);
}

// ---- Legend and explain text --------------------------------------------------

/// The Defrag legend's section on what the bus draws.
pub const LEGEND_TEXT: &[&str] = &[
    "When the signal bus names a model on a chip:",
    "  COMPILE  the blocks hold and a purple scan",
    "           sweeps them (decoration)",
    "  DMA      the channels fill to the model's",
    "           reported progress; with no figure the",
    "           head moves at its own pace (decoration)",
    "  RUN      every channel full; the blocks wash",
    "           teal when the model becomes ready",
    "  EVICT    the blocks drain when the model goes",
    "           down or away",
    "  burst    completed requests flash the blocks",
    "  █        a hivemind spark or kernel compile",
    "  label    name, phase and tokens/s right of the",
    "           power bar; '~' marks an estimate",
    "  flash    the chip's label row shows reversed",
    "           after a reset",
];

/// The Defrag explain text's paragraph on what the bus draws.
pub const EXPLAIN_TEXT: &[&str] = &[
    "",
    "Signal bus: a model on a chip sets its phase.",
    "Compiling holds the blocks under a purple",
    "scan (decoration). Loading fills every channel",
    "to the reported progress. Ready fills them and",
    "washes them teal. A model that goes down or",
    "away evicts its chips. Completed requests",
    "flash the blocks, and sparks and compiles",
    "light one. The model's name, phase and",
    "tokens/s sit right of the power bar; '~'",
    "marks an estimate such as mock data. A reset",
    "flashes the chip's label row. Attached to a",
    "headless host, only this machine's chips",
    "are drawn.",
];

/// The legend lines for [`LEGEND_TEXT`], with the overlay's left bar.
pub fn legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
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
