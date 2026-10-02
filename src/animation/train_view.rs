// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Training view — a live tt-train run drawn as a starfield of step times over
//! a weave of chip signals, with loss "mountains" against an aurora nightscape.
//!
//! ## The colour language
//!
//! Each channel carries a different real signal — nothing is decorative:
//!
//! | Channel | Encodes |
//! |---|---|
//! | mountain hue red→cyan | loss magnitude (per-cell gradient in the river) |
//! | star height (starfield, braille, 2 steps per column) | wall time per step, trainer-reported or observed from a progress bar (the title says which) |
//! | star glyph teal `⠂` / purple `◆` / amber `✺` | normal step / program cache grew / checkpoint written |
//! | dotted `┈` horizon | the median step time of the steps shown |
//! | swell of the newest 3 stars | brightness cycles once per measured step (never faster than 5/s) |
//! | chip row height | power as a fraction of that chip's TDP (coral = aiclk dropped) |
//! | aiclk / pcie / host row height | busiest chip's aiclk vs its best, PCIe vs best seen, host CPU vs the window maximum |
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
use crate::animation::train_canvas::{
    braille_char, median_cell_row, place_stars, weave_columns, y_range, StarKind, DOTS_X,
};
use crate::animation::train_sky::sky_cell;
use crate::animation::train_tapestry::{
    advance_phase, bar_cell, convergence_parts, diagnose, median, pass_secs, plan_band, BandWants,
    ChipHistory, Diagnosis, DiagnosisKind, Readings, AICLK_DROP_FRAC, LABEL_W, MAX_LANES,
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

/// Muted violet-blue used for the frame border everywhere it appears.
const BORDER: Color = Color::Rgb(90, 90, 130);

/// Star colours. Teal and yellow are the docs-site brand tints; the compile
/// purple is the LIVE panel's cache colour so one meaning has one hue.
const BAR_NORMAL: Color = Color::Rgb(116, 197, 223);
const BAR_COMPILE: Color = Color::Rgb(180, 140, 230);
const BAR_CHECKPOINT: Color = Color::Rgb(246, 188, 66);
/// A chip or aiclk cell where aiclk had dropped well below that chip's best.
const AICLK_DROP: Color = Color::Rgb(255, 158, 138);
/// Resting colour of the median horizon and of the no-sample marker.
const WIRE_REST: Color = Color::Rgb(80, 90, 115);
/// Marker for a weave column with no reading. A glyph used nowhere else, so
/// a test can count missing samples exactly.
const NO_SAMPLE: char = '⋅';
/// A weave cell for a reading that exists but has no height: the row's scale
/// is 0 (no TDP known and every chip sample in the window reads 0 W), or the
/// reading itself is 0. The lowest bar glyph, so the cell says "sampled, at
/// the bottom of the scale" and is never taken for a missing sample.
const ZERO_SCALE_SAMPLE: char = '▁';
/// Weave row colours: aiclk, PCIe and host CPU. Chip power rows use the
/// chip's temperature colour.
const AICLK_ROW: Color = Color::Rgb(140, 190, 235);
const PCIE_ROW: Color = Color::Rgb(111, 171, 160);
const HOST_ROW: Color = Color::Rgb(210, 200, 150);
/// Widest value label at the right of a weave row (`1086 MHz`, `cpu 140%`).
/// A label that does not fit whole is left out.
const VALUE_W: usize = 10;
/// Data columns the band keeps before it gives room to the value labels.
const MIN_DATA_W: usize = 20;
/// The newest stars swell at the step rate.
const SWELL_STARS: usize = 3;

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
    /// Chip readings per step and the best-so-far values the weave rows are
    /// scaled to. Filled by `sample` during `render(&self, ...)`, so it is a
    /// `RefCell` for the same reason `cache_last` is a `Cell`.
    history: RefCell<ChipHistory>,
    /// Where the newest stars are in their swell, `[0, 1)`. Accumulated
    /// across frames so a change in step time alters the swell's speed and
    /// keeps its place in the cycle (an absolute-time phase jumped on every
    /// step-time update).
    swell_phase: StdCell<f32>,
    /// The `frame` the phase was last advanced to.
    swell_frame: StdCell<u64>,
    /// `(pid, attach time)` of the run `history` belongs to. The view lives
    /// across runs (it is rebuilt only on resize), and a new run's sample
    /// sequence number need not go down: two runs with no samples both sit
    /// at 0, and a backlog read at attach can start the new run at the old
    /// run's last number. `sample` clears `history` when this changes.
    run_id: StdCell<Option<(i32, Option<std::time::Instant>)>>,
}

/// One weave row: its label, one cell per data column (`None` for a column
/// left of the oldest shown step) and its current value.
struct WeaveRow {
    label: String,
    cells: Vec<Option<(char, Color)>>,
    value: String,
}

/// Where the band's data columns go. The starfield and every weave row share
/// them, so a step's star and its readings are in one column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BandGeom {
    /// First data column (after the 6-column labels).
    x_data: usize,
    /// Data columns. Each holds two steps.
    data_w: usize,
    /// First column of the value labels, `None` when the band is too narrow
    /// to keep [`MIN_DATA_W`] data columns beside them.
    value_x: Option<usize>,
}

/// The chip drawing the most power right now, standing in for "the chip the
/// training run is on". The diagnosis and the aiclk row both describe it.
/// Every chip the backend can see is a candidate because the trainer's chips
/// are not identified, so an idle neighbour never dilutes the reading the way
/// an average would.
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
            swell_phase: StdCell::new(0.0),
            swell_frame: StdCell::new(0),
            run_id: StdCell::new(None),
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

    /// `(x0, w)` of the tapestry band (header, starfield, weave rows,
    /// diagnosis line, convergence strip) — the space between whichever side
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
        // tapestry band, the river (which gets whatever's left over, since
        // it's the visual centerpiece), CHIPS, and the legend.
        let legend_row = self.height.saturating_sub(2);
        let chips_row = legend_row.saturating_sub(2);
        let network_top = 3;
        // Tall terminals give the tapestry band enough rows for its starfield
        // and weave rows (see `plan_band` in `train_tapestry`); short ones
        // fall back. Capped at
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
        // A chunk-local step is shown alone: it has no place in the run's
        // budget, so neither the total nor a percentage is drawn with it.
        // A step past its budget (a stale budget from an earlier run, or a
        // trainer that overran) is shown alone as well, never as `x / y`
        // with x above y.
        let chunk_local = st.step_is_chunk_local() || st.step > st.max_steps && st.max_steps > 0;
        let right_w = if st.max_steps > 0 || chunk_local {
            26
        } else {
            0
        };
        let left_room = self.width.saturating_sub(right_w + 2);
        self.text(
            buf,
            1,
            1,
            &Self::clip(&attach_desc, left_room),
            Color::Rgb(150, 160, 190),
            false,
        );

        if chunk_local {
            let step_str = format!("step {}", group_thousands(st.step as usize));
            let sx = self.width.saturating_sub(step_str.chars().count() + 1);
            self.text(buf, sx, 1, &step_str, bright, false);
        } else if st.max_steps > 0 {
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

    /// Record this frame's chip readings against the run's current sample
    /// sequence number, and fold PCIe throughput into its best-so-far value.
    /// A new run (another pid or attach time) first clears everything
    /// recorded for the previous one. `ChipHistory::record` also clears on a
    /// lower sequence number, which remains as a fallback.
    fn sample(&self, st: &TrainState, backend: &dyn TelemetryBackend) {
        let id = st.proc.as_ref().map(|p| (p.pid, st.first_seen));
        if self.run_id.get() != id {
            *self.history.borrow_mut() = ChipHistory::default();
            self.run_id.set(id);
        }
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
        h.record(
            st.step_seq,
            &chips,
            Self::pcie_total(backend),
            st.host_cpu_pct,
        );
        if let Some(bps) = Self::pcie_total(backend) {
            h.note_pcie(bps);
        }
    }

    /// Advance the swell phase to the current frame and return it, or `None`
    /// when no step time is known (the phase is then left alone). Advances by
    /// the frame delta, so repeated renders in one frame do not double-step.
    fn advance_swell(&self, secs: Option<f32>) -> Option<f32> {
        let secs = secs?;
        let frames = self.frame.saturating_sub(self.swell_frame.get());
        let phase = advance_phase(
            self.swell_phase.get(),
            frames,
            crate::animation::train_sky::ANIM_FPS,
            secs,
        );
        self.swell_phase.set(phase);
        self.swell_frame.set(self.frame);
        Some(phase)
    }

    /// Summed in+out PCIe bytes/sec across chips that report it, `None` when
    /// no chip does (only the sysfs/hybrid backends have these counters).
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

    /// Where the data columns go in a band starting at `x0`, `w` wide. The
    /// labels take the first [`LABEL_W`] columns and one column is left free
    /// at the right, as the band has always done. The value labels take a
    /// one-column gap plus [`VALUE_W`] columns, but only when at least
    /// [`MIN_DATA_W`] data columns remain beside them. Otherwise they are
    /// left out whole and the data takes their columns.
    fn band_geom(x0: usize, w: usize) -> BandGeom {
        let avail = w.saturating_sub(LABEL_W + 1);
        let x_data = x0 + LABEL_W;
        if avail >= MIN_DATA_W + 1 + VALUE_W {
            let data_w = avail - 1 - VALUE_W;
            BandGeom {
                x_data,
                data_w,
                value_x: Some(x_data + data_w + 1),
            }
        } else {
            BandGeom {
                x_data,
                data_w: avail,
                value_x: None,
            }
        }
    }

    /// The colour of a newest star at swell phase `phase`: `base` at the
    /// start of each cycle, lifted 60% of the way to white at its middle,
    /// and back. The lift follows `0.5 - 0.5 cos(2 pi phase)`, which is
    /// continuous across the wrap from 1 to 0, so a cycle boundary does not
    /// show as a jump. Only the brightness changes; the glyph stays.
    fn swell_color(base: Color, phase: f32) -> Color {
        let k = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
        let lift = |c: u8| -> u8 {
            let c = c as f32;
            (c + (255.0 - c) * 0.6 * k).round().clamp(0.0, 255.0) as u8
        };
        match base {
            Color::Rgb(r, g, b) => Color::Rgb(lift(r), lift(g), lift(b)),
            other => other,
        }
    }

    /// One weave row's cells from per-step readings (oldest first, one per
    /// shown step) on `cols` data columns. Two steps share a column and the
    /// larger reading wins (`weave_columns`, the same alignment as the
    /// stars). A column left of the oldest step is `None`. A column inside
    /// the data with no reading draws [`NO_SAMPLE`]. A reading draws a bar
    /// glyph at `reading / scale`, or [`ZERO_SCALE_SAMPLE`] when it has no
    /// height. `dropped`, when given, holds 1 for a step where aiclk had
    /// dropped (0 otherwise), and such a column is drawn in [`AICLK_DROP`].
    fn weave_cells(
        values: &[Option<f32>],
        dropped: Option<&[Option<f32>]>,
        cols: usize,
        scale: f32,
        color: Color,
    ) -> Vec<Option<(char, Color)>> {
        let n = values.len().min(cols * DOTS_X);
        let first = cols - n.div_ceil(DOTS_X);
        let merged = weave_columns(values, cols);
        let drops = dropped.map(|d| weave_columns(d, cols));
        merged
            .iter()
            .enumerate()
            .map(|(c, v)| {
                if c < first {
                    return None;
                }
                let Some(v) = v else {
                    return Some((NO_SAMPLE, WIRE_REST));
                };
                let ch = if scale > 0.0 {
                    bar_cell(v / scale, 0, 1)
                } else {
                    ' '
                };
                // A reading that exists is never drawn blank.
                let ch = if ch == ' ' { ZERO_SCALE_SAMPLE } else { ch };
                let hot = drops.as_ref().and_then(|d| d[c]).is_some_and(|f| f > 0.0);
                Some((ch, if hot { AICLK_DROP } else { color }))
            })
            .collect()
    }

    /// 1 when `aiclk` is below [`AICLK_DROP_FRAC`] of the chip's best
    /// `aimax`, 0 otherwise. Unknown readings never count as a drop.
    fn drop_flag(aiclk: u32, aimax: u32) -> f32 {
        let dropped = aiclk > 0 && aimax > 0 && (aiclk as f32) < AICLK_DROP_FRAC * aimax as f32;
        if dropped {
            1.0
        } else {
            0.0
        }
    }

    /// The chip power rows, one per chip in `devices`, over the `shown`
    /// steps. Each is scaled to the chip's TDP, or to the highest power in
    /// the window when no TDP is known, coloured by the chip's temperature,
    /// and coral where aiclk had dropped. The value is the chip's current
    /// power as a share of TDP, or in watts with no TDP.
    fn chip_rows(
        &self,
        backend: &dyn TelemetryBackend,
        devices: &[&Device],
        shown: &[crate::workload::train::StepSample],
        cols: usize,
    ) -> Vec<WeaveRow> {
        let hist = self.history.borrow();
        devices
            .iter()
            .map(|dev| {
                let telem = backend.telemetry(dev.index);
                let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);
                let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
                let samples: Vec<_> = shown
                    .iter()
                    .map(|s| hist.sample_at(dev.index, s.seq))
                    .collect();
                let tdp = Self::chip_tdp(backend, dev);
                let scale = tdp.unwrap_or_else(|| {
                    samples
                        .iter()
                        .flatten()
                        .map(|c| c.power_w)
                        .fold(0.0, f32::max)
                });
                let aimax = hist.aiclk_max(dev.index);
                let values: Vec<Option<f32>> =
                    samples.iter().map(|s| s.map(|c| c.power_w)).collect();
                let drops: Vec<Option<f32>> = samples
                    .iter()
                    .map(|s| s.map(|c| Self::drop_flag(c.aiclk_mhz, aimax)))
                    .collect();
                let value = match tdp {
                    Some(t) => format!("{:.0}% TDP", power / t * 100.0),
                    None => format!("{power:.0} W"),
                };
                WeaveRow {
                    label: format!("chip{}", dev.index),
                    cells: Self::weave_cells(
                        &values,
                        Some(&drops),
                        cols,
                        scale,
                        colors::temp_color(temp),
                    ),
                    value,
                }
            })
            .collect()
    }

    /// The aux rows that have a signal, in display order: the busiest chip's
    /// aiclk (relative to that chip's best, coral where it dropped), summed
    /// PCIe throughput (relative to the best seen) and host CPU (relative to
    /// the window maximum). A row whose signal is missing is left out.
    fn aux_rows(
        &self,
        st: &TrainState,
        backend: &dyn TelemetryBackend,
        busiest: Option<&Busiest>,
        shown: &[crate::workload::train::StepSample],
        cols: usize,
    ) -> Vec<WeaveRow> {
        let hist = self.history.borrow();
        let mut out = Vec::new();
        if let Some(chip) = busiest.filter(|c| c.aiclk_mhz > 0) {
            let aimax = hist.aiclk_max(chip.index).max(chip.aiclk_mhz);
            let samples: Vec<_> = shown
                .iter()
                .map(|s| hist.sample_at(chip.index, s.seq))
                .collect();
            let values: Vec<Option<f32>> = samples
                .iter()
                .map(|s| s.map(|c| c.aiclk_mhz as f32))
                .collect();
            let drops: Vec<Option<f32>> = samples
                .iter()
                .map(|s| s.map(|c| Self::drop_flag(c.aiclk_mhz, aimax)))
                .collect();
            out.push(WeaveRow {
                label: "aiclk".into(),
                cells: Self::weave_cells(&values, Some(&drops), cols, aimax as f32, AICLK_ROW),
                value: format!("{} MHz", chip.aiclk_mhz),
            });
        }
        if let Some(bps) = Self::pcie_total(backend) {
            // Relative to the highest throughput seen, because no single
            // ceiling is right for every link generation and width.
            let best = hist.best_pcie_bps().max(bps) as f32;
            let values: Vec<Option<f32>> = shown
                .iter()
                .map(|s| hist.pcie_at(s.seq).map(|v| v as f32))
                .collect();
            out.push(WeaveRow {
                label: "pcie".into(),
                cells: Self::weave_cells(&values, None, cols, best, PCIE_ROW),
                value: format!("{:.0} MB/s", bps / 1e6),
            });
        }
        if let Some(cpu) = st.host_cpu_pct {
            let values: Vec<Option<f32>> = shown.iter().map(|s| hist.host_at(s.seq)).collect();
            let scale = values.iter().flatten().copied().fold(0.0, f32::max);
            out.push(WeaveRow {
                label: "host".into(),
                cells: Self::weave_cells(&values, None, cols, scale, HOST_ROW),
                value: format!("cpu {cpu:.0}%"),
            });
        }
        out
    }

    /// Draw one weave row at `y`: label, cells, and the value when the band
    /// has a value column and the value fits it whole.
    fn draw_weave_row(
        &self,
        buf: &mut [Vec<Cell>],
        x0: usize,
        y: usize,
        g: BandGeom,
        row: &WeaveRow,
    ) {
        self.text(
            buf,
            x0,
            y,
            &Self::clip(&row.label, LABEL_W),
            Color::Rgb(120, 130, 155),
            false,
        );
        for (c, cell) in row.cells.iter().enumerate() {
            if let Some((ch, col)) = cell {
                self.put(buf, g.x_data + c, y, *ch, *col, false);
            }
        }
        if let Some(vx) = g.value_x {
            if row.value.chars().count() <= VALUE_W {
                self.text(buf, vx, y, &row.value, Color::Rgb(210, 230, 220), false);
            }
        }
    }

    /// The tapestry band: a header, a starfield of step times, a weave of
    /// chip power, aiclk, PCIe and host CPU rows on the same time axis, a
    /// one-line diagnosis and a convergence strip, drawn only from
    /// `st.step_history`, the measured step time, chip telemetry, the host
    /// reading, the loss history and the run config. A layer whose signal is
    /// missing is left out. Text (header clauses, value labels, diagnosis,
    /// strip) shows whole or not at all: a clause that does not fit the band
    /// is dropped, never cut.
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
        let geom = Self::band_geom(x0, w);
        let cols = geom.data_w;

        let lane_devices: Vec<&Device> = backend
            .devices()
            .iter()
            .filter(|d| backend.telemetry(d.index).is_some())
            .take(MAX_LANES)
            .collect();
        let busiest = self.busiest_chip(backend);
        // The convergence strip keeps whole leading clauses only; later
        // clauses that do not fit are dropped, never cut.
        let parts = convergence_parts(
            &st.loss_history,
            st.config.learning_rate,
            st.scheduler.as_deref(),
            st.step,
            // Once the bar has restarted, or the step counts within a chunk,
            // the step cannot be placed in the run's budget, so 0 (unknown)
            // keeps the strip from claiming a schedule position. A step past
            // the budget gets the same treatment.
            if st.chunked_bar || st.step_is_chunk_local() || st.step > st.max_steps {
                0
            } else {
                st.max_steps
            },
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
        // The newest steps that fit, two per data column, right-aligned so
        // the latest step is always in the last data column and every weave
        // row shares the same columns.
        let hist = &st.step_history;
        let n = hist.len().min(cols * DOTS_X);
        let shown = &hist[hist.len() - n..];
        // Step times the canvas can place. The axis range, the horizon and
        // the header's median all come from these, so they agree.
        let ms: Vec<f32> = shown
            .iter()
            .map(|s| s.ms)
            .filter(|m| m.is_finite() && *m > 0.0)
            .collect();
        // The plan grants star rows only when this range exists, and the
        // starfield below draws from the same range, so every granted star
        // row is drawn and no star is drawn in a row that was not granted.
        let range = y_range(&ms);
        let lanes = self.chip_rows(backend, &lane_devices, shown, cols);
        let mut aux = self.aux_rows(st, backend, busiest.as_ref(), shown, cols);
        if shown.is_empty() {
            // With no steps an aux row has no cells, so it is kept only when
            // its value can be shown; a bare label states nothing.
            aux.retain(|r| geom.value_x.is_some() && r.value.chars().count() <= VALUE_W);
        }
        let plan = plan_band(
            network_h,
            BandWants {
                steps: !shown.is_empty(),
                stars: range.is_some(),
                lanes: lanes.len(),
                aux: aux.len(),
                verdict: verdict.is_some(),
                strip: !strip.is_empty(),
            },
        );

        // Advanced even when no star row is drawn, so the swell does not
        // jump when the starfield reappears.
        let phase = self.advance_swell(pass_secs(st.step_ms));

        // ── header ───────────────────────────────────────────────────
        // The header is a title followed by clauses. Clauses are kept from
        // the left while they fit whole; the first one that does not fit and
        // every one after it are dropped, so a reading is never cut to a
        // different number ("median 112" for "median 1125 ms"). The title is
        // always drawn, clipped only when the band is narrower than the title.
        let (title, clauses): (&str, Vec<String>) = if hist.is_empty() {
            (
                "STEP ANATOMY",
                vec!["no per-step times reported".to_string()],
            )
        } else {
            // Times polled from a progress bar are disclosed in the title so
            // they are never taken for ones the trainer printed.
            let title = if st.step_time_source == StepTimeSource::Observed {
                "STEP ANATOMY (from bar)"
            } else {
                "STEP ANATOMY"
            };
            let mut clauses = Vec::new();
            if n > 0 {
                clauses.push(format!("last {n} steps"));
            }
            if let Some(med) = median(&ms) {
                clauses.push(format!("median {med:.0} ms"));
            }
            (title, clauses)
        };
        let mut header = title.to_string();
        for (i, c) in clauses.iter().enumerate() {
            // Two spaces after the title, a middle dot between clauses.
            let sep = if i == 0 { "  " } else { " · " };
            if header.chars().count() + sep.chars().count() + c.chars().count() > w {
                break;
            }
            header.push_str(sep);
            header.push_str(c);
        }
        self.text(buf, x0, network_top, &Self::clip(&header, w), label, false);

        // ── starfield ────────────────────────────────────────────────
        let y_stars = network_top + 1;
        let rows = plan.star_rows;
        // `place_stars` is only called with a usable range: `y_range` is
        // `None` when no step time can be placed, and then `rows` is 0.
        if let (true, Some((lo, hi))) = (rows > 0, range) {
            // The scale is printed: the top of the range on the first star
            // row and the bottom on the last. A value too wide for the label
            // column is left out.
            let mut axis = |y: usize, v: f32| {
                let s = format!("{v:>5.0}");
                if s.chars().count() < LABEL_W {
                    self.text(buf, x0, y, &s, dim, false);
                }
            };
            axis(y_stars, hi);
            if rows > 1 {
                axis(y_stars + rows - 1, lo);
            }
            let grid = place_stars(shown, cols, rows, lo, hi, SWELL_STARS);
            let med_row = median(&ms).and_then(|m| median_cell_row(m, lo, hi, rows));
            for (r, line) in grid.iter().enumerate() {
                let y = y_stars + r;
                for (c, cell) in line.iter().enumerate() {
                    let x = geom.x_data + c;
                    if cell.is_empty() {
                        // The median horizon shows only where no star is.
                        if med_row == Some(r) {
                            self.put(buf, x, y, '┈', WIRE_REST, false);
                        }
                        continue;
                    }
                    let (ch, base) = match cell.kind {
                        StarKind::Compile => ('◆', BAR_COMPILE),
                        StarKind::Checkpoint => ('✺', BAR_CHECKPOINT),
                        StarKind::Normal => (braille_char(cell.bits), BAR_NORMAL),
                    };
                    let col = match phase {
                        Some(p) if cell.newest > 0 => Self::swell_color(base, p),
                        _ => base,
                    };
                    self.put(buf, x, y, ch, col, false);
                }
            }
            if let (Some(r), Some(vx)) = (med_row, geom.value_x) {
                self.text(buf, vx, y_stars + r, "median", dim, false);
            }
        }

        // ── weave: chip power rows, then aiclk, PCIe, host ───────────
        let mut y = y_stars + rows;
        for row in lanes.iter().take(plan.lane_rows) {
            self.draw_weave_row(buf, x0, y, geom, row);
            y += 1;
        }
        for row in aux.iter().take(plan.aux_rows) {
            self.draw_weave_row(buf, x0, y, geom, row);
            y += 1;
        }

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
            // The row states a count, so it needs one the trainer reported.
            // A trainer that prints no cache count (or only a bar) leaves
            // `cache_entries` at 0, and "cache 0 steady" would claim a
            // reading nobody made. The tracking above still runs so a count
            // that appears later starts from the right baseline.
            if st.cache_entries > 0 {
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
        let all: [(char, &str, Color); 10] = [
            ('●', "loss", loss_color),
            ('·', "step", BAR_NORMAL),
            ('◆', "compile", BAR_COMPILE),
            ('✺', "ckpt", BAR_CHECKPOINT),
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
        // The star glyphs (`step`, `compile`, `ckpt`) need step samples, so a
        // trainer that prints no per-step time does not advertise them. The
        // `✦ checkpoint` entry is the river's comet, a different glyph on
        // purpose, and stays.
        //
        // This tracks whether the river is drawn, not whether a stream
        // exists — an earlier version keyed on the stream and dropped the
        // aurora and comet, which was right only while the no-log state
        // replaced the river wholesale.
        let has_steps = !st.step_history.is_empty();
        let entries: Vec<(char, &str, Color)> = all
            .into_iter()
            .filter(|(_, label, _)| has_stream || !matches!(*label, "loss" | "loss ↓" | "loss ↑"))
            .filter(|(_, label, _)| has_steps || !matches!(*label, "step" | "compile" | "ckpt"))
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
        // whichever side panels `panel_fit` says are shown, the tapestry
        // band's own bounds must never reach into them.
        for w in 20..=140usize {
            let v = TrainView::new(w, 40);
            let (show_model, show_live) = v.panel_fit();
            let (x0, netw) = v.network_bounds();
            let net_end = x0 + netw;
            if show_live {
                let rx = w.saturating_sub(v.right_w() + 1);
                assert!(
                    net_end <= rx,
                    "band overruns the LIVE panel at w={w}: x0={x0} netw={netw} rx={rx}"
                );
            } else {
                assert!(
                    net_end <= w,
                    "band overruns the screen at w={w}: x0={x0} netw={netw}"
                );
            }
            if show_model {
                assert!(x0 >= 3, "model card column missing room at w={w}");
            } else {
                assert_eq!(x0, 2, "band should start at the left margin at w={w}");
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
    /// run: lanes and best-so-far values keep their history. The samples'
    /// step numbers (3193..=3195, then 1..=3) differ from their sequence
    /// numbers (1..=6), so a lane looked up by step would come up empty.
    #[test]
    fn a_bar_restart_does_not_clear_the_chip_lanes() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = [3193u64, 3194, 3195, 1, 2, 3]
            .into_iter()
            .zip(1..=6u64)
            .map(|(step, seq)| crate::workload::train::StepSample {
                seq,
                ..sample_at(step, 285.0, 0)
            })
            .collect();
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

    /// A trainer that prints its own global step times and also shows a bar
    /// that restarts every chunk. Driven through `apply_event`, so the strip
    /// depends on the state noticing the restart by itself.
    #[test]
    fn a_chunked_bar_beside_reported_steps_makes_no_schedule_claim() {
        use crate::workload::train::TrainEvent;
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.scheduler = Some("cosine".into());
        st.apply_event(TrainEvent::MaxSteps(63906));
        for i in 0..20u64 {
            st.apply_event(TrainEvent::StepAndMs {
                step: 25546 + i,
                loss: 3.0 - 0.01 * i as f32,
                ms: 285.0,
            });
        }
        for step in [630u64, 1] {
            st.apply_event(TrainEvent::BarProgress {
                step,
                max_steps: 3195,
                loss: 2.8,
            });
        }
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("cosine"), "{out}");
        assert!(!out.contains("% through"), "{out}");
    }

    /// A bar-only trainer whose summary states the run budget and whose bar
    /// counts within a chunk. The header shows the chunk-local step with no
    /// budget and no percentage, both before and after the bar restarts.
    #[test]
    fn a_chunk_local_step_is_shown_without_the_run_budget() {
        use crate::workload::train::TrainEvent;
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let bar = |step: u64| TrainEvent::BarProgress {
            step,
            max_steps: 3195,
            loss: 2.8,
        };
        let header_row = |st: &TrainState| -> String {
            rows_of(&TrainView::new(134, 40).render(st, &b))
                .into_iter()
                .find(|r| r.contains("auto-attached"))
                .expect("the second header row")
        };
        let mut st = live_state();
        st.apply_event(TrainEvent::HarnessSummary {
            max_steps: 63906,
            batch_size: 8,
            seq_len: 512,
        });
        st.step_ms = 285.0;
        // Before any restart, then after one.
        for (step, label) in [(3194u64, "before"), (630, "after")] {
            st.apply_event(bar(step));
            let row = header_row(&st);
            let want = format!("step {}", group_thousands(step as usize));
            assert!(row.contains(&want), "{label}: {row:?}");
            assert!(
                !row.contains("63,906") && !row.contains("3,195"),
                "{label}: {row:?}"
            );
            assert!(
                !row.contains('%') && !row.contains(" / "),
                "{label}: {row:?}"
            );
        }
        assert!(st.chunked_bar);
    }

    /// The header counts the steps the starfield shows and states their
    /// median. 200 steps do not fit the 56 data columns at 134 wide, so the
    /// header says 112 (two per column), and the slow outliers among them do
    /// not move the median.
    #[test]
    fn the_header_counts_the_steps_shown_and_states_their_median() {
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        let mut st = live_state();
        st.step_history = (1..=31u64)
            .map(|i| sample_at(i, if i == 20 { 400.0 } else { 100.0 }, 0))
            .collect();
        st.step = 31;
        st.step_ms = 100.0;
        let header = |st: &TrainState| -> String {
            rows_of(&TrainView::new(134, 40).render(st, &b))
                .into_iter()
                .find(|r| r.contains("STEP ANATOMY"))
                .expect("the band header should render")
        };
        let h = header(&st);
        assert!(h.contains("last 31 steps · median 100 ms"), "{h}");
        st.step_history = (1..=200u64)
            .map(|i| sample_at(i, if i % 10 == 0 { 900.0 } else { 100.0 }, 0))
            .collect();
        let h = header(&st);
        assert!(h.contains("last 112 steps · median 100 ms"), "{h}");
    }

    /// The header drops whole clauses from the right when the band is narrow.
    /// A reading is never cut to a different number ("median 112" for
    /// "median 1125 ms"), and the step count is never shown in part.
    #[test]
    fn the_header_drops_whole_clauses_and_never_cuts_a_reading() {
        use crate::workload::train::StepTimeSource;
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        // `last N steps` with N all digits, if the header shows `last` at all.
        let step_count_whole = |h: &str| -> bool {
            match h.find("last ") {
                None => true,
                Some(i) => {
                    let rest = &h[i + "last ".len()..];
                    let digits = rest.chars().take_while(char::is_ascii_digit).count();
                    digits > 0 && rest[digits..].starts_with(" steps")
                }
            }
        };
        for (med, source) in [285u32, 1125]
            .into_iter()
            .flat_map(|m| [(m, StepTimeSource::Observed), (m, StepTimeSource::Reported)])
        {
            let mut st = live_state();
            st.step_history = (1..=64u64).map(|i| sample_at(i, med as f32, 0)).collect();
            st.step_seq = 64;
            st.step = 64;
            st.step_ms = med as f32;
            st.step_time_source = source;
            for w in 60..=134usize {
                let rows = rows_of(&TrainView::new(w, 40).render(&st, &b));
                let header = rows
                    .iter()
                    .find(|r| r.contains("STEP ANATOMY"))
                    .unwrap_or_else(|| panic!("w={w}: no header"));
                if header.contains("median") {
                    assert!(
                        header.contains(&format!("median {med} ms")),
                        "w={w} med={med}: {header:?}"
                    );
                }
                assert!(step_count_whole(header), "w={w} med={med}: {header:?}");
                // Not vacuous: the widest terminal has room for the median.
                if w == 134 {
                    assert!(header.contains("median"), "w={w}: {header:?}");
                }
            }
        }
    }

    /// Changing step_ms between frames changes the swell's speed and keeps
    /// its place in the cycle. An absolute-time phase jumped by most of a
    /// pass.
    #[test]
    fn the_swell_phase_is_continuous_across_step_time_changes() {
        let mut v = TrainView::new(134, 40);
        v.frame = 36000;
        v.advance_swell(Some(0.5));
        let mut prev = v.advance_swell(Some(0.5)).unwrap();
        for (i, secs) in [0.505f32, 0.48, 0.5, 0.52].into_iter().enumerate() {
            v.frame = 36001 + i as u64;
            let cur = v.advance_swell(Some(secs)).unwrap();
            let d = (cur - prev).rem_euclid(1.0);
            assert!(d < 0.05, "phase jumped {d:.3} in one frame");
            prev = cur;
        }
        // Unknown step time leaves the phase alone.
        let before = v.swell_phase.get();
        v.frame += 10;
        assert_eq!(v.advance_swell(None), None);
        assert_eq!(v.swell_phase.get(), before);
    }

    #[test]
    fn the_swell_phase_advances_at_the_step_rate_for_a_constant_step_time() {
        let mut v = TrainView::new(134, 40);
        v.advance_swell(Some(1.0));
        let start = v.swell_phase.get();
        // Two renders in the same frame must not advance it twice.
        v.frame = 30;
        v.advance_swell(Some(1.0));
        v.advance_swell(Some(1.0));
        let d = (v.swell_phase.get() - start).rem_euclid(1.0);
        assert!((d - 0.5).abs() < 1e-3, "30 frames of a 1 s pass: {d}");
    }

    /// A backend that reports no chips: no lanes, and nothing panics.
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

    /// The chip row shares the starfield's columns, two steps per column, and
    /// a column with no chip sample says so with the no-sample marker.
    #[test]
    fn chip_lanes_line_up_with_the_stars_and_mark_missing_samples() {
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
        // The view only sees steps 1..=4: steps 5..=8 fill two columns, and
        // both are missing.
        for step in 1..=4u64 {
            st.step = step;
            st.step_seq = step;
            v.render(&st, &b);
        }
        let row = lane_row(&v, &st);
        assert_eq!(row.matches(NO_SAMPLE).count(), 2, "{row:?}");
        // Seeing the rest fills every column.
        for step in 5..=8u64 {
            st.step = step;
            st.step_seq = step;
            v.render(&st, &b);
        }
        let row = lane_row(&v, &st);
        assert_eq!(row.matches(NO_SAMPLE).count(), 0, "{row:?}");
        // Right-aligned: the newest step's column is the last data column,
        // for the lane and for the stars, and the lane's four columns are
        // the four columns the stars occupy.
        let rows: Vec<Vec<char>> = rows_of(&v.render(&st, &b))
            .iter()
            .map(|r| r.chars().collect())
            .collect();
        let (x0, _, x_data, data_w, _) = band_geom(&v);
        let lane = band_rows(&rows, x0, "chip0")[0];
        let header = band_rows(&rows, x0, "STEP ANATOMY")[0];
        let lane_cols: Vec<usize> = (x_data..x_data + data_w)
            .filter(|&x| rows[lane][x] != ' ')
            .collect();
        let star_cols: Vec<usize> = (x_data..x_data + data_w)
            .filter(|&x| (header + 1..lane).any(|r| is_star(rows[r][x])))
            .collect();
        assert_eq!(
            lane_cols,
            (x_data + data_w - 4..x_data + data_w).collect::<Vec<_>>()
        );
        assert_eq!(lane_cols, star_cols, "{:?}", rows[lane]);
    }

    /// Every width from 20 to 134 and every height from 10 to 40, with every
    /// weave row present: no right-side border and no line wider than the
    /// terminal.
    #[test]
    fn the_tapestry_fits_every_terminal_size() {
        let b = pcie_double(3, Some(3.1e8));
        let mut st = live_state();
        st.step_history = (1..=64u64)
            .map(|i| sample_at(i, 100.0 + i as f32, (i % 9 == 0) as u32))
            .collect();
        st.step = 64;
        st.step_seq = 64;
        st.step_ms = 150.0;
        st.host_cpu_pct = Some(140.0);
        for w in 20..=134usize {
            for h in 10..=40usize {
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
        for absent in ["· step", "◆ compile", "✺ ckpt"] {
            assert!(!without.contains(absent), "{absent:?} in {without}");
        }
        // The checkpoint comet is drawn either way and keeps its own entry.
        assert!(without.contains("✦ checkpoint"), "{without}");
        st.step_history = vec![sample_at(1, 100.0, 0)];
        let with = legend(&st);
        for present in ["· step", "◆ compile", "✺ ckpt", "✦ checkpoint"] {
            assert!(with.contains(present), "{present:?} missing from {with}");
        }
        assert!(!with.contains("step time"), "{with}");
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

    /// A state for a run attached at `first_seen` with process `pid` and a
    /// measured step time of `step_ms`.
    fn run_state(pid: i32, first_seen: std::time::Instant, step_ms: f32) -> TrainState {
        let mut st = live_state();
        if let Some(p) = st.proc.as_mut() {
            p.pid = pid;
        }
        st.first_seen = Some(first_seen);
        st.config.max_sequence_length = Some(256);
        st.batch_size = 8;
        st.step_ms = step_ms;
        st
    }

    /// The view outlives a run. A new run (another pid and attach time) must
    /// start with an empty chip history even when its sample sequence number
    /// does not go down: here run B's backlog is read at attach, so its first
    /// frame is already at sequence number 5, the same as run A's last. Run
    /// A's samples must not fill run B's chip lane.
    #[test]
    fn a_new_run_starts_with_an_empty_chip_lane_through_the_same_view() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let v = TrainView::new(134, 40);
        let t0 = std::time::Instant::now();
        let history: Vec<_> = (1..=5u64).map(|i| sample_at(i, 100.0, 0)).collect();

        let mut a = run_state(100, t0, 100.0);
        a.step_history = history.clone();
        for seq in 1..=5u64 {
            a.step_seq = seq;
            a.step = seq;
            v.render(&a, &b);
        }

        // Another pid and attach time: the view must treat this as a new run.
        let mut run_b = run_state(200, t0 + std::time::Duration::from_secs(1), 200.0);
        run_b.step_history = history;
        run_b.step_seq = 5;
        run_b.step = 5;
        let rows = rows_of(&v.render(&run_b, &b));
        let lane = rows
            .iter()
            .find(|r| r.contains("chip0"))
            .expect("a lane for chip 0");
        // Five steps take three columns (1 | 2 3 | 4 5). Only step 5 was
        // seen in run B, so the first two columns are missing.
        assert_eq!(
            lane.matches(NO_SAMPLE).count(),
            2,
            "run A's samples must not fill run B's columns: {lane:?}"
        );
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

    /// `sample` hands the summed PCIe throughput and the host CPU reading to
    /// the history under the run's sample sequence number, and a missing
    /// signal stays missing. The ring arithmetic has its own tests in `train_tapestry`.
    #[test]
    fn sample_passes_pcie_and_host_cpu_to_the_history() {
        use crate::backend::pcie_counters::PcieBandwidth;
        struct WithPcie {
            inner: MockBackend,
            pcie: bool,
        }
        impl TelemetryBackend for WithPcie {
            fn init(&mut self) -> crate::error::BackendResult<()> {
                self.inner.init()
            }
            fn update(&mut self) -> crate::error::BackendResult<()> {
                self.inner.update()
            }
            fn devices(&self) -> &[Device] {
                self.inner.devices()
            }
            fn telemetry(&self, i: usize) -> Option<&crate::models::Telemetry> {
                self.inner.telemetry(i)
            }
            fn smbus_telemetry(&self, i: usize) -> Option<&crate::models::SmbusTelemetry> {
                self.inner.smbus_telemetry(i)
            }
            fn backend_info(&self) -> String {
                "pcie-double".into()
            }
            fn pcie_bandwidth(&self, _i: usize) -> Option<PcieBandwidth> {
                self.pcie.then_some(PcieBandwidth {
                    rx_bytes_per_sec: 1.0e9,
                    tx_bytes_per_sec: 0.5e9,
                })
            }
        }
        let mut inner = MockBackend::new(2);
        inner.init().unwrap();
        let mut b = WithPcie { inner, pcie: true };
        let v = TrainView::new(134, 40);
        let mut st = live_state();
        st.step_seq = 7;
        st.host_cpu_pct = Some(42.0);
        v.sample(&st, &b);
        // Two chips at 1.5e9 each.
        assert_eq!(v.history.borrow().pcie_at(7), Some(3.0e9));
        assert_eq!(v.history.borrow().host_at(7), Some(42.0));
        // Next sample: no PCIe counters and no host reading.
        b.pcie = false;
        st.step_seq = 8;
        st.host_cpu_pct = None;
        v.sample(&st, &b);
        assert_eq!(v.history.borrow().pcie_at(8), None);
        assert_eq!(v.history.borrow().host_at(8), None);
        assert_eq!(v.history.borrow().pcie_at(7), Some(3.0e9));
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

    /// A chip with no known TDP whose every sample in the window reads 0 W.
    /// The scale is then 0, and each cell must still show that a sample
    /// exists: the lowest bar glyph, never the no-sample marker.
    #[test]
    fn a_zero_power_window_with_no_tdp_draws_samples_as_present() {
        let mut inner = MockBackend::new(1);
        inner.init().unwrap();
        struct ZeroPower {
            devices: Vec<Device>,
            telem: crate::models::Telemetry,
        }
        impl TelemetryBackend for ZeroPower {
            fn init(&mut self) -> crate::error::BackendResult<()> {
                Ok(())
            }
            fn update(&mut self) -> crate::error::BackendResult<()> {
                Ok(())
            }
            fn devices(&self) -> &[Device] {
                &self.devices
            }
            fn telemetry(&self, _i: usize) -> Option<&crate::models::Telemetry> {
                Some(&self.telem)
            }
            fn smbus_telemetry(&self, _i: usize) -> Option<&crate::models::SmbusTelemetry> {
                None // no TDP from SMBUS
            }
            fn backend_info(&self) -> String {
                "zero power".into()
            }
        }
        let mut telem = inner.telemetry(0).unwrap().clone();
        telem.power = Some(0.0);
        let mut devices = inner.devices().to_vec();
        for d in &mut devices {
            d.limits = None; // no TDP from the device limits either
        }
        let b = ZeroPower { devices, telem };
        assert_eq!(TrainView::chip_tdp(&b, &b.devices[0]), None);
        let mut st = live_state();
        st.step_history = (1..=6u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step_ms = 100.0;
        let v = TrainView::new(134, 40);
        for seq in 1..=6u64 {
            st.step_seq = seq;
            st.step = seq;
            v.render(&st, &b);
        }
        let lane = rows_of(&v.render(&st, &b))
            .into_iter()
            .find(|r| r.contains("chip0"))
            .expect("a lane for chip 0");
        // Six steps, two per column: three sampled columns.
        assert_eq!(lane.matches(NO_SAMPLE).count(), 0, "{lane:?}");
        assert_eq!(lane.matches(ZERO_SCALE_SAMPLE).count(), 3, "{lane:?}");
    }

    #[test]
    fn the_live_panel_shows_a_cache_row_only_when_a_cache_count_was_reported() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.step = 50;
        st.cache_entries = 0;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(!out.contains("cache "), "no count reported, no row:\n{out}");
        st.cache_entries = 21;
        let out = text_of(&TrainView::new(134, 40).render(&st, &b));
        assert!(out.contains("cache   21"), "{out}");
    }

    /// The second header row of a render at 134 columns.
    fn header_row_of(st: &TrainState) -> String {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        rows_of(&TrainView::new(134, 40).render(st, &b))
            .into_iter()
            .find(|r| r.contains("auto-attached"))
            .expect("the second header row")
    }

    #[test]
    fn the_header_never_draws_a_step_past_its_budget() {
        let mut st = live_state();
        st.step = 38340;
        st.max_steps = 25560;
        let row = header_row_of(&st);
        assert!(row.contains("step 38,340"), "{row:?}");
        assert!(!row.contains("25,560") && !row.contains(" / "), "{row:?}");
        st.step = 41541;
        st.max_steps = 63906;
        let row = header_row_of(&st);
        assert!(row.contains("step 41,541 / 63,906  65.0%"), "{row:?}");
    }

    #[test]
    fn the_strip_makes_no_schedule_claim_for_a_step_past_its_budget() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        st.loss_history = (0..120).map(|i| 5.0 - 0.01 * i as f32).collect();
        st.scheduler = Some("cosine".into());
        st.step = 38340;
        st.max_steps = 25560;
        let out = text_of(&TrainView::new(160, 40).render(&st, &b));
        assert!(out.contains("cosine"), "{out}");
        assert!(!out.contains("% through"), "{out}");
    }

    /// A state with a process and a log but no step data, as at attach.
    fn attached_state() -> TrainState {
        use crate::workload::train::{LogSource, TrainProcess};
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "python".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st
    }

    /// The `x` and `y` of a `step x / y` clause in a header row, if drawn.
    fn drawn_ratio(row: &str) -> Option<(u64, u64)> {
        let t = &row[row.find("step ")? + 5..];
        let (x, y) = t.split_once(" / ")?;
        let num = |s: &str| -> u64 {
            s.trim()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .filter(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap()
        };
        Some((num(x), num(y)))
    }

    /// Replays the shape of the user's appended two-run log through the parser
    /// and the state, from a fresh attach, checking the header after every
    /// line. `with_resume` includes the resume line a harness may not print.
    fn replay_two_run_log(with_resume: bool) -> TrainState {
        use crate::workload::train::parse_train_line;
        let dash = "\u{2014}";
        let mut lines = vec![
            format!("tt-tnt training {dash} steps=63906 batch=64 seq_len=512 arch=blackhole"),
            "  step=   3195 train_loss=3.5 val_loss=3.6 lr=3.0e-4".to_string(),
            "  step=  38340 train_loss=3.1 val_loss=3.2 lr=2.0e-4".to_string(),
            format!("tt-tnt training {dash} steps=25560 batch=64 seq_len=512 arch=blackhole"),
        ];
        if with_resume {
            lines.push("  resumed from a/tt_tnt_step00038346.pkl at step 38346 (created_at=2026-10-02T03:28:05+00:00); running 25560 more steps to step 63906".to_string());
        }
        lines.push("  step=  41541 train_loss=3.0 val_loss=3.1 lr=2.0e-4".to_string());
        let mut st = attached_state();
        for l in &lines {
            st.apply_event(parse_train_line(l).expect("every replayed line parses"));
            let row = header_row_of(&st);
            if let Some((x, y)) = drawn_ratio(&row) {
                assert!(x <= y, "after {l:?}: {row:?}");
            }
        }
        st
    }

    #[test]
    fn replaying_a_resumed_two_run_log_never_draws_x_over_y_below_x() {
        let st = replay_two_run_log(true);
        assert_eq!((st.step, st.max_steps), (41541, 63906));
        // Only the second run's one sample survives the reset.
        assert_eq!(st.loss_history.len(), 1);
    }

    /// A harness that does not print the resume line leaves the header's
    /// relative budget (25560) in place beside absolute steps, so the guard
    /// is the only protection here.
    #[test]
    fn replaying_without_the_resume_line_still_never_draws_x_over_y_below_x() {
        let st = replay_two_run_log(false);
        assert_eq!((st.step, st.max_steps), (41541, 25560));
        assert_eq!(st.loss_history.len(), 1);
        assert!(!header_row_of(&st).contains(" / "));
    }

    /// Between the resume line and the first val line the header already
    /// places the run at its start step.
    #[test]
    fn a_resumed_run_shows_its_start_step_before_the_first_val_line() {
        use crate::workload::train::TrainEvent;
        let mut st = attached_state();
        st.apply_event(TrainEvent::HarnessSummary {
            max_steps: 25560,
            batch_size: 64,
            seq_len: 512,
        });
        st.apply_event(TrainEvent::Resumed {
            start_step: 38346,
            end_step: 63906,
        });
        let row = header_row_of(&st);
        assert!(row.contains("step 38,346 / 63,906  60.0%"), "{row:?}");
    }

    // ---- starfield and signal weave ------------------------------------

    /// A mock backend with summed PCIe counters set by the test. `rx` of
    /// `None` reports no counters at all, as the mock and tt-smi backends do.
    struct PcieDouble {
        inner: MockBackend,
        rx: Option<f64>,
    }

    impl TelemetryBackend for PcieDouble {
        fn init(&mut self) -> crate::error::BackendResult<()> {
            self.inner.init()
        }
        fn update(&mut self) -> crate::error::BackendResult<()> {
            self.inner.update()
        }
        fn devices(&self) -> &[Device] {
            self.inner.devices()
        }
        fn telemetry(&self, i: usize) -> Option<&crate::models::Telemetry> {
            self.inner.telemetry(i)
        }
        fn smbus_telemetry(&self, i: usize) -> Option<&crate::models::SmbusTelemetry> {
            self.inner.smbus_telemetry(i)
        }
        fn backend_info(&self) -> String {
            "pcie-double".into()
        }
        fn pcie_bandwidth(&self, i: usize) -> Option<crate::backend::pcie_counters::PcieBandwidth> {
            // Only chip 0 reports, so the summed total is exactly `rx`.
            self.rx
                .filter(|_| i == 0)
                .map(|rx| crate::backend::pcie_counters::PcieBandwidth {
                    rx_bytes_per_sec: rx,
                    tx_bytes_per_sec: 0.0,
                })
        }
    }

    fn pcie_double(chips: usize, rx: Option<f64>) -> PcieDouble {
        let mut inner = MockBackend::new(chips);
        inner.init().unwrap();
        PcieDouble { inner, rx }
    }

    /// Every rendered cell as `(glyph, colour)`, row by row.
    fn cells_of(lines: &[Line<'static>]) -> Vec<Vec<(char, Color)>> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .flat_map(|s| {
                        let fg = s.style.fg.unwrap_or(Color::Reset);
                        s.content.chars().map(move |c| (c, fg))
                    })
                    .collect()
            })
            .collect()
    }

    /// A glyph the starfield draws for a star: a braille dot pattern (never
    /// the blank braille cell) or a compile or checkpoint marker.
    fn is_star(c: char) -> bool {
        ('\u{2801}'..='\u{28FF}').contains(&c) || c == '◆' || c == '✺'
    }

    /// Where the band puts its data columns, from the rule in the brief and
    /// independent of the renderer: `(x0, band width, first data column,
    /// data width, value column)`. The value column (10 wide, after a
    /// one-column gap) is drawn only when at least 20 data columns remain
    /// after the 6-column labels; otherwise the data takes its place.
    fn band_geom(v: &TrainView) -> (usize, usize, usize, usize, Option<usize>) {
        let (x0, w) = v.network_bounds();
        let avail = w.saturating_sub(LABEL_W + 1);
        let x_data = x0 + LABEL_W;
        if avail >= 20 + 11 {
            let data_w = avail - 11;
            (x0, w, x_data, data_w, Some(x_data + data_w + 1))
        } else {
            (x0, w, x_data, avail, None)
        }
    }

    /// The text from column `from` to `to` (exclusive) of a row.
    fn span_text(row: &[char], from: usize, to: usize) -> String {
        row.get(from..to.min(row.len()))
            .map(|s| s.iter().collect())
            .unwrap_or_default()
    }

    /// Row indices of the band's header and of each row whose label column
    /// starts with `label`.
    fn band_rows(rows: &[Vec<char>], x0: usize, label: &str) -> Vec<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, r)| span_text(r, x0, x0 + label.chars().count()) == label)
            .map(|(i, _)| i)
            .collect()
    }

    /// The starfield as drawn at 134x40 from a hand-built history: one star
    /// per step, right-aligned, high steps nearer the top, markers for a
    /// compile and a checkpoint, a dotted median horizon only where no star
    /// is drawn on its row, and the axis range printed at the left.
    #[test]
    fn the_starfield_draws_each_step_as_a_star_on_a_right_aligned_canvas() {
        use crate::animation::train_canvas::{median_cell_row, place_stars, y_range};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = live_state();
        let n = 40usize;
        st.step_history = (0..n)
            .map(|i| {
                let mut s = sample_at(i as u64 + 1, 100.0, 0);
                match i {
                    30 => s.ms = 300.0,
                    10 => s.ms = 50.0,
                    20 => s.cache_delta = 3,
                    25 => s.checkpoint = true,
                    _ => {}
                }
                s
            })
            .collect();
        st.step = n as u64;
        st.step_seq = n as u64;
        st.step_ms = 100.0;
        let v = TrainView::new(134, 40);
        let rows: Vec<Vec<char>> = rows_of(&v.render(&st, &b))
            .iter()
            .map(|r| r.chars().collect())
            .collect();
        let (x0, _, x_data, data_w, _) = band_geom(&v);
        let header = band_rows(&rows, x0, "STEP ANATOMY")[0];
        let chip = band_rows(&rows, x0, "chip0")[0];
        let star_rows: Vec<usize> = (header + 1..chip).collect();
        assert!(star_rows.len() >= 2, "{star_rows:?}");
        let nrows = star_rows.len();
        // Column of step `i`: two steps per column, newest in the last one.
        let col = |i: usize| x_data + (2 * data_w - n + i) / 2;
        let has_star = |r: usize, x: usize| rows[r].get(x).copied().is_some_and(is_star);
        // The newest step is in the last data column, and nothing is drawn
        // as a star to its right.
        assert_eq!(col(n - 1), x_data + data_w - 1);
        assert!(star_rows.iter().any(|&r| has_star(r, col(n - 1))));
        for &r in &star_rows {
            for x in x_data + data_w..rows[r].len() {
                assert!(!is_star(rows[r][x]), "row {r} col {x}: {:?}", rows[r]);
            }
        }
        // The slowest step is on the top star row, the fastest on the bottom.
        assert!(has_star(star_rows[0], col(30)), "{:?}", rows[star_rows[0]]);
        assert!(has_star(star_rows[nrows - 1], col(10)));
        // Markers in their cells.
        assert!(star_rows.iter().any(|&r| rows[r][col(20)] == '◆'));
        assert!(star_rows.iter().any(|&r| rows[r][col(25)] == '✺'));
        // Cell for cell against the pure helpers: a star where one landed,
        // the horizon on the median's row where none did, blank elsewhere.
        let ms: Vec<f32> = st.step_history.iter().map(|s| s.ms).collect();
        let (lo, hi) = y_range(&ms).unwrap();
        let grid = place_stars(&st.step_history, data_w, nrows, lo, hi, 3);
        let med_row = median_cell_row(100.0, lo, hi, nrows).unwrap();
        let mut horizon = 0;
        for (gr, &r) in star_rows.iter().enumerate() {
            for (c, cell) in grid[gr].iter().enumerate() {
                let got = rows[r][x_data + c];
                if cell.is_empty() {
                    let want = if gr == med_row { '┈' } else { ' ' };
                    assert_eq!(got, want, "row {gr} col {c}: {:?}", rows[r]);
                    horizon += usize::from(got == '┈');
                } else {
                    assert!(is_star(got), "row {gr} col {c}: {got:?}");
                }
            }
        }
        assert!(horizon > 0, "the horizon shows through somewhere");
        // The axis range, top and bottom, in the label column.
        let label = |r: usize| span_text(&rows[r], x0, x0 + 5);
        assert_eq!(label(star_rows[0]), format!("{hi:>5.0}"));
        assert_eq!(label(star_rows[nrows - 1]), format!("{lo:>5.0}"));
    }

    /// Steps whose times are all 0 have no place on the canvas. No star row
    /// is granted, so the chip row follows the header directly with no blank
    /// rows in between, and nothing panics. A history that mixes such steps
    /// with placeable ones still draws its starfield.
    #[test]
    fn steps_with_no_placeable_time_get_no_star_rows() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let render = |times: &dyn Fn(u64) -> f32| -> (Vec<Vec<char>>, TrainView) {
            let mut st = live_state();
            st.step_history = (1..=12u64).map(|i| sample_at(i, times(i), 0)).collect();
            st.step = 12;
            st.step_seq = 12;
            st.step_ms = 100.0;
            let v = TrainView::new(134, 40);
            let rows = rows_of(&v.render(&st, &b))
                .iter()
                .map(|r| r.chars().collect())
                .collect();
            (rows, v)
        };
        for zero in [0.0f32, -3.0, f32::NAN] {
            let (rows, v) = render(&|_| zero);
            let (x0, bw, ..) = band_geom(&v);
            let header = band_rows(&rows, x0, "STEP ANATOMY")[0];
            let chip = band_rows(&rows, x0, "chip0");
            assert_eq!(
                chip,
                vec![header + 1],
                "times {zero}: {:?}",
                rows[header + 1]
            );
            let Layout {
                network_top,
                network_h,
                ..
            } = v.layout();
            for row in &rows[network_top..network_top + network_h] {
                let band = span_text(row, x0, x0 + bw);
                assert!(!band.chars().any(is_star), "times {zero}: {band:?}");
            }
        }
        // Every other step placeable: star rows are granted and each holds
        // a star or the median horizon.
        let (rows, v) = render(&|i| if i % 2 == 0 { 0.0 } else { 100.0 + i as f32 });
        let (x0, ..) = band_geom(&v);
        let header = band_rows(&rows, x0, "STEP ANATOMY")[0];
        let chip = band_rows(&rows, x0, "chip0")[0];
        let star_rows = chip - header - 1;
        assert!(star_rows >= 2, "star rows {star_rows}");
        for r in header + 1..chip {
            assert!(
                rows[r].iter().any(|&c| is_star(c) || c == '┈'),
                "a granted star row is blank: {:?}",
                rows[r]
            );
        }
    }

    /// The expected value-column text for each weave row of `b` and `st`.
    fn weave_values(b: &dyn TelemetryBackend, st: &TrainState) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for d in b.devices().iter().take(MAX_LANES) {
            let t = b.telemetry(d.index).unwrap();
            let tdp = TrainView::chip_tdp(b, d).unwrap();
            out.push((
                format!("chip{}", d.index),
                format!("{:.0}% TDP", t.power_w() / tdp * 100.0),
            ));
        }
        let busiest = TrainView::new(134, 40).busiest_chip(b).unwrap();
        out.push(("aiclk".into(), format!("{} MHz", busiest.aiclk_mhz)));
        if let Some(bps) = TrainView::pcie_total(b) {
            out.push(("pcie".into(), format!("{:.0} MB/s", bps / 1e6)));
        }
        if let Some(cpu) = st.host_cpu_pct {
            out.push(("host".into(), format!("cpu {cpu:.0}%")));
        }
        out
    }

    fn weave_state(host: Option<f32>) -> TrainState {
        let mut st = live_state();
        st.step_history = (1..=10u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step = 10;
        st.step_seq = 10;
        st.step_ms = 100.0;
        st.host_cpu_pct = host;
        st
    }

    /// One row per signal, in order: chip power rows, the busiest chip's
    /// aiclk, PCIe and host CPU, each ending in its current value. PCIe and
    /// host rows appear only with their signal.
    #[test]
    fn the_weave_draws_a_row_per_signal_with_its_current_value() {
        let cases = [
            (None, None, vec!["chip0", "chip1", "aiclk"]),
            (Some(1.5e9), None, vec!["chip0", "chip1", "aiclk", "pcie"]),
            (None, Some(42.0), vec!["chip0", "chip1", "aiclk", "host"]),
            (
                Some(3.1e8),
                Some(140.0),
                vec!["chip0", "chip1", "aiclk", "pcie", "host"],
            ),
        ];
        for (rx, host, want_labels) in cases {
            let b = pcie_double(2, rx);
            let st = weave_state(host);
            let v = TrainView::new(134, 40);
            let rows: Vec<Vec<char>> = rows_of(&v.render(&st, &b))
                .iter()
                .map(|r| r.chars().collect())
                .collect();
            let (x0, w, _, _, value_x) = band_geom(&v);
            let value_x = value_x.expect("134 columns leave room for values");
            let values = weave_values(&b, &st);
            let labels: Vec<&str> = values.iter().map(|(l, _)| l.as_str()).collect();
            assert_eq!(labels, want_labels, "rx={rx:?} host={host:?}");
            // A row whose signal is missing is left out.
            for absent in ["chip2", "aiclk", "pcie", "host"] {
                if !want_labels.contains(&absent) {
                    assert!(
                        band_rows(&rows, x0, absent).is_empty(),
                        "rx={rx:?} host={host:?}: a {absent} row with no signal"
                    );
                }
            }
            let mut last = 0;
            for (label, value) in &values {
                let found = band_rows(&rows, x0, label);
                assert_eq!(found.len(), 1, "{label}: {found:?}");
                let r = found[0];
                assert!(r > last, "{label} is out of order");
                last = r;
                let shown = span_text(&rows[r], value_x, x0 + w);
                assert_eq!(shown.trim_end(), value, "{label}: {:?}", rows[r]);
            }
        }
    }

    /// At every band width the value column is either drawn with whole
    /// values or left out entirely, and when it is left out the strips take
    /// its columns: the newest sample is always in the last data column.
    #[test]
    fn weave_values_are_whole_or_absent_and_the_data_widens_without_them() {
        let b = pcie_double(2, Some(3.1e8));
        let st = weave_state(Some(140.0));
        let values = weave_values(&b, &st);
        let (mut with, mut without) = (0, 0);
        for w in 20..=160usize {
            let v = TrainView::new(w, 40);
            let rows: Vec<Vec<char>> = rows_of(&v.render(&st, &b))
                .iter()
                .map(|r| r.chars().collect())
                .collect();
            let (x0, bw, x_data, data_w, value_x) = band_geom(&v);
            for (label, value) in &values {
                let Some(&r) = band_rows(&rows, x0, label).first() else {
                    continue;
                };
                let row = &rows[r];
                // The newest column always holds a reading.
                let newest = row.get(x_data + data_w - 1).copied().unwrap_or(' ');
                assert!(
                    newest != ' ' && newest != NO_SAMPLE,
                    "w={w} {label}: newest column {newest:?} in {row:?}"
                );
                let tail = span_text(row, x_data + data_w, x0 + bw);
                match value_x {
                    Some(vx) => {
                        with += 1;
                        assert_eq!(
                            span_text(row, vx, x0 + bw).trim_end(),
                            value,
                            "w={w} {label}: {row:?}"
                        );
                    }
                    None => {
                        without += 1;
                        assert!(tail.trim().is_empty(), "w={w} {label}: {tail:?}");
                    }
                }
            }
        }
        assert!(with > 100 && without > 10, "{with} {without}");
    }

    /// The pulse row, the node grid and the gauges are gone from the band.
    #[test]
    fn the_band_draws_no_pulse_row_no_node_grid_and_no_gauges() {
        let b = pcie_double(2, Some(3.1e8));
        let mut st = weave_state(Some(140.0));
        st.step_ms = 83.0; // the old "(max 5/s)" case
        st.config.max_sequence_length = Some(256);
        st.batch_size = 8;
        assert!(st.tokens_per_sec().is_some());
        for (w, h) in [(134usize, 40usize), (160, 40), (100, 30), (134, 20)] {
            let v = TrainView::new(w, h);
            let rows: Vec<Vec<char>> = rows_of(&v.render(&st, &b))
                .iter()
                .map(|r| r.chars().collect())
                .collect();
            let (x0, bw, ..) = band_geom(&v);
            let Layout {
                network_top,
                network_h,
                ..
            } = v.layout();
            for row in &rows[network_top..network_top + network_h] {
                let band = span_text(row, x0, x0 + bw);
                for gone in ["pulse", "tok/s", "of best", "power "] {
                    assert!(!band.contains(gone), "w={w} h={h}: {gone:?} in {band:?}");
                }
                for g in ['░', '●', '◉', '○', '◇'] {
                    assert!(!band.contains(g), "w={w} h={h}: {g:?} in {band:?}");
                }
            }
        }
    }

    /// A trainer that prints a loss but no per-step time: the header says
    /// so, there are no star rows and no chip rows, and the aux rows still
    /// show their current values.
    #[test]
    fn with_no_step_samples_the_band_keeps_its_aux_rows_only() {
        let mut b3 = MockBackend::new(3);
        b3.init().unwrap();
        let mut st = live_state();
        st.step_ms = 400.0; // derived from log cadence; no per-step history
        st.host_cpu_pct = Some(120.0);
        for h in [40usize, 24, 20] {
            let v = TrainView::new(134, h);
            let rows: Vec<Vec<char>> = rows_of(&v.render(&st, &b3))
                .iter()
                .map(|r| r.chars().collect())
                .collect();
            let (x0, bw, ..) = band_geom(&v);
            let out = rows
                .iter()
                .map(|r| r.iter().collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(out.contains("no per-step times reported"), "h={h}\n{out}");
            let Layout {
                network_top,
                network_h,
                ..
            } = v.layout();
            for row in &rows[network_top..network_top + network_h] {
                let band = span_text(row, x0, x0 + bw);
                assert!(!band.chars().any(is_star), "h={h}: {band:?}");
            }
            // The verdict may name the busiest chip; a lane row starts with
            // its `chipN` label.
            assert!(
                band_rows(&rows, x0, "chip").is_empty(),
                "h={h}: no samples, no lanes\n{out}"
            );
            let aiclk = band_rows(&rows, x0, "aiclk");
            assert_eq!(aiclk.len(), 1, "h={h}\n{out}");
            assert!(
                span_text(&rows[aiclk[0]], x0, x0 + bw).contains("MHz"),
                "h={h}\n{out}"
            );
        }
    }

    /// The fg colour of the newest star and of the oldest drawn star.
    fn newest_and_oldest_star_colours(v: &TrainView, st: &TrainState) -> (Color, Color) {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let cells = cells_of(&v.render(st, &b));
        let (x0, _, x_data, data_w, _) = band_geom(v);
        let Layout {
            network_top,
            network_h,
            ..
        } = v.layout();
        let in_col = |x: usize| {
            (network_top..network_top + network_h)
                .find_map(|r| cells[r].get(x).filter(|(c, _)| is_star(*c)).map(|p| p.1))
        };
        let _ = x0;
        let newest = in_col(x_data + data_w - 1).expect("a newest star");
        let oldest = (x_data..x_data + data_w)
            .find_map(in_col)
            .expect("an oldest star");
        (newest, oldest)
    }

    /// The newest stars swell with the step rate: their colour follows the
    /// phase while the older stars keep the plain teal.
    #[test]
    fn the_newest_stars_swell_with_the_step_phase() {
        let mut st = live_state();
        st.step_history = (1..=20u64).map(|i| sample_at(i, 1000.0, 0)).collect();
        st.step = 20;
        st.step_seq = 20;
        st.step_ms = 1000.0;
        let mut v = TrainView::new(134, 40);
        let mut seen = std::collections::BTreeSet::new();
        for f in [0u64, 15, 30, 45] {
            v.frame = f;
            let (newest, oldest) = newest_and_oldest_star_colours(&v, &st);
            assert_eq!(oldest, BAR_NORMAL, "f={f}");
            seen.insert(format!("{newest:?}"));
        }
        assert!(
            seen.len() >= 3,
            "the swell must change with the phase: {seen:?}"
        );
    }

    /// With no step time there is no rate to swell at: the newest stars are
    /// drawn like the rest.
    #[test]
    fn with_no_step_time_nothing_swells() {
        let mut st = live_state();
        st.step_history = (1..=20u64).map(|i| sample_at(i, 1000.0, 0)).collect();
        st.step = 20;
        st.step_seq = 20;
        st.step_ms = 0.0;
        let mut v = TrainView::new(134, 40);
        for f in [0u64, 15, 30, 45] {
            v.frame = f;
            let (newest, oldest) = newest_and_oldest_star_colours(&v, &st);
            assert_eq!((newest, oldest), (BAR_NORMAL, BAR_NORMAL), "f={f}");
        }
    }

    /// A change in step time changes the swell's speed and never its place in
    /// the cycle, so the newest star's colour moves by small steps between
    /// frames. A phase computed from absolute time jumps here.
    #[test]
    fn the_swell_does_not_jump_when_the_step_time_changes() {
        let mut st = live_state();
        st.step_history = (1..=20u64).map(|i| sample_at(i, 1000.0, 0)).collect();
        st.step = 20;
        st.step_seq = 20;
        let mut v = TrainView::new(134, 40);
        let rgb = |c: Color| match c {
            Color::Rgb(r, g, b) => [r as i32, g as i32, b as i32],
            other => panic!("expected an RGB colour, got {other:?}"),
        };
        let mut prev: Option<[i32; 3]> = None;
        let mut worst = 0;
        for i in 0..120u64 {
            v.frame = 36_000 + i;
            st.step_ms = [1000.0, 1010.0, 980.0, 1500.0][(i % 4) as usize];
            let cur = rgb(newest_and_oldest_star_colours(&v, &st).0);
            if let Some(p) = prev {
                let d = (0..3).map(|k| (cur[k] - p[k]).abs()).max().unwrap();
                worst = worst.max(d);
            }
            prev = Some(cur);
        }
        assert!(worst <= 12, "the swell jumped by {worst} in one frame");
    }

    /// The bests a weave row is scaled to belong to one run. Run A saw PCIe
    /// at 3 GB/s; run B, through the same view, at 1 GB/s. B's newest PCIe
    /// cell is full height only if A's best was cleared.
    #[test]
    fn a_new_run_does_not_inherit_the_previous_runs_pcie_best() {
        let t0 = std::time::Instant::now();
        let v = TrainView::new(134, 40);
        let b_a = pcie_double(1, Some(3.0e9));
        let a = run_state(100, t0, 100.0);
        v.render(&a, &b_a);
        let b_b = pcie_double(1, Some(1.0e9));
        let mut run_b = run_state(200, t0 + std::time::Duration::from_secs(1), 100.0);
        v.render(&run_b, &b_b);
        run_b.step_history = vec![sample_at(1, 100.0, 0)];
        run_b.step_seq = 1;
        run_b.step = 1;
        let rows: Vec<Vec<char>> = rows_of(&v.render(&run_b, &b_b))
            .iter()
            .map(|r| r.chars().collect())
            .collect();
        let (x0, _, x_data, data_w, _) = band_geom(&v);
        let r = band_rows(&rows, x0, "pcie")[0];
        assert_eq!(rows[r][x_data + data_w - 1], '█', "{:?}", rows[r]);
    }

    /// A backend double whose chips report readings set per frame:
    /// `frames[k][chip]` is chip `chip`'s telemetry while `k` is the current
    /// frame. No chip has a TDP (no device limits and no SMBUS).
    struct Scripted {
        devices: Vec<Device>,
        frames: Vec<Vec<crate::models::Telemetry>>,
        k: std::sync::atomic::AtomicUsize,
    }

    impl TelemetryBackend for Scripted {
        fn init(&mut self) -> crate::error::BackendResult<()> {
            Ok(())
        }
        fn update(&mut self) -> crate::error::BackendResult<()> {
            Ok(())
        }
        fn devices(&self) -> &[Device] {
            &self.devices
        }
        fn telemetry(&self, i: usize) -> Option<&crate::models::Telemetry> {
            self.frames[self.k.load(std::sync::atomic::Ordering::Relaxed)].get(i)
        }
        fn smbus_telemetry(&self, _i: usize) -> Option<&crate::models::SmbusTelemetry> {
            None
        }
        fn backend_info(&self) -> String {
            "scripted".into()
        }
    }

    /// `chips` chips with no TDP; frame `k` gives chip `c` the power and
    /// aiclk `reading(k, c)` returns.
    fn scripted(
        chips: usize,
        frames: usize,
        reading: impl Fn(usize, usize) -> (f32, u32),
    ) -> Scripted {
        let mut inner = MockBackend::new(chips);
        inner.init().unwrap();
        let mut devices = inner.devices().to_vec();
        for d in &mut devices {
            d.limits = None;
        }
        let frames = (0..frames)
            .map(|k| {
                (0..chips)
                    .map(|c| {
                        let mut t = inner.telemetry(c).unwrap().clone();
                        let (p, a) = reading(k, c);
                        t.power = Some(p);
                        t.aiclk = Some(a);
                        t
                    })
                    .collect()
            })
            .collect();
        Scripted {
            devices,
            frames,
            k: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Render `b` once per step of an 8-step history, frame `k` for step
    /// `k + 1`, so the view records each frame's readings against its step.
    /// Returns the last frame's rows and colours.
    fn render_steps(b: &Scripted, v: &TrainView) -> Vec<Vec<(char, Color)>> {
        let mut st = live_state();
        st.step_history = (1..=8u64).map(|i| sample_at(i, 100.0, 0)).collect();
        st.step_ms = 100.0;
        let mut last = Vec::new();
        for k in 0..8usize {
            b.k.store(k, std::sync::atomic::Ordering::Relaxed);
            st.step_seq = k as u64 + 1;
            st.step = k as u64 + 1;
            last = cells_of(&v.render(&st, b));
        }
        last
    }

    /// A chip with no TDP is scaled to the highest power in the window and
    /// its value is shown in watts. Steps draw 20, 40, 60 and 80 W two at a
    /// time, so the four columns are a quarter, a half, three quarters and
    /// all of the 80 W maximum.
    #[test]
    fn a_chip_with_no_tdp_is_scaled_to_the_window_maximum_and_shown_in_watts() {
        let b = scripted(1, 8, |k, _| (20.0 * (k / 2 + 1) as f32, 1000));
        assert_eq!(TrainView::chip_tdp(&b, &b.devices[0]), None);
        let v = TrainView::new(134, 40);
        let cells = render_steps(&b, &v);
        let rows: Vec<Vec<char>> = cells
            .iter()
            .map(|r| r.iter().map(|p| p.0).collect())
            .collect();
        let (x0, w, x_data, data_w, value_x) = band_geom(&v);
        let r = band_rows(&rows, x0, "chip0")[0];
        assert_eq!(
            span_text(&rows[r], x_data + data_w - 4, x_data + data_w),
            "▂▄▆█",
            "{:?}",
            rows[r]
        );
        let shown = span_text(&rows[r], value_x.unwrap(), x0 + w);
        assert_eq!(shown.trim_end(), "80 W", "{:?}", rows[r]);
    }

    /// The aiclk row describes the busiest chip. Chip 1 draws the most power
    /// and its aiclk falls from 1000 to 800 MHz on the last two steps; chip 0
    /// holds 1200 MHz. The row's value and its coral drop cell come from
    /// chip 1.
    #[test]
    fn the_aiclk_row_follows_the_busiest_chip() {
        let b = scripted(2, 8, |k, c| match c {
            0 => (30.0, 1200),
            _ => (90.0, if k >= 6 { 800 } else { 1000 }),
        });
        let v = TrainView::new(134, 40);
        assert_eq!(v.busiest_chip(&b).map(|c| c.index), Some(1));
        let cells = render_steps(&b, &v);
        let rows: Vec<Vec<char>> = cells
            .iter()
            .map(|r| r.iter().map(|p| p.0).collect())
            .collect();
        let (x0, w, x_data, data_w, value_x) = band_geom(&v);
        let r = band_rows(&rows, x0, "aiclk")[0];
        let shown = span_text(&rows[r], value_x.unwrap(), x0 + w);
        assert_eq!(shown.trim_end(), "800 MHz", "{:?}", rows[r]);
        // Four columns of two steps each: only the last one dropped.
        let colours: Vec<Color> = (x_data + data_w - 4..x_data + data_w)
            .map(|x| cells[r][x].1)
            .collect();
        assert_eq!(
            colours,
            vec![AICLK_ROW, AICLK_ROW, AICLK_ROW, AICLK_DROP],
            "{:?}",
            rows[r]
        );
    }

    /// At every width from 20 to 134 and every height from 10 to 40, the band
    /// draws exactly the rows `plan_band` granted, in order, inside its
    /// rectangle. Below it the loss river's `LOSS` header row is unchanged
    /// and in its place, and no band glyph appears in the MODEL card or LIVE
    /// panel columns.
    #[test]
    fn the_band_draws_only_the_rows_it_was_granted_inside_its_rectangle() {
        use crate::animation::train_tapestry::{diagnose, Readings};
        let b = pcie_double(3, Some(3.1e8));
        let mut st = live_state();
        // A falling loss gives the convergence strip something to say.
        for i in 2..=40u64 {
            st.apply_event(crate::workload::train::TrainEvent::Step {
                step: i,
                loss: 2.0 - i as f32 * 0.01,
            });
        }
        // The newest step compiled, so the verdict is the fixed compile line.
        st.step_history = (1..=64u64)
            .map(|i| sample_at(i, 100.0 + i as f32, u32::from(i == 64)))
            .collect();
        st.step = 64;
        st.step_seq = 64;
        st.step_ms = 150.0;
        st.host_cpu_pct = Some(140.0);
        let compile = diagnose(&Readings {
            compiled_last_step: true,
            busiest_tdp_frac: None,
            host_cpu_pct: None,
        })
        .unwrap();
        let loss_label = "LOSS  · mountains colored by their own value";
        let mut granted_strip = 0;
        for w in 20..=134usize {
            for h in 10..=40usize {
                let v = TrainView::new(w, h);
                let rows: Vec<Vec<char>> = rows_of(&v.render(&st, &b))
                    .iter()
                    .map(|r| r.chars().collect())
                    .collect();
                let (x0, bw, _, data_w, value_x) = band_geom(&v);
                let Layout {
                    network_top,
                    network_h,
                    river_top,
                    river_bottom,
                    ..
                } = v.layout();
                let ctx = format!("w={w} h={h}");
                // What the band asks for, stated from the inputs: one row
                // per chip and per aux signal while there are data columns
                // (an aux row with no steps needs its value column), the
                // compile verdict when it fits, the strip when a clause fits.
                let steps = data_w > 0;
                let strip_parts = convergence_parts(
                    &st.loss_history,
                    st.config.learning_rate,
                    st.scheduler.as_deref(),
                    st.step,
                    if st.step > st.max_steps {
                        0
                    } else {
                        st.max_steps
                    },
                );
                let want = BandWants {
                    steps,
                    stars: steps,
                    lanes: 3,
                    aux: if steps || value_x.is_some() { 3 } else { 0 },
                    verdict: TrainView::verdict_line(&compile, bw).is_some(),
                    strip: !TrainView::fit_parts(&strip_parts, bw).is_empty(),
                };
                let p = plan_band(network_h, want);
                granted_strip += usize::from(p.strip_row);
                let label_at = |y: usize, label: &str| {
                    span_text(&rows[y], x0, x0 + label.chars().count()) == label
                };
                // Row by row, top to bottom, the layers the plan granted.
                let mut y = network_top + 1 + p.star_rows;
                for lane in 0..p.lane_rows {
                    assert!(label_at(y, &format!("chip{lane}")), "{ctx} {p:?} y={y}");
                    y += 1;
                }
                for aux in ["aiclk", "pcie", "host"].iter().take(p.aux_rows) {
                    assert!(label_at(y, aux), "{ctx} {p:?} y={y} {:?}", rows[y]);
                    y += 1;
                }
                if p.verdict_row {
                    assert!(label_at(y, "▸ compiling"), "{ctx} {p:?} y={y}");
                    y += 1;
                }
                if p.strip_row {
                    assert!(label_at(y, "loss"), "{ctx} {p:?} y={y} {:?}", rows[y]);
                    y += 1;
                }
                // No layer row above the weave: the star rows hold no label.
                for r in network_top + 1..network_top + 1 + p.star_rows {
                    for label in ["chip", "aiclk", "pcie", "host", "▸", "loss"] {
                        assert!(!label_at(r, label), "{ctx} {p:?} star row {r}");
                    }
                }
                // Granted rows end at `y`; the rest of the band is empty.
                for r in y..network_top + network_h {
                    let band = span_text(&rows[r], x0, x0 + bw);
                    assert!(band.trim().is_empty(), "{ctx} {p:?} row {r}: {band:?}");
                }
                // Nothing the band draws lands outside its columns: on the
                // band's rows every other column holds exactly what the frame,
                // the MODEL card and the LIVE panel draw on their own.
                let panels = {
                    let p = TrainView::new(w, h);
                    let mut buf = vec![vec![Cell::default(); p.width]; p.height];
                    p.draw_frame(&mut buf);
                    let (show_model, show_live) = p.panel_fit();
                    if show_model {
                        p.draw_model_card(&mut buf, &st);
                    }
                    if show_live {
                        p.draw_live_stats(&mut buf, &st);
                    }
                    buf
                };
                for r in network_top..network_top + network_h {
                    for (x, &c) in rows[r].iter().enumerate() {
                        if x < x0 || x >= x0 + bw {
                            assert_eq!(c, panels[r][x].ch, "{ctx} row {r} col {x}: {:?}", rows[r]);
                        }
                    }
                }
                // The first row below the band is the river's header, as the
                // river draws it.
                let below = network_top + network_h;
                assert_eq!(river_top - 1, below, "{ctx}");
                if river_bottom > river_top + 1 {
                    assert_eq!(
                        span_text(&rows[below], 2, w).trim_end(),
                        TrainView::clip(loss_label, w.saturating_sub(3)).trim_end(),
                        "{ctx}"
                    );
                }
            }
        }
        assert!(granted_strip > 0, "the sweep never granted a strip");
    }

    /// A value wider than the value column is left out whole; one that fits
    /// is drawn.
    #[test]
    fn a_value_wider_than_the_value_column_is_left_out_whole() {
        let v = TrainView::new(134, 40);
        let g = TrainView::band_geom(10, 80);
        let vx = g.value_x.expect("80 columns leave room for values");
        for (value, drawn) in [("1234567890", true), ("12345678901", false)] {
            let mut buf = vec![vec![Cell::default(); 134]; 3];
            let row = WeaveRow {
                label: "pcie".into(),
                cells: vec![Some(('▄', PCIE_ROW)); g.data_w],
                value: value.into(),
            };
            v.draw_weave_row(&mut buf, 10, 1, g, &row);
            let text: String = buf[1][vx..vx + VALUE_W + 2].iter().map(|c| c.ch).collect();
            if drawn {
                assert_eq!(text.trim_end(), value);
            } else {
                assert!(text.trim().is_empty(), "{value}: {text:?}");
            }
            // The cells are drawn either way.
            assert_eq!(buf[1][g.x_data].ch, '▄');
        }
    }
}
