// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! BBS sysop takeover — "the sysop wants to chat." Per-chip log lines
//! scroll in one at a time, paced by `chip_count`, looping while the real
//! reset is still in progress rather than racing ahead of it. Rendered
//! psychedelic-BBS-ANSI-art style: header, border, and each chip line cycle
//! through the hue wheel, driven by the takeover's own real elapsed time
//! (not a decorative timer independent of it) — settles to a steady green
//! once the real reset has actually finished, as a clear "back to normal"
//! signal against the churn.

use super::{render_takeover_frame, TakeoverClock};
use crate::animation::hsv_to_rgb;
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
const BEAT_MS: u128 = 400;
/// Degrees/second the header + border hue rotates while the reset is in
/// progress — a full rotation roughly every 4 seconds.
const HUE_ROTATION_DEG_PER_SEC: f32 = 90.0;
/// Hue spacing between successive chip lines, so simultaneous lines read as
/// distinct colors rather than one flat rainbow smear.
const CHIP_LINE_HUE_SPREAD_DEG: f32 = 40.0;

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

    /// Header/border hue for this frame: a steady rotation through the full
    /// hue wheel driven by real elapsed time, frozen at a fixed green once
    /// the real reset has finished (see module docs).
    fn header_hue(&self) -> f32 {
        (self.clock.elapsed().as_secs_f32() * HUE_ROTATION_DEG_PER_SEC) % 360.0
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let finished = !self.clock.in_progress();
        let header_color: Color = if finished {
            colors::rgb(0, 255, 0)
        } else {
            hsv_to_rgb(self.header_hue(), 1.0, 1.0)
        };
        let mut lines = vec![
            Line::from(Span::styled(
                "** SYSOP HAS TAKEN OVER THIS TERMINAL **",
                Style::default().fg(header_color),
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
        for (i, &real_chip) in self.device_indices.iter().take(visible).enumerate() {
            let line_color = if finished {
                colors::rgb(0, 220, 0)
            } else {
                hsv_to_rgb(
                    (self.header_hue() + i as f32 * CHIP_LINE_HUE_SPREAD_DEG) % 360.0,
                    0.9,
                    1.0,
                )
            };
            lines.push(Line::from(Span::styled(
                format!("CHIP {real_chip}: reset issued..."),
                Style::default().fg(line_color),
            )));
        }
        if finished {
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
        let border_color = if finished {
            colors::rgb(0, 220, 0)
        } else {
            hsv_to_rgb((self.header_hue() + 180.0) % 360.0, 0.8, 0.9)
        };
        render_takeover_frame(f, area, title, border_color, lines);
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

    /// Render `t` and return the foreground color of the first `'*'` cell
    /// (part of the header's `"** SYSOP ..."` banner) — used to prove the
    /// header actually changes color over real elapsed time, rather than
    /// scanning for one fixed cell position that could shift with layout.
    fn header_fg(t: &BbsTakeover) -> ratatui::style::Color {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| t.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "*" {
                    return cell.fg;
                }
            }
        }
        panic!("header '*' not found in rendered buffer");
    }

    /// Render `t` and return the foreground color of `needle`'s own first
    /// character cell — NOT just "the first non-space cell on that row",
    /// since the row's leftmost cell is the border glyph (styled with the
    /// border color, not the line's own text color). Every symbol here is
    /// single-width ASCII, so a character offset into the flattened row
    /// string maps 1:1 to an x coordinate.
    fn line_fg_containing(t: &BbsTakeover, needle: &str) -> ratatui::style::Color {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| t.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if let Some(byte_offset) = row.find(needle) {
                let x = row[..byte_offset].chars().count() as u16;
                return buf[(x, y)].fg;
            }
        }
        panic!("row containing {needle:?} not found in rendered buffer");
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

    /// The header must actually cycle color over real elapsed time (not sit
    /// on one static color) — this is the "psychedelic" requirement, and it
    /// must be driven by the takeover's own real clock, not a decorative
    /// timer independent of it.
    #[test]
    fn header_color_cycles_with_real_elapsed_time() {
        let mut t = BbsTakeover::new(&ev(true, 4));
        let color_at_start = header_fg(&t);
        t.tick(Duration::from_millis(2000)); // 2s * 90deg/s = 180deg — opposite hue
        let color_later = header_fg(&t);
        assert_ne!(
            color_at_start, color_later,
            "header color should have visibly changed after 2s of real elapsed time"
        );
    }

    /// Two simultaneously-visible chip lines must get distinct colors (a
    /// hue-spread rainbow line-up), not all render the same flat color.
    #[test]
    fn simultaneous_chip_lines_get_distinct_colors() {
        let mut t = BbsTakeover::new(&ev_subset(2, vec![0, 1]));
        // beat = 450ms / 400ms = 1 → (1 % 2) + 1 = 2 lines visible.
        t.tick(Duration::from_millis(450));
        assert_eq!(t.visible_lines(), 2, "test setup: expected both lines visible");
        let color_chip_0 = line_fg_containing(&t, "CHIP 0");
        let color_chip_1 = line_fg_containing(&t, "CHIP 1");
        assert_ne!(
            color_chip_0, color_chip_1,
            "simultaneously-visible chip lines should have distinct colors"
        );
    }

    /// Once the real reset finishes, the takeover settles into a fixed green
    /// (both header and border) rather than continuing to cycle — a clear
    /// "back to normal" signal distinct from the in-progress churn.
    #[test]
    fn settles_to_fixed_green_once_finished() {
        let mut t = BbsTakeover::new(&ev(true, 4));
        t.tick(Duration::from_millis(1300)); // arbitrary mid-cycle elapsed time
        t.note_reset_finished();
        let color_a = header_fg(&t);
        t.tick(Duration::from_millis(900)); // more time passes post-finish
        let color_b = header_fg(&t);
        assert_eq!(
            color_a, color_b,
            "header color must stop cycling once the real reset has finished"
        );
        assert_eq!(
            color_a,
            colors::rgb(0, 255, 0),
            "finished header should settle to the fixed 'connection restored' green"
        );
    }
}
