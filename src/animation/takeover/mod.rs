// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Full-screen "reset takeover" animations, triggered by
//! `crate::workload::reset_detect` when someone else runs `tt-smi -r`. Each
//! variant is a concrete struct (not a trait object — matching this
//! codebase's `DisplayMode`/`EventKind` convention of enum + match rather
//! than `dyn Trait`), driven by the *real* reset lifecycle via
//! [`TakeoverClock`]: `note_reset_finished` is called the tick the real
//! process exits, and a variant is only done once both the real reset has
//! finished AND its own short "done" resolution beat has played — never a
//! fixed fake timer running independent of the real reset.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use std::time::Duration;

/// Shared lifecycle bookkeeping every takeover variant embeds.
pub(crate) struct TakeoverClock {
    elapsed: Duration,
    finished_at: Option<Duration>,
    skipped: bool,
}

impl TakeoverClock {
    pub(crate) fn new() -> Self {
        Self {
            elapsed: Duration::ZERO,
            finished_at: None,
            skipped: false,
        }
    }

    pub(crate) fn tick(&mut self, dt: Duration) {
        self.elapsed += dt;
    }

    /// Call the tick `ResetDetector::is_finished()` first goes true.
    /// Idempotent — a later call does not push the recorded time forward.
    pub(crate) fn note_reset_finished(&mut self) {
        if self.finished_at.is_none() {
            self.finished_at = Some(self.elapsed);
        }
    }

    pub(crate) fn skip(&mut self) {
        self.skipped = true;
    }

    /// True once skipped, or once the real reset has finished AND
    /// `done_tail` has elapsed since then.
    pub(crate) fn is_done(&self, done_tail: Duration) -> bool {
        self.skipped
            || self
                .finished_at
                .map(|f| self.elapsed.saturating_sub(f) >= done_tail)
                .unwrap_or(false)
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// True until the real reset process has been observed to finish.
    pub(crate) fn in_progress(&self) -> bool {
        self.finished_at.is_none()
    }
}

/// Shared full-screen frame: clears the terminal cells, paints a bordered
/// block (left/bottom borders only, per this project's no-right-border-glyph
/// convention) with `title`, and renders `lines` as a centered paragraph.
/// Every variant's `render` calls this so a takeover always reads as one
/// consistent "something big just happened" moment. No-ops on a terminal too
/// small to safely draw into (matches `render_overlay_panel`'s guard).
pub(crate) fn render_takeover_frame(
    f: &mut Frame,
    area: Rect,
    title: &str,
    border_color: Color,
    lines: Vec<Line<'static>>,
) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::LEFT | Borders::BOTTOM)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let para = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_not_done_while_in_progress() {
        let mut c = TakeoverClock::new();
        c.tick(Duration::from_secs(10));
        assert!(!c.is_done(Duration::from_millis(500)));
        assert!(c.in_progress());
    }

    #[test]
    fn clock_done_after_finish_plus_tail() {
        let mut c = TakeoverClock::new();
        c.tick(Duration::from_millis(100));
        c.note_reset_finished();
        assert!(!c.in_progress());
        assert!(!c.is_done(Duration::from_millis(500))); // tail hasn't elapsed yet
        c.tick(Duration::from_millis(500));
        assert!(c.is_done(Duration::from_millis(500)));
    }

    #[test]
    fn clock_skip_is_immediately_done() {
        let mut c = TakeoverClock::new();
        c.skip();
        assert!(c.is_done(Duration::from_secs(999)));
    }

    #[test]
    fn second_note_reset_finished_does_not_reset_the_tail() {
        let mut c = TakeoverClock::new();
        c.tick(Duration::from_millis(100));
        c.note_reset_finished();
        c.tick(Duration::from_millis(300));
        c.note_reset_finished(); // should be a no-op
        c.tick(Duration::from_millis(300));
        // total elapsed since first finish = 600ms >= 500ms tail
        assert!(c.is_done(Duration::from_millis(500)));
    }

    #[test]
    fn render_takeover_frame_does_not_panic_on_tiny_area() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(3, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_takeover_frame(f, f.area(), "X", Color::White, vec![Line::raw("hi")]);
            })
            .unwrap();
    }

    #[test]
    fn render_takeover_frame_does_not_panic_on_normal_area() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_takeover_frame(
                    f,
                    f.area(),
                    "RESET",
                    Color::Red,
                    vec![Line::raw("line one"), Line::raw("line two")],
                );
            })
            .unwrap();
    }
}
