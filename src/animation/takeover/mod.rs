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

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
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

mod quiet_notice;
pub use quiet_notice::QuietNoticeTakeover;

mod missile_command;
pub use missile_command::MissileCommandTakeover;

mod bbs;
pub use bbs::BbsTakeover;

mod blackhole_swarm;
pub use blackhole_swarm::BlackholeSwarmTakeover;

mod hatch_countdown;
pub use hatch_countdown::HatchCountdownTakeover;

mod trek_reset;
pub use trek_reset::TrekResetTakeover;

mod fail_whale;
pub use fail_whale::FailWhaleTakeover;

/// How much a `TintOverlay` blends toward its tint color: 0.0 leaves cells
/// untouched, 1.0 fully replaces them. Chosen so the real screen beneath a
/// takeover stays clearly visible (a "color filter", per the design ask)
/// without competing with the takeover's own text for attention.
const TINT_ALPHA: f32 = 0.55;

/// Extract an RGB triple from a `Color`, treating any non-`Rgb` variant as
/// black — every takeover variant already builds its accent color via
/// `colors::rgb(...)`, so this only falls back for a color this module
/// didn't construct itself.
fn as_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

fn blend_channel(base: u8, tint: u8, alpha: f32) -> u8 {
    (base as f32 * (1.0 - alpha) + tint as f32 * alpha)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn blend_color(base: Color, tint: (u8, u8, u8), alpha: f32) -> Color {
    let (br, bg, bb) = as_rgb(base);
    Color::Rgb(
        blend_channel(br, tint.0, alpha),
        blend_channel(bg, tint.1, alpha),
        blend_channel(bb, tint.2, alpha),
    )
}

/// A widget that blends every existing cell's fg and bg toward `tint` by
/// `alpha`, in place — instead of clearing the area, this is how a takeover
/// reveals the real screen beneath it through a color wash. Any cell a
/// takeover's own `Block`/`Paragraph` draws into afterward still gets its
/// own crisp, unblended color (both only patch the style fields they
/// explicitly set), so the takeover's text stays legible over the tinted,
/// still-recognizable backdrop.
struct TintOverlay {
    tint: (u8, u8, u8),
    alpha: f32,
}

impl Widget for TintOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.fg = blend_color(cell.fg, self.tint, self.alpha);
                    cell.bg = blend_color(cell.bg, self.tint, self.alpha);
                }
            }
        }
    }
}

/// Shared full-screen frame: tints the terminal cells toward `border_color`
/// (see [`TintOverlay`] — the real screen shows through a color wash rather
/// than being cleared to black), paints a bordered block (left/bottom
/// borders only, per this project's no-right-border-glyph convention) with
/// `title`, and renders `lines` as a centered paragraph on top. Every
/// variant's `render` calls this so a takeover always reads as one
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
    f.render_widget(
        TintOverlay {
            tint: as_rgb(border_color),
            alpha: TINT_ALPHA,
        },
        area,
    );
    let block = Block::default()
        .borders(Borders::LEFT | Borders::BOTTOM)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let para = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(para, inner);
}

use crate::workload::reset_detect::ResetEvent;

/// One active full-screen takeover animation.
pub enum Takeover {
    Bbs(BbsTakeover),
    BlackholeSwarm(BlackholeSwarmTakeover),
    HatchCountdown(HatchCountdownTakeover),
    MissileCommand(MissileCommandTakeover),
    TrekReset(TrekResetTakeover),
    FailWhale(FailWhaleTakeover),
    QuietNotice(QuietNoticeTakeover),
}

impl Takeover {
    pub fn tick(&mut self, elapsed: Duration) {
        match self {
            Takeover::Bbs(v) => v.tick(elapsed),
            Takeover::BlackholeSwarm(v) => v.tick(elapsed),
            Takeover::HatchCountdown(v) => v.tick(elapsed),
            Takeover::MissileCommand(v) => v.tick(elapsed),
            Takeover::TrekReset(v) => v.tick(elapsed),
            Takeover::FailWhale(v) => v.tick(elapsed),
            Takeover::QuietNotice(v) => v.tick(elapsed),
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        match self {
            Takeover::Bbs(v) => v.render(f, area),
            Takeover::BlackholeSwarm(v) => v.render(f, area),
            Takeover::HatchCountdown(v) => v.render(f, area),
            Takeover::MissileCommand(v) => v.render(f, area),
            Takeover::TrekReset(v) => v.render(f, area),
            Takeover::FailWhale(v) => v.render(f, area),
            Takeover::QuietNotice(v) => v.render(f, area),
        }
    }

    pub fn is_done(&self) -> bool {
        match self {
            Takeover::Bbs(v) => v.is_done(),
            Takeover::BlackholeSwarm(v) => v.is_done(),
            Takeover::HatchCountdown(v) => v.is_done(),
            Takeover::MissileCommand(v) => v.is_done(),
            Takeover::TrekReset(v) => v.is_done(),
            Takeover::FailWhale(v) => v.is_done(),
            Takeover::QuietNotice(v) => v.is_done(),
        }
    }

    pub fn note_reset_finished(&mut self) {
        match self {
            Takeover::Bbs(v) => v.note_reset_finished(),
            Takeover::BlackholeSwarm(v) => v.note_reset_finished(),
            Takeover::HatchCountdown(v) => v.note_reset_finished(),
            Takeover::MissileCommand(v) => v.note_reset_finished(),
            Takeover::TrekReset(v) => v.note_reset_finished(),
            Takeover::FailWhale(v) => v.note_reset_finished(),
            Takeover::QuietNotice(v) => v.note_reset_finished(),
        }
    }

    pub fn skip(&mut self) {
        match self {
            Takeover::Bbs(v) => v.skip(),
            Takeover::BlackholeSwarm(v) => v.skip(),
            Takeover::HatchCountdown(v) => v.skip(),
            Takeover::MissileCommand(v) => v.skip(),
            Takeover::TrekReset(v) => v.skip(),
            Takeover::FailWhale(v) => v.skip(),
            Takeover::QuietNotice(v) => v.skip(),
        }
    }
}

/// Weighted-random pick of a variant for a detected reset (real entropy —
/// see `pick_takeover_from_roll` for the deterministic, testable core).
/// `is_full` weights toward the five spectacle variants; a subset reset
/// weights toward `MissileCommand` (scoped to the real targets) and
/// `QuietNotice`.
pub fn pick_takeover(ev: &ResetEvent) -> Takeover {
    use rand::Rng;
    let roll: u8 = rand::rng().random_range(0..100);
    pick_takeover_from_roll(ev, roll)
}

/// Pure selection core: `roll` in `0..100` maps to a variant. Full-reset
/// weights: Bbs 20, BlackholeSwarm 20, HatchCountdown 16, TrekReset 16,
/// FailWhale 16, MissileCommand 6, QuietNotice 6. Subset weights:
/// MissileCommand 38, QuietNotice 28, FailWhale 10, TrekReset 8, Bbs 6,
/// BlackholeSwarm 4, HatchCountdown 6.
fn pick_takeover_from_roll(ev: &ResetEvent, roll: u8) -> Takeover {
    if ev.is_full {
        match roll {
            0..=19 => Takeover::Bbs(BbsTakeover::new(ev)),
            20..=39 => Takeover::BlackholeSwarm(BlackholeSwarmTakeover::new(ev)),
            40..=55 => Takeover::HatchCountdown(HatchCountdownTakeover::new(ev)),
            56..=71 => Takeover::TrekReset(TrekResetTakeover::new(ev)),
            72..=87 => Takeover::FailWhale(FailWhaleTakeover::new(ev)),
            88..=93 => Takeover::MissileCommand(MissileCommandTakeover::new(ev)),
            _ => Takeover::QuietNotice(QuietNoticeTakeover::new(ev)),
        }
    } else {
        match roll {
            0..=37 => Takeover::MissileCommand(MissileCommandTakeover::new(ev)),
            38..=65 => Takeover::QuietNotice(QuietNoticeTakeover::new(ev)),
            66..=75 => Takeover::FailWhale(FailWhaleTakeover::new(ev)),
            76..=83 => Takeover::TrekReset(TrekResetTakeover::new(ev)),
            84..=89 => Takeover::Bbs(BbsTakeover::new(ev)),
            90..=93 => Takeover::BlackholeSwarm(BlackholeSwarmTakeover::new(ev)),
            _ => Takeover::HatchCountdown(HatchCountdownTakeover::new(ev)),
        }
    }
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

    #[test]
    fn blend_color_at_zero_alpha_leaves_the_original_color_untouched() {
        let original = Color::Rgb(10, 20, 30);
        assert_eq!(blend_color(original, (255, 0, 0), 0.0), original);
    }

    #[test]
    fn blend_color_at_full_alpha_becomes_the_tint_exactly() {
        let original = Color::Rgb(10, 20, 30);
        assert_eq!(blend_color(original, (200, 100, 50), 1.0), Color::Rgb(200, 100, 50));
    }

    /// The whole point of `TintOverlay` replacing `Clear`: a cell the
    /// takeover itself never draws into must still show the REAL underlying
    /// color, blended toward the tint — not black, and not the raw
    /// untouched original either. This proves the "reveal the screen
    /// beneath, with a color filter" behavior end to end through
    /// `render_takeover_frame`, not just the `blend_color` helper in
    /// isolation.
    ///
    /// Both the "underlying view" and the takeover overlay are rendered
    /// inside the SAME `terminal.draw()` closure, exactly matching how the
    /// real main loop wires it (`src/ui/tui/mod.rs` renders whatever
    /// `display_mode` picks, then calls the active takeover's `render` as
    /// the last step of that same draw) — `ratatui::Terminal` hands each
    /// separate `draw()` call a fresh buffer, so two separate `draw()` calls
    /// would NOT exercise this at all.
    #[test]
    fn render_takeover_frame_tints_rather_than_clears_the_underlying_screen() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(40, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        let underlying_bg = Color::Rgb(20, 80, 200);
        let tint = (255, 0, 255); // an arbitrary, distinctive accent color
        terminal
            .draw(|f| {
                // Stands in for "whatever real telemetry view already
                // painted this frame" — the takeover overlay renders on top
                // of it, in the same frame, just like the real main loop.
                f.render_widget(
                    Block::default().style(Style::default().bg(underlying_bg)),
                    f.area(),
                );
                render_takeover_frame(
                    f,
                    f.area(),
                    "TITLE",
                    Color::Rgb(tint.0, tint.1, tint.2),
                    vec![Line::raw("one line of content")],
                );
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let corner = &buf[(0, 0)];
        let expected = blend_color(underlying_bg, tint, TINT_ALPHA);
        assert_eq!(
            corner.bg, expected,
            "an untouched corner cell should show the real underlying color blended toward the tint, not a hard clear"
        );
        assert_ne!(
            corner.bg,
            Color::Rgb(0, 0, 0),
            "must not have been cleared to black"
        );
        assert_ne!(
            corner.bg,
            underlying_bg,
            "must actually be tinted, not left 100% raw"
        );
    }

    fn full_ev() -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: true,
            chip_count: 4,
            total_devices: 4,
            device_indices: vec![0, 1, 2, 3],
            raw_targets: vec![],
        }
    }

    fn subset_ev() -> ResetEvent {
        ResetEvent {
            pid: 1,
            is_full: false,
            chip_count: 1,
            total_devices: 4,
            device_indices: vec![2],
            raw_targets: vec!["2".to_string()],
        }
    }

    #[test]
    fn full_reset_roll_boundaries_pick_expected_variant() {
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 0),
            Takeover::Bbs(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 19),
            Takeover::Bbs(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 20),
            Takeover::BlackholeSwarm(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 72),
            Takeover::FailWhale(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 87),
            Takeover::FailWhale(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 88),
            Takeover::MissileCommand(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&full_ev(), 99),
            Takeover::QuietNotice(_)
        ));
    }

    #[test]
    fn subset_reset_roll_boundaries_pick_expected_variant() {
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 0),
            Takeover::MissileCommand(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 38),
            Takeover::QuietNotice(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 66),
            Takeover::FailWhale(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 75),
            Takeover::FailWhale(_)
        ));
        assert!(matches!(
            pick_takeover_from_roll(&subset_ev(), 99),
            Takeover::HatchCountdown(_)
        ));
    }

    #[test]
    fn every_roll_value_produces_some_variant() {
        // Exhaustiveness check: no roll value should ever fail to match
        // (the match is total via `_`, but this pins that every branch is
        // actually reachable without panicking across the full range).
        for roll in 0..=255u8 {
            let _ = pick_takeover_from_roll(&full_ev(), roll);
            let _ = pick_takeover_from_roll(&subset_ev(), roll);
        }
    }
}
