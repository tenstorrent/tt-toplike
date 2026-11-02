// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What Memory Castle draws from the signal bus: a wing that rises in a
//! chip's tower while a model comes up, a banner when it serves, messengers
//! that climb the tower when requests complete, torches that flicker on
//! hivemind sparks and kernel compiles, cracks that heal on ECC ticks, and a
//! flash when the chip is reset.
//!
//! # Two steps
//!
//! The castle reads the world the way the arcade does, through
//! [`ArcadeFx::from_world`] (see `arcade_bus`), so the two modes agree on
//! what a model's phase change, a request, a spark and a reset are, and on
//! the history rule. `MemoryCastle::set_bus` runs that step once per castle
//! update. The frame path then calls [`draw_row`] for each tower row (and
//! [`draw_cell`] for each fleet-grid cell) with the chip's [`ChipFx`]. Those
//! two functions do arithmetic on the `ChipFx` and never read the world.
//!
//! A chip with nothing from the bus has no `ChipFx`, and the castle calls
//! neither function for it, so an empty world draws exactly what the castle
//! drew before the bus.
//!
//! # What drives what
//!
//! Each device's tower is its column of the canvas (the whole canvas with
//! one device). Row 0 is the top of the tower; the memory particles enter at
//! the bottom.
//!
//! * **The wing.** A model in `Compiling` or `Loading` raises a wing from the
//!   bottom of its chip's tower: the rows up to the reported progress are
//!   tinted in the load purple, and their empty cells fill with `░`. With no
//!   progress figure a purple band climbs the tower instead (decoration: it
//!   says the model is busy and does not say how far).
//! * **Settle and fade.** A change to `Ready` lights the tower in the serving
//!   teal and fades it over [`SETTLE_NS`]; a change to `Down` does the same
//!   in grey.
//! * **The banner** on the tower's top row: the model's name, phase and
//!   tokens per second in the phase colour, with `⚑` in front while it
//!   serves and `~` in front when its readings are estimates
//!   (`model_text_is_estimated`, through `ModelFlag::texts`). A tower too
//!   narrow for any form of the text still shows a bare `~` for an estimate.
//! * **Messengers**: each `RequestsCompleted` for a model on the chip sends
//!   one `♞` per request (up to [`MAX_HIT_GLYPHS`]) up the tower over
//!   [`PULSE_NS`].
//! * **Torches**: each hivemind spark or kernel compile naming the chip
//!   lights a flickering `☼` in the tower for [`SPARK_NS`], in the spark
//!   colours (`spark_rgb`) or the load purple.
//! * **Cracks**: each ECC tick on the chip opens a red `╳` at the bottom of
//!   the tower, under the channel's share of the width. It heals through `╱`
//!   and `·` over [`SETTLE_NS`].
//! * **Reset flash**: the whole tower shows reversed for [`FLASH_NS`] after a
//!   new `reset_seq` on the chip.
//!
//! In the fleet-grid tier (too many chips for towers) each chip's cell gets
//! the gauge on its power bar, the reset flash, and the `~` for an estimate.
//! The messengers, torches and cracks need a tower and are not drawn there.
//!
//! # Provenance
//!
//! The wing's height is derived from the reported progress. The banner's
//! numbers are what the source measured, marked `~` when estimated. The
//! following are decoration: the climbing band shown with no progress
//! figure, where in the tower a torch, messenger or crack appears, how a
//! messenger climbs, the torch flicker, the crack's healing glyphs, and the
//! lengths of the settle, fade, flash and heal.

use crate::animation::arcade_bus::{
    left, restyle, splice_str, stepped_secs, tint, ChipFx, Gauge, Hit, Particle, MAX_HIT_GLYPHS,
};
use crate::animation::die3d::overlay::{
    lerp, ModelFlag, ALARM_RED, BAND_HALF, DOWN_GREY, FLASH_NS, LOAD_PURPLE, PULSE_NS, SERVE_TEAL,
    SETTLE_NS, SPARK_NS, SWEEP_PERIOD_NS,
};
use crate::ui::colors;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tt_toplike_bus::ModelPhase;
use unicode_width::UnicodeWidthStr;

const NS_PER_S: f32 = 1_000_000_000.0;
/// How much the raised wing is tinted toward the load purple.
const WING_WEIGHT: f32 = 0.6;
/// The weight of the climbing band (no progress figure).
const BAND_WEIGHT: f32 = 0.8;

/// Run `f` on the spans `spans[start..]` as one line (row 0), then put the
/// result back. The castle builds a row device by device, so the spans from
/// `start` on are one device's share of the row.
fn on_segment(spans: &mut Vec<Span<'static>>, start: usize, f: impl FnOnce(&mut [Line<'static>])) {
    if start > spans.len() {
        return;
    }
    let tail = spans.split_off(start);
    let mut one = [Line::from(tail)];
    f(&mut one);
    let [line] = one;
    spans.extend(line.spans);
}

/// Write `ch` in `style` at column `col` of row 0 of `lines`.
fn put(lines: &mut [Line<'static>], col: usize, ch: char, style: Style) {
    let mut buf = [0u8; 4];
    splice_str(lines, 0, col, ch.encode_utf8(&mut buf), style);
}

/// The character at column `col` of row 0 of `lines`, if any.
fn char_at(lines: &[Line<'static>], col: usize) -> Option<char> {
    let mut at = 0usize;
    for span in &lines.first()?.spans {
        for ch in span.content.chars() {
            if at == col {
                return Some(ch);
            }
            at += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if at > col {
                return None;
            }
        }
    }
    None
}

/// A bold style in `rgb` on the canvas background.
fn glyph_style(rgb: [u8; 3]) -> Style {
    Style::default()
        .bg(colors::canvas_bg())
        .fg(colors::rgb(rgb[0], rgb[1], rgb[2]))
        .add_modifier(Modifier::BOLD)
}

/// Draw `co` on row `row` (0 is the top) of a tower `width` columns wide and
/// `height` rows tall, whose cells for this row are `spans[start..]`, at bus
/// time `now`. Arithmetic on `co` only.
pub fn draw_row(
    co: &ChipFx,
    now: u64,
    row: usize,
    width: usize,
    height: usize,
    spans: &mut Vec<Span<'static>>,
    start: usize,
) {
    if width == 0 || height == 0 || row >= height {
        return;
    }
    on_segment(spans, start, |lines| {
        draw_gauge(co, now, row, width, height, lines);
        if co.flash_ns.is_some_and(|t| left(t, now, FLASH_NS) > 0.0) {
            restyle(lines, 0, 0..width, |s| s.add_modifier(Modifier::REVERSED));
        }
        for p in &co.particles {
            draw_torch(p, now, row, width, height, lines);
        }
        for k in &co.cracks {
            draw_crack(k.channel, k.start_ns, now, row, width, height, lines);
        }
        for h in &co.hits {
            draw_messengers(h, now, row, width, height, lines);
        }
        if row == 0 {
            if let Some(flag) = &co.flag {
                draw_banner(flag, &co.label_texts, width, lines);
            }
        }
    });
}

/// The wing, the climbing band, the settle or the fade on one row.
fn draw_gauge(
    co: &ChipFx,
    now: u64,
    row: usize,
    width: usize,
    height: usize,
    lines: &mut [Line<'static>],
) {
    let Some(gauge) = co.gauge else {
        return;
    };
    // How far up from the bottom this row is: 0 for the bottom row, just
    // under 1 for the top row.
    let up = (height - 1 - row) as f32 / height as f32;
    match gauge {
        Gauge::Progress(p) => {
            let raised = (p * height as f32).round() as usize;
            if height - row <= raised {
                tint(lines, 0, 0..width, LOAD_PURPLE, WING_WEIGHT);
                fill_blanks(lines, '░', glyph_style(LOAD_PURPLE));
            }
        }
        Gauge::Sweep { since_ns } => {
            // Decoration: the band says the model is busy and does not say
            // how far.
            let period = SWEEP_PERIOD_NS as f32 / NS_PER_S;
            let phase = (stepped_secs(since_ns, now) / period).fract();
            let centre = phase * (1.0 + 2.0 * BAND_HALF) - BAND_HALF;
            if (up - centre).abs() <= BAND_HALF {
                tint(lines, 0, 0..width, LOAD_PURPLE, BAND_WEIGHT);
            }
        }
        Gauge::Settle { since_ns } => {
            let w = left(since_ns, now, SETTLE_NS);
            if w > 0.0 {
                tint(lines, 0, 0..width, SERVE_TEAL, w);
            }
        }
        Gauge::Fade { since_ns } => {
            let w = left(since_ns, now, SETTLE_NS);
            if w > 0.0 {
                tint(lines, 0, 0..width, DOWN_GREY, w);
            }
        }
    }
}

/// Replace each blank cell of row 0 with `ch` in `style`.
fn fill_blanks(lines: &mut [Line<'static>], ch: char, style: Style) {
    let Some(line) = lines.first_mut() else {
        return;
    };
    let mut buf = [0u8; 4];
    let glyph: &str = ch.encode_utf8(&mut buf);
    for span in line.spans.iter_mut() {
        if !span.content.is_empty() && span.content.chars().all(|c| c == ' ') {
            *span = Span::styled(glyph.repeat(span.content.chars().count()), style);
        }
    }
}

/// A torch for a spark or compile, at a place picked by its sequence number
/// (decoration), flickering while it burns down.
fn draw_torch(
    p: &Particle,
    now: u64,
    row: usize,
    width: usize,
    height: usize,
    lines: &mut [Line<'static>],
) {
    let w = left(p.start_ns, now, SPARK_NS);
    if w <= 0.0 {
        return;
    }
    if (p.seq.wrapping_mul(31) % height as u64) as usize != row {
        return;
    }
    let col = (p.seq.wrapping_mul(7919) % width as u64) as usize;
    // Decoration: the flicker alternates every step while the torch is bright.
    let step = (stepped_secs(p.start_ns, now) * 12.0) as u64;
    let ch = if w <= 0.4 {
        '·'
    } else if step % 2 == 0 {
        '☼'
    } else {
        '✶'
    };
    put(lines, col, ch, glyph_style(p.rgb));
}

/// A crack for an ECC tick on `channel`, at the bottom of the tower under
/// the channel's share of the width, healing as it ages.
fn draw_crack(
    channel: u8,
    start_ns: u64,
    now: u64,
    row: usize,
    width: usize,
    height: usize,
    lines: &mut [Line<'static>],
) {
    let w = left(start_ns, now, SETTLE_NS);
    if w <= 0.0 || row + 1 != height {
        return;
    }
    let ch_count = crate::animation::die3d::overlay::GDDR_CHANNELS as usize;
    let col = (channel as usize % ch_count) * width / ch_count + width / (2 * ch_count);
    if col >= width {
        return;
    }
    // Decoration: the healing glyphs and the colour's fade.
    let glyph = if w > 0.6 {
        '╳'
    } else if w > 0.25 {
        '╱'
    } else {
        '·'
    };
    put(
        lines,
        col,
        glyph,
        glyph_style(lerp(DOWN_GREY, ALARM_RED, w)),
    );
}

/// The messengers of one hit: one `♞` per request, spread across the tower
/// (decoration), climbing from the bottom row to the top over [`PULSE_NS`].
fn draw_messengers(
    h: &Hit,
    now: u64,
    row: usize,
    width: usize,
    height: usize,
    lines: &mut [Line<'static>],
) {
    if left(h.start_ns, now, PULSE_NS) <= 0.0 {
        return;
    }
    let t = stepped_secs(h.start_ns, now) / (PULSE_NS as f32 / NS_PER_S);
    let climbed = (t.clamp(0.0, 1.0) * (height - 1) as f32).round() as usize;
    if height - 1 - climbed.min(height - 1) != row {
        return;
    }
    let n = h.glyphs.clamp(1, MAX_HIT_GLYPHS) as usize;
    for j in 0..n {
        let col = (j * width / n + (h.seq as usize % 7)) % width;
        put(lines, col, '♞', glyph_style(SERVE_TEAL));
    }
}

/// The banner: the longest of `texts` that fits in the tower with one column
/// spare on each side, centred, with `⚑ ` in front while the model serves
/// and there is room. With no form that fits, an estimate still gets a bare
/// `~`.
fn draw_banner(flag: &ModelFlag, texts: &[String], width: usize, lines: &mut [Line<'static>]) {
    let room = width.saturating_sub(2);
    let style = glyph_style(flag.colour);
    let flown = flag.phase == ModelPhase::Ready;
    let fits = |t: &String| UnicodeWidthStr::width(t.as_str()) <= room;
    let Some(text) = texts.iter().find(|t| fits(t)) else {
        if flag.estimated && room >= 1 {
            put(lines, 1, '~', style);
        }
        return;
    };
    let w = UnicodeWidthStr::width(text.as_str());
    let with_flag = flown && w + 2 <= room;
    let total = w + if with_flag { 2 } else { 0 };
    let col = 1 + (room - total) / 2;
    if with_flag {
        put(lines, col, '⚑', style);
        put(lines, col + 1, ' ', style);
        splice_str(lines, 0, col + 2, text, style);
    } else {
        splice_str(lines, 0, col, text, style);
    }
}

/// Draw `co` on one fleet-grid cell, whose spans are `spans[start..]`: the
/// gauge on the power bar (`bar` columns of the cell), the reset flash, and
/// a `~` for an estimate in the label's pad column (the bar's first column
/// when the label has no pad). Arithmetic on `co`
/// only.
pub fn draw_cell(
    co: &ChipFx,
    now: u64,
    bar: std::ops::Range<usize>,
    spans: &mut Vec<Span<'static>>,
    start: usize,
) {
    on_segment(spans, start, |lines| {
        let len = bar.len() as f32;
        match co.gauge {
            Some(Gauge::Progress(p)) => {
                let end = bar.start + (p * len).round() as usize;
                tint(lines, 0, bar.start..end.min(bar.end), LOAD_PURPLE, 1.0);
            }
            Some(Gauge::Sweep { since_ns }) => {
                // Decoration, as in the tower.
                let period = SWEEP_PERIOD_NS as f32 / NS_PER_S;
                let phase = (stepped_secs(since_ns, now) / period).fract();
                let centre = phase * (1.0 + 2.0 * BAND_HALF) - BAND_HALF;
                let lo = ((centre - BAND_HALF).max(0.0) * len).floor() as usize;
                let hi = ((centre + BAND_HALF).clamp(0.0, 1.0) * len).ceil() as usize;
                if hi > lo {
                    tint(
                        lines,
                        0,
                        (bar.start + lo)..(bar.start + hi).min(bar.end),
                        LOAD_PURPLE,
                        BAND_WEIGHT,
                    );
                }
            }
            Some(Gauge::Settle { since_ns }) => {
                let w = left(since_ns, now, SETTLE_NS);
                if w > 0.0 {
                    tint(lines, 0, bar.clone(), SERVE_TEAL, w);
                }
            }
            Some(Gauge::Fade { since_ns }) => {
                let w = left(since_ns, now, SETTLE_NS);
                if w > 0.0 {
                    tint(lines, 0, bar.clone(), DOWN_GREY, w);
                }
            }
            None => {}
        }
        if co.flash_ns.is_some_and(|t| left(t, now, FLASH_NS) > 0.0) {
            restyle(lines, 0, 0..usize::MAX, |s| {
                s.add_modifier(Modifier::REVERSED)
            });
        }
        // The `~` goes in the label's pad column. A label that fills its
        // columns has no pad, so the `~` takes the bar's first column and
        // the label keeps its last character.
        if let Some(flag) = &co.flag {
            if flag.estimated && bar.start > 0 {
                let col = match char_at(lines, bar.start - 1) {
                    Some(' ') => bar.start - 1,
                    _ => bar.start,
                };
                put(lines, col, '~', glyph_style(flag.colour));
            }
        }
    });
}

// ---- Legend and explain text --------------------------------------------------

/// The castle legend's section on what the bus draws.
pub const LEGEND_TEXT: &[&str] = &[
    "Each chip's tower shows what the signal bus says:",
    "  wing    a purple wing climbs the tower as a model",
    "          compiles or loads, as far as its progress;",
    "          with no figure a band climbs (decoration)",
    "  banner  model, phase and tokens/s on the top row;",
    "          ⚑ while it serves; '~' marks an estimate",
    "  ♞       climbs per completed request (8 max)",
    "  ☼       a hivemind spark or kernel compile",
    "  ╳       an ECC tick; it heals over two seconds",
    "  flash   the tower shows reversed after a reset",
];

/// The castle explain text's paragraph on what the bus draws.
pub const EXPLAIN_TEXT: &[&str] = &[
    "",
    "Signal bus: a model compiling or loading raises a",
    "purple wing up its chip's tower, as far as its",
    "progress. With no figure a band climbs instead, which",
    "is decoration. The tower lights teal at ready and grey",
    "when the model goes down. A banner names the model,",
    "phase and tokens/s ('~' marks an estimate). Requests",
    "send ♞ knights up the tower, sparks light ☼, an ECC",
    "tick opens a ╳ that heals, and a reset flashes it all.",
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
