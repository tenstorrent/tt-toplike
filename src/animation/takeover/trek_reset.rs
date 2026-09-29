// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Classic 1970s-BASIC-style "Star Trek" reset screen: a STARDATE header, a
//! short-range sensor scan grid (one cell per known chip, targeted ones
//! shown as Klingons under fire), and a torpedo/destroyed status line.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);

pub struct TrekResetTakeover {
    clock: TakeoverClock,
    device_indices: Vec<u8>,
    total_devices: usize,
    /// Cosmetic flavor text only — derived from real wall-clock time so
    /// repeated runs aren't identical, never used for any real pacing
    /// decision (pacing is entirely `TakeoverClock`-driven).
    stardate: String,
}

impl TrekResetTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let stardate = format!("{:.1}", (now.as_secs() as f64 / 86_400.0) % 10_000.0);
        Self {
            clock: TakeoverClock::new(),
            device_indices: ev.device_indices.clone(),
            total_devices: ev.total_devices,
            stardate,
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
        let mut lines = vec![
            Line::from(Span::styled(
                format!("STARDATE {}", self.stardate),
                Style::default().fg(colors::rgb(255, 204, 0)),
            )),
            Line::from(Span::raw("SHORT RANGE SENSOR SCAN")),
            Line::from(Span::raw("")),
        ];
        let mut row = String::new();
        let total = self.total_devices.max(1);
        for i in 0..total {
            let targeted = self.device_indices.contains(&(i as u8));
            row.push(if !targeted {
                '.'
            } else if self.clock.in_progress() {
                'K'
            } else {
                '*'
            });
            row.push(' ');
        }
        lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(colors::rgb(0, 255, 120)),
        )));
        lines.push(Line::from(Span::raw(if self.clock.in_progress() {
            "PHOTON TORPEDOES AWAY..."
        } else {
            "KLINGON BATTLE CRUISER DESTROYED"
        })));
        render_takeover_frame(f, area, "USS ENTERPRISE", colors::rgb(0, 180, 255), lines);
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
        let mut t = TrekResetTakeover::new(&ev(vec![0], 4));
        t.tick(Duration::from_secs(2));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = TrekResetTakeover::new(&ev(vec![0], 4));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_with_full_and_subset_targets() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let full = TrekResetTakeover::new(&ev(vec![0, 1, 2, 3], 4));
        terminal.draw(|f| full.render(f, f.area())).unwrap();
        let subset = TrekResetTakeover::new(&ev(vec![2], 4));
        terminal.draw(|f| subset.render(f, f.area())).unwrap();
    }
}
