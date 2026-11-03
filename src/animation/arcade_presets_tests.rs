// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The built-in layouts, the cycle `{` and `}` step through, and the
//! `/arcade layout` command. Frames drawn with each layout are tested in
//! `arcade_preset_frame_tests.rs`.

use super::*;
use crate::animation::arcade_layout::{default_rows, layout, PanelSpec, RowSpec, View};

fn named(name: &str, rows: Vec<RowSpec>) -> NamedLayout {
    NamedLayout {
        name: name.into(),
        description: format!("{name} for a test"),
        rows,
    }
}

fn flow_only() -> Vec<RowSpec> {
    vec![RowSpec {
        height: 1,
        panels: vec![PanelSpec {
            view: View::Flow,
            width: 1,
        }],
    }]
}

fn names(c: &LayoutCycle) -> Vec<String> {
    c.layouts().iter().map(|l| l.name.clone()).collect()
}

#[test]
fn there_are_eight_presets_with_valid_unique_names_and_a_description() {
    let p = presets();
    assert_eq!(
        p.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        vec![
            "classic",
            "now-serving",
            "defrag-wide",
            "waffler",
            "workload",
            "deck",
            "skyline",
            "memory-lane"
        ],
        "the user's order: now-serving right after classic"
    );
    for l in p {
        assert!(name_problem(&l.name).is_none(), "{}", l.name);
        assert!(!l.description.is_empty(), "{}", l.name);
        assert!(
            l.description.chars().count() <= MAX_DESCRIPTION,
            "{}: {} characters",
            l.name,
            l.description.chars().count()
        );
        // Each view at most once, every row with a panel.
        let mut seen = Vec::new();
        for r in &l.rows {
            assert!(!r.panels.is_empty(), "{}", l.name);
            for panel in &r.panels {
                assert!(!seen.contains(&panel.view), "{}: {}", l.name, panel.view);
                seen.push(panel.view);
            }
        }
    }
}

#[test]
fn classic_is_the_default_layout() {
    assert_eq!(preset("classic").unwrap().rows, default_rows());
    assert_eq!(presets()[0].name, "classic");
}

#[test]
fn the_waffler_preset_names_the_view_by_its_display_name() {
    let d = &preset("waffler").unwrap().description;
    assert!(d.contains(crate::animation::DIE3D_DISPLAY_NAME), "{d}");
    let d = &preset("now-serving").unwrap().description;
    assert!(d.contains(crate::animation::DIE3D_DISPLAY_NAME), "{d}");
    // `no_user_facing_text_uses_the_old_die3d_name` checks that no
    // description uses the old name.
}

#[test]
fn without_a_config_the_cycle_is_the_presets_and_classic_comes_first() {
    let c = LayoutCycle::new(&[], None);
    assert_eq!(
        names(&c),
        presets().iter().map(|l| l.name.clone()).collect::<Vec<_>>()
    );
    assert_eq!(c.current().name, "classic");
    assert_eq!(c.index(), 0);
}

#[test]
fn the_cycle_is_config_then_the_files_layouts_then_the_presets() {
    let file = vec![
        named(CONFIG_LAYOUT, flow_only()),
        named("mine", flow_only()),
        named("theirs", flow_only()),
    ];
    let c = LayoutCycle::new(&file, None);
    let n = names(&c);
    assert_eq!(&n[..4], &["config", "mine", "theirs", "classic"]);
    assert_eq!(n.len(), 3 + presets().len());
    assert_eq!(c.current().name, "config", "the first layout starts");
}

#[test]
fn next_then_prev_returns_to_the_start_and_both_ends_wrap() {
    let mut c = LayoutCycle::new(&[named("mine", flow_only())], None);
    let n = c.len();
    assert_eq!(c.index(), 0);
    c.next();
    assert_eq!(c.current().name, "classic");
    c.prev();
    assert_eq!(c.current().name, "mine");
    // Back from the first is the last.
    c.prev();
    assert_eq!(c.index(), n - 1);
    assert_eq!(c.current().name, "memory-lane");
    // On from the last is the first.
    c.next();
    assert_eq!(c.index(), 0);
    // A whole lap ends where it started.
    for _ in 0..n {
        c.next();
    }
    assert_eq!(c.index(), 0);
}

#[test]
fn a_start_name_picks_the_layout_and_an_unknown_one_starts_with_the_first() {
    let c = LayoutCycle::new(&[], Some("deck"));
    assert_eq!(c.current().name, "deck");
    let c = LayoutCycle::new(&[], Some("DECK"));
    assert_eq!(c.current().name, "deck", "letter case is ignored");
    let c = LayoutCycle::new(&[named("mine", flow_only())], Some("nope"));
    assert_eq!(c.current().name, "mine");
}

#[test]
fn select_finds_a_layout_by_name() {
    let mut c = LayoutCycle::new(&[], None);
    assert!(c.select("skyline"));
    assert_eq!(c.current().name, "skyline");
    assert!(!c.select("nope"));
    assert_eq!(
        c.current().name,
        "skyline",
        "an unknown name changes nothing"
    );
}

#[test]
fn the_status_line_names_the_layout_its_place_and_its_description() {
    let mut c = LayoutCycle::new(&[], None);
    c.next();
    c.next();
    let d = &preset("defrag-wide").unwrap().description;
    assert_eq!(c.status(), format!("layout: defrag-wide (3/8) — {d}"));
    c.prev();
    assert_eq!(
        c.status(),
        "layout: now-serving (2/8) — inference over Wafer Waffler and defrag"
    );
    let mut c = LayoutCycle::new(
        &[NamedLayout {
            name: "bare".into(),
            description: String::new(),
            rows: flow_only(),
        }],
        None,
    );
    assert_eq!(c.status(), "layout: bare (1/9)", "no description, no dash");
    c.next();
    assert!(c.status().starts_with("layout: classic (2/9) — "));
}

#[test]
fn the_layout_command_steps_selects_and_lists() {
    let mut c = LayoutCycle::new(&[], None);
    let r = command("layout next", &mut c);
    assert_eq!(c.current().name, "now-serving");
    assert!(!r.error && r.relayout);
    assert_eq!(r.message, c.status());
    let r = command("layout prev", &mut c);
    assert_eq!(c.current().name, "classic");
    assert!(r.relayout);
    let r = command("layout memory-lane", &mut c);
    assert_eq!(c.current().name, "memory-lane");
    assert!(r.relayout && !r.error);
    let r = command("layout list", &mut c);
    assert!(!r.relayout && !r.error);
    assert_eq!(
        r.message,
        "layouts, now memory-lane (8/8):\n  classic, now-serving, defrag-wide, waffler, workload, deck, skyline,\n  memory-lane"
    );
    assert_eq!(command("layout", &mut c).message, r.message);
    let r = command("layout castel", &mut c);
    assert!(r.error && !r.relayout);
    assert_eq!(
        r.message,
        "no arcade layout `castel`\n/arcade layout list names them"
    );
    // A typed control character is shown escaped.
    let r = command("layout a\u{1b}[2J", &mut c);
    assert!(r.error && !r.message.contains('\u{1b}'), "{:?}", r.message);
    assert_eq!(
        c.current().name,
        "memory-lane",
        "a bad name changes nothing"
    );
    assert!(command("layout next please", &mut c).error);
}

/// `/arcade cloudfield` is still understood and says the arcade carries no
/// backdrop; it changes nothing.
#[test]
fn the_old_cloudfield_command_only_says_there_is_no_backdrop() {
    let mut c = LayoutCycle::new(&[], None);
    for arg in ["cloudfield on", "cloudfield off", "cloudfield maybe"] {
        let r = command(arg, &mut c);
        assert_eq!(r.message, "the arcade carries no backdrop", "{arg}");
        assert!(!r.error && !r.relayout, "{arg}");
        assert_eq!(c.index(), 0);
    }
    let r = command("sparkle", &mut c);
    assert!(r.error);
    assert_eq!(r.message, USAGE);
}

#[test]
fn names_are_short_plain_words() {
    assert!(name_problem("my-layout_2").is_none());
    for bad in ["", "a b", "naïve", "x\u{1b}[31m", &"x".repeat(MAX_NAME + 1)] {
        assert!(name_problem(bad).is_some(), "{bad:?}");
    }
    assert!(is_reserved("next") && is_reserved("PREV") && is_reserved("list"));
    assert!(is_reserved("config"));
    assert!(!is_reserved("deck"));
}

/// What each preset shows at 80x24 and 60x20. docs/arcade-layout.md says
/// the same; change both together.
#[test]
fn each_preset_degrades_as_the_docs_say() {
    let shown = |name: &str, w, h| -> (Vec<View>, Vec<View>) {
        let l = layout(&preset(name).unwrap().rows, w, h);
        (
            l.panels().map(|p| p.view).collect(),
            l.rows
                .iter()
                .flat_map(|r| r.hidden.iter().map(|(v, _)| *v))
                .collect(),
        )
    };
    use View::*;
    let everything = |name: &str| {
        preset(name)
            .unwrap()
            .rows
            .iter()
            .flat_map(|r| r.panels.iter().map(|p| p.view))
            .collect::<Vec<_>>()
    };
    // At 80x24.
    for name in [
        "classic",
        "defrag-wide",
        "waffler",
        "workload",
        "skyline",
        "memory-lane",
    ] {
        assert_eq!(
            shown(name, 80, 24),
            (everything(name), vec![]),
            "{name} 80x24"
        );
    }
    assert_eq!(
        shown("deck", 80, 24),
        (
            vec![Starfield, Castle, Defrag, Die3d, Flow],
            vec![Training, Inference]
        )
    );
    // At 60x20.
    for name in [
        "classic",
        "defrag-wide",
        "waffler",
        "workload",
        "skyline",
        "memory-lane",
    ] {
        assert_eq!(
            shown(name, 60, 20),
            (everything(name), vec![]),
            "{name} 60x20"
        );
    }
    assert_eq!(
        shown("deck", 60, 20),
        (
            vec![Starfield, Castle, Defrag, Die3d, Flow],
            vec![Training, Inference]
        )
    );
    // now-serving keeps the inference panel at 80x24; at 60x20 its row is
    // 6 rows, under the panel's 8, so it is taken out and its rows go to
    // the Wafer Waffler and defrag.
    assert_eq!(
        shown("now-serving", 80, 24),
        (everything("now-serving"), vec![])
    );
    assert_eq!(
        shown("now-serving", 60, 20),
        (vec![Die3d, Defrag], vec![Inference])
    );
    let l = layout(&preset("now-serving").unwrap().rows, 80, 24);
    assert_eq!(l.find(Inference).unwrap().rect.h, 8);
    assert!(
        l.rows[1].height > l.rows[0].height,
        "the bottom row is the big one"
    );
    // Everything shows at 120x40 and 200x60.
    for l in presets() {
        for (w, h) in [(120, 40), (200, 60)] {
            assert_eq!(
                shown(&l.name, w, h),
                (everything(&l.name), vec![]),
                "{} {w}x{h}",
                l.name
            );
        }
    }
}

/// docs/arcade-layout.md has a section for every preset, and its sketches
/// have a left bar and a bottom bar only: no line ends in a right-side
/// border and no right-hand corner appears.
#[test]
fn the_docs_sketch_every_preset_with_no_right_border() {
    let doc = include_str!("../../docs/arcade-layout.md");
    for l in presets() {
        assert!(
            doc.contains(&format!("### `{}`\n", l.name)),
            "no section for {}",
            l.name
        );
    }
    let mut in_text = false;
    let mut sketches = 0;
    for line in doc.lines() {
        match line.trim_start() {
            "```text" => {
                in_text = true;
                sketches += 1;
            }
            "```" => in_text = false,
            body if in_text => {
                let end = body.trim_end().chars().last().unwrap_or(' ');
                assert!(
                    !crate::animation::arcade::RIGHT_EDGE_GLYPHS.contains(&end),
                    "ends in a right border: {body:?}"
                );
                assert!(!body.contains(['╗', '╝', '┐', '┘']), "{body:?}");
            }
            _ => {}
        }
    }
    assert!(sketches >= presets().len(), "{sketches} sketches");
}

#[test]
fn now_serving_has_no_starfield() {
    let ns = preset("now-serving").unwrap();
    let views: Vec<View> = ns
        .rows
        .iter()
        .flat_map(|r| r.panels.iter().map(|p| p.view))
        .collect();
    assert_eq!(views, vec![View::Inference, View::Die3d, View::Defrag]);
}

/// Every built-in layout's status line fits an 80-column status bar
/// whole (S2 of the review), and a long user description is cut with `…`.
#[test]
fn every_status_line_fits_80_columns() {
    let mut c = LayoutCycle::new(&[], None);
    for _ in 0..c.len() {
        let s = c.status();
        assert!(
            s.chars().count() <= STATUS_MAX,
            "{s}: {}",
            s.chars().count()
        );
        assert!(!s.ends_with('…'), "{s}");
        assert!(s.contains(&c.current().description), "{s}");
        c.next();
    }
    let long = NamedLayout {
        name: "n".repeat(MAX_NAME),
        description: "d".repeat(MAX_DESCRIPTION),
        rows: flow_only(),
    };
    let c = LayoutCycle::new(&[long], None);
    let s = c.status();
    assert_eq!(s.chars().count(), STATUS_MAX, "{s}");
    assert!(s.ends_with('…') && s.contains(") — "), "{s}");
    // The list, the usage and the reply to an unknown name (at its longest
    // shown name, re-review M4) are lines of at most 78 characters.
    let mut c = LayoutCycle::new(&[], None);
    let unknown = command(&format!("layout {}", "x".repeat(100)), &mut c);
    assert!(unknown.error && unknown.message.contains("/arcade layout list"));
    for text in [c.wrapped_names(), USAGE.to_string(), unknown.message] {
        for line in text.lines() {
            assert!(line.chars().count() <= STATUS_MAX, "{line}");
        }
    }
}
