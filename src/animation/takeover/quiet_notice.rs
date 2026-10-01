// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Minimal status readout (a backdrop sized to the takeover box) — "the tool doing its thing." Still
//! the deliberately low-key option (no box art competing for attention,
//! no hue-cycling), but dressed in a field of shaded ANSI blocks — the
//! same block-character/value vocabulary this app already uses for its
//! Greyskull castle theme — twinkling gently over real elapsed time, with
//! the status message legible on top. Rendered through
//! [`crate::animation::hsv_to_grayskull`] unconditionally (calm greys plus
//! a cyan/purple tint, regardless of the app's active theme), the same way
//! the loading snake stays calm and monochrome rather than a neon sweep —
//! fitting for the one variant that's meant to read as quiet.

use super::{render_takeover_frame, takeover_interior, TakeoverClock};
use crate::animation::{hsv_to_grayskull, value_to_char_intensity, BLOCK_CHARS};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(600);
const CYAN_HUE: f32 = 185.0;
const PURPLE_HUE: f32 = 280.0;
/// Slow and small — a gentle shimmer, not the livelier twinkle Blackhole
/// Swarm uses; this is the quiet variant. The *motion* stays gentle; the
/// *texture* (below) is where the dithered look comes from.
const TWINKLE_RATE: f32 = 0.7;
const TWINKLE_AMPLITUDE: f32 = 0.14;
const BASE_VALUE: f32 = 0.45;
/// How strongly the ordered-dither offset perturbs each cell's brightness
/// before it's mapped to a glyph. This is what actually produces the
/// dithered look — real dithering is a structured per-cell offset, not a
/// smooth gradient, so nearby cells land in visibly different glyph bands
/// (sparse `·` right next to a bold `▓`) even though the underlying
/// twinkle value barely moves.
const DITHER_STRENGTH: f32 = 0.55;

/// Classic 4x4 ordered (Bayer) dither matrix, values 0-15.
const BAYER_4X4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Per-cell dither offset in roughly `[-0.5, 0.47]`, tiled every 4 cells in
/// each direction — a fixed, structured pattern (not random, not
/// time-varying), the same way real ordered dithering works.
fn dither_offset(row: usize, col: usize) -> f32 {
    let v = BAYER_4X4[row % 4][col % 4] as f32 / 16.0;
    v - 0.5
}

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

    /// Deterministic per-cell phase offset in `[0, TAU)`, so cells don't
    /// all shimmer in lockstep.
    fn cell_phase(seed: usize) -> f32 {
        let h = seed.wrapping_mul(2_654_435_761) % 10_000;
        (h as f32 / 10_000.0) * std::f32::consts::TAU
    }

    /// Deterministic per-cell hue: mostly the cyan band, a sparser purple
    /// accent, both real bands `hsv_to_grayskull` actually tints (see its
    /// own doc comment) — a plain hue outside those bands would just come
    /// back grey anyway.
    fn cell_hue(seed: usize) -> f32 {
        if seed % 5 == 0 {
            PURPLE_HUE
        } else {
            CYAN_HUE
        }
    }

    /// Brightness for cell `seed` at the current elapsed time — frozen
    /// once the real reset has finished (settles, doesn't keep shimmering
    /// after the fact).
    fn cell_value(&self, seed: usize) -> f32 {
        if !self.clock.in_progress() {
            return BASE_VALUE;
        }
        let t = self.clock.elapsed().as_secs_f32() * TWINKLE_RATE + Self::cell_phase(seed);
        (BASE_VALUE + TWINKLE_AMPLITUDE * t.sin()).clamp(0.1, 0.8)
    }

    fn background_cell(&self, row: usize, col: usize, seed: usize) -> (char, Color) {
        let twinkle_value = self.cell_value(seed);
        let dithered_value =
            (twinkle_value + dither_offset(row, col) * DITHER_STRENGTH).clamp(0.05, 0.95);
        let ch = value_to_char_intensity(dithered_value, &BLOCK_CHARS);
        let color = hsv_to_grayskull(Self::cell_hue(seed), 0.6, dithered_value);
        (ch, color)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let finished = !self.clock.in_progress();
        // Compose the backdrop for the box interior (71x23 at full size),
        // not for the whole screen. The floors keep the message-row math
        // valid on a terminal too small to draw (the frame draws nothing
        // there).
        let interior = takeover_interior(area);
        let cols = (interior.width as usize).max(8);
        let rows = (interior.height as usize).max(3);

        let scope = if self.is_full {
            "all chips".to_string()
        } else {
            format!("{} chip(s)", self.chip_count)
        };
        let status = if finished {
            "reset complete"
        } else {
            "resetting…"
        };
        // A one-space gap on each side keeps the text from butting directly
        // against the block texture — a small but real legibility win.
        let msg1: Vec<char> = format!(" tt-smi -r — {scope} ").chars().collect();
        let msg2: Vec<char> = format!(" · {status} ").chars().collect();
        let text_color = if finished {
            colors::rgb(210, 235, 225)
        } else {
            colors::rgb(225, 230, 235)
        };

        let msg1_row = rows / 2 - 1;
        let msg2_row = rows / 2;
        let msg1_start = cols.saturating_sub(msg1.len()) / 2;
        let msg2_start = cols.saturating_sub(msg2.len()) / 2;

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows);
        for row in 0..rows {
            let (message, msg_start) = if row == msg1_row {
                (Some(&msg1), msg1_start)
            } else if row == msg2_row {
                (Some(&msg2), msg2_start)
            } else {
                (None, 0)
            };
            let mut spans = Vec::with_capacity(cols);
            for col in 0..cols {
                if let Some(msg) = message {
                    if col >= msg_start && col - msg_start < msg.len() {
                        let ch = msg[col - msg_start];
                        spans.push(Span::styled(
                            ch.to_string(),
                            Style::default().fg(text_color),
                        ));
                        continue;
                    }
                }
                let seed = row * cols + col;
                let (ch, color) = self.background_cell(row, col, seed);
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            }
            lines.push(Line::from(spans));
        }

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

    #[test]
    fn render_does_not_panic_on_a_tiny_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(9, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = QuietNoticeTakeover::new(&ev(true, 4));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    /// The background must actually shimmer over real elapsed time — the
    /// whole point of this pass.
    #[test]
    fn background_cell_value_varies_with_real_elapsed_time() {
        let mut t = QuietNoticeTakeover::new(&ev(true, 4));
        let v0 = t.cell_value(7);
        t.tick(Duration::from_millis(600));
        let v1 = t.cell_value(7);
        assert_ne!(
            v0, v1,
            "background cell brightness should vary with real elapsed time"
        );
    }

    /// Different cells must be out of phase — a gentle organic shimmer,
    /// not one flat pulse across the whole screen.
    #[test]
    fn different_cells_are_out_of_phase() {
        let t = QuietNoticeTakeover::new(&ev(true, 4));
        assert_ne!(
            t.cell_value(0),
            t.cell_value(1),
            "cells should be out of phase with each other"
        );
    }

    /// The whole point of this pass: the ordered dither must actually
    /// spread nearby cells across MULTIPLE glyph classes (a real dithered
    /// look), not leave them all landing on the same one or two characters
    /// near the middle of the ramp.
    #[test]
    fn dither_spreads_glyphs_across_the_full_ramp() {
        let t = QuietNoticeTakeover::new(&ev(true, 4));
        let mut seen = std::collections::HashSet::new();
        for row in 0..4 {
            for col in 0..8 {
                let (ch, _) = t.background_cell(row, col, row * 8 + col);
                seen.insert(ch);
            }
        }
        assert!(
            seen.len() >= 3,
            "expected the dither to span at least 3 distinct glyph classes over a 4x8 patch, got {seen:?}"
        );
    }

    /// Once finished, the background settles and stops shimmering.
    #[test]
    fn background_settles_once_finished() {
        let mut t = QuietNoticeTakeover::new(&ev(true, 4));
        t.tick(Duration::from_millis(400));
        t.note_reset_finished();
        let a = t.cell_value(3);
        t.tick(Duration::from_secs(2));
        let b = t.cell_value(3);
        assert_eq!(a, b, "background should stop changing once finished");
    }

    /// The status message must still read clearly — present, on screen —
    /// over the busy background.
    #[test]
    fn status_message_is_present_over_the_background() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let t = QuietNoticeTakeover::new(&ev(true, 4));
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
        assert!(
            painted.contains("tt-smi -r — all chips"),
            "expected the status message over the background:\n{painted}"
        );
    }
}
