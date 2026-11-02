// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What the Starfield mode draws from the signal bus: a bright cluster of
//! stars for a model coming up on a chip, a comet for each batch of
//! completed requests, faster twinkle and orbits from tokens per second, a
//! flare on a DDR planet whose ECC count rose, a twinkle for a spark or a
//! kernel compile, a flash of a chip's stars after a reset, and the model's
//! label at the top of the chip's share of the field.
//!
//! # Two steps
//!
//! The shape is the arcade's (`arcade_bus`) and Die 3D's
//! (`die3d::overlay`):
//!
//! 1. [`StarfieldFx::from_world`] runs in `HardwareStarfield::set_bus`, once
//!    per update, before `update_from_telemetry`. It reads the world through
//!    [`WorldRead`], compares it with what the field saw last
//!    ([`StarfieldBusState`]) and returns a [`StarfieldFx`]. All reading,
//!    matching and bookkeeping happens here.
//! 2. [`draw`] runs in `HardwareStarfield::render` (the frame path). It
//!    changes cells of the render canvas at a bus time with arithmetic only.
//!    It never reads the world and allocates nothing.
//!
//! An empty world gives an empty `StarfieldFx`. `render` skips [`draw`] for
//! an empty one and `update_from_telemetry` adds nothing to the twinkle or
//! orbit, so the field draws exactly what it drew before the bus. The arcade
//! embeds a `HardwareStarfield` and never hands it a world, so the arcade's
//! starfield band is unchanged too.
//!
//! # Where things go
//!
//! Each device has an equal share of the width, in device order, as the
//! stars are laid out: its *slot*. A chip's stars and DDR planets are the
//! ones `initialize_from_devices` placed for it.
//!
//! * **The cluster.** While a model compiles or loads, the chip's stars
//!   within a radius of the grid's centre turn to `●` in the load purple.
//!   The radius is the reported progress: 0 lights none, 1 lights the whole
//!   grid. The star's `depth` already encodes the distance (0 at
//!   the centre, 1 at a corner). With no progress figure, a ring of stars
//!   sweeps out from the centre (decoration). A change to `Ready` tints all
//!   the chip's stars teal and fades over [`SETTLE_NS`]; a change to `Down`
//!   does the same in grey.
//! * **The label**: the model's name, phase, progress and tokens/s on the
//!   top row of the slot, in the phase colour, with `~` in front when the
//!   readings are estimates (`model_text_is_estimated`). The longest form
//!   that fits is drawn. When none fits, an estimate still gets a bare `~`.
//! * **Comets.** A `RequestsCompleted` for a model on one of this field's
//!   chips sends a teal comet from the left edge of that chip's slot across
//!   the field, wrapping at the right edge. Its tail has one cell per
//!   request, up to [`MAX_HIT_GLYPHS`].
//! * **Twinkle and drift.** The smoothed tokens per second of the ready
//!   model on a chip speed up the twinkle of the chip's stars and the
//!   orbits of its planets. With no fresh reading they move as before.
//! * **ECC flares.** An ECC counter of `dram:<chip>/<n>` that rose since the
//!   last update, or an `EccTick` event, draws a red flare on the chip's DDR
//!   planet for channel `n` for [`FLASH_NS`]. A channel with no planet on
//!   screen flares at the centre of the chip's slot.
//! * **Sparks and compiles.** A `Spark` naming a chip makes one of its stars
//!   twinkle `✦` in `spark_rgb` colours for [`SPARK_NS`]; a `KernelCompiled`
//!   does the same in the load purple. Events with no chip draw nothing.
//! * **Resets.** A new `reset_seq` on a chip turns its stars white and fades
//!   them back over [`FLASH_NS`].
//!
//! The fleet galaxy view (32 or more devices) has no per-chip stars and
//! draws no bus effects.
//!
//! # History
//!
//! Events already in the world at the first read draw nothing, and so do
//! events that arrived while the field was away (see `bus_intake`). An ECC
//! counter or a `reset_seq` seen for the first time, or first seen again
//! after being away, is recorded and draws nothing.
//!
//! # Provenance
//!
//! The label's numbers are what the source measured, with `~` for estimates.
//! The cluster's radius is derived from the reported progress, and the comet
//! tail's length is the request count. The following are decoration: the
//! sweep shown with no progress figure (it says the model is busy and does
//! not say how far), the comet's row, speed and fade, which star twinkles
//! for a spark, the shape of the flares, the durations of the settle, fade
//! and flash, and how much a given rate speeds the twinkle and the orbits
//! (the rate itself is measured).

use crate::animation::arcade_bus::{
    gauge_of, left, slot_cols, stepped_secs, Gauge, Particle, MAX_HIT_GLYPHS,
};
use crate::animation::bus_intake::{
    ecc_rose, fresh_tps, one_column, smooth_tps, EventIntake, PhaseBook,
};
use crate::animation::die3d::overlay::{
    flag_of, lerp, spark_rgb, ModelFlag, ALARM_RED, BAND_HALF, DOWN_GREY, FLASH_NS, LOAD_PURPLE,
    MAX_PULSES, MAX_SPARKS, SERVE_TEAL, SETTLE_NS, SPARK_NS, SWEEP_PERIOD_NS,
};
use crate::animation::{MemoryPlanet, Star};
use crate::ui::colors;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, Event, WorldRead};

const NS_PER_S: u64 = 1_000_000_000;
/// How long a comet takes to cross the field. Decoration.
pub const COMET_NS: u64 = 2 * NS_PER_S;
/// How much the twinkle speeds up at most from tokens per second, in phase
/// per update (the twinkle runs at 0.1 to 0.55 without it). Decoration.
const TWINKLE_GAIN: f32 = 0.3;
/// How much faster the planets orbit at most from tokens per second, as a
/// multiple of their speed. Decoration.
const DRIFT_GAIN: f32 = 1.0;
/// Tokens per second that give half of the gain. The arcade's heartbeat
/// uses the same rate to halve its period.
const TPS_HALF: f64 = 50.0;
/// The white a reset flash starts from.
const FLASH_WHITE: [u8; 3] = [255, 255, 255];

/// One drawn chip: its slot (position among the devices), its own device
/// index (the key its stars carry), its chip key and its DRAM channel count.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldChip {
    pub slot: usize,
    pub index: usize,
    pub key: EntityKey,
    pub channels: u8,
}

impl FieldChip {
    /// The drawn chips of `devices`. The bus names a chip by its PCI
    /// address, so a device with none is left out.
    pub fn of(devices: &[crate::models::Device]) -> Vec<FieldChip> {
        devices
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.bus_id.is_empty())
            .map(|(slot, d)| FieldChip {
                slot,
                index: d.index,
                key: EntityKey::chip(&d.bus_id),
                channels: d.memory_channels().min(u8::MAX as usize) as u8,
            })
            .collect()
    }
}

/// One comet: a batch of completed requests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comet {
    pub start_ns: u64,
    /// Cells in the tail: the requests completed, 1 to [`MAX_HIT_GLYPHS`].
    pub tail: u32,
    /// The event's sequence number, which picks the comet's row
    /// (decoration).
    pub seq: u64,
    /// The column it starts from: the left edge of the model's first drawn
    /// chip's slot, as a slot number.
    pub slot: usize,
}

/// One ECC flare on a chip's DDR planet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EccFlare {
    pub channel: u8,
    pub start_ns: u64,
}

/// Everything the bus puts on one chip's stars.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StarChipFx {
    pub slot: usize,
    /// The device index its stars and planets carry (`Star::device_idx`).
    pub index: usize,
    pub flag: Option<ModelFlag>,
    /// The flag's text forms, longest first, one column per character,
    /// worked out here so the frame path builds no strings.
    pub label_texts: Vec<String>,
    pub cluster: Option<Gauge>,
    pub sparks: Vec<Particle>,
    pub flash_ns: Option<u64>,
    pub ecc: Vec<EccFlare>,
    /// Smoothed tokens per second of the ready model on this chip. `None`
    /// with no fresh reading.
    pub tokens_per_s: Option<f64>,
}

impl StarChipFx {
    fn is_empty(&self) -> bool {
        self.flag.is_none()
            && self.cluster.is_none()
            && self.sparks.is_empty()
            && self.flash_ns.is_none()
            && self.ecc.is_empty()
            && self.tokens_per_s.is_none()
    }
}

/// What the bus puts on the Starfield. See the module doc.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StarfieldFx {
    /// How many slots the width is cut into: the device count.
    pub slots: usize,
    /// One entry per chip that has something, in slot order.
    pub chips: Vec<StarChipFx>,
    pub comets: Vec<Comet>,
}

/// What the field remembers between two [`StarfieldFx::from_world`] calls.
/// Start with the default; set a new default when the field stops reading a
/// world, so the next world is a first sight.
#[derive(Debug, Clone, Default)]
pub struct StarfieldBusState {
    intake: EventIntake,
    phases: PhaseBook,
    comets: Vec<Comet>,
    /// By chip key.
    sparks: HashMap<EntityKey, Vec<Particle>>,
    /// The last `reset_seq` seen, by chip key.
    resets: HashMap<EntityKey, Option<u64>>,
    /// When each chip's reset flash started, by chip key.
    flashes: HashMap<EntityKey, u64>,
    /// The last ECC total, by DRAM channel key.
    ecc: HashMap<EntityKey, u64>,
    /// When each DRAM channel's flare started, by DRAM channel key.
    ecc_flares: HashMap<EntityKey, u64>,
    /// Smoothed tokens per second, by chip key.
    tps: HashMap<EntityKey, f64>,
}

impl StarfieldFx {
    /// Whether nothing is drawn from the bus.
    pub fn is_empty(&self) -> bool {
        self.chips.is_empty() && self.comets.is_empty()
    }

    /// The effects of `chips` out of `slots` device slots, from `world` at
    /// bus time `now`, updating `prev`. See the module doc.
    pub fn from_world(
        world: &dyn WorldRead,
        chips: &[FieldChip],
        slots: usize,
        now: u64,
        prev: &mut StarfieldBusState,
    ) -> StarfieldFx {
        let slot_of = |c: &EntityKey| chips.iter().find(|f| f.key == *c).map(|f| f.slot);
        let drawn = |c: &EntityKey| slot_of(c).is_some();
        let models = world.models();
        let intake = prev.intake.take(world, now);

        for (q, e) in &intake.events {
            match e {
                Event::RequestsCompleted { model, n } if *n > 0 => {
                    // The comet leaves from the model's first drawn chip.
                    let slot = models
                        .iter()
                        .find(|m| m.key == *model)
                        .and_then(|m| m.chips.iter().find_map(slot_of));
                    if let Some(slot) = slot {
                        add_comet(&mut prev.comets, now, *n, *q, slot);
                    }
                }
                Event::Spark {
                    chip: Some(c),
                    kind,
                } if drawn(c) => add_spark(prev, c, now, *q, spark_rgb(kind)),
                Event::KernelCompiled { chip: Some(c), .. } if drawn(c) => {
                    add_spark(prev, c, now, *q, LOAD_PURPLE)
                }
                Event::EccTick { chip, channel } if drawn(chip) => {
                    prev.ecc_flares
                        .insert(EntityKey::dram_channel(chip, *channel), now);
                }
                // Model events are announcements: the phase is read from the
                // model below. Resets are read from `reset_seq`.
                _ => {}
            }
        }

        // Resets: a `reset_seq` that moved since the last update flashes the
        // chip's stars.
        for c in chips {
            let latest = world.reset_seq(&c.key);
            let before = prev.resets.insert(c.key.clone(), latest);
            if before.is_some_and(|b| latest.is_some() && latest != b) && intake.live {
                prev.flashes.insert(c.key.clone(), now);
            }
        }
        prev.resets.retain(|k, _| drawn(k));

        prev.phases.update(&models, now);

        let mut out = StarfieldFx {
            slots,
            ..StarfieldFx::default()
        };
        for c in chips {
            let mut co = StarChipFx {
                slot: c.slot,
                index: c.index,
                ..StarChipFx::default()
            };
            let model = world.model_on_chip(&c.key).filter(|m| !m.chips.is_empty());
            if let Some(m) = &model {
                let flag = flag_of(world, m);
                co.label_texts = flag.texts().iter().map(|t| one_column(t)).collect();
                co.flag = Some(flag);
                co.cluster = gauge_of(m, prev.phases.get(&m.key), now);
            }
            // Tokens per second of the ready model on this chip, smoothed.
            let raw = model.as_ref().and_then(|m| fresh_tps(world, m, now));
            let smoothed = smooth_tps(raw, prev.tps.get(&c.key).copied(), intake.gap);
            match smoothed {
                Some(t) => prev.tps.insert(c.key.clone(), t),
                None => prev.tps.remove(&c.key),
            };
            co.tokens_per_s = smoothed;

            // ECC: a total that rose flares the channel's planet.
            for ch in 0..c.channels {
                let dk = EntityKey::dram_channel(&c.key, ch);
                if ecc_rose(world, &dk, &mut prev.ecc) && intake.live {
                    prev.ecc_flares.insert(dk.clone(), now);
                }
                if let Some(t) = prev.ecc_flares.get(&dk) {
                    if now.saturating_sub(*t) < FLASH_NS {
                        co.ecc.push(EccFlare {
                            channel: ch,
                            start_ns: *t,
                        });
                    }
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
        prev.comets
            .retain(|h| now.saturating_sub(h.start_ns) < COMET_NS);
        out.comets = prev.comets.clone();

        // Nothing is kept for a chip no longer drawn, or an effect that ended.
        let owner_drawn = |k: &EntityKey| k.chip_owner().is_some_and(|c| drawn(&c));
        prev.sparks.retain(|k, p| drawn(k) && !p.is_empty());
        prev.flashes
            .retain(|k, t| drawn(k) && now.saturating_sub(*t) < FLASH_NS);
        prev.ecc.retain(|k, _| owner_drawn(k));
        prev.ecc_flares
            .retain(|k, t| owner_drawn(k) && now.saturating_sub(*t) < FLASH_NS);
        prev.tps.retain(|k, _| drawn(k));
        out
    }

    /// The smoothed tokens per second on the chip whose stars carry device
    /// index `index`, if it has a fresh reading.
    pub fn tokens_per_s(&self, index: usize) -> Option<f64> {
        self.chips
            .iter()
            .find(|c| c.index == index)
            .and_then(|c| c.tokens_per_s)
    }
}

/// How much faster a star twinkles, in phase per update, at `tps` tokens per
/// second: 0 at none, rising toward [`TWINKLE_GAIN`].
pub fn twinkle_boost(tps: f64) -> f32 {
    (TWINKLE_GAIN as f64 * tps / (tps + TPS_HALF)) as f32
}

/// The factor a planet's orbit speed is multiplied by at `tps` tokens per
/// second: 1 at none, rising toward `1 +` [`DRIFT_GAIN`].
pub fn drift_factor(tps: f64) -> f32 {
    1.0 + (DRIFT_GAIN as f64 * tps / (tps + TPS_HALF)) as f32
}

/// Add a comet for `n` requests at `now`, keeping at most [`MAX_PULSES`]:
/// past that the newest takes this one's tail on top of its own (up to
/// [`MAX_HIT_GLYPHS`]) and starts again, as Die 3D merges pulses.
fn add_comet(list: &mut Vec<Comet>, now: u64, n: u32, seq: u64, slot: usize) {
    list.retain(|h| now.saturating_sub(h.start_ns) < COMET_NS);
    let tail = n.clamp(1, MAX_HIT_GLYPHS);
    if list.len() < MAX_PULSES {
        list.push(Comet {
            start_ns: now,
            tail,
            seq,
            slot,
        });
    } else if let Some(last) = list.last_mut() {
        last.tail = (last.tail + tail).min(MAX_HIT_GLYPHS);
        last.start_ns = now;
    }
}

/// Add a spark on `chip` for the event with sequence number `seq`, keeping at
/// most [`MAX_SPARKS`] per chip.
fn add_spark(prev: &mut StarfieldBusState, chip: &EntityKey, now: u64, seq: u64, rgb: [u8; 3]) {
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

/// A canvas cell's colour as RGB, or `fallback` for a colour that is not
/// RGB (a terminal without true colour gets indexed colours).
fn rgb_of(c: Color, fallback: [u8; 3]) -> [u8; 3] {
    match c {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => fallback,
    }
}

fn colour(rgb: [u8; 3]) -> Color {
    colors::rgb(rgb[0], rgb[1], rgb[2])
}

/// A star's distance from its grid's centre, 0 at the centre and 1 at a
/// corner, from its `depth` (`initialize_from_devices` sets depth to
/// `1 - 0.6 * (dx + dy) / (cols + rows)`, so a corner has 0.7).
fn star_distance(s: &Star) -> f32 {
    ((1.0 - s.depth) / 0.3).clamp(0.0, 1.0)
}

/// Set one cell, if it is on the canvas.
fn put(canvas: &mut [Vec<(char, Color)>], x: i64, y: i64, cell: (char, Color)) {
    if x < 0 || y < 0 {
        return;
    }
    if let Some(c) = canvas
        .get_mut(y as usize)
        .and_then(|row| row.get_mut(x as usize))
    {
        *c = cell;
    }
}

/// Draw `fx` at bus time `now` onto the render canvas, whose stars and
/// planets are `stars` and `planets`. Arithmetic on `fx` only.
pub fn draw(
    fx: &StarfieldFx,
    now: u64,
    stars: &[Star],
    planets: &[MemoryPlanet],
    canvas: &mut [Vec<(char, Color)>],
) {
    let height = canvas.len();
    let width = canvas.first().map_or(0, Vec::len);
    if width == 0 || height == 0 {
        return;
    }
    for co in &fx.chips {
        let mine = || stars.iter().filter(|s| s.device_idx == co.index);
        draw_cluster(co, now, mine(), canvas);
        if let Some(t) = co.flash_ns {
            let w = left(t, now, FLASH_NS);
            if w > 0.0 {
                for s in mine() {
                    if let Some(cell) = canvas.get_mut(s.y).and_then(|r| r.get_mut(s.x)) {
                        cell.1 = colour(lerp(rgb_of(cell.1, FLASH_WHITE), FLASH_WHITE, w));
                    }
                }
            }
        }
        // Sparks: one star of the chip, picked by the event (decoration).
        let count = mine().count();
        if count > 0 {
            for p in &co.sparks {
                let w = left(p.start_ns, now, SPARK_NS);
                if w <= 0.0 {
                    continue;
                }
                let k = (p.seq.wrapping_mul(7919) % count as u64) as usize;
                if let Some(s) = mine().nth(k) {
                    let glyph = if w > 0.5 { '✦' } else { '∘' };
                    put(canvas, s.x as i64, s.y as i64, (glyph, colour(p.rgb)));
                }
            }
        }
        let cols = slot_cols(co.slot, fx.slots, width);
        for f in &co.ecc {
            let centre = planets
                .iter()
                .find(|p| {
                    p.fleet_pos == co.slot && p.level == 2 && p.channel_idx == f.channel as usize
                })
                .map_or(((cols.start + cols.end) / 2, height / 2), |p| (p.x, p.y));
            draw_ecc_flare(f, now, centre, canvas);
        }
    }
    for c in &fx.comets {
        draw_comet(c, now, fx.slots, canvas);
    }
    // Labels last, over everything else on the top row.
    for co in &fx.chips {
        if let Some(flag) = &co.flag {
            draw_label(
                flag,
                &co.label_texts,
                slot_cols(co.slot, fx.slots, width),
                canvas,
            );
        }
    }
}

/// The cluster of a chip coming up, or its settle or fade.
fn draw_cluster<'a>(
    co: &StarChipFx,
    now: u64,
    stars: impl Iterator<Item = &'a Star>,
    canvas: &mut [Vec<(char, Color)>],
) {
    let Some(g) = co.cluster else {
        return;
    };
    // What each star becomes, from its distance `d` to the grid's centre:
    // stars with `lo <= d <= hi` take `glyph` (or keep theirs) and blend
    // toward `rgb` by `w`.
    let (lo, hi, glyph, rgb, w) = match g {
        Gauge::Progress(p) if p > 0.0 => (0.0, p, Some('●'), LOAD_PURPLE, 0.7),
        Gauge::Progress(_) => return,
        Gauge::Sweep { since_ns } => {
            // Decoration: the ring says the model is busy and does not say
            // how far.
            let period = SWEEP_PERIOD_NS as f32 / NS_PER_S as f32;
            let phase = (stepped_secs(since_ns, now) / period).fract();
            let centre = phase * (1.0 + 2.0 * BAND_HALF) - BAND_HALF;
            (
                centre - BAND_HALF,
                centre + BAND_HALF,
                Some('◉'),
                LOAD_PURPLE,
                0.6,
            )
        }
        Gauge::Settle { since_ns } => (0.0, 1.0, None, SERVE_TEAL, left(since_ns, now, SETTLE_NS)),
        Gauge::Fade { since_ns } => (0.0, 1.0, None, DOWN_GREY, left(since_ns, now, SETTLE_NS)),
    };
    if w <= 0.0 {
        return;
    }
    for s in stars {
        let d = star_distance(s);
        if d < lo || d > hi {
            continue;
        }
        if let Some(cell) = canvas.get_mut(s.y).and_then(|r| r.get_mut(s.x)) {
            if let Some(g) = glyph {
                cell.0 = g;
            }
            cell.1 = colour(lerp(rgb_of(cell.1, rgb), rgb, w));
        }
    }
}

/// A red flare around `centre`: a `✸` with a ring that grows as it fades.
/// The shape is decoration.
fn draw_ecc_flare(
    f: &EccFlare,
    now: u64,
    centre: (usize, usize),
    canvas: &mut [Vec<(char, Color)>],
) {
    let w = left(f.start_ns, now, FLASH_NS);
    if w <= 0.0 {
        return;
    }
    let c = colour(lerp(DOWN_GREY, ALARM_RED, w.max(0.3)));
    let (x, y) = (centre.0 as i64, centre.1 as i64);
    put(canvas, x, y, ('✸', c));
    let r = 1 + (stepped_secs(f.start_ns, now) * 5.0) as i64;
    for (dx, dy) in [(r * 2, 0), (-r * 2, 0), (0, r), (0, -r)] {
        put(canvas, x + dx, y + dy, ('·', c));
    }
}

/// A comet from the left edge of slot `c.slot`, across the field and
/// wrapping, with a tail of `c.tail` cells. Its row is picked by the event,
/// below the label row (decoration).
fn draw_comet(c: &Comet, now: u64, slots: usize, canvas: &mut [Vec<(char, Color)>]) {
    let w = left(c.start_ns, now, COMET_NS);
    let height = canvas.len();
    let width = canvas.first().map_or(0, Vec::len);
    if w <= 0.0 || width == 0 || height < 2 {
        return;
    }
    let row = 1 + (c.seq.wrapping_mul(7) % (height as u64 - 1)) as usize;
    let start = slot_cols(c.slot, slots, width).start;
    let travelled = (1.0 - w) * width as f32;
    let head = start + travelled as usize;
    for k in 0..=c.tail as usize {
        // Cells behind the head that have not left the start are not drawn.
        if k > travelled as usize {
            break;
        }
        let x = (head - k) % width;
        let (glyph, fade) = match k {
            0 => ('✦', 1.0),
            _ if k == c.tail as usize => ('·', 0.4),
            _ => ('─', 0.8 - 0.4 * k as f32 / c.tail as f32),
        };
        let rgb = lerp(DOWN_GREY, SERVE_TEAL, fade);
        put(canvas, x as i64, row as i64, (glyph, colour(rgb)));
    }
}

/// The model's label on the top row of the slot `cols`, one column in from
/// its left edge: the longest of `texts` that fits with a column to spare
/// on the right. When none fits, an estimate still gets a bare `~`.
fn draw_label(
    flag: &ModelFlag,
    texts: &[String],
    cols: std::ops::Range<usize>,
    canvas: &mut [Vec<(char, Color)>],
) {
    let room = cols.len().saturating_sub(2);
    let c = colour(flag.colour);
    let Some(row) = canvas.first_mut() else {
        return;
    };
    // Each text is one column per character (`one_column`).
    match texts.iter().find(|t| t.chars().count() <= room) {
        Some(t) => {
            for (i, ch) in t.chars().enumerate() {
                if let Some(cell) = row.get_mut(cols.start + 1 + i) {
                    *cell = (ch, c);
                }
            }
        }
        None if flag.estimated && room > 0 => {
            if let Some(cell) = row.get_mut(cols.start + 1) {
                *cell = ('~', c);
            }
        }
        None => {}
    }
}

// ---- Legend and explain text --------------------------------------------------

/// The Starfield legend's section on what the bus draws.
pub const LEGEND_TEXT: &[&str] = &[
    "Signal bus (each chip has its share of the width):",
    "  ● cluster  stars from the grid's centre turn purple",
    "             as a model compiles or loads, out to its",
    "             reported progress; with no progress a",
    "             ring sweeps out (decoration). Teal when",
    "             it becomes ready, grey when it goes down",
    "  label      the model's name, phase and tokens/s on",
    "             the top row; '~' marks an estimate",
    "  ✦── comet  completed requests, one tail cell per",
    "             request, up to eight",
    "  twinkle    faster as the model's tokens/s rise",
    "  ✸ flare    a DDR planet whose ECC count rose",
    "  ✦ star     a hivemind spark or a kernel compile",
    "  white      the chip's stars after a reset",
];

/// The Starfield explain text's paragraph on what the bus draws.
pub const EXPLAIN_TEXT: &[&str] = &[
    "",
    "Signal bus: each chip has its share of the",
    "width. While a model compiles or loads, the",
    "chip's stars light purple from the centre out",
    "to its reported progress (a sweeping ring,",
    "which is decoration, when there is no figure).",
    "They turn teal for a moment when it is ready",
    "and grey when it goes down. The model's name,",
    "phase and tokens/s sit on the top row; '~'",
    "marks an estimate such as mock data. Each",
    "batch of completed requests sends a comet",
    "across, one tail cell per request. Tokens/s",
    "speed the twinkle and the orbits. A DDR planet",
    "flares red when its ECC count rises, a spark",
    "or compile makes one star twinkle, and a reset",
    "turns the chip's stars white. Attached to a",
    "headless host, only this machine's chips are",
    "drawn.",
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
#[path = "starfield_bus_tests.rs"]
pub(crate) mod tests;
