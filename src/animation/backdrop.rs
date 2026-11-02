// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The shared backdrop: one signal-driven background that any full-screen
//! view can lay behind what it draws.
//!
//! There are three kinds, and each is one of the painters that already
//! existed, unchanged:
//!
//! * **scene** is the Inference view's atmosphere ([`super::atmosphere`]): a
//!   forge while a model compiles or loads, a storm during prefill, an
//!   aurora while tokens flow, a tide for the KV cache, comets for running
//!   requests and a flare for each finished one.
//! * **sky** is the Training view's aurora and stars ([`super::train_sky`]).
//!   Its speed follows the training step time; the stars and the aurora
//!   themselves are decoration.
//! * **clouds** is the Die 3D view's cloud field ([`super::die3d::clouds`]),
//!   sampled once per cell. The drift follows the PCIe traffic of the local
//!   chips and the brightness follows the busiest chip's load.
//!
//! Every kind is transparent: [`Backdrop::layer`] and
//! [`Backdrop::layer_buffer`] fill blank cells only, so whatever the view
//! draws always wins, and no background colour is painted.
//!
//! The two-step shape is the bus ports' (see `die3d::overlay`):
//! [`BackdropSignals::gather`] reads the world and the backend at the data
//! rate, and [`Backdrop::observe`] keeps the little each kind needs; drawing
//! is arithmetic over that state.
//!
//! Terminal traffic. The sky and the clouds move on the clouds' step clock
//! (4 steps a second, `die3d::clouds::CloudClock`), so between steps they
//! change no cell at all. The clouds also keep their three flat density bands
//! and their brightness hysteresis, and the sky's aurora is coloured in flat
//! bands too (the Training view's own sky is not). The scene keeps the atmosphere's own
//! clock (one frame per update), because its comets, rain and flares are the
//! signals themselves; stepping them four times a second would hide the
//! request rate it shows.
//!
//! Which view uses which kind is a setting ([`BackdropConfig`],
//! [`BackdropSettings`]): by default the Inference view draws the scene, the
//! Training view the sky and Die 3D the clouds, each with its own painter as
//! before, and every other view draws none.

use super::atmosphere::{self, Atmosphere, Canvas};
use super::die3d::clouds::{CloudClock, Clouds, CLOCK_STEPS_PER_SEC};
use super::train_bus::{PACE_MAX, PACE_MIN, PACE_REF_MS, RUN_STALE_MS};
use super::train_sky::{sky_cell, ANIM_FPS};
use crate::backend::TelemetryBackend;
use crate::workload::inference_server::ServiceState;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;
use std::cell::RefCell;
use tt_toplike_bus::{EntityKey, EntityKind, Provenance, WorldRead};

/// The three backdrops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackdropKind {
    /// The Inference view's atmosphere.
    Scene,
    /// The Training view's aurora and stars.
    Sky,
    /// The Die 3D view's clouds.
    Clouds,
}

/// A view's backdrop: a kind, or `None` for off.
pub type Choice = Option<BackdropKind>;

impl BackdropKind {
    /// Every kind, in the order the key cycles through them.
    pub const ALL: [BackdropKind; 3] =
        [BackdropKind::Scene, BackdropKind::Sky, BackdropKind::Clouds];

    /// The name a person types and reads.
    pub fn name(self) -> &'static str {
        match self {
            BackdropKind::Scene => "scene",
            BackdropKind::Sky => "sky",
            BackdropKind::Clouds => "clouds",
        }
    }

    /// The signals the kind follows, for the legend line.
    pub fn follows(self) -> &'static str {
        match self {
            BackdropKind::Scene => "model phase, tokens/s, KV cache and requests",
            BackdropKind::Sky => "training step time; its stars are decoration",
            BackdropKind::Clouds => "PCIe (drift) and chip load (light); decoration",
        }
    }
}

/// The name of a choice: a kind's name, or `off`.
pub fn choice_name(c: Choice) -> &'static str {
    c.map_or("off", BackdropKind::name)
}

/// The choice `text` names (`off`, `scene`, `sky` or `clouds`, any case,
/// surrounding space ignored). `None` when it names none of them.
pub fn parse_choice(text: &str) -> Option<Choice> {
    match text.trim().to_ascii_lowercase().as_str() {
        "off" => Some(None),
        "scene" => Some(Some(BackdropKind::Scene)),
        "sky" => Some(Some(BackdropKind::Sky)),
        "clouds" => Some(Some(BackdropKind::Clouds)),
        _ => None,
    }
}

/// The values a setting may take, for messages.
pub const CHOICES: &str = "off, scene, sky or clouds";

/// The views that can draw a backdrop, by the names the bus publishes them
/// under (`ViewChanged`), which are also the keys of `[backdrop.views]`.
pub const VIEWS: [&str; 11] = [
    "insights",
    "grid",
    "starfield",
    "castle",
    "flow",
    "arcade",
    "defrag",
    "inference",
    "hivemind",
    "training",
    "die3d",
];

/// The view `name` stands for: one of [`VIEWS`], or `waffler` for `die3d`
/// and `~` for `hivemind`. `None` for anything else.
pub fn canonical_view(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    match name.as_str() {
        "waffler" => Some("die3d"),
        "~" => Some("hivemind"),
        n => VIEWS.iter().copied().find(|v| *v == n),
    }
}

/// The name a person reads for `view` in a message: the view's config key,
/// except `die3d`, which is shown by its display name.
pub fn view_label(view: &str) -> &str {
    if view == "die3d" {
        crate::animation::DIE3D_DISPLAY_NAME
    } else {
        view
    }
}

/// The backdrop a view draws with its own painter when nothing is set:
/// the scene in the Inference view, the sky in Training, the clouds in Die
/// 3D, and none elsewhere.
pub fn builtin(view: &str) -> Choice {
    match view {
        "inference" => Some(BackdropKind::Scene),
        "training" => Some(BackdropKind::Sky),
        "die3d" => Some(BackdropKind::Clouds),
        _ => None,
    }
}

/// Why `view` refuses `kind`, or `None` when it takes it. The Starfield
/// refuses the scene and the sky: every star in it is a Tensix core, and
/// both of those draw stars that would look like cores.
pub fn refusal(view: &str, kind: BackdropKind) -> Option<&'static str> {
    match (view, kind) {
        ("starfield", BackdropKind::Scene | BackdropKind::Sky) => {
            Some("its stars are Tensix cores, and this backdrop draws stars too")
        }
        _ => None,
    }
}

/// The choice after `current` for `view`, in the order off, scene, sky,
/// clouds, skipping what the view refuses. The `w` key uses it.
pub fn next_choice(view: &str, current: Choice) -> Choice {
    let order: [Choice; 4] = [
        None,
        Some(BackdropKind::Scene),
        Some(BackdropKind::Sky),
        Some(BackdropKind::Clouds),
    ];
    let at = order.iter().position(|c| *c == current).unwrap_or(0);
    (1..=order.len())
        .map(|i| order[(at + i) % order.len()])
        .find(|c| c.is_none_or(|k| refusal(view, k).is_none()))
        .unwrap_or(None)
}

/// The key that cycles the backdrop of the view on screen.
pub const KEY: char = 'w';

/// The legend's line for a view's backdrop. `estimated` puts the `~` marker
/// in front of the signals, as Die 3D marks an estimated value.
pub fn legend_line(c: Choice, estimated: bool) -> String {
    match c {
        None => format!("Backdrop: off ({KEY} or /backdrop picks scene, sky, clouds)"),
        Some(k) => format!(
            "Backdrop: {} follows {}{}",
            k.name(),
            if estimated { "~" } else { "" },
            k.follows()
        ),
    }
}

// ---- Settings -------------------------------------------------------------

/// The `[backdrop]` config table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackdropConfig {
    /// `default`: the backdrop of every view that has no backdrop of its
    /// own. `None` when the file does not set it (off).
    pub default: Option<Choice>,
    /// `[backdrop.views]`: one choice per view, by canonical name, in file
    /// order.
    pub views: Vec<(&'static str, Choice)>,
}

impl BackdropConfig {
    /// The backdrop `view` starts with: its `[backdrop.views]` entry, else
    /// its own backdrop ([`builtin`]), else `default`, else off.
    pub fn choice(&self, view: &str) -> Choice {
        if let Some((_, c)) = self.views.iter().rev().find(|(v, _)| *v == view) {
            return *c;
        }
        builtin(view).or(self.default.flatten())
    }
}

/// The backdrop of each view while the TUI runs: the config's, changed by
/// `/backdrop` and the `w` key. A change holds until the TUI exits.
#[derive(Debug, Clone, Default)]
pub struct BackdropSettings {
    config: BackdropConfig,
    changed: Vec<(&'static str, Choice)>,
}

impl BackdropSettings {
    pub fn new(config: BackdropConfig) -> Self {
        Self {
            config,
            changed: Vec::new(),
        }
    }

    /// The backdrop `view` draws now.
    pub fn choice(&self, view: &str) -> Choice {
        match self.changed.iter().find(|(v, _)| *v == view) {
            Some((_, c)) => *c,
            None => self.config.choice(view),
        }
    }

    /// Set `view`'s backdrop. Refused, with the reason, when the view
    /// refuses the kind or is not a view.
    pub fn set(&mut self, view: &str, c: Choice) -> Result<(), String> {
        let Some(v) = canonical_view(view) else {
            return Err(format!("backdrop: no view named {view:?}"));
        };
        if let Some(why) = c.and_then(|k| refusal(v, k)) {
            return Err(format!("backdrop: {v} refuses {}: {why}", choice_name(c)));
        }
        self.changed.retain(|(x, _)| *x != v);
        self.changed.push((v, c));
        Ok(())
    }

    /// The config the TUI started from.
    pub fn config(&self) -> &BackdropConfig {
        &self.config
    }
}

/// Run the `/backdrop` command for `view`: no argument shows the setting,
/// `off`, `scene`, `sky` or `clouds` sets it. Returns the status-line
/// message and whether it is an error.
pub fn command(arg: &str, view: &str, settings: &mut BackdropSettings) -> (String, bool) {
    let arg = arg.trim();
    if arg.is_empty() {
        let c = settings.choice(view);
        return (
            format!(
                "backdrop ({}): {}. {}",
                view_label(view),
                choice_name(c),
                legend_line(c, false)
            ),
            false,
        );
    }
    match parse_choice(arg) {
        None => (format!("/backdrop: expected {CHOICES}, got {arg:?}"), true),
        Some(c) => match settings.set(view, c) {
            Ok(()) => (
                format!("backdrop ({}): {}", view_label(view), choice_name(c)),
                false,
            ),
            Err(e) => (e, true),
        },
    }
}

/// The usage line for `/help`.
pub const USAGE: &str = "/backdrop [off|scene|sky|clouds]";

// ---- Signals --------------------------------------------------------------

/// A reading older than this is not used: the source has gone quiet.
const STALE_MS: u64 = super::arcade_bus::TPS_STALE_MS;

/// What the backdrops follow, read once per update.
#[derive(Debug, Clone, Default)]
pub struct BackdropSignals {
    /// View time, in seconds. The sky and the clouds step on it.
    pub now_s: f64,
    /// The inference services, as the `i` view draws them (the local
    /// monitor's, or the attached host's). The scene follows these.
    pub services: Vec<ServiceState>,
    /// PCIe traffic of the local chips, in and out summed, bytes a second.
    /// `None` without a reading.
    pub pcie_bps: Option<f64>,
    /// Load of the busiest local chip, power over its TDP, 0 to 1. `None`
    /// without a reading.
    pub load: Option<f32>,
    /// The training step time, in milliseconds, of the newest run seen in
    /// the world. `None` without a fresh one.
    pub step_ms: Option<f32>,
    /// Whether any value above is an estimate (the mock backend). The
    /// legend line then carries the `~` marker.
    pub estimated: bool,
}

/// A reading's number when it is fresh at `now`.
fn fresh(
    world: &dyn WorldRead,
    e: &EntityKey,
    signal: &str,
    now: u64,
    max_ms: u64,
) -> Option<(f64, Provenance)> {
    let r = world.reading(e, signal)?;
    if world.age_ms(&r, now) > max_ms {
        return None;
    }
    super::bus_intake::number(&r.value)
        .filter(|v| v.is_finite())
        .map(|v| (v, r.prov))
}

impl BackdropSignals {
    /// Read the signals for this update.
    ///
    /// Chip signals come from the world (`world`, as `TuiBus::read` gives
    /// it) for the backend's own chips only, keyed by PCI address, so an
    /// attached host's other chips are never read. A chip with no fresh
    /// reading in the world falls back to the backend's own telemetry,
    /// which is all there is when `world` is `None`. `estimated_backend`
    /// marks the backend's values as estimates (the mock backend).
    pub fn gather(
        world: Option<&dyn WorldRead>,
        backend: &dyn TelemetryBackend,
        services: Vec<ServiceState>,
        now_s: f64,
        estimated_backend: bool,
    ) -> BackdropSignals {
        use crate::bus::signals as sig;
        let mut estimated = false;
        let mut pcie: Option<f64> = None;
        let mut load: Option<f32> = None;
        let mut add_pcie = |b: f64| pcie = Some(pcie.unwrap_or(0.0) + b.max(0.0));
        let now = world.map(|w| w.now_mono_ns()).unwrap_or(0);
        for d in backend.devices() {
            let local = super::die3d::scene::chip_inputs(backend, d);
            let key = (!d.bus_id.is_empty()).then(|| EntityKey::chip(&d.bus_id));
            let read = |s: &str| {
                world
                    .zip(key.as_ref())
                    .and_then(|(w, k)| fresh(w, k, s, now, STALE_MS))
            };
            // PCIe: the world's readings, else the backend's counters.
            let (pin, pout) = (read(sig::PCIE_IN_BPS), read(sig::PCIE_OUT_BPS));
            if pin.is_some() || pout.is_some() {
                for (v, prov) in [pin, pout].into_iter().flatten() {
                    estimated |= prov == Provenance::Estimated;
                    add_pcie(v);
                }
            } else {
                for v in [local.pcie_in_bps, local.pcie_out_bps]
                    .into_iter()
                    .flatten()
                {
                    if v.is_finite() {
                        estimated |= estimated_backend;
                        add_pcie(v);
                    }
                }
            }
            // Load: the world's power over the chip's TDP, else the
            // backend's (`chip_inputs` makes the same division).
            let tdp = super::die3d::scene::chip_tdp(backend, d);
            let l = match (read(sig::TDP_W), tdp) {
                (Some((p, prov)), Some(t)) => {
                    estimated |= prov == Provenance::Estimated;
                    Some((p as f32 / t).clamp(0.0, 1.0))
                }
                _ => local.power_w.map(|_| {
                    estimated |= estimated_backend;
                    local.load
                }),
            };
            if let Some(l) = l.filter(|l| !l.is_nan()) {
                load = Some(load.map_or(l, |m: f32| m.max(l)));
            }
        }
        // The step time of the newest fresh training run.
        let step_ms = world.and_then(|w| {
            w.entities(EntityKind::TrainingRun)
                .into_iter()
                .filter_map(|k| {
                    let r = w.reading(&k, sig::TRAIN_STEP_MS)?;
                    let (v, prov) = fresh(w, &k, sig::TRAIN_STEP_MS, now, RUN_STALE_MS)?;
                    (v > 0.0).then_some((r.seq, v as f32, prov))
                })
                .max_by_key(|(seq, _, _)| *seq)
        });
        if let Some((_, _, prov)) = step_ms {
            estimated |= prov == Provenance::Estimated;
        }
        // A model whose text is an estimate marks the scene's signals too.
        if let Some(w) = world {
            estimated |= w
                .models()
                .iter()
                .any(|m| super::die3d::overlay::model_text_is_estimated(w, m));
        }
        // So does a service the `i` view marks as one: an attached host's
        // estimated model (`~` before its label) or a mock service.
        estimated |= services.iter().any(|s| {
            s.label.starts_with('~')
                || crate::workload::inference_server::mock::is_mock_label(&s.label)
        });
        BackdropSignals {
            now_s,
            services,
            pcie_bps: pcie,
            load,
            step_ms: step_ms.map(|(_, v, _)| v),
            estimated,
        }
    }
}

// ---- The backdrop ---------------------------------------------------------

/// The sky's state: its own frame clock, which runs on the clouds' step
/// clock at the pace the training step time sets.
#[derive(Debug, Clone, Copy, Default)]
struct SkyClock {
    /// The last step reached; `None` before the first update.
    step: Option<i64>,
    /// Frames of sky motion so far (the unit `sky_cell` takes).
    frames: f64,
}

impl SkyClock {
    /// Move to view time `now` at `pace` times the usual speed. Each whole
    /// step adds a quarter second of frames; between steps nothing moves. A
    /// `now` before the last step restarts the clock there, as
    /// `CloudClock::advance` does.
    fn advance(&mut self, now: f64, pace: f32) {
        let steps = now * CLOCK_STEPS_PER_SEC;
        if !steps.is_finite() || steps.abs() >= i64::MAX as f64 / 2.0 {
            return;
        }
        let step = steps.floor() as i64;
        if let Some(last) = self.step {
            if step > last {
                let per_step = ANIM_FPS as f64 / CLOCK_STEPS_PER_SEC;
                self.frames += (step - last) as f64 * per_step * pace as f64;
            }
        }
        self.step = Some(step);
    }

    fn frame(&self) -> u64 {
        self.frames as u64
    }
}

/// The sky's speed for a training step time: [`PACE_REF_MS`] over the step
/// time, kept between [`PACE_MIN`] and [`PACE_MAX`], the Training view's
/// rule. 1 without a step time.
fn sky_pace(step_ms: Option<f32>) -> f32 {
    match step_ms {
        Some(ms) if ms.is_finite() && ms > 0.0 => (PACE_REF_MS / ms).clamp(PACE_MIN, PACE_MAX),
        _ => 1.0,
    }
}

/// Aurora brightness bands of the shared sky: a lit value is shown at the
/// middle of its band.
const AURORA_BANDS: f32 = 4.0;
/// Hue steps of the shared sky's aurora, in degrees.
const AURORA_HUE_STEP: f32 = 30.0;

/// The colour of an aurora cell of the shared sky: the Training view's
/// formula (`train_sky::sky_cell`) with the brightness in
/// [`AURORA_BANDS`] flat bands over its lit range (0.14 to 1) and the hue in
/// steps of [`AURORA_HUE_STEP`], so a cell changes colour only when its
/// value crosses a band.
fn aurora_colour(lit: f32, hue: f32) -> Color {
    let (lo, hi) = (0.14f32, 1.0f32);
    let band = (((lit - lo) / (hi - lo)) * AURORA_BANDS)
        .floor()
        .clamp(0.0, AURORA_BANDS - 1.0);
    let lit = lo + (band + 0.5) * (hi - lo) / AURORA_BANDS;
    let hue = (hue / AURORA_HUE_STEP).round() * AURORA_HUE_STEP;
    let (r, g, b) = super::common::hsv_to_rgb_bytes(hue.rem_euclid(360.0), 0.62, 0.10 + lit * 0.15);
    Color::Rgb(r, g, b)
}

/// The glyph of each cloud band, thin to thick.
const CLOUD_GLYPHS: [char; 3] = ['░', '▒', '▓'];

/// What a cached canvas was drawn from. The sky and the clouds change only
/// on a step, so their canvas is drawn once per step and size.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CacheKey {
    kind: BackdropKind,
    w: usize,
    h: usize,
    /// The sky frame, or the cloud offset and intensity, as bits.
    a: u64,
    b: u32,
    theme: crate::ui::colors::Theme,
}

/// One view's backdrop. See the module doc.
#[derive(Debug, Clone)]
pub struct Backdrop {
    kind: Choice,
    atmosphere: Atmosphere,
    clouds: Clouds,
    cloud_clock: CloudClock,
    sky: SkyClock,
    estimated: bool,
    cache: RefCell<Option<(CacheKey, Canvas)>>,
}

impl Backdrop {
    /// A backdrop of `kind` (`None`: off) with nothing observed yet.
    pub fn new(kind: Choice) -> Self {
        Self {
            kind,
            atmosphere: Atmosphere::new(),
            clouds: Clouds::new(super::die3d::CLOUD_SEED),
            cloud_clock: CloudClock::default(),
            sky: SkyClock::default(),
            estimated: false,
            cache: RefCell::new(None),
        }
    }

    /// The kind drawn, `None` when off.
    pub fn kind(&self) -> Choice {
        self.kind
    }

    /// Change the kind from the next frame. Every kind keeps observing while
    /// another is shown, so a switch shows it where it would be.
    pub fn set_kind(&mut self, kind: Choice) {
        self.kind = kind;
    }

    /// Whether anything is drawn: a kind is set and, for the scene, a
    /// service is featured.
    pub fn active(&self) -> bool {
        match self.kind {
            None => false,
            Some(BackdropKind::Scene) => self.atmosphere.active(),
            Some(BackdropKind::Sky | BackdropKind::Clouds) => true,
        }
    }

    /// Whether the last signals held an estimate.
    pub fn estimated(&self) -> bool {
        self.estimated
    }

    /// Forget the cached canvas, so the cost measurement draws it afresh.
    #[cfg(test)]
    pub(crate) fn clear_cache(&self) {
        *self.cache.borrow_mut() = None;
    }

    /// The legend line for this backdrop.
    pub fn legend_line(&self) -> String {
        legend_line(self.kind, self.estimated)
    }

    /// Take in this update's signals. Every kind is fed, whichever is shown.
    pub fn observe(&mut self, s: &BackdropSignals) {
        self.atmosphere.observe(&s.services);
        self.cloud_clock.advance(
            s.now_s,
            Clouds::drift_px_per_s(s.pcie_bps),
            Clouds::intensity(s.load.unwrap_or(0.0)),
        );
        self.sky.advance(s.now_s, sky_pace(s.step_ms));
        self.estimated = s.estimated;
    }

    /// The backdrop for `w` by `h` cells. Empty when nothing is drawn.
    pub fn canvas(&self, w: usize, h: usize) -> Canvas {
        self.with_canvas(w, h, |c| c.clone())
            .unwrap_or_else(|| vec![vec![None; w]; h])
    }

    /// Run `f` on the backdrop for `w` by `h` cells; `None` when nothing is
    /// drawn. The scene draws its canvas each time, as the Inference view
    /// always has. The sky and the clouds draw theirs once per step and size
    /// and lend the cached one, so a frame between steps allocates nothing.
    fn with_canvas<R>(&self, w: usize, h: usize, f: impl FnOnce(&Canvas) -> R) -> Option<R> {
        let kind = self.kind.filter(|_| self.active())?;
        if kind == BackdropKind::Scene {
            return Some(f(&self.atmosphere.render_backdrop(w, h)));
        }
        let key = CacheKey {
            kind,
            w,
            h,
            a: match kind {
                BackdropKind::Sky => self.sky.frame(),
                _ => self.cloud_clock.offset_px().to_bits(),
            },
            b: self.cloud_clock.intensity().to_bits(),
            theme: crate::ui::colors::current_theme(),
        };
        let mut cache = self.cache.borrow_mut();
        if cache.as_ref().is_none_or(|(k, _)| *k != key) {
            let canvas = match kind {
                BackdropKind::Sky => self.sky_canvas(w, h),
                _ => self.cloud_canvas(w, h, key.theme),
            };
            *cache = Some((key, canvas));
        }
        cache.as_ref().map(|(_, c)| f(c))
    }

    /// The sky over the whole area, as the Training view draws it while
    /// scanning: `rel_y` runs from 0 at the top to 1 at the bottom.
    ///
    /// The stars are the Training view's, cell for cell. The aurora cells
    /// are the same cells with the same glyphs, but their colour is taken in
    /// flat bands ([`aurora_colour`]): the Training view's aurora changes the
    /// colour of every aurora cell at every step, and here that would repaint
    /// a third of the screen four times a second.
    fn sky_canvas(&self, w: usize, h: usize) -> Canvas {
        let frame = self.sky.frame();
        (0..h)
            .map(|y| {
                let rel_y = y as f32 / h.max(1) as f32;
                (0..w)
                    .map(|x| {
                        sky_cell(x, y, rel_y, frame).map(|c| {
                            if super::train_sky::star_hash(x, y) > super::train_sky::STAR_THRESHOLD
                            {
                                (c.ch, c.color)
                            } else {
                                let (lit, hue) = super::train_sky::aurora(x, rel_y, frame);
                                (c.ch, aurora_colour(lit, hue))
                            }
                        })
                    })
                    .collect()
            })
            .collect()
    }

    /// The clouds, one band per cell. A cell stands for the 2 x 4 pixels a
    /// braille cell has in the Waffler, so the banks have the size they have
    /// there; it takes the band of its top-left pixel.
    fn cloud_canvas(&self, w: usize, h: usize, theme: crate::ui::colors::Theme) -> Canvas {
        let pal = super::die3d::palette::for_theme(theme);
        let colours = Clouds::band_colours(pal, self.cloud_clock.intensity());
        let offset = self.cloud_clock.offset_px();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| {
                        let band =
                            self.clouds
                                .band_at((2 * x) as f64 + 0.5, (4 * y) as f64 + 0.5, offset);
                        (band > 0).then(|| {
                            let [r, g, b] = colours[band as usize - 1];
                            (CLOUD_GLYPHS[band as usize - 1], Color::Rgb(r, g, b))
                        })
                    })
                    .collect()
            })
            .collect()
    }

    /// Lay the backdrop under `lines`, `width` by `height` cells, filling
    /// blank cells only (see [`atmosphere::composite`]). With nothing to
    /// draw the lines come back as they are.
    pub fn layer(
        &self,
        lines: Vec<Line<'static>>,
        width: usize,
        height: usize,
    ) -> Vec<Line<'static>> {
        if !self.active() {
            return lines;
        }
        // `active` was checked, so a canvas is lent; the fallback keeps the
        // lines as they are.
        let mut lines = Some(lines);
        self.with_canvas(width, height, |c| {
            atmosphere::composite(c, lines.take().unwrap_or_default(), width)
        })
        .or(lines)
        .unwrap_or_default()
    }

    /// Lay the backdrop into `area` of a drawn buffer, filling blank cells
    /// only: a space with no reverse, underline or strike-through, on the
    /// canvas background (the terminal's own, or black). A cell the view
    /// drew, or gave a colour of its own, is left alone. The glyph takes the
    /// backdrop's colour and the cell keeps its background.
    pub fn layer_buffer(&self, buf: &mut Buffer, area: Rect) {
        let area = area.intersection(buf.area);
        if !self.active() || area.is_empty() {
            return;
        }
        self.with_canvas(area.width as usize, area.height as usize, |canvas| {
            for (dy, row) in canvas.iter().enumerate() {
                for (dx, cell) in row.iter().enumerate() {
                    let Some((ch, colour)) = *cell else {
                        continue;
                    };
                    let c = &mut buf[(area.x + dx as u16, area.y + dy as u16)];
                    if is_blank(c) {
                        c.set_char(ch);
                        c.set_fg(colour);
                    }
                }
            }
        });
    }
}

/// Whether a drawn buffer cell is blank: see [`Backdrop::layer_buffer`].
pub fn is_blank(c: &ratatui::buffer::Cell) -> bool {
    let visible = Modifier::REVERSED | Modifier::UNDERLINED | Modifier::CROSSED_OUT;
    c.symbol() == " "
        && matches!(c.bg, Color::Reset | Color::Black | Color::Rgb(0, 0, 0))
        && !c.modifier.intersects(visible)
}

#[cfg(test)]
#[path = "backdrop_tests.rs"]
mod tests;
