use super::camera::TransitionMode;
use super::scene::{has_per_tile_data, ESTIMATE_LABEL};
use super::test_backend::Fake;
use super::*;
use crate::backend::pcie_counters::PcieBandwidth;
use std::collections::HashMap;

/// TDP the fakes report, so `power` watts is a load of `power / 100`
/// while a chip's `LoadScale` has fewer than 5 samples (the first 4
/// seconds of view time). The tests that cut on a rise from 0 W to 90 W
/// within those seconds use that fallback; after it, the load is
/// relative to the chip's own idle (see
/// `the_view_feeds_relative_load_to_the_scene_and_the_triggers`).
const TDP: &str = "100";
/// A powered chip's idle draw in watts. Zero is not an idle: a reading under
/// a watt is a sensor glitch and `LoadScale` ignores it.
const IDLE_W: f32 = 37.0;

/// The text of each line, spans joined.
fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// The footer: the last two lines, joined.
fn footer(lines: &[Line<'static>]) -> String {
    let t = text(lines);
    t[t.len().saturating_sub(2)..].join("\n")
}

/// Two p300 boards: chips 0 and 1 on board 0, chips 2 and 3 on board 1,
/// all idle (which arms the load triggers on the first update).
fn two_boards() -> Fake {
    Fake::boards(&["p300"; 4], Some(TDP))
}

/// A 150 x 42 view over two boards that has seen one idle tick at 0.0 and
/// then a load rise on chip 2 (board 1) at 1.0.
fn switched_by_chip_2() -> (Die3dView, Fake) {
    let mut b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    b.set_power(2, 90.0);
    v.update(&b, 1.0);
    (v, b)
}

#[test]
fn render_gives_one_line_per_row_at_any_size() {
    let b = two_boards();
    // Below 100 x 30 a smaller layout is drawn (see `tiers`); at the
    // tiniest sizes, and at none at all, nothing panics. Every size,
    // the minimum of the full layout included, gives one line per row.
    for (c, r) in [
        (99, 40),
        (150, 29),
        (10, 3),
        (0, 0),
        (0, 5),
        (5, 0),
        (100, 30),
    ] {
        let mut v = Die3dView::new(c, r);
        v.update(&b, 0.0);
        let lines = v.render(0.0);
        assert_eq!(lines.len(), r, "{c} x {r}: {:?}", text(&lines));
    }
}

#[test]
fn a_tiny_view_shows_the_summary() {
    let b = two_boards();
    // Below SCENE_MIN_CELLS in rows: a scene clipped to four rows would
    // show too little to be worth drawing.
    let mut v = Die3dView::new(30, SCENE_MIN_CELLS.1 - 2);
    v.update(&b, 0.0);
    let t = text(&v.render(0.0));
    assert_eq!(t.len(), SCENE_MIN_CELLS.1 - 2);
    // The Fake backend's four chips (two boards) report 0 W and no PCIe
    // counters: the exact summary lines, then empty rows.
    for (i, name) in ["BH0", "BH1", "BH2", "BH3"].iter().enumerate() {
        assert_eq!(t[i], format!("{name} 0W  in -  out -"), "{t:?}");
    }
    assert!(t[4..].iter().all(|l| l.is_empty()), "{t:?}");
    // No scene glyph: no braille cell and no half-block.
    assert!(
        !t.iter()
            .any(|l| l.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)
                || matches!(c, '\u{2580}' | '\u{2584}' | '\u{2588}'))),
        "{t:?}"
    );
}

/// `/theme` reaches the scene: the same view drawn in the default theme is
/// full colour, in Grayskull it is greys, and the change shows in the very
/// next frame even though the frame cache would otherwise return the last one.
#[test]
fn the_theme_changes_the_scene_palette_and_is_not_served_from_the_cache() {
    use crate::ui::colors::Theme;
    let mut v = two_die_view(150, 42).with_theme(Theme::Grayskull);
    let spread = |c: [u8; 3]| c.iter().max().unwrap() - c.iter().min().unwrap();
    let most_colourful = |v: &Die3dView, now: f64| -> u8 {
        let (buf, _) = v.scene_pixels(now).expect("a scene");
        (0..buf.w * buf.h)
            .filter(|&i| buf.cover[i])
            .map(|i| spread(buf.rgb[i]))
            .max()
            .unwrap()
    };
    let grey = most_colourful(&v, 1.0);
    let first = text(&v.render(1.0));
    v.set_theme(Theme::Default);
    // Same view time, so only the revision tells the cache the frame is stale.
    let second = text(&v.render(1.0));
    let rainbow = most_colourful(&v, 1.0);
    assert!(rainbow >= 150, "the default theme is colourful: {rainbow}");
    assert!(
        grey < rainbow,
        "grayskull ({grey}) is less colourful than rainbow ({rainbow})"
    );
    assert_ne!(
        first, second,
        "the theme change was served from the frame cache"
    );
    // Setting the same theme again changes nothing and costs no new frame.
    let rev = v.revision;
    v.set_theme(Theme::Default);
    assert_eq!(v.revision, rev);
}

/// A `cols` x `rows` view over one two-die board (the fake's p300 is one
/// die, so two of them) that has read the backend once, at 0.0.
fn two_die_view(cols: usize, rows: usize) -> Die3dView {
    let b = Fake::boards(&["p300"; 2], Some(TDP));
    let mut v = Die3dView::new(cols, rows);
    v.update(&b, 0.0);
    v
}

/// A window 50 columns wide and 54 tall (a slim Konsole window) is too
/// narrow for a four pixel tile, which used to leave the chip summary
/// where the scene should be. The scene shrinks to a two pixel tile
/// instead, and is not clipped.
#[test]
fn a_narrow_tall_window_draws_the_scene_not_the_summary() {
    let v = two_die_view(50, 54);
    let t = text(&v.render(0.0));
    assert_eq!(t.len(), 54);
    assert!(
        t.iter().any(|l| has_braille_dots(l)),
        "no scene in a 50 x 54 view: {t:?}"
    );
    assert!(
        !t.iter().any(|l| l.contains("0W  in")),
        "the summary is drawn instead of the scene: {t:?}"
    );
    assert_eq!(v.scene_pixels(0.0).expect("a scene").1, Shown::Board(0));
}

/// A window too small for even the two pixel tile shows the scene at that
/// tile, clipped to its middle, and says what size shows all of it.
#[test]
fn a_window_too_small_for_the_scene_shows_its_middle_and_says_so() {
    // 20 x 10 cells: 40 x 40 pixels, under the two pixel scene's width.
    let v = two_die_view(20, 10);
    let (_, shown) = v.scene_pixels(0.0).expect("a clipped scene");
    let Shown::Clipped(board, need_cols, need_rows) = shown else {
        panic!("expected a clipped scene, got {shown:?}");
    };
    assert_eq!(board, 0);
    assert!(
        need_cols > 20 || need_rows > 10,
        "needs {need_cols}x{need_rows} but 20 x 10 was clipped"
    );
    let t = text(&v.render(0.0));
    assert_eq!(t.len(), 10);
    assert!(t.iter().any(|l| has_braille_dots(l)), "no scene: {t:?}");
    // Under 16 rows there is no footer, so the note is over the last row.
    assert_eq!(
        t[9],
        clip_note(need_cols, need_rows),
        "the note is on the last row: {t:?}"
    );
}

/// With a footer (16 rows or more), the note leads the status line, so a
/// narrow row that cuts from the right keeps it.
#[test]
fn a_clipped_view_with_a_footer_leads_with_the_note() {
    let v = two_die_view(24, 17);
    let (_, shown) = v.scene_pixels(0.0).expect("a scene");
    let Shown::Clipped(_, c, r) = shown else {
        panic!("expected a clipped scene at 24 x 17, got {shown:?}");
    };
    let t = text(&v.render(0.0));
    assert_eq!(t.len(), 17);
    let note = clip_note(c, r);
    assert!(
        t[16].contains(&note) || t[16].contains(&note[..note.len().min(20)]),
        "footer row {:?} lacks the note {note:?}",
        t[16]
    );
}

/// The clipped window is a crop of the same picture: the whole scene
/// drawn at the same tile on a buffer big enough for it has, at the
/// window's offset, exactly the pixels the window shows.
#[test]
fn the_clipped_window_is_a_crop_of_the_whole_picture() {
    let v = two_die_view(20, 10);
    let now = 0.0;
    let (got, shown) = v.scene_pixels(now).expect("a scene");
    assert!(matches!(shown, Shown::Clipped(..)), "{shown:?}");
    let grid = board_grid(2);
    let (pw, ph) = (got.w as i32, got.h as i32);
    let small = scene::fit_last_resort(pw, ph, grid);
    let (vw, vh) = small.virtual_px.expect("clipped");
    // The whole picture is what a buffer of exactly its size shows, drawn
    // independently of the clipped fit; the window is its middle, offset
    // by the floored half of the size difference.
    let whole_fit = scene::fit_last_resort(vw, vh, grid);
    let (dx, dy) = ((vw - pw).div_euclid(2), (vh - ph).div_euclid(2));
    let cam = v.director.camera_at(now);
    let window = aim(&small, Aim::At(cam), grid);
    let whole_fit = aim(&whole_fit, Aim::At(cam), grid);
    let mut whole = PixelBuf::new(vw as usize, vh as usize);
    whole.clear_transparent();
    draw_board_scene(
        &mut whole,
        &v.chips[0],
        &whole_fit,
        now as f32,
        Detail::Full,
    );
    // The window drawn directly, as the view does.
    let mut direct = PixelBuf::new(pw as usize, ph as usize);
    direct.clear_transparent();
    draw_board_scene(&mut direct, &v.chips[0], &window, now as f32, Detail::Full);
    assert_eq!(got.rgb, direct.rgb, "the view draws the clipped fit");
    let mut painted = 0;
    for y in 0..ph {
        for x in 0..pw {
            let (wx, wy) = (x + dx, y + dy);
            let want = if wx >= 0 && wy >= 0 && wx < vw && wy < vh {
                whole.rgb[wy as usize * vw as usize + wx as usize]
            } else {
                [0; 3]
            };
            let have = got.rgb[y as usize * pw as usize + x as usize];
            assert_eq!(
                have, want,
                "pixel ({x}, {y}) differs from the whole picture"
            );
            painted += (have != [0; 3]) as usize;
        }
    }
    assert!(painted > 100, "the window shows almost nothing: {painted}");
    // And it really is a crop: the whole picture has painted pixels
    // outside the window, or it would not have needed clipping.
    let outside = (0..vh)
        .flat_map(|y| (0..vw).map(move |x| (x, y)))
        .filter(|&(x, y)| x < dx || y < dy || x >= dx + pw || y >= dy + ph)
        .filter(|&(x, y)| whole.rgb[y as usize * vw as usize + x as usize] != [0; 3])
        .count();
    assert!(outside > 0, "nothing was cut off");
}

/// The arrow keys move the window over the same whole picture: after a pan the
/// window shows the whole picture at the middle offset plus the pan, a key at
/// the scene's edge changes nothing, and the key back moves at once.
#[test]
fn panning_moves_the_window_over_the_whole_picture() {
    let mut v = two_die_view(20, 10);
    let now = 0.0;
    let grid = board_grid(2);
    // One frame first: it reports how far the window can move.
    let (first, _) = v.scene_pixels(now).expect("a scene");
    let (pw, ph) = (first.w as i32, first.h as i32);
    let (vw, vh) = scene::fit_last_resort(pw, ph, grid)
        .virtual_px
        .expect("clipped");
    let whole_at = |v: &Die3dView, x0: i32, y0: i32| {
        let fit = scene::fit_last_resort(vw, vh, grid);
        let fit = aim(&fit, Aim::At(v.director.camera_at(now)), grid);
        let mut whole = PixelBuf::new(vw as usize, vh as usize);
        whole.clear_transparent();
        draw_board_scene(&mut whole, &v.chips[0], &fit, now as f32, Detail::Full);
        let mut crop = vec![[0u8; 3]; (pw * ph) as usize];
        for y in 0..ph {
            for x in 0..pw {
                let (wx, wy) = (x + x0, y + y0);
                if wx >= 0 && wy >= 0 && wx < vw && wy < vh {
                    crop[(y * pw + x) as usize] = whole.rgb[(wy * vw + wx) as usize];
                }
            }
        }
        crop
    };
    let (mid_x, mid_y) = ((vw - pw).div_euclid(2), (vh - ph).div_euclid(2));
    assert_eq!(
        first.rgb,
        whole_at(&v, mid_x, mid_y),
        "no pan shows the middle"
    );

    // Right and down by one step each.
    v.handle_key(Die3dKey::PanRight, now);
    v.handle_key(Die3dKey::PanDown, now);
    let (got, _) = v.scene_pixels(now).expect("a scene");
    let (step_x, step_y) = {
        let room = v.cache.borrow().pan_room;
        (room.2, room.3)
    };
    assert!(step_x > 0 && step_y > 0);
    assert_eq!(
        v.pan,
        (
            step_x.min(v.cache.borrow().pan_room.0),
            step_y.min(v.cache.borrow().pan_room.1)
        )
    );
    assert_eq!(got.rgb, whole_at(&v, mid_x + v.pan.0, mid_y + v.pan.1));
    assert_ne!(got.rgb, first.rgb, "the window moved");

    // Far past the edge: the window stops at the picture's own edge.
    for _ in 0..50 {
        v.handle_key(Die3dKey::PanLeft, now);
        v.handle_key(Die3dKey::PanUp, now);
    }
    let room = v.cache.borrow().pan_room;
    assert_eq!(v.pan, (-room.0, -room.1));
    let (got, _) = v.scene_pixels(now).expect("a scene");
    let (edge_x, edge_y) = (mid_x - room.0, mid_y - room.1);
    assert!(
        edge_x == 0 || edge_x == -1 || vw <= pw,
        "left edge {edge_x}"
    );
    assert_eq!(got.rgb, whole_at(&v, edge_x, edge_y));
    // One key back moves at once, not after undoing the overshoot.
    v.handle_key(Die3dKey::PanRight, now);
    assert_eq!(v.pan.0, -room.0 + step_x.min(2 * room.0));
}

/// A scene that fits has nothing to pan over: the arrows change nothing.
#[test]
fn panning_a_scene_that_fits_changes_nothing() {
    let mut v = p150_view(120, 40);
    let before = text(&v.render(0.0));
    for k in [
        Die3dKey::PanLeft,
        Die3dKey::PanRight,
        Die3dKey::PanUp,
        Die3dKey::PanDown,
    ] {
        v.handle_key(k, 0.0);
    }
    assert_eq!(v.pan, (0, 0));
    assert_eq!(text(&v.render(0.0)), before);
}

/// Insets keep their own size rules: a scene that does not fit them is
/// not clipped into them.
#[test]
fn an_inset_is_never_clipped() {
    let b = Fake::boards(&["p300"; 2], Some(TDP));
    let mut v = Die3dView::inset(20, 8);
    v.update(&b, 0.0);
    assert!(
        v.scene_pixels(0.0).is_none(),
        "a 20 x 8 inset cannot hold the scene and must not clip it"
    );
}

/// A `cols` x `rows` view over one p150 that has read the backend once,
/// at 0.0.
fn p150_view(cols: usize, rows: usize) -> Die3dView {
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(cols, rows);
    v.update(&b, 0.0);
    v
}

/// True when `line` holds a braille glyph with at least one dot set
/// (U+2801 to U+28FF; U+2800 is the empty cell).
fn has_braille_dots(line: &str) -> bool {
    line.chars()
        .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
}

#[test]
fn below_100_by_30_a_scene_is_drawn() {
    let v = p150_view(80, 24);
    let t = text(&v.render(0.0));
    // One line per row: the scene and a one-row footer.
    assert_eq!(t.len(), 24, "{t:?}");
    assert!(
        t[..23].iter().any(|l| has_braille_dots(l)),
        "no braille in the scene rows: {t:?}"
    );
    assert!(t.iter().all(|l| !l.contains("needs a terminal")), "{t:?}");
}

#[test]
fn clouds_are_off_below_100_by_30() {
    // The control: at exactly 100 x 30 the clouds are drawn, so pressing
    // W changes the scene rows. Without it the check below could pass on
    // a scene with no clouds at any size.
    let mut v = p150_view(100, 30);
    let on = text(&v.render(0.0));
    v.handle_key(Die3dKey::ToggleClouds, 0.0);
    let off = text(&v.render(0.0));
    assert_ne!(on[..28], off[..28], "clouds not visible at 100 x 30");
    // One column narrower the tier turns them off: W on and W off draw
    // the same frame. The one-row footer has no switch strip, so the
    // footer is the same too.
    let mut v = p150_view(99, 40);
    let on = text(&v.render(0.0));
    assert_eq!(on.len(), 40, "a scene is drawn: {on:?}");
    v.handle_key(Die3dKey::ToggleClouds, 0.0);
    assert_eq!(text(&v.render(0.0)), on);
}

#[test]
fn the_footer_is_one_row_then_none() {
    // 99 x 40: one footer row, the status line, naming the scene.
    let t = text(&p150_view(99, 40).render(0.0));
    assert_eq!(t.len(), 40, "{t:?}");
    let last = &t[39];
    assert!(last.starts_with("\u{2551} "), "{last}");
    assert!(last.contains("board 1/1"), "{last}");
    assert!(!t[38].starts_with("\u{2551} "), "{}", t[38]);
    // 59 x 40: no footer. The last line is a scene row.
    let t = text(&p150_view(59, 40).render(0.0));
    assert_eq!(t.len(), 40, "{t:?}");
    assert!(
        t.iter()
            .all(|l| !l.starts_with('\u{2551}') && !l.starts_with('\u{255a}')),
        "{t:?}"
    );
    assert!(t.iter().any(|l| has_braille_dots(l)), "{t:?}");
}

#[test]
fn flags_are_off_below_60_by_16() {
    // The control: at 60 x 40 a flag finds room, so its pin shows.
    let t = text(&p150_view(60, 40).render(0.0));
    assert!(
        t.iter().any(|l| l.contains('●')),
        "no flag at 60 x 40: {t:?}"
    );
    // One column narrower: no flags at all.
    let t = text(&p150_view(59, 40).render(0.0));
    assert_eq!(t.len(), 40, "a scene is drawn: {t:?}");
    assert!(t.iter().all(|l| !l.contains('●')), "{t:?}");
}

#[test]
fn render_with_no_devices_does_not_panic() {
    let b = Fake::boards(&[], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    // Before any update, then after one with no devices.
    assert_eq!(v.render(0.0).len(), 1);
    v.update(&b, 0.0);
    for k in [
        Die3dKey::NextBoard,
        Die3dKey::PrevBoard,
        Die3dKey::ToggleScene,
        Die3dKey::CutNow,
        Die3dKey::ToggleBraille,
        Die3dKey::CycleTransition,
    ] {
        v.handle_key(k, 0.5);
    }
    let lines = v.render(0.6);
    assert_eq!(lines.len(), 1);
    assert!(text(&lines)[0].contains("no Tenstorrent device"));
}

#[test]
fn a_non_blackhole_device_is_named_in_the_footer() {
    // Two single-chip cards: a p150 (drawn) and an n150 (not drawn).
    let b = Fake::boards(&["p150", "n150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let lines = v.render(0.0);
    assert_eq!(lines.len(), 42);
    assert!(
        footer(&lines).contains("chip 1: Wormhole (not drawn)"),
        "{}",
        footer(&lines)
    );
    // With no Blackhole at all: the hint plus the same note, nothing else.
    let b = Fake::boards(&["n300", "n300"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let t = text(&v.render(0.0));
    assert_eq!(t.len(), 2, "{t:?}");
    assert!(t[0].contains("Blackhole"));
    assert!(t[1].contains("chip 0: Wormhole (not drawn)"));
    assert!(t[1].contains("chip 1: Wormhole (not drawn)"));
}

#[test]
fn one_chip_without_telemetry_renders() {
    let mut b = Fake::boards(&["p150"], None);
    b.telem.clear();
    b.has_smbus = false;
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let lines = v.render(0.0);
    assert_eq!(lines.len(), 42);
    // One board: the super scene is that board's scene.
    assert!(footer(&lines).contains("board 1/1"), "{}", footer(&lines));
    // The scene rows carry the drawing: 150 cells each.
    let t = text(&lines);
    assert_eq!(t[0].chars().count(), 150);
    assert_eq!(t[39].chars().count(), 150);
}

#[test]
fn a_load_rise_in_the_super_scene_switches_to_that_chips_board_at_the_cut_midpoint() {
    let (v, _) = switched_by_chip_2();
    let at = v
        .director
        .swapped_at()
        .expect("the load rise cut the camera");
    // A scene change is a chaos or a dissolve; both swap at their midpoint.
    let kind = v.director.transition_kind().unwrap();
    assert_ne!(kind, TransitionKind::Spin);
    assert!((at - (1.0 + kind.secs() / 2.0)).abs() < 1e-9);
    // Still the super scene up to the midpoint, board 2 from it.
    assert_eq!(v.switcher.scene_at(at - 0.01), Scene::All);
    assert_eq!(v.switcher.scene_at(at), Scene::Board(1));
    assert!(footer(&v.render(at - 0.01)).contains("all boards"));
    let f = footer(&v.render(at + 0.01));
    assert!(f.contains("board 2/2 board-1"), "{f}");
    // A PCIe burst in the super scene cuts the angle but keeps the scene.
    let mut b = two_boards();
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 0.0,
        tx_bytes_per_sec: 0.0,
    });
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 3.0e9,
        tx_bytes_per_sec: 0.0,
    });
    v.update(&b, 1.0);
    assert!(
        v.director.swapped_at().is_some(),
        "the burst cut the camera"
    );
    assert_eq!(v.switcher.scene_at(5.0), Scene::All);
}

#[test]
fn a_trigger_in_a_board_scene_cuts_only_for_that_boards_chips() {
    let mut b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    // Show board 1 (chips 2 and 3) by hand.
    v.handle_key(Die3dKey::NextBoard, 0.0);
    v.handle_key(Die3dKey::NextBoard, 0.0);
    assert_eq!(v.switcher.scene_at(0.0), Scene::Board(1));
    // Chip 0 is on board 0: no cut, and no switch.
    b.set_power(0, 90.0);
    v.update(&b, 1.0);
    assert!(v.director.swapped_at().is_none());
    assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1));
    // Chip 3 is on board 1: the camera cuts, the scene stays.
    b.set_power(3, 90.0);
    v.update(&b, 2.0);
    assert!(v.director.swapped_at().is_some());
    assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1));
}

/// The highest heat any Tensix tile of `chip` shows over a few seconds of
/// the estimate's wave. Taken over many `t` so the wave's position does
/// not decide the comparison.
fn peak_tile_heat(chip: &ChipInputs) -> f32 {
    let layout = super::layout::DieLayout::blackhole();
    let mut best = 0.0f32;
    for step in 0..200 {
        let t = step as f32 * 0.1;
        for tile in &layout.tiles {
            best = best.max(super::scene::tile_value(chip, tile, t));
        }
    }
    best
}

#[test]
fn the_view_feeds_relative_load_to_the_scene_and_the_triggers() {
    // Measured on real hardware: a 125 W power limit, 37 W idle, 68 W
    // under serving. As power over the limit that is 0.30 then 0.54, and
    // 0.30 is above LOAD_REARM (0.25), so the rise could never fire.
    let mut b = Fake::boards(&["p150"], Some("125"));
    b.set_power(0, 37.0);
    let mut v = Die3dView::new(150, 42);
    // Steady load is not an event; the idle turn (a spin when nothing has
    // cut for a while) is tested on its own.
    v.director.set_idle_turn(false);
    // 30 s of idle, ticked four times a second like the TUI might.
    let mut now = 0.0;
    while now < 30.0 {
        v.update(&b, now);
        now += 0.25;
    }
    assert!(v.director.swapped_at().is_none(), "idle must not cut");
    let rest = v.chips[0][0].clone();
    assert_eq!(rest.load, 0.0, "idle is zero load against its own idle");
    // The rise to 68 W.
    b.set_power(0, 68.0);
    v.update(&b, now);
    let busy = v.chips[0][0].clone();
    assert_eq!(busy.load, 1.0, "68 W is full load against a 37 W idle");
    assert!(
        v.director.swapped_at().is_some(),
        "LoadRise fired and cut the camera"
    );
    // The scene reads the same load: the tiles glow brighter than at rest.
    let (cold, hot) = (peak_tile_heat(&rest), peak_tile_heat(&busy));
    assert!(hot > cold + 0.5, "tile heat {cold} at rest, {hot} busy");
}

#[test]
fn a_chip_with_no_power_reading_stays_at_zero_load_and_never_fires() {
    // Its scale never gets a sample, so the view keeps the fallback,
    // which is 0.0 without a power reading.
    let mut b = Fake::boards(&["p150"], Some("125"));
    b.telem[0].power = None;
    let mut v = Die3dView::new(150, 42);
    // A minute of quiet is not a cut: only an event is under test.
    v.director.set_idle_turn(false);
    for i in 0..120 {
        v.update(&b, i as f64 * 0.5);
        assert_eq!(v.chips[0][0].load, 0.0);
    }
    assert!(v.director.swapped_at().is_none());
}

#[test]
fn a_busy_chip_that_loses_its_power_reading_reads_zero_load_at_once() {
    // 30 s idle at 37 W, then busy at 68 W: full load.
    let mut b = Fake::boards(&["p150"], Some("125"));
    b.set_power(0, 37.0);
    let mut v = Die3dView::new(150, 42);
    for i in 0..30 {
        v.update(&b, i as f64);
    }
    b.set_power(0, 68.0);
    v.update(&b, 30.0);
    assert_eq!(v.chips[0][0].load, 1.0);
    let cut = v.director.cut_at();
    assert!(cut.is_some(), "the rise cut the camera");
    // The reading goes away. Nothing was read, so the chip is not shown
    // as busy: load 0.0 on the very next update, at a tick inside the
    // same second as well as a second later, and no cut.
    b.telem[0].power = None;
    for now in [30.5, 31.0, 32.0, 40.0] {
        v.update(&b, now);
        assert_eq!(v.chips[0][0].load, 0.0, "at {now}");
        assert_eq!(v.director.cut_at(), cut, "at {now}");
    }
    // The clouds, which follow the busiest chip, dim with it.
    assert_eq!(v.cloud_intensity, Clouds::intensity(0.0));
    // A NaN reading is no reading either.
    b.set_power(0, 68.0);
    v.update(&b, 41.0);
    assert_eq!(v.chips[0][0].load, 1.0);
    b.telem[0].power = Some(f32::NAN);
    v.update(&b, 42.0);
    assert_eq!(v.chips[0][0].load, 0.0);
}

#[test]
fn a_chip_that_disappears_loses_its_scale() {
    let mut b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let mut keys: Vec<usize> = v.scales.keys().copied().collect();
    keys.sort_unstable();
    assert_eq!(keys, [0, 1, 2, 3]);
    // Board 1 (chips 2 and 3) goes away.
    b.devices.truncate(2);
    v.update(&b, 1.0);
    let mut keys: Vec<usize> = v.scales.keys().copied().collect();
    keys.sort_unstable();
    assert_eq!(keys, [0, 1]);
}

#[test]
fn the_view_returns_to_the_super_scene_after_20_seconds_without_triggers() {
    let (mut v, mut b) = switched_by_chip_2();
    v.update(&b, 1.5);
    assert_eq!(v.switcher.scene_at(1.5), Scene::Board(1));
    // 19.9 s after the load rise: still on the board.
    v.update(&b, 20.9);
    assert_eq!(v.switcher.scene_at(20.9), Scene::Board(1));
    // 20 s after it: back to every board.
    v.update(&b, 21.0);
    assert_eq!(v.switcher.scene_at(21.0), Scene::All);

    // A load rise on the board on show restarts the 20 s (see
    // `a_rise_on_the_board_on_show_does_not_return`); one on another
    // board does not (see `the_return_timer_ignores_rises_on_other_boards`).
}

/// `switched_by_chip_2` settled on board 1: the switch's cut (at 1.0)
/// has passed its midpoint and chip 2 still draws 90 W.
fn on_board_1_chosen_by_the_view() -> (Die3dView, Fake) {
    let (mut v, b) = switched_by_chip_2();
    v.update(&b, 4.0);
    assert_eq!(v.switcher.scene_at(4.0), Scene::Board(1));
    assert_eq!(v.director.cut_at(), Some(1.0));
    (v, b)
}

#[test]
fn a_rise_on_another_board_returns_to_all_boards_when_on_an_auto_chosen_board() {
    let (mut v, mut b) = on_board_1_chosen_by_the_view();
    // Chip 0, on board 0, rises 9 s after the last cut (past the 8 s
    // cooldown): a cut that goes back to every board at its midpoint.
    b.set_power(0, 90.0);
    v.update(&b, 10.0);
    assert_eq!(v.director.cut_at(), Some(10.0), "the rise cut the camera");
    let kind = v.director.transition_kind().unwrap();
    assert_ne!(kind, TransitionKind::Spin, "a scene change is never a spin");
    let at = v.director.swapped_at().unwrap();
    assert_eq!(v.switcher.scene_at(at - 0.01), Scene::Board(1));
    assert_eq!(v.switcher.scene_at(at), Scene::All);
    assert_eq!(v.transition_scenes(10.1), (Scene::Board(1), Scene::All));
    v.update(&b, at + 0.1);
    assert!(footer(&v.render(at + 0.1)).contains("all boards"));
    // Back on every board, the view stays there: no return pending.
    v.update(&b, 60.0);
    assert_eq!(v.switcher.scene_at(60.0), Scene::All);

    // Inside the cooldown the rise is ignored: no cut, no switch, and
    // nothing kept for later. The 20 s return still comes, at 21 s.
    let (mut v, mut b) = on_board_1_chosen_by_the_view();
    b.set_power(0, 90.0);
    v.update(&b, 5.0);
    assert_eq!(v.director.cut_at(), Some(1.0));
    assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1));
    v.update(&b, 12.0);
    assert_eq!(v.director.cut_at(), Some(1.0));
    assert_eq!(v.switcher.scene_at(12.0), Scene::Board(1));
}

#[test]
fn a_rise_on_the_board_on_show_does_not_return() {
    let (mut v, mut b) = on_board_1_chosen_by_the_view();
    // Chip 3, on board 1, rises at 10 s (chip 2 dropped back first, so
    // the detector re-armed): the camera cuts, the board stays, and the
    // 20 s return now counts from 10 s.
    b.set_power(2, 0.0);
    v.update(&b, 6.0);
    b.set_power(3, 90.0);
    v.update(&b, 10.0);
    assert_eq!(v.director.cut_at(), Some(10.0));
    for now in [10.5, 15.0, 21.0, 29.9] {
        v.update(&b, now);
        assert_eq!(v.switcher.scene_at(now), Scene::Board(1), "{now}");
    }
    v.update(&b, 30.0);
    assert_eq!(v.switcher.scene_at(30.0), Scene::All);
}

#[test]
fn the_return_timer_ignores_rises_on_other_boards() {
    // A rise on board 0 inside the cooldown neither cuts nor moves the
    // return: the view goes back to every board 20 s after the rise
    // that chose board 1 (at 1 s).
    let (mut v, mut b) = on_board_1_chosen_by_the_view();
    b.set_power(0, 90.0);
    v.update(&b, 5.0);
    v.update(&b, 20.9);
    assert_eq!(v.switcher.scene_at(20.9), Scene::Board(1));
    v.update(&b, 21.0);
    assert_eq!(v.switcher.scene_at(21.0), Scene::All);
}

/// A 150 x 42 view over two boards (chips 0 and 1, then 2 and 3) with the
/// idle turn off, that has seen one quiet tick at 0.0. The next event is
/// the first cut.
fn quiet_two_boards(b: &Fake) -> Die3dView {
    let mut v = Die3dView::new(150, 42);
    v.director.set_idle_turn(false);
    v.update(b, 0.0);
    assert!(v.director.cut_at().is_none(), "a quiet start cuts nothing");
    v
}

/// What an event did: the cut's kind, and where the view was going to look.
fn cut_by_event(v: &Die3dView, at: f64) -> (Option<camera::TransitionKind>, Scene) {
    assert_eq!(v.director.cut_at(), Some(at), "the event cut the camera");
    (v.director.transition_kind(), v.switcher.scene_at(at + 5.0))
}

#[test]
fn a_process_opening_a_device_cuts_with_chaos_and_opens_that_board() {
    use camera::TransitionKind::Chaos;
    let mut b = two_boards();
    let mut v = quiet_two_boards(&b);
    b.open_device(4242, 2); // chip 2 is on board 1
    v.update(&b, 1.0);
    assert_eq!(cut_by_event(&v, 1.0), (Some(Chaos), Scene::Board(1)));
    // The same process closing it is an event too, after the cooldown.
    b.close_device(4242, 2);
    v.update(&b, 20.0);
    assert_eq!(v.director.cut_at(), Some(20.0));
    assert_eq!(v.director.transition_kind(), Some(Chaos));
}

#[test]
fn a_process_already_there_at_the_start_is_not_an_event() {
    let mut b = two_boards();
    b.open_device(1, 0);
    let mut v = quiet_two_boards(&b);
    v.update(&b, 1.0);
    v.update(&b, 2.0);
    assert!(v.director.cut_at().is_none());
}

#[test]
fn a_temperature_rise_cuts_with_chaos() {
    use camera::TransitionKind::Chaos;
    let mut b = two_boards();
    b.telem[2].asic_temperature = Some(45.0);
    let mut v = quiet_two_boards(&b);
    b.telem[2].asic_temperature = Some(46.0);
    v.update(&b, 1.0);
    assert!(v.director.cut_at().is_none(), "a degree is not a rise");
    b.telem[2].asic_temperature = Some(52.0);
    v.update(&b, 2.0);
    assert_eq!(cut_by_event(&v, 2.0), (Some(Chaos), Scene::Board(1)));
}

#[test]
fn hot_memory_cuts_with_a_dissolve() {
    use camera::TransitionKind::Dissolve;
    let mut b = two_boards();
    b.set_gddr_temp(45.0);
    let mut v = quiet_two_boards(&b);
    b.set_gddr_temp(70.0);
    v.update(&b, 1.0);
    let (kind, _) = cut_by_event(&v, 1.0);
    assert_eq!(kind, Some(Dissolve));
}

#[test]
fn an_ethernet_link_change_cuts_with_a_dissolve() {
    use camera::TransitionKind::Dissolve;
    let mut b = two_boards();
    b.smbus.eth_live_status = Some(0b0011);
    let mut v = quiet_two_boards(&b);
    v.update(&b, 1.0);
    assert!(
        v.director.cut_at().is_none(),
        "the same links are not a change"
    );
    b.smbus.eth_live_status = Some(0b0111);
    v.update(&b, 2.0);
    let (kind, _) = cut_by_event(&v, 2.0);
    assert_eq!(kind, Some(Dissolve));
}

/// A load rise is chaos, a PCIe burst alone is chaos too but opens no board:
/// it says something happened, not where to look.
#[test]
fn a_pcie_burst_cuts_with_chaos_and_keeps_the_scene() {
    use camera::TransitionKind::Chaos;
    let mut b = two_boards();
    // The burst detector arms on a quiet reading, so start with one.
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 0.0,
        tx_bytes_per_sec: 0.0,
    });
    let mut v = quiet_two_boards(&b);
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 2.0e9,
        tx_bytes_per_sec: 0.0,
    });
    v.update(&b, 1.0);
    assert_eq!(cut_by_event(&v, 1.0), (Some(Chaos), Scene::All));
}

/// With the forced mode the reasons no longer pick the kind.
#[test]
fn a_forced_transition_mode_beats_the_reason_in_the_view() {
    use camera::TransitionKind::Spin;
    let mut b = two_boards();
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 0.0,
        tx_bytes_per_sec: 0.0,
    });
    let mut v = quiet_two_boards(&b);
    v.command("transition", "spin", 0.5);
    b.open_device(7, 0);
    v.update(&b, 1.0);
    // A scene change cannot spin, so a forced spin dissolves it...
    assert_eq!(
        v.director.transition_kind(),
        Some(camera::TransitionKind::Dissolve)
    );
    // ...and a camera-only event spins.
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 2.0e9,
        tx_bytes_per_sec: 0.0,
    });
    v.update(&b, 20.0);
    assert_eq!(v.director.transition_kind(), Some(Spin));
}

/// A view made outside the app holds braille, so every test and library user
/// gets the same picture each time.
#[test]
fn a_new_view_holds_braille_and_the_footer_says_so() {
    let v = two_die_view(150, 42);
    assert_eq!(v.cell_style(0.0), CellStyle::Braille);
    assert!(footer(&v.render(0.0)).contains("/cells braille"));
}

/// In auto the hardware picks the starting style, and a different hardware
/// state can pick a different one.
#[test]
fn auto_starts_in_a_style_the_hardware_picks() {
    let start = |watts: f32| {
        let mut b = two_boards();
        for i in 0..4 {
            b.set_power(i, watts + i as f32);
        }
        let mut v = Die3dView::new(150, 42).with_cell_mode(CellMode::Auto);
        v.update(&b, 0.0);
        v.cell_style(0.0)
    };
    assert_eq!(start(40.0), start(40.0), "same hardware, same start");
    let styles: std::collections::HashSet<_> =
        (0..60).map(|i| start(30.0 + i as f32 * 0.7)).collect();
    assert_eq!(
        styles.len(),
        3,
        "the start varies with the hardware: {styles:?}"
    );
}

/// An event that cuts the camera changes the style, but only after the dwell,
/// and at the cut's swap, a moment after the event.
#[test]
fn an_event_changes_the_cell_style_after_the_dwell_at_the_cut_swap() {
    let mut b = two_boards();
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 0.0,
        tx_bytes_per_sec: 0.0,
    });
    let mut v = Die3dView::new(150, 42).with_cell_mode(CellMode::Auto);
    v.director.set_idle_turn(false);
    v.update(&b, 0.0);
    let start = v.cell_style(0.0);
    // An event inside the dwell cuts the camera but keeps the style.
    b.open_device(1, 0);
    v.update(&b, 10.0);
    assert!(v.director.cut_at() == Some(10.0));
    assert_eq!(v.cell_style(20.0), start, "too soon after the start");
    // After the dwell, a second event changes it from the cut's swap.
    let at = cellmode::MIN_DWELL_SECS + 20.0;
    b.close_device(1, 0);
    v.update(&b, at);
    assert_eq!(v.director.cut_at(), Some(at));
    let swap = v.director.swapped_at().unwrap();
    assert_ne!(
        v.cell_style(swap - 0.01),
        cellmode::next_style(CellMode::Auto, start)
    );
    assert_eq!(
        v.cell_style(swap),
        cellmode::next_style(CellMode::Auto, start)
    );
    assert!(swap > at, "the change waits for the swap");
}

/// Half blocks halve the pixel width, so in a small view an automatic mode
/// uses quarter blocks instead, and a style chosen by hand is kept.
#[test]
fn auto_never_picks_half_blocks_in_a_small_view_but_a_choice_is_kept() {
    let start_in = |cols: usize, rows: usize, watts: f32| {
        let mut b = two_boards();
        for i in 0..4 {
            b.set_power(i, watts + i as f32);
        }
        let mut v = Die3dView::new(cols, rows).with_cell_mode(CellMode::Auto);
        v.update(&b, 0.0);
        v.cell_style(0.0)
    };
    let big: std::collections::HashSet<_> = (0..60)
        .map(|i| start_in(150, 42, 30.0 + i as f32 * 0.7))
        .collect();
    assert!(
        big.contains(&CellStyle::HalfBlock),
        "control: it is used when it fits"
    );
    for i in 0..60 {
        let s = start_in(80, 24, 30.0 + i as f32 * 0.7);
        assert_ne!(s, CellStyle::HalfBlock, "watts {i}");
    }
    // By hand it is kept at any size.
    let mut v = two_die_view(80, 24);
    v.command("cells", "half", 0.0);
    assert_eq!(v.cell_style(0.0), CellStyle::HalfBlock);
}

/// The picture is drawn in the chosen style: the frame has the glyphs of
/// that style and the label flags still land on the dies in each.
#[test]
fn every_cell_style_draws_its_glyphs_and_keeps_the_flags() {
    for (mode, glyphs) in [
        (CellMode::Fixed(CellStyle::Braille), ('\u{2801}'..='\u{28ff}').collect::<Vec<_>>()),
        (
            CellMode::Fixed(CellStyle::Quadrant),
            "\u{2598}\u{259D}\u{2580}\u{2596}\u{258C}\u{259E}\u{259B}\u{2597}\u{259A}\u{2590}\u{259C}\u{2584}\u{2599}\u{259F}"
                .chars()
                .collect(),
        ),
        (CellMode::Fixed(CellStyle::HalfBlock), vec!['\u{2580}', '\u{2584}']),
    ] {
        let mut v = two_die_view(150, 42);
        v.command("cells", mode.state(CellStyle::Braille).split(": ").last().unwrap(), 0.0);
        let lines = v.render(0.0);
        let t = text(&lines);
        assert_eq!(t.len(), 42);
        assert!(
            t[..40].iter().any(|l| l.chars().any(|c| glyphs.contains(&c))),
            "{mode:?}: no glyph of its own in the scene"
        );
        // The same two chips are labelled whatever the cells.
        let labels = labels_in(&lines, 40);
        assert!(!labels.is_empty(), "{mode:?}: no labels");
        for (row, col, _) in labels {
            assert!(row < 40 && col < 150, "{mode:?}: a label off the scene");
        }
    }
}

/// Quarter blocks use the braille grid, so the picture is the same size and
/// shape as braille: the same cells are covered or empty.
#[test]
fn quarter_blocks_cover_the_cells_braille_covers() {
    // A cell is covered unless it is the transparent one: a space on the
    // terminal's own background with no colour of its own. Judged by the
    // style (the character cannot tell): a flat covered cell is a space in both.
    let covered = |style: &str| -> Vec<Vec<bool>> {
        let mut v = two_die_view(150, 42);
        v.command("clouds", "off", 0.0);
        v.command("labels", "off", 0.0);
        v.command("history", "off", 0.0);
        v.command("cells", style, 0.0);
        v.render(0.0)[..40]
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .flat_map(|sp| {
                        let transparent = sp.style.fg.is_none()
                            && sp.style.bg == Some(Color::Reset)
                            && sp.content.chars().all(|c| c == ' ');
                        sp.content.chars().map(move |_| !transparent)
                    })
                    .collect()
            })
            .collect()
    };
    let (braille, quarter) = (covered("braille"), covered("quarter"));
    let differing: usize = braille
        .iter()
        .zip(&quarter)
        .map(|(a, b)| a.iter().zip(b).filter(|(x, y)| x != y).count())
        .sum();
    let total: usize = braille
        .iter()
        .map(|r| r.iter().filter(|c| **c).count())
        .sum();
    // The same grid, so the same cells are covered; a quarter can miss a
    // lone pixel braille shows, so allow a few percent.
    assert!(total > 500, "the scene is drawn: {total}");
    assert!(
        differing * 20 < total,
        "{differing} of {total} cells differ"
    );
}

/// The blocks command never reaches braille, however the style changes.
#[test]
fn blocks_mode_stays_in_the_block_forms_through_many_events() {
    let mut b = two_boards();
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 0.0,
        tx_bytes_per_sec: 0.0,
    });
    let mut v = Die3dView::new(150, 42).with_cell_mode(CellMode::Blocks);
    v.director.set_idle_turn(false);
    v.update(&b, 0.0);
    let mut t = 0.0;
    let mut seen = std::collections::HashSet::new();
    for pid in 0..8 {
        t += cellmode::MIN_DWELL_SECS + 10.0;
        b.open_device(pid, 0);
        v.update(&b, t);
        b.close_device(pid, 0);
        seen.insert(v.cell_style(t + 2.0));
        assert!(v.cell_style(t + 2.0).is_block(), "braille at {t}");
    }
    assert_eq!(seen.len(), 2, "both block forms come up: {seen:?}");
}

/// A view nobody touches and nothing happens to still turns, once, after
/// the idle time: a spin with no board switch.
#[test]
fn a_quiet_view_spins_once_after_the_idle_time() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.update(&b, camera::IDLE_ROTATE_SECS - 1.0);
    assert!(v.director.cut_at().is_none(), "not yet");
    let at = camera::IDLE_ROTATE_SECS + 1.0;
    v.update(&b, at);
    assert_eq!(v.director.cut_at(), Some(at));
    assert_eq!(
        v.director.transition_kind(),
        Some(camera::TransitionKind::Spin)
    );
    assert_eq!(
        v.switcher.scene_at(at + 5.0),
        Scene::All,
        "the scene did not change"
    );
    // Not again at once.
    v.update(&b, at + 2.0);
    assert_eq!(v.director.cut_at(), Some(at));
}

#[test]
fn a_hand_chosen_board_is_never_changed_by_activity() {
    let mut b = two_boards();
    let mut v = Die3dView::new(150, 42);
    // Over 40 s of quiet and events: only the events are under test.
    v.director.set_idle_turn(false);
    v.update(&b, 0.0);
    // Board 1 by hand: `]` opens the board shown last (0), `]` again 1.
    v.handle_key(Die3dKey::NextBoard, 0.0);
    v.handle_key(Die3dKey::NextBoard, 0.0);
    assert_eq!(v.switcher.scene_at(0.0), Scene::Board(1));
    // Rises on board 0, again and again, well apart: no cut, no switch.
    for (on, off) in [(1.0, 5.0), (20.0, 25.0), (40.0, 45.0)] {
        b.set_power(0, 90.0);
        v.update(&b, on);
        b.set_power(0, 0.0);
        v.update(&b, off);
    }
    assert!(v.director.cut_at().is_none());
    // A rise on board 1 cuts the angle only.
    b.set_power(3, 90.0);
    v.update(&b, 50.0);
    assert_eq!(v.director.cut_at(), Some(50.0));
    v.update(&b, 120.0);
    assert_eq!(v.switcher.scene_at(120.0), Scene::Board(1));
}

#[test]
fn a_manual_scene_choice_cancels_the_auto_return() {
    // `]` after the automatic switch: the user's board stays.
    let (mut v, b) = switched_by_chip_2();
    v.update(&b, 1.5);
    v.handle_key(Die3dKey::NextBoard, 2.0);
    assert_eq!(v.switcher.scene_at(2.0), Scene::Board(0));
    v.update(&b, 60.0);
    assert_eq!(v.switcher.scene_at(60.0), Scene::Board(0));

    // `m` twice: back on the board the view chose, now as the user's.
    let (mut v, b) = switched_by_chip_2();
    v.update(&b, 1.5);
    v.handle_key(Die3dKey::ToggleScene, 2.0);
    assert_eq!(v.switcher.scene_at(2.0), Scene::All);
    v.handle_key(Die3dKey::ToggleScene, 2.1);
    assert_eq!(v.switcher.scene_at(2.1), Scene::Board(1));
    v.update(&b, 60.0);
    assert_eq!(v.switcher.scene_at(60.0), Scene::Board(1));

    // `m` before the midpoint cancels the pending switch too.
    let (mut v, b) = switched_by_chip_2();
    v.handle_key(Die3dKey::ToggleScene, 1.1);
    v.update(&b, 1.5);
    assert_eq!(v.switcher.scene_at(1.5), Scene::Board(0));
    v.update(&b, 60.0);
    assert_eq!(v.switcher.scene_at(60.0), Scene::Board(0));
}

#[test]
fn frames_a_whole_number_of_days_apart_are_identical() {
    // After a month of uptime, f32 view time steps by 0.25 s, so the
    // wave and the packets would move at 4 Hz. The scene's time wraps
    // once a day in f64 first, so a frame 30 days on is the frame of
    // the same time of day.
    let mut b = two_boards();
    b.set_power(0, 60.0);
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 2.0e9,
        tx_bytes_per_sec: 5.0e8,
    });
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let month = 86_400.0 * 30.0;
    for t0 in [1.3, 7.6, 1234.5678, 86_399.9] {
        let (now, _) = v.scene_pixels(t0).unwrap();
        let (later, _) = v.scene_pixels(t0 + month).unwrap();
        assert!(
            now.rgb == later.rgb && now.cover == later.cover,
            "at {t0} s and 30 days on"
        );
        // The instrument: half a second later the frame does differ.
        let (moved, _) = v.scene_pixels(t0 + 0.5).unwrap();
        assert!(now.rgb != moved.rgb, "nothing moved at {t0}");
    }
}

#[test]
fn resize_keeps_switches_history_and_load_calibration() {
    let mut b = two_boards();
    let mut v = Die3dView::with_reserved_rows(150, 42, 1);
    for t in 0..8 {
        b.set_power(2, 30.0 + t as f32);
        v.update(&b, t as f64);
    }
    // Every switch away from its default, a board chosen by hand.
    for k in [
        Die3dKey::ToggleBraille,
        Die3dKey::ToggleTransparent,
        Die3dKey::ToggleClouds,
        Die3dKey::ToggleHistory,
        Die3dKey::ToggleLabels,
        Die3dKey::CycleTransition,
        Die3dKey::ToggleAuto,
        Die3dKey::NextBoard,
        Die3dKey::NextBoard,
    ] {
        v.handle_key(k, 8.0);
    }
    let switches = v.switches_at(8.0);
    let scene = v.switcher.scene_at(8.0);
    let mode = v.director.transition_mode();
    let samples = v.history.power(2).map(<[_]>::len);
    assert_eq!(samples, Some(8));
    assert!(v.scales[&2].ready());
    let cloud_offset = v.cloud_clock.get().offset_px();
    let revision = v.revision;
    v.resize(200, 55, 1);
    assert_eq!((v.cols, v.rows, v.reserved_rows), (200, 55, 1));
    assert_eq!(v.switches_at(8.0), switches);
    assert_eq!(v.switcher.scene_at(8.0), scene);
    assert_eq!(v.director.transition_mode(), mode);
    assert!(!v.director.auto());
    assert_eq!(v.history.power(2).map(<[_]>::len), samples);
    assert!(v.scales[&2].ready());
    assert_eq!(v.scales[&2].len(), 8);
    assert_eq!(v.cloud_clock.get().offset_px(), cloud_offset);
    assert_eq!(v.chips.len(), 2, "the boards read before the resize stay");
    assert!(v.revision > revision, "a resize is a change to show");
}

#[test]
fn resize_drops_the_fit_cache_and_redraws_at_once() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    assert_eq!(v.render(1.0).len(), 42);
    assert!(!v.cache.borrow().fits.is_empty());
    assert!(v.cache.borrow().last.is_some());
    v.resize(120, 36, 0);
    {
        let cache = v.cache.borrow();
        assert!(cache.fits.is_empty(), "fits of the old size dropped");
        assert!(cache.spare.is_empty(), "buffers of the old size dropped");
        assert!(cache.last.is_none(), "the old frame dropped");
    }
    // 0.01 s later, well inside the frame interval: a fresh frame at the
    // new size, never the old one again.
    let lines = v.render(1.01);
    assert_eq!(lines.len(), 36);
    assert!(lines[0].width() <= 120);
    // Below 100 x 30 the smaller layout is drawn at once: one line per
    // row, the last the one-row footer. Growing again draws the full
    // layout.
    v.resize(80, 20, 1);
    let small = text(&v.render(1.02));
    assert_eq!(small.len(), 20, "{small:?}");
    assert!(small[19].starts_with("\u{2551} "), "{small:?}");
    v.resize(150, 42, 0);
    assert_eq!(v.render(1.03).len(), 42);
}

#[test]
fn render_during_a_cut_then_new_at_another_size_does_not_panic() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_transition_mode(TransitionMode::Chaos);
    v.handle_key(Die3dKey::CutNow, 5.0);
    assert!(v.director.chaos(5.3).is_some(), "mid-chaos");
    assert_eq!(v.render(5.3).len(), 42);
    // Every size must render, in every transition, at several points of
    // it: for a view made at that size, and for one resized to it in
    // the middle of the cut (as the TUI does on a terminal resize).
    for (c, r) in [(120, 35), (100, 30), (220, 60), (101, 31)] {
        for braille in [true, false] {
            for mode in [
                TransitionMode::Chaos,
                TransitionMode::Dissolve,
                TransitionMode::Spin,
            ] {
                let mut v = Die3dView::new(c, r);
                v.update(&b, 5.0);
                v.director.set_transition_mode(mode);
                if !braille {
                    v.handle_key(Die3dKey::ToggleBraille, 5.0);
                }
                v.handle_key(Die3dKey::CutNow, 5.0);
                for dt in [0.0, 0.3, 0.7, 1.2, 1.7] {
                    assert_eq!(
                        v.render(5.0 + dt).len(),
                        r,
                        "{c} x {r} braille={braille} {mode:?} +{dt}"
                    );
                }
                let mut w = Die3dView::new(150, 42);
                w.update(&b, 5.0);
                w.director.set_transition_mode(mode);
                if !braille {
                    w.handle_key(Die3dKey::ToggleBraille, 5.0);
                }
                w.handle_key(Die3dKey::CutNow, 5.0);
                w.render(5.3);
                w.resize(c, r, 0);
                for dt in [0.31, 0.7, 1.2, 1.7] {
                    assert_eq!(
                        w.render(5.0 + dt).len(),
                        r,
                        "resized to {c} x {r} braille={braille} {mode:?} +{dt}"
                    );
                }
            }
        }
    }
}

#[test]
fn key_to_die3d_maps_only_the_keys_that_mean_the_same_everywhere() {
    assert_eq!(key_to_die3d(']'), Some(Die3dKey::NextBoard));
    assert_eq!(key_to_die3d('['), Some(Die3dKey::PrevBoard));
    assert_eq!(key_to_die3d(' '), Some(Die3dKey::CutNow));
    // The letters that used to be this view's keys are slash commands now.
    for old in "mcCBTXLWH".chars().chain(['b', 'M', '3', 'x']) {
        assert_eq!(key_to_die3d(old), None, "{old:?}");
    }
}

#[test]
fn the_footer_says_estimate_until_per_tile_data_exists() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let all: Vec<ChipInputs> = v.chips.concat();
    assert!(!has_per_tile_data(&all));
    assert!(footer(&v.render(0.0)).contains(ESTIMATE_LABEL));
    // Measured data on one chip: the label goes. At 0.1, past the 1/20 s
    // for which `render` would return the frame it drew at 0.0: the data
    // is set here directly, not through a key or a change of boards.
    v.chips[1][0].tile_flow = Some(HashMap::new());
    assert!(has_per_tile_data(&v.chips.concat()));
    assert!(!footer(&v.render(0.1)).contains(ESTIMATE_LABEL));
}

#[test]
fn the_footer_names_the_mode_and_auto_cuts() {
    let b = two_boards();
    // 180 columns: the action keys fit after the switches from 172.
    let mut v = Die3dView::new(180, 42);
    v.update(&b, 0.0);
    let f = footer(&v.render(0.0));
    for want in [
        "braille",
        "/autocut on",
        "[ ] board",
        "/scene all boards",
        "space cut",
    ] {
        assert!(f.contains(want), "{want}: {f}");
    }
    v.handle_key(Die3dKey::ToggleBraille, 0.0);
    v.handle_key(Die3dKey::ToggleAuto, 0.0);
    let lines = v.render(0.0);
    assert_eq!(lines.len(), 42);
    let f = footer(&lines);
    assert!(
        f.contains("half-block") && f.contains("/autocut off"),
        "{f}"
    );
    // Half-block rows are one cell per pixel column.
    assert_eq!(text(&lines)[10].chars().count(), 180);
}

#[test]
fn render_draws_the_frame_not_the_bare_scene() {
    // Pins `render` to `frame`: a regression that drew `scene_pixels`
    // would skip the chaos noise, and this would fail.
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    // Labels and the history panel off: they are drawn over the cells,
    // and the bare scene has neither.
    v.handle_key(Die3dKey::ToggleLabels, 0.0);
    v.handle_key(Die3dKey::ToggleHistory, 0.0);
    v.director.set_transition_mode(TransitionMode::Chaos);
    v.handle_key(Die3dKey::CutNow, 10.0);
    let mid = v.director.swapped_at().unwrap();
    let scene_rows = 42 - FOOTER_ROWS;
    let rows = |lines: &[Line<'static>]| text(&lines[..scene_rows]);
    let bare = |t: f64| text(&cells::to_braille_lines(&v.scene_pixels(t).unwrap().0));
    // Mid-cut the rendered rows are scrambled, so they differ from the
    // bare scene at the same instant.
    assert_ne!(rows(&v.render(mid)), bare(mid));
    // After the transition the chaos is gone and render is the scene.
    let after = 10.0 + camera::CHAOS_SECS + 0.01;
    assert_eq!(rows(&v.render(after)), bare(after));
}

#[test]
fn the_tiers_are_measured_on_the_views_own_area() {
    let b = two_boards();
    // A 100 x 30 terminal under a one-row status bar: a 100 x 29 area,
    // which is below the full layout. The footer is one row.
    let mut v = Die3dView::with_reserved_rows(100, 29, 1);
    v.update(&b, 0.0);
    let t = text(&v.render(0.0));
    assert_eq!(t.len(), 29, "{t:?}");
    assert!(t[28].starts_with("\u{2551} "), "{}", t[28]);
    assert!(!t[27].starts_with("\u{2551} "), "{}", t[27]);
    // One more row and the full layout's two-row footer is drawn.
    let mut v = Die3dView::with_reserved_rows(100, 30, 1);
    v.update(&b, 0.0);
    let t = text(&v.render(0.0));
    assert_eq!(t.len(), 30);
    assert!(t[28].starts_with("\u{2551} "), "{}", t[28]);
    assert!(t[29].starts_with("\u{255a}"), "{}", t[29]);
}

#[test]
fn the_super_scene_falls_back_to_the_board_scene_when_it_does_not_fit() {
    // Four boards of two dies do not fit at the minimum size.
    let b = Fake::boards(&["p300"; 8], Some(TDP));
    let mut v = Die3dView::new(100, 30);
    v.update(&b, 0.0);
    let (_, shown) = v.scene_pixels(0.0).expect("the board scene fits");
    assert_eq!(shown, Shown::Fallback(0));
    let lines = v.render(0.0);
    assert_eq!(lines.len(), 30);
    let f = footer(&lines);
    assert!(f.contains("board 1/4") && f.contains("do not fit"), "{f}");
    // The fallback is the board scene, pixel for pixel. From the super
    // scene the first `]` opens the board shown last (index 0), and `[`
    // steps back to it after a second `]` has moved on.
    v.handle_key(Die3dKey::NextBoard, 0.0);
    assert_eq!(v.scene_pixels(0.0).unwrap().1, Shown::Board(0));
    v.handle_key(Die3dKey::NextBoard, 0.0);
    v.handle_key(Die3dKey::PrevBoard, 0.0);
    let (board, shown) = v.scene_pixels(0.0).unwrap();
    assert_eq!(shown, Shown::Board(0));
    v.handle_key(Die3dKey::ToggleScene, 0.0);
    let (fallback, _) = v.scene_pixels(0.0).unwrap();
    assert_eq!(fallback.rgb, board.rgb);
    // At the reference size they fit, and the footer says all boards.
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    assert_eq!(v.scene_pixels(0.0).unwrap().1, Shown::All);
    assert!(footer(&v.render(0.0)).contains("all boards"));
}

#[test]
fn drawing_uses_the_camera_at_now_until_the_swap() {
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut still = Die3dView::new(150, 42);
    still.update(&b, 0.0);
    let mut cut = Die3dView::new(150, 42);
    cut.update(&b, 0.0);
    cut.director.set_transition_mode(TransitionMode::Chaos);
    cut.handle_key(Die3dKey::CutNow, 10.0);
    let at = cut.director.swapped_at().unwrap();
    // Before the swap the old camera is drawn, so the scene (before the
    // chaos noise) is the uncut view's.
    let t = at - 0.05;
    assert_eq!(
        cut.scene_pixels(t).unwrap().0.rgb,
        still.scene_pixels(t).unwrap().0.rgb
    );
    // After it the new camera is drawn.
    let t = at + 0.05;
    assert_ne!(
        cut.scene_pixels(t).unwrap().0.rgb,
        still.scene_pixels(t).unwrap().0.rgb
    );
}

#[test]
fn chaos_scrambles_the_frame_during_a_cut() {
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    // No cut yet: the frame is the scene.
    assert_eq!(
        v.frame(3.0).unwrap().0.rgb,
        v.scene_pixels(3.0).unwrap().0.rgb
    );
    v.director.set_transition_mode(TransitionMode::Chaos);
    v.handle_key(Die3dKey::CutNow, 10.0);
    let mid = v.director.swapped_at().unwrap();
    assert_ne!(
        v.frame(mid).unwrap().0.rgb,
        v.scene_pixels(mid).unwrap().0.rgb
    );
    // After the transition the frame is clean again.
    let after = 10.0 + camera::CHAOS_SECS + 0.01;
    assert_eq!(
        v.frame(after).unwrap().0.rgb,
        v.scene_pixels(after).unwrap().0.rgb
    );
}

#[test]
fn opaque_mode_covers_every_pixel() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    // Transparent (the default): the corner pixel is not covered.
    let (buf, _) = v.scene_pixels(0.0).unwrap();
    assert!(!buf.covered(0, 0));
    v.handle_key(Die3dKey::ToggleTransparent, 0.1);
    let (buf, _) = v.scene_pixels(0.2).unwrap();
    assert!((0..buf.h).all(|y| (0..buf.w).all(|x| buf.covered(x, y))));
}

#[test]
fn t_toggles_transparency_and_the_footer_says_which() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    assert!(v.transparent);
    let f = footer(&v.render(0.0));
    assert!(f.contains("transparent") && !f.contains("black"), "{f}");
    assert!(f.contains("/bg transparent"), "key list: {f}");
    v.handle_key(Die3dKey::ToggleTransparent, 0.1);
    assert!(!v.transparent);
    let f = footer(&v.render(0.2));
    assert!(f.contains("/bg black"), "{f}");
    v.handle_key(Die3dKey::ToggleTransparent, 0.3);
    assert!(v.transparent);
}

/// The switch strip as the footer's second row shows it at the view's
/// width. Built from `switch_strip` directly, so the fallbacks can be
/// checked at widths where the size tier leaves no second row.
fn strip(v: &Die3dView) -> String {
    format!(
        "\u{255a}\u{2550} {}",
        v.switch_strip(0.0, v.cols.saturating_sub(BAR_WIDTH))
    )
}

/// A view over two boards whose first board label is 12 characters.
fn wide_label_view(cols: usize) -> Die3dView {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.labels[0] = "abcdefghijkl".to_string();
    v.cols = cols;
    v
}

#[test]
fn the_footer_names_every_switch_and_its_state() {
    let b = two_boards();
    // Wide enough for the action keys after every switch.
    let mut v = Die3dView::new(190, 42);
    v.update(&b, 0.0);
    let f = footer(&v.render(0.0));
    for s in v.switches() {
        let pair = format!("{} {}", s.cmd, s.state);
        assert!(f.contains(&pair), "{pair}: {f}");
    }
    assert!(
        f.contains("[ ] board") && f.contains("space cut") && f.contains("3 leave"),
        "{f}"
    );
}

#[test]
fn the_action_keys_survive_below_172_columns_once_the_state_words_fit() {
    // 110 columns holds the state words and the action keys but not the
    // `/cmd state` form, so the keys must still be listed.
    let v = wide_label_view(110);
    let s = strip(&v);
    assert!(s.contains("space cut"), "{s}");
    assert!(!s.contains("/cells"), "the full form does not fit: {s}");
    assert!(s.chars().count() <= 110, "{s}");
    for sw in v.switches() {
        assert!(s.contains(sw.state), "{} in {s}", sw.state);
    }
}

#[test]
fn the_footer_keeps_every_switch_state_at_100_columns() {
    // 100 columns: the full layout's two rows hold every switch state
    // and the estimate label, each row within the width.
    let v = wide_label_view(100);
    let f = text(&v.footer(0.0, Some(Shown::All)));
    assert_eq!(f.len(), 2, "{f:?}");
    let joined = f.join("\n");
    for s in v.switches() {
        assert!(joined.contains(s.state), "{} in {joined}", s.state);
    }
    assert!(joined.contains(ESTIMATE_LABEL), "{joined}");
    for line in &f {
        assert!(line.chars().count() <= 100, "{line}");
    }
    // 80 columns: the size tier keeps the status line alone, with the
    // estimate label. The switch strip at that width still keeps every
    // state; the view does not draw it there.
    let v = wide_label_view(80);
    let f = text(&v.footer(0.0, Some(Shown::All)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains(ESTIMATE_LABEL), "{f:?}");
    assert!(f[0].chars().count() <= 80, "{f:?}");
    let s = strip(&v);
    for sw in v.switches() {
        assert!(s.contains(sw.state), "{} in {s}", sw.state);
    }
    assert!(s.chars().count() <= 80, "{s}");
}

#[test]
fn action_keys_are_dropped_before_switch_states() {
    let v = wide_label_view(180);
    let full = strip(&v);
    assert!(full.contains("space cut"), "{full}");
    // Just wide enough for the switches and nothing after them.
    let switches_only = full.split(" · [ ]").next().unwrap().chars().count();
    let mut v = v;
    v.cols = switches_only;
    let s = strip(&v);
    assert!(!s.contains("space cut") && !s.contains("leave"), "{s}");
    for sw in v.switches() {
        assert!(s.contains(sw.state), "{}: {s}", sw.state);
    }
}

#[test]
fn switches_reflect_the_view_state() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    const CMDS: [&str; 8] = [
        "/cells",
        "/bg",
        "/autocut",
        "/transition",
        "/scene",
        "/labels",
        "/clouds",
        "/history",
    ];
    let state =
        |v: &Die3dView, c: &str| v.switches().into_iter().find(|s| s.cmd == c).unwrap().state;
    let before: Vec<_> = CMDS.iter().map(|c| state(&v, c)).collect();
    assert_eq!(
        before,
        [
            "braille",
            "transparent",
            "on",
            "rotate",
            "all boards",
            "on",
            "on",
            "on"
        ]
    );
    for k in [
        Die3dKey::ToggleBraille,
        Die3dKey::ToggleTransparent,
        Die3dKey::ToggleAuto,
        Die3dKey::CycleTransition,
        Die3dKey::ToggleScene,
        Die3dKey::ToggleLabels,
        Die3dKey::ToggleClouds,
        Die3dKey::ToggleHistory,
    ] {
        v.handle_key(k, 0.0);
    }
    let after: Vec<_> = CMDS.iter().map(|c| state(&v, c)).collect();
    assert_eq!(
        after,
        [
            "half-block",
            "black",
            "off",
            "chaos",
            "board",
            "off",
            "off",
            "off"
        ]
    );
}

#[test]
fn every_switch_is_a_listed_command_and_every_toggle_has_a_switch() {
    let v = Die3dView::new(150, 42);
    let switches = v.switches();
    for sw in &switches {
        let verb = sw.cmd.trim_start_matches('/');
        assert!(commands::is_command(verb), "{} is not in COMMANDS", sw.cmd);
    }
    let toggling = Die3dKey::ALL.iter().filter(|k| k.toggles()).count();
    assert_eq!(
        toggling,
        switches.len(),
        "a toggle with no switch, or the reverse"
    );
}

/// A key is for what means the same in every view. A key that flips one of
/// this view's own settings would use up a letter; those are commands.
#[test]
fn only_the_keys_that_do_not_toggle_have_a_character() {
    for (i, k) in Die3dKey::ALL.iter().enumerate() {
        assert!(!Die3dKey::ALL[..i].contains(k), "{k:?} twice");
        let has_char = (0u8..128).any(|c| key_to_die3d(char::from(c)) == Some(*k));
        // The arrows are the one exception: they are keys, but not
        // characters (the TUI maps them, see `die3d_pan_for`).
        let is_arrow = matches!(
            k,
            Die3dKey::PanLeft | Die3dKey::PanRight | Die3dKey::PanUp | Die3dKey::PanDown
        );
        assert_eq!(has_char, !k.toggles() && !is_arrow, "{k:?}");
    }
}

#[test]
fn the_switch_strip_falls_back_to_bare_then_a_cut() {
    let v = wide_label_view(150);
    let sw = v.switches();
    let full = sw
        .iter()
        .map(|s| format!("{} {}", s.cmd, s.state))
        .collect::<Vec<_>>()
        .join(SEP);
    let bare = sw.iter().map(|s| s.state).collect::<Vec<_>>().join(SEP);
    let bare_keys = format!("{bare}{SEP}{ACTION_KEYS}");
    let (full_len, bare_len) = (full.chars().count(), bare.chars().count());
    let keys_len = bare_keys.chars().count();
    assert!(bare_len < keys_len && keys_len < full_len);
    let at = |cols: usize| {
        let v = wide_label_view(cols);
        let s = strip(&v);
        assert!(s.chars().count() <= cols, "{cols}: {s}");
        s.trim_start_matches("\u{255a}\u{2550} ").to_string()
    };
    // Exactly wide enough for every command with its state.
    assert_eq!(at(full_len + BAR_WIDTH), full);
    // One column short: the commands go, the state words and the action
    // keys stay whole.
    assert_eq!(at(full_len - 1 + BAR_WIDTH), bare_keys);
    assert_eq!(at(keys_len + BAR_WIDTH), bare_keys);
    // One column short of that: the action keys go, the state words stay.
    assert_eq!(at(keys_len - 1 + BAR_WIDTH), bare);
    assert_eq!(at(bare_len + BAR_WIDTH), bare);
    // One column short of bare: the last resort cuts at a character and
    // marks the cut with an ellipsis.
    let cut = at(bare_len - 1 + BAR_WIDTH);
    assert_eq!(cut.chars().count(), bare_len - 1);
    assert!(cut.ends_with('\u{2026}'), "{cut}");
    assert!(bare.starts_with(cut.trim_end_matches('\u{2026}')), "{cut}");
}

#[test]
fn opaque_mode_fills_pure_black() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.handle_key(Die3dKey::ToggleTransparent, 0.0);
    // Clouds off: they sit on the black fill and would cover corners.
    v.handle_key(Die3dKey::ToggleClouds, 0.0);
    let lines = v.render(0.0);
    let black = Some(Color::Rgb(0, 0, 0));
    // The scene corners are empty floor: a flat cell on pure black.
    let scene_rows = 42 - FOOTER_ROWS;
    for row in [0, scene_rows - 1] {
        let spans = &lines[row].spans;
        for span in [spans.first().unwrap(), spans.last().unwrap()] {
            assert_eq!(span.style.bg, black, "row {row}: {span:?}");
        }
    }
    // In pixels: every uncovered-by-the-scene pixel is (0, 0, 0).
    let (buf, _) = v.scene_pixels(0.0).unwrap();
    assert_eq!(buf.get(0, 0), [0, 0, 0]);
    assert!(buf.covered(0, 0));
    // Half-block too.
    v.handle_key(Die3dKey::ToggleBraille, 0.0);
    let lines = v.render(0.0);
    assert_eq!(lines[0].spans[0].style.bg, black, "{:?}", lines[0].spans[0]);
}

/// The two frames a transition at `now` mixes: the scene from the old
/// camera and the scene from the new one, each at the same `now`.
fn endpoints(v: &Die3dView, now: f64) -> (PixelBuf, PixelBuf) {
    let (from, to) = v.director.cameras();
    let (s0, s1) = v.transition_scenes(now);
    (
        v.scene_with(now, s0, Aim::At(from)).unwrap().0,
        v.scene_with(now, s1, Aim::At(to)).unwrap().0,
    )
}

#[test]
fn a_spin_frame_midway_differs_from_both_endpoints_and_stays_inside() {
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.handle_key(Die3dKey::CycleTransition, 0.0);
    v.handle_key(Die3dKey::CycleTransition, 0.0);
    v.handle_key(Die3dKey::CycleTransition, 0.0);
    assert_eq!(v.director.transition_mode(), TransitionMode::Spin);
    // Cut until the camera turns (a cut can also change only the pitch).
    let mut start = 10.0;
    v.handle_key(Die3dKey::CutNow, start);
    while {
        let (from, to) = v.director.cameras();
        from.quarter_turns == to.quarter_turns
    } {
        start += 10.0;
        v.handle_key(Die3dKey::CutNow, start);
    }
    assert_eq!(v.director.transition_kind(), Some(TransitionKind::Spin));
    let mid = start + TransitionKind::Spin.secs() / 2.0;
    let (old, new) = endpoints(&v, mid);
    let frame = v.frame(mid).unwrap().0;
    assert_ne!(frame.rgb, old.rgb);
    assert_ne!(frame.rgb, new.rgb);
    // Pinned to `lerp_view`: the frame is the scene projected part way
    // between the two cameras, at the transition's progress.
    let (from, to) = v.director.cameras();
    let p = v.director.progress(mid).unwrap();
    let between = v
        .scene_with(mid, v.switcher.scene_at(mid), Aim::Between(from, to, p))
        .unwrap()
        .0;
    assert_eq!(frame.rgb, between.rgb);
    // The frames along the way move: a spin is animated, not a cut. 15
    // frames a tenth of a second apart are mostly distinct.
    let mut frames: Vec<Vec<[u8; 3]>> = (1..16)
        .map(|k| v.frame(start + k as f64 * 0.1).unwrap().0.rgb)
        .collect();
    frames.dedup();
    assert!(frames.len() >= 10, "{} distinct of 15", frames.len());
    assert_eq!(v.render(mid).len(), 42);

    // Every preset to every other, part way along, stays inside the
    // view's own buffer (300 x 160 pixels: 40 braille rows above the
    // footer) for a one- and a two-die board and four boards of two dies.
    //
    // All 36 pairs at every eighth of the way are checked without
    // drawing: the grid corners with the allowance `fit_pixels` makes for
    // a block (`scene::corner_outside`). Drawing them all took about
    // 19 s; real pictures are drawn for a few pairs below.
    let (pw, ph) = (300i32, 160i32);
    let grids = [
        ("1 die", board_grid(1)),
        ("2 dies", board_grid(2)),
        ("4 x 2", super_grid(&[2, 2, 2, 2]).0),
    ];
    for (what, grid) in grids {
        let base = fit_pixels(pw, ph, grid).unwrap();
        for from in camera::PRESETS {
            for to in camera::PRESETS {
                for i in 0..=8 {
                    let iso = Camera::lerp_view(&from, &to, i as f32 / 8.0, &base.iso, grid);
                    let out = scene::corner_outside(&iso, grid, pw, ph);
                    assert!(out.is_none(), "{what} {from:?} -> {to:?} at {i}/8: {out:?}");
                }
            }
        }
    }
    // Drawn: pairs that turn one quarter each way, turn a half, and
    // change pitch while turning, at a quarter, half and three quarters.
    let p = camera::PRESETS;
    let pairs = [
        (p[0], p[1]),
        (p[3], p[0]),
        (p[1], p[3]),
        (p[4], p[2]),
        (p[2], p[5]),
        (p[5], p[4]),
    ];
    const PAD: i32 = 80;
    let one: Vec<ChipInputs> = v.chips[0].clone();
    let two = vec![one[0].clone(), one[0].clone()];
    let four: Vec<Vec<ChipInputs>> = (0..4).map(|_| two.clone()).collect();
    let draws: [(&str, (usize, usize), &dyn Fn(&mut PixelBuf, &SceneFit)); 3] = [
        ("1 die", board_grid(1), &|buf, f| {
            draw_board_scene(buf, &one, f, 1.3, Detail::Full)
        }),
        ("2 dies", board_grid(2), &|buf, f| {
            draw_board_scene(buf, &two, f, 1.3, Detail::Full)
        }),
        ("4 x 2", super_grid(&[2, 2, 2, 2]).0, &|buf, f| {
            draw_super_scene(buf, &four, f, 1.3)
        }),
    ];
    for (what, grid, draw) in draws {
        let base = fit_pixels(pw, ph, grid).unwrap();
        for (from, to) in pairs {
            for i in [2, 4, 6] {
                let mut iso = Camera::lerp_view(&from, &to, i as f32 / 8.0, &base.iso, grid);
                iso.ox += PAD;
                iso.oy += PAD;
                let f = SceneFit { iso, ..base };
                let mut buf = PixelBuf::new((pw + 2 * PAD) as usize, (ph + 2 * PAD) as usize);
                draw(&mut buf, &f);
                for y in 0..buf.h {
                    for x in 0..buf.w {
                        let (xi, yi) = (x as i32 - PAD, y as i32 - PAD);
                        assert!(
                            !buf.covered(x, y) || (0..pw).contains(&xi) && (0..ph).contains(&yi),
                            "{what} {from:?} -> {to:?} at {i}/8: pixel {xi},{yi}"
                        );
                    }
                }
            }
        }
    }
}

/// Pixels where `frame` matches `a` and not `b`.
fn only_from(frame: &PixelBuf, a: &PixelBuf, b: &PixelBuf) -> usize {
    (0..frame.rgb.len())
        .filter(|&i| {
            let px = |p: &PixelBuf| (p.rgb[i], p.cover[i]);
            px(frame) == px(a) && px(frame) != px(b)
        })
        .count()
}

#[test]
fn a_dissolve_frame_midway_contains_pixels_of_both_frames() {
    // A camera-only cut.
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_transition_mode(TransitionMode::Dissolve);
    v.handle_key(Die3dKey::CutNow, 10.0);
    let mid = 10.0 + TransitionKind::Dissolve.secs() / 2.0;
    let (old, new) = endpoints(&v, mid);
    let frame = v.frame(mid).unwrap().0;
    assert!(only_from(&frame, &old, &new) > 500, "old pixels");
    assert!(only_from(&frame, &new, &old) > 500, "new pixels");
    assert_eq!(v.render(mid).len(), 42);

    // A scene change: the old frame is every board, the new one board 2.
    let mut b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_transition_mode(TransitionMode::Dissolve);
    b.set_power(2, 90.0);
    v.update(&b, 1.0);
    assert_eq!(v.director.transition_kind(), Some(TransitionKind::Dissolve));
    // Past the midpoint, after the switch has been committed.
    let late = 1.0 + 0.7;
    v.update(&b, late);
    assert_eq!(v.switcher.scene_at(late), Scene::Board(1));
    assert_eq!(v.transition_scenes(late), (Scene::All, Scene::Board(1)));
    let (old, new) = endpoints(&v, late);
    assert_eq!(
        v.scene_with(late, Scene::All, Aim::At(v.director.cameras().0))
            .unwrap()
            .1,
        Shown::All
    );
    let frame = v.frame(late).unwrap().0;
    assert!(only_from(&frame, &old, &new) > 200, "old scene pixels");
    assert!(only_from(&frame, &new, &old) > 200, "new scene pixels");
    // Once it ends only the new scene is left.
    let after = 1.0 + TransitionKind::Dissolve.secs() + 0.01;
    assert_eq!(
        v.frame(after).unwrap().0.rgb,
        v.scene_pixels(after).unwrap().0.rgb
    );
}

/// Row 1 of the footer: the line above the switch strip.
fn status_row(lines: &[Line<'static>]) -> String {
    text(lines)[lines.len() - 2].clone()
}

/// A view in a fixed transition mode (cut by hand at `at`).
fn cut_view(b: &Fake, mode: TransitionMode, at: f64) -> Die3dView {
    let mut v = Die3dView::new(150, 42);
    v.update(b, 0.0);
    v.director.set_transition_mode(mode);
    v.handle_key(Die3dKey::CutNow, at);
    v
}

#[test]
fn the_footer_shows_the_camera_caption() {
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    // The first camera is preset 0: quarter turn 0 at Iso pitch.
    let row = status_row(&v.render(0.0));
    assert!(row.contains("view: front-right"), "{row}");
    // After the scene name and the estimate label.
    let scene = row.find("all boards").unwrap();
    let est = row.find(ESTIMATE_LABEL).unwrap();
    let cap = row.find("view: ").unwrap();
    assert!(scene < est && est < cap, "{row}");
    // After a cut has finished, it names the new camera.
    v.director.set_transition_mode(TransitionMode::Chaos);
    v.handle_key(Die3dKey::CutNow, 10.0);
    let row = status_row(&v.render(11.0));
    let want = format!("view: {}", v.director.camera().side_name());
    assert!(row.contains(&want), "{want}: {row}");
    assert!(!row.contains("->"), "{row}");
}

#[test]
fn the_caption_shows_from_and_to_during_a_transition() {
    let b = two_boards();
    for mode in [
        TransitionMode::Chaos,
        TransitionMode::Dissolve,
        TransitionMode::Spin,
    ] {
        let v = cut_view(&b, mode, 10.0);
        let (from, to) = v.director.cameras();
        let want = format!("view: {} -> {}", from.side_name(), to.side_name());
        // Before and after the swap alike: the whole transition.
        for dt in [
            0.05,
            0.5,
            v.director.transition_kind().unwrap().secs() - 0.05,
        ] {
            let row = status_row(&v.render(10.0 + dt));
            assert!(row.contains(&want), "{mode:?} +{dt}: {row}");
        }
        let row = status_row(&v.render(12.0));
        assert!(row.contains(&format!("view: {}", to.side_name())), "{row}");
        assert!(!row.contains("->"), "{row}");
    }
}

#[test]
fn the_caption_is_dropped_before_the_estimate_label_when_narrow() {
    // A transition caption: `view: <from> -> <to>`.
    let mut v = wide_label_view(150);
    v.director.set_transition_mode(TransitionMode::Spin);
    v.handle_key(Die3dKey::CutNow, 10.0);
    let now = 10.1;
    let (from, to) = v.director.cameras();
    let caption = format!("view: {} -> {}", from.side_name(), to.side_name());
    // At every width the estimate label stays, and the caption is either
    // there whole or absent: never a cut-off part of it.
    let (mut shown, mut dropped) = (0, 0);
    for cols in 80..=150 {
        v.cols = cols;
        let row = status_text(&v, now);
        assert!(row.contains(ESTIMATE_LABEL), "{cols}: {row}");
        assert!(row.chars().count() <= cols, "{cols}: {row}");
        if row.contains(&caption) {
            shown += 1;
        } else {
            assert!(!row.contains("view:"), "{cols}: {row}");
            dropped += 1;
        }
    }
    // Not vacuous: the sweep crosses the width where it stops fitting.
    assert!(shown > 0 && dropped > 0, "{shown} shown, {dropped} dropped");
}

/// Footer row 1 as text, for the shown board 0, at `now`.
fn status_text(v: &Die3dView, now: f64) -> String {
    v.footer(now, Some(Shown::Board(0)))[0]
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect()
}

#[test]
fn the_caption_is_never_cut_and_the_note_never_left_as_a_stub() {
    // The mock's devices: a Grayskull and a Wormhole not drawn, one
    // Blackhole drawn. Cuts by hand collect resting and transition
    // captions, short and long (`top-down, ...` is the longest name).
    let b = Fake::boards(&["e150", "n150", "p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let note = v.undrawn_note();
    let mut kinds = std::collections::HashSet::new();
    for k in 0..12 {
        let at = 10.0 * (k + 1) as f64;
        v.handle_key(Die3dKey::CutNow, at);
        for now in [at + 0.1, at + 5.0] {
            let caption = v.caption(now);
            kinds.insert((caption.contains("->"), caption.contains("top-down")));
            // From the narrowest row the view allows (100 columns) up.
            for cols in 100..=163 {
                v.cols = cols;
                let row = status_text(&v, now);
                let what = format!("{cols} cols, {caption:?}: {row:?}");
                assert!(row.chars().count() <= cols, "{what}");
                assert!(row.contains(ESTIMATE_LABEL), "{what}");
                assert!(row.contains(&caption) || !row.contains("view:"), "{what}");
                for stub in [" \u{b7}\u{2026}", " \u{b7} \u{2026}", " \u{b7}", " \u{b7} "] {
                    assert!(!row.ends_with(stub), "stub {stub:?}: {what}");
                }
                // The note is whole, absent, or cut after at least one
                // of its own characters.
                if let Some(i) = row.find("chip 0") {
                    let tail = &row[i..];
                    assert!(tail == note || tail.ends_with('\u{2026}'), "{what}");
                }
            }
            v.cols = 150;
        }
    }
    // Short and long, resting and transition captions were all seen.
    for want in [(false, false), (false, true), (true, false), (true, true)] {
        assert!(kinds.contains(&want), "{want:?} not seen: {kinds:?}");
    }
}

/// Every label in the first `rows` rows: (row, start column, text) for
/// each `BH<n>`, with its ` <w>W` power when one follows.
fn labels_in(lines: &[Line<'static>], rows: usize) -> Vec<(usize, usize, String)> {
    let digits = |c: &[char], mut j: usize| {
        while c.get(j).is_some_and(char::is_ascii_digit) {
            j += 1;
        }
        j
    };
    let mut found = Vec::new();
    for (r, row) in text(lines).iter().enumerate().take(rows) {
        let c: Vec<char> = row.chars().collect();
        let mut i = 0;
        while i + 2 < c.len() {
            if !(c[i] == 'B' && c[i + 1] == 'H' && c[i + 2].is_ascii_digit()) {
                i += 1;
                continue;
            }
            let mut end = digits(&c, i + 2);
            if c.get(end) == Some(&' ') {
                let w = digits(&c, end + 1);
                if w > end + 1 && c.get(w) == Some(&'W') {
                    end = w + 1;
                }
            }
            found.push((r, i, c[i..end].iter().collect()));
            i = end;
        }
    }
    found
}

#[test]
fn each_die_gets_a_flag_with_its_name_and_power() {
    // One p300 board: two dies.
    let mut b = Fake::boards(&["p300"; 2], Some(TDP));
    b.set_power(0, 44.0);
    b.set_power(1, 51.6);
    for braille in [true, false] {
        let mut v = Die3dView::new(150, 42);
        v.update(&b, 0.0);
        // The history panel lists the chip names too: off, so only the
        // flags are found.
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        if !braille {
            v.handle_key(Die3dKey::ToggleBraille, 0.0);
        }
        let lines = v.render(0.0);
        let found = labels_in(&lines, 40);
        let texts: Vec<&str> = found.iter().map(|f| f.2.as_str()).collect();
        assert_eq!(texts, ["BH0 44W", "BH1 52W"], "braille={braille}");
        // Each text is on a flag: the cell before it is the pole's corner
        // (or the padding after it) and a pin `●` stands on its pole.
        let rows = text(&lines);
        let all: String = rows.concat();
        assert_eq!(
            all.matches('●').count(),
            2,
            "one pin each (braille={braille})"
        );
        for (r, c, _) in &found {
            let row: Vec<char> = rows[*r].chars().collect();
            let corner = row[c.saturating_sub(2)];
            assert!(
                matches!(corner, '┌' | '└')
                    || row.get(c + 7 + 1) == Some(&'┐')
                    || row.get(c + 7 + 1) == Some(&'┘'),
                "{r},{c}: {corner} (braille={braille})"
            );
        }
    }
    // Without a power reading the label is the name alone.
    let mut b = Fake::boards(&["p150"], Some(TDP));
    b.telem[0].power = None;
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.handle_key(Die3dKey::ToggleHistory, 0.0);
    let found = labels_in(&v.render(0.0), 40);
    assert_eq!(
        found.iter().map(|f| f.2.as_str()).collect::<Vec<_>>(),
        ["BH0"]
    );
}

#[test]
fn labels_are_hidden_during_a_transition() {
    let b = two_boards();
    for mode in [
        TransitionMode::Chaos,
        TransitionMode::Dissolve,
        TransitionMode::Spin,
    ] {
        let mut v = cut_view(&b, mode, 10.0);
        // The history panel stays through a transition and lists the
        // chip names: off, so only the labels are found.
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
        assert!(!labels_in(&v.render(9.9), 40).is_empty(), "{mode:?} before");
        let secs = v.director.transition_kind().unwrap().secs();
        for dt in [0.0, 0.2, secs / 2.0, secs - 0.01] {
            let found = labels_in(&v.render(10.0 + dt), 40);
            assert!(found.is_empty(), "{mode:?} +{dt}: {found:?}");
        }
        assert!(
            !labels_in(&v.render(10.0 + secs), 40).is_empty(),
            "{mode:?} after"
        );
    }
}

#[test]
fn labels_never_overwrite_the_footer_rows_or_leave_the_view() {
    // Through the view: every size from the minimum up, both cell modes,
    // one board and four, after cuts to every preset. The footer rows are
    // exactly the footer, every row is the view's width, and the labels
    // found are all in the scene rows.
    let one = Fake::boards(&["p300"; 2], Some(TDP));
    let four = Fake::boards(&["p300"; 8], Some(TDP));
    for (c, r) in [(100, 30), (101, 31), (120, 35), (150, 42), (220, 60)] {
        for b in [&one, &four] {
            for braille in [true, false] {
                let mut v = Die3dView::new(c, r);
                v.update(b, 0.0);
                v.director.set_auto(false);
                if !braille {
                    v.handle_key(Die3dKey::ToggleBraille, 0.0);
                }
                let (mut labelled, mut drawn) = (0, 0);
                for k in 0..6 {
                    let now = 10.0 * k as f64;
                    if k > 0 {
                        v.handle_key(Die3dKey::CutNow, now - 5.0);
                    }
                    let lines = v.render(now);
                    assert_eq!(lines.len(), r);
                    let t = text(&lines);
                    let scene_rows = r - FOOTER_ROWS;
                    // The footer rows are the footer's: its two bars.
                    assert!(t[scene_rows].starts_with("\u{2551} "), "{}", t[scene_rows]);
                    assert!(t[r - 1].starts_with("\u{255a}\u{2550} "), "{}", t[r - 1]);
                    // A scene row is the view's width. Where no scene
                    // fits (half-block at the smallest sizes) the rows
                    // hold the chip summary instead: lines at most the
                    // view's width, then empty rows.
                    let hint_only = t[0].starts_with("BH0");
                    for (i, row) in t[..scene_rows].iter().enumerate() {
                        let n = row.chars().count();
                        let ok = n == c || (hint_only && n <= c);
                        assert!(ok, "{c} x {r} row {i}: {n}");
                    }
                    // Every label found anywhere is on a scene row.
                    let all = labels_in(&lines, r);
                    assert!(all.iter().all(|l| l.0 < scene_rows), "{c} x {r}: {all:?}");
                    labelled += all.len();
                    drawn += !hint_only as usize;
                }
                // Not vacuous: wherever a scene was drawn, labels were.
                assert!(
                    drawn == 0 || labelled > 0,
                    "{c} x {r}: no label drawn at all"
                );
            }
        }
    }

    // At the layer that writes: flags whose cells are on the footer rows,
    // past the right edge and hanging off the left edge.
    let width = 20;
    let scene_rows = 3;
    let mut lines: Vec<Line<'static>> = (0..scene_rows + FOOTER_ROWS)
        .map(|i| Line::from(Span::raw(format!("{i}").repeat(width))))
        .collect();
    let before = text(&lines);
    let style = Style::default().fg(Color::White);
    let flag = |pin: (i32, i32), rise: i32, left: bool, t: &str| {
        Some(Flag {
            pin,
            rise,
            left,
            text: t.to_string(),
        })
    };
    let flags = vec![
        // A pole standing on the footer rows.
        flag((5, scene_rows as i32 + 1), 3, false, "BH8 10W"),
        // A flag wholly past the right edge.
        flag((width as i32 + 3, 2), 1, false, "BH7"),
        // A flag flying left off the left edge.
        flag((3, 2), 1, true, "BH0 123W"),
        None,
    ];
    draw_flags(&mut lines, scene_rows, width, &flags, style, style);
    let after = text(&lines);
    assert_eq!(after[scene_rows..], before[scene_rows..], "footer rows");
    assert!(
        after.iter().all(|r| !r.contains("BH7")),
        "past the edge: {after:?}"
    );
    // The left-flying text is cut at column 0 (its last two characters
    // show), the pole's corner and padding follow, then the other flag.
    assert!(
        after[1].starts_with("3W \u{2510}1\u{250c} BH8 10W"),
        "{}",
        after[1]
    );
    // The pin and the pole's last cell above the footer.
    assert!(after[2].starts_with("222\u{25cf}2\u{2502}"), "{}", after[2]);
    for row in &after {
        assert_eq!(row.chars().count(), width, "{row}");
    }
}

#[test]
fn flags_never_cover_a_drawn_cell_but_the_pin() {
    // Flags are written only on cells the scene left empty. Without the
    // clouds, the cells a flag changes were blank before it was written,
    // whatever the board count, cell mode, size or camera.
    let one = Fake::boards(&["p300"; 2], Some(TDP));
    let four = Fake::boards(&["p300", "p300", "p150", "p300"], Some(TDP));
    let mut flagged = 0;
    for (c, r) in [(100, 31), (120, 37), (150, 42), (220, 60)] {
        for b in [&one, &four] {
            for braille in [true, false] {
                let mut v = Die3dView::new(c, r);
                v.update(b, 0.0);
                v.director.set_auto(false);
                v.handle_key(Die3dKey::ToggleClouds, 0.0);
                v.handle_key(Die3dKey::ToggleHistory, 0.0);
                if !braille {
                    v.handle_key(Die3dKey::ToggleBraille, 0.0);
                }
                for k in 0..6 {
                    let now = 10.0 * k as f64;
                    if k > 0 {
                        v.handle_key(Die3dKey::CutNow, now - 5.0);
                    }
                    v.handle_key(Die3dKey::ToggleLabels, now);
                    let off = text(&v.render(now));
                    v.handle_key(Die3dKey::ToggleLabels, now);
                    let on = text(&v.render(now));
                    for (row, (a, b)) in off.iter().zip(&on).enumerate().take(r - FOOTER_ROWS) {
                        for (col, (x, y)) in a.chars().zip(b.chars()).enumerate() {
                            if x == y {
                                continue;
                            }
                            flagged += 1;
                            let blank = x == ' ' || x == '\u{2800}';
                            // The pin and the pole stand on the board;
                            // only they may cover a drawn cell.
                            assert!(
                                blank || y == '●' || y == '│',
                                "{c}x{r} braille={braille} cut {k}: {y:?} over {x:?} at {row},{col}"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(flagged > 0, "no flag was ever drawn");
}

#[test]
fn labels_stay_transparent_in_transparent_mode() {
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    // The history panel's rows name the chip too, in its own style.
    v.handle_key(Die3dKey::ToggleHistory, 0.0);
    let label_span = |lines: &[Line<'static>]| {
        lines[..40]
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains("BH0"))
            .cloned()
            .expect("a label span")
    };
    let s = label_span(&v.render(0.0));
    assert_eq!(s.style.bg, Some(Color::Reset), "{s:?}");
    assert_eq!(s.style.fg, Some(rgb(GRAYSKULL.white)), "{s:?}");
    assert!(s.style.add_modifier.contains(Modifier::BOLD), "{s:?}");
    // Opaque: the label stands on the black fill.
    v.handle_key(Die3dKey::ToggleTransparent, 0.0);
    let s = label_span(&v.render(0.0));
    assert_eq!(s.style.bg, Some(rgb(OPAQUE_BG)), "{s:?}");
}

#[test]
fn the_labels_command_toggles_labels_and_the_footer_says_which() {
    // Not a key: `L` and `l` are free for the app, and the setting is a command.
    assert_eq!(key_to_die3d('L'), None);
    assert_eq!(key_to_die3d('l'), None, "l is the TUI's legend key");
    assert!(Die3dKey::ToggleLabels.toggles());
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    // The history panel lists the chip names too: off, so only the
    // labels are found.
    v.command("history", "off", 0.0);
    let lines = v.render(0.0);
    assert!(footer(&lines).contains("/labels on"), "{}", footer(&lines));
    assert!(!labels_in(&lines, 40).is_empty());
    v.command("labels", "", 0.0);
    let lines = v.render(0.0);
    assert!(footer(&lines).contains("/labels off"), "{}", footer(&lines));
    assert!(labels_in(&lines, 40).is_empty());
    v.command("labels", "", 0.0);
    assert!(!labels_in(&v.render(0.0), 40).is_empty());
}

#[test]
fn a_hand_cut_while_a_scene_switch_is_pending_never_spins_or_jumps_mid_turn() {
    // The load rise in the super scene cuts and schedules a switch to
    // board 2 at that cut's midpoint. `c` before the midpoint starts a
    // new cut; the switch is the new cut's to cover.
    let (mut v, _) = switched_by_chip_2();
    let first = v.director.swapped_at().unwrap();
    assert_eq!(v.switcher.scene_at(first - 0.01), Scene::All);
    // In spin mode a camera-only hand cut would spin.
    v.director.set_transition_mode(TransitionMode::Spin);
    let at = 1.1;
    assert!(at < first);
    v.handle_key(Die3dKey::CutNow, at);
    assert_ne!(v.director.transition_kind(), Some(TransitionKind::Spin));
    // The scene changes once, at the new cut's swap, and the transition
    // knows both scenes for its whole length.
    let swap = v.director.swapped_at().unwrap();
    let secs = v.director.transition_kind().unwrap().secs();
    for i in 0..=40 {
        let now = at + secs * i as f64 / 40.0;
        let want = if now < swap {
            Scene::All
        } else {
            Scene::Board(1)
        };
        assert_eq!(v.switcher.scene_at(now), want, "+{}", now - at);
        if now < at + secs {
            assert_eq!(
                v.transition_scenes(now),
                (Scene::All, Scene::Board(1)),
                "+{}",
                now - at
            );
        }
    }
    // And in the default mode, over many hand cuts at that moment, never a spin.
    for seed in 0..40u64 {
        let mut b = two_boards();
        let mut v = Die3dView::new(150, 42);
        v.director = Director::new(seed);
        v.update(&b, 0.0);
        b.set_power(2, 90.0);
        v.update(&b, 1.0);
        // The load rise cut and scheduled the switch, still pending.
        assert_eq!(v.switcher.scene_at(1.1), Scene::All, "seed {seed}");
        assert_eq!(v.switcher.scene_at(5.0), Scene::Board(1), "seed {seed}");
        v.handle_key(Die3dKey::CutNow, 1.1);
        assert_ne!(
            v.director.transition_kind(),
            Some(TransitionKind::Spin),
            "seed {seed}"
        );
    }
}

#[test]
fn the_caption_outranks_the_not_drawn_note_during_a_cut() {
    // As in the mock: a Grayskull and a Wormhole the view does not draw,
    // and one Blackhole that it does.
    let b = Fake::boards(&["e150", "n150", "p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let note = v.undrawn_note();
    assert!(
        note.contains("Grayskull") && note.contains("Wormhole"),
        "{note}"
    );
    v.director.set_transition_mode(TransitionMode::Spin);
    v.handle_key(Die3dKey::CutNow, 10.0);
    let (from, to) = v.director.cameras();
    let caption = format!("view: {} -> {}", from.side_name(), to.side_name());
    let row = status_row(&v.render(10.3));
    assert!(row.chars().count() <= 150, "{row}");
    assert!(row.contains(&caption), "{row}");
    assert!(row.contains("view:") && row.contains("->"), "{row}");
    assert!(row.contains(ESTIMATE_LABEL), "{row}");
    // The note is the part that shortens: it starts after the caption
    // and is cut with an ellipsis.
    assert!(!row.contains(&note), "the whole note fits: {row}");
    assert!(row.ends_with('\u{2026}'), "{row}");
    let at_note = row.find("chip 0: Grayskull").expect("the note's start");
    assert!(row.find(&caption).unwrap() < at_note, "{row}");
    // At rest the shorter caption and the whole note both fit.
    let row = status_row(&v.render(20.0));
    assert!(row.contains("view: ") && row.contains(&note), "{row}");
}

#[test]
fn the_transition_command_cycles_the_mode_and_the_footer_says_which() {
    assert_eq!(key_to_die3d('X'), None);
    assert!(Die3dKey::CycleTransition.toggles());
    let b = two_boards();
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    for (want_state, want_mode) in [
        ("rotate", TransitionMode::Rotate),
        ("chaos", TransitionMode::Chaos),
        ("dissolve", TransitionMode::Dissolve),
        ("spin", TransitionMode::Spin),
        ("rotate", TransitionMode::Rotate),
    ] {
        assert_eq!(v.director.transition_mode(), want_mode);
        let f = footer(&v.render(0.0));
        assert!(f.contains(&format!("/transition {want_state}")), "{f}");
        v.command("transition", "", 0.0);
    }
}

/// A one-board (p150) view over `b`, auto-cuts off, so nothing but the
/// test moves the camera.
fn cloud_view(b: &Fake) -> Die3dView {
    let mut v = Die3dView::new(150, 42);
    v.update(b, 0.0);
    v.director.set_auto(false);
    v
}

#[test]
fn the_offset_integrates_speed_so_a_speed_change_never_jumps() {
    let mut b = Fake::boards(&["p150"], Some(TDP));
    let mut v = cloud_view(&b);
    let mut offsets = Vec::new();
    // 20 s, ticks 1/10 s apart, an update and a frame each tick. No PCIe
    // reading for the first 10 s (the 0.8 px/s floor), then 5 GB/s.
    for i in 0..=200 {
        let now = i as f64 * 0.1;
        if i == 100 {
            b.pcie = Some(PcieBandwidth {
                rx_bytes_per_sec: 4.0e9,
                tx_bytes_per_sec: 1.0e9,
            });
        }
        v.update(&b, now);
        v.render(now);
        offsets.push((now, v.cloud_clock.get().offset_px()));
    }
    // Continuous: no tick moves the clouds more than one clock step at
    // the fastest drift (8 px/s for 0.25 s) and a pixel of rounding, and
    // none moves them back.
    for w in offsets.windows(2) {
        let step = w[1].1 - w[0].1;
        assert!(
            (0.0..=3.0).contains(&step),
            "jump of {step} px at {} s",
            w[1].0
        );
    }
    // The floor for the first 10 s: 8 px, less a pixel of rounding.
    let at = |t: f64| offsets.iter().find(|o| (o.0 - t).abs() < 1e-9).unwrap().1;
    assert!((7.0..=8.0).contains(&at(10.0)), "{}", at(10.0));
    // Then the speed for 5 GB/s, about 7.7 px/s, from where it was.
    let fast = Clouds::drift_px_per_s(Some(5.0e9)) as f64;
    let gained = at(20.0) - at(10.0);
    assert!((gained - fast * 10.0).abs() <= 2.0, "{gained} px in 10 s");
}

#[test]
fn the_view_brightens_the_clouds_with_chip_load() {
    let mut b = Fake::boards(&["p300"; 2], Some(TDP));
    let mut v = cloud_view(&b);
    assert_eq!(v.cloud_clock.get().intensity(), 0.25);
    // The busiest chip sets the brightness: 90 W of 100 W is a load of
    // 0.9 while the load scale is still filling, so an intensity of 0.7.
    b.set_power(1, 90.0);
    v.update(&b, 0.5);
    // Taken up at the clock's next step, a quarter second on.
    v.render(0.6);
    assert_eq!(v.cloud_clock.get().intensity(), 0.25);
    v.render(0.75);
    let shown = v.cloud_clock.get().intensity();
    assert!((shown - 0.7).abs() < 1e-6, "{shown}");
}

#[test]
fn clouds_never_paint_over_the_die() {
    for transparent in [true, false] {
        let b = two_boards();
        let mut v = cloud_view(&b);
        if !transparent {
            v.handle_key(Die3dKey::ToggleTransparent, 0.0);
        }
        v.render(0.0);
        v.render(30.0);
        assert!(v.cloud_clock.get().offset_px() > 0.0);
        let (on, _) = v.scene_pixels(30.0).unwrap();
        v.handle_key(Die3dKey::ToggleClouds, 30.0);
        let (off, _) = v.scene_pixels(30.0).unwrap();
        let mut sky = 0;
        for i in 0..on.rgb.len() {
            if transparent && off.cover[i] || !transparent && off.rgb[i] != OPAQUE_BG {
                // Die, slab, packet: the same pixel with clouds on.
                assert_eq!((on.rgb[i], on.cover[i]), (off.rgb[i], true), "pixel {i}");
            } else if on.cover[i] && on.rgb[i] != off.rgb[i] {
                sky += 1;
            }
        }
        // The clouds were drawn somewhere, so the check above is not
        // vacuous.
        assert!(sky > 1000, "transparent={transparent}: {sky} cloud pixels");
    }
}

#[test]
fn the_clouds_command_toggles_clouds_and_the_footer_says_which() {
    assert_eq!(key_to_die3d('W'), None);
    assert_eq!(key_to_die3d('w'), None);
    assert!(Die3dKey::ToggleClouds.toggles());
    let b = two_boards();
    let mut v = cloud_view(&b);
    assert!(v.show_clouds, "on by default");
    let f = footer(&v.render(0.0));
    assert!(f.contains("/clouds on"), "{f}");
    let with = v.scene_pixels(0.0).unwrap().0;
    v.command("clouds", "", 0.1);
    let f = footer(&v.render(0.1));
    assert!(f.contains("/clouds off"), "{f}");
    let without = v.scene_pixels(0.1).unwrap().0;
    let covered = |b: &PixelBuf| b.cover.iter().filter(|c| **c).count();
    assert!(covered(&with) > covered(&without) + 1000);
    v.command("clouds", "", 0.2);
    assert!(footer(&v.render(0.2)).contains("/clouds on"));
}

#[test]
fn chip_labels_stay_readable_over_clouds() {
    for transparent in [true, false] {
        let b = two_boards();
        let mut v = cloud_view(&b);
        if !transparent {
            v.handle_key(Die3dKey::ToggleTransparent, 0.0);
        }
        let now = 12.0;
        let lines = v.render(now);
        let placed = v.frame(now).unwrap().2.unwrap();
        let flags = v.flag_layout(&placed, v.rows - FOOTER_ROWS);
        assert_eq!(flags.iter().flatten().count(), 4);
        let want = if transparent {
            Color::Reset
        } else {
            rgb(OPAQUE_BG)
        };
        for flag in flags.iter().flatten() {
            let text_row = flag.pin.1 - flag.rise;
            for ((col, row), ch) in flag.cells() {
                let cells: Vec<(char, Style)> = lines[row as usize]
                    .spans
                    .iter()
                    .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                    .collect();
                let (got, style) = cells[col as usize];
                assert_eq!(got, ch, "{}", flag.text);
                assert_eq!(style.bg, Some(want), "{}", flag.text);
                if row == text_row && flag.text.contains(ch) && ch != ' ' {
                    assert_eq!(style.fg, Some(rgb(GRAYSKULL.white)), "{}", flag.text);
                }
            }
        }
    }
}

#[test]
fn a_cloud_step_redraws_at_once_and_a_still_sky_does_not() {
    // No PCIe reading: the sky moves at the floor, a pixel per 1.25 s,
    // and no packet moves. Frames 1/120 s apart for 5 s.
    let b = Fake::boards(&["p150"], Some(TDP));
    let v = cloud_view(&b);
    let mut prev = (v.cloud_clock.get().offset_px(), text(&v.render(1.0)));
    let mut steps = 0;
    for i in 1..600 {
        let now = 1.0 + i as f64 / 120.0;
        let lines = text(&v.render(now));
        let offset = v.cloud_clock.get().offset_px();
        if offset != prev.0 {
            // A step is shown in the very frame it happens, even when
            // the last frame is less than 1/20 s old.
            assert_ne!(lines, prev.1, "the step at {now} s was not drawn");
            steps += 1;
        }
        prev = (offset, lines);
    }
    assert!((3..=5).contains(&steps), "{steps} steps in 5 s at 0.8 px/s");
    // Between steps the throttle is as it was: 20 frames drawn a second
    // (counted by the time of the last frame drawn, since a frame drawn
    // can equal the one before it). One step falls in this second, on a
    // frame that was due anyway.
    let drawn_at = |v: &Die3dView| v.cache.borrow().last.as_ref().unwrap().at;
    let mut drawn = 0;
    let mut last = drawn_at(&v);
    for i in 0..120 {
        v.render(10.0 + i as f64 / 120.0);
        drawn += (drawn_at(&v) != last) as usize;
        last = drawn_at(&v);
    }
    assert!(
        (19..=21).contains(&drawn),
        "{drawn} frames drawn in a second"
    );
}

/// The text of the history panel's rows in `lines`: `n` rows from
/// [`PANEL_TOP`], each the [`history::PANEL_WIDTH`] cells from column
/// [`PANEL_LEFT`], trailing spaces cut.
fn panel_text(lines: &[Line<'static>], n: usize) -> Vec<String> {
    text(lines)
        .iter()
        .skip(PANEL_TOP)
        .take(n)
        .map(|row| {
            let cells: String = row
                .chars()
                .skip(PANEL_LEFT)
                .take(history::PANEL_WIDTH)
                .collect();
            cells.trim_end().to_string()
        })
        .collect()
}

/// Whether the panel's title is anywhere in `lines`.
fn has_panel(lines: &[Line<'static>]) -> bool {
    text(lines).iter().any(|row| row.contains(history::TITLE))
}

#[test]
fn the_panel_lists_each_blackhole_chip_with_its_latest_values() {
    // A p150 (drawn) and an n150 (not drawn). The PCIe rates are per
    // device in the fake; the box's total counts the drawn chip only.
    let mut b = Fake::boards(&["p150", "n150"], Some(TDP));
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 1.23e9,
        tx_bytes_per_sec: 3.1e8,
    });
    let mut v = Die3dView::new(150, 42);
    // Five samples, one a second: power 40..50 W, 52..56 C. The ramp
    // spans 10 W, more than the power sparkline's 5 W minimum span, so
    // it fills the sparkline's full height.
    for t in 0..5 {
        b.set_power(0, 40.0 + 2.5 * t as f32);
        b.telem[0].asic_temperature = Some(52.0 + t as f32);
        v.update(&b, t as f64);
    }
    let rows = panel_text(&v.render(4.0), 5);
    assert_eq!(rows[0], history::TITLE, "{rows:#?}");
    // The chip: its name, the rising power as a sparkline whose newest
    // sample is the highest block, then the latest watts and degrees.
    assert!(rows[1].starts_with("BH0"), "{rows:#?}");
    assert!(
        rows[1].contains("\u{2581}\u{2583}\u{2585}\u{2586}\u{2588}"),
        "{rows:#?}"
    );
    assert!(rows[1].ends_with("50W  56\u{b0}"), "{rows:#?}");
    // The box's PCIe, in then out.
    assert!(rows[2].starts_with("PCIe \u{25bc}"), "{rows:#?}");
    assert!(rows[2].ends_with("1.23 GB/s"), "{rows:#?}");
    assert!(rows[3].trim_start().starts_with('\u{25b2}'), "{rows:#?}");
    assert!(rows[3].ends_with("310 MB/s"), "{rows:#?}");
    // Nothing for the chip the view does not draw, and nothing more.
    assert!(rows.iter().all(|r| !r.contains("WH")), "{rows:#?}");
    assert_eq!(rows[4], "", "{rows:#?}");
    // At most six chips: eight p300 chips list BH0 to BH5.
    let b = Fake::boards(&["p300"; 8], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let rows = v.history.panel_rows();
    assert_eq!(rows.len(), 1 + 6 + 2, "{rows:#?}");
    for (i, row) in rows[1..7].iter().enumerate() {
        assert!(row.starts_with(&format!("BH{i} ")), "{rows:#?}");
    }
}

#[test]
fn the_panel_is_hidden_below_120_columns_or_36_rows_and_when_off() {
    let b = Fake::boards(&["p150"], Some(TDP));
    for (c, r, shown) in [
        (100, 30, false),
        (119, 42, false),
        (150, 35, false),
        (120, 36, true),
        (150, 42, true),
    ] {
        let mut v = Die3dView::new(c, r);
        v.update(&b, 0.0);
        assert_eq!(has_panel(&v.render(0.0)), shown, "{c} x {r}");
        if shown {
            v.handle_key(Die3dKey::ToggleHistory, 0.0);
            assert!(!has_panel(&v.render(0.0)), "{c} x {r} with H off");
        }
    }
}

#[test]
fn the_panel_never_touches_the_footer_rows_or_leaves_the_scene_area() {
    // Through the view: two views alike but for `H`, rendered at the same
    // times, at rest and through each kind of cut. Every cell that
    // differs is in the panel's rectangle on a scene row, or in the
    // footer's switch row, which names the H state.
    let mut one = Fake::boards(&["p300"; 2], Some(TDP));
    let mut four = Fake::boards(&["p300"; 8], Some(TDP));
    for b in [&mut one, &mut four] {
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 2.0e7,
            tx_bytes_per_sec: 1.3e7,
        });
    }
    let cells = |lines: &[Line<'static>]| -> Vec<Vec<(char, Style)>> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                    .collect()
            })
            .collect()
    };
    for (c, r) in [(120, 36), (121, 37), (150, 42), (220, 60)] {
        for b in [&one, &four] {
            for braille in [true, false] {
                let mut views: Vec<Die3dView> = [true, false]
                    .into_iter()
                    .map(|on| {
                        let mut v = Die3dView::new(c, r);
                        v.update(b, 0.0);
                        v.update(b, 1.0);
                        v.director.set_auto(false);
                        if !braille {
                            v.handle_key(Die3dKey::ToggleBraille, 1.0);
                        }
                        if !on {
                            v.handle_key(Die3dKey::ToggleHistory, 1.0);
                        }
                        v
                    })
                    .collect();
                let panel = views[0].history.panel_rows();
                let width = panel.iter().map(|p| p.chars().count()).max().unwrap();
                let scene_rows = r - FOOTER_ROWS;
                let mut in_panel = 0;
                for (k, mode) in [
                    TransitionMode::Chaos,
                    TransitionMode::Dissolve,
                    TransitionMode::Spin,
                ]
                .into_iter()
                .enumerate()
                {
                    // At rest half a second before the cut, then 0.3 s
                    // into it.
                    let cut = 2.0 + 3.0 * k as f64;
                    let mut times = vec![cut - 0.5];
                    times.push(cut + 0.3);
                    for (i, now) in times.into_iter().enumerate() {
                        if i == 1 {
                            for v in views.iter_mut() {
                                v.director.set_transition_mode(mode);
                                v.handle_key(Die3dKey::CutNow, cut);
                            }
                            assert!(views[0].director.progress(now).is_some());
                        }
                        let frames: Vec<_> = views.iter().map(|v| v.render(now)).collect();
                        let (on, off) = (cells(&frames[0]), cells(&frames[1]));
                        assert_eq!(on.len(), r);
                        for (row, (a, z)) in on.iter().zip(&off).enumerate() {
                            // Scene rows are the view's width; the
                            // footer rows are their text, never wider.
                            for n in [a.len(), z.len()] {
                                let ok = if row < scene_rows { n == c } else { n <= c };
                                assert!(ok, "{c} x {r} row {row} at {now}: {n}");
                            }
                            for (col, (x, y)) in a.iter().zip(z).enumerate() {
                                if x == y {
                                    continue;
                                }
                                let inside = (PANEL_TOP..PANEL_TOP + panel.len()).contains(&row)
                                    && (PANEL_LEFT..PANEL_LEFT + width).contains(&col)
                                    && row < scene_rows;
                                assert!(
                                    inside || row == r - 1,
                                    "{c} x {r} at {now}: cell {row},{col} changed by the panel"
                                );
                                in_panel += inside as usize;
                            }
                        }
                        // The footer's status row is the same with or
                        // without the panel; its switch row starts with
                        // the bar either way.
                        let (t_on, t_off) = (text(&frames[0]), text(&frames[1]));
                        assert_eq!(t_on[scene_rows], t_off[scene_rows]);
                        assert!(t_on[r - 1].starts_with("\u{255a}\u{2550} "));
                    }
                }
                assert!(in_panel > 0, "{c} x {r}: the panel drew nothing");
            }
        }
    }

    // At the layer that writes: a panel taller than the scene rows and
    // wider than the view.
    let width = 20;
    let scene_rows = 3;
    let mut lines: Vec<Line<'static>> = (0..scene_rows + FOOTER_ROWS)
        .map(|i| Line::from(Span::raw(format!("{i}").repeat(width))))
        .collect();
    let before = text(&lines);
    let rows: Vec<String> = (0..9)
        .map(|i| format!("panel row {i} \u{2581}\u{2588}").repeat(3))
        .collect();
    draw_panel(&mut lines, scene_rows, width, &rows, Style::default());
    let after = text(&lines);
    assert_eq!(after[scene_rows..], before[scene_rows..], "footer rows");
    assert_eq!(after[0], before[0], "the row above the panel");
    for row in &after {
        assert_eq!(row.chars().count(), width, "{row}");
    }
    // Clipped at the right edge, after the left margin.
    let want: String = rows[0].chars().take(width - PANEL_LEFT).collect();
    assert_eq!(after[1], format!("11{want}"));
}

#[test]
fn flags_win_over_the_panel_where_they_overlap() {
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    let scene_rows = 40;
    let mut lines: Vec<Line<'static>> = (0..42)
        .map(|_| Line::from(Span::raw(" ".repeat(150))))
        .collect();
    // A flag whose text row is the panel's title row, its pole four cells
    // in: the corner is at +4, one space, then the text from +6.
    let label = "BH9 99W";
    let flag = Flag {
        pin: (PANEL_LEFT as i32 + 4, PANEL_TOP as i32 + 3),
        rise: 3,
        left: false,
        text: label.to_string(),
    };
    v.overlay(&mut lines, scene_rows, &[Some(flag)]);
    let row: Vec<(char, Style)> = lines[PANEL_TOP]
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    let shown: String = row[PANEL_LEFT..PANEL_LEFT + 14]
        .iter()
        .map(|c| c.0)
        .collect();
    assert!(shown.starts_with("hist\u{250c} BH9 99W"), "{shown}");
    for (i, (_, style)) in row[PANEL_LEFT + 6..PANEL_LEFT + 13].iter().enumerate() {
        assert_eq!(style.fg, Some(rgb(GRAYSKULL.white)), "text cell {i}");
    }
    assert_eq!(row[PANEL_LEFT + 4].1.fg, Some(rgb(GRAYSKULL.eth)), "corner");
    for at in [PANEL_LEFT, PANEL_LEFT + 1, PANEL_LEFT + 2] {
        assert_eq!(row[at].1.fg, Some(rgb(PANEL_FG)), "panel cell {at}");
    }
}

#[test]
fn the_panel_background_is_reset_when_transparent_and_black_when_opaque() {
    let mut b = Fake::boards(&["p150"], Some(TDP));
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 2.3e7,
        tx_bytes_per_sec: 1.3e7,
    });
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.update(&b, 1.0);
    let rows = v.history.panel_rows();
    for (transparent, bg) in [(true, Color::Reset), (false, rgb(OPAQUE_BG))] {
        if !transparent {
            v.handle_key(Die3dKey::ToggleTransparent, 1.0);
        }
        let lines = v.render(1.0);
        for (i, want) in rows.iter().enumerate() {
            let row: Vec<(char, Style)> = lines[PANEL_TOP + i]
                .spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                .collect();
            for (j, ch) in want.chars().enumerate() {
                let (got, style) = row[PANEL_LEFT + j];
                assert_eq!(got, ch, "row {i} col {j}");
                assert_eq!(style.fg, Some(rgb(PANEL_FG)), "row {i} col {j}");
                assert_eq!(style.bg, Some(bg), "row {i} col {j}");
                assert!(
                    !style.add_modifier.contains(Modifier::BOLD),
                    "row {i} col {j}"
                );
            }
        }
    }
}

#[test]
fn the_history_command_toggles_history_and_the_footer_says_which() {
    assert_eq!(key_to_die3d('H'), None);
    assert_eq!(key_to_die3d('h'), None, "h is the TUI's");
    assert!(Die3dKey::ToggleHistory.toggles());
    let b = Fake::boards(&["p150"], Some(TDP));
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    assert!(v.show_history, "on by default");
    let lines = v.render(0.0);
    assert!(footer(&lines).contains("/history on"), "{}", footer(&lines));
    assert!(has_panel(&lines));
    v.command("history", "", 0.0);
    let lines = v.render(0.0);
    assert!(
        footer(&lines).contains("/history off"),
        "{}",
        footer(&lines)
    );
    assert!(!has_panel(&lines));
    v.command("history", "", 0.0);
    assert!(has_panel(&v.render(0.0)));
}

#[test]
fn a_new_sample_is_drawn_at_once_and_a_hidden_panel_forces_no_frame() {
    let rate = |rx: f64| {
        Some(PcieBandwidth {
            rx_bytes_per_sec: rx,
            tx_bytes_per_sec: 1.0e7,
        })
    };
    let mut b = Fake::boards(&["p150"], Some(TDP));
    b.pcie = rate(1.0e8);
    // Clouds off: their steps also force frames, and would blur this.
    let mut v = cloud_view(&b);
    v.handle_key(Die3dKey::ToggleClouds, 0.0);
    v.update(&b, 1.0);
    let before = v.render(1.98);
    assert!(panel_text(&before, 3)[2].ends_with("100 MB/s"));
    // A new sample 0.02 s after the last frame, well inside the 1/20 s
    // the throttle would otherwise hold that frame for.
    b.pcie = rate(1.2e8);
    v.update(&b, 2.0);
    let after = v.render(2.0);
    let rows = panel_text(&after, 3);
    assert!(rows[2].ends_with("120 MB/s"), "{rows:#?}");
    // An unchanged panel forces nothing: the same frame inside 1/20 s.
    assert_eq!(text(&v.render(2.03)), text(&after));
    // With the panel off a sample is taken but forces no frame.
    v.handle_key(Die3dKey::ToggleHistory, 2.5);
    let hidden = text(&v.render(2.97));
    let revision = v.history.revision();
    b.pcie = rate(1.4e8);
    v.update(&b, 3.0);
    assert_eq!(v.history.revision(), revision + 1, "the sample was taken");
    assert_eq!(text(&v.render(3.0)), hidden);
}

/// An inset over `b` that has read it once a second from 0 to `secs`.
fn inset_over(b: &Fake, cols: usize, rows: usize, secs: u32) -> Die3dView {
    let mut v = Die3dView::inset(cols, rows);
    for s in 0..=secs {
        v.update(b, s as f64);
    }
    v
}

/// True when an inset of `cols` x `rows` (one row of it the caption) has
/// room for the super scene of boards with `dies` dies each. This is the
/// sizing the view itself does: braille cells of 2 x 4 pixels and no
/// minimum size. (`fit_super` adds the full layout's 100 x 30 minimum,
/// which no inset reaches.)
fn inset_fits_all(cols: usize, rows: usize, dies: &[usize]) -> bool {
    let grid = super_grid(dies).0;
    rows >= 1 && fit_pixels((cols * 2) as i32, ((rows - 1) * 4) as i32, grid).is_some()
}

/// The smallest inset, from 42 x 14 up, with the same 3 : 1 shape as the
/// Insights inset, where the super scene of `dies` fits.
fn smallest_inset_for_all(dies: &[usize]) -> (usize, usize) {
    (14..=60)
        .map(|rows| (rows * 3, rows))
        .find(|&(c, r)| inset_fits_all(c, r, dies))
        .expect("some inset up to 180 x 60 holds the boards")
}

/// The text of `buf` as braille lines, as the view draws it.
fn braille_text(buf: &PixelBuf) -> Vec<String> {
    text(&cells::to_braille_lines(buf))
}

#[test]
fn the_inset_draws_all_boards_when_they_fit() {
    let b = two_boards();
    let dies = [2, 2];
    let (cols, rows) = smallest_inset_for_all(&dies);
    let v = inset_over(&b, cols, rows, 3);
    let now = 3.0;
    // The super scene drawn directly at the inset's scene area: no
    // clouds, transparent, the director's camera.
    let grid = super_grid(&dies).0;
    let (pw, ph) = (cols * 2, (rows - 1) * 4);
    let fit = fit_pixels(pw as i32, ph as i32, grid).unwrap();
    let fit = aim(&fit, Aim::At(v.director.camera_at(now)), grid);
    let mut want = PixelBuf::new(pw, ph);
    want.clear_transparent();
    draw_super_scene(&mut want, &v.chips, &fit, now as f32);
    let (got, shown) = v.scene_pixels(now).expect("the super scene fits");
    assert_eq!(shown, Shown::All, "{cols} x {rows}");
    assert_eq!(got.rgb, want.rgb);
    assert_eq!(got.cover, want.cover);
    // The frame is that scene, row for row, then the caption.
    let t = text(&v.render(now));
    assert_eq!(t.len(), rows);
    assert_eq!(t[..rows - 1], braille_text(&want)[..]);
}

/// The Insights inset is 52 x 16. A two-die board (a p300; the fake's
/// "p300" is one die, so two of them) needs at least 51 x 16 for its
/// scene, so that inset draws the scene and not the text summary.
#[test]
fn a_two_die_board_draws_a_scene_in_the_inset() {
    let b = Fake::boards(&["p300"; 2], Some(TDP));
    assert!(inset_fits_all(52, 16, &[2]), "a 52 x 16 inset holds it");
    let v = inset_over(&b, 52, 16, 3);
    let (_, shown) = v.scene_pixels(3.0).expect("the scene fits");
    // One board: its own scene (no fallback from a bigger one).
    assert_eq!(shown, Shown::Board(0));
    let t = text(&v.render(3.0));
    assert_eq!(t.len(), 16);
    let braille = t[..15]
        .iter()
        .flat_map(|r| r.chars())
        .filter(|&c| ('\u{2801}'..='\u{28FF}').contains(&c))
        .count();
    assert!(braille > 0, "the scene draws braille glyphs");
    // The summary draws a line per chip ("BH0  in .. out .."). Above the
    // caption row, the scene rows hold no such text.
    let scene = t[..15].concat();
    assert!(!scene.contains("in ") && !scene.contains("out"), "{t:#?}");
    // The last row is the caption, one line for the board.
    assert!(t[15].starts_with("BH0+BH1"), "{t:#?}");
}

#[test]
fn the_inset_shows_the_busiest_board_when_all_do_not_fit() {
    let mut b = Fake::boards(&["p150"; 8], Some(TDP));
    // A powered chip idles near 37 W; a reading under a watt is a glitch
    // the load scale ignores.
    for i in 0..8 {
        b.set_power(i, IDLE_W);
    }
    assert!(!inset_fits_all(42, 14, &[1; 8]), "8 boards fit at 42 x 14");
    let mut v = Die3dView::inset(42, 14);
    // Five idle seconds calibrate every chip's idle at 37 W.
    for s in 0..5 {
        v.update(&b, s as f64);
    }
    // Chip 0 (board 0) is busy first, so board 0 is the first strictly
    // busiest board and is shown at once.
    b.set_power(0, 90.0);
    for s in 5..=6 {
        v.update(&b, s as f64);
    }
    assert_eq!(v.scene_pixels(6.0).unwrap().1, Shown::Fallback(0));
    // Chip 3 (board 3) is busy from 7 s on.
    b.set_power(0, IDLE_W);
    b.set_power(3, 90.0);
    for s in 7..=16 {
        v.update(&b, s as f64);
    }
    // Busiest for 9 s: the board shown is still board 0.
    assert_eq!(v.scene_pixels(16.0).unwrap().1, Shown::Fallback(0));
    for s in 17..=18 {
        v.update(&b, s as f64);
    }
    let now = 18.0;
    let (got, shown) = v.scene_pixels(now).unwrap();
    assert_eq!(shown, Shown::Fallback(3));
    let camera = Aim::At(v.director.camera_at(now));
    let (want, _, _) = v.scene_with(now, Scene::Board(3), camera).unwrap();
    assert_eq!(got.rgb, want.rgb);
    let t = text(&v.render(now));
    assert_eq!(t[..13], braille_text(&want)[..]);
}

#[test]
fn the_inset_never_changes_scene_on_a_trigger() {
    // Two boards in an inset with room for both: a load rise on board 1
    // cuts the camera, and every board stays on show.
    let mut b = two_boards();
    let (cols, rows) = smallest_inset_for_all(&[2, 2]);
    let mut v = inset_over(&b, cols, rows, 0);
    b.set_power(2, 90.0);
    v.update(&b, 1.0);
    assert_eq!(v.director.cut_at(), Some(1.0), "the rise cut the camera");
    assert_eq!(v.switcher.scene_at(10.0), Scene::All);
    for now in [1.0, 1.5, 2.0, 5.0] {
        assert_eq!(v.scene_now(now).unwrap().1, Shown::All, "at {now}");
    }
    // The inset reads its scene through `scene_at`, never from the
    // switcher. Nothing in an inset moves the switcher; moved here by
    // hand, it still leaves every board on show, still, mid-spin and
    // mid-dissolve.
    v.switcher.choose(Scene::Board(1));
    assert_eq!(v.scene_now(6.0).unwrap().1, Shown::All);
    for (mode, at) in [
        (TransitionMode::Spin, 10.0),
        (TransitionMode::Dissolve, 20.0),
    ] {
        v.director.set_transition_mode(mode);
        v.director.cut_by_hand(at);
        assert!(v.director.progress(at + 0.2).is_some());
        let (_, shown, _) = v.frame(at + 0.2).unwrap();
        assert_eq!(shown, Shown::All, "{mode:?}");
    }
    // Eight boards that do not fit: board 3's chip rises, so board 3 is
    // the first strictly busiest board and is shown at once.
    let mut b = Fake::boards(&["p150"; 8], Some(TDP));
    let mut v = inset_over(&b, 42, 14, 0);
    b.set_power(3, 90.0);
    v.update(&b, 1.0);
    assert_eq!(v.director.cut_at(), Some(1.0), "the rise cut the camera");
    assert_eq!(v.switcher.scene_at(10.0), Scene::All);
    assert_eq!(v.scene_now(5.0).unwrap().1, Shown::Fallback(3));
}

#[test]
fn a_hidden_inset_does_not_promote_a_stale_board_when_it_shows_again() {
    let mut b = Fake::boards(&["p150"; 8], Some(TDP));
    for i in 0..8 {
        b.set_power(i, IDLE_W);
    }
    let mut v = inset_over(&b, 42, 14, 4);
    b.set_power(0, 90.0);
    v.update(&b, 5.0);
    assert_eq!(v.scene_now(5.0).unwrap().1, Shown::Fallback(0));
    // Board 3 leads for 9 s while the inset is shown, then it is hidden.
    b.set_power(0, IDLE_W);
    b.set_power(3, 90.0);
    for s in 6..=14 {
        v.update(&b, s as f64);
    }
    v.clear_pending_board();
    // The first reading after it shows again starts the hold afresh.
    v.update(&b, 40.0);
    assert_eq!(v.scene_now(40.0).unwrap().1, Shown::Fallback(0));
    v.update(&b, 50.0);
    assert_eq!(v.scene_now(50.0).unwrap().1, Shown::Fallback(3));
}

#[test]
fn an_inset_showing_one_board_cuts_only_for_triggers_on_that_board() {
    // Eight boards that do not fit: chip 0 (board 0) rises first, so
    // board 0 is the busiest board shown.
    let mut b = Fake::boards(&["p150"; 8], Some(TDP));
    let mut v = inset_over(&b, 42, 14, 0);
    b.set_power(0, 90.0);
    v.update(&b, 1.0);
    assert_eq!(v.scene_now(1.0).unwrap().1, Shown::Fallback(0));
    assert_eq!(v.director.cut_at(), Some(1.0), "board 0 is on show");
    // Chip 3 rises on board 3, which is not shown, after the cut's
    // cooldown (`COOLDOWN_SECS`): no new cut.
    b.set_power(3, 90.0);
    v.update(&b, 10.0);
    assert_eq!(v.scene_now(10.0).unwrap().1, Shown::Fallback(0));
    assert_eq!(v.director.cut_at(), Some(1.0), "board 3 is not on show");
}

#[test]
fn the_inset_has_a_caption_row_and_no_footer() {
    let mut b = Fake::boards(&["p150"], Some(TDP));
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 2.0e9,
        tx_bytes_per_sec: 1.0e6,
    });
    let v = inset_over(&b, 42, 14, 1);
    let t = text(&v.render(1.0));
    assert_eq!(t.len(), 14, "{t:?}");
    let last = &t[13];
    assert!(last.contains("BH0"), "{last}");
    assert!(last.contains("in 2.0G  out 1.0M"), "{last}");
    assert!(last.contains("3 = full view"), "{last}");
    assert!(t.iter().all(|l| !l.contains("board 1/")), "{t:?}");
    assert!(t.iter().all(|l| l.chars().count() <= 42), "{t:?}");
    // A scene is drawn above it, with no flag.
    assert!(t[..13].iter().any(|l| has_braille_dots(l)), "{t:?}");
    assert!(t.iter().all(|l| !l.contains('●')), "{t:?}");
    // One row: the caption alone. No rows: nothing.
    let t = text(&inset_over(&b, 42, 1, 1).render(1.0));
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0].contains("BH0"), "{t:?}");
    assert!(inset_over(&b, 42, 0, 1).render(1.0).is_empty());
}
