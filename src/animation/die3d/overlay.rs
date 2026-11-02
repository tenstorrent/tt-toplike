// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What Die 3D draws from the signal bus: a model flag per chip, a bring-up
//! fill, request-completion pulses, per-channel GDDR rows and hivemind sparks.
//!
//! # Two steps
//!
//! 1. [`Overlay::from_world`] runs in `Die3dView::update` (the data rate). It
//!    reads the world through [`WorldRead`], compares it with the view's
//!    memory ([`OverlayState`]) and returns an [`Overlay`]: for each drawn
//!    chip, what is on it and since when. All the reading, matching and
//!    bookkeeping happens here, off the frame path.
//! 2. [`Overlay::fx_at`] runs in `Die3dView::render` (the frame rate). It
//!    turns the overlay into the per-chip numbers the scene's raster loop
//!    reads ([`ChipFx`]): how far the fill reaches, how brightly the pulses
//!    light the PCIe connector, each GDDR row's heat and flash, each spark's
//!    colour.
//!    It does no lookups in the world, only arithmetic on the overlay.
//!
//! An empty world (no model, no event, no per-channel DRAM reading) gives an
//! empty overlay, and an empty overlay gives an empty [`FxMap`]: the scene
//! then draws exactly what it drew before the bus existed.
//!
//! # What drives what
//!
//! * **Flags** come from `model_on_chip`. A model with an empty chip list is
//!   skipped: an attached world can hold another machine's models, which
//!   `ReadView::only_chips` cuts to no chips here.
//! * **The fill** follows the model's phase and progress, never the
//!   `ModelLoading`/`ModelReady` events: the host re-announces every model
//!   every five minutes, and an announcement with an unchanged phase must
//!   change nothing. A phase change is a change against the phase this view
//!   saw last ([`OverlayState`]); the first sight of a model is not a change,
//!   so a model already `Ready` when the view opens gets no settle.
//! * **Pulses** come from `RequestsCompleted` events seen since the last
//!   update (`events_since`). Events already in the world when the view first
//!   reads it are history and make no pulse. So are events that arrived
//!   while the view was away: an update more than a pulse lifetime after the
//!   last one, and more than four times the usual gap between updates,
//!   records the newest event and draws no pulse or spark for it.
//! * **GDDR rows** read the `dram:<chip>/<n>` entities. An ECC counter that
//!   rose since the last update, or an `EccTick` event, flashes its row.
//! * **Sparks** come from `Spark` events naming a drawn chip and from
//!   `KernelCompiled` events (purple).
//!
//! # Provenance
//!
//! The flag's numbers are what the source measured; a source marked
//! `Estimated` (a mock run) puts `~` before the flag text. The fill is
//! derived from the reported progress, except for the sweep shown while a
//! model compiles or loads with no progress figure, which is decoration (it
//! says the model is busy and does not say how far). Pulses and sparks mark
//! events; their shape and decay are decoration.

use crate::bus::signals as sig;
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, Event, ModelPhase, ModelView, Provenance, Value, WorldRead};

/// The workload colours, one definition shared with `activity_lanes`: load
/// purple, serve teal, train amber, and the error red stalled work uses
/// (`colors::ERROR_RGB`).
pub const LOAD_PURPLE: [u8; 3] = crate::animation::activity_lanes::LOAD_RGB;
pub const SERVE_TEAL: [u8; 3] = crate::animation::activity_lanes::SERVE_RGB;
pub const TRAIN_AMBER: [u8; 3] = crate::animation::activity_lanes::TRAIN_RGB;
pub const ALARM_RED: [u8; 3] = crate::ui::colors::ERROR_RGB;
/// A model that is down: the palette's management grey.
pub const DOWN_GREY: [u8; 3] = [112, 110, 128];

const NS_PER_S: u64 = 1_000_000_000;
/// How long a pulse lasts.
pub const PULSE_NS: u64 = NS_PER_S;
/// Pulses drawn at once on one chip; more merge into the newest.
pub const MAX_PULSES: usize = 4;
/// How long a fill that reached `Ready` takes to settle to the normal glow.
pub const SETTLE_NS: u64 = 2 * NS_PER_S;
/// Period of the sweep shown while a model loads with no progress figure.
pub const SWEEP_PERIOD_NS: u64 = 3 * NS_PER_S;
/// How long an ECC flash lasts.
pub const FLASH_NS: u64 = 600_000_000;
/// How long a spark lasts.
pub const SPARK_NS: u64 = 800_000_000;
/// Sparks kept per chip; the oldest go first.
pub const MAX_SPARKS: usize = 6;
/// GDDR channels on a Blackhole chip: four on each side of the die.
pub const GDDR_CHANNELS: u8 = 8;
/// Ethernet cages along a die's back edge; a spark stands over one of them.
const SPARK_COLUMNS: u64 = 12;

/// Steps per second the effects move at: the tile wave's rate
/// (`scene::WAVE_STEPS_PER_SEC`), so an effect's step and a wave step land on
/// the same frames and the terminal repaints once for both.
const FX_STEPS_PER_S: f64 = 12.0;

/// The model on a chip, as its flag shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelFlag {
    pub label: String,
    pub phase: ModelPhase,
    /// The phase's workload colour.
    pub colour: [u8; 3],
    pub tokens_per_s: Option<f64>,
    pub waiting: Option<u32>,
    pub progress: Option<f32>,
    /// The model's readings are `Estimated` (a mock run): the text starts
    /// with `~`.
    pub estimated: bool,
}

impl ModelFlag {
    /// The phase as one lowercase word.
    pub fn phase_word(&self) -> &'static str {
        match self.phase {
            ModelPhase::Down => "down",
            ModelPhase::Compiling => "compiling",
            ModelPhase::Loading => "loading",
            ModelPhase::Ready => "ready",
            ModelPhase::Alarm => "alarm",
        }
    }

    /// The flag's text forms, longest first: label, phase, progress while
    /// loading, tokens/s and waiting requests; then without the waiting
    /// count; then with the label cut to 12 characters. Every form starts
    /// with `~` when the readings are estimates.
    pub fn texts(&self) -> Vec<String> {
        let tilde = if self.estimated { "~" } else { "" };
        let mut tail = String::new();
        if let (Some(p), ModelPhase::Compiling | ModelPhase::Loading) = (self.progress, self.phase)
        {
            tail.push_str(&format!(" {:.0}%", (p.clamp(0.0, 1.0) * 100.0)));
        }
        if let Some(t) = self.tokens_per_s.filter(|t| t.is_finite() && *t >= 0.0) {
            if t < 10.0 {
                tail.push_str(&format!(" {t:.1}t/s"));
            } else {
                tail.push_str(&format!(" {t:.0}t/s"));
            }
        }
        let waiting = match self.waiting {
            Some(w) if w > 0 => format!(" {w} waiting"),
            _ => String::new(),
        };
        let phase = self.phase_word();
        let short: String = if self.label.chars().count() > 12 {
            let kept: String = self.label.chars().take(11).collect();
            format!("{kept}\u{2026}")
        } else {
            self.label.clone()
        };
        let mut out = vec![
            format!("{tilde}{} {phase}{tail}{waiting}", self.label),
            format!("{tilde}{} {phase}{tail}", self.label),
            format!("{tilde}{short} {phase}{tail}"),
        ];
        out.dedup();
        out
    }
}

/// How far a bring-up fill reaches, and why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fill {
    /// Compiling or loading at this progress (0..1): the fill reaches this
    /// share of the way from the DRAM edge to the middle of the die.
    /// Derived from the reported progress.
    Progress(f32),
    /// Compiling or loading with no progress figure: a band sweeping from
    /// the DRAM edge inward, again and again. Decoration: it says the model
    /// is busy and does not say how far it got. The sweep runs from
    /// `since_ns`.
    Sweep { since_ns: u64 },
    /// The model reached `Ready` at `since_ns`: the whole span is filled and
    /// fades to the normal glow over [`SETTLE_NS`].
    Settle { since_ns: u64 },
}

/// One request-completion pulse. It lights the chip's PCIe connector in
/// the serving teal, fading over [`PULSE_NS`]. (A ring crossing the die
/// looked better and repainted about twice the cells the Task 16 budget
/// allows; the connector is where the requests come and go.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pulse {
    pub start_ns: u64,
    /// 0.4 for one request up to 1.0 for seven or more; a merged pulse
    /// (more than [`MAX_PULSES`] at once) reaches up to 1.4.
    pub strength: f32,
}

/// One GDDR channel's row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GddrRow {
    pub channel: u8,
    /// The channel's hotter temperature mapped from 40..90 C onto 0..1, as
    /// `ChipInputs::gddr_heat` maps the chip's hottest. `None` without a
    /// per-channel reading: the row then takes the chip-wide value.
    pub heat: Option<f32>,
    /// Harvested or not enabled: drawn dim and flat.
    pub dim: bool,
    /// When the row's ECC count last rose.
    pub flash_ns: Option<u64>,
}

/// One spark over a chip's back edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spark {
    pub start_ns: u64,
    /// The Ethernet cage column (1..=12) it stands over.
    pub column: u8,
    pub rgb: [u8; 3],
}

/// Everything the bus puts on one drawn chip.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChipOverlay {
    /// The chip's device index (`ChipInputs::index`).
    pub index: usize,
    pub flag: Option<ModelFlag>,
    pub fill: Option<Fill>,
    pub pulses: Vec<Pulse>,
    pub gddr: Vec<GddrRow>,
    pub sparks: Vec<Spark>,
}

impl ChipOverlay {
    fn is_empty(&self) -> bool {
        self.flag.is_none()
            && self.fill.is_none()
            && self.pulses.is_empty()
            && self.gddr.is_empty()
            && self.sparks.is_empty()
    }
}

/// What the bus puts on the drawn chips: one entry per chip that has
/// something, in the order of the `chips` given to [`Overlay::from_world`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overlay {
    pub chips: Vec<ChipOverlay>,
}

/// The phase this view last saw a model in, and when it changed to it
/// (`None` for the phase the model had when first seen).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PhaseMemo {
    phase: ModelPhase,
    since_ns: Option<u64>,
}

/// What a view remembers between two [`Overlay::from_world`] calls. Start
/// with `OverlayState::default()`; drop it (set a new default) when the view
/// stops reading a world, so the next world is a first sight.
#[derive(Debug, Clone, Default)]
pub struct OverlayState {
    /// The highest event sequence number handled. `None` before the first
    /// world.
    seen_seq: Option<u64>,
    /// Bus time of the last update. See `Overlay::from_world` for when a
    /// gap after it makes the next update a first sight.
    last_now: Option<u64>,
    /// The time between the last two updates: the usual spacing (the data
    /// tick, 100 ms by default).
    last_gap: Option<u64>,
    /// By model key.
    phases: HashMap<EntityKey, PhaseMemo>,
    /// By chip key.
    pulses: HashMap<EntityKey, Vec<Pulse>>,
    /// By chip key.
    sparks: HashMap<EntityKey, Vec<Spark>>,
    /// Sum of the four ECC counters, by DRAM channel key.
    ecc: HashMap<EntityKey, u64>,
    /// When each DRAM channel's last flash started, by DRAM channel key.
    flashes: HashMap<EntityKey, u64>,
}

/// A reading's value as a number (`Text` is not one).
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) => Some(*x).filter(|x| x.is_finite()),
        Value::U64(x) => Some(*x as f64),
        Value::I64(x) => Some(*x as f64),
        Value::Text(_) => None,
    }
}

/// Pulse strength for `n` completed requests.
pub(crate) fn strength(n: u32) -> f32 {
    (0.3 + 0.1 * n as f32).min(1.0)
}

/// The colour of a spark of `kind`, from its first word (the hivemind
/// source: `vLLM.log` is from vLLM). Serving teal for vLLM, purple for the
/// compilers and runtimes, amber for the driver and tools, white otherwise.
pub fn spark_rgb(kind: &str) -> [u8; 3] {
    let first = kind
        .split(|c: char| c == '.' || c.is_whitespace())
        .next()
        .unwrap_or("");
    match first.to_ascii_lowercase().as_str() {
        "vllm" => SERVE_TEAL,
        "ttnn" | "metal" | "tt-lang" | "tt-forge" | "tt-xla" => LOAD_PURPLE,
        "driver" | "tt-smi" | "inspect" | "host" => TRAIN_AMBER,
        _ => [235, 235, 245],
    }
}

/// The colour of a model's flag in `phase`.
pub fn phase_rgb(phase: ModelPhase) -> [u8; 3] {
    match phase {
        ModelPhase::Compiling | ModelPhase::Loading => LOAD_PURPLE,
        ModelPhase::Ready => SERVE_TEAL,
        ModelPhase::Alarm => ALARM_RED,
        ModelPhase::Down => DOWN_GREY,
    }
}

impl Overlay {
    /// Whether nothing is drawn from the bus.
    pub fn is_empty(&self) -> bool {
        self.chips.is_empty()
    }

    /// The overlay of `chips` (each a drawn chip's device index and its
    /// chip key, `EntityKey::chip(bus_id)`) from `world` at bus time
    /// `now_mono_ns`, updating `prev`. See the module doc.
    pub fn from_world(
        world: &dyn WorldRead,
        chips: &[(usize, EntityKey)],
        now_mono_ns: u64,
        prev: &mut OverlayState,
    ) -> Overlay {
        let now = now_mono_ns;
        let drawn = |c: &EntityKey| chips.iter().any(|(_, k)| k == c);
        let models = world.models();

        // Events since the last update. The first world, a world whose head
        // went backwards (a restarted host), and an update after the view
        // was away (off screen, or another view shown) are a first sight:
        // their events are history, and a pulse or spark for them would
        // show something that is over. "Away" is a gap since the last
        // update longer than the pulse lifetime and longer than four times
        // the gap before it, so a slow data tick (`--interval 2000`) still
        // pulses on every update.
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
                    Event::RequestsCompleted { model, n } => {
                        let Some(m) = models.iter().find(|m| m.key == model) else {
                            continue;
                        };
                        for c in m.chips.iter().filter(|c| drawn(c)) {
                            add_pulse(prev.pulses.entry(c.clone()).or_default(), now, n);
                        }
                    }
                    Event::Spark {
                        chip: Some(c),
                        kind,
                    } if drawn(&c) => {
                        add_spark(prev, c, now, q, spark_rgb(&kind));
                    }
                    Event::KernelCompiled { chip: Some(c), .. } if drawn(&c) => {
                        add_spark(prev, c, now, q, LOAD_PURPLE);
                    }
                    Event::EccTick { chip, channel } if drawn(&chip) => {
                        prev.flashes
                            .insert(EntityKey::dram_channel(&chip, channel), now);
                    }
                    // `ModelLoading` and `ModelReady` are announcements; the
                    // phase they set is read from the model below.
                    _ => {}
                }
            }
        }
        prev.seen_seq = Some(seen);

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

        let mut out = Overlay::default();
        for (index, key) in chips {
            let mut co = ChipOverlay {
                index: *index,
                ..ChipOverlay::default()
            };
            if let Some(m) = world.model_on_chip(key).filter(|m| !m.chips.is_empty()) {
                co.flag = Some(flag_of(world, &m));
                co.fill = fill_of(&m, prev.phases.get(&m.key), now);
            }
            if let Some(p) = prev.pulses.get_mut(key) {
                p.retain(|p| now.saturating_sub(p.start_ns) < PULSE_NS);
                co.pulses = p.clone();
            }
            if let Some(s) = prev.sparks.get_mut(key) {
                s.retain(|s| now.saturating_sub(s.start_ns) < SPARK_NS);
                co.sparks = s.clone();
            }
            co.gddr = gddr_of(world, key, now, prev);
            if !co.is_empty() {
                out.chips.push(co);
            }
        }
        // Nothing is kept for a chip no longer drawn, or a flash that ended.
        prev.pulses.retain(|k, p| drawn(k) && !p.is_empty());
        prev.sparks.retain(|k, s| drawn(k) && !s.is_empty());
        prev.flashes
            .retain(|_, t| now.saturating_sub(*t) < FLASH_NS);
        out
    }

    /// The effects to draw at bus time `now_mono_ns`, by device index. An
    /// inset gets the fill and the pulses only. See the module doc.
    pub fn fx_at(&self, now_mono_ns: u64, inset: bool) -> FxMap {
        let mut map = FxMap::new();
        for co in &self.chips {
            let fx = ChipFx::of(co, now_mono_ns, inset);
            if !fx.is_empty() {
                map.insert(co.index, fx);
            }
        }
        map
    }

    /// The model flag of the chip with device index `index`.
    pub fn flag(&self, index: usize) -> Option<&ModelFlag> {
        self.chips
            .iter()
            .find(|c| c.index == index)
            .and_then(|c| c.flag.as_ref())
    }
}

/// Add a pulse for `n` requests at `now` to `list`, keeping at most
/// [`MAX_PULSES`]: past that the newest takes this one's strength on top of
/// its own (up to 1.4) and starts again.
fn add_pulse(list: &mut Vec<Pulse>, now: u64, n: u32) {
    list.retain(|p| now.saturating_sub(p.start_ns) < PULSE_NS);
    let s = strength(n);
    if list.len() < MAX_PULSES {
        list.push(Pulse {
            start_ns: now,
            strength: s,
        });
    } else if let Some(last) = list.last_mut() {
        last.strength = (last.strength + s).min(1.4);
        last.start_ns = now;
    }
}

/// Add a spark on `chip` for the event with sequence number `seq`, which
/// picks the cage column it stands over.
fn add_spark(prev: &mut OverlayState, chip: EntityKey, now: u64, seq: u64, rgb: [u8; 3]) {
    let list = prev.sparks.entry(chip).or_default();
    list.retain(|s| now.saturating_sub(s.start_ns) < SPARK_NS);
    if list.len() >= MAX_SPARKS {
        list.remove(0);
    }
    list.push(Spark {
        start_ns: now,
        column: 1 + (seq % SPARK_COLUMNS) as u8,
        rgb,
    });
}

/// Every model reading that puts text in the flag: the label, the phase,
/// the progress, tokens/s and the waiting count. The `i` view marks a model
/// from the same list (see [`model_text_is_estimated`]).
pub(crate) const FLAG_TEXT_SIGNALS: [&str; 5] = [
    tt_toplike_bus::signals::MODEL_LABEL,
    tt_toplike_bus::signals::MODEL_PHASE,
    tt_toplike_bus::signals::MODEL_PROGRESS,
    tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
    tt_toplike_bus::signals::MODEL_WAITING,
];

/// Whether any reading in [`FLAG_TEXT_SIGNALS`] of model `m` is marked
/// `Estimated` or `Decoration`. Die 3D's flag then starts with `~`, and the
/// `i` view puts `~` before the model's label. Both call this one function,
/// so the two views mark the same models.
pub(crate) fn model_text_is_estimated(world: &dyn WorldRead, m: &ModelView) -> bool {
    FLAG_TEXT_SIGNALS
        .iter()
        .filter_map(|s| world.reading(&m.key, s))
        .any(|r| matches!(r.prov, Provenance::Estimated | Provenance::Decoration))
}

/// The flag of model `m`.
pub(crate) fn flag_of(world: &dyn WorldRead, m: &ModelView) -> ModelFlag {
    let estimated = model_text_is_estimated(world, m);
    ModelFlag {
        label: m.label.clone(),
        phase: m.phase,
        colour: phase_rgb(m.phase),
        tokens_per_s: m.tokens_per_s,
        waiting: m.waiting,
        progress: m.progress,
        estimated,
    }
}

/// The fill of model `m`, whose phase memo is `memo`, at `now`.
fn fill_of(m: &ModelView, memo: Option<&PhaseMemo>, now: u64) -> Option<Fill> {
    let since = memo.and_then(|p| p.since_ns);
    match m.phase {
        ModelPhase::Compiling | ModelPhase::Loading => Some(match m.progress {
            Some(p) if p.is_finite() => Fill::Progress(p.clamp(0.0, 1.0)),
            _ => Fill::Sweep {
                since_ns: since.unwrap_or(0),
            },
        }),
        ModelPhase::Ready => since
            .filter(|t| now.saturating_sub(*t) < SETTLE_NS)
            .map(|t| Fill::Settle { since_ns: t }),
        ModelPhase::Down | ModelPhase::Alarm => None,
    }
}

/// The GDDR rows of `chip` that have a per-channel reading or a flash.
fn gddr_of(
    world: &dyn WorldRead,
    chip: &EntityKey,
    now: u64,
    prev: &mut OverlayState,
) -> Vec<GddrRow> {
    let mut rows = Vec::new();
    for ch in 0..GDDR_CHANNELS {
        let dk = EntityKey::dram_channel(chip, ch);
        let num = |s: &str| world.reading(&dk, s).and_then(|r| number(&r.value));
        let temp = [sig::DRAM_TEMP_TOP_C, sig::DRAM_TEMP_BOTTOM_C]
            .iter()
            .filter_map(|s| num(s))
            .fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t))));
        let harvested = num(sig::DRAM_HARVESTED);
        let enabled = num(sig::DRAM_ENABLED);
        let ecc: Option<u64> = [
            sig::DRAM_ECC_CORR_RD,
            sig::DRAM_ECC_CORR_WR,
            sig::DRAM_ECC_UNCORR_RD,
            sig::DRAM_ECC_UNCORR_WR,
        ]
        .iter()
        .filter_map(|s| num(s))
        .fold(None, |sum: Option<u64>, v| {
            Some(sum.unwrap_or(0).saturating_add(v as u64))
        });
        if let Some(total) = ecc {
            if let Some(old) = prev.ecc.insert(dk.clone(), total) {
                if total > old {
                    prev.flashes.insert(dk.clone(), now);
                }
            }
        }
        let flash_ns = prev
            .flashes
            .get(&dk)
            .copied()
            .filter(|t| now.saturating_sub(*t) < FLASH_NS);
        let any = temp.is_some() || harvested.is_some() || enabled.is_some() || ecc.is_some();
        if !any && flash_ns.is_none() {
            continue;
        }
        rows.push(GddrRow {
            channel: ch,
            heat: temp.map(|c| ((c as f32 - 40.0) / 50.0).clamp(0.0, 1.0)),
            dim: harvested == Some(1.0) || enabled == Some(0.0),
            flash_ns,
        });
    }
    rows
}

// ---- What the raster loop reads ---------------------------------------------

/// The effects of each chip, by device index (`ChipInputs::index`). Empty
/// when nothing comes from the bus.
pub type FxMap = HashMap<usize, ChipFx>;

/// How far a fill reaches at one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillFx {
    /// Every Tensix tile up to `depth` (0..1, see [`tile_depth`]) is tinted
    /// purple by `weight`.
    UpTo { depth: f32, weight: f32 },
    /// Tiles within [`BAND_HALF`] of `centre` are tinted purple by `weight`.
    Band { centre: f32, weight: f32 },
}

/// Half the width of the sweep's band, in depth units.
pub const BAND_HALF: f32 = 0.15;
/// Brightness levels a pulse fades through in its second. Each level
/// repaints the PCIe connector's cells once; a smooth fade would repaint
/// them on every effect step.
pub const PULSE_LEVELS: f32 = 2.0;
/// How strongly a fill tints the tiles it covers.
pub const FILL_WEIGHT: f32 = 0.55;
/// How strongly a full-strength pulse tints the PCIe connector.
pub const PULSE_WEIGHT: f32 = 0.7;

/// One GDDR channel at one frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GddrFx {
    /// `None`: the chip-wide heat.
    pub heat: Option<f32>,
    pub dim: bool,
    /// 0..1: how far the row is blended toward the alarm red.
    pub flash: f32,
}

/// What the raster loop draws on one chip at one frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChipFx {
    pub fill: Option<FillFx>,
    /// How strongly the live pulses light the PCIe connector (0..1): the
    /// brightest of them. 0 with none.
    pub pulse: f32,
    /// By channel; `None` draws the row as before.
    pub gddr: [Option<GddrFx>; GDDR_CHANNELS as usize],
    /// Each live spark's cage column, colour and weight.
    pub sparks: Vec<(u8, [u8; 3], f32)>,
}

/// Seconds since `start` at `now`, stepped to [`FX_STEPS_PER_S`].
fn stepped_secs(start: u64, now: u64) -> f32 {
    let s = now.saturating_sub(start) as f64 / NS_PER_S as f64;
    ((s * FX_STEPS_PER_S).floor() / FX_STEPS_PER_S) as f32
}

/// What is left (1 down to 0) of an effect of `dur_ns` that started at
/// `start` at `now`, on the stepped clock; 0 once `dur_ns` has passed.
fn left(start: u64, now: u64, dur_ns: u64) -> f32 {
    if now.saturating_sub(start) >= dur_ns {
        return 0.0;
    }
    (1.0 - stepped_secs(start, now) / (dur_ns as f32 / NS_PER_S as f32)).max(0.0)
}

impl ChipFx {
    fn of(co: &ChipOverlay, now: u64, inset: bool) -> ChipFx {
        let mut fx = ChipFx {
            fill: co.fill.and_then(|f| match f {
                Fill::Progress(p) => Some(FillFx::UpTo {
                    depth: p,
                    weight: FILL_WEIGHT,
                }),
                Fill::Sweep { since_ns } => {
                    let period = SWEEP_PERIOD_NS as f32 / NS_PER_S as f32;
                    let phase = (stepped_secs(since_ns, now) / period).fract();
                    Some(FillFx::Band {
                        centre: phase * (1.0 + 2.0 * BAND_HALF) - BAND_HALF,
                        weight: FILL_WEIGHT * 0.8,
                    })
                }
                Fill::Settle { since_ns } => {
                    let left = left(since_ns, now, SETTLE_NS);
                    (left > 0.0).then_some(FillFx::UpTo {
                        depth: 1.0,
                        weight: FILL_WEIGHT * left,
                    })
                }
            }),
            pulse: co
                .pulses
                .iter()
                .map(|p| {
                    // Faded in whole levels: 1, then 1/2, then gone.
                    let left =
                        (left(p.start_ns, now, PULSE_NS) * PULSE_LEVELS).ceil() / PULSE_LEVELS;
                    (PULSE_WEIGHT * p.strength * left).min(0.95)
                })
                .fold(0.0, f32::max),
            ..ChipFx::default()
        };
        if inset {
            return fx;
        }
        for row in &co.gddr {
            let flash = row.flash_ns.map_or(0.0, |t| left(t, now, FLASH_NS));
            if let Some(slot) = fx.gddr.get_mut(row.channel as usize) {
                *slot = Some(GddrFx {
                    heat: row.heat,
                    dim: row.dim,
                    flash,
                });
            }
        }
        fx.sparks = co
            .sparks
            .iter()
            .filter_map(|s| {
                let left = left(s.start_ns, now, SPARK_NS);
                (left > 0.0).then_some((s.column, s.rgb, left))
            })
            .collect();
        fx
    }

    fn is_empty(&self) -> bool {
        self.fill.is_none()
            && self.pulse <= 0.0
            && self.gddr.iter().all(Option::is_none)
            && self.sparks.is_empty()
    }

    /// The colour of a Tensix tile in die column `die_x` once the fill is
    /// applied to `base`.
    pub fn tint(&self, die_x: f32, base: [u8; 3]) -> [u8; 3] {
        let mut c = base;
        if let Some(fill) = self.fill {
            let d = tile_depth(die_x);
            let w = match fill {
                FillFx::UpTo { depth, weight } if d <= depth + 1e-4 => weight,
                FillFx::Band { centre, weight } if (d - centre).abs() <= BAND_HALF => weight,
                _ => 0.0,
            };
            if w > 0.0 {
                c = lerp(c, LOAD_PURPLE, w);
            }
        }
        c
    }
}

/// Blackhole die columns: DRAM at 0 and 16, Tensix between.
const DIE_LAST_COL: f32 = 16.0;
/// Columns from a DRAM edge to the middle of the die.
const DIE_HALF_SPAN: f32 = 8.0;

/// How far die column `x` is from the nearer DRAM edge, as a share of the
/// way to the middle: 1/8 for the Tensix column next to the DRAM, 1.0 for
/// the middle column.
pub fn tile_depth(x: f32) -> f32 {
    (x.min(DIE_LAST_COL - x) / DIE_HALF_SPAN).clamp(0.0, 1.0)
}

/// Linear blend of two colours.
pub fn lerp(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = |i: usize| {
        (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [f(0), f(1), f(2)]
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
