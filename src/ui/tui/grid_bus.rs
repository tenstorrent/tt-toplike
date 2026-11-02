// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What Grid mode and the Insights chip panels draw from the signal bus.
//!
//! # Two steps
//!
//! The shape is Die 3D's overlay (`die3d::overlay`) and the arcade's
//! (`arcade_bus`):
//!
//! 1. At the data rate, in the mode's update arm, [`GridBus::update`] and
//!    [`ChipFlags::from_world`] read the world through [`WorldRead`] and keep
//!    what they found: for each chip, the model on it, its text forms worked
//!    out in advance, and (for Grid) the gauge, pulses, sparks and reset
//!    flash with their start times.
//! 2. On the frame, [`draw_grid`] and [`ChipFlags::footer_line`] turn that
//!    into cells with arithmetic only. They never read the world.
//!
//! An empty world (no model, no event, no reset) leaves both empty, and an
//! empty one draws nothing: Grid and the Insights panels then draw exactly
//! what they drew before the bus.
//!
//! # Grid
//!
//! Each chip's cell is the portrait inside a left bar (`╔ ║ ╚`) and a bottom
//! rule (`╚═══`). The bus puts these on a chip's cell:
//!
//! * **The model's label** on the top row after the device label, and **its
//!   phase** on the bottom rule with progress while loading and tokens per
//!   second when they fit, both in the phase's colour. The label is cut with
//!   `…` to the room left. `~` comes first on the rule when the readings are
//!   estimates.
//! * **The bring-up bar** on the left bar. While the model compiles or
//!   loads, the bar fills from the bottom up as far as the reported
//!   progress. With no progress figure a band climbs the bar (decoration).
//!   A change to `Ready` lights the bar in the serving teal and fades it; a
//!   change to `Down` does the same in grey. It is the arcade's gauge,
//!   drawn vertically.
//! * **A pulse per request event.** A `RequestsCompleted` event for the
//!   model on this chip sends a teal light up the PCIe spine (`╋`) of a
//!   Blackhole portrait. A Wormhole portrait has no PCIe cell, so the pulse
//!   climbs the left bar instead.
//! * **Sparks and compiles**: a hivemind `Spark` or a `KernelCompiled` event
//!   naming the chip puts a `*` on one Tensix cell, in the spark colours
//!   (purple for a compile), fading to `·`.
//! * **A reset flash**: a new `reset_seq` on the chip shows the portrait
//!   reversed for a moment.
//!
//! # Insights
//!
//! The footer rule under each chip panel shows the model on that chip:
//! label, phase word, progress while loading, tokens per second and waiting
//! requests, in the phase's colour, with `~` for estimates. While the model
//! compiles or loads, the dashes after the text are a progress bar, purple
//! as far as the reported progress. The Die 3D inset is drawn by Die 3D and is not
//! part of this module.
//!
//! # Rules shared with Die 3D and the arcade
//!
//! * A model with an empty chip list is skipped. Attached to a headless
//!   host, `TuiBus::read` cuts the host's chips away, so only this
//!   machine's chips are drawn.
//! * A phase change is a change against the phase last seen. The first
//!   sight of a model is not a change.
//! * Events already in the world at the first read are history and draw
//!   nothing. So do events that arrived while the view was away: an update
//!   more than [`PULSE_NS`] after the last one, and more than four times the
//!   usual gap between updates, records the newest event and draws nothing
//!   for it. Die 3D's rule, with its constants.
//!
//! # Provenance
//!
//! The text's numbers are what the source measured, and `~` marks readings
//! that are `Estimated` or `Decoration`, through `model_text_is_estimated`.
//! The bar's height and the footer's purple length are derived from the
//! reported progress. Decoration: the climbing band shown with no progress
//! figure, the travel and fade of a pulse, which Tensix cell a spark lands
//! on, and the length of every fade and flash.

use crate::animation::arcade_bus::{gauge_of, left, stepped_secs, Gauge, PhaseMemo};
use crate::animation::die3d::overlay::{
    flag_of, lerp, spark_rgb, ModelFlag, BAND_HALF, DOWN_GREY, FLASH_NS, LOAD_PURPLE, MAX_PULSES,
    MAX_SPARKS, PULSE_NS, SERVE_TEAL, SETTLE_NS, SPARK_NS, SWEEP_PERIOD_NS,
};
use crate::models::{Architecture, Device};
use crate::ui::colors;
use crate::ui::tui::chip_portrait::{core_type_bh, core_type_wh, portrait_dims, CoreType};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, Event, ModelPhase, ModelView, WorldRead};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const NS_PER_S: u64 = 1_000_000_000;

// ---- Text forms -------------------------------------------------------------

/// The numbers that follow the phase in the flag's text: progress while
/// loading and tokens per second, each after a space (`" 40%"`,
/// `" 42t/s"`), or empty. Taken from `ModelFlag::texts`, whose second form
/// is `{tilde}{label} {phase}{numbers}`, so the two never disagree.
fn numbers_of(flag: &ModelFlag) -> String {
    let tilde = if flag.estimated { "~" } else { "" };
    let head = format!("{tilde}{} {}", flag.label, flag.phase_word());
    flag.texts()
        .iter()
        .find_map(|t| {
            t.strip_prefix(head.as_str())
                .filter(|n| !n.contains(" waiting"))
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// The text forms of `flag` for an Insights rule, longest first: the flag's
/// own forms (`ModelFlag::texts`), then the phase with its numbers, then the
/// phase alone. Each starts with `~` when the readings are estimates.
pub(crate) fn text_forms(flag: &ModelFlag) -> Vec<String> {
    let mut out = flag.texts();
    out.extend(rule_forms(flag));
    // Keep the order, drop repeats.
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(t.clone()));
    out
}

/// The text forms of `flag` for a Grid cell's bottom rule, longest first:
/// the phase with its numbers, then the phase alone. The label goes on the
/// cell's top row. Each starts with `~` when the readings are estimates.
pub(crate) fn rule_forms(flag: &ModelFlag) -> Vec<String> {
    let tilde = if flag.estimated { "~" } else { "" };
    let phase = flag.phase_word();
    let numbers = numbers_of(flag);
    let mut out = Vec::with_capacity(2);
    if !numbers.is_empty() {
        out.push(format!("{tilde}{phase}{numbers}"));
    }
    out.push(format!("{tilde}{phase}"));
    out
}

/// The first of `forms` that is at most `width` columns wide.
fn fitting(forms: &[String], width: usize) -> Option<&str> {
    forms
        .iter()
        .find(|t| UnicodeWidthStr::width(t.as_str()) <= width)
        .map(String::as_str)
}

/// The colour of `rgb` in the current theme.
fn rgb(c: [u8; 3]) -> Color {
    colors::rgb(c[0], c[1], c[2])
}

// ---- Insights ---------------------------------------------------------------

/// The model on each of the Insights screen's chips, for the footer rules.
/// Built at the data rate by [`ChipFlags::from_world`].
#[derive(Debug, Clone, Default)]
pub(crate) struct ChipFlags {
    /// By device index: the flag and its text forms ([`text_forms`]).
    chips: Vec<(usize, ModelFlag, Vec<String>)>,
    /// The world came from an attached host ([`ChipFlags::with_attached`]).
    /// The process panel's serving row then reads the world's model.
    attached: bool,
}

impl ChipFlags {
    /// The models on `devices`' chips in `world`. A device with no PCI
    /// address, or whose model's chip list is empty, gets nothing.
    pub(crate) fn from_world(world: &dyn WorldRead, devices: &[Device]) -> ChipFlags {
        let chips = devices
            .iter()
            .filter(|d| !d.bus_id.is_empty())
            .filter_map(|d| {
                let m = world
                    .model_on_chip(&EntityKey::chip(&d.bus_id))
                    .filter(|m| !m.chips.is_empty())?;
                let flag = flag_of(world, &m);
                let forms = text_forms(&flag);
                Some((d.index, flag, forms))
            })
            .collect();
        ChipFlags {
            chips,
            attached: false,
        }
    }

    /// Mark these flags as read from an attached host's world (see
    /// [`ChipFlags::process_row`]).
    pub(crate) fn with_attached(mut self, attached: bool) -> ChipFlags {
        self.attached = attached;
        self
    }

    /// The process panel's serving row for a process on `devices`, from the
    /// world's model on the first of them that has one. `None` on a local
    /// bus, where the caller keeps the local monitor's row, and when no
    /// device has a model.
    pub(crate) fn process_row(&self, devices: &[usize]) -> Option<Line<'static>> {
        if !self.attached {
            return None;
        }
        let (flag, _) = devices.iter().find_map(|&d| self.get(d))?;
        // The local row's shape: `[flavour] phase  model=..  N tok/s`. The
        // source is the bus, so `[bus]`; `~` marks an estimate.
        let ready = flag.phase == ModelPhase::Ready;
        let mut spans = vec![
            Span::raw("       "),
            Span::styled("[bus] ", Style::default().fg(Color::Cyan)),
            Span::styled(
                flag.phase_word(),
                Style::default().fg(if ready { Color::Green } else { Color::Yellow }),
            ),
        ];
        let tilde = if flag.estimated { "~" } else { "" };
        let mut parts = vec![format!(
            "model={tilde}{}",
            crate::ui::tui::truncate(&flag.label, 24)
        )];
        if let (ModelPhase::Compiling | ModelPhase::Loading, Some(p)) = (flag.phase, flag.progress)
        {
            if p.is_finite() {
                parts.push(format!("{:.0}%", p.clamp(0.0, 1.0) * 100.0));
            }
        }
        if let Some(t) = flag.tokens_per_s.filter(|t| t.is_finite()) {
            parts.push(format!("{t:.0}tok/s"));
        }
        if let Some(w) = flag.waiting.filter(|w| *w > 0) {
            parts.push(format!("{w} waiting"));
        }
        spans.push(Span::styled(
            format!("  {}", parts.join("  ")),
            Style::default().fg(Color::Gray),
        ));
        Some(Line::from(spans))
    }

    /// The flag and text forms of device `index`.
    fn get(&self, index: usize) -> Option<(&ModelFlag, &[String])> {
        self.chips
            .iter()
            .find(|(i, _, _)| *i == index)
            .map(|(_, f, t)| (f, t.as_slice()))
    }

    /// The footer rule of device `index`'s panel, `width` columns wide, with
    /// the model's text after `── `, or `None` when the chip has no model
    /// (the caller draws its plain rule). While the model compiles or
    /// loads, the dashes after the text are a progress bar: purple as far
    /// as the reported progress, dark grey past it, as the plain rule is.
    pub(crate) fn footer_line(&self, index: usize, width: usize) -> Option<Line<'static>> {
        let (flag, forms) = self.get(index)?;
        // `── ` before the text, ` ─` after it at least.
        let text = fitting(forms, width.saturating_sub(5))?;
        let text_w = UnicodeWidthStr::width(text);
        // The dashes after the text are the progress bar.
        let start = 3 + text_w + 1;
        let count = width.saturating_sub(start);
        let purple = match (flag.phase, flag.progress) {
            (ModelPhase::Compiling | ModelPhase::Loading, Some(p)) if p.is_finite() => {
                (p.clamp(0.0, 1.0) * count as f32).round() as usize
            }
            _ => 0,
        };
        let mut spans = Vec::with_capacity(6);
        dashes(2, 0, &mut spans);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            text.to_string(),
            Style::default()
                .fg(rgb(flag.colour))
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        dashes(count, purple, &mut spans);
        Some(Line::from(spans))
    }
}

/// `count` dashes, the first `purple` of them purple and the rest dark grey,
/// as at most two spans.
fn dashes(count: usize, purple: usize, out: &mut Vec<Span<'static>>) {
    let purple = purple.min(count);
    if purple > 0 {
        out.push(Span::styled(
            "─".repeat(purple),
            Style::default().fg(rgb(LOAD_PURPLE)),
        ));
    }
    if count > purple {
        out.push(Span::styled(
            "─".repeat(count - purple),
            Style::default().fg(Color::DarkGray),
        ));
    }
}

// ---- Grid: the data rate ----------------------------------------------------

/// One spark or compile on a chip's portrait.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Spark {
    pub(crate) start_ns: u64,
    /// The event's sequence number, which picks the Tensix cell
    /// (decoration).
    pub(crate) seq: u64,
    pub(crate) rgb: [u8; 3],
}

/// Everything the bus puts on one chip's Grid cell.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct GridChip {
    /// The chip's position among the devices: its cell.
    pub(crate) cell: usize,
    pub(crate) flag: Option<ModelFlag>,
    /// The bottom rule's text forms ([`rule_forms`]), longest first, so the
    /// frame path builds no strings.
    pub(crate) forms: Vec<String>,
    pub(crate) gauge: Option<Gauge>,
    /// When each pulse started.
    pub(crate) pulses: Vec<u64>,
    pub(crate) sparks: Vec<Spark>,
    /// When the chip's last reset flash started.
    pub(crate) flash_ns: Option<u64>,
}

impl GridChip {
    fn is_empty(&self) -> bool {
        self.flag.is_none()
            && self.gauge.is_none()
            && self.pulses.is_empty()
            && self.sparks.is_empty()
            && self.flash_ns.is_none()
    }
}

/// What the bus puts on the Grid. See the module doc.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct GridFx {
    /// The bus time of the update that built it; the frame draws at it.
    pub(crate) now_ns: u64,
    /// One entry per chip that has something, in cell order.
    pub(crate) chips: Vec<GridChip>,
}

/// What the Grid remembers between two updates. Start with the default, and
/// set a new default when Grid is left, so the next world is a first sight.
#[derive(Debug, Clone, Default)]
pub(crate) struct GridBusState {
    /// The highest event sequence number handled. `None` before the first
    /// world.
    seen_seq: Option<u64>,
    last_now: Option<u64>,
    /// The time between the last two updates.
    last_gap: Option<u64>,
    /// By model key.
    phases: HashMap<EntityKey, PhaseMemo>,
    /// Pulse start times, by chip key.
    pulses: HashMap<EntityKey, Vec<u64>>,
    /// By chip key.
    sparks: HashMap<EntityKey, Vec<Spark>>,
    /// The last `reset_seq` seen, by chip key.
    resets: HashMap<EntityKey, Option<u64>>,
    /// When each chip's reset flash started, by chip key.
    flashes: HashMap<EntityKey, u64>,
}

impl GridFx {
    /// The effects of `chips` (each a device's cell and its chip key,
    /// `EntityKey::chip(bus_id)`) from `world` at bus time `now`, updating
    /// `prev`. See the module doc.
    pub(crate) fn from_world(
        world: &dyn WorldRead,
        chips: &[(usize, EntityKey)],
        now: u64,
        prev: &mut GridBusState,
    ) -> GridFx {
        let drawn = |c: &EntityKey| chips.iter().any(|(_, k)| k == c);
        let models = world.models();

        // Die 3D's first-sight and away rule, as the arcade has it.
        let first = prev.seen_seq.is_none();
        let head = world.seq();
        let gap = prev.last_now.map(|t| now.saturating_sub(t));
        let away = gap.is_some_and(|g| {
            g > PULSE_NS
                && prev
                    .last_gap
                    .is_none_or(|usual| g > usual.saturating_mul(4))
        });
        prev.last_now = Some(now);
        if gap.is_some() {
            prev.last_gap = gap;
        }
        let since = prev.seen_seq.filter(|s| *s <= head && !away);
        let mut seen = head;
        if let Some(s) = since {
            for (q, e) in world.events_since(s) {
                seen = seen.max(q);
                match e {
                    Event::RequestsCompleted { model, n } if n > 0 => {
                        // One pulse per event on each drawn chip of the model.
                        if let Some(m) = models.iter().find(|m| m.key == model) {
                            for c in m.chips.iter().filter(|c| drawn(c)) {
                                add_pulse(prev.pulses.entry(c.clone()).or_default(), now);
                            }
                        }
                    }
                    Event::Spark {
                        chip: Some(c),
                        kind,
                    } if drawn(&c) => add_spark(prev, c, now, q, spark_rgb(&kind)),
                    Event::KernelCompiled { chip: Some(c), .. } if drawn(&c) => {
                        add_spark(prev, c, now, q, LOAD_PURPLE)
                    }
                    // Model events are announcements: the phase is read from
                    // the model below. Resets are read from `reset_seq`.
                    _ => {}
                }
            }
        }
        prev.seen_seq = Some(seen);

        // Resets: a `reset_seq` that moved since the last update flashes the
        // chip. The first sight, and an update after being away, only record.
        for (_, key) in chips {
            let latest = world.reset_seq(key);
            let before = prev.resets.insert(key.clone(), latest);
            if let Some(before) = before {
                if latest.is_some() && latest != before && !first && since.is_some() {
                    prev.flashes.insert(key.clone(), now);
                }
            }
        }
        prev.resets.retain(|k, _| drawn(k));

        // Phases: a change against the last phase seen is timed from now.
        prev.phases
            .retain(|k, _| models.iter().any(|m| m.key == *k));
        for m in &models {
            let memo = prev.phases.entry(m.key.clone()).or_insert(PhaseMemo {
                phase: m.phase,
                since_ns: None,
            });
            if memo.phase != m.phase {
                *memo = PhaseMemo {
                    phase: m.phase,
                    since_ns: Some(now),
                };
            }
        }

        let mut out = GridFx {
            now_ns: now,
            chips: Vec::new(),
        };
        for (cell, key) in chips {
            let mut co = GridChip {
                cell: *cell,
                ..GridChip::default()
            };
            if let Some(m) = world
                .model_on_chip(key)
                .filter(|m: &ModelView| !m.chips.is_empty())
            {
                let flag = flag_of(world, &m);
                co.forms = rule_forms(&flag);
                co.flag = Some(flag);
                co.gauge = gauge_of(&m, prev.phases.get(&m.key), now);
            }
            if let Some(p) = prev.pulses.get_mut(key) {
                p.retain(|t| now.saturating_sub(*t) < PULSE_NS);
                co.pulses = p.clone();
            }
            if let Some(s) = prev.sparks.get_mut(key) {
                s.retain(|s| now.saturating_sub(s.start_ns) < SPARK_NS);
                co.sparks = s.clone();
            }
            co.flash_ns = prev
                .flashes
                .get(key)
                .copied()
                .filter(|t| now.saturating_sub(*t) < FLASH_NS);
            if !co.is_empty() {
                out.chips.push(co);
            }
        }
        // Nothing is kept for a chip no longer drawn, or an effect that ended.
        prev.pulses.retain(|k, p| drawn(k) && !p.is_empty());
        prev.sparks.retain(|k, s| drawn(k) && !s.is_empty());
        prev.flashes
            .retain(|k, t| drawn(k) && now.saturating_sub(*t) < FLASH_NS);
        out
    }
}

/// Add a pulse at `now`, keeping at most [`MAX_PULSES`]: past that the
/// newest starts again, as Die 3D merges pulses.
fn add_pulse(list: &mut Vec<u64>, now: u64) {
    list.retain(|t| now.saturating_sub(*t) < PULSE_NS);
    if list.len() < MAX_PULSES {
        list.push(now);
    } else if let Some(last) = list.last_mut() {
        *last = now;
    }
}

/// Add a spark on `chip` for the event with sequence number `seq`, keeping
/// at most [`MAX_SPARKS`] per chip.
fn add_spark(prev: &mut GridBusState, chip: EntityKey, now: u64, seq: u64, rgb: [u8; 3]) {
    let list = prev.sparks.entry(chip).or_default();
    list.retain(|s| now.saturating_sub(s.start_ns) < SPARK_NS);
    if list.len() >= MAX_SPARKS {
        list.remove(0);
    }
    list.push(Spark {
        start_ns: now,
        seq,
        rgb,
    });
}

/// The Grid's bus effects and its memory, kept by the TUI loop.
#[derive(Debug, Clone, Default)]
pub(crate) struct GridBus {
    state: GridBusState,
    fx: GridFx,
}

impl GridBus {
    /// Read `world` for `devices`, at the world's own clock. Runs in the Grid
    /// update arm, at the data rate.
    pub(crate) fn update(&mut self, world: &dyn WorldRead, devices: &[Device]) {
        // The bus names a chip by its PCI address, never by position.
        let chips: Vec<(usize, EntityKey)> = devices
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.bus_id.is_empty())
            .map(|(cell, d)| (cell, EntityKey::chip(&d.bus_id)))
            .collect();
        self.fx = GridFx::from_world(world, &chips, world.now_mono_ns(), &mut self.state);
    }

    pub(crate) fn fx(&self) -> &GridFx {
        &self.fx
    }
}

// ---- Grid: the frame path ---------------------------------------------------

/// Where one chip's Grid cell is on screen. The same arithmetic as
/// `render_grid_mode`; `grid_cells_match_what_render_grid_mode_draws` holds
/// the two together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellGeom {
    /// The device's position: its cell.
    pub(crate) cell: usize,
    pub(crate) arch: Architecture,
    /// The left bar's column and the label row.
    pub(crate) x: u16,
    pub(crate) y: u16,
    /// The cell's height: label row, portrait rows, bottom rule.
    pub(crate) h: u16,
    /// The portrait's size.
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

/// The cells `render_grid_mode` draws for `devices` in `area`, in device
/// order. No allocation.
pub(crate) fn grid_cells(area: Rect, devices: &[Device]) -> impl Iterator<Item = CellGeom> + '_ {
    let first = devices.first().map(|d| d.architecture);
    let (gc, gr) = portrait_dims(first.unwrap_or(Architecture::Blackhole));
    let cell_w = gc as u16 + 2;
    let cell_h = gr as u16 + 2;
    let grid_y = area.y + 1;
    let grid_h = area.height.saturating_sub(1);
    let per_row = (area.width / cell_w).max(1) as usize;
    let rows_visible = (grid_h / cell_h).max(1) as usize;
    devices
        .iter()
        .enumerate()
        .take(if first.is_some() {
            per_row * rows_visible
        } else {
            0
        })
        .map(move |(i, d)| {
            let (pc, pr) = portrait_dims(d.architecture);
            CellGeom {
                cell: i,
                arch: d.architecture,
                x: area.x + (i % per_row) as u16 * cell_w,
                y: grid_y + (i / per_row) as u16 * cell_h,
                h: cell_h,
                cols: pc as u16,
                rows: pr as u16,
            }
        })
}

/// Draw `fx` onto the Grid screen `buf` drawn by `render_grid_mode` for
/// `devices` in `area`. Arithmetic on `fx` only. An empty `fx` changes
/// nothing.
pub(crate) fn draw_grid(buf: &mut Buffer, area: Rect, devices: &[Device], fx: &GridFx) {
    if fx.chips.is_empty() {
        return;
    }
    let now = fx.now_ns;
    for g in grid_cells(area, devices) {
        let Some(co) = fx.chips.iter().find(|c| c.cell == g.cell) else {
            continue;
        };
        if co.flash_ns.is_some_and(|t| left(t, now, FLASH_NS) > 0.0) {
            for y in g.y + 1..g.y + 1 + g.rows {
                for x in g.x + 1..g.x + 1 + g.cols {
                    if let Some(c) = buf.cell_mut((x, y)) {
                        c.modifier.insert(Modifier::REVERSED);
                    }
                }
            }
        }
        draw_bar(buf, &g, co.gauge, now);
        for s in &co.sparks {
            draw_spark(buf, &g, s, now);
        }
        for p in &co.pulses {
            draw_pulse(buf, &g, *p, now);
        }
        if let Some(flag) = &co.flag {
            draw_label(buf, &g, flag);
            draw_text(buf, &g, flag, &co.forms);
        }
    }
}

/// The left bar's rows, bottom first: `(x, y)` of the `i`-th of `n`.
fn bar_cell(g: &CellGeom, i: u16) -> (u16, u16) {
    (g.x, g.y + g.h - 2 - i)
}

/// The rows of the left bar between the corners.
fn bar_len(g: &CellGeom) -> u16 {
    g.h.saturating_sub(2)
}

/// Blend the foreground of cell `(x, y)` toward `to` by `weight`.
fn tint(buf: &mut Buffer, x: u16, y: u16, to: [u8; 3], weight: f32) {
    if let Some(c) = buf.cell_mut((x, y)) {
        let base = match c.fg {
            Color::Rgb(r, g, b) => [r, g, b],
            _ => to,
        };
        c.fg = rgb(lerp(base, to, weight.clamp(0.0, 1.0)));
    }
}

/// The bring-up bar on the left bar of `g`.
fn draw_bar(buf: &mut Buffer, g: &CellGeom, gauge: Option<Gauge>, now: u64) {
    let Some(gauge) = gauge else {
        return;
    };
    let n = bar_len(g);
    match gauge {
        Gauge::Progress(p) => {
            let filled = ((p * n as f32).round() as u16).min(n);
            for i in 0..filled {
                let (x, y) = bar_cell(g, i);
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_char('┃');
                    c.fg = rgb(LOAD_PURPLE);
                    c.modifier.insert(Modifier::BOLD);
                }
            }
        }
        Gauge::Sweep { since_ns } => {
            // Decoration: the band says the model is busy and does not say
            // how far.
            let period = SWEEP_PERIOD_NS as f32 / NS_PER_S as f32;
            let phase = (stepped_secs(since_ns, now) / period).fract();
            let centre = phase * (1.0 + 2.0 * BAND_HALF) - BAND_HALF;
            let lo = ((centre - BAND_HALF).max(0.0) * n as f32).floor() as u16;
            let hi = ((centre + BAND_HALF).clamp(0.0, 1.0) * n as f32).ceil() as u16;
            for i in lo..hi.min(n) {
                let (x, y) = bar_cell(g, i);
                tint(buf, x, y, LOAD_PURPLE, 0.8);
            }
        }
        Gauge::Settle { since_ns } | Gauge::Fade { since_ns } => {
            let to = if matches!(gauge, Gauge::Settle { .. }) {
                SERVE_TEAL
            } else {
                DOWN_GREY
            };
            let w = left(since_ns, now, SETTLE_NS);
            if w > 0.0 {
                for i in 0..n {
                    let (x, y) = bar_cell(g, i);
                    tint(buf, x, y, to, w);
                }
            }
        }
    }
}

/// The path a pulse climbs, bottom first: the `i`-th of [`pulse_len`] cells.
/// A Blackhole portrait's PCIe spine (column 8, the rows that are PCIe
/// cells); for any other portrait, the left bar.
fn pulse_cell(g: &CellGeom, i: u16) -> (u16, u16) {
    match g.arch {
        // Spine rows 1..=10; row 10 is the bottom one.
        Architecture::Blackhole => (g.x + 1 + 8, g.y + 1 + 10 - i),
        _ => bar_cell(g, i),
    }
}

fn pulse_len(g: &CellGeom) -> u16 {
    match g.arch {
        Architecture::Blackhole => 10,
        _ => bar_len(g),
    }
}

/// A pulse that started at `start`, climbing its path over [`PULSE_NS`].
/// The travel and the dimmer cell behind the head are decoration.
fn draw_pulse(buf: &mut Buffer, g: &CellGeom, start: u64, now: u64) {
    let n = pulse_len(g);
    if n == 0 || left(start, now, PULSE_NS) <= 0.0 {
        return;
    }
    let frac = stepped_secs(start, now) / (PULSE_NS as f32 / NS_PER_S as f32);
    let head = ((frac * n as f32) as u16).min(n - 1);
    let (x, y) = pulse_cell(g, head);
    if let Some(c) = buf.cell_mut((x, y)) {
        c.fg = rgb(SERVE_TEAL);
        c.modifier.insert(Modifier::BOLD);
    }
    if head > 0 {
        let (x, y) = pulse_cell(g, head - 1);
        tint(buf, x, y, SERVE_TEAL, 0.5);
    }
}

/// A spark on one Tensix cell of the portrait, picked by its sequence
/// number (decoration), as `*` fading to `·`.
fn draw_spark(buf: &mut Buffer, g: &CellGeom, s: &Spark, now: u64) {
    let w = left(s.start_ns, now, SPARK_NS);
    if w <= 0.0 || g.cols < 3 || g.rows < 3 {
        return;
    }
    let is_tensix = |c: usize, r: usize| match g.arch {
        Architecture::Blackhole => core_type_bh(c, r) == CoreType::Tensix,
        Architecture::Wormhole => core_type_wh(c, r) == CoreType::Tensix,
        _ => true,
    };
    let mut col = 1 + (s.seq.wrapping_mul(7919) % (g.cols as u64 - 2)) as usize;
    let mut row = 1 + (s.seq.wrapping_mul(31) % (g.rows as u64 - 2)) as usize;
    // Off a PCIe or DRAM column, or an ETH row, to the next cell.
    if !is_tensix(col, row) {
        col = (col + 1).min(g.cols as usize - 2);
    }
    if !is_tensix(col, row) {
        row = (row + 1).min(g.rows as usize - 2);
    }
    if !is_tensix(col, row) {
        return;
    }
    let (x, y) = (g.x + 1 + col as u16, g.y + 1 + row as u16);
    if let Some(c) = buf.cell_mut((x, y)) {
        c.set_char(if w > 0.5 { '*' } else { '·' });
        c.fg = rgb(s.rgb);
        c.modifier.insert(Modifier::BOLD);
    }
}

/// The model's label on the cell's top row, one space after the device
/// label, in the phase's colour, cut with `…` to the room left. Nothing
/// when fewer than two columns are left.
fn draw_label(buf: &mut Buffer, g: &CellGeom, flag: &ModelFlag) {
    let end = g.x + 1 + g.cols;
    // Where the device label ends: after its last non-blank cell.
    let used = (g.x + 1..end)
        .rev()
        .find(|&x| buf.cell((x, g.y)).is_some_and(|c| c.symbol() != " "))
        .map_or(g.x + 1, |x| x + 1);
    let start = used + 1;
    let room = end.saturating_sub(start);
    if room < 2 {
        return;
    }
    let style = Style::default()
        .fg(rgb(flag.colour))
        .add_modifier(Modifier::BOLD);
    let fits = UnicodeWidthStr::width(flag.label.as_str()) <= room as usize;
    let mut x = start;
    for ch in flag.label.chars() {
        let w = ch.width().unwrap_or(0) as u16;
        if w == 0 {
            continue;
        }
        // Keep a column for the `…` when the label is cut.
        let limit = if fits { end } else { end - 1 };
        if x + w > limit {
            break;
        }
        put(buf, x, g.y, ch, w, style);
        x += w;
    }
    if !fits {
        put(buf, x, g.y, '\u{2026}', 1, style);
    }
}

/// Write `ch`, `w` columns wide, at `(x, y)` in `style`. The second column of
/// a wide character is left blank, as ratatui does.
fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, w: u16, style: Style) {
    if let Some(c) = buf.cell_mut((x, y)) {
        c.set_char(ch);
        c.set_style(style);
    }
    for k in 1..w {
        if let Some(c) = buf.cell_mut((x + k, y)) {
            c.set_symbol("");
        }
    }
}

/// The phase and numbers on the bottom rule, one `═` in from each end: the
/// longest form that fits. When none fits, an estimate keeps a bare `~`.
fn draw_text(buf: &mut Buffer, g: &CellGeom, flag: &ModelFlag, forms: &[String]) {
    let room = g.cols.saturating_sub(2) as usize;
    let y = g.y + g.h - 1;
    let x0 = g.x + 2;
    let text = match fitting(forms, room) {
        Some(t) => t,
        None if flag.estimated && room > 0 => "~",
        None => return,
    };
    let style = Style::default()
        .fg(rgb(flag.colour))
        .add_modifier(Modifier::BOLD);
    let mut x = x0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0) as u16;
        if w == 0 {
            continue;
        }
        put(buf, x, y, ch, w, style);
        x += w;
    }
}

// ---- Legend and explain text -------------------------------------------------

/// The Grid legend's section on what the bus draws.
pub(crate) const GRID_LEGEND_TEXT: &[&str] = &[
    "What the signal bus puts on each chip's cell:",
    "  label  the chip's model, on the top row",
    "  phase  phase, progress, tokens/s on the bottom",
    "         rule; '~' marks an estimate",
    "  ┃ bar  fills purple from the bottom as a model",
    "         compiles or loads, up to its progress.",
    "         With no figure a band climbs, which is",
    "         decoration. Teal at ready, grey when down",
    "  pulse  a teal light up the PCIe spine ╋ when",
    "         requests complete (left bar on Wormhole)",
    "  * cell a hivemind spark or a kernel compile",
    "  flash  the portrait reverses after a reset",
];

/// The Grid explain text's paragraph on what the bus draws.
pub(crate) const GRID_EXPLAIN_TEXT: &[&str] = &[
    "",
    "Signal bus: a cell's top row names the chip's model,",
    "its bottom rule the phase, progress and tokens/s ('~'",
    "marks an estimate). The left bar fills purple as the",
    "model compiles or loads, as far as its progress; with",
    "no figure a band climbs, which is decoration. It lights",
    "teal at ready. Requests send a teal light up the PCIe",
    "spine, a spark or compile lights a core as *, and a",
    "reset flashes the cell.",
];

/// The Insights legend's section on what the bus draws.
pub(crate) const INSIGHTS_LEGEND_TEXT: &[&str] = &[
    "Signal bus, on each chip panel's bottom rule:",
    "  model, phase, progress and tokens/s; '~' marks",
    "  an estimate; purple as far as the progress",
    "  while the model compiles or loads",
];

/// The Insights explain text's paragraph on what the bus draws. It goes
/// near the top of the explain text, which is taller than most terminals,
/// and ends with a blank line.
pub(crate) const INSIGHTS_EXPLAIN_TEXT: &[&str] = &[
    "Signal bus: the rule under each chip panel names its",
    "model, phase, progress and tokens/s ('~' marks an",
    "estimate), and turns purple as far as the progress.",
    "",
];

/// Legend lines for `text`, with the overlay's left bar.
pub(crate) fn legend_lines(
    text: &'static [&'static str],
    bar: Color,
    bg: Color,
    dim: Color,
) -> Vec<Line<'static>> {
    text.iter()
        .map(|s| {
            Line::from(vec![
                Span::styled("║ ", Style::default().fg(bar).bg(bg)),
                Span::styled(*s, Style::default().fg(dim)),
            ])
        })
        .collect()
}

#[cfg(test)]
#[path = "grid_bus_tests.rs"]
mod tests;
