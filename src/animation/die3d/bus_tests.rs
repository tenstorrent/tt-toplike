// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What the view draws from the signal bus (see `overlay`): frames with and
//! without a world, the model flags, the fill, pulses, GDDR rows, sparks and
//! the bus label in the footer.

use super::test_backend::{chip_key, BusFixture, Fake, SEC_NS};
use super::*;
use tt_toplike_bus::{Event, LocalView, Provenance, Value};

const TDP: &str = "100";

/// The text of each line, spans joined.
fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// A view of `fake` at `cols` x `rows` (or an inset), updated once at view
/// time 0 with `world`, auto cuts, clouds and the history panel off.
fn view_with(
    fake: &Fake,
    cols: usize,
    rows: usize,
    inset: bool,
    world: Option<&dyn WorldRead>,
) -> Die3dView {
    let mut v = if inset {
        Die3dView::inset(cols, rows)
    } else {
        Die3dView::new(cols, rows)
    };
    v.update(fake, world, 0.0);
    v.director.set_auto(false);
    if !inset {
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
    }
    v
}

/// One board of two dies (chips 0 and 1).
fn two_dies() -> Fake {
    let mut b = Fake::boards(&["p300"; 2], Some(TDP));
    b.set_power(0, 40.0);
    b.set_power(1, 30.0);
    b
}

#[test]
fn a_world_with_nothing_to_draw_gives_the_frames_of_no_world() {
    // An empty world, and one holding only readings the overlay does not
    // draw, must give the frame a view without a bus gives, at every size
    // the view has a layout for and in the inset.
    let mut quiet = BusFixture::new();
    quiet.sample(
        &chip_key(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    let empty = BusFixture::new();
    for fake in [two_dies(), Fake::boards(&["p300"; 4], Some(TDP))] {
        for (c, r, inset) in [
            (100, 30, false),
            (120, 36, false),
            (150, 42, false),
            (60, 16, false),
            (40, 12, false),
            (52, 16, true),
        ] {
            for f in [&empty, &quiet] {
                let w = f.view(5 * SEC_NS);
                let plain = view_with(&fake, c, r, inset, None);
                let bus = view_with(&fake, c, r, inset, Some(&w));
                for t in [0.0, 1.25, 2.5] {
                    assert_eq!(
                        plain.render(None, t),
                        bus.render(Some(&w), t),
                        "{c}x{r} inset={inset} at {t}"
                    );
                }
            }
        }
    }
}

/// The line holding `needle`, if any.
fn line_with<'a>(lines: &'a [String], needle: &str) -> Option<&'a String> {
    lines.iter().find(|l| l.contains(needle))
}

#[test]
fn chip_0s_flag_shows_its_model_and_chip_1s_does_not() {
    let mut f = BusFixture::new();
    f.ready("llama", "Llama 3.1 8B", &[0], 42.0);
    let w = f.view(5 * SEC_NS);
    let v = view_with(&two_dies(), 150, 42, false, Some(&w));
    let lines = v.render(Some(&w), 0.0);
    let t = text(&lines);
    let bh0 = line_with(&t, "BH0").expect("chip 0 has a flag");
    assert!(bh0.contains("Llama 3.1 8B ready 42t/s"), "{bh0}");
    let bh1 = line_with(&t, "BH1").expect("chip 1 has a flag");
    assert!(!bh1.contains("Llama") && !bh1.contains("t/s"), "{bh1}");
    // The model part is in the serving teal, bold.
    let span = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content.contains("Llama"))
        .unwrap();
    assert_eq!(span.style.fg, Some(rgb(overlay::SERVE_TEAL)));
    // Without the world in `render`, the flag is the plain label.
    let plain = text(&v.render(None, 1.0));
    assert!(line_with(&plain, "Llama").is_none());
}

#[test]
fn an_estimated_model_flag_starts_with_a_tilde() {
    let mut f = BusFixture::new();
    let m = tt_toplike_bus::EntityKey::model("mock");
    f.sample(
        &m,
        tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
        Value::F64(20.0),
        Provenance::Estimated,
    );
    f.event(Event::ModelReady {
        model: m,
        chips: vec![chip_key(0)],
    });
    let w = f.view(5 * SEC_NS);
    let v = view_with(&two_dies(), 150, 42, false, Some(&w));
    let t = text(&v.render(Some(&w), 0.0));
    let bh0 = line_with(&t, "BH0").unwrap();
    assert!(bh0.contains("~mock ready 20t/s"), "{bh0}");
}

/// The frame of a view updated with `world` at bus time `update_ns` and
/// rendered at bus time `render_ns`.
fn frame_at(fake: &Fake, f: &BusFixture, update_ns: u64, render_ns: u64) -> Vec<Line<'static>> {
    let v = view_with(fake, 150, 42, false, Some(&f.view(update_ns)));
    v.render(Some(&f.view(render_ns)), 0.0)
}

/// Each row of a frame as (character, style) cells, spans expanded.
fn cells(lines: &[Line<'static>]) -> Vec<Vec<(char, ratatui::style::Style)>> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                .collect()
        })
        .collect()
}

/// Loading at 0.75 fills more of the die than at 0.25. The flag prints the
/// percentage, so the flag's row (the one showing `25%` or `75%`) is left
/// out, and the cells that differ from the frame without a world (colour or
/// glyph) are counted on every other row. The label, phase and phase timing
/// are the same in both; only the progress differs.
#[test]
fn loading_fills_the_die_and_more_progress_fills_more() {
    let fake = two_dies();
    let at = |p: f32| {
        let mut f = BusFixture::new();
        f.loading("m", &[0], Some(p));
        frame_at(&fake, &f, 5 * SEC_NS, 5 * SEC_NS)
    };
    let none = {
        let v = view_with(&fake, 150, 42, false, None);
        v.render(None, 0.0)
    };
    let (low, high, none) = (cells(&at(0.25)), cells(&at(0.75)), cells(&none));
    let text_of = |row: &Vec<(char, ratatui::style::Style)>| -> String {
        row.iter().map(|(c, _)| *c).collect()
    };
    // The flag's row: it shows the percentage.
    let flag_rows: Vec<usize> = (0..low.len())
        .filter(|&r| text_of(&low[r]).contains("25%") || text_of(&high[r]).contains("75%"))
        .collect();
    assert_eq!(
        flag_rows.len(),
        1,
        "the flag shows the progress on one row: {flag_rows:?}"
    );
    let filled = |frame: &Vec<Vec<(char, ratatui::style::Style)>>| -> usize {
        (0..frame.len())
            .filter(|r| !flag_rows.contains(r))
            .map(|r| {
                frame[r]
                    .iter()
                    .zip(&none[r])
                    .filter(|(a, b)| a != b)
                    .count()
            })
            .sum()
    };
    let (lo, hi) = (filled(&low), filled(&high));
    println!("filled cells: 0.25 -> {lo}, 0.75 -> {hi}");
    assert!(lo > 0, "loading at 0.25 fills part of the die");
    assert!(hi > lo, "0.75 fills more than 0.25: {hi} vs {lo}");
}

#[test]
fn a_same_phase_reannounce_draws_the_same_frame() {
    // The host re-announces every model every five minutes; a `ModelReady`
    // for a model already ready changes nothing on screen.
    let fake = two_dies();
    let mut f = BusFixture::new();
    f.loading("m", &[0], Some(0.8));
    let t0 = 5 * SEC_NS;
    let mut a = view_with(&fake, 150, 42, false, Some(&f.view(t0)));
    f.ready("m", "M", &[0], 12.0);
    a.update(&fake, Some(&f.view(t0)), 0.5);
    let mut b = view_with(&fake, 150, 42, false, Some(&f.view(t0)));
    let later = t0 + 300 * SEC_NS;
    a.update(&fake, Some(&f.view(later)), 1.0);
    f.event(Event::ModelReady {
        model: tt_toplike_bus::EntityKey::model("m"),
        chips: vec![chip_key(0)],
    });
    a.update(&fake, Some(&f.view(later)), 1.5);
    assert_eq!(a.render(Some(&f.view(later)), 2.0), {
        // The frame at 2.0 of a view that never saw the re-announce.
        b.update(&fake, Some(&f.view(later)), 1.0);
        b.render(Some(&f.view(later)), 2.0)
    });
}

#[test]
fn a_pulse_shows_and_is_gone_after_a_second_and_a_half() {
    let fake = two_dies();
    let mut f = BusFixture::new();
    f.ready("m", "M", &[0], 12.0);
    let t0 = 5 * SEC_NS;
    let mut v = view_with(&fake, 150, 42, false, Some(&f.view(t0)));
    // The same world without the event, for comparison.
    let mut quiet = view_with(&fake, 150, 42, false, Some(&f.view(t0)));
    quiet.update(&fake, Some(&f.view(t0)), 0.5);
    f.event(Event::RequestsCompleted {
        model: tt_toplike_bus::EntityKey::model("m"),
        n: 4,
    });
    v.update(&fake, Some(&f.view(t0)), 0.5);
    let mid = t0 + SEC_NS / 2;
    let gone = t0 + 3 * SEC_NS / 2;
    assert_ne!(
        v.render(Some(&f.view(mid)), 1.0),
        quiet.render(Some(&f.view(mid)), 1.0),
        "half a second in, the pulse shows"
    );
    assert_eq!(
        v.render(Some(&f.view(gone)), 2.0),
        quiet.render(Some(&f.view(gone)), 2.0),
        "a second and a half in, it is gone"
    );
}

#[test]
fn two_gddr_channels_at_different_temperatures_draw_different_rows() {
    let fake = Fake::boards(&["p150"], Some(TDP));
    let pixels = |f: &BusFixture| {
        let w = f.view(5 * SEC_NS);
        let v = view_with(&fake, 150, 42, false, Some(&w));
        let fx = v.overlay.fx_at(w.now_mono_ns(), false);
        v.scene_now(0.0, &fx).unwrap().0.rgb
    };
    let mut hot_cold = BusFixture::new();
    hot_cold.gddr_temp(0, 0, 85.0).gddr_temp(0, 1, 45.0);
    let mut hot_hot = BusFixture::new();
    hot_hot.gddr_temp(0, 0, 85.0).gddr_temp(0, 1, 85.0);
    let mut cold_cold = BusFixture::new();
    cold_cold.gddr_temp(0, 0, 45.0).gddr_temp(0, 1, 45.0);
    let a = pixels(&hot_cold);
    // Channel 1's row follows its own reading.
    assert_ne!(a, pixels(&hot_hot));
    assert_ne!(a, pixels(&cold_cold));
    // And the colours differ row by row: the colour of the two rows.
    let colour = |heat: f32| {
        scene::gddr_rgb(
            &palette::GRAYSKULL,
            None,
            Some(overlay::GddrFx {
                heat: Some(heat),
                dim: false,
                flash: 0.0,
            }),
        )
    };
    assert_ne!(colour(0.9), colour(0.1));
}

#[test]
fn an_ecc_increase_flashes_the_row_in_the_frame() {
    let fake = Fake::boards(&["p150"], Some(TDP));
    let ecc = |f: &mut BusFixture, v: u64| {
        let e = tt_toplike_bus::EntityKey::dram_channel(&chip_key(0), 2);
        f.sample(
            &e,
            crate::bus::signals::DRAM_ECC_CORR_RD,
            Value::U64(v),
            Provenance::Measured,
        );
    };
    let mut f = BusFixture::new();
    ecc(&mut f, 1);
    let t0 = 5 * SEC_NS;
    let mut v = view_with(&fake, 150, 42, false, Some(&f.view(t0)));
    ecc(&mut f, 2);
    v.update(&fake, Some(&f.view(t0)), 0.0);
    // A view whose first sight is the higher count: no rise, no flash.
    let fresh = view_with(&fake, 150, 42, false, Some(&f.view(t0)));
    assert_ne!(
        v.render(Some(&f.view(t0)), 1.0),
        fresh.render(Some(&f.view(t0)), 1.0),
        "the rise flashes"
    );
    let after = t0 + overlay::FLASH_NS;
    assert_eq!(
        v.render(Some(&f.view(after)), 2.0),
        fresh.render(Some(&f.view(after)), 2.0),
        "the flash is over"
    );
}

#[test]
fn the_footer_names_the_bus_unless_it_is_plain_local() {
    let fake = two_dies();
    let mut v = view_with(&fake, 150, 42, false, None);
    let footer = |v: &Die3dView, t: f64| text(&v.render(None, t))[40].clone();
    let plain = footer(&v, 0.0);
    v.set_bus_label(Some("bus: local"));
    assert_eq!(footer(&v, 0.0), plain);
    v.set_bus_label(Some("bus: /run/tt/bus.sock (replay)"));
    let attached = footer(&v, 0.0);
    assert!(attached.contains(ESTIMATE_LABEL), "{attached}");
    assert!(
        // Shortened to 24 characters from the left of its path.
        attached.contains("bus: \u{2026}/bus.sock (replay)"),
        "{attached}"
    );
    v.set_bus_label(None);
    assert_eq!(footer(&v, 0.0), plain);
}

#[test]
fn the_inset_draws_the_fill_but_no_text() {
    let fake = two_dies();
    let mut f = BusFixture::new();
    f.loading("m", &[0], Some(0.5));
    let w = f.view(5 * SEC_NS);
    let plain = view_with(&fake, 52, 16, true, None).render(None, 0.0);
    let bus = view_with(&fake, 52, 16, true, Some(&w)).render(Some(&w), 0.0);
    assert_ne!(plain, bus, "the fill shows in the inset");
    // No text: the caption is the same, and the scene rows hold only
    // drawing (braille and blanks), as without the bus.
    let (p, b) = (text(&plain), text(&bus));
    assert_eq!(p.last(), b.last(), "the caption");
    for row in &b[..b.len() - 1] {
        assert!(
            row.chars()
                .all(|c| c == ' ' || ('\u{2800}'..='\u{28ff}').contains(&c)),
            "{row}"
        );
    }
}

/// A world with something on every chip: a model loading on chip 0, one
/// ready on chip 1 with a pulse just begun, per-channel GDDR on chip 0 and
/// a spark on chip 1. Read at bus time `now`; the events land after a
/// first update at `now`, so they count as new.
pub(super) fn decorated(v: &mut Die3dView, fake: &Fake, now: u64) -> LocalView {
    let mut f = BusFixture::new();
    f.loading("load", &[0], Some(0.6))
        .ready("serve", "Llama 3.1 8B", &[1], 42.0)
        .gddr_temp(0, 0, 82.0)
        .gddr_temp(0, 5, 48.0);
    v.update(fake, Some(&f.view(now)), 0.0);
    f.event(Event::RequestsCompleted {
        model: tt_toplike_bus::EntityKey::model("serve"),
        n: 4,
    })
    .event(Event::Spark {
        chip: Some(chip_key(1)),
        kind: "vLLM.log".into(),
    });
    let w = f.view(now);
    v.update(fake, Some(&w), 0.0);
    w
}

/// Prints the decorated frame at 100 x 30 drawn into a ratatui
/// `TestBackend`: at the pulse's start, half way through it, and after it,
/// with a count of the cells whose colour is near each workload colour.
/// For a person to look at; asserts nothing. Run with
/// `cargo test --locked --lib --features tui overlay_smoke_frames -- --ignored --nocapture`.
#[test]
#[ignore = "prints frames for a person to look at"]
fn overlay_smoke_frames() {
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;
    let fake = Fake::boards(&["p300"; 2], Some(TDP));
    let mut v = Die3dView::new(100, 30);
    let w = decorated(&mut v, &fake, 5 * SEC_NS);
    v.director.set_auto(false);
    v.handle_key(Die3dKey::ToggleClouds, 0.0);
    for (what, dt, t) in [
        ("at the pulse's start", 0, 0.0),
        ("0.6 s in (pulse at half)", 600_000_000, 0.6),
        ("1.5 s in (pulse gone)", 1_500_000_000, 1.5),
    ] {
        let mut at = w.clone();
        at.now_mono_ns = 5 * SEC_NS + dt;
        let lines = v.render(Some(&at), t);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| f.render_widget(Paragraph::new(lines.clone()), f.area()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let near = |c: [u8; 3]| {
            buf.content()
                .iter()
                .filter(|cell| match cell.fg {
                    Color::Rgb(r, g, b) => {
                        (r as i32 - c[0] as i32).abs()
                            + (g as i32 - c[1] as i32).abs()
                            + (b as i32 - c[2] as i32).abs()
                            < 60
                    }
                    _ => false,
                })
                .count()
        };
        println!(
            "--- {what}: cells near teal {}, purple {}, red {}",
            near(overlay::SERVE_TEAL),
            near(overlay::LOAD_PURPLE),
            near(overlay::ALARM_RED)
        );
        for y in 0..30 {
            let row: String = (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect();
            println!("{}", row.trim_end());
        }
    }
}

#[test]
fn a_long_bus_label_never_pushes_the_camera_caption_off_the_row() {
    let fake = two_dies();
    let mut v = view_with(&fake, 100, 30, false, None);
    v.set_bus_label(Some(
        "bus: /run/user/1000/tt-toplike/very/long/path/bus.sock (other host) (reconnecting)",
    ));
    let status = text(&v.render(None, 0.0))[28].clone();
    assert!(status.contains("view: "), "{status}");
    assert!(status.contains(ESTIMATE_LABEL), "{status}");
    assert!(status.chars().count() <= 100, "{status}");
    // The label shows, shortened: the bus prefix is there.
    assert!(status.contains("bus"), "{status}");
}

#[test]
fn a_bus_label_is_shortened_from_the_left_of_its_path() {
    // Short labels stay whole.
    assert_eq!(short_bus_label("bus: replay", 24), "bus: replay");
    // The path loses its start; the status words stay.
    let s = short_bus_label("bus: /run/tt/very/long/bus.sock (reconnecting)", 24);
    assert_eq!(s, "bus: \u{2026}ock (reconnecting)");
    assert!(s.chars().count() <= 24);
    let s = short_bus_label("bus: /run/tt/very/long/path/bus.sock", 24);
    assert_eq!(s, "bus: \u{2026}long/path/bus.sock");
    // Status words longer than the room: cut at the end.
    let s = short_bus_label("bus: /a/b.sock (other host) (reconnecting)", 24);
    assert!(s.chars().count() <= 24 && s.ends_with('\u{2026}'), "{s}");
    assert!(s.starts_with("bus: "), "{s}");
}
