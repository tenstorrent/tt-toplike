// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Die 3D view: an isometric, pixel-rendered picture of the board and its
//! Blackhole dies.
//!
//! Each frame is drawn into an RGB pixel buffer (`raster`, `scene`,
//! `super_scene`) and converted to terminal cells as braille or half-block
//! (`cells`). The camera (`camera`) cuts to another angle when a chip becomes
//! busy or its PCIe traffic bursts (`triggers`), and the scene can switch from
//! every board to the busy chip's board (`switching`). This file holds the
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
use camera::{scramble, Camera, Director};
use palette::GRAYSKULL;
use raster::PixelBuf;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use scene::{
    board_grid, chip_inputs, draw_board_scene, fit_pixels, has_per_tile_data, ChipInputs, Detail,
    SceneFit, ESTIMATE_LABEL, MIN_CELLS,
};
use super_scene::{draw_super_scene, super_grid};
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
/// The slab colour times this is the opaque background (`T` off): the slab is
/// the floor of the scene, so the surround is a darker shade of it.
const OPAQUE_BG_SHADE: f32 = 0.55;
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
/// To add a switch: add an entry to `Die3dView::switches`, a `Die3dKey`
/// variant (with its `toggles`, `ALL` and `key_to_die3d` lines) and an arm in
/// `handle_key`. The footer builds itself from the table.
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

/// What a key asks the Die 3D view to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Die3dKey {
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
    /// an opaque dark one.
    ToggleTransparent,
}

impl Die3dKey {
    /// Every key, for tests that must visit them all. `all_keys_are_listed`
    /// (an exhaustive match) fails to compile when a variant is added, which
    /// is the reminder to extend this array.
    #[cfg(test)]
    const ALL: [Die3dKey; 7] = [
        Die3dKey::NextBoard,
        Die3dKey::PrevBoard,
        Die3dKey::ToggleScene,
        Die3dKey::CutNow,
        Die3dKey::ToggleAuto,
        Die3dKey::ToggleBraille,
        Die3dKey::ToggleTransparent,
    ];

    /// Whether the key flips a state the view keeps, and so needs an entry in
    /// `Die3dView::switches`. The match is exhaustive on purpose: a new
    /// variant does not compile until it is classified here.
    pub fn toggles(self) -> bool {
        match self {
            Die3dKey::ToggleScene
            | Die3dKey::ToggleAuto
            | Die3dKey::ToggleBraille
            | Die3dKey::ToggleTransparent => true,
            Die3dKey::NextBoard | Die3dKey::PrevBoard | Die3dKey::CutNow => false,
        }
    }

    /// Compile-time guard for `ALL`: names every variant, so adding one
    /// breaks the build here and not silently in the tests.
    #[cfg(test)]
    #[allow(dead_code)]
    fn all_keys_are_listed(self) {
        match self {
            Die3dKey::NextBoard
            | Die3dKey::PrevBoard
            | Die3dKey::ToggleScene
            | Die3dKey::CutNow
            | Die3dKey::ToggleAuto
            | Die3dKey::ToggleBraille
            | Die3dKey::ToggleTransparent => {}
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
        _ => None,
    }
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
    /// translucency) shows through. Off paints an opaque dark vignette.
    transparent: bool,
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
    /// every board, with automatic cuts on. It shows nothing until `update`.
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
                self.director.cut_by_hand(now);
            }
            Die3dKey::ToggleAuto => self.director.set_auto(!self.director.auto()),
            Die3dKey::ToggleBraille => self.braille = !self.braille,
            Die3dKey::ToggleTransparent => self.transparent = !self.transparent,
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
            Some((buf, _)) if self.braille => cells::to_braille_lines(buf),
            Some((buf, _)) => cells::to_halfblock_lines(buf),
            None => vec![hint(no_fit_hint(self.braille).into())],
        };
        lines.resize(scene_rows, Line::default());
        lines.extend(self.footer(now, frame.map(|(_, shown)| shown)));
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

    /// The scene at `now` with the chaos effect applied during a cut.
    fn frame(&self, now: f64) -> Option<(PixelBuf, Shown)> {
        let (mut buf, shown) = self.scene_pixels(now)?;
        if let Some(intensity) = self.director.chaos(now) {
            // A new noise pattern 60 times a second, the same for a given now.
            scramble(&mut buf, intensity, (now * 60.0) as u32);
        }
        Some((buf, shown))
    }

    /// The scene at `now`, before any chaos, in a buffer the size of the
    /// terminal area above the footer. `None` with no chip to draw or when
    /// even the board scene does not fit.
    ///
    /// The camera is `Director::camera_at(now)`: during a cut the old angle
    /// stays until the midpoint, under the chaos noise, and the new one shows
    /// from there.
    fn scene_pixels(&self, now: f64) -> Option<(PixelBuf, Shown)> {
        let scene_rows = self.rows.checked_sub(FOOTER_ROWS)?;
        // Braille cells hold 2 x 4 pixels, half-block cells 1 x 2.
        let (pw, ph) = if self.braille {
            (self.cols * 2, scene_rows * 4)
        } else {
            (self.cols, scene_rows * 2)
        };
        let camera = self.director.camera_at(now);
        let t = now as f32;
        let mut buf = PixelBuf::new(pw, ph);
        // The scenes paint only what stands in them. Transparent is the fresh,
        // all-uncovered buffer, so it needs nothing; opaque fills the
        // background first.
        if !self.transparent {
            let bg = GRAYSKULL
                .slab
                .map(|v| (v as f32 * OPAQUE_BG_SHADE).round() as u8);
            buf.fill_vignette(bg);
        }
        let mut fallback = false;
        // With one board the super scene is that board's scene.
        if self.switcher.scene_at(now) == Scene::All && self.chips.len() > 1 {
            let dies: Vec<usize> = self.chips.iter().map(Vec::len).collect();
            let (grid, _) = super_grid(&dies);
            match fit_pixels(pw as i32, ph as i32, grid) {
                Some(fit) => {
                    draw_super_scene(&mut buf, &self.chips, &aim(&fit, camera, grid), t);
                    return Some((buf, Shown::All));
                }
                None => fallback = true,
            }
        }
        let board = self
            .switcher
            .board_at(now)
            .min(self.chips.len().checked_sub(1)?);
        let chips = &self.chips[board];
        let grid = board_grid(chips.len());
        let fit = fit_pixels(pw as i32, ph as i32, grid)?;
        draw_board_scene(&mut buf, chips, &aim(&fit, camera, grid), t, Detail::Full);
        let shown = if fallback {
            Shown::Fallback(board)
        } else {
            Shown::Board(board)
        };
        Some((buf, shown))
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
        [
            Line::from(Span::styled(
                format!("\u{2551} {}", fit_width(status.join(SEP), width)),
                Style::default().fg(FOOTER_FG),
            )),
            Line::from(Span::styled(
                format!("\u{255a}\u{2550} {}", self.switch_strip(now, width)),
                Style::default().fg(KEYS_FG),
            )),
        ]
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
                key: 'm',
                label: "",
                state: match self.switcher.scene_at(now) {
                    Scene::All => "all boards",
                    Scene::Board(_) => "board",
                },
            },
        ]
    }

    /// The footer's second row after its bar, at most `width` characters.
    /// From most to least detailed, the first that fits wins:
    ///
    /// 1. `B braille · T transparent · C auto-cuts on · m all boards · [ ] board · c cut · 3 leave`
    /// 2. the same without the action keys
    /// 3. `<key> <state>` for each switch (`C on`)
    /// 4. the state words alone
    ///
    /// A state word is never cut: if even the last form is too wide the row
    /// is cut at its end, which loses the last words, not part of one.
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
        [with_actions, full, keyed, bare]
            .into_iter()
            .find(|row| row.chars().count() <= width)
            .unwrap_or_else(|| fit_width(join(&|s| s.state.to_string()), width))
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

/// `fit` turned to `camera`. `grid` must be the grid `fit` was sized for, so
/// the camera keeps the grid centre where `fit` put it.
fn aim(fit: &SceneFit, camera: Camera, grid: (usize, usize)) -> SceneFit {
    SceneFit {
        iso: camera.apply(&fit.iso, grid),
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
        assert!((at - (1.0 + camera::CHAOS_SECS / 2.0)).abs() < 1e-9);
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
        v.handle_key(Die3dKey::CutNow, 5.0);
        assert!(v.director.chaos(5.3).is_some(), "mid-chaos");
        assert_eq!(v.render(5.3).len(), 42);
        // The TUI rebuilds the view on resize; every size must render.
        for (c, r) in [(120, 35), (100, 30), (220, 60), (101, 31)] {
            for braille in [true, false] {
                let mut v = Die3dView::new(c, r);
                v.update(&b, 5.0);
                if !braille {
                    v.handle_key(Die3dKey::ToggleBraille, 5.0);
                }
                v.handle_key(Die3dKey::CutNow, 5.0);
                assert_eq!(v.render(5.3).len(), r, "{c} x {r} braille={braille}");
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
        let before: Vec<_> = "BTCm".chars().map(|k| state(&v, k)).collect();
        assert_eq!(before, ["braille", "transparent", "on", "all boards"]);
        for k in [
            Die3dKey::ToggleBraille,
            Die3dKey::ToggleTransparent,
            Die3dKey::ToggleAuto,
            Die3dKey::ToggleScene,
        ] {
            v.handle_key(k, 0.0);
        }
        let after: Vec<_> = "BTCm".chars().map(|k| state(&v, k)).collect();
        assert_eq!(after, ["half-block", "opaque", "off", "board"]);
    }

    #[test]
    fn the_switch_table_covers_every_toggle_key() {
        let v = Die3dView::new(150, 42);
        let keys: Vec<char> = v.switches().iter().map(|s| s.key).collect();
        let mut toggling = 0;
        for k in Die3dKey::ALL {
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
}
