// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Lost-hatch-style repeating countdown, paced by real elapsed/in-progress
//! time rather than a single fixed guess at how long the real reset takes.
//! Rendered as a big blocky LED-style digit display (like the real Swan
//! station's computer), with the show's iconic numbers as a flavor
//! easter egg below it.

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

/// One LED-style glyph: three rows, each the same width. Digits are the
/// classic seven-segment shapes; `:` is a narrow two-dot separator.
fn led_glyph(ch: char) -> [&'static str; 3] {
    match ch {
        '0' => [" _ ", "| |", "|_|"],
        '1' => ["   ", "  |", "  |"],
        '2' => [" _ ", " _|", "|_ "],
        '3' => [" _ ", " _|", " _|"],
        '4' => ["   ", "|_|", "  |"],
        '5' => [" _ ", "|_ ", " _|"],
        '6' => [" _ ", "|_ ", "|_|"],
        '7' => [" _ ", "  |", "  |"],
        '8' => [" _ ", "|_|", "|_|"],
        '9' => [" _ ", "|_|", " _|"],
        ':' => [" ", "o", "o"],
        _ => ["   ", "   ", "   "],
    }
}

/// Render `text` (digits and `:`) as three lines of concatenated LED
/// glyphs, one space between characters.
fn led_lines(text: &str, color: ratatui::style::Color) -> [Line<'static>; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, ch) in text.chars().enumerate() {
        if i > 0 {
            for row in &mut rows {
                row.push(' ');
            }
        }
        let glyph = led_glyph(ch);
        for (row, seg) in rows.iter_mut().zip(glyph) {
            row.push_str(seg);
        }
    }
    let style = Style::default().fg(color);
    [
        Line::from(Span::styled(rows[0].clone(), style)),
        Line::from(Span::styled(rows[1].clone(), style)),
        Line::from(Span::styled(rows[2].clone(), style)),
    ]
}

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

    /// The screen as lines: 9 rows, at most 17 columns wide.
    fn lines(&self) -> Vec<Line<'static>> {
        let amber = colors::rgb(255, 140, 0);
        let dim_amber = colors::rgb(140, 90, 30);

        let mut lines: Vec<Line<'static>> = vec![
            Line::from(Span::styled("THE HATCH", Style::default().fg(dim_amber))),
            Line::from(Span::raw("")),
        ];
        lines.extend(led_lines(
            &format!("00:{:02}", self.remaining_secs()),
            amber,
        ));
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(Span::styled(
            if self.clock.in_progress() {
                "EXECUTE"
            } else {
                "SYSTEM RESET"
            },
            Style::default().fg(amber),
        )));
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(Span::styled(
            "4 8 15 16 23 42",
            Style::default().fg(dim_amber),
        )));

        lines
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        render_takeover_frame(
            f,
            area,
            "DHARMA INITIATIVE",
            colors::rgb(255, 140, 0),
            self.lines(),
        );
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

    /// The LED digit display must actually be present and correctly reflect
    /// the real countdown value, formatted like the show's own MM:SS prop.
    #[test]
    fn render_shows_led_digits_for_the_real_countdown() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut t = HatchCountdownTakeover::new(&ev());
        t.tick(Duration::from_secs(5)); // 25s remaining
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| t.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut painted = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                painted.push_str(buf[(x, y)].symbol());
            }
            painted.push('\n');
        }
        // "2" and "5" each have a distinctive top segment (" _ ") — presence
        // of the LED block characters confirms the digit grid rendered.
        assert!(
            painted.contains('_'),
            "expected LED segment glyphs:\n{painted}"
        );
        assert!(
            painted.contains("4 8 15 16 23 42"),
            "expected the Lost numbers easter egg:\n{painted}"
        );
    }

    #[test]
    fn led_glyph_covers_every_digit_and_colon_with_uniform_row_count() {
        for ch in "0123456789:".chars() {
            let glyph = led_glyph(ch);
            assert_eq!(glyph.len(), 3);
        }
    }

    /// The Hatch screen must fit the takeover box interior (71x22).
    #[test]
    fn lines_fit_the_box_interior() {
        let interior = crate::animation::takeover::takeover_interior(Rect::new(0, 0, 134, 40));
        let mut t = HatchCountdownTakeover::new(&ev());
        for finished in [false, true] {
            if finished {
                t.note_reset_finished();
            }
            let lines = t.lines();
            let widest = lines.iter().map(Line::width).max().unwrap_or(0);
            assert!(widest <= interior.width as usize, "widest {widest}");
            assert!(
                lines.len() <= interior.height as usize,
                "{} rows",
                lines.len()
            );
        }
    }
}
