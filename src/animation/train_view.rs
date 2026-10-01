// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Training view — a live tt-train run drawn as a step-time chart over chip power,
//! with loss "mountains" against an aurora nightscape.
//!
//! ## The colour language
//!
//! Each channel carries a different real signal — nothing is decorative:
//!
//! | Channel | Encodes |
//! |---|---|
//! | node/mountain hue red→cyan | loss magnitude (per-cell gradient in the river) |
//! | bar height (step band) | wall time per step, trainer-reported or observed from a progress bar (the title says which) |
//! | bar colour teal / purple / amber | normal step / program cache grew / checkpoint written |
//! | pulse cursor speed | one pass per measured step (never faster than 5/s) |
//! | chip lane height | power as a fraction of that chip's TDP (coral = aiclk dropped) |
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
use crate::animation::inference_load::{fmt_bytes, fmt_elapsed, group_thousands};
use crate::animation::train_sky::sky_cell;
use crate::animation::train_tapestry::{
    advance_phase, bar_cell, cause_of, convergence_parts, diagnose, median, pass_secs, plan_band,
    BandWants, ChipHistory, Diagnosis, DiagnosisKind, Readings, StepCause, AICLK_DROP_FRAC,
    LABEL_W, MAX_LANES,
};
use crate::backend::TelemetryBackend;
use crate::models::Device;
use crate::ui::colors;
use crate::workload::train::{LogSource, StepTimeSource, TrainState};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::cell::{Cell as StdCell, RefCell};

/// The loss a model that has learned nothing would report — the top of the
/// hue ramp, derived from the job rather than fixed.
///
/// For a language model that is `ln(vocab_size)`: cross-entropy against a
/// uniform distribution over the vocabulary. A 32,000-token vocabulary gives
/// 10.373, and no run over that vocabulary can score worse for long. It is the
/// one ceiling that is a property of the *task* rather than of a guess about
/// what losses look like.
///
/// Without a vocabulary — the tt-train regression examples, `linear_regression`
/// and `mnist_mlp`, whose losses live around 0.1–0.5 — there is no such
/// derivation, so the historical fixed anchor stands. That anchor was always
/// right for those runs; it was only ever wrong when applied to a vocabulary.
pub fn loss_ceiling(vocab_size: Option<u32>) -> f32 {
    match vocab_size {
        Some(v) if v > 1 => (v as f32).ln(),
        _ => LEGACY_LOSS_CEILING,
    }
}

/// The pre-2026-08-31 fixed ceiling, kept for runs with no vocabulary.
const LEGACY_LOSS_CEILING: f32 = 4.6;

/// Loss → hue: 186° (deep cyan) … 356° (red), scaled to `ceiling`.
///
/// The ramp runs cyan → blue → violet → magenta → pink → red and stays
/// monotonic in loss, so the colour remains readable as a value rather than
/// decoration.
///
/// WHY THE CEILING IS A PARAMETER. It used to be the constants `0.30` and
/// `4.3`, which put "converged" at a loss of 0.30 and saturated at 4.60. For a
/// language model over a 32k vocabulary both ends are wrong, and measurably so:
/// 0.30 is unreachable — it would need near-certainty on every token, and the
/// best result tt-tnt has ever recorded is 1.73 — while 4.60 is *below* the
/// loss of an untrained model (10.37), so the entire early phase of a run,
/// where the loss falls fastest, rendered as one flat red. The visualisation
/// was blind exactly where the run was most dynamic.
///
/// One fixed ramp cannot serve both a regression example whose loss is 0.2 and
/// a language model whose loss starts above 10. Scaling to the job's own
/// ceiling is what lets a single ramp do both: the run enters at red and walks
/// the spectrum down as it learns, whatever it is training.
pub fn loss_hue(loss: f32, ceiling: f32) -> f32 {
    let ceiling = if ceiling > 0.0 {
        ceiling
    } else {
        LEGACY_LOSS_CEILING
    };
    let t = (loss / ceiling).clamp(0.0, 1.0);
    186.0 + t * 170.0
}

/// How far a mountain's hue drifts as it undulates, in degrees. Small
/// enough that the colour still reads as its loss value — the wobble is
/// meant to make the range feel alive, not to blur what it encodes.
const MOUNTAIN_HUE_WOBBLE: f32 = 14.0;

/// Extra hue the crest of a column carries over its base, in degrees. This
/// is what makes each column a gradient rather than a flat block: the base
/// sits deep and dark, the crest lifts toward the warm end.
const MOUNTAIN_HUE_LIFT: f32 = 26.0;

/// HSV for one mountain cell: `depth` is 0 at the column's base and 1 at its
/// crest, `x` its screen column, `t` elapsed seconds.
///
/// Split out of the renderer so the gradient can be asserted directly. The
/// obvious alternative — reading colours back off rendered spans — silently
/// depends on the terminal: `colors::rgb` returns `Color::Indexed` rather
/// than `Color::Rgb` when the environment reports no true-colour support, so
/// such a test passes on a developer's terminal and fails in CI.
fn mountain_hsv(base_hue: f32, depth: f32, x: usize, t: f32) -> (f32, f32, f32) {
    let wob = (x as f32 * 0.055 + t * 0.4 + depth * 1.9).sin();
    let hue = (base_hue + depth * MOUNTAIN_HUE_LIFT + wob * MOUNTAIN_HUE_WOBBLE).rem_euclid(360.0);
    // Deep and near-fully saturated at the base, easing off as it rises so
    // the crest glows rather than turning to poster ink.
    let sat = (0.95 - depth * 0.28).clamp(0.35, 1.0);
    let val = (0.26 + depth * 0.58 + wob * 0.05).clamp(0.08, 1.0);
    (hue, sat, val)
}

const FWD_HUE: f32 = 42.0;

/// How far behind the sweep head a cell still glows, in sub-columns. The
/// falloff over this distance is what turns a hard lit block into a tail.
const SWEEP_TAIL: f32 = 6.0;

/// How far *ahead* of the head a cell starts brightening. Without this the
/// pulse pops from dark to near-full in a single frame as the head crosses a
/// cell (measured: a 0.95 jump), which reads as a blink rather than an
/// arrival. A short lead-in makes the front edge glide in.
const SWEEP_LEAD: f32 = 3.0;

/// Smoothstep on `t ∈ [0,1]`. Used for both edges of the pulse because its
/// derivative is zero at both ends: a plain `t²` ramp is steepest exactly at
/// the peak, so the frame the head crosses a cell produces the largest jump
/// of the whole pass — the one place it most needs to be smooth.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Glow at sub-column `at` given the head position: brightest under the head,
/// ramping up over [`SWEEP_LEAD`] ahead of it and fading over [`SWEEP_TAIL`]
/// behind, wrapping around `period` so the pulse survives the wrap instead of
/// blinking out. Asymmetric on purpose — a longer tail than lead-in is what
/// makes the motion read directionally.
fn sweep_at(head: f32, at: f32, period: f32) -> f32 {
    if period <= 0.0 {
        return 0.0;
    }
    // Signed distance, wrapped into [-period/2, period/2): positive means the
    // head has already passed this cell, negative means it's approaching.
    let half = period / 2.0;
    let mut d = head - at;
    while d < -half {
        d += period;
    }
    while d >= half {
        d -= period;
    }
    // Smoothstepped falloff on both sides: a bright head with thinning edges
    // reads as motion better than a linear ramp (which looks like a smear),
    // and the flat top means crossing the peak doesn't jolt.
    if (0.0..SWEEP_TAIL).contains(&d) {
        smoothstep(1.0 - d / SWEEP_TAIL)
    } else if (-SWEEP_LEAD..0.0).contains(&d) {
        smoothstep(1.0 - (-d) / SWEEP_LEAD)
    } else {
        0.0
    }
}

/// Muted violet-blue used for the frame border everywhere it appears.
const BORDER: Color = Color::Rgb(90, 90, 130);

/// Step bar colours. Teal, yellow and red are the docs-site brand tints; the
/// compile purple is the LIVE panel's cache colour so one meaning has one hue.
const BAR_NORMAL: Color = Color::Rgb(116, 197, 223);
const BAR_COMPILE: Color = Color::Rgb(180, 140, 230);
const BAR_CHECKPOINT: Color = Color::Rgb(246, 188, 66);
/// A chip lane cell where aiclk had dropped well below that chip's best.
const AICLK_DROP: Color = Color::Rgb(255, 158, 138);
/// Resting colour of the grid backdrop, median line and empty gauge cells.
const WIRE_REST: Color = Color::Rgb(80, 90, 115);
/// Placeholder for a lane column with no chip sample. A glyph used nowhere
/// else, so a test can count missing samples exactly.
const NO_SAMPLE: char = '⋅';
/// Columns between nodes in the grid backdrop.
const GRID_STRIDE: usize = 4;

/// Consecutive `render()` calls with no growth in `cache_entries` before the
/// cache indicator settles from "climbing" to "steady". A single stalled
/// tick doesn't mean compilation is done — a few in a row does.
const CACHE_STEADY_TICKS: u32 = 4;

/// Fraction of the river's height the lowest column still occupies. Without
/// it the window minimum renders as bare ground, and a converged run — where
/// every column *is* the minimum — shows no mountains at all.
const MOUNTAIN_FLOOR: f32 = 0.12;

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
    /// First row of the model-card / tapestry / live-stats band.
    network_top: usize,
    /// Height, in rows, of that band (including its own header row).
    network_h: usize,
    /// First row of the loss river (below the tapestry band).
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
    /// Chip readings per step and best-so-far values for the gauges. Filled
    /// by `sample` during `render(&self, ...)`, so it is a `RefCell` for the
    /// same reason `cache_last` is a `Cell`.
    history: RefCell<ChipHistory>,
    /// Pulse cursor position within its pass, `[0, 1)`. Accumulated across
    /// frames so a change in step time alters the cursor's speed, not its
    /// position (absolute-time phase jumped on every step-time update).
    pulse_phase: StdCell<f32>,
    /// The `frame` the phase was last advanced to.
    pulse_frame: StdCell<u64>,
}

/// One gauge row cell: a label, a fill fraction and the reading in words.
struct Gauge {
    label: &'static str,
    frac: f32,
    text: String,
}

/// The chip drawing the most power right now, standing in for "the chip the
/// training run is on". Every chip the backend can see is a candidate because
/// the trainer's chips are not identified, so an idle neighbour never dilutes
/// the reading the way an average would.
struct Busiest {
    index: usize,
    power_w: f32,
    aiclk_mhz: u32,
    tdp: Option<f32>,
}

impl TrainView {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width: width.max(20),
            height: height.max(10),
            frame: 0,
            cache_last: StdCell::new(0),
            cache_steady_ticks: StdCell::new(0),
            history: RefCell::new(ChipHistory::default()),
            pulse_phase: StdCell::new(0.0),
            pulse_frame: StdCell::new(0),
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
                self.sample(st, backend);
                self.draw_header(&mut buf, st);
                match st.log.as_ref() {
                    Some(LogSource::File(_)) => {
                        // Side panels degrade gracefully rather than
                        // overlapping at narrow widths: the model card and
                        // live-stats panel are dropped, in that priority
                        // order, whenever there isn't room for all three
                        // columns — the tapestry band and the river (the
                        // visual centerpiece) always get drawn.
                        let (show_model, show_live) = self.panel_fit();
                        if show_model {
                            self.draw_model_card(&mut buf, st);
                        }
                        self.draw_tapestry(&mut buf, st, backend);
                        if show_live {
                            self.draw_live_stats(&mut buf, st);
                        }
                        self.draw_river(&mut buf, st);
                    }
                    // The log is unreadable, but the *config* isn't — the
                    // model card and the tapestry band come from it, and
                    // `draw_river` already paints its nightscape when there
                    // is no loss history. Only the mountains genuinely
                    // depend on the stream. Replacing the whole body left
                    // ~30 rows blank between the notice and the chip strip.
                    _ => {
                        let (show_model, show_live) = self.panel_fit();
                        if show_model {
                            self.draw_model_card(&mut buf, st);
                        }
                        self.draw_tapestry(&mut buf, st, backend);
                        // `draw_live_stats` needs no special case: its rows
                        // are emitted only for values that exist, so with no
                        // stream it degrades by itself to cpu / rss /
                        // watching and omits tok/s, step/s, cache and eta.
                        if show_live {
                            self.draw_live_stats(&mut buf, st);
                        }
                        self.draw_river(&mut buf, st);
                        // Drawn last so it sits over the nightscape.
                        self.draw_no_log_notice(&mut buf, st);
                    }
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

    /// The verdict row for a band `w` wide: the full text, else the text
    /// without its `host cpu` clause, else `None`. Never a prefix of either.
    fn verdict_line(d: &Diagnosis, w: usize) -> Option<String> {
        [&d.text, &d.short]
            .into_iter()
            .map(|t| format!("▸ {t}"))
            .find(|t| t.chars().count() <= w)
    }

    /// The leading `parts` that fit in `w` columns whole, joined by two
    /// spaces. Order is kept; the first part that does not fit and every part
    /// after it is dropped, never cut. Empty when not even the first fits.
    fn fit_parts(parts: &[String], w: usize) -> String {
        let mut out = String::new();
        for p in parts {
            let add = if out.is_empty() { 0 } else { 2 } + p.chars().count();
            if out.chars().count() + add > w {
                break;
            }
            if !out.is_empty() {
                out.push_str("  ");
            }
            out.push_str(p);
        }
        out
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
    /// card, a 1-col gap, a minimum 4-col tapestry band, another 1-col gap, the
    /// live panel, and a trailing 1-col margin. `show_live` alone (with the
    /// model card dropped) needs the same minus the card's width.
    fn panel_fit(&self) -> (bool, bool) {
        let left = self.left_w();
        let right = self.right_w();
        let show_model = self.width > 2 + left + 1 + 4 + 1 + right;
        let show_live = show_model || self.width > 2 + 4 + 1 + right;
        (show_model, show_live)
    }

    /// `(x0, w)` of the tapestry band (header, step bars, grid, chip lanes, gauges, diagnosis line, convergence strip) — the space between whichever side
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
        // Tall terminals give the tapestry band enough rows for its step bars,
        // grid row and chip lanes (see `plan_band` in `train_tapestry`); short
        // ones fall back. Capped at
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
        // The hue ramp is scaled to this job's own worst-case loss -- ln(vocab)
        // for a language model -- so a run enters at red and walks the spectrum
        // down as it learns, instead of saturating at a fixed anchor that a
        // 32k-vocab model is above for its entire early phase.
        let ceiling = loss_ceiling(st.config.vocab_size);
        let bright = Color::Rgb(230, 230, 255);
        let binary = st
            .proc
            .as_ref()
            .map(|p| p.binary.as_str())
            .unwrap_or("training");
        let pid = st.proc.as_ref().map(|p| p.pid).unwrap_or(0);

        self.put(buf, 0, 0, '╔', BORDER, false);
        // A fabricated run says so, right in the title. This tool's premise
        // is that every pixel maps to a real signal; synthetic data that
        // looked identical to a real run would quietly break that promise.
        let left = if st.is_mock {
            format!("═[ TRAINING · mock ]  {binary} ")
        } else {
            format!("═[ TRAINING ]  {binary} · pid {pid} ")
        };
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
            let color = hsv_to_rgb(loss_hue(loss, ceiling), 0.75, 0.85);
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
        // Centred in the river band rather than at row 4: the model card and
        // tapestry band occupy the top of the screen now, and the river's
        // nightscape is the space this message used to leave empty.
        let Layout {
            river_top,
            river_bottom,
            ..
        } = self.layout();
        let band = river_bottom.saturating_sub(river_top);
        let row = river_top + band.saturating_sub(5) / 2;
        let w = self.width.saturating_sub(4);

        // Blank the rows this message occupies before writing it. The
        // nightscape is drawn underneath now, and aurora glyphs rendered
        // through the text make it genuinely hard to read — the same failure
        // the explain overlay had, for the same reason: writing a string
        // leaves every cell it doesn't cover untouched.
        for r in row.saturating_sub(1)..=(row + 5).min(river_bottom.saturating_sub(1)) {
            for x in 1..self.width {
                self.put(buf, x, r, ' ', Color::Rgb(22, 29, 38), false);
            }
        }
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

    /// Record this frame's chip readings against the current step, and fold
    /// tokens/sec and PCIe throughput into their best-so-far values.
    fn sample(&self, st: &TrainState, backend: &dyn TelemetryBackend) {
        // A chip with no telemetry is not sampled, so it never gets a lane.
        let chips: Vec<(usize, f32, u32)> = backend
            .devices()
            .iter()
            .filter_map(|d| {
                let t = backend.telemetry(d.index)?;
                Some((d.index, t.power_w(), t.aiclk_mhz()))
            })
            .collect();
        let mut h = self.history.borrow_mut();
        h.record(st.step_seq, &chips);
        if let Some(tps) = st.tokens_per_sec() {
            h.note_tps(tps);
        }
        if let Some(bps) = Self::pcie_total(backend) {
            h.note_pcie(bps);
        }
    }

    /// Summed in+out PCIe bytes/sec across chips that report it, `None` when
    /// no chip does (only the sysfs/hybrid backends have these counters).
    /// Advance the pulse phase to the current frame and return it, or `None`
    /// when no step time is known (the phase is then left alone). Advances by
    /// the frame delta, so repeated renders in one frame do not double-step.
    fn advance_pulse(&self, secs: Option<f32>) -> Option<f32> {
        let secs = secs?;
        let frames = self.frame.saturating_sub(self.pulse_frame.get());
        let phase = advance_phase(
            self.pulse_phase.get(),
            frames,
            crate::animation::train_sky::ANIM_FPS,
            secs,
        );
        self.pulse_phase.set(phase);
        self.pulse_frame.set(self.frame);
        Some(phase)
    }

    fn pcie_total(backend: &dyn TelemetryBackend) -> Option<f64> {
        let mut any = false;
        let mut total = 0.0f64;
        for d in backend.devices() {
            if let Some(bw) = backend.pcie_bandwidth(d.index) {
                any = true;
                total += bw.rx_bytes_per_sec + bw.tx_bytes_per_sec;
            }
        }
        any.then_some(total)
    }

    /// A chip's TDP in watts, from SMBUS telemetry first and the tt-smi
    /// `limits` block second. `None` when neither reports a positive value.
    fn chip_tdp(backend: &dyn TelemetryBackend, device: &Device) -> Option<f32> {
        backend
            .smbus_telemetry(device.index)
            .and_then(|s| s.tdp_watts())
            .or_else(|| device.limits.as_ref().and_then(|l| l.tdp_limit))
            .filter(|v| *v > 0.0)
    }

    /// The busiest chip (most power) among all chips with telemetry, or
    /// `None` when no chip reports any.
    fn busiest_chip(&self, backend: &dyn TelemetryBackend) -> Option<Busiest> {
        backend
            .devices()
            .iter()
            .filter_map(|d| {
                let t = backend.telemetry(d.index)?;
                Some(Busiest {
                    index: d.index,
                    power_w: t.power_w(),
                    aiclk_mhz: t.aiclk_mhz(),
                    tdp: Self::chip_tdp(backend, d),
                })
            })
            .max_by(|a, b| a.power_w.total_cmp(&b.power_w))
    }

    /// The gauges that have a source, in display order. A gauge with no
    /// source is absent, not drawn empty. `busiest` is computed once per frame
    /// by the caller so the gauges and the diagnosis describe the same chip.
    fn gauges(
        &self,
        st: &TrainState,
        backend: &dyn TelemetryBackend,
        busiest: Option<&Busiest>,
    ) -> Vec<Gauge> {
        let hist = self.history.borrow();
        let mut out = Vec::new();
        if let Some(tps) = st.tokens_per_sec() {
            let best = hist.best_tps().max(tps);
            if best > 0.0 {
                out.push(Gauge {
                    label: "tok/s",
                    frac: tps / best,
                    text: format!("{:.0}% of best", tps / best * 100.0),
                });
            }
        }
        if let Some(chip) = busiest {
            if let Some(tdp) = chip.tdp {
                out.push(Gauge {
                    label: "power",
                    frac: chip.power_w / tdp,
                    text: format!("{:.0}% TDP", chip.power_w / tdp * 100.0),
                });
            }
            let amax = hist.aiclk_max(chip.index);
            if chip.aiclk_mhz > 0 && amax > 0 {
                out.push(Gauge {
                    label: "aiclk",
                    frac: chip.aiclk_mhz as f32 / amax as f32,
                    text: format!("{} MHz", chip.aiclk_mhz),
                });
            }
        }
        if let Some(bps) = Self::pcie_total(backend) {
            // The bar is relative to the highest throughput seen, because no
            // single ceiling is right for every link generation and width.
            let best = hist.best_pcie_bps().max(bps);
            out.push(Gauge {
                label: "pcie",
                frac: if best > 0.0 { (bps / best) as f32 } else { 0.0 },
                text: format!("{:.0} MB/s", bps / 1e6),
            });
        }
        out
    }

    /// One gauge in a cell `w` columns wide: label, bar (when there is room
    /// for at least three cells) and the reading.
    fn draw_gauge(&self, buf: &mut [Vec<Cell>], x: usize, y: usize, w: usize, g: &Gauge) {
        let label = format!("{:<width$}", g.label, width = LABEL_W);
        self.text(
            buf,
            x,
            y,
            &Self::clip(&label, w),
            Color::Rgb(150, 200, 255),
            false,
        );
        let val_len = g.text.chars().count();
        let bar_w = w.saturating_sub(LABEL_W + 1 + val_len).min(10);
        let mut cx = x + LABEL_W;
        if bar_w >= 3 {
            let filled = (g.frac.clamp(0.0, 1.0) * bar_w as f32).round() as usize;
            for i in 0..bar_w {
                let (ch, col) = if i < filled {
                    ('█', BAR_NORMAL)
                } else {
                    ('░', WIRE_REST)
                };
                self.put(buf, cx + i, y, ch, col, false);
            }
            cx += bar_w + 1;
        }
        let room = (x + w).saturating_sub(cx);
        self.text(
            buf,
            cx,
            y,
            &Self::clip(&g.text, room),
            Color::Rgb(210, 230, 220),
            false,
        );
    }

    /// The tapestry band: step bars, grid backdrop, chip lanes, hardware
    /// gauges, a one-line diagnosis and a convergence strip, drawn only from
    /// `st.step_history`, the measured step time, chip telemetry, the loss
    /// history and the run config. A layer whose signal is missing is left
    /// out. Text layers (gauges, diagnosis, strip) show whole clauses or
    /// nothing: a clause that does not fit the band is dropped, never cut.
    fn draw_tapestry(
        &self,
        buf: &mut [Vec<Cell>],
        st: &TrainState,
        backend: &dyn TelemetryBackend,
    ) {
        let (x0, w) = self.network_bounds();
        let Layout {
            network_top,
            network_h,
            ..
        } = self.layout();
        let label = Color::Rgb(150, 200, 255);
        let dim = Color::Rgb(120, 130, 155);

        let lane_devices: Vec<&Device> = backend
            .devices()
            .iter()
            .filter(|d| backend.telemetry(d.index).is_some())
            .take(MAX_LANES)
            .collect();
        let busiest = self.busiest_chip(backend);
        let mut gauges = self.gauges(st, backend, busiest.as_ref());
        // Gauge cells are two per row; the left one is `w / 2` wide and the
        // right one the rest, each minus a one-column gap. A gauge is kept
        // only if its label and whole reading fit in the narrower cell, so
        // a reading is never cut ("10" for "1043 MHz") and no label is drawn
        // without its value.
        let gauge_cell = (w / 2).saturating_sub(1);
        gauges.retain(|g| LABEL_W + g.text.chars().count() <= gauge_cell);
        // The convergence strip keeps whole leading clauses only; later
        // clauses that do not fit are dropped, never cut.
        let parts = convergence_parts(
            &st.loss_history,
            st.config.learning_rate,
            st.scheduler.as_deref(),
            st.step,
            // Once the bar has restarted its total is a chunk size, not the
            // run's budget, so 0 (unknown) keeps the strip from claiming a
            // schedule position it cannot know.
            if st.chunked_bar { 0 } else { st.max_steps },
        );
        let strip = Self::fit_parts(&parts, w);
        let diagnosis = diagnose(&Readings {
            compiled_last_step: st
                .step_history
                .last()
                .map(|s| s.cache_delta > 0)
                .unwrap_or(false),
            busiest_tdp_frac: busiest.as_ref().and_then(|b| b.tdp.map(|t| b.power_w / t)),
            host_cpu_pct: st.host_cpu_pct,
        });
        // The verdict is shown whole, or without its `host cpu` clause, or
        // not at all; a reading is never cut mid-number.
        let verdict = diagnosis
            .as_ref()
            .and_then(|d| Self::verdict_line(d, w).map(|t| (d.kind, t)));
        let plan = plan_band(
            network_h,
            BandWants {
                lanes: lane_devices.len(),
                gauges: gauges.len(),
                verdict: verdict.is_some(),
                strip: !strip.is_empty(),
            },
        );

        // The newest `n` steps, right-aligned so the latest step is always at
        // the band's right edge and the lanes below share the same columns.
        let hist = &st.step_history;
        let bar_w = w.saturating_sub(LABEL_W + 1);
        let n = hist.len().min(bar_w);
        let shown = &hist[hist.len() - n..];
        let x_bars = x0 + LABEL_W;
        let col0 = x_bars + (bar_w - n);
        let pass = pass_secs(st.step_ms);

        // Advanced even when the grid row is not drawn, so the cursor does not
        // jump when it reappears.
        let phase = self.advance_pulse(pass.map(|(secs, _)| secs));

        // ── header ───────────────────────────────────────────────────
        let header = if shown.is_empty() {
            "STEP ANATOMY  no per-step times reported".to_string()
        } else {
            // Times polled from a progress bar are disclosed in the title so
            // they are never taken for ones the trainer printed.
            let title = if st.step_time_source == StepTimeSource::Observed {
                "STEP ANATOMY (from bar)"
            } else {
                "STEP ANATOMY"
            };
            let ms: Vec<f32> = shown.iter().map(|s| s.ms).collect();
            let med = median(&ms).unwrap_or(0.0);
            let mut h = format!("{title}  last {n} steps · median {med:.0} ms");
            // The pulse clause is atomic: it is appended only if the whole
            // clause fits, so the "(max 5/s)" note is never cut off while
            // "pulse = 1 step" is still shown.
            let clause = match pass {
                Some((_, true)) => " · pulse = 1 step (max 5/s)",
                Some((_, false)) => " · pulse = 1 step",
                None => "",
            };
            if h.chars().count() + clause.chars().count() <= w {
                h.push_str(clause);
            }
            h
        };
        self.text(buf, x0, network_top, &Self::clip(&header, w), label, false);

        // ── step bars ────────────────────────────────────────────────
        let y_bars = network_top + 1;
        let max_ms = shown.iter().map(|s| s.ms).fold(0.0f32, f32::max);
        if plan.bar_rows > 0 && max_ms > 0.0 {
            // The scale is printed, not implied: window maximum on the top
            // row and zero on the bottom one.
            self.text(buf, x0, y_bars, &format!("{max_ms:>5.0}"), dim, false);
            if plan.bar_rows > 1 {
                let y_last = y_bars + plan.bar_rows - 1;
                self.text(buf, x0, y_last, &format!("{:>5}", 0), dim, false);
            }
            for (i, s) in shown.iter().enumerate() {
                let frac = s.ms / max_ms;
                let color = match cause_of(s) {
                    StepCause::Normal => BAR_NORMAL,
                    StepCause::Compile => BAR_COMPILE,
                    StepCause::Checkpoint => BAR_CHECKPOINT,
                };
                for r in 0..plan.bar_rows {
                    let from_bottom = plan.bar_rows - 1 - r;
                    let ch = bar_cell(frac, from_bottom, plan.bar_rows);
                    self.put(buf, col0 + i, y_bars + r, ch, color, false);
                }
            }
            // Median line: a dotted rule across every column whose bar is
            // shorter than the median, so it shows through gaps and never
            // overwrites a bar.
            let ms: Vec<f32> = shown.iter().map(|s| s.ms).collect();
            let med = median(&ms).unwrap_or(0.0);
            let eighths = ((med / max_ms * plan.bar_rows as f32 * 8.0).round() as usize).max(1);
            let med_from_bottom = ((eighths - 1) / 8).min(plan.bar_rows - 1);
            let y_med = y_bars + plan.bar_rows - 1 - med_from_bottom;
            for (i, s) in shown.iter().enumerate() {
                if bar_cell(s.ms / max_ms, med_from_bottom, plan.bar_rows) == ' ' {
                    self.put(buf, col0 + i, y_med, '┈', WIRE_REST, false);
                }
            }
        }

        // ── grid backdrop with the step-rate cursor ──────────────────
        let mut y = y_bars + plan.bar_rows;
        if plan.grid_row {
            self.text(buf, x0, y, "pulse", dim, false);
            let gw = bar_w;
            let period = gw as f32 + SWEEP_TAIL + SWEEP_LEAD;
            let head = phase.map(|p| p * period);
            let node_glyphs = ['●', '◉', '○', '◇', '·'];
            for c in 0..gw {
                let lit = head.map(|h| sweep_at(h, c as f32, period)).unwrap_or(0.0);
                let ch = if c % GRID_STRIDE == 0 {
                    node_glyphs[((1.0 - lit) * (node_glyphs.len() - 1) as f32).round() as usize
                        % node_glyphs.len()]
                } else {
                    '─'
                };
                let (col, bold) = if lit > 0.04 {
                    (
                        hsv_to_rgb(FWD_HUE, 0.45 + lit * 0.4, 0.35 + lit * 0.6),
                        lit > 0.55,
                    )
                } else {
                    (WIRE_REST, false)
                };
                self.put(buf, x_bars + c, y, ch, col, bold);
            }
            y += 1;
        }

        // ── chip lanes ───────────────────────────────────────────────
        let hist_chips = self.history.borrow();
        for dev in lane_devices.iter().take(plan.lane_rows) {
            self.text(
                buf,
                x0,
                y,
                &Self::clip(&format!("chip{}", dev.index), LABEL_W),
                dim,
                false,
            );
            let temp = backend
                .telemetry(dev.index)
                .map(|t| t.temp_c())
                .unwrap_or(0.0);
            let tcolor = colors::temp_color(temp);
            let samples: Vec<_> = shown
                .iter()
                .map(|s| hist_chips.sample_at(dev.index, s.seq))
                .collect();
            // Scale to the chip's TDP. With no TDP known, to the highest power
            // in the window, so the lane still shows its own shape.
            let scale = Self::chip_tdp(backend, dev).unwrap_or_else(|| {
                samples
                    .iter()
                    .flatten()
                    .map(|c| c.power_w)
                    .fold(0.0, f32::max)
            });
            let aimax = hist_chips.aiclk_max(dev.index);
            for (i, smp) in samples.iter().enumerate() {
                let (ch, col) = match smp {
                    Some(c) if scale > 0.0 => {
                        let dropped = c.aiclk_mhz > 0
                            && aimax > 0
                            && (c.aiclk_mhz as f32) < AICLK_DROP_FRAC * aimax as f32;
                        (
                            bar_cell(c.power_w / scale, 0, 1),
                            if dropped { AICLK_DROP } else { tcolor },
                        )
                    }
                    _ => (NO_SAMPLE, WIRE_REST),
                };
                self.put(buf, col0 + i, y, ch, col, false);
            }
            y += 1;
        }
        drop(hist_chips);

        // ── gauges: two per row, half the band each ──────────────────
        let half = w / 2;
        for (i, g) in gauges.iter().enumerate() {
            let row = i / 2;
            if row >= plan.gauge_rows {
                break;
            }
            let (gx, gw) = if i % 2 == 0 {
                (x0, half)
            } else {
                (x0 + half, w - half)
            };
            self.draw_gauge(buf, gx, y + row, gw.saturating_sub(1), g);
        }
        y += plan.gauge_rows;

        // ── diagnosis ────────────────────────────────────────────────
        if let (true, Some((kind, line))) = (plan.verdict_row, verdict.as_ref()) {
            let color = match kind {
                DiagnosisKind::Compiling => BAR_COMPILE,
                DiagnosisKind::ComputeBound => Color::Rgb(111, 171, 160),
                DiagnosisKind::HostBound => BAR_CHECKPOINT,
            };
            self.text(buf, x0, y, line, color, false);
            y += 1;
        }

        // ── convergence strip ────────────────────────────────────────
        if plan.strip_row {
            self.text(buf, x0, y, &strip, Color::Rgb(190, 200, 220), false);
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
        // Host-side cost. Training is not only a device workload — the data
        // pipeline, tokenisation and kernel compilation all burn host cycles,
        // and a run can be entirely CPU-bound with the chips idle. 100% is
        // one core, so >100 is normal and worth seeing rather than clamping.
        if let Some(cpu) = st.host_cpu_pct {
            line!(format!("cpu     {cpu:.0}%"), val, false);
        }
        if let Some(rss) = st.host_rss_bytes {
            line!(format!("rss     {}", fmt_bytes(rss)), val, false);
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
            line!(Self::watching_line(&elapsed, &eta), val, false);
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
        let ceiling = loss_ceiling(st.config.vocab_size);
        let Layout {
            river_top,
            river_bottom,
            ..
        } = self.layout();
        if river_bottom <= river_top + 1 {
            return;
        }
        // Only label the band once there is something in it. The nightscape
        // is drawn either way — it is the run's sky, not a placeholder — but
        // heading an empty band "LOSS · mountains colored by their own value"
        // promises a plot that isn't there, which is the one thing this view
        // must not do.
        if !st.loss_history.is_empty() {
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
        }

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
        // Span the window's losses are normalised against. Two floors, for
        // two different failure modes:
        //
        // The absolute floor stops a *converged* run — every loss within a
        // hair of the others — normalising to zero everywhere and rendering
        // as bare ground under the aurora. A model that has converged is a
        // run at its best, not an absence of data, and it should still show
        // a range.
        //
        // The relative floor stops the opposite: dividing a 0.0002-wide
        // window by a 0.001 constant amplifies pure step-to-step noise into
        // a full-height sawtooth, drawing a dramatic landscape out of a loss
        // that is not really moving. Scaling the floor with the loss's own
        // magnitude keeps small wobble looking small.
        let range = (max_loss - min_loss).max(min_loss.abs() * 0.05).max(0.02);
        let total_eighths = sky_rows * 8;
        let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let usable_w = self.width.saturating_sub(3).max(1);

        for x in 2..self.width {
            let rel = (x - 2) as f32 / usable_w as f32;
            let idx = ((rel * (hist.len() - 1) as f32).round() as usize).min(hist.len() - 1);
            let loss = hist[idx];
            // Lift the whole band off the floor so the window's lowest
            // column still reads as a mountain rather than as empty sky —
            // the low point of a descending run is a data point, not a gap.
            let norm = ((loss - min_loss) / range).clamp(0.0, 1.0);
            let norm = MOUNTAIN_FLOOR + norm * (1.0 - MOUNTAIN_FLOOR);
            let eighths = (norm * total_eighths as f32).round() as usize;
            let base_hue = loss_hue(loss, ceiling);
            let full_bars = eighths / 8;
            let partial = eighths % 8;
            // Slow drift, in wall-clock seconds so the undulation reads the
            // same at any redraw rate.
            let t = self.frame as f32 / crate::animation::train_sky::ANIM_FPS;

            // Colour for a cell at `row_from_bottom` within this column: the
            // base is deep and saturated, the crest brighter and slightly
            // warmer, with a slow wobble across x and height so the whole
            // range undulates instead of standing still.
            let cell_color = |row_from_bottom: usize, height: usize| {
                let depth = if height == 0 {
                    0.0
                } else {
                    row_from_bottom as f32 / height as f32
                };
                let (h, s, v) = mountain_hsv(base_hue, depth, x, t);
                hsv_to_rgb(h, s, v)
            };

            for row_from_bottom in 0..sky_rows {
                let y = river_bottom - 1 - row_from_bottom;
                if row_from_bottom < full_bars {
                    let c = cell_color(row_from_bottom, full_bars);
                    self.put(buf, x, y, '█', c, false);
                } else if row_from_bottom == full_bars && partial > 0 {
                    let c = cell_color(row_from_bottom, full_bars.max(1));
                    self.put(buf, x, y, bars[partial - 1], c, false);
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

    /// Columns reserved for an elapsed / eta reading.
    ///
    /// `fmt_elapsed` renders `M:SS` with UNPADDED minutes, so the field grows a
    /// column at ten minutes and again at a hundred. Six covers `999:59`,
    /// about sixteen hours — beyond that the line widens once more rather than
    /// continuously, which is the graceful direction to fail.
    const TIME_FIELD_W: usize = 6;

    /// The elapsed/eta line, with both readings right-aligned into a fixed
    /// field.
    ///
    /// Same defect as the CHIPS strip, one line up: these two values share a
    /// row, so when `watching` crosses 9:59 → 10:00 it pushes `eta` and its
    /// label a column to the right. Unlike the CHIPS strip nothing downstream
    /// moves — the other LIVE rows are separate lines — so this is a smaller
    /// twitch, but it is the same artefact of digit count rather than of the
    /// measurement, and it happens once in every run longer than ten minutes.
    ///
    /// Padded here rather than inside `fmt_elapsed`, which is shared with the
    /// inference views: widening a helper for one caller's layout is how the
    /// other callers acquire a bug they never asked for.
    fn watching_line(elapsed: &str, eta: &str) -> String {
        let w = Self::TIME_FIELD_W;
        format!("watching {elapsed:>w$}  eta {eta:>w$}")
    }

    /// One chip's label, at a FIXED width so the strip cannot jitter.
    ///
    /// Temperature and power are right-aligned into their widest plausible
    /// form — five columns for `100.0`, three for `235` — because the caller
    /// advances its x cursor by this string's length. Unpadded, a chip
    /// crossing 9W → 10W or 99.9°C → 100.0°C lengthens its own tag by a
    /// column, which shifts its density bar *and* every device to its right.
    /// With four chips sampled ten times a second that reads as the whole
    /// strip twitching, and the movement encodes nothing: it is an artefact of
    /// how many digits the number happens to have, not of the number.
    ///
    /// ASCII spaces, deliberately. The typographic answer to digits changing
    /// width is U+2007 FIGURE SPACE, which matches a digit's advance in a
    /// PROPORTIONAL font. This is a cell buffer — one character per column,
    /// see `put` — so a plain space is already exactly one digit wide, and
    /// U+2007 would add a font-substitution risk for no gain.
    ///
    /// The widths are ceilings, not guesses: per-chip power is bounded by the
    /// 200W-ish ceiling the density bar below already assumes (a p300c peaks
    /// around 235W under load), and a temperature needing four integer digits
    /// is not a reading, it is a broken sensor.
    fn chip_tag(idx: usize, temp: f32, power: f32) -> String {
        format!("dev{idx} {temp:>5.1}°C {power:>3.0}W ")
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
            let tag = Self::chip_tag(idx, temp, power);
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
        let ceiling = loss_ceiling(st.config.vocab_size);
        let y = self.layout().legend_row;
        let loss_color = st
            .loss
            .map(|l| hsv_to_rgb(loss_hue(l, ceiling), 0.75, 0.85))
            .unwrap_or(Color::Rgb(200, 150, 220));
        // A run whose stdout can't be read draws no loss curve and
        // no comet — so advertising their symbols in the legend describes a
        // screen the viewer is not looking at. Only the channels that survive
        // that state are listed. `log` is `None` while still scanning, which
        // is also a state with no curve.
        let has_stream = matches!(st.log, Some(LogSource::File(_)));
        let all: [(char, &str, Color); 9] = [
            ('●', "loss", loss_color),
            ('▇', "step time", BAR_NORMAL),
            ('▇', "compile", BAR_COMPILE),
            ('▼', "loss ↓", Color::Rgb(120, 230, 190)),
            ('▲', "loss ↑", Color::Rgb(255, 140, 120)),
            ('█', "chip temp", colors::temp_color(70.0)),
            ('▓', "chip power", Color::Rgb(200, 200, 120)),
            ('✦', "checkpoint", Color::Rgb(120, 230, 190)),
            ('░', "aurora", Color::Rgb(120, 180, 150)),
        ];
        // Every channel is drawn without a stream *except* the ones that need
        // a loss value: the tapestry band and the river (nightscape, aurora,
        // and the checkpoint comet drawn inside it) all still render from the
        // config and from chip telemetry. Only the mountains and the header's
        // delta arrows depend on a per-step loss.
        // `step time` and `compile` need step samples, so a trainer that
        // prints no per-step time does not advertise them.
        //
        // This tracks whether the river is drawn, not whether a stream
        // exists — an earlier version keyed on the stream and dropped the
        // aurora and comet, which was right only while the no-log state
        // replaced the river wholesale.
        let has_steps = !st.step_history.is_empty();
        let entries: Vec<(char, &str, Color)> = all
            .into_iter()
            .filter(|(_, label, _)| has_stream || !matches!(*label, "loss" | "loss ↓" | "loss ↑"))
            .filter(|(_, label, _)| has_steps || !matches!(*label, "step time" | "compile"))
            .collect();
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
    fn loss_hue_runs_red_when_high_to_deep_cyan_when_converged() {
        let c = LEGACY_LOSS_CEILING;
        let hot = loss_hue(4.5, c);
        let cool = loss_hue(0.1, c);
        assert!(
            (330.0..=360.0).contains(&hot),
            "high loss should be red/pink, got {hot}"
        );
        assert!(
            (180.0..200.0).contains(&cool),
            "converged loss should be deep cyan, got {cool}"
        );
        // Monotonic in loss, so the colour still reads as a value rather
        // than decoration — the whole ramp is meaningless otherwise.
        let mut prev = loss_hue(0.0, c);
        for i in 1..=40 {
            let h = loss_hue(i as f32 * 0.12, c);
            assert!(h >= prev, "hue must not go backwards as loss rises");
            prev = h;
        }
        // Clamped outside the observed range rather than wrapping around.
        assert!((loss_hue(99.0, c) - loss_hue(c, c)).abs() < 1.0);
        assert!((loss_hue(-1.0, c) - loss_hue(0.0, c)).abs() < 1.0);
    }

    /// The ceiling is a property of the task. For a language model it is the
    /// loss of a uniform distribution over the vocabulary — what a model that
    /// has learned nothing reports — so the ramp spans the run's own range.
    #[test]
    fn the_loss_ceiling_is_ln_vocab_for_a_language_model() {
        let c = loss_ceiling(Some(32_000));
        assert!((c - 10.373).abs() < 0.01, "ln(32000) expected, got {c}");
        // A bigger vocabulary is a harder task and a higher ceiling.
        assert!(loss_ceiling(Some(128_000)) > c);
        // No vocabulary — a regression example — keeps the historical anchor,
        // which was always right for losses that live around 0.1–0.5.
        assert_eq!(loss_ceiling(None), LEGACY_LOSS_CEILING);
        assert_eq!(loss_ceiling(Some(1)), LEGACY_LOSS_CEILING);
    }

    /// The regression this replaces. Under the old fixed ramp every loss at or
    /// above 4.6 was the same red, so a 32k-vocab run rendered flat through the
    /// whole early phase — from an untrained 10.37 down to 4.6 — which is
    /// exactly where the loss moves fastest. Scaled to ln(vocab) those are
    /// distinguishable, and the run walks the spectrum as it learns.
    #[test]
    fn early_training_is_not_one_flat_red_for_a_vocabulary_sized_loss() {
        let c = loss_ceiling(Some(32_000));
        let untrained = loss_hue(10.37, c);
        let early = loss_hue(6.8, c);
        let mid = loss_hue(3.9, c);
        let best = loss_hue(1.73, c);

        // Each phase is a visibly different hue, not the same saturated red.
        for (a, b, what) in [
            (untrained, early, "untrained vs early"),
            (early, mid, "early vs mid"),
            (mid, best, "mid vs best"),
        ] {
            assert!(a - b > 15.0, "{what} must be distinguishable: {a} vs {b}");
        }
        // And the best loss this project has recorded reads as cool, not red.
        assert!(
            best < 240.0,
            "a converged LM should be cyan-blue, got {best}"
        );

        // The old ramp could not tell any of the top three apart.
        let legacy = |l: f32| loss_hue(l, LEGACY_LOSS_CEILING);
        assert!((legacy(10.37) - legacy(6.8)).abs() < 1.0);
        assert!((legacy(6.8) - legacy(4.6)).abs() < 1.0);
    }

    /// `watching` and `eta` share a row, so a run crossing ten minutes pushes
    /// the `eta` label right by a column. Every run longer than ten minutes
    /// does this exactly once, which is enough to be noticed and encodes
    /// nothing.
    #[test]
    fn the_watching_line_keeps_its_width_across_the_ten_minute_boundary() {
        let widths: Vec<usize> = [
            ("0:38", "4:11"),
            ("9:59", "0:01"),
            ("10:00", "0:00"),
            ("100:00", "999:59"),
            ("5:34", "—"), // eta unknown renders as a dash
        ]
        .iter()
        .map(|(e, t)| TrainView::watching_line(e, t).chars().count())
        .collect();
        let first = widths[0];
        assert!(
            widths.iter().all(|w| *w == first),
            "the row must not change width, got {widths:?}"
        );
    }

    #[test]
    fn the_watching_line_still_states_both_readings() {
        let l = TrainView::watching_line("5:34", "0:27");
        assert!(l.contains("5:34") && l.contains("0:27"), "{l}");
        assert!(l.starts_with("watching"), "{l}");
    }

    /// The CHIPS strip advances its x cursor by each tag's length, so a tag
    /// that changes width moves everything to its right. Crossing a digit
    /// boundary is the common case — a chip idling at 9W and loaded at 115W
    /// crosses two — and the resulting movement encodes nothing about the
    /// hardware.
    #[test]
    fn a_chip_tag_keeps_its_width_across_every_digit_boundary() {
        let cases = [
            (0usize, 9.0f32, 9.0f32), // single digit both
            (0, 45.0, 18.0),          // the idle shape
            (0, 62.7, 115.0),         // loaded
            (0, 99.9, 99.0),          // just below both boundaries
            (0, 100.0, 100.0),        // just above both
            (0, 8.4, 235.0),          // p300c peak power, low temp
            (0, 0.0, 0.0),            // telemetry unavailable
        ];
        let widths: Vec<usize> = cases
            .iter()
            .map(|(i, t, p)| TrainView::chip_tag(*i, *t, *p).chars().count())
            .collect();
        let first = widths[0];
        assert!(
            widths.iter().all(|w| *w == first),
            "every tag must be the same width, got {widths:?} for {cases:?}"
        );
    }

    /// Padding must not cost the values themselves — a stable strip showing
    /// the wrong number is worse than a twitching one showing the right one.
    #[test]
    fn a_chip_tag_still_states_its_readings() {
        let tag = TrainView::chip_tag(2, 62.7, 115.0);
        assert!(tag.starts_with("dev2 "), "{tag}");
        assert!(tag.contains("62.7°C"), "{tag}");
        assert!(tag.contains("115W"), "{tag}");
        // Right-aligned, so a narrow reading gains leading blanks rather than
        // shifting what follows it.
        assert!(TrainView::chip_tag(0, 9.0, 9.0).contains("  9.0°C   9W"));
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

    /// A run whose stdout is unreadable draws no loss curve, no sweeps and
    /// no comet, so listing their symbols describes a screen that isn't
    /// there. Raised in review.
    #[test]
    fn the_legend_only_lists_symbols_that_can_appear() {
        use crate::workload::train::{LogSource, TrainProcess};
        let render = |log: Option<LogSource>| -> String {
            let mut b = MockBackend::new(1);
            b.init().unwrap();
            let mut st = TrainState::new();
            st.proc = Some(TrainProcess {
                pid: 1,
                binary: "nano_gpt".into(),
                config_path: None,
            });
            st.log = log;
            let lines: Vec<String> = TrainView::new(120, 40)
                .render(&st, &b)
                .iter()
                .map(|l| l.spans.iter().map(|sp| sp.content.to_string()).collect())
                .collect();
            // The legend row only — matching the whole frame also hits the
            // no-log notice, whose copy legitimately says "checkpoint mtime".
            lines
                .iter()
                .find(|l| l.contains("chip temp"))
                .cloned()
                .unwrap_or_default()
        };

        // Unreadable stdout: no per-step stream exists.
        let out = render(Some(LogSource::NotRedirected));
        // The river and tapestry band still render from the config in this state,
        // so only the loss-derived channels are unreachable.
        for present in ["chip temp", "chip power", "aurora", "checkpoint"] {
            assert!(
                out.contains(present),
                "legend dropped {present:?}, which this state still draws"
            );
        }
        for absent in ["loss ↓", "loss ↑"] {
            assert!(
                !out.contains(absent),
                "legend advertises {absent:?} on a screen that cannot draw it"
            );
        }
        // A readable log gets the full legend back.
        let out = render(Some(LogSource::File("/tmp/x.log".into())));
        for present in ["loss ↓", "checkpoint"] {
            assert!(
                out.contains(present),
                "legend lost {present:?} on a live run"
            );
        }
    }

    /// The river should read as a gradient — deep, dark bases lifting to
    /// brighter crests across a wide hue arc — rather than columns of flat
    /// colour.
    ///
    /// Asserted on `mountain_hsv` rather than on rendered spans, because
    /// reading colours back off the render depends on the *terminal*:
    /// `colors::rgb` yields `Color::Indexed` when the environment reports no
    /// true-colour support, so a span-reading version of this test passed on
    /// a developer machine (COLORTERM=truecolor) and failed in CI on all
    /// three platforms, which is exactly how it was found.
    #[test]
    fn mountains_render_as_a_gradient_not_flat_columns() {
        let hue = loss_hue(2.5, LEGACY_LOSS_CEILING);
        let t = 0.0;

        // Brightness must rise from base to crest — the gradient itself.
        let base = mountain_hsv(hue, 0.0, 10, t);
        let crest = mountain_hsv(hue, 1.0, 10, t);
        assert!(
            crest.2 > base.2 + 0.25,
            "crest must be materially brighter than base: {} vs {}",
            crest.2,
            base.2
        );
        // ...and saturation ease off, so the crest glows instead of going
        // to poster ink.
        assert!(
            crest.1 < base.1,
            "saturation must ease off toward the crest: {} vs {}",
            crest.1,
            base.1
        );

        // Every value stays in range at any depth, column, or moment.
        for d in 0..=10 {
            for x in [0usize, 7, 113] {
                for tt in [0.0f32, 1.7, 40.0] {
                    let (h, sa, v) = mountain_hsv(hue, d as f32 / 10.0, x, tt);
                    assert!((0.0..360.0).contains(&h), "hue out of range: {h}");
                    assert!((0.0..=1.0).contains(&sa), "sat out of range: {sa}");
                    assert!((0.0..=1.0).contains(&v), "val out of range: {v}");
                }
            }
        }

        // The wobble must stay small enough that the colour still reads as
        // its loss value rather than drifting into a neighbouring band.
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for x in 0..200usize {
            for tt in 0..40 {
                let (h, _, _) = mountain_hsv(hue, 0.0, x, tt as f32 * 0.25);
                lo = lo.min(h);
                hi = hi.max(h);
            }
        }
        assert!(
            hi - lo <= MOUNTAIN_HUE_WOBBLE * 2.0 + 1.0,
            "hue wobble must stay bounded, spanned {:.1} deg",
            hi - lo
        );

        // The undulation has to actually move, or "undulating" is a lie.
        let a = mountain_hsv(hue, 0.5, 20, 0.0);
        let b = mountain_hsv(hue, 0.5, 20, 3.0);
        assert!(
            (a.0 - b.0).abs() > 0.5 || (a.2 - b.2).abs() > 0.005,
            "the gradient must drift over time"
        );
    }

    /// Mountain height is the loss's position within the window's own
    /// min..max, so a window whose losses are all close together normalises
    /// to zero everywhere and the range vanishes entirely — leaving aurora
    /// and stars over bare ground. That is exactly what a converged run
    /// looks like, and what a run with one sample so far looks like.
    #[test]
    fn mountains_are_drawn_even_when_the_loss_window_is_flat() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        // Count mountain glyphs ONLY between the "LOSS" header and the
        // low/high scale line. The CHIPS row and the legend both draw power
        // with the same block characters, so counting the whole frame
        // reports success no matter what the river does — the first version
        // of this test did exactly that and passed against the bug.
        let mountain = |st: &TrainState| -> usize {
            let mut b = MockBackend::new(1);
            b.init().unwrap();
            let v = TrainView::new(120, 40);
            let lines: Vec<String> = v
                .render(st, &b)
                .iter()
                .map(|l| l.spans.iter().map(|sp| sp.content.to_string()).collect())
                .collect();
            let top = lines
                .iter()
                .position(|t| t.contains("LOSS  ·"))
                .expect("the river has a header");
            let bottom = lines
                .iter()
                .position(|t| t.contains(" low") && t.contains("high "))
                .unwrap_or(lines.len());
            lines[top + 1..bottom]
                .iter()
                .flat_map(|t| t.chars())
                .filter(|c| "▁▂▃▄▅▆▇█".contains(*c))
                .count()
        };
        let base = |losses: &[f32]| {
            let mut st = TrainState::new();
            st.proc = Some(TrainProcess {
                pid: 1,
                binary: "nano_gpt".into(),
                config_path: None,
            });
            st.log = Some(LogSource::File("/tmp/x.log".into()));
            for (i, l) in losses.iter().enumerate() {
                st.apply_event(TrainEvent::Step {
                    step: i as u64 + 1,
                    loss: *l,
                });
            }
            st
        };

        // A descending run has always drawn mountains — the control.
        let descending: Vec<f32> = (0..60).map(|i| 3.0 - i as f32 * 0.03).collect();
        assert!(
            mountain(&base(&descending)) > 0,
            "a descending run must draw mountains"
        );

        // A converged run: every loss within a hair of every other.
        let flat: Vec<f32> = vec![0.42; 60];
        assert!(
            mountain(&base(&flat)) > 0,
            "a converged run must still show a range, not bare ground"
        );

        // A run that has only reported once.
        assert!(
            mountain(&base(&[2.5])) > 0,
            "a single sample must still draw a ridge"
        );

        // The opposite failure: a window that is nearly-but-not-quite flat
        // must not be amplified into a dramatic landscape. Dividing a
        // 0.0002-wide window by a small constant would turn pure step noise
        // into a full-height sawtooth, which reads as a model doing
        // something when it is not.
        let noisy: Vec<f32> = (0..60).map(|i| 2.0 + (i % 5) as f32 * 0.0001).collect();
        let real: Vec<f32> = (0..60).map(|i| 3.0 - i as f32 * 0.03).collect();
        assert!(
            mountain(&base(&noisy)) < mountain(&base(&real)),
            "step noise must not draw as much mountain as a real descent"
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

    /// Fluidity guard. The cursor head is continuous with a lead-in and a
    /// trailing falloff, so a cell's brightness changes by small increments
    /// between consecutive frames. A regression to on/off would produce a 1.0
    /// jump, and dropping the lead-in ramp measured 0.95.
    #[test]
    fn sweep_intensity_changes_smoothly_between_consecutive_frames() {
        let period = 50.0;
        // A 2 s pass over 50 columns moves the head 25 columns/s (about 0.42
        // per frame), the speed the old fixed sweep used. At a 1 s pass the
        // head moves 0.83 columns/frame and the lead-in's steepest slope
        // alone gives a 0.39 step, so 0.25 would test the wrong speed.
        let head = |f: u64| pass_fraction(f, crate::animation::train_sky::ANIM_FPS, 2.0) * period;
        let mut worst: f32 = 0.0;
        for at in [0.0f32, 7.5, 15.0, 33.0, 49.0] {
            let mut prev = sweep_at(head(0), at, period);
            for f in 1..240u64 {
                let cur = sweep_at(head(f), at, period);
                worst = worst.max((cur - prev).abs());
                prev = cur;
            }
        }
        assert!(
            worst < 0.25,
            "sweep steps by {worst:.3} between frames - motion should be continuous"
        );
        let peak = (0..240u64)
            .map(|f| sweep_at(head(f), 15.0, period))
            .fold(0.0f32, f32::max);
        assert!(
            peak > 0.85,
            "sweep never reaches full brightness: {peak:.3}"
        );
    }

    /// The pulse must travel in one direction and complete one pass per
    /// measured step: at a 1 s step it wraps once per 60 frames.
    #[test]
    fn the_cursor_advances_monotonically_and_wraps_once_per_pass() {
        let fps = crate::animation::train_sky::ANIM_FPS;
        let mut last = pass_fraction(0, fps, 1.0);
        let mut wraps = 0;
        for f in 1..=180u64 {
            let h = pass_fraction(f, fps, 1.0);
            if h < last {
                wraps += 1;
            } else {
                assert!(
                    h - last < 0.05,
                    "cursor jumped {:.3} in one frame",
                    h - last
                );
            }
            last = h;
        }
        assert_eq!(
            wraps, 3,
            "three one-second passes in 180 frames at {fps} fps"
        );
    }

    use crate::animation::train_tapestry::pass_fraction;

    // ---- tapestry band -------------------------------------------------

    fn live_state() -> TrainState {
        use crate::workload::train::{LogSource, TrainProcess};
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(crate::workload::train::TrainEvent::Step { step: 1, loss: 2.0 });
        st
    }

    fn sample_at(step: u64, ms: f32, delta: u32) -> crate::workload::train::StepSample {
        crate::workload::train::StepSample {
            step,
            seq: step,
            ms,
            cache_delta: delta,
            checkpoint: false,
        }
    }

    fn rows_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_title_says_when_step_times_were_observed_from_the_bar() {
        use crate::workload::train::StepTimeSource;
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 285.0, 0)).collect();
        st.step_seq = 10;
        st.step_ms = 285.0;
        st.step_time_source = StepTimeSource::Reported;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY  last 10 steps"), "{out}");
        assert!(!out.contains("from bar"), "{out}");
        st.step_time_source = StepTimeSource::Observed;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(
            out.contains("STEP ANATOMY (from bar)  last 10 steps"),
            "{out}"
        );
    }

    /// A bar restart sends `st.step` back to 1. It must not look like a new
    /// run: lanes and best-so-far values keep their history.
    #[test]
    fn a_bar_restart_does_not_clear_the_chip_lanes() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=6u64).map(|i| sample_at(i, 285.0, 0)).collect();
        st.step_ms = 285.0;
        let v = TrainView::new(134, 40);
        // seq 1..=3 while the bar runs 3193..=3195, then the bar restarts.
        for (seq, bar_step) in [
            (1u64, 3193u64),
            (2, 3194),
            (3, 3195),
            (4, 1),
            (5, 2),
            (6, 3),
        ] {
            st.step_seq = seq;
            st.step = bar_step;
            v.render(&st, &b);
        }
        let row = rows_of(&v.render(&st, &b))
            .into_iter()
            .find(|r| r.contains("chip0"))
            .expect("a lane for chip 0");
        assert_eq!(
            row.matches(NO_SAMPLE).count(),
            0,
            "all six columns sampled: {row:?}"
        );
    }

    #[test]
    fn once_the_bar_is_known_to_be_chunked_the_strip_makes_no_schedule_claim() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.loss_history = (0..120).map(|i| 5.0 - 0.01 * i as f32).collect();
        st.scheduler = Some("cosine".into());
        st.max_steps = 3195; // a chunk size, not the run's budget
        st.step = 630;
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("cosine 19% through"), "{out}");
        st.chunked_bar = true;
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("cosine"), "{out}");
        assert!(!out.contains("% through"), "{out}");
    }

    #[test]
    fn step_bars_are_drawn_from_the_step_history() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        // Thirty normal steps and one four times slower: the bar scale is the
        // window maximum, so only the slow step reaches the top row.
        st.step_history = (1..=31u64)
            .map(|i| sample_at(i, if i == 20 { 400.0 } else { 100.0 }, 0))
            .collect();
        st.step = 31;
        st.step_ms = 100.0;
        let rows = rows_of(&TrainView::new(134, 40).render(&st, &b));
        let header = rows
            .iter()
            .position(|r| r.contains("STEP ANATOMY"))
            .expect("the band header should render");
        assert!(rows[header].contains("last 31 steps"), "{}", rows[header]);
        assert!(rows[header].contains("median 100 ms"), "{}", rows[header]);
        let top = &rows[header + 1];
        assert_eq!(
            top.matches('█').count(),
            1,
            "only the slow step reaches the top bar row: {top:?}"
        );
        // The normal steps are a quarter of the scale: one full bottom cell.
        let bottom = &rows[header + 4];
        assert!(bottom.matches('█').count() >= 31, "{bottom:?}");
    }

    /// Review Focus 1.
    #[test]
    fn a_trainer_with_no_per_step_times_gets_no_bars_but_keeps_the_pulse() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_ms = 400.0; // derived from log cadence; no per-step history
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY"), "{out}");
        assert!(out.contains("no per-step times reported"), "{out}");
        assert!(
            out.contains("pulse"),
            "the grid backdrop still renders:
{out}"
        );
    }

    #[test]
    fn the_header_says_when_the_pulse_is_slowed_to_stay_visible() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 83.0, 0)).collect();
        st.step = 10;
        st.step_ms = 83.0;
        for w in [134usize, 150] {
            let out = text_of(&TrainView::new(w, 40).render(&st, &b));
            assert!(out.contains("pulse = 1 step (max 5/s)"), "w={w}\n{out}");
        }
        st.step_ms = 400.0;
        for w in [134usize, 150] {
            let out = text_of(&TrainView::new(w, 40).render(&st, &b));
            assert!(out.contains("pulse = 1 step"), "w={w}\n{out}");
            assert!(!out.contains("5/s"), "w={w}\n{out}");
        }
    }

    /// The pulse clause is all or nothing: the qualifier is never cut off
    /// while the rest of the clause is shown.
    #[test]
    fn the_pulse_clause_is_never_shown_without_its_qualifier() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 83.0, 0)).collect();
        st.step = 10;
        st.step_ms = 83.0;
        for w in 60..=134usize {
            let out = text_of(&TrainView::new(w, 40).render(&st, &b));
            if out.contains("pulse = 1 step") {
                assert!(out.contains("pulse = 1 step (max 5/s)"), "w={w}\n{out}");
            }
        }
        let out = text_of(&TrainView::new(120, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY"), "{out}");
    }

    /// Changing step_ms between frames must change the cursor's speed, not
    /// its position. Absolute-time phase jumped by most of a pass.
    #[test]
    fn the_pulse_phase_is_continuous_across_step_time_changes() {
        let mut v = TrainView::new(134, 40);
        v.frame = 36000;
        v.advance_pulse(Some(0.5));
        let mut prev = v.advance_pulse(Some(0.5)).unwrap();
        for (i, secs) in [0.505f32, 0.48, 0.5, 0.52].into_iter().enumerate() {
            v.frame = 36001 + i as u64;
            let cur = v.advance_pulse(Some(secs)).unwrap();
            let d = (cur - prev).rem_euclid(1.0);
            assert!(d < 0.05, "phase jumped {d:.3} in one frame");
            prev = cur;
        }
        // Unknown step time leaves the phase alone.
        let before = v.pulse_phase.get();
        v.frame += 10;
        assert_eq!(v.advance_pulse(None), None);
        assert_eq!(v.pulse_phase.get(), before);
    }

    #[test]
    fn the_pulse_phase_advances_at_the_step_rate_for_a_constant_step_time() {
        let mut v = TrainView::new(134, 40);
        v.advance_pulse(Some(1.0));
        let start = v.pulse_phase.get();
        // Two renders in the same frame must not advance it twice.
        v.frame = 30;
        v.advance_pulse(Some(1.0));
        v.advance_pulse(Some(1.0));
        let d = (v.pulse_phase.get() - start).rem_euclid(1.0);
        assert!((d - 0.5).abs() < 1e-3, "30 frames of a 1 s pass: {d}");
    }

    /// Review Focus 2.
    #[test]
    fn a_backend_with_no_chips_draws_no_lanes_and_does_not_panic() {
        // Not `init()`ed: `MockBackend::new(0)` refuses to initialise, and an
        // un-initialised backend reports no devices, which is the case.
        let b = MockBackend::new(0);
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step = 10;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("STEP ANATOMY"), "{out}");
        assert!(
            !out.contains("chip0"),
            "no chip, no lane:
{out}"
        );
    }

    /// The lane columns share the bars' time axis, and a column with no chip
    /// sample says so rather than drawing a value.
    #[test]
    fn chip_lanes_line_up_with_the_step_bars_and_mark_missing_samples() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=8u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step_ms = 100.0;
        let v = TrainView::new(134, 40);
        let lane_row = |v: &TrainView, st: &TrainState| -> String {
            rows_of(&v.render(st, &b))
                .into_iter()
                .find(|r| r.contains("chip0"))
                .expect("a lane for chip 0")
        };
        // The view only sees steps 1..=4: the other four columns are missing.
        for step in 1..=4u64 {
            st.step = step;
            st.step_seq = step;
            v.render(&st, &b);
        }
        let row = lane_row(&v, &st);
        assert_eq!(row.matches(NO_SAMPLE).count(), 4, "{row:?}");
        // Seeing the rest fills every column.
        for step in 5..=8u64 {
            st.step = step;
            st.step_seq = step;
            v.render(&st, &b);
        }
        let row = lane_row(&v, &st);
        assert_eq!(row.matches(NO_SAMPLE).count(), 0, "{row:?}");
        // Right-aligned: the newest step's column is the band's right edge.
        // The `pulse` row always spans the full bar width, so the lane's last
        // glyph must end in the same column as it does.
        let last_col = |r: &str| r.trim_end().chars().count();
        let rows = rows_of(&v.render(&st, &b));
        let pulse = rows
            .iter()
            .find(|r| r.contains("pulse") && !r.contains("STEP ANATOMY"))
            .expect("a pulse row");
        assert_eq!(
            last_col(&row),
            last_col(pulse),
            "lane is not right-aligned with the band:\n{row:?}\n{pulse:?}"
        );
    }

    /// Review Focus 4.
    #[test]
    fn the_tapestry_fits_every_terminal_size() {
        let mut b = MockBackend::new(3);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=64u64)
            .map(|i| sample_at(i, 100.0 + i as f32, (i % 9 == 0) as u32))
            .collect();
        st.step = 64;
        st.step_ms = 150.0;
        for w in [20usize, 40, 60, 80, 100, 134] {
            for h in [10usize, 14, 20, 30, 40] {
                let v = TrainView::new(w, h);
                v.render(&st, &b);
                for line in v.render(&st, &b) {
                    let s: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
                    assert!(
                        !s.contains('╗') && !s.contains('╝'),
                        "right-side corner (w={w} h={h}): {s:?}"
                    );
                    for (pos, _) in s.match_indices('║') {
                        assert_eq!(pos, 0, "`║` only at column 0 (w={w} h={h}): {s:?}");
                    }
                    let cols = unicode_width::UnicodeWidthStr::width(s.as_str());
                    assert!(cols <= w, "line is {cols} cols at width {w}x{h}: {s:?}");
                }
            }
        }
    }

    #[test]
    fn the_legend_lists_step_symbols_only_when_there_are_step_samples() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let legend = |st: &TrainState| -> String {
            rows_of(&TrainView::new(150, 40).render(st, &b))
                .into_iter()
                .find(|r| r.contains("chip temp"))
                .unwrap_or_default()
        };
        let mut st = live_state();
        let without = legend(&st);
        assert!(
            !without.contains("step time") && !without.contains("compile"),
            "{without}"
        );
        st.step_history = vec![sample_at(1, 100.0, 0)];
        let with = legend(&st);
        assert!(
            with.contains("step time") && with.contains("compile"),
            "{with}"
        );
    }

    /// The topology text left the band, so it has to be findable on the MODEL
    /// card, and still must not state a count nobody sourced.
    #[test]
    fn the_model_card_states_known_topology_and_nothing_unknown() {
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let v = TrainView::new(100, 30);

        let st = live_state();
        assert_eq!(st.config.num_blocks, None);
        assert_eq!(st.config.num_heads, None);
        let out = text_of(&v.render(&st, &b));
        assert!(
            !out.contains("blocks") && !out.contains("heads"),
            "must not assert a block/head count when the config is unknown:\n{out}"
        );

        let mut st2 = live_state();
        st2.config.num_blocks = Some(12);
        st2.config.num_heads = Some(8);
        let out2 = text_of(&v.render(&st2, &b));
        assert!(out2.contains("blocks 12"), "{out2}");
        assert!(out2.contains("heads 8"), "{out2}");
    }

    #[test]
    fn gauges_show_the_busiest_chip_and_name_their_readings() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step_ms = 100.0;
        st.step = 10;
        let v = TrainView::new(134, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(out.contains("power") && out.contains("% TDP"), "{out}");
        assert!(out.contains("aiclk") && out.contains("MHz"), "{out}");
        // The mock backend has no PCIe counters, so there is no PCIe gauge.
        assert!(!out.contains("MB/s"), "{out}");
    }

    #[test]
    fn the_tokens_per_second_gauge_is_relative_to_the_runs_own_best() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.config.max_sequence_length = Some(256);
        st.batch_size = 8;
        st.step_history = vec![sample_at(1, 100.0, 0)];
        let v = TrainView::new(134, 40);
        st.step = 1;
        st.step_seq = 1;
        st.step_ms = 100.0;
        v.render(&st, &b); // best so far is set by this step rate
        st.step = 2;
        st.step_seq = 2;
        st.step_ms = 200.0; // half the rate
        let out = text_of(&v.render(&st, &b));
        assert!(out.contains("50% of best"), "{out}");
    }

    #[test]
    fn the_diagnosis_line_appears_with_its_readings() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = vec![sample_at(1, 100.0, 0), sample_at(2, 300.0, 6)];
        st.step = 2;
        st.step_ms = 300.0;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(
            out.contains("compiling - the program cache grew on the latest step"),
            "{out}"
        );
        // No compile and no readings that decide it: no verdict line at all.
        st.step_history = vec![sample_at(1, 100.0, 0), sample_at(2, 100.0, 0)];
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(!out.contains("compiling -"), "{out}");
    }

    #[test]
    fn the_convergence_strip_reads_the_loss_history_and_config() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.loss_history = (0..120).map(|i| 5.0 - 0.01 * i as f32).collect();
        st.config.learning_rate = Some(3.0e-4);
        st.scheduler = Some("cosine".into());
        st.max_steps = 100;
        st.step = 41;
        // 160 columns leaves the band 100 wide; the full strip is 85 to 90.
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("↘"), "{out}");
        assert!(out.contains("base lr 3.0e-4"), "{out}");
        assert!(out.contains("cosine 41% through"), "{out}");
        // A rising loss flips the arrow.
        st.loss_history = (0..120).map(|i| 1.0 + 0.01 * i as f32).collect();
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("↗") && !out.contains("↘"), "{out}");
    }

    /// At every width a gauge label that is drawn has its whole reading in
    /// the same cell. A reading cut to "10" (of "1043 MHz") or a label with
    /// no reading at all would state something the signal does not say.
    #[test]
    fn a_drawn_gauge_always_shows_its_whole_reading() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=64u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step = 64;
        st.step_ms = 100.0;
        st.config.max_sequence_length = Some(256);
        st.batch_size = 8;
        let mut gauges_seen = 0;
        for w in 20..=134usize {
            let v = TrainView::new(w, 40);
            v.render(&st, &b);
            let (x0, bw) = v.network_bounds();
            let half = bw / 2;
            let cells = [
                (x0, half.saturating_sub(1)),
                (x0 + half, (bw - half).saturating_sub(1)),
            ];
            for row in rows_of(&v.render(&st, &b)) {
                let chars: Vec<char> = row.chars().collect();
                for (start, cw) in cells {
                    if chars.len() < start + LABEL_W {
                        continue;
                    }
                    let head: String = chars[start..start + LABEL_W].iter().collect();
                    let suffix = match head.as_str() {
                        "aiclk " => "MHz",
                        "power " => "% TDP",
                        "tok/s " => "of best",
                        _ => continue,
                    };
                    gauges_seen += 1;
                    let cell: String = chars[start..chars.len().min(start + cw)].iter().collect();
                    assert!(
                        cell.contains(suffix),
                        "w={w}: gauge cell {cell:?} lacks its reading: {row:?}"
                    );
                }
            }
        }
        assert!(
            gauges_seen > 100,
            "the sweep must actually meet gauges: {gauges_seen}"
        );
    }

    /// A strip clause that is shown is whole at every width: the shown parts
    /// are exactly a leading run of what `convergence_parts` returned.
    #[test]
    fn the_convergence_strip_never_cuts_a_clause() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.loss_history = (0..120).map(|i| 5.0 - 0.01 * i as f32).collect();
        st.config.learning_rate = Some(3.0e-4);
        st.scheduler = Some("cosine".into());
        st.max_steps = 100;
        st.step = 41;
        let parts = convergence_parts(
            &st.loss_history,
            st.config.learning_rate,
            st.scheduler.as_deref(),
            st.step,
            st.max_steps,
        );
        assert!(parts.len() >= 3, "{parts:?}");
        let mut shown_counts = std::collections::BTreeSet::new();
        for w in 20..=200usize {
            let v = TrainView::new(w, 40);
            let (x0, bw) = v.network_bounds();
            for row in rows_of(&v.render(&st, &b)) {
                let chars: Vec<char> = row.chars().collect();
                // The strip row starts with the first part's arrow.
                if chars.len() <= x0 || !parts[0].starts_with(chars[x0]) {
                    continue;
                }
                let cell: String = chars[x0..chars.len().min(x0 + bw)].iter().collect();
                let cell = cell.trim_end();
                if !cell.starts_with(parts[0].as_str()) {
                    continue;
                }
                // It must be a whole leading run of the parts.
                let k = (1..=parts.len())
                    .find(|k| parts[..*k].join("  ") == cell)
                    .unwrap_or_else(|| {
                        panic!("w={w}: strip {cell:?} is not whole leading parts of {parts:?}")
                    });
                shown_counts.insert(k);
            }
        }
        // The sweep passes through every part count that can occur (not just
        // all or none), so the dropping path is exercised.
        assert!(shown_counts.len() >= 2, "{shown_counts:?}");
        for w in [134usize, 100] {
            let out = text_of(&TrainView::new(w, 40).render(&st, &b));
            assert!(
                !out.contains("cosi\n") && !out.contains("lr 3.0\n"),
                "{out}"
            );
        }
    }

    #[test]
    fn the_verdict_drops_its_cpu_clause_before_it_would_cut_a_reading() {
        use crate::animation::train_tapestry::{diagnose, Readings};
        let d = diagnose(&Readings {
            compiled_last_step: false,
            busiest_tdp_frac: Some(0.58),
            host_cpu_pct: Some(120.0),
        })
        .unwrap();
        let full = format!("▸ {}", d.text);
        let short = format!("▸ {}", d.short);
        assert!(
            full.ends_with("host cpu 120%") && !short.contains("host cpu"),
            "{full} / {short}"
        );
        for w in 0..=full.chars().count() + 2 {
            let got = TrainView::verdict_line(&d, w);
            let want = if w >= full.chars().count() {
                Some(full.clone())
            } else if w >= short.chars().count() {
                Some(short.clone())
            } else {
                None
            };
            assert_eq!(got, want, "w={w}");
        }
    }

    /// The `▸` verdict as rendered, at the view level, for a compute-bound
    /// state with a cpu reading. At each width the drawn line is exactly the
    /// full text, exactly the text without ", host cpu N%", or absent. This
    /// guards the wiring in `draw_tapestry` (a clipped line, or a verdict row
    /// reserved for a line that is not drawn, would fail it).
    #[test]
    fn the_rendered_verdict_is_whole_or_short_or_absent_at_every_width() {
        use crate::animation::train_tapestry::{diagnose, Readings};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        // The mock's power cannot be steered (it wanders and stays well under
        // the compute-bound threshold), so a small test double replaces the
        // one chip's telemetry with a fixed 58% of the mock's 120 W TDP.
        struct Fixed {
            inner: MockBackend,
            telem: crate::models::Telemetry,
        }
        impl TelemetryBackend for Fixed {
            fn init(&mut self) -> crate::error::BackendResult<()> {
                self.inner.init()
            }
            fn update(&mut self) -> crate::error::BackendResult<()> {
                self.inner.update()
            }
            fn devices(&self) -> &[Device] {
                self.inner.devices()
            }
            fn telemetry(&self, _i: usize) -> Option<&crate::models::Telemetry> {
                Some(&self.telem)
            }
            fn smbus_telemetry(&self, i: usize) -> Option<&crate::models::SmbusTelemetry> {
                self.inner.smbus_telemetry(i)
            }
            fn backend_info(&self) -> String {
                "fixed".into()
            }
        }
        let mut telem = b.telemetry(0).unwrap().clone();
        telem.power = Some(0.58 * 120.0);
        let b = Fixed { inner: b, telem };
        let probe = TrainView::new(134, 40);
        let c = probe.busiest_chip(&b).unwrap();
        let frac = c.power_w / c.tdp.expect("the mock reports a TDP");
        assert!((0.57..0.59).contains(&frac), "{frac}");
        let mut st = live_state();
        st.host_cpu_pct = Some(120.0);
        st.step_history = (1..=64u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step = 64;
        st.step_ms = 100.0;
        let d = diagnose(&Readings {
            compiled_last_step: false,
            busiest_tdp_frac: Some(frac),
            host_cpu_pct: Some(120.0),
        })
        .unwrap();
        assert!(
            d.text.ends_with("host cpu 120%") && d.short != d.text,
            "{d:?}"
        );
        let full = format!("▸ {}", d.text);
        let short = format!("▸ {}", d.short);
        let (mut saw_full, mut saw_short, mut saw_none) = (false, false, false);
        for w in 20..=200usize {
            let v = TrainView::new(w, 40);
            let (x0, bw) = v.network_bounds();
            let shown: Vec<String> = rows_of(&v.render(&st, &b))
                .into_iter()
                .filter_map(|r| {
                    let chars: Vec<char> = r.chars().collect();
                    (chars.get(x0) == Some(&'▸')).then(|| {
                        chars[x0..chars.len().min(x0 + bw)]
                            .iter()
                            .collect::<String>()
                            .trim_end()
                            .to_string()
                    })
                })
                .collect();
            assert!(shown.len() <= 1, "w={w}: {shown:?}");
            match shown.first() {
                Some(l) if *l == full => saw_full = true,
                Some(l) if *l == short => saw_short = true,
                Some(l) => {
                    panic!("w={w} (band {bw}): verdict {l:?} is neither {full:?} nor {short:?}")
                }
                None => saw_none = true,
            }
            // The rule itself: which one must appear at this band width.
            let want = if full.chars().count() <= bw {
                Some(&full)
            } else if short.chars().count() <= bw {
                Some(&short)
            } else {
                None
            };
            assert_eq!(shown.first(), want, "w={w} (band {bw})");
        }
        assert!(
            saw_full && saw_short && saw_none,
            "{saw_full} {saw_short} {saw_none}"
        );
    }
}
