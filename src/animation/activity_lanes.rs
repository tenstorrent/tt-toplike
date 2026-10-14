// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Draws Arcade's activity lanes: at most two rows, one per activity. A lane is
//! a left edge, a tag, a name, then fields in priority order. When the width is
//! short the key hint goes first, then fields from the last one back, and a lane
//! that cannot hold its tag and name is skipped. No right-side border.

use crate::animation::activity::{Activity, ActivityKind};
use crate::ui::colors;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

const MAX_LANES: usize = 2;
const SEP: &str = " · ";

fn tag_of(a: &Activity) -> (&'static str, &'static str) {
    if a.stalled {
        return ("✗", "STALLED");
    }
    match a.kind {
        ActivityKind::Train => ("▶", "TRAIN"),
        ActivityKind::Load => ("◐", "LOAD"),
        ActivityKind::Serve => ("●", "SERVE"),
    }
}

fn accent(a: &Activity) -> ratatui::style::Color {
    if a.stalled {
        return colors::error();
    }
    match a.kind {
        ActivityKind::Train => colors::rgb(246, 188, 66),
        ActivityKind::Load => colors::rgb(180, 140, 230),
        ActivityKind::Serve => colors::rgb(79, 209, 197),
    }
}

/// One lane within `width` columns, or `None` when its tag and name do not fit.
fn lane(a: &Activity, width: usize, more: usize) -> Option<Line<'static>> {
    let (glyph, word) = tag_of(a);
    let head = format!("▎{glyph} {word} {}", a.label);
    let more_note = if more > 0 {
        format!("  +{more} more")
    } else {
        String::new()
    };
    let hint = format!("  [{}]", a.key_hint);
    let w = |s: &str| UnicodeWidthStr::width(s);
    if w(&head) > width {
        return None;
    }
    // Keep as many fields as fit, then the hint if it still fits. The "+N more"
    // note outranks the hint but not the fields already shown.
    let mut keep = a.fields.len();
    let total = |keep: usize, hint: &str| -> usize {
        let fields: usize = a.fields[..keep].iter().map(|f| w(SEP) + w(f)).sum();
        w(&head) + fields + w(&more_note) + w(hint)
    };
    while keep > 0 && total(keep, "") > width {
        keep -= 1;
    }
    let show_hint = total(keep, &hint) <= width;
    let show_more = total(keep, "") <= width;

    let color = accent(a);
    let mut spans = vec![
        Span::styled("▎", Style::default().fg(color)),
        Span::styled(
            format!("{glyph} {word} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(a.label.clone(), Style::default().fg(colors::text_primary())),
    ];
    for f in &a.fields[..keep] {
        spans.push(Span::styled(
            SEP,
            Style::default().fg(colors::text_secondary()),
        ));
        spans.push(Span::styled(
            f.clone(),
            Style::default().fg(colors::text_secondary()),
        ));
    }
    if show_more && !more_note.is_empty() {
        spans.push(Span::styled(
            more_note,
            Style::default().fg(colors::text_secondary()),
        ));
    }
    if show_hint {
        spans.push(Span::styled(hint, Style::default().fg(color)));
    }
    Some(Line::from(spans))
}

/// The lanes for `activities` (already in urgency order) within `width`.
pub fn render_lanes(activities: &[Activity], width: usize) -> Vec<Line<'static>> {
    let shown = activities.len().min(MAX_LANES);
    let mut out = Vec::with_capacity(shown);
    for (i, a) in activities.iter().take(MAX_LANES).enumerate() {
        let more = if i + 1 == MAX_LANES {
            activities.len().saturating_sub(MAX_LANES)
        } else {
            0
        };
        if let Some(l) = lane(a, width, more) {
            out.push(l);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::activity::{Activity, ActivityKind};

    fn act(kind: ActivityKind, label: &str, fields: &[&str], hint: char) -> Activity {
        Activity {
            kind,
            label: label.into(),
            stalled: false,
            fields: fields.iter().map(|s| s.to_string()).collect(),
            pulse_seq: 0,
            key_hint: hint,
        }
    }

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn train() -> Activity {
        act(
            ActivityKind::Train,
            "nano_gpt",
            &[
                "step 4,120/10,000 ████░░░░ 41%",
                "12m left",
                "1.8 step/s",
                "loss ↘",
            ],
            't',
        )
    }

    fn load() -> Activity {
        act(
            ActivityKind::Load,
            "rag-gate-27b",
            &[":8100 · chips 0,1", "loading 0:07"],
            'i',
        )
    }

    #[test]
    fn a_wide_lane_shows_everything_in_priority_order() {
        let lanes = render_lanes(&[train()], 120);
        assert_eq!(lanes.len(), 1);
        let t = text(&lanes[0]);
        assert!(t.contains("TRAIN") && t.contains("nano_gpt"), "{t}");
        let (a, b, c, d) = (
            t.find("step 4,120").unwrap(),
            t.find("12m left").unwrap(),
            t.find("1.8 step/s").unwrap(),
            t.find("loss ↘").unwrap(),
        );
        assert!(a < b && b < c && c < d, "{t}");
        assert!(t.trim_end().ends_with("[t]"), "{t}");
    }

    #[test]
    fn a_narrow_lane_drops_the_hint_then_the_last_fields_first() {
        let wide = text(&render_lanes(&[train()], 200)[0]);
        let w = wide.chars().count();
        let mid = text(&render_lanes(&[train()], w - 2)[0]);
        assert!(!mid.contains("[t]"), "the hint goes first: {mid}");
        assert!(mid.contains("loss ↘"), "{mid}");
        let tight = text(&render_lanes(&[train()], 40)[0]);
        assert!(
            tight.contains("TRAIN") && tight.contains("nano_gpt"),
            "{tight}"
        );
        assert!(!tight.contains("loss"), "{tight}");
    }

    #[test]
    fn a_lane_that_cannot_fit_its_tag_and_name_is_skipped() {
        assert!(render_lanes(&[train()], 6).is_empty());
        assert!(render_lanes(&[train()], 0).is_empty());
    }

    #[test]
    fn two_lanes_and_a_more_note_for_three_or_more() {
        let s = act(
            ActivityKind::Serve,
            "Qwen3-32B",
            &[":8000", "42 tok/s"],
            'i',
        );
        let two = render_lanes(&[load(), train()], 120);
        assert_eq!(two.len(), 2);
        let three = render_lanes(&[load(), train(), s], 120);
        assert_eq!(three.len(), 2);
        assert!(text(&three[1]).contains("+1 more"), "{}", text(&three[1]));
    }

    #[test]
    fn a_stalled_activity_is_marked_and_uses_the_error_colour() {
        let mut a = load();
        a.stalled = true;
        let l = &render_lanes(&[a], 120)[0];
        assert!(text(l).contains("✗"), "{}", text(l));
        let err = crate::ui::colors::error();
        assert!(l.spans.iter().any(|s| s.style.fg == Some(err)));
    }

    /// No lane is wider than the terminal at any width, and none has a
    /// right-side border glyph.
    #[test]
    fn lanes_never_exceed_the_width_and_have_no_right_border() {
        use unicode_width::UnicodeWidthStr;
        let s = act(
            ActivityKind::Serve,
            "Qwen3-32B",
            &[":8000 · chips 0,1", "42 tok/s", "3 running"],
            'i',
        );
        let mixes: Vec<Vec<Activity>> = vec![
            vec![train()],
            vec![load()],
            vec![load(), train()],
            vec![load(), train(), s.clone()],
            vec![s],
        ];
        for width in 0..=200 {
            for mix in &mixes {
                for l in render_lanes(mix, width) {
                    let t = text(&l);
                    assert!(
                        UnicodeWidthStr::width(t.as_str()) <= width,
                        "width {width}: {t:?}"
                    );
                    let last = t.trim_end().chars().last().unwrap_or(' ');
                    assert!(
                        !"│┐┘║▕┃".contains(last),
                        "right border at width {width}: {t:?}"
                    );
                }
            }
        }
    }
}
