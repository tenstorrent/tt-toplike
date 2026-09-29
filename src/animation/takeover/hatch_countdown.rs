// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Lost-hatch-style repeating countdown, paced by real elapsed/in-progress
//! time rather than a single fixed guess at how long the real reset takes.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
const CYCLE_MS: u128 = 30_000;

pub struct HatchCountdownTakeover {
    clock: TakeoverClock,
}

impl HatchCountdownTakeover {
    pub fn new(_ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
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

    /// Seconds remaining in the current repeating countdown cycle, or `0`
    /// once the real reset has finished.
    fn remaining_secs(&self) -> u64 {
        if !self.clock.in_progress() {
            return 0;
        }
        let pos = self.clock.elapsed().as_millis() % CYCLE_MS;
        ((CYCLE_MS - pos) / 1000) as u64
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(Span::raw("THE HATCH")),
            Line::from(Span::styled(
                format!("{:04}", self.remaining_secs()),
                Style::default().fg(colors::rgb(255, 80, 0)),
            )),
            Line::from(Span::raw(if self.clock.in_progress() {
                "EXECUTE"
            } else {
                "SYSTEM RESET"
            })),
        ];
        render_takeover_frame(f, area, "DHARMA INITIATIVE", colors::rgb(255, 140, 0), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: true,
            chip_count: 4,
            total_devices: 4,
            device_indices: vec![0, 1, 2, 3],
            raw_targets: vec![],
        }
    }

    #[test]
    fn remaining_secs_counts_down_within_a_cycle() {
        let mut t = HatchCountdownTakeover::new(&ev());
        let start = t.remaining_secs();
        t.tick(Duration::from_secs(5));
        assert_eq!(t.remaining_secs(), start.saturating_sub(5));
    }

    #[test]
    fn remaining_secs_zero_once_finished() {
        let mut t = HatchCountdownTakeover::new(&ev());
        t.note_reset_finished();
        assert_eq!(t.remaining_secs(), 0);
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = HatchCountdownTakeover::new(&ev());
        t.tick(Duration::from_secs(40)); // past one full cycle, still "in progress"
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
        let t = HatchCountdownTakeover::new(&ev());
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
