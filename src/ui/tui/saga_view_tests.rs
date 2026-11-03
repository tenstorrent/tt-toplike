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

const PLAIN: SagaFlags = SagaFlags {
    stopped: false,
    why: false,
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
        let lines = tc(|| render_saga(&state, PLAIN, (w, h), 0));
        let all = text(&lines).join("\n");
        assert!(
            all.contains("the chronicle has not begun"),
            "{w}x{h}: {all}"
        );
        let glyphs = &Pack::builtin().glyphs;
        assert!(!all.contains(glyphs.hero), "no hero yet at {w}x{h}: {all}");
        assert!(!all.contains(glyphs.wall), "no map yet at {w}x{h}: {all}");
    }
}

/// A stopped driver before any world still says the story stopped.
#[test]
fn the_waiting_screen_says_when_the_story_stopped() {
    let state = empty_state(Label::Live);
    let flags = SagaFlags {
        stopped: true,
        why: false,
    };
    let all = text(&tc(|| render_saga(&state, flags, (80, 23), 0))).join("\n");
    assert!(all.contains("the story has stopped"), "{all}");
}

// ---- Sizes -----------------------------------------------------------------

/// 0x0 and 1x1 draw nothing; below about 20x6 the view is one title line;
/// below about 40x12 it is a map with its title; at 80x24 the map sits above
/// the chronicle; from 100 columns the chronicle is beside the map.
#[test]
fn each_size_gets_its_layout() {
    let state = busy_state();
    assert!(tc(|| render_saga(&state, PLAIN, (0, 0), 0)).is_empty());
    assert!(tc(|| render_saga(&state, PLAIN, (1, 1), 0)).is_empty());
    assert!(tc(|| render_saga(&state, PLAIN, (5, 0), 0)).is_empty());
    assert_eq!(tc(|| render_saga(&state, PLAIN, (19, 30), 0)).len(), 1);
    assert_eq!(tc(|| render_saga(&state, PLAIN, (60, 5), 0)).len(), 1);

    // Map only: no chronicle header.
    let small = text(&tc(|| render_saga(&state, PLAIN, (39, 20), 0))).join("\n");
    assert!(!small.contains("CHRONICLE"), "{small}");
    assert!(small.contains('@'), "the hero is on the small map: {small}");

    // 80x23 (24 rows less the status bar): the chronicle under the map.
    let stacked = text(&tc(|| render_saga(&state, PLAIN, (80, 23), 0)));
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
    let side = text(&tc(|| render_saga(&state, PLAIN, (140, 39), 0)));
    let header = side
        .iter()
        .find(|l| l.contains("CHRONICLE"))
        .expect("140x39 has a chronicle");
    assert!(
        header.contains('#'),
        "the header row also carries map cells: {header}"
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
                assert_fits(&render_saga(&busy, PLAIN, (w, h), 7), w, h, "busy");
                assert_fits(&render_saga(&busy, why, (w, h), 7), w, h, "busy+why");
                assert_fits(&render_saga(&waiting, why, (w, h), 7), w, h, "waiting");
                assert_fits(&render_saga_off((w, h), false), w, h, "off");
            }
        }
    });
}

/// Left and bottom bars only (`/home/ttuser/CLAUDE.md`): no row may put a
/// right-side border glyph in the area's last column, the arcade's rule
/// with its shared list ([`crate::animation::arcade::RIGHT_EDGE_GLYPHS`]).
/// The map's walls are `#`, which is not in that list, so a wall in the
/// last column is a map cell. The chronicle's `║` is its left bar and never
/// lands in the last column.
#[test]
fn no_row_ends_in_a_right_side_border() {
    use crate::animation::arcade::RIGHT_EDGE_GLYPHS;
    use unicode_width::UnicodeWidthChar;
    let states = [
        (empty_state(Label::Mock), PLAIN),
        (busy_state(), PLAIN),
        (
            busy_state(),
            SagaFlags {
                stopped: true,
                why: true,
            },
        ),
    ];
    tc(|| {
        for (state, flags) in &states {
            for w in 1..=240u16 {
                for h in [1u16, 2, 5, 6, 11, 12, 23, 24, 39, 60] {
                    for (i, line) in render_saga(state, *flags, (w, h), 3).iter().enumerate() {
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
            let lines = tc(|| render_saga(&state, PLAIN, (w, h), 0));
            assert!(
                text(&lines)[0].contains(mark),
                "{w}x{h}: {:?}",
                text(&lines)
            );
        }
        let waiting = empty_state(label);
        let lines = tc(|| render_saga(&waiting, PLAIN, (80, 23), 0));
        assert!(
            text(&lines)[0].contains(mark),
            "waiting: {:?}",
            text(&lines)
        );
    }
    let live = saga_after(usize::MAX, NameMode::Real, Label::Live)
        .state()
        .clone();
    let first = &text(&tc(|| render_saga(&live, PLAIN, (140, 39), 0)))[0];
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
    };
    for (w, h) in [(80, 23), (140, 39)] {
        let on = text(&tc(|| render_saga(&state, stopped, (w, h), 0))).join("\n");
        assert!(on.contains("the story has stopped"), "{w}x{h}: {on}");
        assert!(on.contains('@'), "the last state is drawn at {w}x{h}");
        assert!(
            on.contains("CHRONICLE"),
            "the chronicle is drawn at {w}x{h}"
        );
        let off = text(&tc(|| render_saga(&state, PLAIN, (w, h), 0))).join("\n");
        assert!(!off.contains("stopped"), "{w}x{h}: {off}");
    }
    // Small sizes say it on the title line.
    for (w, h) in [(30, 10), (25, 3)] {
        let first = text(&tc(|| render_saga(&state, stopped, (w, h), 0)))[0].clone();
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
                let all = text(&render_saga(saga.state(), PLAIN, (w, h), 0)).join("\n");
                assert!(all.contains('@'), "record {i} at {w}x{h}:\n{all}");
            }
        }
    });
}

/// The hero is drawn highlighted: bold, on a background.
#[test]
fn the_hero_is_highlighted() {
    let state = busy_state();
    let lines = tc(|| render_saga(&state, PLAIN, (140, 39), 0));
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
            if !s.has_world() || s.quest.goal == s.hero.pos {
                continue;
            }
            let lines = render_saga(s, PLAIN, (140, 39), 0);
            let goal_style = goal_style();
            let marked = lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|sp| sp.style == goal_style);
            match s.quest.verb {
                Verb::VisitSage if !sage => {
                    let all = text(&lines).join("\n");
                    assert!(all.contains(Pack::builtin().glyphs.npc), "{all}");
                    assert!(marked, "the sage is drawn in the goal style");
                    sage = true;
                }
                Verb::Explore if !other => {
                    assert!(marked, "the explore goal is highlighted");
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
    let look = tc(Look::new);
    let first_title = tc(|| {
        let mut s = state.clone();
        s.hero.floor = 0;
        title_row(&s, &look, 200, false).line()
    });
    let first_map = tc(|| {
        let mut s = state.clone();
        s.hero.floor = 0;
        map_rows(&s, &look, 140, 30, 0)
            .into_iter()
            .map(Row::line)
            .collect::<Vec<_>>()
    });
    state.hero.floor = u8::try_from(n + 3).unwrap_or(u8::MAX);
    assert_eq!(hero_floor_index(&state), Some(0));
    let title = tc(|| title_row(&state, &look, 200, false).line());
    let all = text(std::slice::from_ref(&title)).join("");
    assert!(all.contains(&format!("floor 1 of {n}")), "{all}");
    assert_eq!(text(&[title]), text(&[first_title]));
    let map = tc(|| {
        map_rows(&state, &look, 140, 30, 0)
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
    let lines = text(&tc(|| render_saga(&state, PLAIN, (140, 39), 0)));
    let last = lines.last().expect("a frame");
    assert!(last.contains(&state.hero.name), "{last}");
    assert!(
        last.contains('█') || last.contains('░'),
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
                    };
                    for (w, h) in [(80, 23), (140, 39)] {
                        let t = text(&render_saga(saga.state(), flags, (w, h), 0));
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
                };
                for size in [(80, 23), (140, 39)] {
                    let t = text(&render_saga(saga.state(), flags, size, 0));
                    let head = t.iter().position(|l| l.contains("CHRONICLE")).unwrap();
                    // In the side layout the pane starts after the map pane.
                    let col = t[head].find("╔").unwrap();
                    let first = t[head + 1..t.len() - 1]
                        .iter()
                        .map(|l| l.get(col..).unwrap_or("").trim_start_matches("║ "))
                        .find(|s| !s.trim().is_empty() && *s != "║");
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

/// "why" puts each beat's cause under it: the entity, the signal and the
/// stream ordinal. Without it no cause is shown.
#[test]
fn the_why_toggle_shows_each_beats_cause() {
    let state = busy_state();
    let newest = state.chronicle.lines(1).next().expect("a beat").clone();
    let why = SagaFlags {
        stopped: false,
        why: true,
    };
    let on = text(&tc(|| render_saga(&state, why, (140, 39), 0))).join("\n");
    let cause = format!(
        "{} · {} · #{}",
        newest.cause.entity.as_str(),
        newest.cause.signal,
        newest.cause.ordinal
    );
    assert!(on.contains(&cause), "missing {cause:?} in\n{on}");
    let off = text(&tc(|| render_saga(&state, PLAIN, (140, 39), 0))).join("\n");
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

// ---- The golden frame -----------------------------------------------------------

/// The sizes and cut points the golden hashes pin: 80x23 and 140x39 (an
/// 80x24 and a 140x40 terminal less the status bar), after half the busy
/// log and after all of it, with "why" off and on.
const GOLDEN_SIZES: [(u16, u16); 2] = [(80, 23), (140, 39)];

fn golden_frames() -> Vec<u64> {
    tc(|| {
        let half = busy().len() / 2;
        let mut out = Vec::new();
        for cut in [half, busy().len()] {
            let saga = saga_after(cut, NameMode::Real, Label::Mock);
            for why in [false, true] {
                let flags = SagaFlags {
                    stopped: false,
                    why,
                };
                for size in GOLDEN_SIZES {
                    out.push(frame_hash(&render_saga(saga.state(), flags, size, 5)));
                }
            }
        }
        out
    })
}

/// The hashes of [`golden_frames`]. They pin the drawing as it is now (Task
/// 1.14): a change to the view's look changes them, and they are re-pinned
/// on purpose with `golden_saga_hashes`.
const GOLDEN: [u64; 8] = [
    17747875752450834049,
    8173874115476919137,
    9729913553127627753,
    3579161061891527866,
    15336144298665871893,
    4379483890101020010,
    10166685013815897719,
    14900404724890073878,
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
