// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Which scene the Die 3D view shows, and when it changes on its own.
//!
//! Kept apart from the view so the rules are in one place:
//!
//! - In a board scene, only a trigger from one of that board's chips cuts the
//!   camera, and the cut changes the angle only.
//! - In the super scene ("all boards"), a `LoadRise` that wins a cut also
//!   switches to that chip's board. The switch shows from the cut's midpoint
//!   (`Director::swapped_at`), so the transition covers it: chaos or a
//!   dissolve, never a spin (the director is told the cut changes scene). A
//!   `PcieBurst` in the super scene changes the angle only.
//! - After [`RETURN_SECS`] with no `LoadRise` on any chip, the view goes back
//!   to the super scene, but only when the view made the switch.
//! - A scene chosen by the user (`m`, `[`, `]`) cancels that return, and any
//!   switch still waiting for its midpoint.
//!
//! Nothing here reads a clock: `now` is passed in, as for `Director`.

use super::camera::Director;
use super::triggers::Trigger;

/// Seconds without any `LoadRise` before an automatic board switch is undone.
pub(super) const RETURN_SECS: f64 = 20.0;

/// The scene on show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scene {
    /// Every board in one view.
    All,
    /// One board, by its position in the view's board list.
    Board(usize),
}

#[derive(Debug, Clone)]
pub(super) struct Switcher {
    scene: Scene,
    /// A switch decided by a cut, and the time it shows from.
    pending: Option<(Scene, f64)>,
    /// True while the board scene on show (or pending) was chosen by the view.
    auto_return: bool,
    /// Time of the latest `LoadRise` on any drawn chip.
    last_load_rise: Option<f64>,
    /// The board shown last, so `m` returns to it and the fallback draws it.
    board: usize,
    /// The latest switch made by a cut: the cut's start time, the scene
    /// before it and the scene after it. A dissolve draws both scenes for
    /// the whole transition, past the midpoint where `pending` is committed
    /// and forgotten. Cleared by a scene the user chooses.
    cut_switch: Option<(f64, Scene, Scene)>,
}

impl Switcher {
    /// Start in the super scene, on board 0.
    pub(super) fn new() -> Switcher {
        Switcher {
            scene: Scene::All,
            pending: None,
            auto_return: false,
            last_load_rise: None,
            board: 0,
            cut_switch: None,
        }
    }

    /// The scenes on either side of the cut that started at `cut_at`: (before,
    /// after). For a cut that switched scenes these are the two scenes; for
    /// any other cut both are the scene on show at `now`.
    pub(super) fn transition_scenes(&self, cut_at: f64, now: f64) -> (Scene, Scene) {
        match self.cut_switch {
            Some((at, before, after)) if at == cut_at => (before, after),
            _ => (self.scene_at(now), self.scene_at(now)),
        }
    }

    /// The scene to draw at `now`: a pending switch counts from its midpoint.
    /// Pure, so `render(&self)` can call it between two updates.
    pub(super) fn scene_at(&self, now: f64) -> Scene {
        match self.pending {
            Some((scene, at)) if now >= at => scene,
            _ => self.scene,
        }
    }

    /// The board shown last (or about to be shown).
    pub(super) fn board_at(&self, now: f64) -> usize {
        match self.scene_at(now) {
            Scene::Board(b) => b,
            Scene::All => self.board,
        }
    }

    /// Commit a pending switch whose midpoint has passed, and keep every board
    /// index below `boards` (the board list can shrink between updates).
    pub(super) fn settle(&mut self, now: f64, boards: usize) {
        if let Some((scene, at)) = self.pending {
            if now >= at {
                self.set(scene);
                self.pending = None;
            }
        }
        let last = boards.saturating_sub(1);
        self.board = self.board.min(last);
        if let Scene::Board(b) = self.scene {
            self.scene = Scene::Board(b.min(last));
        }
        if let Some((Scene::Board(b), at)) = self.pending {
            self.pending = Some((Scene::Board(b.min(last)), at));
        }
    }

    fn set(&mut self, scene: Scene) {
        if let Scene::Board(b) = scene {
            self.board = b;
        }
        self.scene = scene;
    }

    /// React to one trigger from a drawn chip on board `board`, out of
    /// `boards` boards. Call [`settle`](Self::settle) first so the rules see
    /// the scene on show.
    pub(super) fn on_trigger(
        &mut self,
        trigger: Trigger,
        board: usize,
        boards: usize,
        director: &mut Director,
        now: f64,
    ) {
        let load_rise = matches!(trigger, Trigger::LoadRise { .. });
        if load_rise {
            self.last_load_rise = Some(now);
        }
        match self.scene {
            Scene::Board(b) if b != board => {}
            Scene::Board(_) => {
                director.on_trigger(now);
            }
            Scene::All => {
                // With one board the super scene is that board's scene, so a
                // switch would change nothing on screen.
                let switches = load_rise && boards > 1;
                if director.on_trigger_with_scene(now, switches) && switches {
                    let at = director.swapped_at().unwrap_or(now);
                    self.pending = Some((Scene::Board(board), at));
                    self.cut_switch = Some((now, Scene::All, Scene::Board(board)));
                    self.auto_return = true;
                }
            }
        }
    }

    /// Go back to the super scene once [`RETURN_SECS`] have passed with no
    /// `LoadRise`, if the view made the switch to the board shown.
    pub(super) fn expire(&mut self, now: f64) {
        let quiet = self.last_load_rise.map_or(true, |t| now - t >= RETURN_SECS);
        if self.auto_return && self.pending.is_none() && quiet {
            self.scene = Scene::All;
            self.auto_return = false;
        }
    }

    /// True while a switch decided by a cut is still waiting for its time:
    /// the scene will change after `now` without anyone asking again.
    pub(super) fn pending_after(&self, now: f64) -> bool {
        matches!(self.pending, Some((_, at)) if now < at)
    }

    /// Hand a pending switch to a new cut that started at `cut_at` and swaps
    /// at `swap_at`: the switch now shows from `swap_at`, and the new cut's
    /// transition is the one that draws both scenes. Without this a cut made
    /// while a switch is pending would change scene at the old cut's midpoint,
    /// part way through its own transition. Does nothing with no switch
    /// pending.
    pub(super) fn hand_pending_to(&mut self, cut_at: f64, swap_at: f64) {
        let Some((scene, _)) = self.pending else {
            return;
        };
        self.pending = Some((scene, swap_at));
        let before = self.cut_switch.map_or(self.scene, |(_, before, _)| before);
        self.cut_switch = Some((cut_at, before, scene));
    }

    /// A scene the user chose: shown at once, with no automatic return and no
    /// pending switch left to override it.
    pub(super) fn choose(&mut self, scene: Scene) {
        self.pending = None;
        self.cut_switch = None;
        self.auto_return = false;
        self.set(scene);
    }
}
