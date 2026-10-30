// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Die 3D view: an isometric, pixel-rendered picture of the board and its
//! Blackhole dies.
//!
//! Each frame is drawn into an RGB pixel buffer (`raster`, `scene`,
//! `super_scene`) and converted to terminal cells as braille or half-block
//! (`cells`). The camera (`camera`) cuts to another angle when a chip becomes
//! busy or its PCIe traffic bursts (`triggers`), with a chaos, dissolve or
//! spin transition, and the scene can switch from every board to the busy
//! chip's board (`switching`). Clouds drift behind the dies (`clouds`), as
//! decoration whose drift follows PCIe traffic and whose brightness follows
//! chip load. A small panel at the top left (`history`) plots the last
//! minute of measured power, temperature and PCIe traffic. This file holds the
//! view that ties them together: it reads the backend in `update`, answers
//! keys in `handle_key` and draws in `render`.
//!
//! Time is `now`, seconds since the view was created, passed in by the caller
//! from one clock. Nothing here reads a clock, so the tests set time exactly.

pub mod bh_layout_data;
pub mod camera;
pub mod cells;
pub mod clouds;
mod flags;
pub mod history;
pub mod layout;
pub mod load;
pub mod palette;
#[cfg(test)]
mod perf_tests;
pub mod raster;
pub mod scene;
pub mod super_scene;
mod switching;
#[cfg(test)]
mod test_backend;
mod tiers;
pub mod triggers;

use crate::animation::topology::BoardTopology;
use crate::backend::TelemetryBackend;
use crate::models::Architecture;
use camera::{dissolve_into, scramble, Camera, Director, TransitionKind};
use clouds::{CloudClock, Clouds};
use flags::{DieFlag, Flag, Hull};
use history::History;
use load::LoadScale;
use palette::GRAYSKULL;
use raster::{Iso, PixelBuf};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use scene::{
    board_band_origin, board_grid, chip_inputs, die_centre, draw_board_scene, fit_pixels,
    has_per_tile_data, lift, ChipInputs, Detail, SceneFit, ESTIMATE_LABEL,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use super_scene::{band_origin, draw_super_scene, super_grid};
use switching::{Scene, Switcher};
use tiers::{tier_for, Tier};
use triggers::TriggerDetector;

/// Terminal rows the footer takes in the full layout (100 x 30 cells and
/// up). The view itself reads the footer's rows from its [`Tier`]; the tests
/// at those sizes use this.
#[cfg(test)]
const FOOTER_ROWS: usize = 2;
/// Seed of the camera director. Fixed, so a run with the same telemetry and
/// times cuts to the same angles; tests depend on that.
const CAMERA_SEED: u64 = 0x3D;
/// Seed of the cloud field. Fixed, so the sky is the same on every run.
const CLOUD_SEED: u64 = 0xC10D;
/// An RGB triple as a ratatui colour (`const`, so the footer colours can come
/// straight from the palette).
const fn rgb(c: [u8; 3]) -> Color {
    Color::Rgb(c[0], c[1], c[2])
}
/// Footer text colour: the palette's lavender.
const FOOTER_FG: Color = rgb(GRAYSKULL.eth);
/// Footer key hints, dimmer than the status line.
const KEYS_FG: Color = rgb(GRAYSKULL.mgmt);
/// The opaque background (`T` off): plain black. A tinted fill drew the eye
/// away from the scene; black lets the blocks stand out.
const OPAQUE_BG: [u8; 3] = [0, 0, 0];
/// The action keys the footer lists after the switches. `3` (leave) is
/// handled by the TUI, not here. Keys that flip a state are not listed here:
/// they come from `Die3dView::switches`.
const ACTION_KEYS: &str = "[ ] board \u{b7} c cut \u{b7} 3 leave";
/// Separator between footer items.
const SEP: &str = " \u{b7} ";
/// Characters the footer's left bar takes (`╚═ ` and `║ `).
const BAR_WIDTH: usize = 3;
/// Least view time between two frames `render` draws while no transition
/// runs: 20 frames a second. The scene's own motion (the heat wave, the
/// packets) reads as smooth at that rate, and every frame drawn changes
/// hundreds of truecolour cells that a slow terminal must repaint.
const STEADY_FRAME_SECS: f64 = 1.0 / 20.0;
/// The same during a cut's transition: 30 frames a second, so a spin or a
/// dissolve stays smooth.
const TRANSITION_FRAME_SECS: f64 = 1.0 / 30.0;
/// How early a frame may come. The TUI asks for a frame every 16 ms or a
/// little more, so a frame due after exactly 1/30 s (two of its ticks) or
/// 1/20 s (three) would, with a tick a fraction of a millisecond short,
/// slip to the tick after and run at 21 or 16 frames a second instead. With
/// this allowance a frame is drawn once the interval less 5 ms has passed.
const FRAME_EARLY_SECS: f64 = 0.005;

/// The period, in seconds of view time, at which the scenes' animation time
/// wraps (see `scene_with`): one day.
const SCENE_TIME_WRAP_S: f64 = 86_400.0;

/// The history panel is drawn only in a view at least this wide and tall
/// (columns, rows). Below it the panel would cover too much of the scene.
const PANEL_MIN_CELLS: (usize, usize) = (120, 36);
/// The panel's first row: one row below the top of the scene area.
const PANEL_TOP: usize = 1;
/// The panel's left margin, in cells.
const PANEL_LEFT: usize = 2;
/// The panel's text colour: the palette's `tile` indigo grey lightened until
/// it reads on both a black and a translucent purple background, and
/// dimmer than the white chip labels.
const PANEL_FG: [u8; 3] = [170, 160, 205];

/// One switch of the view, as the footer shows it: the key that flips it and
/// what it is set to now.
///
/// To add a switch: add an entry to `Die3dView::switches_at`, a `Die3dKey`
/// variant (with its `toggles` and `key_to_die3d` lines; `ALL` follows from
/// the variant list) and an arm in `handle_key`. The footer builds itself
/// from the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    /// The key that flips it.
    pub key: char,
    /// What it controls, written between the key and the state in the full
    /// footer (`C auto-cuts on`). Empty when the state word already says it
    /// (`B braille`, `T transparent`, `m all boards`).
    pub label: &'static str,
    /// The current setting, one short phrase. The footer never cuts it.
    pub state: &'static str,
}

/// Declares `Die3dKey` and, for the tests, `Die3dKey::ALL` from one list of
/// variants, so a variant cannot exist without being in `ALL` and so without
/// being visited by the tests that walk every key.
macro_rules! die3d_keys {
    ($($(#[$doc:meta])* $variant:ident,)+) => {
        /// What a key asks the Die 3D view to do.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Die3dKey {
            $($(#[$doc])* $variant,)+
        }

        impl Die3dKey {
            /// Every key, in declaration order. Generated with the enum.
            #[cfg(test)]
            const ALL: &'static [Die3dKey] = &[$(Die3dKey::$variant,)+];
        }
    };
}

die3d_keys! {
    /// `]`: the next board.
    NextBoard,
    /// `[`: the previous board.
    PrevBoard,
    /// `m`: between the board scene and the scene of every board.
    ToggleScene,
    /// `c`: cut the camera now.
    CutNow,
    /// `C`: automatic cuts on or off. With them off the camera cuts only on
    /// `c`, and the view also stops switching scenes on its own.
    ToggleAuto,
    /// `B`: braille or half-block cells.
    ToggleBraille,
    /// `T`: a transparent background, so the terminal's own shows through, or
    /// an opaque black one.
    ToggleTransparent,
    /// `X`: how cuts change the picture: a shuffle of the kinds, or always
    /// chaos, always a dissolve, or always a spin.
    CycleTransition,
    /// `L`: chip labels on the dies on or off.
    ToggleLabels,
    /// `W`: the clouds behind the die on or off.
    ToggleClouds,
    /// `H`: the history panel on or off.
    ToggleHistory,
}

impl Die3dKey {
    /// Whether the key flips a state the view keeps, and so needs an entry in
    /// `Die3dView::switches`. The match is exhaustive on purpose: a new
    /// variant does not compile until it is classified here.
    pub fn toggles(self) -> bool {
        match self {
            Die3dKey::ToggleScene
            | Die3dKey::ToggleAuto
            | Die3dKey::ToggleBraille
            | Die3dKey::ToggleTransparent
            | Die3dKey::CycleTransition
            | Die3dKey::ToggleLabels
            | Die3dKey::ToggleClouds
            | Die3dKey::ToggleHistory => true,
            Die3dKey::NextBoard | Die3dKey::PrevBoard | Die3dKey::CutNow => false,
        }
    }
}

/// The Die 3D key bound to a character, if any.
pub fn key_to_die3d(c: char) -> Option<Die3dKey> {
    match c {
        ']' => Some(Die3dKey::NextBoard),
        '[' => Some(Die3dKey::PrevBoard),
        'm' => Some(Die3dKey::ToggleScene),
        'c' => Some(Die3dKey::CutNow),
        'C' => Some(Die3dKey::ToggleAuto),
        'B' => Some(Die3dKey::ToggleBraille),
        'T' => Some(Die3dKey::ToggleTransparent),
        'X' => Some(Die3dKey::CycleTransition),
        'L' => Some(Die3dKey::ToggleLabels),
        'W' => Some(Die3dKey::ToggleClouds),
        'H' => Some(Die3dKey::ToggleHistory),
        _ => None,
    }
}

/// How a frame's projection comes from the director's cameras.
#[derive(Debug, Clone, Copy)]
enum Aim {
    /// One camera, as it is.
    At(Camera),
    /// Part way (0..1) through a spin from the first camera to the second,
    /// through `Camera::lerp_view`.
    Between(Camera, Camera, f32),
}

/// What a frame actually shows, which can differ from the chosen scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// Every board, in the super scene.
    All,
    /// One board, by index.
    Board(usize),
    /// The super scene was chosen but does not fit at this size, so this
    /// board's scene is drawn instead.
    Fallback(usize),
}

/// Half the size, in grid tiles, of a die's top face: its 17 x 12 tiles,
/// edge to edge. A flag's pin stands on a corner of it.
const DIE_HALF: (f32, f32) = (8.5, 6.0);

/// Where the dies of a drawn scene are: the projection it was drawn with
/// (spin included), the grid that projection is for, and each die's centre on
/// that grid with the label to put there. The labels are placed from this, so
/// they follow the picture exactly.
#[derive(Debug, Clone)]
struct Placement {
    iso: Iso,
    grid: (usize, usize),
    dies: Vec<((f32, f32), String)>,
}

/// The Die 3D view.
///
/// Keeps its pixel buffers, sizing results and last frame between frames
/// (see [`RenderCache`]). On a terminal resize the TUI calls
/// [`resize`](Die3dView::resize), which drops that cache and keeps the
/// rest of the view's state.
pub struct Die3dView {
    cols: usize,
    rows: usize,
    /// Terminal rows below the view's area that the host draws over (the
    /// TUI's status bar). The size hint that named the terminal size went
    /// when the view learnt to draw at any size (the size tiers measure the
    /// view's own area), so nothing reads this now; it is kept so the host's
    /// calls (`with_reserved_rows`, `resize`) stay as they are.
    #[allow(dead_code)]
    reserved_rows: usize,
    braille: bool,
    /// Leave the background unpainted so the terminal's own colour (and any
    /// translucency) shows through. Off paints it opaque black.
    transparent: bool,
    /// Draw each die's name and power over it (`L`).
    show_labels: bool,
    /// Draw the clouds behind the die (`W`).
    show_clouds: bool,
    /// The cloud field, from a fixed seed.
    clouds: Clouds,
    /// The clouds' drift integrated over view time, and the intensity they
    /// are shown at. Advanced by `update` and by `render` (hence the `Cell`:
    /// `render` takes `&self`).
    cloud_clock: Cell<CloudClock>,
    /// Draw the history panel (`H`).
    show_history: bool,
    /// The last minute of measured power, temperature and PCIe traffic,
    /// sampled by `update` once a second of view time.
    history: History,
    /// Drift speed from the last `update`'s PCIe readings, px/s.
    cloud_speed: f32,
    /// Cloud intensity from the last `update`'s chip loads.
    cloud_intensity: f32,
    director: Director,
    detector: TriggerDetector,
    switcher: Switcher,
    /// Label of each drawn board (boards with at least one Blackhole chip).
    labels: Vec<String>,
    /// The Blackhole chips of each drawn board, read in the last `update`.
    chips: Vec<Vec<ChipInputs>>,
    /// Devices the view does not draw (not Blackhole): index and architecture.
    undrawn: Vec<(usize, Architecture)>,
    /// Each drawn chip's power over the last ten minutes and its idle over
    /// the last day, by device index (a map: indices can be sparse). Its relative load replaces the
    /// power-over-limit load from `chip_inputs` once it has enough samples;
    /// see `load` for why.
    scales: HashMap<usize, LoadScale>,
    /// Counts changes `render` must show at once: every key, and every
    /// update that changes which boards, chips or other devices there are.
    /// A frame drawn under another revision is never reused.
    revision: u64,
    /// What `render` keeps from one frame to the next; see [`RenderCache`].
    cache: RefCell<RenderCache>,
}

/// What the view keeps between frames so that a frame costs less to make
/// and to send. [`Die3dView::resize`] replaces it with an empty one, so
/// nothing here outlives the size it was made for.
#[derive(Default)]
struct RenderCache {
    /// `fit_pixels` results by (pixel width, pixel height, grid).
    fits: HashMap<(i32, i32, (usize, usize)), Option<SceneFit>>,
    /// Pixel buffers kept for the next frame.
    spare: Vec<PixelBuf>,
    /// The last frame `render` drew, returned again until it is due for
    /// another (see `render`).
    last: Option<LastFrame>,
}

/// A frame `render` drew, with the view time it was drawn for and what it
/// was drawn from.
struct LastFrame {
    at: f64,
    key: FrameKey,
    lines: Vec<Line<'static>>,
}

/// What a frame was drawn from besides the time: the view's revision, the
/// start of the latest cut, whether its transition is running, the scene
/// shown, the clouds' whole-pixel offset and intensity (`cloud_key`) and the
/// history panel's sample count while the panel is shown (`history_key`). A
/// frame is reused only while all six are unchanged, so a key, a cut
/// starting or ending, a scene switch, a cloud step or a new sample on the
/// panel shows in the very next frame. A cloud clock step that leaves the
/// offset and intensity as they were forces nothing, and neither does a
/// sample taken while the panel is hidden.
type FrameKey = (
    u64,
    Option<f64>,
    bool,
    Scene,
    Option<(i64, u32)>,
    Option<u64>,
);

impl Die3dView {
    /// A view for a terminal of `cols` x `rows` cells, in braille, showing
    /// every board, with automatic cuts, chip labels, clouds and the history
    /// panel on. It shows nothing until `update`.
    pub fn new(cols: usize, rows: usize) -> Die3dView {
        Die3dView::with_reserved_rows(cols, rows, 0)
    }

    /// Like `new`, for a view whose `rows` are the terminal's minus
    /// `reserved_rows` that the host draws elsewhere. The size tiers (see
    /// `tiers`) are measured on the view's own `cols` x `rows`.
    pub fn with_reserved_rows(cols: usize, rows: usize, reserved_rows: usize) -> Die3dView {
        Die3dView {
            cols,
            rows,
            reserved_rows,
            braille: true,
            transparent: true,
            show_labels: true,
            show_clouds: true,
            show_history: true,
            history: History::new(),
            clouds: Clouds::new(CLOUD_SEED),
            cloud_clock: Cell::new(CloudClock::default()),
            cloud_speed: Clouds::drift_px_per_s(None),
            cloud_intensity: Clouds::intensity(0.0),
            director: Director::new(CAMERA_SEED),
            detector: TriggerDetector::new(),
            switcher: Switcher::new(),
            labels: Vec::new(),
            chips: Vec::new(),
            undrawn: Vec::new(),
            scales: HashMap::new(),
            revision: 0,
            cache: RefCell::new(RenderCache::default()),
        }
    }

    /// The terminal changed size: draw at `cols` x `rows` from now on, with
    /// `reserved_rows` as in [`with_reserved_rows`](Self::with_reserved_rows).
    ///
    /// Only what depends on the size is dropped: the [`RenderCache`] (scene
    /// fits, spare pixel buffers and the last frame). Everything else stays:
    /// every switch, the scene on show and a switch still pending, the
    /// history, each chip's load calibration, the cloud clock, the camera
    /// director and the trigger detector. The revision moves, so the next
    /// `render` draws a fresh frame at the new size. A small size needs
    /// nothing special: `render` picks the layout for the size each frame.
    pub fn resize(&mut self, cols: usize, rows: usize, reserved_rows: usize) {
        self.cols = cols;
        self.rows = rows;
        self.reserved_rows = reserved_rows;
        *self.cache.get_mut() = RenderCache::default();
        self.revision += 1;
    }

    /// Read every chip from the backend, group the chips into boards and feed
    /// the trigger detector. A trigger may cut the camera and, in the super
    /// scene, switch to the busy chip's board (see `switching`).
    ///
    /// Only Blackhole chips are drawn, so only they can trigger a cut; other
    /// devices are listed in the footer instead.
    pub fn update(&mut self, backend: &dyn TelemetryBackend, now: f64) {
        // Integrate the drift up to now at the speed read last time, before
        // this update's readings replace it.
        self.advance_clouds(now);
        let before = self.layout_signature();
        let devices = backend.devices();
        let topology = BoardTopology::from_devices(devices);
        self.undrawn = devices
            .iter()
            .filter(|d| d.architecture != Architecture::Blackhole)
            .map(|d| (d.index, d.architecture))
            .collect();
        self.labels.clear();
        self.chips.clear();
        for board in &topology.boards {
            let chips: Vec<ChipInputs> = board
                .chips
                .iter()
                .filter_map(|&i| devices.iter().find(|d| d.index == i))
                .filter(|d| d.architecture == Architecture::Blackhole)
                .map(|d| chip_inputs(backend, d))
                .collect();
            if !chips.is_empty() {
                self.labels.push(board.label.clone());
                self.chips.push(chips);
            }
        }
        self.calibrate_loads(now);
        self.read_cloud_drivers();
        // One sample a second for the history panel, taken whether or not the
        // panel is shown, so turning it on shows the last minute at once.
        // The chips are copied only when a sample is due.
        if self.history.due(now) {
            let chips: Vec<ChipInputs> = self.chips.iter().flatten().cloned().collect();
            self.history.sample(now, &chips);
        }
        // A board, chip or device that came or went shows in the next frame.
        // Readings that only change (power, PCIe rates) wait for the next
        // frame that is due anyway, except on the history panel, whose new
        // sample `render` shows at once (`history_key`).
        if self.layout_signature() != before {
            self.revision += 1;
        }

        let boards = self.chips.len();
        // Commit a switch whose midpoint has passed before judging new
        // triggers, so they are judged against the scene on screen.
        self.switcher.settle(now, boards);
        for (board, chips) in self.chips.iter().enumerate() {
            for chip in chips {
                let pcie = chip
                    .pcie_in_bps
                    .zip(chip.pcie_out_bps)
                    .map(|(rx, tx)| rx + tx);
                for trigger in self.detector.observe(chip.index, chip.load, pcie) {
                    self.switcher
                        .on_trigger(trigger, board, boards, &mut self.director, now);
                }
            }
        }
        self.switcher.expire(now);
    }

    /// What is drawn where: the board labels, the device index of every
    /// drawn chip by board, and the devices not drawn.
    fn layout_signature(&self) -> (Vec<String>, Vec<Vec<usize>>, Vec<(usize, Architecture)>) {
        (
            self.labels.clone(),
            self.chips
                .iter()
                .map(|b| b.iter().map(|c| c.index).collect())
                .collect(),
            self.undrawn.clone(),
        )
    }

    /// Replace each chip's power-over-limit load with its load relative to
    /// its own idle (`load::LoadScale`), once that chip's scale has enough
    /// samples. Runs before the trigger detector and the scene read `load`,
    /// so both see the relative value.
    ///
    /// The relative value is used only on a tick where the chip has a power
    /// reading (`chip_inputs` already turns NaN and infinity into none).
    /// Without one, `LoadScale::observe` returns its last result, a load
    /// measured when the chip still had a reading; showing it would keep a
    /// chip that was busy glowing, brighten the clouds and hold the triggers
    /// high with nothing read from the hardware. Such a chip keeps the
    /// fallback from `chip_inputs`, which is 0.0 without a power reading,
    /// and its scale keeps its window for when the reading comes back.
    /// Scales of chips no longer present are dropped.
    fn calibrate_loads(&mut self, now: f64) {
        let present: Vec<usize> = self.chips.iter().flatten().map(|c| c.index).collect();
        self.scales.retain(|i, _| present.contains(i));
        for chip in self.chips.iter_mut().flatten() {
            let scale = self.scales.entry(chip.index).or_default();
            let relative = scale.observe(now, chip.power_w);
            if scale.ready() && chip.power_w.is_some() {
                chip.load = relative;
            }
        }
    }

    /// Set the clouds' drift speed from the total PCIe traffic of every
    /// chip drawn (both directions; `None` when no chip has a reading) and
    /// their intensity from the busiest chip's load, relative to its idle
    /// as `calibrate_loads` left it. The busiest chip sets it, so one
    /// busy chip among eight still shows.
    fn read_cloud_drivers(&mut self) {
        let chips = self.chips.iter().flatten();
        let pcie = chips
            .clone()
            .flat_map(|c| [c.pcie_in_bps, c.pcie_out_bps])
            .flatten()
            .filter(|b| b.is_finite())
            .fold(None, |sum: Option<f64>, b| Some(sum.unwrap_or(0.0) + b));
        let load = chips
            .map(|c| c.load)
            .filter(|l| !l.is_nan())
            .fold(0.0, f32::max);
        self.cloud_speed = Clouds::drift_px_per_s(pcie);
        self.cloud_intensity = Clouds::intensity(load);
    }

    /// Move the cloud clock to `now` at the speed and intensity of the last
    /// update. Runs whether or not the clouds are shown, so turning them on
    /// shows them where they would be.
    fn advance_clouds(&self, now: f64) {
        let mut clock = self.cloud_clock.get();
        clock.advance(now, self.cloud_speed, self.cloud_intensity);
        self.cloud_clock.set(clock);
    }

    /// What makes one frame's clouds differ from another's: the whole-pixel
    /// offset and the intensity shown, or nothing while they are not drawn
    /// (switched off, or the view too small for them).
    fn cloud_key(&self) -> Option<(i64, u32)> {
        let clock = self.cloud_clock.get();
        self.clouds_shown()
            .then(|| (clock.offset_px() as i64, clock.intensity().to_bits()))
    }

    /// What makes one frame's history panel differ from another's: the
    /// samples taken, or nothing while the panel is not shown.
    fn history_key(&self) -> Option<u64> {
        self.panel_shown().then(|| self.history.revision())
    }

    /// Act on a key. A scene chosen here (`[`, `]`, `m`) is the user's: the
    /// view will not switch back from it on its own.
    pub fn handle_key(&mut self, key: Die3dKey, now: f64) {
        // Whatever the key does shows in the next frame.
        self.revision += 1;
        let boards = self.chips.len();
        match key {
            Die3dKey::NextBoard | Die3dKey::PrevBoard => {
                if boards == 0 {
                    return;
                }
                let current = self.switcher.board_at(now).min(boards - 1);
                // From the super scene the first press opens the board shown
                // last; after that the keys step through the boards.
                let next = match (self.switcher.scene_at(now), key) {
                    (Scene::All, _) => current,
                    (_, Die3dKey::NextBoard) => (current + 1) % boards,
                    _ => (current + boards - 1) % boards,
                };
                self.switcher.choose(Scene::Board(next));
            }
            Die3dKey::ToggleScene => {
                let next = match self.switcher.scene_at(now) {
                    Scene::All => Scene::Board(self.switcher.board_at(now)),
                    Scene::Board(_) => Scene::All,
                };
                self.switcher.choose(next);
            }
            Die3dKey::CutNow => {
                // A scene switch still waiting for the midpoint of the cut in
                // progress would land part way through this new cut's
                // transition. This cut takes it over: it is told the scene
                // changes (so it never spins) and the switch moves to its
                // swap.
                let switching = self.switcher.pending_after(now);
                self.director.cut_by_hand_with_scene(now, switching);
                if switching {
                    let swap = self.director.swapped_at().unwrap_or(now);
                    self.switcher.hand_pending_to(now, swap);
                }
            }
            Die3dKey::ToggleAuto => self.director.set_auto(!self.director.auto()),
            Die3dKey::ToggleBraille => self.braille = !self.braille,
            Die3dKey::ToggleTransparent => self.transparent = !self.transparent,
            Die3dKey::CycleTransition => self.director.cycle_transition_mode(),
            Die3dKey::ToggleLabels => self.show_labels = !self.show_labels,
            Die3dKey::ToggleClouds => self.show_clouds = !self.show_clouds,
            Die3dKey::ToggleHistory => self.show_history = !self.show_history,
        }
    }

    /// The view at `now`: the scene, then the footer.
    ///
    /// What is drawn depends on the size (see `tiers`). At 100 x 30 cells and
    /// up: the full layout, with a two-row footer. Below that the clouds go
    /// and the footer is the status line alone; below 60 x 16 the flags and
    /// the footer go too. Where no scene fits, a hint line takes its place.
    /// With no Blackhole chip it is a hint line plus, when there are other
    /// devices, a line naming them.
    ///
    /// About 20 frames a second of view time are drawn, 30 during a cut's
    /// transition ([`STEADY_FRAME_SECS`], [`TRANSITION_FRAME_SECS`], each
    /// less [`FRAME_EARLY_SECS`]): called again sooner, `render` returns the
    /// frame it drew last, unchanged, so the terminal has nothing to repaint.
    /// The TUI calls it about 60 times a second. A key, a cut starting or
    /// ending, a scene switch or a change in the boards drawn always gets a
    /// fresh frame, and so does a `now` earlier than the last frame's.
    pub fn render(&self, now: f64) -> Vec<Line<'static>> {
        self.advance_clouds(now);
        let in_transition = self.director.progress(now).is_some();
        let key = (
            self.revision,
            self.director.cut_at(),
            in_transition,
            self.switcher.scene_at(now),
            self.cloud_key(),
            self.history_key(),
        );
        let interval = if in_transition {
            TRANSITION_FRAME_SECS
        } else {
            STEADY_FRAME_SECS
        };
        if let Some(last) = &self.cache.borrow().last {
            if last.key == key && now >= last.at && now - last.at < interval - FRAME_EARLY_SECS {
                return last.lines.clone();
            }
        }
        let lines = self.draw(now);
        self.cache.borrow_mut().last = Some(LastFrame {
            at: now,
            key,
            lines: lines.clone(),
        });
        lines
    }

    /// A fresh frame at `now`, as [`render`](Self::render) describes it.
    fn draw(&self, now: f64) -> Vec<Line<'static>> {
        if self.chips.is_empty() {
            if self.undrawn.is_empty() {
                return vec![hint("Die 3D: no Tenstorrent device found.".into())];
            }
            return vec![
                hint("Die 3D draws Blackhole chips only, and none was found.".into()),
                note(self.undrawn_note()),
            ];
        }
        let tier = self.tier();
        // The tier has no footer rows below 16 rows, so this never goes
        // below zero; the saturating subtraction guards it anyway.
        let scene_rows = self.rows.saturating_sub(tier.footer_rows);
        let (mut lines, frame) = match self.frame(now) {
            Some((buf, shown, placed)) => {
                let lines = if self.braille {
                    cells::to_braille_lines(&buf)
                } else {
                    cells::to_halfblock_lines(&buf)
                };
                self.give_back(buf);
                (lines, Some((shown, placed)))
            }
            None => (vec![hint(no_fit_hint(self.braille).into())], None),
        };
        lines.resize(scene_rows, Line::default());
        lines.extend(self.footer(now, frame.as_ref().map(|f| f.0)));
        // The history panel and the labels go over the converted cells, the
        // labels placed with the projection the frame was drawn with. While
        // a transition runs the picture is noise, a mix of two frames or
        // turning, so no label could stay on its die: they wait for it to
        // end. The panel stays. Below the tier's flag size there are no
        // labels whatever `L` says.
        if let Some((_, placed)) = &frame {
            let labels = self.show_labels && tier.flags;
            let anchors = match placed {
                Some(placed) if labels && self.director.progress(now).is_none() => {
                    self.flag_layout(placed, scene_rows)
                }
                _ => Vec::new(),
            };
            self.overlay(&mut lines, scene_rows, &anchors);
        }
        lines
    }

    /// Write the text that goes over a drawn scene: the history panel, then
    /// the chip labels at `anchors`. The labels go last so that where one
    /// lands on the panel, the label is what shows: it names the die under
    /// it, and the panel's rows are still readable around it.
    ///
    /// The panel does not depend on the picture, so it is drawn during a
    /// transition too, when `anchors` is empty.
    fn overlay(&self, lines: &mut [Line<'static>], scene_rows: usize, anchors: &[Option<Flag>]) {
        if self.panel_shown() {
            draw_panel(
                lines,
                scene_rows,
                self.cols,
                &self.history.panel_rows(),
                self.panel_style(),
            );
        }
        draw_flags(
            lines,
            scene_rows,
            self.cols,
            anchors,
            self.label_style(),
            self.pole_style(),
        );
    }

    /// Whether the history panel is drawn: switched on (`H`) and the view at
    /// least [`PANEL_MIN_CELLS`].
    fn panel_shown(&self) -> bool {
        self.show_history && self.cols >= PANEL_MIN_CELLS.0 && self.rows >= PANEL_MIN_CELLS.1
    }

    /// The panel's text style: [`PANEL_FG`], not bold, on the same background
    /// as the labels (the terminal's own in transparent mode, black
    /// otherwise).
    fn panel_style(&self) -> Style {
        Style::default().fg(rgb(PANEL_FG)).bg(self.background())
    }

    /// What the view draws at its current size: the footer's rows, and
    /// whether the clouds and the flags are drawn (see [`tiers`]).
    ///
    /// The footer, the clouds and the flags read the size through here (the
    /// history panel keeps its own gate, `panel_shown`), so a layout that
    /// overrides the size tiers changes only this method.
    fn tier(&self) -> Tier {
        tier_for(self.cols, self.rows)
    }

    /// Whether the cloud layer is drawn: switched on (`W`) and the view large
    /// enough for its tier to keep clouds.
    fn clouds_shown(&self) -> bool {
        self.show_clouds && self.tier().clouds
    }

    /// The picture at `now`: the scene with the transition of a cut in
    /// progress applied.
    ///
    /// - Chaos: the scene at the director's camera, scrambled.
    /// - Dissolve: the old camera's picture (and, for a cut that switched
    ///   scenes, the old scene) and the new one, both drawn at `now` so the
    ///   tiles keep moving, mixed by `dissolve`. The footer names the old
    ///   scene until the swap at the midpoint, the new one after it.
    /// - Spin: the scene drawn from the angle part way between the two
    ///   cameras. The scene and the footer do not change.
    ///
    /// The placement is that of the one scene drawn; a dissolve mixes two and
    /// has none.
    fn frame(&self, now: f64) -> Option<(PixelBuf, Shown, Option<Placement>)> {
        let progress = self.director.progress(now);
        match (self.director.transition_kind(), progress) {
            (Some(TransitionKind::Dissolve), Some(p)) => {
                let (from, to) = self.director.cameras();
                let (before, after) = self.transition_scenes(now);
                let (old, old_shown, _) = self.scene_with(now, before, Aim::At(from))?;
                let (new, new_shown, _) = self.scene_with(now, after, Aim::At(to))?;
                let swapped = self.director.swapped_at().is_some_and(|at| now >= at);
                let shown = if swapped { new_shown } else { old_shown };
                // One pixel order for the whole dissolve, from the cut's start.
                let seed = self.director.cut_at().map_or(0, |t| (t * 1000.0) as u32);
                // Mixed into `old` in place; `new` goes back for reuse.
                let mut mixed = old;
                dissolve_into(&mut mixed, &new, p, seed);
                self.give_back(new);
                Some((mixed, shown, None))
            }
            (Some(TransitionKind::Spin), Some(p)) => {
                let (from, to) = self.director.cameras();
                let scene = self.switcher.scene_at(now);
                let (buf, shown, placed) =
                    self.scene_with(now, scene, Aim::Between(from, to, p))?;
                Some((buf, shown, Some(placed)))
            }
            _ => {
                let (mut buf, shown, placed) = self.scene_now(now)?;
                if let Some(intensity) = self.director.chaos(now) {
                    // A new noise pattern 60 times a second, the same for a
                    // given now.
                    scramble(&mut buf, intensity, (now * 60.0) as u32);
                }
                Some((buf, shown, Some(placed)))
            }
        }
    }

    /// The scenes before and after the latest cut (see
    /// `Switcher::transition_scenes`); both the scene at `now` before any cut.
    fn transition_scenes(&self, now: f64) -> (Scene, Scene) {
        match self.director.cut_at() {
            Some(at) => self.switcher.transition_scenes(at, now),
            None => (self.switcher.scene_at(now), self.switcher.scene_at(now)),
        }
    }

    /// The scene at `now`, before any transition effect, in a buffer the size
    /// of the terminal area above the footer. `None` with no chip to draw or
    /// when even the board scene does not fit.
    ///
    /// The camera is `Director::camera_at(now)`: during a chaos or dissolve
    /// cut the old angle stays until the midpoint and the new one shows from
    /// there; during a spin the old angle stays until the end.
    fn scene_now(&self, now: f64) -> Option<(PixelBuf, Shown, Placement)> {
        let scene = self.switcher.scene_at(now);
        self.scene_with(now, scene, Aim::At(self.director.camera_at(now)))
    }

    /// [`scene_now`](Self::scene_now) without the placement: the bare scene
    /// the tests compare frames against.
    #[cfg(test)]
    fn scene_pixels(&self, now: f64) -> Option<(PixelBuf, Shown)> {
        let (buf, shown, _) = self.scene_now(now)?;
        Some((buf, shown))
    }

    /// `scene` at `now`, projected as `aim` says, and where its dies landed.
    /// The super scene falls back to the board shown last when it does not
    /// fit. A board index past the end of the board list (it can shrink
    /// between updates) is clamped.
    fn scene_with(
        &self,
        now: f64,
        scene: Scene,
        aim_by: Aim,
    ) -> Option<(PixelBuf, Shown, Placement)> {
        let scene_rows = self.rows.checked_sub(self.tier().footer_rows)?;
        // Braille cells hold 2 x 4 pixels, half-block cells 1 x 2.
        let (pw, ph) = if self.braille {
            (self.cols * 2, scene_rows * 4)
        } else {
            (self.cols, scene_rows * 2)
        };
        // The scenes animate in f32 time (the wave, the packets). f32 has 24
        // bits of mantissa, so past about a month of view time (2.6e6 s) it
        // steps by 0.25 s and the motion would stutter at 4 Hz. The time
        // wraps once a day in f64 first: identical for the first day, full
        // resolution after any uptime. A day is a whole number of the
        // wave's 1/12 s steps; the wave front, the shimmer and the packets
        // have periods that do not divide it, so they jump once a day, at
        // the wrap.
        let t = now.rem_euclid(SCENE_TIME_WRAP_S) as f32;
        // The scenes paint only what stands in them, so the buffer starts
        // clear: all uncovered for transparent, filled black for opaque.
        let mut buf = self.take_buf(pw, ph);
        if self.transparent {
            buf.clear_transparent();
        } else {
            buf.fill_solid(OPAQUE_BG);
        }
        // The clouds go first, so the slab and the dies are drawn over them
        // and are never covered. Clear sky is left as it is: uncovered in
        // transparent mode, black in opaque.
        if self.clouds_shown() {
            let clock = self.cloud_clock.get();
            self.clouds
                .draw(&mut buf, clock.offset_px(), clock.intensity());
        }
        let mut fallback = false;
        // With one board the super scene is that board's scene.
        if scene == Scene::All && self.chips.len() > 1 {
            let dies: Vec<usize> = self.chips.iter().map(Vec::len).collect();
            let (grid, _) = super_grid(&dies);
            match self.fit_for(pw, ph, grid) {
                Some(fit) => {
                    let (_, row_rows) = super_grid(&dies);
                    let fit = aim(&fit, aim_by, grid);
                    draw_super_scene(&mut buf, &self.chips, &fit, t);
                    let placed = Placement {
                        iso: fit.iso,
                        grid,
                        dies: self
                            .chips
                            .iter()
                            .enumerate()
                            .flat_map(|(b, chips)| {
                                chips.iter().enumerate().map(move |(d, chip)| {
                                    (die_centre(band_origin(b, d, row_rows)), chip_label(chip))
                                })
                            })
                            .collect(),
                    };
                    return Some((buf, Shown::All, placed));
                }
                None => fallback = true,
            }
        }
        let board = match scene {
            Scene::Board(b) => b,
            Scene::All => self.switcher.board_at(now),
        }
        .min(self.chips.len().checked_sub(1)?);
        let chips = &self.chips[board];
        let grid = board_grid(chips.len());
        let fit = aim(&self.fit_for(pw, ph, grid)?, aim_by, grid);
        draw_board_scene(&mut buf, chips, &fit, t, Detail::Full);
        let shown = if fallback {
            Shown::Fallback(board)
        } else {
            Shown::Board(board)
        };
        let placed = Placement {
            iso: fit.iso,
            grid,
            dies: chips
                .iter()
                .enumerate()
                .map(|(d, chip)| (die_centre(board_band_origin(d)), chip_label(chip)))
                .collect(),
        };
        Some((buf, shown, placed))
    }

    /// A `pw` x `ph` pixel buffer for a scene: one kept from an earlier frame
    /// when there is one of that size, else a new one. Its contents are
    /// whatever was drawn last; the caller clears or fills it. Reuse saves
    /// allocating (and the kernel zeroing) 190 kB or more each frame.
    fn take_buf(&self, pw: usize, ph: usize) -> PixelBuf {
        let mut cache = self.cache.borrow_mut();
        match cache.spare.pop() {
            Some(buf) if buf.w == pw && buf.h == ph => buf,
            // Another size (`B` changes the pixel grid): let it go.
            _ => PixelBuf::new(pw, ph),
        }
    }

    /// Keep `buf` for a later frame. A dissolve uses two buffers at once and
    /// every other frame one, so two are kept at most.
    fn give_back(&self, buf: PixelBuf) {
        let mut cache = self.cache.borrow_mut();
        if cache.spare.len() < 2 {
            cache.spare.push(buf);
        }
    }

    /// [`fit_pixels`] for a `pw` x `ph` pixel buffer and `grid`, worked out
    /// once per view and key. It samples 72 camera angles for each candidate
    /// tile width, which cost about a tenth of a millisecond a frame when it
    /// ran every frame. The result depends only on the key, so the cached
    /// value is the one a fresh call returns.
    fn fit_for(&self, pw: usize, ph: usize, grid: (usize, usize)) -> Option<SceneFit> {
        let key = (pw as i32, ph as i32, grid);
        *self
            .cache
            .borrow_mut()
            .fits
            .entry(key)
            .or_insert_with(|| fit_pixels(key.0, key.1, grid))
    }

    /// The footer, one line per footer row of the size tier: with two rows,
    /// what is shown and how, then the keys; with one, the first of those
    /// (the status line) alone; with none, nothing. A left bar and a bottom
    /// bar only, no right-side border.
    fn footer(&self, now: f64, shown: Option<Shown>) -> Vec<Line<'static>> {
        let rows = self.tier().footer_rows;
        if rows == 0 {
            return Vec::new();
        }
        let boards = self.chips.len();
        let board_name = |b: usize| {
            let label = self.labels.get(b).map_or("?", String::as_str);
            format!("board {}/{} {}", b + 1, boards, label)
        };
        let scene = match shown {
            Some(Shown::All) => "all boards".to_string(),
            Some(Shown::Board(b)) => board_name(b),
            Some(Shown::Fallback(b)) => format!(
                "{} (all {} boards do not fit at this size)",
                board_name(b),
                boards
            ),
            // Nothing drawn: name the scene that was chosen.
            None => match self.switcher.scene_at(now) {
                Scene::All if boards > 1 => "all boards".to_string(),
                _ => board_name(self.switcher.board_at(now).min(boards.saturating_sub(1))),
            },
        };
        let mut status = vec![scene];
        // Per-tile data on any chip of any board lifts the estimate label.
        if !self.chips.iter().any(|board| has_per_tile_data(board)) {
            status.push(ESTIMATE_LABEL.to_string());
        }
        let width = self.cols.saturating_sub(BAR_WIDTH);
        // Priority on a narrow row: the scene name, the estimate label, the
        // camera caption, then the note on devices not drawn. The caption
        // goes in whole only when it fits after the first two, so it never
        // pushes the estimate label into the cut. The note comes last and is
        // the part `fit_width` shortens: seeing which side the camera looks
        // from during a cut matters more than the full list of chips the
        // view cannot draw.
        let mut status = status.join(SEP);
        let with_caption = format!("{status}{SEP}{}", self.caption(now));
        if with_caption.chars().count() <= width {
            status = with_caption;
        }
        if !self.undrawn.is_empty() {
            // The note goes in whole, or cut after at least one of its own
            // characters (separator, one character, the ellipsis), or not at
            // all. Cut any shorter, `fit_width` would leave a bare ` ·…` or
            // eat into the caption before it.
            let room = width.saturating_sub(status.chars().count());
            let note = self.undrawn_note();
            if room >= SEP.chars().count() + note.chars().count().min(2) {
                status = format!("{status}{SEP}{note}");
            }
        }
        let mut lines = vec![Line::from(Span::styled(
            format!("\u{2551} {}", fit_width(status, width)),
            Style::default().fg(FOOTER_FG),
        ))];
        if rows >= 2 {
            lines.push(Line::from(Span::styled(
                format!("\u{255a}\u{2550} {}", self.switch_strip(now, width)),
                Style::default().fg(KEYS_FG),
            )));
        }
        lines
    }

    /// The footer's camera caption: `view: <corner>`, naming the board corner
    /// nearest the viewer (see `Camera::side_name`), or `view: <from> -> <to>`
    /// for the whole of a cut's transition.
    fn caption(&self, now: f64) -> String {
        if self.director.progress(now).is_some() {
            let (from, to) = self.director.cameras();
            format!("view: {} -> {}", from.side_name(), to.side_name())
        } else {
            format!("view: {}", self.director.camera_at(now).side_name())
        }
    }

    /// The flag of each die, for a frame drawn as `placed` says, in die order
    /// (`None` where no place was found).
    ///
    /// Everything is worked out in cells with the frame's own projection:
    /// braille cells are 2 x 4 pixels, half-block cells 1 x 2. The text stays
    /// outside the hull of the board grid's four corner blocks, each grown by
    /// the size of a block (half a tile each side, [`lift`] above, a row
    /// below) and a cell of margin; the slab fills the whole grid, so that
    /// hull is what the scene covers. A pin stands on the top or bottom
    /// corner of its die's top face.
    fn flag_layout(&self, placed: &Placement, scene_rows: usize) -> Vec<Option<Flag>> {
        let (sx, sy) = if self.braille { (2.0, 4.0) } else { (1.0, 2.0) };
        let iso = &placed.iso;
        let at = |gx: f32, gy: f32| {
            let (x, y) = iso.project(gx, gy, placed.grid);
            (x as f32, y as f32)
        };
        let (hw, up, down) = (
            iso.tw as f32 / 2.0 + sx,
            lift(iso.tw) as f32 + sy,
            (iso.th + 1) as f32 + sy,
        );
        let (gw, gh) = (placed.grid.0 as f32 - 1.0, placed.grid.1 as f32 - 1.0);
        let outline: Vec<(f32, f32)> = [(0.0, 0.0), (gw, 0.0), (gw, gh), (0.0, gh)]
            .into_iter()
            .flat_map(|(gx, gy)| {
                let (x, y) = at(gx, gy);
                [(-hw, -up), (hw, -up), (-hw, down), (hw, down)]
                    .map(|(dx, dy)| ((x + dx) / sx, (y + dy) / sy))
            })
            .collect();
        let hull = Hull::of(&outline);
        let cell = |(x, y): (f32, f32)| ((x / sx).floor() as i32, (y / sy).floor() as i32);
        let dies: Vec<DieFlag> = placed
            .dies
            .iter()
            .map(|((gx, gy), label)| {
                let corners = [
                    at(gx - DIE_HALF.0, gy - DIE_HALF.1),
                    at(gx + DIE_HALF.0, gy - DIE_HALF.1),
                    at(gx + DIE_HALF.0, gy + DIE_HALF.1),
                    at(gx - DIE_HALF.0, gy + DIE_HALF.1),
                ];
                let by_y = |a: &&(f32, f32), b: &&(f32, f32)| a.1.total_cmp(&b.1);
                let top = *corners.iter().min_by(by_y).unwrap();
                let bottom = *corners.iter().max_by(by_y).unwrap();
                // The full label first, then the name alone.
                let mut texts = vec![label.clone()];
                if let Some((name, _)) = label.split_once(' ') {
                    texts.push(name.to_string());
                }
                DieFlag {
                    texts,
                    top: cell(top),
                    bottom: cell(bottom),
                }
            })
            .collect();
        flags::place(&dies, &hull, self.cols, scene_rows)
    }

    /// The pin, pole and corner of a flag: pale lavender, the Ethernet
    /// colour, on the same background as the text.
    fn pole_style(&self) -> Style {
        Style::default()
            .fg(rgb(GRAYSKULL.eth))
            .bg(self.background())
    }

    /// A flag's text: white and bold. The background stays the terminal's own
    /// in transparent mode and is the black fill otherwise, as around the
    /// dies.
    fn label_style(&self) -> Style {
        Style::default()
            .fg(rgb(GRAYSKULL.white))
            .bg(self.background())
            .add_modifier(Modifier::BOLD)
    }

    /// The background of text written over the scene: `Reset` (the
    /// terminal's own) in transparent mode, the black fill otherwise.
    fn background(&self) -> Color {
        if self.transparent {
            Color::Reset
        } else {
            rgb(OPAQUE_BG)
        }
    }

    /// The switches, in the order the footer shows them, with the scene as
    /// last committed. The footer, and the test that every toggle key has an
    /// entry, read this table.
    pub fn switches(&self) -> Vec<Switch> {
        // Before every pending switch's midpoint: only committed state.
        self.switches_at(f64::NEG_INFINITY)
    }

    /// `switches` for the frame at `now`, where a switch that is past its
    /// midpoint already counts. This is the one table; add an entry here.
    fn switches_at(&self, now: f64) -> Vec<Switch> {
        vec![
            Switch {
                key: 'B',
                label: "",
                state: if self.braille {
                    "braille"
                } else {
                    "half-block"
                },
            },
            Switch {
                key: 'T',
                label: "",
                state: if self.transparent {
                    "transparent"
                } else {
                    "opaque"
                },
            },
            Switch {
                key: 'C',
                label: "auto-cuts",
                state: if self.director.auto() { "on" } else { "off" },
            },
            Switch {
                key: 'X',
                label: "transition",
                state: self.director.transition_mode().label(),
            },
            Switch {
                key: 'm',
                label: "",
                state: match self.switcher.scene_at(now) {
                    Scene::All => "all boards",
                    Scene::Board(_) => "board",
                },
            },
            Switch {
                key: 'L',
                label: "labels",
                state: if self.show_labels { "on" } else { "off" },
            },
            Switch {
                key: 'W',
                label: "clouds",
                state: if self.show_clouds { "on" } else { "off" },
            },
            Switch {
                key: 'H',
                label: "history",
                state: if self.show_history { "on" } else { "off" },
            },
        ]
    }

    /// The footer's second row after its bar, at most `width` characters.
    /// From most to least detailed, the first that fits wins:
    ///
    /// 1. `B braille · T transparent · C auto-cuts on · X transition shuffle · m all boards · L labels on · W clouds on · H history on · [ ] board · c cut · 3 leave`
    ///    (153 characters: it needs a view of 156 columns)
    /// 2. the same without the action keys
    /// 3. `<key> <state>` for each switch (`C on`)
    /// 4. the state words alone
    ///
    /// The first four forms keep every state word whole. When even the last
    /// is too wide, the state words are cut at a character with an ellipsis
    /// (`fit_width`), so the last word shown may be partial.
    fn switch_strip(&self, now: f64, width: usize) -> String {
        let switches = self.switches_at(now);
        let join = |each: &dyn Fn(&Switch) -> String| {
            switches.iter().map(each).collect::<Vec<_>>().join(SEP)
        };
        let full = join(&|s| {
            if s.label.is_empty() {
                format!("{} {}", s.key, s.state)
            } else {
                format!("{} {} {}", s.key, s.label, s.state)
            }
        });
        let with_actions = format!("{full}{SEP}{ACTION_KEYS}");
        let keyed = join(&|s| format!("{} {}", s.key, s.state));
        let bare = join(&|s| s.state.to_string());
        let fits = [&with_actions, &full, &keyed, &bare]
            .into_iter()
            .find(|row| row.chars().count() <= width)
            .cloned();
        fits.unwrap_or_else(|| fit_width(bare, width))
    }

    /// `chip 3: Wormhole (not drawn)` for each device the view does not draw.
    fn undrawn_note(&self) -> String {
        self.undrawn
            .iter()
            .map(|(i, arch)| format!("chip {i}: {} (not drawn)", arch.name()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `fit` turned as `aim_by` says. `grid` must be the grid `fit` was sized
/// for, so the camera keeps the grid centre where `fit` put it.
fn aim(fit: &SceneFit, aim_by: Aim, grid: (usize, usize)) -> SceneFit {
    let iso = match aim_by {
        Aim::At(camera) => camera.apply(&fit.iso, grid),
        Aim::Between(from, to, t) => Camera::lerp_view(&from, &to, t, &fit.iso, grid),
    };
    SceneFit {
        iso,
        scale: fit.scale,
    }
}

/// The line shown in place of a scene that does not fit. Braille has the most
/// pixels per cell, so only in half-block mode can `B` give a better fit.
fn no_fit_hint(braille: bool) -> &'static str {
    if braille {
        "This scene does not fit at this size. Enlarge the terminal."
    } else {
        "This scene does not fit at this size. Enlarge the terminal or press B for braille."
    }
}

/// A die's label: its name and, when known, its power in whole watts
/// (`BH0 44W`).
fn chip_label(chip: &ChipInputs) -> String {
    match chip.power_w {
        Some(w) => format!("{} {:.0}W", chip.label, w),
        None => chip.label.clone(),
    }
}

/// Write each flag over `lines`: the text in `style`, the pin, the pole and
/// the corner in `pole_style`.
///
/// Only the first `scene_rows` lines are written: the footer rows below them
/// are never touched, whatever a flag's cells. A cell outside columns
/// `0..cols` or outside the line's own cells is dropped, so no line grows.
fn draw_flags(
    lines: &mut [Line<'static>],
    scene_rows: usize,
    cols: usize,
    flags: &[Option<Flag>],
    style: Style,
    pole_style: Style,
) {
    for flag in flags.iter().flatten() {
        let text_row = flag.pin.1 - flag.rise;
        for ((col, row), ch) in flag.cells() {
            if row < 0 || row as usize >= scene_rows || col < 0 {
                continue;
            }
            let Some(line) = lines.get_mut(row as usize) else {
                continue;
            };
            let st = if row == text_row && !matches!(ch, '┌' | '┐' | '└' | '┘') {
                style
            } else {
                pole_style
            };
            write_over(line, col as i64, &ch.to_string(), cols, st);
        }
    }
}

/// Write the history panel's `rows` over `lines`, the first at row
/// [`PANEL_TOP`] and every one from column [`PANEL_LEFT`], in `style`.
///
/// As with the labels, only the first `scene_rows` lines are written, so the
/// footer rows are never touched, and each row is clipped to columns
/// `0..cols` and to the line's own cells.
fn draw_panel(
    lines: &mut [Line<'static>],
    scene_rows: usize,
    cols: usize,
    rows: &[String],
    style: Style,
) {
    for (i, text) in rows.iter().enumerate() {
        let row = PANEL_TOP + i;
        if row >= scene_rows {
            break;
        }
        if let Some(line) = lines.get_mut(row) {
            write_over(line, PANEL_LEFT as i64, text, cols, style);
        }
    }
}

/// Write `text` over `line` from cell `start` (which may be negative: the
/// characters left of cell 0 are dropped), in `style`. Each cell written
/// takes the text's character and the style. Only cells in `0..cols` that
/// the line already has are written, so the line never grows. Works per
/// character, never per byte: a braille cell is three bytes.
fn write_over(line: &mut Line<'static>, start: i64, text: &str, cols: usize, style: Style) {
    // One (character, style) per cell; every cell is one character.
    let mut cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    for (i, ch) in text.chars().enumerate() {
        let at = start + i as i64;
        if (0..cols as i64).contains(&at) {
            if let Some(cell) = cells.get_mut(at as usize) {
                *cell = (ch, style);
            }
        }
    }
    // Back to spans, one per run of a style.
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (ch, st) in cells {
        match spans.last_mut() {
            Some(last) if last.style == st => last.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), st)),
        }
    }
    line.spans = spans;
}

/// `text` cut to `width` characters, with an ellipsis where it was cut.
fn fit_width(text: String, width: usize) -> String {
    if text.chars().count() <= width {
        return text;
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

fn hint(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(FOOTER_FG)))
}

fn note(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(KEYS_FG)))
}

#[cfg(test)]
mod tests {
    use super::camera::TransitionMode;
    use super::scene::{has_per_tile_data, ESTIMATE_LABEL};
    use super::test_backend::Fake;
    use super::*;
    use crate::backend::pcie_counters::PcieBandwidth;
    use std::collections::HashMap;

    /// TDP the fakes report, so `power` watts is a load of `power / 100`
    /// while a chip's `LoadScale` has fewer than 5 samples (the first 4
    /// seconds of view time). The tests that cut on a rise from 0 W to 90 W
    /// within those seconds use that fallback; after it, the load is
    /// relative to the chip's own idle (see
    /// `the_view_feeds_relative_load_to_the_scene_and_the_triggers`).
    const TDP: &str = "100";

    /// The text of each line, spans joined.
    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// The footer: the last two lines, joined.
    fn footer(lines: &[Line<'static>]) -> String {
        let t = text(lines);
        t[t.len().saturating_sub(2)..].join("\n")
    }

    /// Two p300 boards: chips 0 and 1 on board 0, chips 2 and 3 on board 1,
    /// all idle (which arms the load triggers on the first update).
    fn two_boards() -> Fake {
        Fake::boards(&["p300"; 4], Some(TDP))
    }

    /// A 150 x 42 view over two boards that has seen one idle tick at 0.0 and
    /// then a load rise on chip 2 (board 1) at 1.0.
    fn switched_by_chip_2() -> (Die3dView, Fake) {
        let mut b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        b.set_power(2, 90.0);
        v.update(&b, 1.0);
        (v, b)
    }

    #[test]
    fn render_gives_one_line_per_row_at_any_size() {
        let b = two_boards();
        // Below 100 x 30 a smaller layout is drawn (see `tiers`); at the
        // tiniest sizes, and at none at all, nothing panics. Every size,
        // the minimum of the full layout included, gives one line per row.
        for (c, r) in [
            (99, 40),
            (150, 29),
            (10, 3),
            (0, 0),
            (0, 5),
            (5, 0),
            (100, 30),
        ] {
            let mut v = Die3dView::new(c, r);
            v.update(&b, 0.0);
            let lines = v.render(0.0);
            assert_eq!(lines.len(), r, "{c} x {r}: {:?}", text(&lines));
        }
    }

    /// A `cols` x `rows` view over one p150 that has read the backend once,
    /// at 0.0.
    fn p150_view(cols: usize, rows: usize) -> Die3dView {
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(cols, rows);
        v.update(&b, 0.0);
        v
    }

    /// True when `line` holds a braille glyph with at least one dot set
    /// (U+2801 to U+28FF; U+2800 is the empty cell).
    fn has_braille_dots(line: &str) -> bool {
        line.chars()
            .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
    }

    #[test]
    fn below_100_by_30_a_scene_is_drawn() {
        let v = p150_view(80, 24);
        let t = text(&v.render(0.0));
        // One line per row: the scene and a one-row footer.
        assert_eq!(t.len(), 24, "{t:?}");
        assert!(
            t[..23].iter().any(|l| has_braille_dots(l)),
            "no braille in the scene rows: {t:?}"
        );
        assert!(t.iter().all(|l| !l.contains("needs a terminal")), "{t:?}");
    }

    #[test]
    fn clouds_are_off_below_100_by_30() {
        // The control: at exactly 100 x 30 the clouds are drawn, so pressing
        // W changes the scene rows. Without it the check below could pass on
        // a scene with no clouds at any size.
        let mut v = p150_view(100, 30);
        let on = text(&v.render(0.0));
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
        let off = text(&v.render(0.0));
        assert_ne!(on[..28], off[..28], "clouds not visible at 100 x 30");
        // One column narrower the tier turns them off: W on and W off draw
        // the same frame. The one-row footer has no switch strip, so the
        // footer is the same too.
        let mut v = p150_view(99, 40);
        let on = text(&v.render(0.0));
        assert_eq!(on.len(), 40, "a scene is drawn: {on:?}");
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
        assert_eq!(text(&v.render(0.0)), on);
    }

    #[test]
    fn the_footer_is_one_row_then_none() {
        // 99 x 40: one footer row, the status line, naming the scene.
        let t = text(&p150_view(99, 40).render(0.0));
        assert_eq!(t.len(), 40, "{t:?}");
        let last = &t[39];
        assert!(last.starts_with("\u{2551} "), "{last}");
        assert!(last.contains("board 1/1"), "{last}");
        assert!(!t[38].starts_with("\u{2551} "), "{}", t[38]);
        // 59 x 40: no footer. The last line is a scene row.
        let t = text(&p150_view(59, 40).render(0.0));
        assert_eq!(t.len(), 40, "{t:?}");
        assert!(
            t.iter()
                .all(|l| !l.starts_with('\u{2551}') && !l.starts_with('\u{255a}')),
            "{t:?}"
        );
        assert!(t.iter().any(|l| has_braille_dots(l)), "{t:?}");
    }

    #[test]
    fn flags_are_off_below_60_by_16() {
        // The control: at 60 x 40 a flag finds room, so its pin shows.
        let t = text(&p150_view(60, 40).render(0.0));
        assert!(
            t.iter().any(|l| l.contains('●')),
            "no flag at 60 x 40: {t:?}"
        );
        // One column narrower: no flags at all.
        let t = text(&p150_view(59, 40).render(0.0));
        assert_eq!(t.len(), 40, "a scene is drawn: {t:?}");
        assert!(t.iter().all(|l| !l.contains('●')), "{t:?}");
    }

    #[test]
    fn render_with_no_devices_does_not_panic() {
        let b = Fake::boards(&[], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        // Before any update, then after one with no devices.
        assert_eq!(v.render(0.0).len(), 1);
        v.update(&b, 0.0);
        for k in [
            Die3dKey::NextBoard,
            Die3dKey::PrevBoard,
            Die3dKey::ToggleScene,
            Die3dKey::CutNow,
            Die3dKey::ToggleBraille,
            Die3dKey::CycleTransition,
        ] {
            v.handle_key(k, 0.5);
        }
        let lines = v.render(0.6);
        assert_eq!(lines.len(), 1);
        assert!(text(&lines)[0].contains("no Tenstorrent device"));
    }

    #[test]
    fn a_non_blackhole_device_is_named_in_the_footer() {
        // Two single-chip cards: a p150 (drawn) and an n150 (not drawn).
        let b = Fake::boards(&["p150", "n150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let lines = v.render(0.0);
        assert_eq!(lines.len(), 42);
        assert!(
            footer(&lines).contains("chip 1: Wormhole (not drawn)"),
            "{}",
            footer(&lines)
        );
        // With no Blackhole at all: the hint plus the same note, nothing else.
        let b = Fake::boards(&["n300", "n300"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let t = text(&v.render(0.0));
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(t[0].contains("Blackhole"));
        assert!(t[1].contains("chip 0: Wormhole (not drawn)"));
        assert!(t[1].contains("chip 1: Wormhole (not drawn)"));
    }

    #[test]
    fn one_chip_without_telemetry_renders() {
        let mut b = Fake::boards(&["p150"], None);
        b.telem.clear();
        b.has_smbus = false;
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let lines = v.render(0.0);
        assert_eq!(lines.len(), 42);
        // One board: the super scene is that board's scene.
        assert!(footer(&lines).contains("board 1/1"), "{}", footer(&lines));
        // The scene rows carry the drawing: 150 cells each.
        let t = text(&lines);
        assert_eq!(t[0].chars().count(), 150);
        assert_eq!(t[39].chars().count(), 150);
    }

    #[test]
    fn a_load_rise_in_the_super_scene_switches_to_that_chips_board_at_the_cut_midpoint() {
        let (v, _) = switched_by_chip_2();
        let at = v
            .director
            .swapped_at()
            .expect("the load rise cut the camera");
        // A scene change is a chaos or a dissolve; both swap at their midpoint.
        let kind = v.director.transition_kind().unwrap();
        assert_ne!(kind, TransitionKind::Spin);
        assert!((at - (1.0 + kind.secs() / 2.0)).abs() < 1e-9);
        // Still the super scene up to the midpoint, board 2 from it.
        assert_eq!(v.switcher.scene_at(at - 0.01), Scene::All);
        assert_eq!(v.switcher.scene_at(at), Scene::Board(1));
        assert!(footer(&v.render(at - 0.01)).contains("all boards"));
        let f = footer(&v.render(at + 0.01));
        assert!(f.contains("board 2/2 board-1"), "{f}");
        // A PCIe burst in the super scene cuts the angle but keeps the scene.
        let mut b = two_boards();
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 0.0,
            tx_bytes_per_sec: 0.0,
        });
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 3.0e9,
            tx_bytes_per_sec: 0.0,
        });
        v.update(&b, 1.0);
        assert!(
            v.director.swapped_at().is_some(),
            "the burst cut the camera"
        );
        assert_eq!(v.switcher.scene_at(5.0), Scene::All);
    }

    #[test]
    fn a_trigger_in_a_board_scene_cuts_only_for_that_boards_chips() {
        let mut b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // Show board 1 (chips 2 and 3) by hand.
        v.handle_key(Die3dKey::NextBoard, 0.0);
        v.handle_key(Die3dKey::NextBoard, 0.0);
        assert_eq!(v.switcher.scene_at(0.0), Scene::Board(1));
        // Chip 0 is on board 0: no cut, and no switch.
        b.set_power(0, 90.0);
        v.update(&b, 1.0);
        assert!(v.director.swapped_at().is_none());
        assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1));
        // Chip 3 is on board 1: the camera cuts, the scene stays.
        b.set_power(3, 90.0);
        v.update(&b, 2.0);
        assert!(v.director.swapped_at().is_some());
        assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1));
    }

    /// The highest heat any Tensix tile of `chip` shows over a few seconds of
    /// the estimate's wave. Taken over many `t` so the wave's position does
    /// not decide the comparison.
    fn peak_tile_heat(chip: &ChipInputs) -> f32 {
        let layout = super::layout::DieLayout::blackhole();
        let mut best = 0.0f32;
        for step in 0..200 {
            let t = step as f32 * 0.1;
            for tile in &layout.tiles {
                best = best.max(super::scene::tile_value(chip, tile, t));
            }
        }
        best
    }

    #[test]
    fn the_view_feeds_relative_load_to_the_scene_and_the_triggers() {
        // Measured on real hardware: a 125 W power limit, 37 W idle, 68 W
        // under serving. As power over the limit that is 0.30 then 0.54, and
        // 0.30 is above LOAD_REARM (0.25), so the rise could never fire.
        let mut b = Fake::boards(&["p150"], Some("125"));
        b.set_power(0, 37.0);
        let mut v = Die3dView::new(150, 42);
        // 30 s of idle, ticked four times a second like the TUI might.
        let mut now = 0.0;
        while now < 30.0 {
            v.update(&b, now);
            now += 0.25;
        }
        assert!(v.director.swapped_at().is_none(), "idle must not cut");
        let rest = v.chips[0][0].clone();
        assert_eq!(rest.load, 0.0, "idle is zero load against its own idle");
        // The rise to 68 W.
        b.set_power(0, 68.0);
        v.update(&b, now);
        let busy = v.chips[0][0].clone();
        assert_eq!(busy.load, 1.0, "68 W is full load against a 37 W idle");
        assert!(
            v.director.swapped_at().is_some(),
            "LoadRise fired and cut the camera"
        );
        // The scene reads the same load: the tiles glow brighter than at rest.
        let (cold, hot) = (peak_tile_heat(&rest), peak_tile_heat(&busy));
        assert!(hot > cold + 0.5, "tile heat {cold} at rest, {hot} busy");
    }

    #[test]
    fn a_chip_with_no_power_reading_stays_at_zero_load_and_never_fires() {
        // Its scale never gets a sample, so the view keeps the fallback,
        // which is 0.0 without a power reading.
        let mut b = Fake::boards(&["p150"], Some("125"));
        b.telem[0].power = None;
        let mut v = Die3dView::new(150, 42);
        for i in 0..120 {
            v.update(&b, i as f64 * 0.5);
            assert_eq!(v.chips[0][0].load, 0.0);
        }
        assert!(v.director.swapped_at().is_none());
    }

    #[test]
    fn a_busy_chip_that_loses_its_power_reading_reads_zero_load_at_once() {
        // 30 s idle at 37 W, then busy at 68 W: full load.
        let mut b = Fake::boards(&["p150"], Some("125"));
        b.set_power(0, 37.0);
        let mut v = Die3dView::new(150, 42);
        for i in 0..30 {
            v.update(&b, i as f64);
        }
        b.set_power(0, 68.0);
        v.update(&b, 30.0);
        assert_eq!(v.chips[0][0].load, 1.0);
        let cut = v.director.cut_at();
        assert!(cut.is_some(), "the rise cut the camera");
        // The reading goes away. Nothing was read, so the chip is not shown
        // as busy: load 0.0 on the very next update, at a tick inside the
        // same second as well as a second later, and no cut.
        b.telem[0].power = None;
        for now in [30.5, 31.0, 32.0, 40.0] {
            v.update(&b, now);
            assert_eq!(v.chips[0][0].load, 0.0, "at {now}");
            assert_eq!(v.director.cut_at(), cut, "at {now}");
        }
        // The clouds, which follow the busiest chip, dim with it.
        assert_eq!(v.cloud_intensity, Clouds::intensity(0.0));
        // A NaN reading is no reading either.
        b.set_power(0, 68.0);
        v.update(&b, 41.0);
        assert_eq!(v.chips[0][0].load, 1.0);
        b.telem[0].power = Some(f32::NAN);
        v.update(&b, 42.0);
        assert_eq!(v.chips[0][0].load, 0.0);
    }

    #[test]
    fn a_chip_that_disappears_loses_its_scale() {
        let mut b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let mut keys: Vec<usize> = v.scales.keys().copied().collect();
        keys.sort_unstable();
        assert_eq!(keys, [0, 1, 2, 3]);
        // Board 1 (chips 2 and 3) goes away.
        b.devices.truncate(2);
        v.update(&b, 1.0);
        let mut keys: Vec<usize> = v.scales.keys().copied().collect();
        keys.sort_unstable();
        assert_eq!(keys, [0, 1]);
    }

    #[test]
    fn the_view_returns_to_the_super_scene_after_20_seconds_without_triggers() {
        let (mut v, mut b) = switched_by_chip_2();
        v.update(&b, 1.5);
        assert_eq!(v.switcher.scene_at(1.5), Scene::Board(1));
        // 19.9 s after the load rise: still on the board.
        v.update(&b, 20.9);
        assert_eq!(v.switcher.scene_at(20.9), Scene::Board(1));
        // 20 s after it: back to every board.
        v.update(&b, 21.0);
        assert_eq!(v.switcher.scene_at(21.0), Scene::All);

        // A load rise on the board on show restarts the 20 s (see
        // `a_rise_on_the_board_on_show_does_not_return`); one on another
        // board does not (see `the_return_timer_ignores_rises_on_other_boards`).
    }

    /// `switched_by_chip_2` settled on board 1: the switch's cut (at 1.0)
    /// has passed its midpoint and chip 2 still draws 90 W.
    fn on_board_1_chosen_by_the_view() -> (Die3dView, Fake) {
        let (mut v, b) = switched_by_chip_2();
        v.update(&b, 4.0);
        assert_eq!(v.switcher.scene_at(4.0), Scene::Board(1));
        assert_eq!(v.director.cut_at(), Some(1.0));
        (v, b)
    }

    #[test]
    fn a_rise_on_another_board_returns_to_all_boards_when_on_an_auto_chosen_board() {
        let (mut v, mut b) = on_board_1_chosen_by_the_view();
        // Chip 0, on board 0, rises 9 s after the last cut (past the 8 s
        // cooldown): a cut that goes back to every board at its midpoint.
        b.set_power(0, 90.0);
        v.update(&b, 10.0);
        assert_eq!(v.director.cut_at(), Some(10.0), "the rise cut the camera");
        let kind = v.director.transition_kind().unwrap();
        assert_ne!(kind, TransitionKind::Spin, "a scene change is never a spin");
        let at = v.director.swapped_at().unwrap();
        assert_eq!(v.switcher.scene_at(at - 0.01), Scene::Board(1));
        assert_eq!(v.switcher.scene_at(at), Scene::All);
        assert_eq!(v.transition_scenes(10.1), (Scene::Board(1), Scene::All));
        v.update(&b, at + 0.1);
        assert!(footer(&v.render(at + 0.1)).contains("all boards"));
        // Back on every board, the view stays there: no return pending.
        v.update(&b, 60.0);
        assert_eq!(v.switcher.scene_at(60.0), Scene::All);

        // Inside the cooldown the rise is ignored: no cut, no switch, and
        // nothing kept for later. The 20 s return still comes, at 21 s.
        let (mut v, mut b) = on_board_1_chosen_by_the_view();
        b.set_power(0, 90.0);
        v.update(&b, 5.0);
        assert_eq!(v.director.cut_at(), Some(1.0));
        assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1));
        v.update(&b, 12.0);
        assert_eq!(v.director.cut_at(), Some(1.0));
        assert_eq!(v.switcher.scene_at(12.0), Scene::Board(1));
    }

    #[test]
    fn a_rise_on_the_board_on_show_does_not_return() {
        let (mut v, mut b) = on_board_1_chosen_by_the_view();
        // Chip 3, on board 1, rises at 10 s (chip 2 dropped back first, so
        // the detector re-armed): the camera cuts, the board stays, and the
        // 20 s return now counts from 10 s.
        b.set_power(2, 0.0);
        v.update(&b, 6.0);
        b.set_power(3, 90.0);
        v.update(&b, 10.0);
        assert_eq!(v.director.cut_at(), Some(10.0));
        for now in [10.5, 15.0, 21.0, 29.9] {
            v.update(&b, now);
            assert_eq!(v.switcher.scene_at(now), Scene::Board(1), "{now}");
        }
        v.update(&b, 30.0);
        assert_eq!(v.switcher.scene_at(30.0), Scene::All);
    }

    #[test]
    fn the_return_timer_ignores_rises_on_other_boards() {
        // A rise on board 0 inside the cooldown neither cuts nor moves the
        // return: the view goes back to every board 20 s after the rise
        // that chose board 1 (at 1 s).
        let (mut v, mut b) = on_board_1_chosen_by_the_view();
        b.set_power(0, 90.0);
        v.update(&b, 5.0);
        v.update(&b, 20.9);
        assert_eq!(v.switcher.scene_at(20.9), Scene::Board(1));
        v.update(&b, 21.0);
        assert_eq!(v.switcher.scene_at(21.0), Scene::All);
    }

    #[test]
    fn a_hand_chosen_board_is_never_changed_by_activity() {
        let mut b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // Board 1 by hand: `]` opens the board shown last (0), `]` again 1.
        v.handle_key(Die3dKey::NextBoard, 0.0);
        v.handle_key(Die3dKey::NextBoard, 0.0);
        assert_eq!(v.switcher.scene_at(0.0), Scene::Board(1));
        // Rises on board 0, again and again, well apart: no cut, no switch.
        for (on, off) in [(1.0, 5.0), (20.0, 25.0), (40.0, 45.0)] {
            b.set_power(0, 90.0);
            v.update(&b, on);
            b.set_power(0, 0.0);
            v.update(&b, off);
        }
        assert!(v.director.cut_at().is_none());
        // A rise on board 1 cuts the angle only.
        b.set_power(3, 90.0);
        v.update(&b, 50.0);
        assert_eq!(v.director.cut_at(), Some(50.0));
        v.update(&b, 120.0);
        assert_eq!(v.switcher.scene_at(120.0), Scene::Board(1));
    }

    #[test]
    fn a_manual_scene_choice_cancels_the_auto_return() {
        // `]` after the automatic switch: the user's board stays.
        let (mut v, b) = switched_by_chip_2();
        v.update(&b, 1.5);
        v.handle_key(Die3dKey::NextBoard, 2.0);
        assert_eq!(v.switcher.scene_at(2.0), Scene::Board(0));
        v.update(&b, 60.0);
        assert_eq!(v.switcher.scene_at(60.0), Scene::Board(0));

        // `m` twice: back on the board the view chose, now as the user's.
        let (mut v, b) = switched_by_chip_2();
        v.update(&b, 1.5);
        v.handle_key(Die3dKey::ToggleScene, 2.0);
        assert_eq!(v.switcher.scene_at(2.0), Scene::All);
        v.handle_key(Die3dKey::ToggleScene, 2.1);
        assert_eq!(v.switcher.scene_at(2.1), Scene::Board(1));
        v.update(&b, 60.0);
        assert_eq!(v.switcher.scene_at(60.0), Scene::Board(1));

        // `m` before the midpoint cancels the pending switch too.
        let (mut v, b) = switched_by_chip_2();
        v.handle_key(Die3dKey::ToggleScene, 1.1);
        v.update(&b, 1.5);
        assert_eq!(v.switcher.scene_at(1.5), Scene::Board(0));
        v.update(&b, 60.0);
        assert_eq!(v.switcher.scene_at(60.0), Scene::Board(0));
    }

    #[test]
    fn frames_a_whole_number_of_days_apart_are_identical() {
        // After a month of uptime, f32 view time steps by 0.25 s, so the
        // wave and the packets would move at 4 Hz. The scene's time wraps
        // once a day in f64 first, so a frame 30 days on is the frame of
        // the same time of day.
        let mut b = two_boards();
        b.set_power(0, 60.0);
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 2.0e9,
            tx_bytes_per_sec: 5.0e8,
        });
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let month = 86_400.0 * 30.0;
        for t0 in [1.3, 7.6, 1234.5678, 86_399.9] {
            let (now, _) = v.scene_pixels(t0).unwrap();
            let (later, _) = v.scene_pixels(t0 + month).unwrap();
            assert!(
                now.rgb == later.rgb && now.cover == later.cover,
                "at {t0} s and 30 days on"
            );
            // The instrument: half a second later the frame does differ.
            let (moved, _) = v.scene_pixels(t0 + 0.5).unwrap();
            assert!(now.rgb != moved.rgb, "nothing moved at {t0}");
        }
    }

    #[test]
    fn resize_keeps_switches_history_and_load_calibration() {
        let mut b = two_boards();
        let mut v = Die3dView::with_reserved_rows(150, 42, 1);
        for t in 0..8 {
            b.set_power(2, 30.0 + t as f32);
            v.update(&b, t as f64);
        }
        // Every switch away from its default, a board chosen by hand.
        for k in [
            Die3dKey::ToggleBraille,
            Die3dKey::ToggleTransparent,
            Die3dKey::ToggleClouds,
            Die3dKey::ToggleHistory,
            Die3dKey::ToggleLabels,
            Die3dKey::CycleTransition,
            Die3dKey::ToggleAuto,
            Die3dKey::NextBoard,
            Die3dKey::NextBoard,
        ] {
            v.handle_key(k, 8.0);
        }
        let switches = v.switches_at(8.0);
        let scene = v.switcher.scene_at(8.0);
        let mode = v.director.transition_mode();
        let samples = v.history.power(2).map(<[_]>::len);
        assert_eq!(samples, Some(8));
        assert!(v.scales[&2].ready());
        let cloud_offset = v.cloud_clock.get().offset_px();
        let revision = v.revision;
        v.resize(200, 55, 1);
        assert_eq!((v.cols, v.rows, v.reserved_rows), (200, 55, 1));
        assert_eq!(v.switches_at(8.0), switches);
        assert_eq!(v.switcher.scene_at(8.0), scene);
        assert_eq!(v.director.transition_mode(), mode);
        assert!(!v.director.auto());
        assert_eq!(v.history.power(2).map(<[_]>::len), samples);
        assert!(v.scales[&2].ready());
        assert_eq!(v.scales[&2].len(), 8);
        assert_eq!(v.cloud_clock.get().offset_px(), cloud_offset);
        assert_eq!(v.chips.len(), 2, "the boards read before the resize stay");
        assert!(v.revision > revision, "a resize is a change to show");
    }

    #[test]
    fn resize_drops_the_fit_cache_and_redraws_at_once() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        assert_eq!(v.render(1.0).len(), 42);
        assert!(!v.cache.borrow().fits.is_empty());
        assert!(v.cache.borrow().last.is_some());
        v.resize(120, 36, 0);
        {
            let cache = v.cache.borrow();
            assert!(cache.fits.is_empty(), "fits of the old size dropped");
            assert!(cache.spare.is_empty(), "buffers of the old size dropped");
            assert!(cache.last.is_none(), "the old frame dropped");
        }
        // 0.01 s later, well inside the frame interval: a fresh frame at the
        // new size, never the old one again.
        let lines = v.render(1.01);
        assert_eq!(lines.len(), 36);
        assert!(lines[0].width() <= 120);
        // Below 100 x 30 the smaller layout is drawn at once: one line per
        // row, the last the one-row footer. Growing again draws the full
        // layout.
        v.resize(80, 20, 1);
        let small = text(&v.render(1.02));
        assert_eq!(small.len(), 20, "{small:?}");
        assert!(small[19].starts_with("\u{2551} "), "{small:?}");
        v.resize(150, 42, 0);
        assert_eq!(v.render(1.03).len(), 42);
    }

    #[test]
    fn render_during_a_cut_then_new_at_another_size_does_not_panic() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.director.set_transition_mode(TransitionMode::Chaos);
        v.handle_key(Die3dKey::CutNow, 5.0);
        assert!(v.director.chaos(5.3).is_some(), "mid-chaos");
        assert_eq!(v.render(5.3).len(), 42);
        // Every size must render, in every transition, at several points of
        // it: for a view made at that size, and for one resized to it in
        // the middle of the cut (as the TUI does on a terminal resize).
        for (c, r) in [(120, 35), (100, 30), (220, 60), (101, 31)] {
            for braille in [true, false] {
                for mode in [
                    TransitionMode::Chaos,
                    TransitionMode::Dissolve,
                    TransitionMode::Spin,
                ] {
                    let mut v = Die3dView::new(c, r);
                    v.update(&b, 5.0);
                    v.director.set_transition_mode(mode);
                    if !braille {
                        v.handle_key(Die3dKey::ToggleBraille, 5.0);
                    }
                    v.handle_key(Die3dKey::CutNow, 5.0);
                    for dt in [0.0, 0.3, 0.7, 1.2, 1.7] {
                        assert_eq!(
                            v.render(5.0 + dt).len(),
                            r,
                            "{c} x {r} braille={braille} {mode:?} +{dt}"
                        );
                    }
                    let mut w = Die3dView::new(150, 42);
                    w.update(&b, 5.0);
                    w.director.set_transition_mode(mode);
                    if !braille {
                        w.handle_key(Die3dKey::ToggleBraille, 5.0);
                    }
                    w.handle_key(Die3dKey::CutNow, 5.0);
                    w.render(5.3);
                    w.resize(c, r, 0);
                    for dt in [0.31, 0.7, 1.2, 1.7] {
                        assert_eq!(
                            w.render(5.0 + dt).len(),
                            r,
                            "resized to {c} x {r} braille={braille} {mode:?} +{dt}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn key_to_die3d_maps_the_six_keys() {
        assert_eq!(key_to_die3d(']'), Some(Die3dKey::NextBoard));
        assert_eq!(key_to_die3d('['), Some(Die3dKey::PrevBoard));
        assert_eq!(key_to_die3d('m'), Some(Die3dKey::ToggleScene));
        assert_eq!(key_to_die3d('c'), Some(Die3dKey::CutNow));
        assert_eq!(key_to_die3d('C'), Some(Die3dKey::ToggleAuto));
        assert_eq!(key_to_die3d('B'), Some(Die3dKey::ToggleBraille));
        for other in ['b', 'M', '3', 'x', ' '] {
            assert_eq!(key_to_die3d(other), None, "{other:?}");
        }
    }

    #[test]
    fn the_footer_says_estimate_until_per_tile_data_exists() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let all: Vec<ChipInputs> = v.chips.concat();
        assert!(!has_per_tile_data(&all));
        assert!(footer(&v.render(0.0)).contains(ESTIMATE_LABEL));
        // Measured data on one chip: the label goes. At 0.1, past the 1/20 s
        // for which `render` would return the frame it drew at 0.0: the data
        // is set here directly, not through a key or a change of boards.
        v.chips[1][0].tile_flow = Some(HashMap::new());
        assert!(has_per_tile_data(&v.chips.concat()));
        assert!(!footer(&v.render(0.1)).contains(ESTIMATE_LABEL));
    }

    #[test]
    fn the_footer_names_the_mode_and_auto_cuts() {
        let b = two_boards();
        // 160 columns: since `H` joined the switches, the action keys fit
        // after them only from 156 columns.
        let mut v = Die3dView::new(160, 42);
        v.update(&b, 0.0);
        let f = footer(&v.render(0.0));
        for want in [
            "braille",
            "auto-cuts on",
            "[ ] board",
            "m all boards",
            "c cut",
        ] {
            assert!(f.contains(want), "{want}: {f}");
        }
        v.handle_key(Die3dKey::ToggleBraille, 0.0);
        v.handle_key(Die3dKey::ToggleAuto, 0.0);
        let lines = v.render(0.0);
        assert_eq!(lines.len(), 42);
        let f = footer(&lines);
        assert!(
            f.contains("half-block") && f.contains("auto-cuts off"),
            "{f}"
        );
        // Half-block rows are one cell per pixel column.
        assert_eq!(text(&lines)[10].chars().count(), 160);
    }

    #[test]
    fn render_draws_the_frame_not_the_bare_scene() {
        // Pins `render` to `frame`: a regression that drew `scene_pixels`
        // would skip the chaos noise, and this would fail.
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // Labels and the history panel off: they are drawn over the cells,
        // and the bare scene has neither.
        v.handle_key(Die3dKey::ToggleLabels, 0.0);
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        v.director.set_transition_mode(TransitionMode::Chaos);
        v.handle_key(Die3dKey::CutNow, 10.0);
        let mid = v.director.swapped_at().unwrap();
        let scene_rows = 42 - FOOTER_ROWS;
        let rows = |lines: &[Line<'static>]| text(&lines[..scene_rows]);
        let bare = |t: f64| text(&cells::to_braille_lines(&v.scene_pixels(t).unwrap().0));
        // Mid-cut the rendered rows are scrambled, so they differ from the
        // bare scene at the same instant.
        assert_ne!(rows(&v.render(mid)), bare(mid));
        // After the transition the chaos is gone and render is the scene.
        let after = 10.0 + camera::CHAOS_SECS + 0.01;
        assert_eq!(rows(&v.render(after)), bare(after));
    }

    #[test]
    fn the_tiers_are_measured_on_the_views_own_area() {
        let b = two_boards();
        // A 100 x 30 terminal under a one-row status bar: a 100 x 29 area,
        // which is below the full layout. The footer is one row.
        let mut v = Die3dView::with_reserved_rows(100, 29, 1);
        v.update(&b, 0.0);
        let t = text(&v.render(0.0));
        assert_eq!(t.len(), 29, "{t:?}");
        assert!(t[28].starts_with("\u{2551} "), "{}", t[28]);
        assert!(!t[27].starts_with("\u{2551} "), "{}", t[27]);
        // One more row and the full layout's two-row footer is drawn.
        let mut v = Die3dView::with_reserved_rows(100, 30, 1);
        v.update(&b, 0.0);
        let t = text(&v.render(0.0));
        assert_eq!(t.len(), 30);
        assert!(t[28].starts_with("\u{2551} "), "{}", t[28]);
        assert!(t[29].starts_with("\u{255a}"), "{}", t[29]);
    }

    #[test]
    fn the_no_fit_hint_only_suggests_b_when_b_can_help() {
        // Braille already has the most pixels, so pressing B cannot help.
        assert!(
            !no_fit_hint(true).contains("press B"),
            "{}",
            no_fit_hint(true)
        );
        assert!(no_fit_hint(false).contains("press B"));
    }

    #[test]
    fn the_super_scene_falls_back_to_the_board_scene_when_it_does_not_fit() {
        // Four boards of two dies do not fit at the minimum size.
        let b = Fake::boards(&["p300"; 8], Some(TDP));
        let mut v = Die3dView::new(100, 30);
        v.update(&b, 0.0);
        let (_, shown) = v.scene_pixels(0.0).expect("the board scene fits");
        assert_eq!(shown, Shown::Fallback(0));
        let lines = v.render(0.0);
        assert_eq!(lines.len(), 30);
        let f = footer(&lines);
        assert!(f.contains("board 1/4") && f.contains("do not fit"), "{f}");
        // The fallback is the board scene, pixel for pixel. From the super
        // scene the first `]` opens the board shown last (index 0), and `[`
        // steps back to it after a second `]` has moved on.
        v.handle_key(Die3dKey::NextBoard, 0.0);
        assert_eq!(v.scene_pixels(0.0).unwrap().1, Shown::Board(0));
        v.handle_key(Die3dKey::NextBoard, 0.0);
        v.handle_key(Die3dKey::PrevBoard, 0.0);
        let (board, shown) = v.scene_pixels(0.0).unwrap();
        assert_eq!(shown, Shown::Board(0));
        v.handle_key(Die3dKey::ToggleScene, 0.0);
        let (fallback, _) = v.scene_pixels(0.0).unwrap();
        assert_eq!(fallback.rgb, board.rgb);
        // At the reference size they fit, and the footer says all boards.
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        assert_eq!(v.scene_pixels(0.0).unwrap().1, Shown::All);
        assert!(footer(&v.render(0.0)).contains("all boards"));
    }

    #[test]
    fn drawing_uses_the_camera_at_now_until_the_swap() {
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut still = Die3dView::new(150, 42);
        still.update(&b, 0.0);
        let mut cut = Die3dView::new(150, 42);
        cut.update(&b, 0.0);
        cut.director.set_transition_mode(TransitionMode::Chaos);
        cut.handle_key(Die3dKey::CutNow, 10.0);
        let at = cut.director.swapped_at().unwrap();
        // Before the swap the old camera is drawn, so the scene (before the
        // chaos noise) is the uncut view's.
        let t = at - 0.05;
        assert_eq!(
            cut.scene_pixels(t).unwrap().0.rgb,
            still.scene_pixels(t).unwrap().0.rgb
        );
        // After it the new camera is drawn.
        let t = at + 0.05;
        assert_ne!(
            cut.scene_pixels(t).unwrap().0.rgb,
            still.scene_pixels(t).unwrap().0.rgb
        );
    }

    #[test]
    fn chaos_scrambles_the_frame_during_a_cut() {
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // No cut yet: the frame is the scene.
        assert_eq!(
            v.frame(3.0).unwrap().0.rgb,
            v.scene_pixels(3.0).unwrap().0.rgb
        );
        v.director.set_transition_mode(TransitionMode::Chaos);
        v.handle_key(Die3dKey::CutNow, 10.0);
        let mid = v.director.swapped_at().unwrap();
        assert_ne!(
            v.frame(mid).unwrap().0.rgb,
            v.scene_pixels(mid).unwrap().0.rgb
        );
        // After the transition the frame is clean again.
        let after = 10.0 + camera::CHAOS_SECS + 0.01;
        assert_eq!(
            v.frame(after).unwrap().0.rgb,
            v.scene_pixels(after).unwrap().0.rgb
        );
    }

    #[test]
    fn opaque_mode_covers_every_pixel() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // Transparent (the default): the corner pixel is not covered.
        let (buf, _) = v.scene_pixels(0.0).unwrap();
        assert!(!buf.covered(0, 0));
        v.handle_key(Die3dKey::ToggleTransparent, 0.1);
        let (buf, _) = v.scene_pixels(0.2).unwrap();
        assert!((0..buf.h).all(|y| (0..buf.w).all(|x| buf.covered(x, y))));
    }

    #[test]
    fn t_toggles_transparency_and_the_footer_says_which() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        assert!(v.transparent);
        let f = footer(&v.render(0.0));
        assert!(f.contains("transparent") && !f.contains("opaque"), "{f}");
        assert!(f.contains("T transparent"), "key list: {f}");
        v.handle_key(Die3dKey::ToggleTransparent, 0.1);
        assert!(!v.transparent);
        let f = footer(&v.render(0.2));
        assert!(f.contains("opaque"), "{f}");
        v.handle_key(Die3dKey::ToggleTransparent, 0.3);
        assert!(v.transparent);
    }

    #[test]
    fn key_to_die3d_maps_t_to_toggle_transparent() {
        assert_eq!(key_to_die3d('T'), Some(Die3dKey::ToggleTransparent));
        assert_eq!(key_to_die3d('t'), None);
    }

    /// The switch strip as the footer's second row shows it at the view's
    /// width. Built from `switch_strip` directly, so the fallbacks can be
    /// checked at widths where the size tier leaves no second row.
    fn strip(v: &Die3dView) -> String {
        format!(
            "\u{255a}\u{2550} {}",
            v.switch_strip(0.0, v.cols.saturating_sub(BAR_WIDTH))
        )
    }

    /// A view over two boards whose first board label is 12 characters.
    fn wide_label_view(cols: usize) -> Die3dView {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.labels[0] = "abcdefghijkl".to_string();
        v.cols = cols;
        v
    }

    #[test]
    fn the_footer_names_every_switch_and_its_state() {
        let b = two_boards();
        // Wide enough for the action keys after every switch (156 columns
        // since `H`).
        let mut v = Die3dView::new(160, 42);
        v.update(&b, 0.0);
        let f = footer(&v.render(0.0));
        for s in v.switches() {
            let triple = [s.key.to_string(), s.label.to_string(), s.state.to_string()]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(f.contains(&triple), "{triple}: {f}");
        }
        assert!(
            f.contains("[ ] board") && f.contains("c cut") && f.contains("3 leave"),
            "{f}"
        );
    }

    #[test]
    fn the_footer_keeps_every_switch_state_at_100_columns() {
        // 100 columns: the full layout's two rows hold every switch state
        // and the estimate label, each row within the width.
        let v = wide_label_view(100);
        let f = text(&v.footer(0.0, Some(Shown::All)));
        assert_eq!(f.len(), 2, "{f:?}");
        let joined = f.join("\n");
        for s in v.switches() {
            assert!(joined.contains(s.state), "{} in {joined}", s.state);
        }
        assert!(joined.contains(ESTIMATE_LABEL), "{joined}");
        for line in &f {
            assert!(line.chars().count() <= 100, "{line}");
        }
        // 80 columns: the size tier keeps the status line alone, with the
        // estimate label. The switch strip at that width still keeps every
        // state; the view does not draw it there.
        let v = wide_label_view(80);
        let f = text(&v.footer(0.0, Some(Shown::All)));
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains(ESTIMATE_LABEL), "{f:?}");
        assert!(f[0].chars().count() <= 80, "{f:?}");
        let s = strip(&v);
        for sw in v.switches() {
            assert!(s.contains(sw.state), "{} in {s}", sw.state);
        }
        assert!(s.chars().count() <= 80, "{s}");
    }

    #[test]
    fn action_keys_are_dropped_before_switch_states() {
        let v = wide_label_view(160);
        let full = strip(&v);
        assert!(full.contains("c cut"), "{full}");
        // Just wide enough for the switches and nothing after them.
        let switches_only = full.split(" · [ ]").next().unwrap().chars().count();
        let mut v = v;
        v.cols = switches_only;
        let s = strip(&v);
        assert!(!s.contains("c cut") && !s.contains("leave"), "{s}");
        for sw in v.switches() {
            assert!(s.contains(sw.state), "{}: {s}", sw.state);
        }
    }

    #[test]
    fn switches_reflect_the_view_state() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let state =
            |v: &Die3dView, k: char| v.switches().into_iter().find(|s| s.key == k).unwrap().state;
        let before: Vec<_> = "BTCXmLWH".chars().map(|k| state(&v, k)).collect();
        assert_eq!(
            before,
            [
                "braille",
                "transparent",
                "on",
                "shuffle",
                "all boards",
                "on",
                "on",
                "on"
            ]
        );
        for k in [
            Die3dKey::ToggleBraille,
            Die3dKey::ToggleTransparent,
            Die3dKey::ToggleAuto,
            Die3dKey::CycleTransition,
            Die3dKey::ToggleScene,
            Die3dKey::ToggleLabels,
            Die3dKey::ToggleClouds,
            Die3dKey::ToggleHistory,
        ] {
            v.handle_key(k, 0.0);
        }
        let after: Vec<_> = "BTCXmLWH".chars().map(|k| state(&v, k)).collect();
        assert_eq!(
            after,
            [
                "half-block",
                "opaque",
                "off",
                "chaos",
                "board",
                "off",
                "off",
                "off"
            ]
        );
    }

    #[test]
    fn the_switch_table_still_covers_every_toggle_key() {
        let v = Die3dView::new(150, 42);
        let keys: Vec<char> = v.switches().iter().map(|s| s.key).collect();
        for k in ['X', 'L', 'W', 'H'] {
            assert!(keys.contains(&k), "{k}: {keys:?}");
        }
        let mut toggling = 0;
        for &k in Die3dKey::ALL {
            if !k.toggles() {
                continue;
            }
            toggling += 1;
            let c = (0u8..128)
                .map(char::from)
                .find(|&c| key_to_die3d(c) == Some(k))
                .expect("every key has a character");
            assert!(keys.contains(&c), "{k:?} ({c}) toggles but has no switch");
        }
        assert_eq!(toggling, keys.len(), "a switch with no toggling key");
    }

    #[test]
    fn all_lists_every_key_once_and_each_has_a_character() {
        // `ALL` is generated from the enum's own declaration, so it cannot
        // miss a variant; this checks the other direction: no duplicates, and
        // a character for each (so `the_switch_table_still_covers_every_toggle_key`
        // can find it).
        for (i, k) in Die3dKey::ALL.iter().enumerate() {
            assert!(!Die3dKey::ALL[..i].contains(k), "{k:?} twice");
            assert!(
                (0u8..128).any(|c| key_to_die3d(char::from(c)) == Some(*k)),
                "{k:?} has no character"
            );
        }
        let mapped = (0u8..128)
            .filter(|&c| key_to_die3d(char::from(c)).is_some())
            .count();
        assert_eq!(mapped, Die3dKey::ALL.len());
    }

    #[test]
    fn the_switch_strip_falls_back_to_keyed_then_bare_then_a_cut() {
        // Lengths with the default switches: keyed is 80 characters and bare
        // 64, so a 70-column terminal (67 after the bar) lands on bare.
        let v = wide_label_view(150);
        let sw = v.switches();
        let keyed = sw
            .iter()
            .map(|s| format!("{} {}", s.key, s.state))
            .collect::<Vec<_>>()
            .join(SEP);
        let bare = sw.iter().map(|s| s.state).collect::<Vec<_>>().join(SEP);
        assert_eq!((keyed.chars().count(), bare.chars().count()), (80, 64));
        let at = |cols: usize| {
            let v = wide_label_view(cols);
            let s = strip(&v);
            assert!(s.chars().count() <= cols, "{cols}: {s}");
            s.trim_start_matches("\u{255a}\u{2550} ").to_string()
        };
        // Exactly wide enough for the keyed form.
        assert_eq!(at(80 + BAR_WIDTH), keyed);
        // About 70 columns: the keyed form no longer fits, the bare one does.
        assert_eq!(at(70), bare);
        assert_eq!(at(64 + BAR_WIDTH), bare);
        // One column short of bare: the last resort cuts at a character and
        // marks the cut with an ellipsis.
        let cut = at(63 + BAR_WIDTH);
        assert_eq!(cut.chars().count(), 63);
        assert!(cut.ends_with('\u{2026}'), "{cut}");
        assert!(bare.starts_with(cut.trim_end_matches('\u{2026}')), "{cut}");
    }

    #[test]
    fn opaque_mode_fills_pure_black() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.handle_key(Die3dKey::ToggleTransparent, 0.0);
        // Clouds off: they sit on the black fill and would cover corners.
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
        let lines = v.render(0.0);
        let black = Some(Color::Rgb(0, 0, 0));
        // The scene corners are empty floor: a flat cell on pure black.
        let scene_rows = 42 - FOOTER_ROWS;
        for row in [0, scene_rows - 1] {
            let spans = &lines[row].spans;
            for span in [spans.first().unwrap(), spans.last().unwrap()] {
                assert_eq!(span.style.bg, black, "row {row}: {span:?}");
            }
        }
        // In pixels: every uncovered-by-the-scene pixel is (0, 0, 0).
        let (buf, _) = v.scene_pixels(0.0).unwrap();
        assert_eq!(buf.get(0, 0), [0, 0, 0]);
        assert!(buf.covered(0, 0));
        // Half-block too.
        v.handle_key(Die3dKey::ToggleBraille, 0.0);
        let lines = v.render(0.0);
        assert_eq!(lines[0].spans[0].style.bg, black, "{:?}", lines[0].spans[0]);
    }

    /// The two frames a transition at `now` mixes: the scene from the old
    /// camera and the scene from the new one, each at the same `now`.
    fn endpoints(v: &Die3dView, now: f64) -> (PixelBuf, PixelBuf) {
        let (from, to) = v.director.cameras();
        let (s0, s1) = v.transition_scenes(now);
        (
            v.scene_with(now, s0, Aim::At(from)).unwrap().0,
            v.scene_with(now, s1, Aim::At(to)).unwrap().0,
        )
    }

    #[test]
    fn a_spin_frame_midway_differs_from_both_endpoints_and_stays_inside() {
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.handle_key(Die3dKey::CycleTransition, 0.0);
        v.handle_key(Die3dKey::CycleTransition, 0.0);
        v.handle_key(Die3dKey::CycleTransition, 0.0);
        assert_eq!(v.director.transition_mode(), TransitionMode::Spin);
        // Cut until the camera turns (a cut can also change only the pitch).
        let mut start = 10.0;
        v.handle_key(Die3dKey::CutNow, start);
        while {
            let (from, to) = v.director.cameras();
            from.quarter_turns == to.quarter_turns
        } {
            start += 10.0;
            v.handle_key(Die3dKey::CutNow, start);
        }
        assert_eq!(v.director.transition_kind(), Some(TransitionKind::Spin));
        let mid = start + TransitionKind::Spin.secs() / 2.0;
        let (old, new) = endpoints(&v, mid);
        let frame = v.frame(mid).unwrap().0;
        assert_ne!(frame.rgb, old.rgb);
        assert_ne!(frame.rgb, new.rgb);
        // Pinned to `lerp_view`: the frame is the scene projected part way
        // between the two cameras, at the transition's progress.
        let (from, to) = v.director.cameras();
        let p = v.director.progress(mid).unwrap();
        let between = v
            .scene_with(mid, v.switcher.scene_at(mid), Aim::Between(from, to, p))
            .unwrap()
            .0;
        assert_eq!(frame.rgb, between.rgb);
        // The frames along the way move: a spin is animated, not a cut. 15
        // frames a tenth of a second apart are mostly distinct.
        let mut frames: Vec<Vec<[u8; 3]>> = (1..16)
            .map(|k| v.frame(start + k as f64 * 0.1).unwrap().0.rgb)
            .collect();
        frames.dedup();
        assert!(frames.len() >= 10, "{} distinct of 15", frames.len());
        assert_eq!(v.render(mid).len(), 42);

        // Every preset to every other, part way along, stays inside the
        // view's own buffer (300 x 160 pixels: 40 braille rows above the
        // footer) for a one- and a two-die board and four boards of two dies.
        //
        // All 36 pairs at every eighth of the way are checked without
        // drawing: the grid corners with the allowance `fit_pixels` makes for
        // a block (`scene::corner_outside`). Drawing them all took about
        // 19 s; real pictures are drawn for a few pairs below.
        let (pw, ph) = (300i32, 160i32);
        let grids = [
            ("1 die", board_grid(1)),
            ("2 dies", board_grid(2)),
            ("4 x 2", super_grid(&[2, 2, 2, 2]).0),
        ];
        for (what, grid) in grids {
            let base = fit_pixels(pw, ph, grid).unwrap();
            for from in camera::PRESETS {
                for to in camera::PRESETS {
                    for i in 0..=8 {
                        let iso = Camera::lerp_view(&from, &to, i as f32 / 8.0, &base.iso, grid);
                        let out = scene::corner_outside(&iso, grid, pw, ph);
                        assert!(out.is_none(), "{what} {from:?} -> {to:?} at {i}/8: {out:?}");
                    }
                }
            }
        }
        // Drawn: pairs that turn one quarter each way, turn a half, and
        // change pitch while turning, at a quarter, half and three quarters.
        let p = camera::PRESETS;
        let pairs = [
            (p[0], p[1]),
            (p[3], p[0]),
            (p[1], p[3]),
            (p[4], p[2]),
            (p[2], p[5]),
            (p[5], p[4]),
        ];
        const PAD: i32 = 80;
        let one: Vec<ChipInputs> = v.chips[0].clone();
        let two = vec![one[0].clone(), one[0].clone()];
        let four: Vec<Vec<ChipInputs>> = (0..4).map(|_| two.clone()).collect();
        let draws: [(&str, (usize, usize), &dyn Fn(&mut PixelBuf, &SceneFit)); 3] = [
            ("1 die", board_grid(1), &|buf, f| {
                draw_board_scene(buf, &one, f, 1.3, Detail::Full)
            }),
            ("2 dies", board_grid(2), &|buf, f| {
                draw_board_scene(buf, &two, f, 1.3, Detail::Full)
            }),
            ("4 x 2", super_grid(&[2, 2, 2, 2]).0, &|buf, f| {
                draw_super_scene(buf, &four, f, 1.3)
            }),
        ];
        for (what, grid, draw) in draws {
            let base = fit_pixels(pw, ph, grid).unwrap();
            for (from, to) in pairs {
                for i in [2, 4, 6] {
                    let mut iso = Camera::lerp_view(&from, &to, i as f32 / 8.0, &base.iso, grid);
                    iso.ox += PAD;
                    iso.oy += PAD;
                    let f = SceneFit { iso, ..base };
                    let mut buf = PixelBuf::new((pw + 2 * PAD) as usize, (ph + 2 * PAD) as usize);
                    draw(&mut buf, &f);
                    for y in 0..buf.h {
                        for x in 0..buf.w {
                            let (xi, yi) = (x as i32 - PAD, y as i32 - PAD);
                            assert!(
                                !buf.covered(x, y)
                                    || (0..pw).contains(&xi) && (0..ph).contains(&yi),
                                "{what} {from:?} -> {to:?} at {i}/8: pixel {xi},{yi}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Pixels where `frame` matches `a` and not `b`.
    fn only_from(frame: &PixelBuf, a: &PixelBuf, b: &PixelBuf) -> usize {
        (0..frame.rgb.len())
            .filter(|&i| {
                let px = |p: &PixelBuf| (p.rgb[i], p.cover[i]);
                px(frame) == px(a) && px(frame) != px(b)
            })
            .count()
    }

    #[test]
    fn a_dissolve_frame_midway_contains_pixels_of_both_frames() {
        // A camera-only cut.
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.director.set_transition_mode(TransitionMode::Dissolve);
        v.handle_key(Die3dKey::CutNow, 10.0);
        let mid = 10.0 + TransitionKind::Dissolve.secs() / 2.0;
        let (old, new) = endpoints(&v, mid);
        let frame = v.frame(mid).unwrap().0;
        assert!(only_from(&frame, &old, &new) > 500, "old pixels");
        assert!(only_from(&frame, &new, &old) > 500, "new pixels");
        assert_eq!(v.render(mid).len(), 42);

        // A scene change: the old frame is every board, the new one board 2.
        let mut b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.director.set_transition_mode(TransitionMode::Dissolve);
        b.set_power(2, 90.0);
        v.update(&b, 1.0);
        assert_eq!(v.director.transition_kind(), Some(TransitionKind::Dissolve));
        // Past the midpoint, after the switch has been committed.
        let late = 1.0 + 0.7;
        v.update(&b, late);
        assert_eq!(v.switcher.scene_at(late), Scene::Board(1));
        assert_eq!(v.transition_scenes(late), (Scene::All, Scene::Board(1)));
        let (old, new) = endpoints(&v, late);
        assert_eq!(
            v.scene_with(late, Scene::All, Aim::At(v.director.cameras().0))
                .unwrap()
                .1,
            Shown::All
        );
        let frame = v.frame(late).unwrap().0;
        assert!(only_from(&frame, &old, &new) > 200, "old scene pixels");
        assert!(only_from(&frame, &new, &old) > 200, "new scene pixels");
        // Once it ends only the new scene is left.
        let after = 1.0 + TransitionKind::Dissolve.secs() + 0.01;
        assert_eq!(
            v.frame(after).unwrap().0.rgb,
            v.scene_pixels(after).unwrap().0.rgb
        );
    }

    /// Row 1 of the footer: the line above the switch strip.
    fn status_row(lines: &[Line<'static>]) -> String {
        text(lines)[lines.len() - 2].clone()
    }

    /// A view in a fixed transition mode (cut by hand at `at`).
    fn cut_view(b: &Fake, mode: TransitionMode, at: f64) -> Die3dView {
        let mut v = Die3dView::new(150, 42);
        v.update(b, 0.0);
        v.director.set_transition_mode(mode);
        v.handle_key(Die3dKey::CutNow, at);
        v
    }

    #[test]
    fn the_footer_shows_the_camera_caption() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // The first camera is preset 0: quarter turn 0 at Iso pitch.
        let row = status_row(&v.render(0.0));
        assert!(row.contains("view: front-right"), "{row}");
        // After the scene name and the estimate label.
        let scene = row.find("all boards").unwrap();
        let est = row.find(ESTIMATE_LABEL).unwrap();
        let cap = row.find("view: ").unwrap();
        assert!(scene < est && est < cap, "{row}");
        // After a cut has finished, it names the new camera.
        v.director.set_transition_mode(TransitionMode::Chaos);
        v.handle_key(Die3dKey::CutNow, 10.0);
        let row = status_row(&v.render(11.0));
        let want = format!("view: {}", v.director.camera().side_name());
        assert!(row.contains(&want), "{want}: {row}");
        assert!(!row.contains("->"), "{row}");
    }

    #[test]
    fn the_caption_shows_from_and_to_during_a_transition() {
        let b = two_boards();
        for mode in [
            TransitionMode::Chaos,
            TransitionMode::Dissolve,
            TransitionMode::Spin,
        ] {
            let v = cut_view(&b, mode, 10.0);
            let (from, to) = v.director.cameras();
            let want = format!("view: {} -> {}", from.side_name(), to.side_name());
            // Before and after the swap alike: the whole transition.
            for dt in [
                0.05,
                0.5,
                v.director.transition_kind().unwrap().secs() - 0.05,
            ] {
                let row = status_row(&v.render(10.0 + dt));
                assert!(row.contains(&want), "{mode:?} +{dt}: {row}");
            }
            let row = status_row(&v.render(12.0));
            assert!(row.contains(&format!("view: {}", to.side_name())), "{row}");
            assert!(!row.contains("->"), "{row}");
        }
    }

    #[test]
    fn the_caption_is_dropped_before_the_estimate_label_when_narrow() {
        // A transition caption: `view: <from> -> <to>`.
        let mut v = wide_label_view(150);
        v.director.set_transition_mode(TransitionMode::Spin);
        v.handle_key(Die3dKey::CutNow, 10.0);
        let now = 10.1;
        let (from, to) = v.director.cameras();
        let caption = format!("view: {} -> {}", from.side_name(), to.side_name());
        // At every width the estimate label stays, and the caption is either
        // there whole or absent: never a cut-off part of it.
        let (mut shown, mut dropped) = (0, 0);
        for cols in 80..=150 {
            v.cols = cols;
            let row = status_text(&v, now);
            assert!(row.contains(ESTIMATE_LABEL), "{cols}: {row}");
            assert!(row.chars().count() <= cols, "{cols}: {row}");
            if row.contains(&caption) {
                shown += 1;
            } else {
                assert!(!row.contains("view:"), "{cols}: {row}");
                dropped += 1;
            }
        }
        // Not vacuous: the sweep crosses the width where it stops fitting.
        assert!(shown > 0 && dropped > 0, "{shown} shown, {dropped} dropped");
    }

    /// Footer row 1 as text, for the shown board 0, at `now`.
    fn status_text(v: &Die3dView, now: f64) -> String {
        v.footer(now, Some(Shown::Board(0)))[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect()
    }

    #[test]
    fn the_caption_is_never_cut_and_the_note_never_left_as_a_stub() {
        // The mock's devices: a Grayskull and a Wormhole not drawn, one
        // Blackhole drawn. Cuts by hand collect resting and transition
        // captions, short and long (`top-down, ...` is the longest name).
        let b = Fake::boards(&["e150", "n150", "p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let note = v.undrawn_note();
        let mut kinds = std::collections::HashSet::new();
        for k in 0..12 {
            let at = 10.0 * (k + 1) as f64;
            v.handle_key(Die3dKey::CutNow, at);
            for now in [at + 0.1, at + 5.0] {
                let caption = v.caption(now);
                kinds.insert((caption.contains("->"), caption.contains("top-down")));
                // From the narrowest row the view allows (100 columns) up.
                for cols in 100..=163 {
                    v.cols = cols;
                    let row = status_text(&v, now);
                    let what = format!("{cols} cols, {caption:?}: {row:?}");
                    assert!(row.chars().count() <= cols, "{what}");
                    assert!(row.contains(ESTIMATE_LABEL), "{what}");
                    assert!(row.contains(&caption) || !row.contains("view:"), "{what}");
                    for stub in [" \u{b7}\u{2026}", " \u{b7} \u{2026}", " \u{b7}", " \u{b7} "] {
                        assert!(!row.ends_with(stub), "stub {stub:?}: {what}");
                    }
                    // The note is whole, absent, or cut after at least one
                    // of its own characters.
                    if let Some(i) = row.find("chip 0") {
                        let tail = &row[i..];
                        assert!(tail == note || tail.ends_with('\u{2026}'), "{what}");
                    }
                }
                v.cols = 150;
            }
        }
        // Short and long, resting and transition captions were all seen.
        for want in [(false, false), (false, true), (true, false), (true, true)] {
            assert!(kinds.contains(&want), "{want:?} not seen: {kinds:?}");
        }
    }

    /// Every label in the first `rows` rows: (row, start column, text) for
    /// each `BH<n>`, with its ` <w>W` power when one follows.
    fn labels_in(lines: &[Line<'static>], rows: usize) -> Vec<(usize, usize, String)> {
        let digits = |c: &[char], mut j: usize| {
            while c.get(j).is_some_and(char::is_ascii_digit) {
                j += 1;
            }
            j
        };
        let mut found = Vec::new();
        for (r, row) in text(lines).iter().enumerate().take(rows) {
            let c: Vec<char> = row.chars().collect();
            let mut i = 0;
            while i + 2 < c.len() {
                if !(c[i] == 'B' && c[i + 1] == 'H' && c[i + 2].is_ascii_digit()) {
                    i += 1;
                    continue;
                }
                let mut end = digits(&c, i + 2);
                if c.get(end) == Some(&' ') {
                    let w = digits(&c, end + 1);
                    if w > end + 1 && c.get(w) == Some(&'W') {
                        end = w + 1;
                    }
                }
                found.push((r, i, c[i..end].iter().collect()));
                i = end;
            }
        }
        found
    }

    #[test]
    fn each_die_gets_a_flag_with_its_name_and_power() {
        // One p300 board: two dies.
        let mut b = Fake::boards(&["p300"; 2], Some(TDP));
        b.set_power(0, 44.0);
        b.set_power(1, 51.6);
        for braille in [true, false] {
            let mut v = Die3dView::new(150, 42);
            v.update(&b, 0.0);
            // The history panel lists the chip names too: off, so only the
            // flags are found.
            v.handle_key(Die3dKey::ToggleHistory, 0.0);
            if !braille {
                v.handle_key(Die3dKey::ToggleBraille, 0.0);
            }
            let lines = v.render(0.0);
            let found = labels_in(&lines, 40);
            let texts: Vec<&str> = found.iter().map(|f| f.2.as_str()).collect();
            assert_eq!(texts, ["BH0 44W", "BH1 52W"], "braille={braille}");
            // Each text is on a flag: the cell before it is the pole's corner
            // (or the padding after it) and a pin `●` stands on its pole.
            let rows = text(&lines);
            let all: String = rows.concat();
            assert_eq!(
                all.matches('●').count(),
                2,
                "one pin each (braille={braille})"
            );
            for (r, c, _) in &found {
                let row: Vec<char> = rows[*r].chars().collect();
                let corner = row[c.saturating_sub(2)];
                assert!(
                    matches!(corner, '┌' | '└')
                        || row.get(c + 7 + 1) == Some(&'┐')
                        || row.get(c + 7 + 1) == Some(&'┘'),
                    "{r},{c}: {corner} (braille={braille})"
                );
            }
        }
        // Without a power reading the label is the name alone.
        let mut b = Fake::boards(&["p150"], Some(TDP));
        b.telem[0].power = None;
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        let found = labels_in(&v.render(0.0), 40);
        assert_eq!(
            found.iter().map(|f| f.2.as_str()).collect::<Vec<_>>(),
            ["BH0"]
        );
    }

    #[test]
    fn labels_are_hidden_during_a_transition() {
        let b = two_boards();
        for mode in [
            TransitionMode::Chaos,
            TransitionMode::Dissolve,
            TransitionMode::Spin,
        ] {
            let mut v = cut_view(&b, mode, 10.0);
            // The history panel stays through a transition and lists the
            // chip names: off, so only the labels are found.
            v.handle_key(Die3dKey::ToggleHistory, 0.0);
            assert!(!labels_in(&v.render(9.9), 40).is_empty(), "{mode:?} before");
            let secs = v.director.transition_kind().unwrap().secs();
            for dt in [0.0, 0.2, secs / 2.0, secs - 0.01] {
                let found = labels_in(&v.render(10.0 + dt), 40);
                assert!(found.is_empty(), "{mode:?} +{dt}: {found:?}");
            }
            assert!(
                !labels_in(&v.render(10.0 + secs), 40).is_empty(),
                "{mode:?} after"
            );
        }
    }

    #[test]
    fn labels_never_overwrite_the_footer_rows_or_leave_the_view() {
        // Through the view: every size from the minimum up, both cell modes,
        // one board and four, after cuts to every preset. The footer rows are
        // exactly the footer, every row is the view's width, and the labels
        // found are all in the scene rows.
        let one = Fake::boards(&["p300"; 2], Some(TDP));
        let four = Fake::boards(&["p300"; 8], Some(TDP));
        for (c, r) in [(100, 30), (101, 31), (120, 35), (150, 42), (220, 60)] {
            for b in [&one, &four] {
                for braille in [true, false] {
                    let mut v = Die3dView::new(c, r);
                    v.update(b, 0.0);
                    v.director.set_auto(false);
                    if !braille {
                        v.handle_key(Die3dKey::ToggleBraille, 0.0);
                    }
                    let (mut labelled, mut drawn) = (0, 0);
                    for k in 0..6 {
                        let now = 10.0 * k as f64;
                        if k > 0 {
                            v.handle_key(Die3dKey::CutNow, now - 5.0);
                        }
                        let lines = v.render(now);
                        assert_eq!(lines.len(), r);
                        let t = text(&lines);
                        let scene_rows = r - FOOTER_ROWS;
                        // The footer rows are the footer's: its two bars.
                        assert!(t[scene_rows].starts_with("\u{2551} "), "{}", t[scene_rows]);
                        assert!(t[r - 1].starts_with("\u{255a}\u{2550} "), "{}", t[r - 1]);
                        // A scene row is the view's width, or empty below
                        // the no-fit hint (half-block at the smallest sizes).
                        let hint_only = t[0] == no_fit_hint(braille);
                        for (i, row) in t[..scene_rows].iter().enumerate() {
                            let n = row.chars().count();
                            let ok = n == c || (hint_only && (i == 0 || n == 0));
                            assert!(ok, "{c} x {r} row {i}: {n}");
                        }
                        // Every label found anywhere is on a scene row.
                        let all = labels_in(&lines, r);
                        assert!(all.iter().all(|l| l.0 < scene_rows), "{c} x {r}: {all:?}");
                        labelled += all.len();
                        drawn += !hint_only as usize;
                    }
                    // Not vacuous: wherever a scene was drawn, labels were.
                    assert!(
                        drawn == 0 || labelled > 0,
                        "{c} x {r}: no label drawn at all"
                    );
                }
            }
        }

        // At the layer that writes: flags whose cells are on the footer rows,
        // past the right edge and hanging off the left edge.
        let width = 20;
        let scene_rows = 3;
        let mut lines: Vec<Line<'static>> = (0..scene_rows + FOOTER_ROWS)
            .map(|i| Line::from(Span::raw(format!("{i}").repeat(width))))
            .collect();
        let before = text(&lines);
        let style = Style::default().fg(Color::White);
        let flag = |pin: (i32, i32), rise: i32, left: bool, t: &str| {
            Some(Flag {
                pin,
                rise,
                left,
                text: t.to_string(),
            })
        };
        let flags = vec![
            // A pole standing on the footer rows.
            flag((5, scene_rows as i32 + 1), 3, false, "BH8 10W"),
            // A flag wholly past the right edge.
            flag((width as i32 + 3, 2), 1, false, "BH7"),
            // A flag flying left off the left edge.
            flag((3, 2), 1, true, "BH0 123W"),
            None,
        ];
        draw_flags(&mut lines, scene_rows, width, &flags, style, style);
        let after = text(&lines);
        assert_eq!(after[scene_rows..], before[scene_rows..], "footer rows");
        assert!(
            after.iter().all(|r| !r.contains("BH7")),
            "past the edge: {after:?}"
        );
        // The left-flying text is cut at column 0 (its last two characters
        // show), the pole's corner and padding follow, then the other flag.
        assert!(
            after[1].starts_with("3W \u{2510}1\u{250c} BH8 10W"),
            "{}",
            after[1]
        );
        // The pin and the pole's last cell above the footer.
        assert!(after[2].starts_with("222\u{25cf}2\u{2502}"), "{}", after[2]);
        for row in &after {
            assert_eq!(row.chars().count(), width, "{row}");
        }
    }

    #[test]
    fn flags_never_cover_a_drawn_cell_but_the_pin() {
        // Flags are written only on cells the scene left empty. Without the
        // clouds, the cells a flag changes were blank before it was written,
        // whatever the board count, cell mode, size or camera.
        let one = Fake::boards(&["p300"; 2], Some(TDP));
        let four = Fake::boards(&["p300", "p300", "p150", "p300"], Some(TDP));
        let mut flagged = 0;
        for (c, r) in [(100, 31), (120, 37), (150, 42), (220, 60)] {
            for b in [&one, &four] {
                for braille in [true, false] {
                    let mut v = Die3dView::new(c, r);
                    v.update(b, 0.0);
                    v.director.set_auto(false);
                    v.handle_key(Die3dKey::ToggleClouds, 0.0);
                    v.handle_key(Die3dKey::ToggleHistory, 0.0);
                    if !braille {
                        v.handle_key(Die3dKey::ToggleBraille, 0.0);
                    }
                    for k in 0..6 {
                        let now = 10.0 * k as f64;
                        if k > 0 {
                            v.handle_key(Die3dKey::CutNow, now - 5.0);
                        }
                        v.handle_key(Die3dKey::ToggleLabels, now);
                        let off = text(&v.render(now));
                        v.handle_key(Die3dKey::ToggleLabels, now);
                        let on = text(&v.render(now));
                        for (row, (a, b)) in off.iter().zip(&on).enumerate().take(r - FOOTER_ROWS) {
                            for (col, (x, y)) in a.chars().zip(b.chars()).enumerate() {
                                if x == y {
                                    continue;
                                }
                                flagged += 1;
                                let blank = x == ' ' || x == '\u{2800}';
                                // The pin and the pole stand on the board;
                                // only they may cover a drawn cell.
                                assert!(
                                    blank || y == '●' || y == '│',
                                    "{c}x{r} braille={braille} cut {k}: {y:?} over {x:?} at {row},{col}"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(flagged > 0, "no flag was ever drawn");
    }

    #[test]
    fn labels_stay_transparent_in_transparent_mode() {
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // The history panel's rows name the chip too, in its own style.
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        let label_span = |lines: &[Line<'static>]| {
            lines[..40]
                .iter()
                .flat_map(|l| l.spans.iter())
                .find(|s| s.content.contains("BH0"))
                .cloned()
                .expect("a label span")
        };
        let s = label_span(&v.render(0.0));
        assert_eq!(s.style.bg, Some(Color::Reset), "{s:?}");
        assert_eq!(s.style.fg, Some(rgb(GRAYSKULL.white)), "{s:?}");
        assert!(s.style.add_modifier.contains(Modifier::BOLD), "{s:?}");
        // Opaque: the label stands on the black fill.
        v.handle_key(Die3dKey::ToggleTransparent, 0.0);
        let s = label_span(&v.render(0.0));
        assert_eq!(s.style.bg, Some(rgb(OPAQUE_BG)), "{s:?}");
    }

    #[test]
    fn the_l_key_toggles_labels_and_the_footer_says_which() {
        assert_eq!(key_to_die3d('L'), Some(Die3dKey::ToggleLabels));
        assert_eq!(key_to_die3d('l'), None, "l is the TUI's legend key");
        assert!(Die3dKey::ToggleLabels.toggles());
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // The history panel lists the chip names too: off, so only the
        // labels are found.
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        let lines = v.render(0.0);
        assert!(footer(&lines).contains("L labels on"), "{}", footer(&lines));
        assert!(!labels_in(&lines, 40).is_empty());
        v.handle_key(Die3dKey::ToggleLabels, 0.0);
        let lines = v.render(0.0);
        assert!(
            footer(&lines).contains("L labels off"),
            "{}",
            footer(&lines)
        );
        assert!(labels_in(&lines, 40).is_empty());
        v.handle_key(Die3dKey::ToggleLabels, 0.0);
        assert!(!labels_in(&v.render(0.0), 40).is_empty());
    }

    #[test]
    fn a_hand_cut_while_a_scene_switch_is_pending_never_spins_or_jumps_mid_turn() {
        // The load rise in the super scene cuts and schedules a switch to
        // board 2 at that cut's midpoint. `c` before the midpoint starts a
        // new cut; the switch is the new cut's to cover.
        let (mut v, _) = switched_by_chip_2();
        let first = v.director.swapped_at().unwrap();
        assert_eq!(v.switcher.scene_at(first - 0.01), Scene::All);
        // In spin mode a camera-only hand cut would spin.
        v.director.set_transition_mode(TransitionMode::Spin);
        let at = 1.1;
        assert!(at < first);
        v.handle_key(Die3dKey::CutNow, at);
        assert_ne!(v.director.transition_kind(), Some(TransitionKind::Spin));
        // The scene changes once, at the new cut's swap, and the transition
        // knows both scenes for its whole length.
        let swap = v.director.swapped_at().unwrap();
        let secs = v.director.transition_kind().unwrap().secs();
        for i in 0..=40 {
            let now = at + secs * i as f64 / 40.0;
            let want = if now < swap {
                Scene::All
            } else {
                Scene::Board(1)
            };
            assert_eq!(v.switcher.scene_at(now), want, "+{}", now - at);
            if now < at + secs {
                assert_eq!(
                    v.transition_scenes(now),
                    (Scene::All, Scene::Board(1)),
                    "+{}",
                    now - at
                );
            }
        }
        // And in shuffle mode, over many hand cuts at that moment, never a spin.
        for seed in 0..40u64 {
            let mut b = two_boards();
            let mut v = Die3dView::new(150, 42);
            v.director = Director::new(seed);
            v.update(&b, 0.0);
            b.set_power(2, 90.0);
            v.update(&b, 1.0);
            // The load rise cut and scheduled the switch, still pending.
            assert_eq!(v.switcher.scene_at(1.1), Scene::All, "seed {seed}");
            assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1), "seed {seed}");
            v.handle_key(Die3dKey::CutNow, 1.1);
            assert_ne!(
                v.director.transition_kind(),
                Some(TransitionKind::Spin),
                "seed {seed}"
            );
        }
    }

    #[test]
    fn the_caption_outranks_the_not_drawn_note_during_a_cut() {
        // As in the mock: a Grayskull and a Wormhole the view does not draw,
        // and one Blackhole that it does.
        let b = Fake::boards(&["e150", "n150", "p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let note = v.undrawn_note();
        assert!(
            note.contains("Grayskull") && note.contains("Wormhole"),
            "{note}"
        );
        v.director.set_transition_mode(TransitionMode::Spin);
        v.handle_key(Die3dKey::CutNow, 10.0);
        let (from, to) = v.director.cameras();
        let caption = format!("view: {} -> {}", from.side_name(), to.side_name());
        let row = status_row(&v.render(10.3));
        assert!(row.chars().count() <= 150, "{row}");
        assert!(row.contains(&caption), "{row}");
        assert!(row.contains("view:") && row.contains("->"), "{row}");
        assert!(row.contains(ESTIMATE_LABEL), "{row}");
        // The note is the part that shortens: it starts after the caption
        // and is cut with an ellipsis.
        assert!(!row.contains(&note), "the whole note fits: {row}");
        assert!(row.ends_with('\u{2026}'), "{row}");
        let at_note = row.find("chip 0: Grayskull").expect("the note's start");
        assert!(row.find(&caption).unwrap() < at_note, "{row}");
        // At rest the shorter caption and the whole note both fit.
        let row = status_row(&v.render(20.0));
        assert!(row.contains("view: ") && row.contains(&note), "{row}");
    }

    #[test]
    fn the_x_key_cycles_the_transition_mode_and_the_footer_says_which() {
        assert_eq!(key_to_die3d('X'), Some(Die3dKey::CycleTransition));
        assert!(Die3dKey::CycleTransition.toggles());
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        for (want_state, want_mode) in [
            ("shuffle", TransitionMode::Shuffle),
            ("chaos", TransitionMode::Chaos),
            ("dissolve", TransitionMode::Dissolve),
            ("spin", TransitionMode::Spin),
            ("shuffle", TransitionMode::Shuffle),
        ] {
            assert_eq!(v.director.transition_mode(), want_mode);
            let f = footer(&v.render(0.0));
            assert!(f.contains(&format!("X transition {want_state}")), "{f}");
            v.handle_key(Die3dKey::CycleTransition, 0.0);
        }
    }

    /// A one-board (p150) view over `b`, auto-cuts off, so nothing but the
    /// test moves the camera.
    fn cloud_view(b: &Fake) -> Die3dView {
        let mut v = Die3dView::new(150, 42);
        v.update(b, 0.0);
        v.director.set_auto(false);
        v
    }

    #[test]
    fn the_offset_integrates_speed_so_a_speed_change_never_jumps() {
        let mut b = Fake::boards(&["p150"], Some(TDP));
        let mut v = cloud_view(&b);
        let mut offsets = Vec::new();
        // 20 s, ticks 1/10 s apart, an update and a frame each tick. No PCIe
        // reading for the first 10 s (the 0.8 px/s floor), then 5 GB/s.
        for i in 0..=200 {
            let now = i as f64 * 0.1;
            if i == 100 {
                b.pcie = Some(PcieBandwidth {
                    rx_bytes_per_sec: 4.0e9,
                    tx_bytes_per_sec: 1.0e9,
                });
            }
            v.update(&b, now);
            v.render(now);
            offsets.push((now, v.cloud_clock.get().offset_px()));
        }
        // Continuous: no tick moves the clouds more than one clock step at
        // the fastest drift (8 px/s for 0.25 s) and a pixel of rounding, and
        // none moves them back.
        for w in offsets.windows(2) {
            let step = w[1].1 - w[0].1;
            assert!(
                (0.0..=3.0).contains(&step),
                "jump of {step} px at {} s",
                w[1].0
            );
        }
        // The floor for the first 10 s: 8 px, less a pixel of rounding.
        let at = |t: f64| offsets.iter().find(|o| (o.0 - t).abs() < 1e-9).unwrap().1;
        assert!((7.0..=8.0).contains(&at(10.0)), "{}", at(10.0));
        // Then the speed for 5 GB/s, about 7.7 px/s, from where it was.
        let fast = Clouds::drift_px_per_s(Some(5.0e9)) as f64;
        let gained = at(20.0) - at(10.0);
        assert!((gained - fast * 10.0).abs() <= 2.0, "{gained} px in 10 s");
    }

    #[test]
    fn the_view_brightens_the_clouds_with_chip_load() {
        let mut b = Fake::boards(&["p300"; 2], Some(TDP));
        let mut v = cloud_view(&b);
        assert_eq!(v.cloud_clock.get().intensity(), 0.25);
        // The busiest chip sets the brightness: 90 W of 100 W is a load of
        // 0.9 while the load scale is still filling, so an intensity of 0.7.
        b.set_power(1, 90.0);
        v.update(&b, 0.5);
        // Taken up at the clock's next step, a quarter second on.
        v.render(0.6);
        assert_eq!(v.cloud_clock.get().intensity(), 0.25);
        v.render(0.75);
        let shown = v.cloud_clock.get().intensity();
        assert!((shown - 0.7).abs() < 1e-6, "{shown}");
    }

    #[test]
    fn clouds_never_paint_over_the_die() {
        for transparent in [true, false] {
            let b = two_boards();
            let mut v = cloud_view(&b);
            if !transparent {
                v.handle_key(Die3dKey::ToggleTransparent, 0.0);
            }
            v.render(0.0);
            v.render(30.0);
            assert!(v.cloud_clock.get().offset_px() > 0.0);
            let (on, _) = v.scene_pixels(30.0).unwrap();
            v.handle_key(Die3dKey::ToggleClouds, 30.0);
            let (off, _) = v.scene_pixels(30.0).unwrap();
            let mut sky = 0;
            for i in 0..on.rgb.len() {
                if transparent && off.cover[i] || !transparent && off.rgb[i] != OPAQUE_BG {
                    // Die, slab, packet: the same pixel with clouds on.
                    assert_eq!((on.rgb[i], on.cover[i]), (off.rgb[i], true), "pixel {i}");
                } else if on.cover[i] && on.rgb[i] != off.rgb[i] {
                    sky += 1;
                }
            }
            // The clouds were drawn somewhere, so the check above is not
            // vacuous.
            assert!(sky > 1000, "transparent={transparent}: {sky} cloud pixels");
        }
    }

    #[test]
    fn the_w_key_toggles_clouds_and_the_footer_says_which() {
        assert_eq!(key_to_die3d('W'), Some(Die3dKey::ToggleClouds));
        assert_eq!(key_to_die3d('w'), None);
        assert!(Die3dKey::ToggleClouds.toggles());
        let b = two_boards();
        let mut v = cloud_view(&b);
        assert!(v.show_clouds, "on by default");
        let f = footer(&v.render(0.0));
        assert!(f.contains("W clouds on"), "{f}");
        let with = v.scene_pixels(0.0).unwrap().0;
        v.handle_key(Die3dKey::ToggleClouds, 0.1);
        let f = footer(&v.render(0.1));
        assert!(f.contains("W clouds off"), "{f}");
        let without = v.scene_pixels(0.1).unwrap().0;
        let covered = |b: &PixelBuf| b.cover.iter().filter(|c| **c).count();
        assert!(covered(&with) > covered(&without) + 1000);
        v.handle_key(Die3dKey::ToggleClouds, 0.2);
        assert!(footer(&v.render(0.2)).contains("W clouds on"));
    }

    #[test]
    fn chip_labels_stay_readable_over_clouds() {
        for transparent in [true, false] {
            let b = two_boards();
            let mut v = cloud_view(&b);
            if !transparent {
                v.handle_key(Die3dKey::ToggleTransparent, 0.0);
            }
            let now = 12.0;
            let lines = v.render(now);
            let placed = v.frame(now).unwrap().2.unwrap();
            let flags = v.flag_layout(&placed, v.rows - FOOTER_ROWS);
            assert_eq!(flags.iter().flatten().count(), 4);
            let want = if transparent {
                Color::Reset
            } else {
                rgb(OPAQUE_BG)
            };
            for flag in flags.iter().flatten() {
                let text_row = flag.pin.1 - flag.rise;
                for ((col, row), ch) in flag.cells() {
                    let cells: Vec<(char, Style)> = lines[row as usize]
                        .spans
                        .iter()
                        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                        .collect();
                    let (got, style) = cells[col as usize];
                    assert_eq!(got, ch, "{}", flag.text);
                    assert_eq!(style.bg, Some(want), "{}", flag.text);
                    if row == text_row && flag.text.contains(ch) && ch != ' ' {
                        assert_eq!(style.fg, Some(rgb(GRAYSKULL.white)), "{}", flag.text);
                    }
                }
            }
        }
    }

    #[test]
    fn a_cloud_step_redraws_at_once_and_a_still_sky_does_not() {
        // No PCIe reading: the sky moves at the floor, a pixel per 1.25 s,
        // and no packet moves. Frames 1/120 s apart for 5 s.
        let b = Fake::boards(&["p150"], Some(TDP));
        let v = cloud_view(&b);
        let mut prev = (v.cloud_clock.get().offset_px(), text(&v.render(1.0)));
        let mut steps = 0;
        for i in 1..600 {
            let now = 1.0 + i as f64 / 120.0;
            let lines = text(&v.render(now));
            let offset = v.cloud_clock.get().offset_px();
            if offset != prev.0 {
                // A step is shown in the very frame it happens, even when
                // the last frame is less than 1/20 s old.
                assert_ne!(lines, prev.1, "the step at {now} s was not drawn");
                steps += 1;
            }
            prev = (offset, lines);
        }
        assert!((3..=5).contains(&steps), "{steps} steps in 5 s at 0.8 px/s");
        // Between steps the throttle is as it was: 20 frames drawn a second
        // (counted by the time of the last frame drawn, since a frame drawn
        // can equal the one before it). One step falls in this second, on a
        // frame that was due anyway.
        let drawn_at = |v: &Die3dView| v.cache.borrow().last.as_ref().unwrap().at;
        let mut drawn = 0;
        let mut last = drawn_at(&v);
        for i in 0..120 {
            v.render(10.0 + i as f64 / 120.0);
            drawn += (drawn_at(&v) != last) as usize;
            last = drawn_at(&v);
        }
        assert!(
            (19..=21).contains(&drawn),
            "{drawn} frames drawn in a second"
        );
    }

    /// The text of the history panel's rows in `lines`: `n` rows from
    /// [`PANEL_TOP`], each the [`history::PANEL_WIDTH`] cells from column
    /// [`PANEL_LEFT`], trailing spaces cut.
    fn panel_text(lines: &[Line<'static>], n: usize) -> Vec<String> {
        text(lines)
            .iter()
            .skip(PANEL_TOP)
            .take(n)
            .map(|row| {
                let cells: String = row
                    .chars()
                    .skip(PANEL_LEFT)
                    .take(history::PANEL_WIDTH)
                    .collect();
                cells.trim_end().to_string()
            })
            .collect()
    }

    /// Whether the panel's title is anywhere in `lines`.
    fn has_panel(lines: &[Line<'static>]) -> bool {
        text(lines).iter().any(|row| row.contains(history::TITLE))
    }

    #[test]
    fn the_panel_lists_each_blackhole_chip_with_its_latest_values() {
        // A p150 (drawn) and an n150 (not drawn). The PCIe rates are per
        // device in the fake; the box's total counts the drawn chip only.
        let mut b = Fake::boards(&["p150", "n150"], Some(TDP));
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 1.23e9,
            tx_bytes_per_sec: 3.1e8,
        });
        let mut v = Die3dView::new(150, 42);
        // Five samples, one a second: power 40..50 W, 52..56 C. The ramp
        // spans 10 W, more than the power sparkline's 5 W minimum span, so
        // it fills the sparkline's full height.
        for t in 0..5 {
            b.set_power(0, 40.0 + 2.5 * t as f32);
            b.telem[0].asic_temperature = Some(52.0 + t as f32);
            v.update(&b, t as f64);
        }
        let rows = panel_text(&v.render(4.0), 5);
        assert_eq!(rows[0], history::TITLE, "{rows:#?}");
        // The chip: its name, the rising power as a sparkline whose newest
        // sample is the highest block, then the latest watts and degrees.
        assert!(rows[1].starts_with("BH0"), "{rows:#?}");
        assert!(
            rows[1].contains("\u{2581}\u{2583}\u{2585}\u{2586}\u{2588}"),
            "{rows:#?}"
        );
        assert!(rows[1].ends_with("50W  56\u{b0}"), "{rows:#?}");
        // The box's PCIe, in then out.
        assert!(rows[2].starts_with("PCIe \u{25bc}"), "{rows:#?}");
        assert!(rows[2].ends_with("1.23 GB/s"), "{rows:#?}");
        assert!(rows[3].trim_start().starts_with('\u{25b2}'), "{rows:#?}");
        assert!(rows[3].ends_with("310 MB/s"), "{rows:#?}");
        // Nothing for the chip the view does not draw, and nothing more.
        assert!(rows.iter().all(|r| !r.contains("WH")), "{rows:#?}");
        assert_eq!(rows[4], "", "{rows:#?}");
        // At most six chips: eight p300 chips list BH0 to BH5.
        let b = Fake::boards(&["p300"; 8], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let rows = v.history.panel_rows();
        assert_eq!(rows.len(), 1 + 6 + 2, "{rows:#?}");
        for (i, row) in rows[1..7].iter().enumerate() {
            assert!(row.starts_with(&format!("BH{i} ")), "{rows:#?}");
        }
    }

    #[test]
    fn the_panel_is_hidden_below_120_columns_or_36_rows_and_when_off() {
        let b = Fake::boards(&["p150"], Some(TDP));
        for (c, r, shown) in [
            (100, 30, false),
            (119, 42, false),
            (150, 35, false),
            (120, 36, true),
            (150, 42, true),
        ] {
            let mut v = Die3dView::new(c, r);
            v.update(&b, 0.0);
            assert_eq!(has_panel(&v.render(0.0)), shown, "{c} x {r}");
            if shown {
                v.handle_key(Die3dKey::ToggleHistory, 0.0);
                assert!(!has_panel(&v.render(0.0)), "{c} x {r} with H off");
            }
        }
    }

    #[test]
    fn the_panel_never_touches_the_footer_rows_or_leaves_the_scene_area() {
        // Through the view: two views alike but for `H`, rendered at the same
        // times, at rest and through each kind of cut. Every cell that
        // differs is in the panel's rectangle on a scene row, or in the
        // footer's switch row, which names the H state.
        let mut one = Fake::boards(&["p300"; 2], Some(TDP));
        let mut four = Fake::boards(&["p300"; 8], Some(TDP));
        for b in [&mut one, &mut four] {
            b.pcie = Some(PcieBandwidth {
                rx_bytes_per_sec: 2.0e7,
                tx_bytes_per_sec: 1.3e7,
            });
        }
        let cells = |lines: &[Line<'static>]| -> Vec<Vec<(char, Style)>> {
            lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                        .collect()
                })
                .collect()
        };
        for (c, r) in [(120, 36), (121, 37), (150, 42), (220, 60)] {
            for b in [&one, &four] {
                for braille in [true, false] {
                    let mut views: Vec<Die3dView> = [true, false]
                        .into_iter()
                        .map(|on| {
                            let mut v = Die3dView::new(c, r);
                            v.update(b, 0.0);
                            v.update(b, 1.0);
                            v.director.set_auto(false);
                            if !braille {
                                v.handle_key(Die3dKey::ToggleBraille, 1.0);
                            }
                            if !on {
                                v.handle_key(Die3dKey::ToggleHistory, 1.0);
                            }
                            v
                        })
                        .collect();
                    let panel = views[0].history.panel_rows();
                    let width = panel.iter().map(|p| p.chars().count()).max().unwrap();
                    let scene_rows = r - FOOTER_ROWS;
                    let mut in_panel = 0;
                    for (k, mode) in [
                        TransitionMode::Chaos,
                        TransitionMode::Dissolve,
                        TransitionMode::Spin,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        // At rest half a second before the cut, then 0.3 s
                        // into it.
                        let cut = 2.0 + 3.0 * k as f64;
                        let mut times = vec![cut - 0.5];
                        times.push(cut + 0.3);
                        for (i, now) in times.into_iter().enumerate() {
                            if i == 1 {
                                for v in views.iter_mut() {
                                    v.director.set_transition_mode(mode);
                                    v.handle_key(Die3dKey::CutNow, cut);
                                }
                                assert!(views[0].director.progress(now).is_some());
                            }
                            let frames: Vec<_> = views.iter().map(|v| v.render(now)).collect();
                            let (on, off) = (cells(&frames[0]), cells(&frames[1]));
                            assert_eq!(on.len(), r);
                            for (row, (a, z)) in on.iter().zip(&off).enumerate() {
                                // Scene rows are the view's width; the
                                // footer rows are their text, never wider.
                                for n in [a.len(), z.len()] {
                                    let ok = if row < scene_rows { n == c } else { n <= c };
                                    assert!(ok, "{c} x {r} row {row} at {now}: {n}");
                                }
                                for (col, (x, y)) in a.iter().zip(z).enumerate() {
                                    if x == y {
                                        continue;
                                    }
                                    let inside = (PANEL_TOP..PANEL_TOP + panel.len())
                                        .contains(&row)
                                        && (PANEL_LEFT..PANEL_LEFT + width).contains(&col)
                                        && row < scene_rows;
                                    assert!(
                                        inside || row == r - 1,
                                        "{c} x {r} at {now}: cell {row},{col} changed by the panel"
                                    );
                                    in_panel += inside as usize;
                                }
                            }
                            // The footer's status row is the same with or
                            // without the panel; its switch row starts with
                            // the bar either way.
                            let (t_on, t_off) = (text(&frames[0]), text(&frames[1]));
                            assert_eq!(t_on[scene_rows], t_off[scene_rows]);
                            assert!(t_on[r - 1].starts_with("\u{255a}\u{2550} "));
                        }
                    }
                    assert!(in_panel > 0, "{c} x {r}: the panel drew nothing");
                }
            }
        }

        // At the layer that writes: a panel taller than the scene rows and
        // wider than the view.
        let width = 20;
        let scene_rows = 3;
        let mut lines: Vec<Line<'static>> = (0..scene_rows + FOOTER_ROWS)
            .map(|i| Line::from(Span::raw(format!("{i}").repeat(width))))
            .collect();
        let before = text(&lines);
        let rows: Vec<String> = (0..9)
            .map(|i| format!("panel row {i} \u{2581}\u{2588}").repeat(3))
            .collect();
        draw_panel(&mut lines, scene_rows, width, &rows, Style::default());
        let after = text(&lines);
        assert_eq!(after[scene_rows..], before[scene_rows..], "footer rows");
        assert_eq!(after[0], before[0], "the row above the panel");
        for row in &after {
            assert_eq!(row.chars().count(), width, "{row}");
        }
        // Clipped at the right edge, after the left margin.
        let want: String = rows[0].chars().take(width - PANEL_LEFT).collect();
        assert_eq!(after[1], format!("11{want}"));
    }

    #[test]
    fn flags_win_over_the_panel_where_they_overlap() {
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        let scene_rows = 40;
        let mut lines: Vec<Line<'static>> = (0..42)
            .map(|_| Line::from(Span::raw(" ".repeat(150))))
            .collect();
        // A flag whose text row is the panel's title row, its pole four cells
        // in: the corner is at +4, one space, then the text from +6.
        let label = "BH9 99W";
        let flag = Flag {
            pin: (PANEL_LEFT as i32 + 4, PANEL_TOP as i32 + 3),
            rise: 3,
            left: false,
            text: label.to_string(),
        };
        v.overlay(&mut lines, scene_rows, &[Some(flag)]);
        let row: Vec<(char, Style)> = lines[PANEL_TOP]
            .spans
            .iter()
            .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
            .collect();
        let shown: String = row[PANEL_LEFT..PANEL_LEFT + 14]
            .iter()
            .map(|c| c.0)
            .collect();
        assert!(shown.starts_with("hist\u{250c} BH9 99W"), "{shown}");
        for (i, (_, style)) in row[PANEL_LEFT + 6..PANEL_LEFT + 13].iter().enumerate() {
            assert_eq!(style.fg, Some(rgb(GRAYSKULL.white)), "text cell {i}");
        }
        assert_eq!(row[PANEL_LEFT + 4].1.fg, Some(rgb(GRAYSKULL.eth)), "corner");
        for at in [PANEL_LEFT, PANEL_LEFT + 1, PANEL_LEFT + 2] {
            assert_eq!(row[at].1.fg, Some(rgb(PANEL_FG)), "panel cell {at}");
        }
    }

    #[test]
    fn the_panel_background_is_reset_when_transparent_and_black_when_opaque() {
        let mut b = Fake::boards(&["p150"], Some(TDP));
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 2.3e7,
            tx_bytes_per_sec: 1.3e7,
        });
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.update(&b, 1.0);
        let rows = v.history.panel_rows();
        for (transparent, bg) in [(true, Color::Reset), (false, rgb(OPAQUE_BG))] {
            if !transparent {
                v.handle_key(Die3dKey::ToggleTransparent, 1.0);
            }
            let lines = v.render(1.0);
            for (i, want) in rows.iter().enumerate() {
                let row: Vec<(char, Style)> = lines[PANEL_TOP + i]
                    .spans
                    .iter()
                    .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                    .collect();
                for (j, ch) in want.chars().enumerate() {
                    let (got, style) = row[PANEL_LEFT + j];
                    assert_eq!(got, ch, "row {i} col {j}");
                    assert_eq!(style.fg, Some(rgb(PANEL_FG)), "row {i} col {j}");
                    assert_eq!(style.bg, Some(bg), "row {i} col {j}");
                    assert!(
                        !style.add_modifier.contains(Modifier::BOLD),
                        "row {i} col {j}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_h_key_toggles_history_and_the_footer_says_which() {
        assert_eq!(key_to_die3d('H'), Some(Die3dKey::ToggleHistory));
        assert_eq!(key_to_die3d('h'), None, "h is the TUI's");
        assert!(Die3dKey::ToggleHistory.toggles());
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        assert!(v.show_history, "on by default");
        let lines = v.render(0.0);
        assert!(
            footer(&lines).contains("H history on"),
            "{}",
            footer(&lines)
        );
        assert!(has_panel(&lines));
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        let lines = v.render(0.0);
        assert!(
            footer(&lines).contains("H history off"),
            "{}",
            footer(&lines)
        );
        assert!(!has_panel(&lines));
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        assert!(has_panel(&v.render(0.0)));
    }

    #[test]
    fn a_new_sample_is_drawn_at_once_and_a_hidden_panel_forces_no_frame() {
        let rate = |rx: f64| {
            Some(PcieBandwidth {
                rx_bytes_per_sec: rx,
                tx_bytes_per_sec: 1.0e7,
            })
        };
        let mut b = Fake::boards(&["p150"], Some(TDP));
        b.pcie = rate(1.0e8);
        // Clouds off: their steps also force frames, and would blur this.
        let mut v = cloud_view(&b);
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
        v.update(&b, 1.0);
        let before = v.render(1.98);
        assert!(panel_text(&before, 3)[2].ends_with("100 MB/s"));
        // A new sample 0.02 s after the last frame, well inside the 1/20 s
        // the throttle would otherwise hold that frame for.
        b.pcie = rate(1.2e8);
        v.update(&b, 2.0);
        let after = v.render(2.0);
        let rows = panel_text(&after, 3);
        assert!(rows[2].ends_with("120 MB/s"), "{rows:#?}");
        // An unchanged panel forces nothing: the same frame inside 1/20 s.
        assert_eq!(text(&v.render(2.03)), text(&after));
        // With the panel off a sample is taken but forces no frame.
        v.handle_key(Die3dKey::ToggleHistory, 2.5);
        let hidden = text(&v.render(2.97));
        let revision = v.history.revision();
        b.pcie = rate(1.4e8);
        v.update(&b, 3.0);
        assert_eq!(v.history.revision(), revision + 1, "the sample was taken");
        assert_eq!(text(&v.render(3.0)), hidden);
    }
}
