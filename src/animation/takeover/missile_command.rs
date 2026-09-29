// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Missile Command — crosshairs/explosions only at the actual affected
//! chip(s), so a subset reset visibly fires at just those chips.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(800);
/// How long each `[*]`/`[ ]` blink half-cycle lasts while in progress.
const BLINK_MS: u128 = 250;

pub struct MissileCommandTakeover {
    clock: TakeoverClock,
    device_indices: Vec<u8>,
    total_devices: usize,
}

impl MissileCommandTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            device_indices: ev.device_indices.clone(),
            total_devices: ev.total_devices,
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
        let frame_on = (self.clock.elapsed().as_millis() / BLINK_MS) % 2 == 0;
        let mut cells = String::new();
        let total = self.total_devices.max(1);
        for i in 0..total {
            let targeted = self.device_indices.contains(&(i as u8));
            let glyph = if !targeted {
                "[ ]"
            } else if self.clock.in_progress() {
                if frame_on {
                    "[*]"
                } else {
                    "[ ]"
                }
            } else {
                "[X]"
            };
            cells.push_str(glyph);
        }
        let lines = vec![
            Line::from(Span::raw("INCOMING RESET")),
            Line::from(Span::styled(
                cells,
                Style::default().fg(colors::rgb(255, 120, 90)),
            )),
        ];
        render_takeover_frame(f, area, "MISSILE COMMAND", colors::rgb(255, 90, 60), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(device_indices: Vec<u8>, total_devices: usize) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: device_indices.len() == total_devices,
            chip_count: device_indices.len(),
            total_devices,
            device_indices,
            raw_targets: vec![],
        }
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = MissileCommandTakeover::new(&ev(vec![1], 4));
        t.tick(Duration::from_secs(3));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = MissileCommandTakeover::new(&ev(vec![0], 4));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_with_subset_targets() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = MissileCommandTakeover::new(&ev(vec![1, 2], 4));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_zero_total_devices() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = MissileCommandTakeover::new(&ev(vec![], 0));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
