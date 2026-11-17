// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What the `~` view (HivemindSweeper) draws from the signal bus: a strip of
//! model cells above the board, bus rows in the feed, and the KITT scanner's
//! bus input.
//!
//! # Two steps
//!
//! The shape is Die 3D's overlay (`crate::animation::die3d::overlay`):
//!
//! 1. [`HivemindBus::read_world`] runs at the data rate, in the sweeper's
//!    update arm in `crate::ui::tui` (through [`HivemindBus::update`]). It
//!    reads the world through [`WorldRead`], compares it with what it saw
//!    last, and keeps the result: the strip cells (text already formatted),
//!    the coalesced bus rows, the smoothed request rate, the tokens/s sum and
//!    the time of the last request surge. All the reading and matching
//!    happens here.
//! 2. The frame path ([`HivemindBus::strip_line`], [`HivemindBus::rows`],
//!    [`HivemindBus::kitt_activity`]) only picks among the kept strings,
//!    filters and sorts the kept rows, and does arithmetic. It takes no lock
//!    and reads nothing from the world.
//!
//! An empty world (no model, no event) leaves the strip empty, adds no row
//! and adds nothing to the scanner, so the view draws exactly what it drew
//! before the bus (pinned by a test against hashes taken at the base commit).
//!
//! # The strip
//!
//! One cell per local chip that runs a model (`model_on_chip`), in device
//! order: `D<n>`, the model flag text Die 3D draws (`ModelFlag::texts`, with
//! the `~` mark from `model_text_is_estimated`), and an eight-cell bar. The
//! bar follows the reported progress while the model compiles or loads,
//! is full when it is ready, and is left out when it is down, in alarm, or
//! loading with no progress figure. The cell is drawn in `phase_rgb`. When
//! the strip is wider than the board, each cell steps down to a shorter
//! flag text, then the bars go, then the line is cut with `…`. With no
//! model on any chip the strip takes no row.
//!
//! # Bus rows in the feed
//!
//! Bus events become feed rows next to the sweeper's own rows. Each starts
//! with `bus: `. Rows coalesce like the sweeper's: one row per kind,
//! subject and device, with a count and a per-second rate.
//!
//! | Event               | Source column       | Text                               | Severity |
//! |---------------------|---------------------|------------------------------------|----------|
//! | `ModelLoading`      | `model`             | `<label> compiling/loading [n%]`   | notice   |
//! | `ModelReady`        | `model`             | `<label> ready`                    | notice   |
//! | `ModelGone`         | `model`             | `<label> gone`                     | warn     |
//! | `RequestsCompleted` | `model`             | `<label> requests completed`       | info     |
//! | `KernelCompiled`    | the compiler's name | `kernel compiled`                  | info     |
//! | `Spark`             | the spark's source  | the spark kind (`metal.log`)       | info     |
//! | `Reset`             | `tt-smi`            | `chip reset` (drawn as a reset row)| warn     |
//!
//! A model that comes up is a milestone (notice); a model that goes away
//! or a chip that resets may stop someone's work (warn); traffic and
//! passive activity are routine (info). A requests row counts requests, so
//! its rate is requests per second. The `s` floor and the `f` unified mode
//! filter bus rows by the same rules as the sweeper's rows: a cell feed
//! shows a bus row when its source and device match the cell (model rows
//! have no sweeper source and show only in the unified feed).
//!
//! Re-announcements: the host announces every model again every five
//! minutes. An announcement with the same phase and chips as the last one
//! seen for that model adds nothing.
//!
//! History: events already in the world when the view first reads it, and
//! events that arrived while the view was away, are shown dimmed below the
//! live rows. They add no rate and no scanner surge. "Away" is Die 3D's
//! rule with Die 3D's constant: an update more than [`PULSE_NS`] after the
//! last one and more than four times the usual gap between updates. A live
//! event that coalesces into a history row makes it live again.
//!
//! The sweeper's own events: on a bus this TUI hosts, the sweeper's
//! collectors and reset detector publish `Spark`, `KernelCompiled` and
//! `Reset` records under the source name `hivemind`. The sweeper's feed
//! already shows those, so the view skips a record when the host says that
//! source published it (`BusHost::from_hivemind_source`). The same events
//! from any other source show: gozer publishes a `Reset` on the same bus
//! when it releases a lease and resets the chips, and the sweeper's
//! detector may not have seen that reset. Attached to a headless host,
//! nothing is the TUI's own, and every event shows.
//!
//! Chips this TUI does not have: an event that names chips is shown only
//! when one of them is a local chip. An event that names no chip (a spark
//! with no chip, a reset whose chip was not resolved) is shown with the
//! `host` device column. A model's rows need the model on a local chip,
//! except on a local bus, where every model is this machine's. When
//! attached to a host on another machine, no chip is local
//! (`TuiBus::origin_is_local`), since its PCI addresses are not this
//! machine's.
//!
//! # The KITT scanner
//!
//! Its activity input is the larger of the board's (`kitt_activity_norm` of
//! the feed's total rate, as before) and the bus's. The bus's is the larger
//! of the smoothed request rate over [`REQ_FULL_RATE`] and the summed
//! tokens/s of the local models over [`TPS_FULL`], plus a surge when
//! requests complete. The surge starts at [`SURGE_MAX`] times Die 3D's
//! pulse strength for the request count and falls to nothing over
//! [`PULSE_NS`], the length of Die 3D's request pulse. With no bus signal
//! the input is the board's alone.
//!
//! # Provenance
//!
//! The strip text is what the source reported, marked `~` when estimated.
//! The bar is derived from the reported progress (and is full for a ready
//! model). The scanner's motion is decoration: its speed, focus and
//! brightness respond to the measured rates, but the beam itself shows no
//! value. The surge's shape and length are decoration that marks a real
//! event.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tt_toplike_bus::{EntityKey, Event, ModelPhase, ModelView, WorldRead};
use unicode_width::UnicodeWidthStr;

use crate::animation::die3d::overlay::{
    flag_of, model_text_is_estimated, phase_rgb, spark_rgb, strength, LOAD_PURPLE, PULSE_NS,
    SERVE_TEAL,
};
use crate::backend::TelemetryBackend;
use crate::bus::tui_bus::TuiBus;
use crate::ui::colors;
use crate::workload::hivemind::event::{Severity, Source};

/// Requests per second that count as full activity for the scanner.
pub const REQ_FULL_RATE: f64 = 4.0;
/// Time constant of the request-rate smoothing, in seconds.
pub const REQ_TAU_S: f64 = 3.0;
/// Tokens per second (summed over the local models) that count as full
/// activity for the scanner.
pub const TPS_FULL: f64 = 100.0;
/// The largest surge a request completion adds to the scanner's activity.
pub const SURGE_MAX: f32 = 0.5;
/// Bus rows kept; past this the row with the oldest event goes.
pub const MAX_ROWS: usize = 64;
/// What every bus row's text starts with.
pub const MARK: &str = "bus: ";
/// Per-second buckets behind a row's rate and sparkline: the sweeper's
/// own feed keeps six (`workload::hivemind::feed`).
pub const BUCKETS: usize = 6;
/// Cells in a strip cell's progress bar.
pub const BAR_W: usize = 8;

const NS_PER_S: f64 = 1e9;

/// What a bus row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusKind {
    ModelLoading,
    ModelReady,
    ModelGone,
    Requests,
    Kernel,
    Spark,
    Reset,
}

impl BusKind {
    /// The severity in the module doc's table.
    pub fn severity(self) -> Severity {
        match self {
            BusKind::ModelLoading | BusKind::ModelReady => Severity::Notice,
            BusKind::ModelGone | BusKind::Reset => Severity::Warn,
            BusKind::Requests | BusKind::Kernel | BusKind::Spark => Severity::Info,
        }
    }
}

/// One cell of the strip: a local chip and the model on it.
#[derive(Debug, Clone, PartialEq)]
pub struct StripCell {
    pub device: usize,
    /// The cell's text forms, longest first: `D<n>` and each of the flag's
    /// texts, then `D<n>` and the phase word alone.
    pub forms: Vec<String>,
    /// How much of the bar is filled (0..1), or no bar.
    pub bar: Option<f32>,
    pub rgb: [u8; 3],
}

/// One coalesced bus row of the feed.
#[derive(Debug, Clone)]
pub struct BusRow {
    pub kind: BusKind,
    /// The coalescing key with `kind` and `device`: the model key, the
    /// spark kind or the compiler's name.
    subject: String,
    /// The sweeper source a cell filter matches it on; `None` for model
    /// rows.
    pub source: Option<Source>,
    /// The source column's text.
    pub label: &'static str,
    /// The source column's colour (the shared workload and spark colours).
    pub rgb: [u8; 3],
    pub device: Option<u8>,
    /// The text after [`MARK`].
    pub display: String,
    /// Events folded in; requests for a requests row.
    pub count: u64,
    pub severity: Severity,
    /// When a live event last folded in (when the view read it).
    pub last: Instant,
    /// Sequence number of the newest event folded in.
    pub seq: u64,
    /// Every event folded in was history.
    pub history: bool,
    /// Per-second counts, oldest first.
    buckets: VecDeque<u32>,
    /// Counted since the last roll.
    current: u32,
    /// Events (or requests) per second over the buckets.
    pub rate: f32,
}

impl BusRow {
    /// Whether the feed draws it as a reset row.
    pub fn is_reset(&self) -> bool {
        self.kind == BusKind::Reset
    }

    /// The per-second counts for the row's sparkline, oldest first, padded
    /// with zeros at the front.
    pub fn spark(&self) -> [u8; BUCKETS] {
        let mut s = [0u8; BUCKETS];
        let pad = BUCKETS.saturating_sub(self.buckets.len());
        for (i, c) in self.buckets.iter().enumerate() {
            s[pad + i] = (*c).min(u8::MAX as u32) as u8;
        }
        s
    }
}

/// The last announcement seen for a model.
#[derive(Debug, Clone)]
struct Announce {
    kind: BusKind,
    chips: Vec<EntityKey>,
    label: String,
    estimated: bool,
}

/// What the `~` view keeps from the bus between data ticks. Start with
/// `HivemindBus::default()`. See the module doc.
#[derive(Debug, Default)]
pub struct HivemindBus {
    /// The highest event sequence number handled. `None` before the first
    /// world.
    seen_seq: Option<u64>,
    /// Bus time of the last update.
    last_now: Option<u64>,
    /// The gap between the last two updates.
    last_gap: Option<u64>,
    /// By model key.
    announced: HashMap<EntityKey, Announce>,
    rows: Vec<BusRow>,
    /// When the row buckets last rolled.
    last_roll: Option<Instant>,
    strip: Vec<StripCell>,
    /// Smoothed requests per second.
    req_rate: f64,
    /// Summed tokens per second of the local models.
    tps: f64,
    /// The last surge: when it started and how strong it was.
    surge: Option<(Instant, f32)>,
}

/// Every sweeper source, for matching a source name to one.
const SOURCES: [Source; 12] = [
    Source::Ttnn,
    Source::TtMetal,
    Source::TtLang,
    Source::TtForge,
    Source::TtXla,
    Source::Vllm,
    Source::Workload,
    Source::TtSmi,
    Source::Driver,
    Source::Inspector,
    Source::Host,
    Source::Unknown,
];

/// The sweeper source whose label is `name`'s first word (`metal.log` is
/// `metal`), ignoring case.
fn source_named(name: &str) -> Option<Source> {
    let first = name
        .split(|c: char| c == '.' || c.is_whitespace())
        .next()
        .unwrap_or("");
    SOURCES
        .iter()
        .copied()
        .find(|s| s.label().eq_ignore_ascii_case(first))
}

/// What one update needs while it reads events.
struct Ctx<'a> {
    chips: &'a [(usize, EntityKey)],
    local: bool,
    history: bool,
    at: Instant,
}

impl Ctx<'_> {
    /// The device index of `chip` when it is a local chip.
    fn device(&self, chip: &EntityKey) -> Option<u8> {
        self.chips
            .iter()
            .find(|(_, k)| k == chip)
            .and_then(|(i, _)| u8::try_from(*i).ok())
    }

    /// The first local chip of `chips`, as a device index.
    fn first_device(&self, chips: &[EntityKey]) -> Option<u8> {
        chips.iter().find_map(|c| self.device(c))
    }

    /// Whether a model on `chips` is this TUI's (see the module doc).
    fn model_is_ours(&self, chips: &[EntityKey]) -> bool {
        self.local || chips.iter().any(|c| self.device(c).is_some())
    }

    /// Whether an event naming `chips` may be shown: it names none, or one
    /// of them is local.
    fn chips_are_ours(&self, chips: &[EntityKey]) -> bool {
        chips.is_empty() || chips.iter().any(|c| self.device(c).is_some())
    }
}

/// A new row's fields, before it is folded in.
struct NewRow {
    kind: BusKind,
    subject: String,
    source: Option<Source>,
    label: &'static str,
    rgb: [u8; 3],
    device: Option<u8>,
    display: String,
    n: u32,
}

impl HivemindBus {
    /// Read the bus for the `~` view: the world as Die 3D reads it
    /// (`TuiBus::read`, limited to local chips when attached) and this
    /// TUI's chips. Call at the data rate while `~` is on screen.
    pub fn update(&mut self, bus: &TuiBus, backend: &dyn TelemetryBackend) {
        let chips: Vec<(usize, EntityKey)> = if bus.origin_is_local() {
            backend
                .devices()
                .iter()
                .filter(|d| !d.bus_id.is_empty())
                .map(|d| (d.index, EntityKey::chip(&d.bus_id)))
                .collect()
        } else {
            Vec::new()
        };
        let world = bus.read();
        let now = world.now_mono_ns();
        let host = bus.host();
        let own = |q: u64| host.is_some_and(|h| h.from_hivemind_source(q));
        self.read_world_from(
            &world,
            &chips,
            !bus.is_attached(),
            &own,
            now,
            Instant::now(),
        );
    }

    /// [`HivemindBus::read_world_from`] for a world in which no record came
    /// from this TUI's own sweeper. For tests.
    #[cfg(test)]
    pub fn read_world(
        &mut self,
        world: &dyn WorldRead,
        chips: &[(usize, EntityKey)],
        local: bool,
        now_ns: u64,
        at: Instant,
    ) {
        self.read_world_from(world, chips, local, &|_| false, now_ns, at);
    }

    /// Read `world` at bus time `now_ns`, with `chips` this TUI's chips
    /// (device index and chip key). `local` is true when this TUI hosts the
    /// bus. `own` says whether the record with a given sequence number came
    /// from this TUI's own sweeper (`BusHost::from_hivemind_source`). `at`
    /// is the frame clock's reading for the same moment: row times and the
    /// surge are kept on it, so the frame path needs no bus read. See the
    /// module doc.
    pub fn read_world_from(
        &mut self,
        world: &dyn WorldRead,
        chips: &[(usize, EntityKey)],
        local: bool,
        own: &dyn Fn(u64) -> bool,
        now_ns: u64,
        at: Instant,
    ) {
        // Die 3D's rule for history: the first world, a world whose head
        // went backwards (a restarted host), and an update after the view
        // was away are a first sight.
        let head = world.seq();
        let gap = self.last_now.map(|t| now_ns.saturating_sub(t));
        let away = gap.is_some_and(|g| {
            g > PULSE_NS
                && self
                    .last_gap
                    .is_none_or(|usual| g > usual.saturating_mul(4))
        });
        self.last_now = Some(now_ns);
        if gap.is_some() {
            self.last_gap = gap;
        }
        let from = self.seen_seq.filter(|s| *s <= head);
        let ctx = Ctx {
            chips,
            local,
            history: from.is_none() || away,
            at,
        };

        self.roll(at);
        let models = world.models();
        let mut requests: u32 = 0;
        let mut seen = head;
        for (q, e) in world.events_since(from.unwrap_or(0)) {
            seen = seen.max(q);
            match e {
                Event::ModelLoading {
                    model,
                    chips: on,
                    progress,
                } => self.announce(
                    world,
                    &models,
                    &ctx,
                    q,
                    BusKind::ModelLoading,
                    model,
                    on,
                    progress,
                ),
                Event::ModelReady { model, chips: on } => self.announce(
                    world,
                    &models,
                    &ctx,
                    q,
                    BusKind::ModelReady,
                    model,
                    on,
                    None,
                ),
                Event::ModelGone { model } => {
                    let Some(a) = self.announced.remove(&model) else {
                        continue;
                    };
                    let tilde = if a.estimated { "~" } else { "" };
                    self.fold(
                        &ctx,
                        q,
                        NewRow {
                            kind: BusKind::ModelGone,
                            subject: model.to_string(),
                            source: None,
                            label: "model",
                            rgb: phase_rgb(ModelPhase::Down),
                            device: ctx.first_device(&a.chips),
                            display: format!("{tilde}{} gone", a.label),
                            n: 1,
                        },
                    );
                }
                Event::RequestsCompleted { model, n } => {
                    let Some(m) = models.iter().find(|m| m.key == model) else {
                        continue;
                    };
                    if !ctx.model_is_ours(&m.chips) {
                        continue;
                    }
                    let tilde = if model_text_is_estimated(world, m) {
                        "~"
                    } else {
                        ""
                    };
                    self.fold(
                        &ctx,
                        q,
                        NewRow {
                            kind: BusKind::Requests,
                            subject: model.to_string(),
                            source: None,
                            label: "model",
                            rgb: SERVE_TEAL,
                            device: ctx.first_device(&m.chips),
                            display: format!("{tilde}{} requests completed", m.label),
                            n,
                        },
                    );
                    if !ctx.history {
                        requests = requests.saturating_add(n);
                    }
                }
                // What this TUI's own sweeper published is already on
                // screen (see the module doc).
                Event::KernelCompiled { .. } | Event::Spark { .. } | Event::Reset { .. }
                    if own(q) => {}
                Event::KernelCompiled { chip, source } => {
                    if !ctx.chips_are_ours(chip.as_slice()) {
                        continue;
                    }
                    let src = source_named(&source);
                    self.fold(
                        &ctx,
                        q,
                        NewRow {
                            kind: BusKind::Kernel,
                            subject: source.clone(),
                            source: src,
                            label: src.map_or("kernel", |s| s.label()),
                            rgb: LOAD_PURPLE,
                            device: chip.as_ref().and_then(|c| ctx.device(c)),
                            display: "kernel compiled".to_string(),
                            n: 1,
                        },
                    );
                }
                Event::Spark { chip, kind } => {
                    if !ctx.chips_are_ours(chip.as_slice()) {
                        continue;
                    }
                    let src = source_named(&kind);
                    self.fold(
                        &ctx,
                        q,
                        NewRow {
                            kind: BusKind::Spark,
                            subject: kind.clone(),
                            source: src,
                            label: src.map_or("spark", |s| s.label()),
                            rgb: spark_rgb(&kind),
                            device: chip.as_ref().and_then(|c| ctx.device(c)),
                            display: kind,
                            n: 1,
                        },
                    );
                }
                Event::Reset { chips: on } => {
                    if !ctx.chips_are_ours(&on) {
                        continue;
                    }
                    self.fold(
                        &ctx,
                        q,
                        NewRow {
                            kind: BusKind::Reset,
                            subject: String::new(),
                            source: Some(Source::TtSmi),
                            label: Source::TtSmi.label(),
                            rgb: spark_rgb("tt-smi"),
                            device: ctx.first_device(&on),
                            display: "chip reset".to_string(),
                            n: 1,
                        },
                    );
                }
                _ => {}
            }
        }
        self.seen_seq = Some(seen);

        // The request rate, smoothed over `REQ_TAU_S`. History adds nothing,
        // and a long gap lets the rate fall to what this update saw.
        if let Some(g) = gap.filter(|g| *g > 0) {
            let dt = g as f64 / NS_PER_S;
            let inst = requests as f64 / dt;
            let alpha = 1.0 - (-dt / REQ_TAU_S).exp();
            self.req_rate += (inst - self.req_rate) * alpha;
        }
        if requests > 0 {
            self.surge = Some((at, SURGE_MAX * strength(requests)));
        }

        // Tokens per second of the models on this TUI's chips.
        self.tps = models
            .iter()
            .filter(|m| ctx.model_is_ours(&m.chips))
            .filter_map(|m| m.tokens_per_s)
            .filter(|t| t.is_finite() && *t >= 0.0)
            .sum();

        // The strip: one cell per local chip that runs a model.
        self.strip = chips
            .iter()
            .filter_map(|(i, k)| {
                let m = world.model_on_chip(k).filter(|m| !m.chips.is_empty())?;
                Some(strip_cell(world, *i, &m))
            })
            .collect();
    }

    /// Note a `ModelLoading` or `ModelReady` announcement.
    #[allow(clippy::too_many_arguments)]
    fn announce(
        &mut self,
        world: &dyn WorldRead,
        models: &[ModelView],
        ctx: &Ctx<'_>,
        q: u64,
        kind: BusKind,
        model: EntityKey,
        on: Vec<EntityKey>,
        progress: Option<f32>,
    ) {
        if !ctx.model_is_ours(&on) {
            return;
        }
        let m = models.iter().find(|m| m.key == model);
        let label = m.map_or_else(|| model.to_string(), |m| m.label.clone());
        let estimated = m.is_some_and(|m| model_text_is_estimated(world, m));
        let same = self
            .announced
            .get(&model)
            .is_some_and(|a| a.kind == kind && a.chips == on);
        let device = ctx.first_device(&on);
        self.announced.insert(
            model.clone(),
            Announce {
                kind,
                chips: on,
                label: label.clone(),
                estimated,
            },
        );
        if same {
            return;
        }
        let tilde = if estimated { "~" } else { "" };
        let (display, phase) = match kind {
            BusKind::ModelLoading => {
                let word = if m.is_some_and(|m| m.phase == ModelPhase::Compiling) {
                    "compiling"
                } else {
                    "loading"
                };
                let pct = progress
                    .filter(|p| p.is_finite())
                    .map(|p| format!(" {:.0}%", p.clamp(0.0, 1.0) * 100.0))
                    .unwrap_or_default();
                (format!("{tilde}{label} {word}{pct}"), ModelPhase::Loading)
            }
            _ => (format!("{tilde}{label} ready"), ModelPhase::Ready),
        };
        self.fold(
            ctx,
            q,
            NewRow {
                kind,
                subject: model.to_string(),
                source: None,
                label: "model",
                rgb: phase_rgb(phase),
                device,
                display,
                n: 1,
            },
        );
    }

    /// Fold an event into its row, or start one.
    fn fold(&mut self, ctx: &Ctx<'_>, q: u64, new: NewRow) {
        let live = !ctx.history;
        if let Some(r) = self
            .rows
            .iter_mut()
            .find(|r| r.kind == new.kind && r.subject == new.subject && r.device == new.device)
        {
            r.count += new.n as u64;
            r.display = new.display;
            r.rgb = new.rgb;
            r.seq = r.seq.max(q);
            if live {
                r.last = ctx.at;
                r.history = false;
                r.current = r.current.saturating_add(new.n);
            }
            return;
        }
        self.rows.push(BusRow {
            kind: new.kind,
            subject: new.subject,
            source: new.source,
            label: new.label,
            rgb: new.rgb,
            device: new.device,
            display: new.display,
            count: new.n as u64,
            severity: new.kind.severity(),
            last: ctx.at,
            seq: q,
            history: !live,
            buckets: VecDeque::with_capacity(BUCKETS),
            current: if live { new.n } else { 0 },
            rate: 0.0,
        });
        if self.rows.len() > MAX_ROWS {
            if let Some(i) = self
                .rows
                .iter()
                .enumerate()
                .min_by_key(|(_, r)| r.seq)
                .map(|(i, _)| i)
            {
                self.rows.remove(i);
            }
        }
    }

    /// Roll the rows' per-second buckets, as the sweeper's feed does.
    fn roll(&mut self, at: Instant) {
        let secs = match self.last_roll {
            None => {
                self.last_roll = Some(at);
                return;
            }
            Some(t) => at.saturating_duration_since(t).as_secs_f32(),
        };
        if secs < 1.0 {
            return;
        }
        let steps = (secs.floor() as usize).min(BUCKETS + 1);
        for r in &mut self.rows {
            for i in 0..steps {
                r.buckets.push_back(if i == 0 { r.current } else { 0 });
                if r.buckets.len() > BUCKETS {
                    r.buckets.pop_front();
                }
            }
            r.current = 0;
            r.rate = r.buckets.iter().sum::<u32>() as f32 / BUCKETS as f32;
        }
        self.last_roll = Some(at);
    }

    /// The strip's cells, in device order. Empty when no local chip runs a
    /// model. The view draws it through [`HivemindBus::strip_line`].
    #[cfg(test)]
    pub fn strip(&self) -> &[StripCell] {
        &self.strip
    }

    /// The strip as one line at most `width` columns wide, or `None` when
    /// it is empty. See the module doc for how it fits.
    pub fn strip_line(&self, width: usize) -> Option<Line<'static>> {
        if self.strip.is_empty() {
            return None;
        }
        let deepest = self.strip.iter().map(|c| c.forms.len()).max().unwrap_or(1);
        let measure = |level: usize, bars: bool| -> usize {
            self.strip
                .iter()
                .map(|c| {
                    let form = &c.forms[level.min(c.forms.len() - 1)];
                    let bar = if bars && c.bar.is_some() {
                        1 + BAR_W
                    } else {
                        0
                    };
                    form.width() + bar
                })
                .sum::<usize>()
                + 2 * (self.strip.len() - 1)
        };
        // Shorter flag texts with bars, then the same without bars, then the
        // phase word alone with and without bars. The last form of every
        // cell is the phase word alone, so `deepest - 1` reaches it.
        let texts = deepest.saturating_sub(1).max(1);
        let choice = (0..texts)
            .map(|l| (l, true))
            .chain((0..texts).map(|l| (l, false)))
            .chain([(deepest - 1, true), (deepest - 1, false)])
            .find(|(l, b)| measure(*l, *b) <= width)
            .unwrap_or((deepest - 1, false));
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, c) in self.strip.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let fg = colors::rgb(c.rgb[0], c.rgb[1], c.rgb[2]);
            let form = c.forms[choice.0.min(c.forms.len() - 1)].clone();
            spans.push(Span::styled(form, Style::default().fg(fg)));
            if let (Some(p), true) = (c.bar, choice.1) {
                let filled = ((p.clamp(0.0, 1.0) * BAR_W as f32).round() as usize).min(BAR_W);
                spans.push(Span::raw(" "));
                spans.push(Span::styled("█".repeat(filled), Style::default().fg(fg)));
                spans.push(Span::styled(
                    "░".repeat(BAR_W - filled),
                    Style::default().fg(fg).add_modifier(Modifier::DIM),
                ));
            }
        }
        Some(Line::from(fit_spans(spans, width)))
    }

    /// The bus rows that pass the feed's filters: `cell` as the sweeper's
    /// feed takes it (`None` is the unified feed) and the severity floor.
    /// Live rows come first, newest first; then history, newest first.
    pub fn rows(&self, cell: Option<(Source, Option<u8>)>, floor: Severity) -> Vec<&BusRow> {
        let mut out: Vec<&BusRow> = self
            .rows
            .iter()
            .filter(|r| {
                r.severity >= floor
                    && cell.is_none_or(|(s, d)| r.source == Some(s) && r.device == d)
            })
            .collect();
        out.sort_by(|a, b| {
            a.history
                .cmp(&b.history)
                .then(b.last.cmp(&a.last))
                .then(b.seq.cmp(&a.seq))
        });
        out
    }

    /// The KITT scanner's activity (0..1) at frame time `now`, with
    /// `board_rate` the sweeper feed's total rate. Arithmetic only. See the
    /// module doc.
    pub fn kitt_activity(&self, board_rate: f32, now: Instant) -> f32 {
        let board = super::hivemind_view::kitt_activity_norm(board_rate);
        let rate = (self.req_rate / REQ_FULL_RATE).clamp(0.0, 1.0) as f32;
        let tps = (self.tps / TPS_FULL).clamp(0.0, 1.0) as f32;
        let surge = self.surge.map_or(0.0, |(t, s)| {
            let pulse = Duration::from_nanos(PULSE_NS);
            let age = now.saturating_duration_since(t);
            if age >= pulse {
                0.0
            } else {
                s * (1.0 - age.as_secs_f32() / pulse.as_secs_f32())
            }
        });
        let bus = (rate.max(tps) + surge).clamp(0.0, 1.0);
        board.max(bus)
    }
}

/// The strip cell for chip `device`, which runs `m`.
fn strip_cell(world: &dyn WorldRead, device: usize, m: &ModelView) -> StripCell {
    let flag = flag_of(world, m);
    let tilde = if flag.estimated { "~" } else { "" };
    let mut forms: Vec<String> = flag
        .texts()
        .into_iter()
        .map(|t| format!("D{device} {t}"))
        .collect();
    forms.push(format!("D{device} {tilde}{}", flag.phase_word()));
    forms.dedup();
    let bar = match m.phase {
        ModelPhase::Compiling | ModelPhase::Loading => m
            .progress
            .filter(|p| p.is_finite())
            .map(|p| p.clamp(0.0, 1.0)),
        ModelPhase::Ready => Some(1.0),
        ModelPhase::Down | ModelPhase::Alarm => None,
    };
    StripCell {
        device,
        forms,
        bar,
        rgb: flag.colour,
    }
}

/// `spans` cut to `width` columns, ending in `…` when anything was cut.
fn fit_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.width()).sum();
    if total <= width {
        return spans;
    }
    let mut out = Vec::new();
    if width == 0 {
        return out;
    }
    // Room for the ellipsis.
    let mut left = width - 1;
    let mut last_style = Style::default();
    for s in spans {
        last_style = s.style;
        let mut kept = String::new();
        for ch in s.content.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            if w > left {
                left = 0;
                break;
            }
            kept.push(ch);
            left -= w;
        }
        let full = kept.len() == s.content.len();
        if !kept.is_empty() {
            out.push(Span::styled(kept, s.style));
        }
        if !full {
            break;
        }
    }
    out.push(Span::styled("…", last_style));
    out
}

#[cfg(test)]
#[path = "hivemind_bus_tests.rs"]
mod tests;
