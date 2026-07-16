// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Rendering for the HivemindSweeper TUI mode (`~` to activate): a
//! source × column heat board plus a filtered live event feed underneath.
//!
//! `board_rows` is a pure helper (no terminal needed) so the board layout is
//! independently testable; `render_hivemind` does the actual Ratatui drawing.
//! Follows the project's left/bottom-border-only convention (see the module
//! doc in `crate::ui::tui`) — never a right-side box glyph, and the panel is
//! sized to the terminal `area` it is given rather than a hardcoded width.

use std::time::Instant;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::ui::colors;
use crate::workload::hivemind::{
    collector::CollectorStatus,
    event::{Severity, Source},
    grid::{self, heat_char, Column},
    Hivemind,
};

/// Map a recent per-second activity count (a `grid::spark`/feed-row bucket
/// value) to a block glyph. Uses the same intensity ramp as `heat_char` but
/// over a much smaller range (a handful of events/sec is already "a lot" for
/// a sparkline bucket, vs. heat's decaying 0..=8 scale), and treats zero as
/// the coldest glyph in the ramp (a quiet second, not "no data") so a cell's
/// sparkline reads as a continuous waveform rather than blanks.
pub fn spark_char(count: u8) -> char {
    crate::animation::common::value_to_block_char((count as f32 / 4.0).clamp(0.0, 1.0))
}

/// One board cell: which `Column` it is, its decaying heat glyph, its recent
/// per-second activity sparkline, and whether it was active recently enough
/// to "pulse" (render brighter/bold).
#[derive(Clone, Copy)]
pub struct BoardCell {
    pub col: Column,
    pub glyph: char,
    pub spark: [u8; grid::SPARK_W],
    pub pulsing: bool,
}

/// One row of the source × column heat board: the emitting `Source`, and one
/// cell per active column (`Device(0)..=max_device`, then `Host`).
pub struct BoardRow {
    pub source: Source,
    pub cells: Vec<BoardCell>,
}

/// Build the board's rows straight from the engine's heat grid — pure and
/// terminal-free so it's unit-testable on its own (see `tests` below).
///
/// Rows are `hive.grid().active_sources()`, in first-seen order. Columns are
/// every `Column::Device(0..=max_device)` (omitted entirely if no device has
/// ever been seen) followed by `Column::Host` — every row gets the same
/// column set, so the board reads as a proper table even for sources that
/// haven't touched every column yet (those cells are simply cold/zero heat).
///
/// `now` drives the pulse check (`grid::HeatGrid::is_pulsing`) — passed in
/// explicitly (rather than read internally via `Instant::now()`) so this
/// stays a pure function of its inputs and is fully deterministic in tests.
pub fn board_rows(hive: &Hivemind, now: Instant) -> Vec<BoardRow> {
    let grid = hive.grid();
    let mut columns: Vec<Column> = Vec::new();
    if let Some(max_device) = grid.max_device() {
        for d in 0..=max_device {
            columns.push(Column::Device(d));
        }
    }
    columns.push(Column::Host);

    grid.active_sources()
        .into_iter()
        .map(|source| {
            let cells = columns
                .iter()
                .map(|&col| BoardCell {
                    col,
                    glyph: heat_char(grid.heat(source, col)),
                    spark: grid.spark(source, col),
                    pulsing: grid.is_pulsing(source, col, now),
                })
                .collect();
            BoardRow { source, cells }
        })
        .collect()
}

/// Short column header label: "D0", "D1", … or "Host".
fn col_label(col: Column) -> String {
    match col {
        Column::Device(d) => format!("D{d}"),
        Column::Host => "Host".to_string(),
    }
}

/// Given the board rows and the current cursor `(row, col)` (clamped into
/// range), the `(Source, Column)` pair the cursor is sitting on — `None` if
/// the board is empty (no active sources yet).
fn selected_cell(rows: &[BoardRow], cursor: (usize, usize)) -> Option<(Source, Column)> {
    if rows.is_empty() {
        return None;
    }
    let row = &rows[cursor.0.min(rows.len() - 1)];
    if row.cells.is_empty() {
        return None;
    }
    let cell = row.cells[cursor.1.min(row.cells.len() - 1)];
    Some((row.source, cell.col))
}

/// A board `Column` as the `(Source, Option<device>)` shape
/// `FeedAgg::rows`/`Hivemind::feed` filters on — `Column::Host` has no device
/// index, `Column::Device(d)` carries it.
fn column_to_device(col: Column) -> Option<u8> {
    match col {
        Column::Device(d) => Some(d),
        Column::Host => None,
    }
}

fn severity_label(sev: Severity) -> &'static str {
    match sev {
        Severity::Trace => "trace",
        Severity::Info => "info",
        Severity::Notice => "notice",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

fn severity_style(sev: Severity) -> Style {
    let c = match sev {
        Severity::Trace => colors::rgb(110, 110, 130),
        Severity::Info => colors::rgb(140, 200, 255),
        Severity::Notice => colors::rgb(210, 210, 120),
        Severity::Warn => colors::rgb(255, 180, 80),
        Severity::Error => colors::rgb(255, 100, 100),
    };
    Style::default().fg(c)
}

fn collector_glyph(status: &CollectorStatus) -> (&'static str, Color) {
    match status {
        CollectorStatus::Ok => ("●", colors::rgb(90, 220, 140)),
        CollectorStatus::PermDenied(_) => ("▲", colors::rgb(230, 190, 90)),
        CollectorStatus::Err(_) => ("✗", colors::rgb(255, 100, 100)),
    }
}

/// Take as many leading chars of `s` as fit in `max_w` display columns
/// (unicode-width aware — an emoji or box glyph counts as the columns it
/// actually occupies, not as one `char`). Used for the feed pane so an
/// arbitrarily long log/event line never pushes the row past the panel's
/// right edge — we truncate deliberately and visibly rather than let
/// `Paragraph` clip it silently.
fn truncate_to_width(s: &str, max_w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max_w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
        if w + cw > max_w {
            out.push('…');
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

/// Rolling total events/sec across the whole board (see
/// `FeedAgg::total_rate`) that reads as "fully active" for the KITT scanner
/// bar — beyond this reading the beam is already at its slowest/tightest/
/// brightest, so a louder burst doesn't push it any further.
const KITT_FULL_RATE: f32 = 12.0;

/// Per-frame sweep step at zero activity. Tuned against the ~60 FPS animated-
/// mode redraw cadence (`ui_poll_rate_anim` in `crate::ui::tui::run_tui`) so
/// an idle full left-right sweep (`pos` 0.0→1.0) takes roughly 1.5-2.5s —
/// brisk enough to read as alive, not so fast it's distracting.
const KITT_SPEED_IDLE: f32 = 0.009;

/// At full activity the sweep slows to this fraction of its idle speed (see
/// `KittScanner::speed`) — "more hardware activity = a slower, more
/// deliberate beam."
const KITT_ACTIVE_SPEED_FRACTION: f32 = 0.3;

/// Overall brightness floor/ceiling (0..1) the beam's bright center reaches
/// at zero vs. full activity. A low idle floor keeps it subtle and dark; the
/// active ceiling is fully saturated red.
const KITT_IDLE_FLOOR: f32 = 0.12;
const KITT_ACTIVE_MAX: f32 = 1.0;

/// Gaussian falloff `sigma`, as a fraction of the beam's width, at zero vs.
/// full activity — wide/diffuse when idle, tight/sharp when active, so the
/// bright spot visibly "focuses" as hardware activity rises.
const KITT_SIGMA_WIDE_IDLE: f32 = 0.22;
const KITT_SIGMA_NARROW_ACTIVE: f32 = 0.05;

/// Normalize a rolling total events/sec figure (`FeedAgg::total_rate`) into
/// the 0..1 `activity` reading the KITT scanner's speed/focus/brightness are
/// all driven by. Pure and shared so the per-frame `KittScanner::advance`
/// step (in `crate::ui::tui::run_tui`) and the `beam_cells` render step below
/// always agree on the same activity reading for a given frame.
pub fn kitt_activity_norm(total_rate: f32) -> f32 {
    (total_rate / KITT_FULL_RATE).clamp(0.0, 1.0)
}

/// Persistent animation state for the KITT-style (Knight Rider) red-gradient
/// scanner bar drawn on a dedicated row at the bottom of the HivemindSweeper
/// view (see `render_hivemind` and `beam_cells`). `pos` is the bright
/// center's position across the beam's content width, normalized to
/// 0.0..=1.0; `dir` is the direction of travel (`+1.0` or `-1.0`), flipped
/// whenever `pos` reaches an edge.
///
/// Owned as a loop-local in `run_tui` (next to `hm_cursor`), persisting
/// across frames and mode toggles the same way other per-mode animation
/// state (starfield, arcade, …) does.
#[derive(Clone, Copy, Debug)]
pub struct KittScanner {
    pub pos: f32,
    pub dir: f32,
}

impl KittScanner {
    pub fn new() -> Self {
        Self { pos: 0.0, dir: 1.0 }
    }

    /// Per-frame step size at a given normalized `activity` (0..1) — larger
    /// (faster sweep) at idle, smaller (slower, more deliberate) at full
    /// activity: "more hardware activity = a slower, sharper, brighter beam."
    pub fn speed(activity: f32) -> f32 {
        KITT_SPEED_IDLE * (1.0 - (1.0 - KITT_ACTIVE_SPEED_FRACTION) * activity.clamp(0.0, 1.0))
    }

    /// Advance `pos` by one frame's step at the given `activity`, bouncing
    /// `dir` at the 0.0/1.0 edges. Call once per animated redraw (see the
    /// `DisplayMode::HivemindSweeper` draw branch in `crate::ui::tui`).
    pub fn advance(&mut self, activity: f32) {
        self.pos += self.dir * Self::speed(activity);
        if self.pos >= 1.0 {
            self.pos = 1.0;
            self.dir = -1.0;
        } else if self.pos <= 0.0 {
            self.pos = 0.0;
            self.dir = 1.0;
        }
    }
}

impl Default for KittScanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Render the KITT scanner bar as `width` `(glyph, color)` cells: a bright
/// center at `pos * width` with a red-gradient falloff to near-black at the
/// edges. `activity` (0..1, see `kitt_activity_norm`) drives both the
/// falloff width (`sigma` — wide/diffuse at idle, narrow/sharp when active)
/// and the overall brightness ceiling (a dim, subtle drift at idle; a vivid
/// red beam at full activity). The color formula is dominantly red at every
/// brightness, with faint green/blue only near a fully-bright core (reading
/// as a slightly hot-white-red center rather than pure red at max value).
///
/// Pure — no terminal/`Frame` involved — so it's directly unit-testable (see
/// `tests` below).
pub fn beam_cells(width: usize, pos: f32, activity: f32) -> Vec<(char, Color)> {
    if width == 0 {
        return Vec::new();
    }
    let activity = activity.clamp(0.0, 1.0);
    let center = pos.clamp(0.0, 1.0) * width.saturating_sub(1) as f32;
    let sigma =
        (crate::animation::common::lerp(KITT_SIGMA_WIDE_IDLE, KITT_SIGMA_NARROW_ACTIVE, activity)
            * width as f32)
            .max(0.5); // guard against a degenerate zero-width falloff at tiny widths
    let brightness = crate::animation::common::lerp(KITT_IDLE_FLOOR, KITT_ACTIVE_MAX, activity);

    (0..width)
        .map(|x| {
            let d = (x as f32 - center).abs();
            let intensity = (-(d / sigma).powi(2)).exp();
            let v = (brightness * intensity).clamp(0.0, 1.0);
            let glyph = crate::animation::common::value_to_block_char(v);
            let color = Color::Rgb(
                (40.0 + v * 215.0) as u8,
                (v * v * 40.0) as u8,
                (v * v * 30.0) as u8,
            );
            (glyph, color)
        })
        .collect()
}

/// Draw the HivemindSweeper board + feed into `area`.
///
/// - `cursor` selects a `(row, col)` board cell; when `unified` is false the
///   feed pane is filtered to just that cell's `(source, column)`. When
///   `unified` is true every event (subject to `sev_floor`) is shown instead.
/// - `sev_floor` is an inclusive floor: events strictly below it are hidden
///   from the feed (the board itself is unaffected — heat is severity-blind).
///
/// The feed pane renders `hive.feed().rows(...)` — coalesced count+rate rows
/// (see `workload::hivemind::feed`), newest-first — rather than iterating the
/// raw event ring, so repeated/near-identical events (device-poll open/close
/// churn above all) fold into one line instead of flooding the pane. Each
/// board cell also shows a tiny recent-activity sparkline next to its heat
/// glyph, and pulses brighter/bold immediately after a bump
/// (`grid::HeatGrid::is_pulsing`), so the board visibly animates even when
/// heat itself changes slowly.
///
/// Left/bottom borders only (`╔═…` / `║ …` / `╚═…`) — no right-side glyph —
/// per the project's TUI convention; the panel fills exactly the `area` it's
/// given rather than a fixed width, and the board's label/column widths are
/// sized to their actual content so nothing is silently truncated the way a
/// hardcoded panel width has bitten this project before. The per-cell
/// sparkline reuses column width that was already reserved (every column is
/// padded to fit the "Host" label, 4 columns, even though a glyph alone is
/// only 1) — so adding it never widens the board past what it already drew.
pub fn render_hivemind(
    f: &mut Frame,
    area: Rect,
    hive: &Hivemind,
    cursor: (usize, usize),
    unified: bool,
    sev_floor: Severity,
    kitt: &KittScanner,
) {
    if area.width < 12 || area.height < 6 {
        return; // Nothing sensible to draw in a sliver of a terminal.
    }

    let now = Instant::now();
    let width = area.width as usize;
    let content_w = width.saturating_sub(2); // minus the "║ " left border
    let content_rows = (area.height as usize).saturating_sub(2); // minus top/bottom border rows
                                                                 // Reserve exactly one content row, just above the bottom border, for the
                                                                 // KITT scanner bar (see `beam_cells`) — it never steals space from the
                                                                 // board or feed rendering below. If there's truly no room (shouldn't
                                                                 // happen given the `area.height < 6` guard above, which always leaves
                                                                 // >= 4 content rows), skip the beam gracefully rather than clip or panic.
    let beam_reserved = content_rows > 0;
    let body_rows_avail = content_rows.saturating_sub(if beam_reserved { 1 } else { 0 });

    let rows = board_rows(hive, now);
    let selected = selected_cell(&rows, cursor);

    // Column label width sized to the widest label actually in use ("Host" is
    // 4 cols; "D0".."D9" are 2, "D10"+ are 3) so multi-digit device indices
    // never get clipped.
    let col_w = rows
        .first()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| unicode_width::UnicodeWidthStr::width(col_label(c.col).as_str()))
                .max()
                .unwrap_or(4)
        })
        .unwrap_or(4)
        .max(4);
    // How many trailing spark-bucket columns fit inside `col_w` once the
    // 1-column heat glyph is accounted for, capped at the grid's actual
    // spark width. This is space `col_w` already reserves (every column is
    // padded to the "Host" label's width regardless of its own glyph width),
    // so showing the spark here never widens the board.
    let spark_show = col_w.saturating_sub(1).min(grid::SPARK_W);
    // Source label width sized to the widest label in play (all ASCII labels
    // today, but measured via unicode-width for the same reason as col_w —
    // consistency, and safety if a future Source label isn't ASCII).
    let label_w = rows
        .iter()
        .map(|r| unicode_width::UnicodeWidthStr::width(r.source.label()))
        .max()
        .unwrap_or(6)
        .max(6);

    let dim = colors::rgb(120, 130, 150);
    let head = colors::rgb(200, 220, 255);
    let accent = colors::rgb(120, 220, 200);
    let selected_bg = colors::rgb(40, 70, 70);

    let mut header_body: Vec<Line> = Vec::new();

    // ── Title / counters line ───────────────────────────────────────────
    let running_note = if hive.is_running() { "" } else { " [stopped]" };
    // `rows:` is the coalesced feed's row count (distinct source/device/
    // pattern combinations currently tracked) — a cheap, useful sanity
    // signal that the folding is actually collapsing the raw event count
    // down, without duplicating the whole raw feed on screen.
    let feed_row_count = hive.feed().rows(None, Severity::Trace).len();
    header_body.push(Line::from(vec![
        Span::styled(
            "HIVEMINDSWEEPER",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  events:{}  dropped:{}  rows:{}{}",
                hive.events().len(),
                hive.dropped(),
                feed_row_count,
                running_note
            ),
            Style::default().fg(dim),
        ),
    ]));

    // ── Collector status line ───────────────────────────────────────────
    let mut collector_spans: Vec<Span> = Vec::new();
    for (name, status) in hive.statuses() {
        let (glyph, color) = collector_glyph(&status);
        collector_spans.push(Span::styled(format!("{name} "), Style::default().fg(dim)));
        collector_spans.push(Span::styled(glyph, Style::default().fg(color)));
        collector_spans.push(Span::raw("  "));
    }
    if collector_spans.is_empty() {
        collector_spans.push(Span::styled(
            "(no collectors spawned)",
            Style::default().fg(dim),
        ));
    }
    header_body.push(Line::from(collector_spans));

    // ── Rule ─────────────────────────────────────────────────────────────
    header_body.push(rule_line(content_w, dim));

    // ── Column header + board rows ──────────────────────────────────────
    if rows.is_empty() {
        header_body.push(Line::from(Span::styled(
            "  (no activity observed yet — waiting on collectors)",
            Style::default().fg(dim),
        )));
    } else {
        let mut hdr_spans = vec![Span::raw(format!("{:label_w$}", ""))];
        for cell in &rows[0].cells {
            hdr_spans.push(Span::raw("  "));
            hdr_spans.push(Span::styled(
                format!("{:>col_w$}", col_label(cell.col)),
                Style::default().fg(head).add_modifier(Modifier::BOLD),
            ));
        }
        header_body.push(Line::from(hdr_spans));

        // Brighter pulse color for a cell that was bumped within the last
        // `PULSE_WINDOW` — makes the board visibly animate on every event,
        // not just when the decaying heat glyph itself changes level.
        let pulse_accent = colors::rgb(190, 255, 235);

        for (ri, row) in rows.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!("{:label_w$}", row.source.label()),
                Style::default().fg(head),
            )];
            for (ci, cell) in row.cells.iter().enumerate() {
                spans.push(Span::raw("  "));
                let is_selected = !unified && ri == cursor.0.min(rows.len() - 1) && {
                    let last_ci = row.cells.len().saturating_sub(1);
                    ci == cursor.1.min(last_ci)
                };
                let mut style = Style::default().fg(accent);
                if cell.pulsing {
                    style = style.fg(pulse_accent).add_modifier(Modifier::BOLD);
                }
                if is_selected {
                    style = style.bg(selected_bg).add_modifier(Modifier::BOLD);
                }
                // Cell content is the heat glyph followed by up to
                // `spark_show` of its most recent activity buckets
                // (oldest-of-the-shown first, so time still reads
                // left-to-right) — e.g. "▓░▒█" instead of a lone "▓".
                let spark_tail = &cell.spark[grid::SPARK_W - spark_show..];
                let mut cell_str = String::with_capacity(1 + spark_show);
                cell_str.push(cell.glyph);
                for &v in spark_tail {
                    cell_str.push(spark_char(v));
                }
                spans.push(Span::styled(format!("{:>col_w$}", cell_str), style));
            }
            header_body.push(Line::from(spans));
        }
    }

    // ── Rule ─────────────────────────────────────────────────────────────
    header_body.push(rule_line(content_w, dim));

    // ── Feed header ──────────────────────────────────────────────────────
    let feed_title = if unified {
        format!("FEED — unified · severity ≥ {}", severity_label(sev_floor))
    } else if let Some((src, col)) = selected {
        format!(
            "FEED — {} · {} · severity ≥ {}",
            src.label(),
            col_label(col),
            severity_label(sev_floor)
        )
    } else {
        format!("FEED · severity ≥ {}", severity_label(sev_floor))
    };
    header_body.push(Line::from(Span::styled(
        feed_title,
        Style::default().fg(head).add_modifier(Modifier::BOLD),
    )));

    // ── Feed lines ───────────────────────────────────────────────────────
    // Coalesced rows (see `workload::hivemind::feed`) instead of the raw
    // event ring: repeated/near-identical events — device-poll open/close
    // churn above all — fold into one line with a running count and a
    // recent events/sec rate, rather than flooding the pane with dozens of
    // near-duplicate lines. `rows()` is already newest-first, so the most
    // recently active row renders at the top of the pane.
    let feed_h = body_rows_avail.saturating_sub(header_body.len());
    let cell_filter = if unified {
        None
    } else {
        selected.map(|(src, col)| (src, column_to_device(col)))
    };
    let feed_rows = hive.feed().rows(cell_filter, sev_floor);
    let mut feed_lines: Vec<Line> = Vec::new();
    for row in feed_rows.iter().take(feed_h) {
        let dev_str = match row.device {
            Some(d) => format!("D{d}"),
            None => "host".to_string(),
        };
        // Trailing "x<count>  <rate>/s  <spark>" summary — a coalesced row's
        // whole point is showing how often it's firing, not just its text.
        let count_str = format!("x{}", row.count);
        let rate_str = format!("{:.1}/s", row.rate);
        let spark_str: String = row.spark.iter().map(|&c| spark_char(c)).collect();
        let trailing = format!("  {count_str}  {rate_str}  {spark_str}");
        let trailing_w = unicode_width::UnicodeWidthStr::width(trailing.as_str());

        let prefix_w = 7 /* severity */ + 1 + label_w + 1 + 4 /* dev field */ + 1;
        let text_w = content_w
            .saturating_sub(prefix_w)
            .saturating_sub(trailing_w);
        feed_lines.push(Line::from(vec![
            Span::styled(
                format!("{:<7}", severity_label(row.max_severity)),
                severity_style(row.max_severity),
            ),
            Span::raw(" "),
            Span::styled(
                format!("{:label_w$}", row.source.label()),
                Style::default().fg(dim),
            ),
            Span::raw(" "),
            Span::styled(format!("{:<4}", dev_str), Style::default().fg(dim)),
            Span::raw(" "),
            Span::raw(truncate_to_width(&row.display, text_w)),
            Span::styled(trailing, Style::default().fg(accent)),
        ]));
    }

    let mut body = header_body;
    body.extend(feed_lines);

    // ── Wrap with the left/bottom-only border and render ────────────────
    let border_style = Style::default().fg(colors::rgb(60, 80, 100));
    let mut display_lines: Vec<Line> = Vec::with_capacity(body.len() + 3);
    display_lines.push(Line::from(Span::styled(
        format!("╔{}", "═".repeat(width.saturating_sub(1))),
        border_style,
    )));
    for line in body.into_iter().take(body_rows_avail) {
        let mut spans = vec![Span::styled("║ ", border_style)];
        spans.extend(line.spans);
        display_lines.push(Line::from(spans));
    }
    // ── KITT scanner bar: one dedicated row, just above the bottom border ──
    // Driven by total board activity (`FeedAgg::total_rate`) — idle reads as
    // dim/wide/brisk, busy reads as bright/tight/deliberate.
    if beam_reserved {
        let activity = kitt_activity_norm(hive.feed().total_rate());
        let mut spans = vec![Span::styled("║ ", border_style)];
        for (glyph, color) in beam_cells(content_w, kitt.pos, activity) {
            spans.push(Span::styled(glyph.to_string(), Style::default().fg(color)));
        }
        display_lines.push(Line::from(spans));
    }
    display_lines.push(Line::from(Span::styled(
        format!("╚{}", "═".repeat(width.saturating_sub(1))),
        border_style,
    )));

    f.render_widget(Paragraph::new(display_lines), area);
}

fn rule_line(content_w: usize, dim: Color) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(content_w),
        Style::default().fg(dim),
    ))
}

/// Legend lines for the `l` overlay while in HivemindSweeper — mirrors the
/// `{mode}_legend_lines` free functions in `crate::ui::tui` (e.g.
/// `castle_legend_lines`), just kept local to this module since the board's
/// symbols are entirely defined here.
pub(crate) fn legend_lines(bar: Color, bg: Color, dim: Color) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("rows   ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled(
                "= active sources (ttnn, metal, …)",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("cols   ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled("= per-device columns + Host", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("░▒▓█   ", Style::default().fg(colors::rgb(120, 220, 200))),
            Span::styled(
                "= decaying heat + a tiny recent-activity spark; pulses on new events",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("feed   ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled(
                "= coalesced events (count/rate/spark) for the selected cell",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled(
                "←↑↓→ / hjk ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= move the board cursor", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled(
                "l / ?      ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= this legend / help", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled(
                "f          ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled(
                "= toggle unified feed (all cells vs. selected)",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled(
                "s          ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled(
                "= cycle severity floor (trace→info→notice→warn→error)",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled(
                "/watch /wrap  ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled(
                "= point the sniffer at a path/pid/command",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled(
                "bottom bar ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled(
                "= KITT scanner — dim/wide/brisk idle, bright/tight/slow when busy",
                Style::default().fg(dim)
            ),
        ]),
    ]
}

/// Explain-panel body for HivemindSweeper (`!` overlay) — passed to the
/// shared `explain_lines(bar, bg, dim, lbl, TEXT)` builder in `mod.rs`.
pub(crate) const EXPLAIN_TEXT: &[&str] = &[
    "HivemindSweeper — Activity Sniffer",
    "",
    "Opt-in, read-only sniffer that correlates driver",
    "messages, compile-cache churn, log tails, and /proc",
    "activity into one source × device/host heat board.",
    "",
    "Each cell's glyph (░▒▓█) decays over time — a cold",
    "cell just hasn't seen activity recently. A cell also",
    "shows a tiny recent-activity spark and briefly pulses",
    "brighter right after a new event.",
    "",
    "The feed pane below coalesces repeated/near-identical",
    "events (e.g. device-poll open/close churn) into one",
    "row with a running count and events/sec rate, for",
    "whichever cell is selected (or every row, unified).",
    "",
    "Controls: arrows (or vim h/j/k) move the cursor, f",
    "toggles the unified feed, s raises the severity floor,",
    "l opens this legend.",
    "",
    "/watch <path> or /watch pid <n> points the sniffer at",
    "a log file or a process's device fds + open logs.",
    "/wrap <cmd…> spawns and captures a command — the one",
    "side effect here, so it asks for confirmation first",
    "(repeat the same /wrap to confirm).",
    "",
    "A red KITT-style scanner bar sweeps the bottom row,",
    "grounding the view. It reflects TOTAL board activity:",
    "idle is dim, wide, and brisk; busy activity makes it",
    "slow down, focus into a tighter beam, and brighten.",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::{event::*, grid::Column, Hivemind};

    #[test]
    fn board_lists_active_sources_with_host_and_device_columns() {
        let mut h = Hivemind::new();
        h.bump_grid_for_test(Source::TtMetal, Some(0));
        h.bump_grid_for_test(Source::Ttnn, None);
        let rows = board_rows(&h, Instant::now());
        let sources: Vec<_> = rows.iter().map(|r| r.source).collect();
        assert_eq!(sources, vec![Source::TtMetal, Source::Ttnn]);
        // Column set spans D0..=max_device plus Host.
        assert!(rows[0].cells.iter().any(|c| c.col == Column::Device(0)));
        assert!(rows[0].cells.iter().any(|c| c.col == Column::Host));
    }

    /// A cell bumped just before `board_rows` is built should read as
    /// pulsing — this is the "board barely animates" complaint's fix: every
    /// event should visibly nudge its cell, not just the slow-decaying heat
    /// glyph.
    #[test]
    fn recently_bumped_cell_is_pulsing_in_board_rows() {
        let mut h = Hivemind::new();
        h.bump_grid_for_test(Source::TtMetal, Some(0));
        let rows = board_rows(&h, Instant::now());
        let cell = rows[0]
            .cells
            .iter()
            .find(|c| c.col == Column::Device(0))
            .expect("Device(0) column present");
        assert!(cell.pulsing, "a just-bumped cell should be pulsing");
    }

    // ── KITT scanner bar ─────────────────────────────────────────────────

    /// Extract the red channel from a `beam_cells` color — with the color
    /// formula `R = 40 + v*215` this is a monotonic (linear) proxy for the
    /// cell's underlying brightness `v`, so tests can compare cells without
    /// `beam_cells` needing to expose `v` directly.
    fn red_channel(color: Color) -> u8 {
        match color {
            Color::Rgb(r, _, _) => r,
            other => panic!("expected Color::Rgb, got {other:?}"),
        }
    }

    #[test]
    fn beam_center_is_brightest_cell() {
        let width = 41;
        let cells = beam_cells(width, 0.5, 0.5);
        let center = width / 2; // pos=0.5 * (width-1) == 20.0 exactly for width=41
        let center_r = red_channel(cells[center].1);
        for (i, (_, color)) in cells.iter().enumerate() {
            assert!(
                red_channel(*color) <= center_r,
                "cell {i} brighter than the center cell {center}"
            );
        }
    }

    #[test]
    fn beam_edges_are_dark() {
        let width = 41;
        let cells = beam_cells(width, 0.5, 0.5);
        let center_r = red_channel(cells[width / 2].1);
        assert!(
            red_channel(cells[0].1) < center_r,
            "left edge should be dark"
        );
        assert!(
            red_channel(cells[width - 1].1) < center_r,
            "right edge should be dark"
        );
    }

    /// For the SAME `pos`, full activity should yield a brighter peak AND a
    /// narrower bright region than idle — "more activity = slower, sharper,
    /// brighter beam."
    #[test]
    fn activity_brightens_and_narrows_the_beam() {
        let width = 61;
        let pos = 0.5;
        let idle = beam_cells(width, pos, 0.0);
        let active = beam_cells(width, pos, 1.0);
        let center = width / 2;

        let idle_peak = red_channel(idle[center].1);
        let active_peak = red_channel(active[center].1);
        assert!(
            active_peak > idle_peak,
            "active peak ({active_peak}) should be brighter than idle peak ({idle_peak})"
        );

        // Count cells whose red channel clears a fixed threshold (chosen
        // below both peaks' brightness so each distribution has some cells
        // above it) as a proxy for the bright region's width.
        let threshold = 55u8;
        let count_bright = |cells: &[(char, Color)]| {
            cells
                .iter()
                .filter(|(_, c)| red_channel(*c) > threshold)
                .count()
        };
        let idle_count = count_bright(&idle);
        let active_count = count_bright(&active);
        assert!(
            active_count < idle_count,
            "active bright region ({active_count} cells) should be narrower than idle's ({idle_count} cells)"
        );
    }

    #[test]
    fn scanner_speed_slows_down_with_activity() {
        assert!(
            KittScanner::speed(1.0) < KittScanner::speed(0.0),
            "full activity should sweep slower than idle"
        );
    }

    #[test]
    fn scanner_advance_bounces_at_edges() {
        let mut kitt = KittScanner::new();
        assert_eq!(kitt.pos, 0.0);
        assert_eq!(kitt.dir, 1.0);

        // Drive it up to the right edge, stopping as soon as `pos` reaches
        // it — `advance` clamps internally on the step that crosses 1.0, so
        // once the loop observes `pos >= 1.0` it must be exactly 1.0 with
        // `dir` already flipped, not merely "close" (continuing to advance
        // past that point would just bounce it back down, which a naive
        // "run N steps then assert == 1.0" test would miss).
        for _ in 0..10_000 {
            if kitt.pos >= 1.0 {
                break;
            }
            kitt.advance(0.0);
        }
        assert_eq!(kitt.pos, 1.0);
        assert_eq!(kitt.dir, -1.0);

        // ...and back down to the left edge, same reasoning.
        for _ in 0..10_000 {
            if kitt.pos <= 0.0 {
                break;
            }
            kitt.advance(0.0);
        }
        assert_eq!(kitt.pos, 0.0);
        assert_eq!(kitt.dir, 1.0);
    }

    /// Rendering smoke test (mirrors `grid_mode_narrow_terminal_does_not_panic`
    /// / `render_snake_view_with_band_does_not_panic` in `mod.rs`): draw both
    /// an empty engine and one with injected activity, across a range of
    /// terminal sizes down to tiny, in both unified and per-cell-filtered
    /// modes, and confirm none of it panics.
    #[test]
    fn render_hivemind_does_not_panic_across_sizes_and_states() {
        use ratatui::{backend::TestBackend, Terminal};
        use std::time::Instant;

        let empty = Hivemind::new();

        let mut active = Hivemind::new();
        active.bump_grid_for_test(Source::TtMetal, Some(0));
        active.bump_grid_for_test(Source::Ttnn, None);
        active.push_for_test(SniffEvent {
            ts: Instant::now(),
            source: Source::TtMetal,
            device: Some(0),
            severity: Severity::Warn,
            kind: EventKind::Compile,
            text: "brisc.cpp.o compiled in 1.2s (this line is intentionally very long to exercise feed-text truncation at the panel's right edge)".to_string(),
            origin: "cache_watch".to_string(),
        });

        for (w, h) in [(120u16, 40u16), (60, 20), (20, 8), (12, 6), (5, 4)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
            for hive in [&empty, &active] {
                for unified in [false, true] {
                    terminal
                        .draw(|f| {
                            let area = f.area();
                            render_hivemind(
                                f,
                                area,
                                hive,
                                (0, 0),
                                unified,
                                Severity::Trace,
                                &KittScanner::new(),
                            );
                        })
                        .expect("draw must not panic");
                }
            }
        }
    }
}
