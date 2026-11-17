// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the party view (`saga_party.rs`, Task A.7 of the party plan).
//!
//! The states come from the saga crate's fixtures fed through a real
//! [`Saga`]: `solo.jsonl` (one chip), `busy.jsonl` (two), `party4.jsonl`
//! (four), and the 32-chip and 40-chip logs, which the saga crate's
//! generator (`tests/support/party.rs`) builds in memory. Colours run inside
//! [`tc`], as in the single-floor view's tests, so the frames hash the same
//! under `TERM=dumb`.

use super::*;
use crate::ui::colors::{self, Theme};
use ratatui::style::Color;
use std::sync::OnceLock;
use tt_toplike_bus::segment::read_segment;
use tt_toplike_bus::Record;
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::{Label, Saga, SagaConfig};

/// The saga crate's party generator, the source of the 32-chip and 40-chip
/// logs. It reads only the bus crate, so it builds here too.
#[path = "../../../crates/tt-toplike-saga/tests/support/party.rs"]
#[allow(dead_code)]
mod party_gen;

// ---- Helpers ---------------------------------------------------------------

/// Truecolor on and the default theme, as every Saga golden runs.
fn tc<R>(f: impl FnOnce() -> R) -> R {
    colors::with_truecolor(true, || colors::with_theme(Theme::Default, f))
}

/// The ticks of the in-memory 32-chip log: as long as `party4.jsonl`.
const LOG32_TICKS: u64 = 600;

/// The saga config of the view's tests: a 200-tick season.
fn config(names: NameMode) -> SagaConfig {
    SagaConfig {
        season_ticks: 200,
        names,
        label: Label::Mock,
    }
}

/// A committed fixture of the saga crate, read once per call site.
fn fixture(name: &str) -> Vec<Record> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/tt-toplike-saga/tests/fixtures")
        .join(name);
    let read = read_segment(&path).expect("the fixture is committed");
    assert_eq!(read.bad_lines, 0, "{name} decodes");
    read.records
}

fn solo() -> &'static [Record] {
    static R: OnceLock<Vec<Record>> = OnceLock::new();
    R.get_or_init(|| fixture("solo.jsonl"))
}

fn busy() -> &'static [Record] {
    static R: OnceLock<Vec<Record>> = OnceLock::new();
    R.get_or_init(|| fixture("busy.jsonl"))
}

fn party4() -> &'static [Record] {
    static R: OnceLock<Vec<Record>> = OnceLock::new();
    R.get_or_init(|| fixture("party4.jsonl"))
}

fn log32() -> &'static [Record] {
    static R: OnceLock<Vec<Record>> = OnceLock::new();
    R.get_or_init(|| party_gen::records(32, LOG32_TICKS))
}

/// The 32-chip log with a ring of links and a model ready on every chip
/// (the generator's `records_with_ring_model`): every floor offers
/// crossings.
fn log32_ring() -> &'static [Record] {
    static R: OnceLock<Vec<Record>> = OnceLock::new();
    R.get_or_init(|| party_gen::records_with_ring_model(32, LOG32_TICKS))
}

/// Forty chips: the 32-chip script and eight chips that only sweep. The
/// world keeps 32 floors.
fn log40() -> &'static [Record] {
    static R: OnceLock<Vec<Record>> = OnceLock::new();
    R.get_or_init(|| party_gen::records_with_extra_chips(40, 60))
}

/// A saga fed every record and a party memory that observed the state
/// after each one, as the TUI's loop does.
fn run(records: &[Record], names: NameMode) -> (Saga, PartyMemory) {
    let mut saga = Saga::new(config(names));
    let mut party = PartyMemory::new();
    for r in records {
        saga.feed(r);
        party.observe(saga.state());
    }
    (saga, party)
}

/// The end of each log, real names, cached per log.
fn end_of(which: &str) -> &'static (SagaState, PartyMemory) {
    static SOLO: OnceLock<(SagaState, PartyMemory)> = OnceLock::new();
    static BUSY: OnceLock<(SagaState, PartyMemory)> = OnceLock::new();
    static P4: OnceLock<(SagaState, PartyMemory)> = OnceLock::new();
    static P32: OnceLock<(SagaState, PartyMemory)> = OnceLock::new();
    static P40: OnceLock<(SagaState, PartyMemory)> = OnceLock::new();
    static CROSS: OnceLock<(SagaState, PartyMemory)> = OnceLock::new();
    let (cell, log): (&OnceLock<_>, fn() -> &'static [Record]) = match which {
        "solo" => (&SOLO, solo),
        "busy" => (&BUSY, busy),
        "party4" => (&P4, party4),
        "32" => (&P32, log32),
        "40" => (&P40, log40),
        "cross" => (&CROSS, cross4),
        _ => unreachable!("no log {which}"),
    };
    cell.get_or_init(|| {
        let (saga, party) = run(log(), NameMode::Real);
        (saga.state().clone(), party)
    })
}

/// The flags with `focus`, `zoom`, `why` and `stopped` set.
fn flags(focus: usize, zoom: bool, why: bool, stopped: bool) -> SagaFlags {
    SagaFlags {
        stopped,
        why,
        focus,
        zoom,
    }
}

fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn width(line: &Line<'static>) -> usize {
    use unicode_width::UnicodeWidthStr;
    line.spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

fn assert_fits(lines: &[Line<'static>], w: u16, h: u16, what: &str) {
    assert!(
        lines.len() <= h as usize,
        "{what} at {w}x{h}: {} lines",
        lines.len()
    );
    for (i, l) in lines.iter().enumerate() {
        assert!(
            width(l) <= w as usize,
            "{what} at {w}x{h}: line {i} is {} columns",
            width(l)
        );
    }
}

/// Each line's cells, one entry per column: the character and its style.
fn cells(line: &Line<'static>) -> Vec<(char, Style)> {
    use unicode_width::UnicodeWidthChar;
    let mut out = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for ch in span.content.chars() {
            for _ in 0..UnicodeWidthChar::width(ch).unwrap_or(0) {
                out.push((ch, style));
            }
        }
    }
    out
}

/// FNV-1a over each cell's character, colours and modifiers, as the
/// single-floor view's `frame_hash`.
fn frame_hash(lines: &[Line<'static>]) -> u64 {
    fn bytes(h: &mut u64, b: &[u8]) {
        for &x in b {
            *h ^= x as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn color(h: &mut u64, c: Option<Color>) {
        match c {
            None => bytes(h, &[0]),
            Some(Color::Reset) => bytes(h, &[1]),
            Some(Color::Rgb(r, g, b)) => bytes(h, &[2, r, g, b]),
            Some(other) => bytes(h, format!("{other:?}").as_bytes()),
        }
    }
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for line in lines {
        for span in &line.spans {
            let style = line.style.patch(span.style);
            for ch in span.content.chars() {
                bytes(&mut h, &(ch as u32).to_le_bytes());
                color(&mut h, style.fg);
                color(&mut h, style.bg);
                bytes(&mut h, &style.add_modifier.bits().to_le_bytes());
                bytes(&mut h, &style.sub_modifier.bits().to_le_bytes());
            }
        }
        bytes(&mut h, &[0xff, 0xfe]);
    }
    h
}

/// The sizes of the addendum's table, as the view gets them: 80x24,
/// 140x40 and 200x60 terminals less the app's status bar.
const TABLE_SIZES: [(u16, u16); 3] = [(80, 23), (140, 39), (200, 59)];

// ---- The grid ----------------------------------------------------------------

/// `party_grid` gives the shapes of the addendum's table (section 5.2) for
/// 1, 2, 4 and 32 chips, as its rule computes them: the tiles share every
/// row the chronicle leaves (ruling R34: the code's rule stands and the
/// addendum's table is corrected in Task A.8). At 80x23 that is one row of
/// two 39x16 tiles for 2 chips and 2 x 2 tiles of 39x8 for 4 chips (the
/// addendum's working had 39x11 and 39x7). At 200x59 the table's blank
/// cells are 2 tiles of 54x39 for 2 chips and 2 x 2 tiles of 54x28 for 4.
/// The 32-chip cells and the 140x39 cells are the addendum's numbers.
#[test]
fn party_grid_gives_the_addendum_table() {
    let g = |cols, rows, tile_w, tile_h, per_page| Grid {
        cols,
        rows,
        tile_w,
        tile_h,
        per_page,
    };
    let table: [(usize, (usize, usize), Grid); 12] = [
        (1, (80, 23), g(1, 1, 54, 16, 1)),
        (1, (140, 39), g(1, 1, 54, 37, 1)),
        (1, (200, 59), g(1, 1, 54, 39, 1)),
        (2, (80, 23), g(2, 1, 39, 16, 2)),
        (2, (140, 39), g(2, 1, 41, 37, 2)),
        (2, (200, 59), g(2, 1, 54, 39, 2)),
        (4, (80, 23), g(2, 2, 39, 8, 4)),
        (4, (140, 39), g(2, 2, 41, 18, 4)),
        (4, (200, 59), g(2, 2, 54, 28, 4)),
        (32, (80, 23), g(3, 2, 26, 7, 6)),
        (32, (140, 39), g(3, 5, 27, 7, 15)),
        (32, (200, 59), g(4, 8, 29, 7, 32)),
    ];
    for (n, (w, h), want) in table {
        assert_eq!(party_grid(n, w, h), Some(want), "{n} chips at {w}x{h}");
    }
    assert_eq!(party_grid(32, 80, 23).unwrap().pages(32), 6);
    assert_eq!(party_grid(32, 140, 39).unwrap().pages(32), 3);
    assert_eq!(party_grid(32, 200, 59).unwrap().pages(32), 1);
    // Every grid of every size holds tiles of at least the minimum and
    // fits its area.
    for n in [1usize, 2, 4, 32] {
        for w in 0..=260 {
            for h in (0..=80).step_by(3) {
                let Some(g) = party_grid(n, w, h) else {
                    continue;
                };
                assert!(
                    g.tile_w >= TILE_MIN.0 && g.tile_h >= TILE_MIN.1,
                    "{n} {w}x{h}"
                );
                assert!(g.width() <= w && g.height() + 2 <= h, "{n} {w}x{h}: {g:?}");
                assert_eq!(g.per_page, g.cols * g.rows);
            }
        }
    }
    assert_eq!(party_grid(0, 80, 23), None, "no floors, no grid");
    assert_eq!(party_grid(4, 23, 60), None, "no tile is 23 columns wide");
}

/// When not even one tile fits, the view draws the focused floor alone, as
/// a world of one floor draws at that size (the map alone, or the title).
#[test]
fn below_the_minimum_tile_the_focused_floor_draws_alone() {
    let (state, party) = end_of("party4");
    tc(|| {
        for (w, h) in [(30u16, 10u16), (60, 11), (80, 11), (23, 40), (12, 4)] {
            assert_eq!(party_grid(4, w as usize, h as usize), None, "{w}x{h}");
            let f = flags(2, false, false, false);
            let got = render_saga(state, party, f, (w, h), 5);
            let want = render_floor(
                state,
                party.floor(2).unwrap(),
                floor_seat(state, 2),
                f,
                (w, h),
                5,
            );
            assert_eq!(frame_hash(&got), frame_hash(&want), "{w}x{h}");
            if w >= 40 {
                assert!(text(&got)[0].contains("floor 3 of 4"), "{w}x{h}");
            }
        }
    });
}

// ---- Sizes and borders -----------------------------------------------------------

/// Every size from 0x0 to 500x200 fits for 1, 2, 4 and 32 chips, and for
/// the end of the four-chip cross log: no line wider than the area and no
/// more lines than its rows, with the focus on several floors, with "why"
/// on and the story stopped, and zoomed.
#[test]
fn the_party_view_fits_every_size_for_1_2_4_and_32_chips() {
    let widths: Vec<u16> = (0..=130)
        .chain((131..=500).step_by(17))
        .chain([500])
        .collect();
    let heights: Vec<u16> = (0..=30)
        .chain((31..=200).step_by(13))
        .chain([200])
        .collect();
    tc(|| {
        for which in ["solo", "busy", "party4", "32", "cross"] {
            let (state, party) = end_of(which);
            let n = state.world.floors.len();
            let variants = [
                flags(0, false, false, false),
                flags(n - 1, false, true, true),
                flags(n / 2, true, true, false),
                flags(n + 5, false, false, false),
            ];
            for &w in &widths {
                for &h in &heights {
                    for f in variants {
                        let lines = render_saga(state, party, f, (w, h), 7);
                        assert_fits(&lines, w, h, &format!("{which} {f:?}"));
                    }
                }
            }
        }
    });
}

/// A copy of `party` in which every floor has seen every cell, so every
/// wall each floor has is drawn.
fn all_seen(party: &PartyMemory) -> PartyMemory {
    let mut all = party.clone();
    for m in &mut all.floors {
        m.see_everything();
    }
    all
}

/// Left and bottom bars only: no row of the party view ends in a
/// right-side border glyph, and each tile's last column is blank on every
/// row of the grid, so no wall glyph ever closes a tile on its right. Every
/// cell of every floor is seen, and each hero stands at its floor's right
/// edge, so every tile's window scrolls to the floor's last column. The
/// 40-chip story has chips without a floor, so its roster row shows on one
/// page and its grid is what `fit` gives with the roster counted. The cross
/// log's end has portals bound to links (`the_cross_scenes_fit_every_size`
/// checks its crossing scenes).
#[test]
fn no_party_row_ends_in_a_right_side_border() {
    use crate::animation::arcade::RIGHT_EDGE_GLYPHS;
    for which in ["busy", "party4", "32", "40", "cross"] {
        let (state, party) = end_of(which);
        let party = all_seen(party);
        let mut right = state.clone();
        for m in 0..right.party.len() {
            let f = member_floor(&right, m).unwrap();
            let floor = &right.world.floors[f];
            let y = floor.h / 2;
            let x = (0..floor.w)
                .rev()
                .find(|&x| floor.cell(x, y).walkable())
                .unwrap_or(1);
            right.party[m].hero.pos = (x, y);
        }
        let n = state.world.floors.len();
        for s in [state, &right] {
            for (w, h) in TABLE_SIZES {
                for f in [
                    flags(0, false, false, false),
                    flags(n - 1, false, true, true),
                ] {
                    let lines = tc(|| render_saga(s, &party, f, (w, h), 3));
                    for (i, line) in lines.iter().enumerate() {
                        if let Some(&(ch, _)) = cells(line).get(w as usize - 1) {
                            assert!(
                                !RIGHT_EDGE_GLYPHS.contains(&ch),
                                "{which} {w}x{h} row {i} ends in {ch:?}"
                            );
                        }
                    }
                    // The grid's rows start after the title, the banner
                    // and the roster.
                    let avail = h as usize - usize::from(f.stopped);
                    let extra = s.chips_seen > s.world.chips.len();
                    let g = if extra {
                        fit(n, w as usize, avail, true)
                    } else {
                        party_grid(n, w as usize, avail)
                    }
                    .expect("a grid at the table's sizes");
                    let roster = extra || g.per_page < n;
                    assert_eq!(roster_of(&lines).is_some(), roster, "{which} {w}x{h}");
                    let first = 1 + usize::from(f.stopped) + usize::from(roster);
                    let on_page = (n - (f.focus / g.per_page) * g.per_page).min(g.per_page);
                    for slot in 0..on_page {
                        let (gx, gy) = (slot % g.cols, slot / g.cols);
                        let edge = gx * (g.tile_w + 1) + g.tile_w - 1;
                        for r in 0..g.tile_h {
                            let row = first + gy * g.tile_h + r;
                            let c = cells(&lines[row]);
                            let ch = c.get(edge).map_or(' ', |&(ch, _)| ch);
                            assert_eq!(
                                ch, ' ',
                                "{which} {w}x{h}: tile {slot}'s last column on row {row}"
                            );
                        }
                    }
                }
            }
        }
    }
}

// ---- The roster, the focus and the zoom ---------------------------------------

/// The roster row of a frame: the line that starts with ` party `.
fn roster_of(lines: &[Line<'static>]) -> Option<(usize, String)> {
    text(lines)
        .into_iter()
        .enumerate()
        .find(|(_, t)| t.starts_with(" party "))
}

/// The roster names every member with one glyph, in index order, and ends
/// with the chips that have no floor. 32 chips over several pages show the
/// roster and the page; 40 chips show `+8 chips without a floor` even when
/// every tile fits on one page.
#[test]
fn the_roster_shows_every_member_and_the_chips_without_a_floor() {
    let hero = Pack::builtin().glyphs.hero;
    tc(|| {
        let (state, party) = end_of("32");
        assert_eq!(state.party.len(), 32);
        let lines = render_saga(state, party, flags(0, false, false, false), (80, 23), 0);
        let (_, roster) = roster_of(&lines).expect("a roster at 80x23");
        assert_eq!(roster.matches(hero).count(), 32, "{roster}");
        assert!(roster.ends_with(" 1/6"), "{roster}");
        let lines = render_saga(state, party, flags(0, false, false, false), (140, 39), 0);
        let (_, roster) = roster_of(&lines).expect("a roster at 140x39");
        assert!(roster.ends_with(" page 1 of 3"), "{roster}");
        assert!(!roster.contains("without"), "{roster}");
        // One page holds them all at 200x59: no roster with no extra chips.
        let lines = render_saga(state, party, flags(0, false, false, false), (200, 59), 0);
        assert!(roster_of(&lines).is_none());

        let (state, party) = end_of("40");
        assert_eq!(state.world.floors.len(), 32, "a world keeps 32 floors");
        for (w, h) in [(140u16, 39u16), (200, 59)] {
            let lines = render_saga(state, party, flags(0, false, false, false), (w, h), 0);
            let (_, roster) = roster_of(&lines).expect("a roster for the chips without a floor");
            assert_eq!(roster.matches(hero).count(), 32, "{roster}");
            assert!(
                roster.contains("+8 chips without a floor"),
                "{w}x{h}: {roster}"
            );
        }
        let lines = render_saga(state, party, flags(0, false, false, false), (80, 23), 0);
        let (_, roster) = roster_of(&lines).expect("a roster at 80x23");
        assert!(roster.contains("+8 chips"), "{roster}");
        // The member's glyphs: the focused one on its hue, the page's
        // bright, the others dim.
        let (state, party) = end_of("32");
        let lines = render_saga(state, party, flags(7, false, false, false), (80, 23), 0);
        let (row, _) = roster_of(&lines).unwrap();
        let glyphs: Vec<Style> = cells(&lines[row])
            .into_iter()
            .filter(|&(c, _)| c == hero)
            .map(|(_, s)| s)
            .collect();
        assert_eq!(glyphs.len(), 32);
        assert!(glyphs[7].bg.is_some(), "the focused member stands out");
        assert!(
            glyphs[6].add_modifier.contains(Modifier::BOLD),
            "on the page"
        );
        assert!(
            !glyphs[0].add_modifier.contains(Modifier::BOLD),
            "off the page"
        );
    });
}

/// The HUD rows' floor tags in a frame, in order of appearance.
fn hud_tags(lines: &[Line<'static>]) -> Vec<String> {
    let mut out = Vec::new();
    for t in text(lines) {
        for part in t.split('▌').skip(1) {
            if let Some(tag) = part.split(' ').next() {
                if tag.starts_with('F') && tag[1..].chars().all(|c| c.is_ascii_digit()) {
                    out.push(tag.to_string());
                }
            }
        }
    }
    out
}

/// `[` and `]` move the focus, and the page follows it: 32 floors at 80x23
/// show six tiles a page, so focus 7 shows floors 7 to 12, and the last
/// floor's page holds floors 31 and 32. A focus past the last floor is the
/// last floor. The focused tile's tag is reversed and the status row
/// follows its hero.
#[test]
fn focus_moves_the_page() {
    let (state, party) = end_of("32");
    tc(|| {
        let tags = |focus| {
            let lines = render_saga(state, party, flags(focus, false, false, false), (80, 23), 0);
            hud_tags(&lines)
        };
        let names = |a: usize, b: usize| (a..=b).map(|i| format!("F{i}")).collect::<Vec<_>>();
        assert_eq!(tags(0), names(1, 6));
        assert_eq!(tags(5), names(1, 6));
        assert_eq!(tags(6), names(7, 12));
        assert_eq!(tags(31), names(31, 32));
        assert_eq!(tags(99), names(31, 32), "past the end is the last floor");
        let lines = render_saga(state, party, flags(8, false, false, false), (80, 23), 0);
        let (_, roster) = roster_of(&lines).unwrap();
        assert!(roster.ends_with(" 2/6"), "{roster}");
        // The status row names the focused hero, and from 140 columns it
        // has room for the party's keys.
        let status = text(&lines).pop().unwrap();
        let name = &state.party[8].hero.name;
        assert!(status.contains(name.as_str()), "{status} names {name}");
        let wide = render_saga(state, party, flags(8, false, false, false), (140, 39), 0);
        let status = text(&wide).pop().unwrap();
        assert!(status.contains(name.as_str()), "{status} names {name}");
        assert!(status.contains("[ ] focus  z zoom"), "{status}");
        // At 80 columns the keys still show, in their short form. They go
        // before the tick, which a long quest can push off the row.
        for focus in [0, 8, 31] {
            let lines = render_saga(state, party, flags(focus, false, false, false), (80, 23), 0);
            let status = text(&lines).pop().unwrap();
            assert!(status.contains("• [ ] z"), "{status}");
        }
        // The focused tile's tag is drawn on its hue.
        let p = &Pack::builtin().palette;
        let hue = rgb(floor_hue(p, 8));
        let tagged = lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.content == "F9" && s.style.bg == Some(hue))
        });
        assert!(tagged, "F9 is reversed");
    });
}

/// `z` draws the focused floor alone, through the single-floor drawing:
/// the title names that floor and its chip, the map follows its hero, and
/// the status row names that hero.
#[test]
fn zoom_draws_the_focused_floor_like_today() {
    let (state, party) = end_of("party4");
    tc(|| {
        for focus in 0..4 {
            for (w, h) in [(80u16, 23u16), (140, 39)] {
                let f = flags(focus, true, false, false);
                let got = render_saga(state, party, f, (w, h), 5);
                let seat = floor_seat(state, focus);
                assert_eq!(
                    seat.member,
                    Some(focus),
                    "member {focus} stands on floor {focus}"
                );
                let want = render_floor(state, party.floor(focus).unwrap(), seat, f, (w, h), 5);
                assert_eq!(frame_hash(&got), frame_hash(&want), "focus {focus} {w}x{h}");
                let lines = text(&got);
                assert!(lines[0].contains(&format!("floor {} of 4", focus + 1)));
                let chip = key_id(state.world.chips[focus].as_str());
                assert!(lines[0].contains(chip), "{}", lines[0]);
                let name = &state.party[focus].hero.name;
                assert!(lines.last().unwrap().contains(name.as_str()));
                // The zoomed map is the floor's own map, centred on its hero.
                let look = Look::for_floor(state, party.floor(focus).unwrap(), focus);
                assert!(hud_tags(&got).is_empty(), "no tiles when zoomed");
                let map = map_rows(
                    state,
                    party.floor(focus).unwrap(),
                    &look,
                    seat,
                    10,
                    3,
                    5,
                    Motion::Full,
                );
                assert!(!map.is_empty());
            }
        }
        // The lead's floor zoomed is the lead's single-floor frame.
        let f = flags(0, true, true, false);
        let got = render_saga(state, party, f, (140, 39), 5);
        let want = render_floor(state, party.lead(state), lead_seat(state), f, (140, 39), 5);
        assert_eq!(frame_hash(&got), frame_hash(&want));
    });
}

/// A world of one floor draws exactly as before the party whatever the
/// focus, the zoom and the chip count say: the solo log's frames are the
/// same for every value.
#[test]
fn one_floor_ignores_focus_and_zoom() {
    let (state, party) = end_of("solo");
    assert_eq!(state.world.floors.len(), 1);
    tc(|| {
        for (w, h) in [(80u16, 23u16), (140, 39), (30, 10)] {
            for why in [false, true] {
                let plain = render_saga(state, party, flags(0, false, why, false), (w, h), 5);
                for focus in [0, 1, 7] {
                    for zoom in [false, true] {
                        let f = flags(focus, zoom, why, false);
                        let got = render_saga(state, party, f, (w, h), 5);
                        assert_eq!(frame_hash(&got), frame_hash(&plain), "{f:?} {w}x{h}");
                    }
                }
                let all = text(&plain).join("\n");
                assert!(!all.contains("party of"), "{all}");
                assert!(!all.contains("[ ] focus"), "{all}");
            }
        }
    });
}

// ---- The chronicle ---------------------------------------------------------------

/// In a party, each member's line carries its floor's tag after its tone
/// mark, in the floor's hue, and its "why" line names the floor's chip; a
/// world line (no floor) has no tag. With one floor no line has a tag.
#[test]
fn member_lines_carry_their_floor_tag_and_world_lines_none() {
    let (state, _) = end_of("party4");
    let look = Look::new(state.world.biome, None);
    let why = flags(0, false, true, false);
    let p = &Pack::builtin().palette;
    let beats: Vec<&Beat> = state.chronicle.lines(500).collect();
    let mut tagged = 0;
    let mut untagged = 0;
    tc(|| {
        for beat in &beats {
            let tag = floor_tag(state, beat);
            let rows: Vec<String> = beat_rows(beat, "", 0, why, &look, 200, 198, tag.as_ref())
                .into_iter()
                .map(|r| text(&[r.line()]).remove(0))
                .collect();
            let first = rows[0].clone();
            let mark = tone_mark(beat.tone);
            match beat.floor {
                Some(f) => {
                    let want = format!("t{} {mark} F{} ", beat.tick, f + 1);
                    assert!(first.starts_with(&format!("{BAR}{want}")), "{first}");
                    let t = tag.as_ref().unwrap();
                    assert_eq!(t.style.fg, Some(rgb(floor_hue(p, f))));
                    let why_line = rows.last().unwrap();
                    if beat.tick >= state.world.born_tick {
                        let chip = key_id(state.world.chips[f as usize].as_str());
                        assert!(why_line.contains(&format!("F{} is chip {chip}", f + 1)));
                    }
                    tagged += 1;
                }
                None => {
                    assert!(tag.is_none());
                    let want = format!("{BAR}t{} {mark} ", beat.tick);
                    assert!(first.starts_with(&want), "{first}");
                    let after = &first[want.len()..];
                    assert!(
                        !(after.starts_with('F')
                            && after[1..].starts_with(|c: char| c.is_ascii_digit())),
                        "a world line has no tag: {first}"
                    );
                    untagged += 1;
                }
            }
        }
    });
    assert!(
        tagged > 10 && untagged > 0,
        "{tagged} tagged, {untagged} world lines"
    );
    // The frames: tags in the party view and in zoom, none for one floor.
    tc(|| {
        let (state, party) = end_of("party4");
        for zoom in [false, true] {
            let all = text(&render_saga(
                state,
                party,
                flags(1, zoom, false, false),
                (140, 39),
                0,
            ))
            .join("\n");
            assert!(
                (1..=4).any(|f| all.contains(&format!(" F{f} "))),
                "zoom {zoom}: tags shown"
            );
        }
        let (solo, party) = end_of("solo");
        let all = text(&render_saga(
            solo,
            party,
            flags(0, false, true, false),
            (140, 39),
            0,
        ))
        .join("\n");
        assert!(!all.contains(" F1 "), "one floor has no tags: {all}");
    });
}

// ---- Names -------------------------------------------------------------------

/// The fixtures' real names: the generator's models and the busy log's.
const REAL_NAMES: [&str; 3] = ["demo-sage", "demo-scribe", "demo-run"];

/// In generated mode no tile HUD, roster, title, chronicle line or status
/// row of the party view names a real model, for 4 and 32 chips at every
/// size of the table, every focus page, and "why" off and on. Real mode is
/// the control: its frames do name one, so the check can see a leak.
#[test]
fn no_tile_hud_or_roster_shows_a_fixture_name() {
    let mut seen_real = false;
    for log in [busy(), party4(), log32()] {
        for names in [NameMode::Generated, NameMode::Real] {
            let (saga, party) = run(log, names);
            let state = saga.state();
            let n = state.world.floors.len();
            tc(|| {
                for (w, h) in TABLE_SIZES {
                    for focus in (0..n).step_by(5) {
                        for why in [false, true] {
                            let f = flags(focus, false, why, false);
                            let all = text(&render_saga(state, &party, f, (w, h), 0)).join("\n");
                            let leak = REAL_NAMES.iter().find(|r| all.contains(*r));
                            match names {
                                NameMode::Generated => {
                                    assert!(leak.is_none(), "{n} chips {w}x{h} shows {leak:?}")
                                }
                                NameMode::Real => seen_real |= leak.is_some(),
                            }
                        }
                    }
                }
                // Every HUD of every floor, through the tile itself.
                for floor in 0..n {
                    let memory = party.floor(floor).unwrap();
                    let look = Look::for_floor(state, memory, floor);
                    let hud = tile_hud(state, &look, floor_seat(state, floor), 200, false);
                    let t = text(&[hud.line()]).remove(0);
                    if names == NameMode::Generated {
                        assert!(!REAL_NAMES.iter().any(|r| t.contains(r)), "{t}");
                    } else {
                        seen_real |= REAL_NAMES.iter().any(|r| t.contains(r));
                    }
                }
            });
        }
    }
    assert!(
        seen_real,
        "real mode names a model somewhere, so a leak would show"
    );
}

/// Each tile's HUD shows its own member's quest label from the core.
#[test]
fn each_tile_shows_its_own_members_quest_label() {
    let (state, party) = end_of("party4");
    let mut state = state.clone();
    for (i, m) in state.party.iter_mut().enumerate() {
        m.quest.verb = tt_toplike_saga::quest::Verb::VisitSage;
        m.quest.target_label = Some(format!("Label-{i}"));
    }
    let lines = tc(|| render_saga(&state, party, flags(0, false, false, false), (200, 59), 0));
    let all = text(&lines).join("\n");
    for i in 0..4 {
        assert!(all.contains(&format!("visit Label-{i}")), "{all}");
    }
}

// ---- The core never sees the view ------------------------------------------------

/// The story does not depend on the terminal or the focus: a saga fed the
/// four-chip log while its states are drawn every 50 records, at sizes and
/// focus values that change each time, ends with the same digest as a saga
/// fed the same records with no drawing.
#[test]
fn the_core_never_reads_view_state() {
    let sizes = [(80u16, 23u16), (500, 200), (140, 39), (30, 8), (200, 59)];
    let mut drawn = Saga::new(config(NameMode::Generated));
    let mut plain = Saga::new(config(NameMode::Generated));
    let mut party = PartyMemory::new();
    let mut draws = 0;
    tc(|| {
        for (i, r) in party4().iter().enumerate() {
            drawn.feed(r);
            plain.feed(r);
            party.observe(drawn.state());
            if i % 50 == 0 {
                let k = i / 50;
                let f = flags(k % 5, k % 3 == 0, k % 2 == 0, false);
                let lines = render_saga(drawn.state(), &party, f, sizes[k % sizes.len()], k as u64);
                std::hint::black_box(lines);
                draws += 1;
            }
        }
    });
    assert!(draws > 10, "{draws} draws");
    assert_eq!(drawn.digest(), plain.digest());
}

// ---- Golden frames ---------------------------------------------------------------

/// `records` up to the birth of the log's last world: both logs end a few
/// records after a birth, when every hero still stands at its gate, so the
/// frames are taken at the end of the world before, after its heroes have
/// walked and lit their floors for 300 ticks.
fn before_last_world(records: &[Record]) -> &[Record] {
    let mut saga = Saga::new(config(NameMode::Real));
    let (mut cut, mut world) = (0, 0);
    for (n, r) in records.iter().enumerate() {
        saga.feed(r);
        if saga.state().world.id != world {
            world = saga.state().world.id;
            cut = n;
        }
    }
    &records[..cut]
}

/// The party view of `party4.jsonl` and of the 32-chip log at 80x23 and
/// 140x39, "why" off and on, focus on the second floor, with a memory that
/// observed the state after every record: eight frames.
fn party_frames() -> Vec<u64> {
    tc(|| {
        let mut out = Vec::new();
        for log in [party4(), log32()] {
            let (saga, party) = run(before_last_world(log), NameMode::Real);
            assert!(saga.state().world.floors.len() >= 4);
            for why in [false, true] {
                for size in [(80u16, 23u16), (140, 39)] {
                    let f = flags(1, false, why, false);
                    out.push(frame_hash(&render_saga(saga.state(), &party, f, size, 5)));
                }
            }
        }
        out
    })
}

/// The hashes of [`party_frames`], pinned in Task A.7 of the party plan
/// when the party view was first drawn. A change to the tiles, the HUD,
/// the roster, the grid's shape or the chronicle's tags moves them; re-pin
/// on purpose with `print_party_frames` and say why in the commit.
///
/// Re-pinned in fix round 1 of Task A.7. The status row puts the party's
/// keys right after the quest (in the short form `[ ] z` at 80 columns),
/// then the tick, then the mapped share; and only the focused tile marks a
/// reached goal with `✓`. Every frame's status row moved, so all eight
/// frames moved.
///
/// Re-pinned in Task A.9 (the party view's terminal traffic). All eight
/// frames moved, for two reasons. The tiles: the unfocused tiles are drawn
/// calm (one colour and one light level for what they have seen, no glow
/// ring, a still goal, the held picture of their floor), the focused tile's
/// light and trail come in three shades, and every tile's window centres on
/// its floor's anchor (the dead zone). The chronicle: the party view shows
/// the beats up to the last multiple of 20 ticks. With the batching undone
/// the frames still differ from the old pins, so both changes move them.
///
/// Re-pinned in fix round 1 of Task B.5: an unfocused tile's held picture
/// is taken every 5 ticks (`CALM_EVERY`, was 3), because single `--mock 32`
/// runs with heroes crossing sent up to 22 KB/s against the 20 KB/s budget.
/// Every frame has unfocused tiles whose held hero now stands elsewhere, so
/// all eight moved. These logs have no peers, so the other change of that
/// round (an empty floor keeps its window) moves none of them.
const PARTY_GOLDEN: [u64; 8] = [
    3934237442390221048,
    8885479378402613832,
    17416489920458339929,
    8826553909904938266,
    1075205828068214293,
    756655480256273938,
    16015793166138571427,
    15330896973939589821,
];

/// Prints the hashes for [`PARTY_GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui print_party_frames -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn print_party_frames() {
    println!("{:?}", party_frames());
}

#[test]
fn party_frames_are_pinned() {
    assert_eq!(party_frames(), PARTY_GOLDEN.to_vec());
}

/// The roster's `+N chips without a floor` counts the chips of the stream
/// the story reads (`SagaState::chips_seen`). Under a replay or an attached
/// bus the local backend is another box, and nothing of it reaches the view
/// (the flags carry no chip count, and `run_app` builds them from the focus,
/// the zoom and "why" alone: `view_state_never_reaches_the_saga_driver` in
/// `saga_view_tests.rs`). So the four-chip story shows no chips without a
/// floor, the forty-chip story shows its eight, and a story whose stream
/// held two chips more than its world kept shows those two.
///
/// Seen red on the code before this fix, which took the count from the
/// local backend's devices: the four-chip story drawn with a local count of
/// 8 printed `+4 chips without a floor` and a roster row.
#[test]
fn the_roster_counts_the_storys_chips_not_the_local_boxs() {
    tc(|| {
        let (state, party) = end_of("party4");
        assert_eq!(state.chips_seen, 4, "the stream held four chips");
        let lines = render_saga(state, party, flags(0, false, false, false), (140, 39), 0);
        assert!(roster_of(&lines).is_none(), "no roster on one page");
        assert!(!text(&lines).join("\n").contains("without a floor"));

        let mut more = state.clone();
        more.chips_seen = 6;
        let lines = render_saga(&more, party, flags(0, false, false, false), (140, 39), 0);
        let (_, roster) = roster_of(&lines).expect("a roster for the chips without a floor");
        assert!(roster.contains("+2 chips without a floor"), "{roster}");

        let (state, party) = end_of("40");
        assert_eq!((state.chips_seen, state.world.chips.len()), (40, 32));
        let lines = render_saga(state, party, flags(0, false, false, false), (140, 39), 0);
        let (_, roster) = roster_of(&lines).expect("a roster");
        assert!(roster.contains("+8 chips without a floor"), "{roster}");
    });
}

/// A stopped driver in the party view: the banner sits under the title and
/// the grid still draws every tile of the page under it.
#[test]
fn the_party_view_shows_the_stopped_banner_over_the_grid() {
    let (state, party) = end_of("party4");
    tc(|| {
        for (w, h) in TABLE_SIZES {
            let lines = render_saga(state, party, flags(0, false, false, true), (w, h), 0);
            assert_fits(&lines, w, h, "stopped");
            let t = text(&lines);
            assert!(t[0].contains("party of 4"), "{}", t[0]);
            assert!(t[1].contains("the story has stopped"), "{w}x{h}: {}", t[1]);
            assert_eq!(hud_tags(&lines), ["F1", "F2", "F3", "F4"], "{w}x{h}");
        }
    });
}

/// Only the focused tile marks a reached goal with `✓`, so a grid of many
/// tiles is not a wall of marks.
#[test]
fn only_the_focused_tile_marks_a_reached_goal() {
    let (state, party) = end_of("party4");
    let mut done = state.clone();
    for m in done.party.iter_mut() {
        m.quest.verb = tt_toplike_saga::quest::Verb::Explore;
        m.quest.goal = m.hero.pos;
    }
    for focus in 0..4 {
        let lines = tc(|| {
            render_saga(
                &done,
                party,
                flags(focus, false, false, false),
                (140, 39),
                0,
            )
        });
        let huds: Vec<String> = text(&lines)
            .into_iter()
            .filter(|t| t.starts_with('▌') && !t.contains("SAGA"))
            .collect();
        let marked: Vec<bool> = huds
            .iter()
            .flat_map(|t| {
                t.split('▌')
                    .skip(1)
                    .map(|h| h.contains('✓'))
                    .collect::<Vec<_>>()
            })
            .collect();
        let want: Vec<bool> = (0..4).map(|i| i == focus).collect();
        assert_eq!(marked, want, "focus {focus}: {huds:?}");
    }
}

// ---- Terminal traffic ------------------------------------------------------------

/// The 32-chip log's state at each tick, with the party memory that
/// observed every record up to it: one entry per tick, taken right after
/// the first record of the tick. The TUI advances the story one tick and
/// draws one frame per update (`--interval`, 100 ms by default), so
/// consecutive entries are consecutive frames.
fn ticks_of(records: &[Record]) -> Vec<(SagaState, PartyMemory)> {
    let mut saga = Saga::new(config(NameMode::Real));
    let mut party = PartyMemory::new();
    let mut out = Vec::new();
    let mut last = u64::MAX;
    for r in records {
        saga.feed(r);
        party.observe(saga.state());
        if saga.state().tick != last {
            last = saga.state().tick;
            out.push((saga.state().clone(), party.clone()));
        }
    }
    out
}

/// One frame drawn into a buffer of `w` x `h`, as `ui_saga` draws it.
fn buffer_of(lines: Vec<Line<'static>>, w: u16, h: u16) -> ratatui::buffer::Buffer {
    use ratatui::widgets::{Paragraph, Widget};
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    Paragraph::new(lines)
        .style(Style::default().bg(colors::canvas_bg()))
        .render(area, &mut buf);
    buf
}

/// The bytes a terminal is sent for `cells` (changed cells from
/// `Buffer::diff`): crossterm's own writer, as the TUI's backend writes a
/// frame (cursor moves, colour and modifier changes, the characters).
fn bytes_of<'a>(cells: impl Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>) -> usize {
    use ratatui::backend::{Backend, CrosstermBackend};
    let mut out: Vec<u8> = Vec::new();
    {
        let mut backend = CrosstermBackend::new(&mut out);
        backend.draw(cells).expect("a Vec takes every byte");
    }
    out.len()
}

/// What made a changed cell change, for [`traffic_by_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Title,
    Roster,
    Hud,
    /// The tile's window moved, so every cell of the tile's map may move.
    Scroll,
    /// Any other change on the focused tile's map, which animates fully.
    Focused,
    /// The hero's cell and the glow ring around it.
    HeroGlow,
    /// The goal beacon or the sage at the goal.
    Goal,
    Trail,
    /// The light's falloff moved with the hero (the window did not).
    Light,
    /// A cell seen for the first time.
    Fog,
    /// Anything else on a map: the portals' shimmer, the heat tint.
    OtherMap,
    Chronicle,
    Status,
}

/// The changed cells of each frame from `from` to `to` (tick indexes into
/// [`ticks_of`] of the 32-chip log) at `w` x `h`, focus on floor 2, sorted
/// by source: (source, changed cells, estimated bytes), summed over the
/// frames. Each source's bytes are those crossterm writes for its cells
/// alone, so the sum is close to the frame's total but not equal to it.
fn traffic_by_source(
    records: &[Record],
    from: usize,
    to: usize,
    w: u16,
    h: u16,
) -> Vec<(Source, usize, usize)> {
    use std::collections::BTreeMap;
    let ticks = ticks_of(records);
    let to = to.min(ticks.len());
    let f = flags(1, false, false, false);
    let mut sums: BTreeMap<Source, (usize, usize)> = BTreeMap::new();
    tc(|| {
        let mut prev: Option<(usize, ratatui::buffer::Buffer)> = None;
        for i in from..to {
            let (state, party) = &ticks[i];
            let buf = buffer_of(render_saga(state, party, f, (w, h), i as u64), w, h);
            if let Some((p, pbuf)) = &prev {
                let diff = pbuf.diff(&buf);
                let mut groups: BTreeMap<Source, Vec<(u16, u16, &ratatui::buffer::Cell)>> =
                    BTreeMap::new();
                for (x, y, c) in diff {
                    let s = source_of(&ticks[*p], &ticks[i], f, (w, h), x as usize, y as usize);
                    groups.entry(s).or_default().push((x, y, c));
                }
                for (s, cells) in groups {
                    let e = sums.entry(s).or_default();
                    e.0 += cells.len();
                    e.1 += bytes_of(cells.into_iter());
                }
            }
            prev = Some((i, buf));
        }
    });
    sums.into_iter().map(|(s, (c, b))| (s, c, b)).collect()
}

/// The source of a change at `(x, y)` between two consecutive states (see
/// [`Source`]), from the grid's geometry and the two states' heroes, goals,
/// trails, light and memory.
fn source_of(
    a: &(SagaState, PartyMemory),
    b: &(SagaState, PartyMemory),
    f: SagaFlags,
    (w, h): (u16, u16),
    x: usize,
    y: usize,
) -> Source {
    let (w, h) = (w as usize, h as usize);
    let state = &b.0;
    let n = state.world.floors.len();
    let Some(grid) = party_grid(n, w, h) else {
        return Source::OtherMap;
    };
    let roster = grid.per_page < n;
    let top = 1 + usize::from(roster);
    if y == 0 {
        return Source::Title;
    }
    if roster && y == 1 {
        return Source::Roster;
    }
    if y == h - 1 {
        return Source::Status;
    }
    if x > grid.width() || y >= top + grid.height() {
        return Source::Chronicle;
    }
    let (gx, col) = (x / (grid.tile_w + 1), x % (grid.tile_w + 1));
    let (gy, row) = ((y - top) / grid.tile_h, (y - top) % grid.tile_h);
    let floor = (f.focus / grid.per_page) * grid.per_page + gy * grid.cols + gx;
    if row == 0 {
        return Source::Hud;
    }
    if floor >= n || col >= grid.tile_w {
        return Source::OtherMap;
    }
    let fl = &state.world.floors[floor];
    let (fw, fh) = (fl.w as usize, fl.h as usize);
    let focused = floor == f.focus;
    let win = |(s, p): &(SagaState, PartyMemory)| {
        let (_, seat) = tile_view(s, p.floor(floor).unwrap(), floor, focused);
        let hero = seat
            .member
            .map(|m| seat.pose.map_or(s.party[m].hero.pos, |p| p.hero));
        map_window(fl, seat, hero, grid.tile_w, grid.tile_h - 1)
    };
    let (x0, pad, y0) = win(b);
    if a.0.world.id != state.world.id || win(a) != (x0, pad, y0) {
        return Source::Scroll;
    }
    if floor == f.focus {
        return Source::Focused;
    }
    if col < pad {
        return Source::OtherMap;
    }
    let (fx, fy) = (x0 + col - pad, y0 + row - 1);
    if fx >= fw || fy >= fh {
        return Source::OtherMap;
    }
    let at = (fx as u8, fy as u8);
    // The hero's and the goal's cells as each frame drew them, and the
    // memory each frame drew from.
    let drawn = |(s, p): &(SagaState, PartyMemory)| {
        let (m, seat) = tile_view(s, p.floor(floor).unwrap(), floor, focused);
        let pose = seat.member.map(|i| {
            let live = &s.party[i];
            seat.pose
                .map_or((live.hero.pos, live.quest.goal), |p| (p.hero, p.goal))
        });
        (m.clone(), pose)
    };
    let ((ma, pa), (mb, pb)) = (drawn(a), drawn(b));
    let near = |p: Option<((u8, u8), (u8, u8))>| {
        p.is_some_and(|(h, _)| fx.abs_diff(h.0 as usize) <= 1 && fy.abs_diff(h.1 as usize) <= 1)
    };
    if near(pa) || near(pb) {
        return Source::HeroGlow;
    }
    if pa.is_some_and(|(_, g)| g == at) || pb.is_some_and(|(_, g)| g == at) {
        return Source::Goal;
    }
    if ma.trail_age(at) != mb.trail_age(at) {
        return Source::Trail;
    }
    let lit = |p: Option<((u8, u8), (u8, u8))>| p.map_or(0, |(h, _)| light(fl, h)[fy * fw + fx]);
    if lit(pa) != lit(pb) {
        return Source::Light;
    }
    if ma.seen(fx, fy) != mb.seen(fx, fy) {
        return Source::Fog;
    }
    Source::OtherMap
}

/// Prints the changed cells and estimated bytes per frame by source, for
/// the 32-chip log at 140x39 and 80x23 over 100 consecutive frames of one
/// world. Run with
/// `cargo test --locked --lib --features tui print_party_traffic -- --ignored --nocapture`.
///
/// With `SAGA_TRAFFIC_LOG` set to a bus log file (one `bus-N.jsonl`, for
/// example from `tt-toplike-tui --mock 32 --mode saga --record DIR`), it
/// reads that log and prints ticks 170 to 340 (about 20 s to 40 s of a run).
#[test]
#[ignore = "prints the traffic per source"]
fn print_party_traffic() {
    let recorded = std::env::var_os("SAGA_TRAFFIC_LOG").map(|p| {
        let read = read_segment(std::path::Path::new(&p)).expect("a bus log");
        read.records
    });
    let (records, from, to) = match &recorded {
        Some(r) => (r.as_slice(), 170, 340),
        None => (log32(), 150, 250),
    };
    for (w, h) in [(140u16, 39u16), (80, 23)] {
        let to = to.min(ticks_of(records).len());
        let frames = (to - from - 1) as f64;
        let rows = traffic_by_source(records, from, to, w, h);
        let total: usize = rows.iter().map(|r| r.2).sum();
        println!(
            "{w}x{h}, ticks {from}..{to}: {:.0} bytes/frame",
            total as f64 / frames
        );
        let mut rows = rows;
        rows.sort_by_key(|r| std::cmp::Reverse(r.2));
        for (s, c, b) in rows {
            println!(
                "  {s:?}: {:.1} cells/frame, {:.0} bytes/frame",
                c as f64 / frames,
                b as f64 / frames
            );
        }
    }
}

/// The mean bytes per frame the terminal is sent for the party view of
/// `records` from tick index `from` to `to` at `w` x `h`, focus on floor 2:
/// each frame's `Buffer::diff` from the frame before, written by crossterm
/// ([`bytes_of`]).
fn party_bytes_per_frame(records: &[Record], from: usize, to: usize, w: u16, h: u16) -> usize {
    let ticks = ticks_of(records);
    let f = flags(1, false, false, false);
    tc(|| {
        let mut total = 0;
        let mut prev: Option<ratatui::buffer::Buffer> = None;
        for (i, (state, party)) in ticks.iter().enumerate().take(to).skip(from) {
            let buf = buffer_of(render_saga(state, party, f, (w, h), i as u64), w, h);
            if let Some(p) = &prev {
                total += bytes_of(p.diff(&buf).into_iter());
            }
            prev = Some(buf);
        }
        total / (to - from - 1)
    })
}

/// The party view's budget for a quiet world is 20 KB/s at 140 x 40 (the
/// party plan, spec 9). The TUI draws Saga ten times a second, so that is
/// 2,000 bytes a frame. The bound here is 1,800: the frames below lack the
/// app's own status bar and the chips' heat, which `tools/party_traffic.py`
/// measured at about 2 KB/s together on a real `--mock 32` run.
const PARTY_BYTES_PER_FRAME: usize = 1_800;

/// A hundred consecutive frames of the 32-chip log at 140x39 (a 140x40
/// terminal less the status bar) stay inside the budget: 1,654 bytes a
/// frame when this was written (Task A.9 of the party plan).
///
/// Seen red three ways, each restored: with every tile's window centred on
/// its hero at every step (`tile_seat` ignoring the anchor), 4,338 bytes;
/// with every tile animated as the focused one (`Motion::Banded` for all),
/// 4,278; with the unfocused tiles drawn from the live state at every tick
/// (no held picture), 2,071. The code before Task A.9 sent 14,110.
///
/// The second case (Task B.5's fix round) is the same 32 chips in a ring of
/// links with a model ready on every chip (`records_with_ring_model`), so
/// every floor offers crossings: 10 to 24 crossings per 100 ticks, with
/// tiles emptying and filling. With `CALM_EVERY` 5 the two cases send
/// 1,469 and 1,242 bytes a frame (1,603 and 1,364 with 3). This case does
/// not go red when an empty floor's window re-centres again (1,260): in
/// this model the window moves of a crossing cost little (18 bytes a
/// frame), and the bound is the budget's. It guards that crossings at 32
/// chips keep the view near the quiet log's traffic.
#[test]
fn the_party_view_stays_inside_its_traffic_budget() {
    for (what, log) in [("no links", log32()), ("ring", log32_ring())] {
        let per_frame = party_bytes_per_frame(log, 150, 250, 140, 39);
        assert!(
            per_frame <= PARTY_BYTES_PER_FRAME,
            "{what}: {per_frame} bytes a frame, over {PARTY_BYTES_PER_FRAME}"
        );
        // The bound is not met by an empty screen: the frames do change.
        assert!(per_frame > 500, "{what}: {per_frame} bytes a frame");
    }
}

/// Prints the bytes a frame of the traffic guard's two logs, and the ring
/// log's crossings per 100 ticks. Run with
/// `cargo test --locked --lib --features tui print_traffic_guard -- --ignored --nocapture`.
#[test]
#[ignore = "prints the guard's numbers"]
fn print_traffic_guard() {
    for (what, log) in [("no links", log32()), ("ring", log32_ring())] {
        for (from, to) in [(150, 250), (250, 350), (350, 450)] {
            println!(
                "{what} {from}..{to}: {} bytes a frame",
                party_bytes_per_frame(log, from, to, 140, 39)
            );
        }
    }
    let mut saga = Saga::new(config(NameMode::Real));
    let mut per_window = std::collections::BTreeMap::new();
    for r in log32_ring() {
        saga.feed(r);
        for e in saga.drain_events() {
            if matches!(e, tt_toplike_saga::SagaEvent::HeroCrossed { .. }) {
                *per_window.entry(saga.state().tick / 100).or_insert(0) += 1;
            }
        }
    }
    println!("crossings per 100 ticks: {per_window:?}");
}

/// Every tile on the page shows its hero, though a tile's window moves only
/// when the hero leaves its dead zone: at each of 100 consecutive frames of
/// the 32-chip log and at the three sizes of the addendum's table, the grid
/// holds one `@` per tile shown.
///
/// The zone fits the smallest tile, so `map_window`'s fallback (centre on
/// the hero when it would be off the window) is not needed here. Seen red
/// with the zone made taller than the smallest window (4 rows above and
/// below) and the fallback removed: five heroes on a page of six at 80x23.
#[test]
fn every_tile_keeps_its_hero_in_its_window() {
    let ticks = ticks_of(log32());
    let hero = Pack::builtin().glyphs.hero;
    tc(|| {
        for (w, h) in TABLE_SIZES {
            let grid = party_grid(32, w as usize, h as usize).expect("a grid");
            for (i, (state, party)) in ticks.iter().enumerate().take(250).skip(150) {
                let f = flags(1, false, false, false);
                let lines = text(&render_saga(state, party, f, (w, h), i as u64));
                // The grid's rows: after the title and the roster row.
                let tiles: usize = lines[2..2 + grid.height()]
                    .iter()
                    .map(|l| {
                        let grid_part: String = l.chars().take(grid.width()).collect();
                        grid_part.chars().filter(|&c| c == hero).count()
                    })
                    .sum();
                assert_eq!(tiles, grid.per_page, "{w}x{h} tick index {i}");
            }
        }
    });
}

/// The dead zone (`follow`): the anchor holds while the hero stays within
/// the zone, and moves so the hero stands at the zone's other end when it
/// steps out, clamped to the floor.
#[test]
fn a_tiles_anchor_moves_only_when_the_hero_leaves_the_zone() {
    let (bx, ax) = ANCHOR_ZONE_X;
    let (by, ay) = ANCHOR_ZONE_Y;
    let size = (60, 40);
    assert_eq!(follow(None, (20, 20), size), (20, 20));
    // Inside the zone, at each of its ends: no move.
    for hero in [(20 + ax, 20), (20 - bx, 20), (20, 20 + ay), (20, 20 - by)] {
        assert_eq!(follow(Some((20, 20)), hero, size), (20, 20), "{hero:?}");
    }
    // One step past the far end: the hero is now at the near end.
    assert_eq!(
        follow(Some((20, 20)), (21 + ax, 20), size),
        (21 + ax + bx, 20)
    );
    assert_eq!(
        follow(Some((20, 20)), (20, 21 + ay), size),
        (20, 21 + ay + by)
    );
    // One step before the near end: the hero is now at the far end.
    assert_eq!(
        follow(Some((20, 20)), (19 - bx, 20), size),
        (19 - bx - ax, 20)
    );
    assert_eq!(
        follow(Some((20, 20)), (20, 19 - by), size),
        (20, 19 - by - ay)
    );
    // Clamped to the floor.
    assert_eq!(follow(Some((45, 20)), (59, 20), size).0, 59);
    assert_eq!(follow(Some((10, 20)), (0, 20), size).0, 0);
}

/// A tile's heat tint holds until the chip's reading moves by
/// `TILE_HEAT_STEP` or more; the single-floor view's heat follows every
/// reading.
#[test]
fn a_tiles_heat_holds_through_small_changes() {
    let mut m = SagaMemory::new();
    m.set_heat(Some(60.0));
    assert_eq!((m.heat, m.tile_heat), (Some(60.0), Some(60.0)));
    m.set_heat(Some(60.0 + TILE_HEAT_STEP - 0.5));
    assert_eq!((m.heat, m.tile_heat), (Some(62.5), Some(60.0)));
    m.set_heat(Some(60.0 - TILE_HEAT_STEP + 0.5));
    assert_eq!(m.tile_heat, Some(60.0));
    m.set_heat(Some(60.0 + TILE_HEAT_STEP));
    assert_eq!(m.tile_heat, Some(63.0));
    m.set_heat(None);
    assert_eq!(m.tile_heat, None);
}

/// An unfocused tile draws its floor's held picture, taken at the start of
/// each period of `CALM_EVERY` ticks with the period offset by the floor's
/// index; the focused tile draws the floor as it is now.
#[test]
fn an_unfocused_tile_draws_the_held_picture() {
    let ticks = ticks_of(log32());
    let (mut held_differs, mut checked) = (0, 0);
    for (state, party) in ticks.iter().take(250).skip(150) {
        for floor in 0..4 {
            let memory = party.floor(floor).expect("a floor");
            let (held, pose) = memory.held().expect("a picture");
            let period = (state.tick + floor as u64) / CALM_EVERY;
            assert_eq!(memory.held.as_ref().unwrap().period, period);
            let (m, seat) = tile_view(state, memory, floor, false);
            assert!(std::ptr::eq(m, held));
            assert_eq!(seat.pose, pose);
            let (m, seat) = tile_view(state, memory, floor, true);
            assert!(std::ptr::eq(m, memory));
            assert_eq!(seat.pose, None);
            let live = state.party[floor_seat(state, floor).member.unwrap()]
                .hero
                .pos;
            checked += 1;
            if pose.map(|p| p.hero) != Some(live) {
                held_differs += 1;
            }
        }
    }
    // The held hero is behind the live one at some ticks and level with it
    // at others.
    assert!(
        held_differs > 0 && held_differs < checked,
        "{held_differs} of {checked}"
    );
}

// ---- Crossing (Task B.5) ---------------------------------------------------------
//
// `cross4.jsonl` is four chips in a ring of links (the saga crate's
// generator, `records_with_links`): floor 1's port 1 to floor 2's port 0,
// floor 2's port 2 to floor 3's port 0, floor 3's port 3 to floor 4's port
// 0, floor 4's port 4 to floor 1's port 0 (floors counted from 1, as the
// view's tags count them). Link 1 (floor 2's port 2) is down from tick 400
// to 429, and floor 1's two ring ports go dark for good at tick 600. Its
// first crossing is member 1, from floor 2 to floor 1, at tick 202. It has
// no failed crossing and no dark portal (ruling R42 keeps the fixture as it
// is), so those scenes inject a link going down into the log, as the saga
// crate's own tests do.

fn cross4() -> &'static [Record] {
    static R: OnceLock<Vec<Record>> = OnceLock::new();
    R.get_or_init(|| fixture("cross4.jsonl"))
}

/// A crossing as `HeroCrossed` reports it, with floors and member as
/// indices.
#[derive(Debug, Clone, Copy)]
struct Crossing {
    member: usize,
    from: usize,
    from_port: u8,
    to: usize,
    to_port: u8,
}

/// A saga and a party memory that observes after each record, as the TUI
/// does, over `cross4.jsonl` and any records a scene adds.
struct Scene {
    saga: Saga,
    party: PartyMemory,
    /// The next record of `cross4.jsonl` to feed.
    next: usize,
}

impl Scene {
    fn new() -> Scene {
        Scene {
            saga: Saga::new(config(NameMode::Real)),
            party: PartyMemory::new(),
            next: 0,
        }
    }

    fn feed(&mut self, r: &Record) -> Vec<tt_toplike_saga::SagaEvent> {
        self.saga.feed(r);
        self.party.observe(self.saga.state());
        self.saga.drain_events()
    }

    /// Feeds the log until `stop` is true of the saga and the events of
    /// the record just fed.
    fn until(&mut self, mut stop: impl FnMut(&Saga, &[tt_toplike_saga::SagaEvent]) -> bool) {
        while let Some(r) = cross4().get(self.next) {
            self.next += 1;
            let events = self.feed(r);
            if stop(&self.saga, &events) {
                return;
            }
        }
        panic!("cross4.jsonl ended before the scene");
    }

    /// Feeds the log up to its next `HeroCrossed`.
    fn until_crossing(&mut self) -> Crossing {
        let mut found = None;
        self.until(|_, events| {
            found = events.iter().find_map(|e| match *e {
                tt_toplike_saga::SagaEvent::HeroCrossed {
                    member,
                    from_floor,
                    from_port,
                    to_floor,
                    to_port,
                    ..
                } => Some(Crossing {
                    member: member as usize,
                    from: from_floor as usize,
                    from_port,
                    to: to_floor as usize,
                    to_port,
                }),
                _ => None,
            });
            found.is_some()
        });
        found.unwrap()
    }

    /// Takes port `port` of floor `floor`'s chip down, as the bus source
    /// writes it: the chip's live-mask sample without the port's bit, then
    /// a `LinkChanged`. The saga reads neither record's number nor time.
    fn link_down(&mut self, floor: usize, port: u8) {
        use tt_toplike_bus::{Event, Payload, Provenance, Sample, Value};
        let chip = self.saga.state().world.chips[floor].clone();
        let mask = self.saga.links().mask(&chip).expect("the chip has a mask") & !(1u64 << port);
        let record = |payload| Record {
            seq: 0,
            t_mono_ns: 0,
            t_wall_ms: 0,
            source: "backend".to_string(),
            payload,
        };
        self.feed(&record(Payload::Sample(Sample {
            entity: chip.clone(),
            signal: "toplike.eth.live_mask".to_string(),
            value: Value::U64(mask),
            prov: Provenance::Estimated,
        })));
        self.feed(&record(Payload::Event(Event::LinkChanged {
            chip,
            mask: mask as u16,
        })));
    }

    fn state(&self) -> &SagaState {
        self.saga.state()
    }
}

/// The log fed up to its first crossing: member 1 is in transit from floor
/// 2 to floor 1.
fn transit_scene() -> (Scene, Crossing) {
    let mut s = Scene::new();
    let c = s.until_crossing();
    (s, c)
}

/// The first crossing fails: the far end goes down while the hero is in
/// transit, and the log is fed to the `CrossingFailed`, which comes on the
/// hero's next tick.
fn failure_scene() -> (Scene, Crossing) {
    let (mut s, c) = transit_scene();
    s.link_down(c.to, c.to_port);
    s.until(|_, events| {
        events
            .iter()
            .any(|e| matches!(e, tt_toplike_saga::SagaEvent::CrossingFailed { .. }))
    });
    (s, c)
}

/// The first crossing arrives: the log is fed past its transit.
fn arrival_scene() -> (Scene, Crossing) {
    let (mut s, c) = transit_scene();
    s.until(|saga, _| saga.state().party[c.member].hero.transit.is_none());
    (s, c)
}

/// A portal goes dark under a hero walking to it: the log is fed until a
/// hero sets out to cross, the far end of its portal is taken down, and the
/// log is fed until the hero reaches the dark portal and takes another
/// quest. Returns the member.
fn dark_scene() -> (Scene, usize) {
    let mut s = Scene::new();
    let mut walker = None;
    s.until(|saga, _| {
        walker = saga.state().party.iter().position(|m| {
            m.quest.verb == Verb::Cross && m.hero.transit.is_none() && m.hero.pos != m.quest.goal
        });
        walker.is_some()
    });
    let m = walker.unwrap();
    let state = s.state();
    let hero = &state.party[m];
    let floor = hero.hero.floor as usize;
    let port = state.world.floors[floor]
        .portals
        .iter()
        .find(|&&(_, cell)| cell == hero.quest.goal)
        .map(|&(p, _)| p)
        .expect("a crossing's goal is a portal");
    let (far, far_port) = s
        .saga
        .links()
        .bound(&state.world, floor as u8, port)
        .expect("the portal is bound");
    s.link_down(far as usize, far_port);
    s.until(|saga, _| saga.state().party[m].quest.verb != Verb::Cross);
    let dark = s
        .state()
        .chronicle
        .lines(5)
        .any(|b| b.text.contains("dark") && b.floor == Some(floor as u8));
    assert!(dark, "the chronicle says the portal is dark");
    (s, m)
}

/// The log fed to tick 410: link 1 (floor 2's port 2 to floor 3's port 0)
/// is down, so its two portals are closed.
fn link_down_scene() -> Scene {
    let mut s = Scene::new();
    s.until(|saga, _| saga.state().tick == 410);
    s
}

/// `portal_style` for the portal on `floor` with port `port`, fully lit and
/// with everything moving.
fn lit_portal(state: &SagaState, floor: usize, port: u8) -> Style {
    let look = Look::new(state.world.biome, None);
    portal_style(state, &look, floor, port, 0, 0, 100, Motion::Full)
}

/// Every cell of floor `floor` as [`map_rows`] draws it from that floor's
/// seat, every cell seen, the window as large as the floor.
fn floor_cells(
    state: &SagaState,
    party: &PartyMemory,
    floor: usize,
    motion: Motion,
) -> Vec<Vec<(char, Style)>> {
    let fl = &state.world.floors[floor];
    let mut memory = party.floor(floor).unwrap().clone();
    memory.see_everything();
    let look = Look::for_floor(state, &memory, floor);
    map_rows(
        state,
        &memory,
        &look,
        floor_seat(state, floor),
        fl.w as usize + 1,
        fl.h as usize,
        0,
        motion,
    )
    .into_iter()
    .map(|r| cells(&r.line()))
    .collect()
}

/// A colour's brightness: the sum of its channels.
fn brightness(c: Option<Color>) -> u32 {
    match c {
        Some(Color::Rgb(r, g, b)) => r as u32 + g as u32 + b as u32,
        other => panic!("expected a truecolor colour, got {other:?}"),
    }
}

/// Every portal's cell on floor `floor` is drawn with `portal_style`, so
/// what the tests below check of the style is what the map shows. The
/// hero, a goal and the glow ring are drawn over a portal, so a cell under
/// one of them is skipped.
fn assert_map_uses_portal_style(state: &SagaState, party: &PartyMemory, floor: usize) {
    let fl = &state.world.floors[floor];
    let mut memory = party.floor(floor).unwrap().clone();
    memory.see_everything();
    let look = Look::for_floor(state, &memory, floor);
    let glyph = Pack::builtin().glyphs.portal;
    for motion in [Motion::Full, Motion::Banded, Motion::Calm] {
        let rows = floor_cells(state, party, floor, motion);
        let seat = floor_seat(state, floor);
        let hero = seat.member.map(|m| state.party[m].hero.pos);
        let lit = hero.map_or(vec![0; fl.w as usize * fl.h as usize], |h| light(fl, h));
        for &(port, (x, y)) in &fl.portals {
            let (ch, style) = rows[y as usize][x as usize];
            let near = hero.is_some_and(|h| h.0.abs_diff(x) <= 1 && h.1.abs_diff(y) <= 1);
            if ch != glyph || near {
                continue;
            }
            let l = lit[y as usize * fl.w as usize + x as usize] as u32;
            let l = match motion {
                Motion::Calm => Calm::LIGHT,
                Motion::Banded => band(l),
                Motion::Full => l,
            };
            let want = portal_style(state, &look, floor, port, x as usize, 0, l, motion);
            assert_eq!(style, want, "floor {floor} port {port} {motion:?}");
        }
    }
}

/// An open portal is drawn in the hue of the floor at the other end of its
/// link, so the eye can follow the link from tile to tile. At tick 100 of
/// the cross log every ring link is open: floor 1's port 1 leads to floor 2
/// and its port 0 to floor 4.
#[test]
fn an_open_portal_takes_the_far_floor_hue() {
    let mut s = Scene::new();
    s.until(|saga, _| saga.state().tick == 100);
    let state = s.state();
    let p = &Pack::builtin().palette;
    tc(|| {
        let open: Vec<_> = state.portals.iter().filter(|p| p.open).collect();
        assert_eq!(open.len(), 8, "four links, two ends each: {open:?}");
        for portal in open {
            let (far, _) = portal.far.unwrap();
            let style = lit_portal(state, portal.floor as usize, portal.port);
            assert_eq!(
                style.fg,
                Some(rgb(saga_party::floor_hue(p, far))),
                "{portal:?}"
            );
            assert!(style.add_modifier.contains(Modifier::BOLD), "{portal:?}");
        }
        assert_eq!(
            lit_portal(state, 0, 1).fg,
            Some(rgb(saga_party::floor_hue(p, 1)))
        );
        assert_eq!(
            lit_portal(state, 0, 0).fg,
            Some(rgb(saga_party::floor_hue(p, 3)))
        );
        for floor in 0..4 {
            assert_map_uses_portal_style(state, &s.party, floor);
        }
    });
}

/// A bound portal whose link is down is drawn dim and not bold, in a
/// darker shade of the far floor's hue: at tick 410 link 1 is down, so
/// floor 2's port 2 and floor 3's port 0 are closed.
#[test]
fn a_closed_portal_is_dim() {
    let s = link_down_scene();
    let state = s.state();
    let p = &Pack::builtin().palette;
    let closed: Vec<(u8, u8)> = state
        .portals
        .iter()
        .filter(|p| p.far.is_some() && !p.open)
        .map(|p| (p.floor, p.port))
        .collect();
    assert_eq!(closed, [(1, 2), (2, 0)], "link 1 down, portals closed");
    tc(|| {
        for (floor, port) in closed {
            let style = lit_portal(state, floor as usize, port);
            let far = if floor == 1 { 2 } else { 1 };
            let open = rgb(saga_party::floor_hue(p, far));
            assert!(
                brightness(style.fg) < brightness(Some(open)),
                "floor {floor} port {port}: {style:?}"
            );
            assert!(!style.add_modifier.contains(Modifier::BOLD));
        }
        for floor in [1, 2] {
            assert_map_uses_portal_style(state, &s.party, floor);
        }
    });
}

/// A portal no peer record leads to is drawn as before the crossing: the
/// teal shimmer, or its dim teal on a calm tile. The party logs have no
/// peers, and in the cross log floor 2's port 1 and floor 3's ports 1 and 2
/// lead nowhere.
#[test]
fn a_portal_without_a_peer_is_drawn_as_before() {
    let (party4_state, _) = end_of("party4");
    let s = link_down_scene();
    tc(|| {
        for state in [party4_state, s.state()] {
            let look = Look::new(state.world.biome, None);
            let base = look.p.base;
            let mut unbound = 0;
            for portal in state.portals.iter().filter(|p| p.far.is_none()) {
                unbound += 1;
                let (f, port) = (portal.floor as usize, portal.port);
                for x in 0..8 {
                    for frame in [0, 3, 6, 9] {
                        for light in [0, 55, 100] {
                            assert_eq!(
                                portal_style(state, &look, f, port, x, frame, light, Motion::Full),
                                look.cell(Cell::Portal, x, frame).at(base, light)
                            );
                            assert_eq!(
                                portal_style(state, &look, f, port, x, frame, light, Motion::Calm),
                                look.portal_dim.at(base, light)
                            );
                        }
                    }
                }
            }
            assert!(unbound >= 3, "{unbound} unbound portals");
        }
    });
}

/// While a hero is in transit, the portal it stepped into and the portal it
/// will step out of both glow in the hero's colour, and both tiles' HUDs
/// say `crossing F2 → F1`. No other portal glows.
#[test]
fn a_hero_in_transit_glows_on_both_portals() {
    let (s, c) = transit_scene();
    let state = s.state();
    assert_eq!(
        (c.member, c.from, c.to),
        (1, 1, 0),
        "the log's first crossing"
    );
    let p = &Pack::builtin().palette;
    tc(|| {
        let glow = lit_portal(state, c.from, c.from_port);
        assert_eq!(glow, lit_portal(state, c.to, c.to_port));
        assert_eq!(glow.fg, Some(rgb(p.yellow)), "the hero's colour");
        assert!(glow.bg.is_some(), "a glow behind the glyph");
        for portal in &state.portals {
            let (f, port) = (portal.floor as usize, portal.port);
            if (f, port) != (c.from, c.from_port) && (f, port) != (c.to, c.to_port) {
                assert_ne!(lit_portal(state, f, port), glow, "{portal:?}");
            }
        }
        // The glow holds still: the same at every frame, on a calm tile too.
        let look = Look::new(state.world.biome, None);
        for frame in 0..12 {
            for motion in [Motion::Full, Motion::Calm] {
                let s = portal_style(state, &look, c.from, c.from_port, 3, frame, 50, motion);
                assert_eq!(s.bg, glow.bg);
            }
        }
        assert_map_uses_portal_style(state, &s.party, c.from);
        assert_map_uses_portal_style(state, &s.party, c.to);
        let lines = render_saga(
            state,
            &s.party,
            flags(c.from, false, false, false),
            (140, 39),
            0,
        );
        let huds: Vec<String> = text(&lines)
            .into_iter()
            .flat_map(|t| t.split('▌').skip(1).map(str::to_string).collect::<Vec<_>>())
            .filter(|h| h.starts_with('F'))
            .collect();
        let crossing: Vec<&String> = huds
            .iter()
            .filter(|h| h.contains("crossing F2 → F1"))
            .collect();
        assert_eq!(crossing.len(), 2, "{huds:?}");
        assert!(
            crossing.iter().any(|h| h.starts_with("F1 "))
                && crossing.iter().any(|h| h.starts_with("F2 "))
        );
        let status = text(&lines).last().unwrap().clone();
        assert!(status.contains("crossing F2 → F1"), "{status}");
    });
}

/// A floor whose only hero has crossed away has a tile with no hero: its
/// HUD says `no hero here`, nothing on it is drawn as a hero in any
/// motion, and its window stays where its hero left it. The floor's seat,
/// which the zoomed view draws from, centres on the gate.
#[test]
fn an_empty_floor_tile_says_so() {
    let (s, c) = arrival_scene();
    let state = s.state();
    let hero = Pack::builtin().glyphs.hero;
    assert_eq!(state.on_floor(c.from as u8).count(), 0, "floor 2 is empty");
    let seat = floor_seat(state, c.from);
    assert_eq!(seat.member, None);
    let gate = state.world.floors[c.from].entrance;
    assert_eq!(seat.centre, (gate.0 as usize, gate.1 as usize));
    tc(|| {
        for focus in [c.from, c.to] {
            let lines = render_saga(
                state,
                &s.party,
                flags(focus, false, false, false),
                (140, 39),
                0,
            );
            let huds = text(&lines);
            assert!(
                huds.iter().any(|t| t.contains("▌F2 no hero here")),
                "{huds:?}"
            );
        }
        for motion in [Motion::Full, Motion::Banded, Motion::Calm] {
            let rows = floor_cells(state, &s.party, c.from, motion);
            assert!(
                rows.iter().flatten().all(|&(ch, _)| ch != hero),
                "{motion:?}"
            );
        }
        // The tile keeps the window its hero left it with.
        let memory = s.party.floor(c.from).unwrap();
        let anchor = memory.anchor.expect("the window stays put");
        let (_, tile) = tile_view(state, memory, c.from, true);
        assert_eq!(tile.centre, (anchor.0 as usize, anchor.1 as usize));
    });
}

/// Two heroes on one floor are both drawn: after the first crossing,
/// floor 1 holds its own hero and the hero who arrived.
#[test]
fn two_heroes_on_one_tile_both_draw() {
    let (s, c) = arrival_scene();
    let state = s.state();
    let hero = Pack::builtin().glyphs.hero;
    assert_eq!(state.on_floor(c.to as u8).count(), 2);
    tc(|| {
        for motion in [Motion::Full, Motion::Banded, Motion::Calm] {
            let rows = floor_cells(state, &s.party, c.to, motion);
            let mut at: Vec<(usize, usize)> = Vec::new();
            for (y, row) in rows.iter().enumerate() {
                for (x, &(ch, _)) in row.iter().enumerate() {
                    if ch == hero {
                        at.push((x, y));
                    }
                }
            }
            let mut want: Vec<(usize, usize)> = state
                .on_floor(c.to as u8)
                .map(|(_, m)| (m.hero.pos.0 as usize, m.hero.pos.1 as usize))
                .collect();
            want.sort_by_key(|&(x, y)| (y, x));
            assert_eq!(at, want, "{motion:?}");
        }
    });
}

/// A floor's memory belongs to the floor: its lit cells stay when its hero
/// leaves, through the transit and after the hero arrives elsewhere. The
/// lead (member 0) crosses too, from floor 1 at tick 280, and its old
/// floor keeps its fog; the lead's memory (the single-floor view's and the
/// arcade panel's) stays on that floor while the lead is in transit.
#[test]
fn a_floor_keeps_its_memory_after_its_hero_leaves() {
    let seen = |m: &SagaMemory| m.seen.iter().filter(|&&s| s).count();
    let mut s = Scene::new();
    // Member 1 leaves floor 2.
    let c = s.until_crossing();
    assert_eq!((c.member, c.from), (1, 1));
    let before = seen(s.party.floor(1).unwrap());
    assert!(before > 50, "{before} cells seen");
    s.until(|saga, _| saga.state().tick > 220);
    assert_eq!(seen(s.party.floor(1).unwrap()), before);
    // The lead leaves floor 1.
    let c = s.until_crossing();
    assert_eq!((c.member, c.from), (0, 0), "the lead crosses");
    let state = s.state();
    let before = seen(s.party.floor(0).unwrap());
    assert!(before > 50, "{before} cells seen");
    let lead = s.party.lead(state);
    assert!(
        lead.fits_floor(state, 0),
        "the lead's memory is its old floor's"
    );
    assert_eq!(seen(lead), before);
    // The arcade panel copies the lead's memory and observes the state.
    let mut panel = SagaMemory::new();
    panel.copy_from(lead);
    panel.observe(state);
    assert!(panel.fits_floor(state, 0));
    assert_eq!(seen(&panel), before);
    // After the arrival, up to the world's end at tick 300.
    s.until(|saga, _| saga.state().party[0].hero.transit.is_none());
    s.until(|saga, _| saga.state().tick > 295);
    assert_eq!(s.state().world.born_tick, 80, "the same world");
    assert!(seen(s.party.floor(0).unwrap()) >= before);
}

/// The arcade panel (and a world drawn through the lead's seat) shows the
/// floor the lead left while the lead is in transit, with no hero drawn on
/// the lead's portal, and its HUD says where the lead is going.
#[test]
fn the_arcade_panel_follows_the_lead_through_a_crossing() {
    let mut s = Scene::new();
    let c = loop {
        let c = s.until_crossing();
        if c.member == 0 {
            break c;
        }
    };
    let state = s.state();
    let seat = lead_seat(state);
    assert_eq!(seat.floor, c.from);
    assert_eq!(seat.member, None, "the lead is on no map");
    // The panel's memory, taken as `ArcadeVisualization::set_saga` takes
    // it, is the old floor's, with what its heroes saw there.
    let mut panel = SagaMemory::new();
    panel.copy_from(s.party.lead(state));
    panel.observe(state);
    assert!(panel.fits_floor(state, c.from));
    assert!(panel.seen.iter().filter(|&&s| s).count() > 50);
    // No hero is drawn where the lead stepped into the portal.
    let look = Look::for_state(state, &panel);
    let fl = &state.world.floors[c.from];
    let rows = map_rows(
        state,
        &panel,
        &look,
        seat,
        fl.w as usize + 1,
        fl.h as usize,
        0,
        Motion::Full,
    );
    let (x, y) = state.party[0].hero.pos;
    let row = rows.into_iter().nth(y as usize).unwrap();
    let at = cells(&row.line())[x as usize].0;
    assert_ne!(
        at,
        Pack::builtin().glyphs.hero,
        "the lead is drawn on its portal"
    );
    let feed = PanelFeed::Live {
        state: std::sync::Arc::new(state.clone()),
        stopped: false,
    };
    let lines = tc(|| render_saga_panel(&feed, &panel, (70, 16), 0));
    let t = text(&lines);
    assert!(t[0].contains("crossing F1 → F2"), "{}", t[0]);
}

/// The HUD and the status row say where a crossing stands: walking to the
/// portal, in transit, arrived, or thrown back. After a failure the hero
/// stands on its goal, and the view no longer calls that a finished quest.
#[test]
fn the_quest_words_follow_the_crossing() {
    let (s, c) = failure_scene();
    let state = s.state();
    let m = &state.party[c.member];
    assert_eq!(m.quest.verb, Verb::Cross);
    assert_eq!(m.hero.pos, m.quest.goal, "back on its portal");
    let text = quest_text(state, m);
    assert!(text.contains("thrown back"), "{text}");
    assert!(!text.contains("(done)"), "{text}");
    let word = quest_word(state, m, true);
    assert!(word.starts_with("thrown back"), "{word}");
    assert!(!word.contains('✓'), "{word}");

    let (s, c) = arrival_scene();
    let state = s.state();
    let m = &state.party[c.member];
    assert_eq!(m.quest.verb, Verb::Cross, "the dwell keeps the quest");
    assert_eq!(quest_text(state, m), "arrived on F1 through a portal");
    assert_eq!(quest_word(state, m, true), "arrived");

    let (s, c) = transit_scene();
    let state = s.state();
    let m = &state.party[c.member];
    assert_eq!(quest_text(state, m), "crossing F2 → F1");
    assert_eq!(quest_word(state, m, false), "crossing F2 → F1");

    let mut s = Scene::new();
    let mut walker = None;
    s.until(|saga, _| {
        walker = saga
            .state()
            .party
            .iter()
            .position(|m| m.quest.verb == Verb::Cross && m.hero.pos != m.quest.goal);
        walker.is_some()
    });
    let state = s.state();
    let m = &state.party[walker.unwrap()];
    assert_eq!(quest_text(state, m), "heading for the portal to F1");
    assert_eq!(quest_word(state, m, false), "cross to F1");
}

/// The roster draws a member in transit with its own glyph, in the hue of
/// the floor it is going to.
#[test]
fn the_roster_marks_a_hero_in_transit() {
    let (s, c) = transit_scene();
    let state = s.state();
    let p = &Pack::builtin().palette;
    let grid = party_grid(4, 140, 39).unwrap();
    let row = tc(|| roster_row(state, flags(0, false, false, false), &grid, 0, 140).line());
    let cs = cells(&row);
    // ` party ` then two columns per member.
    let (ch, style) = cs[7 + 2 * c.member];
    assert_eq!(ch, TRANSIT_GLYPH);
    let hue = tc(|| rgb(saga_party::floor_hue(p, c.to as u8)));
    assert_eq!(style.fg, Some(hue));
    let hero = Pack::builtin().glyphs.hero;
    for m in (0..4).filter(|&m| m != c.member) {
        assert_eq!(cs[7 + 2 * m].0, hero);
    }
}

/// The cross log's scenes fit every size, and no row of them ends in a
/// right-side border glyph.
#[test]
fn the_cross_scenes_fit_every_size() {
    use crate::animation::arcade::RIGHT_EDGE_GLYPHS;
    let scenes = cross_scenes();
    let widths: Vec<u16> = (0..=90).chain((91..=500).step_by(23)).collect();
    let heights: Vec<u16> = (0..=30).chain((31..=200).step_by(17)).collect();
    tc(|| {
        for (what, scene, focus) in &scenes {
            let state = scene.state();
            let party = all_seen(&scene.party);
            for &w in &widths {
                for &h in &heights {
                    for f in [
                        flags(*focus, false, false, false),
                        flags(*focus, false, true, true),
                        flags(*focus, true, false, false),
                    ] {
                        let lines = render_saga(state, &party, f, (w, h), 7);
                        assert_fits(&lines, w, h, what);
                        if w == 0 {
                            continue;
                        }
                        for (i, line) in lines.iter().enumerate() {
                            if let Some(&(ch, _)) = cells(line).get(w as usize - 1) {
                                assert!(
                                    !RIGHT_EDGE_GLYPHS.contains(&ch),
                                    "{what} {w}x{h} row {i} ends in {ch:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    });
}

/// The scenes of the cross log the golden frames draw, with the floor each
/// focuses on:
///
/// 1. tick 410: link 1 down, its two portals closed;
/// 2. the first crossing in transit (member 1, floor 2 to floor 1);
/// 3. that crossing failed (the far end taken down in transit);
/// 4. a hero turned away from a dark portal;
/// 5. the end of the log's last full world, after five crossings (and with
///    floor 1's two ring ports dark since tick 600).
fn cross_scenes() -> Vec<(&'static str, Scene, usize)> {
    let (transit, c) = transit_scene();
    let (failure, _) = failure_scene();
    let (dark, m) = dark_scene();
    let dark_floor = dark.state().party[m].hero.floor as usize;
    let mut end = Scene::new();
    for r in before_last_world(cross4()) {
        end.feed(r);
    }
    vec![
        ("link 1 down, portals closed", link_down_scene(), 1),
        ("in transit", transit, c.from),
        ("thrown back", failure, c.from),
        ("dark portal", dark, dark_floor),
        ("the end", end, 1),
    ]
}

/// The cross log's scenes ([`cross_scenes`]) at 80x23 and 140x39: ten
/// frames.
fn cross_frames() -> Vec<u64> {
    tc(|| {
        let mut out = Vec::new();
        for (_, scene, focus) in cross_scenes() {
            for size in [(80u16, 23u16), (140, 39)] {
                let f = flags(focus, false, false, false);
                out.push(frame_hash(&render_saga(
                    scene.state(),
                    &scene.party,
                    f,
                    size,
                    5,
                )));
            }
        }
        out
    })
}

/// The hashes of [`cross_frames`], pinned in Task B.5 of the party plan
/// when crossings were first drawn (two per scene, 80x23 then 140x39). A
/// change to the portals' colours, the crossing HUDs, the empty and crowded
/// tiles or anything else the party view draws moves them; re-pin on
/// purpose with `print_cross_frames` and say why in the commit.
///
/// Re-pinned in fix round 1 of Task B.5, for two changes, all ten frames:
/// * An empty floor keeps the window its hero left it with (it re-centred
///   on the portal and then the gate), and a hero arriving there moves
///   that window by the dead-zone rule (it started a new one on the
///   arrival cell). Alone, this moved the tick-410 frames (floor 1 is
///   empty: its hero crossed at tick 404) and the end frames (floors that
///   emptied and filled again after the crossings at 514, 515 and 585); the
///   transit, thrown-back and dark frames kept their hashes.
/// * The held picture of an unfocused tile is taken every 5 ticks
///   (`CALM_EVERY`, was 3). This moved the transit, thrown-back, dark and
///   end frames; the tick-410 frames fall where both periods hold the same
///   picture.
const CROSS_GOLDEN: [u64; 10] = [
    16621094620833925702,
    12081447335216494757,
    9352530000089581407,
    12858411009499260224,
    17231023619959074393,
    954191240695233659,
    13504525090761453278,
    3007435991782443636,
    951394070982446229,
    18228818691771887738,
];

/// Prints the hashes for [`CROSS_GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui print_cross_frames -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn print_cross_frames() {
    println!("{:?}", cross_frames());
}

/// Prints the frames of [`cross_scenes`] as text at 140x39, to look at.
#[test]
#[ignore = "prints the frames"]
fn print_cross_scenes() {
    for (what, scene, focus) in cross_scenes() {
        println!("== {what}, tick {}", scene.state().tick);
        let lines = tc(|| {
            render_saga(
                scene.state(),
                &scene.party,
                flags(focus, false, false, false),
                (140, 39),
                5,
            )
        });
        for t in text(&lines) {
            println!("{t}");
        }
    }
}

#[test]
fn cross_frames_are_pinned() {
    assert_eq!(cross_frames(), CROSS_GOLDEN.to_vec());
}

/// Crossings keep the party view inside its traffic budget: the frames of
/// the cross log from tick 190 to 300 (two crossings' worth of glow, HUD
/// changes, and a floor left empty) at 140x39.
#[test]
fn crossings_stay_inside_the_traffic_budget() {
    let per_frame = party_bytes_per_frame(cross4(), 190, 300, 140, 39);
    assert!(
        per_frame <= PARTY_BYTES_PER_FRAME,
        "{per_frame} bytes a frame, over {PARTY_BYTES_PER_FRAME}"
    );
}
