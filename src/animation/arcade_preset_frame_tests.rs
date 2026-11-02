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
        cloudfield: false,
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

/// A few updates with a serving model, a training run and the cloudfield
/// on, so every panel has something to draw.
fn busy(a: &mut ArcadeVisualization, b: &Fixed) {
    a.set_cloudfield(true);
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

/// Characters that would be a right-side frame edge at the end of a row.
/// The same set `arcade_panel_tests` uses.
const RIGHT_BORDERS: &str = "│║▕┤╣";

/// Every layout, the presets and a user one: config, then presets.
fn every_layout() -> Vec<NamedLayout> {
    let mine = NamedLayout {
        name: "mine".into(),
        description: String::new(),
        rows: crate::animation::arcade_layout::example_rows(),
        backdrop: None,
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
                    // One known exception: the defrag view's own status
                    // line (`  DEFRAG │ 2 devices │ ...`) cut by a narrow
                    // panel right after a field separator. That is the
                    // view's text, drawn so in the pinned base frame
                    // (classic at 24x8), and no frame edge.
                    let defrag_cut = t.trim_end().ends_with("DEFRAG │");
                    if w > 0
                        && y < h
                        && cols.len() >= w
                        && RIGHT_BORDERS.contains(cols[w - 1])
                        && !defrag_cut
                    {
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
            // The cloudfield fills blank cells, the spaces of a label too.
            a.set_cloudfield(false);
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
            // The sub-views the arcade built are the ones named.
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
        // A kind other than the old cloudfield, so the whole backdrop is
        // seen to move over.
        a.set_backdrop(Some(crate::animation::backdrop::BackdropKind::Clouds));

        let a = a.relayout(120, 40, &config_of(deck), &b);
        assert_eq!(a.layout(), &arcade_layout::layout(&deck.rows, 120, 40));
        assert_eq!(a.config().rows, deck.rows);
        assert!(a.die3d.is_some() && a.train.is_some());
        assert_eq!((a.hero_x, a.hero_y), (30.0, 20.0));
        assert_eq!((a.hero_target_x, a.hero_target_y), (33.0, 21.0));
        assert_eq!(a.hero_trail.len(), trail);
        assert_eq!(
            a.backdrop().kind(),
            Some(crate::animation::backdrop::BackdropKind::Clouds),
            "the backdrop is kept"
        );
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
