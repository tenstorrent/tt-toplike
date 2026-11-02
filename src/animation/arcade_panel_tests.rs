// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Arcade frames with a configured layout: one test per new panel kind, the
//! cloudfield, the width ratio, and every panel kind at several sizes.

use super::bus_tests::{frame_hash, Fixed};
use super::*;
use crate::animation::arcade_layout::{example_rows, PanelSpec, RowSpec};
use crate::workload::inference_server::{mock::mock_services, Phase};
use ratatui::text::Line;

fn tc<R>(f: impl FnOnce() -> R) -> R {
    crate::ui::colors::with_theme(crate::ui::colors::Theme::Default, || {
        crate::ui::colors::with_truecolor(true, f)
    })
}

fn config(rows: Vec<RowSpec>) -> ArcadeConfig {
    ArcadeConfig {
        cloudfield: false,
        rows,
    }
}

fn one(view: View) -> Vec<RowSpec> {
    vec![RowSpec {
        height: 1,
        panels: vec![PanelSpec { view, width: 1 }],
    }]
}

fn arcade_with(rows: Vec<RowSpec>, w: usize, h: usize) -> (ArcadeVisualization, Fixed) {
    let backend = Fixed::new(2);
    let mut a = ArcadeVisualization::with_config(w, h, &config(rows));
    a.initialize_from_devices(backend.devices());
    a.initialize_topology(&backend);
    (a, backend)
}

/// The cells of `rect` in `lines`, as (char, style) rows.
fn cells_in(lines: &[Line<'static>], rect: arcade_layout::Rect) -> Vec<Vec<(char, Style)>> {
    rect.rows()
        .map(|y| {
            let mut row: Vec<(char, Style)> = Vec::new();
            if let Some(l) = lines.get(y) {
                for s in &l.spans {
                    for ch in s.content.chars() {
                        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                        row.push((ch, l.style.patch(s.style)));
                        for _ in 1..w {
                            row.push(('\0', s.style));
                        }
                    }
                }
            }
            row.resize(rect.x + rect.w, (' ', Style::default()));
            row[rect.x..rect.x + rect.w].to_vec()
        })
        .collect()
}

/// `lines` (a view's own output) as cells, `w` wide and `h` high.
fn cells_of(lines: &[Line<'static>], w: usize, h: usize) -> Vec<Vec<(char, Style)>> {
    cells_in(lines, arcade_layout::Rect { x: 0, y: 0, w, h })
}

fn text_in(lines: &[Line<'static>], rect: arcade_layout::Rect) -> String {
    cells_in(lines, rect)
        .iter()
        .map(|r| r.iter().map(|(c, _)| *c).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_die3d_panel_draws_what_the_die3d_view_draws_at_that_size() {
    tc(|| {
        // A model ready on chip 0, so the scene carries the bus overlay.
        let (mut a, b) = arcade_with(one(View::Die3d), 100, 40);
        let rect = a.layout().find(View::Die3d).unwrap().rect;
        let mut solo = crate::animation::Die3dView::new(rect.w, rect.h)
            .with_transparent(crate::ui::colors::canvas_transparent())
            .with_theme(crate::ui::colors::current_theme())
            .with_cell_mode(crate::animation::die3d::cellmode::CellMode::Auto);
        let mut world = tt_toplike_bus::World::new();
        world.apply(&tt_toplike_bus::Record {
            seq: 1,
            t_mono_ns: 1_000_000_000,
            t_wall_ms: 1,
            source: "test".into(),
            payload: tt_toplike_bus::Payload::Event(tt_toplike_bus::Event::ModelReady {
                model: tt_toplike_bus::EntityKey::model("m"),
                chips: vec![tt_toplike_bus::EntityKey::chip("0000:01:00.0")],
            }),
        });
        let view = |t: u64| tt_toplike_bus::LocalView {
            seq: world.seq(),
            world: world.clone(),
            now_mono_ns: t,
            header_kind: tt_toplike_bus::HeaderKind::Live,
        };
        for i in 1..=12u64 {
            let v = view(2_000_000_000 + i * 33_000_000);
            a.set_clock(i as f64 / 20.0);
            a.set_bus(Some(&v), &b);
            a.update(&b);
            solo.update(&b, Some(&v), i as f64 / 20.0);
        }
        let v = view(2_000_000_000 + 12 * 33_000_000);
        let want = solo.render(Some(&v), 12.0 / 20.0);
        let got = a.render(&b);
        // The hero may sit in the panel; compare every other cell.
        let (hx, hy) = (a.hero_x as usize, a.hero_y as usize);
        let trail: Vec<(usize, usize)> = a
            .hero_trail
            .iter()
            .map(|t| (t.x as usize, t.y as usize))
            .chain([(hx, hy)])
            .collect();
        let g = cells_in(&got, rect);
        let w = cells_of(&want, rect.w, rect.h);
        let mut differ = 0;
        for (r, (gr, wr)) in g.iter().zip(&w).enumerate() {
            for (c, (gc, wc)) in gr.iter().zip(wr).enumerate() {
                if gc != wc && !trail.contains(&(rect.x + c, rect.y + r)) {
                    differ += 1;
                }
            }
        }
        assert_eq!(differ, 0, "the panel is the Die 3D view's frame");
        let text = text_in(&got, rect);
        assert!(!text.contains("no Tenstorrent device"), "{text}");
    })
}

#[test]
fn the_die3d_panel_without_a_bus_is_the_die3d_view_without_one() {
    tc(|| {
        let (mut a, b) = arcade_with(one(View::Die3d), 80, 30);
        for _ in 0..3 {
            a.update(&b);
        }
        assert_eq!(a.die3d_bus_now, None);
        let rect = a.layout().find(View::Die3d).unwrap().rect;
        let text = text_in(&a.render(&b), rect);
        assert!(text.trim().len() > 20, "the scene is drawn: {text}");
    })
}

#[test]
fn the_training_panel_says_no_run_and_draws_a_run_when_there_is_one() {
    tc(|| {
        let (mut a, b) = arcade_with(one(View::Training), 100, 30);
        a.update(&b);
        let rect = a.layout().find(View::Training).unwrap().rect;
        let text = text_in(&a.render(&b), rect);
        assert!(text.starts_with(" no training run"), "{text}");
        assert_eq!(text.lines().filter(|l| !l.trim().is_empty()).count(), 1);

        let run = crate::workload::train::MockTrainRun::new();
        let st = run.state_at(30.0);
        a.set_train_state(Some(st.clone()));
        a.update(&b);
        let got = a.render(&b);
        // The panel is the Training view drawn at the panel's size.
        let mut tv = crate::animation::TrainView::new(rect.w, rect.h);
        tv.update();
        tv.update();
        let want = tv.render(&st, &b);
        // Every cell but the hero's and its trail's is the Training view's.
        let trail: Vec<(usize, usize)> = a
            .hero_trail
            .iter()
            .map(|t| (t.x as usize, t.y as usize))
            .chain([(a.hero_x as usize, a.hero_y as usize)])
            .collect();
        let (g, w) = (cells_in(&got, rect), cells_of(&want, rect.w, rect.h));
        let mut differ = Vec::new();
        for (r, (gr, wr)) in g.iter().zip(&w).enumerate() {
            for (c, (gc, wc)) in gr.iter().zip(wr).enumerate() {
                if gc != wc && !trail.contains(&(rect.x + c, rect.y + r)) {
                    differ.push((c, r, gc.0, wc.0));
                }
            }
        }
        assert!(differ.is_empty(), "{differ:?}");
        assert!(text_in(&got, rect).contains("TRAINING"));
    })
}

#[test]
fn the_inference_panel_says_no_server_and_draws_the_snake_when_one_runs() {
    tc(|| {
        let (mut a, b) = arcade_with(one(View::Inference), 100, 30);
        a.update(&b);
        let rect = a.layout().find(View::Inference).unwrap().rect;
        let text = text_in(&a.render(&b), rect);
        assert!(text.starts_with(" no inference server"), "{text}");

        // A server that is down is still no server.
        let mut down = mock_services(0.0);
        for s in &mut down {
            s.phase = Phase::Down;
        }
        a.set_services(down);
        a.update(&b);
        assert!(text_in(&a.render(&b), rect).starts_with(" no inference server"));

        let rows = mock_services(200.0);
        assert!(rows.iter().any(|s| s.phase != Phase::Down));
        a.set_services(rows);
        a.update(&b);
        let text = text_in(&a.render(&b), rect);
        assert!(!text.contains("no inference server"), "{text}");
        assert!(
            text.lines().filter(|l| !l.trim().is_empty()).count() > 3,
            "{text}"
        );
    })
}

#[test]
fn the_cloudfield_fills_only_blank_cells_and_off_draws_the_pinned_frame() {
    tc(|| {
        // A server generating tokens: the atmosphere has an aurora to draw.
        let serving: Vec<_> = (0..200)
            .map(|i| mock_services(i as f32))
            .find(|r| {
                r.iter()
                    .any(|s| s.serving.is_some_and(|v| v.generation_tps >= 1.0))
            })
            .expect("a mock roster that generates");
        let frame = |on: bool, rows: Vec<_>| {
            let (mut a, b) = arcade_with(arcade_layout::default_rows(), 120, 40);
            a.set_cloudfield(on);
            for _ in 0..10 {
                a.set_services(rows.clone());
                a.update(&b);
            }
            a.render(&b)
        };
        let plain = frame(false, Vec::new());
        let off = frame(false, serving.clone());
        let on = frame(true, serving.clone());
        assert_eq!(
            frame_hash(&off),
            frame_hash(&plain),
            "cloudfield off: services change nothing in the default layout"
        );
        assert_ne!(frame_hash(&on), frame_hash(&off), "on draws a backdrop");
        // Every cell the backdrop changed was a blank cell.
        let full = arcade_layout::Rect {
            x: 0,
            y: 0,
            w: 120,
            h: off.len(),
        };
        let (a, b) = (cells_in(&off, full), cells_in(&on, full));
        let mut filled = 0;
        for (ra, rb) in a.iter().zip(&b) {
            for (ca, cb) in ra.iter().zip(rb) {
                if ca != cb {
                    assert_eq!(ca.0, ' ', "only a blank cell is filled");
                    filled += 1;
                }
            }
        }
        assert!(filled > 0);
    })
}

#[test]
fn defrag_at_width_three_draws_three_times_the_castle() {
    tc(|| {
        let rows = vec![RowSpec {
            height: 1,
            panels: vec![
                PanelSpec {
                    view: View::Defrag,
                    width: 3,
                },
                PanelSpec {
                    view: View::Castle,
                    width: 1,
                },
            ],
        }];
        let (mut a, b) = arcade_with(rows, 161, 30);
        a.update(&b);
        let lines = a.render(&b);
        let l = a.layout().rows[0].clone();
        let (d, c) = (l.panels[0].rect, l.panels[1].rect);
        assert!(
            (d.w as i64 - 3 * c.w as i64).abs() <= 4,
            "defrag {} castle {}",
            d.w,
            c.w
        );
        // The divider is in the column between them on every content row.
        for y in d.rows() {
            let row = &cells_in(
                &lines,
                arcade_layout::Rect {
                    x: 0,
                    y,
                    w: 161,
                    h: 1,
                },
            )[0];
            assert_eq!(row[d.w].0, '│', "row {y}");
        }
    })
}

/// Characters that would be a right-side frame edge at the end of a row.
/// (The inference panel's loading snake draws boxed stage names such as
/// `┌load───┐`, as the `i` view does; a box corner is not a frame edge.)
const RIGHT_BORDERS: &str = "│║▕┤╣";

#[test]
fn every_panel_kind_at_several_sizes_fits_its_row() {
    tc(|| {
        let b = Fixed::new(2);
        for (w, h) in [
            (40, 12),
            (80, 24),
            (120, 40),
            (160, 50),
            (200, 60),
            (233, 71),
        ] {
            let mut a = ArcadeVisualization::with_config(w, h, &config(example_rows()));
            a.initialize_from_devices(b.devices());
            a.initialize_topology(&b);
            a.set_cloudfield(true);
            let run = crate::workload::train::MockTrainRun::new();
            for i in 0..4 {
                a.set_services(mock_services(100.0 + i as f32));
                a.set_train_state(Some(run.state_at(40.0)));
                a.update(&b);
            }
            let lines = a.render(&b);
            let rules: Vec<usize> = a.layout().rows.iter().map(|r| r.rule_y).collect();
            for (y, line) in lines.iter().enumerate() {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let width = unicode_width::UnicodeWidthStr::width(text.as_str());
                // A rule is cut by characters, not columns (`bbs_rule`), so
                // a label with a wide emoji runs one column per emoji past
                // the edge, as it always has; ratatui clips it. The new
                // labels have no wide characters.
                let slack = if rules.contains(&y) {
                    text.chars()
                        .filter(|c| unicode_width::UnicodeWidthChar::width(*c) == Some(2))
                        .count()
                } else {
                    0
                };
                assert!(
                    width <= w + slack,
                    "{w}x{h} row {y} is {width} wide: {text:?}"
                );
                // No frame edge in the last column of the row. (A panel's own
                // content may hold a `│` anywhere, such as the castle's
                // channel separators; the arcade adds a `│` only between two
                // panels.)
                let row = &cells_in(&lines, arcade_layout::Rect { x: 0, y, w, h: 1 })[0];
                let edges = [w - 1];
                for x in edges {
                    assert!(
                        !RIGHT_BORDERS.contains(row[x].0),
                        "{w}x{h} row {y} col {x} is a right border: {text:?}"
                    );
                }
            }
        }
    })
}

#[test]
fn a_panel_too_small_leaves_a_note_on_its_rule_and_draws_nothing() {
    tc(|| {
        let rows = vec![RowSpec {
            height: 1,
            panels: vec![
                PanelSpec {
                    view: View::Castle,
                    width: 4,
                },
                PanelSpec {
                    view: View::Die3d,
                    width: 1,
                },
            ],
        }];
        let (mut a, b) = arcade_with(rows, 60, 30);
        assert!(a.die3d.is_none(), "a hidden panel builds no view");
        a.update(&b);
        let lines = a.render(&b);
        let rule: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rule.contains("◌ DIE 3D needs 16x6"), "{rule}");
    })
}

#[test]
fn the_gauge_moves_to_the_gauge_panel_and_sparks_need_a_starfield() {
    use tt_toplike_bus::{EntityKey, Event, HeaderKind, LocalView, Payload, Record, World};
    tc(|| {
        // Flow on top, training beside castle below: the gauge goes on the
        // castle's rule, across the castle's columns only.
        let rows = vec![
            RowSpec {
                height: 1,
                panels: vec![PanelSpec {
                    view: View::Flow,
                    width: 1,
                }],
            },
            RowSpec {
                height: 1,
                panels: vec![
                    PanelSpec {
                        view: View::Training,
                        width: 1,
                    },
                    PanelSpec {
                        view: View::Castle,
                        width: 1,
                    },
                ],
            },
        ];
        let (mut a, b) = arcade_with(rows, 120, 40);
        let castle = a.layout().find(View::Castle).unwrap().rect;
        let gauge_row = castle.y - 1;
        let mut world = World::new();
        let mut seq = 0;
        let mut push = |w: &mut World, e: Event| {
            seq += 1;
            w.apply(&Record {
                seq,
                t_mono_ns: 1_000_000_000 + seq * 1_000_000,
                t_wall_ms: seq,
                source: "t".into(),
                payload: Payload::Event(e),
            });
        };
        push(
            &mut world,
            Event::ModelLoading {
                model: EntityKey::model("m"),
                chips: vec![EntityKey::chip("0000:01:00.0")],
                progress: Some(1.0),
            },
        );
        let view = |w: &World, t: u64| LocalView {
            seq: w.seq(),
            world: w.clone(),
            now_mono_ns: t,
            header_kind: HeaderKind::Live,
        };
        a.set_bus(Some(&view(&world, 5_000_000_000)), &b);
        a.update(&b);
        push(
            &mut world,
            Event::Spark {
                chip: Some(EntityKey::chip("0000:01:00.0")),
                kind: "vLLM.log".into(),
            },
        );
        a.set_bus(Some(&view(&world, 5_033_000_000)), &b);
        a.update(&b);
        let lines = a.render(&b);
        let purple = Some(crate::ui::colors::rgb(
            crate::animation::die3d::overlay::LOAD_PURPLE[0],
            crate::animation::die3d::overlay::LOAD_PURPLE[1],
            crate::animation::die3d::overlay::LOAD_PURPLE[2],
        ));
        let row = &cells_in(
            &lines,
            arcade_layout::Rect {
                x: 0,
                y: gauge_row,
                w: 120,
                h: 1,
            },
        )[0];
        // Chip 0 is the left half of the castle's columns, at 100%.
        let slot = castle.x..castle.x + castle.w / 2;
        let lit = row
            .iter()
            .enumerate()
            .filter(|(i, (_, s))| slot.contains(i) && s.fg == purple)
            .count();
        assert!(
            lit >= slot.len() - 2,
            "the gauge fills chip 0's slot: {lit}"
        );
        let outside = row
            .iter()
            .enumerate()
            .filter(|(i, (_, s))| *i < castle.x && s.fg == purple)
            .count();
        assert_eq!(outside, 0, "nothing on the training panel's columns");
        // No starfield: the spark is read but has nowhere to rise, so the
        // stage's sky is empty and `draw_particle` draws nothing.
        assert!(a.layout().find(View::Starfield).is_none());
        assert_eq!(
            a.bus_fx
                .chips
                .iter()
                .map(|c| c.particles.len())
                .sum::<usize>(),
            1,
            "the spark is read"
        );
    })
}

#[test]
fn the_hero_ranges_over_the_laid_out_rows() {
    tc(|| {
        let (mut a, b) = arcade_with(example_rows(), 160, 50);
        for _ in 0..200 {
            a.update(&b);
        }
        let bands = a.layout().bands();
        let y = a.hero_y as usize;
        assert!(
            bands.iter().any(|r| r.contains(&y)),
            "hero at row {y}, bands {bands:?}"
        );
        assert!(a.hero_x >= 0.0 && (a.hero_x as usize) < 160);
    })
}
