// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The party view: every floor of the world on screen at once, one tile per
//! floor, each tile centred on its own hero (the party addendum, section 5).
//!
//! [`super::render_saga`] sends a world of two or more floors here. A world
//! of one floor never comes here and draws exactly as it did before the
//! party. The tiles reuse the single-floor drawing: each tile's map comes
//! from [`super::map_rows`] with that floor's own memory
//! ([`super::PartyMemory::floor`]), so the walls, the fog, the light, the
//! remembered cells, the trail, the glow and the heat colour are the same as
//! in the single-floor view.
//!
//! # The screen
//!
//! * **Title row:** `▌ SAGA`, the label mark, the world's id and biome, and
//!   `party of N`.
//! * **Roster row:** shown when the grid has more than one page, or when
//!   some chips have no floor. `party`, then one hero glyph per member in
//!   index order, two columns each, in the member's floor hue: bright when
//!   the member's tile is on the page shown, dim otherwise, orange below
//!   half vitality, and drawn on its hue's background when focused. Then
//!   `+N chips without a floor` when the world left chips out, and the page
//!   (`page 2 of 6`, or `2/6` when the row is short of room).
//! * **The grid** ([`party_grid`]): under 100 columns it sits above the
//!   chronicle and takes every row except the chronicle's header and four
//!   lines; from 100 columns it sits left of the chronicle and takes up to
//!   three fifths of the width, as the single-floor map pane does.
//! * **A tile** ([`tile_rows`]): a HUD row in a dark band (`▌F3`, the hero's
//!   name, a four-cell vitality bar and the quest with its target label),
//!   then the floor's map centred on its hero. Each band is tinted with its
//!   floor's hue; the focused tile's band is brighter and its tag is drawn
//!   reversed.
//! * **Chronicle:** the merged chronicle, each member's line tagged with its
//!   floor (`t42 · F3 Wren sets out`) in the floor's hue.
//! * **Status row:** the focused hero's name, vitality, quest and mapped
//!   share, then `[ ] focus  z zoom` and the tick.
//!
//! # Borders
//!
//! Tiles are separated by one blank column, and each tile's last column is
//! blank: the map keeps its last column blank (see "Borders" in the parent
//! module), and the HUD band stops one column short of the tile's edge. So
//! no right-edge glyph from `crate::animation::arcade::RIGHT_EDGE_GLYPHS`
//! ever ends a tile, a row or the grid.

use super::*;

/// The smallest tile, (columns, rows): a HUD row over six map rows, and a
/// map window of 23 columns after the blank last one. The hero's light is
/// 19 cells across, so the window holds all of it from side to side.
pub const TILE_MIN: (usize, usize) = (24, 7);

/// The largest tile, (columns, rows): a Blackhole floor (53 x 38 cells, the
/// largest floor a chip gets) with the blank last column and the HUD row. A
/// larger share of the screen would show only blank cells around the floor.
const TILE_MAX: (usize, usize) = (54, 39);

/// Rows the chronicle keeps under the grid below 100 columns: its header
/// and four lines.
const CHRONICLE_KEEP: usize = 5;

/// Cells of a tile HUD's vitality bar.
const HUD_BAR_CELLS: usize = 4;

/// The brand's red `#FF9E8A`, and the `+` tints of teal `#9BDBF0` and green
/// `#92C9BF` (the docs-site theme, `tt_theme.css`). The pack's palette has
/// no red and no tints; these serve only the floor hues.
const RED: Rgb = [0xFF, 0x9E, 0x8A];
const TEAL_PLUS: Rgb = [0x9B, 0xDB, 0xF0];
const GREEN_PLUS: Rgb = [0x92, 0xC9, 0xBF];

/// The colour of floor `floor`: a fixed cycle of eight hues from the brand
/// palette (teal, blue, green, yellow, red, orange, then the lighter teal
/// and green), floor `i` taking hue `i mod 8` (the party addendum, section
/// 5.4). Floor tags, tile HUDs and roster glyphs use it.
pub fn floor_hue(palette: &Palette, floor: u8) -> Rgb {
    match floor % 8 {
        0 => palette.teal,
        1 => palette.accent,
        2 => palette.green,
        3 => palette.yellow,
        4 => RED,
        5 => palette.orange,
        6 => TEAL_PLUS,
        _ => GREEN_PLUS,
    }
}

/// How the floors tile the screen: `cols` x `rows` tiles of `tile_w` x
/// `tile_h` cells, with one blank column between tiles, and `per_page`
/// tiles on a page (`cols * rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub tile_w: usize,
    pub tile_h: usize,
    pub per_page: usize,
}

impl Grid {
    /// Pages for `n` floors.
    pub fn pages(&self, n: usize) -> usize {
        n.div_ceil(self.per_page.max(1)).max(1)
    }

    /// The grid's width in columns, gaps included.
    fn width(&self) -> usize {
        self.cols * self.tile_w + self.cols.saturating_sub(1)
    }

    /// The grid's height in rows.
    fn height(&self) -> usize {
        self.rows * self.tile_h
    }
}

/// The grid for `n` floors in a view of `w` x `h` (the Saga view's area,
/// the app's status bar already taken off), or `None` when not even one
/// tile of [`TILE_MIN`] fits. The roster row is counted when the floors
/// take more than one page.
///
/// The rule (the party addendum, section 5.2):
///
/// * The grid's area: the view less its title and status rows (and the
///   roster). Under 100 columns it is the whole width, and every row except
///   the chronicle's [`CHRONICLE_KEEP`]. From 100 columns it is three
///   fifths of the width and every row.
/// * For each column count, the tiles share the width evenly after one
///   blank column between them, and as many rows as the floors need (or as
///   fit) share the height evenly.
/// * The shape that shows the most floors wins. On a tie, the larger tiles
///   win, measured by the map cells a tile shows, each side capped at
///   [`TILE_MAX`]. On a further tie, the fewer columns win.
/// * A tile never grows past [`TILE_MAX`].
pub fn party_grid(n: usize, w: usize, h: usize) -> Option<Grid> {
    let g = fit(n, w, h, false)?;
    if g.per_page >= n {
        Some(g)
    } else {
        fit(n, w, h, true)
    }
}

/// The grid's area for a view of `w` x `h`, (columns, rows), and whether
/// the chronicle sits beside it: `None` below the size at which the
/// single-floor view draws a chronicle at all.
fn grid_area(w: usize, h: usize, roster: bool) -> Option<(usize, usize, bool)> {
    if w < CHRONICLE_MIN.0 || h < CHRONICLE_MIN.1 {
        return None;
    }
    let body = h.checked_sub(2 + usize::from(roster))?;
    if w < SIDE_MIN_COLS {
        Some((w, body.checked_sub(CHRONICLE_KEEP)?, false))
    } else {
        Some((w * 3 / 5, body, true))
    }
}

/// The best grid for `n` floors in the area [`grid_area`] gives (see
/// [`party_grid`] for the rule).
fn fit(n: usize, w: usize, h: usize, roster: bool) -> Option<Grid> {
    if n == 0 {
        return None;
    }
    let (gw, gh, _) = grid_area(w, h, roster)?;
    let max_rows = gh / TILE_MIN.1;
    if max_rows == 0 {
        return None;
    }
    // (floors shown, map cells a tile shows, fewer columns) ranks a shape.
    let mut best: Option<((usize, usize, std::cmp::Reverse<usize>), Grid)> = None;
    for cols in 1..=n {
        let tile_w = (gw - (cols - 1).min(gw)) / cols;
        if tile_w < TILE_MIN.0 {
            break;
        }
        let rows = n.div_ceil(cols).min(max_rows);
        let tile_h = gh / rows;
        let (tile_w, tile_h) = (tile_w.min(TILE_MAX.0), tile_h.min(TILE_MAX.1));
        let grid = Grid {
            cols,
            rows,
            tile_w,
            tile_h,
            per_page: cols * rows,
        };
        let cells = (tile_w - 1) * (tile_h - 1);
        let rank = (grid.per_page.min(n), cells, std::cmp::Reverse(cols));
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, grid));
        }
    }
    best.map(|(_, g)| g)
}

/// The party view of a world with two or more floors, or `None` when not
/// even one tile fits (the caller then draws the focused floor alone). At
/// most `area.1` lines, none wider than `area.0`.
pub(super) fn render_party(
    state: &SagaState,
    party: &PartyMemory,
    flags: SagaFlags,
    area: (u16, u16),
    frame: u64,
) -> Option<Vec<Line<'static>>> {
    let (w, h) = (area.0 as usize, area.1 as usize);
    let n = state.world.floors.len();
    let focus = flags.focus.min(n.checked_sub(1)?);
    let banner = usize::from(flags.stopped);
    let extra = state.chips_seen.saturating_sub(state.world.chips.len());
    let h = h.checked_sub(banner)?;
    // Chips without a floor need the roster row whatever the page count.
    let grid = if extra > 0 {
        fit(n, w, h, true)?
    } else {
        party_grid(n, w, h)?
    };
    let roster = extra > 0 || grid.per_page < n;
    let (_, _, side) = grid_area(w, h, roster)?;
    let h = h + banner;
    let focus_memory = party.floor(focus).unwrap_or(&NO_MEMORY);
    let look = Look::for_floor(state, focus_memory, focus);
    let page = focus / grid.per_page;

    let mut out = vec![title_row(state, &look, w, false, None).line()];
    if flags.stopped {
        out.push(banner_row(&look, w).line());
    }
    if roster {
        out.push(roster_row(state, flags, &grid, page, w).line());
    }
    let body = h - out.len() - 1;
    let grid_w = if side { grid.width() } else { w };
    let tiles = grid_rows(state, party, &grid, page, focus, grid_w, frame);
    if side {
        let chron_w = w - grid_w - 1;
        let chron = chronicle_rows(state, flags, &look, chron_w, body, true, true);
        let mut tiles = tiles.into_iter();
        out.extend(chron.into_iter().map(|c| {
            let t = tiles.next().unwrap_or_else(|| Row::new(grid_w));
            t.join(1, c)
        }));
    } else {
        let chron_h = body - grid.height();
        out.extend(tiles.into_iter().map(Row::line));
        out.extend(
            chronicle_rows(state, flags, &look, w, chron_h, true, true)
                .into_iter()
                .map(Row::line),
        );
    }
    let seat = floor_seat(state, focus);
    out.push(status_row(state, focus_memory, &look, w, seat, true).line());
    Some(out)
}

/// The grid's rows for page `page`: each tile's rows laid side by side with
/// one blank column between tiles, every row `grid_w` wide at most.
fn grid_rows(
    state: &SagaState,
    party: &PartyMemory,
    grid: &Grid,
    page: usize,
    focus: usize,
    grid_w: usize,
    frame: u64,
) -> Vec<Row> {
    let n = state.world.floors.len();
    let first = page * grid.per_page;
    let mut out: Vec<Row> = (0..grid.height()).map(|_| Row::new(grid_w)).collect();
    for slot in 0..grid.per_page {
        let floor = first + slot;
        if floor >= n {
            break;
        }
        let (gx, gy) = (slot % grid.cols, slot / grid.cols);
        let memory = party.floor(floor).unwrap_or(&NO_MEMORY);
        let look = Look::for_floor(state, memory, floor);
        let rows = tile_rows(
            state,
            memory,
            &look,
            floor,
            grid.tile_w,
            grid.tile_h,
            floor == focus,
            frame,
        );
        let x = gx * (grid.tile_w + 1);
        for (r, tile_row) in rows.into_iter().enumerate() {
            let line = &mut out[gy * grid.tile_h + r];
            line.pad_to(x);
            line.append(tile_row);
        }
    }
    out
}

/// One tile, `w` x `h`: the HUD row ([`tile_hud`]) over `h - 1` rows of
/// floor `floor` centred on its hero ([`map_rows`]). `memory` is that
/// floor's memory and `look` its look (its own heat). Every row leaves the
/// tile's last column blank.
#[allow(clippy::too_many_arguments)]
fn tile_rows(
    state: &SagaState,
    memory: &SagaMemory,
    look: &Look,
    floor: usize,
    w: usize,
    h: usize,
    focused: bool,
    frame: u64,
) -> Vec<Row> {
    let seat = floor_seat(state, floor);
    let mut out = vec![tile_hud(state, look, seat, w, focused)];
    out.extend(map_rows(
        state,
        memory,
        look,
        seat,
        w,
        h.saturating_sub(1),
        frame,
    ));
    out
}

/// A tile's HUD row: `▌F3 Wren the Unthrottled ▰▰▰▱ visit demo-sage` in a
/// dark band that stops one column short of the tile's edge. The tag is in
/// the floor's hue and the band is tinted with it; on the focused tile the
/// band is brighter and the tag is reversed. When the row is narrow the hero's title goes
/// first, then the quest's target, then the row is cut at the tile's edge.
fn tile_hud(state: &SagaState, look: &Look, seat: Seat, w: usize, focused: bool) -> Row {
    let p = &look.p;
    let hue = floor_hue(p, seat.floor as u8);
    // Every band carries a little of its floor's hue, so a tile keeps its
    // colour across the screen; the focused band carries more.
    let band_bg = if focused {
        mix(p.base, hue, 40)
    } else {
        mix(p.base, hue, 16)
    };
    let band = Style::default().bg(rgb(band_bg));
    let inner = w.saturating_sub(1);
    let mut row = Row::new(inner);
    let tag = format!("F{}", seat.floor + 1);
    let tag_style = if focused {
        fg(p.base).bg(rgb(hue)).add_modifier(Modifier::BOLD)
    } else {
        fg(hue).bg(rgb(band_bg)).add_modifier(Modifier::BOLD)
    };
    row.push("▌", fg(hue).bg(rgb(band_bg)))
        .push(&tag, tag_style)
        .push(" ", band);
    let Some(member) = seat.member.and_then(|m| state.party.get(m)) else {
        row.push("no hero here", look.dim.bg(rgb(band_bg)));
        row.fill_to(inner, band);
        return row;
    };
    let (v, bar_style) = vitality(member, look);
    let filled = ((v * HUD_BAR_CELLS as f32).round() as usize).min(HUD_BAR_CELLS);
    let quest = quest_word(member);
    let width = |s: &str| -> usize {
        s.chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    };
    let name = member.hero.name.as_str();
    // The whole name when it leaves room for the bar and the quest, else
    // its first word. A quest that still does not fit keeps its verb alone,
    // so the row never ends in half a name.
    let rest = |n: &str| row.used + width(n) + 1 + HUD_BAR_CELLS + 1;
    let shown = if rest(name) + width(&quest) <= inner {
        name
    } else {
        name.split(' ').next().unwrap_or(name)
    };
    let quest = if rest(shown) + width(&quest) <= inner {
        quest
    } else {
        quest.split(' ').next().unwrap_or_default().to_string()
    };
    row.push(shown, look.name.bg(rgb(band_bg)))
        .push(" ", band)
        .push(&"▰".repeat(filled), bar_style.bg(rgb(band_bg)))
        .push(
            &"▱".repeat(HUD_BAR_CELLS - filled),
            look.dim.bg(rgb(band_bg)),
        )
        .push(" ", band)
        .push(&quest, look.quest.bg(rgb(band_bg)));
    row.fill_to(inner, band);
    row
}

/// A member's quest in a word or two for its tile's HUD: `explore`,
/// `rest`, `visit demo-sage` (the core's target label, the alias in
/// generated mode), with `✓` once the hero stands on its goal.
fn quest_word(member: &Member) -> String {
    let q = &member.quest;
    let target = quest_target(member);
    let named = |what: &str, fallback: &str| match &target {
        Some(t) => format!("{what} {t}"),
        None => fallback.to_string(),
    };
    let mut text = match q.verb {
        Verb::Explore => "explore".to_string(),
        Verb::Rest => "rest".to_string(),
        Verb::VisitSage => named("visit", "visit a sage"),
        Verb::Slay => named("hunt", "hunt"),
        Verb::Escort => named("escort", "escort"),
        Verb::Cleanse => named("cleanse", "cleanse"),
        Verb::Rebuild => named("rebuild", "rebuild"),
    };
    if q.verb != Verb::Rest && member.hero.pos == q.goal {
        text.push_str(" ✓");
    }
    text
}

/// The roster row: `party` and one hero glyph per member, two columns
/// each, then the page and the chips without a floor (see "The screen" in
/// the module comment). `page` is the page shown.
fn roster_row(state: &SagaState, flags: SagaFlags, grid: &Grid, page: usize, w: usize) -> Row {
    let p = &Pack::builtin().palette;
    let glyph = Pack::builtin().glyphs.hero;
    let n = state.world.floors.len();
    let focus = flags.focus.min(n.saturating_sub(1));
    let shown = page * grid.per_page..(page + 1) * grid.per_page;
    let look = Look::new(state.world.biome, None);
    let mut row = Row::new(w);
    row.push(" party ", look.head);
    for m in 0..state.party.len() {
        let floor = member_floor(state, m).unwrap_or(0);
        let hue = floor_hue(p, floor as u8);
        let weak = state.party[m].hero.vitality < REST_BELOW;
        let colour = if weak { p.orange } else { hue };
        let style = if floor == focus {
            fg(p.base).bg(rgb(colour)).add_modifier(Modifier::BOLD)
        } else if shown.contains(&floor) {
            fg(colour).add_modifier(Modifier::BOLD)
        } else {
            fg(mix(p.base, colour, 45))
        };
        row.put(glyph, style).push(" ", Style::default());
    }
    let pages = grid.pages(n);
    let extra = state.chips_seen.saturating_sub(state.world.chips.len());
    // Each segment goes whole or not at all, the chips first.
    if extra > 0 {
        let long = format!(" +{extra} chips without a floor");
        let short = format!(" +{extra} chips");
        if row.room() >= long.len() {
            row.push(&long, fg(p.orange));
        } else if row.room() >= short.len() {
            row.push(&short, fg(p.orange));
        }
    }
    if pages > 1 {
        let long = format!(" page {} of {pages}", page + 1);
        let short = format!(" {}/{pages}", page + 1);
        if row.room() >= long.len() {
            row.push(&long, look.dim);
        } else if row.room() >= short.len() {
            row.push(&short, look.dim);
        }
    }
    row
}

#[cfg(test)]
#[path = "saga_party_tests.rs"]
mod saga_party_tests;
