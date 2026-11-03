// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the Saga view (`saga_view.rs`).
//!
//! The states come from the saga crate's `busy.jsonl` fixture fed through a
//! real [`Saga`], so the view is tested against the worlds, heroes, quests
//! and chronicles the core actually makes. Every colour goes through
//! `colors::rgb`, so the golden frames and colour checks run inside [`tc`]:
//! truecolor on and the default theme under the theme lock. That keeps them
//! passing under `TERM=dumb`, as CI runs the lib tests.

use super::*;
use crate::ui::colors::{self, Theme};
use ratatui::style::Color;
use std::sync::OnceLock;
use tt_toplike_bus::segment::read_segment;
use tt_toplike_bus::Record;
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::quest::Verb;
use tt_toplike_saga::{Label, Saga, SagaConfig};

// ---- Helpers ---------------------------------------------------------------

/// Runs `f` with truecolor on and the default theme, as the arcade's golden
/// tests do, so a frame hashes the same on every terminal and in CI.
fn tc<R>(f: impl FnOnce() -> R) -> R {
    colors::with_truecolor(true, || colors::with_theme(Theme::Default, f))
}

/// Every record of the saga crate's busy fixture: two chips, a model coming
/// up and going away, requests, a reset, a throttle, ECC errors and a
/// training run. Read once per test process.
fn busy() -> &'static [Record] {
    static RECORDS: OnceLock<Vec<Record>> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/tt-toplike-saga/tests/fixtures/busy.jsonl");
        let read = read_segment(&path).expect("busy.jsonl is committed");
        assert_eq!(read.bad_lines, 0, "busy.jsonl decodes");
        read.records
    })
}

/// The saga config the tests use: a 200-tick season (as the saga crate's
/// determinism tests use, so quiet stretches end worlds), `names` and
/// `label` as given.
fn config(names: NameMode, label: Label) -> SagaConfig {
    SagaConfig {
        season_ticks: 200,
        names,
        label,
    }
}

/// A saga fed the first `n` busy records.
fn saga_after(n: usize, names: NameMode, label: Label) -> Saga {
    let mut saga = Saga::new(config(names, label));
    for r in busy().iter().take(n) {
        saga.feed(r);
    }
    saga
}

/// The state after every busy record, real names, mock label.
fn busy_state() -> SagaState {
    saga_after(usize::MAX, NameMode::Real, Label::Mock)
        .state()
        .clone()
}

/// The state before any record: the placeholder world.
fn empty_state(label: Label) -> SagaState {
    Saga::new(config(NameMode::Real, label)).state().clone()
}

/// One frame with a memory that has observed only `state`: the hero's light
/// and nothing remembered. Tests that need a remembered map build the memory
/// over the log ([`saga_and_memory`]).
fn draw(state: &SagaState, flags: SagaFlags, area: (u16, u16), frame: u64) -> Vec<Line<'static>> {
    let mut memory = SagaMemory::new();
    memory.observe(state);
    render_lead(state, &memory, flags, area, frame)
}

/// The single-floor drawing of the lead's floor ([`render_floor`] with
/// [`lead_seat`]) from one floor's memory: what [`render_saga`] draws for a
/// world of one floor, and what it drew for every world before the party.
/// The tests of the floor drawing itself (fog, walls, sizes, the status
/// row) use it on the two-floor busy log, whose full view is now the grid.
fn render_lead(
    state: &SagaState,
    memory: &SagaMemory,
    flags: SagaFlags,
    area: (u16, u16),
    frame: u64,
) -> Vec<Line<'static>> {
    render_floor(state, memory, lead_seat(state), flags, area, frame)
}

/// A saga fed the first `n` busy records and a memory that observed the
/// state after each record, as the TUI's loop does on each update.
fn saga_and_memory(n: usize) -> (Saga, SagaMemory) {
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    let mut memory = SagaMemory::new();
    for r in busy().iter().take(n) {
        saga.feed(r);
        memory.observe(saga.state());
    }
    (saga, memory)
}

const PLAIN: SagaFlags = SagaFlags {
    stopped: false,
    why: false,
    focus: 0,
    zoom: false,
};

/// The text of each line, spans joined.
fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// A line's width in terminal columns.
fn width(line: &Line<'static>) -> usize {
    use unicode_width::UnicodeWidthStr;
    line.spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Every line fits `w` x `h`: at most `h` lines, each at most `w` columns.
fn assert_fits(lines: &[Line<'static>], w: u16, h: u16, what: &str) {
    assert!(
        lines.len() <= h as usize,
        "{what} at {w}x{h}: {} lines",
        lines.len()
    );
    for (i, l) in lines.iter().enumerate() {
        assert!(
            width(l) <= w as usize,
            "{what} at {w}x{h}: line {i} is {} columns: {:?}",
            width(l),
            text(std::slice::from_ref(l))
        );
    }
}

/// FNV-1a, 64 bit, over each cell's character, colours and modifiers, with
/// a marker at each line end (the arcade's `frame_hash`, so span grouping
/// does not enter the hash).
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

// ---- The waiting screen ----------------------------------------------------

/// Before the first world the core holds a placeholder world (id 0, a
/// nameless hero). The view does not draw it: it says the chronicle has not
/// begun, and draws no map and no hero.
#[test]
fn an_empty_state_draws_a_quiet_world() {
    let state = empty_state(Label::Live);
    assert!(!state.has_world(), "the fixture is the placeholder");
    for (w, h) in [(80, 23), (140, 39), (30, 10)] {
        let lines = tc(|| draw(&state, PLAIN, (w, h), 0));
        let all = text(&lines).join("\n");
        assert!(
            all.contains("the chronicle has not begun"),
            "{w}x{h}: {all}"
        );
        let glyphs = &Pack::builtin().glyphs;
        assert!(!all.contains(glyphs.hero), "no hero yet at {w}x{h}: {all}");
        assert!(!all.contains(glyphs.wall), "no map yet at {w}x{h}: {all}");
        assert!(
            !all.contains(glyphs.door) && !all.contains(glyphs.vault),
            "no map yet at {w}x{h}: {all}"
        );
    }
}

/// A stopped driver before any world still says the story stopped.
#[test]
fn the_waiting_screen_says_when_the_story_stopped() {
    let state = empty_state(Label::Live);
    let flags = SagaFlags {
        stopped: true,
        why: false,
        ..SagaFlags::default()
    };
    let all = text(&tc(|| draw(&state, flags, (80, 23), 0))).join("\n");
    assert!(all.contains("the story has stopped"), "{all}");
}

// ---- Sizes -----------------------------------------------------------------

/// 0x0 and 1x1 draw nothing; below about 20x6 the view is one title line;
/// below about 40x12 it is a map with its title; at 80x24 the map sits above
/// the chronicle; from 100 columns the chronicle is beside the map.
#[test]
fn each_size_gets_its_layout() {
    let state = busy_state();
    assert!(tc(|| draw(&state, PLAIN, (0, 0), 0)).is_empty());
    assert!(tc(|| draw(&state, PLAIN, (1, 1), 0)).is_empty());
    assert!(tc(|| draw(&state, PLAIN, (5, 0), 0)).is_empty());
    assert_eq!(tc(|| draw(&state, PLAIN, (19, 30), 0)).len(), 1);
    assert_eq!(tc(|| draw(&state, PLAIN, (60, 5), 0)).len(), 1);

    // Map only: no chronicle header.
    let small = text(&tc(|| draw(&state, PLAIN, (39, 20), 0))).join("\n");
    assert!(!small.contains("CHRONICLE"), "{small}");
    assert!(small.contains('@'), "the hero is on the small map: {small}");

    // 80x23 (24 rows less the status bar): the chronicle under the map.
    let stacked = text(&tc(|| draw(&state, PLAIN, (80, 23), 0)));
    let header = stacked
        .iter()
        .position(|l| l.contains("CHRONICLE"))
        .expect("80x23 has a chronicle");
    let hero = stacked
        .iter()
        .position(|l| l.contains('@'))
        .expect("80x23 has the hero");
    assert!(
        hero < header,
        "the map is above the chronicle: {stacked:#?}"
    );

    // 140x39: the chronicle beside the map, on the same rows.
    let side = text(&tc(|| draw(&state, PLAIN, (140, 39), 0)));
    let header = side
        .iter()
        .find(|l| l.contains("CHRONICLE"))
        .expect("140x39 has a chronicle");
    let floor_w = state.world.floors[hero_floor_index(&state).unwrap()].w as usize;
    assert!(
        header.chars().position(|c| c == '╔') > Some(floor_w),
        "the chronicle starts after the map pane on the same row: {header}"
    );
}

/// Every size up to 500x200 fits: no line wider than the area and no more
/// lines than its rows, for the waiting screen, the busy end state, and the
/// busy state with "why" on and the story stopped. Every width to 130 and
/// every height to 30 are covered, then larger sizes in steps.
#[test]
fn every_size_from_0x0_to_500x200_draws_without_overflow() {
    let waiting = empty_state(Label::Replay);
    let busy = busy_state();
    let why = SagaFlags {
        stopped: true,
        why: true,
        ..SagaFlags::default()
    };
    let widths: Vec<u16> = (0..=130)
        .chain((131..=500).step_by(17))
        .chain([500])
        .collect();
    let heights: Vec<u16> = (0..=30)
        .chain((31..=200).step_by(13))
        .chain([200])
        .collect();
    tc(|| {
        for &w in &widths {
            for &h in &heights {
                assert_fits(&draw(&busy, PLAIN, (w, h), 7), w, h, "busy");
                assert_fits(&draw(&busy, why, (w, h), 7), w, h, "busy+why");
                assert_fits(&draw(&waiting, why, (w, h), 7), w, h, "waiting");
                assert_fits(&render_saga_off((w, h), false), w, h, "off");
            }
        }
    });
}

/// Left and bottom bars only (`/home/ttuser/CLAUDE.md`): no row may put a
/// right-side border glyph in the area's last column, the arcade's rule
/// with its shared list ([`crate::animation::arcade::RIGHT_EDGE_GLYPHS`]).
/// The wall tiles hold right-edge shapes (`┐`, `┤`, `╗`), so the map keeps
/// its pane's last column blank. The memory here has seen every cell, so
/// every wall the floor has is drawn, and the widths below the floor's
/// width make the map scroll right up to the edge. The chronicle's `║` is
/// its left bar and never lands in the last column.
#[test]
fn no_row_ends_in_a_right_side_border() {
    use crate::animation::arcade::RIGHT_EDGE_GLYPHS;
    use unicode_width::UnicodeWidthChar;
    let busy = busy_state();
    let mut all_seen = SagaMemory::new();
    all_seen.observe(&busy);
    all_seen.see_everything();
    let mut none = SagaMemory::new();
    none.observe(&empty_state(Label::Mock));
    let states = [
        (empty_state(Label::Mock), &none, PLAIN),
        (busy.clone(), &all_seen, PLAIN),
        (
            busy.clone(),
            &all_seen,
            SagaFlags {
                stopped: true,
                why: true,
                ..SagaFlags::default()
            },
        ),
    ];
    // The hero at each corner of the floor too, so the map scrolls to its
    // last column at every width narrower than the floor.
    let floor = hero_floor(&busy).expect("a floor").clone();
    let mut corners = Vec::new();
    for pos in [
        (1, 1),
        (floor.w - 2, 1),
        (1, floor.h - 2),
        (floor.w - 2, floor.h - 2),
    ] {
        let mut s = busy.clone();
        s.party[0].hero.pos = pos;
        corners.push(s);
    }
    tc(|| {
        let mut runs: Vec<(&SagaState, &SagaMemory, SagaFlags)> =
            states.iter().map(|(s, m, f)| (s, *m, *f)).collect();
        runs.extend(corners.iter().map(|s| (s, &all_seen, PLAIN)));
        for (state, memory, flags) in runs {
            for w in 1..=240u16 {
                for h in [1u16, 2, 5, 6, 11, 12, 23, 24, 39, 60] {
                    let frame = render_lead(state, memory, flags, (w, h), 3);
                    for (i, line) in frame.iter().enumerate() {
                        let mut col = 0usize;
                        for ch in text(std::slice::from_ref(line))[0].chars() {
                            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                            if col + cw == w as usize {
                                assert!(
                                    !RIGHT_EDGE_GLYPHS.contains(&ch),
                                    "{w}x{h} row {i} ends in {ch:?}"
                                );
                            }
                            col += cw;
                        }
                    }
                }
            }
        }
    });
}

// ---- Labels and banners ----------------------------------------------------

/// Spec 3.9: "(mock)" or "(replay)" in the title, nothing for live. The
/// title is the first line at every size that draws one.
#[test]
fn the_mock_label_shows_in_the_title() {
    for (label, mark) in [(Label::Mock, "(mock)"), (Label::Replay, "(replay)")] {
        let state = saga_after(usize::MAX, NameMode::Real, label)
            .state()
            .clone();
        for (w, h) in [(20, 1), (39, 11), (80, 23), (140, 39)] {
            let lines = tc(|| draw(&state, PLAIN, (w, h), 0));
            assert!(
                text(&lines)[0].contains(mark),
                "{w}x{h}: {:?}",
                text(&lines)
            );
        }
        let waiting = empty_state(label);
        let lines = tc(|| draw(&waiting, PLAIN, (80, 23), 0));
        assert!(
            text(&lines)[0].contains(mark),
            "waiting: {:?}",
            text(&lines)
        );
    }
    let live = saga_after(usize::MAX, NameMode::Real, Label::Live)
        .state()
        .clone();
    let first = &text(&tc(|| draw(&live, PLAIN, (140, 39), 0)))[0];
    assert!(
        !first.contains("(mock)") && !first.contains("(replay)"),
        "{first}"
    );
}

/// A stopped driver: a banner says so, and the last state is still drawn
/// (the hero and the chronicle are on screen).
#[test]
fn the_stopped_banner_shows_over_the_last_state() {
    let state = busy_state();
    let stopped = SagaFlags {
        stopped: true,
        why: false,
        ..SagaFlags::default()
    };
    for (w, h) in [(80, 23), (140, 39)] {
        let on = text(&tc(|| draw(&state, stopped, (w, h), 0))).join("\n");
        assert!(on.contains("the story has stopped"), "{w}x{h}: {on}");
        assert!(on.contains('@'), "the last state is drawn at {w}x{h}");
        assert!(
            on.contains("CHRONICLE"),
            "the chronicle is drawn at {w}x{h}"
        );
        let off = text(&tc(|| draw(&state, PLAIN, (w, h), 0))).join("\n");
        assert!(!off.contains("stopped"), "{w}x{h}: {off}");
    }
    // Small sizes say it on the title line.
    for (w, h) in [(30, 10), (25, 3)] {
        let first = text(&tc(|| draw(&state, stopped, (w, h), 0)))[0].clone();
        assert!(first.contains("stopped"), "{w}x{h}: {first}");
    }
}

// ---- The map ---------------------------------------------------------------

/// The hero stays in view while the floor (53 x 38 for a Blackhole die is
/// typical; the fixture's chips are generic) is larger than the map pane:
/// at every hundredth record of the busy log, every size with a map shows
/// the hero's `@`.
#[test]
fn the_hero_is_always_in_view() {
    tc(|| {
        let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
        for (i, r) in busy().iter().enumerate() {
            saga.feed(r);
            if i % 100 != 0 || !saga.state().has_world() {
                continue;
            }
            for (w, h) in [(20, 6), (39, 11), (80, 23), (100, 12), (140, 39)] {
                let all = text(&draw(saga.state(), PLAIN, (w, h), 0)).join("\n");
                assert!(all.contains('@'), "record {i} at {w}x{h}:\n{all}");
            }
        }
    });
}

/// The hero is drawn highlighted: bold, on a background.
#[test]
fn the_hero_is_highlighted() {
    let state = busy_state();
    let lines = tc(|| draw(&state, PLAIN, (140, 39), 0));
    let hero = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content == "@")
        .expect("the hero has its own span");
    assert!(hero.style.add_modifier.contains(Modifier::BOLD));
    assert!(hero.style.bg.is_some(), "{:?}", hero.style);
}

/// The quest's goal is marked: a sage quest draws the sage's `&` at the
/// goal, any other quest highlights the goal cell's background.
#[test]
fn the_quest_goal_is_marked() {
    tc(|| {
        let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
        let (mut sage, mut other) = (false, false);
        for r in busy() {
            saga.feed(r);
            let s = saga.state();
            if !s.has_world() || s.party[0].quest.goal == s.party[0].hero.pos {
                continue;
            }
            // A memory that saw only this state: the goal is drawn even
            // where the map is still dark.
            let lines = draw(s, PLAIN, (140, 39), 0);
            let has_style = |want: Style| {
                lines
                    .iter()
                    .flat_map(|l| l.spans.iter())
                    .any(|sp| sp.style == want)
            };
            let marked = has_style(goal_style(0));
            match s.party[0].quest.verb {
                Verb::VisitSage if !sage => {
                    let all = text(&lines).join("\n");
                    assert!(all.contains(Pack::builtin().glyphs.npc), "{all}");
                    assert!(has_style(sage_style(0)), "the sage is drawn pulsing");
                    sage = true;
                }
                Verb::Explore if !other => {
                    assert!(marked, "the explore goal is highlighted");
                    let all = text(&lines).join("\n");
                    assert!(all.contains(Pack::builtin().glyphs.goal), "{all}");
                    other = true;
                }
                _ => {}
            }
        }
        assert!(
            sage && other,
            "the busy log has a sage quest and an explore quest"
        );
    });
}

/// A hero floor index past the end of the world's floors (the core never
/// makes one, but the view must not disagree with itself if it did): the
/// title and the map both fall back to the first floor, as the core's
/// planner does. The title once clamped to the last floor while the map
/// showed the first.
#[test]
fn an_out_of_range_floor_is_the_first_floor_in_title_and_map() {
    let mut state = busy_state();
    let n = state.world.floors.len();
    assert!(
        n >= 2,
        "the busy world has two floors, so first and last differ"
    );
    let look = tc(|| Look::new(state.world.biome, None));
    let memory = SagaMemory::new();
    let first_title = tc(|| {
        let mut s = state.clone();
        s.party[0].hero.floor = 0;
        title_row(&s, &look, 200, false, Some(lead_seat(&s).floor)).line()
    });
    let first_map = tc(|| {
        let mut s = state.clone();
        s.party[0].hero.floor = 0;
        map_rows(&s, &memory, &look, lead_seat(&s), 140, 30, 0)
            .into_iter()
            .map(Row::line)
            .collect::<Vec<_>>()
    });
    state.party[0].hero.floor = u8::try_from(n + 3).unwrap_or(u8::MAX);
    assert_eq!(hero_floor_index(&state), Some(0));
    let title = tc(|| title_row(&state, &look, 200, false, Some(lead_seat(&state).floor)).line());
    let all = text(std::slice::from_ref(&title)).join("");
    assert!(all.contains(&format!("floor 1 of {n}")), "{all}");
    assert_eq!(text(&[title]), text(&[first_title]));
    let map = tc(|| {
        map_rows(&state, &memory, &look, lead_seat(&state), 140, 30, 0)
            .into_iter()
            .map(Row::line)
            .collect::<Vec<_>>()
    });
    assert_eq!(text(&map), text(&first_map));
}

// ---- The status line and names ---------------------------------------------

/// The bottom row names the hero and shows its vitality as a bar and its
/// quest. The world's id is on the title row.
#[test]
fn the_status_line_names_the_hero_and_the_quest() {
    let state = busy_state();
    let lines = text(&tc(|| draw(&state, PLAIN, (140, 39), 0)));
    let last = lines.last().expect("a frame");
    assert!(last.contains(&state.party[0].hero.name), "{last}");
    assert!(
        last.contains('▰') || last.contains('▱'),
        "a vitality bar: {last}"
    );
    assert!(
        lines[0].contains(&format!("world {}", state.world.id)),
        "{}",
        lines[0]
    );
}

/// The real names in the busy fixture: two model ids and a training run.
const REAL_NAMES: [&str; 3] = ["demo-sage", "demo-scribe", "demo-run"];

/// In generated mode no real name reaches the screen. The chronicle, the
/// "why" lines and the status line's quest are all scanned. The
/// state's `quest` keeps the real target in both modes (the saga module
/// comment, "Names"), so this scans every hundredth state of the busy log at
/// two sizes with "why" off and on. The real-name run is the control: the
/// same scan there finds the sage's real name on the status line.
#[test]
fn generated_mode_shows_no_real_name() {
    let scan = |names: NameMode| -> (String, String) {
        tc(|| {
            let mut saga = Saga::new(config(names, Label::Mock));
            let mut all = String::new();
            let mut status = String::new();
            for (i, r) in busy().iter().enumerate() {
                saga.feed(r);
                if i % 100 != 0 && i + 1 != busy().len() {
                    continue;
                }
                for why in [false, true] {
                    let flags = SagaFlags {
                        stopped: false,
                        why,
                        ..SagaFlags::default()
                    };
                    for (w, h) in [(80, 23), (140, 39)] {
                        let t = text(&draw(saga.state(), flags, (w, h), 0));
                        status.push_str(t.last().map_or("", |s| s.as_str()));
                        status.push('\n');
                        all.push_str(&t.join("\n"));
                    }
                }
            }
            (all, status)
        })
    };
    let (all, _) = scan(NameMode::Generated);
    for name in REAL_NAMES {
        assert!(!all.contains(name), "generated mode shows {name}");
    }
    let (real_all, real_status) = scan(NameMode::Real);
    assert!(real_all.contains("demo-sage"), "the control finds the name");
    assert!(
        real_status.contains("sage demo-sage"),
        "the control's status line names the sage it visits"
    );
}

// ---- The chronicle -----------------------------------------------------------

/// The pane shows whole beats: the first row under the header with text is
/// a beat's first row (it starts with its tick), so no cut-off tail of an
/// older beat sits at the top. Checked at every hundredth state of the busy
/// log, with "why" off and on, at two sizes.
#[test]
fn the_chronicle_shows_whole_beats() {
    tc(|| {
        let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
        for (i, r) in busy().iter().enumerate() {
            saga.feed(r);
            if i % 100 != 0 || !saga.state().has_world() {
                continue;
            }
            for why in [false, true] {
                let flags = SagaFlags {
                    stopped: false,
                    why,
                    ..SagaFlags::default()
                };
                for size in [(80, 23), (140, 39)] {
                    let t = text(&draw(saga.state(), flags, size, 0));
                    let head = t.iter().position(|l| l.contains("CHRONICLE")).unwrap();
                    // In the side layout the pane starts after the map pane.
                    // Map glyphs are several bytes long, so count characters.
                    let col = t[head].chars().position(|c| c == '╔').unwrap();
                    let first = t[head + 1..t.len() - 1]
                        .iter()
                        .map(|l| l.chars().skip(col).collect::<String>())
                        .map(|l| l.trim_start_matches("║ ").to_string())
                        .find(|s| !s.trim().is_empty() && s != "║");
                    if let Some(first) = first {
                        assert!(
                            first.starts_with('t'),
                            "record {i} {size:?} why {why}: the pane starts with {first:?}"
                        );
                    }
                }
            }
        }
    });
}

// ---- Why ---------------------------------------------------------------------

/// "why" puts each beat's cause under it, in plain words (`saga_why`).
/// Without it no cause is shown.
#[test]
fn the_why_toggle_shows_each_beats_cause() {
    let state = busy_state();
    let newest = state.chronicle.lines(1).next().expect("a beat").clone();
    let why = SagaFlags {
        stopped: false,
        why: true,
        ..SagaFlags::default()
    };
    let on = text(&tc(|| draw(&state, why, (140, 39), 0))).join("\n");
    // The line wraps at the pane's width: its start is what must be on screen.
    let cause: String = super::super::saga_why::why_line(&newest.cause)
        .chars()
        .take(32)
        .collect();
    assert!(on.contains(&cause), "missing {cause:?} in\n{on}");
    assert!(!on.contains(" · tt_"), "the raw form is still shown:\n{on}");
    let off = text(&tc(|| draw(&state, PLAIN, (140, 39), 0))).join("\n");
    assert!(!off.contains(&cause), "why is off:\n{off}");
    assert!(!off.contains("why: "), "why is off:\n{off}");
}

// ---- The driver is off -------------------------------------------------------

/// With `[saga] enabled = false` there is no driver, and the view says so
/// on one line with the way to turn it on.
#[test]
fn a_disabled_driver_gets_a_note() {
    let lines = tc(|| render_saga_off((80, 23), false));
    let all = text(&lines);
    let note: Vec<&String> = all.iter().filter(|l| l.contains("enabled")).collect();
    assert_eq!(note.len(), 1, "{all:#?}");
    assert!(note[0].contains("[saga] enabled = true"), "{}", note[0]);
    assert!(note[0].contains("config.toml"), "{}", note[0]);
    // A driver that is on but did not start says something else.
    let failed = text(&tc(|| render_saga_off((80, 23), true))).join("\n");
    assert!(!failed.contains("enabled = true"), "{failed}");
    assert!(failed.contains("log"), "{failed}");
}

// ---- Overlays and keys ---------------------------------------------------------

/// The Saga overlays fit the row targets at 80 columns (Help 24, Legend 30,
/// Explain 30), the same arithmetic as
/// `every_overlay_fits_its_row_target_at_80_columns`, which also lists Saga.
#[test]
fn the_overlays_fit_their_row_targets() {
    use super::super::{
        overlay_lines, overlay_row_target, overlay_rows_needed, overlay_wrap_cols, DisplayMode,
        OverlayPanel,
    };
    for kind in [
        OverlayPanel::Help,
        OverlayPanel::Legend,
        OverlayPanel::Explain,
    ] {
        let lines = overlay_lines(kind, DisplayMode::Saga, overlay_wrap_cols(80));
        assert!(
            overlay_rows_needed(lines.len()) <= overlay_row_target(kind),
            "{kind:?} needs {} rows",
            overlay_rows_needed(lines.len())
        );
        assert!(lines.len() > 3, "{kind:?} has Saga text");
        // The party's rows (Task A.7): its keys in Help and Explain, its
        // colours and roster in the legend.
        let all: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        let want: &[&str] = match kind {
            OverlayPanel::Help => &["[ ]", "focus the floor", "zoom"],
            OverlayPanel::Legend => &["THE PARTY", "F1 ", "F8 ", "roster"],
            OverlayPanel::Explain => &["F3", "z shows the focused floor"],
        };
        for w in want {
            assert!(all.contains(w), "{kind:?} has {w:?}: {all}");
        }
    }
}

/// The legend names every glyph the map draws, from the pack.
#[test]
fn the_legend_names_every_glyph() {
    let lines = text(&tc(|| {
        legend_lines(Color::Reset, Color::Reset, Color::Reset)
    }))
    .join("\n");
    for (role, glyph) in Pack::builtin().glyphs.all() {
        assert!(lines.contains(glyph), "{role} {glyph:?} in\n{lines}");
    }
}

/// The literal characters of every `KeyCode::Char('x')` pattern in the TUI
/// key handler (`run_app` in `mod.rs`), with the line each sits on.
fn handler_keys() -> Vec<(char, String)> {
    let src = include_str!("mod.rs");
    let start = src.find("\nfn run_app(").expect("run_app");
    let end = src[start..].find("\n}\n").map_or(src.len(), |i| start + i);
    let body = &src[start..end];
    let mut out = Vec::new();
    for line in body.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("KeyCode::Char('") {
            let after = &rest[at + "KeyCode::Char('".len()..];
            if let Some(c) = after.chars().next() {
                out.push((c, line.trim().to_string()));
            }
            rest = after;
        }
    }
    out
}

/// The entry key is free in every view: no arm of the key handler names it,
/// and no view's own key table (Die 3D's keys, the arcade's layout keys,
/// the backdrop key) takes it. The "why" key is taken elsewhere only by arms
/// guarded to another mode, so in Saga it means "why".
#[test]
fn the_entry_key_is_free() {
    use super::super::{arcade_step_for, die3d_key_for, saga_key_for, DisplayMode};
    let keys = handler_keys();
    assert!(keys.len() > 20, "the scan finds the handler's keys");
    assert!(
        !keys.iter().any(|(c, _)| *c == ENTRY_KEY),
        "another arm takes {ENTRY_KEY:?}: {keys:?}"
    );
    assert_ne!(crate::animation::backdrop::KEY, ENTRY_KEY);
    assert_ne!(crate::animation::backdrop::KEY, WHY_KEY);
    for m in super::super::ALL_DISPLAY_MODES {
        assert_eq!(die3d_key_for(m, ENTRY_KEY), None, "{m:?}");
        assert!(arcade_step_for(m, ENTRY_KEY).is_none(), "{m:?}");
        assert!(arcade_step_for(m, WHY_KEY).is_none(), "{m:?}");
        assert_eq!(die3d_key_for(m, WHY_KEY), None, "{m:?}");
        // The why key is a Saga key only.
        assert_eq!(saga_key_for(m, WHY_KEY), m == DisplayMode::Saga, "{m:?}");
    }
    for (c, line) in &keys {
        if *c == WHY_KEY {
            assert!(
                line.contains("if display_mode == DisplayMode::")
                    && !line.contains("DisplayMode::Saga"),
                "an arm takes {WHY_KEY:?} outside a guard to another mode: {line}"
            );
        }
    }
}

/// The party view's keys (`[`, `]` and `z`, Task A.7) are free in Saga:
/// no arm of the key handler names them, no view's own key table takes
/// them in Saga, the backdrop key is another, and `saga_key_for` takes
/// them there. They move the focus one floor back or on, wrapping.
#[test]
fn the_party_keys_are_free_in_saga_mode() {
    use super::super::{
        arcade_step_for, die3d_key_for, saga_focus_step, saga_key_for, DisplayMode,
    };
    let keys = handler_keys();
    for k in [PREV_FLOOR_KEY, NEXT_FLOOR_KEY, ZOOM_KEY] {
        assert!(
            !keys.iter().any(|(c, _)| *c == k),
            "an arm of the handler takes {k:?}"
        );
        assert_ne!(crate::animation::backdrop::KEY, k);
        assert_ne!(k, ENTRY_KEY);
        assert_ne!(k, WHY_KEY);
        assert_eq!(die3d_key_for(DisplayMode::Saga, k), None);
        assert!(arcade_step_for(DisplayMode::Saga, k).is_none());
        assert!(saga_key_for(DisplayMode::Saga, k), "Saga takes {k:?}");
    }
    // `]` on, `[` back, wrapping at both ends; a focus left past the end
    // by a smaller world starts from the last floor.
    assert_eq!(saga_focus_step(0, 4, true), 1);
    assert_eq!(saga_focus_step(3, 4, true), 0);
    assert_eq!(saga_focus_step(0, 4, false), 3);
    assert_eq!(saga_focus_step(9, 4, false), 2);
    assert_eq!(saga_focus_step(9, 4, true), 0);
    assert_eq!(saga_focus_step(0, 1, true), 0);
    assert_eq!(saga_focus_step(0, 0, false), 0);
}

/// Outside Saga the party keys keep their meanings: `saga_key_for` takes
/// none of them in another mode, so Die 3D still changes boards with `[`
/// and `]`.
#[test]
fn the_party_keys_are_not_taken_outside_saga() {
    use super::super::{die3d_key_for, saga_key_for, DisplayMode};
    for m in super::super::ALL_DISPLAY_MODES {
        if m == DisplayMode::Saga {
            continue;
        }
        for k in [PREV_FLOOR_KEY, NEXT_FLOOR_KEY, ZOOM_KEY] {
            assert!(!saga_key_for(m, k), "{m:?} takes {k:?}");
        }
    }
    assert!(die3d_key_for(DisplayMode::Die3d, PREV_FLOOR_KEY).is_some());
    assert!(die3d_key_for(DisplayMode::Die3d, NEXT_FLOOR_KEY).is_some());
}

/// The body of the function `name` in `mod.rs`: from `\nfn name(` to the
/// first line that is a lone `}` at the left margin.
fn mod_fn(name: &str) -> &'static str {
    let src = include_str!("mod.rs");
    let start = src
        .find(&format!("\nfn {name}("))
        .unwrap_or_else(|| panic!("{name} in mod.rs"));
    let end = src[start..].find("\n}\n").map_or(src.len(), |i| start + i);
    &src[start..end]
}

/// The party view's state (the focus, the zoom, the draw size) never
/// reaches the saga driver, so it cannot change a story or a digest (party
/// Review Focus item 2). `run_tui` needs a terminal, so the wiring is
/// pinned by its source, as `season_tag_tests` pins the season tag. The
/// scan covers:
///
/// * every line of `run_app` that names `saga_focus` or `saga_zoom`: each
///   is one of the declarations, the key arm's assignments or the two
///   fields of the single `SagaFlags` literal, so neither is ever an argument
///   of a call on the driver (`saga`, `d`) or of anything else;
/// * the single `SagaFlags` literal in `run_app`: its fields are exactly the
///   stopped flag, "why", the focus and the zoom (no backend count), and
///   the flags go straight to `ui_saga`;
/// * every line of `ui_saga`: its only call on the driver is
///   `d.published()`, and the flags and the draw size go only to
///   `render_saga` and `render_saga_off`;
/// * the driver's public methods (`src/saga/driver.rs`): exactly its
///   lifecycle and its read-only views, and the file never names
///   `SagaFlags`.
///
/// `the_core_never_reads_view_state` (`saga_party_tests.rs`) checks the
/// other half: drawing a state leaves its saga's digest alone.
#[test]
fn view_state_never_reaches_the_saga_driver() {
    let run_app = mod_fn("run_app");
    let allowed = [
        "let mut saga_focus: usize = 0;",
        "let mut saga_zoom = false;",
        "saga_focus = saga_focus_step(saga_focus, floors, false);",
        "saga_focus = saga_focus_step(saga_focus, floors, true);",
        "saga_view::ZOOM_KEY => saga_zoom = !saga_zoom,",
        "focus: saga_focus,",
        "zoom: saga_zoom,",
    ];
    let mut named = 0;
    for line in run_app.lines().map(str::trim) {
        if line.starts_with("//") {
            continue;
        }
        if line.contains("saga_focus") || line.contains("saga_zoom") {
            named += 1;
            assert!(
                allowed.contains(&line),
                "run_app passes the party view's state on: {line}"
            );
        }
    }
    assert_eq!(named, allowed.len(), "every allowed line is there once");

    // The single SagaFlags literal, its fields, and where it goes.
    let lit = "let flags = saga_view::SagaFlags {";
    assert_eq!(
        run_app.matches("SagaFlags").count(),
        1,
        "one SagaFlags in run_app"
    );
    let at = run_app.find(lit).expect("the SagaFlags literal");
    let rest = &run_app[at + lit.len()..];
    let close = rest.find("};").expect("the literal closes");
    let fields: Vec<&str> = rest[..close]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        fields,
        [
            "stopped: false,",
            "why: saga_why,",
            "focus: saga_focus,",
            "zoom: saga_zoom,"
        ]
    );
    let after = rest[close + 2..].trim_start();
    assert!(
        after.starts_with("ui_saga("),
        "the flags go to ui_saga: {after:.40}"
    );

    // ui_saga reads the driver once and hands the view state to the view.
    let ui = mod_fn("ui_saga");
    for line in ui.lines().map(str::trim) {
        if line.contains("d.") && !line.starts_with("//") {
            assert_eq!(line, "let p = d.published();", "ui_saga's driver calls");
        }
        let call = line.contains("render_saga(") || line.contains("render_saga_off(");
        if line.contains("size") && !line.starts_with("let size") {
            assert!(call, "the draw size goes only to the view: {line}");
        }
    }
    assert!(ui.contains("saga_view::render_saga(&p.state, party, flags, size, frame)"));

    // The driver takes nothing from a view.
    let driver = include_str!("../../saga/driver.rs");
    // Its methods: the indented `pub fn`s of its `impl` blocks.
    let mut pubs: Vec<&str> = driver
        .lines()
        .filter_map(|l| l.strip_prefix("    pub fn "))
        .map(|l| l.split('(').next().unwrap_or(""))
        .collect();
    pubs.sort_unstable();
    assert_eq!(
        pubs,
        [
            "published",
            "spawn",
            "spawn_remote",
            "state",
            "stop",
            "stopped",
            "wait_for"
        ],
        "the driver's public functions"
    );
    assert!(
        !driver.contains("SagaFlags"),
        "the driver never sees the view's flags"
    );
}

// ---- Walls, fog and memory ---------------------------------------------------

/// A floor `w` x `h` of open cells, with walls at `walls`. One room covers
/// the whole floor.
fn open_floor(w: u8, h: u8, walls: &[(u8, u8)]) -> Floor {
    let mut cells = vec![Cell::Floor; w as usize * h as usize];
    for &(x, y) in walls {
        cells[y as usize * w as usize + x as usize] = Cell::Wall;
    }
    Floor {
        w,
        h,
        cells,
        rooms: vec![tt_toplike_saga::maze::Rect { x: 0, y: 0, w, h }],
        entrance: (0, h - 1),
        exit: (w - 1, 0),
    }
}

/// The glyph each arm mask must draw inside a floor, written out here so a
/// swapped entry in the pack's table fails.
const INNER_TILES: [char; 16] = [
    '▪', '│', '─', '└', '│', '│', '┌', '├', '─', '┘', '─', '┴', '┐', '┤', '┬', '┼',
];

/// Every one of the 16 arm masks: a wall at the middle of an open 9 x 9
/// floor, with a two-cell wall arm in each direction the mask names, gets
/// exactly that mask and the matching thin-line glyph.
#[test]
fn wall_autotiling_picks_the_glyph_for_each_neighbour_mask() {
    use tt_toplike_saga::pack::{ARM_E, ARM_N, ARM_S, ARM_W};
    let glyphs = &Pack::builtin().glyphs;
    for mask in 0u8..16 {
        let mut walls = vec![(4u8, 4u8)];
        for (bit, dx, dy) in [
            (ARM_N, 0i8, -1i8),
            (ARM_E, 1, 0),
            (ARM_S, 0, 1),
            (ARM_W, -1, 0),
        ] {
            if mask & bit != 0 {
                for k in 1..=2i8 {
                    walls.push(((4 + dx * k) as u8, (4 + dy * k) as u8));
                }
            }
        }
        let floor = open_floor(9, 9, &walls);
        assert_eq!(wall_mask(&floor, 4, 4), mask, "mask {mask:04b}");
        assert_eq!(
            wall_glyph(glyphs, &floor, 4, 4),
            INNER_TILES[mask as usize],
            "mask {mask:04b}"
        );
    }
}

/// A wall two cells thick between two rooms draws two parallel lines and no
/// rungs between them; a wall cell no open cell touches is rock (blank); the
/// outer shell is double, with `╔` at a corner and `╤` where an inner wall
/// meets the top row; and a wall runs up to a door set in it.
#[test]
fn thick_walls_rock_shells_and_doors_are_drawn_as_architecture() {
    let glyphs = &Pack::builtin().glyphs;
    // A 2-thick wall at columns 3 and 4 of an open 8 x 5 floor.
    let walls: Vec<(u8, u8)> = (0..5).flat_map(|y| [(3, y), (4, y)]).collect();
    let floor = open_floor(8, 5, &walls);
    for y in 1..4 {
        assert_eq!(wall_glyph(glyphs, &floor, 3, y), '│', "left face row {y}");
        assert_eq!(wall_glyph(glyphs, &floor, 4, y), '│', "right face row {y}");
    }
    // Rock: the middle of a solid 5 x 5 block inside a 9 x 9 floor.
    let block: Vec<(u8, u8)> = (2..7).flat_map(|y| (2..7).map(move |x| (x, y))).collect();
    let floor = open_floor(9, 9, &block);
    assert_eq!(wall_glyph(glyphs, &floor, 4, 4), glyphs.rock);
    assert_eq!(glyphs.rock, ' ', "rock is blank");
    // A shell: walls all round a 7 x 5 floor, an inner wall down from the top
    // at column 3, and a door in the bottom row at column 3.
    let mut shell: Vec<(u8, u8)> = Vec::new();
    for x in 0..7 {
        shell.push((x, 0));
        shell.push((x, 4));
    }
    for y in 0..5 {
        shell.push((0, y));
        shell.push((6, y));
    }
    shell.push((3, 1));
    let mut floor = open_floor(7, 5, &shell);
    floor.cells[4 * 7 + 3] = Cell::Door;
    assert_eq!(wall_glyph(glyphs, &floor, 0, 0), '╔');
    assert_eq!(wall_glyph(glyphs, &floor, 6, 4), '╝');
    assert_eq!(wall_glyph(glyphs, &floor, 1, 0), '═');
    assert_eq!(wall_glyph(glyphs, &floor, 0, 2), '║');
    assert_eq!(
        wall_glyph(glyphs, &floor, 3, 0),
        '╤',
        "an inner wall meets the shell"
    );
    assert_eq!(wall_glyph(glyphs, &floor, 3, 1), '│');
    // The shell beside the door joins it: the line runs into the door.
    assert_eq!(
        wall_mask(&floor, 2, 4) & tt_toplike_saga::pack::ARM_E,
        tt_toplike_saga::pack::ARM_E,
        "the wall west of the door joins it"
    );
}

/// The busy log's end state with a memory built over the whole log, drawn
/// with the whole floor in view (one cell per column, no padding): each
/// cell's glyph and style, row-major, and the floor.
fn whole_floor_cells(state: &SagaState, memory: &SagaMemory) -> (Vec<Vec<(char, Style)>>, Floor) {
    let floor = hero_floor(state).expect("a floor").clone();
    let look = Look::for_state(state, memory);
    let rows = map_rows(
        state,
        memory,
        &look,
        lead_seat(state),
        floor.w as usize + 1,
        floor.h as usize,
        0,
    );
    let cells = rows
        .into_iter()
        .map(|r| {
            r.line()
                .spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                .collect()
        })
        .collect();
    (cells, floor)
}

/// A colour's brightness: the sum of its channels.
fn brightness(c: Option<Color>) -> u32 {
    match c {
        Some(Color::Rgb(r, g, b)) => r as u32 + g as u32 + b as u32,
        other => panic!("expected a truecolor colour, got {other:?}"),
    }
}

/// Fog: a walkable cell the hero has never lit is blank, a remembered floor
/// cell is dimmer than a lit one, and a lit cell next to nothing special is
/// drawn with the floor glyph. The trail shows behind the hero.
#[test]
fn fog_hides_the_unseen_and_dims_the_remembered() {
    tc(|| {
        let (saga, memory) = saga_and_memory(busy().len());
        let state = saga.state();
        let (cells, floor) = whole_floor_cells(state, &memory);
        let lit = light(&floor, state.party[0].hero.pos);
        let g = &Pack::builtin().glyphs;
        let (mut unseen, mut remembered, mut visible) = (0, Vec::new(), Vec::new());
        assert_eq!(cells.len(), floor.h as usize, "one row per floor row");
        for (y, row) in cells.iter().enumerate() {
            assert_eq!(row.len(), floor.w as usize, "one glyph per cell");
            for (x, &(ch, style)) in row.iter().enumerate() {
                let at = (x as u8, y as u8);
                if at == state.party[0].hero.pos
                    || at == state.party[0].quest.goal
                    || at == floor.exit
                {
                    continue;
                }
                let i = y * floor.w as usize + x;
                let plain_floor = floor.cells[i] == Cell::Floor
                    && memory.trail_age(at).is_none()
                    && x.abs_diff(state.party[0].hero.pos.0 as usize) > 1;
                if lit[i] == 0 && !memory.seen(x, y) {
                    assert_eq!(ch, ' ', "unseen ({x}, {y}) is blank");
                    unseen += 1;
                } else if plain_floor && lit[i] == 0 {
                    assert_eq!(ch, g.floor);
                    remembered.push(brightness(style.fg));
                } else if plain_floor && lit[i] > 0 {
                    assert_eq!(ch, g.floor);
                    visible.push(brightness(style.fg));
                }
            }
        }
        assert!(unseen > 0, "the busy floor still has unseen cells");
        assert!(!remembered.is_empty() && !visible.is_empty());
        let brightest_remembered = remembered.iter().max().unwrap();
        let dimmest_visible = visible.iter().min().unwrap();
        assert!(
            brightest_remembered < dimmest_visible,
            "remembered {brightest_remembered} is dimmer than visible {dimmest_visible}"
        );
        let all: String = cells.iter().flatten().map(|(c, _)| *c).collect();
        assert!(all.contains(g.trail), "the trail is drawn");
    });
}

/// The memory belongs to one floor of one world. Right after a new world is
/// born it holds exactly what the hero's light shows from where it stands,
/// and nothing from the world before; moving the hero to another floor
/// starts over too, with the light of every hero on that floor.
#[test]
fn the_explored_set_resets_on_a_new_world_and_floor() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    let mut memory = SagaMemory::new();
    let mut world = 0;
    let mut births = 0;
    for r in busy() {
        saga.feed(r);
        memory.observe(saga.state());
        let s = saga.state();
        if !s.has_world() || s.world.id == world {
            continue;
        }
        world = s.world.id;
        births += 1;
        let floor = hero_floor(s).unwrap();
        let lit = light(floor, s.party[0].hero.pos);
        for (i, &l) in lit.iter().enumerate() {
            let (x, y) = (i % floor.w as usize, i / floor.w as usize);
            assert_eq!(
                memory.seen(x, y),
                l > 0,
                "world {world}: ({x}, {y}) seen only if lit now"
            );
        }
        assert_eq!(memory.trails.len(), 1, "one hero on the floor");
        assert_eq!(memory.trails[0].steps.len(), 1, "a new trail");
    }
    assert!(births >= 2, "the busy log makes more than one world");
    // Another floor of the same world.
    let mut s = saga.state().clone();
    assert!(s.world.floors.len() >= 2);
    let before: usize = memory.seen.iter().filter(|&&b| b).count();
    s.party[0].hero.floor = if s.party[0].hero.floor == 0 { 1 } else { 0 };
    memory.observe(&s);
    let after: usize = memory.seen.iter().filter(|&&b| b).count();
    // Every hero on that floor lights it: the lead and the floor's own hero.
    let on: Vec<(u8, u8)> = s
        .on_floor(s.party[0].hero.floor)
        .map(|(_, m)| m.hero.pos)
        .collect();
    assert_eq!(on.len(), 2, "the lead joined the floor's own hero");
    assert_fresh(&memory, hero_floor(&s).unwrap(), &on, "another floor");
    assert!(
        before > after,
        "the old floor's map is gone: {before} -> {after}"
    );
}

/// Every character the view draws is one column wide: every role glyph and
/// tile in the pack, and every character of the busy frames with a full
/// memory, at three sizes.
#[test]
fn every_drawn_glyph_is_one_column_wide() {
    use unicode_width::UnicodeWidthChar;
    let g = &Pack::builtin().glyphs;
    for (role, c) in g.all() {
        assert_eq!(UnicodeWidthChar::width(c), Some(1), "{role} {c:?}");
    }
    for c in g.tiles() {
        assert_eq!(UnicodeWidthChar::width(c), Some(1), "tile {c:?}");
    }
    let state = busy_state();
    let mut memory = SagaMemory::new();
    memory.observe(&state);
    memory.see_everything();
    let why = SagaFlags {
        stopped: false,
        why: true,
        ..SagaFlags::default()
    };
    tc(|| {
        for (w, h) in [(80, 23), (140, 39), (200, 59)] {
            for flags in [PLAIN, why] {
                for line in render_lead(&state, &memory, flags, (w, h), 4) {
                    for span in &line.spans {
                        for c in span.content.chars() {
                            assert_eq!(UnicodeWidthChar::width(c), Some(1), "{c:?} at {w}x{h}");
                        }
                    }
                }
            }
            for line in legend_lines(Color::Reset, Color::Reset, Color::Reset) {
                for span in &line.spans {
                    for c in span.content.chars() {
                        assert_eq!(UnicodeWidthChar::width(c), Some(1), "legend {c:?}");
                    }
                }
            }
        }
    });
}

/// The right-border rule at the three sizes the brief names (80x24,
/// 140x40, 200x60, less the status bar), with every cell seen and the hero
/// at each corner of the floor: no row ends in a right-edge glyph, and the
/// map pane's last column is blank.
#[test]
fn the_map_keeps_its_last_column_blank() {
    use crate::animation::arcade::RIGHT_EDGE_GLYPHS;
    let state = busy_state();
    let floor = hero_floor(&state).unwrap().clone();
    let look = tc(|| Look::for_state(&state, &SagaMemory::new()));
    for pos in [(1, 1), (floor.w - 2, floor.h - 2), state.party[0].hero.pos] {
        let mut s = state.clone();
        s.party[0].hero.pos = pos;
        let mut memory = SagaMemory::new();
        memory.observe(&s);
        memory.see_everything();
        tc(|| {
            // The map pane alone, narrower than the floor so it scrolls.
            for cols in [20usize, 39, 52, 53, 54] {
                for row in map_rows(&s, &memory, &look, lead_seat(&s), cols, 30, 0) {
                    let t: String = row
                        .line()
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect();
                    let chars: Vec<char> = t.chars().collect();
                    if chars.len() == cols {
                        assert_eq!(chars[cols - 1], ' ', "cols {cols} pos {pos:?}: {t}");
                    }
                }
            }
            for (w, h) in [(80u16, 23u16), (140, 39), (200, 59)] {
                for line in render_lead(&s, &memory, PLAIN, (w, h), 0) {
                    let t: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    if t.chars().count() == w as usize {
                        let last = t.chars().last().unwrap();
                        assert!(!RIGHT_EDGE_GLYPHS.contains(&last), "{w}x{h}: {t}");
                    }
                }
            }
        });
    }
}

/// Truecolor draws RGB colours; without truecolor every colour is a
/// 256-colour index, and no glyph on the map (lit or remembered) is drawn
/// in black, the cube's floor, so the dim layers stay legible.
#[test]
fn truecolor_and_256_colour_frames_both_draw_the_map() {
    let (saga, memory) = saga_and_memory(busy().len());
    let state = saga.state();
    let colours = |on: bool| {
        colors::with_truecolor(on, || {
            colors::with_theme(Theme::Default, || {
                let (cells, _) = whole_floor_cells(state, &memory);
                let frame = render_lead(state, &memory, PLAIN, (140, 39), 0);
                (cells, frame)
            })
        })
    };
    let (cells, frame) = colours(true);
    for (c, style) in cells.iter().flatten() {
        if *c != ' ' {
            assert!(matches!(style.fg, Some(Color::Rgb(..))), "{c:?} {style:?}");
        }
    }
    let (cells, frame_256) = colours(false);
    // A glyph on its own background (the hero, the goal) may be dark on
    // light; every other glyph must not be black.
    for (c, style) in cells.iter().flatten() {
        if *c != ' ' && style.bg.is_none() {
            assert!(
                matches!(style.fg, Some(Color::Indexed(i)) if i != 16),
                "{c:?} is drawn in {style:?} without truecolor"
            );
        }
    }
    for line in &frame_256 {
        for span in &line.spans {
            for c in [span.style.fg, span.style.bg].into_iter().flatten() {
                assert!(
                    !matches!(c, Color::Rgb(..)),
                    "an RGB colour without truecolor"
                );
            }
        }
    }
    assert_eq!(text(&frame), text(&frame_256), "the same glyphs either way");
}

/// The goal pulses: its glyph and background change over a pulse and come
/// back, so a still story never looks frozen. The frame counter alone
/// drives it.
#[test]
fn the_goal_pulses_with_the_frame_counter() {
    let g = &Pack::builtin().glyphs;
    assert_ne!(goal_style(0), goal_style(PULSE_FRAMES / 2));
    assert_eq!(goal_style(0), goal_style(PULSE_FRAMES));
    assert_ne!(goal_glyph(g, 0), goal_glyph(g, PULSE_FRAMES / 2));
    assert_ne!(sage_style(0), sage_style(PULSE_FRAMES / 2));
}

/// The chronicle names in bold: the hero's name, and a lexicon place with
/// its adjective and its "of the" tail. Words around them stay plain.
#[test]
fn the_chronicle_bolds_names() {
    let hero = "Wren the Distilled";
    let text = "Wren the Distilled has mapped another room of Wormhole Deep of the LoRA.";
    let e = emphasis(text, hero);
    let words: Vec<&str> = text.split(' ').collect();
    let bold: Vec<&str> = words
        .iter()
        .zip(&e)
        .filter(|(_, b)| **b)
        .map(|(w, _)| *w)
        .collect();
    assert_eq!(
        bold,
        [
            "Wren",
            "the",
            "Distilled",
            "Wormhole",
            "Deep",
            "of",
            "the",
            "LoRA."
        ]
    );
    let e = emphasis("A new world pushes in, and world 1 gives way.", hero);
    assert!(e.iter().all(|b| !b), "no names here");
}

// ---- The golden frame -----------------------------------------------------------

/// The sizes and cut points the golden hashes pin: 80x23 and 140x39 (an
/// 80x24 and a 140x40 terminal less the status bar), after half the busy
/// log and after all of it, with "why" off and on. The memory observed the
/// state after every record, so the frames pin the fog, the remembered map
/// and the trail too.
const GOLDEN_SIZES: [(u16, u16); 2] = [(80, 23), (140, 39)];

fn golden_frames() -> Vec<u64> {
    tc(|| {
        let half = busy().len() / 2;
        let mut out = Vec::new();
        for cut in [half, busy().len()] {
            let (saga, party) = saga_and_party(&busy()[..cut]);
            for why in [false, true] {
                let flags = SagaFlags {
                    stopped: false,
                    why,
                    ..SagaFlags::default()
                };
                for size in GOLDEN_SIZES {
                    out.push(frame_hash(&render_saga(
                        saga.state(),
                        &party,
                        flags,
                        size,
                        5,
                    )));
                }
            }
        }
        out
    })
}

/// The hashes of [`golden_frames`]. They pin the drawing as it is now (Task
/// 1.14): a change to the view's look changes them, and they are re-pinned
/// on purpose with `golden_saga_hashes`.
///
/// Re-pinned in the final fix wave when the feed stopped giving `ViewChanged`
/// events an ordinal (finding C1). `busy.jsonl` holds one, at record 1,506,
/// before both cut points, so every later beat cause and world seed moved.
/// Re-pinned again when the seed ring began to keep each value's whole
/// 64-bit pattern (finding I1): every world seed in the log changed, so
/// every map did.
///
/// Re-pinned in 0.16.1 for the new look, with no change to the core: walls
/// drawn as box-drawing lines and rock left blank, the fog and the
/// remembered map (the frames now take a [`SagaMemory`] built over the log),
/// the trail and the hero's glow, the pulsing goal, the new glyphs, the
/// biome colours, the title band, the tone marks and bold names in the
/// chronicle, and the `▰▱` status row with the mapped share and the tick.
/// All eight frames moved.
///
/// Re-pinned in Task A.3 of the party plan (the party in the core). The
/// busy log has two chips, so every world now has a second member, the hero
/// of chip B's floor. Its quest beats join the chronicle these frames show,
/// and it shares the chronicle's generator, so the lead's later lines choose
/// other templates. A model's dungeon also spans both chips now. The view
/// still draws only the lead's floor (the party view is Task A.7). All eight
/// frames moved. The solo frames below did not.
///
/// Re-pinned in Task A.4 (the merged chronicle). Each busy world has two
/// members, so its birth writes one party line, and the pace leaves some of
/// chip B's member's quest lines unwritten (and some of the lead's, in
/// windows chip B owns). The chronicle these frames show changed, and the
/// generator's later draws with it. All eight frames moved. The solo frames
/// below did not.
///
/// Re-pinned in Task A.7 (the party view). The busy log's worlds have two
/// floors, so the view now draws them as two tiles (two side by side at
/// both sizes), each from its own floor's memory, with floor tags on the
/// chronicle's member lines and the party's keys on the status row. The
/// frames are drawn from a `PartyMemory` built over the log, as the TUI
/// draws them. All eight frames moved. The solo frames below did not.
///
/// Re-pinned in fix round 1 of Task A.7, for the same two changes as the
/// party frames: the status row's party keys come right after the quest
/// (short form at 80 columns), then the tick and the mapped share, and only
/// the focused tile shows `✓`. All eight frames moved. The solo frames did
/// not: a world of one floor has no keys and no tiles.
const GOLDEN: [u64; 8] = [
    11917712682132199184,
    11920290857694584326,
    16856285753327876517,
    624419604945615386,
    15220565761517867015,
    13439700959490747240,
    17051540309075168881,
    8921639599995668388,
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui golden_saga_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn golden_saga_hashes() {
    println!("{:?}", golden_frames());
}

#[test]
fn the_busy_frames_are_the_pinned_frames() {
    assert_eq!(golden_frames(), GOLDEN.to_vec());
}

// ---- The solo frames, pinned before the party ------------------------------------

/// Every record of the saga crate's solo fixture: one chip, 1,200 ticks,
/// made-up names. Read once per test process.
fn solo() -> &'static [Record] {
    static RECORDS: OnceLock<Vec<Record>> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/tt-toplike-saga/tests/fixtures/solo.jsonl");
        let read = read_segment(&path).expect("solo.jsonl is committed");
        assert_eq!(read.bad_lines, 0, "solo.jsonl decodes");
        read.records
    })
}

/// The four frames of the whole solo log, as [`golden_frames`] draws the
/// busy log: 80x23 and 140x39, "why" off and on, with a memory that observed
/// the state after every record.
fn solo_frames() -> Vec<u64> {
    tc(|| {
        let (saga, party) = saga_and_party(solo());
        let mut out = Vec::new();
        for why in [false, true] {
            let flags = SagaFlags {
                stopped: false,
                why,
                ..SagaFlags::default()
            };
            for size in GOLDEN_SIZES {
                out.push(frame_hash(&render_saga(
                    saga.state(),
                    &party,
                    flags,
                    size,
                    5,
                )));
            }
        }
        out
    })
}

/// The hashes of [`solo_frames`], taken on the single-chip code before the
/// party (0.16.4, with the why line as reworded in 0.16.3). One chip must
/// keep drawing exactly this through Phase A of the party work.
const SOLO_GOLDEN: [u64; 4] = [
    15638877983819768657,
    15019330008394832811,
    10571686743467978106,
    10878471527206318858,
];

/// Prints the hashes for [`SOLO_GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui print_solo_frames -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn print_solo_frames() {
    println!("{:?}", solo_frames());
}

#[test]
fn solo_frames_are_the_pre_party_frames() {
    assert_eq!(solo_frames(), SOLO_GOLDEN.to_vec());
}

/// The status row prints the quest's target label from the core (Task A.4
/// of the party plan). A label written into the state by hand shows on the
/// row, and with no label the row names no sage. On the solo log the label
/// equals what the view's old lookup found (the newest beat with the
/// reason's signal and ordinal), in both name modes, at every record, so one
/// chip's status row does not change.
#[test]
fn the_status_row_names_the_target_from_the_core() {
    let mut state = busy_state();
    {
        let q = &mut state.party[0].quest;
        q.verb = Verb::VisitSage;
        q.target = Some(tt_toplike_bus::EntityKey::model("demo-sage"));
        q.reason.entity = tt_toplike_bus::EntityKey::model("demo-sage");
        q.target_label = Some("Hand-Label".to_string());
    }
    assert_eq!(quest_target(state.lead()).as_deref(), Some("Hand-Label"));
    let row = |state: &SagaState| {
        let lines = text(&tc(|| draw(state, PLAIN, (140, 39), 0)));
        lines.last().cloned().unwrap_or_default()
    };
    assert!(
        row(&state).contains("visiting the sage Hand-Label"),
        "{}",
        row(&state)
    );
    state.party[0].quest.target_label = None;
    assert!(row(&state).contains("visiting a sage"), "{}", row(&state));

    // The old lookup, kept here to compare against.
    let old = |state: &SagaState| -> Option<String> {
        let q = &state.lead().quest;
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
    };
    for names in [NameMode::Real, NameMode::Generated] {
        let mut saga = Saga::new(config(names, Label::Mock));
        let mut named = 0;
        for r in solo() {
            saga.feed(r);
            let state = saga.state();
            assert_eq!(
                quest_target(state.lead()),
                old(state),
                "{names:?} at tick {}",
                state.tick
            );
            named += quest_target(state.lead()).is_some() as usize;
        }
        assert!(named > 0, "{names:?}: the solo hero never visits a sage");
    }
}

// ---- The arcade panel (0.16.4) ------------------------------------------------

/// A live feed of `state`, not stopped.
fn live(state: &SagaState, stopped: bool) -> PanelFeed {
    PanelFeed::Live {
        state: std::sync::Arc::new(state.clone()),
        stopped,
    }
}

/// The panel's frame with a memory that observed only `state`.
fn panel(state: &SagaState, stopped: bool, area: (u16, u16)) -> Vec<Line<'static>> {
    let mut memory = SagaMemory::new();
    memory.observe(state);
    render_saga_panel(&live(state, stopped), &memory, area, 3)
}

/// The panel's sizes: the HUD on the first row, then the map; a chronicle
/// strip under the map from 60x12, and beside it from 100 columns. Below
/// 60x12 there is no chronicle.
#[test]
fn the_panel_draws_the_hud_over_the_map_and_a_chronicle_when_it_has_room() {
    let state = busy_state();
    let hero = state.party[0].hero.name.clone();
    tc(|| {
        for (w, h, chronicle) in [
            (20, 6, false),
            (39, 11, false),
            (59, 16, false),
            (60, 12, true),
            (70, 16, true),
            (99, 30, true),
            (120, 30, true),
        ] {
            let t = text(&panel(&state, false, (w, h)));
            assert_eq!(t.len(), h as usize, "{w}x{h}: {t:#?}");
            assert!(t[0].starts_with(" (mock) "), "{w}x{h}: {:?}", t[0]);
            assert!(
                t[0].contains(&hero) || w < 30,
                "{w}x{h}: the HUD names the hero: {:?}",
                t[0]
            );
            assert!(t[0].contains('▰') || t[0].contains('▱') || w < 40);
            let has = t.iter().any(|l| l.contains("CHRONICLE"));
            assert_eq!(has, chronicle, "{w}x{h}: {t:#?}");
            // The arcade's `y` does nothing, so the panel gives no hint.
            assert!(!t.iter().any(|l| l.contains("y: why")), "{w}x{h}");
            assert!(t.iter().any(|l| l.contains('@')), "{w}x{h}: no hero");
        }
        // At 70x16: the HUD, ten map rows, the strip of five (header and
        // four rows) at the bottom.
        let t = text(&panel(&state, false, (70, 16)));
        assert!(t[11].starts_with("╔═ CHRONICLE"), "{t:#?}");
        // From 100 columns the strip sits right of the map.
        let t = text(&panel(&state, false, (120, 30)));
        let head = t.iter().position(|l| l.contains("CHRONICLE")).unwrap();
        assert_eq!(head, 1, "the strip starts under the HUD: {t:#?}");
        assert!(!t[1].starts_with('╔'), "it starts right of the map");
    })
}

/// Every panel size up to 160x50 fits its area, and no row ends in a
/// right-edge glyph: the map's last column is always blank and the strip's
/// header ends in `═`.
#[test]
fn every_panel_size_fits_and_no_row_ends_in_a_right_edge_glyph() {
    use crate::animation::arcade::RIGHT_EDGE_GLYPHS;
    let state = busy_state();
    let mut all_seen = SagaMemory::new();
    all_seen.observe(&state);
    all_seen.see_everything();
    tc(|| {
        for stopped in [false, true] {
            let feed = live(&state, stopped);
            for w in 1..=160u16 {
                for h in [1u16, 2, 5, 6, 11, 12, 16, 20, 31, 50] {
                    let lines = render_saga_panel(&feed, &all_seen, (w, h), 3);
                    assert_fits(&lines, w, h, "panel");
                    for (i, l) in lines.iter().enumerate() {
                        let t = text(std::slice::from_ref(l)).remove(0);
                        if width(l) == w as usize {
                            let last = t.chars().last().unwrap();
                            assert!(
                                !RIGHT_EDGE_GLYPHS.contains(&last),
                                "{w}x{h} row {i} ends in {last:?}: {t:?}"
                            );
                        }
                    }
                }
            }
        }
    })
}

/// The notes: before the first world, with `[saga] enabled = false`, with a
/// driver that did not start, and a stopped driver with no world. Each is
/// one line at a width that holds it. The off note wraps, so the way to
/// turn Saga on is never cut.
#[test]
fn the_panel_notes_say_why_there_is_no_map() {
    let memory = SagaMemory::new();
    let empty = empty_state(Label::Live);
    let one = |feed: &PanelFeed, w: u16| text(&tc(|| render_saga_panel(feed, &memory, (w, 11), 0)));
    assert_eq!(
        one(&PanelFeed::Waiting, 70),
        vec![format!(" {PANEL_WAITING}")]
    );
    assert_eq!(
        one(&live(&empty, false), 70),
        vec![format!(" {PANEL_WAITING}")]
    );
    assert_eq!(PANEL_WAITING, "saga waiting for the first world");
    assert_eq!(
        one(&PanelFeed::Off { enabled: false }, 70),
        vec![format!(" {PANEL_OFF}")]
    );
    assert!(PANEL_OFF.contains("[saga] enabled = true") && PANEL_OFF.contains("config.toml"));
    assert_eq!(
        one(&PanelFeed::Off { enabled: true }, 70),
        vec![format!(" {PANEL_NOT_RUNNING}")]
    );
    assert_eq!(
        one(&live(&empty, true), 70),
        vec![format!(" {PANEL_STOPPED_EARLY}")]
    );
    // At 39 columns the off note wraps, and all of it is there.
    let narrow = one(&PanelFeed::Off { enabled: false }, 39);
    assert!(narrow.len() > 1, "{narrow:?}");
    let joined: Vec<&str> = narrow.iter().map(|l| l.trim()).collect();
    assert_eq!(joined.join(" "), PANEL_OFF);
    // The notes take the arcade's note colour.
    let lines = tc(|| render_saga_panel(&PanelFeed::Waiting, &memory, (70, 11), 0));
    let note = lines[0].spans.last().unwrap();
    assert_eq!(note.style.fg, Some(tc(colors::text_secondary)));
}

/// A stopped driver with a world: the HUD, then the full view's stopped
/// banner, then the map of the last state.
#[test]
fn a_stopped_driver_shows_the_banner_over_the_last_map() {
    let state = busy_state();
    let t = text(&tc(|| panel(&state, true, (70, 16))));
    assert!(t[1].contains("the story has stopped"), "{t:#?}");
    assert!(t.iter().any(|l| l.contains('@')), "the last map is drawn");
    let running = text(&tc(|| panel(&state, false, (70, 16))));
    assert!(!running.iter().any(|l| l.contains("stopped")));
}

/// The panel shares the view's renderers: its map rows are the rows
/// `map_rows` gives the full view for the same size, cell for cell, and its
/// HUD is the full view's status row after the label mark.
#[test]
fn the_panel_map_and_hud_are_the_views_own() {
    let (saga, memory) = saga_and_memory(busy().len());
    let state = saga.state();
    tc(|| {
        let feed = PanelFeed::Live {
            state: std::sync::Arc::new(state.clone()),
            stopped: false,
        };
        let lines = render_saga_panel(&feed, &memory, (39, 11), 5);
        let look = Look::for_state(state, &memory);
        let want: Vec<Line<'static>> = map_rows(state, &memory, &look, lead_seat(state), 39, 10, 5)
            .into_iter()
            .map(Row::line)
            .collect();
        assert_eq!(frame_hash(&lines[1..]), frame_hash(&want));
        // Where the whole status row fits up to the quest (70 columns
        // here), the HUD is the status row after the mark.
        let wide = render_saga_panel(&feed, &memory, (70, 16), 5);
        let status = text(&[status_row(state, &memory, &look, 63, lead_seat(state), false).line()])
            .remove(0);
        let hud = text(&wide[..1]).remove(0);
        assert_eq!(hud, format!(" (mock){status}"));
    })
}

/// A HUD too narrow for the status row keeps the mark, the hero, the
/// vitality as a percentage and the quest: the whole name when it fits,
/// else its first word. At 20 columns the row is cut at the edge.
#[test]
fn a_narrow_hud_keeps_the_hero_the_vitality_and_the_quest() {
    let state = busy_state();
    let name = state.party[0].hero.name.clone();
    let first = name.split(' ').next().unwrap().to_string();
    let quest = quest_text(state.lead());
    let pct = format!(
        "{:>3.0}%",
        state.party[0].hero.vitality.clamp(0.0, 1.0) * 100.0
    );
    let hud = |w: u16| text(&tc(|| panel(&state, false, (w, 11))))[0].clone();
    // " (mock)" is 7 columns; " name 100% • quest" adds the rest.
    let roomy = (7 + 1 + name.chars().count() + 5 + 3 + quest.chars().count()) as u16;
    assert!(
        roomy < 60,
        "the busy hero's short form is narrower than the full row"
    );
    assert_eq!(hud(roomy), format!(" (mock) {name} {pct} • {quest}"));
    assert_eq!(hud(roomy - 1), format!(" (mock) {first} {pct} • {quest}"));
    assert!(hud(20).starts_with(" (mock) "), "{:?}", hud(20));
    assert!(!hud(roomy).contains('▰') && !hud(roomy).contains('▱'));
}

/// Generated names reach the panel exactly as they reach the view: no real
/// model or process name in the HUD or the chronicle strip, at every
/// hundredth state of the busy log and two panel sizes. The real-name run
/// is the control.
#[test]
fn the_panel_in_generated_mode_shows_no_real_name() {
    let scan = |names: NameMode| -> String {
        tc(|| {
            let mut saga = Saga::new(config(names, Label::Mock));
            let mut memory = SagaMemory::new();
            let mut all = String::new();
            for (i, r) in busy().iter().enumerate() {
                saga.feed(r);
                memory.observe(saga.state());
                if i % 100 != 0 && i + 1 != busy().len() {
                    continue;
                }
                let feed = live(saga.state(), false);
                for size in [(70, 16), (120, 30)] {
                    all.push_str(&text(&render_saga_panel(&feed, &memory, size, 0)).join("\n"));
                }
            }
            all
        })
    };
    let generated = scan(NameMode::Generated);
    for name in REAL_NAMES {
        assert!(!generated.contains(name), "the panel shows {name}");
    }
    assert!(
        scan(NameMode::Real).contains("demo-sage"),
        "the control finds the name"
    );
}

/// The panel's frames at 70x16 and at its smallest size, 20x6, after half
/// the busy log and after all of it, with a memory built over the log.
fn golden_panel_frames() -> Vec<u64> {
    tc(|| {
        let half = busy().len() / 2;
        let mut out = Vec::new();
        for cut in [half, busy().len()] {
            let (saga, memory) = saga_and_memory(cut);
            let feed = live(saga.state(), false);
            for size in [(70u16, 16u16), (PANEL_MIN.0 as u16, PANEL_MIN.1 as u16)] {
                out.push(frame_hash(&render_saga_panel(&feed, &memory, size, 5)));
            }
        }
        out
    })
}

/// The hashes of [`golden_panel_frames`], pinned when the panel was added
/// in 0.16.4 and looked at in a 140x40 and an 80x24 tmux capture.
///
/// Re-pinned before the first commit's merge, after the tmux capture at
/// 80x24 showed the 39-column HUD cut inside the vitality bar: the HUD
/// got its short form ([`hud_row`]). Three frames moved, the ones whose
/// status row does not fit to the end of its quest: 70x16 at the half cut
/// (its quest names the sage) and 20x6 at both cuts. 70x16 at the end of
/// the log did not move.
///
/// Re-pinned in Task A.3 of the party plan, for the reason given at
/// [`GOLDEN`]: chip B's member writes quest beats into the chronicle, and
/// shares its generator. The two 70x16 frames, which show the chronicle,
/// moved. The two 20x6 frames show only the lead's status row and did not.
///
/// Re-pinned in Task A.4, for the reason given at [`GOLDEN`]: the party
/// line and the pace change the chronicle strip. The two 70x16 frames moved
/// again. The two 20x6 frames did not, so the status row, which now prints
/// the core's target label, is unchanged on the busy log.
const GOLDEN_PANEL: [u64; 4] = [
    12535114590062944551,
    14813228560897824468,
    13927346534510494317,
    413437031925912577,
];

/// Prints the hashes for [`GOLDEN_PANEL`]. Run with
/// `cargo test --locked --lib --features tui golden_panel_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn golden_panel_hashes() {
    println!("{:?}", golden_panel_frames());
}

#[test]
fn the_panel_frames_are_the_pinned_frames() {
    assert_eq!(golden_panel_frames(), GOLDEN_PANEL.to_vec());
}

/// What a panel frame costs next to the full view's at the same size, in a
/// release build. It prints; it asserts nothing about time. Run with
/// `cargo test --release --locked --lib --features tui panel_cost -- --ignored --nocapture`.
#[test]
#[ignore = "prints timings"]
fn panel_cost() {
    let (saga, memory) = saga_and_memory(busy().len());
    let state = saga.state();
    let feed = live(state, false);
    let n = 2000;
    for size in [(70u16, 16u16), (140, 39)] {
        let t = std::time::Instant::now();
        for f in 0..n {
            std::hint::black_box(render_lead(state, &memory, PLAIN, size, f));
        }
        let view = t.elapsed() / n as u32;
        let t = std::time::Instant::now();
        for f in 0..n {
            std::hint::black_box(render_saga_panel(&feed, &memory, size, f));
        }
        let panel = t.elapsed() / n as u32;
        println!("{size:?}: view {view:?} a frame, panel {panel:?} a frame");
    }
}

// ---- One memory and one heat per floor (Task A.6 of the party plan) ---------

/// Every record of the saga crate's four-chip fixture: four floors, four
/// heroes, 600 ticks, made-up names. Read once per test process.
fn party4() -> &'static [Record] {
    static RECORDS: OnceLock<Vec<Record>> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/tt-toplike-saga/tests/fixtures/party4.jsonl");
        let read = read_segment(&path).expect("party4.jsonl is committed");
        assert_eq!(read.bad_lines, 0, "party4.jsonl decodes");
        read.records
    })
}

/// The four-chip records up to the birth of the log's last world, which
/// comes eight records before the end: at this cut every hero has walked a
/// while in the same world.
fn party4_explored() -> &'static [Record] {
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    let mut cut = 0;
    let mut world = 0;
    for (n, r) in party4().iter().enumerate() {
        saga.feed(r);
        if saga.state().world.id != world {
            world = saga.state().world.id;
            cut = n;
        }
    }
    &party4()[..cut]
}

/// How many cells `pos` lights on `floor`.
fn lit_count(floor: &Floor, pos: (u8, u8)) -> usize {
    light(floor, pos).iter().filter(|&&l| l > 0).count()
}

/// A saga fed every record of `records` and a party memory that observed
/// the state after each record, as the TUI's loop does on each update.
fn saga_and_party(records: &[Record]) -> (Saga, PartyMemory) {
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    let mut party = PartyMemory::new();
    for r in records {
        saga.feed(r);
        party.observe(saga.state());
    }
    (saga, party)
}

/// How many cells of a memory have been lit.
fn seen_count(memory: &SagaMemory) -> usize {
    memory.seen.iter().filter(|&&b| b).count()
}

/// Asserts that `memory` holds exactly the cells that `pos` lights on
/// `floor` and a one-step trail per hero: what a memory holds right after
/// it starts over.
fn assert_fresh(memory: &SagaMemory, floor: &Floor, pos: &[(u8, u8)], what: &str) {
    let mut want = vec![false; floor.w as usize * floor.h as usize];
    for &p in pos {
        for (i, l) in light(floor, p).into_iter().enumerate() {
            want[i] |= l > 0;
        }
    }
    assert_eq!(memory.seen, want, "{what}: only what the light shows now");
    assert_eq!(memory.trails.len(), pos.len(), "{what}: one trail per hero");
    for t in &memory.trails {
        assert_eq!(t.steps.len(), 1, "{what}: each trail starts over");
    }
}

/// Each floor of a party has its own memory: it is kept under its own floor
/// index, it is the size of its own floor, it holds the light of its own
/// hero, and it equals a lone memory that followed that floor alone. Two
/// heroes on one floor both light it and each keeps its own trail.
#[test]
fn each_floor_remembers_its_own_cells() {
    let (saga, party) = saga_and_party(party4_explored());
    let state = saga.state();
    assert_eq!(state.world.floors.len(), 4, "party4 has four floors");
    let mut lone: Vec<SagaMemory> = (0..4).map(|_| SagaMemory::new()).collect();
    let mut s = Saga::new(config(NameMode::Real, Label::Mock));
    for r in party4_explored() {
        s.feed(r);
        for (i, m) in lone.iter_mut().enumerate() {
            m.observe_floor(s.state(), i);
        }
    }
    for i in 0..4 {
        let memory = party.floor(i).expect("one memory per floor");
        let floor = &state.world.floors[i];
        assert_eq!(
            memory.key,
            Some((state.world.id, state.world.seed, i)),
            "floor {i}: kept under its own index"
        );
        assert_eq!((memory.w, memory.h), (floor.w as usize, floor.h as usize));
        let pos = state.party[i].hero.pos;
        for (c, l) in light(floor, pos).into_iter().enumerate() {
            if l > 0 {
                assert!(memory.seen[c], "floor {i}: its own hero's light is seen");
            }
        }
        assert_eq!(memory.trails.len(), 1, "floor {i}: one hero, one trail");
        assert_eq!(
            memory.trails[0].member, i,
            "floor {i}: its own hero's trail"
        );
        assert_eq!(memory.trails[0].steps.front(), Some(&pos));
        assert_eq!(memory.seen, lone[i].seen, "floor {i}: as a lone memory");
        assert_eq!(memory.trails, lone[i].trails, "floor {i}: as a lone memory");
        assert!(
            seen_count(memory) > lit_count(floor, pos),
            "floor {i} remembers more than its hero's light now"
        );
    }
    assert!(party.floor(4).is_none(), "no memory past the last floor");

    // Two heroes on floor 0: hero 1 is put on a walkable cell of floor 0
    // away from hero 0.
    let mut two = state.clone();
    let floor = two.world.floors[0].clone();
    let lead = two.party[0].hero.pos;
    let far = (0..floor.h)
        .flat_map(|y| (0..floor.w).map(move |x| (x, y)))
        .filter(|&(x, y)| floor.cells[y as usize * floor.w as usize + x as usize].walkable())
        .max_by_key(|&(x, y)| x.abs_diff(lead.0) as u32 + y.abs_diff(lead.1) as u32)
        .expect("a walkable cell");
    two.party[1].hero.floor = 0;
    two.party[1].hero.pos = far;
    two.world.seed ^= 1;
    let mut memory = SagaMemory::new();
    memory.observe_floor(&two, 0);
    assert_fresh(&memory, &floor, &[lead, far], "two heroes on floor 0");
    assert_eq!(
        memory.trails.iter().map(|t| t.member).collect::<Vec<_>>(),
        vec![0, 1],
        "the trails are in member order"
    );
    // Hero 1 leaves floor 0: its trail goes and hero 0's stays.
    two.party[1].hero.floor = 1;
    memory.observe_floor(&two, 0);
    assert_eq!(memory.trails.len(), 1);
    assert_eq!(memory.trails[0].member, 0);
}

/// A new world (a new id or a new seed) clears the memory of every floor:
/// right after it, each floor holds only what its hero's light shows.
#[test]
fn a_new_world_clears_every_floor_memory() {
    let (saga, mut party) = saga_and_party(party4_explored());
    let state = saga.state().clone();
    for i in 0..4 {
        assert!(
            seen_count(party.floor(i).unwrap())
                > lit_count(&state.world.floors[i], state.party[i].hero.pos),
            "floor {i} remembers more than its light before the new world"
        );
    }
    let mut next = state.clone();
    next.world.seed = next.world.seed.wrapping_add(1);
    party.observe(&next);
    for i in 0..4 {
        let pos = next.party[i].hero.pos;
        assert_fresh(
            party.floor(i).unwrap(),
            &next.world.floors[i],
            &[pos],
            &format!("new seed, floor {i}"),
        );
    }
    // Walk the old world again, then a new id with the same seed.
    let (_, mut party) = saga_and_party(party4_explored());
    let mut next = state.clone();
    next.world.id += 1;
    party.observe(&next);
    for i in 0..4 {
        assert_fresh(
            party.floor(i).unwrap(),
            &next.world.floors[i],
            &[next.party[i].hero.pos],
            &format!("new id, floor {i}"),
        );
    }
    // A world with fewer floors keeps only its own floors' memories.
    let mut small = next.clone();
    small.world.id += 1;
    small.world.floors = small.world.floors[..2].to_vec().into();
    small.world.chips.truncate(2);
    small.party.truncate(2);
    party.observe(&small);
    assert!(party.floor(1).is_some() && party.floor(2).is_none());
}

/// Each floor keeps the temperature of its own chip. `set_heats` takes the
/// reading of each floor's chip (`floor_chip`), a floor past the last one
/// is ignored, a reading that is not a number is dropped, and the readings
/// survive the next update of the same world.
#[test]
fn heat_is_per_floor() {
    let (saga, mut party) = saga_and_party(party4_explored());
    let state = saga.state();
    for i in 0..4 {
        assert_eq!(floor_chip(state, i), state.world.chips.get(i));
    }
    assert_eq!(floor_chip(state, 4), None, "no chip past the last floor");
    assert_eq!(
        hero_floor_index(state).and_then(|f| floor_chip(state, f)),
        floor_chip(state, 0),
        "the lead's chip"
    );
    assert_eq!(floor_chip(&empty_state(Label::Mock), 0), None);

    // One reading per chip, told apart by the chip's index.
    let temps = [41.0f32, 57.5, f32::NAN, 88.0];
    party.set_heats(state, |chip| {
        let i = state.world.chips.iter().position(|c| c == chip)?;
        Some(temps[i])
    });
    let heats: Vec<Option<f32>> = (0..4).map(|i| party.floor(i).unwrap().heat).collect();
    assert_eq!(heats, vec![Some(41.0), Some(57.5), None, Some(88.0)]);

    party.set_heat(1, None);
    party.set_heat(3, Some(20.0));
    party.set_heat(9, Some(99.0));
    party.observe(state);
    let heats: Vec<Option<f32>> = (0..4).map(|i| party.floor(i).unwrap().heat).collect();
    assert_eq!(heats, vec![Some(41.0), None, None, Some(20.0)]);

    // The view draws the lead's floor in the heat of the lead's chip.
    party.set_heat(0, Some(80.0));
    let warm = Look::for_state(state, party.lead(state));
    let plain = Look::new(state.world.biome, None);
    assert_ne!(format!("{:?}", warm.floor), format!("{:?}", plain.floor));
}

/// With one chip, the party memory's only floor is the memory the view drew
/// before the party: the solo log draws the same frames from either, and
/// the memory the view picks for the lead (`PartyMemory::lead`) is that
/// floor's.
///
/// Both sides run the same `observe_floor`, so this guards the
/// `PartyMemory` wrapper and `render_saga`'s choice of the single-floor path
/// (a party memory that started over on every update, or a one-floor world
/// sent to the grid, turns it red). "The same as before the party" is
/// guarded by the pinned frames, `solo_frames_are_the_pre_party_frames`.
#[test]
fn a_party_memory_of_one_equals_a_saga_memory() {
    tc(|| {
        let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
        let mut party = PartyMemory::new();
        let mut memory = SagaMemory::new();
        for (n, r) in solo().iter().enumerate() {
            saga.feed(r);
            party.observe(saga.state());
            memory.observe(saga.state());
            if n % 97 == 0 {
                let a = render_saga(saga.state(), &party, PLAIN, (80, 23), 5);
                let b = render_lead(saga.state(), &memory, PLAIN, (80, 23), 5);
                assert_eq!(frame_hash(&a), frame_hash(&b), "record {n}");
            }
        }
        let state = saga.state();
        assert_eq!(state.world.floors.len(), 1);
        assert!(party.floor(1).is_none());
        let lead = party.lead(state);
        assert!(std::ptr::eq(lead, party.floor(0).unwrap()));
        for why in [false, true] {
            let flags = SagaFlags {
                stopped: false,
                why,
                ..SagaFlags::default()
            };
            for size in GOLDEN_SIZES {
                let a = render_saga(state, &party, flags, size, 5);
                let b = render_lead(state, lead, flags, size, 5);
                let c = render_lead(state, &memory, flags, size, 5);
                assert_eq!(frame_hash(&b), frame_hash(&c), "{size:?} why {why}");
                assert_eq!(frame_hash(&a), frame_hash(&b), "{size:?} why {why}");
            }
        }
        // Before the first world the lead memory is an empty one.
        let empty = empty_state(Label::Mock);
        let fresh = PartyMemory::new();
        assert!(fresh.lead(&empty).key.is_none());
    });
}
