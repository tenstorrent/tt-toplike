// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What the arcade draws from the signal bus: a bring-up gauge and a model
//! label per chip, request hits around the hero, the hero's heartbeat from
//! tokens per second, spark and compile particles in the starfield, and a
//! flash on a chip that was reset.
//!
//! # Two steps
//!
//! The shape is Die 3D's overlay (`die3d::overlay`):
//!
//! 1. [`ArcadeFx::from_world`] runs in `ArcadeVisualization::set_bus`, once
//!    per arcade update. It reads the world through [`WorldRead`], compares
//!    it with what the arcade saw last ([`ArcadeBusState`]) and returns an
//!    [`ArcadeFx`]: for each chip, what is on it and since when, plus the
//!    hits and the smoothed tokens per second. All the reading, matching and
//!    bookkeeping happens here.
//! 2. [`draw`] runs in `ArcadeVisualization::render` (the frame path). It
//!    turns the `ArcadeFx` into cells at a bus time with arithmetic only. It
//!    never reads the world.
//!
//! An empty world (no model, no event, no reset) gives an empty `ArcadeFx`,
//! and [`draw`] does nothing with an empty one, so the arcade draws exactly
//! what it drew before the bus.
//!
//! # Where things go
//!
//! Each device gets an equal share of the screen's width, in device order:
//! its *slot*. The starfield's separator row (the `✧ STARFIELD` rule) is the
//! gauge row. Within a chip's slot it shows:
//!
//! * **The gauge.** A model in `Compiling` or `Loading` colours the rule in
//!   the load purple from the slot's left edge, as far as the reported
//!   progress. With no progress figure a purple band sweeps across the slot.
//!   A change to `Ready` lights the whole slot in the serving teal and fades
//!   it over [`SETTLE_NS`]; a change to `Down` does the same in grey.
//! * **The model label**, right-aligned in the slot, in the phase's colour,
//!   with `~` in front when the model's readings are estimates.
//! * **A reset flash**: the slot's cells shown reversed for [`FLASH_NS`]
//!   after a new `reset_seq` on the chip.
//!
//! Request hits are a ring of `✦` glyphs that grows out from the hero. Spark
//! and compile particles rise in the starfield rows of the chip's slot.
//!
//! # What drives what
//!
//! * **Labels and gauges** come from `model_on_chip`. A model with an empty
//!   chip list is skipped: an attached world can name another machine's
//!   chips, which `ReadView::only_chips` cuts away.
//! * **A phase change** is a change against the phase this arcade saw last.
//!   The first sight of a model is not a change, and a re-announcement with
//!   the same phase changes nothing.
//! * **Hits** come from `RequestsCompleted` events seen since the last
//!   update, for a model on one of this arcade's chips. Their glyph count
//!   follows the event's `n`, up to [`MAX_HIT_GLYPHS`].
//! * **Particles** come from `Spark` events naming one of this arcade's
//!   chips, in `spark_rgb` colours, and from `KernelCompiled` events naming
//!   one, in the load purple. Events with no chip draw nothing, as in Die 3D.
//! * **Resets** come from `reset_seq` of each chip.
//! * **The heartbeat**: the sum of `model.tokens_per_s` over the ready models
//!   on this arcade's chips, when the reading is at most [`TPS_STALE_MS`]
//!   old, smoothed over [`TPS_TAU_S`]. A faster rate makes the hero's
//!   heartbeat faster. With no fresh reading the hero beats as before.
//!
//! Events already in the world at the first read are history and draw
//! nothing. So do events that arrived while the arcade was away (off screen
//! or another mode shown): an update more than [`PULSE_NS`] after the last
//! one, and more than four times the usual gap between updates, records the
//! newest event and draws nothing for it. This is Die 3D's rule, with its
//! constants.
//!
//! # Provenance
//!
//! The label's numbers are what the source measured. A model whose readings
//! are `Estimated` (the mock backend) or `Decoration` gets `~` before its
//! label, through `model_text_is_estimated`, the helper Die 3D's flag and
//! the `i` view use. The gauge's length is derived from the reported
//! progress. The following are decoration: the sweep shown with no progress
//! figure (it says the model is busy and does not say how far), the shape,
//! growth and fade of the hit ring and of the particles, where in the slot a
//! particle starts, and the length of the settle, fade and flash.

use crate::animation::die3d::overlay::{
    lerp, model_text_is_estimated, phase_rgb, spark_rgb, ModelFlag, BAND_HALF, DOWN_GREY, FLASH_NS,
    LOAD_PURPLE, MAX_PULSES, MAX_SPARKS, PULSE_NS, SERVE_TEAL, SETTLE_NS, SPARK_NS,
    SWEEP_PERIOD_NS,
};
use crate::ui::colors;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use std::ops::Range;
use tt_toplike_bus::{EntityKey, Event, ModelPhase, ModelView, WorldRead};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const NS_PER_S: u64 = 1_000_000_000;
/// Glyphs in the ring of one hit at most. One request draws one glyph.
pub const MAX_HIT_GLYPHS: u32 = 8;
/// How fast a hit's ring grows, in rows per second (columns grow twice as
/// fast, since a terminal cell is about twice as tall as it is wide).
/// Decoration.
const HIT_ROWS_PER_S: f32 = 4.0;
/// How fast a particle rises, in rows per second. Decoration.
const PARTICLE_ROWS_PER_S: f32 = 4.0;
/// A tokens-per-second reading older than this is ignored, and the hero
/// beats as it did before the bus.
pub const TPS_STALE_MS: u64 = 10_000;
/// The time constant of the tokens-per-second smoothing, in seconds.
pub const TPS_TAU_S: f64 = 2.0;
/// The hero's heartbeat period in frames with no serving rate: the period
/// `ArcadeVisualization::overlay_hero` has always used.
pub const HEARTBEAT_FRAMES: u32 = 60;
/// The shortest heartbeat period, in frames.
const HEARTBEAT_MIN_FRAMES: u32 = 12;
/// Tokens per second that halve the heartbeat period.
const HEARTBEAT_TPS: f64 = 50.0;
/// Steps per second effects move at, as in Die 3D.
const FX_STEPS_PER_S: f64 = 12.0;

/// How far a chip's gauge reaches, and why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gauge {
    /// Compiling or loading at this progress (0..1). Derived from the
    /// reported progress.
    Progress(f32),
    /// Compiling or loading with no progress figure: a band sweeping across
    /// the slot from `since_ns`. Decoration.
    Sweep { since_ns: u64 },
    /// The model reached `Ready` at `since_ns`: the slot lights in teal and
    /// fades over [`SETTLE_NS`].
    Settle { since_ns: u64 },
    /// The model went `Down` at `since_ns`: the slot greys and fades over
    /// [`SETTLE_NS`].
    Fade { since_ns: u64 },
}

/// One request hit: a ring of glyphs around the hero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub start_ns: u64,
    /// Glyphs in the ring: the requests completed, 1 to [`MAX_HIT_GLYPHS`].
    pub glyphs: u32,
    /// The event's sequence number, which turns the ring (decoration).
    pub seq: u64,
}

/// One spark or compile particle in a chip's slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Particle {
    pub start_ns: u64,
    /// The event's sequence number, which picks where in the slot it starts
    /// (decoration).
    pub seq: u64,
    pub rgb: [u8; 3],
}

/// Everything the bus puts on one chip's slot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChipFx {
    /// The chip's position among the devices: its slot.
    pub slot: usize,
    pub flag: Option<ModelFlag>,
    pub gauge: Option<Gauge>,
    pub particles: Vec<Particle>,
    /// When the chip's last reset flash started.
    pub flash_ns: Option<u64>,
}

impl ChipFx {
    fn is_empty(&self) -> bool {
        self.flag.is_none()
            && self.gauge.is_none()
            && self.particles.is_empty()
            && self.flash_ns.is_none()
    }
}

/// What the bus puts on the arcade. See the module doc.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArcadeFx {
    /// How many slots the width is cut into: the device count.
    pub slots: usize,
    /// One entry per chip that has something, in slot order.
    pub chips: Vec<ChipFx>,
    pub hits: Vec<Hit>,
    /// Requests completed in the events this update handled, for this
    /// arcade's chips. The duel's snake lunges when it is above 0.
    pub new_requests: u32,
    /// Smoothed tokens per second of the ready models on this arcade's
    /// chips. `None` with no fresh reading.
    pub tokens_per_s: Option<f64>,
}

/// The phase this arcade last saw a model in, and when it changed to it
/// (`None` for the phase the model had when first seen).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PhaseMemo {
    phase: ModelPhase,
    since_ns: Option<u64>,
}

/// What the arcade remembers between two [`ArcadeFx::from_world`] calls.
/// Start with the default; set a new default when the arcade stops reading a
/// world, so the next world is a first sight.
#[derive(Debug, Clone, Default)]
pub struct ArcadeBusState {
    /// The highest event sequence number handled. `None` before the first
    /// world.
    seen_seq: Option<u64>,
    /// Bus time of the last update.
    last_now: Option<u64>,
    /// The time between the last two updates.
    last_gap: Option<u64>,
    /// By model key.
    phases: HashMap<EntityKey, PhaseMemo>,
    hits: Vec<Hit>,
    /// By chip key.
    particles: HashMap<EntityKey, Vec<Particle>>,
    /// The last `reset_seq` seen, by chip key.
    resets: HashMap<EntityKey, Option<u64>>,
    /// When each chip's reset flash started, by chip key.
    flashes: HashMap<EntityKey, u64>,
    /// The smoothed tokens per second.
    tps: Option<f64>,
}

impl ArcadeFx {
    /// Whether nothing is drawn from the bus.
    pub fn is_empty(&self) -> bool {
        self.chips.is_empty() && self.hits.is_empty() && self.tokens_per_s.is_none()
    }

    /// The effects of `chips` (each a device's slot and its chip key,
    /// `EntityKey::chip(bus_id)`) out of `slots` device slots, from `world`
    /// at bus time `now_mono_ns`, updating `prev`. See the module doc.
    pub fn from_world(
        world: &dyn WorldRead,
        chips: &[(usize, EntityKey)],
        slots: usize,
        now_mono_ns: u64,
        prev: &mut ArcadeBusState,
    ) -> ArcadeFx {
        let now = now_mono_ns;
        let drawn = |c: &EntityKey| chips.iter().any(|(_, k)| k == c);
        let ours = |m: &ModelView| m.chips.iter().any(&drawn);
        let models = world.models();

        // Die 3D's first-sight rule: the first world, a world whose head went
        // backwards, and an update after the arcade was away handle no event.
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
        let mut new_requests = 0u32;
        if let Some(s) = since {
            for (q, e) in world.events_since(s) {
                seen = seen.max(q);
                match e {
                    Event::RequestsCompleted { model, n } => {
                        if models.iter().any(|m| m.key == model && ours(m)) {
                            new_requests = new_requests.saturating_add(n);
                            add_hit(&mut prev.hits, now, n, q);
                        }
                    }
                    Event::Spark {
                        chip: Some(c),
                        kind,
                    } if drawn(&c) => add_particle(prev, c, now, q, spark_rgb(&kind)),
                    Event::KernelCompiled { chip: Some(c), .. } if drawn(&c) => {
                        add_particle(prev, c, now, q, LOAD_PURPLE)
                    }
                    // Model events are announcements: the phase is read from
                    // the model below. Resets are read from `reset_seq`.
                    _ => {}
                }
            }
        }
        prev.seen_seq = Some(seen);

        // Resets: a `reset_seq` that moved since the last update flashes the
        // chip. The first sight of a chip, and an update after being away,
        // only record it.
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

        // Tokens per second: the ready models on these chips with a fresh
        // reading, smoothed. No fresh reading drops the smoothing.
        let raw: Option<f64> = models
            .iter()
            .filter(|m| m.phase == ModelPhase::Ready && ours(m))
            .filter_map(|m| {
                let r = world.reading(&m.key, tt_toplike_bus::signals::MODEL_TOKENS_PER_S)?;
                if world.age_ms(&r, now) > TPS_STALE_MS {
                    return None;
                }
                m.tokens_per_s.filter(|t| t.is_finite() && *t >= 0.0)
            })
            .fold(None, |sum, t| Some(sum.unwrap_or(0.0) + t));
        prev.tps = match (raw, prev.tps, gap) {
            (None, _, _) => None,
            (Some(r), Some(s), Some(g)) => {
                let alpha = 1.0 - (-(g as f64 / NS_PER_S as f64) / TPS_TAU_S).exp();
                Some(s + (r - s) * alpha)
            }
            (Some(r), _, _) => Some(r),
        };

        let mut out = ArcadeFx {
            slots,
            new_requests,
            tokens_per_s: prev.tps,
            ..ArcadeFx::default()
        };
        for (slot, key) in chips {
            let mut co = ChipFx {
                slot: *slot,
                ..ChipFx::default()
            };
            if let Some(m) = world.model_on_chip(key).filter(|m| !m.chips.is_empty()) {
                co.flag = Some(ModelFlag {
                    label: m.label.clone(),
                    phase: m.phase,
                    colour: phase_rgb(m.phase),
                    tokens_per_s: m.tokens_per_s,
                    waiting: m.waiting,
                    progress: m.progress,
                    estimated: model_text_is_estimated(world, &m),
                });
                co.gauge = gauge_of(&m, prev.phases.get(&m.key), now);
            }
            if let Some(p) = prev.particles.get_mut(key) {
                p.retain(|p| now.saturating_sub(p.start_ns) < SPARK_NS);
                co.particles = p.clone();
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
        prev.hits
            .retain(|h| now.saturating_sub(h.start_ns) < PULSE_NS);
        out.hits = prev.hits.clone();
        // Nothing is kept for a chip no longer drawn, or a flash that ended.
        prev.particles.retain(|k, p| drawn(k) && !p.is_empty());
        prev.flashes
            .retain(|k, t| drawn(k) && now.saturating_sub(*t) < FLASH_NS);
        out
    }

    /// The hero's heartbeat period in frames: [`HEARTBEAT_FRAMES`] with no
    /// serving rate, shorter as the smoothed tokens per second rise, down to
    /// 12 frames.
    pub fn heartbeat_frames(&self) -> u32 {
        match self.tokens_per_s {
            None => HEARTBEAT_FRAMES,
            Some(t) => {
                let p = HEARTBEAT_FRAMES as f64 / (1.0 + t / HEARTBEAT_TPS);
                (p.round() as u32).clamp(HEARTBEAT_MIN_FRAMES, HEARTBEAT_FRAMES)
            }
        }
    }
}

/// Add a hit for `n` requests at `now`, keeping at most [`MAX_PULSES`]: past
/// that the newest takes this one's glyphs on top of its own (up to
/// [`MAX_HIT_GLYPHS`]) and starts again, as Die 3D merges pulses.
fn add_hit(list: &mut Vec<Hit>, now: u64, n: u32, seq: u64) {
    list.retain(|h| now.saturating_sub(h.start_ns) < PULSE_NS);
    let glyphs = n.clamp(1, MAX_HIT_GLYPHS);
    if list.len() < MAX_PULSES {
        list.push(Hit {
            start_ns: now,
            glyphs,
            seq,
        });
    } else if let Some(last) = list.last_mut() {
        last.glyphs = (last.glyphs + glyphs).min(MAX_HIT_GLYPHS);
        last.start_ns = now;
    }
}

/// Add a particle on `chip` for the event with sequence number `seq`,
/// keeping at most [`MAX_SPARKS`] per chip.
fn add_particle(prev: &mut ArcadeBusState, chip: EntityKey, now: u64, seq: u64, rgb: [u8; 3]) {
    let list = prev.particles.entry(chip).or_default();
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

/// The gauge of model `m`, whose phase memo is `memo`, at `now`.
fn gauge_of(m: &ModelView, memo: Option<&PhaseMemo>, now: u64) -> Option<Gauge> {
    let since = memo.and_then(|p| p.since_ns);
    let recent = |t: &u64| now.saturating_sub(*t) < SETTLE_NS;
    match m.phase {
        ModelPhase::Compiling | ModelPhase::Loading => Some(match m.progress {
            Some(p) if p.is_finite() => Gauge::Progress(p.clamp(0.0, 1.0)),
            _ => Gauge::Sweep {
                since_ns: since.unwrap_or(0),
            },
        }),
        ModelPhase::Ready => since.filter(recent).map(|t| Gauge::Settle { since_ns: t }),
        ModelPhase::Down => since.filter(recent).map(|t| Gauge::Fade { since_ns: t }),
        ModelPhase::Alarm => None,
    }
}

// ---- The frame path ---------------------------------------------------------

/// Seconds since `start` at `now`, stepped to [`FX_STEPS_PER_S`] (Die 3D's
/// stepping, so effects move in whole steps).
fn stepped_secs(start: u64, now: u64) -> f32 {
    let s = now.saturating_sub(start) as f64 / NS_PER_S as f64;
    ((s * FX_STEPS_PER_S).floor() / FX_STEPS_PER_S) as f32
}

/// What is left (1 down to 0) of an effect of `dur_ns` that started at
/// `start`, on the stepped clock; 0 once `dur_ns` has passed.
fn left(start: u64, now: u64, dur_ns: u64) -> f32 {
    if now.saturating_sub(start) >= dur_ns {
        return 0.0;
    }
    (1.0 - stepped_secs(start, now) / (dur_ns as f32 / NS_PER_S as f32)).max(0.0)
}

/// Where on the arcade's lines the bus effects go.
#[derive(Debug, Clone)]
pub struct Stage {
    pub width: usize,
    /// The gauge row: the starfield's separator.
    pub gauge_row: usize,
    /// Columns at the start of the gauge row that hold its label; the model
    /// label never covers them.
    pub label_cols: usize,
    /// The starfield rows below the lanes, where particles rise.
    pub sky: Range<usize>,
    /// The hero's cell, and the first row it may use (below the lanes).
    pub hero: (f32, f32),
    pub top_row: usize,
}

/// The columns of slot `slot` of `slots` across `width`.
fn slot_cols(slot: usize, slots: usize, width: usize) -> Range<usize> {
    let n = slots.max(1);
    (slot * width / n)..((slot + 1) * width / n)
}

/// Draw `fx` at bus time `now` onto `lines`. Arithmetic on `fx` only. An
/// empty `fx` changes nothing.
pub fn draw(fx: &ArcadeFx, now: u64, stage: &Stage, lines: &mut [Line<'static>]) {
    if fx.chips.is_empty() && fx.hits.is_empty() {
        return;
    }
    for co in &fx.chips {
        let cols = slot_cols(co.slot, fx.slots, stage.width);
        if cols.is_empty() {
            continue;
        }
        draw_gauge(co, now, cols.clone(), stage.gauge_row, lines);
        if co.flash_ns.is_some_and(|t| left(t, now, FLASH_NS) > 0.0) {
            restyle(lines, stage.gauge_row, cols.clone(), |s| {
                s.add_modifier(Modifier::REVERSED)
            });
        }
        if let Some(flag) = &co.flag {
            draw_label(flag, cols.clone(), stage, lines);
        }
        for p in &co.particles {
            draw_particle(p, now, cols.clone(), &stage.sky, lines);
        }
    }
    for h in &fx.hits {
        draw_hit(h, now, stage, lines);
    }
}

/// The gauge of `co` on row `row` across `cols`.
fn draw_gauge(co: &ChipFx, now: u64, cols: Range<usize>, row: usize, lines: &mut [Line<'static>]) {
    let Some(gauge) = co.gauge else {
        return;
    };
    let len = cols.len() as f32;
    let a = cols.start;
    match gauge {
        Gauge::Progress(p) => {
            let end = a + (p * len).round() as usize;
            tint(lines, row, a..end.min(cols.end), LOAD_PURPLE, 1.0);
        }
        Gauge::Sweep { since_ns } => {
            // Decoration: the band says the model is busy, not how far.
            let period = SWEEP_PERIOD_NS as f32 / NS_PER_S as f32;
            let phase = (stepped_secs(since_ns, now) / period).fract();
            let centre = phase * (1.0 + 2.0 * BAND_HALF) - BAND_HALF;
            let lo = ((centre - BAND_HALF).max(0.0) * len).floor() as usize;
            let hi = ((centre + BAND_HALF).clamp(0.0, 1.0) * len).ceil() as usize;
            if hi > lo {
                tint(
                    lines,
                    row,
                    (a + lo)..(a + hi).min(cols.end),
                    LOAD_PURPLE,
                    0.8,
                );
            }
        }
        Gauge::Settle { since_ns } => {
            let w = left(since_ns, now, SETTLE_NS);
            if w > 0.0 {
                tint(lines, row, cols, SERVE_TEAL, w);
            }
        }
        Gauge::Fade { since_ns } => {
            let w = left(since_ns, now, SETTLE_NS);
            if w > 0.0 {
                tint(lines, row, cols, DOWN_GREY, w);
            }
        }
    }
}

/// The model's label, right-aligned in the slot with one column to spare,
/// clear of the gauge row's own label. The longest of the flag's texts that
/// fits; none when none fits.
fn draw_label(flag: &ModelFlag, cols: Range<usize>, stage: &Stage, lines: &mut [Line<'static>]) {
    let start_min = cols.start.max(stage.label_cols + 1);
    let end = cols.end.saturating_sub(1);
    let Some(text) = flag.texts().into_iter().find(|t| {
        let w = UnicodeWidthStr::width(t.as_str());
        end >= w && end - w >= start_min
    }) else {
        return;
    };
    let style = Style::default()
        .fg(colors::rgb(flag.colour[0], flag.colour[1], flag.colour[2]))
        .add_modifier(Modifier::BOLD);
    let mut col = end - UnicodeWidthStr::width(text.as_str());
    for ch in text.chars() {
        crate::animation::ArcadeVisualization::splice_char(lines, stage.gauge_row, col, ch, style);
        col += ch.width().unwrap_or(1);
    }
}

/// A particle rising in the sky rows of the slot `cols`.
fn draw_particle(
    p: &Particle,
    now: u64,
    cols: Range<usize>,
    sky: &Range<usize>,
    lines: &mut [Line<'static>],
) {
    let w = left(p.start_ns, now, SPARK_NS);
    if w <= 0.0 || sky.is_empty() || cols.is_empty() {
        return;
    }
    // Decoration: where in the slot it starts, and how it rises.
    let col = cols.start + (p.seq.wrapping_mul(7919) % cols.len() as u64) as usize;
    let base = sky.start + (p.seq.wrapping_mul(31) % sky.len() as u64) as usize;
    let rise = (stepped_secs(p.start_ns, now) * PARTICLE_ROWS_PER_S) as usize;
    let row = base.saturating_sub(rise).max(sky.start);
    let glyph = if w > 0.5 { '*' } else { '·' };
    let style = Style::default()
        .fg(colors::rgb(p.rgb[0], p.rgb[1], p.rgb[2]))
        .add_modifier(Modifier::BOLD);
    crate::animation::ArcadeVisualization::splice_char(lines, row, col, glyph, style);
}

/// A hit's ring of glyphs around the hero, growing as it fades.
fn draw_hit(h: &Hit, now: u64, stage: &Stage, lines: &mut [Line<'static>]) {
    let w = left(h.start_ns, now, PULSE_NS);
    if w <= 0.0 {
        return;
    }
    // Decoration: the ring's size, turn and fade.
    let r = 1.0 + stepped_secs(h.start_ns, now) * HIT_ROWS_PER_S;
    let glyph = if w > 0.5 { '✦' } else { '·' };
    let style = Style::default()
        .fg(colors::rgb(SERVE_TEAL[0], SERVE_TEAL[1], SERVE_TEAL[2]))
        .add_modifier(Modifier::BOLD);
    let turn = (h.seq % 8) as f32 * std::f32::consts::PI / 8.0;
    let (hx, hy) = stage.hero;
    for j in 0..h.glyphs {
        let angle = turn + j as f32 * std::f32::consts::TAU / h.glyphs as f32;
        let x = hx + angle.cos() * r * 2.0;
        let y = hy + angle.sin() * r;
        if x < 0.0 || y < stage.top_row as f32 {
            continue;
        }
        let (col, row) = (x as usize, y as usize);
        // The hero's own cell stays the hero's.
        if col >= stage.width || row >= lines.len() || (col, row) == (hx as usize, hy as usize) {
            continue;
        }
        crate::animation::ArcadeVisualization::splice_char(lines, row, col, glyph, style);
    }
}

/// Blend the foreground of the cells `cols` of `row` toward `rgb` by
/// `weight`, keeping the characters.
fn tint(lines: &mut [Line<'static>], row: usize, cols: Range<usize>, rgb: [u8; 3], weight: f32) {
    restyle(lines, row, cols, |s| {
        let base = match s.fg {
            Some(Color::Rgb(r, g, b)) => [r, g, b],
            _ => rgb,
        };
        let c = lerp(base, rgb, weight.clamp(0.0, 1.0));
        s.fg(colors::rgb(c[0], c[1], c[2]))
    });
}

/// Apply `f` to the style of each cell in `cols` of `row`, keeping the
/// characters and the other cells' styles.
fn restyle(
    lines: &mut [Line<'static>],
    row: usize,
    cols: Range<usize>,
    f: impl Fn(Style) -> Style,
) {
    let Some(line) = lines.get_mut(row) else {
        return;
    };
    if cols.is_empty() {
        return;
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 2);
    let mut col = 0usize;
    for span in std::mem::take(&mut line.spans) {
        let w = UnicodeWidthStr::width(span.content.as_ref());
        if col + w <= cols.start || col >= cols.end {
            col += w;
            out.push(span);
            continue;
        }
        // The span crosses the range: split it into runs inside and outside.
        let mut run = String::new();
        let mut run_in = false;
        for ch in span.content.chars() {
            let inside = cols.contains(&col);
            if inside != run_in && !run.is_empty() {
                let style = if run_in { f(span.style) } else { span.style };
                out.push(Span::styled(std::mem::take(&mut run), style));
            }
            run_in = inside;
            run.push(ch);
            col += ch.width().unwrap_or(1);
        }
        if !run.is_empty() {
            let style = if run_in { f(span.style) } else { span.style };
            out.push(Span::styled(run, style));
        }
    }
    line.spans = out;
}

// ---- Legend and explain text --------------------------------------------------

/// The arcade legend's section on what the bus draws.
pub const LEGEND_TEXT: &[&str] = &[
    "Each chip has an equal share of the width, chip 0",
    "on the left. What the signal bus puts there:",
    "  gauge   the ✧ STARFIELD rule turns purple while a",
    "          model compiles or loads, as far as its",
    "          reported progress; with no progress a band",
    "          sweeps across (decoration). It lights teal",
    "          when the model becomes ready and grey when",
    "          it goes down, fading over two seconds",
    "  label   the model's name, phase and tokens/s at the",
    "          right of the rule, in the phase colour; '~'",
    "          marks an estimate such as mock data",
    "  ✦ ring  grows from the hero when requests complete:",
    "          one glyph per request, up to eight",
    "  * rise  a hivemind spark or a kernel compile on the",
    "          chip, in the spark colours; the motion is",
    "          decoration",
    "  flash   the chip's share of the rule shows reversed",
    "          for a moment after the chip is reset",
    "  @ beat  the hero pulses faster as the ready models'",
    "          tokens/s rise; with no fresh rate it beats",
    "          as it always has",
];

/// The arcade explain text's paragraph on what the bus draws.
pub const EXPLAIN_TEXT: &[&str] = &[
    "",
    "Signal bus: each chip has an equal share of",
    "the width, chip 0 on the left. The STARFIELD",
    "rule is a gauge: purple as a model compiles",
    "or loads, as far as its reported progress (a",
    "sweeping band, which is decoration, when there",
    "is no progress figure), teal for a moment when",
    "it becomes ready, grey when it goes down. The",
    "model's name, phase and tokens/s sit at the",
    "right of the rule; '~' marks an estimate such",
    "as mock data. Completed requests send a ring",
    "of ✦ out from the hero, one per request up to",
    "eight. Hivemind sparks and kernel compiles rise",
    "as * in the chip's share of the stars. A chip",
    "reset flashes its share of the rule. The hero",
    "beats faster as tokens/s rise. Attached to a",
    "headless host, only this machine's chips are",
    "drawn, and the duel uses the host's serving",
    "numbers.",
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

/// The bus's hero serving stats when the TUI is attached to a host: the
/// first ready row's, from the rows built from the host's world. The local
/// monitor's stats belong to this machine and say nothing about the host.
pub fn attached_serving(
    rows: &[crate::workload::inference_server::ServiceState],
) -> Option<crate::workload::inference_server::ServingStats> {
    use crate::workload::inference_server::Phase;
    rows.iter()
        .find(|s| s.phase == Phase::Ready)
        .and_then(|s| s.serving)
}
