// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Die 3D view's slash commands.
//!
//! The view used to take a letter key for each of its settings. Keys are kept
//! for what is the same everywhere (`[` `]` change board, Space cuts, `3` opens
//! and leaves the view); every setting that belongs to this view alone is a slash
//! command here, so it can be named in `/help` and in the footer and no letter is
//! used up.
//!
//! [`COMMANDS`] is the one list. The help overlay and the footer read it, and a
//! test checks that every entry parses and every verb the parser knows is in it.
//! To add a command: add an entry, then a match arm in `Die3dView::command`.

use super::camera::TransitionMode;
use super::switching::Scene;
use super::Die3dView;

/// One command, as `/help` lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandInfo {
    /// The verb, without the slash.
    pub name: &'static str,
    /// What follows the verb, as `[on|off]`. Empty when it takes nothing.
    pub args: &'static str,
    /// One line: what it does.
    pub help: &'static str,
}

/// Every Die 3D command, in the order `/help` lists them.
pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "board",
        args: "[next|prev|N]",
        help: "show board N (1 is the first); next and prev step like [ and ]",
    },
    CommandInfo {
        name: "scene",
        args: "[all|board]",
        help: "every board together, or one board; bare flips",
    },
    CommandInfo {
        name: "cut",
        args: "",
        help: "cut the camera now (Space does the same)",
    },
    CommandInfo {
        name: "autocut",
        args: "[on|off]",
        help: "cut the camera when something happens on a chip",
    },
    CommandInfo {
        name: "transition",
        args: "[shuffle|chaos|dissolve|spin]",
        help: "how cuts change the picture; bare steps to the next",
    },
    CommandInfo {
        name: "cells",
        args: "[braille|half]",
        help: "braille dots or half blocks; bare flips",
    },
    CommandInfo {
        name: "bg",
        args: "[black|transparent]",
        help: "black or the terminal's own background; bare flips",
    },
    CommandInfo {
        name: "labels",
        args: "[on|off]",
        help: "a flag with name and power on each chip",
    },
    CommandInfo {
        name: "clouds",
        args: "[on|off]",
        help: "the clouds behind the board; their drift follows PCIe traffic",
    },
    CommandInfo {
        name: "history",
        args: "[on|off]",
        help: "the power and PCIe plot, top left",
    },
];

/// Whether `verb` (lowercase, no slash) is a Die 3D command.
pub fn is_command(verb: &str) -> bool {
    COMMANDS.iter().any(|c| c.name == verb)
}

/// `/name args` for `/help` and error messages.
pub fn usage(info: &CommandInfo) -> String {
    if info.args.is_empty() {
        format!("/{}", info.name)
    } else {
        format!("/{} {}", info.name, info.args)
    }
}

/// An on/off argument: empty flips `current`; `on`, `1` and `true` set it;
/// `off`, `0` and `false` clear it; anything else is `None`.
fn on_off(arg: &str, current: bool) -> Option<bool> {
    match arg {
        "" => Some(!current),
        "on" | "1" | "true" | "yes" => Some(true),
        "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn word(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

impl Die3dView {
    /// Run `/verb arg` and return the message to show and whether it is an
    /// error. `verb` is lowercase without the slash. A verb that is not in
    /// [`COMMANDS`] is an error here too, so a caller need not check first.
    ///
    /// Like a key, a command shows in the next frame, and a scene chosen
    /// with `/board` or `/scene` is the user's: the view will not switch back
    /// from it on its own.
    pub fn command(&mut self, verb: &str, arg: &str, now: f64) -> (String, bool) {
        let arg = arg.trim().to_lowercase();
        let arg = arg.as_str();
        let Some(info) = COMMANDS.iter().find(|c| c.name == verb) else {
            return (format!("unknown Die 3D command: /{verb}"), true);
        };
        let bad = || (format!("usage: {}", usage(info)), true);
        // Whatever a command does shows in the next frame.
        self.revision += 1;
        let boards = self.chips.len();
        match verb {
            "board" => {
                if boards == 0 {
                    return ("no board to show".to_string(), true);
                }
                match arg {
                    "" | "next" => self.handle_key(super::Die3dKey::NextBoard, now),
                    "prev" | "previous" => self.handle_key(super::Die3dKey::PrevBoard, now),
                    n => match n.parse::<usize>() {
                        Ok(n) if (1..=boards).contains(&n) => {
                            self.switcher.choose(Scene::Board(n - 1))
                        }
                        _ => return (format!("board: 1 to {boards}"), true),
                    },
                }
                let shown = self.switcher.board_at(now).min(boards - 1);
                (format!("board {}/{}", shown + 1, boards), false)
            }
            "scene" => {
                let all = matches!(self.scene_at(now), Scene::All);
                let want_all = match arg {
                    "" => !all,
                    "all" => true,
                    "board" | "one" => false,
                    _ => return bad(),
                };
                let next = if want_all {
                    Scene::All
                } else {
                    Scene::Board(self.switcher.board_at(now))
                };
                self.switcher.choose(next);
                (
                    format!(
                        "scene: {}",
                        if want_all { "all boards" } else { "one board" }
                    ),
                    false,
                )
            }
            "cut" => {
                self.handle_key(super::Die3dKey::CutNow, now);
                ("cut".to_string(), false)
            }
            "autocut" => match on_off(arg, self.director.auto()) {
                Some(on) => {
                    self.director.set_auto(on);
                    (format!("autocut: {}", word(on)), false)
                }
                None => bad(),
            },
            "transition" => {
                let mode = match arg {
                    "" => self.director.transition_mode().next(),
                    a => match TransitionMode::parse(a) {
                        Some(m) => m,
                        None => return bad(),
                    },
                };
                self.director.set_transition_mode(mode);
                (format!("transition: {}", mode.label()), false)
            }
            "cells" => {
                let braille = match arg {
                    "" => !self.braille,
                    "braille" => true,
                    "half" | "half-block" | "blocks" => false,
                    _ => return bad(),
                };
                self.braille = braille;
                (
                    format!("cells: {}", if braille { "braille" } else { "half-block" }),
                    false,
                )
            }
            "bg" => {
                let transparent = match arg {
                    "" => !self.transparent,
                    "transparent" | "terminal" => true,
                    "black" | "opaque" => false,
                    _ => return bad(),
                };
                self.transparent = transparent;
                (
                    format!(
                        "background: {}",
                        if transparent { "transparent" } else { "black" }
                    ),
                    false,
                )
            }
            "labels" => match on_off(arg, self.show_labels) {
                Some(on) => {
                    self.show_labels = on;
                    (format!("labels: {}", word(on)), false)
                }
                None => bad(),
            },
            "clouds" => match on_off(arg, self.show_clouds) {
                Some(on) => {
                    self.show_clouds = on;
                    (format!("clouds: {}", word(on)), false)
                }
                None => bad(),
            },
            "history" => match on_off(arg, self.show_history) {
                Some(on) => {
                    self.show_history = on;
                    (format!("history: {}", word(on)), false)
                }
                None => bad(),
            },
            _ => unreachable!("every name in COMMANDS has an arm"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::die3d::test_backend::Fake;

    fn view() -> Die3dView {
        let b = Fake::boards(&["p300"; 4], Some("100"));
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        v
    }

    #[test]
    fn every_listed_command_runs_and_every_verb_is_listed() {
        for info in COMMANDS {
            let mut v = view();
            let (msg, err) = v.command(info.name, "", 1.0);
            assert!(!err, "/{} with no argument failed: {msg}", info.name);
            assert!(is_command(info.name));
        }
        let mut v = view();
        assert!(v.command("nope", "", 1.0).1, "an unlisted verb is an error");
        assert!(!is_command("nope"));
        // A listed name must be unique, or the second would never run.
        let mut names: Vec<_> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len());
    }

    #[test]
    fn on_off_commands_set_flip_and_refuse() {
        let mut v = view();
        assert!(v.show_clouds);
        assert_eq!(
            v.command("clouds", "off", 1.0),
            ("clouds: off".into(), false)
        );
        assert!(!v.show_clouds);
        assert_eq!(v.command("clouds", "", 1.0), ("clouds: on".into(), false));
        assert!(v.show_clouds, "bare flips");
        let (msg, err) = v.command("clouds", "maybe", 1.0);
        assert!(err && msg.contains("usage: /clouds"), "{msg}");
        assert!(v.show_clouds, "a refused argument changes nothing");
        // Case and spacing do not matter.
        assert_eq!(v.command("labels", "  OFF ", 1.0).0, "labels: off");
    }

    #[test]
    fn board_takes_next_prev_and_a_number() {
        let mut v = view();
        // 4 p300 chips are two boards.
        let boards = v.chips.len();
        assert_eq!(boards, 2);
        assert_eq!(v.command("board", "2", 1.0), ("board 2/2".into(), false));
        assert_eq!(v.command("board", "1", 2.0).0, "board 1/2");
        assert_eq!(v.command("board", "next", 3.0).0, "board 2/2");
        assert_eq!(v.command("board", "prev", 4.0).0, "board 1/2");
        for bad in ["0", "3", "x"] {
            assert!(v.command("board", bad, 5.0).1, "{bad}");
        }
    }

    #[test]
    fn a_command_does_what_its_old_key_did() {
        // The same state change as the key it replaces, so the two cannot drift.
        let mut by_cmd = view();
        let mut by_key = view();
        by_cmd.command("cells", "", 1.0);
        by_key.handle_key(super::super::Die3dKey::ToggleBraille, 1.0);
        assert_eq!(by_cmd.braille, by_key.braille);
        by_cmd.command("bg", "", 1.0);
        by_key.handle_key(super::super::Die3dKey::ToggleTransparent, 1.0);
        assert_eq!(by_cmd.transparent, by_key.transparent);
        by_cmd.command("scene", "", 1.0);
        by_key.handle_key(super::super::Die3dKey::ToggleScene, 1.0);
        assert_eq!(by_cmd.scene_at(2.0), by_key.scene_at(2.0));
        by_cmd.command("transition", "", 1.0);
        by_key.handle_key(super::super::Die3dKey::CycleTransition, 1.0);
        assert_eq!(
            by_cmd.director.transition_mode(),
            by_key.director.transition_mode()
        );
    }
}
