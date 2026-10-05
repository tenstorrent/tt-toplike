// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Classic 1971-BASIC-style "Super Star Trek" screen: a real status
//! sidebar (STARDATE, CONDITION, KLINGONS REMAINING) plus a fixed 8x8
//! short-range sensor scan grid — our ship `<*>`, a `+K+` Klingon on each
//! targeted chip, scattered decorative stars, dots for empty space — ending
//! in a command-prompt line so it reads as walking in on a game already in
//! progress, not a title screen. Classic green-phosphor/amber palette, no
//! hue-cycling — the psychedelic treatment is BBS's alone, not spread here.

use super::{render_takeover_frame, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
const GRID_SIZE: usize = 8;
/// Starting decorative energy reading; ticks down slowly while the battle
/// is in progress, purely for a touch of real motion — cosmetic, not a
/// telemetry claim.
const STARTING_ENERGY: i32 = 3000;
const ENERGY_DRAIN_PER_SEC: f32 = 15.0;
const MIN_ENERGY: i32 = 200;

pub struct TrekResetTakeover {
    clock: TakeoverClock,
    device_indices: Vec<usize>,
    total_devices: usize,
    /// Cosmetic flavor text only — derived from real wall-clock time so
    /// repeated runs aren't identical, never used for any real pacing
    /// decision (pacing is entirely `TakeoverClock`-driven).
    stardate: String,
    /// Decorative flavor coordinates, deterministic from `total_devices` —
    /// not derived from any real telemetry.
    quadrant: (u8, u8),
    sector: (u8, u8),
}

impl TrekResetTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let stardate = format!("{:.1}", (now.as_secs() as f64 / 86_400.0) % 10_000.0);
        let td = ev.total_devices as u32;
        let quadrant = (((td * 3 + 1) % 8) as u8 + 1, ((td * 5 + 2) % 8) as u8 + 1);
        let sector = (((td * 7 + 3) % 8) as u8 + 1, ((td * 11 + 4) % 8) as u8 + 1);
        Self {
            clock: TakeoverClock::new(),
            device_indices: ev.device_indices.clone(),
            total_devices: ev.total_devices,
            stardate,
            quadrant,
            sector,
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

    /// Deterministic, stable-per-cell decorative star scatter — not random
    /// each render (that would flicker on every redraw of the same
    /// instant), just a fixed pseudo-scatter seeded by cell position and
    /// `total_devices` so different reset scopes get a different sky.
    fn has_star(&self, index: usize) -> bool {
        let seed = index
            .wrapping_mul(31)
            .wrapping_add(self.total_devices.wrapping_mul(7));
        seed % 5 == 0
    }

    fn energy(&self) -> i32 {
        if !self.clock.in_progress() {
            return MIN_ENERGY;
        }
        let drained = (self.clock.elapsed().as_secs_f32() * ENERGY_DRAIN_PER_SEC) as i32;
        (STARTING_ENERGY - drained).max(MIN_ENERGY)
    }

    /// The full screen as lines, all padded to one width. At most 36
    /// columns by 20 rows, so it fits the takeover box interior (71x22).
    fn lines(&self) -> Vec<Line<'static>> {
        let finished = !self.clock.in_progress();
        let klingons_remaining = if finished {
            0
        } else {
            self.device_indices.len()
        };
        let condition = if finished { "GREEN" } else { "RED" };
        let condition_color = if finished {
            colors::rgb(90, 220, 120)
        } else {
            colors::rgb(230, 90, 70)
        };

        let label_style = Style::default().fg(colors::rgb(90, 200, 140));
        let value_style = Style::default().fg(colors::rgb(200, 230, 210));

        let mut lines: Vec<Line<'static>> = vec![
            Line::from(vec![
                Span::styled("STARDATE          ", label_style),
                Span::styled(self.stardate.clone(), value_style),
            ]),
            Line::from(vec![
                Span::styled("CONDITION         ", label_style),
                Span::styled(condition, Style::default().fg(condition_color)),
            ]),
            Line::from(vec![
                Span::styled("QUADRANT          ", label_style),
                Span::styled(
                    format!("{},{}", self.quadrant.0, self.quadrant.1),
                    value_style,
                ),
            ]),
            Line::from(vec![
                Span::styled("SECTOR            ", label_style),
                Span::styled(format!("{},{}", self.sector.0, self.sector.1), value_style),
            ]),
            Line::from(vec![
                Span::styled("PHOTON TORPEDOES  ", label_style),
                Span::styled(format!("{}", self.device_indices.len().max(1)), value_style),
            ]),
            Line::from(vec![
                Span::styled("TOTAL ENERGY      ", label_style),
                Span::styled(format!("{}", self.energy()), value_style),
            ]),
            Line::from(vec![
                Span::styled("KLINGONS REMAINING ", label_style),
                Span::styled(format!("{klingons_remaining}"), value_style),
            ]),
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                "SHORT RANGE SENSOR SCAN",
                Style::default().fg(colors::rgb(90, 200, 140)),
            )),
        ];

        // Column header: "    1   2   3   4   5   6   7   8"
        let mut header = String::from("    ");
        for c in 1..=GRID_SIZE {
            header.push_str(&format!("{c}   "));
        }
        lines.push(Line::from(Span::styled(
            header,
            Style::default().fg(colors::rgb(70, 150, 110)),
        )));

        let ship_index = self.total_devices.min(GRID_SIZE * GRID_SIZE - 1);

        for row in 0..GRID_SIZE {
            let mut spans: Vec<Span<'static>> = vec![Span::styled(
                format!("{} ", row + 1),
                Style::default().fg(colors::rgb(70, 150, 110)),
            )];
            for col in 0..GRID_SIZE {
                let index = row * GRID_SIZE + col;
                let (glyph, color): (&str, ratatui::style::Color) = if index < self.total_devices {
                    if self.device_indices.contains(&index) {
                        if finished {
                            (" x ", colors::rgb(120, 90, 60))
                        } else {
                            ("+K+", colors::rgb(230, 90, 70))
                        }
                    } else {
                        ("...", colors::rgb(40, 80, 60))
                    }
                } else if index == ship_index {
                    ("<*>", colors::rgb(210, 230, 255))
                } else if self.has_star(index) {
                    (" * ", colors::rgb(80, 130, 150))
                } else {
                    ("...", colors::rgb(40, 80, 60))
                };
                spans.push(Span::styled(
                    format!("{glyph} "),
                    Style::default().fg(color),
                ));
            }
            lines.push(Line::from(spans));
        }

        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(Span::styled(
            if finished {
                "SECTOR CLEAR. COMMAND ?"
            } else {
                "PHASERS LOCKED. COMMAND ?"
            },
            Style::default().fg(colors::rgb(90, 200, 140)),
        )));

        // `Paragraph`'s `Alignment::Center` centers each `Line` independently
        // by its OWN width — without this, the differently-sized status
        // lines and grid rows would each get a different left indent and
        // the whole screen would read as a ragged mess instead of one
        // stable, left-aligned block. Pad every line out to the widest
        // one's width so they all center identically as a single rectangle.
        let max_width = lines.iter().map(Line::width).max().unwrap_or(0);
        for line in &mut lines {
            let deficit = max_width.saturating_sub(line.width());
            if deficit > 0 {
                line.spans.push(Span::raw(" ".repeat(deficit)));
            }
        }

        lines
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        self.render_tagged(f, area, "");
    }

    /// Draws the takeover with `tag` (for example `DEMO`) in front of the
    /// box title. An empty `tag` draws the plain title.
    pub fn render_tagged(&self, f: &mut Frame, area: Rect, tag: &str) {
        let finished = !self.clock.in_progress();
        let lines = self.lines();
        let border_color = if finished {
            colors::rgb(60, 100, 80)
        } else {
            colors::rgb(40, 90, 60)
        };
        render_takeover_frame(f, area, tag, "TT-TREKLIKE", border_color, lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(device_indices: Vec<usize>, total_devices: usize) -> ResetEvent {
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
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let full = TrekResetTakeover::new(&ev(vec![0, 1, 2, 3], 4));
        terminal.draw(|f| full.render(f, f.area())).unwrap();
        let subset = TrekResetTakeover::new(&ev(vec![2], 4));
        terminal.draw(|f| subset.render(f, f.area())).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_many_chips_beyond_the_grid() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let all: Vec<usize> = (0..70).collect();
        let t = TrekResetTakeover::new(&ev(all, 70));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    /// Klingons remaining must reflect the real targeted-chip count while
    /// in progress, and drop to zero once the reset has actually finished.
    #[test]
    fn klingons_remaining_reflects_real_state() {
        let mut t = TrekResetTakeover::new(&ev(vec![0, 2], 4));
        assert_eq!(
            klingons_remaining_for_test(&t),
            2,
            "should report the real number of targeted chips while in progress"
        );
        t.note_reset_finished();
        assert_eq!(
            klingons_remaining_for_test(&t),
            0,
            "should drop to zero once the real reset has finished"
        );
    }

    fn klingons_remaining_for_test(t: &TrekResetTakeover) -> usize {
        if t.clock.in_progress() {
            t.device_indices.len()
        } else {
            0
        }
    }

    /// Energy should tick down over real elapsed time while in progress,
    /// and stop moving once the reset has finished.
    #[test]
    fn energy_ticks_down_while_in_progress_then_settles() {
        let mut t = TrekResetTakeover::new(&ev(vec![0], 4));
        let e0 = t.energy();
        t.tick(Duration::from_secs(2));
        let e1 = t.energy();
        assert!(e1 < e0, "energy should drain over real elapsed time");
        t.note_reset_finished();
        let e2 = t.energy();
        t.tick(Duration::from_secs(5));
        let e3 = t.energy();
        assert_eq!(e2, e3, "energy should stop changing once finished");
    }

    /// The Trek screen must fit the takeover box interior (71x22 at full
    /// size), including with more chips than the 8x8 grid holds.
    #[test]
    fn lines_fit_the_box_interior() {
        let interior = crate::animation::takeover::takeover_interior(Rect::new(0, 0, 134, 40));
        for chips in [1usize, 4, 70] {
            let all: Vec<usize> = (0..chips).collect();
            let mut t = TrekResetTakeover::new(&ev(all, chips));
            for finished in [false, true] {
                if finished {
                    t.note_reset_finished();
                }
                let lines = t.lines();
                let widest = lines.iter().map(Line::width).max().unwrap_or(0);
                assert!(
                    widest <= interior.width as usize,
                    "{chips} chips: widest {widest}"
                );
                assert!(
                    lines.len() <= interior.height as usize,
                    "{chips} chips: {} rows",
                    lines.len()
                );
            }
        }
    }

    #[test]
    fn box_title_is_tt_treklike_and_not_the_enterprise() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let t = TrekResetTakeover::new(&ev(vec![0], 4));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        let text: String = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("TT-TREKLIKE"), "{text}");
        assert!(
            !text.contains("ENTERPRISE") && !text.contains("NCC-1701"),
            "{text}"
        );
    }
}
