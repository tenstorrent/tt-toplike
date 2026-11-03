// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the arcade layout engine.

use super::*;

fn row(height: u32, panels: &[(View, u32)]) -> RowSpec {
    RowSpec {
        height,
        panels: panels
            .iter()
            .map(|&(view, width)| PanelSpec { view, width })
            .collect(),
    }
}

/// The old fixed layout's arithmetic, copied from `ArcadeVisualization::new`
/// at f9ebf418: (starfield, castle, flow) rows and (castle, defrag) columns.
fn old_rows(height: usize) -> (usize, usize, usize) {
    let content_height = height.saturating_sub(6);
    let starfield_height = (content_height as f32 * 0.40) as usize;
    let bottom_height = content_height.saturating_sub(starfield_height);
    let castle_height = bottom_height / 2;
    (
        starfield_height,
        castle_height,
        bottom_height - castle_height,
    )
}

fn old_cols(width: usize) -> (usize, usize) {
    let castle_col_w = (width as f32 * 0.55) as usize;
    (
        castle_col_w,
        width.saturating_sub(castle_col_w).saturating_sub(1),
    )
}

#[test]
fn the_default_rows_give_the_old_heights_at_every_size() {
    for h in 0..2000 {
        let l = layout(&default_rows(), 200, h);
        let got: Vec<usize> = l.rows.iter().map(|r| r.height).collect();
        let (s, c, f) = old_rows(h);
        assert_eq!(got, vec![s, c, f], "height {h}");
    }
}

#[test]
fn the_default_columns_give_the_old_widths_at_every_size() {
    for w in 1..2000 {
        let l = layout(&default_rows(), w, 60);
        let mid = &l.rows[1].panels;
        let (c, d) = old_cols(w);
        assert_eq!(mid[0].rect.w, c, "castle at width {w}");
        assert_eq!(mid[1].rect.w, d, "defrag at width {w}");
        assert_eq!(mid[1].rect.x, c + 1, "the divider sits right of the castle");
        assert_eq!(l.rows[0].panels[0].rect.w, w, "starfield is full width");
    }
}

#[test]
fn the_default_layout_hides_nothing_at_any_size() {
    // The old layout drew all three rules with their plain labels at every
    // size, even when a row got no content rows or a panel no columns.
    let want = [
        "✧ STARFIELD",
        "🏰 MEMORY CASTLE  │  ▓ DEFRAG",
        "🌊 MEMORY FLOW",
    ];
    for w in 0..=240 {
        for h in 0..=20 {
            let l = layout(&default_rows(), w, h);
            let labels: Vec<&str> = l.rows.iter().map(|r| r.label()).collect();
            assert_eq!(labels, want, "{w}x{h}");
            assert!(l.rows.iter().all(|r| r.hidden.is_empty()), "{w}x{h}");
        }
    }
}

#[test]
fn a_view_with_a_real_minimum_is_hidden_from_an_empty_row() {
    // Only the four original views draw in an empty panel.
    assert!(View::Castle.fits(0, 0) && View::Flow.fits(5, 0) && View::Defrag.fits(0, 5));
    assert!(!View::Die3d.fits(40, 0) && !View::Training.fits(0, 20));
    assert!(!View::Inference.fits(23, 8) && View::Inference.fits(24, 8));
}

#[test]
fn the_default_rule_rows_are_where_the_old_layout_put_them() {
    let l = layout(&default_rows(), 120, 40);
    let (s, c, _) = old_rows(40);
    let rules: Vec<usize> = l.rows.iter().map(|r| r.rule_y).collect();
    assert_eq!(rules, vec![2, 3 + s, 4 + s + c]);
    assert_eq!(l.rows[1].label(), "🏰 MEMORY CASTLE  │  ▓ DEFRAG");
    assert_eq!(l.lines(), 40 - FOOTER_ROWS);
}

#[test]
fn cuts_use_every_unit_once_and_follow_the_weights() {
    for total in 0..300 {
        for weights in [&[1u32][..], &[1, 1], &[3, 1], &[2, 3, 2], &[1, 7, 1, 1]] {
            let ends = cuts(total, weights);
            assert_eq!(*ends.last().unwrap(), total, "{total} {weights:?}");
            assert!(ends.windows(2).all(|w| w[0] <= w[1]));
        }
    }
    assert_eq!(cuts(10, &[1, 1]), vec![5, 10]);
    assert_eq!(cuts(10, &[3, 1, 1]), vec![6, 8, 10]);
    assert_eq!(
        cuts(7, &[1, 1]),
        vec![3, 7],
        "the remainder goes to the end"
    );
}

#[test]
fn columns_have_no_gap_and_no_overlap_and_one_divider_between_panels() {
    for width in 10..400 {
        for weights in [&[1u32][..], &[1, 1], &[3, 1], &[2, 1, 2], &[1, 1, 1, 1]] {
            let spans = split_cols(width, weights);
            let mut col = 0;
            for (i, &(x, w)) in spans.iter().enumerate() {
                let want = if i == 0 { col } else { col + 1 };
                assert_eq!(x, want, "panel {i} at width {width} {weights:?}");
                col = x + w;
            }
            assert_eq!(col, width, "every column used, width {width} {weights:?}");
        }
    }
}

#[test]
fn defrag_at_width_three_is_three_times_the_castle() {
    let rows = vec![row(1, &[(View::Defrag, 3), (View::Castle, 1)])];
    for width in [40, 80, 121, 160, 203, 240] {
        let l = layout(&rows, width, 30);
        let p = &l.rows[0].panels;
        let (d, c) = (p[0].rect.w as i64, p[1].rect.w as i64);
        // The divider comes out of the castle's share, so the castle may be
        // one column short of a quarter, and rounding may cost one more.
        assert!(
            (d - 3 * c).abs() <= 4,
            "width {width}: defrag {d}, castle {c}"
        );
        assert_eq!((d + c + 1) as usize, width);
    }
}

#[test]
fn a_panel_under_its_minimum_is_named_on_the_rule_and_the_others_take_its_columns() {
    // Die 3D needs 16 columns; a quarter of 40 is 10.
    let rows = vec![row(1, &[(View::Castle, 3), (View::Die3d, 1)])];
    let l = layout(&rows, 40, 30);
    let r = &l.rows[0];
    assert_eq!(r.panels.len(), 1);
    assert_eq!(r.panels[0].view, View::Castle);
    assert_eq!(r.panels[0].rect.w, 40, "the castle takes the whole row");
    assert_eq!(r.hidden, vec![(View::Die3d, (16, 6))]);
    let note = format!(
        "◌ {} needs 16x6",
        crate::animation::DIE3D_DISPLAY_NAME.to_uppercase()
    );
    assert!(r.label().contains(&note), "{}", r.label());
    assert!(r.label().starts_with("🏰 MEMORY CASTLE"), "{}", r.label());
}

#[test]
fn a_panel_too_short_is_hidden_too() {
    // Training needs 10 rows. Two rows of weight 1 at height 15 give it 5.
    let rows = vec![
        row(1, &[(View::Starfield, 1)]),
        row(1, &[(View::Flow, 1), (View::Training, 1)]),
    ];
    let l = layout(&rows, 100, 15);
    assert_eq!(l.rows[1].panels.len(), 1);
    assert_eq!(l.rows[1].hidden[0].0, View::Training);
    assert_eq!(l.rows[1].panels[0].rect.w, 100);
}

#[test]
fn a_row_with_nothing_left_gives_its_rows_to_the_others() {
    let rows = vec![row(1, &[(View::Starfield, 1)]), row(1, &[(View::Die3d, 1)])];
    // 10 columns: Die 3D never fits, so its row has no content rows and the
    // starfield gets all of them. The rule stays, with the note.
    let l = layout(&rows, 10, 30);
    assert_eq!(l.rows[1].height, 0);
    assert!(l.rows[1].panels.is_empty());
    assert_eq!(l.rows[0].height, 30 - HEADER_ROWS - 2 - FOOTER_ROWS);
    assert_eq!(l.rows[1].rule_y, l.rows[0].rule_y + 1 + l.rows[0].height);
    assert!(l.rows[1]
        .label()
        .starts_with(&hidden_note(View::Die3d, 16, 6)));
    assert!(l.rows[1].label().contains(" needs "));
    assert_eq!(l.lines(), 30 - FOOTER_ROWS);
}

#[test]
fn removing_one_panel_can_make_room_for_another() {
    // At 50 columns three equal panels get 16 each (Die 3D fits at 16) but
    // the inference panel (24) does not; with it gone, the two left get 24
    // and 25 and both fit, so only the inference panel is hidden.
    let rows = vec![row(
        1,
        &[(View::Die3d, 1), (View::Castle, 1), (View::Inference, 1)],
    )];
    let l = layout(&rows, 50, 30);
    let shown: Vec<View> = l.rows[0].panels.iter().map(|p| p.view).collect();
    assert_eq!(shown, vec![View::Die3d, View::Castle]);
    assert_eq!(l.rows[0].hidden.len(), 1);
}

#[test]
fn every_layout_uses_each_row_once_at_many_sizes() {
    let rows = vec![
        row(2, &[(View::Starfield, 1)]),
        row(3, &[(View::Defrag, 3), (View::Castle, 1), (View::Die3d, 2)]),
        row(
            2,
            &[(View::Flow, 1), (View::Training, 1), (View::Inference, 1)],
        ),
    ];
    for w in [0, 5, 20, 60, 120, 200, 333] {
        for h in [0, 3, 6, 12, 24, 40, 60, 99] {
            let l = layout(&rows, w, h);
            let mut y = HEADER_ROWS;
            for r in &l.rows {
                assert_eq!(r.rule_y, y, "{w}x{h}");
                for p in &r.panels {
                    assert_eq!(p.rect.y, r.rule_y + 1);
                    assert_eq!(p.rect.h, r.height);
                    assert!(p.rect.x + p.rect.w <= w, "{w}x{h} {p:?}");
                    // At least the minimum, or an empty panel of a view
                    // whose minimum is 1x1 (it draws nothing there).
                    let (mw, mh) = p.view.min_size();
                    let empty = p.rect.w == 0 || p.rect.h == 0;
                    assert!(
                        (p.rect.w >= mw && p.rect.h >= mh) || (empty && (mw, mh) == (1, 1)),
                        "{w}x{h} {p:?}"
                    );
                }
                y = r.rule_y + 1 + r.height;
            }
            if h >= HEADER_ROWS + rows.len() + FOOTER_ROWS && l.panels().next().is_some() {
                assert_eq!(l.lines(), h - FOOTER_ROWS, "{w}x{h}");
            }
        }
    }
}

#[test]
fn the_gauge_goes_on_the_topmost_leftmost_host_panel() {
    let l = layout(&default_rows(), 120, 40);
    assert_eq!(l.gauge_panel().unwrap().view, View::Starfield);
    let rows = vec![
        row(1, &[(View::Flow, 1), (View::Training, 1)]),
        row(
            1,
            &[(View::Inference, 1), (View::Defrag, 1), (View::Castle, 1)],
        ),
    ];
    let l = layout(&rows, 120, 40);
    assert_eq!(l.gauge_panel().unwrap().view, View::Defrag);
    let rows = vec![row(1, &[(View::Flow, 1), (View::Training, 1)])];
    let l = layout(&rows, 120, 40);
    assert_eq!(l.gauge_panel().unwrap().view, View::Flow, "else the first");
    let l = layout(&[row(1, &[(View::Die3d, 1)])], 4, 40);
    assert!(l.gauge_panel().is_none());
}

#[test]
fn view_names_parse_with_the_waffler_alias() {
    for v in View::ALL {
        assert_eq!(View::from_key(v.key()), Some(v));
    }
    assert_eq!(View::from_key("waffler"), Some(View::Die3d));
    assert_eq!(View::from_key("Die3D"), Some(View::Die3d));
    assert_eq!(View::from_key("castel"), None);
}

/// `/arcade cloudfield`, with any value, is still understood and only says
/// the arcade carries no backdrop.
#[test]
fn the_old_cloudfield_command_says_the_arcade_carries_no_backdrop() {
    for arg in [
        "cloudfield on",
        "cloudfield off",
        "cloudfield",
        "cloudfield maybe",
    ] {
        assert_eq!(command(arg), (NO_BACKDROP_NOTE.to_string(), false), "{arg}");
    }
    assert!(command("sparkle").1);
    assert!(command("").1);
}

// ---- Saga (0.16.4) -----------------------------------------------------------

/// `saga` is a view like the others: its key parses in any case, it is in
/// [`View::ALL`] (so the "unknown view" message lists it), and its rule
/// label is `Ψ SAGA`, one column per character.
#[test]
fn saga_is_a_view_with_a_key_and_a_single_width_label() {
    assert_eq!(View::from_key("saga"), Some(View::Saga));
    assert_eq!(View::from_key(" Saga "), Some(View::Saga));
    assert_eq!(View::Saga.key(), "saga");
    assert!(View::ALL.contains(&View::Saga));
    assert_eq!(View::Saga.label(), "Ψ SAGA");
    let label = View::Saga.label();
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(label),
        label.chars().count(),
        "every character of the label is one column wide"
    );
}

/// A Saga panel needs 20x6, the HUD over five map rows. Smaller, it is
/// taken out of its row and named on the rule, and the other panel takes
/// the columns.
#[test]
fn a_saga_panel_under_20x6_is_named_on_the_rule() {
    assert_eq!(View::Saga.min_size(), (20, 6));
    assert!(View::Saga.fits(20, 6));
    assert!(!View::Saga.fits(19, 6));
    assert!(!View::Saga.fits(20, 5));
    assert!(!View::Saga.fits(0, 0), "an empty Saga panel is not drawn");
    // 30 columns shared 1:1 give Saga 14.
    let rows = vec![row(1, &[(View::Castle, 1), (View::Saga, 1)])];
    let l = layout(&rows, 30, 30);
    let r = &l.rows[0];
    assert_eq!(r.hidden, vec![(View::Saga, (20, 6))]);
    assert_eq!(r.panels.len(), 1);
    assert_eq!(r.panels[0].rect.w, 30);
    assert_eq!(hidden_note(View::Saga, 20, 6), "◌ SAGA needs 20x6");
    assert!(r.label().contains("◌ SAGA needs 20x6"), "{}", r.label());
    // 50 columns give it 24: it shows.
    let l = layout(&rows, 50, 30);
    assert!(l.rows[0].hidden.is_empty());
    assert_eq!(l.find(View::Saga).unwrap().rect.w, 24);
}
