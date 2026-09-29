// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! "What would 1024 Blackholes look like" — a dense synthetic glyph swarm
//! whose density scales with the real `chip_count`, capped to what the
//! terminal can actually show.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
/// Synthetic glyphs per real targeted chip — a stylized multiplier, not a
/// claim about real telemetry (see design spec: content should *scale with*
/// chip_count, not literally count real per-chip signals we don't have).
const GLYPHS_PER_CHIP: usize = 32;
const MAX_GLYPHS: usize = 1024;

pub struct BlackholeSwarmTakeover {
    clock: TakeoverClock,
    density: usize,
}

impl BlackholeSwarmTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        let density = (ev.chip_count.max(1) * GLYPHS_PER_CHIP).min(MAX_GLYPHS);
        Self {
            clock: TakeoverClock::new(),
            density,
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
        let cols = area.width.saturating_sub(4).max(1) as usize;
        let rows = area.height.saturating_sub(4).max(1) as usize;
        let cap = cols * rows;
        let filled = self.density.min(cap);
        let mut lines = Vec::with_capacity(rows);
        let mut remaining = filled;
        for _ in 0..rows {
            let take = remaining.min(cols);
            remaining -= take;
            let mut row = String::with_capacity(cols);
            for c in 0..cols {
                row.push(if c < take { '¤' } else { ' ' });
            }
            lines.push(Line::from(Span::styled(
                row,
                Style::default().fg(colors::rgb(160, 100, 255)),
            )));
        }
        render_takeover_frame(
            f,
            area,
            &format!("{} BLACKHOLES", self.density),
            colors::rgb(160, 100, 255),
            lines,
        );
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
    fn density_scales_with_chip_count_and_caps() {
        assert_eq!(BlackholeSwarmTakeover::new(&ev(1)).density, 32);
        assert_eq!(BlackholeSwarmTakeover::new(&ev(4)).density, 128);
        assert_eq!(BlackholeSwarmTakeover::new(&ev(64)).density, 1024); // capped
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = BlackholeSwarmTakeover::new(&ev(4));
        t.tick(Duration::from_secs(2));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_on_small_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(10, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = BlackholeSwarmTakeover::new(&ev(32));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }
}
