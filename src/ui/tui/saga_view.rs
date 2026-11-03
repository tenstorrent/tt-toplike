// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Saga view (`S`): a hero's quest through a dungeon that the machine's
//! own signals build, with a running chronicle of what happens (spec
//! sections 1, 5 and 8).
//!
//! The story is told by the pure core in the `tt_toplike_saga` crate, which
//! the saga driver (`crate::saga::SagaDriver`) runs on its own thread from
//! start-up. This module only draws the driver's last published
//! [`SagaState`]: [`render_saga`] turns it into lines for one frame, and
//! keeps no state of its own between frames.
//!
//! # What is drawn
//!
//! * **Title row.** `▌ SAGA`, the label mark ("(mock)" or "(replay)", spec
//!   3.9), the world's id and biome, and the hero's floor and its chip.
//! * **Map.** The hero's floor, with the cell glyphs from the pack's glyph
//!   set ([`Pack::glyphs`]) and colours mixed from the pack's palette
//!   ([`Pack::palette`], the Tenstorrent brand colours). The hero `@` is
//!   bold on a yellow background (orange while its vitality is below
//!   [`REST_BELOW`]); the quest's goal is highlighted, and a sage quest
//!   draws the sage's `&` there. A floor larger than the pane (floors are
//!   at most 120 x 60 cells) scrolls to keep the hero in view; a smaller one
//!   is centred across the pane and drawn from the top.
//! * **Chronicle.** The newest whole beats, newest at the bottom, each in its
//!   tone's colour (teal calm, orange tense, yellow triumphant) after its
//!   tick in a dim colour. With "why" on (`y`, spec 5) each beat is
//!   followed by its cause: the entity, the signal and the record's
//!   position in the stream. Beats do not store their world, so the world
//!   id is on the title row only.
//! * **Status row.** The hero's name, its vitality as a small bar, and its
//!   quest.
//!
//! Before the first world (the core holds a placeholder world with id 0,
//! [`SagaState::has_world`]) the view says the chronicle has not begun. When
//! the driver has stopped it says so in a banner and still draws the last
//! state.
//!
//! # Names
//!
//! In generated mode (`[saga] names = "generated"`) the chronicle's beats
//! and their causes already carry aliases (ledger ruling R17). The state's
//! `quest` does not: it keeps the real target so both modes plan alike (the
//! saga module comment, "Names"). So the status row never prints a
//! non-hardware quest target from the quest itself. It takes the target's
//! name from the newest beat whose cause is the quest's reason (same signal
//! and ordinal); that beat's cause is aliased in generated mode and real in
//! real mode. Chip keys are PCI addresses and show as they are, as the core
//! keeps them.
//!
//! # Sizes
//!
//! | Area | Layout |
//! |---|---|
//! | `w < 2` or `h == 0` | nothing |
//! | below 20 x 6 | the title row only |
//! | below 40 x 12 | the title row and the map |
//! | 40 x 12 and up, under 100 columns | the map above the chronicle (80 x 24 is this one) |
//! | 100 columns and up | the map left, the chronicle right |
//!
//! Every row is built by [`Row`], which never lets a row grow past the
//! area's width, so no size up to 500 x 200 overflows.
//!
//! # Borders
//!
//! Left and bottom bars only (`/home/ttuser/CLAUDE.md`). The chronicle has a
//! `║` bar on its left and a `╔═` header; nothing draws a bar on the right.
//! The map's walls use the pack's wall glyph, `#`, which is not one of the
//! right-edge border glyphs (`crate::animation::arcade::RIGHT_EDGE_GLYPHS`),
//! so a wall in the last column is a map cell. A future pack that picks `|`
//! for walls would need a check here.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tt_toplike_saga::chronicle::Beat;
use tt_toplike_saga::maze::{Cell, Floor};
use tt_toplike_saga::pack::{Pack, Rgb};
use tt_toplike_saga::quest::{Verb, REST_BELOW};
use tt_toplike_saga::{Biome, SagaState};
use unicode_width::UnicodeWidthChar;

use crate::ui::colors;

/// The key that enters and leaves Saga from any view. Free in every view
/// and in every view's own key table (`the_entry_key_is_free`).
pub const ENTRY_KEY: char = 'S';

/// The key that shows or hides each chronicle line's cause, in Saga only.
/// Insights uses `y` for its kill dialog; that arm is guarded to Insights.
pub const WHY_KEY: char = 'y';

/// The view's name on the status bar, in help and in command replies.
pub const DISPLAY_NAME: &str = "Saga";

/// The mode's name for `/mode saga`, `/saga` and the `ViewChanged` event.
pub const MODE_NAME: &str = "saga";

/// Saga's own rows in the Help overlay, as `(key, what it does)` cells.
pub const HELP_KEYS: &[&[(&str, &str)]] = &[
    &[("y", "why: each line's cause"), ("S Esc", "leave")],
    &[("/saga", "open from the command bar")],
];

/// The `!` text. Each claim is checked against the code: the floors in
/// `tt_toplike_saga::maze`, worlds in `world`, determinism in the crate's
/// docs, causes in `chronicle`, names in `config.rs`, labels in spec 3.9.
pub const EXPLAIN_TEXT: &[&str] = &[
    "Saga",
    "",
    "A hero quests through a dungeon built from this box's",
    "own signals. Each chip is a floor: rooms and walls from",
    "its tile grid, a vault per GDDR channel, a portal per",
    "live Ethernet link and a gate for PCIe. A model coming",
    "up raises a world with a sage to visit. A reset, the",
    "model going away or a quiet season ends it, and the",
    "next world gets a new maze.",
    "",
    "The same recording gives the same story. The saga reads",
    "the order of the bus's records and ignores their clocks.",
    "",
    "The fiction is decoration. Every chronicle line names",
    "the signal behind it: press y to see each line's",
    "entity, signal and place in the stream.",
    "",
    "For screen sharing, set names = \"generated\" under",
    "[saga] in config.toml to replace model and process",
    "names with made-up ones.",
    "",
    "(mock) in the title means the readings are estimates",
    "from --mock. (replay) means a recorded log is playing.",
];

/// What the view draws besides the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SagaFlags {
    /// The driver has stopped (`Published::stopped`): show the banner.
    pub stopped: bool,
    /// Show each chronicle line's cause (the `y` key).
    pub why: bool,
}

/// From this width the chronicle sits beside the map.
const SIDE_MIN_COLS: usize = 100;
/// From this size (columns, rows) the chronicle is drawn at all.
const CHRONICLE_MIN: (usize, usize) = (40, 12);
/// From this size (columns, rows) the map is drawn; below it, the title row.
const MAP_MIN: (usize, usize) = (20, 6);
/// The vitality bar's cells.
const VITALITY_CELLS: usize = 8;
/// Frames per step of the portals' shimmer.
const SHIMMER_FRAMES: u64 = 3;
/// The chronicle's left bar.
const BAR: &str = "║ ";

/// The arrangement for an area. See "Sizes" in the module comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Nothing,
    TitleOnly,
    MapOnly,
    Stacked,
    Side,
}

fn layout_for(w: usize, h: usize) -> Layout {
    if w < 2 || h == 0 {
        Layout::Nothing
    } else if w < MAP_MIN.0 || h < MAP_MIN.1 {
        Layout::TitleOnly
    } else if w < CHRONICLE_MIN.0 || h < CHRONICLE_MIN.1 {
        Layout::MapOnly
    } else if w < SIDE_MIN_COLS {
        Layout::Stacked
    } else {
        Layout::Side
    }
}

// ---------------------------------------------------------------------------
// Rows that cannot overflow
// ---------------------------------------------------------------------------

/// One row of the frame, built left to right, that never grows past `cap`
/// columns: [`Row::push`] cuts what does not fit. Runs of one style are
/// kept in one span.
struct Row {
    spans: Vec<Span<'static>>,
    run: String,
    run_style: Style,
    used: usize,
    cap: usize,
}

impl Row {
    fn new(cap: usize) -> Row {
        Row {
            spans: Vec::new(),
            run: String::new(),
            run_style: Style::default(),
            used: 0,
            cap,
        }
    }

    /// Columns still free.
    fn room(&self) -> usize {
        self.cap - self.used
    }

    /// Appends `text` in `style`, cut to the room left. A control character
    /// (text from the bus can hold any) shows as `?`, and a zero-width one
    /// is dropped, so the column count stays true.
    fn push(&mut self, text: &str, style: Style) -> &mut Row {
        for ch in text.chars() {
            let ch = if ch.is_control() { '?' } else { ch };
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if cw == 0 {
                continue;
            }
            if cw > self.room() {
                break;
            }
            if style != self.run_style {
                self.flush();
                self.run_style = style;
            }
            self.run.push(ch);
            self.used += cw;
        }
        self
    }

    /// Pads with unstyled spaces to column `col` (or the cap).
    fn pad_to(&mut self, col: usize) -> &mut Row {
        let n = col.min(self.cap).saturating_sub(self.used);
        self.push(&" ".repeat(n), Style::default())
    }

    fn flush(&mut self) {
        if !self.run.is_empty() {
            self.spans
                .push(Span::styled(std::mem::take(&mut self.run), self.run_style));
        }
    }

    fn line(mut self) -> Line<'static> {
        self.flush();
        Line::from(self.spans)
    }

    /// This row, padded to its cap, then `gap` spaces, then `right`: one
    /// line no wider than `cap + gap + right.cap`.
    fn join(mut self, gap: usize, right: Row) -> Line<'static> {
        self.pad_to(self.cap);
        self.flush();
        let mut spans = self.spans;
        if gap > 0 {
            spans.push(Span::raw(" ".repeat(gap)));
        }
        spans.extend(right.line().spans);
        Line::from(spans)
    }
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// `pct` percent of the way from `a` to `b`, in integers so a frame's
/// colours are the same on every machine.
fn mix(a: Rgb, b: Rgb, pct: u32) -> Rgb {
    let m = |x: u8, y: u8| ((x as u32 * (100 - pct) + y as u32 * pct + 50) / 100) as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

fn rgb(c: Rgb) -> Color {
    colors::rgb(c[0], c[1], c[2])
}

fn fg(c: Rgb) -> Style {
    Style::default().fg(rgb(c))
}

/// Every style the view uses, mixed from the pack's palette once a frame
/// (through `colors::rgb`, so the app's theme and colour depth apply).
struct Look {
    wall: Style,
    floor: Style,
    ruin: Style,
    vault: Style,
    gate: Style,
    portal_dim: Style,
    portal_lit: Style,
    door: Style,
    exit: Style,
    hero_well: Style,
    hero_weak: Style,
    goal: Style,
    title: Style,
    title_bar: Style,
    label: Style,
    banner: Style,
    dim: Style,
    bar: Style,
    head: Style,
    name: Style,
    quest: Style,
    why: Style,
    calm: Style,
    tense: Style,
    triumphant: Style,
}

impl Look {
    fn new() -> Look {
        let p = &Pack::builtin().palette;
        let bold = Modifier::BOLD;
        Look {
            wall: fg(mix(p.base, p.green, 45)),
            floor: fg(mix(p.base, p.teal, 50)),
            ruin: fg(mix(p.base, p.yellow, 55)),
            vault: fg(p.yellow).add_modifier(bold),
            gate: fg(p.accent).add_modifier(bold),
            portal_dim: fg(mix(p.base, p.teal, 70)),
            portal_lit: fg(p.teal).add_modifier(bold),
            door: fg(mix(p.base, p.orange, 75)),
            exit: fg(p.green).add_modifier(bold),
            hero_well: fg(p.base).bg(rgb(p.yellow)).add_modifier(bold),
            hero_weak: fg(p.base).bg(rgb(p.orange)).add_modifier(bold),
            goal: goal_style(),
            title: fg(p.teal).add_modifier(bold),
            title_bar: fg(p.accent),
            label: fg(p.yellow).add_modifier(bold),
            banner: fg(p.base).bg(rgb(p.orange)).add_modifier(bold),
            dim: fg(mix(p.base, p.green, 60)),
            bar: fg(mix(p.base, p.accent, 60)),
            head: fg(p.teal).add_modifier(bold),
            name: fg(p.teal).add_modifier(bold),
            quest: fg(p.green),
            why: fg(mix(p.base, p.green, 60)).add_modifier(Modifier::ITALIC),
            calm: fg(p.tone(tt_toplike_saga::chronicle::Tone::Calm)),
            tense: fg(p.tone(tt_toplike_saga::chronicle::Tone::Tense)),
            triumphant: fg(p.tone(tt_toplike_saga::chronicle::Tone::Triumphant)),
        }
    }

    fn tone(&self, beat: &Beat) -> Style {
        use tt_toplike_saga::chronicle::Tone;
        match beat.tone {
            Tone::Calm => self.calm,
            Tone::Tense => self.tense,
            Tone::Triumphant => self.triumphant,
        }
    }
}

/// The style of the quest's goal cell: yellow and bold on a dark yellow
/// background. Public for the tests, which look for it on the map.
pub fn goal_style() -> Style {
    let p = &Pack::builtin().palette;
    fg(p.yellow)
        .bg(rgb(mix(p.base, p.yellow, 35)))
        .add_modifier(Modifier::BOLD)
}

// ---------------------------------------------------------------------------
// The frame
// ---------------------------------------------------------------------------

/// One frame of the Saga view for an area of `area` = (columns, rows): at
/// most `rows` lines, none wider than `columns`. `frame` drives the small
/// animations (the portals' shimmer, the waiting dots), so a fixed frame
/// draws a fixed picture. See the module comment for the layout.
pub fn render_saga(
    state: &SagaState,
    flags: SagaFlags,
    area: (u16, u16),
    frame: u64,
) -> Vec<Line<'static>> {
    let (w, h) = (area.0 as usize, area.1 as usize);
    let layout = layout_for(w, h);
    if layout == Layout::Nothing {
        return Vec::new();
    }
    let look = Look::new();
    if !state.has_world() {
        return waiting(state, flags, &look, layout, w, h, frame);
    }
    // At the two small sizes the stopped mark goes on the title row; the
    // larger ones give it a row of its own.
    let small = matches!(layout, Layout::TitleOnly | Layout::MapOnly);
    let mut out = vec![title_row(state, &look, w, flags.stopped && small).line()];
    match layout {
        Layout::Nothing | Layout::TitleOnly => {}
        Layout::MapOnly => {
            out.extend(
                map_rows(state, &look, w, h - 1, frame)
                    .into_iter()
                    .map(Row::line),
            );
        }
        Layout::Stacked => {
            if flags.stopped {
                out.push(banner_row(&look, w).line());
            }
            // Title, banner and status rows; the rest is shared by the map
            // and the chronicle, the map taking up to three fifths and the
            // chronicle at least its header and two lines.
            let body = h - out.len() - 1;
            let floor_h = hero_floor(state).map_or(1, |f| f.h as usize);
            let map_h = floor_h.min(body * 3 / 5).max(1).min(body - 3);
            out.extend(
                map_rows(state, &look, w, map_h, frame)
                    .into_iter()
                    .map(Row::line),
            );
            out.extend(
                chronicle_rows(state, flags, &look, w, body - map_h)
                    .into_iter()
                    .map(Row::line),
            );
            out.push(status_row(state, &look, w).line());
        }
        Layout::Side => {
            if flags.stopped {
                out.push(banner_row(&look, w).line());
            }
            let body = h - out.len() - 1;
            // The map pane is the floor's width and a column each side, up
            // to three fifths of the width; the chronicle has the rest, after
            // a one-column gap.
            let floor_w = hero_floor(state).map_or(1, |f| f.w as usize);
            let map_w = (floor_w + 2).min(w * 3 / 5);
            let chron_w = w - map_w - 1;
            let maps = map_rows(state, &look, map_w, body, frame);
            let chron = chronicle_rows(state, flags, &look, chron_w, body);
            out.extend(maps.into_iter().zip(chron).map(|(m, c)| m.join(1, c)));
            out.push(status_row(state, &look, w).line());
        }
    }
    out
}

/// The view when there is no driver: `[saga] enabled = false` (`enabled`
/// false), or a driver that could not start. A title row and a note, wrapped
/// to the width.
pub fn render_saga_off(area: (u16, u16), enabled: bool) -> Vec<Line<'static>> {
    let (w, h) = (area.0 as usize, area.1 as usize);
    if layout_for(w, h) == Layout::Nothing {
        return Vec::new();
    }
    let look = Look::new();
    let mut title = Row::new(w);
    title
        .push("▌", look.title_bar)
        .push(" SAGA", look.title)
        .push(" · off", look.dim);
    let mut out = vec![title.line()];
    let note = if enabled {
        "Saga is not running: there is no bus to read here, or the story could not start (the log says why)."
    } else {
        "Saga is off: set [saga] enabled = true in config.toml and restart."
    };
    // A blank row, then the note two columns in, wrapped to the width.
    if h > 2 {
        out.push(Line::default());
    }
    for text in wrap(note, w.saturating_sub(2), w.saturating_sub(2)) {
        if out.len() >= h {
            break;
        }
        let mut row = Row::new(w);
        row.push("  ", Style::default()).push(&text, look.quest);
        out.push(row.line());
    }
    out
}

/// The hero's floor, or the world's first floor when the hero's index is
/// past the end (the core plans on the first floor then too).
fn hero_floor(state: &SagaState) -> Option<&Floor> {
    let floors = &state.world.floors;
    floors.get(state.hero.floor as usize).or(floors.first())
}

/// `▌ SAGA (mock) · world 3 · dungeon · floor 1 of 2 · chip 0000:01:00.0`,
/// cut to `w`. The label comes right after the name so a narrow title keeps
/// it, then the stopped mark when `stopped_mark` is set.
fn title_row(state: &SagaState, look: &Look, w: usize, stopped_mark: bool) -> Row {
    let mut row = Row::new(w);
    row.push("▌", look.title_bar).push(" SAGA", look.title);
    if let Some(mark) = state.label.mark() {
        row.push(" ", Style::default()).push(mark, look.label);
    }
    if stopped_mark {
        row.push(" stopped", look.banner);
    }
    if state.has_world() {
        let world = &state.world;
        let mut info = format!(" · world {} · {}", world.id, biome_word(world.biome));
        let n = world.floors.len();
        if n > 0 {
            let i = (state.hero.floor as usize).min(n - 1);
            info.push_str(&format!(" · floor {} of {n}", i + 1));
            if let Some(chip) = world.chips.get(i) {
                info.push_str(&format!(" · chip {}", key_id(chip.as_str())));
            }
        }
        row.push(&info, look.dim);
    } else {
        row.push(" · waiting", look.dim);
    }
    row
}

/// The stopped banner, a row of its own at the larger sizes.
fn banner_row(look: &Look, w: usize) -> Row {
    let mut row = Row::new(w);
    row.push(
        " the story has stopped · showing its last state ",
        look.banner,
    );
    row
}

fn biome_word(b: Biome) -> &'static str {
    match b {
        Biome::Dungeon => "dungeon",
        Biome::Forge => "forge",
        Biome::Dream => "dream",
    }
}

/// An entity key's id: `model:demo-sage` gives `demo-sage`, a chip key its
/// PCI address.
fn key_id(key: &str) -> &str {
    key.split_once(':').map_or(key, |(_, id)| id)
}

// ---------------------------------------------------------------------------
// The waiting screen
// ---------------------------------------------------------------------------

/// Before the first world: the title, then a few calm centred lines.
fn waiting(
    state: &SagaState,
    flags: SagaFlags,
    look: &Look,
    layout: Layout,
    w: usize,
    h: usize,
    frame: u64,
) -> Vec<Line<'static>> {
    let small = matches!(layout, Layout::TitleOnly | Layout::MapOnly);
    let mut out = vec![title_row(state, look, w, flags.stopped && small).line()];
    if layout == Layout::TitleOnly {
        return out;
    }
    if flags.stopped && !small {
        out.push(banner_row(look, w).line());
    }
    // A dot that walks along three, one step every other frame.
    let lit = (frame / 2 % 3) as usize;
    let dots: Vec<(&str, Style)> = (0..3)
        .flat_map(|i| {
            let s = if i == lit { look.title } else { look.dim };
            [("·", s), (if i < 2 { "   " } else { "" }, Style::default())]
        })
        .collect();
    let lines: [Vec<(&str, Style)>; 4] = [
        vec![("the chronicle has not begun", look.head)],
        vec![("the machine's first signals will start the story", look.dim)],
        vec![],
        dots,
    ];
    let top = out.len() + (h - out.len()).saturating_sub(lines.len()) / 2;
    while out.len() < top {
        out.push(Line::default());
    }
    for parts in lines {
        if out.len() >= h {
            break;
        }
        let len: usize = parts.iter().map(|(t, _)| t.chars().count()).sum();
        let mut row = Row::new(w);
        row.pad_to(w.saturating_sub(len) / 2);
        for (t, s) in parts {
            row.push(t, s);
        }
        out.push(row.line());
    }
    out
}

// ---------------------------------------------------------------------------
// The map
// ---------------------------------------------------------------------------

/// The first floor cell to show and the blank columns before it, for a
/// floor `len` cells long in a pane `view` cells long, following `focus`.
/// A floor that fits is offset by `center` (half the spare cells, or none);
/// a larger one scrolls to keep `focus` in the middle, clamped to its ends.
fn window(len: usize, view: usize, focus: usize, center: bool) -> (usize, usize) {
    if len <= view {
        (0, if center { (view - len) / 2 } else { 0 })
    } else {
        (focus.saturating_sub(view / 2).min(len - view), 0)
    }
}

/// `rows` rows of the hero's floor, each at most `cols` wide.
fn map_rows(state: &SagaState, look: &Look, cols: usize, rows: usize, frame: u64) -> Vec<Row> {
    let mut out: Vec<Row> = (0..rows).map(|_| Row::new(cols)).collect();
    let Some(floor) = hero_floor(state) else {
        if let Some(row) = out.first_mut() {
            row.push(" this world has no floor to draw", look.dim);
        }
        return out;
    };
    let glyphs = &Pack::builtin().glyphs;
    let (fw, fh) = (floor.w as usize, floor.h as usize);
    let (hx, hy) = (state.hero.pos.0 as usize, state.hero.pos.1 as usize);
    let (x0, pad) = window(fw, cols, hx, true);
    let (y0, _) = window(fh, rows, hy, false);
    let hero = if state.hero.vitality < REST_BELOW {
        look.hero_weak
    } else {
        look.hero_well
    };
    for (r, row) in out.iter_mut().enumerate() {
        let y = y0 + r;
        if y >= fh {
            break;
        }
        row.pad_to(pad);
        for x in x0..fw.min(x0 + cols - pad) {
            let at = (x as u8, y as u8);
            let (glyph, style) = if at == state.hero.pos {
                (glyphs.hero, hero)
            } else if at == state.quest.goal {
                let g = if state.quest.verb == Verb::VisitSage {
                    glyphs.npc
                } else {
                    glyphs.cell(floor.cell(at.0, at.1))
                };
                (g, look.goal)
            } else if at == floor.exit {
                (glyphs.exit, look.exit)
            } else {
                let cell = floor.cell(at.0, at.1);
                (glyphs.cell(cell), cell_style(look, cell, x, frame))
            };
            let mut buf = [0u8; 4];
            row.push(glyph.encode_utf8(&mut buf), style);
        }
    }
    out
}

/// A cell's colour. Portals shimmer: each lights for one step in four, the
/// steps offset along the edge.
fn cell_style(look: &Look, cell: Cell, x: usize, frame: u64) -> Style {
    match cell {
        Cell::Wall => look.wall,
        Cell::Floor => look.floor,
        Cell::Ruin => look.ruin,
        Cell::Vault => look.vault,
        Cell::Gate => look.gate,
        Cell::Door => look.door,
        Cell::Portal => {
            if (frame / SHIMMER_FRAMES + x as u64 * 3) % 4 == 0 {
                look.portal_lit
            } else {
                look.portal_dim
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The chronicle
// ---------------------------------------------------------------------------

/// The chronicle pane: a header row, then the newest beats with the newest
/// at the bottom, every row `cols` wide at most and starting with the bar.
/// Exactly `rows` rows.
fn chronicle_rows(
    state: &SagaState,
    flags: SagaFlags,
    look: &Look,
    cols: usize,
    rows: usize,
) -> Vec<Row> {
    let mut out = Vec::with_capacity(rows);
    if rows == 0 {
        return out;
    }
    let mut head = Row::new(cols);
    head.push("╔═ ", look.bar).push("CHRONICLE", look.head);
    let hint = if flags.why {
        "  y hides why "
    } else {
        "  y: why "
    };
    head.push(hint, look.dim);
    let rule = "═".repeat(head.room());
    head.push(&rule, look.bar);
    out.push(head);

    // Text width after the bar.
    let text_w = cols.saturating_sub(BAR.chars().count());
    let want = rows - 1;
    // Newest first: each beat's rows, while the whole beat fits, so the
    // pane never starts with the tail of a cut-off beat. The newest beat is
    // always shown; when it alone is taller than the pane it loses its
    // first rows.
    let mut blocks: Vec<Vec<Row>> = Vec::new();
    let mut have = 0;
    for beat in newest_first(state, want) {
        let block = beat_rows(beat, flags, look, cols, text_w);
        if !blocks.is_empty() && have + block.len() > want {
            break;
        }
        have += block.len();
        blocks.push(block);
        if have >= want {
            break;
        }
    }
    let mut body: Vec<Row> = blocks.into_iter().rev().flatten().collect();
    let cut = body.len().saturating_sub(want);
    body.drain(..cut);
    if body.is_empty() && want > 0 {
        let mut row = Row::new(cols);
        row.push(BAR, look.bar)
            .push("the chronicle is empty", look.dim);
        body.push(row);
    }
    // Blank rows (with the bar) above, so the newest line sits at the bottom.
    while out.len() + body.len() < rows {
        let mut row = Row::new(cols);
        row.push(BAR.trim_end(), look.bar);
        out.push(row);
    }
    out.extend(body);
    out
}

/// The newest `n` beats, newest first.
fn newest_first(state: &SagaState, n: usize) -> impl Iterator<Item = &Beat> {
    let beats: Vec<&Beat> = state.chronicle.lines(n).collect();
    beats.into_iter().rev()
}

/// One beat's rows: its tick and text, wrapped under a hanging indent, then
/// its cause when "why" is on.
fn beat_rows(beat: &Beat, flags: SagaFlags, look: &Look, cols: usize, text_w: usize) -> Vec<Row> {
    let tick = format!("t{} ", beat.tick);
    let indent = tick.chars().count().min(text_w / 2);
    let mut out = Vec::new();
    let lines = wrap(
        &beat.text,
        text_w.saturating_sub(tick.chars().count()),
        text_w - indent,
    );
    for (i, text) in lines.iter().enumerate() {
        let mut row = Row::new(cols);
        row.push(BAR, look.bar);
        if i == 0 {
            row.push(&tick, look.dim);
        } else {
            row.pad_to(BAR.chars().count() + indent);
        }
        row.push(text, look.tone(beat));
        out.push(row);
    }
    if flags.why {
        let c = &beat.cause;
        let signal = if c.signal.is_empty() {
            "no signal"
        } else {
            c.signal.as_str()
        };
        let why = format!("why: {} · {} · #{}", c.entity.as_str(), signal, c.ordinal);
        // Under the beat's text, its continuation lines two columns in.
        let lines = wrap(&why, text_w - indent, text_w.saturating_sub(indent + 2));
        for (i, text) in lines.iter().enumerate() {
            let mut row = Row::new(cols);
            row.push(BAR, look.bar)
                .pad_to(BAR.chars().count() + indent + if i == 0 { 0 } else { 2 });
            row.push(text, look.why);
            out.push(row);
        }
    }
    out
}

/// `text` in lines of at most `first` columns for the first and `rest` for
/// the others, broken at spaces where it can and inside a word that is
/// longer than a line. Always at least one line.
fn wrap(text: &str, first: usize, rest: usize) -> Vec<String> {
    let width = |s: &str| -> usize {
        s.chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    };
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let limit = |n: usize| if n == 0 { first } else { rest }.max(1);
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        let sep = usize::from(!line.is_empty());
        if width(&line) + sep + width(word) <= limit(out.len()) {
            if sep == 1 {
                line.push(' ');
            }
            line.push_str(word);
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        // A word longer than a line is cut where the line ends.
        for ch in word.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width(&line) + cw > limit(out.len()) && !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            line.push(ch);
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

// ---------------------------------------------------------------------------
// The status row
// ---------------------------------------------------------------------------

/// `  Wren the Unthrottled  ██████░░ 74%  · visiting the sage demo-sage`.
fn status_row(state: &SagaState, look: &Look, w: usize) -> Row {
    let p = &Pack::builtin().palette;
    let hero = &state.hero;
    let v = if hero.vitality.is_finite() {
        hero.vitality.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = ((v * VITALITY_CELLS as f32).round() as usize).min(VITALITY_CELLS);
    let bar_style = if v >= 0.75 {
        fg(p.green)
    } else if v >= REST_BELOW {
        fg(p.yellow)
    } else {
        fg(p.orange)
    };
    let mut row = Row::new(w);
    row.push(" ", Style::default())
        .push(&hero.name, look.name)
        .push("  ", Style::default())
        .push(&"█".repeat(filled), bar_style)
        .push(&"░".repeat(VITALITY_CELLS - filled), look.dim)
        .push(&format!(" {:>3.0}%", v * 100.0), bar_style)
        .push("  · ", look.dim)
        .push(&quest_text(state), look.quest);
    row
}

/// The quest in words: `exploring`, `resting`, `visiting the sage NAME`,
/// with `(done)` once the hero stands on the goal. See "Names" in the
/// module comment for where NAME comes from.
fn quest_text(state: &SagaState) -> String {
    let q = &state.quest;
    let target = quest_target(state);
    let named = |what: &str, fallback: &str| match &target {
        Some(t) => format!("{what} {t}"),
        None => fallback.to_string(),
    };
    let mut text = match q.verb {
        Verb::Explore => "exploring".to_string(),
        Verb::Rest => "resting".to_string(),
        Verb::VisitSage => named("visiting the sage", "visiting a sage"),
        Verb::Slay => named("hunting", "hunting a monster"),
        Verb::Escort => named("escorting", "escorting a traveller"),
        Verb::Cleanse => named("cleansing", "cleansing a vault"),
        Verb::Rebuild => named("rebuilding", "rebuilding a bridge"),
    };
    if q.verb != Verb::Rest && state.hero.pos == q.goal {
        text.push_str(" (done)");
    }
    text
}

/// The name to show for the quest's target, or `None` to show none.
///
/// * A chip-owned key (a chip, tile, DRAM channel, Ethernet port) is a
///   hardware identifier that the core never aliases: its id.
/// * A target that is the quest's reason's entity (a sage's model): the id
///   of the cause of the newest beat with the reason's signal and ordinal.
///   That beat came from the same record, and its cause is aliased in
///   generated mode.
/// * Anything else, or no such beat in the chronicle: `None`.
fn quest_target(state: &SagaState) -> Option<String> {
    let q = &state.quest;
    let target = q.target.as_ref()?;
    if target.chip_owner().is_some() {
        return Some(key_id(target.as_str()).to_string());
    }
    if *target != q.reason.entity {
        return None;
    }
    let beats: Vec<&Beat> = state.chronicle.lines(usize::MAX).collect();
    beats
        .into_iter()
        .rev()
        .find(|b| b.cause.ordinal == q.reason.ordinal && b.cause.signal == q.reason.signal)
        .map(|b| key_id(b.cause.entity.as_str()).to_string())
}

// ---------------------------------------------------------------------------
// The legend
// ---------------------------------------------------------------------------

/// The `l` legend: every map glyph from the pack, in the map's colours, then
/// the colours of the status bar and the chronicle.
pub fn legend_lines(bar: Color, bg: Color, dim: Color) -> Vec<Line<'static>> {
    let look = Look::new();
    let g = &Pack::builtin().glyphs;
    let dim = Style::default().fg(dim);
    let line = |parts: Vec<(String, Style)>| {
        let mut spans = vec![Span::styled(BAR, Style::default().fg(bar).bg(bg))];
        spans.extend(parts.into_iter().map(|(t, s)| Span::styled(t, s)));
        Line::from(spans)
    };
    let glyph =
        |c: char, s: Style, what: &str| line(vec![(c.to_string(), s), (format!("  {what}"), dim)]);
    let p = &Pack::builtin().palette;
    vec![
        line(vec![("THE MAP".to_string(), look.head)]),
        glyph(
            g.hero,
            look.hero_well,
            "the hero; orange when its vitality is below half",
        ),
        line(vec![(
            "   (vitality: its chip's heartbeat, clock and heat)".to_string(),
            dim,
        )]),
        glyph(g.wall, look.wall, "wall: solid rock"),
        glyph(g.floor, look.floor, "floor: rooms and corridors"),
        glyph(
            g.ruin,
            look.ruin,
            "ruin: fallen wall over a harvested row or column",
        ),
        glyph(
            g.vault,
            look.vault,
            "vault: a GDDR channel, on the west or east edge",
        ),
        glyph(
            g.gate,
            look.gate,
            "gate: the PCIe link on the south edge; heroes enter",
        ),
        glyph(
            g.portal,
            look.portal_lit,
            "portal: a live Ethernet link on the north edge",
        ),
        glyph(g.door, look.door, "door: where a corridor meets a room"),
        glyph(g.exit, look.exit, "exit: the room farthest from the gate"),
        glyph(
            g.npc,
            look.goal,
            "a sage: a ready model, when the hero visits it",
        ),
        line(vec![
            ("x".to_string(), look.goal),
            ("  the highlighted cell is the hero's goal".to_string(), dim),
        ]),
        line(vec![]),
        line(vec![("THE CHRONICLE".to_string(), look.head)]),
        line(vec![
            ("calm".to_string(), look.calm),
            ("  ".to_string(), dim),
            ("tense".to_string(), look.tense),
            ("  ".to_string(), dim),
            ("triumphant".to_string(), look.triumphant),
            ("  the mood of each line".to_string(), dim),
        ]),
        line(vec![(
            "t42  the tick the line was written at (one per sweep)".to_string(),
            dim,
        )]),
        line(vec![(
            "y    each line's cause: entity · signal · #record".to_string(),
            dim,
        )]),
        line(vec![]),
        line(vec![
            ("██".to_string(), fg(p.green)),
            ("██".to_string(), fg(p.yellow)),
            ("██".to_string(), fg(p.orange)),
            (
                "  the hero's vitality: well, tiring, resting".to_string(),
                dim,
            ),
        ]),
    ]
}

#[cfg(test)]
#[path = "saga_view_tests.rs"]
mod saga_view_tests;
