// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Silly Cetacean — a flock of little birds gently carries a whale down
//! through a drifting sky, bobbing and wing-flapping while the real reset is
//! still in progress (never faking a landing time we don't know), then easing
//! into a soft, happy touchdown on the water once the reset has actually
//! finished. Bird count scales with the real chip count — a bigger reset
//! needs more birds to carry.
//!
//! The scene is composed on a character canvas the size of the box interior:
//! a sun, three clouds drifting at different speeds and a shimmering sea are
//! drawn first, then the whale, ropes and birds on top. Every background
//! position is a pure function of the takeover's own elapsed time, so
//! repeated renders of the same instant match and nothing reads a clock.

use super::{render_takeover_frame, takeover_interior, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(1200);
/// How long the final descent-to-ground glide takes once the real reset
/// has finished — kept at/under `DONE_TAIL` so it completes right as the
/// takeover itself is allowed to end.
const LANDING_SECS: f32 = 1.0;
/// Gentle hover bob while airborne: still slow, but wide enough to
/// actually cross row boundaries in a character grid (a sub-row amplitude
/// would compute a "moving" position that renders as a frozen scene, since
/// terminal cells only have integer rows).
const BOB_AMPLITUDE_ROWS: f32 = 1.6;
const BOB_RATE: f32 = 1.1;
const FLAP_MS: u128 = 350;

const WHALE_BODY: [&str; 7] = [
    "        .    .         ",
    "         \\  /          ",
    "     .--=======--.     ",
    "   ,'             `.   ",
    "  /        o        \\  ",
    "  \\                 ,'>",
    "   `--...______...--'  ",
];
const WHALE_WIDTH: usize = 24;

pub struct SillyCetaceanTakeover {
    clock: TakeoverClock,
    bird_count: usize,
    /// Snapshot of `clock.elapsed()` the instant the real reset finished —
    /// tracked locally (rather than adding this to the shared
    /// `TakeoverClock`) so the landing glide can be paced by real
    /// time-since-finished without new shared-framework surface.
    finished_at: Option<Duration>,
}

impl SillyCetaceanTakeover {
    pub fn new(ev: &ResetEvent) -> Self {
        Self {
            clock: TakeoverClock::new(),
            bird_count: ev.chip_count.clamp(1, 5),
            finished_at: None,
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        self.clock.tick(dt);
    }

    pub fn note_reset_finished(&mut self) {
        if self.finished_at.is_none() {
            self.finished_at = Some(self.clock.elapsed());
        }
        self.clock.note_reset_finished();
    }

    pub fn skip(&mut self) {
        self.clock.skip();
    }

    pub fn is_done(&self) -> bool {
        self.clock.is_done(DONE_TAIL)
    }

    /// `0.0` = still airborne, `1.0` = fully landed and settled. Only
    /// starts moving once the real reset has actually finished.
    fn landing_progress(&self) -> f32 {
        match self.finished_at {
            None => 0.0,
            Some(f) => {
                let since = self.clock.elapsed().saturating_sub(f).as_secs_f32();
                (since / LANDING_SECS).clamp(0.0, 1.0)
            }
        }
    }

    fn wings_flap_open(&self) -> bool {
        (self.clock.elapsed().as_millis() / FLAP_MS) % 2 == 0
    }

    /// The scene's vertical position (in rows from the top of the box
    /// interior) at the current elapsed time — a pure function of state, kept
    /// separate from `render` so the real motion it produces is directly
    /// testable without depending on exactly when a floating-point bob
    /// happens to cross an integer row boundary in a rendered snapshot.
    fn scene_top(&self, interior_height: u16) -> f32 {
        let progress = self.landing_progress();
        let landed = progress >= 1.0;
        // Quadratic ease-out: fast at first, gentle as it settles.
        let eased = 1.0 - (1.0 - progress) * (1.0 - progress);
        let bob = if landed {
            0.0
        } else {
            BOB_AMPLITUDE_ROWS * (self.clock.elapsed().as_secs_f32() * BOB_RATE).sin()
        };
        let available_rows = interior_height.saturating_sub(6).max(12) as f32;
        let hover_row = available_rows * 0.18;
        // Leave room below `ground_row` for the whale body itself plus the
        // rope/bird (or flight/ground) row that follows it — otherwise a
        // "landed" position computed from the bottom of the area alone
        // pushes the whale's own lower half and the ground line straight
        // off the bottom of the screen.
        let scene_height = WHALE_BODY.len() as f32 + 2.0;
        let ground_row = (available_rows - scene_height).max(hover_row);
        hover_row + (ground_row - hover_row) * eased + bob
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        self.render_tagged(f, area, "");
    }

    /// Draws the takeover with `tag` (for example `DEMO`) in front of the
    /// box title. An empty `tag` draws the plain title.
    pub fn render_tagged(&self, f: &mut Frame, area: Rect, tag: &str) {
        let progress = self.landing_progress();
        let landed = progress >= 1.0;

        let interior = takeover_interior(area);
        let (w, h) = (interior.width as usize, interior.height as usize);
        let mut canvas = Canvas::new(w, h);
        self.draw_sky(&mut canvas);
        self.draw_whale_scene(&mut canvas, interior.height, landed);

        let title = if landed {
            "SILLY CETACEAN - TOUCHDOWN"
        } else {
            "SILLY CETACEAN"
        };
        let tint = if landed {
            colors::rgb(120, 165, 90)
        } else {
            colors::rgb(90, 130, 180)
        };
        render_takeover_frame(f, area, tag, title, tint, canvas.into_lines());
    }

    /// Sun, drifting clouds and sea: the background the whale is drawn over.
    fn draw_sky(&self, canvas: &mut Canvas) {
        let t = self.clock.elapsed().as_secs_f32();

        // Sun, top right. Its rays alternate between two shapes.
        let sun_color = colors::rgb(240, 200, 90);
        let rays_a = [r"\ | /", "- O -", r"/ | \"];
        let rays_b = [" .|. ", "-(O)-", " '|' "];
        let rays = if (self.clock.elapsed().as_millis() / SUN_FLICKER_MS) % 2 == 0 {
            rays_a
        } else {
            rays_b
        };
        let sun_x = canvas.width as i32 - 8;
        for (dy, row) in rays.iter().enumerate() {
            canvas.draw(sun_x, 1 + dy as i32, row, sun_color, true);
        }

        // Clouds drift right at different speeds (a parallax cue), wrapping
        // around the canvas edge. Three rows apart, none under the sun.
        let cloud_color = colors::rgb(95, 118, 145);
        let far_color = colors::rgb(70, 88, 112);
        for (i, cloud) in CLOUDS.iter().enumerate() {
            let sprite_w = cloud.sprite[0].chars().count() as i32;
            let span = canvas.width as i32 + sprite_w;
            let x = ((cloud.start + t * cloud.speed) as i32).rem_euclid(span.max(1)) - sprite_w;
            let color = if i == 0 { far_color } else { cloud_color };
            for (dy, row) in cloud.sprite.iter().enumerate() {
                canvas.draw(x, cloud.row + dy as i32, row, color, false);
            }
        }

        // Sea along the bottom. The shimmer moves one cell every
        // SEA_SHIMMER_MS; the characters stay `~` so the line never breaks.
        let sea_dark = colors::rgb(70, 120, 165);
        let sea_light = colors::rgb(120, 175, 215);
        let sea_rows = if canvas.height >= 8 { 2 } else { 1 };
        let phase = (self.clock.elapsed().as_millis() / SEA_SHIMMER_MS) as usize;
        for r in 0..sea_rows {
            let y = canvas.height as i32 - 1 - r as i32;
            for x in 0..canvas.width {
                let lit = (x + phase + r * 2) % 4 < 2;
                canvas.put(x as i32, y, '~', if lit { sea_light } else { sea_dark });
            }
        }
    }

    /// The whale, the ropes and birds (airborne) or the freed birds
    /// (landed), laid out from `scene_top`.
    fn draw_whale_scene(&self, canvas: &mut Canvas, interior_height: u16, landed: bool) {
        let whale_blue = colors::rgb(130, 165, 200);
        let bird_color = colors::rgb(210, 215, 225);
        let rope_color = colors::rgb(150, 140, 120);

        let top = self.scene_top(interior_height).max(0.0) as i32;
        let x0 = (canvas.width as i32 - WHALE_WIDTH as i32 - 1) / 2;
        let birds = self.bird_count.max(1);
        let cell_w = WHALE_WIDTH / birds;

        let mut y = top;
        if landed {
            // The birds have let go and flown off, happy, above the whale.
            let flight: String = (0..birds)
                .map(|_| format!("{:^w$}", "^", w = cell_w))
                .collect();
            canvas.draw(x0, y, &flight, bird_color, false);
            y += 1;
        }
        for row in WHALE_BODY {
            // Opaque between the outline's edges, so clouds behind the whale
            // do not show through its belly.
            canvas.draw(x0, y, row, whale_blue, true);
            y += 1;
        }
        if !landed {
            let wing = if self.wings_flap_open() {
                "/|\\"
            } else {
                "\\|/"
            };
            let ropes: String = (0..birds)
                .map(|_| format!("{:^w$}", "|", w = cell_w))
                .collect();
            let wings: String = (0..birds)
                .map(|_| format!("{:^w$}", wing, w = cell_w))
                .collect();
            canvas.draw(x0, y, &ropes, rope_color, false);
            canvas.draw(x0, y + 1, &wings, bird_color, false);
        }
    }
}

/// Half-cycle of the sun's ray flicker.
const SUN_FLICKER_MS: u128 = 600;
/// Time between one-cell steps of the sea shimmer.
const SEA_SHIMMER_MS: u128 = 300;

/// One drifting cloud: where it starts (cells right of the left edge), how
/// fast it moves (cells per second), its first row, and its sprite.
struct Cloud {
    start: f32,
    speed: f32,
    row: i32,
    sprite: [&'static str; 2],
}

const CLOUDS: [Cloud; 3] = [
    Cloud {
        start: 6.0,
        speed: 1.2,
        row: 0,
        sprite: [" .--~~--. ", "(________)"],
    },
    Cloud {
        start: 38.0,
        speed: 2.4,
        row: 5,
        sprite: ["  .~~~.   ", " (_____)-."],
    },
    Cloud {
        start: 62.0,
        speed: 3.6,
        row: 9,
        sprite: [" .-~-. ", "(_____)"],
    },
];

/// A character grid the size of the box interior. Each cell holds a glyph
/// and a foreground color; drawing clips to the grid.
struct Canvas {
    width: usize,
    height: usize,
    cells: Vec<Vec<(char, Option<ratatui::style::Color>)>>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![vec![(' ', None); width]; height],
        }
    }

    fn put(&mut self, x: i32, y: i32, ch: char, color: ratatui::style::Color) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        self.cells[y as usize][x as usize] = (ch, Some(color));
    }

    /// Draws `text` with its first glyph at (`x`, `y`). Spaces leave the
    /// background alone unless `opaque`, in which case the spaces between the
    /// first and last glyph overwrite it.
    fn draw(&mut self, x: i32, y: i32, text: &str, color: ratatui::style::Color, opaque: bool) {
        let chars: Vec<char> = text.chars().collect();
        let first = chars.iter().position(|c| *c != ' ');
        let last = chars.iter().rposition(|c| *c != ' ');
        let (Some(first), Some(last)) = (first, last) else {
            return;
        };
        for (i, ch) in chars.iter().enumerate() {
            if *ch == ' ' && !(opaque && i > first && i < last) {
                continue;
            }
            self.put(x + i as i32, y, *ch, color);
        }
    }

    /// One `Line` per row, with runs of the same color merged into one span.
    fn into_lines(self) -> Vec<Line<'static>> {
        self.cells
            .into_iter()
            .map(|row| {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut run = String::new();
                let mut run_color = None;
                for (ch, color) in row {
                    if color != run_color && !run.is_empty() {
                        spans.push(styled_run(std::mem::take(&mut run), run_color));
                    }
                    run_color = color;
                    run.push(ch);
                }
                if !run.is_empty() {
                    spans.push(styled_run(run, run_color));
                }
                Line::from(spans)
            })
            .collect()
    }
}

fn styled_run(text: String, color: Option<ratatui::style::Color>) -> Span<'static> {
    match color {
        Some(c) => Span::styled(text, Style::default().fg(c)),
        None => Span::raw(text),
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
            device_indices: (0..chip_count).collect(),
            raw_targets: vec![],
        }
    }

    #[test]
    fn not_done_until_finished_and_tail_elapsed() {
        let mut t = SillyCetaceanTakeover::new(&ev(4));
        t.tick(Duration::from_secs(3));
        assert!(!t.is_done());
        t.note_reset_finished();
        t.tick(DONE_TAIL);
        assert!(t.is_done());
    }

    #[test]
    fn skip_is_immediately_done() {
        let mut t = SillyCetaceanTakeover::new(&ev(2));
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn render_does_not_panic_across_bird_counts_and_states() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        for chips in [1, 2, 4, 5, 8] {
            let backend = TestBackend::new(80, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut t = SillyCetaceanTakeover::new(&ev(chips));
            terminal.draw(|f| t.render(f, f.area())).unwrap();
            t.tick(Duration::from_millis(500));
            terminal.draw(|f| t.render(f, f.area())).unwrap();
            t.note_reset_finished();
            terminal.draw(|f| t.render(f, f.area())).unwrap();
            t.tick(Duration::from_secs(2));
            terminal.draw(|f| t.render(f, f.area())).unwrap();
        }
    }

    #[test]
    fn render_does_not_panic_on_a_small_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(40, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let t = SillyCetaceanTakeover::new(&ev(4));
        terminal.draw(|f| t.render(f, f.area())).unwrap();
    }

    /// Bird count scales with the real chip count — a bigger reset needs
    /// more birds to carry, capped to a sane visual range.
    #[test]
    fn bird_count_scales_with_chip_count_and_caps() {
        assert_eq!(SillyCetaceanTakeover::new(&ev(1)).bird_count, 1);
        assert_eq!(SillyCetaceanTakeover::new(&ev(3)).bird_count, 3);
        assert_eq!(SillyCetaceanTakeover::new(&ev(20)).bird_count, 5);
    }

    /// The whale must not start descending before the real reset finishes
    /// — no faked landing time.
    #[test]
    fn landing_progress_stays_zero_while_in_progress() {
        let mut t = SillyCetaceanTakeover::new(&ev(4));
        t.tick(Duration::from_secs(10));
        assert_eq!(t.landing_progress(), 0.0);
    }

    /// Once finished, landing progress must actually advance over real
    /// elapsed time, then settle at 1.0 and stop moving.
    #[test]
    fn landing_progress_advances_then_settles() {
        let mut t = SillyCetaceanTakeover::new(&ev(4));
        t.note_reset_finished();
        let p0 = t.landing_progress();
        t.tick(Duration::from_millis(400));
        let p1 = t.landing_progress();
        assert!(p1 > p0, "landing should advance over real elapsed time");
        t.tick(Duration::from_secs(5));
        let p2 = t.landing_progress();
        assert_eq!(
            p2, 1.0,
            "landing should settle at fully-landed and stop climbing past it"
        );
    }

    /// While airborne, the scene must actually bob — real motion, not a
    /// static hover. Asserted directly on the position calculation (not a
    /// rendered snapshot), so this doesn't depend on a floating-point bob
    /// happening to cross an integer row boundary within an arbitrary tick
    /// window.
    #[test]
    fn hovers_with_real_motion_while_in_progress() {
        let mut t = SillyCetaceanTakeover::new(&ev(4));
        let a = t.scene_top(30);
        t.tick(Duration::from_millis(700));
        let b = t.scene_top(30);
        assert_ne!(
            a, b,
            "the scene's position should change while hovering, not sit static"
        );
    }

    /// The bob amplitude must be wide enough to actually cross a row
    /// boundary over a bob cycle — otherwise the "motion" is invisible in
    /// a character grid regardless of what the floating-point math says.
    #[test]
    fn bob_amplitude_crosses_at_least_one_row_boundary() {
        let mut t = SillyCetaceanTakeover::new(&ev(4));
        let mut floors = std::collections::HashSet::new();
        for _ in 0..40 {
            floors.insert(t.scene_top(30).floor() as i32);
            t.tick(Duration::from_millis(150));
        }
        assert!(
            floors.len() >= 2,
            "expected the hover to visibly cross at least one row boundary, floors seen: {floors:?}"
        );
    }

    /// Renders `t` into a `w` x `h` buffer and returns it as one string per row.
    fn rows_of(t: &SillyCetaceanTakeover, w: u16, h: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| t.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    #[test]
    fn title_is_silly_cetacean_and_changes_on_touchdown() {
        let mut t = SillyCetaceanTakeover::new(&ev(4));
        t.tick(Duration::from_millis(500));
        let airborne = rows_of(&t, 100, 30).join("\n");
        assert!(airborne.contains("SILLY CETACEAN"), "{airborne}");
        assert!(!airborne.contains("FAIL WHALE") && !airborne.contains("TOUCHDOWN"));
        t.note_reset_finished();
        t.tick(Duration::from_secs(3));
        let landed = rows_of(&t, 100, 30).join("\n");
        assert!(landed.contains("SILLY CETACEAN - TOUCHDOWN"), "{landed}");
    }

    /// The sky is alive: clouds drift, so two frames a few seconds apart
    /// differ in the cloud rows even though the whale scene stays put.
    #[test]
    fn clouds_drift_over_time() {
        let mut t = SillyCetaceanTakeover::new(&ev(4));
        // By 6 s the first cloud is fully on screen.
        t.tick(Duration::from_secs(6));
        let a = rows_of(&t, 134, 40);
        assert!(a.join("\n").contains("(________)"), "{}", a.join("\n"));
        t.tick(Duration::from_secs(4));
        let b = rows_of(&t, 134, 40);
        assert_ne!(a, b, "the background should move");
    }

    /// The sun and the sea are part of the background, and the sea is one
    /// unbroken run of `~` along the bottom of the box.
    #[test]
    fn sun_and_sea_are_drawn() {
        let t = SillyCetaceanTakeover::new(&ev(4));
        let text = rows_of(&t, 134, 40).join("\n");
        assert!(text.contains("- O -") || text.contains("-(O)-"), "{text}");
        assert!(text.contains(&"~".repeat(40)), "{text}");
    }

    /// Whatever is behind the whale must not show through its body. The canvas
    /// is filled with `#`, the whale is drawn over it, and no `#` may remain
    /// between the outline's edges on the eye row. (The belly row has no
    /// interior spaces, so it could not tell opaque from transparent.)
    #[test]
    fn nothing_behind_the_whale_shows_through_its_body() {
        let t = SillyCetaceanTakeover::new(&ev(4));
        let mut canvas = Canvas::new(71, 22);
        for row in canvas.cells.iter_mut() {
            for cell in row.iter_mut() {
                *cell = ('#', None);
            }
        }
        t.draw_whale_scene(&mut canvas, 22, false);
        let eye_row: String = canvas
            .cells
            .iter()
            .map(|r| r.iter().map(|c| c.0).collect::<String>())
            .find(|r| r.contains('o'))
            .expect("the whale's eye row is drawn");
        let left = eye_row.find('/').expect("left edge of the body");
        let right = eye_row.rfind('\\').expect("right edge of the body");
        assert!(
            !eye_row[left..right].contains('#'),
            "background shows through the whale: {eye_row:?}"
        );
    }

    /// A transparent draw leaves the background in the gaps; an opaque one
    /// fills the gaps between the first and last glyph but not outside them.
    #[test]
    fn canvas_draw_opaque_fills_gaps_only_between_glyphs() {
        let fill = |opaque| {
            let mut c = Canvas::new(8, 1);
            c.cells[0] = vec![('#', None); 8];
            c.draw(1, 0, " a  b ", colors::rgb(1, 2, 3), opaque);
            c.cells[0].iter().map(|x| x.0).collect::<String>()
        };
        assert_eq!(fill(false), "##a##b##");
        assert_eq!(fill(true), "##a  b##");
    }

    #[test]
    fn render_does_not_panic_on_tiny_boxes() {
        for (w, h) in [(8u16, 4u16), (20, 8), (40, 12), (12, 30)] {
            let mut t = SillyCetaceanTakeover::new(&ev(3));
            for finished in [false, true] {
                if finished {
                    t.note_reset_finished();
                    t.tick(Duration::from_secs(3));
                }
                let _ = rows_of(&t, w, h);
            }
        }
    }
}
