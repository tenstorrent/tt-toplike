// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Minimal full-screen status readout — "the tool doing its thing." The
//! deliberately low-key option: no box art, no color shifts, just a
//! consistent, calmly-styled status line that breathes very slightly
//! while the reset is in progress, so it doesn't read as inert. This
//! variant's whole point is to be the quiet one — it stays that way even
//! after this pass, it just no longer has one unstyled line next to a
//! styled one.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(600);
/// Full breathing cycle for the status line's brightness while in progress.
const BREATHE_PERIOD_SECS: f32 = 1.6;

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

    /// Status-line brightness: a very slight breathing pulse while the
    /// real reset is in progress (never a claim of anything but "still
    /// going"), settled to a steady, brighter value once finished.
    fn status_brightness(&self) -> f32 {
        if !self.clock.in_progress() {
            return 1.0;
        }
        let phase =
            (self.clock.elapsed().as_secs_f32() / BREATHE_PERIOD_SECS) * std::f32::consts::TAU;
        0.6 + 0.25 * phase.sin()
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let finished = !self.clock.in_progress();
        let status = if finished { "reset complete" } else { "resetting…" };
        let scope = if self.is_full {
            "all chips".to_string()
        } else {
            format!("{} chip(s)", self.chip_count)
        };

        let main_color = colors::rgb(205, 208, 214);
        let status_color = if finished {
            colors::rgb(90, 210, 130)
        } else {
            let b = self.status_brightness();
            let base = 150.0;
            let range = 60.0;
            let v = (base + range * b).clamp(0.0, 255.0) as u8;
            colors::rgb(v, v, (v as f32 * 1.03).min(255.0) as u8)
        };

        let lines = vec![
            Line::from(Span::styled(
                format!("tt-smi -r — {scope}"),
                Style::default().fg(main_color),
            )),
            Line::from(Span::styled(
                format!("· {status}"),
                Style::default().fg(status_color),
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

    /// The status line must actually breathe — a real, if subtle, change
    /// in brightness over real elapsed time while in progress.
    #[test]
    fn status_brightness_varies_with_real_elapsed_time() {
        let mut t = QuietNoticeTakeover::new(&ev(true, 4));
        let b0 = t.status_brightness();
        t.tick(Duration::from_millis(400));
        let b1 = t.status_brightness();
        assert_ne!(b0, b1, "brightness should change with real elapsed time");
    }

    /// Once finished, brightness must stop moving — settled, not still
    /// breathing after the reset is over.
    #[test]
    fn status_brightness_settles_once_finished() {
        let mut t = QuietNoticeTakeover::new(&ev(true, 4));
        t.tick(Duration::from_millis(300));
        t.note_reset_finished();
        let a = t.status_brightness();
        t.tick(Duration::from_secs(3));
        let b = t.status_brightness();
        assert_eq!(a, b, "brightness should be stable once finished");
        assert_eq!(a, 1.0);
    }
}
