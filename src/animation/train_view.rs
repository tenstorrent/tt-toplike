// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Training view — a live tt-train run drawn as a character-grid network
//! being fed tokens, with loss "mountains" against an aurora nightscape.
//!
//! ## The colour language
//!
//! Each channel carries a different real signal — nothing is decorative:
//!
//! | Channel | Encodes |
//! |---|---|
//! | node/mountain hue magenta→teal | loss magnitude |
//! | amber sweep left→right | forward pass |
//! | violet sweep right→left | backward pass / gradients |
//! | per-column river hue | the run's history (each column keeps its own loss's hue) |
//! | mint ▼ / coral ▲ | loss delta direction |
//! | cyan→green→amber→red | chip temperature (the app's existing ramp) |
//! | bar density █▓▒░· | chip power draw |
//! | violet shimmer → dim | kernel cache compiling → steady |
//! | mint burst + comet | checkpoint saved |
//!
//! Everything here is driven by what tt-train actually prints plus
//! tt-toplike's own chip telemetry; no metric is invented.

use crate::animation::common::hsv_to_rgb;
use crate::animation::inference_load::{fmt_elapsed, group_thousands};
use crate::animation::train_sky::sky_cell;
use crate::backend::TelemetryBackend;
use crate::ui::colors;
use crate::workload::train::{LogSource, TrainState};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::cell::Cell as StdCell;

/// Loss → hue: 158° (teal, converged) … 325° (magenta, chaotic).
pub fn loss_hue(loss: f32) -> f32 {
    let t = ((loss - 0.30) / 4.3).clamp(0.0, 1.0);
    158.0 + t * 167.0
}

const FWD_HUE: f32 = 42.0;
const BWD_HUE: f32 = 268.0;

/// Muted violet-blue used for the frame border everywhere it appears.
const BORDER: Color = Color::Rgb(90, 90, 130);

/// Consecutive `render()` calls with no growth in `cache_entries` before the
/// cache indicator settles from "climbing" to "steady". A single stalled
/// tick doesn't mean compilation is done — a few in a row does.
const CACHE_STEADY_TICKS: u32 = 4;

/// The monitor's checkpoint-pulse window (`CKPT_PULSE_TICKS` in
/// `workload::train::monitor`, not re-exported — it's an animation-speed
/// constant, not part of the state contract). Mirrored here so the comet's
/// progress (`1 - pulse/window`) tracks one full pulse release.
const CKPT_PULSE_WINDOW: f32 = 40.0;

/// Compact magnitude suffix: `11.2M`, `1.20B`, `640K`. Distinct from
/// `fmt_bytes` (binary/KiB units) — this is a plain decimal parameter count.
fn format_count(n: u64) -> String {
    let f = n as f64;
    if n >= 1_000_000_000 {
        format!("{:.2}B", f / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", f / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", f / 1e3)
    } else {
        n.to_string()
    }
}

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    fg: Color,
    bold: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Rgb(22, 29, 38),
            bold: false,
        }
    }
}

/// Row boundaries for the content bands between the title bar and the
/// bottom border. Computed fresh from `(width, height)` on every render
/// call — cheap, and keeps every `draw_*` method agreeing on where things
/// go without threading a dozen extra parameters through `render()`.
#[derive(Clone, Copy)]
struct Layout {
    /// First row of the model-card / network / live-stats band.
    network_top: usize,
    /// Height, in rows, of that band (including its own header row).
    network_h: usize,
    /// First row of the loss river (below the network band).
    river_top: usize,
    /// One past the last row of the river (the row the "low/high" labels
    /// and the blank spacer before CHIPS occupy).
    river_bottom: usize,
    /// Row the CHIPS line is drawn on.
    chips_row: usize,
    /// Row the legend is drawn on, directly above the bottom border.
    legend_row: usize,
}

pub struct TrainView {
    width: usize,
    height: usize,
    frame: u64,
    /// Last-observed `cache_entries`, tracked across render calls so the
    /// LIVE panel can report the real climbing→steady derivative instead of
    /// guessing from the step count. `render(&self, …)` can't hold a plain
    /// field for this — `Cell` is the standard escape hatch for
    /// "mutate a little state from an otherwise-immutable render pass"
    /// without changing any prescribed method signature.
    cache_last: StdCell<u32>,
    /// Consecutive render calls since `cache_entries` last grew.
    cache_steady_ticks: StdCell<u32>,
}

impl TrainView {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width: width.max(20),
            height: height.max(10),
            frame: 0,
            cache_last: StdCell::new(0),
            cache_steady_ticks: StdCell::new(0),
        }
    }

    pub fn update(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn render(&self, st: &TrainState, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let mut buf = vec![vec![Cell::default(); self.width]; self.height];
        self.draw_frame(&mut buf);

        match st.proc.as_ref() {
            None => self.draw_scanning(&mut buf),
            Some(_) => {
                self.draw_header(&mut buf, st);
                match st.log.as_ref() {
                    Some(LogSource::File(_)) => {
                        // Side panels degrade gracefully rather than
                        // overlapping at narrow widths: the model card and
                        // live-stats panel are dropped, in that priority
                        // order, whenever there isn't room for all three
                        // columns — the network grid and the river (the
                        // visual centerpiece) always get drawn.
                        let (show_model, show_live) = self.panel_fit();
                        if show_model {
                            self.draw_model_card(&mut buf, st);
                        }
                        self.draw_network(&mut buf, st);
                        if show_live {
                            self.draw_live_stats(&mut buf, st);
                        }
                        self.draw_river(&mut buf, st);
                    }
                    _ => self.draw_no_log_notice(&mut buf, st),
                }
                self.draw_chips(&mut buf, backend);
                self.draw_legend(&mut buf, st);
            }
        }
        self.to_lines(buf)
    }

    // ── the pieces (see defrag.rs for the same buffer→Line shape) ──
    // NOTE TO IMPLEMENTER: each `draw_*` writes into `buf`; keep every write
    // bounds-checked via `put`, and never emit ╗ or ╝ (left+bottom borders
    // only). The reference mockup for exact glyphs and layout is
    // docs/superpowers/specs/2026-08-29-training-view-design.md.

    fn put(&self, buf: &mut [Vec<Cell>], x: usize, y: usize, ch: char, fg: Color, bold: bool) {
        if y < self.height && x < self.width {
            buf[y][x] = Cell { ch, fg, bold };
        }
    }

    fn text(&self, buf: &mut [Vec<Cell>], x: usize, y: usize, s: &str, fg: Color, bold: bool) {
        for (i, ch) in s.chars().enumerate() {
            self.put(buf, x + i, y, ch, fg, bold);
        }
    }

    /// Truncate `s` to at most `w` characters — used everywhere a panel
    /// column has a fixed width, so a long value never bleeds into its
    /// neighbour (writes past the screen edge are already safe via `put`,
    /// but this keeps adjacent *panels* from overlapping each other).
    fn clip(s: &str, w: usize) -> String {
        s.chars().take(w).collect()
    }

    /// Column width of the left-hand MODEL/BATCH card.
    fn left_w(&self) -> usize {
        (self.width / 5).clamp(14, 26)
    }

    /// Column width of the right-hand LIVE stats panel.
    fn right_w(&self) -> usize {
        (self.width / 4).clamp(18, 30)
    }

    /// Which of the two side panels fit at the current width, in priority
    /// order (model card first, live-stats second) — checked *before* any
    /// panel is drawn, rather than drawn-then-clipped, since an overlapping
    /// draw would corrupt whichever panel is drawn first.
    ///
    /// `show_model` requires room for: a 2-col left margin, the model
    /// card, a 1-col gap, a minimum 4-col network, another 1-col gap, the
    /// live panel, and a trailing 1-col margin. `show_live` alone (with the
    /// model card dropped) needs the same minus the card's width.
    fn panel_fit(&self) -> (bool, bool) {
        let left = self.left_w();
        let right = self.right_w();
        let show_model = self.width > 2 + left + 1 + 4 + 1 + right;
        let show_live = show_model || self.width > 2 + 4 + 1 + right;
        (show_model, show_live)
    }

    /// `(x0, w)` of the network grid — the space between whichever side
    /// panels are actually being drawn (see `panel_fit`).
    fn network_bounds(&self) -> (usize, usize) {
        let (show_model, show_live) = self.panel_fit();
        let x0 = if show_model { 2 + self.left_w() + 1 } else { 2 };
        let right_reserved = if show_live { self.right_w() } else { 0 };
        let w = self.width.saturating_sub(x0 + right_reserved + 1).max(4);
        (x0, w)
    }

    fn layout(&self) -> Layout {
        // Row 0/1 are the title + subtitle bars, row `height-1` is the
        // bottom border — everything else is shared out between the
        // network band, the river (which gets whatever's left over, since
        // it's the visual centerpiece), CHIPS, and the legend.
        let legend_row = self.height.saturating_sub(2);
        let chips_row = legend_row.saturating_sub(2);
        let network_top = 3;
        // Tall terminals give the network enough rows to space its heads out
        // (see `draw_network`'s `row_stride`); short ones fall back. Capped at
        // half the content band either way so the river — the centerpiece —
        // always keeps the larger share.
        let content_h = chips_row.saturating_sub(network_top + 3);
        let network_h = 13usize.min(content_h / 2).max(2).min(content_h.max(2));
        let river_top = network_top + network_h + 1;
        let river_bottom = chips_row.saturating_sub(1).max(river_top + 1);
        Layout {
            network_top,
            network_h,
            river_top,
            river_bottom,
            chips_row,
            legend_row,
        }
    }

    fn draw_frame(&self, buf: &mut [Vec<Cell>]) {
        // Left border down every content row except the title bar (row 0
        // draws its own `╔`) and the bottom border row (drawn below).
        for y in 1..self.height.saturating_sub(1) {
            self.put(buf, 0, y, '║', BORDER, false);
        }
        let last = self.height - 1; // height >= 10, guaranteed by `new`.
        self.put(buf, 0, last, '╚', BORDER, false);
        for x in 1..self.width {
            self.put(buf, x, last, '═', BORDER, false);
        }
    }

    fn draw_scanning(&self, buf: &mut [Vec<Cell>]) {
        let dim = Color::Rgb(150, 155, 175);

        self.put(buf, 0, 0, '╔', BORDER, false);
        self.text(buf, 1, 0, "═[ TRAINING ]", BORDER, true);
        for x in 14..self.width {
            self.put(buf, x, 0, '═', BORDER, false);
        }

        // Idle nightscape behind the scanning message — even with nothing
        // to monitor yet, the negative space stays alive.
        let top = 1;
        let bottom = self.height.saturating_sub(1);
        let span = bottom.saturating_sub(top).max(1);
        for y in top..bottom {
            let rel_y = (y - top) as f32 / span as f32;
            for x in 1..self.width {
                if let Some(sc) = sky_cell(x, y, rel_y, self.frame) {
                    self.put(buf, x, y, sc.ch, sc.color, false);
                }
            }
        }

        let title = "SCANNING FOR TRAINING";
        let tx = self.width.saturating_sub(title.chars().count()) / 2;
        let mid = self.height / 2;
        self.text(
            buf,
            tx.max(2),
            mid.saturating_sub(2),
            title,
            Color::Rgb(220, 200, 255),
            true,
        );

        let checklist = [
            "· /proc scan for tt-train binaries",
            "· tt-train binaries (nano_gpt, mnist_mlp, linear_regression)",
            "· /proc/<pid>/fd/1 → regular file?",
            "· log tail",
        ];
        for (i, line) in checklist.iter().enumerate() {
            let y = mid + i;
            if y >= bottom {
                break;
            }
            self.text(
                buf,
                3,
                y,
                &Self::clip(line, self.width.saturating_sub(4)),
                dim,
                false,
            );
        }
    }

    fn draw_header(&self, buf: &mut [Vec<Cell>], st: &TrainState) {
        let bright = Color::Rgb(230, 230, 255);
        let binary = st
            .proc
            .as_ref()
            .map(|p| p.binary.as_str())
            .unwrap_or("training");
        let pid = st.proc.as_ref().map(|p| p.pid).unwrap_or(0);

        self.put(buf, 0, 0, '╔', BORDER, false);
        let left = format!("═[ TRAINING ]  {binary} · pid {pid} ");
        let left_cols = left.chars().count();
        self.text(
            buf,
            1,
            0,
            &Self::clip(&left, self.width.saturating_sub(1)),
            BORDER,
            true,
        );
        let mut x = 1 + left_cols;
        while x < self.width {
            self.put(buf, x, 0, '═', BORDER, false);
            x += 1;
        }

        // Right-aligned loss + delta — drawn after the dash fill so it wins,
        // but never allowed to start before the run's name/pid ends. Without
        // this floor, a long binary name (`linear_regression` is a real
        // tt-train example) plus a multi-digit pid can run right up against
        // — or past — where the loss readout wants to start, and the loss
        // text (drawn last) would overwrite the very thing the header exists
        // to identify.
        if let Some(loss) = st.loss {
            let color = hsv_to_rgb(loss_hue(loss), 0.75, 0.85);
            let loss_part = format!("LOSS {loss:.4}  ");
            let (delta_str, delta_color) = match st.prev_loss {
                Some(prev) => {
                    let d = loss - prev;
                    if d <= 0.0 {
                        (format!("▼{:.4}", -d), Color::Rgb(120, 230, 190))
                    } else {
                        (format!("▲{d:.4}"), Color::Rgb(255, 140, 120))
                    }
                }
                None => (String::new(), Color::Reset),
            };
            let total = loss_part.chars().count() + delta_str.chars().count();
            // Leave at least a 1-col gap after the run's name before the
            // loss readout is allowed to start.
            let min_rx = 1 + left_cols + 1;
            let rx = self.width.saturating_sub(total + 1).max(min_rx);
            self.text(buf, rx, 0, &loss_part, color, true);
            self.text(
                buf,
                rx + loss_part.chars().count(),
                0,
                &delta_str,
                delta_color,
                true,
            );
        }

        // Second header row: how we attached, plus the step counter.
        let attach_desc = match st.log.as_ref() {
            Some(LogSource::File(p)) => format!(" auto-attached  {}", p.display()),
            Some(LogSource::NotRedirected) => " auto-attached  stdout not redirected".to_string(),
            None => " scanning for log".to_string(),
        };
        let right_w = if st.max_steps > 0 { 26 } else { 0 };
        let left_room = self.width.saturating_sub(right_w + 2);
        self.text(
            buf,
            1,
            1,
            &Self::clip(&attach_desc, left_room),
            Color::Rgb(150, 160, 190),
            false,
        );

        if st.max_steps > 0 {
            let pct = (st.step as f64 / st.max_steps as f64 * 100.0).clamp(0.0, 100.0);
            let step_str = format!(
                "step {} / {}  {pct:.1}%",
                group_thousands(st.step as usize),
                group_thousands(st.max_steps as usize),
            );
            let sx = self.width.saturating_sub(step_str.chars().count() + 1);
            self.text(buf, sx, 1, &step_str, bright, false);
        }
    }

    fn draw_no_log_notice(&self, buf: &mut [Vec<Cell>], st: &TrainState) {
        let dim = Color::Rgb(160, 165, 185);
        let warn = Color::Rgb(255, 190, 120);
        let binary = st
            .proc
            .as_ref()
            .map(|p| p.binary.as_str())
            .unwrap_or("The training run");
        let row = 4;
        let w = self.width.saturating_sub(4);
        self.text(
            buf,
            2,
            row,
            &Self::clip(
                &format!("{binary} is running, but its output isn't visible here."),
                w,
            ),
            dim,
            false,
        );
        self.text(
            buf,
            2,
            row + 2,
            &Self::clip(
                "stdout is not redirected to a file — relaunch with '> train.log' for per-step metrics",
                w,
            ),
            warn,
            true,
        );
        self.text(
            buf,
            2,
            row + 4,
            &Self::clip(
                "process, chip telemetry, and checkpoint mtime are still tracked below.",
                w,
            ),
            dim,
            false,
        );
    }

    // The macro's final `y += 1` in each of these two methods is never read
    // afterward — harmless (it's only ever the last line drawn), but the
    // compiler can't see that across an early-return-free macro expansion.
    #[allow(unused_assignments)]
    fn draw_model_card(&self, buf: &mut [Vec<Cell>], st: &TrainState) {
        let Layout {
            network_top,
            network_h,
            ..
        } = self.layout();
        let max_y = network_top + network_h;
        let card_w = self.left_w().saturating_sub(1);
        let label = Color::Rgb(140, 200, 255);
        let val = Color::Rgb(210, 210, 230);
        let mut y = network_top;

        macro_rules! line {
            ($text:expr, $color:expr, $bold:expr) => {
                if y < max_y {
                    let s = Self::clip(&$text, card_w);
                    self.text(buf, 2, y, &s, $color, $bold);
                    y += 1;
                }
            };
        }

        line!("MODEL", label, true);
        if st.param_count > 0 {
            line!(
                format!("params {}", format_count(st.param_count)),
                val,
                false
            );
        }
        if let Some(n) = st.config.num_blocks {
            line!(format!("blocks {n}"), val, false);
        }
        if let Some(n) = st.config.num_heads {
            line!(format!("heads {n}"), val, false);
        }
        if let Some(n) = st.config.embedding_dim {
            line!(format!("d_model {n}"), val, false);
        }
        if let Some(n) = st.config.vocab_size {
            line!(format!("vocab {n}"), val, false);
        }
        if let Some(n) = st.config.max_sequence_length {
            line!(format!("seq {n}"), val, false);
        }
        if st.batch_size > 0 || st.config.learning_rate.is_some() {
            line!("BATCH", label, true);
            if st.batch_size > 0 {
                let accum = if st.grad_accum > 1 {
                    format!(" x{}", st.grad_accum)
                } else {
                    String::new()
                };
                line!(format!("size {}{accum}", st.batch_size), val, false);
            }
            if let Some(lr) = st.config.learning_rate {
                line!(format!("lr {lr:.1e}"), val, false);
            }
        }
    }

    fn draw_network(&self, buf: &mut [Vec<Cell>], st: &TrainState) {
        let (x0, w) = self.network_bounds();
        let Layout {
            network_top,
            network_h,
            ..
        } = self.layout();
        let label = Color::Rgb(150, 200, 255);
        // The grid's row/column counts are decorative shape only — when the
        // real config is unknown we still need *some* shape to animate, so
        // default to nanollama3's 6x6. But the header text is a claim about
        // the actual model, so it must never assert those defaulted numbers
        // as fact: only print counts when both are genuinely known.
        let blocks = st.config.num_blocks.unwrap_or(6).max(1) as usize;
        let heads = st.config.num_heads.unwrap_or(6).max(1) as usize;

        let topology_label = match (st.config.num_blocks, st.config.num_heads) {
            (Some(b), Some(h)) => format!("{b} blocks × {h} heads"),
            _ => "topology unknown".to_string(),
        };

        self.text(
            buf,
            x0,
            network_top,
            &Self::clip(&format!("tokens →   [ {topology_label} ]   ∇ gradients"), w),
            label,
            false,
        );

        // ── Grid sizing ──────────────────────────────────────────────
        // The band has spare width at typical terminal sizes, so the grid
        // breathes: pick the widest per-block stride that still fits, and
        // put a blank row between head rows when the band is tall enough.
        // Both back off to the old tight packing on small terminals — the
        // river below is the centerpiece and never gives up height for this.
        let max_rows = network_h.saturating_sub(1).max(1);
        let usable_w = w.saturating_sub(1);

        // stride = columns consumed per block (1 node + stride-1 connectors).
        // `(cols-1)*stride + 1` must fit `usable_w`.
        let cols_at = |stride: usize| -> usize {
            if usable_w == 0 {
                return 1;
            }
            (usable_w.saturating_sub(1) / stride + 1).max(1)
        };
        let stride = [5usize, 4, 3]
            .into_iter()
            .find(|&s| cols_at(s) >= blocks)
            .unwrap_or(2);
        let cols = blocks.min(cols_at(stride)).max(1);

        // Row stride 2 (a blank row between heads) only when every head still
        // fits; otherwise stay at 1 rather than truncating the network.
        let row_stride = if heads > 0 && (heads - 1) * 2 < max_rows {
            2
        } else {
            1
        };
        let rows = heads
            .min((max_rows.saturating_sub(1)) / row_stride + 1)
            .max(1);

        let node_glyphs = ['●', '◉', '○', '◇', '·'];

        for r in 0..rows {
            let y = network_top + 1 + r * row_stride;
            for c in 0..cols {
                let x = x0 + c * stride;
                if x >= x0 + w {
                    break;
                }
                // Forward pass sweeps left→right, backward right→left, both
                // paced by `self.frame` — a lit node briefly outshines the
                // resting loss-hue coloring.
                let period = cols + 4;
                let fwd_on = (self.frame as usize / 2 + c) % period < 2;
                let bwd_on = (self.frame as usize / 2 + (cols - 1 - c)) % period < 2;

                // Connectors run from this node toward the next block. They
                // light with whichever sweep is passing, so the pulse reads as
                // travelling *through* the network rather than hopping nodes.
                if c + 1 < cols {
                    for k in 1..stride {
                        let cx = x + k;
                        if cx >= x0 + w {
                            break;
                        }
                        // Fan the middle of a wide gap into ╱/╲ so adjacent
                        // heads look wired together, not just railed straight.
                        let ch = if stride >= 4 && k == stride / 2 {
                            if (r + c) % 2 == 0 {
                                '╲'
                            } else {
                                '╱'
                            }
                        } else {
                            '─'
                        };
                        let (col, bold) = if fwd_on {
                            (hsv_to_rgb(FWD_HUE, 0.75, 0.7), true)
                        } else if bwd_on {
                            (hsv_to_rgb(BWD_HUE, 0.6, 0.65), true)
                        } else {
                            (Color::Rgb(80, 90, 115), false)
                        };
                        self.put(buf, cx, y, ch, col, bold);
                    }
                }

                let glyph = node_glyphs[(r + c + (self.frame as usize / 6)) % node_glyphs.len()];
                let color = if fwd_on {
                    hsv_to_rgb(FWD_HUE, 0.85, 0.95)
                } else if bwd_on {
                    hsv_to_rgb(BWD_HUE, 0.7, 0.9)
                } else if let Some(loss) = st.loss {
                    hsv_to_rgb(loss_hue(loss), 0.55, 0.6)
                } else {
                    Color::Rgb(90, 100, 130)
                };
                self.put(buf, x, y, glyph, color, fwd_on || bwd_on);
            }
        }
    }

    #[allow(unused_assignments)]
    fn draw_live_stats(&self, buf: &mut [Vec<Cell>], st: &TrainState) {
        let Layout {
            network_top,
            network_h,
            ..
        } = self.layout();
        let max_y = network_top + network_h;
        let rw = self.right_w();
        let rx = self.width.saturating_sub(rw + 1);
        let val = Color::Rgb(210, 230, 220);
        let label = Color::Rgb(150, 220, 200);
        let mut y = network_top;

        macro_rules! line {
            ($text:expr, $color:expr, $bold:expr) => {
                if y < max_y {
                    let s = Self::clip(&$text, rw);
                    self.text(buf, rx, y, &s, $color, $bold);
                    y += 1;
                }
            };
        }

        line!("LIVE", label, true);
        if let Some(tps) = st.tokens_per_sec() {
            line!(
                format!("tok/s   {}", group_thousands(tps.round().max(0.0) as usize)),
                val,
                false
            );
        }
        let sps = st.steps_per_sec();
        if sps > 0.0 {
            line!(format!("step/s  {sps:.2}"), val, false);
        }
        if st.step > 0 {
            // The real derivative, not a step-count guess: compare this
            // tick's `cache_entries` against the last render's (tracked in
            // `self.cache_last`/`self.cache_steady_ticks`, both `Cell`s —
            // see the field docs on `TrainView`). Climbing while it keeps
            // growing; steady only once it's held flat for
            // `CACHE_STEADY_TICKS` renders in a row, so one stalled tick
            // (e.g. between two log lines) doesn't flip it prematurely.
            let prev = self.cache_last.replace(st.cache_entries);
            if st.cache_entries > prev {
                self.cache_steady_ticks.set(0);
            } else {
                self.cache_steady_ticks
                    .set(self.cache_steady_ticks.get().saturating_add(1));
            }
            let climbing =
                st.cache_entries > 0 && self.cache_steady_ticks.get() < CACHE_STEADY_TICKS;
            let (txt, color) = if climbing {
                (
                    format!("cache   {} climbing", st.cache_entries),
                    Color::Rgb(180, 140, 230),
                )
            } else {
                (
                    format!("cache   {} steady", st.cache_entries),
                    Color::Rgb(120, 120, 140),
                )
            };
            line!(txt, color, false);
        }
        if let Some(first_seen) = st.first_seen {
            let elapsed = fmt_elapsed(first_seen.elapsed().as_secs());
            let eta = st
                .eta_secs()
                .map(|e| fmt_elapsed(e.max(0.0) as u64))
                .unwrap_or_else(|| "—".to_string());
            line!(format!("watching {elapsed}  eta {eta}"), val, false);
        }
        if st.checkpoint_step > 0 || st.checkpoint_pulse > 0 {
            let color = if st.checkpoint_pulse > 0 {
                Color::Rgb(120, 230, 190)
            } else {
                Color::Rgb(150, 150, 170)
            };
            line!(
                format!("ckpt @ {}", group_thousands(st.checkpoint_step as usize)),
                color,
                true
            );
        }
    }

    /// The comet released across the sky while a checkpoint pulse is
    /// active (`st.checkpoint_pulse > 0`) — `Some((glyph, color))` for a
    /// head or trail cell at this column, `None` otherwise, including when
    /// the mountain at this column would be tall enough to hide it (clipped
    /// against the skyline, per the design doc).
    fn comet_glyph_at(
        &self,
        x: usize,
        sky_rows: usize,
        mountain_full_bars: usize,
        pulse: u8,
    ) -> Option<(char, Color)> {
        if pulse == 0 {
            return None;
        }
        let usable_w = self.width.saturating_sub(3).max(1);
        let progress = (1.0 - pulse as f32 / CKPT_PULSE_WINDOW).clamp(0.0, 1.0);
        let head_x = 2 + (progress * usable_w as f32) as usize;
        let glyph = if x == head_x {
            '✦'
        } else if x + 2 == head_x {
            '∙'
        } else if x + 4 == head_x {
            '·'
        } else {
            return None;
        };
        // The comet flies near the top of the sky band; if the mountain at
        // this column is tall enough to reach that row, it's hidden rather
        // than drawn on top of the peak.
        let comet_row_from_bottom = sky_rows.saturating_sub(2);
        if comet_row_from_bottom < mountain_full_bars {
            return None;
        }
        Some((glyph, Color::Rgb(150, 235, 205)))
    }

    fn draw_river(&self, buf: &mut [Vec<Cell>], st: &TrainState) {
        let Layout {
            river_top,
            river_bottom,
            ..
        } = self.layout();
        if river_bottom <= river_top + 1 {
            return;
        }
        let label_row = river_top.saturating_sub(1);
        self.text(
            buf,
            2,
            label_row,
            &Self::clip(
                "LOSS  · mountains colored by their own value",
                self.width.saturating_sub(3),
            ),
            Color::Rgb(160, 170, 200),
            false,
        );

        let sky_rows = river_bottom - river_top;

        if st.loss_history.is_empty() {
            // Nothing to plot yet — keep the region alive with the same
            // nightscape rather than an empty void. No mountains exist yet,
            // so a checkpoint comet (if one is in flight) is never clipped.
            for x in 1..self.width {
                for y in river_top..river_bottom {
                    let rel_y = (y - river_top) as f32 / sky_rows.max(1) as f32;
                    if let Some(sc) = sky_cell(x, y, rel_y, self.frame) {
                        self.put(buf, x, y, sc.ch, sc.color, false);
                    }
                }
                if let Some((glyph, color)) =
                    self.comet_glyph_at(x, sky_rows, 0, st.checkpoint_pulse)
                {
                    let row_from_bottom = sky_rows.saturating_sub(2);
                    let y = river_bottom.saturating_sub(1 + row_from_bottom);
                    self.put(buf, x, y, glyph, color, true);
                }
            }
            return;
        }

        let hist = &st.loss_history;
        let (min_loss, max_loss) = hist
            .iter()
            .fold((f32::MAX, f32::MIN), |(mn, mx), &l| (mn.min(l), mx.max(l)));
        let range = (max_loss - min_loss).max(0.001);
        let total_eighths = sky_rows * 8;
        let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let usable_w = self.width.saturating_sub(3).max(1);

        for x in 2..self.width {
            let rel = (x - 2) as f32 / usable_w as f32;
            let idx = ((rel * (hist.len() - 1) as f32).round() as usize).min(hist.len() - 1);
            let loss = hist[idx];
            let norm = ((loss - min_loss) / range).clamp(0.0, 1.0);
            let eighths = (norm * total_eighths as f32).round() as usize;
            let color = hsv_to_rgb(loss_hue(loss), 0.8, 0.75);
            let full_bars = eighths / 8;
            let partial = eighths % 8;

            for row_from_bottom in 0..sky_rows {
                let y = river_bottom - 1 - row_from_bottom;
                if row_from_bottom < full_bars {
                    self.put(buf, x, y, '█', color, false);
                } else if row_from_bottom == full_bars && partial > 0 {
                    self.put(buf, x, y, bars[partial - 1], color, false);
                } else {
                    // Sky above the mountain line — `rel_y` runs 0 at the
                    // top of the band, 1 at the mountain line, so it's the
                    // inverse of `row_from_bottom`.
                    let rel_y = 1.0 - (row_from_bottom as f32 / sky_rows.max(1) as f32);
                    if let Some(sc) = sky_cell(x, y, rel_y, self.frame) {
                        self.put(buf, x, y, sc.ch, sc.color, false);
                    }
                }
            }

            // Checkpoint comet: drawn after the mountain/sky for this column
            // so it wins over the aurora, but clipped against this column's
            // own mountain height.
            if let Some((glyph, color)) =
                self.comet_glyph_at(x, sky_rows, full_bars, st.checkpoint_pulse)
            {
                let row_from_bottom = sky_rows.saturating_sub(2);
                let y = river_bottom - 1 - row_from_bottom;
                self.put(buf, x, y, glyph, color, true);
            }
        }

        if river_bottom < self.height.saturating_sub(1) {
            self.text(
                buf,
                2,
                river_bottom,
                &format!("{min_loss:.2} low"),
                Color::Rgb(140, 220, 190),
                false,
            );
            let high = format!("high {max_loss:.2}");
            let hx = self.width.saturating_sub(high.chars().count() + 1);
            self.text(
                buf,
                hx,
                river_bottom,
                &high,
                Color::Rgb(255, 150, 130),
                false,
            );
        }
    }

    fn draw_chips(&self, buf: &mut [Vec<Cell>], backend: &dyn TelemetryBackend) {
        let y = self.layout().chips_row;
        let label = Color::Rgb(180, 200, 255);
        self.text(buf, 2, y, "CHIPS", label, true);
        let mut x = 8;
        for device in backend.devices() {
            if x + 6 >= self.width {
                break;
            }
            let idx = device.index;
            let temp = backend.telemetry(idx).map(|t| t.temp_c()).unwrap_or(0.0);
            let power = backend.telemetry(idx).map(|t| t.power_w()).unwrap_or(0.0);
            let tcolor = colors::temp_color(temp);
            let tag = format!("dev{idx} {temp:.1}°C {power:.0}W ");
            let tag = Self::clip(&tag, self.width.saturating_sub(x + 1));
            self.text(buf, x, y, &tag, tcolor, false);
            x += tag.chars().count();

            let bar_w = 10usize.min(self.width.saturating_sub(x + 2));
            // Density bar: fraction of a rough 200W per-chip ceiling, with a
            // partial-fill glyph at the fading edge so the bar reads
            // smoothly instead of a hard step.
            let frac = (power / 200.0).clamp(0.0, 1.0);
            for i in 0..bar_w {
                let level = frac * bar_w as f32 - i as f32;
                let ch = if level >= 1.0 {
                    '█'
                } else if level >= 0.75 {
                    '▓'
                } else if level >= 0.5 {
                    '▒'
                } else if level >= 0.25 {
                    '░'
                } else {
                    '·'
                };
                self.put(buf, x + i, y, ch, tcolor, false);
            }
            x += bar_w + 2;
        }
    }

    fn draw_legend(&self, buf: &mut [Vec<Cell>], st: &TrainState) {
        let y = self.layout().legend_row;
        let loss_color = st
            .loss
            .map(|l| hsv_to_rgb(loss_hue(l), 0.75, 0.85))
            .unwrap_or(Color::Rgb(200, 150, 220));
        let entries: [(char, &str, Color); 9] = [
            ('●', "loss", loss_color),
            ('─', "forward", hsv_to_rgb(FWD_HUE, 0.85, 0.9)),
            ('∙', "gradients", hsv_to_rgb(BWD_HUE, 0.7, 0.85)),
            ('▼', "loss ↓", Color::Rgb(120, 230, 190)),
            ('▲', "loss ↑", Color::Rgb(255, 140, 120)),
            ('█', "chip temp", colors::temp_color(70.0)),
            ('▓', "chip power", Color::Rgb(200, 200, 120)),
            ('✦', "checkpoint", Color::Rgb(120, 230, 190)),
            ('░', "aurora", Color::Rgb(120, 180, 150)),
        ];
        let mut x = 2;
        for (glyph, label, color) in entries {
            if x + 3 >= self.width.saturating_sub(1) {
                break;
            }
            self.put(buf, x, y, glyph, color, false);
            x += 2;
            let remaining = self.width.saturating_sub(x + 2);
            let text = Self::clip(label, remaining);
            let n = text.chars().count();
            self.text(buf, x, y, &text, Color::Rgb(170, 170, 190), false);
            x += n + 2;
        }
    }

    fn to_lines(&self, buf: Vec<Vec<Cell>>) -> Vec<Line<'static>> {
        buf.into_iter()
            .map(|row| {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut run = String::new();
                let mut cur: Option<(Color, bool)> = None;
                for c in row {
                    let key = (c.fg, c.bold);
                    if Some(key) != cur {
                        if !run.is_empty() {
                            let (fg, b) = cur.unwrap();
                            let mut st = Style::default().fg(fg);
                            if b {
                                st = st.add_modifier(Modifier::BOLD);
                            }
                            spans.push(Span::styled(std::mem::take(&mut run), st));
                        }
                        cur = Some(key);
                    }
                    run.push(c.ch);
                }
                if !run.is_empty() {
                    let (fg, b) = cur.unwrap_or((Color::Reset, false));
                    let mut st = Style::default().fg(fg);
                    if b {
                        st = st.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(run, st));
                }
                Line::from(spans)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::backend::TelemetryBackend;
    use crate::workload::train::TrainState;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn loss_hue_runs_magenta_when_high_to_teal_when_converged() {
        let hot = loss_hue(4.5);
        let cool = loss_hue(0.35);
        assert!(hot > 300.0, "high loss should be magenta-ish, got {hot}");
        assert!(
            (150.0..180.0).contains(&cool),
            "converged loss should be teal-ish, got {cool}"
        );
        // Clamped outside the observed range rather than wrapping around.
        assert!((loss_hue(99.0) - loss_hue(4.6)).abs() < 1.0);
        assert!((loss_hue(-1.0) - loss_hue(0.30)).abs() < 1.0);
    }

    #[test]
    fn format_count_thresholds() {
        assert_eq!(format_count(640), "640");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1.0K");
        assert_eq!(format_count(11_200_000), "11.2M");
        assert_eq!(format_count(1_200_000_000), "1.20B");
    }

    #[test]
    fn with_no_process_it_says_it_is_scanning_and_draws_no_metrics() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let v = TrainView::new(120, 40);
        let out = text_of(&v.render(&TrainState::new(), &b));
        assert!(
            out.contains("SCANNING"),
            "expected a scanning state:\n{out}"
        );
        assert!(
            !out.contains("LOSS  "),
            "must not draw a loss panel with no data:\n{out}"
        );
    }

    #[test]
    fn an_unredirected_stdout_explains_itself_instead_of_faking_a_curve() {
        use crate::workload::train::{LogSource, TrainProcess};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 4242,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::NotRedirected);

        let v = TrainView::new(120, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(
            out.contains("nano_gpt"),
            "should still name the run:\n{out}"
        );
        assert!(
            out.to_lowercase().contains("redirect"),
            "must explain why per-step metrics are missing:\n{out}"
        );
        // A regression that drew the notice *and* a fake/empty curve would
        // still pass the two assertions above — this is the discriminator.
        assert!(
            !out.contains("LOSS  "),
            "must not draw a loss panel when the log isn't redirected:\n{out}"
        );
        assert!(
            !out.contains("mountains colored"),
            "must not draw the river label when there's no data to plot:\n{out}"
        );
    }

    #[test]
    fn a_live_run_draws_its_numbers() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 48213,
            binary: "nano_gpt".into(),
            config_path: Some("t.yaml".into()),
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(TrainEvent::MaxSteps(50000));
        st.apply_event(TrainEvent::BatchSize(64));
        for i in 1..=60u64 {
            st.apply_event(TrainEvent::Step {
                step: i,
                loss: 4.5 - (i as f32) * 0.05,
            });
        }
        st.apply_event(TrainEvent::StepTime {
            ms: 1000.0,
            cache_entries: 21,
        });

        let v = TrainView::new(134, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(out.contains("nano_gpt"));
        assert!(out.contains("48213"), "pid should be shown:\n{out}");
        assert!(out.contains("50,000"), "max steps should be shown:\n{out}");
        assert!(out.contains("LOSS"), "loss panel should be present:\n{out}");
    }

    #[test]
    fn header_loss_readout_never_overwrites_the_run_name() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = TrainState::new();
        // `linear_regression` is a real tt-train example binary (it's in
        // this file's own scanning checklist) and a 5-digit pid is
        // realistic — this combination is what pushes the run's name past
        // where the loss readout wants to start at a narrow width.
        st.proc = Some(TrainProcess {
            pid: 48213,
            binary: "linear_regression".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(TrainEvent::Step {
            step: 1,
            loss: 4.55,
        });
        st.apply_event(TrainEvent::Step {
            step: 2,
            loss: 4.50,
        });

        let v = TrainView::new(60, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(
            out.contains("linear_regression · pid 48213"),
            "long binary name + pid must survive intact in the header, not be \
             overwritten by the right-aligned loss readout:\n{out}"
        );
    }

    #[test]
    fn side_panels_never_invert_at_any_width() {
        // Direct check of the layout invariant itself (not text-scraping):
        // whichever side panels `panel_fit` says are shown, the network
        // grid's own bounds must never reach into them.
        for w in 20..=140usize {
            let v = TrainView::new(w, 40);
            let (show_model, show_live) = v.panel_fit();
            let (x0, netw) = v.network_bounds();
            let net_end = x0 + netw;
            if show_live {
                let rx = w.saturating_sub(v.right_w() + 1);
                assert!(
                    net_end <= rx,
                    "network overruns the LIVE panel at w={w}: x0={x0} netw={netw} rx={rx}"
                );
            } else {
                assert!(
                    net_end <= w,
                    "network overruns the screen at w={w}: x0={x0} netw={netw}"
                );
            }
            if show_model {
                assert!(x0 >= 3, "model card column missing room at w={w}");
            } else {
                assert_eq!(x0, 2, "network should start at the left margin at w={w}");
            }
        }
    }

    #[test]
    fn narrow_widths_drop_side_panels_instead_of_garbling_them() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: Some("t.yaml".into()),
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(TrainEvent::MaxSteps(1000));
        for i in 1..=10u64 {
            st.apply_event(TrainEvent::Step { step: i, loss: 3.0 });
        }

        for w in [20usize, 30] {
            let v = TrainView::new(w, 40);
            let (show_model, show_live) = v.panel_fit();
            assert!(
                !show_model,
                "model card should be dropped, not squeezed, at w={w}"
            );
            let out = text_of(&v.render(&st, &b));
            assert!(
                !out.contains("MODEL"),
                "dropped model card must not appear at w={w}:\n{out}"
            );
            if !show_live {
                assert!(
                    !out.contains("LIVE"),
                    "dropped live panel must not appear at w={w}:\n{out}"
                );
            }
            for line in v.render(&st, &b) {
                let s: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
                assert!(!s.contains('╗') && !s.contains('╝'), "w={w}: {s:?}");
                let cols = unicode_width::UnicodeWidthStr::width(s.as_str());
                assert!(cols <= w, "line is {cols} cols at width {w}: {s:?}");
            }
        }
    }

    #[test]
    fn cache_reads_climbing_then_steady_from_the_real_derivative_not_a_step_count() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(TrainEvent::Step { step: 1, loss: 4.0 });
        st.apply_event(TrainEvent::StepTime {
            ms: 10.0,
            cache_entries: 5,
        });

        let v = TrainView::new(134, 40);
        let out1 = text_of(&v.render(&st, &b));
        assert!(
            out1.contains("climbing"),
            "a freshly-growing cache should read climbing:\n{out1}"
        );

        // The cache stops growing for several renders in a row — this must
        // settle to steady from the real observed plateau, not from any
        // fixed step-count threshold (the old, replaced heuristic).
        let mut out_last = out1;
        for _ in 0..(CACHE_STEADY_TICKS + 2) {
            out_last = text_of(&v.render(&st, &b));
        }
        assert!(
            out_last.contains("steady"),
            "a plateaued cache must read steady, not stay climbing forever:\n{out_last}"
        );
    }

    #[test]
    fn a_checkpoint_pulse_releases_a_comet_across_the_sky() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        for i in 1..=20u64 {
            st.apply_event(TrainEvent::Step {
                step: i,
                loss: 3.0 - i as f32 * 0.05,
            });
        }
        st.checkpoint_pulse = 20;
        st.checkpoint_step = 20;

        let v = TrainView::new(134, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(
            out.contains('✦') || out.contains('∙'),
            "a checkpoint pulse should release a visible comet:\n{out}"
        );
    }

    #[test]
    fn never_emits_a_right_side_border_and_fits_the_width() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        for i in 1..=200u64 {
            st.apply_event(TrainEvent::Step { step: i, loss: 2.0 });
        }

        // Narrow terminals are where wrapping bugs show up.
        for w in [60usize, 80, 100, 134] {
            let v = TrainView::new(w, 30);
            for line in v.render(&st, &b) {
                let s: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
                // No right-side border glyphs anywhere, and any `║` that
                // does appear must be the left border at column 0.
                assert!(
                    !s.contains('╗') && !s.contains('╝'),
                    "right-side corner characters are forbidden (w={w}): {s:?}"
                );
                for (pos, _) in s.match_indices('║') {
                    assert_eq!(pos, 0, "`║` must only appear at column 0 (w={w}): {s:?}");
                }
                let cols = unicode_width::UnicodeWidthStr::width(s.as_str());
                assert!(cols <= w, "line is {cols} cols at width {w}: {s:?}");
            }
        }
    }

    /// The grid should breathe on a normal terminal: blocks spaced several
    /// columns apart with visible connectors between them, and — when the
    /// band is tall enough — a blank row between head rows. A regression to
    /// the old tight `node,connector,node` packing (2 columns per block,
    /// rows touching) would make the network read as a solid clump, so this
    /// asserts the spacing directly rather than eyeballing it.
    #[test]
    fn network_grid_is_spaced_out_on_a_roomy_terminal() {
        use crate::workload::train::{LogSource, TrainProcess};
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.config.num_blocks = Some(6);
        st.config.num_heads = Some(6);
        st.loss = Some(2.0);

        let v = TrainView::new(134, 40);
        let lines = v.render(&st, &b);
        let rows: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        // Scope to the network band: everything above the river's "LOSS"
        // label. The legend row also mixes node glyphs with `─`, so an
        // unscoped scan would count it as a seventh head row.
        let river_label = rows
            .iter()
            .position(|r| r.contains("LOSS  ·"))
            .expect("river label row should render");
        let node_rows: Vec<usize> = rows
            .iter()
            .enumerate()
            .take(river_label)
            .filter(|(_, r)| r.contains('─') && r.chars().any(|c| "●◉○◇·".contains(c)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            node_rows.len(),
            6,
            "all six head rows should render; got {node_rows:?}"
        );

        // Vertical: consecutive head rows are two apart (a blank row between).
        for pair in node_rows.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                2,
                "head rows should be spaced on a 40-row terminal; got {node_rows:?}"
            );
        }

        // Horizontal: at least two connector chars between adjacent nodes.
        // The old packing put exactly one, so `──` never appeared.
        let first = &rows[node_rows[0]];
        assert!(
            first.contains("──"),
            "expected multi-column connectors between blocks, got {first:?}"
        );
    }

    #[test]
    fn network_header_never_fabricates_topology_it_cannot_source() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let v = TrainView::new(100, 30);

        // Unknown config (no readable YAML): the header must not assert a
        // specific block/head count, fabricated or otherwise.
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(TrainEvent::Step { step: 1, loss: 2.0 });
        assert_eq!(st.config.num_blocks, None);
        assert_eq!(st.config.num_heads, None);
        let out: String = v
            .render(&st, &b)
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !out.contains("6 blocks"),
            "must not assert a fabricated block/head count when config is unknown:\n{out}"
        );
        assert!(
            out.contains("topology unknown"),
            "should say the topology is unknown rather than guess:\n{out}"
        );

        // Known config: the header should state the real numbers.
        let mut st2 = TrainState::new();
        st2.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st2.log = Some(LogSource::File("/tmp/x.log".into()));
        st2.apply_event(TrainEvent::Step { step: 1, loss: 2.0 });
        st2.config.num_blocks = Some(12);
        st2.config.num_heads = Some(8);
        let out2: String = v
            .render(&st2, &b)
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            out2.contains("12 blocks × 8 heads"),
            "known topology should be stated exactly:\n{out2}"
        );
    }
}
