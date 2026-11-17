// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The seven widgets a declarative viz can draw.
//!
//! Each widget is a small function over `(&mut Buffer, Rect, &WidgetBody,
//! &Inst, &VizCtx)`; [`draw`] picks the function from the widget kind. They
//! share the rules below, so a reader of a YAML file can predict any of them.
//!
//! # Which entity does a widget read?
//!
//! * Inside a row with `each: chip`, the row is drawn once per chip and the
//!   widget reads that chip ([`Inst::chip`]). A widget in such a row whose
//!   `kind` is not `chip` has no defined entity (the schema does not say how
//!   `each` combines with another kind), so it draws the visible note
//!   `each: needs chip` instead of guessing.
//! * Outside `each`, `table` and `grid` show every entity of the widget's
//!   kind (default `chip`). The single-value widgets (`gauge`, `text`,
//!   `sparkline`, `heat_strip`) read the first entity of that kind in key
//!   order; use `each: chip` to get one per chip.
//!
//! # Missing and odd values
//!
//! A reading that does not exist draws `--`. A non-finite number counts as
//! missing. Text values are shown with control characters removed, because a
//! bus source can put any text in a value and an ESC would drive the
//! terminal.
//!
//! # Colour
//!
//! Colours come from [`ColorName::color`], which goes through the TUI palette
//! in `crate::ui::colors` and so follows the theme and the colour mode. No
//! RGB value is written here. When a widget has `thresholds`, the colour is
//! that of the threshold with the highest `at` that the value has reached,
//! whatever order the file lists them in. With no threshold reached the
//! widget uses a default: `primary` for a gauge and a sparkline. For `grid`
//! and `heat_strip` the default depends on whether the file gave a scale:
//! with `max` the red/yellow/green heat ramp ([`heat_color`]) shows how
//! close the value is to that `max`; without `max` there is no fixed scale
//! to measure against (the series' own peak would always read as "full"), so
//! they draw in one neutral colour, `primary`, and only thresholds recolour
//! them.
//!
//! # Cost per frame
//!
//! A frame may hold up to 64 widget instances. Entity lists are fetched
//! once per kind per frame through [`EntityCache`] and shared, and only by
//! the widgets that read entities (never `log`). `log` filters events by
//! name before it formats any of them.
//!
//! # Clipping
//!
//! Every write goes through [`put`], which cuts the text to the rectangle it
//! was given. A widget never indexes outside its `Rect`, and a zero-size
//! `Rect` draws nothing.

use super::schema::{ColorName, KindName, WidgetBody, WidgetDef};
use crate::ui::colors;
use crate::viz::VizCtx;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tt_toplike_bus::{EntityKey, EntityKind, Event, Value};
use unicode_width::UnicodeWidthStr;

/// A gauge with no `max` and no history scales to this.
pub const DEFAULT_GAUGE_MAX: f64 = 100.0;

/// Block characters for a sparkline, lowest to highest.
const BLOCKS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// Which entity a widget instance is drawn for.
#[derive(Debug, Clone, Default)]
pub struct Inst {
    /// The chip of an `each: chip` row; `None` outside `each`.
    pub chip: Option<EntityKey>,
}

/// The entity lists of one frame, fetched once per kind and shared.
///
/// `WorldRead::entities` builds a new `Vec` on every call, and a frame can
/// hold 64 widgets, so the renderer makes one of these per frame and every
/// widget that reads entities asks it. The world has no cheaper call (there
/// is no "first entity of kind"), so one list per kind per frame is the
/// floor. Interior mutability keeps `draw`'s signature a shared borrow.
#[derive(Default)]
pub struct EntityCache {
    by_kind: RefCell<HashMap<EntityKind, Rc<Vec<EntityKey>>>>,
}

impl EntityCache {
    /// An empty cache; make one per frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// The entities of `kind` in key order, fetched on first use.
    pub fn get(&self, ctx: &VizCtx, kind: EntityKind) -> Rc<Vec<EntityKey>> {
        self.by_kind
            .borrow_mut()
            .entry(kind)
            .or_insert_with(|| Rc::new(ctx.world.entities(kind)))
            .clone()
    }
}

/// What a widget has to draw for: the entities, or the note that the
/// combination of `each` and `kind` has no meaning.
enum Target {
    Entities(Rc<Vec<EntityKey>>),
    NeedsChip,
}

/// Draw one widget into `area`. The caller has already clipped `area` to
/// the buffer. `cache` is the frame's [`EntityCache`].
pub fn draw(
    buf: &mut Buffer,
    area: Rect,
    w: &WidgetDef,
    inst: &Inst,
    ctx: &VizCtx,
    cache: &EntityCache,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // `log` reads events, not entities: it never resolves a target.
    if let WidgetDef::Log(b) = w {
        return log(buf, area, b, ctx);
    }
    let body = w.body();
    let entities = match target(body, inst, ctx, cache) {
        Target::Entities(e) => e,
        Target::NeedsChip => {
            put(
                buf,
                area,
                0,
                0,
                "each: needs chip",
                Style::default().fg(colors::warning()),
            );
            return;
        }
    };
    match w {
        WidgetDef::Gauge(b) => gauge(buf, area, b, inst, entities.first(), ctx),
        WidgetDef::Sparkline(b) => series(buf, area, b, inst, entities.first(), ctx, false),
        WidgetDef::HeatStrip(b) => series(buf, area, b, inst, entities.first(), ctx, true),
        WidgetDef::Text(b) => text(buf, area, b, inst, entities.first(), ctx),
        WidgetDef::Table(b) => table(buf, area, b, inst, &entities, ctx),
        WidgetDef::Grid(b) => grid(buf, area, b, inst, &entities, ctx),
        WidgetDef::Log(_) => unreachable!("handled above"),
    }
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

/// The entities a widget reads (see the module comment).
fn target(body: &WidgetBody, inst: &Inst, ctx: &VizCtx, cache: &EntityCache) -> Target {
    let kind = body.kind.unwrap_or(KindName::Chip);
    match &inst.chip {
        Some(chip) if kind == KindName::Chip => Target::Entities(Rc::new(vec![chip.clone()])),
        Some(_) => Target::NeedsChip,
        None => Target::Entities(cache.get(ctx, kind.entity_kind())),
    }
}

/// The short name of an entity for display: the key without its `kind:`
/// prefix, and for a PCI address without the `0000:` domain, so
/// `chip:0000:01:00.0` shows as `01:00.0`.
pub fn short_name(key: &EntityKey) -> String {
    let rest = key
        .as_str()
        .split_once(':')
        .map_or(key.as_str(), |(_, r)| r);
    let looks_like_domain = |d: &str| d.len() == 4 && d.chars().all(|c| c.is_ascii_hexdigit());
    match rest.split_once(':') {
        Some((d, tail)) if key.kind() == EntityKind::Chip && looks_like_domain(d) => {
            tail.to_string()
        }
        _ => rest.to_string(),
    }
}

/// The widget's caption with `{chip}` replaced by the row's chip name (or
/// `all` outside an `each` row).
fn label_of(body: &WidgetBody, inst: &Inst) -> Option<String> {
    body.label.as_ref().map(|l| {
        let name = inst.chip.as_ref().map_or("all".to_string(), short_name);
        l.replace("{chip}", &name)
    })
}

/// Write `s` at offset (`dx`, `dy`) inside `area`, cut to the area's right
/// edge. Returns the number of columns used. Nothing is written when the
/// offset is outside the area.
fn put(buf: &mut Buffer, area: Rect, dx: u16, dy: u16, s: &str, style: Style) -> u16 {
    if dx >= area.width || dy >= area.height {
        return 0;
    }
    let room = (area.width - dx) as usize;
    let (x, _) = buf.set_stringn(area.x + dx, area.y + dy, s, room, style);
    x - (area.x + dx)
}

/// The numeric value of a reading, if it has one and it is finite.
fn number(v: &Value) -> Option<f64> {
    let f = match v {
        Value::F64(f) => *f,
        Value::U64(u) => *u as f64,
        Value::I64(i) => *i as f64,
        Value::Text(_) => return None,
    };
    f.is_finite().then_some(f)
}

/// A number for display: no decimals when it is whole, otherwise one.
/// Negative zero, and a small negative that rounds to zero, print without
/// the sign (`0`, `0.0`).
fn fmt_num(f: f64) -> String {
    let f = if f == 0.0 { 0.0 } else { f };
    let s = if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.0}")
    } else {
        format!("{f:.1}")
    };
    match s.as_str() {
        "-0" => "0".to_string(),
        "-0.0" => "0.0".to_string(),
        _ => s,
    }
}

/// A value for display. `None` (missing or non-finite) is `--`.
fn fmt_value(v: Option<&Value>) -> String {
    match v {
        Some(Value::Text(t)) => t.chars().filter(|c| !c.is_control()).collect(),
        Some(v) => number(v).map_or("--".to_string(), fmt_num),
        None => "--".to_string(),
    }
}

/// The latest reading of the widget's signal on `entity`.
fn read(ctx: &VizCtx, entity: Option<&EntityKey>, body: &WidgetBody) -> Option<Value> {
    ctx.world.reading(entity?, &body.signal).map(|r| r.value)
}

/// The colour of the threshold with the highest `at` that `v` has reached.
/// The list need not be sorted.
fn threshold_color(body: &WidgetBody, v: f64) -> Option<Color> {
    body.thresholds
        .iter()
        .filter(|t| v >= t.at)
        .max_by(|a, b| a.at.total_cmp(&b.at))
        .map(|t| t.color.color())
}

/// The default colour of a value by its share of the scale: under half is
/// `success`, under four fifths `warning`, above that `error`. Used by `grid`
/// and `heat_strip` only when the file gave a `max` (see [`neutral_or_heat`]).
fn heat_color(frac: f64) -> Color {
    let name = if frac < 0.5 {
        ColorName::Success
    } else if frac < 0.8 {
        ColorName::Warning
    } else {
        ColorName::Error
    };
    name.color()
}

/// The default colour of a `grid` cell or `heat_strip` sample that reached no
/// threshold: the heat ramp when the file gave a `max`, otherwise the neutral
/// `primary`. Without `max` the scale would be the series' own peak, which
/// would paint a reading near its peak red whatever its size.
fn neutral_or_heat(body: &WidgetBody, frac: f64) -> Color {
    if body.max.is_some() {
        heat_color(frac)
    } else {
        colors::primary()
    }
}

/// The value that fills a widget. Order: the widget's own `max`; else the
/// largest value in the history series of this signal on this entity; else
/// [`DEFAULT_GAUGE_MAX`]. A series whose largest value is not above zero
/// also falls back to the default, since it cannot be a scale.
fn scale_max(body: &WidgetBody, entity: Option<&EntityKey>, ctx: &VizCtx) -> f64 {
    if let Some(m) = body.max {
        return m;
    }
    let hist_max = entity
        .map(|e| ctx.history.series(e, &body.signal))
        .unwrap_or(&[])
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    if hist_max > 0.0 {
        hist_max
    } else {
        DEFAULT_GAUGE_MAX
    }
}

/// The part of `s` that fits in `cols` columns.
fn clip(s: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = UnicodeWidthStr::width(c.to_string().as_str());
        if used + w > cols {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

/// The display width of `s`, capped at `cap` so it always fits a `u16`.
/// (A text value from the bus has no length limit.)
fn text_cols(s: &str, cap: u16) -> u16 {
    UnicodeWidthStr::width(s).min(cap as usize) as u16
}

fn muted() -> Style {
    Style::default().fg(colors::text_secondary())
}

// ---------------------------------------------------------------------
// The widgets
// ---------------------------------------------------------------------

/// `label ████░░░░ 90`. On a row two or more lines high the label gets its
/// own line and the bar sits below it. The filled part is
/// `value / max` of the bar (see [`scale_max`]), in the threshold colour.
fn gauge(
    buf: &mut Buffer,
    area: Rect,
    body: &WidgetBody,
    inst: &Inst,
    entity: Option<&EntityKey>,
    ctx: &VizCtx,
) {
    let reading = read(ctx, entity, body);
    let v = reading.as_ref().and_then(number);
    let value_text = fmt_value(reading.as_ref());
    let label = label_of(body, inst).unwrap_or_default();
    // Label on its own line when there is room; the bar line then starts
    // at column 0.
    let (bar_y, label_cols) = if area.height >= 2 {
        let l = clip(&label, area.width as usize);
        put(
            buf,
            area,
            0,
            0,
            &l,
            Style::default().fg(colors::text_primary()),
        );
        (1, 0)
    } else {
        // Inline: at most a third of the width goes to the label.
        let l = clip(&label, (area.width / 3) as usize);
        let cols = put(
            buf,
            area,
            0,
            0,
            &l,
            Style::default().fg(colors::text_primary()),
        );
        (0, if cols > 0 { cols + 1 } else { 0 })
    };
    let value_cols = text_cols(&value_text, area.width);
    // One space between the bar and the value.
    let bar_w = area
        .width
        .saturating_sub(label_cols.saturating_add(value_cols).saturating_add(1));
    if bar_w >= 1 {
        let frac = v.map_or(0.0, |v| (v / scale_max(body, entity, ctx)).clamp(0.0, 1.0));
        let filled = ((frac * bar_w as f64).round() as u16).min(bar_w);
        let fill_color = v
            .and_then(|v| threshold_color(body, v))
            .unwrap_or_else(colors::primary);
        put(
            buf,
            area,
            label_cols,
            bar_y,
            &"█".repeat(filled as usize),
            Style::default().fg(fill_color),
        );
        put(
            buf,
            area,
            label_cols + filled,
            bar_y,
            &"░".repeat((bar_w - filled) as usize),
            muted(),
        );
    }
    let value_style = Style::default().fg(v
        .and_then(|v| threshold_color(body, v))
        .unwrap_or_else(colors::text_primary));
    let value_x = label_cols + bar_w + u16::from(bar_w > 0);
    put(buf, area, value_x, bar_y, &value_text, value_style);
}

/// `sparkline` and `heat_strip`: the history of the signal on one line,
/// followed by the latest value. A sparkline draws block heights in one
/// colour; a heat strip draws full blocks, each coloured by its own value.
/// With no history the strip is empty and only the latest reading shows;
/// with neither, `--`.
fn series(
    buf: &mut Buffer,
    area: Rect,
    body: &WidgetBody,
    inst: &Inst,
    entity: Option<&EntityKey>,
    ctx: &VizCtx,
    heat: bool,
) {
    let hist: Vec<f64> = entity
        .map(|e| ctx.history.series(e, &body.signal))
        .unwrap_or(&[])
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    let reading = read(ctx, entity, body);
    let latest = reading.as_ref().and_then(number).or(hist.last().copied());
    let value_text = match (&reading, latest) {
        (Some(Value::Text(_)), _) => fmt_value(reading.as_ref()),
        (_, Some(v)) => fmt_num(v),
        _ => "--".to_string(),
    };

    let label = clip(
        &label_of(body, inst).unwrap_or_default(),
        (area.width / 3) as usize,
    );
    let mut x = put(
        buf,
        area,
        0,
        0,
        &label,
        Style::default().fg(colors::text_primary()),
    );
    if x > 0 {
        x += 1;
    }
    let value_cols = text_cols(&value_text, area.width);
    let strip_w = area
        .width
        .saturating_sub(x.saturating_add(value_cols).saturating_add(1)) as usize;
    let shown = &hist[hist.len().saturating_sub(strip_w)..];
    let max = if body.max.is_some() {
        scale_max(body, entity, ctx)
    } else {
        // For a strip the scale is the series' own maximum.
        shown
            .iter()
            .copied()
            .fold(0.0, f64::max)
            .max(f64::MIN_POSITIVE)
    };
    let base = latest
        .and_then(|v| threshold_color(body, v))
        .unwrap_or_else(colors::primary);
    for (i, &v) in shown.iter().enumerate() {
        let frac = (v / max).clamp(0.0, 1.0);
        let (sym, color) = if heat {
            (
                "█",
                threshold_color(body, v).unwrap_or_else(|| neutral_or_heat(body, frac)),
            )
        } else {
            (BLOCKS[(frac * 7.0).round() as usize], base)
        };
        put(
            buf,
            area,
            x.saturating_add(i as u16),
            0,
            sym,
            Style::default().fg(color),
        );
    }
    let vstyle = Style::default().fg(latest
        .and_then(|v| threshold_color(body, v))
        .unwrap_or_else(colors::text_primary));
    put(
        buf,
        area,
        x.saturating_add(shown.len() as u16).saturating_add(1),
        0,
        &value_text,
        vstyle,
    );
}

/// `label: value`, the value in its threshold colour. The caption is the
/// label, or the signal name when there is none.
fn text(
    buf: &mut Buffer,
    area: Rect,
    body: &WidgetBody,
    inst: &Inst,
    entity: Option<&EntityKey>,
    ctx: &VizCtx,
) {
    let reading = read(ctx, entity, body);
    let caption = label_of(body, inst).unwrap_or_else(|| body.signal.clone());
    let head = format!("{caption}: ");
    let x = put(
        buf,
        area,
        0,
        0,
        &head,
        Style::default().fg(colors::text_primary()),
    );
    let color = reading
        .as_ref()
        .and_then(number)
        .and_then(|v| threshold_color(body, v))
        .unwrap_or_else(colors::text_primary);
    put(
        buf,
        area,
        x,
        0,
        &fmt_value(reading.as_ref()),
        Style::default().fg(color),
    );
}

/// One line per entity, `name value`, largest value first; entities with no
/// reading come last as `--`. With a label and more than one line of room,
/// the label is a header line. When the lines do not fit, the last one says
/// `+N more`.
fn table(
    buf: &mut Buffer,
    area: Rect,
    body: &WidgetBody,
    inst: &Inst,
    entities: &[EntityKey],
    ctx: &VizCtx,
) {
    let mut rows: Vec<(String, Option<Value>)> = entities
        .iter()
        .map(|e| (short_name(e), read(ctx, Some(e), body)))
        .collect();
    // Descending by number; missing (and text) after every number. The sort
    // is stable, so ties keep key order.
    rows.sort_by(|a, b| {
        let (x, y) = (a.1.as_ref().and_then(number), b.1.as_ref().and_then(number));
        match (x, y) {
            (Some(x), Some(y)) => y.total_cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    let mut y = 0u16;
    if let Some(l) = label_of(body, inst) {
        if area.height > 1 {
            put(buf, area, 0, 0, &l, muted());
            y = 1;
        }
    }
    if rows.is_empty() {
        put(buf, area, 0, y, "--", muted());
        return;
    }
    let room = (area.height - y) as usize;
    let name_w = rows
        .iter()
        .map(|r| UnicodeWidthStr::width(r.0.as_str()))
        .max()
        .unwrap_or(0)
        .min((area.width / 2) as usize);
    // When the rows do not fit, the last line becomes the `+N more` line.
    let (shown, hidden) = if rows.len() > room {
        (room.saturating_sub(1), rows.len() - room.saturating_sub(1))
    } else {
        (rows.len(), 0)
    };
    for (i, (name, v)) in rows.iter().take(shown).enumerate() {
        put(
            buf,
            area,
            0,
            y + i as u16,
            &clip(name, name_w),
            Style::default().fg(colors::text_primary()),
        );
        let color = v
            .as_ref()
            .and_then(number)
            .and_then(|v| threshold_color(body, v))
            .unwrap_or_else(colors::text_primary);
        put(
            buf,
            area,
            name_w as u16 + 1,
            y + i as u16,
            &fmt_value(v.as_ref()),
            Style::default().fg(color),
        );
    }
    if hidden > 0 {
        put(
            buf,
            area,
            0,
            y + shown as u16,
            &format!("+{hidden} more"),
            muted(),
        );
    }
}

/// One two-column cell per entity, coloured by value: `██`, or `--` when the
/// entity has no reading. Cells run left to right and wrap to the next line.
/// The label, if any, starts the first line.
fn grid(
    buf: &mut Buffer,
    area: Rect,
    body: &WidgetBody,
    inst: &Inst,
    entities: &[EntityKey],
    ctx: &VizCtx,
) {
    let mut x = 0u16;
    if let Some(l) = label_of(body, inst) {
        let cols = put(
            buf,
            area,
            0,
            0,
            &clip(&l, (area.width / 3) as usize),
            Style::default().fg(colors::text_primary()),
        );
        x = if cols > 0 { cols + 1 } else { 0 };
    }
    if entities.is_empty() {
        put(buf, area, x, 0, "--", muted());
        return;
    }
    let mut y = 0u16;
    for e in entities {
        if x.saturating_add(2) > area.width {
            x = 0;
            y += 1;
        }
        if y >= area.height {
            break;
        }
        let reading = read(ctx, Some(e), body);
        match reading.as_ref().and_then(number) {
            Some(v) => {
                let frac = (v / scale_max(body, Some(e), ctx)).clamp(0.0, 1.0);
                let color = threshold_color(body, v).unwrap_or_else(|| neutral_or_heat(body, frac));
                put(buf, area, x, y, "██", Style::default().fg(color));
            }
            None => {
                put(buf, area, x, y, "--", muted());
            }
        }
        x += 2;
    }
}

/// The newest events, oldest first, one per line. `signal` filters them: any
/// text matches the event's name (`KernelCompiled`, `Reset`, ...) ignoring
/// ASCII case, and `*` shows every event. Events come from the bus, not from
/// a signal, so `kind` is not used.
///
/// Cost: the world hands over a copy of its retained events (there is no
/// cheaper call), so this clones them once per log widget. Beyond that the
/// work is bounded by the area: the filter looks at each event's static
/// name only, and just the last `area.height` matches are formatted.
fn log(buf: &mut Buffer, area: Rect, body: &WidgetBody, ctx: &VizCtx) {
    let events = ctx.world.events_since(0);
    let all = body.signal == "*";
    let matches: Vec<&Event> = events
        .iter()
        .map(|(_, e)| e)
        .filter(|e| all || contains_ignore_case(event_name(e), &body.signal))
        .collect();
    if matches.is_empty() {
        put(buf, area, 0, 0, "no events", muted());
        return;
    }
    let room = area.height as usize;
    for (i, e) in matches[matches.len().saturating_sub(room)..]
        .iter()
        .enumerate()
    {
        let line: String = event_line(e).chars().filter(|c| !c.is_control()).collect();
        put(
            buf,
            area,
            0,
            i as u16,
            &line,
            Style::default().fg(colors::text_primary()),
        );
    }
}

/// Does `hay` contain `needle`, ignoring ASCII case? An empty needle matches.
fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// The event's name, as the bus spells it. Every variant is listed, so a new
/// bus event fails to compile here and gets a name on purpose.
fn event_name(e: &Event) -> &'static str {
    match e {
        Event::ModelLoading { .. } => "ModelLoading",
        Event::ModelReady { .. } => "ModelReady",
        Event::ModelGone { .. } => "ModelGone",
        Event::RequestsCompleted { .. } => "RequestsCompleted",
        Event::KernelCompiled { .. } => "KernelCompiled",
        Event::Reset { .. } => "Reset",
        Event::LinkChanged { .. } => "LinkChanged",
        Event::LeaseGranted { .. } => "LeaseGranted",
        Event::LeaseReleased { .. } => "LeaseReleased",
        Event::EccTick { .. } => "EccTick",
        Event::ViewChanged { .. } => "ViewChanged",
        Event::Spark { .. } => "Spark",
        Event::LinkPeer { .. } => "LinkPeer",
    }
}

/// `n` chips, or `1 chip`.
fn chips_phrase(n: usize) -> String {
    format!("{n} chip{}", if n == 1 { "" } else { "s" })
}

/// One event as one readable line: its name, then the fields that matter in
/// plain words. Events without a hand-written line fall back to the name and
/// the derived `Debug` text of the event.
fn event_line(e: &Event) -> String {
    let name = event_name(e);
    match e {
        Event::KernelCompiled { chip, source } => match chip {
            Some(c) => format!("{name}: {source} on {}", short_name(c)),
            None => format!("{name}: {source}"),
        },
        Event::LinkChanged { chip, mask } => {
            format!(
                "{name}: {} links {mask:#06x} ({} up)",
                short_name(chip),
                mask.count_ones()
            )
        }
        Event::RequestsCompleted { model, n } => {
            format!("{name}: {n} on {}", short_name(model))
        }
        Event::Reset { chips } => format!("{name}: {}", chips_phrase(chips.len())),
        Event::ModelReady { model, chips } => {
            format!(
                "{name}: {} on {}",
                short_name(model),
                chips_phrase(chips.len())
            )
        }
        Event::ModelGone { model } => format!("{name}: {}", short_name(model)),
        Event::ViewChanged { view } => format!("{name}: {view}"),
        // The rest: name plus the derived text. Rare in a log widget.
        other => format!("{other:?}"),
    }
}
