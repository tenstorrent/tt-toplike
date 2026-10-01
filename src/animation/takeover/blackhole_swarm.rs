// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! "What would 1024 Blackholes look like" — a scattered glyph swarm whose
//! density scales with the real `chip_count`, capped to what the terminal
//! can actually show. Each glyph twinkles independently over real elapsed
//! time (a fixed purple hue brightening and dimming, like this app's own
//! Starfield) rather than sitting static — positions are deterministic
//! (a real swarm's stars don't drift, they shimmer), only brightness moves.
//! Settles once the real reset has finished.

use super::{render_takeover_frame, takeover_interior, TakeoverClock};
use crate::animation::hsv_to_rgb;
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
/// Fixed hue for every glyph — a monochrome shimmer, not a rainbow.
const PURPLE_HUE: f32 = 265.0;
/// Twinkle angular rate; each glyph gets its own phase offset so they don't
/// all pulse in lockstep.
const TWINKLE_RATE: f32 = 1.6;

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

    /// Deterministic pseudo-random cell for glyph `i` within a `cols x
    /// rows` grid — a real swarm's stars don't relocate frame to frame,
    /// they just twinkle in place, so position is a pure function of the
    /// glyph's own index, not of elapsed time.
    fn glyph_pos(i: usize, cols: usize, rows: usize) -> (usize, usize) {
        let col = i.wrapping_mul(1_103_515_245).wrapping_add(12_345) / 65_536 % cols.max(1);
        let row = i.wrapping_mul(2_246_822_519).wrapping_add(3_266_489_917) / 65_536 % rows.max(1);
        (row, col)
    }

    /// Deterministic per-glyph phase offset in `[0, TAU)`, so glyphs don't
    /// all twinkle in lockstep.
    fn glyph_phase(i: usize) -> f32 {
        let h = i.wrapping_mul(2_654_435_761) % 10_000;
        (h as f32 / 10_000.0) * std::f32::consts::TAU
    }

    /// Brightness (0.35-1.0, never fully dark) for glyph `i` at the current
    /// elapsed time — frozen once the real reset has finished.
    fn glyph_brightness(&self, i: usize) -> f32 {
        if !self.clock.in_progress() {
            return 0.75;
        }
        let phase = Self::glyph_phase(i);
        let t = self.clock.elapsed().as_secs_f32() * TWINKLE_RATE + phase;
        0.35 + 0.65 * ((t.sin() + 1.0) / 2.0)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        // The swarm is laid out across the takeover box interior.
        let interior = takeover_interior(area);
        let cols = (interior.width as usize).max(1);
        let rows = (interior.height as usize).max(1);
        let cap = cols * rows;
        let filled = self.density.min(cap);

        let mut grid: Vec<Vec<Option<f32>>> = vec![vec![None; cols]; rows];
        for i in 0..filled {
            let (row, col) = Self::glyph_pos(i, cols, rows);
            let brightness = self.glyph_brightness(i);
            let cell = &mut grid[row][col];
            *cell = Some(cell.map_or(brightness, |existing| existing.max(brightness)));
        }

        let lines: Vec<Line<'static>> = grid
            .into_iter()
            .map(|row| {
                let spans: Vec<Span<'static>> = row
                    .into_iter()
                    .map(|cell| match cell {
                        Some(brightness) => Span::styled(
                            "¤",
                            Style::default().fg(hsv_to_rgb(PURPLE_HUE, 0.85, brightness)),
                        ),
                        None => Span::raw(" "),
                    })
                    .collect();
                Line::from(spans)
            })
            .collect();

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

    /// The whole point of this pass: glyphs must actually twinkle over
    /// real elapsed time, not sit at one fixed brightness forever.
    #[test]
    fn glyph_brightness_varies_with_real_elapsed_time() {
        let mut t = BlackholeSwarmTakeover::new(&ev(4));
        let b0 = t.glyph_brightness(0);
        t.tick(Duration::from_millis(400));
        let b1 = t.glyph_brightness(0);
        assert_ne!(
            b0, b1,
            "glyph brightness should vary with real elapsed time"
        );
    }

    /// Different glyphs must not all twinkle in lockstep — that would read
    /// as one flat pulse rather than an organic shimmer.
    #[test]
    fn different_glyphs_are_out_of_phase() {
        let t = BlackholeSwarmTakeover::new(&ev(4));
        assert_ne!(
            t.glyph_brightness(0),
            t.glyph_brightness(1),
            "glyphs should be out of phase with each other"
        );
    }

    /// Brightness never dips to fully dark — a shimmer, not a strobe.
    #[test]
    fn glyph_brightness_stays_within_a_visible_range() {
        let t = BlackholeSwarmTakeover::new(&ev(4));
        for i in 0..64 {
            let b = t.glyph_brightness(i);
            assert!(
                (0.35..=1.0).contains(&b),
                "brightness {b} out of range for glyph {i}"
            );
        }
    }

    /// Once finished, brightness settles and stops changing.
    #[test]
    fn glyph_brightness_settles_once_finished() {
        let mut t = BlackholeSwarmTakeover::new(&ev(4));
        t.tick(Duration::from_millis(500));
        t.note_reset_finished();
        let a = t.glyph_brightness(3);
        t.tick(Duration::from_secs(2));
        let b = t.glyph_brightness(3);
        assert_eq!(a, b, "brightness should stop changing once finished");
    }
}
