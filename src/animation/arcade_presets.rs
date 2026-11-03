// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Named arcade layouts and the cycle that `{` and `}` step through.
//!
//! A layout is a name, a one-line description and the same rows of
//! weighted panels the `[arcade]` config uses (`arcade_layout::RowSpec`).
//! There are eight built-in layouts ([`presets`]). The config file can add
//! its own: the old `[[arcade.row]]` tables become a layout named `config`,
//! and each `[[arcade.layouts]]` table is one more (see `crate::config` and
//! `docs/arcade-layout.md`).
//!
//! # The cycle
//!
//! [`LayoutCycle`] holds every layout in the order `}` steps through them:
//! `config` when the file has one, then the file's `[[arcade.layouts]]` in
//! file order, then the presets. With no config file `classic` (the
//! arcade's own layout) is first. `{` steps back. Both ends wrap.
//!
//! The TUI keeps one cycle for the whole session, so the choice holds
//! across a resize and when you leave the arcade and come back. A switch
//! builds the arcade again at the same size from the new rows
//! (`ArcadeVisualization::relayout`), as a resize does.

use crate::animation::arcade_layout::{self, PanelSpec, RowSpec, View};

/// A layout with a name and a description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedLayout {
    /// Unique, case-insensitive. Letters, digits, `-` and `_`.
    pub name: String,
    /// One line for the status line, `/arcade layout list` and the docs.
    /// May be empty for a user layout.
    pub description: String,
    pub rows: Vec<RowSpec>,
    /// The arcade backdrop this layout sets when it is selected
    /// (`backdrop = "clouds"` in an `[[arcade.layouts]]` table; the older
    /// `cloudfield = true` is `backdrop = "scene"`). `Some(None)` turns the
    /// backdrop off; `None` leaves the current one alone. `w`, `c`,
    /// `/backdrop` and `/arcade cloudfield` still change it for the
    /// session; the next switch to a layout that has a backdrop applies it
    /// again. Applied by [`apply_backdrop`] to the TUI's backdrop settings
    /// for the `arcade` view.
    pub backdrop: Option<crate::animation::backdrop::Choice>,
}

/// The name the `[[arcade.row]]` tables get as a layout.
pub const CONFIG_LAYOUT: &str = "config";

/// The longest layout name, in characters.
pub const MAX_NAME: usize = 32;
/// The longest layout description, in characters.
pub const MAX_DESCRIPTION: usize = 80;

/// Words a user layout cannot be called: `config` is the `[[arcade.row]]`
/// layout, and the others are `/arcade layout` arguments.
const RESERVED: [&str; 4] = [CONFIG_LAYOUT, "next", "prev", "list"];

/// Whether `name` is kept for the `[[arcade.row]]` layout or a command word.
pub fn is_reserved(name: &str) -> bool {
    RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name))
}

/// What is wrong with `name` as a layout name, or `None` when it is fine.
/// A name is 1 to [`MAX_NAME`] characters of ASCII letters, digits, `-` and
/// `_`, so it can be typed after `/arcade layout` and printed on the status
/// line as it is.
pub fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("is empty".into());
    }
    if name.chars().count() > MAX_NAME {
        return Some(format!("is longer than {MAX_NAME} characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some("may hold only letters, digits, `-` and `_`".into());
    }
    None
}

/// The built-in layouts, in cycle order. `classic` is first and is the
/// arcade's own layout (`arcade_layout::default_rows`).
///
/// Each looks right at 80x24 and at 200x60. At small sizes the layout
/// engine takes out a panel that does not fit (the rightmost first) and
/// names it on its row's rule; `docs/arcade-layout.md` says what each one
/// shows at 80x24 and 60x20, and `each_preset_degrades_as_the_docs_say`
/// holds the code to it. No preset puts the castle in the last column: its
/// tower wall would end the row with a `│`.
pub fn presets() -> &'static [NamedLayout] {
    static PRESETS: std::sync::LazyLock<Vec<NamedLayout>> = std::sync::LazyLock::new(|| {
        use View::*;
        let p = |view, width| PanelSpec { view, width };
        let row = |height, panels: Vec<PanelSpec>| RowSpec { height, panels };
        let named = |name: &str, description: &str, rows| NamedLayout {
            name: name.into(),
            description: description.into(),
            rows,
            backdrop: None,
        };
        // The cycle order. The user asked for `now-serving` right after
        // `classic` "for now": the set of defaults is still being thought
        // through, so the order lives here and nowhere else.
        vec![
            named(
                "classic",
                "starfield; castle and defrag; flow (the default)",
                arcade_layout::default_rows(),
            ),
            // The user's words: "now serving: top: 3 wide inference
            // server, big bottom: left: 3d cube, right: defrag", and "this is
            // not the default" (no starfield) and "clouds should go behind
            // the whole arcade" (the layout sets the clouds backdrop, which
            // draws with or without an inference service). Heights 3:4
            // keep the inference panel at its 8 rows at 80x24 with the
            // bottom row the bigger one.
            NamedLayout {
                backdrop: Some(Some(crate::animation::backdrop::BackdropKind::Clouds)),
                ..named(
                    "now-serving",
                    concat!(
                        "inference over ",
                        crate::die3d_display_name!(),
                        " and defrag"
                    ),
                    vec![
                        row(3, vec![p(Inference, 1)]),
                        row(4, vec![p(Die3d, 1), p(Defrag, 1)]),
                    ],
                )
            },
            named(
                "defrag-wide",
                "starfield; castle beside a wide defrag; flow",
                vec![
                    row(3, vec![p(Starfield, 1)]),
                    row(4, vec![p(Castle, 1), p(Defrag, 3)]),
                    row(3, vec![p(Flow, 1)]),
                ],
            ),
            named(
                "waffler",
                concat!(
                    "starfield; castle beside a big ",
                    crate::die3d_display_name!(),
                    "; flow"
                ),
                vec![
                    row(2, vec![p(Starfield, 1)]),
                    row(5, vec![p(Castle, 2), p(Die3d, 3)]),
                    row(2, vec![p(Flow, 1)]),
                ],
            ),
            named(
                "workload",
                "training and inference over defrag",
                vec![
                    row(2, vec![p(Training, 1), p(Inference, 1)]),
                    row(1, vec![p(Defrag, 1)]),
                ],
            ),
            named(
                "deck",
                "all seven views; best at 120x40 and up",
                vec![
                    row(2, vec![p(Starfield, 1)]),
                    row(4, vec![p(Castle, 1), p(Defrag, 1), p(Die3d, 1)]),
                    row(3, vec![p(Flow, 1), p(Training, 1), p(Inference, 1)]),
                ],
            ),
            named(
                "skyline",
                "a tall starfield over the flow",
                vec![row(3, vec![p(Starfield, 1)]), row(1, vec![p(Flow, 1)])],
            ),
            named(
                "memory-lane",
                "memory only: flow over castle and a wide defrag",
                vec![
                    row(3, vec![p(Flow, 1)]),
                    row(2, vec![p(Castle, 1), p(Defrag, 3)]),
                ],
            ),
        ]
    });
    &PRESETS
}

/// The built-in layout called `name` (letter case ignored).
pub fn preset(name: &str) -> Option<&'static NamedLayout> {
    presets().iter().find(|l| l.name.eq_ignore_ascii_case(name))
}

/// Every layout in cycle order, and which is on screen.
#[derive(Debug, Clone)]
pub struct LayoutCycle {
    layouts: Vec<NamedLayout>,
    index: usize,
}

impl LayoutCycle {
    /// The cycle of the file's layouts `file` (already in cycle order:
    /// `config` first when there is one) followed by the presets, starting
    /// at `start`. An unknown or absent `start` starts with the first layout.
    /// `crate::config` reports an unknown name; here it only falls back.
    pub fn new(file: &[NamedLayout], start: Option<&str>) -> Self {
        let mut layouts = file.to_vec();
        layouts.extend(presets().iter().cloned());
        let mut c = LayoutCycle { layouts, index: 0 };
        if let Some(s) = start {
            c.select(s);
        }
        c
    }

    /// Every layout, in cycle order.
    pub fn layouts(&self) -> &[NamedLayout] {
        &self.layouts
    }

    /// How many layouts there are. Never 0: the presets are always there.
    pub fn len(&self) -> usize {
        self.layouts.len()
    }

    /// Always false; for clippy's `len_without_is_empty`.
    pub fn is_empty(&self) -> bool {
        self.layouts.is_empty()
    }

    /// The position of the layout on screen, from 0.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The layout on screen.
    pub fn current(&self) -> &NamedLayout {
        &self.layouts[self.index]
    }

    /// Step to the next layout (`}`); after the last comes the first.
    pub fn next(&mut self) {
        self.index = (self.index + 1) % self.layouts.len();
    }

    /// Step to the previous layout (`{`); before the first comes the last.
    pub fn prev(&mut self) {
        self.index = (self.index + self.layouts.len() - 1) % self.layouts.len();
    }

    /// The position of the layout called `name` (letter case ignored).
    pub fn find(&self, name: &str) -> Option<usize> {
        self.layouts
            .iter()
            .position(|l| l.name.eq_ignore_ascii_case(name))
    }

    /// Show the layout called `name`. False, and no change, when there is
    /// none.
    pub fn select(&mut self, name: &str) -> bool {
        match self.find(name) {
            Some(i) => {
                self.index = i;
                true
            }
            None => false,
        }
    }

    /// The layouts' names, comma-separated, in cycle order.
    pub fn names(&self) -> String {
        self.layouts
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The status line for the layout on screen:
    /// `layout: <name> (<i>/<n>), <backdrop> — <description>`. The backdrop
    /// part is there only for a layout that sets one, and the dash only
    /// with a description. The line is at most [`STATUS_MAX`] characters,
    /// so it fits an 80-column status bar with its two-column indent: the
    /// name, the place and the backdrop come first, and a description that
    /// does not fit is cut with `…`.
    pub fn status(&self) -> String {
        let l = self.current();
        let mut line = format!("layout: {} ({}/{})", l.name, self.index + 1, self.len());
        if let Some(c) = l.backdrop {
            match c {
                Some(k) => line.push_str(&format!(", {}", k.name())),
                None => line.push_str(", backdrop off"),
            }
        }
        if !l.description.is_empty() {
            line.push_str(" — ");
            line.push_str(&l.description);
        }
        cut(&line, STATUS_MAX)
    }

    /// The names of every layout, in cycle order, wrapped to lines of at
    /// most [`STATUS_MAX`] characters, each indented by two spaces.
    pub fn wrapped_names(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::from(" ");
        for (i, l) in self.layouts.iter().enumerate() {
            let word = if i + 1 < self.layouts.len() {
                format!(" {},", l.name)
            } else {
                format!(" {}", l.name)
            };
            if cur.chars().count() + word.chars().count() > STATUS_MAX && cur.trim() != "" {
                lines.push(cur);
                cur = String::from(" ");
            }
            cur.push_str(&word);
        }
        lines.push(cur);
        lines.join("\n")
    }

    /// The message for an unknown layout name, for the config report.
    pub fn unknown(&self, name: &str) -> String {
        format!(
            "unknown arcade layout `{}` (expected one of {})",
            shown(name),
            self.names()
        )
    }
}

/// What `/arcade ...` did: the status-line message, whether it is an error,
/// and whether the layout changed, so the arcade must be laid out again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub message: String,
    pub error: bool,
    pub relayout: bool,
}

/// The longest status-line message this module gives, in characters: an
/// 80-column status bar less its two-column indent.
pub const STATUS_MAX: usize = 78;

/// `text` cut to `max` characters, with `…` as the last one when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A name a person typed, as it can be shown: control characters escaped
/// and cut to 40 characters, so a message cannot carry a terminal escape.
pub fn shown(name: &str) -> String {
    cut(&name.escape_debug().to_string(), 40)
}

/// The usage text for `/arcade`, one line per verb.
pub const USAGE: &str = "/arcade layout next|prev|<name>|list   switch the layout ({ } step)\n/arcade cloudfield on|off   the old name for /backdrop scene|off here";

/// The view name the arcade's backdrop setting is kept under.
const ARCADE: &str = "arcade";

/// The command bar's `/arcade` command. `layout next`, `layout prev`,
/// `layout <name>` switch the layout and apply its backdrop; `layout list`
/// (or `layout` alone) names them all. Anything else goes to
/// `arcade_layout::command`, which handles `cloudfield` (the scene
/// backdrop on or off) in `settings`.
pub fn command(
    arg: &str,
    settings: &mut crate::animation::backdrop::BackdropSettings,
    cycle: &mut LayoutCycle,
) -> Reply {
    use crate::animation::backdrop::BackdropKind;
    let mut words = arg.split_whitespace();
    if words.next() != Some("layout") {
        let mut on = settings.choice(ARCADE) == Some(BackdropKind::Scene);
        let (message, error) = arcade_layout::command(arg, &mut on);
        // Only a good command changes the setting: a typo must not turn
        // off a sky or clouds backdrop.
        if !error {
            let _ = settings.set(ARCADE, on.then_some(BackdropKind::Scene));
        }
        // A bad command shows the usage of both verbs.
        let message = if error && message == arcade_layout::USAGE {
            USAGE.to_string()
        } else {
            message
        };
        return Reply {
            message,
            error,
            relayout: false,
        };
    }
    let reply = |message: String, error, relayout| Reply {
        message,
        error,
        relayout,
    };
    match (words.next(), words.next()) {
        (None, _) | (Some("list"), None) => reply(
            format!(
                "layouts, now {} ({}/{}):\n{}",
                cycle.current().name,
                cycle.index() + 1,
                cycle.len(),
                cycle.wrapped_names()
            ),
            false,
            false,
        ),
        (Some("next"), None) => reply(step_and_apply(cycle, Step::Next, settings), false, true),
        (Some("prev"), None) => reply(step_and_apply(cycle, Step::Prev, settings), false, true),
        (Some(name), None) => {
            if cycle.select(name) {
                apply_backdrop(cycle.current(), settings);
                reply(cycle.status(), false, true)
            } else {
                reply(
                    format!(
                        // Two lines, so the longest shown name fits 80 columns.
                        "no arcade layout `{}`\n/arcade layout list names them",
                        shown(name)
                    ),
                    true,
                    false,
                )
            }
        }
        (Some(_), Some(_)) => reply(USAGE.to_string(), true, false),
    }
}

/// Apply `layout`'s own backdrop, if it has one, to the arcade's setting
/// in `settings` (the TUI hands it to the arcade on the next update).
/// Called when a layout is selected: at start-up, on `{` or `}`, and on
/// `/arcade layout`.
pub fn apply_backdrop(
    layout: &NamedLayout,
    settings: &mut crate::animation::backdrop::BackdropSettings,
) {
    if let Some(c) = layout.backdrop {
        // The arcade refuses no kind, so this cannot fail.
        let _ = settings.set(ARCADE, c);
    }
}

/// Take `step` through `cycle`, apply the new layout's backdrop to
/// `settings`, and return the status line. The `{` and `}` keys.
pub fn step_and_apply(
    cycle: &mut LayoutCycle,
    step: Step,
    settings: &mut crate::animation::backdrop::BackdropSettings,
) -> String {
    cycle.step(step);
    apply_backdrop(cycle.current(), settings);
    cycle.status()
}

/// The arcade backdrop after the `c` key: off when one is drawn, else the
/// layout's own kind, else the scene (the old cloudfield).
pub fn toggled_backdrop(
    layout: &NamedLayout,
    current: crate::animation::backdrop::Choice,
) -> crate::animation::backdrop::Choice {
    use crate::animation::backdrop::BackdropKind;
    match current {
        Some(_) => None,
        None => Some(layout.backdrop.flatten().unwrap_or(BackdropKind::Scene)),
    }
}

/// The key that steps to the next layout while the arcade is on screen.
pub const NEXT_KEY: char = '}';
/// The key that steps to the previous layout while the arcade is on screen.
pub const PREV_KEY: char = '{';

/// A step through the cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Prev,
}

/// The step key `c` stands for, or `None` for any other key.
pub fn step_for(c: char) -> Option<Step> {
    match c {
        NEXT_KEY => Some(Step::Next),
        PREV_KEY => Some(Step::Prev),
        _ => None,
    }
}

impl LayoutCycle {
    /// Take `step`.
    pub fn step(&mut self, step: Step) {
        match step {
            Step::Next => self.next(),
            Step::Prev => self.prev(),
        }
    }
}

#[cfg(test)]
#[path = "arcade_presets_tests.rs"]
mod tests;
