// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! BBS sysop takeover — "the sysop wants to chat." Per-chip log lines
//! scroll in one at a time, paced by `chip_count`, looping while the real
//! reset is still in progress rather than racing ahead of it.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
const BEAT_MS: u128 = 400;

pub struct BbsTakeover {
    clock: TakeoverClock,
    chip_count: usize,
    is_full: bool,
}

impl BbsTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            chip_count: ev.chip_count.max(1),
            is_full: ev.is_full,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    /// How many "CHIP N: reset ack" lines should be visible right now,
    /// capped at `chip_count` and looping (mod `chip_count`) while the real
    /// reset is still in progress, so a longer real reset doesn't run out of
    /// choreography.
    fn visible_lines(&self) -> usize {
        let beat = (self.clock.elapsed().as_millis() / BEAT_MS) as usize;
        if self.clock.in_progress() {
            (beat % self.chip_count) + 1
        } else {
            self.chip_count
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![
            Line::from(Span::styled(
                "** SYSOP HAS TAKEN OVER THIS TERMINAL **",
                Style::default().fg(colors::rgb(0, 255, 0)),
            )),
            Line::from(Span::raw("")),
        ];
        let visible = self.visible_lines();
        for chip in 0..visible.min(self.chip_count) {
            lines.push(Line::from(Span::raw(format!(
                "CHIP {chip}: reset ack received..."
            ))));
        }
        if !self.clock.in_progress() {
            lines.push(Line::from(Span::styled(
                "CONNECTION RESTORED.",
                Style::default().fg(colors::rgb(0, 255, 0)),
            )));
        }
        let title = if self.is_full {
            "SYSTEM-WIDE RESET"
        } else {
            "PARTIAL RESET"
        };
        render_takeover_frame(f, area, title, colors::rgb(0, 220, 0), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(is_full: bool, chip_count: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full,
            chip_count,
            total_devices: 4,
            device_indices: vec![0],
            raw_targets: vec![],
        }
    }

    #[test]
    fn visible_lines_loops_while_in_progress() {
        let mut t = BbsTakeover::new(&ev(true, 2));
        t.tick(Duration::from_millis(BEAT_MS as u64 * 3)); // beat=3, 3%2=1 → 2 lines visible
        assert_eq!(t.visible_lines(), 2);
    }

    #[test]
    fn visible_lines_is_full_chip_count_once_finished() {
        let mut t = BbsTakeover::new(&ev(true, 5));
        t.note_reset_finished();
        assert_eq!(t.visible_lines(), 5);
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = BbsTakeover::new(&ev(false, 1));
        t.tick(Duration::from_secs(2));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = BbsTakeover::new(&ev(true, 8));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
