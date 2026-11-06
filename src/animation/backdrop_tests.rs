// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests of the shared backdrop: the settings, the transparency of both
//! layers, each kind through `Backdrop` against its painter, the step clock,
//! and the signals read from a fake world.

use super::*;
use crate::animation::atmosphere::composite;
use crate::animation::backdrop_pins::{foreground, scene_rosters};
use crate::animation::bus_test_support::{chip, frame_hash, tc, Bus, Fixed, SEC};
use crate::bus::signals as sig;
use ratatui::style::Style;
use ratatui::text::Span;
use tt_toplike_bus::{Provenance, Value};

const SCENE: Choice = Some(BackdropKind::Scene);
const SKY: Choice = Some(BackdropKind::Sky);
const CLOUDS: Choice = Some(BackdropKind::Clouds);

// ---- Names and settings ---------------------------------------------------

#[test]
fn every_choice_parses_from_its_name_and_nothing_else_does() {
    for c in [None, SCENE, SKY, CLOUDS] {
        assert_eq!(parse_choice(choice_name(c)), Some(c));
        assert_eq!(
            parse_choice(&format!(" {} ", choice_name(c).to_uppercase())),
            Some(c)
        );
    }
    for bad in ["", "cloud", "on", "true", "aurora", "scenes"] {
        assert_eq!(parse_choice(bad), None, "{bad:?}");
    }
}

#[test]
fn the_defaults_are_the_looks_each_view_had() {
    let s = BackdropSettings::new(BackdropConfig::default());
    for v in VIEWS {
        let want = match v {
            "inference" => SCENE,
            "training" => SKY,
            "die3d" => CLOUDS,
            _ => None,
        };
        assert_eq!(s.choice(v), want, "{v}");
        assert_eq!(builtin(v), want, "{v}");
    }
}

/// The views that take a backdrop, by `accepts`.
const TAKERS: [&str; 3] = ["inference", "training", "die3d"];

#[test]
fn only_the_inference_training_and_waffler_views_take_a_backdrop() {
    for v in VIEWS {
        let want: &[BackdropKind] = if TAKERS.contains(&v) {
            &BackdropKind::ALL
        } else {
            &[]
        };
        assert_eq!(accepts(v), want, "{v}");
        assert_eq!(takes_backdrop(v), TAKERS.contains(&v), "{v}");
        for k in BackdropKind::ALL {
            assert_eq!(refusal(v, k).is_some(), !TAKERS.contains(&v), "{v} {k:?}");
        }
    }
    assert_eq!(
        takers(),
        "only inference, training and Wafer Waffler take a backdrop"
    );
    // Saga, as a view and as an arcade panel, takes none: it is not a
    // backdrop view, and the arcade carries none behind any panel.
    assert!(accepts("saga").is_empty());
    assert!(!takes_backdrop("saga"));
}

#[test]
fn a_view_entry_wins_for_a_view_that_takes_one_and_is_ignored_elsewhere() {
    let cfg = BackdropConfig {
        default: Some(CLOUDS),
        views: vec![
            ("castle", SKY),
            ("training", None),
            ("arcade", SCENE),
            ("die3d", SKY),
            ("starfield", CLOUDS),
        ],
    };
    assert_eq!(cfg.choice("training"), None, "the sky turned off");
    assert_eq!(cfg.choice("die3d"), SKY);
    assert_eq!(cfg.choice("inference"), SCENE, "its own");
    // A view that takes none: its entry and the default are ignored.
    for v in ["castle", "arcade", "starfield", "grid"] {
        assert_eq!(cfg.choice(v), None, "{v}");
    }
}

#[test]
fn a_view_that_takes_no_backdrop_refuses_every_setting() {
    let mut s = BackdropSettings::new(BackdropConfig::default());
    for v in VIEWS.iter().filter(|v| !TAKERS.contains(v)) {
        for c in [None, SCENE, SKY, CLOUDS] {
            assert_eq!(s.set(v, c), Err(NO_BACKDROP.to_string()), "{v} {c:?}");
            assert_eq!(s.choice(v), None, "{v}: a refused setting changes nothing");
        }
    }
    s.set("training", CLOUDS).unwrap();
    assert_eq!(s.choice("training"), CLOUDS);
}

#[test]
fn the_key_cycles_off_scene_sky_clouds_where_a_view_takes_one() {
    let mut s = BackdropSettings::new(BackdropConfig::default());
    let mut seen = Vec::new();
    for _ in 0..4 {
        key_message("training", &mut s);
        seen.push(s.choice("training"));
    }
    assert_eq!(seen, vec![CLOUDS, None, SCENE, SKY], "from its own sky");
    assert_eq!(
        key_message("die3d", &mut s),
        "backdrop (Wafer Waffler): off"
    );
    // A view that takes none: the message, and nothing changes.
    for v in VIEWS.iter().filter(|v| !TAKERS.contains(v)) {
        assert_eq!(key_message(v, &mut s), NO_BACKDROP, "{v}");
        assert_eq!(s.choice(v), None, "{v}");
    }
}

#[test]
fn the_command_shows_sets_and_refuses() {
    let mut s = BackdropSettings::new(BackdropConfig::default());
    let (msg, err) = command("", "training", &mut s);
    assert!(!err && msg.starts_with("backdrop (training): sky"), "{msg}");
    assert_eq!(
        command("clouds", "training", &mut s),
        ("backdrop (training): clouds".into(), false)
    );
    assert_eq!(s.choice("training"), CLOUDS);
    let (msg, err) = command("fog", "training", &mut s);
    assert!(
        err && msg.contains("expected off, scene, sky or clouds"),
        "{msg}"
    );
    // A view that takes none says so for every form, and changes nothing.
    for arg in ["", "scene", "off", "fog"] {
        assert_eq!(
            command(arg, "starfield", &mut s),
            (NO_BACKDROP.to_string(), false),
            "{arg:?}"
        );
    }
    assert_eq!(s.choice("starfield"), None);
    // The views' other names.
    assert_eq!(canonical_view("waffler"), Some("die3d"));
    assert_eq!(canonical_view("~"), Some("hivemind"));
    assert_eq!(canonical_view("nope"), None);
}

#[test]
fn the_legend_line_names_the_kind_its_signals_and_marks_an_estimate() {
    for c in [None, SCENE, SKY, CLOUDS] {
        for est in [false, true] {
            let l = legend_line(c, est);
            assert!(
                l.starts_with(&format!("Backdrop: {}", choice_name(c))),
                "{l}"
            );
            // The overlay draws "║ " and two spaces before it, in 78 columns.
            assert!(l.chars().count() + 4 <= 78, "{l}");
            assert_eq!(l.contains('~'), est && c.is_some(), "{l}");
        }
    }
    assert!(legend_line(CLOUDS, false).contains("PCIe"));
}

// ---- Transparency ---------------------------------------------------------

/// A backdrop of `kind` that has seen `n` updates of busy signals.
fn busy(kind: Choice, n: u64) -> Backdrop {
    let (_, rows) = scene_rosters()
        .into_iter()
        .find(|(n, _)| *n == "aurora")
        .unwrap();
    let mut b = Backdrop::new(kind);
    for i in 1..=n {
        b.observe(&BackdropSignals {
            now_s: i as f64 / 60.0,
            services: rows(i),
            pcie_bps: Some(2e9),
            load: Some(0.8),
            step_ms: Some(200.0),
            chips: Vec::new(),
            estimated: false,
        });
    }
    b
}

/// Each cell of `lines` as (char, style), wide characters taking their
/// columns.
fn cells(lines: &[Line<'static>]) -> Vec<Vec<(char, Style)>> {
    crate::animation::bus_test_support::cells(lines)
}

#[test]
fn a_backdrop_never_overwrites_a_cell_the_view_drew() {
    tc(|| {
        for kind in [SCENE, SKY, CLOUDS] {
            let b = busy(kind, 120);
            let (w, h) = (100, 30);
            let mut fg = foreground(w, h);
            // A panel: blanks with their own background belong to the view.
            fg[5] = Line::from(vec![Span::styled(
                " ".repeat(w),
                Style::default().bg(Color::Blue),
            )]);
            let before = cells(&fg);
            let out = b.layer(fg, w, h);
            let after = cells(&out);
            let mut filled = 0;
            for (y, (ra, rb)) in before.iter().zip(&after).enumerate() {
                for (x, (ca, cb)) in ra.iter().zip(rb).enumerate() {
                    if ca != cb {
                        assert!(
                            ca.0 == ' ' && ca.1.bg.is_none(),
                            "{kind:?} ({x},{y}) {ca:?} -> {cb:?}"
                        );
                        filled += 1;
                    }
                }
            }
            assert!(filled > 0, "{kind:?} drew nothing");
            assert!(
                after[5].iter().all(|c| c.0 == ' '),
                "{kind:?} drew on the panel"
            );
        }
    })
}

#[test]
fn off_leaves_the_lines_and_the_buffer_as_they_were() {
    tc(|| {
        let b = busy(None, 60);
        assert!(!b.active());
        let fg = foreground(80, 24);
        assert_eq!(b.layer(fg.clone(), 80, 24), fg);
        assert!(b.canvas(80, 24).iter().flatten().all(|c| c.is_none()));
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        buf.set_string(3, 3, "text", Style::default());
        let copy = buf.clone();
        b.layer_buffer(&mut buf, area);
        assert_eq!(buf, copy);
    })
}

#[test]
fn the_buffer_layer_fills_only_blank_cells() {
    tc(|| {
        for kind in [SCENE, SKY, CLOUDS] {
            let b = busy(kind, 120);
            let area = Rect::new(2, 1, 100, 30);
            let mut buf = Buffer::empty(Rect::new(0, 0, 110, 35));
            // The canvas fill every view paints, then a view's drawing.
            for y in 0..35 {
                for x in 0..110 {
                    buf[(x, y)].set_bg(Color::Rgb(0, 0, 0));
                }
            }
            for y in (1..31).step_by(3) {
                buf.set_string(2, y, "drawn by the view", Style::default().fg(Color::Red));
            }
            for x in 2..102 {
                buf[(x, 10)].set_bg(Color::Blue); // a panel
                buf[(x, 12)].set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
            let before = buf.clone();
            b.layer_buffer(&mut buf, area);
            let mut filled = 0;
            for y in 0..35u16 {
                for x in 0..110u16 {
                    let (a, c) = (&before[(x, y)], &buf[(x, y)]);
                    if a != c {
                        assert!(is_blank(a), "{kind:?} ({x},{y}) was {a:?}");
                        assert!(area.contains((x, y).into()), "{kind:?} outside the area");
                        assert_eq!(a.bg, c.bg, "the background is kept");
                        filled += 1;
                    }
                }
            }
            assert!(filled > 0, "{kind:?} drew nothing");
            // Asserted on the fixture, so the blank rule itself is checked
            // (review S5): the panel row and the reversed row are the
            // view's, and so is its text.
            for x in 0..110u16 {
                for y in [10u16, 12] {
                    assert_eq!(before[(x, y)], buf[(x, y)], "{kind:?} ({x},{y})");
                }
            }
            for y in (1..31u16).step_by(3) {
                // The letters stay; the spaces between words are blank.
                for (i, ch) in "drawn by the view".chars().enumerate() {
                    let x = 2 + i as u16;
                    if ch != ' ' {
                        assert_eq!(before[(x, y)], buf[(x, y)], "{kind:?} text ({x},{y})");
                    }
                }
            }
        }
    })
}

/// A cell is blank only when it is a space on Reset or black with no
/// reverse, underline or strike-through. Each case fails when the rule is
/// reduced to "is a space".
#[test]
fn a_blank_cell_is_a_plain_space_on_the_canvas_black() {
    use ratatui::buffer::Cell;
    let cell = |sym: &str, bg: Color, m: Modifier| {
        let mut c = Cell::default();
        c.set_symbol(sym);
        c.set_bg(bg);
        c.modifier = m;
        c
    };
    let none = Modifier::empty();
    for bg in [Color::Reset, Color::Black, Color::Rgb(0, 0, 0)] {
        assert!(is_blank(&cell(" ", bg, none)), "{bg:?}");
    }
    for (c, why) in [
        (cell("x", Color::Reset, none), "a glyph"),
        (cell(" ", Color::Blue, none), "a panel colour"),
        (cell(" ", Color::Rgb(8, 14, 22), none), "a dialog colour"),
        (cell(" ", Color::Reset, Modifier::REVERSED), "reversed"),
        (cell(" ", Color::Black, Modifier::UNDERLINED), "underlined"),
        (cell(" ", Color::Black, Modifier::CROSSED_OUT), "struck"),
    ] {
        assert!(!is_blank(&c), "{why}");
    }
}

// ---- Each kind is its painter --------------------------------------------

/// The shared scene is the atmosphere, advanced at each step by the frames
/// a quarter second holds at `SCENE_FPS`, with its colours banded, and
/// composited as the Inference view does.
#[test]
fn the_scene_is_the_atmosphere_stepped_and_banded() {
    tc(|| {
        let per_step = (SCENE_FPS / CLOCK_STEPS_PER_SEC).round() as usize;
        for (name, rows) in scene_rosters() {
            let mut a = crate::animation::atmosphere::Atmosphere::new();
            let mut b = Backdrop::new(SCENE);
            for i in 0..40u64 {
                // Four updates a step: only the first of each step moves it.
                for sub in 0..4u64 {
                    b.observe(&BackdropSignals {
                        services: rows(i + 1),
                        now_s: i as f64 / CLOCK_STEPS_PER_SEC + sub as f64 * 0.05,
                        ..Default::default()
                    });
                }
                let n = if i == 0 { 1 } else { per_step };
                for _ in 0..n {
                    a.observe(&rows(i + 1));
                }
                for (w, h) in [(40, 12), (120, 40)] {
                    let fg = foreground(w, h);
                    let want = if a.active() {
                        composite(&band_colours(a.render_backdrop(w, h)), fg.clone(), w)
                    } else {
                        fg.clone()
                    };
                    assert_eq!(
                        frame_hash(&b.layer(fg, w, h)),
                        frame_hash(&want),
                        "{name} {i}"
                    );
                }
            }
        }
    })
}

/// The scene changes at most four times a second and runs at the same
/// speed whatever the view's frame rate (review S2 and M4).
#[test]
fn the_scene_steps_four_times_a_second_at_any_frame_rate() {
    tc(|| {
        let (_, rows) = scene_rosters()
            .into_iter()
            .find(|(n, _)| *n == "aurora")
            .unwrap();
        let run = |fps: u32| {
            let mut b = Backdrop::new(SCENE);
            let mut changes = 0;
            let mut last: Option<u64> = None;
            for i in 0..fps * 3 {
                b.observe(&BackdropSignals {
                    services: rows(i as u64),
                    now_s: i as f64 / fps as f64,
                    ..Default::default()
                });
                let h = frame_hash(&b.layer(foreground(80, 24), 80, 24));
                if last.is_some_and(|l| l != h) {
                    changes += 1;
                }
                last = Some(h);
            }
            (changes, b.atmosphere.frame())
        };
        let (c10, f10) = run(10);
        let (c60, f60) = run(60);
        assert!(c10 <= 12 && c60 <= 12, "{c10} {c60} changes in 3 s");
        assert_eq!(f10, f60, "the same atmosphere frame after 3 s");
    })
}

#[test]
fn the_sky_is_the_training_sky_at_the_stepped_frame() {
    tc(|| {
        let b = busy(SKY, 200);
        let frame = b.sky.frame();
        let (w, h) = (90, 25);
        let c = b.canvas(w, h);
        let mut aurora = 0;
        for (y, row) in c.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let rel_y = y as f32 / h as f32;
                let want = sky_cell(x, y, rel_y, frame).map(|s| (s.ch, s.color));
                // The same cells and glyphs; a star's colour too.
                assert_eq!(cell.map(|c| c.0), want.map(|c| c.0), "({x},{y})");
                let star = crate::animation::train_sky::star_hash(x, y)
                    > crate::animation::train_sky::STAR_THRESHOLD;
                if star {
                    assert_eq!(*cell, want, "({x},{y})");
                } else if let Some((_, colour)) = cell {
                    let (lit, hue) = crate::animation::train_sky::aurora(x, rel_y, frame);
                    assert_eq!(*colour, aurora_colour(lit, hue));
                    aurora += 1;
                }
            }
        }
        assert!(aurora > 0);
    })
}

#[test]
fn the_clouds_are_the_die3d_cloud_bands_one_per_cell() {
    tc(|| {
        let b = busy(CLOUDS, 200);
        let clouds = Clouds::new(super::super::die3d::CLOUD_SEED);
        let offset = b.cloud_clock.offset_px();
        let c = b.canvas(80, 20);
        let mut lit = 0;
        for (y, row) in c.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let band = clouds.band_at((2 * x) as f64 + 0.5, (4 * y) as f64 + 0.5, offset);
                assert_eq!(cell.is_some(), band > 0, "({x},{y})");
                if let Some((ch, _)) = cell {
                    assert_eq!(*ch, CLOUD_GLYPHS[band as usize - 1]);
                    lit += 1;
                }
            }
        }
        assert!(lit > 0 && lit < 80 * 20, "{lit}");
    })
}

// ---- Motion follows signals, on the step clock ---------------------------

/// How many times the canvas of `b` changes over `frames` updates at 60 a
/// second with `signals` (its `now_s` is set per frame).
fn canvas_changes(b: &mut Backdrop, signals: &BackdropSignals, frames: u64) -> usize {
    let mut last = b.canvas(120, 40);
    let mut n = 0;
    for i in 1..=frames {
        b.observe(&BackdropSignals {
            now_s: 10.0 + i as f64 / 60.0,
            ..signals.clone()
        });
        let c = b.canvas(120, 40);
        n += usize::from(c != last);
        last = c;
    }
    n
}

#[test]
fn the_sky_and_the_clouds_change_at_most_four_times_a_second() {
    tc(|| {
        let s = BackdropSignals {
            pcie_bps: Some(1e10),
            load: Some(1.0),
            step_ms: Some(100.0),
            ..Default::default()
        };
        for kind in [SKY, CLOUDS] {
            let mut b = Backdrop::new(kind);
            b.observe(&BackdropSignals {
                now_s: 10.0,
                ..s.clone()
            });
            let n = canvas_changes(&mut b, &s, 120);
            assert!(n > 0 && n <= 8, "{kind:?}: {n} changes in 2 s");
        }
    })
}

#[test]
fn the_clouds_drift_faster_with_pcie_traffic_and_brighten_with_load() {
    let after = |pcie: Option<f64>, load: Option<f32>| {
        let mut b = Backdrop::new(CLOUDS);
        for i in 0..=400 {
            b.observe(&BackdropSignals {
                now_s: i as f64 / 60.0,
                pcie_bps: pcie,
                load,
                ..Default::default()
            });
        }
        (b.cloud_clock.offset_px(), b.cloud_clock.intensity())
    };
    let (slow, dim) = after(None, None);
    let (fast, bright) = after(Some(5e9), Some(1.0));
    assert!(fast > 4.0 * slow, "drift {slow} -> {fast}");
    assert!(bright > dim, "intensity {dim} -> {bright}");
}

#[test]
fn the_sky_runs_faster_for_a_quicker_training_step() {
    let frames = |step_ms: Option<f32>| {
        let mut b = Backdrop::new(SKY);
        for i in 0..=240 {
            b.observe(&BackdropSignals {
                now_s: i as f64 / 60.0,
                step_ms,
                ..Default::default()
            });
        }
        b.sky.frame()
    };
    let (none, slow, quick) = (frames(None), frames(Some(1000.0)), frames(Some(250.0)));
    assert_eq!(none, 240, "4 s at 60 frames a second, on whole steps");
    assert!(slow < none && none < quick, "{slow} {none} {quick}");
}

// ---- Signals --------------------------------------------------------------

/// A world with PCIe and power on chip 0, a reading on a chip the backend
/// does not have, and a training run, at bus time `now`.
fn world(prov: Provenance) -> Bus {
    let mut bus = Bus::new();
    bus.sample(&chip(0), sig::PCIE_IN_BPS, Value::F64(3e9), prov)
        .sample(&chip(0), sig::PCIE_OUT_BPS, Value::F64(1e9), prov)
        .sample(&chip(0), sig::TDP_W, Value::F64(80.0), prov)
        // Chip 7 is the attached host's; this TUI does not have it.
        .sample(&chip(7), sig::PCIE_IN_BPS, Value::F64(9e12), prov)
        .sample(&chip(7), sig::TDP_W, Value::F64(500.0), prov);
    let run = tt_toplike_bus::EntityKey::training_run("nano_gpt:1@2");
    bus.sample(&run, sig::TRAIN_STEP, Value::U64(10), prov)
        .sample(&run, sig::TRAIN_STEP_MS, Value::F64(250.0), prov);
    bus
}

#[test]
fn the_signals_come_from_the_world_for_this_tuis_chips_only() {
    let backend = Fixed::new(2);
    let w = world(Provenance::Measured).view(2 * SEC);
    let s = BackdropSignals::gather(Some(&w), &backend, Vec::new(), 1.0, false);
    assert_eq!(
        s.pcie_bps,
        Some(4e9),
        "chip 0 in and out; chip 7 is not ours"
    );
    // Chip 0: 80 W of its 100 W limit. Chip 1 has no world reading: the
    // backend's 54 W.
    assert!((s.load.unwrap() - 0.8).abs() < 1e-6, "{:?}", s.load);
    assert_eq!(s.step_ms, Some(250.0));
    assert!(!s.estimated);
    assert_eq!(s.now_s, 1.0);
}

#[test]
fn estimated_readings_mark_the_signals() {
    let backend = Fixed::new(2);
    let w = world(Provenance::Estimated).view(2 * SEC);
    let s = BackdropSignals::gather(Some(&w), &backend, Vec::new(), 0.0, false);
    assert!(s.estimated);
    // Mock services are estimates too.
    let rows = crate::workload::inference_server::mock::mock_services(5.0);
    let s = BackdropSignals::gather(None, &backend, rows, 0.0, false);
    assert!(s.estimated);
}

#[test]
fn stale_readings_are_not_used_and_no_world_falls_back_to_the_backend() {
    let backend = Fixed::new(2);
    // Two minutes later every reading is stale.
    let w = world(Provenance::Measured).view(121 * SEC);
    let s = BackdropSignals::gather(Some(&w), &backend, Vec::new(), 0.0, true);
    assert_eq!(s.pcie_bps, None, "the fixed backend has no PCIe counters");
    assert_eq!(s.step_ms, None);
    // The backend's power: 45 and 54 W of 100.
    assert!((s.load.unwrap() - 0.54).abs() < 1e-6, "{:?}", s.load);
    assert!(s.estimated, "the backend's values are estimates here");
    let none = BackdropSignals::gather(None, &backend, Vec::new(), 0.0, false);
    assert!((none.load.unwrap() - 0.54).abs() < 1e-6);
    assert!(!none.estimated);
}

// ---- The views' own painters ----------------------------------------------

/// The Inference view's snake draws the atmosphere as it did (the scene is
/// its default), and draws only its foreground when another backdrop is
/// chosen.
#[test]
fn the_snake_draws_its_atmosphere_unless_told_not_to() {
    tc(|| {
        let (_, rows) = scene_rosters()
            .into_iter()
            .find(|(n, _)| *n == "aurora")
            .unwrap();
        let mut snake = crate::animation::Snake::new();
        let mut atm = crate::animation::atmosphere::Atmosphere::new();
        let (w, h) = (100, 30);
        for i in 1..=30 {
            let r = rows(i);
            snake.update(&crate::animation::SnakeWorld {
                rows: &r,
                catalog: &[],
                arch: "Blackhole",
                chips: &[],
                cadence_secs: 5,
                width: w,
                height: h,
            });
            atm.observe(&r);
        }
        let fg = snake.render_foreground(w, h);
        assert!(atm.active());
        let want = composite(&atm.render_backdrop(w, h), fg.clone(), w);
        assert_eq!(frame_hash(&snake.render(w, h)), frame_hash(&want));
        snake.set_atmosphere(false);
        assert_eq!(frame_hash(&snake.render(w, h)), frame_hash(&fg));
    })
}

/// With the sky off the Training view draws no aurora or star, and every
/// other cell is the same: the pinned frames see the sky.
#[test]
fn the_training_view_leaves_the_sky_blank_when_told_to() {
    tc(|| {
        let backend = Fixed::new(2);
        for (_, st) in crate::animation::backdrop_pins::train_states() {
            let mut on = crate::animation::TrainView::new(120, 40);
            let mut off = crate::animation::TrainView::new(120, 40);
            off.set_sky(false);
            for _ in 0..25 {
                on.update();
                off.update();
            }
            let (a, b) = (
                cells(&on.render(&st, &backend)),
                cells(&off.render(&st, &backend)),
            );
            let mut blanked = 0;
            for (ra, rb) in a.iter().zip(&b) {
                for (ca, cb) in ra.iter().zip(rb) {
                    if ca != cb {
                        assert!("✦∙·▒░".contains(ca.0), "{ca:?} -> {cb:?}");
                        assert_eq!(cb.0, ' ');
                        blanked += 1;
                    }
                }
            }
            assert!(blanked > 0);
        }
    })
}

// ---- The clouds' load is the Waffler's -----------------------------------

/// The shared clouds take each chip's load as the Waffler does: power over
/// TDP for the first samples, then the draw against the chip's own idle
/// (`die3d::load::LoadScale`). An idle box reads dim and a burst reads
/// full (review S4).
#[test]
fn the_clouds_load_is_measured_against_each_chips_own_idle() {
    use crate::animation::die3d::load::{LoadScale, MIN_SAMPLES};
    let tdp = 125.0f32;
    let sig = |t: f64, w: f32| BackdropSignals {
        now_s: t,
        chips: vec![ChipDraw {
            key: "0000:01:00.0".into(),
            power_w: Some(w),
            fallback: Some(w / tdp),
        }],
        ..Default::default()
    };
    let mut scales = LoadScales::default();
    let mut waffler = LoadScale::new();
    for i in 0..20 {
        let t = i as f64;
        let mut s = sig(t, 40.0);
        scales.apply(&mut s);
        let want = waffler.observe(t, Some(40.0));
        if i + 1 < MIN_SAMPLES {
            assert_eq!(s.load, Some(40.0 / tdp), "power over TDP at first");
        } else {
            assert_eq!(s.load, Some(want), "the Waffler's value at {i}");
            assert!(want < 0.05, "an idle chip is near 0, got {want}");
        }
    }
    let mut s = sig(20.0, 70.0);
    scales.apply(&mut s);
    assert!(s.load.unwrap() > 0.9, "a burst over the idle: {:?}", s.load);
    // A chip that goes away takes its scale with it.
    let mut none = BackdropSignals {
        now_s: 21.0,
        chips: vec![ChipDraw {
            key: "other".into(),
            power_w: Some(40.0),
            fallback: Some(0.32),
        }],
        ..Default::default()
    };
    scales.apply(&mut none);
    assert_eq!(none.load, Some(0.32));
}
