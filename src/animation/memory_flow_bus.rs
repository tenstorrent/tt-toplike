// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What the MemoryFlow mode draws from the signal bus: the DDR lanes take
//! each channel's state and temperature from its `dram:<chip>/<n>` entity, a
//! model loading streams weights along the DDR IN lane, completed requests
//! pulse both lanes, an ECC rise flashes its channel, a model row under the
//! DDR IN lane shows each chip's model with a bring-up gauge, a reset
//! flashes the chip's share of that row, and sparks and kernel compiles
//! twinkle in the chip's share of the core grid.
//!
//! # Two steps
//!
//! The shape is the arcade's (`arcade_bus`) and Die 3D's
//! (`die3d::overlay`):
//!
//! 1. [`MemoryFlowFx::from_world`] runs in `MemoryFlowVis::set_bus`, once
//!    per update. It reads the world through [`WorldRead`], compares it with
//!    what the view saw last ([`MemoryFlowBusState`]) and returns a
//!    [`MemoryFlowFx`]. All reading, matching and bookkeeping happens here.
//! 2. `MemoryFlowVis::render` (the frame path) reads the lane states from
//!    the `MemoryFlowFx` when it builds the DDR bars, and then calls
//!    [`draw`], which changes the rendered lines at a bus time with
//!    arithmetic on the `MemoryFlowFx` only. Neither reads the world.
//!
//! An empty world gives an empty `MemoryFlowFx`, and then the view draws
//! exactly what it drew before the bus. The arcade embeds a `MemoryFlowVis`
//! and never hands it a world, so the arcade's flow band is unchanged too.
//!
//! # What drives what
//!
//! * **Lane state.** The DDR bars are one segment per channel of the first
//!   device, as before. When `dram:<chip>/<n>` of that device's chip has
//!   readings, channel `n`'s segment takes its state from them, in the
//!   order the GDDR telemetry block is read: harvested or not enabled is
//!   idle, a failed training or BIST is an error, anything else is trained.
//!   A channel with no reading keeps the state from the SMBUS telemetry.
//! * **Lane heat.** The channel's hotter temperature, mapped from 40..90 C
//!   onto 0..1 as Die 3D maps it, tints both of its segments toward red.
//! * **The weight stream.** While a model on one of this view's chips is
//!   `Loading`, `▸` fills each DDR IN segment from its left edge as far as
//!   the reported progress. With no progress figure a short run of `▸`
//!   travels along each segment (decoration). The first chip in device
//!   order with a loading model sets it.
//! * **Request pulses.** A `RequestsCompleted` for a model on one of this
//!   view's chips brightens both lanes toward teal, by Die 3D's pulse
//!   strength for the count, fading over [`PULSE_NS`].
//! * **ECC flashes.** An ECC counter of `dram:<chip>/<n>` of the device in
//!   slot 0 (whose channels the lanes show) that rose since the last
//!   update, or an `EccTick` event for that chip, shows channel `n`'s
//!   segments reversed in red for [`FLASH_NS`]. Another chip's rise draws
//!   nothing here, since it would show on the wrong chip's channel.
//! * **The model row** (the blank line under DDR IN). Each device has an
//!   equal share of the width. A chip's model gets a gauge there (`▸` as far
//!   as the progress while compiling or loading, a travelling band with no
//!   figure, a teal rule fading over [`SETTLE_NS`] when it becomes ready, a
//!   grey one when it goes down) and its label right-aligned, in the phase
//!   colour, with `~` for estimates (`model_text_is_estimated`). The label
//!   shows the tokens per second. A new `reset_seq` shows the share
//!   reversed for [`FLASH_NS`].
//! * **Sparks and compiles.** A `Spark` naming a chip puts a `✦` in
//!   `spark_rgb` colours in the chip's share of the core grid for
//!   [`SPARK_NS`]; a `KernelCompiled` does the same in the load purple.
//!
//! Events already in the world at the first read draw nothing, and so do
//! events that arrived while the view was away (see `bus_intake`). An ECC
//! counter or `reset_seq` seen for the first time, or again after being
//! away, is recorded and draws nothing.
//!
//! # Provenance
//!
//! The lane states, the heat and the label's numbers are what the sources
//! measured, with `~` for estimates. The stream's and the gauge's lengths
//! are derived from the reported progress. The following are decoration:
//! the travelling run and band shown with no progress figure (they say the
//! model is busy and do not say how far), how strongly heat tints a lane,
//! the fade of the pulses, where a spark lands, and the durations of the
//! settle, fade and flash.

use crate::animation::arcade_bus::{
    gauge_of, left, restyle, slot_cols, splice_str, stepped_secs, tint, Gauge, Particle,
};
use crate::animation::bus_intake::{ecc_rose, number, EventIntake, PhaseBook};
use crate::animation::die3d::overlay::{
    flag_of, lerp, spark_rgb, strength, ModelFlag, Pulse, ALARM_RED, BAND_HALF, DOWN_GREY,
    FLASH_NS, LOAD_PURPLE, MAX_PULSES, MAX_SPARKS, PULSE_NS, SERVE_TEAL, SETTLE_NS, SPARK_NS,
    SWEEP_PERIOD_NS,
};
use crate::animation::starfield_bus::FieldChip;
use crate::bus::signals as sig;
use crate::ui::colors;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, Event, ModelPhase, WorldRead};
use unicode_width::UnicodeWidthStr;

const NS_PER_S: u64 = 1_000_000_000;
/// Columns before the first channel segment: `"DDR IN:  "` and
/// `"DDR OUT: "`.
pub const LANE_LABEL_COLS: usize = 9;
/// How far heat tints a lane toward red at most. Decoration.
const HEAT_WEIGHT: f32 = 0.5;
/// How far a full-strength pulse brightens a lane toward teal.
const PULSE_WEIGHT: f32 = 0.8;
/// The empty background the model row's rule fades from.
const ROW_DARK: [u8; 3] = [40, 40, 60];

/// One DDR channel's lane state from the bus.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LaneFx {
    pub channel: u8,
    /// The state in `MemoryFlowVis::channel_status`'s terms (0 idle, 2
    /// trained, 3 error). `None` with no reading: the SMBUS state stays.
    pub status: Option<u8>,
    /// The hotter temperature mapped from 40..90 C onto 0..1.
    pub heat: Option<f32>,
    /// When the channel's ECC flash started.
    pub flash_ns: Option<u64>,
}

impl LaneFx {
    fn is_empty(&self) -> bool {
        self.status.is_none() && self.heat.is_none() && self.flash_ns.is_none()
    }
}

/// Everything the bus puts on one chip's share of the view.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlowChipFx {
    pub slot: usize,
    pub flag: Option<ModelFlag>,
    /// The flag's text forms, longest first, worked out here so the frame
    /// path builds none.
    pub label_texts: Vec<String>,
    pub gauge: Option<Gauge>,
    pub sparks: Vec<Particle>,
    /// When the chip's reset flash started.
    pub flash_ns: Option<u64>,
}

impl FlowChipFx {
    fn is_empty(&self) -> bool {
        self.flag.is_none()
            && self.gauge.is_none()
            && self.sparks.is_empty()
            && self.flash_ns.is_none()
    }
}

/// What the bus puts on MemoryFlow. See the module doc.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemoryFlowFx {
    /// How many slots the width is cut into: the device count.
    pub slots: usize,
    /// One entry per chip that has something, in slot order.
    pub chips: Vec<FlowChipFx>,
    /// One entry per channel that has something.
    pub lanes: Vec<LaneFx>,
    /// The weight stream along DDR IN: `Progress` or `Sweep` of the first
    /// loading model.
    pub stream: Option<Gauge>,
    pub pulses: Vec<Pulse>,
}

/// What the view remembers between two [`MemoryFlowFx::from_world`] calls.
/// Start with the default; set a new default when the view stops reading a
/// world, so the next world is a first sight.
#[derive(Debug, Clone, Default)]
pub struct MemoryFlowBusState {
    intake: EventIntake,
    phases: PhaseBook,
    pulses: Vec<Pulse>,
    /// By chip key.
    sparks: HashMap<EntityKey, Vec<Particle>>,
    /// The last `reset_seq` seen, by chip key.
    resets: HashMap<EntityKey, Option<u64>>,
    /// When each chip's reset flash started, by chip key.
    flashes: HashMap<EntityKey, u64>,
    /// The last ECC total, by DRAM channel key.
    ecc: HashMap<EntityKey, u64>,
    /// When each channel's ECC flash started, by channel number of the
    /// device in slot 0 (the device whose channels the lanes show).
    ecc_flashes: HashMap<u8, u64>,
}

impl MemoryFlowFx {
    /// Whether nothing is drawn from the bus.
    pub fn is_empty(&self) -> bool {
        self.chips.is_empty()
            && self.lanes.is_empty()
            && self.stream.is_none()
            && self.pulses.is_empty()
    }

    /// The lane state of channel `ch`, if the bus has one.
    pub fn lane_status(&self, ch: usize) -> Option<u8> {
        self.lanes
            .iter()
            .find(|l| l.channel as usize == ch)
            .and_then(|l| l.status)
    }

    /// The effects of `chips` out of `slots` device slots, from `world` at
    /// bus time `now`, updating `prev`. The lanes are those of the device in
    /// slot 0, the device whose channels the bars show. See the module doc.
    pub fn from_world(
        world: &dyn WorldRead,
        chips: &[FieldChip],
        slots: usize,
        now: u64,
        prev: &mut MemoryFlowBusState,
    ) -> MemoryFlowFx {
        let drawn = |c: &EntityKey| chips.iter().any(|f| f.key == *c);
        let ours = |m: &tt_toplike_bus::ModelView| m.chips.iter().any(&drawn);
        let models = world.models();
        let intake = prev.intake.take(world, now);
        // The lanes are the channels of the device in slot 0, so only that
        // chip's ECC rises flash them.
        let lane_chip = chips.iter().find(|c| c.slot == 0).map(|c| &c.key);

        for (q, e) in &intake.events {
            match e {
                Event::RequestsCompleted { model, n } if *n > 0 => {
                    if models.iter().any(|m| m.key == *model && ours(m)) {
                        add_pulse(&mut prev.pulses, now, *n);
                    }
                }
                Event::Spark {
                    chip: Some(c),
                    kind,
                } if drawn(c) => add_spark(prev, c, now, *q, spark_rgb(kind)),
                Event::KernelCompiled { chip: Some(c), .. } if drawn(c) => {
                    add_spark(prev, c, now, *q, LOAD_PURPLE)
                }
                Event::EccTick { chip, channel } if lane_chip == Some(chip) => {
                    prev.ecc_flashes.insert(*channel, now);
                }
                // Model events are announcements: the phase is read from the
                // model below. Resets are read from `reset_seq`.
                _ => {}
            }
        }

        for c in chips {
            let latest = world.reset_seq(&c.key);
            let before = prev.resets.insert(c.key.clone(), latest);
            if before.is_some_and(|b| latest.is_some() && latest != b) && intake.live {
                prev.flashes.insert(c.key.clone(), now);
            }
            // ECC rises flash the channel's lane, which shows slot 0's
            // channels: another chip's rise would show on the wrong chip.
            if c.slot != 0 {
                continue;
            }
            for ch in 0..c.channels {
                let dk = EntityKey::dram_channel(&c.key, ch);
                if ecc_rose(world, &dk, &mut prev.ecc) && intake.live {
                    prev.ecc_flashes.insert(ch, now);
                }
            }
        }
        prev.resets.retain(|k, _| drawn(k));

        prev.phases.update(&models, now);

        let mut out = MemoryFlowFx {
            slots,
            ..MemoryFlowFx::default()
        };
        for c in chips {
            let mut co = FlowChipFx {
                slot: c.slot,
                ..FlowChipFx::default()
            };
            if let Some(m) = world.model_on_chip(&c.key).filter(|m| !m.chips.is_empty()) {
                let flag = flag_of(world, &m);
                co.label_texts = flag.texts();
                co.flag = Some(flag);
                co.gauge = gauge_of(&m, prev.phases.get(&m.key), now);
                if m.phase == ModelPhase::Loading && out.stream.is_none() {
                    out.stream = co.gauge;
                }
            }
            if let Some(p) = prev.sparks.get_mut(&c.key) {
                p.retain(|p| now.saturating_sub(p.start_ns) < SPARK_NS);
                co.sparks = p.clone();
            }
            co.flash_ns = prev
                .flashes
                .get(&c.key)
                .copied()
                .filter(|t| now.saturating_sub(*t) < FLASH_NS);
            if !co.is_empty() {
                out.chips.push(co);
            }
        }

        // The lanes: the channels of the device in slot 0.
        prev.ecc_flashes
            .retain(|_, t| now.saturating_sub(*t) < FLASH_NS);
        if let Some(first) = chips.iter().find(|c| c.slot == 0) {
            for ch in 0..first.channels {
                let lane = lane_of(world, &first.key, ch, prev.ecc_flashes.get(&ch).copied());
                if !lane.is_empty() {
                    out.lanes.push(lane);
                }
            }
        }

        prev.pulses
            .retain(|p| now.saturating_sub(p.start_ns) < PULSE_NS);
        out.pulses = prev.pulses.clone();
        let owner_drawn = |k: &EntityKey| k.chip_owner().is_some_and(|c| drawn(&c));
        prev.sparks.retain(|k, p| drawn(k) && !p.is_empty());
        prev.flashes
            .retain(|k, t| drawn(k) && now.saturating_sub(*t) < FLASH_NS);
        prev.ecc.retain(|k, _| owner_drawn(k));
        out
    }
}

/// The lane of channel `ch` of `chip`, with the flash that started at
/// `flash_ns`.
fn lane_of(world: &dyn WorldRead, chip: &EntityKey, ch: u8, flash_ns: Option<u64>) -> LaneFx {
    let dk = EntityKey::dram_channel(chip, ch);
    let num = |s: &str| world.reading(&dk, s).and_then(|r| number(&r.value));
    let harvested = num(sig::DRAM_HARVESTED);
    let enabled = num(sig::DRAM_ENABLED);
    let training = num(sig::DRAM_TRAINING_PASS);
    let bist = num(sig::DRAM_BIST_PASS);
    let temp = [sig::DRAM_TEMP_TOP_C, sig::DRAM_TEMP_BOTTOM_C]
        .iter()
        .filter_map(|s| num(s))
        .fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t))));
    // The order of `MemoryFlowVis::channel_status_from_gddr_telemetry`.
    // Only a training or BIST result says a channel trained. A temperature
    // (or a harvested or enabled flag) alone gives no state, so the SMBUS
    // state of the channel, an error for one, stays.
    let status = if harvested == Some(1.0) || enabled == Some(0.0) {
        Some(0)
    } else if training == Some(0.0) || bist == Some(0.0) {
        Some(3)
    } else if training.is_some() || bist.is_some() {
        Some(2)
    } else {
        None
    };
    LaneFx {
        channel: ch,
        status,
        heat: temp.map(|c| ((c as f32 - 40.0) / 50.0).clamp(0.0, 1.0)),
        flash_ns,
    }
}

/// Add a pulse for `n` requests at `now`, keeping at most [`MAX_PULSES`]:
/// past that the newest takes this one's strength on top of its own and
/// starts again, as Die 3D merges pulses.
fn add_pulse(list: &mut Vec<Pulse>, now: u64, n: u32) {
    list.retain(|p| now.saturating_sub(p.start_ns) < PULSE_NS);
    if list.len() < MAX_PULSES {
        list.push(Pulse {
            start_ns: now,
            strength: strength(n),
        });
    } else if let Some(last) = list.last_mut() {
        last.strength = (last.strength + strength(n)).min(1.4);
        last.start_ns = now;
    }
}

/// Add a spark on `chip` for the event with sequence number `seq`, keeping at
/// most [`MAX_SPARKS`] per chip.
fn add_spark(prev: &mut MemoryFlowBusState, chip: &EntityKey, now: u64, seq: u64, rgb: [u8; 3]) {
    let list = prev.sparks.entry(chip.clone()).or_default();
    list.retain(|p| now.saturating_sub(p.start_ns) < SPARK_NS);
    if list.len() >= MAX_SPARKS {
        list.remove(0);
    }
    list.push(Particle {
        start_ns: now,
        seq,
        rgb,
    });
}

// ---- The frame path ---------------------------------------------------------

/// Where the bus effects go on MemoryFlow's lines.
#[derive(Debug, Clone, Copy)]
pub struct FlowStage {
    pub width: usize,
    /// Channels the bars show, and the width of each channel's segment.
    pub channels: usize,
    pub channel_width: usize,
}

impl FlowStage {
    /// The columns of channel `ch`'s segment.
    fn segment(&self, ch: usize) -> std::ops::Range<usize> {
        let a = LANE_LABEL_COLS + ch * self.channel_width;
        a..a + self.channel_width
    }
    /// The columns of every segment.
    fn all_segments(&self) -> std::ops::Range<usize> {
        LANE_LABEL_COLS..LANE_LABEL_COLS + self.channels * self.channel_width
    }
}

fn colour(rgb: [u8; 3]) -> Color {
    colors::rgb(rgb[0], rgb[1], rgb[2])
}

/// Draw `fx` at bus time `now` onto MemoryFlow's rendered `lines` (DDR IN
/// first, the model row second, the core grid, a blank line, DDR OUT and the
/// stats line last). Arithmetic on `fx` only. An empty `fx` changes nothing.
pub fn draw(fx: &MemoryFlowFx, now: u64, stage: &FlowStage, lines: &mut Vec<Line<'static>>) {
    if fx.is_empty() || lines.len() < 5 {
        return;
    }
    let (lane_in, lane_out) = (0, lines.len() - 2);
    let grid = 2..lines.len() - 3;

    // Lane heat, then the stream, the pulses and the ECC flashes on top.
    for l in &fx.lanes {
        if let Some(h) = l.heat {
            for row in [lane_in, lane_out] {
                tint(
                    lines,
                    row,
                    stage.segment(l.channel as usize),
                    ALARM_RED,
                    h * HEAT_WEIGHT,
                );
            }
        }
    }
    if let Some(g) = fx.stream {
        for ch in 0..stage.channels {
            draw_run(g, now, stage.segment(ch), lane_in, LOAD_PURPLE, lines);
        }
    }
    for p in &fx.pulses {
        let w = left(p.start_ns, now, PULSE_NS) * p.strength.min(1.0);
        if w > 0.0 {
            for row in [lane_in, lane_out] {
                tint(
                    lines,
                    row,
                    stage.all_segments(),
                    SERVE_TEAL,
                    w * PULSE_WEIGHT,
                );
                restyle(lines, row, stage.all_segments(), |s| {
                    s.add_modifier(Modifier::BOLD)
                });
            }
        }
    }
    for l in &fx.lanes {
        if l.flash_ns.is_some_and(|t| left(t, now, FLASH_NS) > 0.0) {
            for row in [lane_in, lane_out] {
                restyle(lines, row, stage.segment(l.channel as usize), |s| {
                    s.fg(colour(ALARM_RED)).add_modifier(Modifier::REVERSED)
                });
            }
        }
    }

    // The model row: padded to the width so its cells can be styled.
    if !fx.chips.is_empty() {
        let w: usize = lines[1]
            .spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        if w < stage.width {
            lines[1].spans.push(Span::raw(" ".repeat(stage.width - w)));
        }
    }
    for co in &fx.chips {
        let cols = slot_cols(co.slot, fx.slots, stage.width);
        if cols.is_empty() {
            continue;
        }
        if let Some(g) = co.gauge {
            draw_rule(g, now, cols.clone(), lines);
        }
        if let Some(flag) = &co.flag {
            draw_label(flag, &co.label_texts, cols.clone(), lines);
        }
        if co.flash_ns.is_some_and(|t| left(t, now, FLASH_NS) > 0.0) {
            restyle(lines, 1, cols.clone(), |s| {
                s.add_modifier(Modifier::REVERSED)
            });
        }
        for p in &co.sparks {
            draw_spark(p, now, cols.clone(), grid.clone(), lines);
        }
    }
}

/// `▸` over `cols` of `row` for gauge `g`: as far as the progress, or a short
/// travelling run with no figure (decoration). Settle and fade draw nothing
/// here.
fn draw_run(
    g: Gauge,
    now: u64,
    cols: std::ops::Range<usize>,
    row: usize,
    rgb: [u8; 3],
    lines: &mut [Line<'static>],
) {
    let len = cols.len();
    if len == 0 {
        return;
    }
    let (a, b) = match g {
        Gauge::Progress(p) => (0, (p * len as f32).round() as usize),
        Gauge::Sweep { since_ns } => band(since_ns, now, len),
        Gauge::Settle { .. } | Gauge::Fade { .. } => return,
    };
    if b > a {
        let text: String = "▸".repeat(b.min(len) - a);
        splice_str(
            lines,
            row,
            cols.start + a,
            &text,
            Style::default().fg(colour(rgb)),
        );
    }
}

/// The cells `a..b` of a band travelling across `len` cells, the sweep Die
/// 3D and the arcade draw. Decoration.
fn band(since_ns: u64, now: u64, len: usize) -> (usize, usize) {
    let period = SWEEP_PERIOD_NS as f32 / NS_PER_S as f32;
    let phase = (stepped_secs(since_ns, now) / period).fract();
    let centre = phase * (1.0 + 2.0 * BAND_HALF) - BAND_HALF;
    let lo = ((centre - BAND_HALF).max(0.0) * len as f32).floor() as usize;
    let hi = ((centre + BAND_HALF).clamp(0.0, 1.0) * len as f32).ceil() as usize;
    (lo.min(len), hi.min(len))
}

/// The chip's gauge on the model row: `▸` as far as the progress or a
/// travelling band, a teal rule fading after `Ready`, a grey one after
/// `Down`.
fn draw_rule(g: Gauge, now: u64, cols: std::ops::Range<usize>, lines: &mut [Line<'static>]) {
    match g {
        Gauge::Progress(_) | Gauge::Sweep { .. } => draw_run(g, now, cols, 1, LOAD_PURPLE, lines),
        Gauge::Settle { since_ns } | Gauge::Fade { since_ns } => {
            let w = left(since_ns, now, SETTLE_NS);
            if w <= 0.0 {
                return;
            }
            let to = if matches!(g, Gauge::Settle { .. }) {
                SERVE_TEAL
            } else {
                DOWN_GREY
            };
            let text: String = "─".repeat(cols.len());
            splice_str(
                lines,
                1,
                cols.start,
                &text,
                Style::default().fg(colour(lerp(ROW_DARK, to, w))),
            );
        }
    }
}

/// The model's label, right-aligned in the slot with one column to spare:
/// the longest of `texts` that fits. When none fits, an estimate still gets a
/// bare `~`.
fn draw_label(
    flag: &ModelFlag,
    texts: &[String],
    cols: std::ops::Range<usize>,
    lines: &mut [Line<'static>],
) {
    let end = cols.end.saturating_sub(1);
    let style = Style::default()
        .fg(colour(flag.colour))
        .add_modifier(Modifier::BOLD);
    match texts.iter().find(|t| {
        let w = UnicodeWidthStr::width(t.as_str());
        end >= w && end - w > cols.start
    }) {
        Some(t) => {
            let col = end - UnicodeWidthStr::width(t.as_str());
            splice_str(lines, 1, col, t, style);
        }
        None if flag.estimated && end > cols.start + 1 => {
            crate::animation::ArcadeVisualization::splice_char(lines, 1, end - 1, '~', style);
        }
        None => {}
    }
}

/// A spark in the core grid rows `rows` of the slot `cols`. Where it lands
/// is decoration.
fn draw_spark(
    p: &Particle,
    now: u64,
    cols: std::ops::Range<usize>,
    rows: std::ops::Range<usize>,
    lines: &mut [Line<'static>],
) {
    let w = left(p.start_ns, now, SPARK_NS);
    if w <= 0.0 || rows.is_empty() || cols.is_empty() {
        return;
    }
    let col = cols.start + (p.seq.wrapping_mul(7919) % cols.len() as u64) as usize;
    let row = rows.start + (p.seq.wrapping_mul(31) % rows.len() as u64) as usize;
    let glyph = if w > 0.5 { '✦' } else { '·' };
    let style = Style::default()
        .fg(colour(p.rgb))
        .add_modifier(Modifier::BOLD);
    crate::animation::ArcadeVisualization::splice_char(lines, row, col, glyph, style);
}

// ---- Legend and explain text --------------------------------------------------

/// The MemoryFlow legend's section on what the bus draws.
pub const LEGEND_TEXT: &[&str] = &[
    "Signal bus:",
    "  lanes    DDR segments take their channel's state and",
    "           temperature from the bus (hotter is redder)",
    "  ▸ stream weights flow in along DDR IN as a model",
    "           loads, to its progress (with none, a run",
    "           travels: decoration)",
    "  teal     both lanes brighten when requests complete",
    "  red      a channel flashes when its ECC count rises",
    "  model    row under DDR IN: model, phase, tokens/s and",
    "           a ▸ gauge; '~' marks an estimate; reversed",
    "           after a reset",
    "  ✦        a hivemind spark or a kernel compile",
];

/// The MemoryFlow explain text's paragraph on what the bus draws.
pub const EXPLAIN_TEXT: &[&str] = &[
    "",
    "Signal bus: DDR segments take each channel's state and",
    "temperature from the bus. While a model loads, ▸ fills",
    "DDR IN to its progress (with no figure, a travelling",
    "run: decoration). Requests brighten both lanes, and a",
    "rising ECC count flashes its channel red. The row under",
    "DDR IN gives each chip's model, phase and tokens/s ('~'",
    "marks an estimate), reversed after a reset. Sparks and",
    "compiles show as ✦.",
];

/// The legend lines for [`LEGEND_TEXT`], with the overlay's left bar.
pub fn legend_lines(bar: Color, bg: Color, dim: Color) -> Vec<Line<'static>> {
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
#[path = "memory_flow_bus_tests.rs"]
mod tests;
