// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Minimal full-screen status readout — "the tool doing its thing."

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(600);

pub struct QuietNoticeTakeover {
    clock: TakeoverClock,
    is_full: bool,
    chip_count: usize,
}

impl QuietNoticeTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            is_full: ev.is_full,
            chip_count: ev.chip_count,
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

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let status = if self.clock.in_progress() {
            "resetting…"
        } else {
            "reset complete"
        };
        let scope = if self.is_full {
            "all chips".to_string()
        } else {
            format!("{} chip(s)", self.chip_count)
        };
        let lines = vec![
            Line::from(Span::raw(format!("tt-smi -r — {scope}"))),
            Line::from(Span::styled(
                status,
                Style::default().fg(colors::rgb(180, 180, 190)),
            )),
        ];
        render_takeover_frame(f, area, "RESET", colors::rgb(140, 140, 150), lines);
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
            raw_targets: vec!["0".to_string()],
        }
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = QuietNoticeTakeover::new(&ev(true, 4));
        t.tick(Duration::from_secs(5));
        assert!(!t.is_done());
        t.note_reset_finished();
        assert!(!t.is_done());
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = QuietNoticeTakeover::new(&ev(false, 2));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = QuietNoticeTakeover::new(&ev(false, 2));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
