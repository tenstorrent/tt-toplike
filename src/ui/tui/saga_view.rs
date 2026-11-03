// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Saga view (`S`): a hero's quest through a dungeon that the machine's
//! own signals build, with a running chronicle of what happens (spec
//! sections 1, 5 and 8).
//!
//! The story is told by the pure core in the `tt_toplike_saga` crate, which
//! the saga driver (`crate::saga::SagaDriver`) runs on its own thread from
//! start-up. This module draws the driver's last published [`SagaState`]:
//! [`render_saga`] turns it into lines for one frame. The only thing the
//! view keeps between frames is [`SagaMemory`]: which cells of the hero's
//! floor have been seen, the hero's last few steps, and its chip's
//! temperature. The core never sees the memory, so nothing here can change
//! a digest.
//!
//! # What is drawn
//!
//! * **Title row.** A dark band: `▌ SAGA`, the label mark ("(mock)" or
//!   "(replay)", spec 3.9), the world's id and biome, and the hero's floor
//!   and its chip, fading out to the right with `▓▒░`.
//! * **Map.** The hero's floor, drawn as architecture (see "The map" below)
//!   with the glyphs from the pack's glyph set ([`Pack::glyphs`]) and colours
//!   mixed from the pack's palette ([`Pack::palette`], the Tenstorrent brand
//!   colours). A floor larger than the pane (floors are at most 120 x 60
//!   cells) scrolls to keep the hero in view; a smaller one is centred across
//!   the pane and drawn from the top.
//! * **Chronicle.** The newest whole beats, newest at the bottom. Each beat
//!   has its tick in a dim colour, a mark for its tone (`·` calm, `▲` tense,
//!   `★` triumphant), and its text in the tone's colour, with the names of
//!   heroes, places, sages and monsters in bold. Older beats fade a little.
//!   With "why" on (`y`, spec 5) each beat is followed by its cause: the
//!   entity, the signal and the record's position in the stream. Beats do
//!   not store their world, so the world id is on the title row only.
//! * **Status row.** The hero's name, its vitality as a bar of `▰` and `▱`,
//!   its quest, how much of the floor the view has mapped, and the tick.
//!
//! Before the first world (the core holds a placeholder world with id 0,
//! [`SagaState::has_world`]) the view says the chronicle has not begun. When
//! the driver has stopped it says so in a banner and still draws the last
//! state.
//!
//! # The map
//!
//! * **Walls are lines.** A wall cell that touches a walkable cell (eight
//!   neighbours) is a wall face. Each face joins the faces to its north,
//!   east, south and west when the two cells together border open space, and
//!   the joins pick a box-drawing glyph from the pack's tile tables
//!   ([`wall_mask`], [`Glyphs::wall_tile`]): thin lines inside the floor,
//!   double lines on the outer shell. A wall cell that touches nothing
//!   walkable is solid rock and is left blank, so rooms and corridors stand
//!   out.
//! * **Fog.** The hero carries a light: cells within an ellipse of
//!   [`LIGHT_RX`] by [`LIGHT_RY`] cells that the hero can see (no wall in
//!   between) are lit, brightest at the hero, and so is the whole room the
//!   hero stands in. Cells lit before are remembered: the same hue at a low
//!   value. Cells never lit are blank. The quest's goal is always drawn, so
//!   the hero's destination shows through the dark.
//! * **The hero** `@` is bold on yellow (orange while its vitality is below
//!   [`REST_BELOW`]), with a faint yellow glow on the cells around it and a
//!   fading trail of `•` over its last [`TRAIL_LEN`] steps.
//! * **The goal** pulses between `◈` and `◆` in yellow, or shows the sage
//!   `Ψ` in a pulsing teal when the quest is a visit.
//! * **Colour.** Each biome has its own floor and wall hues (a dungeon is
//!   teal and blue, a forge yellow and orange, a dream green and teal). The
//!   hero's chip's temperature, when the view knows it, warms the floor from
//!   the biome's hue towards yellow at 65 °C and orange at 85 °C.
//!
//! All colours go through `colors::rgb`, so a terminal without truecolor gets
//! the 256-colour cube, as every other view does. Every dim colour keeps one
//! channel high enough to land off black in that cube. On a 16-colour
//! terminal the shapes still tell rooms, walls and the hero apart.
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
//! keeps them. Making a name bold prints no new text.
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
//! The wall tiles include right-edge shapes (`┐`, `┤`, `╗` and others in
//! `crate::animation::arcade::RIGHT_EDGE_GLYPHS`), so the map never draws in
//! its pane's last column: that column is always blank, and a wall in the
//! floor's last column lands one column short of the edge.

use std::collections::{BTreeMap, VecDeque};
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tt_toplike_saga::chronicle::{Beat, Tone};
use tt_toplike_saga::hero::astar;
use tt_toplike_saga::maze::{Cell, Floor};
use tt_toplike_saga::pack::{Glyphs, Pack, Palette, Rgb, WallKind, ARM_E, ARM_N, ARM_S, ARM_W};
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
    &[("y", "why: what set each line off"), ("S Esc", "leave")],
    &[("/saga", "open from the command bar")],
];

/// The `!` text. Each claim is checked against the code: the floors in
/// `tt_toplike_saga::maze` and `topology`, worlds in `world`, determinism in the crate's
/// docs, causes in `chronicle`, names in `config.rs`, labels in spec 3.9,
/// the light and memory in this module.
pub const EXPLAIN_TEXT: &[&str] = &[
    "Saga",
    "",
    "A hero quests through a dungeon built from this box's",
    "own signals. Each chip is a floor. A Blackhole floor",
    "follows the chip's tile grid, with ruins over harvested",
    "columns, a vault per GDDR channel and a portal per live",
    "Ethernet link. Other chips get a default floor. Every",
    "floor has a gate for PCIe. The seed, taken from the low",
    "bits of the readings, places the rooms. A model coming",
    "up raises a world with a sage to visit, and a training",
    "run raises a forge that ends when the run stops. A",
    "reset, the model going away or a quiet season ends a",
    "world, and the next world gets a new maze.",
    "",
    "The hero's light shows what it can see. The map keeps",
    "what it has seen, dimmed, until the floor or world",
    "changes. A hot chip warms its floor's colour.",
    "",
    "The same recording gives the same story. Every line has",
    "a cause: press y to see its entity, its signal (or \"no",
    "signal\") and its place in the stream.",
    "",
    "For screen sharing, set names = \"generated\" under",
    "[saga]. (mock) in the title means --mock estimates;",
    "(replay) means a recorded log is playing.",
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
const VITALITY_CELLS: usize = 10;
/// Frames per step of the portals' shimmer.
const SHIMMER_FRAMES: u64 = 3;
/// Frames in one pulse of the goal and the sage.
const PULSE_FRAMES: u64 = 12;
/// The chronicle's left bar.
const BAR: &str = "║ ";
/// The hero's light: half-width and half-height of its ellipse, in cells.
/// A terminal cell is about twice as tall as it is wide, so the light is
/// twice as wide as it is tall and looks round.
pub const LIGHT_RX: i32 = 9;
pub const LIGHT_RY: i32 = 4;
/// Steps of the hero's trail.
pub const TRAIL_LEN: usize = 12;
/// The value of a lit cell at the edge of the light, in percent of a cell
/// at the hero. Cells lit as part of the hero's room but outside the
/// ellipse get this too.
const LIGHT_EDGE: u32 = 55;
/// The value of a remembered cell's colour, in percent of the way from the
/// base to its lit colour.
const REMEMBERED: u32 = 40;

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

    /// Appends one character in `style`.
    fn put(&mut self, ch: char, style: Style) -> &mut Row {
        let mut buf = [0u8; 4];
        self.push(ch.encode_utf8(&mut buf), style)
    }

    /// Pads with unstyled spaces to column `col` (or the cap).
    fn pad_to(&mut self, col: usize) -> &mut Row {
        self.fill_to(col, Style::default())
    }

    /// Pads with spaces in `style` to column `col` (or the cap).
    fn fill_to(&mut self, col: usize, style: Style) -> &mut Row {
        let n = col.min(self.cap).saturating_sub(self.used);
        self.push(&" ".repeat(n), style)
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
// What the view remembers
// ---------------------------------------------------------------------------

/// What the view keeps between frames: the cells of the hero's floor that
/// have been lit, the hero's last [`TRAIL_LEN`] steps, and the temperature
/// of the hero's chip. The TUI holds one, calls [`SagaMemory::observe`] on
/// every update whether or not Saga is on screen, and passes it to
/// [`render_saga`].
///
/// The memory belongs to one floor of one world. It starts empty when a new
/// world is born (a new id or seed) and when the hero moves to another
/// floor. A memory kept for a different floor draws as empty, so a stale
/// memory can never paint the wrong map.
#[derive(Debug, Clone, Default)]
pub struct SagaMemory {
    /// World id, world seed and floor index the memory belongs to.
    key: Option<(u64, u64, usize)>,
    w: usize,
    h: usize,
    /// Row-major, `w * h`: whether the cell has been lit.
    seen: Vec<bool>,
    /// The hero's newest positions, newest first. The first is where the
    /// hero stands now.
    trail: VecDeque<(u8, u8)>,
    /// The hero's position at the last observation.
    last: Option<(u8, u8)>,
    /// The hero's chip's temperature in °C, when known.
    heat: Option<f32>,
}

impl SagaMemory {
    pub fn new() -> SagaMemory {
        SagaMemory::default()
    }

    /// Takes in one published state: starts over on a new world or floor,
    /// then lights the cells around every cell the hero has walked since
    /// the last observation. When the hero moved more than one cell (the
    /// view was not observing, or the driver published less often than the
    /// hero stepped) the walk between is filled in with the core's own
    /// shortest path, the route the hero most likely took.
    pub fn observe(&mut self, state: &SagaState) {
        let heat = self.heat;
        let Some((key, floor)) = floor_key(state) else {
            *self = SagaMemory {
                heat,
                ..SagaMemory::default()
            };
            return;
        };
        let (w, h) = (floor.w as usize, floor.h as usize);
        if self.key != Some(key) || self.w != w || self.h != h {
            *self = SagaMemory {
                key: Some(key),
                w,
                h,
                seen: vec![false; w * h],
                heat,
                ..SagaMemory::default()
            };
        }
        let pos = state.hero.pos;
        if self.last == Some(pos) {
            return;
        }
        let steps: Vec<(u8, u8)> = match self.last {
            Some(from) if from.0.abs_diff(pos.0) + from.1.abs_diff(pos.1) > 1 => {
                astar(floor, from, pos)
                    .map(|p| p.into_iter().skip(1).collect())
                    .unwrap_or_else(|| vec![pos])
            }
            _ => vec![pos],
        };
        for step in steps {
            for (i, lit) in light(floor, step).into_iter().enumerate() {
                if lit > 0 {
                    self.seen[i] = true;
                }
            }
            self.trail.push_front(step);
        }
        self.trail.truncate(TRAIL_LEN + 1);
        self.last = Some(pos);
    }

    /// Sets the temperature of the hero's chip (`None` when not known). See
    /// [`hero_chip`] for which chip that is.
    pub fn set_heat(&mut self, celsius: Option<f32>) {
        self.heat = celsius.filter(|c| c.is_finite());
    }

    /// Makes this memory a copy of `other`, reusing this memory's buffers.
    /// The arcade's Saga panel keeps a copy of the TUI's memory and takes
    /// it once per update; after the first copy of a floor this allocates
    /// nothing (`Vec::clone_from` and `VecDeque::clone_from` keep their
    /// capacity).
    pub fn copy_from(&mut self, other: &SagaMemory) {
        self.key = other.key;
        self.w = other.w;
        self.h = other.h;
        self.seen.clone_from(&other.seen);
        self.trail.clone_from(&other.trail);
        self.last = other.last;
        self.heat = other.heat;
    }

    /// Marks every cell of the memory's floor seen, so a test can draw every
    /// wall a floor has.
    #[cfg(test)]
    fn see_everything(&mut self) {
        self.seen.iter_mut().for_each(|s| *s = true);
    }

    /// Whether this memory belongs to the floor `state` draws.
    fn fits(&self, state: &SagaState) -> bool {
        self.key.is_some() && floor_key(state).map(|(k, _)| k) == self.key
    }

    /// Whether the cell at `(x, y)` has been lit, for a memory that fits.
    fn seen(&self, x: usize, y: usize) -> bool {
        x < self.w && y < self.h && self.seen[y * self.w + x]
    }

    /// The age of the trail at `(x, y)`: 1 for the step before the hero's
    /// cell, up to [`TRAIL_LEN`]; `None` when the trail is not there.
    fn trail_age(&self, at: (u8, u8)) -> Option<usize> {
        self.trail
            .iter()
            .skip(1)
            .position(|&p| p == at)
            .map(|i| i + 1)
    }

    /// The share of the floor's walkable cells that have been lit, in
    /// percent, rounded down.
    fn mapped_pct(&self, floor: &Floor) -> usize {
        let mut walkable = 0usize;
        let mut seen = 0usize;
        for (i, c) in floor.cells.iter().enumerate() {
            if c.walkable() {
                walkable += 1;
                if self.seen.get(i).copied().unwrap_or(false) {
                    seen += 1;
                }
            }
        }
        (seen * 100).checked_div(walkable).unwrap_or(0)
    }
}

/// The key a memory is kept under for the floor `state` draws, and that
/// floor. `None` before the first world or when the world has no floor.
fn floor_key(state: &SagaState) -> Option<((u64, u64, usize), &Floor)> {
    if !state.has_world() {
        return None;
    }
    let i = hero_floor_index(state)?;
    let floor = state.world.floors.get(i)?;
    Some(((state.world.id, state.world.seed, i), floor))
}

/// The chip of the floor the hero is on. Its temperature tints the floor
/// ([`SagaMemory::set_heat`]).
pub fn hero_chip(state: &SagaState) -> Option<&tt_toplike_bus::EntityKey> {
    if !state.has_world() {
        return None;
    }
    hero_floor_index(state).and_then(|i| state.world.chips.get(i))
}

/// The light the hero at `from` casts on `floor`: for each cell, row-major,
/// 0 when it is dark or its value in percent (100 at the hero, falling to
/// [`LIGHT_EDGE`] at the edge of the ellipse). A cell is lit when it lies in
/// the ellipse and no wall stands on the straight line from the hero to it,
/// or when it lies in or on the edge of the room the hero stands in.
fn light(floor: &Floor, from: (u8, u8)) -> Vec<u8> {
    let (w, h) = (floor.w as i32, floor.h as i32);
    let mut out = vec![0u8; (w * h) as usize];
    let (hx, hy) = (from.0 as i32, from.1 as i32);
    if hx >= w || hy >= h {
        return out;
    }
    let (rx2, ry2) = (LIGHT_RX * LIGHT_RX, LIGHT_RY * LIGHT_RY);
    let edge = rx2 * ry2;
    for y in (hy - LIGHT_RY).max(0)..=(hy + LIGHT_RY).min(h - 1) {
        for x in (hx - LIGHT_RX).max(0)..=(hx + LIGHT_RX).min(w - 1) {
            let (dx, dy) = (x - hx, y - hy);
            let d = dx * dx * ry2 + dy * dy * rx2;
            if d > edge || !clear_line(floor, (hx, hy), (x, y)) {
                continue;
            }
            let fall = (100 - LIGHT_EDGE) as i32 * d / edge;
            out[(y * w + x) as usize] = (100 - fall) as u8;
        }
    }
    if let Some(room) = floor.rooms.iter().find(|r| r.contains(from.0, from.1)) {
        let (x0, y0) = (room.x as i32 - 1, room.y as i32 - 1);
        let (x1, y1) = (x0 + room.w as i32 + 1, y0 + room.h as i32 + 1);
        for y in y0.max(0)..=y1.min(h - 1) {
            for x in x0.max(0)..=x1.min(w - 1) {
                let i = (y * w + x) as usize;
                out[i] = out[i].max(LIGHT_EDGE as u8);
            }
        }
    }
    out
}

/// Whether no wall stands strictly between `a` and `b` on the straight
/// (Bresenham) line between them. The ends do not count, so a wall face is
/// lit when the hero can see it.
fn clear_line(floor: &Floor, a: (i32, i32), b: (i32, i32)) -> bool {
    let (dx, dy) = ((b.0 - a.0).abs(), -(b.1 - a.1).abs());
    let (sx, sy) = ((b.0 - a.0).signum(), (b.1 - a.1).signum());
    let (mut x, mut y, mut err) = (a.0, a.1, dx + dy);
    loop {
        if (x, y) == b {
            return true;
        }
        if (x, y) != a && !floor.cell(x as u8, y as u8).walkable() {
            return false;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

// ---------------------------------------------------------------------------
// Walls as architecture
// ---------------------------------------------------------------------------

/// Whether `(x, y)` is on the floor and walkable.
fn open(floor: &Floor, x: i32, y: i32) -> bool {
    x >= 0
        && y >= 0
        && x < floor.w as i32
        && y < floor.h as i32
        && floor.cell(x as u8, y as u8).walkable()
}

/// Whether `(x, y)` is on the floor's outer shell.
fn on_shell(floor: &Floor, x: i32, y: i32) -> bool {
    x == 0 || y == 0 || x == floor.w as i32 - 1 || y == floor.h as i32 - 1
}

/// Whether `(x, y)` is a wall face: a wall on the floor that is on the
/// shell or touches a walkable cell (eight neighbours). Any other wall is
/// solid rock.
fn face(floor: &Floor, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= floor.w as i32 || y >= floor.h as i32 {
        return false;
    }
    if floor.cell(x as u8, y as u8).walkable() {
        return false;
    }
    if on_shell(floor, x, y) {
        return true;
    }
    (-1..=1).any(|dy| (-1..=1).any(|dx| open(floor, x + dx, y + dy)))
}

/// Whether `(x, y)` is a cell set into a wall: a door, the gate, a portal
/// or a vault. A wall line runs up to it, so the door sits in the wall.
fn set_in_wall(floor: &Floor, x: i32, y: i32) -> bool {
    x >= 0
        && y >= 0
        && x < floor.w as i32
        && y < floor.h as i32
        && matches!(
            floor.cell(x as u8, y as u8),
            Cell::Door | Cell::Gate | Cell::Portal | Cell::Vault
        )
}

/// Whether the face `a` and its four-way neighbour `b` (a face, or a cell
/// [`set_in_wall`]) are joined by a line: both on the same side of the
/// shell, or together bordering open space (a walkable cell beside either
/// of them, across the line they make). Two faces of a wall two cells thick
/// each border their own side, so they get two parallel lines and no ladder
/// of rungs between them.
fn joined(floor: &Floor, a: (i32, i32), b: (i32, i32)) -> bool {
    if !face(floor, a.0, a.1) || !(face(floor, b.0, b.1) || set_in_wall(floor, b.0, b.1)) {
        return false;
    }
    let (last_x, last_y) = (floor.w as i32 - 1, floor.h as i32 - 1);
    if a.1 == b.1 {
        // East-west: the same shell row, or open space north or south.
        if a.1 == 0 || a.1 == last_y {
            return true;
        }
        [a.0, b.0]
            .iter()
            .any(|&x| open(floor, x, a.1 - 1) || open(floor, x, a.1 + 1))
    } else {
        // North-south: the same shell column, or open space west or east.
        if a.0 == 0 || a.0 == last_x {
            return true;
        }
        [a.1, b.1]
            .iter()
            .any(|&y| open(floor, a.0 - 1, y) || open(floor, a.0 + 1, y))
    }
}

/// The arm mask of the wall face at `(x, y)`: the `ARM_*` bit for each
/// neighbour it is [`joined`] to. 0 for a cell that is not a face.
pub fn wall_mask(floor: &Floor, x: i32, y: i32) -> u8 {
    if !face(floor, x, y) {
        return 0;
    }
    let mut m = 0;
    for (bit, dx, dy) in [(ARM_N, 0, -1), (ARM_E, 1, 0), (ARM_S, 0, 1), (ARM_W, -1, 0)] {
        if joined(floor, (x, y), (x + dx, y + dy)) {
            m |= bit;
        }
    }
    m
}

/// Which of the pack's tile tables the wall at `(x, y)` is drawn from.
fn wall_kind(floor: &Floor, x: i32, y: i32) -> WallKind {
    let edge_x = x == 0 || x == floor.w as i32 - 1;
    let edge_y = y == 0 || y == floor.h as i32 - 1;
    match (edge_x, edge_y) {
        (true, true) => WallKind::ShellCorner,
        (false, true) => WallKind::ShellRow,
        (true, false) => WallKind::ShellCol,
        (false, false) => WallKind::Inner,
    }
}

/// The glyph for the wall at `(x, y)`: a tile for a face, rock otherwise.
fn wall_glyph(glyphs: &Glyphs, floor: &Floor, x: i32, y: i32) -> char {
    if face(floor, x, y) {
        glyphs.wall_tile(wall_kind(floor, x, y), wall_mask(floor, x, y))
    } else {
        glyphs.rock
    }
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// `pct` percent of the way from `a` to `b`, in integers so a frame's
/// colours are the same on every machine.
fn mix(a: Rgb, b: Rgb, pct: u32) -> Rgb {
    let pct = pct.min(100);
    let m = |x: u8, y: u8| ((x as u32 * (100 - pct) + y as u32 * pct + 50) / 100) as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

fn rgb(c: Rgb) -> Color {
    colors::rgb(c[0], c[1], c[2])
}

fn fg(c: Rgb) -> Style {
    Style::default().fg(rgb(c))
}

/// 0 to 100 and back over [`PULSE_FRAMES`] frames: a triangle wave, so the
/// pulse needs no clock and a fixed frame draws a fixed picture.
fn pulse(frame: u64) -> u32 {
    let t = (frame % PULSE_FRAMES) as u32;
    let half = (PULSE_FRAMES / 2) as u32;
    if t <= half {
        t * 100 / half
    } else {
        (PULSE_FRAMES as u32 - t) * 100 / half
    }
}

/// The floor's warmth from its chip's temperature: the hue to shift towards
/// and how far, in percent. Below 45 °C nothing; teal to yellow from 45 to
/// 65 °C, yellow to orange from 65 to 85 °C, the shift growing to 70% at
/// 85 °C and staying there.
fn heat_shift(p: &Palette, celsius: Option<f32>) -> (Rgb, u32) {
    let Some(c) = celsius.filter(|c| c.is_finite()) else {
        return (p.teal, 0);
    };
    if c <= 45.0 {
        return (p.teal, 0);
    }
    let c = c.min(85.0);
    let hue = if c < 65.0 {
        mix(p.teal, p.yellow, ((c - 45.0) * 5.0) as u32)
    } else {
        mix(p.yellow, p.orange, ((c - 65.0) * 5.0) as u32)
    };
    (hue, ((c - 45.0) * 70.0 / 40.0) as u32)
}

/// The colours of one map role: its full colour when lit and whether it is
/// bold. The remembered colour and the falloff are mixed from these.
#[derive(Debug, Clone, Copy)]
struct Tint {
    lit: Rgb,
    bold: bool,
}

impl Tint {
    fn new(lit: Rgb) -> Tint {
        Tint { lit, bold: false }
    }

    fn bold(lit: Rgb) -> Tint {
        Tint { lit, bold: true }
    }

    /// The style at light value `light` (percent, 1 to 100), or remembered
    /// when `light` is 0.
    fn at(&self, base: Rgb, light: u32) -> Style {
        let dim = mix(base, self.lit, REMEMBERED);
        let style = if light == 0 {
            fg(dim)
        } else {
            fg(mix(dim, self.lit, light))
        };
        if self.bold && light > 0 {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }
}

/// Every colour the view uses, mixed from the pack's palette once a frame
/// for the world's biome and the hero's chip's temperature (through
/// `colors::rgb`, so the app's theme and colour depth apply).
struct Look {
    p: Palette,
    floor: Tint,
    wall: Tint,
    shell: Tint,
    ruin: Tint,
    vault: Tint,
    gate: Tint,
    portal_dim: Tint,
    portal_lit: Tint,
    door: Tint,
    exit: Tint,
    hero_well: Style,
    hero_weak: Style,
    /// The background of the cells around the hero.
    halo: Color,
    title: Style,
    title_bar: Style,
    band: Style,
    /// The `▓▒░` that ends the band, in the band's own colour.
    band_fade: Style,
    label: Style,
    banner: Style,
    dim: Style,
    bar: Style,
    head: Style,
    name: Style,
    quest: Style,
    why: Style,
}

impl Look {
    fn new(biome: Biome, heat: Option<f32>) -> Look {
        let p = Pack::builtin().palette;
        let bold = Modifier::BOLD;
        let (floor_hue, wall_hue, shell_hue) = match biome {
            Biome::Dungeon => (p.teal, mix(p.accent, p.teal, 35), p.accent),
            Biome::Forge => (
                mix(p.yellow, p.orange, 30),
                mix(p.orange, p.yellow, 25),
                p.orange,
            ),
            Biome::Dream => (p.green, mix(p.teal, p.green, 40), p.teal),
        };
        let (warm, by) = heat_shift(&p, heat);
        let floor_hue = mix(floor_hue, warm, by);
        // The title band: the base lifted towards the accent, so the band
        // shows on a black terminal and lands on a dark teal (not black) in
        // the 256-colour cube.
        let band_bg = mix(p.base, p.accent, 30);
        Look {
            p,
            floor: Tint::new(mix(p.base, floor_hue, 80)),
            wall: Tint::new(wall_hue),
            shell: Tint::bold(shell_hue),
            ruin: Tint::new(mix(p.base, p.yellow, 70)),
            vault: Tint::bold(p.yellow),
            gate: Tint::bold(p.teal),
            portal_dim: Tint::new(mix(p.accent, p.teal, 40)),
            portal_lit: Tint::bold(p.teal),
            door: Tint::bold(mix(p.yellow, p.orange, 55)),
            exit: Tint::bold(p.green),
            hero_well: fg(p.base).bg(rgb(p.yellow)).add_modifier(bold),
            hero_weak: fg(p.base).bg(rgb(p.orange)).add_modifier(bold),
            halo: rgb(mix(p.base, p.yellow, 22)),
            title: fg(p.teal).bg(rgb(band_bg)).add_modifier(bold),
            title_bar: fg(p.yellow).bg(rgb(band_bg)),
            band: fg(p.green).bg(rgb(band_bg)),
            band_fade: fg(band_bg),
            label: fg(p.yellow).bg(rgb(band_bg)).add_modifier(bold),
            banner: fg(p.base).bg(rgb(p.orange)).add_modifier(bold),
            dim: fg(mix(p.base, p.green, 60)),
            bar: fg(mix(p.base, p.accent, 70)),
            head: fg(p.teal).add_modifier(bold),
            name: fg(p.yellow).add_modifier(bold),
            quest: fg(p.green),
            why: fg(mix(p.base, p.green, 60)).add_modifier(Modifier::ITALIC),
        }
    }

    /// The look for the state's world (the dungeon look before one).
    fn for_state(state: &SagaState, memory: &SagaMemory) -> Look {
        let heat = if memory.fits(state) {
            memory.heat
        } else {
            None
        };
        Look::new(state.world.biome, heat)
    }

    /// A beat's colour, `age` beats from the newest: older beats fade a
    /// little towards the base, at most a third of the way.
    fn tone(&self, tone: Tone, age: usize) -> Rgb {
        mix(self.p.tone(tone), self.p.base, (age as u32 * 4).min(33))
    }

    /// The tint of a cell that is not the hero, the goal or the exit.
    fn cell(&self, cell: Cell, x: usize, frame: u64) -> Tint {
        match cell {
            Cell::Wall => self.wall,
            Cell::Floor => self.floor,
            Cell::Ruin => self.ruin,
            Cell::Vault => self.vault,
            Cell::Gate => self.gate,
            Cell::Door => self.door,
            // Portals shimmer: each lights for one step in four, the steps
            // offset along the edge.
            Cell::Portal => {
                if (frame / SHIMMER_FRAMES + x as u64 * 3) % 4 == 0 {
                    self.portal_lit
                } else {
                    self.portal_dim
                }
            }
        }
    }
}

/// The style of the quest's goal beacon at `frame`: bold yellow on a dark
/// yellow background that brightens and fades with [`pulse`]. Public for
/// the tests, which look for it on the map.
pub fn goal_style(frame: u64) -> Style {
    let p = &Pack::builtin().palette;
    fg(p.yellow)
        .bg(rgb(mix(p.base, p.yellow, 18 + pulse(frame) * 22 / 100)))
        .add_modifier(Modifier::BOLD)
}

/// The style of a sage at the goal at `frame`: bold, pulsing from teal to
/// green, on the goal's background.
pub fn sage_style(frame: u64) -> Style {
    let p = &Pack::builtin().palette;
    fg(mix(p.teal, p.green, pulse(frame)))
        .bg(rgb(mix(p.base, p.yellow, 18 + pulse(frame) * 22 / 100)))
        .add_modifier(Modifier::BOLD)
}

/// The goal beacon's glyph at `frame`: the two glyphs swap at each half
/// pulse.
fn goal_glyph(glyphs: &Glyphs, frame: u64) -> char {
    if (frame / (PULSE_FRAMES / 2)) % 2 == 0 {
        glyphs.goal
    } else {
        glyphs.goal_pulse
    }
}

// ---------------------------------------------------------------------------
// The frame
// ---------------------------------------------------------------------------

/// One frame of the Saga view for an area of `area` = (columns, rows): at
/// most `rows` lines, none wider than `columns`. `memory` holds the lit
/// cells, the trail and the heat (see [`SagaMemory`]); a memory that does
/// not fit the state counts as empty. `frame` drives the small animations
/// (the pulse, the portals' shimmer, the waiting dots), so a fixed frame
/// draws a fixed picture. See the module comment for the layout.
pub fn render_saga(
    state: &SagaState,
    memory: &SagaMemory,
    flags: SagaFlags,
    area: (u16, u16),
    frame: u64,
) -> Vec<Line<'static>> {
    let (w, h) = (area.0 as usize, area.1 as usize);
    let layout = layout_for(w, h);
    if layout == Layout::Nothing {
        return Vec::new();
    }
    let look = Look::for_state(state, memory);
    if !state.has_world() {
        return waiting(state, flags, &look, layout, w, h, frame);
    }
    // At the two small sizes the stopped mark goes on the title row; the
    // larger ones give it a row of its own.
    let small = matches!(layout, Layout::TitleOnly | Layout::MapOnly);
    let mut out = vec![title_row(state, &look, w, flags.stopped && small).line()];
    let map = |cols, rows| map_rows(state, memory, &look, cols, rows, frame);
    match layout {
        Layout::Nothing | Layout::TitleOnly => {}
        Layout::MapOnly => {
            out.extend(map(w, h - 1).into_iter().map(Row::line));
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
            out.extend(map(w, map_h).into_iter().map(Row::line));
            out.extend(
                chronicle_rows(state, flags, &look, w, body - map_h, true)
                    .into_iter()
                    .map(Row::line),
            );
            out.push(status_row(state, memory, &look, w).line());
        }
        Layout::Side => {
            if flags.stopped {
                out.push(banner_row(&look, w).line());
            }
            let body = h - out.len() - 1;
            // The map pane is the floor's width, a column on its left and
            // the blank last column (see "Borders"), up to three fifths of
            // the width; the chronicle has the rest, after a one-column gap.
            let floor_w = hero_floor(state).map_or(1, |f| f.w as usize);
            let map_w = (floor_w + 3).min(w * 3 / 5);
            let chron_w = w - map_w - 1;
            let maps = map(map_w, body);
            let chron = chronicle_rows(state, flags, &look, chron_w, body, true);
            out.extend(maps.into_iter().zip(chron).map(|(m, c)| m.join(1, c)));
            out.push(status_row(state, memory, &look, w).line());
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
    let look = Look::new(Biome::Dungeon, None);
    let mut title = Row::new(w);
    title
        .push("▌", look.title_bar)
        .push(" SAGA", look.title)
        .push(" · off ", look.band);
    let mut out = vec![band_end(title, &look, w).line()];
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

/// The index of the hero's floor in the world's floors: the hero's own
/// index, or 0 (the first floor) when that index is past the end, since the
/// core plans on the first floor then too. `None` when the world has no
/// floors. The title and the map both use it, so they always name the same
/// floor.
fn hero_floor_index(state: &SagaState) -> Option<usize> {
    let n = state.world.floors.len();
    let i = state.hero.floor as usize;
    match n {
        0 => None,
        _ if i < n => Some(i),
        _ => Some(0),
    }
}

/// The hero's floor (see [`hero_floor_index`]).
fn hero_floor(state: &SagaState) -> Option<&Floor> {
    hero_floor_index(state).and_then(|i| state.world.floors.get(i))
}

/// Ends a title-band row: fills it with the band's background to three
/// columns short of `w`, then fades out with `▓▒░` in the band's colour.
/// A row with no room for the fade is filled to its end.
fn band_end(mut row: Row, look: &Look, w: usize) -> Row {
    const FADE: &str = "▓▒░";
    let fade = FADE.chars().count();
    if row.room() > fade {
        row.fill_to(w - fade, look.band);
        row.push(FADE, look.band_fade);
    } else {
        row.fill_to(w, look.band);
    }
    row
}

/// `▌ SAGA (mock) · world 3 · dungeon · floor 1 of 2 · chip 0000:01:00.0`
/// on the title band, cut to `w`. The label comes right after the name so
/// a narrow title keeps it, then the stopped mark when `stopped_mark` is
/// set.
fn title_row(state: &SagaState, look: &Look, w: usize, stopped_mark: bool) -> Row {
    let mut row = Row::new(w);
    row.push("▌", look.title_bar).push(" SAGA", look.title);
    if let Some(mark) = state.label.mark() {
        row.push(" ", look.band).push(mark, look.label);
    }
    if stopped_mark {
        row.push(" stopped", look.banner);
    }
    if state.has_world() {
        let world = &state.world;
        let mut info = format!(" · world {} · {}", world.id, biome_word(world.biome));
        let n = world.floors.len();
        if let Some(i) = hero_floor_index(state) {
            info.push_str(&format!(" · floor {} of {n}", i + 1));
            if let Some(chip) = world.chips.get(i) {
                info.push_str(&format!(" · chip {}", key_id(chip.as_str())));
            }
        }
        info.push(' ');
        row.push(&info, look.band);
    } else {
        row.push(" · waiting ", look.band);
    }
    band_end(row, look, w)
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
            let s = if i == lit { look.head } else { look.dim };
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

/// `rows` rows of the hero's floor, each at most `cols` wide, the last
/// column always blank (see "Borders" in the module comment).
fn map_rows(
    state: &SagaState,
    memory: &SagaMemory,
    look: &Look,
    cols: usize,
    rows: usize,
    frame: u64,
) -> Vec<Row> {
    let mut out: Vec<Row> = (0..rows).map(|_| Row::new(cols)).collect();
    let Some(floor) = hero_floor(state) else {
        if let Some(row) = out.first_mut() {
            row.push(" this world has no floor to draw", look.dim);
        }
        return out;
    };
    let glyphs = &Pack::builtin().glyphs;
    let base = look.p.base;
    let (fw, fh) = (floor.w as usize, floor.h as usize);
    let (hx, hy) = (state.hero.pos.0 as usize, state.hero.pos.1 as usize);
    let draw_cols = cols.saturating_sub(1);
    let (x0, pad) = window(fw, draw_cols, hx, true);
    let (y0, _) = window(fh, rows, hy, false);
    let hero = if state.hero.vitality < REST_BELOW {
        look.hero_weak
    } else {
        look.hero_well
    };
    let lit = light(floor, state.hero.pos);
    let fits = memory.fits(state);
    let sage_quest = state.quest.verb == Verb::VisitSage;
    for (r, row) in out.iter_mut().enumerate() {
        let y = y0 + r;
        if y >= fh {
            break;
        }
        row.pad_to(pad);
        for x in x0..fw.min(x0 + draw_cols - pad) {
            let at = (x as u8, y as u8);
            let light = lit[y * fw + x] as u32;
            let seen = light > 0 || (fits && memory.seen(x, y));
            let near_hero = x.abs_diff(hx) <= 1 && y.abs_diff(hy) <= 1;
            let (glyph, style) = if at == state.hero.pos {
                (glyphs.hero, hero)
            } else if at == state.quest.goal {
                // The goal shows through the dark.
                if sage_quest {
                    (glyphs.npc, sage_style(frame))
                } else {
                    (goal_glyph(glyphs, frame), goal_style(frame))
                }
            } else if !seen {
                (' ', Style::default())
            } else {
                let cell = floor.cell(at.0, at.1);
                let (glyph, tint) = if at == floor.exit {
                    (glyphs.exit, look.exit)
                } else if cell == Cell::Wall {
                    let tint = if on_shell(floor, x as i32, y as i32) {
                        look.shell
                    } else {
                        look.wall
                    };
                    (wall_glyph(glyphs, floor, x as i32, y as i32), tint)
                } else {
                    (glyphs.cell(cell), look.cell(cell, x, frame))
                };
                let mut style = tint.at(base, light);
                // The trail: footprints on plain floor, fading with age.
                if cell == Cell::Floor && at != floor.exit {
                    if let Some(age) = fits.then(|| memory.trail_age(at)).flatten() {
                        let left = (TRAIL_LEN + 1 - age) as u32;
                        let warm = mix(base, look.p.yellow, 35 + left * 50 / TRAIL_LEN as u32);
                        style = fg(warm);
                        if age <= TRAIL_LEN / 2 {
                            row.put(glyphs.trail, halo(style, near_hero, cell, look));
                            continue;
                        }
                    }
                }
                (glyph, halo(style, near_hero, cell, look))
            };
            row.put(glyph, style);
        }
    }
    out
}

/// `style` with the hero's glow behind it, for a walkable cell next to the
/// hero.
fn halo(style: Style, near_hero: bool, cell: Cell, look: &Look) -> Style {
    if near_hero && cell.walkable() {
        style.bg(look.halo)
    } else {
        style
    }
}

// ---------------------------------------------------------------------------
// The chronicle
// ---------------------------------------------------------------------------

/// The chronicle pane: a header row, then the newest beats with the newest
/// at the bottom, every row `cols` wide at most and starting with the bar.
/// Exactly `rows` rows. `hint` puts the `y` key's hint on the header; the
/// arcade panel, where `y` does nothing, leaves it off.
fn chronicle_rows(
    state: &SagaState,
    flags: SagaFlags,
    look: &Look,
    cols: usize,
    rows: usize,
    hint: bool,
) -> Vec<Row> {
    let mut out = Vec::with_capacity(rows);
    if rows == 0 {
        return out;
    }
    let mut head = Row::new(cols);
    head.push("╔═ ", look.bar).push("CHRONICLE", look.head);
    if hint {
        let hint = if flags.why {
            "  y hides why "
        } else {
            "  y: why "
        };
        head.push(hint, look.dim);
    } else {
        head.push(" ", Style::default());
    }
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
    for (age, beat) in newest_first(state, want).enumerate() {
        let block = beat_rows(beat, &state.hero.name, age, flags, look, cols, text_w);
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

/// A tone's mark, between a beat's tick and its text.
fn tone_mark(tone: Tone) -> &'static str {
    match tone {
        Tone::Calm => "·",
        Tone::Tense => "▲",
        Tone::Triumphant => "★",
    }
}

/// One beat's rows: its tick, its tone's mark and its text, wrapped under a
/// hanging indent, then its cause when "why" is on. `age` is 0 for the
/// newest beat. Names (see [`emphasis`]) are bold and at full colour.
fn beat_rows(
    beat: &Beat,
    hero: &str,
    age: usize,
    flags: SagaFlags,
    look: &Look,
    cols: usize,
    text_w: usize,
) -> Vec<Row> {
    let tick = format!("t{} ", beat.tick);
    let mark = tone_mark(beat.tone);
    let lead = tick.chars().count() + mark.chars().count() + 1;
    let indent = lead.min(text_w / 2);
    let colour = look.tone(beat.tone, age);
    let plain = fg(colour);
    let strong = fg(look.p.tone(beat.tone)).add_modifier(Modifier::BOLD);
    let emph = emphasis(&beat.text, hero);
    let mut out = Vec::new();
    let lines = wrap_words(&beat.text, text_w.saturating_sub(lead), text_w - indent);
    for (i, pieces) in lines.iter().enumerate() {
        let mut row = Row::new(cols);
        row.push(BAR, look.bar);
        if i == 0 {
            row.push(&tick, look.dim)
                .push(mark, fg(look.p.tone(beat.tone)))
                .push(" ", Style::default());
        } else {
            row.pad_to(BAR.chars().count() + indent);
        }
        for (j, (word, piece)) in pieces.iter().enumerate() {
            if j > 0 {
                // A space inside a name is part of the name.
                let inside = emph.get(*word).copied().unwrap_or(false)
                    && pieces[j - 1].0 + 1 == *word
                    && emph.get(*word - 1).copied().unwrap_or(false);
                row.push(" ", if inside { strong } else { plain });
            }
            let style = if emph.get(*word).copied().unwrap_or(false) {
                strong
            } else {
                plain
            };
            row.push(piece, style);
        }
        out.push(row);
    }
    if flags.why {
        let why = super::saga_why::why_line(&beat.cause);
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

/// The names the chronicle draws in bold, from the pack's lexicon: every
/// phrase under a `place:`, `npc:`, `monster:`, `item:`, `hero:` or `title:`
/// tag, by its first word; the adjectives that can come before a composed
/// name; and the lore words that can end one ("of the KV Cache").
struct Names {
    by_first: BTreeMap<&'static str, Vec<Vec<&'static str>>>,
    adjectives: Vec<&'static str>,
    lore: Vec<Vec<&'static str>>,
}

fn names() -> &'static Names {
    static NAMES: OnceLock<Names> = OnceLock::new();
    NAMES.get_or_init(|| {
        let lex = &Pack::builtin().lexicon;
        let mut by_first: BTreeMap<&'static str, Vec<Vec<&'static str>>> = BTreeMap::new();
        let mut adjectives = Vec::new();
        for tag in lex.tags() {
            let named = ["place:", "npc:", "monster:", "item:", "hero:", "title:"]
                .iter()
                .any(|p| tag.starts_with(p));
            for &word in lex.words(tag) {
                if named {
                    let parts: Vec<&'static str> = word.split(' ').collect();
                    by_first.entry(parts[0]).or_default().push(parts);
                } else if tag.starts_with("adjective:") {
                    adjectives.push(word);
                }
            }
        }
        // Longest first, so "Tensix Grid" wins over "Tensix".
        for list in by_first.values_mut() {
            list.sort_by_key(|p| std::cmp::Reverse(p.len()));
        }
        let mut lore: Vec<Vec<&'static str>> = lex
            .words(tt_toplike_saga::pack::LORE)
            .iter()
            .map(|w| w.split(' ').collect())
            .collect();
        lore.sort_by_key(|p| std::cmp::Reverse(p.len()));
        Names {
            by_first,
            adjectives,
            lore,
        }
    })
}

/// A word without the punctuation around it, for matching names.
fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
}

/// Whether `phrase` matches the words from `at`, punctuation aside.
fn matches_at(words: &[&str], at: usize, phrase: &[&str]) -> bool {
    at + phrase.len() <= words.len() && phrase.iter().zip(&words[at..]).all(|(p, w)| bare(w) == *p)
}

/// For each word of `text` (split at spaces, empty words dropped, as
/// [`wrap_words`] numbers them), whether it is part of a name: the hero's
/// own name `hero`, or a lexicon name with the adjective before it and the
/// "of the <lore>" after it. The text is the beat's own, so in generated
/// mode it already carries aliases and this prints nothing new.
fn emphasis(text: &str, hero: &str) -> Vec<bool> {
    let words: Vec<&str> = text.split(' ').filter(|w| !w.is_empty()).collect();
    let hero: Vec<&str> = hero.split(' ').filter(|w| !w.is_empty()).collect();
    let n = names();
    let mut out = vec![false; words.len()];
    let mut i = 0;
    while i < words.len() {
        let len = if !hero.is_empty() && matches_at(&words, i, &hero) {
            hero.len()
        } else {
            n.by_first
                .get(bare(words[i]))
                .and_then(|list| list.iter().find(|p| matches_at(&words, i, p)))
                .map_or(0, Vec::len)
        };
        if len == 0 {
            i += 1;
            continue;
        }
        let (mut start, mut end) = (i, i + len);
        if start > 0 && n.adjectives.contains(&bare(words[start - 1])) {
            start -= 1;
        }
        if end + 2 < words.len() && words[end] == "of" && words[end + 1] == "the" {
            if let Some(l) = n.lore.iter().find(|l| matches_at(&words, end + 2, l)) {
                end += 2 + l.len();
            }
        }
        out[start..end].iter_mut().for_each(|e| *e = true);
        i = end;
    }
    out
}

/// `text` in lines of at most `first` columns for the first and `rest` for
/// the others, broken at spaces where it can and inside a word that is
/// longer than a line. Always at least one line.
fn wrap(text: &str, first: usize, rest: usize) -> Vec<String> {
    wrap_words(text, first, rest)
        .into_iter()
        .map(|pieces| {
            pieces
                .into_iter()
                .map(|(_, p)| p)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// [`wrap`], keeping each line as its pieces: `(word index, text)`, where a
/// word longer than a line is split into several pieces with one index.
/// Pieces on one line are joined by one space.
fn wrap_words(text: &str, first: usize, rest: usize) -> Vec<Vec<(usize, String)>> {
    let width = |s: &str| -> usize {
        s.chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    };
    let mut out: Vec<Vec<(usize, String)>> = Vec::new();
    let mut line: Vec<(usize, String)> = Vec::new();
    let mut used = 0usize;
    let limit = |n: usize| if n == 0 { first } else { rest }.max(1);
    for (idx, word) in text.split(' ').filter(|w| !w.is_empty()).enumerate() {
        let sep = usize::from(!line.is_empty());
        if used + sep + width(word) <= limit(out.len()) {
            used += sep + width(word);
            line.push((idx, word.to_string()));
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
            used = 0;
        }
        // A word longer than a line is cut where the line ends.
        let mut piece = String::new();
        for ch in word.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > limit(out.len()) && !piece.is_empty() {
                line.push((idx, std::mem::take(&mut piece)));
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            piece.push(ch);
            used += cw;
        }
        line.push((idx, piece));
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

// ---------------------------------------------------------------------------
// The status row
// ---------------------------------------------------------------------------

/// `  Wren the Unthrottled  ▰▰▰▰▰▰▰▱▱▱  74%  • visiting the sage demo-sage
/// • mapped 34%  • t208`.
fn status_row(state: &SagaState, memory: &SagaMemory, look: &Look, w: usize) -> Row {
    status_after(Row::new(w), state, memory, look)
}

/// The status row's segments (hero, vitality, quest, mapped share, tick)
/// pushed after what `row` already holds. The full view starts from an
/// empty row ([`status_row`]); the arcade panel's HUD starts with the
/// "(mock)" or "(replay)" mark ([`hud_row`]).
fn status_after(mut row: Row, state: &SagaState, memory: &SagaMemory, look: &Look) -> Row {
    let p = &look.p;
    let hero = &state.hero;
    let (v, bar_style) = vitality(state, look);
    let filled = ((v * VITALITY_CELLS as f32).round() as usize).min(VITALITY_CELLS);
    let sep = separator(look);
    row.push(" ", Style::default())
        .push(&hero.name, look.name)
        .push("  ", Style::default())
        .push(&"▰".repeat(filled), bar_style)
        .push(&"▱".repeat(VITALITY_CELLS - filled), look.dim)
        .push(
            &format!(" {:>3.0}%", v * 100.0),
            bar_style.add_modifier(Modifier::BOLD),
        )
        .push("  • ", sep)
        .push(&quest_text(state), look.quest);
    // The two last segments are dropped whole when they do not fit, so a
    // narrow row never ends in half a word.
    if let Some(floor) = hero_floor(state).filter(|_| memory.fits(state)) {
        let pct = format!("{}%", memory.mapped_pct(floor));
        // `  • mapped ` is 11 columns, `  • ` four.
        if row.room() >= 11 + pct.len() {
            row.push("  • ", sep)
                .push("mapped ", look.dim)
                .push(&pct, fg(p.teal));
        }
    }
    let tick = format!("t{}", state.tick);
    if row.room() >= 4 + tick.len() {
        row.push("  • ", sep).push(&tick, look.dim);
    }
    row
}

/// The hero's vitality, clamped to 0..=1 (0 when it is not a number), and
/// the bar's style for it: green from 75%, yellow from [`REST_BELOW`],
/// orange under it.
fn vitality(state: &SagaState, look: &Look) -> (f32, Style) {
    let p = &look.p;
    let raw = state.hero.vitality;
    let v = if raw.is_finite() {
        raw.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let style = if v >= 0.75 {
        fg(p.green)
    } else if v >= REST_BELOW {
        fg(p.yellow)
    } else {
        fg(p.orange)
    };
    (v, style)
}

/// The `•` between the status row's segments.
fn separator(look: &Look) -> Style {
    fg(mix(look.p.base, look.p.accent, 80))
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
// The arcade panel
// ---------------------------------------------------------------------------

/// The smallest Saga panel in an arcade layout (columns, rows): the HUD
/// row over five map rows. It is the full view's own threshold for drawing
/// the map ([`MAP_MIN`]). `arcade_layout::View::Saga` reads it, so a
/// smaller panel is taken out of its row and named on the rule
/// (`◌ SAGA needs 20x6`).
pub const PANEL_MIN: (usize, usize) = MAP_MIN;
/// From this size (columns, rows) the panel draws a chronicle strip under
/// the map. 70x16 has one; the Saga panel of `now-serving` at 80x24
/// (39x11) does not.
pub const PANEL_CHRONICLE_MIN: (usize, usize) = (60, 12);

/// What the arcade's Saga panel draws, set by the TUI once per update
/// (`ArcadeVisualization::set_saga`).
#[derive(Debug, Clone)]
pub enum PanelFeed {
    /// Nothing has been fed yet: the arcade was just made.
    Waiting,
    /// There is no driver: `[saga] enabled = false` (`enabled` false), or
    /// a driver that could not start.
    Off { enabled: bool },
    /// The driver's last published state (an `Arc`, so a feed copies no
    /// state) and whether the driver has stopped.
    Live {
        state: std::sync::Arc<SagaState>,
        stopped: bool,
    },
}

/// The note the panel draws before its first world.
pub const PANEL_WAITING: &str = "saga waiting for the first world";
/// The note for `[saga] enabled = false`.
pub const PANEL_OFF: &str = "saga is off: set [saga] enabled = true in config.toml";
/// The note for a driver that is on in the config but did not start.
pub const PANEL_NOT_RUNNING: &str = "saga is not running; the log says why";
/// The note for a driver that stopped before the first world.
pub const PANEL_STOPPED_EARLY: &str = "the story stopped before its first world";

/// The Saga view as an arcade panel of `area` = (columns, rows): at most
/// `rows` lines, none wider than `columns`, and the last column always
/// blank on a map row (the map's own rule, see "Borders").
///
/// * Before the first world, with no driver, or with a driver that stopped
///   before one: a note, one column in like the arcade's other notes. The
///   off note wraps to the panel's width so the way to turn Saga on is
///   never cut.
/// * With a world: the HUD row (the "(mock)" or "(replay)" mark, the
///   hero's name, the vitality bar, the quest, the mapped share and the
///   tick: the full view's status row, [`status_after`]), the stopped
///   banner when the driver has stopped, then the hero's floor from
///   [`map_rows`], the same renderer the full view uses, so the fog, the
///   glow, the trail and the colours are the view's own and a floor larger
///   than the panel scrolls to keep the hero in the middle.
/// * From [`PANEL_CHRONICLE_MIN`]: a chronicle strip from
///   [`chronicle_rows`], without the `y` hint (the key does nothing in the
///   arcade). Under [`SIDE_MIN_COLS`] it is a third of the rows under the
///   map; from there it sits right of the map, as in the full view.
///
/// Names come from the state's beats and causes, so generated mode aliases
/// the panel exactly as it does the full view. `frame` drives the pulse and
/// the shimmer; the arcade passes its own frame count.
pub fn render_saga_panel(
    feed: &PanelFeed,
    memory: &SagaMemory,
    area: (u16, u16),
    frame: u64,
) -> Vec<Line<'static>> {
    let (w, h) = (area.0 as usize, area.1 as usize);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (state, stopped) = match feed {
        PanelFeed::Waiting => return panel_note(PANEL_WAITING, w, h),
        PanelFeed::Off { enabled: false } => return panel_note(PANEL_OFF, w, h),
        PanelFeed::Off { enabled: true } => return panel_note(PANEL_NOT_RUNNING, w, h),
        PanelFeed::Live { state, stopped } => (state.as_ref(), *stopped),
    };
    if !state.has_world() {
        let text = if stopped {
            PANEL_STOPPED_EARLY
        } else {
            PANEL_WAITING
        };
        return panel_note(text, w, h);
    }
    let look = Look::for_state(state, memory);
    let mut out = vec![hud_row(state, memory, &look, w).line()];
    if stopped && out.len() < h {
        out.push(banner_row(&look, w).line());
    }
    let body = h - out.len();
    if body == 0 {
        return out;
    }
    let map = |cols, rows| map_rows(state, memory, &look, cols, rows, frame);
    let flags = SagaFlags {
        stopped,
        why: false,
    };
    let chronicle = w >= PANEL_CHRONICLE_MIN.0 && h >= PANEL_CHRONICLE_MIN.1;
    if chronicle && w >= SIDE_MIN_COLS {
        // As the full view's side layout: the map pane is the floor's width
        // and three columns, up to three fifths of the panel.
        let floor_w = hero_floor(state).map_or(1, |f| f.w as usize);
        let map_w = (floor_w + 3).min(w * 3 / 5);
        let chron_w = w - map_w - 1;
        let maps = map(map_w, body);
        let chron = chronicle_rows(state, flags, &look, chron_w, body, false);
        out.extend(maps.into_iter().zip(chron).map(|(m, c)| m.join(1, c)));
    } else if chronicle {
        // A third of the rows, and at least the header and three beats'
        // first lines.
        let chron_h = (body / 3).max(4).min(body - 1);
        out.extend(map(w, body - chron_h).into_iter().map(Row::line));
        out.extend(
            chronicle_rows(state, flags, &look, w, chron_h, false)
                .into_iter()
                .map(Row::line),
        );
    } else {
        out.extend(map(w, body).into_iter().map(Row::line));
    }
    out
}

/// The panel's HUD: the label mark, then the full view's status row when
/// it fits up to the end of the quest. A narrower panel (the Saga panel of
/// `now-serving` at 80x24 is 39 columns) gets a short form that keeps the
/// three things the HUD is for: the hero, its vitality as a percentage in
/// the bar's colour, and the quest. The short form drops the hero's title
/// (`Yarrow` for `Yarrow the Well-Cached`) when the whole name does not
/// fit. The label mark comes first in both, so a narrow panel keeps it.
fn hud_row(state: &SagaState, memory: &SagaMemory, look: &Look, w: usize) -> Row {
    let cols = |s: &str| -> usize {
        s.chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    };
    let mut row = Row::new(w);
    let mark = state.label.mark();
    if let Some(mark) = mark {
        row.push(" ", Style::default())
            .push(mark, fg(look.p.yellow).add_modifier(Modifier::BOLD));
    }
    let lead = mark.map_or(0, |m| 1 + cols(m));
    let name = state.hero.name.as_str();
    let quest = quest_text(state);
    // " name  ▰▰▰▰▰▰▰▰▰▰  99%  • quest", as `status_after` draws it.
    let full = lead + 1 + cols(name) + 2 + VITALITY_CELLS + 5 + 4 + cols(&quest);
    if full <= w {
        return status_after(row, state, memory, look);
    }
    // " name 99% • quest".
    let short = |n: &str| lead + 1 + cols(n) + 5 + 3 + cols(&quest);
    let first = name.split(' ').next().unwrap_or(name);
    let shown = if short(name) <= w { name } else { first };
    let (v, bar_style) = vitality(state, look);
    row.push(" ", Style::default())
        .push(shown, look.name)
        .push(
            &format!(" {:>3.0}%", v * 100.0),
            bar_style.add_modifier(Modifier::BOLD),
        )
        .push(" • ", separator(look))
        .push(&quest, look.quest);
    row
}

/// A note in the panel: ` text` in the arcade's note colour, wrapped to
/// the panel's width, at most `h` lines.
fn panel_note(text: &str, w: usize, h: usize) -> Vec<Line<'static>> {
    let style = Style::default().fg(colors::text_secondary());
    wrap(text, w.saturating_sub(1), w.saturating_sub(1))
        .into_iter()
        .take(h)
        .map(|t| {
            let mut row = Row::new(w);
            row.push(" ", Style::default()).push(&t, style);
            row.line()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The legend
// ---------------------------------------------------------------------------

/// The `l` legend: every map glyph from the pack, in the map's colours (a
/// dungeon's, lit), then the light, the chronicle and the status bar.
pub fn legend_lines(bar: Color, bg: Color, dim: Color) -> Vec<Line<'static>> {
    let look = Look::new(Biome::Dungeon, None);
    let g = &Pack::builtin().glyphs;
    let base = look.p.base;
    let p = look.p;
    let dim = Style::default().fg(dim);
    let line = |parts: Vec<(String, Style)>| {
        let mut spans = vec![Span::styled(BAR, Style::default().fg(bar).bg(bg))];
        spans.extend(parts.into_iter().map(|(t, s)| Span::styled(t, s)));
        Line::from(spans)
    };
    let lit = |t: Tint| t.at(base, 100);
    // Two glyphs per row: `a  what a is   b  what b is`, the glyphs padded
    // to five columns and the first label to 23, so the columns line up.
    let pair = |a: (String, Style, &str), b: Option<(String, Style, &str)>| {
        let pad = " ".repeat(5usize.saturating_sub(a.0.chars().count()));
        let mut parts = vec![(a.0, a.1), (format!("{pad}{:<23}", a.2), dim)];
        if let Some(b) = b {
            let pad = " ".repeat(5usize.saturating_sub(b.0.chars().count()));
            parts.push((b.0, b.1));
            parts.push((format!("{pad}{}", b.2), dim));
        }
        line(parts)
    };
    let c = |ch: char| ch.to_string();
    let inner = |m: u8| g.wall_tile(WallKind::Inner, m);
    let walls: String = [
        ARM_E | ARM_S,
        ARM_E | ARM_W,
        ARM_E | ARM_S | ARM_W,
        ARM_S | ARM_W,
    ]
    .iter()
    .map(|&m| inner(m))
    .collect();
    let shell: String = [ARM_E | ARM_S, ARM_E | ARM_W, ARM_S | ARM_W]
        .iter()
        .map(|&m| g.wall_tile(WallKind::ShellCorner, m))
        .collect();
    vec![
        line(vec![("THE MAP".to_string(), look.head)]),
        pair(
            (c(g.hero), look.hero_well, "the hero"),
            Some((c(g.trail), fg(mix(base, p.yellow, 80)), "its trail")),
        ),
        line(vec![
            (c(g.hero), look.hero_weak),
            (
                "    below half vitality (heartbeat, clock, heat)".to_string(),
                dim,
            ),
        ]),
        pair(
            (c(g.npc), sage_style(0), "a sage to visit"),
            Some((
                format!("{}{}", g.goal, g.goal_pulse),
                goal_style(0),
                "the goal, pulsing",
            )),
        ),
        pair(
            (walls, lit(look.wall), "walls"),
            Some((shell, lit(look.shell), "the outer shell")),
        ),
        pair(
            (c(g.floor), lit(look.floor), "floor"),
            Some((c(g.ruin), lit(look.ruin), "ruin: harvested column")),
        ),
        pair(
            (c(g.door), lit(look.door), "door"),
            Some((c(g.exit), lit(look.exit), "exit: farthest room")),
        ),
        pair(
            (c(g.vault), lit(look.vault), "vault: GDDR channel"),
            Some((c(g.portal), lit(look.portal_lit), "portal: live Ethernet")),
        ),
        pair(
            (c(g.gate), lit(look.gate), "gate: PCIe, the way in"),
            Some((c(g.wall), lit(look.wall), "a pillar")),
        ),
        line(vec![]),
        line(vec![("THE LIGHT".to_string(), look.head)]),
        line(vec![
            (c(g.floor).repeat(3), look.floor.at(base, 100)),
            ("  in the hero's light   ".to_string(), dim),
            (c(g.floor).repeat(3), look.floor.at(base, 0)),
            ("  seen before".to_string(), dim),
        ]),
        line(vec![(
            "blank: not seen yet, or solid rock. The memory starts over".to_string(),
            dim,
        )]),
        line(vec![(
            "on a new floor or world. A hot chip warms its floor:".to_string(),
            dim,
        )]),
        line(vec![
            (c(g.floor).repeat(3), fg(p.teal)),
            (c(g.floor).repeat(3), fg(p.yellow)),
            (c(g.floor).repeat(3), fg(p.orange)),
            ("  45 °C, 65 °C, 85 °C".to_string(), dim),
        ]),
        line(vec![]),
        line(vec![("THE CHRONICLE".to_string(), look.head)]),
        line(vec![
            (format!("{} calm", tone_mark(Tone::Calm)), fg(p.teal)),
            ("  ".to_string(), dim),
            (format!("{} tense", tone_mark(Tone::Tense)), fg(p.orange)),
            ("  ".to_string(), dim),
            (
                format!("{} triumphant", tone_mark(Tone::Triumphant)),
                fg(p.yellow),
            ),
            ("  names in bold".to_string(), dim),
        ]),
        line(vec![(
            "t42  the tick the line was written at (one per sweep)".to_string(),
            dim,
        )]),
        line(vec![(
            "y    what set each line off, in words".to_string(),
            dim,
        )]),
        line(vec![
            ("▰▰".to_string(), fg(p.green)),
            ("▰▰".to_string(), fg(p.yellow)),
            ("▰▰".to_string(), fg(p.orange)),
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
