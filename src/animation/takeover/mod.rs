// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! "Reset takeover" animations drawn in one fixed, centered box over a
//! full-screen color wash, triggered by
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
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
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

mod demo;
pub use demo::{DemoSequence, DemoSource, RESOLVE_AT, SLOT_LEN};

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

/// Width of the takeover box on a terminal large enough to hold it.
pub(crate) const BOX_WIDTH: u16 = 72;
/// Height of the takeover box on a terminal large enough to hold it.
pub(crate) const BOX_HEIGHT: u16 = 24;

/// The takeover box: a fixed [`BOX_WIDTH`] x [`BOX_HEIGHT`] rectangle
/// centered in `area`. On a smaller terminal it shrinks to leave at least
/// 2 columns and 1 row of margin in total, and it never exceeds `area`.
/// Every variant and [`render_takeover_frame`] take their sizes from here,
/// so all seven animations share one box.
pub(crate) fn takeover_box(area: Rect) -> Rect {
    let width = BOX_WIDTH.min(area.width.saturating_sub(2));
    let height = BOX_HEIGHT.min(area.height.saturating_sub(1));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// The drawable interior of [`takeover_box`]: the box minus its left border
/// column, its top row (ratatui gives a block title a row of its own even
/// with no top border) and its bottom border row. At full size this is 71
/// columns by 22 rows. Variants that size their art to the screen use this
/// in place of `area`.
pub(crate) fn takeover_interior(area: Rect) -> Rect {
    let b = takeover_box(area);
    Rect::new(
        b.x + 1,
        b.y + 1,
        b.width.saturating_sub(1),
        b.height.saturating_sub(2),
    )
}

/// True when `area` is large enough for [`render_takeover_frame`] to draw
/// anything: at least 8 columns by 4 rows (the same guard as
/// `render_overlay_panel`). Below that the frame draws nothing.
pub(crate) fn takeover_fits(area: Rect) -> bool {
    area.width >= 8 && area.height >= 4
}

/// Shared takeover frame. The whole `area` is tinted toward `border_color`
/// (see [`TintOverlay`]), so the real screen shows through a color wash.
/// A fixed, centered box ([`takeover_box`]) is then cleared to the
/// terminal's default background. It gets a left and bottom border (no
/// right border, per this project's no-right-border-glyph convention) with
/// `title` on its top row, preceded by `tag - ` when `tag` is not empty (a
/// demo run passes `DEMO`; every other takeover passes `""`). `lines` are
/// rendered as a centered paragraph in the box interior
/// ([`takeover_interior`]) only: lines wider or taller than the interior
/// are clipped at the box edge. Every variant's `render` calls this, so
/// every takeover gets the same box. No-ops on a terminal too small to
/// safely draw into ([`takeover_fits`], the same guard as
/// `render_overlay_panel`).
pub(crate) fn render_takeover_frame(
    f: &mut Frame,
    area: Rect,
    tag: &str,
    title: &str,
    border_color: Color,
    lines: Vec<Line<'static>>,
) {
    if !takeover_fits(area) {
        return;
    }
    f.render_widget(
        TintOverlay {
            tint: as_rgb(border_color),
            alpha: TINT_ALPHA,
        },
        area,
    );
    let box_area = takeover_box(area);
    // `Clear` resets every cell in the box (blank symbol, default fg/bg), so
    // no tinted or underlying cell shows through inside it.
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::LEFT | Borders::BOTTOM)
        .title(if tag.is_empty() {
            format!(" {title} ")
        } else {
            format!(" {tag} - {title} ")
        })
        .border_style(Style::default().fg(border_color));
    let inner = takeover_interior(area);
    // `Paragraph` itself cuts lines wider than `inner` at its right edge
    // and stops after `inner.height` rows, so oversized art is clipped.
    let para = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(block, box_area);
    f.render_widget(para, inner);
}

use crate::workload::reset_detect::ResetEvent;

/// One active takeover animation (drawn in the shared centered box).
pub enum Takeover {
    Bbs(BbsTakeover),
    BlackholeSwarm(BlackholeSwarmTakeover),
    HatchCountdown(HatchCountdownTakeover),
    MissileCommand(MissileCommandTakeover),
    TrekReset(TrekResetTakeover),
    FailWhale(FailWhaleTakeover),
    QuietNotice(QuietNoticeTakeover),
    /// The `demo` behavior: all seven in a fixed order. See [`demo`].
    Demo(DemoSequence),
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
            Takeover::Demo(v) => v.tick(elapsed),
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
            Takeover::Demo(v) => v.render(f, area),
        }
    }

    /// Like `render`, with `tag` in front of the box title (see
    /// [`render_takeover_frame`]). A demo sequence supplies its own tag, so
    /// a nested call is not expected and draws the sequence as usual.
    pub(crate) fn render_tagged(&self, f: &mut Frame, area: Rect, tag: &str) {
        match self {
            Takeover::Bbs(v) => v.render_tagged(f, area, tag),
            Takeover::BlackholeSwarm(v) => v.render_tagged(f, area, tag),
            Takeover::HatchCountdown(v) => v.render_tagged(f, area, tag),
            Takeover::MissileCommand(v) => v.render_tagged(f, area, tag),
            Takeover::TrekReset(v) => v.render_tagged(f, area, tag),
            Takeover::FailWhale(v) => v.render_tagged(f, area, tag),
            Takeover::QuietNotice(v) => v.render_tagged(f, area, tag),
            Takeover::Demo(v) => v.render(f, area),
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
            Takeover::Demo(v) => v.is_done(),
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
            Takeover::Demo(v) => v.note_reset_finished(),
        }
    }

    /// Starts the boot demo: every takeover in a row over a synthetic full
    /// reset of `device_count` chips (`pid` 0, so nothing real is implied).
    pub fn demo_boot(device_count: usize) -> Takeover {
        let ev = ResetEvent {
            pid: 0,
            is_full: true,
            chip_count: device_count,
            total_devices: device_count,
            device_indices: (0..device_count).map(|i| i as u8).collect(),
            raw_targets: vec![],
        };
        Takeover::Demo(DemoSequence::new(DemoSource::Boot, ev))
    }

    /// Starts the demo for a real reset: every takeover in a row, scoped to
    /// the real event's chips.
    pub fn demo_real(ev: &ResetEvent) -> Takeover {
        Takeover::Demo(DemoSequence::new(DemoSource::RealReset, ev.clone()))
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Takeover::Demo(_))
    }

    /// Ends a demo at once. A single takeover has no early exit other than
    /// `skip`, so this is a no-op for the other variants.
    pub fn end(&mut self) {
        if let Takeover::Demo(v) = self {
            v.end();
        }
    }

    /// The variant's name; for a demo, the animation now running.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Takeover::Bbs(_) => "Bbs",
            Takeover::BlackholeSwarm(_) => "BlackholeSwarm",
            Takeover::HatchCountdown(_) => "HatchCountdown",
            Takeover::MissileCommand(_) => "MissileCommand",
            Takeover::TrekReset(_) => "TrekReset",
            Takeover::FailWhale(_) => "FailWhale",
            Takeover::QuietNotice(_) => "QuietNotice",
            Takeover::Demo(v) => v.current_name(),
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
            Takeover::Demo(v) => v.skip(),
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
                render_takeover_frame(f, f.area(), "", "X", Color::White, vec![Line::raw("hi")]);
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
                    "",
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
        assert_eq!(
            blend_color(original, (200, 100, 50), 1.0),
            Color::Rgb(200, 100, 50)
        );
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
                    "",
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
            corner.bg, underlying_bg,
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

    // ---- Centered box: geometry and rendering ------------------------------

    #[test]
    fn takeover_box_is_72x24_centered_on_a_large_terminal() {
        let b = takeover_box(Rect::new(0, 0, 134, 40));
        assert_eq!(b, Rect::new(31, 8, 72, 24));
    }

    #[test]
    fn takeover_box_on_80x24_keeps_two_columns_and_one_row_of_margin() {
        let b = takeover_box(Rect::new(0, 0, 80, 24));
        assert_eq!((b.width, b.height), (72, 23));
        assert_eq!((b.x, b.y), (4, 0));
    }

    #[test]
    fn takeover_box_shrinks_exactly_at_the_boundary() {
        // 74x25 is the smallest area that still holds the full 72x24 box.
        let full = takeover_box(Rect::new(0, 0, 74, 25));
        assert_eq!((full.width, full.height), (72, 24));
        let one_less_w = takeover_box(Rect::new(0, 0, 73, 25));
        assert_eq!((one_less_w.width, one_less_w.height), (71, 24));
        let one_less_h = takeover_box(Rect::new(0, 0, 74, 24));
        assert_eq!((one_less_h.width, one_less_h.height), (72, 23));
    }

    #[test]
    fn takeover_box_respects_the_area_origin() {
        let b = takeover_box(Rect::new(10, 5, 134, 40));
        assert_eq!(b, Rect::new(41, 13, 72, 24));
    }

    #[test]
    fn takeover_box_never_exceeds_the_area_at_any_size() {
        for w in 8..=200u16 {
            for h in 4..=80u16 {
                let area = Rect::new(0, 0, w, h);
                let b = takeover_box(area);
                assert!(
                    b.width <= BOX_WIDTH && b.height <= BOX_HEIGHT,
                    "{w}x{h}: {b:?}"
                );
                assert_eq!(b.width, BOX_WIDTH.min(w - 2), "{w}x{h}");
                assert_eq!(b.height, BOX_HEIGHT.min(h - 1), "{w}x{h}");
                assert!(b.x >= area.x && b.right() <= area.right(), "{w}x{h}: {b:?}");
                assert!(
                    b.y >= area.y && b.bottom() <= area.bottom(),
                    "{w}x{h}: {b:?}"
                );
                // Centered by integer division: the spare space splits
                // evenly, with any odd cell going to the right/bottom.
                assert_eq!(b.x - area.x, (w - b.width) / 2, "{w}x{h}");
                assert_eq!(b.y - area.y, (h - b.height) / 2, "{w}x{h}");
            }
        }
    }

    #[test]
    fn takeover_interior_drops_the_left_border_title_row_and_bottom_border() {
        let area = Rect::new(0, 0, 134, 40);
        let b = takeover_box(area);
        let i = takeover_interior(area);
        assert_eq!((i.width, i.height), (71, 22));
        assert_eq!((i.x, i.y), (b.x + 1, b.y + 1));
    }

    /// The interior must match what ratatui itself reserves for a titled
    /// block with only left and bottom borders, at every size.
    #[test]
    fn takeover_interior_matches_the_titled_blocks_own_inner_area() {
        for (w, h) in [(134u16, 40u16), (80, 24), (40, 12), (20, 8), (8, 4)] {
            let area = Rect::new(0, 0, w, h);
            let block = Block::default()
                .borders(Borders::LEFT | Borders::BOTTOM)
                .title(" T ");
            assert_eq!(
                takeover_interior(area),
                block.inner(takeover_box(area)),
                "{w}x{h}"
            );
        }
    }

    /// A buffer cell value that no takeover ever writes, so any cell that
    /// still holds it after a render was never touched by the art.
    const SEED_SYMBOL: &str = ".";
    const SEED_BG: Color = Color::Rgb(20, 80, 200);
    const SEED_FG: Color = Color::Rgb(200, 200, 100);

    /// Render `draw` onto a `w`x`h` terminal whose cells were first filled
    /// with the seed value (standing in for the real screen beneath).
    fn render_over_seed(
        w: u16,
        h: u16,
        draw: impl Fn(&mut Frame, Rect),
    ) -> ratatui::buffer::Buffer {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                for cell in f.buffer_mut().content.iter_mut() {
                    cell.set_symbol(SEED_SYMBOL);
                    cell.fg = SEED_FG;
                    cell.bg = SEED_BG;
                }
                draw(f, f.area());
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// One of each variant, built from the same event.
    fn every_variant(ev: &ResetEvent) -> Vec<(&'static str, Takeover)> {
        vec![
            ("bbs", Takeover::Bbs(BbsTakeover::new(ev))),
            (
                "blackhole",
                Takeover::BlackholeSwarm(BlackholeSwarmTakeover::new(ev)),
            ),
            (
                "hatch",
                Takeover::HatchCountdown(HatchCountdownTakeover::new(ev)),
            ),
            (
                "missile",
                Takeover::MissileCommand(MissileCommandTakeover::new(ev)),
            ),
            ("trek", Takeover::TrekReset(TrekResetTakeover::new(ev))),
            ("whale", Takeover::FailWhale(FailWhaleTakeover::new(ev))),
            ("quiet", Takeover::QuietNotice(QuietNoticeTakeover::new(ev))),
        ]
    }

    /// Every variant in both lifecycle states (running, and finished with
    /// time for the landing/settling beats to play), for both a full and a
    /// subset reset.
    fn every_variant_state() -> Vec<(String, Takeover)> {
        let mut out = Vec::new();
        for (scope, ev) in [("full", full_ev()), ("subset", subset_ev())] {
            for finished in [false, true] {
                for (name, mut t) in every_variant(&ev) {
                    t.tick(Duration::from_millis(900));
                    if finished {
                        t.note_reset_finished();
                        t.tick(Duration::from_secs(3));
                    }
                    out.push((format!("{name}/{scope}/finished={finished}"), t));
                }
            }
        }
        out
    }

    const TERMINAL_SIZES: [(u16, u16); 6] =
        [(134, 40), (80, 24), (74, 25), (40, 12), (20, 8), (8, 4)];

    #[test]
    fn every_variant_clears_exactly_the_box_to_the_default_background() {
        for (name, t) in every_variant_state() {
            for (w, h) in TERMINAL_SIZES {
                let buf = render_over_seed(w, h, |f, a| t.render(f, a));
                let b = takeover_box(Rect::new(0, 0, w, h));
                for y in 0..h {
                    for x in 0..w {
                        let cell = &buf[(x, y)];
                        let inside = x >= b.x && x < b.right() && y >= b.y && y < b.bottom();
                        if inside {
                            assert_eq!(
                                cell.bg,
                                Color::Reset,
                                "{name} {w}x{h}: cell ({x},{y}) inside the box must have the default bg"
                            );
                        } else {
                            assert_ne!(
                                cell.bg,
                                Color::Reset,
                                "{name} {w}x{h}: cell ({x},{y}) outside the box must stay tinted"
                            );
                            assert_ne!(
                                cell.bg, SEED_BG,
                                "{name} {w}x{h}: cell ({x},{y}) outside the box must be tinted"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nothing_is_drawn_outside_the_box_except_the_tint() {
        for (name, t) in every_variant_state() {
            for (w, h) in TERMINAL_SIZES {
                let buf = render_over_seed(w, h, |f, a| t.render(f, a));
                let b = takeover_box(Rect::new(0, 0, w, h));
                for y in 0..h {
                    for x in 0..w {
                        let inside = x >= b.x && x < b.right() && y >= b.y && y < b.bottom();
                        if !inside {
                            assert_eq!(
                                buf[(x, y)].symbol(),
                                SEED_SYMBOL,
                                "{name} {w}x{h}: art spilled to ({x},{y}) outside the box"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_variant_draws_the_same_box_at_a_given_terminal_size() {
        for (w, h) in TERMINAL_SIZES {
            let mut boxes: Vec<(String, Vec<(u16, u16)>)> = Vec::new();
            for (name, t) in every_variant_state() {
                let buf = render_over_seed(w, h, |f, a| t.render(f, a));
                // The cleared rectangle, found from the buffer itself.
                let mut reset_cells = Vec::new();
                for y in 0..h {
                    for x in 0..w {
                        if buf[(x, y)].bg == Color::Reset {
                            reset_cells.push((x, y));
                        }
                    }
                }
                boxes.push((name, reset_cells));
            }
            let (first_name, first) = &boxes[0];
            for (name, cells) in &boxes {
                assert_eq!(
                    cells, first,
                    "{name} drew a different box than {first_name} at {w}x{h}"
                );
            }
            let b = takeover_box(Rect::new(0, 0, w, h));
            assert_eq!(first.len(), (b.width * b.height) as usize, "{w}x{h}");
        }
    }

    #[test]
    fn every_variant_draws_something_inside_the_box() {
        for (name, t) in every_variant_state() {
            let buf = render_over_seed(134, 40, |f, a| t.render(f, a));
            let i = takeover_interior(Rect::new(0, 0, 134, 40));
            let mut painted = 0;
            for y in i.y..i.bottom() {
                for x in i.x..i.right() {
                    if buf[(x, y)].symbol().trim().is_empty() {
                        continue;
                    }
                    painted += 1;
                }
            }
            assert!(
                painted > 10,
                "{name}: expected art inside the box, found {painted} glyph cells"
            );
        }
    }

    #[test]
    fn no_right_side_border_glyphs_appear_in_the_box() {
        for (name, t) in every_variant_state() {
            for (w, h) in TERMINAL_SIZES {
                let buf = render_over_seed(w, h, |f, a| t.render(f, a));
                let b = takeover_box(Rect::new(0, 0, w, h));
                for y in b.y..b.bottom() {
                    for x in b.x..b.right() {
                        let sym = buf[(x, y)].symbol();
                        assert!(
                            !matches!(sym, "\u{2557}" | "\u{255D}" | "\u{2551}"),
                            "{name} {w}x{h}: double-line right-side glyph {sym:?} at ({x},{y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn takeover_renders_do_not_panic_on_tiny_terminals() {
        for w in 0..=12u16 {
            for h in 0..=6u16 {
                for (_, t) in every_variant_state() {
                    let _ = render_over_seed(w, h, |f, a| t.render(f, a));
                }
            }
        }
    }

    /// A line wider than the box interior is cut off at the box edge. It is
    /// not wrapped onto the next row, and nothing lands outside the box.
    #[test]
    fn a_line_wider_than_the_interior_is_clipped_not_wrapped() {
        let wide = "X".repeat(200);
        let buf = render_over_seed(134, 40, |f, a| {
            render_takeover_frame(
                f,
                a,
                "",
                "T",
                Color::Rgb(255, 0, 255),
                vec![Line::raw(wide.clone()), Line::raw("second")],
            );
        });
        let b = takeover_box(Rect::new(0, 0, 134, 40));
        let row_text = |y: u16| -> String {
            (0..134u16)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };
        // Row 0 of the interior (below the title row) holds the clipped
        // wide line.
        let first = row_text(b.y + 1);
        assert_eq!(first.matches('X').count(), 71, "wide line missing: {first}");
        // The second line is on the next row, and the X run did not wrap.
        let second = row_text(b.y + 2);
        assert!(second.contains("second"), "second line missing: {second}");
        assert!(
            !second.contains('X'),
            "wide line wrapped onto the next row: {second}"
        );
        // Nothing outside the box columns.
        for y in 0..40u16 {
            for x in 0..134u16 {
                if x < b.x || x >= b.right() {
                    assert_eq!(buf[(x, y)].symbol(), SEED_SYMBOL, "spill at ({x},{y})");
                }
            }
        }
    }

    /// More lines than the interior has rows are cut at the bottom border,
    /// and the border itself stays intact.
    #[test]
    fn lines_taller_than_the_interior_are_clipped_at_the_bottom_border() {
        let many: Vec<Line<'static>> = (0..100).map(|i| Line::raw(format!("row {i}"))).collect();
        let buf = render_over_seed(134, 40, |f, a| {
            render_takeover_frame(f, a, "", "T", Color::Rgb(255, 0, 255), many.clone());
        });
        let b = takeover_box(Rect::new(0, 0, 134, 40));
        assert_eq!(
            buf[(b.x, b.bottom() - 1)].symbol(),
            "\u{2514}",
            "bottom-left corner lost"
        );
        for y in b.bottom()..40 {
            for x in 0..134u16 {
                assert_eq!(
                    buf[(x, y)].symbol(),
                    SEED_SYMBOL,
                    "spill below the box at ({x},{y})"
                );
            }
        }
        // The bottom border row holds the border only, no art text.
        let last: String = (b.x + 1..b.right())
            .map(|x| buf[(x, b.bottom() - 1)].symbol().to_string())
            .collect();
        assert!(
            !last.contains("row"),
            "art drawn over the bottom border: {last}"
        );
    }

    // ---- Per-variant sizing: art is composed for the box ----
    //
    // The frame clips whatever a variant draws, so a variant that still sized
    // its art from the whole screen would look fine in the "nothing spills"
    // tests above and simply lose the part of its art that lands past the box
    // edge. These tests check that the art survives whole inside the box.

    /// Text of the box rows (left border column included), one string per row.
    fn box_rows(buf: &ratatui::buffer::Buffer, w: u16, h: u16) -> Vec<String> {
        let b = takeover_box(Rect::new(0, 0, w, h));
        (b.y..b.bottom())
            .map(|y| {
                (b.x..b.right())
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    fn box_text(t: &Takeover, w: u16, h: u16) -> String {
        let buf = render_over_seed(w, h, |f, a| t.render(f, a));
        box_rows(&buf, w, h).join("\n")
    }

    #[test]
    fn quiet_notice_message_sits_whole_inside_the_box_on_a_big_terminal() {
        let t = Takeover::QuietNotice(QuietNoticeTakeover::new(&full_ev()));
        let text = box_text(&t, 134, 40);
        assert!(
            text.contains("tt-smi -r \u{2014} all chips"),
            "message cut off or off-box:\n{text}"
        );
        assert!(
            text.contains("resetting"),
            "status line cut off or off-box:\n{text}"
        );
    }

    #[test]
    fn blackhole_swarm_fills_the_box_not_the_screen() {
        let ev = full_ev(); // 4 chips -> 128 glyphs
        let t = Takeover::BlackholeSwarm(BlackholeSwarmTakeover::new(&ev));
        let text = box_text(&t, 134, 40);
        let glyphs = text.matches('\u{a4}').count();
        // Placed across the 71x22 interior, nearly all 128 glyphs are visible
        // (a few can land on the same cell). Placed across the screen, about a
        // third would fall outside the box and be clipped.
        assert!(
            glyphs >= 110,
            "only {glyphs} of 128 swarm glyphs are inside the box"
        );
    }

    #[test]
    fn missile_command_lanes_are_sized_to_fit_the_box_width() {
        // 32 idle lanes: sized from the 71-column interior they are 2 columns
        // wide and all 32 silo markers fit. Sized from a 134-column screen
        // they would be 4 wide and half would be clipped away.
        let ev = ResetEvent {
            pid: 1,
            is_full: false,
            chip_count: 0,
            total_devices: 32,
            device_indices: vec![],
            raw_targets: vec![],
        };
        let t = Takeover::MissileCommand(MissileCommandTakeover::new(&ev));
        let text = box_text(&t, 134, 40);
        assert_eq!(
            text.matches('\u{b7}').count(),
            32,
            "silo markers lost:\n{text}"
        );
    }

    #[test]
    fn fail_whale_lands_inside_the_box_on_a_tall_terminal() {
        let mut inner = FailWhaleTakeover::new(&full_ev());
        inner.tick(Duration::from_millis(500));
        inner.note_reset_finished();
        inner.tick(Duration::from_secs(3));
        let t = Takeover::FailWhale(inner);
        let text = box_text(&t, 134, 60);
        assert!(
            text.contains("~~~~~~~~"),
            "ground line past the box bottom:\n{text}"
        );
        assert!(
            text.contains("...______..."),
            "whale belly past the box bottom:\n{text}"
        );
    }

    #[test]
    fn bbs_scrolls_its_chip_list_to_keep_the_closing_line_inside_the_box() {
        let ev = ResetEvent {
            pid: 1,
            is_full: true,
            chip_count: 40,
            total_devices: 40,
            device_indices: (0..40).collect(),
            raw_targets: vec![],
        };
        let mut inner = BbsTakeover::new(&ev);
        inner.tick(Duration::from_millis(500));
        inner.note_reset_finished();
        let t = Takeover::Bbs(inner);
        let text = box_text(&t, 134, 40);
        assert!(
            text.contains("CONNECTION RESTORED."),
            "closing line clipped away:\n{text}"
        );
        assert!(
            text.contains("CHIP 39"),
            "newest chip line missing:\n{text}"
        );
    }
}
