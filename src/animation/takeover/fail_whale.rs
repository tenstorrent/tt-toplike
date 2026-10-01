// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Fail Whale — a flock of little birds gently carries the whale down,
//! bobbing and wing-flapping while the real reset is still in progress
//! (never faking a landing time we don't know), then easing into a soft,
//! happy touchdown once the reset has actually finished. Bird count scales
//! with the real chip count — a bigger reset needs more birds to carry.

use super::{render_takeover_frame, takeover_interior, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(1200);
/// How long the final descent-to-ground glide takes once the real reset
/// has finished — kept at/under `DONE_TAIL` so it completes right as the
/// takeover itself is allowed to end.
const LANDING_SECS: f32 = 1.0;
/// Gentle hover bob while airborne: still slow, but wide enough to
/// actually cross row boundaries in a character grid (a sub-row amplitude
/// would compute a "moving" position that renders as a frozen scene, since
/// terminal cells only have integer rows).
const BOB_AMPLITUDE_ROWS: f32 = 1.6;
const BOB_RATE: f32 = 1.1;
const FLAP_MS: u128 = 350;

const WHALE_BODY: [&str; 7] = [
    "        .    .         ",
    "         \\  /          ",
    "     .--=======--.     ",
    "   ,'             `.   ",
    "  /        o        \\  ",
    "  \\                 ,'>",
    "   `--...______...--'  ",
];
const WHALE_WIDTH: usize = 24;

pub struct FailWhaleTakeover {
    clock: TakeoverClock,
    bird_count: usize,
    /// Snapshot of `clock.elapsed()` the instant the real reset finished —
    /// tracked locally (rather than adding this to the shared
    /// `TakeoverClock`) so the landing glide can be paced by real
    /// time-since-finished without new shared-framework surface.
    finished_at: Option<Duration>,
}

impl FailWhaleTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            bird_count: ev.chip_count.clamp(1, 5),
            finished_at: None,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        if self.finished_at.is_none() {
            self.finished_at = Some(self.clock.elapsed());
        }
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    /// `0.0` = still airborne, `1.0` = fully landed and settled. Only
    /// starts moving once the real reset has actually finished.
    fn landing_progress(&self) -> f32 {
        match self.finished_at {
            None => 0.0,
            Some(f) => {
                let since = self.clock.elapsed().saturating_sub(f).as_secs_f32();
                (since / LANDING_SECS).clamp(0.0, 1.0)
            }
        }
    }

    fn wings_flap_open(&self) -> bool {
        (self.clock.elapsed().as_millis() / FLAP_MS) % 2 == 0
    }

    /// The scene's vertical position (in rows from the top of the box
    /// interior) at the current elapsed time — a pure function of state, kept
    /// separate from `render` so the real motion it produces is directly
    /// testable without depending on exactly when a floating-point bob
    /// happens to cross an integer row boundary in a rendered snapshot.
    fn scene_top(&self, interior_height: u16) -> f32 {
        let progress = self.landing_progress();
        let landed = progress >= 1.0;
        // Quadratic ease-out: fast at first, gentle as it settles.
        let eased = 1.0 - (1.0 - progress) * (1.0 - progress);
        let bob = if landed {
            0.0
        } else {
            BOB_AMPLITUDE_ROWS * (self.clock.elapsed().as_secs_f32() * BOB_RATE).sin()
        };
        let available_rows = interior_height.saturating_sub(6).max(12) as f32;
        let hover_row = available_rows * 0.18;
        // Leave room below `ground_row` for the whale body itself plus the
        // rope/bird (or flight/ground) row that follows it — otherwise a
        // "landed" position computed from the bottom of the area alone
        // pushes the whale's own lower half and the ground line straight
        // off the bottom of the screen.
        let scene_height = WHALE_BODY.len() as f32 + 2.0;
        let ground_row = (available_rows - scene_height).max(hover_row);
        hover_row + (ground_row - hover_row) * eased + bob
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let progress = self.landing_progress();
        let landed = progress >= 1.0;

        let whale_blue = colors::rgb(130, 165, 200);
        let bird_color = colors::rgb(70, 75, 90);
        let ground_color = colors::rgb(120, 165, 90);
        let rope_color = colors::rgb(150, 140, 120);

        // The scene is laid out against the box interior height.
        let top_pad = self.scene_top(takeover_interior(area).height).max(0.0) as usize;

        let mut lines: Vec<Line<'static>> = Vec::new();
        for _ in 0..top_pad {
            lines.push(Line::from(Span::raw("")));
        }

        for body_line in WHALE_BODY {
            lines.push(Line::from(Span::styled(
                body_line.to_string(),
                Style::default().fg(whale_blue),
            )));
        }

        if !landed {
            // Ropes + birds carrying the whale, wings alternating.
            let wing = if self.wings_flap_open() {
                "/|\\"
            } else {
                "\\|/"
            };
            let rope_row: String = (0..self.bird_count)
                .map(|_| format!("{:^w$}", "|", w = WHALE_WIDTH / self.bird_count.max(1)))
                .collect();
            let bird_row: String = (0..self.bird_count)
                .map(|_| format!("{:^w$}", wing, w = WHALE_WIDTH / self.bird_count.max(1)))
                .collect();
            lines.push(Line::from(Span::styled(
                rope_row,
                Style::default().fg(rope_color),
            )));
            lines.push(Line::from(Span::styled(
                bird_row,
                Style::default().fg(bird_color),
            )));
        } else {
            // Settled: birds have let go and flown off, happy, above.
            let flight_row: String = (0..self.bird_count)
                .map(|_| format!("{:^w$}", "^", w = WHALE_WIDTH / self.bird_count.max(1)))
                .collect();
            let ground_row_str = "~".repeat(WHALE_WIDTH + 4);
            lines.insert(
                top_pad,
                Line::from(Span::styled(flight_row, Style::default().fg(bird_color))),
            );
            lines.push(Line::from(Span::styled(
                ground_row_str,
                Style::default().fg(ground_color),
            )));
        }

        let title = if landed { "TOUCHDOWN" } else { "FAIL WHALE" };
        let tint = if landed {
            ground_color
        } else {
            colors::rgb(90, 130, 180)
        };
        render_takeover_frame(f, area, title, tint, lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(chip_count: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: true,
            chip_count,
            total_devices: chip_count,
            device_indices: (0..chip_count as u8).collect(),
            raw_targets: vec![],
        }
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = FailWhaleTakeover::new(&ev(4));
        t.tick(Duration::from_secs(3));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = FailWhaleTakeover::new(&ev(2));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_across_bird_counts_and_states() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        for chips in [1, 2, 4, 5, 8] {
            let backend = TestBackend::new(80, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut t = FailWhaleTakeover::new(&ev(chips));
            terminal.draw(|f| t.render(f, f.area())).unwrap();
            t.tick(Duration::from_millis(500));
            terminal.draw(|f| t.render(f, f.area())).unwrap();
            t.note_reset_finished();
            terminal.draw(|f| t.render(f, f.area())).unwrap();
            t.tick(Duration::from_secs(2));
            terminal.draw(|f| t.render(f, f.area())).unwrap();
        }
    }

    #[test]
    fn render_does_not_panic_on_a_small_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(40, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = FailWhaleTakeover::new(&ev(4));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    /// Bird count scales with the real chip count — a bigger reset needs
    /// more birds to carry, capped to a sane visual range.
    #[test]
    fn bird_count_scales_with_chip_count_and_caps() {
        assert_eq!(FailWhaleTakeover::new(&ev(1)).bird_count, 1);
        assert_eq!(FailWhaleTakeover::new(&ev(3)).bird_count, 3);
        assert_eq!(FailWhaleTakeover::new(&ev(20)).bird_count, 5);
    }

    /// The whale must not start descending before the real reset finishes
    /// — no faked landing time.
    #[test]
    fn landing_progress_stays_zero_while_in_progress() {
        let mut t = FailWhaleTakeover::new(&ev(4));
        t.tick(Duration::from_secs(10));
        assert_eq!(t.landing_progress(), 0.0);
    }

    /// Once finished, landing progress must actually advance over real
    /// elapsed time, then settle at 1.0 and stop moving.
    #[test]
    fn landing_progress_advances_then_settles() {
        let mut t = FailWhaleTakeover::new(&ev(4));
        t.note_reset_finished();
        let p0 = t.landing_progress();
        t.tick(Duration::from_millis(400));
        let p1 = t.landing_progress();
        assert!(p1 > p0, "landing should advance over real elapsed time");
        t.tick(Duration::from_secs(5));
        let p2 = t.landing_progress();
        assert_eq!(
            p2, 1.0,
            "landing should settle at fully-landed and stop climbing past it"
        );
    }

    /// While airborne, the scene must actually bob — real motion, not a
    /// static hover. Asserted directly on the position calculation (not a
    /// rendered snapshot), so this doesn't depend on a floating-point bob
    /// happening to cross an integer row boundary within an arbitrary tick
    /// window.
    #[test]
    fn hovers_with_real_motion_while_in_progress() {
        let mut t = FailWhaleTakeover::new(&ev(4));
        let a = t.scene_top(30);
        t.tick(Duration::from_millis(700));
        let b = t.scene_top(30);
        assert_ne!(
            a, b,
            "the scene's position should change while hovering, not sit static"
        );
    }

    /// The bob amplitude must be wide enough to actually cross a row
    /// boundary over a bob cycle — otherwise the "motion" is invisible in
    /// a character grid regardless of what the floating-point math says.
    #[test]
    fn bob_amplitude_crosses_at_least_one_row_boundary() {
        let mut t = FailWhaleTakeover::new(&ev(4));
        let mut floors = std::collections::HashSet::new();
        for _ in 0..40 {
            floors.insert(t.scene_top(30).floor() as i32);
            t.tick(Duration::from_millis(150));
        }
        assert!(
            floors.len() >= 2,
            "expected the hover to visibly cross at least one row boundary, floors seen: {floors:?}"
        );
    }
}
