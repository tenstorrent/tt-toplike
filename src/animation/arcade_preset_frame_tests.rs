// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Arcade frames drawn with each named layout, and a switch between
//! layouts (`ArcadeVisualization::relayout`).

use super::bus_tests::Fixed;
use super::*;
use crate::animation::arcade_presets::{presets, LayoutCycle, NamedLayout};
use crate::workload::inference_server::mock::mock_services;
use ratatui::text::Line;

fn tc<R>(f: impl FnOnce() -> R) -> R {
    crate::ui::colors::with_theme(crate::ui::colors::Theme::Default, || {
        crate::ui::colors::with_truecolor(true, f)
    })
}

fn config_of(l: &NamedLayout) -> ArcadeConfig {
    ArcadeConfig {
        rows: l.rows.clone(),
    }
}

/// An arcade laid out by `l`, set up as the TUI sets one up.
fn arcade_of(l: &NamedLayout, w: usize, h: usize, b: &Fixed) -> ArcadeVisualization {
    let mut a = ArcadeVisualization::with_config(w, h, &config_of(l));
    a.initialize_from_devices(b.devices());
    a.initialize_topology(b);
    a
}

/// A few updates with a serving model and a training run, so every panel
/// has something to draw.
fn busy(a: &mut ArcadeVisualization, b: &Fixed) {
    let run = crate::workload::train::MockTrainRun::new();
    for i in 0..4 {
        a.set_services(mock_services(100.0 + i as f32));
        a.set_train_state(Some(run.state_at(40.0)));
        a.set_clock(i as f64 / 20.0);
        a.update(b);
    }
}

fn text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The characters of `line`, one per column (a wide character takes two).
fn columns(line: &Line<'static>) -> Vec<char> {
    let mut row = Vec::new();
    for s in &line.spans {
        for ch in s.content.chars() {
            row.push(ch);
            for _ in 1..unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) {
                row.push('\0');
            }
        }
    }
    row
}

/// `ch` is one of the right-edge border glyphs the arcade's guard clears
/// ([`RIGHT_EDGE_GLYPHS`], which the guard reads too).
fn is_right_border(ch: char) -> bool {
    RIGHT_EDGE_GLYPHS.contains(&ch)
}

/// Every layout, the presets and a user one: config, then presets.
fn every_layout() -> Vec<NamedLayout> {
    let mine = NamedLayout {
        name: "mine".into(),
        description: String::new(),
        rows: crate::animation::arcade_layout::example_rows(),
    };
    LayoutCycle::new(&[mine], None).layouts().to_vec()
}

const SIZES: [(usize, usize); 12] = [
    (0, 0),
    (1, 1),
    (2, 2),
    (5, 3),
    (12, 6),
    (24, 8),
    (40, 12),
    (60, 20),
    (80, 24),
    (120, 40),
    (160, 50),
    (200, 60),
];

/// Every layout at every size: no row wider than the terminal (a rule may
/// run one column per wide emoji past the edge, as the arcade's rules always
/// have; ratatui clips it), every panel inside the terminal above the
/// footer, no right-side border in the last column, and the frame draws
/// into a terminal-sized buffer without a panic.
#[test]
fn every_layout_at_every_size_fits_and_draws_no_right_border() {
    tc(|| {
        let b = Fixed::new(2);
        let mut bad = Vec::new();
        for l in every_layout() {
            for (w, h) in SIZES {
                let mut a = arcade_of(&l, w, h, &b);
                busy(&mut a, &b);
                let lines = a.render(&b);
                let lay = a.layout().clone();
                for p in lay.panels() {
                    let r = p.rect;
                    if r.w > 0 && r.h > 0 && (r.x + r.w > w || r.y + r.h > h.saturating_sub(1)) {
                        bad.push(format!("{} {w}x{h}: {} at {r:?}", l.name, p.view));
                    }
                }
                let fixed =
                    arcade_layout::HEADER_ROWS + lay.rows.len() + arcade_layout::FOOTER_ROWS;
                if h >= fixed && lay.lines() + arcade_layout::FOOTER_ROWS > h {
                    bad.push(format!("{} {w}x{h}: {} lines", l.name, lay.lines()));
                }
                let rules: Vec<usize> = lay.rows.iter().map(|r| r.rule_y).collect();
                for (y, line) in lines.iter().enumerate() {
                    let t = text(line);
                    let width = unicode_width::UnicodeWidthStr::width(t.as_str());
                    let slack = if rules.contains(&y) {
                        t.chars()
                            .filter(|c| unicode_width::UnicodeWidthChar::width(*c) == Some(2))
                            .count()
                    } else {
                        0
                    };
                    if width > w + slack {
                        bad.push(format!("{} {w}x{h} row {y} is {width} wide", l.name));
                    }
                    let cols = columns(line);
                    if w > 0 && y < h && cols.len() >= w && is_right_border(cols[w - 1]) {
                        bad.push(format!(
                            "{} {w}x{h} row {y} ends in {:?}: {t:?}",
                            l.name,
                            cols[w - 1]
                        ));
                    }
                }
                // As `ui_arcade` draws it.
                use ratatui::widgets::{Paragraph, Widget};
                let area = ratatui::layout::Rect::new(0, 0, w as u16, h as u16);
                let mut buf = ratatui::buffer::Buffer::empty(area);
                Paragraph::new(lines).render(area, &mut buf);
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    })
}

/// One frame per preset at 200x60: each panel the preset names is drawn,
/// its label is on its row's rule, and no panel is hidden.
#[test]
fn each_preset_draws_every_panel_it_names() {
    tc(|| {
        let b = Fixed::new(2);
        for l in presets() {
            let mut a = arcade_of(l, 200, 60, &b);
            busy(&mut a, &b);
            // The hero and its trail may sit on a rule; put them in the
            // footer row for this frame.
            a.hero_trail.clear();
            (a.hero_x, a.hero_y) = (199.0, 59.0);
            let lines = a.render(&b);
            for (i, row) in l.rows.iter().enumerate() {
                let laid = &a.layout().rows[i];
                assert!(laid.hidden.is_empty(), "{}: {:?}", l.name, laid.hidden);
                let rule = text(&lines[laid.rule_y]);
                for p in &row.panels {
                    assert!(
                        rule.contains(p.view.label()),
                        "{}: {} not on {rule:?}",
                        l.name,
                        p.view
                    );
                    let shown = laid.panels.iter().find(|q| q.view == p.view).unwrap();
                    // The panel has content: some cell in it is not blank.
                    let drawn = shown.rect.rows().any(|y| {
                        let cols = columns(&lines[y]);
                        shown
                            .rect
                            .cols()
                            .any(|x| cols.get(x).is_some_and(|c| *c != ' ' && *c != '\0'))
                    });
                    assert!(drawn, "{}: {} draws nothing", l.name, p.view);
                }
            }
            // The arcade built a sub-view for each named panel and no other.
            let named: Vec<View> = l
                .rows
                .iter()
                .flat_map(|r| r.panels.iter().map(|p| p.view))
                .collect();
            assert_eq!(
                a.starfield.is_some(),
                named.contains(&View::Starfield),
                "{}",
                l.name
            );
            assert_eq!(
                a.memory_castle.is_some(),
                named.contains(&View::Castle),
                "{}",
                l.name
            );
            assert_eq!(
                a.memory_flow.is_some(),
                named.contains(&View::Flow),
                "{}",
                l.name
            );
            assert_eq!(
                a.defrag.is_some(),
                named.contains(&View::Defrag),
                "{}",
                l.name
            );
            assert_eq!(
                a.die3d.is_some(),
                named.contains(&View::Die3d),
                "{}",
                l.name
            );
            assert_eq!(
                a.train.is_some(),
                named.contains(&View::Training),
                "{}",
                l.name
            );
            assert_eq!(
                a.snake.is_some(),
                named.contains(&View::Inference),
                "{}",
                l.name
            );
            assert_eq!(a.saga.is_some(), named.contains(&View::Saga), "{}", l.name);
        }
    })
}

/// A switch keeps the hero where it was (scaled to a new size), the
/// backdrop, the sensitivity, the bus state and the running state, and
/// lays the arcade out from the new rows.
#[test]
fn a_switch_keeps_the_hero_the_backdrop_and_the_bus_state() {
    tc(|| {
        let b = Fixed::new(2);
        let classic = &presets()[0];
        let deck = crate::animation::arcade_presets::preset("deck").unwrap();
        let mut a = arcade_of(classic, 120, 40, &b);
        a.set_sensitivity(2.5);
        busy(&mut a, &b);
        a.hero_x = 30.0;
        a.hero_y = 20.0;
        a.hero_target_x = 33.0;
        a.hero_target_y = 21.0;
        a.bus_now = 77;
        a.bus_state = ArcadeBusState::default();
        a.power_peak = 123.0;
        let frame = a.frame;
        let trail = a.hero_trail.len();
        assert!(trail > 0);

        let a = a.relayout(120, 40, &config_of(deck), &b);
        assert_eq!(a.layout(), &arcade_layout::layout(&deck.rows, 120, 40));
        assert_eq!(a.config().rows, deck.rows);
        assert!(a.die3d.is_some() && a.train.is_some());
        assert_eq!((a.hero_x, a.hero_y), (30.0, 20.0));
        assert_eq!((a.hero_target_x, a.hero_target_y), (33.0, 21.0));
        assert_eq!(a.hero_trail.len(), trail);
        assert_eq!(a.sensitivity, 2.5);
        assert_eq!(a.bus_now, 77);
        assert_eq!(a.power_peak, 123.0);
        assert_eq!(a.frame, frame);
        assert!(!a.services.is_empty() && a.train_state.is_some());

        // At another size the hero's place is scaled.
        let a = a.relayout(60, 20, &config_of(classic), &b);
        assert_eq!((a.hero_x, a.hero_y), (15.0, 10.0));
        assert_eq!((a.hero_target_x, a.hero_target_y), (16.5, 10.5));
        assert!(a.hero_trail.iter().all(|t| t.x < 60.0 && t.y < 20.0));
        assert_eq!((a.width(), a.height()), (60, 20));
    })
}

/// Switching through every layout and back at every size, drawing right
/// after each switch (the TUI draws before the next update), never panics,
/// and next then prev gives the layout it started with.
#[test]
fn switching_through_every_layout_at_any_size_never_panics() {
    tc(|| {
        let b = Fixed::new(2);
        for (w, h) in SIZES {
            let mut cycle = LayoutCycle::new(&every_layout()[..1], None);
            let start = cycle.current().clone();
            let mut a = arcade_of(&start, w, h, &b);
            busy(&mut a, &b);
            for _ in 0..cycle.len() {
                cycle.next();
                a = a.relayout(w, h, &config_of(cycle.current()), &b);
                let _ = a.render(&b);
                a.update(&b);
                let _ = a.render(&b);
            }
            assert_eq!(cycle.current(), &start, "a lap ends where it started");
            cycle.next();
            cycle.prev();
            a = a.relayout(w, h, &config_of(cycle.current()), &b);
            assert_eq!(
                a.layout(),
                &arcade_layout::layout(&start.rows, w, h),
                "{w}x{h}"
            );
        }
        // To and from 0x0.
        let a = arcade_of(&presets()[4], 80, 24, &b);
        let a = a.relayout(0, 0, &config_of(&presets()[4]), &b);
        let _ = a.render(&b);
        let a = a.relayout(80, 24, &config_of(&presets()[1]), &b);
        let _ = a.render(&b);
    })
}

/// The frame after a switch to the same layout equals the frame the arcade
/// would have drawn without the switch, so a switch draws nothing stray.
#[test]
fn a_switch_to_the_same_layout_draws_the_same_scene_rows() {
    tc(|| {
        let b = Fixed::new(2);
        let classic = &presets()[0];
        let mut a = arcade_of(classic, 80, 24, &b);
        for _ in 0..3 {
            a.update(&b);
        }
        let before = a.render(&b);
        let a = a.relayout(80, 24, &config_of(classic), &b);
        let after = a.render(&b);
        // The title, Row 1 and every rule are drawn from state the switch
        // keeps, so they are the same; the panels are new views.
        let mut same = vec![0, 1];
        same.extend(a.layout().rows.iter().map(|r| r.rule_y));
        for y in same {
            assert_eq!(text(&before[y]), text(&after[y]), "row {y}");
        }
        assert_eq!(before.len(), after.len());
    })
}

/// Every width from 1 to 240 at several heights, every built-in layout
/// idle and busy and a user layout with dividers: no row ends in a
/// right-side border glyph (review S3; the user's rule is never to use
/// one). The base grid samples fewer widths; this sweep found the cut
/// separators at widths 18, 23, 24, 31, 32, 47, 48 and on.
#[test]
fn no_row_ends_in_a_right_border_at_any_width() {
    let layouts = every_layout();
    let bad: Vec<String> = tc(|| {
        std::thread::scope(|s| {
            let workers: Vec<_> = layouts
                .iter()
                .map(|l| {
                    s.spawn(move || {
                        crate::ui::colors::with_truecolor(true, || {
                            let b = Fixed::new(2);
                            let mut bad = Vec::new();
                            for h in [3usize, 6, 8, 12, 24, 40] {
                                for w in 1..=240usize {
                                    for is_busy in [false, true] {
                                        let mut a = arcade_of(l, w, h, &b);
                                        if is_busy {
                                            busy(&mut a, &b);
                                        } else {
                                            for _ in 0..3 {
                                                a.update(&b);
                                            }
                                        }
                                        for (y, line) in a.render(&b).iter().enumerate() {
                                            let cols = columns(line);
                                            if y < h
                                                && cols.len() >= w
                                                && is_right_border(cols[w - 1])
                                            {
                                                bad.push(format!(
                                                    "{} {w}x{h} busy={is_busy} row {y}: {:?}",
                                                    l.name,
                                                    text(line)
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                            bad
                        })
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|t| t.join().unwrap())
                .collect()
        })
    });
    assert!(
        bad.is_empty(),
        "{} rows end in a right border, the first: {:?}",
        bad.len(),
        &bad[..bad.len().min(10)]
    );
}

/// The glyphs AGENTS.md names as right-side borders: the characters of each
/// backtick span made of glyphs (no letter, digit or `_`, so a code name
/// such as `clear_right_edge` is skipped) on a line that says "right-side
/// border". Today that is the BBS box's `┐ │ ┘`.
fn agents_md_right_borders() -> Vec<char> {
    let doc = include_str!("../../AGENTS.md");
    let mut named = Vec::new();
    for line in doc.lines().filter(|l| l.contains("right-side border")) {
        // The text between each pair of backticks.
        for (i, part) in line.split('`').enumerate() {
            let glyphs = part
                .chars()
                .all(|c| c.is_whitespace() || !(c.is_alphanumeric() || c == '_'));
            if i % 2 == 1 && glyphs {
                named.extend(part.chars().filter(|c| !c.is_whitespace()));
            }
        }
    }
    named
}

/// The guard's list holds every right-side border AGENTS.md names, and the
/// corners and ASCII bar that a cut can leave in the last column.
#[test]
fn the_right_edge_list_holds_every_border_agents_md_names() {
    let named = agents_md_right_borders();
    // The extraction found the BBS box's glyphs, so this test cannot pass
    // on an empty list.
    for ch in ['┐', '│', '┘'] {
        assert!(
            named.contains(&ch),
            "AGENTS.md no longer names {ch:?}: {named:?}"
        );
    }
    let missing: Vec<char> = named
        .iter()
        .copied()
        .filter(|c| !RIGHT_EDGE_GLYPHS.contains(c))
        .collect();
    assert!(missing.is_empty(), "not in RIGHT_EDGE_GLYPHS: {missing:?}");
    for ch in "│┐┘┤║╗╝╣┃┓┛┫╮╯▐▕|".chars() {
        assert!(
            RIGHT_EDGE_GLYPHS.contains(&ch),
            "not in RIGHT_EDGE_GLYPHS: {ch:?}"
        );
    }
}

// ---- The Saga panel (0.16.4) ---------------------------------------------------

/// The saga state after every record of the saga crate's busy fixture, mock
/// label, with `names`, and a memory that observed each state on the way,
/// as the TUI's loop does.
fn busy_saga(
    names: tt_toplike_saga::chronicle::NameMode,
) -> (std::sync::Arc<tt_toplike_saga::SagaState>, SagaMemory) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/tt-toplike-saga/tests/fixtures/busy.jsonl");
    let read = tt_toplike_bus::segment::read_segment(&path).expect("busy.jsonl");
    let mut saga = tt_toplike_saga::Saga::new(tt_toplike_saga::SagaConfig {
        season_ticks: 200,
        names,
        label: tt_toplike_saga::Label::Mock,
    });
    let mut memory = SagaMemory::new();
    for r in &read.records {
        saga.feed(r);
        memory.observe(saga.state());
    }
    (std::sync::Arc::new(saga.state().clone()), memory)
}

/// The cells of `rect` in `lines`, one string per row.
fn cells(lines: &[Line<'static>], rect: arcade_layout::Rect) -> Vec<String> {
    rect.rows()
        .map(|y| {
            let cols = columns(&lines[y]);
            rect.cols()
                .map(|x| match cols.get(x) {
                    Some('\0') | None => ' ',
                    Some(c) => *c,
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// `now-serving` with a live saga fed through `set_saga`, as the TUI feeds
/// it: at 80x24, 140x40 and 200x60 the Saga panel shows what
/// `render_saga_panel` draws for its rectangle (the wiring, cell for cell),
/// its label is on the rule, and no row ends in a right-edge glyph.
#[test]
fn now_serving_draws_the_fed_saga_and_no_right_edge_glyph() {
    tc(|| {
        let b = Fixed::new(2);
        let ns = crate::animation::arcade_presets::preset("now-serving").unwrap();
        let (state, memory) = busy_saga(tt_toplike_saga::chronicle::NameMode::Real);
        for (w, h) in [(80, 24), (140, 40), (200, 60)] {
            let mut a = arcade_of(ns, w, h, &b);
            busy(&mut a, &b);
            a.set_saga(
                PanelFeed::Live {
                    state: state.clone(),
                    stopped: false,
                },
                &memory,
            );
            a.hero_trail.clear();
            (a.hero_x, a.hero_y) = ((w - 1) as f32, (h - 1) as f32);
            let lines = a.render(&b);
            let p = *a.layout().find(View::Saga).expect("a Saga panel");
            let rule = text(&lines[a.layout().row_of(&p).unwrap().rule_y]);
            assert!(rule.contains("Ψ SAGA"), "{w}x{h}: {rule:?}");
            let want = saga_view::render_saga_panel(
                &PanelFeed::Live {
                    state: state.clone(),
                    stopped: false,
                },
                &memory,
                (p.rect.w as u16, p.rect.h as u16),
                u64::from(a.frame),
            );
            let want_cells: Vec<String> = want
                .iter()
                .map(|l| text(l).trim_end().to_string())
                .chain(std::iter::repeat(String::new()))
                .take(p.rect.h)
                .collect();
            assert_eq!(cells(&lines, p.rect), want_cells, "{w}x{h}");
            assert!(want_cells.iter().any(|l| l.contains('@')), "{w}x{h}");
            for (y, line) in lines.iter().enumerate() {
                let cols = columns(line);
                assert!(
                    !(cols.len() >= w && is_right_border(cols[w - 1])),
                    "{w}x{h} row {y} ends in {:?}: {:?}",
                    cols[w - 1],
                    text(line)
                );
            }
        }
    })
}

/// Before the first feed the Saga panel says it is waiting; a feed with no
/// driver says why; a layout switch keeps what was fed, so the frame after
/// `{` or `}` still shows the map. A layout without a Saga panel ignores a
/// feed.
#[test]
fn the_saga_panel_waits_takes_a_feed_and_keeps_it_across_a_switch() {
    tc(|| {
        let b = Fixed::new(2);
        let ns = crate::animation::arcade_presets::preset("now-serving").unwrap();
        let panel_text = |a: &ArcadeVisualization| {
            let lines = a.render(&b);
            cells(&lines, a.layout().find(View::Saga).unwrap().rect).join("\n")
        };
        let mut a = arcade_of(ns, 140, 40, &b);
        a.update(&b);
        assert!(panel_text(&a).contains(saga_view::PANEL_WAITING));
        a.set_saga(PanelFeed::Off { enabled: false }, &SagaMemory::new());
        assert!(panel_text(&a).contains("[saga] enabled = true"));
        let (state, memory) = busy_saga(tt_toplike_saga::chronicle::NameMode::Real);
        a.set_saga(
            PanelFeed::Live {
                state,
                stopped: true,
            },
            &memory,
        );
        assert!(panel_text(&a).contains("the story has stopped"));
        let a = a.relayout(120, 40, &config_of(ns), &b);
        let after = panel_text(&a);
        assert!(after.contains("the story has stopped"), "{after}");
        assert!(after.contains('@'), "{after}");
        // classic has no Saga panel: the feed is dropped, nothing breaks.
        let mut c = arcade_of(&presets()[0], 120, 40, &b);
        c.set_saga(PanelFeed::Off { enabled: true }, &memory);
        assert!(c.saga.is_none());
        let _ = c.render(&b);
    })
}

/// In generated mode the panel inside the arcade shows no real name, as the
/// full view shows none.
#[test]
fn the_saga_panel_in_the_arcade_shows_no_real_name_in_generated_mode() {
    tc(|| {
        let b = Fixed::new(2);
        let ns = crate::animation::arcade_presets::preset("now-serving").unwrap();
        for (names, control) in [
            (tt_toplike_saga::chronicle::NameMode::Generated, false),
            (tt_toplike_saga::chronicle::NameMode::Real, true),
        ] {
            let (state, memory) = busy_saga(names);
            let mut a = arcade_of(ns, 200, 60, &b);
            a.update(&b);
            a.set_saga(
                PanelFeed::Live {
                    state,
                    stopped: false,
                },
                &memory,
            );
            let all: String = a.render(&b).iter().map(text).collect::<Vec<_>>().join("\n");
            for name in ["demo-sage", "demo-scribe", "demo-run"] {
                if !control {
                    assert!(!all.contains(name), "generated mode shows {name}");
                }
            }
            if control {
                assert!(
                    ["demo-sage", "demo-scribe", "demo-run"]
                        .iter()
                        .any(|n| all.contains(n)),
                    "the control finds a real name:\n{all}"
                );
            }
        }
    })
}
