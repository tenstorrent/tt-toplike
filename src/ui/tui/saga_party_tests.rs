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
    let (cell, log): (&OnceLock<_>, fn() -> &'static [Record]) = match which {
        "solo" => (&SOLO, solo),
        "busy" => (&BUSY, busy),
        "party4" => (&P4, party4),
        "32" => (&P32, log32),
        "40" => (&P40, log40),
        _ => unreachable!("no log {which}"),
    };
    cell.get_or_init(|| {
        let (saga, party) = run(log(), NameMode::Real);
        (saga.state().clone(), party)
    })
}

/// The flags with `focus`, `zoom`, `why`, `stopped` and `chips` set.
fn flags(focus: usize, zoom: bool, why: bool, stopped: bool, chips: usize) -> SagaFlags {
    SagaFlags {
        stopped,
        why,
        focus,
        zoom,
        chips,
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
/// 1, 2, 4 and 32 chips. The 4-chip tiles at 80x23 are a row taller than
/// the addendum's working (39 x 8), since the tiles share every row the
/// chronicle leaves. The others are the table's numbers.
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
            let f = flags(2, false, false, false, 0);
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

/// Every size from 0x0 to 500x200 fits for 1, 2, 4 and 32 chips: no line
/// wider than the area and no more lines than its rows, with the focus on
/// several floors, with "why" on and the story stopped, and zoomed.
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
        for which in ["solo", "busy", "party4", "32"] {
            let (state, party) = end_of(which);
            let n = state.world.floors.len();
            let variants = [
                flags(0, false, false, false, 0),
                flags(n - 1, false, true, true, n + 8),
                flags(n / 2, true, true, false, 0),
                flags(n + 5, false, false, false, 0),
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
/// edge, so every tile's window scrolls to the floor's last column.
#[test]
fn no_party_row_ends_in_a_right_side_border() {
    use crate::animation::arcade::RIGHT_EDGE_GLYPHS;
    for which in ["busy", "party4", "32"] {
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
                    flags(0, false, false, false, 0),
                    flags(n - 1, false, true, true, 0),
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
                    let g = party_grid(n, w as usize, h as usize - usize::from(f.stopped))
                        .expect("a grid at the table's sizes");
                    let first = 1 + usize::from(f.stopped) + usize::from(g.per_page < n);
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
        let lines = render_saga(state, party, flags(0, false, false, false, 32), (80, 23), 0);
        let (_, roster) = roster_of(&lines).expect("a roster at 80x23");
        assert_eq!(roster.matches(hero).count(), 32, "{roster}");
        assert!(roster.ends_with(" 1/6"), "{roster}");
        let lines = render_saga(
            state,
            party,
            flags(0, false, false, false, 32),
            (140, 39),
            0,
        );
        let (_, roster) = roster_of(&lines).expect("a roster at 140x39");
        assert!(roster.ends_with(" page 1 of 3"), "{roster}");
        assert!(!roster.contains("without"), "{roster}");
        // One page holds them all at 200x59: no roster with no extra chips.
        let lines = render_saga(
            state,
            party,
            flags(0, false, false, false, 32),
            (200, 59),
            0,
        );
        assert!(roster_of(&lines).is_none());

        let (state, party) = end_of("40");
        assert_eq!(state.world.floors.len(), 32, "a world keeps 32 floors");
        for (w, h) in [(140u16, 39u16), (200, 59)] {
            let lines = render_saga(state, party, flags(0, false, false, false, 40), (w, h), 0);
            let (_, roster) = roster_of(&lines).expect("a roster for the chips without a floor");
            assert_eq!(roster.matches(hero).count(), 32, "{roster}");
            assert!(
                roster.contains("+8 chips without a floor"),
                "{w}x{h}: {roster}"
            );
        }
        let lines = render_saga(state, party, flags(0, false, false, false, 40), (80, 23), 0);
        let (_, roster) = roster_of(&lines).expect("a roster at 80x23");
        assert!(roster.contains("+8 chips"), "{roster}");
        // The member's glyphs: the focused one on its hue, the page's
        // bright, the others dim.
        let (state, party) = end_of("32");
        let lines = render_saga(state, party, flags(7, false, false, false, 0), (80, 23), 0);
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
            let lines = render_saga(
                state,
                party,
                flags(focus, false, false, false, 0),
                (80, 23),
                0,
            );
            hud_tags(&lines)
        };
        let names = |a: usize, b: usize| (a..=b).map(|i| format!("F{i}")).collect::<Vec<_>>();
        assert_eq!(tags(0), names(1, 6));
        assert_eq!(tags(5), names(1, 6));
        assert_eq!(tags(6), names(7, 12));
        assert_eq!(tags(31), names(31, 32));
        assert_eq!(tags(99), names(31, 32), "past the end is the last floor");
        let lines = render_saga(state, party, flags(8, false, false, false, 0), (80, 23), 0);
        let (_, roster) = roster_of(&lines).unwrap();
        assert!(roster.ends_with(" 2/6"), "{roster}");
        // The status row names the focused hero, and from 140 columns it
        // has room for the party's keys.
        let status = text(&lines).pop().unwrap();
        let name = &state.party[8].hero.name;
        assert!(status.contains(name.as_str()), "{status} names {name}");
        let wide = render_saga(state, party, flags(8, false, false, false, 0), (140, 39), 0);
        let status = text(&wide).pop().unwrap();
        assert!(status.contains(name.as_str()), "{status} names {name}");
        assert!(status.contains("[ ] focus  z zoom"), "{status}");
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
                let f = flags(focus, true, false, false, 0);
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
                let map = map_rows(state, party.floor(focus).unwrap(), &look, seat, 10, 3, 5);
                assert!(!map.is_empty());
            }
        }
        // The lead's floor zoomed is the lead's single-floor frame.
        let f = flags(0, true, true, false, 0);
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
                let plain = render_saga(state, party, flags(0, false, why, false, 0), (w, h), 5);
                for focus in [0, 1, 7] {
                    for zoom in [false, true] {
                        for chips in [0, 1, 40] {
                            let f = flags(focus, zoom, why, false, chips);
                            let got = render_saga(state, party, f, (w, h), 5);
                            assert_eq!(frame_hash(&got), frame_hash(&plain), "{f:?} {w}x{h}");
                        }
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
    let why = flags(0, false, true, false, 0);
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
                flags(1, zoom, false, false, 0),
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
            flags(0, false, true, false, 0),
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
    for log in [party4(), log32()] {
        for names in [NameMode::Generated, NameMode::Real] {
            let (saga, party) = run(log, names);
            let state = saga.state();
            let n = state.world.floors.len();
            tc(|| {
                for (w, h) in TABLE_SIZES {
                    for focus in (0..n).step_by(5) {
                        for why in [false, true] {
                            let f = flags(focus, false, why, false, n + 3);
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
    let lines = tc(|| {
        render_saga(
            &state,
            party,
            flags(0, false, false, false, 0),
            (200, 59),
            0,
        )
    });
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
                let f = flags(k % 5, k % 3 == 0, k % 2 == 0, false, k % 7);
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
                    let f = flags(1, false, why, false, 0);
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
const PARTY_GOLDEN: [u64; 8] = [
    14822553245536003162,
    18407291322666978306,
    16685342915309974070,
    5941585132995016157,
    5922583961810723963,
    7362451235971816641,
    11507435028002847143,
    10433589368542686763,
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
