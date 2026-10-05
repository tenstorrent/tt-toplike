// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! BBS sysop takeover — "the sysop wants to chat." The sysop is a chatbot
//! from the early days of AI, and it types out a short scripted conversation
//! (see [`CHAT`]) in the manner of ELIZA, Dr. Sbaitso and the WOPR from
//! WarGames. The chat is flavor only: it never says the reset has finished
//! until it has, and the real per-chip lines below it stay the real status.
//! A classic ANSI-art
//! terminal interrupt: fixed limited palette (bright green base, sparse
//! cyan/yellow accents — no hue-cycling, that's BBS's alone to have given
//! up, not everyone else's to inherit), a box-drawing border for real
//! texture, modem-connect flavor lines, and each new chip status line
//! typing out character-by-character in real time (a classic terminal
//! typewriter effect) rather than popping in fully formed. Loops while the
//! real reset is still in progress rather than racing ahead of it.

use super::{render_takeover_frame, takeover_interior, TakeoverClock};
use crate::ui::colors;
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

const DONE_TAIL: Duration = Duration::from_millis(700);
const BEAT_MS: u128 = 400;
/// How long a newly-appearing chip line takes to finish "typing" — well
/// under `BEAT_MS`, so it sits fully typed for the rest of its beat before
/// the next line begins.
const TYPE_MS: u128 = 150;
/// Banner blink half-cycle: a real ANSI blink (two fixed states), not a
/// continuous hue rotation.
const BLINK_MS: u128 = 500;
/// Rows above the chip lines (3 flavor lines, a blank, the banner, a blank).
const HEADER_ROWS: usize = 6;
/// Rows reserved below the chip lines for "CONNECTION RESTORED.".
const CLOSING_ROWS: usize = 1;
/// Rows the ANSI box adds (top and bottom edge).
const BOX_ROWS: usize = 2;
/// Rows the chat window can use when there is room for it.
const CHAT_ROWS: usize = 5;
/// A new chat line starts this often while the reset is in progress.
const CHAT_LINE_MS: u128 = 1000;
/// Typing speed of a chat line, in characters per second.
const CHAT_CPS: u128 = 60;

/// Who speaks a chat line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Speaker {
    Sysop,
    You,
}

/// The conversation, in the style of three old AI harnesses:
/// - WarGames' WOPR: the greeting and the "strange game" ending.
/// - ELIZA: turning each answer back into a question ("WHY ARE YOU...?",
///   "HOW DOES ... MAKE ... FEEL?", "TELL ME MORE ABOUT ...").
/// - Dr. Sbaitso: the cheerful "I am here to help" manner, and the line about
///   memory being wiped when you leave, which is literally what a reset does.
///
/// Every line is at most 62 characters so the chat fits the box interior.
const CHAT: [(Speaker, &str); 8] = [
    (
        Speaker::Sysop,
        "SYSOP> GREETINGS, PROFESSOR. SHALL WE PLAY A GAME?",
    ),
    (Speaker::You, "YOU>   NO. I AM RESETTING THE CHIPS."),
    (Speaker::Sysop, "SYSOP> WHY ARE YOU RESETTING THE CHIPS?"),
    (Speaker::You, "YOU>   THEY ARE STUCK."),
    (
        Speaker::Sysop,
        "SYSOP> HOW DOES BEING STUCK MAKE THE CHIPS FEEL?",
    ),
    (Speaker::You, "YOU>   THEIR MEMORY WILL BE WIPED."),
    (
        Speaker::Sysop,
        "SYSOP> I AM HERE TO HELP. TELL ME MORE ABOUT THEIR MEMORY.",
    ),
    (
        Speaker::Sysop,
        "SYSOP> A STRANGE GAME. THE ONLY WINNING MOVE IS NOT TO RESET.",
    ),
];
/// Shown only once the real reset has finished.
const CHAT_GOODBYE: (Speaker, &str) = (Speaker::Sysop, "SYSOP> GOODBYE, PROFESSOR.");

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

    /// How many "CHIP N: reset issued" lines should be visible right now,
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

    /// Fraction (0.0-1.0) of the most-recently-appeared chip line's text
    /// that should be visible right now. Only meaningful when there's more
    /// than one chip line to accumulate — with exactly one, there's never a
    /// genuinely "new" line to type, so it stays fully shown throughout.
    fn typing_progress(&self) -> f32 {
        if self.chip_count <= 1 {
            return 1.0;
        }
        let within_beat = self.clock.elapsed().as_millis() % BEAT_MS;
        (within_beat as f32 / TYPE_MS as f32).min(1.0)
    }

    /// Rows the chat window gets for an interior `interior_height` rows tall:
    /// [`CHAT_ROWS`] when there is room, fewer on a short box, and always
    /// leaving at least one row for a chip line.
    fn chat_rows(interior_height: usize) -> usize {
        CHAT_ROWS.min(interior_height.saturating_sub(HEADER_ROWS + CLOSING_ROWS + BOX_ROWS + 1))
    }

    /// The last `rows` chat lines as they should look right now, padded with
    /// blank lines to exactly `rows` so the chip lines below never move as the
    /// chat fills. Lines start [`CHAT_LINE_MS`] apart and type at
    /// [`CHAT_CPS`]; the newest line of a running chat ends in a blinking
    /// block cursor. Once the real reset has finished every line is shown in
    /// full, followed by the goodbye.
    fn chat_lines(&self, rows: usize) -> Vec<Line<'static>> {
        if rows == 0 {
            return Vec::new();
        }
        let finished = !self.clock.in_progress();
        let elapsed = self.clock.elapsed().as_millis();
        let dim_green = colors::rgb(20, 140, 60);
        let cyan = colors::rgb(60, 200, 210);

        // (speaker, text so far), oldest first.
        let mut shown: Vec<(Speaker, String)> = Vec::new();
        for (i, (who, text)) in CHAT.iter().enumerate() {
            let start = i as u128 * CHAT_LINE_MS;
            if !finished && elapsed < start {
                break;
            }
            // The speaker tag ("SYSOP> ") appears at once; only the message
            // after it types out.
            let tag = text.find('>').map_or(0, |i| {
                i + 1 + text[i + 1..].chars().take_while(|c| *c == ' ').count()
            });
            let typed = if finished {
                usize::MAX
            } else {
                ((elapsed - start) * CHAT_CPS / 1000) as usize
            };
            shown.push((*who, text.chars().take(tag.saturating_add(typed)).collect()));
        }
        if finished {
            shown.push((CHAT_GOODBYE.0, CHAT_GOODBYE.1.to_string()));
        } else if (self.clock.elapsed().as_millis() / BLINK_MS) % 2 == 0 {
            if let Some((_, text)) = shown.last_mut() {
                text.push('\u{2588}');
            }
        }

        let first = shown.len().saturating_sub(rows);
        let mut lines: Vec<Line<'static>> = shown
            .into_iter()
            .skip(first)
            .map(|(who, text)| {
                let color = if who == Speaker::Sysop {
                    cyan
                } else {
                    dim_green
                };
                Line::from(Span::styled(text, Style::default().fg(color)))
            })
            .collect();
        while lines.len() < rows {
            lines.push(Line::from(Span::raw("")));
        }
        lines
    }

    /// Build the (unboxed-by-the-frame) screen lines, fitted to
    /// `interior_height` rows: the 6 header rows, the chat window, the chip
    /// lines and the closing line, plus the 2 rows of the ANSI box. When there are more
    /// chip lines than fit, only the newest ones are shown, like a terminal
    /// scrolling.
    fn lines(&self, interior_height: usize) -> Vec<Line<'static>> {
        let finished = !self.clock.in_progress();
        let green = colors::rgb(40, 220, 90);
        let cyan = colors::rgb(60, 200, 210);
        let dim_green = colors::rgb(20, 140, 60);
        let yellow = colors::rgb(255, 210, 70);
        let yellow_dim = colors::rgb(120, 100, 40);

        let blink_on = (self.clock.elapsed().as_millis() / BLINK_MS) % 2 == 0;
        let banner_color = if finished {
            green
        } else if blink_on {
            yellow
        } else {
            yellow_dim
        };

        let mut lines: Vec<Line<'static>> = vec![
            Line::from(Span::styled(
                "RING... RING...",
                Style::default().fg(dim_green),
            )),
            Line::from(Span::styled(
                "CONNECT 14400",
                Style::default().fg(dim_green),
            )),
            Line::from(Span::styled(
                "NODE 1 - TENSTORRENT BBS",
                Style::default().fg(cyan),
            )),
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                "** SYSOP HAS TAKEN OVER THIS TERMINAL **",
                Style::default().fg(banner_color),
            )),
            Line::from(Span::raw("")),
        ];

        let chat_rows = Self::chat_rows(interior_height);
        lines.extend(self.chat_lines(chat_rows));

        let visible = self.visible_lines();
        let typing_progress = self.typing_progress();
        let max_chip_lines = interior_height
            .saturating_sub(HEADER_ROWS + chat_rows + CLOSING_ROWS + BOX_ROWS)
            .max(1);
        let first_shown = visible
            .min(self.device_indices.len())
            .saturating_sub(max_chip_lines);
        for (i, &real_chip) in self
            .device_indices
            .iter()
            .take(visible)
            .enumerate()
            .skip(first_shown)
        {
            let full_text = format!("CHIP {real_chip}: reset issued...");
            let is_newest = i + 1 == visible && self.clock.in_progress();
            let text = if is_newest && typing_progress < 1.0 {
                let total_chars = full_text.chars().count();
                let shown = ((total_chars as f32) * typing_progress).ceil() as usize;
                full_text.chars().take(shown.max(1)).collect()
            } else {
                full_text
            };
            lines.push(Line::from(Span::styled(text, Style::default().fg(green))));
        }
        if finished {
            lines.push(Line::from(Span::styled(
                "CONNECTION RESTORED.",
                Style::default().fg(green),
            )));
        }

        box_it(lines, cyan)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        self.render_tagged(f, area, "");
    }

    /// Draws the takeover with `tag` (for example `DEMO`) in front of the
    /// box title. An empty `tag` draws the plain title.
    pub fn render_tagged(&self, f: &mut Frame, area: Rect, tag: &str) {
        let cyan = colors::rgb(60, 200, 210);
        let lines = self.lines(takeover_interior(area).height as usize);

        let title = if self.is_full {
            "SYSTEM-WIDE RESET"
        } else {
            "PARTIAL RESET"
        };
        render_takeover_frame(f, area, tag, title, cyan, lines);
    }
}

/// Pad every line to the same width (see `TrekResetTakeover` for why —
/// `Paragraph`'s `Alignment::Center` centers each `Line` independently, so
/// without this every line would get a different left indent), then wrap
/// the whole block in an ANSI box-drawing border for real terminal-art
/// texture.
fn box_it(mut lines: Vec<Line<'static>>, border_color: Color) -> Vec<Line<'static>> {
    let inner_width = lines.iter().map(Line::width).max().unwrap_or(0).max(20);
    for line in &mut lines {
        let deficit = inner_width.saturating_sub(line.width());
        if deficit > 0 {
            line.spans.push(Span::raw(" ".repeat(deficit)));
        }
    }
    let border_style = Style::default().fg(border_color);
    let mut boxed = Vec::with_capacity(lines.len() + 2);
    boxed.push(Line::from(Span::styled(
        format!("┌{}┐", "─".repeat(inner_width + 2)),
        border_style,
    )));
    for line in lines {
        let mut spans = vec![Span::styled("│ ", border_style)];
        spans.extend(line.spans);
        spans.push(Span::styled(" │", border_style));
        boxed.push(Line::from(spans));
    }
    boxed.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(inner_width + 2)),
        border_style,
    )));
    boxed
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
    /// With `chip_count == 1` there's no typewriter reveal (nothing new
    /// ever accumulates), so the full text is present from the first tick.
    #[test]
    fn render_labels_the_real_targeted_chip_not_loop_position() {
        let mut t = BbsTakeover::new(&ev_subset(1, vec![2]));
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

    /// The newest chip line (multi-chip case) must actually type out over
    /// real time — a partial reveal shortly after it appears, the full
    /// text once its type window has elapsed.
    #[test]
    fn newest_chip_line_types_out_over_real_time() {
        let mut t = BbsTakeover::new(&ev_subset(2, vec![0, 7]));
        // beat 0: only "CHIP 0" visible. Advance into beat 1, just after it
        // starts, so "CHIP 7" is the newest line and is still mid-type.
        t.tick(Duration::from_millis(BEAT_MS as u64 + 20));
        let mid_type = rendered_text(&t);
        assert!(
            !mid_type.contains("CHIP 7: reset issued..."),
            "the newest line should not be fully typed yet, 20ms into a 150ms type window:\n{mid_type}"
        );

        // Well past TYPE_MS within the same beat: fully typed now.
        t.tick(Duration::from_millis((TYPE_MS as u64) + 50));
        let fully_typed = rendered_text(&t);
        assert!(
            fully_typed.contains("CHIP 7: reset issued..."),
            "the line should be fully typed once its type window has elapsed:\n{fully_typed}"
        );
    }

    /// The SYSOP banner blinks between two fixed states while in progress —
    /// not a continuous hue rotation.
    #[test]
    fn banner_blinks_between_two_fixed_states() {
        let mut t = BbsTakeover::new(&ev(true, 4));
        let banner_a = rendered_banner_fg(&t);
        t.tick(Duration::from_millis(BLINK_MS as u64));
        let banner_b = rendered_banner_fg(&t);
        assert_ne!(
            banner_a, banner_b,
            "banner should toggle across a blink boundary"
        );
        t.tick(Duration::from_millis(BLINK_MS as u64));
        let banner_c = rendered_banner_fg(&t);
        assert_eq!(
            banner_a, banner_c,
            "banner should return to its first state, not drift through a spectrum"
        );
    }

    /// Once finished, the banner settles to a steady green and stops
    /// blinking — a clear "back to normal" signal, matching the other
    /// variants' settle-on-finish convention.
    #[test]
    fn banner_settles_to_steady_green_once_finished() {
        let mut t = BbsTakeover::new(&ev(true, 4));
        t.tick(Duration::from_millis(300));
        t.note_reset_finished();
        let a = rendered_banner_fg(&t);
        t.tick(Duration::from_millis(BLINK_MS as u64 * 3));
        let b = rendered_banner_fg(&t);
        assert_eq!(a, b, "banner should stop changing once finished");
    }

    fn rendered_banner_fg(t: &BbsTakeover) -> Color {
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
            if let Some(idx) = row.find('S') {
                if row[idx..].starts_with("SYSOP") {
                    let col = row[..idx].chars().count() as u16;
                    return buf[(col, y)].fg;
                }
            }
        }
        panic!("SYSOP banner not found in rendered buffer");
    }

    /// The box-drawing border must actually be present — real ANSI-art
    /// texture, not just plain text floating on a flat background.
    #[test]
    fn render_wraps_content_in_a_box_border() {
        let t = BbsTakeover::new(&ev(true, 4));
        let painted = rendered_text(&t);
        assert!(
            painted.contains('┌'),
            "expected a box-drawing top-left corner:\n{painted}"
        );
        assert!(
            painted.contains('┘'),
            "expected a box-drawing bottom-right corner:\n{painted}"
        );
    }

    /// The BBS screen must fit the takeover box interior (71x22 at full
    /// size) however many chips are targeted: the chip list scrolls instead
    /// of growing past the box, and the closing line stays visible.
    #[test]
    fn lines_fit_the_box_interior_for_any_chip_count() {
        let interior = crate::animation::takeover::takeover_interior(Rect::new(0, 0, 134, 40));
        for chips in [1usize, 4, 13, 14, 40, 200] {
            let indices: Vec<u8> = (0..chips as u16).map(|i| (i % 250) as u8).collect();
            let mut t = BbsTakeover::new(&ev_subset(chips, indices));
            for finished in [false, true] {
                t.tick(Duration::from_millis(BEAT_MS as u64 * 7 + 300));
                if finished {
                    t.note_reset_finished();
                }
                let lines = t.lines(interior.height as usize);
                let widest = lines.iter().map(Line::width).max().unwrap_or(0);
                assert!(
                    widest <= interior.width as usize,
                    "{chips} chips: widest {widest}"
                );
                assert!(
                    lines.len() <= interior.height as usize,
                    "{chips} chips finished={finished}: {} rows",
                    lines.len()
                );
                let text: String = lines
                    .iter()
                    .map(|l| {
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert_eq!(text.contains("CONNECTION RESTORED."), finished, "{text}");
            }
        }
    }

    // ---- Chat window -----------------------------------------------------

    fn line_text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The sysop opens with the WarGames greeting, typing it out over real
    /// time rather than showing it whole on the first frame.
    #[test]
    fn chat_opens_with_the_wopr_greeting_and_types_it_out() {
        let mut t = BbsTakeover::new(&ev(true, 4));
        t.tick(Duration::from_millis(100));
        let early = line_text(&t.chat_lines(5)[0]);
        assert!(early.starts_with("SYSOP> G"), "{early:?}");
        assert!(!early.contains("SHALL WE PLAY A GAME?"), "{early:?}");
        t.tick(Duration::from_millis(900));
        let later = line_text(&t.chat_lines(5)[0]);
        assert!(later.contains("SHALL WE PLAY A GAME?"), "{later:?}");
    }

    /// The conversation advances one line per CHAT_LINE_MS and holds on its
    /// last line instead of looping, so a long real reset does not replay it.
    #[test]
    fn chat_advances_then_holds_on_the_last_line() {
        let mut t = BbsTakeover::new(&ev(true, 4));
        t.tick(Duration::from_millis(CHAT_LINE_MS as u64 * 2 + 900));
        let text: String = t
            .chat_lines(8)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("WHY ARE YOU RESETTING THE CHIPS?"), "{text}");
        assert!(!text.contains("STRANGE GAME"), "{text}");
        t.tick(Duration::from_secs(60));
        assert!(t.clock.in_progress());
        let end: String = t
            .chat_lines(8)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            end.contains("THE ONLY WINNING MOVE IS NOT TO RESET."),
            "{end}"
        );
        assert!(
            !end.contains("GOODBYE"),
            "no goodbye while still resetting:\n{end}"
        );
    }

    /// ELIZA and Dr. Sbaitso are in the script, and only the real finish
    /// brings the goodbye.
    #[test]
    fn chat_has_its_ai_nods_and_says_goodbye_only_when_finished() {
        let script: String = CHAT.iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n");
        assert!(script.contains("WHY ARE YOU"), "ELIZA-style question");
        assert!(script.contains("I AM HERE TO HELP"), "Dr. Sbaitso manner");
        assert!(script.contains("SHALL WE PLAY A GAME?"), "WOPR greeting");
        let mut t = BbsTakeover::new(&ev(true, 4));
        t.tick(Duration::from_secs(2));
        t.note_reset_finished();
        let done: String = t
            .chat_lines(5)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(done.contains("GOODBYE, PROFESSOR."), "{done}");
    }

    /// The chat window is always exactly the rows it was given, and no chat
    /// line is wider than the box interior, so the chip lines never move.
    #[test]
    fn chat_window_is_a_fixed_height_and_fits_the_interior() {
        assert!(CHAT.iter().all(|(_, l)| l.chars().count() <= 62));
        let t = BbsTakeover::new(&ev(true, 4));
        assert_eq!(t.chat_lines(5).len(), 5);
        assert_eq!(t.chat_lines(0).len(), 0);
        assert_eq!(BbsTakeover::chat_rows(22), 5);
        assert_eq!(BbsTakeover::chat_rows(11), 1);
        assert_eq!(BbsTakeover::chat_rows(10), 0);
        assert_eq!(BbsTakeover::chat_rows(5), 0);
    }
}
