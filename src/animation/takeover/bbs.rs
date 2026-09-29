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
    /// Real targeted device indices (see `ResetEvent::device_indices`), used
    /// so each "CHIP N" line names the chip actually being reset rather than
    /// a fake index derived from loop position.
    device_indices: Vec<u8>,
}

impl BbsTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            chip_count: ev.chip_count.max(1),
            is_full: ev.is_full,
            device_indices: ev.device_indices.clone(),
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
        // Iterate the REAL targeted chip indices (not `0..visible`, which was
        // just loop position masquerading as a chip id) — a subset reset
        // (e.g. `tt-smi -r 2`) must name chip 2, not chip 0. Wording stays in
        // the present/in-progress tense ("issued", not "ack received") since
        // per-chip completion isn't actually known; the aggregate completion
        // claim ("CONNECTION RESTORED.") is made only once, below, and only
        // once the real reset has actually finished.
        for &real_chip in self.device_indices.iter().take(visible) {
            lines.push(Line::from(Span::raw(format!(
                "CHIP {real_chip}: reset issued..."
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

    /// Like `ev()`, but lets a test control the real targeted device indices
    /// independently of `chip_count` — needed to exercise a subset reset
    /// (e.g. `tt-smi -r 2`) where the targeted chip is NOT chip 0.
    fn ev_subset(chip_count: usize, device_indices: Vec<u8>) -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: false,
            chip_count,
            total_devices: 4,
            device_indices,
            raw_targets: vec![],
        }
    }

    /// Render `t` into an 80x24 `TestBackend` and flatten the buffer into
    /// plain-text rows (row-major, so a single on-screen line stays
    /// contiguous) — same pattern used in `src/ui/tui/mod.rs`'s
    /// `overlay_panel_does_not_truncate_wide_content`.
    fn rendered_text(t: &BbsTakeover) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| t.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut rows: Vec<String> = Vec::new();
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            rows.push(row);
        }
        rows.join("\n")
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

    /// Regression for Finding 2: a subset reset (`tt-smi -r 2`, targeting
    /// only chip 2 on a 4-chip box) must not panic when rendered.
    #[test]
    fn render_does_not_panic_with_subset_targets() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = BbsTakeover::new(&ev_subset(1, vec![2]));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    /// Regression for Finding 2: the rendered line must name the REAL
    /// targeted chip (2), not a fake index derived from loop position (0).
    #[test]
    fn render_labels_the_real_targeted_chip_not_loop_position() {
        let mut t = BbsTakeover::new(&ev_subset(1, vec![2]));
        // Advance well past the first beat so at least one line is visible.
        t.tick(Duration::from_millis(BEAT_MS as u64));
        let painted = rendered_text(&t);
        assert!(
            painted.contains("CHIP 2"),
            "expected the real targeted chip (2) to appear:\n{painted}"
        );
        assert!(
            !painted.contains("CHIP 0"),
            "must not fabricate CHIP 0 from loop position when chip 2 is the real target:\n{painted}"
        );
    }

    /// Regression for Finding 2: while the real reset is still in progress,
    /// the per-chip line must not claim completion ("ack received") — only
    /// the aggregate "CONNECTION RESTORED." line (gated on
    /// `!clock.in_progress()`) may claim that, and only once the reset has
    /// actually finished.
    #[test]
    fn render_does_not_claim_per_chip_completion_while_in_progress() {
        let mut t = BbsTakeover::new(&ev_subset(1, vec![2]));
        t.tick(Duration::from_millis(BEAT_MS as u64));
        assert!(t.clock.in_progress());
        let painted = rendered_text(&t);
        assert!(
            !painted.contains("ack received"),
            "must not claim completion while still in progress:\n{painted}"
        );
        assert!(
            !painted.contains("CONNECTION RESTORED"),
            "must not claim completion while still in progress:\n{painted}"
        );
        assert!(
            painted.contains("reset issued"),
            "expected honest in-progress wording:\n{painted}"
        );
    }
}
