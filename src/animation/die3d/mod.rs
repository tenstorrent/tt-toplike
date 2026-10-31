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
//! minute of measured power and PCIe traffic and shows each chip's latest
//! temperature. This file holds the
//! view that ties them together: it reads the backend in `update`, answers
//! keys in `handle_key` and draws in `render`.
//!
//! Time is `now`, seconds since the view was created, passed in by the caller
//! from one clock. Nothing here reads a clock, so the tests set time exactly.

pub mod bh_layout_data;
mod busiest;
pub mod camera;
pub mod cellmode;
pub mod cells;
pub mod clouds;
pub mod commands;
mod flags;
pub mod history;
pub mod layout;
pub mod load;
pub mod palette;
#[cfg(test)]
mod perf_tests;
pub mod raster;
pub mod scene;
mod summary;
pub mod super_scene;
mod switching;
#[cfg(test)]
mod test_backend;
mod tiers;
pub mod triggers;

use crate::animation::topology::BoardTopology;
use crate::backend::TelemetryBackend;
use crate::models::Architecture;
use busiest::BusiestBoard;
use camera::{dissolve_into, scramble, Camera, Director, TransitionKind};
use cellmode::{CellMode, CellState};
use cells::CellStyle;
use clouds::{CloudClock, Clouds};
use flags::{DieFlag, Flag, Hull};
use history::History;
use load::LoadScale;
use palette::GRAYSKULL;
use raster::{Iso, PixelBuf};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
#[cfg(test)]
use scene::draw_board_scene;
use scene::{
    board_band_origin, board_grid, chip_inputs, die_centre, draw_board_scene_in, fit_last_resort,
    fit_pixels, has_per_tile_data, lift, ChipInputs, Detail, SceneFit, ESTIMATE_LABEL,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
#[cfg(test)]
use super_scene::draw_super_scene;
use super_scene::{band_origin, draw_super_scene_in, super_grid};
use switching::{Scene, Switcher};
use tiers::{tier_for, Tier};
use triggers::{ChipExtras, Trigger, TriggerDetector};

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
const ACTION_KEYS: &str = "[ ] board \u{b7} space cut \u{b7} 3 leave";
/// Separator between footer items.
const SEP: &str = " \u{b7} ";
/// The hint at the end of an inset's caption row: the key that opens the
/// full Die 3D view (handled by the TUI, as `3` is in the full view).
const INSET_HINT: &str = "3 = full view";
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

/// Below this size (columns, rows) the view shows the text summary instead of
/// a scene. Above it a scene is always drawn: shrunk to a two pixel tile
/// when the four pixel one does not fit, and clipped around its middle when
/// even that does not (see `scene::fit_last_resort`). A window smaller than
/// this shows too little of a clipped scene to be worth drawing.
const SCENE_MIN_CELLS: (usize, usize) = (16, 6);

/// Smallest view (columns, rows) in which an automatic cell mode uses half
/// blocks: the size where the whole scene of one board fits in them.
const HALF_BLOCK_MIN_CELLS: (usize, usize) = (100, 30);

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

/// One switch of the view, as the footer shows it: the command that sets it
/// and what it is set to now.
///
/// To add a switch: add an entry to `Die3dView::switches_at` and a command in
/// `commands::COMMANDS` with its arm in `Die3dView::command`. The footer
/// builds itself from the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    /// The command that sets it, with its slash: `/clouds`.
    pub cmd: &'static str,
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
    /// `/scene`: between the board scene and the scene of every board.
    ToggleScene,
    /// Space or `/cut`: cut the camera now.
    CutNow,
    /// `/autocut`: automatic cuts on or off. With them off the camera cuts only on
    /// `c`, and the view also stops switching scenes on its own.
    ToggleAuto,
    /// `/cells`: braille or half-block cells.
    ToggleBraille,
    /// `/bg`: a transparent background, so the terminal's own shows through, or
    /// an opaque black one.
    ToggleTransparent,
    /// `/transition`: how cuts change the picture: rotate (a spin, or by reason), or always
    /// chaos, always a dissolve, or always a spin.
    CycleTransition,
    /// `/labels`: chip labels on the dies on or off.
    ToggleLabels,
    /// `/clouds`: the clouds behind the die on or off.
    ToggleClouds,
    /// `/history`: the history panel on or off.
    ToggleHistory,
    /// The arrow keys: move the window over a clipped scene. They do
    /// nothing when the whole scene fits.
    PanLeft,
    PanRight,
    PanUp,
    PanDown,
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
            Die3dKey::NextBoard
            | Die3dKey::PrevBoard
            | Die3dKey::CutNow
            | Die3dKey::PanLeft
            | Die3dKey::PanRight
            | Die3dKey::PanUp
            | Die3dKey::PanDown => false,
        }
    }
}

/// The Die 3D key bound to a character, if any. Only the keys that mean the
/// same thing in every view are keys: `[` and `]` change board and Space cuts
/// (a nudge, whatever the view does with one). Every other setting is a slash
/// command (`commands`).
pub fn key_to_die3d(c: char) -> Option<Die3dKey> {
    match c {
        ']' => Some(Die3dKey::NextBoard),
        '[' => Some(Die3dKey::PrevBoard),
        ' ' => Some(Die3dKey::CutNow),
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
    /// This board's scene (by index), drawn at the smallest tile and still
    /// bigger than the window, so the window shows its middle. The two
    /// numbers are the columns and rows (footer included) a window needs to
    /// show all of it.
    Clipped(usize, usize, usize),
}

/// The note that a scene is clipped and what size shows all of it, as
/// `clipped, needs 26x14`.
fn clip_note(cols: usize, rows: usize) -> String {
    format!("clipped, needs {cols}x{rows}")
}

/// Cells one arrow key moves the window over a clipped scene.
const PAN_STEP_CELLS: i32 = 4;

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
    /// The pixels a terminal cell covers in this frame's buffer (wide, tall),
    /// which the labels are placed with.
    cell: (usize, usize),
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
    /// How the picture is drawn in cells, and when that changes (`/cells`).
    cells: CellState,
    /// Leave the background unpainted so the terminal's own colour (and any
    /// translucency) shows through. Off paints it opaque black.
    transparent: bool,
    /// Which palette the scene is painted in (`/theme`). Grayskull until the app
    /// says otherwise, so a view made outside the app is the same every time.
    theme: crate::ui::colors::Theme,
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
    /// Where the window sits on a clipped scene, in pixels from the middle
    /// (right and down are positive). Always inside the room the last frame
    /// reported, so a key at an edge does nothing and the next key back moves
    /// at once.
    pan: (i32, i32),
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
    /// True for the compact layout of the Insights screen's inset (see
    /// [`inset`](Die3dView::inset)): every board when they fit, else the
    /// busiest board, a caption row in place of the footer, and no clouds,
    /// flags or history panel. Fixed when the view is made.
    inset: bool,
    /// The busiest board by mean chip load, held for
    /// [`busiest::BUSIEST_HOLD_SECS`] before it changes. Fed by every
    /// `update`; only an inset draws from it.
    busiest: BusiestBoard,
}

/// What the view keeps between frames so that a frame costs less to make
/// and to send. [`Die3dView::resize`] replaces it with an empty one, so
/// nothing here outlives the size it was made for.
#[derive(Default)]
struct RenderCache {
    /// `fit_pixels` results by (pixel width, pixel height, grid).
    fits: HashMap<(i32, i32, (usize, usize)), Option<SceneFit>>,
    /// `fit_last_resort` results, by the same key.
    small_fits: HashMap<(i32, i32, (usize, usize)), SceneFit>,
    /// How far the window can move from the middle of a clipped scene, and
    /// how far one arrow key moves it: `(room_x, room_y, step_x, step_y)`
    /// in pixels. All zero while the whole scene fits.
    pan_room: (i32, i32, i32, i32),
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
/// history panel's sample count while the panel is shown (`history_key`) and
/// the cell style (a pending change counts from its time). A
/// frame is reused only while all seven are unchanged, so a key, a cut
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
    CellStyle,
);

impl Die3dView {
    /// A view for a terminal of `cols` x `rows` cells, in braille, showing
    /// every board, with automatic cuts, chip labels, clouds and the history
    /// panel on. It shows nothing until `update`.
    pub fn new(cols: usize, rows: usize) -> Die3dView {
        Die3dView::with_reserved_rows(cols, rows, 0)
    }

    /// This view with a transparent background (the terminal's own shows
    /// through) or an opaque black one. The app sets it from
    /// `colors::canvas_transparent`; the `T` key flips it afterwards.
    pub fn with_transparent(mut self, transparent: bool) -> Die3dView {
        self.transparent = transparent;
        self
    }

    /// This view painted in the palette of `theme`: greys for Grayskull, full
    /// colour for the default theme. The app sets it from `/theme`.
    pub fn with_theme(mut self, theme: crate::ui::colors::Theme) -> Die3dView {
        self.theme = theme;
        self
    }

    /// Change the palette while the view is open (`/theme` while it is on
    /// screen). The next frame is drawn afresh.
    pub fn set_theme(&mut self, theme: crate::ui::colors::Theme) {
        if self.theme != theme {
            self.theme = theme;
            self.revision += 1;
        }
    }

    /// The theme this view paints in.
    pub fn theme(&self) -> crate::ui::colors::Theme {
        self.theme
    }

    /// This view drawing its cells in `mode`. The app sets `CellMode::Auto`, so
    /// the style follows the hardware; a view made without it holds braille.
    pub fn with_cell_mode(mut self, mode: CellMode) -> Die3dView {
        self.cells = CellState::new(mode);
        self
    }

    /// How the cells are chosen: held, or following the hardware.
    pub fn cell_mode(&self) -> CellMode {
        self.cells.mode()
    }

    /// The cell style drawn at `now`: the one the mode has chosen, except that
    /// an automatic mode does not use half blocks in a view under
    /// [`HALF_BLOCK_MIN_CELLS`] (their pixels are half as wide, so the board
    /// would not fit) and uses quarter blocks there. A style chosen by hand is
    /// always honoured.
    pub fn cell_style(&self, now: f64) -> CellStyle {
        self.style_at(now)
    }

    fn style_at(&self, now: f64) -> CellStyle {
        let style = self.cells.style_at(now);
        let small = self.cols < HALF_BLOCK_MIN_CELLS.0 || self.rows < HALF_BLOCK_MIN_CELLS.1;
        if style == CellStyle::HalfBlock && self.cells.mode().is_automatic() && small {
            CellStyle::Quadrant
        } else {
            style
        }
    }

    /// Whether the background is transparent now.
    pub fn is_transparent(&self) -> bool {
        self.transparent
    }

    /// A compact view of `cols` x `rows` cells for the Insights screen's
    /// inset. `rows` includes one caption row at the bottom; the scene gets
    /// the rest.
    ///
    /// The inset differs from the full view in these ways:
    ///
    /// - Its scene is always every board (`Scene::All`). Where the super
    ///   scene does not fit, it draws the busiest board instead, by mean chip
    ///   load, changing board only after another has been the busiest for
    ///   [`busiest::BUSIEST_HOLD_SECS`] (see `BusiestBoard`).
    /// - A trigger cuts the camera but never switches the scene, so a cut
    ///   may spin.
    /// - The footer is one caption row: the boards or chips drawn, their
    ///   PCIe rates and the hint `3 = full view`.
    /// - No clouds, no chip flags and no history panel, at any size.
    ///
    /// Automatic cuts are on. The inset takes no keys.
    pub fn inset(cols: usize, rows: usize) -> Die3dView {
        let mut v = Die3dView::with_reserved_rows(cols, rows, 0);
        v.inset = true;
        v
    }

    /// Like `new`, for a view whose `rows` are the terminal's minus
    /// `reserved_rows` that the host draws elsewhere. The size tiers (see
    /// `tiers`) are measured on the view's own `cols` x `rows`.
    pub fn with_reserved_rows(cols: usize, rows: usize, reserved_rows: usize) -> Die3dView {
        Die3dView {
            cols,
            rows,
            reserved_rows,
            cells: CellState::new(CellMode::Fixed(CellStyle::Braille)),
            transparent: true,
            theme: crate::ui::colors::Theme::Grayskull,
            show_labels: true,
            show_clouds: true,
            show_history: true,
            pan: (0, 0),
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
            inset: false,
            busiest: BusiestBoard::new(),
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

    /// The size the view draws at, as (columns, rows). The Insights host
    /// loop compares it with the inset's rect to tell whether a resize is due.
    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    /// Forget a board that is waiting out its hold to become the busiest.
    /// The host calls this while the inset is hidden, so the hold starts
    /// again when the inset shows.
    pub fn clear_pending_board(&mut self) {
        self.busiest.clear_candidate();
    }

    /// A text snapshot of the state a resize must keep: the camera the
    /// director holds and each chip's load calibration (chip index, samples
    /// seen, ready). Tests compare it before and after a resize.
    #[cfg(test)]
    pub fn kept_state(&self) -> String {
        let mut scales: Vec<(usize, usize, bool)> = self
            .scales
            .iter()
            .map(|(i, s)| (*i, s.len(), s.ready()))
            .collect();
        scales.sort();
        format!("{:?} {:?}", self.director.camera_at(f64::INFINITY), scales)
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
        // A style change decided by an earlier event shows from its time.
        self.cells.settle(now);
        // The first reading of the hardware picks the starting cell style.
        if !self.chips.is_empty()
            && self
                .cells
                .seed_once(now, cellmode::hardware_seed(&self.chips))
        {
            self.revision += 1;
        }
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

        // Each board's mean load, relative to idle as `calibrate_loads` left
        // it, for the inset's busiest board. A board the inset changes to
        // shows in the next frame.
        let board_loads: Vec<f32> = self
            .chips
            .iter()
            .map(|chips| chips.iter().map(|c| c.load).sum::<f32>() / chips.len() as f32)
            .collect();
        let busiest_before = self.busiest.board();
        self.busiest.observe(now, &board_loads);
        if self.inset && self.busiest.board() != busiest_before {
            self.revision += 1;
        }

        let boards = self.chips.len();
        // Commit a switch whose midpoint has passed before judging new
        // triggers, so they are judged against the scene on screen.
        self.switcher.settle(now, boards);
        // Everything that happened this tick, with the board it happened on:
        // the load and PCIe edges, the temperature, memory and Ethernet
        // readings, and processes opening or closing a device.
        let cut_before = self.director.cut_at();
        let mut events: Vec<(usize, Trigger)> = Vec::new();
        for (board, chips) in self.chips.iter().enumerate() {
            for chip in chips {
                let pcie = chip
                    .pcie_in_bps
                    .zip(chip.pcie_out_bps)
                    .map(|(rx, tx)| rx + tx);
                let extras = ChipExtras {
                    temp_c: chip.temp_c,
                    gddr_heat: chip.gddr_heat,
                    eth_live: chip.eth_known.then_some(chip.eth_live),
                };
                let found = self
                    .detector
                    .observe(chip.index, chip.load, pcie)
                    .into_iter()
                    .chain(self.detector.observe_extras(now, chip.index, &extras));
                events.extend(found.map(|t| (board, t)));
            }
        }
        let opened: Vec<(i64, usize)> = backend
            .device_processes()
            .iter()
            .filter_map(|p| p.pid.zip(p.device))
            .collect();
        for trigger in self.detector.observe_processes(&opened) {
            // A device the view does not draw has no board here.
            if let Some(board) = self
                .chips
                .iter()
                .position(|chips| chips.iter().any(|c| c.index == trigger.chip()))
            {
                events.push((board, trigger));
            }
        }
        for (board, trigger) in events {
            if self.inset {
                // The inset's scene never changes: a trigger only asks for a
                // camera cut, which may spin. It cuts for a trigger on the
                // board it shows. With one board that is every trigger. This
                // does not check whether the super scene fits, which would
                // show every board; it cuts for the busiest board's triggers
                // only, the board the fallback shows.
                if boards == 1 || board == self.busiest.board() {
                    self.director.on_event(now, false, Some(trigger.reason()));
                }
            } else {
                self.switcher
                    .on_trigger(trigger, board, boards, &mut self.director, now);
            }
        }
        // Something on the hardware that cut the camera may also change the cell
        // style, at the cut's midpoint where the transition hides it.
        if self.director.cut_at() != cut_before {
            let swap = self.director.swapped_at().unwrap_or(now);
            if self.cells.on_event_cut(now, swap) {
                self.revision += 1;
            }
        }
        // A quiet view still turns now and then. A cut for an event or by hand
        // restarts the wait.
        self.director.on_idle(now);
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

    /// Act on a key. A scene chosen here (`[`, `]`) is the user's: the
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
            Die3dKey::ToggleBraille => {
                // Braille and half blocks, held: the two the old key flipped.
                let held = match self.cells.style_at(now) {
                    CellStyle::Braille => CellStyle::HalfBlock,
                    _ => CellStyle::Braille,
                };
                self.cells.set_mode(CellMode::Fixed(held), now);
            }
            Die3dKey::ToggleTransparent => self.transparent = !self.transparent,
            Die3dKey::CycleTransition => self.director.cycle_transition_mode(),
            Die3dKey::ToggleLabels => self.show_labels = !self.show_labels,
            Die3dKey::ToggleClouds => self.show_clouds = !self.show_clouds,
            Die3dKey::ToggleHistory => self.show_history = !self.show_history,
            Die3dKey::PanLeft => self.pan_by(-1, 0),
            Die3dKey::PanRight => self.pan_by(1, 0),
            Die3dKey::PanUp => self.pan_by(0, -1),
            Die3dKey::PanDown => self.pan_by(0, 1),
        }
    }

    /// Move the window `dx` / `dy` steps over a clipped scene, stopping at
    /// the scene's edge.
    fn pan_by(&mut self, dx: i32, dy: i32) {
        let (room_x, room_y, step_x, step_y) = self.cache.borrow().pan_room;
        self.pan = (
            (self.pan.0 + dx * step_x).clamp(-room_x, room_x),
            (self.pan.1 + dy * step_y).clamp(-room_y, room_y),
        );
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
            self.scene_at(now),
            self.cloud_key(),
            self.history_key(),
            self.style_at(now),
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
                let lines = self.style_at(now).lines(&buf);
                self.give_back(buf);
                (lines, Some((shown, placed)))
            }
            None => (
                summary::summary_lines(&self.chips, self.cols, scene_rows),
                None,
            ),
        };
        lines.resize(scene_rows, Line::default());
        lines.extend(self.footer(now, frame.as_ref().map(|f| f.0)));
        // A clipped scene must say so. With a footer the note is in it; a view
        // too short for a footer (under 16 rows) puts it over the scene's last
        // row instead.
        if let Some((Shown::Clipped(_, c, r), _)) = &frame {
            if tier.footer_rows == 0 && scene_rows > 0 {
                lines[scene_rows - 1] = hint(fit_width(clip_note(*c, *r), self.cols));
            }
        }
        // The history panel and the labels go over the converted cells, the
        // labels placed with the projection the frame was drawn with. While
        // a transition runs the picture is noise, a mix of two frames or
        // turning, so no label could stay on its die: they wait for it to
        // end. The panel stays. Below the tier's flag size there are no
        // labels whatever `L` says.
        if let Some((_, placed)) = &frame {
            // A clipped scene has dies outside the window, so no flag could
            // be anchored; clipping only happens below the flags' size anyway.
            let clipped = matches!(frame, Some((Shown::Clipped(..), _)));
            let labels = self.show_labels && tier.flags && !clipped;
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

    /// Whether the history panel is drawn: switched on (`H`), the view at
    /// least [`PANEL_MIN_CELLS`], and not an inset (which never has one).
    fn panel_shown(&self) -> bool {
        !self.inset
            && self.show_history
            && self.cols >= PANEL_MIN_CELLS.0
            && self.rows >= PANEL_MIN_CELLS.1
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
    ///
    /// An inset overrides the size tiers: whatever its size, its last row is
    /// the caption (none when it has no rows), and it has no clouds and no
    /// flags.
    fn tier(&self) -> Tier {
        if self.inset {
            return Tier {
                footer_rows: self.rows.min(1),
                clouds: false,
                flags: false,
            };
        }
        tier_for(self.cols, self.rows)
    }

    /// The scene to draw at `now`: every board for an inset, else the
    /// switcher's scene (see `Switcher::scene_at`). Every place that draws
    /// or names the scene reads it through here.
    fn scene_at(&self, now: f64) -> Scene {
        if self.inset {
            Scene::All
        } else {
            self.switcher.scene_at(now)
        }
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
                let scene = self.scene_at(now);
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
    /// An inset's scene never changes, so for it both are every board.
    fn transition_scenes(&self, now: f64) -> (Scene, Scene) {
        match self.director.cut_at() {
            Some(_) if self.inset => (Scene::All, Scene::All),
            Some(at) => self.switcher.transition_scenes(at, now),
            None => (self.scene_at(now), self.scene_at(now)),
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
        let scene = self.scene_at(now);
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
    /// fit, or in an inset to the busiest board (`BusiestBoard`). A board
    /// index past the end of the board list (it can shrink between updates)
    /// is clamped.
    fn scene_with(
        &self,
        now: f64,
        scene: Scene,
        aim_by: Aim,
    ) -> Option<(PixelBuf, Shown, Placement)> {
        let scene_rows = self.rows.checked_sub(self.tier().footer_rows)?;
        // Braille cells hold 2 x 4 pixels, half-block cells 1 x 2.
        let cell = self.style_at(now).px_per_cell();
        let (pw, ph) = (self.cols * cell.0, scene_rows * cell.1);
        // The scenes animate in f32 time (the wave, the packets). f32 has 24
        // bits of mantissa, so past about a month of view time (2.6e6 s) it
        // steps by 0.25 s and the motion would stutter at 4 Hz. The time
        // wraps once a day in f64 first: identical for the first day, full
        // resolution after any uptime. A day is a whole number of the
        // wave's 1/12 s steps; the wave front, the shimmer and the packets
        // have periods that do not divide it, so they jump once a day, at
        // the wrap.
        let t = now.rem_euclid(SCENE_TIME_WRAP_S) as f32;
        let pal = *palette::for_theme(self.theme);
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
                .draw_in(&pal, &mut buf, clock.offset_px(), clock.intensity());
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
                    draw_super_scene_in(pal, &mut buf, &self.chips, &fit, t);
                    let placed = Placement {
                        iso: fit.iso,
                        grid,
                        cell,
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
            Scene::All if self.inset => self.busiest.board(),
            Scene::All => self.switcher.board_at(now),
        }
        .min(self.chips.len().checked_sub(1)?);
        let chips = &self.chips[board];
        let grid = board_grid(chips.len());
        // One board's scene is the last resort, so it goes below the four
        // pixel tile and then clips (an inset keeps its size rules and the
        // summary below them).
        let found = self.fit_for(pw, ph, grid);
        let base = match found {
            Some(fit) => fit,
            None if !self.inset
                && self.cols >= SCENE_MIN_CELLS.0
                && self.rows >= SCENE_MIN_CELLS.1 =>
            {
                self.fit_small(pw, ph, grid)
            }
            None => return None,
        };
        let base = self.panned(base, (pw as i32, ph as i32), cell);
        let fit = aim(&base, aim_by, grid);
        draw_board_scene_in(pal, &mut buf, chips, &fit, t, Detail::Full);
        let shown = match base.virtual_px {
            Some((vw, vh)) => {
                // Cells of the whole picture, and the footer rows a window
                // needs on top of them.
                let (cw, ch) = cell;
                Shown::Clipped(
                    board,
                    (vw as usize).div_ceil(cw),
                    (vh as usize).div_ceil(ch) + self.tier().footer_rows,
                )
            }
            None if fallback => Shown::Fallback(board),
            None => Shown::Board(board),
        };
        let placed = Placement {
            iso: fit.iso,
            grid,
            cell,
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

    /// `fit` with the window moved by the view's pan. A fit that shows the
    /// whole scene comes back as it is. For a clipped one the room to move
    /// (half of what does not fit, each way) and the step of one key are
    /// recorded for `pan_by`, and a pan left over from a larger room is
    /// pulled back inside it.
    fn panned(&self, fit: SceneFit, (pw, ph): (i32, i32), cell: (usize, usize)) -> SceneFit {
        let Some((vw, vh)) = fit.virtual_px else {
            self.cache.borrow_mut().pan_room = (0, 0, 0, 0);
            return fit;
        };
        let room = ((vw - pw).max(0) / 2, (vh - ph).max(0) / 2);
        let step = (
            cell.0 as i32 * PAN_STEP_CELLS,
            cell.1 as i32 * PAN_STEP_CELLS,
        );
        self.cache.borrow_mut().pan_room = (room.0, room.1, step.0, step.1);
        let pan = (
            self.pan.0.clamp(-room.0, room.0),
            self.pan.1.clamp(-room.1, room.1),
        );
        let mut moved = fit;
        moved.iso.ox -= pan.0;
        moved.iso.oy -= pan.1;
        moved
    }

    /// [`fit_last_resort`] for a `pw` x `ph` pixel buffer and `grid`, cached
    /// like [`fit_for`](Self::fit_for).
    fn fit_small(&self, pw: usize, ph: usize, grid: (usize, usize)) -> SceneFit {
        let key = (pw as i32, ph as i32, grid);
        *self
            .cache
            .borrow_mut()
            .small_fits
            .entry(key)
            .or_insert_with(|| fit_last_resort(key.0, key.1, grid))
    }

    /// The footer, one line per footer row of the size tier: with two rows,
    /// what is shown and how, then the keys; with one, the first of those
    /// (the status line) alone; with none, nothing. A left bar and a bottom
    /// bar only, no right-side border.
    ///
    /// An inset has the caption row of [`inset_caption`](Self::inset_caption)
    /// in its place.
    fn footer(&self, now: f64, shown: Option<Shown>) -> Vec<Line<'static>> {
        let rows = self.tier().footer_rows;
        if rows == 0 {
            return Vec::new();
        }
        if self.inset {
            return vec![hint(self.inset_caption(shown))];
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
            // The clip note leads, so a narrow row (which is cut from the
            // right) still shows it.
            Some(Shown::Clipped(b, c, r)) => {
                format!("{}{SEP}arrows pan{SEP}{}", clip_note(c, r), board_name(b))
            }
            // Nothing drawn: name the scene that was chosen.
            None => match self.scene_at(now) {
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

    /// An inset's caption row, at most `cols` characters: what is drawn,
    /// its PCIe rates and the key that opens the full view, as
    /// `BH0  in 2.0G  out 1.0M · 3 = full view`.
    ///
    /// What is drawn is named by its chips (`BH0+BH1` for a board of two),
    /// or as `N boards` when the super scene shows more than one. With no
    /// scene drawn (the text summary), it names every chip or board. The
    /// rates are the sums over the chips named, in the summary's compact
    /// form, `-` when none of them has a reading. When the whole caption is
    /// too wide the rates go first; what is left is cut at `cols` with an
    /// ellipsis. There is no border on either side.
    fn inset_caption(&self, shown: Option<Shown>) -> String {
        let (name, chips): (String, Vec<&ChipInputs>) = match shown {
            Some(Shown::Board(b)) | Some(Shown::Fallback(b)) | Some(Shown::Clipped(b, ..)) => {
                let chips: Vec<&ChipInputs> = self.chips.get(b).into_iter().flatten().collect();
                let name = chips
                    .iter()
                    .map(|c| c.label.as_str())
                    .collect::<Vec<_>>()
                    .join("+");
                (name, chips)
            }
            _ => {
                let chips: Vec<&ChipInputs> = self.chips.iter().flatten().collect();
                let name = if self.chips.len() > 1 {
                    format!("{} boards", self.chips.len())
                } else {
                    chips
                        .iter()
                        .map(|c| c.label.as_str())
                        .collect::<Vec<_>>()
                        .join("+")
                };
                (name, chips)
            }
        };
        // A direction's total over the chips that have a finite reading;
        // `None` when none does.
        let total = |rate: fn(&ChipInputs) -> Option<f64>| {
            chips
                .iter()
                .filter_map(|c| rate(c))
                .filter(|b| b.is_finite())
                .fold(None, |sum: Option<f64>, b| Some(sum.unwrap_or(0.0) + b))
        };
        let rates = format!(
            "in {}  out {}",
            summary::compact_rate(total(|c| c.pcie_in_bps)),
            summary::compact_rate(total(|c| c.pcie_out_bps))
        );
        let full = format!("{name}  {rates}{SEP}{INSET_HINT}");
        if full.chars().count() <= self.cols {
            return full;
        }
        fit_width(format!("{name}{SEP}{INSET_HINT}"), self.cols)
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
        let (sx, sy) = (placed.cell.0 as f32, placed.cell.1 as f32);
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
                cmd: "/cells",
                state: self.cells.mode().state(self.style_at(now)),
            },
            Switch {
                cmd: "/bg",
                state: if self.transparent {
                    "transparent"
                } else {
                    "black"
                },
            },
            Switch {
                cmd: "/autocut",
                state: if self.director.auto() { "on" } else { "off" },
            },
            Switch {
                cmd: "/transition",
                state: self.director.transition_mode().label(),
            },
            Switch {
                cmd: "/scene",
                state: match self.scene_at(now) {
                    Scene::All => "all boards",
                    Scene::Board(_) => "board",
                },
            },
            Switch {
                cmd: "/labels",
                state: if self.show_labels { "on" } else { "off" },
            },
            Switch {
                cmd: "/clouds",
                state: if self.show_clouds { "on" } else { "off" },
            },
            Switch {
                cmd: "/history",
                state: if self.show_history { "on" } else { "off" },
            },
        ]
    }

    /// The footer's second row after its bar, at most `width` characters.
    /// From most to least detailed, the first that fits wins:
    ///
    /// 1. `/cells braille · /bg black · /autocut on · /transition rotate · /scene all boards · /labels on · /clouds on · /history on · [ ] board · space cut · 3 leave`
    /// 2. the same without the action keys
    /// 3. the state words and the action keys (`braille · black · on · ... · [ ] board · space cut · 3 leave`)
    /// 4. the state words alone
    ///
    /// The first three forms keep every state word whole. When even the last is too
    /// wide, the state words are cut at a character with an ellipsis
    /// (`fit_width`), so the last word shown may be partial.
    fn switch_strip(&self, now: f64, width: usize) -> String {
        let switches = self.switches_at(now);
        let join = |each: &dyn Fn(&Switch) -> String| {
            switches.iter().map(each).collect::<Vec<_>>().join(SEP)
        };
        let full = join(&|s| format!("{} {}", s.cmd, s.state));
        let with_actions = format!("{full}{SEP}{ACTION_KEYS}");
        let bare = join(&|s| s.state.to_string());
        let bare_with_actions = format!("{bare}{SEP}{ACTION_KEYS}");
        let fits = [&with_actions, &full, &bare_with_actions, &bare]
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
        virtual_px: fit.virtual_px,
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
mod tests;
