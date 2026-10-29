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
//! chip's board (`switching`). This file holds the
//! view that ties them together: it reads the backend in `update`, answers
//! keys in `handle_key` and draws in `render`.
//!
//! Time is `now`, seconds since the view was created, passed in by the caller
//! from one clock. Nothing here reads a clock, so the tests set time exactly.

pub mod bh_layout_data;
pub mod camera;
pub mod cells;
pub mod layout;
pub mod palette;
pub mod raster;
pub mod scene;
pub mod super_scene;
mod switching;
#[cfg(test)]
mod test_backend;
pub mod triggers;

use crate::animation::topology::BoardTopology;
use crate::backend::TelemetryBackend;
use crate::models::Architecture;
use camera::{dissolve, scramble, Camera, Director, TransitionKind};
use palette::GRAYSKULL;
use raster::{Iso, PixelBuf};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use scene::{
    board_band_origin, board_grid, chip_inputs, die_centre, draw_board_scene, fit_pixels,
    has_per_tile_data, ChipInputs, Detail, SceneFit, ESTIMATE_LABEL, MIN_CELLS,
};
use super_scene::{band_origin, draw_super_scene, super_grid};
use switching::{Scene, Switcher};
use triggers::TriggerDetector;

/// Terminal rows kept below the scene for the footer.
const FOOTER_ROWS: usize = 2;
/// Seed of the camera director. Fixed, so a run with the same telemetry and
/// times cuts to the same angles; tests depend on that.
const CAMERA_SEED: u64 = 0x3D;
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
            | Die3dKey::ToggleLabels => true,
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
/// Holds no pixel buffers between frames: `render` allocates one sized to the
/// terminal each time. The TUI rebuilds the view on a resize, so `new` must
/// stay cheap and nothing may outlive the size it was made for.
pub struct Die3dView {
    cols: usize,
    rows: usize,
    /// Terminal rows below the view's area that the host draws over (the
    /// TUI's status bar). Only used to word the size hint in terminal terms.
    reserved_rows: usize,
    braille: bool,
    /// Leave the background unpainted so the terminal's own colour (and any
    /// translucency) shows through. Off paints it opaque black.
    transparent: bool,
    /// Draw each die's name and power over it (`L`).
    show_labels: bool,
    director: Director,
    detector: TriggerDetector,
    switcher: Switcher,
    /// Label of each drawn board (boards with at least one Blackhole chip).
    labels: Vec<String>,
    /// The Blackhole chips of each drawn board, read in the last `update`.
    chips: Vec<Vec<ChipInputs>>,
    /// Devices the view does not draw (not Blackhole): index and architecture.
    undrawn: Vec<(usize, Architecture)>,
}

impl Die3dView {
    /// A view for a terminal of `cols` x `rows` cells, in braille, showing
    /// every board, with automatic cuts and chip labels on. It shows nothing until `update`.
    pub fn new(cols: usize, rows: usize) -> Die3dView {
        Die3dView::with_reserved_rows(cols, rows, 0)
    }

    /// Like `new`, for a view whose `rows` are the terminal's minus
    /// `reserved_rows` that the host draws elsewhere. The view still needs
    /// `MIN_CELLS` in its own area; the size hint adds the reserved rows back
    /// so it names the terminal size the user has to reach.
    pub fn with_reserved_rows(cols: usize, rows: usize, reserved_rows: usize) -> Die3dView {
        Die3dView {
            cols,
            rows,
            reserved_rows,
            braille: true,
            transparent: true,
            show_labels: true,
            director: Director::new(CAMERA_SEED),
            detector: TriggerDetector::new(),
            switcher: Switcher::new(),
            labels: Vec::new(),
            chips: Vec::new(),
            undrawn: Vec::new(),
        }
    }

    /// Read every chip from the backend, group the chips into boards and feed
    /// the trigger detector. A trigger may cut the camera and, in the super
    /// scene, switch to the busy chip's board (see `switching`).
    ///
    /// Only Blackhole chips are drawn, so only they can trigger a cut; other
    /// devices are listed in the footer instead.
    pub fn update(&mut self, backend: &dyn TelemetryBackend, now: f64) {
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

    /// Act on a key. A scene chosen here (`[`, `]`, `m`) is the user's: the
    /// view will not switch back from it on its own.
    pub fn handle_key(&mut self, key: Die3dKey, now: f64) {
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
        }
    }

    /// The view at `now`: the scene, then a two-row footer.
    ///
    /// Below 100 x 30 cells this is a single hint line. With no Blackhole chip
    /// it is a hint line plus, when there are other devices, a line naming
    /// them.
    pub fn render(&self, now: f64) -> Vec<Line<'static>> {
        if self.cols < MIN_CELLS.0 || self.rows < MIN_CELLS.1 {
            return vec![hint(self.too_small_hint())];
        }
        if self.chips.is_empty() {
            if self.undrawn.is_empty() {
                return vec![hint("Die 3D: no Tenstorrent device found.".into())];
            }
            return vec![
                hint("Die 3D draws Blackhole chips only, and none was found.".into()),
                note(self.undrawn_note()),
            ];
        }
        let scene_rows = self.rows - FOOTER_ROWS;
        let frame = self.frame(now);
        let mut lines = match &frame {
            Some((buf, ..)) if self.braille => cells::to_braille_lines(buf),
            Some((buf, ..)) => cells::to_halfblock_lines(buf),
            None => vec![hint(no_fit_hint(self.braille).into())],
        };
        lines.resize(scene_rows, Line::default());
        lines.extend(self.footer(now, frame.as_ref().map(|f| f.1)));
        // The labels go over the converted cells, placed with the projection
        // the frame was drawn with. While a transition runs the picture is
        // noise, a mix of two frames or turning, so no label could stay on
        // its die: they wait for it to end.
        if let Some((_, _, Some(placed))) = &frame {
            if self.show_labels && self.director.progress(now).is_none() {
                let anchors = self.label_anchors(placed, scene_rows);
                draw_labels(
                    &mut lines,
                    scene_rows,
                    self.cols,
                    &anchors,
                    self.label_style(),
                );
            }
        }
        lines
    }

    /// The "terminal too small" line, in terminal cells: the view's own
    /// minimum plus the rows the host keeps, against the terminal's size.
    fn too_small_hint(&self) -> String {
        format!(
            "Die 3D needs a terminal of at least {} x {} cells (this one is {} x {}). Press 3 to leave.",
            MIN_CELLS.0,
            MIN_CELLS.1 + self.reserved_rows,
            self.cols,
            self.rows + self.reserved_rows
        )
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
                Some((dissolve(&old, &new, p, seed), shown, None))
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
        let scene_rows = self.rows.checked_sub(FOOTER_ROWS)?;
        // Braille cells hold 2 x 4 pixels, half-block cells 1 x 2.
        let (pw, ph) = if self.braille {
            (self.cols * 2, scene_rows * 4)
        } else {
            (self.cols, scene_rows * 2)
        };
        let t = now as f32;
        let mut buf = PixelBuf::new(pw, ph);
        // The scenes paint only what stands in them. Transparent is the fresh,
        // all-uncovered buffer, so it needs nothing; opaque fills the
        // background first.
        if !self.transparent {
            buf.fill_solid(OPAQUE_BG);
        }
        let mut fallback = false;
        // With one board the super scene is that board's scene.
        if scene == Scene::All && self.chips.len() > 1 {
            let dies: Vec<usize> = self.chips.iter().map(Vec::len).collect();
            let (grid, _) = super_grid(&dies);
            match fit_pixels(pw as i32, ph as i32, grid) {
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
        let fit = aim(&fit_pixels(pw as i32, ph as i32, grid)?, aim_by, grid);
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

    /// The two footer rows: what is shown and how, then the keys. A left bar
    /// and a bottom bar only, no right-side border.
    fn footer(&self, now: f64, shown: Option<Shown>) -> [Line<'static>; 2] {
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
        if !self.undrawn.is_empty() {
            status.push(self.undrawn_note());
        }
        let width = self.cols.saturating_sub(BAR_WIDTH);
        // The camera caption comes last and goes whole when the row is too
        // narrow for it, so it never pushes the estimate label (or the note)
        // into the cut.
        let status = status.join(SEP);
        let with_caption = format!("{status}{SEP}{}", self.caption(now));
        let status = if with_caption.chars().count() <= width {
            with_caption
        } else {
            status
        };
        [
            Line::from(Span::styled(
                format!("\u{2551} {}", fit_width(status, width)),
                Style::default().fg(FOOTER_FG),
            )),
            Line::from(Span::styled(
                format!("\u{255a}\u{2550} {}", self.switch_strip(now, width)),
                Style::default().fg(KEYS_FG),
            )),
        ]
    }

    /// The footer's camera caption: `view: <side>`, naming the board edge
    /// facing the viewer (see `Camera::side_name`), or `view: <from> -> <to>`
    /// for the whole of a cut's transition.
    fn caption(&self, now: f64) -> String {
        if self.director.progress(now).is_some() {
            let (from, to) = self.director.cameras();
            format!("view: {} -> {}", from.side_name(), to.side_name())
        } else {
            format!("view: {}", self.director.camera_at(now).side_name())
        }
    }

    /// The cell (column, row) and text of each die's label, for a frame drawn
    /// as `placed` says. The die centre is projected with the frame's own
    /// projection and turned into a cell: braille cells are 2 x 4 pixels,
    /// half-block cells 1 x 2. A die whose centre lands outside the scene
    /// area gets no label.
    fn label_anchors(&self, placed: &Placement, scene_rows: usize) -> Vec<(usize, usize, String)> {
        placed
            .dies
            .iter()
            .filter_map(|((gx, gy), label)| {
                let (x, y) = placed.iso.project(*gx, *gy, placed.grid);
                let (col, row) = if self.braille {
                    (x.div_euclid(2), y.div_euclid(4))
                } else {
                    (x, y.div_euclid(2))
                };
                let on_screen =
                    (0..self.cols as i32).contains(&col) && (0..scene_rows as i32).contains(&row);
                on_screen.then(|| (col as usize, row as usize, label.clone()))
            })
            .collect()
    }

    /// White bold text. The background stays the terminal's own in
    /// transparent mode and is the black fill otherwise, as around the dies.
    fn label_style(&self) -> Style {
        let bg = if self.transparent {
            Color::Reset
        } else {
            rgb(OPAQUE_BG)
        };
        Style::default()
            .fg(rgb(GRAYSKULL.white))
            .bg(bg)
            .add_modifier(Modifier::BOLD)
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
        ]
    }

    /// The footer's second row after its bar, at most `width` characters.
    /// From most to least detailed, the first that fits wins:
    ///
    /// 1. `B braille · T transparent · C auto-cuts on · X transition shuffle · m all boards · L labels on · [ ] board · c cut · 3 leave`
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

/// Write each label over `lines`, centred on its anchor (column, row), in
/// `style`. Each cell a label covers takes the label's character and style.
///
/// Only the first `scene_rows` lines are written: the footer rows below them
/// are never touched, whatever the anchor. An anchor at or past column `cols`
/// draws nothing, and a label is clipped to columns `0..cols` and to the
/// line's own cells, so no line grows.
fn draw_labels(
    lines: &mut [Line<'static>],
    scene_rows: usize,
    cols: usize,
    anchors: &[(usize, usize, String)],
    style: Style,
) {
    for (col, row, label) in anchors {
        if *row >= scene_rows || *col >= cols {
            continue;
        }
        let Some(line) = lines.get_mut(*row) else {
            continue;
        };
        // One (character, style) per cell; every cell is one character.
        let mut cells: Vec<(char, Style)> = line
            .spans
            .iter()
            .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
            .collect();
        let start = *col as i64 - label.chars().count() as i64 / 2;
        for (i, ch) in label.chars().enumerate() {
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

    /// TDP the fakes report, so `power` watts is a load of `power / 100`.
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
    fn render_returns_a_hint_below_the_minimum_size() {
        let b = two_boards();
        for (c, r) in [(99, 40), (150, 29), (10, 3), (0, 0)] {
            let mut v = Die3dView::new(c, r);
            v.update(&b, 0.0);
            let lines = v.render(0.0);
            assert_eq!(lines.len(), 1, "{c} x {r}");
            assert!(text(&lines)[0].contains("100 x 30"), "{:?}", text(&lines));
        }
        // At the minimum the scene is drawn: one line per terminal row.
        let mut v = Die3dView::new(100, 30);
        v.update(&b, 0.0);
        assert_eq!(v.render(0.0).len(), 30);
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

        // A load rise on any chip restarts the 20 s, even one on another
        // board that does not cut the camera.
        let (mut v, _) = switched_by_chip_2();
        b = two_boards();
        b.set_power(2, 90.0);
        v.update(&b, 1.5);
        b.set_power(0, 90.0);
        v.update(&b, 10.0);
        v.update(&b, 21.0);
        assert_eq!(v.switcher.scene_at(21.0), Scene::Board(1));
        v.update(&b, 30.0);
        assert_eq!(v.switcher.scene_at(30.0), Scene::All);
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
    fn render_during_a_cut_then_new_at_another_size_does_not_panic() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.director.set_transition_mode(TransitionMode::Chaos);
        v.handle_key(Die3dKey::CutNow, 5.0);
        assert!(v.director.chaos(5.3).is_some(), "mid-chaos");
        assert_eq!(v.render(5.3).len(), 42);
        // The TUI rebuilds the view on resize; every size must render, in
        // every transition, at several points of it.
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
        // Measured data on one chip: the label goes.
        v.chips[1][0].tile_flow = Some(HashMap::new());
        assert!(has_per_tile_data(&v.chips.concat()));
        assert!(!footer(&v.render(0.0)).contains(ESTIMATE_LABEL));
    }

    #[test]
    fn the_footer_names_the_mode_and_auto_cuts() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
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
        assert_eq!(text(&lines)[10].chars().count(), 150);
    }

    #[test]
    fn render_draws_the_frame_not_the_bare_scene() {
        // Pins `render` to `frame`: a regression that drew `scene_pixels`
        // would skip the chaos noise, and this would fail.
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // Labels off: they are drawn over the cells after the transition,
        // and the bare scene has none.
        v.handle_key(Die3dKey::ToggleLabels, 0.0);
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
    fn the_size_hint_names_terminal_cells_including_the_reserved_row() {
        let b = two_boards();
        // A 100 x 30 terminal under a one-row status bar: a 100 x 29 area.
        let mut v = Die3dView::with_reserved_rows(100, 29, 1);
        v.update(&b, 0.0);
        let t = text(&v.render(0.0));
        assert_eq!(t.len(), 1);
        assert!(t[0].contains("at least 100 x 31"), "{}", t[0]);
        assert!(t[0].contains("this one is 100 x 30"), "{}", t[0]);
        assert!(!t[0].contains("29"), "{}", t[0]);
        // One more row and the scene is drawn.
        let mut v = Die3dView::with_reserved_rows(100, 30, 1);
        v.update(&b, 0.0);
        assert_eq!(v.render(0.0).len(), 30);
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

    /// The switch strip as the footer's second row shows it.
    fn strip(v: &Die3dView) -> String {
        v.footer(0.0, None)[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
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
        let mut v = Die3dView::new(150, 42);
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
        for cols in [100, 80] {
            let v = wide_label_view(cols);
            let f = v.footer(0.0, Some(Shown::All)).map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            });
            let f = f.join("\n");
            for s in v.switches() {
                assert!(f.contains(s.state), "{cols}: {} in {f}", s.state);
            }
            assert!(f.contains(ESTIMATE_LABEL), "{cols}: {f}");
            for line in f.lines() {
                assert!(line.chars().count() <= cols, "{cols}: {line}");
            }
        }
    }

    #[test]
    fn action_keys_are_dropped_before_switch_states() {
        let v = wide_label_view(150);
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
        let before: Vec<_> = "BTCXmL".chars().map(|k| state(&v, k)).collect();
        assert_eq!(
            before,
            [
                "braille",
                "transparent",
                "on",
                "shuffle",
                "all boards",
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
        ] {
            v.handle_key(k, 0.0);
        }
        let after: Vec<_> = "BTCXmL".chars().map(|k| state(&v, k)).collect();
        assert_eq!(
            after,
            ["half-block", "opaque", "off", "chaos", "board", "off"]
        );
    }

    #[test]
    fn the_switch_table_still_covers_every_toggle_key() {
        let v = Die3dView::new(150, 42);
        let keys: Vec<char> = v.switches().iter().map(|s| s.key).collect();
        assert!(keys.contains(&'X') && keys.contains(&'L'), "{keys:?}");
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
        // Lengths with the default switches: keyed is 66 characters and bare
        // 54, so a 60-column terminal (57 after the bar) lands on bare.
        let v = wide_label_view(150);
        let sw = v.switches();
        let keyed = sw
            .iter()
            .map(|s| format!("{} {}", s.key, s.state))
            .collect::<Vec<_>>()
            .join(SEP);
        let bare = sw.iter().map(|s| s.state).collect::<Vec<_>>().join(SEP);
        assert_eq!((keyed.chars().count(), bare.chars().count()), (66, 54));
        let at = |cols: usize| {
            let v = wide_label_view(cols);
            let s = strip(&v);
            assert!(s.chars().count() <= cols, "{cols}: {s}");
            s.trim_start_matches("\u{255a}\u{2550} ").to_string()
        };
        // Exactly wide enough for the keyed form.
        assert_eq!(at(66 + BAR_WIDTH), keyed);
        // About 60 columns: the keyed form no longer fits, the bare one does.
        assert_eq!(at(60), bare);
        assert_eq!(at(54 + BAR_WIDTH), bare);
        // One column short of bare: the last resort cuts at a character and
        // marks the cut with an ellipsis.
        let cut = at(53 + BAR_WIDTH);
        assert_eq!(cut.chars().count(), 53);
        assert!(cut.ends_with('\u{2026}'), "{cut}");
        assert!(bare.starts_with(cut.trim_end_matches('\u{2026}')), "{cut}");
    }

    #[test]
    fn opaque_mode_fills_pure_black() {
        let b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v.handle_key(Die3dKey::ToggleTransparent, 0.0);
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
        const PAD: i32 = 80;
        let (pw, ph) = (300i32, 160i32);
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
            for from in camera::PRESETS {
                for to in camera::PRESETS {
                    for i in 0..=8 {
                        let mut iso =
                            Camera::lerp_view(&from, &to, i as f32 / 8.0, &base.iso, grid);
                        iso.ox += PAD;
                        iso.oy += PAD;
                        let f = SceneFit { iso, ..base };
                        let mut buf =
                            PixelBuf::new((pw + 2 * PAD) as usize, (ph + 2 * PAD) as usize);
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
        assert!(row.contains("view: front (PCIe edge)"), "{row}");
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
        // A long caption: a transition between two turns.
        let mut v = wide_label_view(150);
        v.director.set_transition_mode(TransitionMode::Spin);
        let mut at = 10.0;
        v.handle_key(Die3dKey::CutNow, at);
        while {
            let (a, b) = v.director.cameras();
            a.side_name() == "top-down" || b.side_name() == "top-down"
        } {
            at += 10.0;
            v.handle_key(Die3dKey::CutNow, at);
        }
        let now = at + 0.1;
        let (from, to) = v.director.cameras();
        let caption = format!("view: {} -> {}", from.side_name(), to.side_name());
        // Wide: the caption is there, whole.
        let row = status_row(&v.render(now));
        assert!(row.contains(&caption), "{row}");
        // At 100 columns the estimate label stays and the caption, if it
        // does not fit whole, goes whole: never a cut-off part of it.
        for cols in [100, 90, 80] {
            v.cols = cols;
            let row = v.footer(now, Some(Shown::Board(0)))[0]
                .spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>();
            assert!(row.contains(ESTIMATE_LABEL), "{cols}: {row}");
            assert!(row.chars().count() <= cols, "{cols}: {row}");
            assert!(
                row.contains(&caption) || !row.contains("view"),
                "{cols}: {row}"
            );
        }
        v.cols = 100;
        let row: String = v.footer(now, Some(Shown::Board(0)))[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(!row.contains("view"), "100 is too narrow for it: {row}");
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

    /// The cell where the centre of die `d` of a lone board of `dies` dies
    /// lands at preset 0, worked out here from the grid layout (the die's 17
    /// x 12 tiles start after `MARGIN_X` columns and `MARGIN_Y` rows of its
    /// band), independently of the view's own helpers.
    fn expected_centre(v: &Die3dView, dies: usize, d: usize) -> (i64, i64) {
        let scene_rows = v.rows - FOOTER_ROWS;
        let (pw, ph) = if v.braille {
            (v.cols * 2, scene_rows * 4)
        } else {
            (v.cols, scene_rows * 2)
        };
        let grid = board_grid(dies);
        let fit = fit_pixels(pw as i32, ph as i32, grid).unwrap();
        let iso = v.director.camera_at(0.0).apply(&fit.iso, grid);
        let gx = scene::MARGIN_X as f32 + 8.0;
        let gy = (d * scene::BOARD_BAND + scene::MARGIN_Y) as f32 + 5.5;
        let (x, y) = iso.project(gx, gy, grid);
        if v.braille {
            ((x / 2) as i64, (y / 4) as i64)
        } else {
            (x as i64, (y / 2) as i64)
        }
    }

    #[test]
    fn each_die_gets_a_label_at_its_centre() {
        // One p300 board: two dies.
        let mut b = Fake::boards(&["p300"; 2], Some(TDP));
        b.set_power(0, 44.0);
        b.set_power(1, 51.6);
        for braille in [true, false] {
            let mut v = Die3dView::new(150, 42);
            v.update(&b, 0.0);
            if !braille {
                v.handle_key(Die3dKey::ToggleBraille, 0.0);
            }
            let lines = v.render(0.0);
            let found = labels_in(&lines, 40);
            for (d, want) in [(0, "BH0 44W"), (1, "BH1 52W")] {
                let (col, row) = expected_centre(&v, 2, d);
                let hit = found.iter().find(|(_, _, t)| t == want);
                let (r, c, _) =
                    hit.unwrap_or_else(|| panic!("{want} in {found:?} (braille={braille})"));
                let centre = *c as i64 + want.chars().count() as i64 / 2;
                assert!(
                    (*r as i64 - row).abs() <= 1 && (centre - col).abs() <= 1,
                    "{want} at row {r} col {c}, centre {centre}; want {row},{col} (braille={braille})"
                );
            }
        }
        // Without a power reading the label is the name alone.
        let mut b = Fake::boards(&["p150"], Some(TDP));
        b.telem[0].power = None;
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
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
            let v = cut_view(&b, mode, 10.0);
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

        // At the layer that writes: anchors on the footer rows, past the
        // right edge and hanging off the left edge.
        let width = 20;
        let scene_rows = 3;
        let mut lines: Vec<Line<'static>> = (0..scene_rows + FOOTER_ROWS)
            .map(|i| Line::from(Span::raw(format!("{i}").repeat(width))))
            .collect();
        let before = text(&lines);
        let style = Style::default().fg(Color::White);
        let anchors = vec![
            (5, scene_rows, "BH8 10W".to_string()),
            (5, scene_rows + 1, "BH9 10W".to_string()),
            (width + 3, 1, "BH7".to_string()),
            (1, 0, "BH0 123W".to_string()),
            (width - 1, 2, "BH2 99W".to_string()),
        ];
        draw_labels(&mut lines, scene_rows, width, &anchors, style);
        let after = text(&lines);
        assert_eq!(after[scene_rows..], before[scene_rows..], "footer rows");
        assert_eq!(
            after[1], before[1],
            "an anchor past the right edge draws nothing"
        );
        // Clipped at both edges, never wider than the view.
        // "BH0 123W" centred on column 1 starts at -3; "BH2 99W" on column
        // 19 starts at 16.
        assert!(after[0].starts_with(" 123W"), "{}", after[0]);
        assert!(after[2].ends_with("BH2 "), "{}", after[2]);
        for row in &after {
            assert_eq!(row.chars().count(), width, "{row}");
        }
    }

    #[test]
    fn labels_stay_transparent_in_transparent_mode() {
        let b = Fake::boards(&["p150"], Some(TDP));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
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
}
