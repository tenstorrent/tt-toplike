// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Missile Command — a real, moving night-sky defense scene: one lane per
//! device, a warm-amber missile trail actually descends over real elapsed
//! time toward each actually-targeted chip's lane, then blooms into an
//! expanding, color-fading burst ring on impact. Staggered per chip (by
//! lane index) so simultaneous impacts don't all land in lockstep — loops
//! while the real reset is still in progress, freezes into a calm settled
//! ember once it's actually finished. Untargeted lanes stay idle (content
//! scoped by the real reset, never overlay size — see the design spec).

use super::{render_takeover_frame, TakeoverClock};
use crate::animation::hsv_to_rgb;
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(800);

/// Full descend-then-explode loop duration for one missile, in ms.
const CYCLE_MS: u128 = 2200;
/// Fraction of `CYCLE_MS` spent descending; the remainder is the burst.
const DESCENT_FRACTION: f32 = 0.65;
/// Per-lane phase offset (by lane index) so impacts don't all land at once.
const STAGGER_MS: u128 = 350;
/// Rows of "altitude" the missile falls through before impact.
const ALTITUDE_ROWS: usize = 7;
/// Max burst ring radius, in character cells (columns).
const MAX_BURST_RADIUS: f32 = 3.0;
/// Rows are visually taller than columns in most terminals — scale a
/// vertical offset by this before computing radial distance, so a burst
/// reads as round rather than a tall oval.
const ROW_ASPECT: f32 = 1.6;

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

    /// This lane's position in its descend/explode loop, in `[0, 1)`,
    /// offset by `lane_index` so lanes don't impact in unison.
    fn lane_phase(&self, lane_index: usize) -> f32 {
        let offset = lane_index as u128 * STAGGER_MS;
        let phase_ms = (self.clock.elapsed().as_millis() + offset) % CYCLE_MS;
        phase_ms as f32 / CYCLE_MS as f32
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let total = self.total_devices.max(1);
        let usable_width = area.width.saturating_sub(4).max(8) as usize;
        let lane_width = (usable_width / total).clamp(2, 7);
        let width = lane_width * total;
        let finished = !self.clock.in_progress();

        // `None` cells are left as plain spaces with no style override, so
        // the real screen beneath (revealed and tinted by
        // `render_takeover_frame`'s `TintOverlay`) shows through even
        // inside this canvas's empty space, not just its outer margins.
        let mut grid: Vec<Vec<Option<(char, Color)>>> = vec![vec![None; width]; ALTITUDE_ROWS];
        let impact_row = ALTITUDE_ROWS - 1;

        for lane in 0..total {
            let center_col = lane * lane_width + lane_width / 2;
            let targeted = self.device_indices.contains(&(lane as u8));
            if !targeted {
                // Idle silo marker — present, but visibly inactive.
                grid[impact_row][center_col] = Some(('·', colors::rgb(70, 80, 100)));
                continue;
            }
            if finished {
                // Settled ember: calm, cooling, no further motion.
                grid[impact_row][center_col] = Some(('+', colors::rgb(140, 70, 40)));
                continue;
            }

            let t = self.lane_phase(lane);
            if t < DESCENT_FRACTION {
                let descent_t = t / DESCENT_FRACTION;
                let head_row_f = descent_t * (impact_row as f32);
                let head_row = (head_row_f.round() as usize).min(impact_row);
                for (dist, (glyph, value)) in
                    [('▼', 1.0_f32), ('¦', 0.65), ('·', 0.35)].into_iter().enumerate()
                {
                    if dist > head_row {
                        break;
                    }
                    let row = head_row - dist;
                    grid[row][center_col] = Some((glyph, hsv_to_rgb(42.0, 0.85, value)));
                }
                // A dim reticle at the impact point previews where this
                // missile is headed, before it actually arrives.
                if head_row != impact_row {
                    grid[impact_row][center_col]
                        .get_or_insert(('x', colors::rgb(90, 70, 50)));
                }
            } else {
                let burst_t = (t - DESCENT_FRACTION) / (1.0 - DESCENT_FRACTION);
                let radius = burst_t * MAX_BURST_RADIUS;
                let value = 1.0 - burst_t * 0.75;
                let hue = 50.0 - (radius / MAX_BURST_RADIUS) * 45.0; // hot yellow -> red
                let max_r = MAX_BURST_RADIUS.ceil() as i32;
                for dy in -2i32..=0 {
                    let row_i = impact_row as i32 + dy;
                    if row_i < 0 {
                        continue;
                    }
                    let row = row_i as usize;
                    for dx in -max_r..=max_r {
                        let col_i = center_col as i32 + dx;
                        if col_i < 0 || col_i as usize >= width {
                            continue;
                        }
                        let dist =
                            ((dx * dx) as f32 + (dy as f32 * ROW_ASPECT).powi(2)).sqrt();
                        if (dist - radius).abs() >= 0.9 {
                            continue;
                        }
                        let glyph = if dist < 0.8 {
                            '*'
                        } else if dist < 1.8 {
                            '+'
                        } else {
                            '.'
                        };
                        grid[row][col_i as usize] = Some((glyph, hsv_to_rgb(hue, 0.9, value)));
                    }
                }
            }
        }

        let mut lines: Vec<Line<'static>> = vec![
            Line::from(Span::raw("INCOMING RESET")),
            Line::from(Span::raw("")),
        ];
        for row in grid {
            let spans: Vec<Span<'static>> = row
                .into_iter()
                .map(|cell| match cell {
                    Some((ch, color)) => {
                        Span::styled(ch.to_string(), Style::default().fg(color))
                    }
                    None => Span::raw(" "),
                })
                .collect();
            lines.push(Line::from(spans));
        }

        let border_color = if finished {
            colors::rgb(60, 60, 80)
        } else {
            colors::rgb(25, 30, 55) // deep night-sky steel-blue, not flat red
        };
        render_takeover_frame(f, area, "MISSILE COMMAND", border_color, lines);
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

    #[test]
    fn render_does_not_panic_on_a_tiny_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(9, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = MissileCommandTakeover::new(&ev(vec![0, 1, 2, 3], 4));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_many_chips_on_a_narrow_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let all: Vec<u8> = (0..32).collect();
        let t = MissileCommandTakeover::new(&ev(all, 32));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    /// The missile actually has to move: the head's altitude row must
    /// change as real elapsed time advances through the descent phase (not
    /// a static glyph blink like the old design).
    #[test]
    fn lane_phase_advances_with_real_elapsed_time() {
        let t = MissileCommandTakeover::new(&ev(vec![0], 1));
        let phase_at_start = t.lane_phase(0);
        let mut t = t;
        t.tick(Duration::from_millis(500));
        let phase_later = t.lane_phase(0);
        assert_ne!(
            phase_at_start, phase_later,
            "lane phase should advance with real elapsed time"
        );
    }

    /// Two lanes must not be in the same point of their cycle at the same
    /// instant — this is the "staggered, not synced" requirement.
    #[test]
    fn different_lanes_are_staggered_not_synchronized() {
        let mut t = MissileCommandTakeover::new(&ev(vec![0, 1], 2));
        t.tick(Duration::from_millis(100));
        assert_ne!(
            t.lane_phase(0),
            t.lane_phase(1),
            "lanes should be out of phase with each other"
        );
    }

    /// Once the real reset has finished, lanes must stop advancing through
    /// the descend/explode cycle — a real Missile Command shouldn't keep
    /// launching missiles after the reset it's dramatizing is already over.
    #[test]
    fn finished_state_is_a_stable_settled_frame_not_still_animating() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut t = MissileCommandTakeover::new(&ev(vec![0], 2));
        t.tick(Duration::from_millis(900));
        t.note_reset_finished();

        let render_once = |t: &MissileCommandTakeover| {
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| t.render(f, f.area())).unwrap();
            terminal.backend().buffer().clone()
        };
        let a = render_once(&t);
        t.tick(Duration::from_millis(400));
        let b = render_once(&t);
        assert_eq!(
            a, b,
            "finished takeover should render a stable settled frame, not keep animating"
        );
    }
}
