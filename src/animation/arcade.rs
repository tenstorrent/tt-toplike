// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Arcade Mode - Unified Psychedelic Visualization
//!
//! Combines all three visualizations (Starfield, Memory Castle, Memory Flow) into a single
//! all-encompassing view, with one shared telemetry strip (or the hero ⚔ snake
//! duel while a model serves) on Row 1.
//!
//! Layout: rows of panels from the `[arcade]` config section (see
//! `arcade_layout` and `docs/arcade-layout.md`). The default, with no config:
//! - Top 40%: Starfield (stars = Tensix cores, planets = memory hierarchy)
//! - Middle 30%: Memory Castle (55% of the width) beside Defrag
//! - Bottom 30%: Memory Flow (NoC particles, DDR perimeter, heat map center)
//!
//! Die 3D, Training, the inference server and Saga can be panels too. Each panel's
//! view is built at its panel's size when the arcade is made, and the arcade
//! is made again on a resize, so the frame path does no layout work.
//!
//! Until 0.16.5 a roaming `@` hero moved over the whole arcade (power set its
//! row, current its column) and left a trail. It was removed because it
//! distracted from the panels. The duel's `@` on Row 1 is a different glyph
//! and stays (see `duel`).

use crate::animation::arcade_bus::{self, ArcadeBusState, ArcadeFx};
use crate::animation::arcade_layout::{self, ArcadeConfig, Layout, Panel, View};
use crate::animation::{
    bbs_rule, hsv_to_rgb, temp_to_hue, AdaptiveBaseline, BoardTopology, DefragVis, DuelState,
    HardwareStarfield, MemoryCastle, MemoryFlowVis,
};
use crate::backend::TelemetryBackend;
use crate::ui::colors;
use crate::ui::tui::saga_view::{self, PanelFeed, SagaMemory};
use crate::workload::inference_server::ServingStats;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// The Saga panel's input: the last feed and a copy of the TUI's memory.
#[derive(Debug, Clone)]
struct SagaPanel {
    feed: PanelFeed,
    memory: SagaMemory,
}

impl Default for SagaPanel {
    fn default() -> Self {
        SagaPanel {
            feed: PanelFeed::Waiting,
            memory: SagaMemory::new(),
        }
    }
}

/// Arcade visualization: the panels of the configured layout under one
/// shared header.
pub struct ArcadeVisualization {
    width: usize,
    height: usize,

    // Sub-visualizations, one per panel the layout shows (`None` for a view
    // the layout leaves out). Each is built at its panel's size.
    pub starfield: Option<HardwareStarfield>,
    pub memory_castle: Option<MemoryCastle>,
    pub memory_flow: Option<MemoryFlowVis>,
    defrag: Option<DefragVis>,
    /// The Die 3D panel: the full Die 3D view at the panel's size, so its
    /// scene and bus overlay are the Die 3D mode's own.
    die3d: Option<crate::animation::Die3dView>,
    /// The Training panel: the Training view at the panel's size.
    train: Option<crate::animation::TrainView>,
    /// The inference panel: the `i` view's serving snake.
    snake: Option<crate::animation::Snake>,
    /// The Saga panel: what the TUI last fed it (`set_saga`).
    saga: Option<SagaPanel>,

    /// Where every panel goes, worked out once at construction.
    layout: Layout,
    /// The config the layout came from.
    config: ArcadeConfig,
    /// The inference service rows of this tick, for the inference panel.
    services: Vec<crate::workload::inference_server::ServiceState>,
    /// The training run of this tick, for the Training panel.
    train_state: Option<crate::workload::train::TrainState>,
    /// The state the Training panel draws with no run, made once so a frame
    /// does not build one.
    idle_train: crate::workload::train::TrainState,
    /// Seconds of view time for the Die 3D panel, set by the TUI each update
    /// (`set_clock`). `None` uses the frame count at 60 frames a second,
    /// which keeps tests on a fixed clock.
    clock_s: Option<f64>,
    /// The bus time the Die 3D panel draws its overlay at; `None` without a
    /// bus.
    die3d_bus_now: Option<u64>,
    /// Whether `set_bus` has updated the Die 3D panel since the last update.
    die3d_fed: bool,

    // Animation state
    frame: u32,
    /// The first device's learned power and current baseline. The duel's
    /// hero lunges when power rises more than 15% over it.
    baseline: AdaptiveBaseline,

    /// Board topology — used to render the header topology diagram line.
    board_topology: Option<BoardTopology>,

    // ── Hero ⚔ snake duel ──────────────────────────────────────────────────
    /// Live serving telemetry of the first Ready inference service, threaded in
    /// from the TUI. `None` when nothing is serving — the duel strip is hidden
    /// and Row 1 stays exactly as Task 3 left it.
    serving: Option<ServingStats>,
    /// Activities running this tick (training, loading, serving), most urgent first.
    activities: Vec<crate::animation::Activity>,
    /// The last pulse count seen per activity id, to tell a rise from a reset.
    /// Keyed by identity, not lane position, because lanes reorder and exit.
    last_pulse: std::collections::HashMap<String, u64>,
    /// Motion + glyph state for the duel strip.
    duel: DuelState,
    /// Running peak of observed device power (W), floored at 1.0. Normalizes the
    /// hardware-headroom signal so the duel is sensitive on any hardware.
    power_peak: f32,

    // ── Signal bus (see `arcade_bus`) ─────────────────────────────────────
    /// What the bus puts on the arcade, as of the last `set_bus`. Empty
    /// without a bus, and then the arcade draws what it drew before the bus.
    bus_fx: ArcadeFx,
    /// What `ArcadeFx::from_world` remembers between updates.
    bus_state: ArcadeBusState,
    /// The bus time of the last `set_bus`: the time the frames are drawn at.
    bus_now: u64,

    /// The sensitivity multiplier last set with `set_sensitivity`, kept so
    /// [`ArcadeVisualization::relayout`] can hand it to the new panels.
    sensitivity: f32,
}

impl ArcadeVisualization {
    /// An arcade with the default layout, which is the layout it has always had.
    pub fn new(width: usize, height: usize) -> Self {
        Self::with_config(width, height, &ArcadeConfig::default())
    }

    /// An arcade laid out by `config` on a `width` x `height` terminal.
    ///
    /// The rows (see `arcade_layout`):
    ///   Row 0:         title bar
    ///   Row 1:         telemetry strip, topology diagram or the duel
    ///   then per configured row: a `╔══[ LABEL ]` rule and its content
    ///   Last row:      legend/status bar (overwritten by command-bar overlay)
    pub fn with_config(width: usize, height: usize, config: &ArcadeConfig) -> Self {
        let layout = arcade_layout::layout(&config.rows, width, height);
        let rect = |v: View| layout.find(v).map(|p| p.rect);

        // Embedded sub-visualizations render with chrome OFF: each one drops its
        // own per-device W/°C header so the composite view isn't printing the
        // same telemetry ~3×.  Arcade renders ONE shared telemetry strip (Row 1)
        // instead.  Standalone modes keep chrome on (their default).
        let starfield = rect(View::Starfield).map(|r| {
            let mut sf = HardwareStarfield::new(r.w, r.h);
            sf.set_chrome(false);
            sf
        });
        let memory_castle = rect(View::Castle).map(|r| {
            let mut c = MemoryCastle::new_with_density(r.w, r.h, 200, 10);
            c.set_chrome(false);
            c
        });
        let memory_flow = rect(View::Flow).map(|r| {
            let mut f = MemoryFlowVis::new_with_density(r.w, r.h, 100);
            f.set_chrome(false);
            f
        });
        let defrag = rect(View::Defrag).map(|r| DefragVis::new(r.w, r.h));
        // The same settings the TUI gives the Die 3D view (`new_die3d_view`).
        let die3d = rect(View::Die3d).map(|r| {
            crate::animation::Die3dView::new(r.w, r.h)
                .with_transparent(colors::canvas_transparent())
                .with_theme(colors::current_theme())
                .with_cell_mode(crate::animation::die3d::cellmode::CellMode::Auto)
        });
        let train = rect(View::Training).map(|r| crate::animation::TrainView::new(r.w, r.h));
        let snake = rect(View::Inference).map(|_| crate::animation::Snake::new());
        let saga = rect(View::Saga).map(|_| SagaPanel::default());

        Self {
            width,
            height,
            starfield,
            memory_castle,
            defrag,
            memory_flow,
            die3d,
            train,
            snake,
            saga,
            layout,
            config: config.clone(),
            services: Vec::new(),
            train_state: None,
            idle_train: crate::workload::train::TrainState::default(),
            clock_s: None,
            die3d_bus_now: None,
            die3d_fed: false,
            frame: 0,
            baseline: AdaptiveBaseline::new(),
            board_topology: None,
            serving: None,
            activities: Vec::new(),
            last_pulse: std::collections::HashMap::new(),
            duel: DuelState::new(),
            power_peak: 1.0,
            bus_fx: ArcadeFx::default(),
            bus_state: ArcadeBusState::default(),
            bus_now: 0,
            sensitivity: 1.0,
        }
    }

    /// This arcade laid out again from `config` at `width` x `height`: a
    /// switch to another named layout (`arcade_presets`), or the same
    /// layout at a new size.
    ///
    /// The panels are built new at their new sizes, set up as the TUI sets
    /// up a new arcade (devices, sensitivity, topology). Everything that
    /// does not depend on the layout is kept: the frame count and the
    /// clock, the power baseline
    /// and peak, the duel, the activity lanes and their pulse counts, the
    /// service rows and training run of the last tick, and the bus state.
    /// Keeping the bus state means events already seen before the switch
    /// make no new pulse after it (port contract rule 3).
    ///
    /// The result draws without an update first, so the TUI can draw it on
    /// the same keypress.
    pub fn relayout(
        self,
        width: usize,
        height: usize,
        config: &ArcadeConfig,
        backend: &dyn TelemetryBackend,
    ) -> Self {
        let mut next = Self::with_config(width, height, config);
        next.initialize_from_devices(backend.devices());
        next.set_sensitivity(self.sensitivity);
        next.initialize_topology(backend);
        next.frame = self.frame;
        next.clock_s = self.clock_s;
        next.baseline = self.baseline;
        next.power_peak = self.power_peak;
        next.serving = self.serving;
        next.duel = self.duel;
        next.activities = self.activities;
        next.last_pulse = self.last_pulse;
        next.services = self.services;
        next.train_state = self.train_state;
        next.bus_fx = self.bus_fx;
        next.bus_state = self.bus_state;
        next.bus_now = self.bus_now;
        // The Die 3D panel, if the new layout has one, draws its overlay at
        // the last bus time until the next `set_bus` feeds it.
        next.die3d_bus_now = self.die3d_bus_now.filter(|_| next.die3d.is_some());
        // The Saga panel keeps what it was last fed, so the frame after a
        // switch shows the map. A new panel shows the waiting note until
        // its first feed.
        if let (Some(old), Some(new)) = (self.saga, next.saga.as_mut()) {
            *new = old;
        }
        next
    }

    /// Row `i` of the layout as the old region fields gave it: its rule and
    /// its content rows (`starfield_start .. starfield_end` for row 0 of the
    /// default layout).
    #[cfg(test)]
    fn region(&self, i: usize) -> std::ops::Range<usize> {
        let r = &self.layout.rows[i];
        r.rule_y..r.rule_y + r.height
    }

    /// Where every panel goes.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The config this arcade was laid out from.
    pub fn config(&self) -> &ArcadeConfig {
        &self.config
    }

    /// Hand the arcade this tick's inference service rows (the rows the `i`
    /// view draws, from `inference_bus::attached_rows` when attached). They
    /// feed the inference panel.
    pub fn set_services(&mut self, rows: Vec<crate::workload::inference_server::ServiceState>) {
        self.services = rows;
    }

    /// Hand the arcade this tick's training run, for the Training panel.
    pub fn set_train_state(&mut self, st: Option<crate::workload::train::TrainState>) {
        self.train_state = st;
    }

    /// Hand the arcade the saga driver's last published state (or why there
    /// is none) and the TUI's Saga memory (the lit cells, the trail and the
    /// heat), once per update. Only a layout with a Saga panel keeps them.
    ///
    /// The memory is copied into the panel's own buffers
    /// ([`SagaMemory::copy_from`]), which allocates nothing once the floor
    /// is known, and then takes in the fed state, so the panel's fog
    /// matches its state even when the TUI observes the state after this
    /// call. The state is an `Arc`, so no state is copied.
    pub fn set_saga(&mut self, feed: PanelFeed, memory: &SagaMemory) {
        if let Some(p) = self.saga.as_mut() {
            p.memory.copy_from(memory);
            if let PanelFeed::Live { state, .. } = &feed {
                p.memory.observe(state);
            }
            p.feed = feed;
        }
    }

    /// Set the Die 3D panel's view time, in seconds.
    pub fn set_clock(&mut self, secs: f64) {
        self.clock_s = Some(secs);
    }

    /// The Die 3D panel's view time.
    fn now_s(&self) -> f64 {
        self.clock_s.unwrap_or(self.frame as f64 / 60.0)
    }

    /// Hand the arcade the signal bus's world (`TuiBus::read`), once per
    /// update, before `update`. It works out what the bus puts on the arcade
    /// (see `arcade_bus`): bring-up gauges and model labels, spark and
    /// compile particles, and reset flashes, and counts the requests that
    /// make the duel's snake lunge. Only `backend`'s chips are drawn on.
    /// With `None` the arcade draws what it drew before the bus, and the next
    /// world is read as a first sight.
    pub fn set_bus(
        &mut self,
        world: Option<&dyn tt_toplike_bus::WorldRead>,
        backend: &dyn TelemetryBackend,
    ) {
        match world {
            Some(w) => {
                let devices = backend.devices();
                // The bus names a chip by its PCI address, never by position.
                let chips: Vec<(usize, tt_toplike_bus::EntityKey)> = devices
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| !d.bus_id.is_empty())
                    .map(|(slot, d)| (slot, tt_toplike_bus::EntityKey::chip(&d.bus_id)))
                    .collect();
                let now = w.now_mono_ns();
                self.bus_fx =
                    ArcadeFx::from_world(w, &chips, devices.len(), now, &mut self.bus_state);
                self.bus_now = now;
                // The Die 3D and Training panels read the world as their
                // own modes do.
                let secs = self.now_s();
                if let Some(d) = self.die3d.as_mut() {
                    d.update(backend, Some(w), secs);
                    self.die3d_fed = true;
                    self.die3d_bus_now = Some(now);
                }
                if let Some(t) = self.train.as_mut() {
                    t.set_bus(Some(w), backend);
                }
            }
            None => {
                self.bus_fx = ArcadeFx::default();
                self.bus_state = ArcadeBusState::default();
                self.die3d_bus_now = None;
                if let Some(t) = self.train.as_mut() {
                    t.set_bus(None, backend);
                }
            }
        }
    }

    /// Thread the first Ready inference service's serving stats into the duel.
    ///
    /// `Some(stats)` lights up the Row 1 duel strip; `None` hides it and leaves
    /// Row 1 as the plain telemetry strip. Called once per tick from the TUI.
    pub fn set_serving(&mut self, serving: Option<ServingStats>) {
        self.serving = serving;
    }

    /// Hand Arcade the activities running this tick (already in urgency order).
    /// An empty list leaves Arcade exactly as it was.
    pub fn set_activities(&mut self, activities: Vec<crate::animation::Activity>) {
        self.activities = activities;
    }

    /// Rows the lanes take from the top of the starfield region: up to 2, and 0
    /// unless the starfield has at least 6 rows so it stays readable.
    ///
    /// The lanes go in the starfield panel; a layout with no starfield shows
    /// none.
    pub fn lane_rows(&self) -> usize {
        const MIN_STARFIELD_ROWS: usize = 6;
        match self.layout.find(View::Starfield) {
            Some(p) if !self.activities.is_empty() && p.rect.h >= MIN_STARFIELD_ROWS => {
                self.activities.len().min(2)
            }
            _ => 0,
        }
    }

    /// The tint for the most urgent activity: red stalled, violet loading, warm
    /// training, teal serving. `None` when nothing runs.
    fn activity_tint(&self) -> Option<crate::animation::ActivityTint> {
        use crate::animation::{ActivityKind, ActivityTint};
        let a = self.activities.first()?;
        let hue = if a.stalled {
            0.0
        } else {
            match a.kind {
                ActivityKind::Train => 40.0,
                ActivityKind::Load => 275.0,
                ActivityKind::Serve => 175.0,
            }
        };
        Some(ActivityTint { hue, strength: 0.3 })
    }

    /// One flare per new pulse (at most three per activity per tick). Counts are
    /// followed by activity id, so a reorder or an exit moves no baseline. The
    /// first sight of an activity is a baseline, a count that goes down (a new
    /// run, a restarted server) is a reset, and an unavailable count keeps the
    /// last baseline, so a count that returns unchanged adds nothing.
    fn pulse_activities(&mut self) {
        let mut flares = 0;
        for a in &self.activities {
            let Some(now) = a.pulse_seq else {
                continue;
            };
            if let Some(prev) = self.last_pulse.get(&a.id) {
                if now > *prev {
                    flares += (now - *prev).min(3);
                }
            }
            self.last_pulse.insert(a.id.clone(), now);
        }
        let ids: Vec<&str> = self.activities.iter().map(|a| a.id.as_str()).collect();
        self.last_pulse.retain(|id, _| ids.contains(&id.as_str()));
        if let Some(sf) = self.starfield.as_mut() {
            for _ in 0..flares {
                sf.pulse();
            }
        }
    }

    /// Propagate a sensitivity multiplier to all three sub-visualizations.
    ///
    /// Must be called after construction if a non-default sensitivity profile is in use
    /// (e.g. `--profile paranoid`).  Without this call, `ArcadeVisualization` would
    /// silently stay at sensitivity=1.0 while the individual standalone modes
    /// (Starfield, MemoryCastle, MemoryFlow) correctly use the configured value.
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn set_sensitivity(&mut self, s: f32) {
        self.sensitivity = s;
        if let Some(v) = self.starfield.as_mut() {
            v.set_sensitivity(s);
        }
        if let Some(v) = self.memory_castle.as_mut() {
            v.set_sensitivity(s);
        }
        if let Some(v) = self.memory_flow.as_mut() {
            v.set_sensitivity(s);
        }
    }

    /// Initialize from devices (called once after creation)
    pub fn initialize_from_devices(&mut self, devices: &[crate::models::Device]) {
        if let Some(v) = self.starfield.as_mut() {
            v.initialize_from_devices(devices);
        }
    }

    /// Build and install board topology from live SMBUS data.
    ///
    /// Call this once after backend init.  Propagates topology to all
    /// sub-visualizations so the header diagram, stream characters, and
    /// column separators are all topology-aware in the same frame.
    pub fn initialize_topology(&mut self, backend: &dyn TelemetryBackend) {
        use crate::animation::topology::BoardTopology;
        let board_ids: Vec<Option<String>> = backend
            .devices()
            .iter()
            .map(|d| {
                backend
                    .smbus_telemetry(d.index)
                    .and_then(|s| s.board_id.clone())
            })
            .collect();
        let topo = BoardTopology::from_devices_with_ids(backend.devices(), &board_ids);

        // Push topology to sub-visualizations.
        if let Some(v) = self.starfield.as_mut() {
            v.set_topology(topo.clone());
        }
        if let Some(v) = self.memory_castle.as_mut() {
            v.set_topology(topo.clone());
        }
        self.board_topology = Some(topo);
    }

    /// Render a one-line topology diagram for the arcade header.
    ///
    /// **≤ 8 chips** — detailed format:
    /// `[BH0 ██░ 16W 43°C] ←→ [BH1 ██░ 14W 42°C]  ═══  [BH2 ██░ 12W 45°C] …`
    ///
    /// **> 8 chips** — compact mini-bar (one character per chip, color = temp):
    /// `32× BH  [░▒▓█░░░░▒▓█░░▒▓█░░▒▓█▒░░░░▒▓█░▒]`
    ///
    /// Returns `None` when device count < 2 or no topology is available.
    pub fn topology_diagram_line(&self, backend: &dyn TelemetryBackend) -> Option<Line<'static>> {
        let devices = backend.devices();
        if devices.len() < 2 {
            return None;
        }

        let topo = self.board_topology.as_ref()?;
        let has_multi = topo.has_multi_chip_boards();

        // Compact mini-bar path for large chip counts.
        if devices.len() > 8 {
            return Some(self.topology_minibar_line(backend, topo));
        }

        // Detailed path for small chip counts.
        //
        // Show topology structure only — no watts/temp (footer covers those).
        // Layout for multi-chip boards:   [BH0 ←→ BH1]  ═══  [BH2 ←→ BH3]
        // Layout for single-chip cards:   [BH0]  [BH1]  [BH2]  [BH3]
        let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
        let num_boards = topo.boards.len();

        for (b_idx, board) in topo.boards.iter().enumerate() {
            let board_color = hsv_to_rgb(board.hue, 0.85, 0.9);

            spans.push(Span::styled("[", Style::default().fg(board_color)));

            for (c_idx, &chip_idx) in board.chips.iter().enumerate() {
                let device = devices.get(chip_idx);
                let telem = backend.telemetry(chip_idx);
                let temp = telem.map(|t| t.temp_c()).unwrap_or(25.0);

                let arch_label = device.map(|d| d.architecture.abbrev()).unwrap_or("?");
                let chip_color = hsv_to_rgb(temp_to_hue(temp), 0.85, 0.9);
                spans.push(Span::styled(
                    format!("{}{}", arch_label, chip_idx),
                    Style::default().fg(chip_color),
                ));

                if c_idx + 1 < board.chips.len() {
                    spans.push(Span::styled(
                        " ←→ ",
                        Style::default()
                            .fg(board_color)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
            }

            spans.push(Span::styled("]", Style::default().fg(board_color)));

            if b_idx + 1 < num_boards {
                if has_multi {
                    spans.push(Span::styled(
                        "  ═══  ",
                        Style::default()
                            .fg(colors::rgb(200, 160, 60))
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw("  "));
                }
            }
        }

        Some(Line::from(spans))
    }

    /// Compact mini-bar topology line used when chip count > 8.
    ///
    /// Each chip is represented by a single character (power level) coloured
    /// by temperature.  Fits any chip count in a single terminal line.
    fn topology_minibar_line(
        &self,
        backend: &dyn TelemetryBackend,
        topo: &crate::animation::topology::BoardTopology,
    ) -> Line<'static> {
        use crate::animation::common::temp_to_hue;

        let devices = backend.devices();
        let n = devices.len();

        // Architecture summary for the label.
        let bh = devices
            .iter()
            .filter(|d| matches!(d.architecture, crate::models::Architecture::Blackhole))
            .count();
        let wh = devices
            .iter()
            .filter(|d| matches!(d.architecture, crate::models::Architecture::Wormhole))
            .count();
        let gs = devices
            .iter()
            .filter(|d| matches!(d.architecture, crate::models::Architecture::Grayskull))
            .count();
        let arch_str = match (bh, wh, gs) {
            (b, 0, 0) => format!("{}× BH", b),
            (0, w, 0) => format!("{}× WH", w),
            (0, 0, g) => format!("{}× GS", g),
            _ => format!("{} chips", n),
        };

        let mut spans: Vec<Span<'static>> = vec![
            Span::raw("  "),
            Span::styled(
                format!("{}  [", arch_str),
                Style::default()
                    .fg(colors::rgb(180, 180, 200))
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        // One char per chip.  Cap display at 64 to avoid overflowing very wide
        // terminals; chips beyond that get a "+N" suffix.
        const MAX_BAR_CHIPS: usize = 64;
        let bar_n = n.min(MAX_BAR_CHIPS);

        for chip_idx in 0..bar_n {
            let device = &devices[chip_idx];
            let telem = backend.telemetry(device.index);
            let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
            let temp = telem.map(|t| t.temp_c()).unwrap_or(25.0);

            let act = (power / 80.0).clamp(0.0, 1.0);
            let ch = if act > 0.75 {
                '█'
            } else if act > 0.50 {
                '▓'
            } else if act > 0.25 {
                '▒'
            } else {
                '░'
            };

            let hue = temp_to_hue(temp);
            let color = hsv_to_rgb(hue, 0.85, 0.85 + act * 0.15);
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));

            // Faint board-boundary marker for multi-chip boards only.
            // Use device.index (hardware ID), not chip_idx (vector position) —
            // same_board() queries by hardware index, not array slot.
            if topo.has_multi_chip_boards() && chip_idx + 1 < bar_n {
                let next_device = &devices[chip_idx + 1];
                if !topo.same_board(device.index, next_device.index) {
                    spans.push(Span::styled(
                        "|",
                        Style::default().fg(colors::rgb(100, 90, 60)),
                    ));
                }
            }
        }

        spans.push(Span::styled(
            "]",
            Style::default()
                .fg(colors::rgb(180, 180, 200))
                .add_modifier(Modifier::BOLD),
        ));

        if n > MAX_BAR_CHIPS {
            spans.push(Span::styled(
                format!(" +{}", n - MAX_BAR_CHIPS),
                Style::default().fg(colors::rgb(140, 140, 140)),
            ));
        }

        Line::from(spans)
    }

    /// Update every panel, the power baseline and the duel from telemetry.
    pub fn update(&mut self, backend: &dyn TelemetryBackend) {
        self.frame = self.frame.wrapping_add(1);

        // Update sub-visualizations
        let tint = self.activity_tint();
        if let Some(sf) = self.starfield.as_mut() {
            sf.update_from_telemetry(backend);
            sf.set_activity_tint(tint);
        }
        self.pulse_activities();
        if let Some(v) = self.memory_castle.as_mut() {
            v.update(backend);
        }
        if let Some(v) = self.memory_flow.as_mut() {
            v.update(backend);
        }
        if let Some(v) = self.defrag.as_mut() {
            v.update(backend);
        }
        self.update_new_panels(backend);

        // Feed the first device's power baseline. The duel's hero lunges on
        // a spike against it (`hero_lunge` below).
        if let Some(device) = backend.devices().first() {
            if let Some(telemetry) = backend.telemetry(device.index) {
                let smbus = backend.smbus_telemetry(device.index);

                // Prefer SMBUS TDP/TDC (covers all power domains) over sysfs when available.
                let power = smbus
                    .and_then(|s| s.tdp_watts())
                    .unwrap_or_else(|| telemetry.power.unwrap_or(0.0));
                let current = smbus
                    .and_then(|s| s.tdc_amperes())
                    .unwrap_or_else(|| telemetry.current.unwrap_or(0.0));

                self.baseline.update(
                    device.index,
                    power,
                    current,
                    telemetry.asic_temperature.unwrap_or(25.0),
                    telemetry.aiclk.unwrap_or(1000) as f32,
                );
            }
        }

        // ── Duel signals ──────────────────────────────────────────────────
        // Every offset below traces back to live telemetry — no scripted motion.
        //
        // hw = mean over devices of power / running-peak. We take the same
        // SMBUS-preferred power reading the baseline uses, update the running peak
        // (floored at 1.0), then normalize so the duel is sensitive on any card.
        let mut powers: Vec<f32> = Vec::new();
        for device in backend.devices() {
            if let Some(telemetry) = backend.telemetry(device.index) {
                let smbus = backend.smbus_telemetry(device.index);
                let power = smbus
                    .and_then(|s| s.tdp_watts())
                    .unwrap_or_else(|| telemetry.power.unwrap_or(0.0));
                self.power_peak = self.power_peak.max(power);
                powers.push(power);
            }
        }
        self.power_peak = self.power_peak.max(1.0);
        let hw = if powers.is_empty() {
            0.0
        } else {
            powers.iter().map(|p| p / self.power_peak).sum::<f32>() / powers.len() as f32
        };

        // hero_lunge fires only on a real power spike relative to the learned
        // baseline (fed above for the first device). >15% deviation once the
        // baseline is established.
        let hero_lunge = match (backend.devices().first(), powers.first()) {
            (Some(device), Some(&p0)) => {
                self.baseline.is_established()
                    && self.baseline.power_change(device.index, p0) > 0.15
            }
            _ => false,
        };

        // serve blends token throughput (weighted 0.7) with queue depth (0.3);
        // snake_lunge fires when a request actually completed this tick; kv is
        // the KV-cache usage that colors the snake's heat.
        // Requests the bus saw complete since the last update also make the
        // snake lunge: an attached host's stats carry no completion delta.
        let bus_requests = std::mem::take(&mut self.bus_fx.new_requests);
        let (serve, snake_lunge, kv) = match self.serving {
            Some(s) => {
                let tps = (s.generation_tps / 200.0).min(1.0);
                let load = ((s.requests_running + s.requests_waiting) as f32 / 8.0).min(1.0);
                (
                    0.7 * tps + 0.3 * load,
                    s.completed_delta > 0 || bus_requests > 0,
                    s.kv_cache_usage,
                )
            }
            None => (0.0, false, 0.0),
        };

        self.duel.update(hw, serve, hero_lunge, snake_lunge, kv);
    }

    /// Render the complete Arcade visualization
    pub fn render(&self, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.height);

        // Header: exactly 2 rows — title bar + Row 1.
        //
        // Row 1 carries the single shared telemetry strip (the embedded sections
        // no longer print their own W/°C).  The topology diagram shares the row
        // when both fit; otherwise the strip wins.
        // On a terminal narrower than either, it is cut at the edge, so no
        // row of any layout runs past the terminal. (Only rows that are too
        // wide change, which is below about 30 columns.)
        lines.push(fit_width(self.render_header(backend), self.width));
        lines.push(fit_width(self.render_row1(backend), self.width));

        // Each configured row: its rule, then its panels side by side.
        for row in &self.layout.rows {
            lines.push(self.render_separator(row.label(), row.height));
            self.compose_row(row, backend, &mut lines);
        }

        // The bus effects (see `arcade_bus`), under the lanes. With no bus
        // this does nothing.
        self.draw_bus(&mut lines);

        // Lay the activity lanes over the first starfield rows last, so
        // nothing covers them. With no activities `lane_rows()` is 0 and this
        // does nothing.
        let n = self.lane_rows();
        if let (true, Some(p)) = (n > 0, self.layout.find(View::Starfield)) {
            let r = p.rect;
            let lanes = crate::animation::render_lanes(&self.activities, r.w);
            for (i, lane) in lanes.into_iter().enumerate() {
                if let Some(slot) = lines.get_mut(r.y + i) {
                    if r.x == 0 && r.w == self.width {
                        *slot = lane;
                    } else {
                        put_spans(slot, r.x, r.w, lane.spans);
                    }
                }
            }
        }
        // No row ends in a right-side border, at any width.
        for line in lines.iter_mut() {
            clear_right_edge(line, self.width);
        }
        lines
    }

    /// Update the Die 3D, Training and inference panels, when shown.
    fn update_new_panels(&mut self, backend: &dyn TelemetryBackend) {
        let secs = self.now_s();
        if let Some(d) = self.die3d.as_mut() {
            d.set_theme(colors::current_theme());
            // `set_bus` updated it with the world this tick; without a bus it
            // is updated here, with none.
            if !self.die3d_fed {
                d.update(backend, None, secs);
            }
        }
        self.die3d_fed = false;
        if let Some(t) = self.train.as_mut() {
            t.update();
        }
        if let (Some(snake), Some(p)) = (self.snake.as_mut(), self.layout.find(View::Inference)) {
            let chips: Vec<crate::animation::ChipReading> = backend
                .devices()
                .iter()
                .map(|d| {
                    let t = backend.telemetry(d.index);
                    crate::animation::ChipReading {
                        index: d.index,
                        arch: d.architecture.name(),
                        power_w: t.and_then(|t| t.power),
                        temp_c: t.and_then(|t| t.asic_temperature),
                        aiclk_mhz: t.and_then(|t| t.aiclk),
                    }
                })
                .collect();
            let arch = backend
                .devices()
                .first()
                .map(|d| d.architecture.name())
                .unwrap_or("Unknown");
            snake.update(&crate::animation::SnakeWorld {
                rows: &self.services,
                // The catalog only feeds the cold starfield, which the panel
                // never shows: with no server it shows its note instead.
                catalog: &[],
                arch,
                chips: &chips,
                cadence_secs: crate::workload::inference_server::CADENCE_SECS,
                width: p.rect.w,
                height: p.rect.h,
            });
        }
    }

    /// Whether an inference service is up or coming up: the inference panel
    /// draws the serving snake then, and its note otherwise.
    fn has_server(&self) -> bool {
        use crate::workload::inference_server::Phase;
        self.services.iter().any(|s| s.phase != Phase::Down)
    }

    /// The lines of panel `p`, at most its height, each at most its width.
    fn panel_lines(&self, p: &Panel, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let note = |text: &str| {
            vec![Line::from(Span::styled(
                format!(" {text}"),
                Style::default().fg(colors::text_secondary()),
            ))]
        };
        let (w, h) = (p.rect.w, p.rect.h);
        match p.view {
            View::Starfield => self.starfield.as_ref().map(|v| v.render()),
            View::Castle => self.memory_castle.as_ref().map(|v| v.render(backend)),
            View::Flow => self.memory_flow.as_ref().map(|v| v.render(backend)),
            View::Defrag => self.defrag.as_ref().map(|v| v.render_embedded(backend)),
            View::Die3d => self
                .die3d
                .as_ref()
                .map(|v| v.render_at(self.die3d_bus_now, self.now_s())),
            View::Training => self.train.as_ref().map(|t| {
                let st = self.train_state.as_ref().unwrap_or(&self.idle_train);
                if st.proc.is_none() && !t.shows_bus_run() {
                    note("no training run")
                } else {
                    t.render(st, backend)
                }
            }),
            View::Inference => self.snake.as_ref().map(|s| {
                if self.has_server() {
                    s.render(w, h)
                } else {
                    note("no inference server")
                }
            }),
            View::Saga => self.saga.as_ref().map(|s| {
                saga_view::render_saga_panel(
                    &s.feed,
                    &s.memory,
                    (
                        w.min(u16::MAX as usize) as u16,
                        h.min(u16::MAX as usize) as u16,
                    ),
                    u64::from(self.frame),
                )
            }),
        }
        .unwrap_or_default()
    }

    /// Push the content rows of `row` onto `lines`: its panels side by side,
    /// a `│` between two of them. A row holding one full-width panel pushes
    /// that panel's lines as they are (clamped to the width), as the arcade
    /// always did for the starfield and the flow.
    fn compose_row(
        &self,
        row: &arcade_layout::LaidRow,
        backend: &dyn TelemetryBackend,
        lines: &mut Vec<Line<'static>>,
    ) {
        if row.height == 0 {
            return;
        }
        let rendered: Vec<Vec<Line<'static>>> = row
            .panels
            .iter()
            .map(|p| self.panel_lines(p, backend))
            .collect();
        if let [p] = row.panels.as_slice() {
            if p.rect.x == 0 && p.rect.w == self.width {
                // The bottom row's panel pushes the lines it returns, as
                // many as it returns, as the arcade always let the flow do:
                // the flow adds a legend line in the footer row (which the
                // status bar covers) and at some sizes draws fewer or more
                // rows than it was given. ratatui clips anything below the
                // terminal. Every other row gets exactly its height, so the
                // rows below it stay where the layout put them.
                let is_last = self
                    .layout
                    .rows
                    .last()
                    .is_some_and(|r| std::ptr::eq(r, row));
                let n = if is_last {
                    rendered[0].len()
                } else {
                    row.height
                };
                for r in 0..n {
                    let line = rendered[0].get(r).cloned().unwrap_or_default();
                    let spans = clamp_spans_to_width(line.spans.clone(), p.rect.w);
                    lines.push(Line { spans, ..line });
                }
                return;
            }
        }
        let div = Span::styled("│", Style::default().fg(colors::rgb(50, 70, 90)));
        let last = row.panels.len().saturating_sub(1);
        for r in 0..row.height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut col = 0usize;
            for (i, p) in row.panels.iter().enumerate() {
                if i > 0 {
                    // The divider is the column before the panel.
                    let at = p.rect.x.saturating_sub(1);
                    if col < at {
                        spans.push(Span::raw(" ".repeat(at - col)));
                    }
                    spans.push(div.clone());
                    col = at + 1;
                }
                let line = rendered[i]
                    .get(r)
                    .cloned()
                    .unwrap_or_else(|| Line::from(""));
                let part = clamp_spans_to_width(line.spans, p.rect.w);
                let used: usize = part
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                spans.extend(part);
                col += used;
                // Pad every panel but the last to its width, so the next
                // divider lands in its column.
                if i < last && used < p.rect.w {
                    spans.push(Span::raw(" ".repeat(p.rect.w - used)));
                    col += p.rect.w - used;
                }
            }
            lines.push(Line::from(spans));
        }
    }

    /// Draw the bus effects at the bus time of the last `set_bus`.
    fn draw_bus(&self, lines: &mut [Line<'static>]) {
        if self.bus_fx.is_empty() {
            return;
        }
        // The gauges and labels go on the rule of the gauge panel (see
        // `Layout::gauge_panel`), across its columns. Particles rise in the
        // starfield panel below the lanes; with no starfield they are not
        // drawn. With the default layout this is the starfield's rule and
        // rows across the whole width, as before.
        let lanes = self.lane_rows();
        let gauge = self.layout.gauge_panel().and_then(|p| {
            let row = self.layout.row_of(p)?;
            Some((row.rule_y, p.rect.cols(), row.label()))
        });
        let (gauge_row, gauge_cols, label_cols) = match &gauge {
            Some((y, cols, label)) => (
                Some(*y),
                cols.clone(),
                UnicodeWidthStr::width("╔══[ ")
                    + UnicodeWidthStr::width(*label)
                    + UnicodeWidthStr::width(" ]"),
            ),
            None => (None, 0..0, 0),
        };
        let (sky, sky_cols) = match self.layout.find(View::Starfield) {
            Some(p) => ((p.rect.y + lanes)..(p.rect.y + p.rect.h), p.rect.cols()),
            None => (0..0, 0..0),
        };
        let stage = arcade_bus::Stage {
            width: self.width,
            gauge_row,
            gauge_cols,
            label_cols,
            sky,
            sky_cols,
        };
        arcade_bus::draw(&self.bus_fx, self.bus_now, &stage, lines);
    }

    /// Build the single shared per-device telemetry strip.
    ///
    /// Format: `BH0 92W 78°C 1.35GHz · BH1 …`.  Fields a device does not
    /// report are skipped.  Character-safe: when the devices don't fit in
    /// `width` display columns the string is truncated on a character boundary
    /// and a trailing `…` appended.  Returns an empty string when there are no
    /// devices.  This is the ONE place Arcade prints per-device W/°C — the
    /// embedded sections render with chrome off.
    fn telemetry_strip(backend: &dyn TelemetryBackend, width: usize) -> String {
        const SEP: &str = " · ";
        let mut entries: Vec<String> = Vec::new();
        for device in backend.devices() {
            let telem = backend.telemetry(device.index);
            // `short_label()` ("BH0") always present for orientation; each
            // numeric field is added only when the device actually reports
            // it. The label is the device's own index, not its position in
            // the strip: those differ whenever the index set is sparse (a
            // salvaged tt-smi snapshot that dropped a malformed
            // `device_info[]` entry, or a chip whose ARC never answered),
            // and the label must name the card whose watts follow it.
            let mut parts: Vec<String> = vec![device.short_label()];
            if let Some(t) = telem {
                if let Some(p) = t.power {
                    parts.push(format!("{:.0}W", p));
                }
                if let Some(temp) = t.asic_temperature {
                    parts.push(format!("{:.0}°C", temp));
                }
                if let Some(clk) = t.aiclk {
                    parts.push(format!("{:.2}GHz", clk as f32 / 1000.0));
                }
            }
            entries.push(parts.join(" "));
        }

        let full = entries.join(SEP);
        if UnicodeWidthStr::width(full.as_str()) <= width {
            return full;
        }
        if width == 0 {
            return String::new();
        }

        // Reserve one column for the ellipsis; walk chars by display width so
        // wide glyphs (°, …) never split and the result never exceeds `width`.
        let budget = width.saturating_sub(1);
        let mut out = String::new();
        let mut used = 0usize;
        for ch in full.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            if used + cw > budget {
                break;
            }
            used += cw;
            out.push(ch);
        }
        out.push('…');
        out
    }

    /// Color the duel strip's glyphs into themed spans.
    ///
    /// - hero `@` → `colors::info()` (the silicon's cool accent), bold
    /// - marker `⚔` → `colors::text_primary()`, bold
    /// - snake glyphs (`▶ ● ◉ ~`) → heat color by KV-cache usage via
    ///   `hsv_to_rgb`, so a busy cache runs the snake toward hot pink (the
    ///   grayskull theme maps the hue automatically)
    /// - blanks stay uncolored
    fn duel_strip_spans(&self, strip: &str) -> Vec<Span<'static>> {
        // KV 0 → cyan (200°), KV 1 → hot pink (330°): a live cache heats up.
        let snake_hue = 200.0 + self.duel.kv.clamp(0.0, 1.0) * 130.0;
        let snake_color = hsv_to_rgb(snake_hue, 0.85, 0.9);

        let hero_style = Style::default()
            .fg(colors::info())
            .add_modifier(Modifier::BOLD);
        let marker_style = Style::default()
            .fg(colors::text_primary())
            .add_modifier(Modifier::BOLD);
        let snake_style = Style::default().fg(snake_color);

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(strip.chars().count());
        for ch in strip.chars() {
            let style = match ch {
                '@' => hero_style,
                '⚔' => marker_style,
                '▶' | '●' | '◉' | '~' => snake_style,
                _ => Style::default(),
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        spans
    }

    /// Color the plain telemetry strip into themed spans (per-device chunks in
    /// the primary text color, `·` separators dimmed).
    fn strip_spans(strip: &str) -> Vec<Span<'static>> {
        const SEP: &str = " · ";
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, entry) in strip.split(SEP).enumerate() {
            if i > 0 {
                spans.push(Span::styled(
                    SEP.to_string(),
                    Style::default().fg(colors::text_secondary()),
                ));
            }
            spans.push(Span::styled(
                entry.to_string(),
                Style::default().fg(colors::text_primary()),
            ));
        }
        spans
    }

    /// Render Arcade's Row 1: the shared telemetry strip, with the topology
    /// diagram to its right when both fit the row (else the strip wins).  Falls
    /// back to the topology line (or a blank row) when there are no devices.
    fn render_row1(&self, backend: &dyn TelemetryBackend) -> Line<'static> {
        // When something is serving, Row 1 becomes the duel: the strip claims
        // the left ~half and the shared telemetry strip fills the remainder.
        if self.serving.is_some() {
            // Keep the duel a tight, readable strip. Left unbounded (width/2) it
            // stretches the hero and snake to opposite ends of half a huge
            // terminal, scattering the tug-of-war across dozens of empty cells;
            // capping it keeps the battle compact on any size, with the shared
            // telemetry strip filling whatever remains.
            const DUEL_MAX_W: usize = 44;
            let duel_w = (self.width / 2).min(DUEL_MAX_W);
            let strip_w = self.width.saturating_sub(duel_w);
            let duel = self.duel.render_strip(duel_w);
            let mut spans = self.duel_strip_spans(&duel);
            let strip = Self::telemetry_strip(backend, strip_w);
            spans.extend(Self::strip_spans(&strip));
            return Line::from(spans);
        }

        let strip = Self::telemetry_strip(backend, self.width);
        let topo = self.topology_diagram_line(backend);

        if strip.is_empty() {
            // Nothing to summarize — preserve the prior behavior.
            return topo.unwrap_or_else(|| Line::from(""));
        }

        let strip_w = UnicodeWidthStr::width(strip.as_str());
        let strip_spans = Self::strip_spans(&strip);

        if let Some(topo_line) = topo {
            let topo_w: usize = topo_line
                .spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            const GAP: usize = 2;
            if strip_w + GAP + topo_w <= self.width {
                let mut spans = strip_spans;
                spans.push(Span::raw("  "));
                spans.extend(topo_line.spans);
                return Line::from(spans);
            }
        }

        // Strip wins — topology dropped this frame.
        Line::from(strip_spans)
    }

    /// Render header for Arcade mode
    fn render_header(&self, backend: &dyn TelemetryBackend) -> Line<'static> {
        let device_count = backend.devices().len();
        let label = "  🎮 ARCADE MODE ";
        // "🎮" is 2 display cols wide; UnicodeWidthStr measures correctly.
        let label_w = UnicodeWidthStr::width(label);
        let device_text = format!(
            " {} Device{} ",
            device_count,
            if device_count == 1 { "" } else { "s" }
        );
        let device_w = label_w + 1 + device_text.len() + 1; // +1 for each │
        let hint = " Press 'v' to cycle modes ";
        let has_hint = self.width > device_w + hint.len();

        let mut spans = vec![
            Span::styled(
                label,
                Style::default()
                    .fg(colors::rgb(220, 240, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("│", Style::default().fg(colors::rgb(150, 120, 180))),
            Span::styled(device_text, Style::default().fg(colors::rgb(80, 220, 200))),
        ];
        if has_hint {
            spans.push(Span::styled(
                "│",
                Style::default().fg(colors::rgb(150, 120, 180)),
            ));
            spans.push(Span::styled(
                hint,
                Style::default().fg(colors::rgb(160, 160, 160)),
            ));
        }
        Line::from(spans)
    }

    /// Render region separator
    ///
    /// BBS-chrome rule (`bbs_rule`): `╔══[ LABEL ]══════▓▒░` — the boxed
    /// label keeps the bright "active section" color, the rule/box-drawing
    /// portion keeps the animated hue-cycling color, matching the previous
    /// dash-separator's palette split.
    fn render_separator(&self, label: &str, _region_height: usize) -> Line<'static> {
        // Animated color cycling for separator
        let hue = (self.frame as f32 * 2.0) % 360.0;
        let separator_color = hsv_to_rgb(hue, 0.6, 0.8);
        let label_color = Style::default()
            .fg(colors::rgb(220, 240, 255))
            .add_modifier(Modifier::BOLD);
        let rule_color = Style::default()
            .fg(separator_color)
            .add_modifier(Modifier::BOLD);

        let rule = bbs_rule(label, self.width);
        // Split the boxed label back out so it can keep its distinct color;
        // if the label got char-truncated (narrow width) it won't be found
        // verbatim, so fall back to coloring the whole rule uniformly.
        match rule.find(label) {
            Some(idx) => Line::from(vec![
                Span::styled(rule[..idx].to_string(), rule_color),
                Span::styled(label.to_string(), label_color),
                Span::styled(rule[idx + label.len()..].to_string(), rule_color),
            ]),
            None => Line::from(vec![Span::styled(rule, rule_color)]),
        }
    }

    /// Replace the character at display column `col` in `lines[row]` without
    /// disturbing the rest of the row's spans.  Works span-by-span so the
    /// surrounding colors (defrag blocks, castle particles, etc.) are untouched.
    pub(crate) fn splice_char(
        lines: &mut [Line<'static>],
        row: usize,
        col: usize,
        ch: char,
        style: Style,
    ) {
        if row >= lines.len() {
            return;
        }

        let mut new_spans: Vec<Span<'static>> = Vec::new();
        let mut cursor = 0usize; // display column of the start of the current span
        let mut spliced = false;

        for span in std::mem::take(&mut lines[row].spans) {
            let text: String = span.content.as_ref().to_string();
            let span_cols = unicode_width::UnicodeWidthStr::width(text.as_str());
            let span_end = cursor + span_cols;

            if spliced || col < cursor || col >= span_end {
                // The column is outside this span — keep as-is.
                new_spans.push(span);
            } else {
                // The column falls inside this span.  Walk chars by display width
                // so wide glyphs (emoji, CJK) are accounted for correctly.
                let target_col = col; // display column we want to replace
                let mut char_col = cursor; // running display-column counter
                let mut before = String::new();
                let mut after_start = false;
                let mut after = String::new();
                let mut replaced = false;
                for c in text.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
                    if after_start {
                        after.push(c);
                    } else if char_col == target_col {
                        // Skip this char (and any extra cols it occupies) — `ch` replaces it.
                        after_start = true;
                        replaced = true;
                    } else if char_col + cw > target_col {
                        // Wide glyph straddles target col — treat as replaced.
                        after_start = true;
                        replaced = true;
                    } else {
                        before.push(c);
                    }
                    char_col += cw;
                }
                if !replaced {
                    // col was in range but walk didn't land exactly — keep span.
                    new_spans.push(span);
                } else {
                    if !before.is_empty() {
                        new_spans.push(Span::styled(before, span.style));
                    }
                    new_spans.push(Span::styled(ch.to_string(), style));
                    if !after.is_empty() {
                        new_spans.push(Span::styled(after, span.style));
                    }
                    spliced = true;
                }
            }
            cursor = span_end;
        }

        // If col was past the end of all spans, pad and append.
        if !spliced {
            if cursor < col {
                new_spans.push(Span::raw(" ".repeat(col - cursor)));
            }
            new_spans.push(Span::styled(ch.to_string(), style));
        }

        lines[row].spans = new_spans;
    }
}

/// `line` cut to `w` display columns when it is wider; unchanged otherwise.
fn fit_width(mut line: Line<'static>, w: usize) -> Line<'static> {
    let cols: usize = line
        .spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    if cols > w {
        line.spans = clamp_spans_to_width(std::mem::take(&mut line.spans), w);
    }
    line
}

/// Glyphs that draw a right-side border when they sit in a row's last
/// column. The user's rule is no right-side border characters, at any width.
///
/// The list is every box-drawing character with a vertical stroke and no
/// stroke to the right (verticals, dashed verticals, half verticals, right
/// corners and right-edge joints, in light, heavy, double and rounded
/// forms), the two right-hand block pieces `▐` and `▕`, and the ASCII bar
/// `|`. The arcade's guard ([`clear_right_edge`]), the frame sweeps in
/// `arcade_preset_frame_tests`, the panel test and the docs-sketch test all
/// read this one list.
///
/// In order: the light, heavy, double and dashed verticals; the half and
/// mixed verticals; the top-right corners; the bottom-right corners; the
/// right-edge joints; the right-hand block pieces; the ASCII bar.
#[rustfmt::skip]
pub(crate) const RIGHT_EDGE_GLYPHS: &[char] = &[
    '│', '┃', '║', '╎', '╏', '┆', '┇', '┊', '┋',
    '╵', '╷', '╹', '╻', '╽', '╿',
    '┐', '┑', '┒', '┓', '╕', '╖', '╗', '╮',
    '┘', '┙', '┚', '┛', '╛', '╜', '╝', '╯',
    '┤', '┥', '┦', '┧', '┨', '┩', '┪', '┫', '╡', '╢', '╣',
    '▐', '▕',
    '|',
];

/// Put a space for the glyph in column `w - 1` of `line` when it is one of
/// [`RIGHT_EDGE_GLYPHS`]: a panel divider, a field separator, a panel's
/// wall, a stage card's right corner or a scene's scaffold beam that has
/// landed in the terminal's last column. Nothing else changes: the space
/// keeps the glyph's style, and no other column is touched.
fn clear_right_edge(line: &mut Line<'static>, w: usize) {
    if w == 0 {
        return;
    }
    let mut col = 0usize;
    for span in line.spans.iter_mut() {
        let mut out = String::with_capacity(span.content.len());
        let mut changed = false;
        for ch in span.content.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if col == w - 1 && cw == 1 && RIGHT_EDGE_GLYPHS.contains(&ch) {
                out.push(' ');
                changed = true;
            } else {
                out.push(ch);
            }
            col += cw;
        }
        if changed {
            span.content = out.into();
            return;
        }
        if col >= w {
            return;
        }
    }
}

/// Write `spans` over the columns `x .. x + w` of `line`, keeping what is
/// left and right of them. The spans are clamped to `w` and padded to it.
fn put_spans(line: &mut Line<'static>, x: usize, w: usize, spans: Vec<Span<'static>>) {
    let old = std::mem::take(&mut line.spans);
    let mut out = clamp_spans_to_width(old.clone(), x);
    let left: usize = out
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    if left < x {
        out.push(Span::raw(" ".repeat(x - left)));
    }
    let mid = clamp_spans_to_width(spans, w);
    let used: usize = mid
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    out.extend(mid);
    if used < w {
        out.push(Span::raw(" ".repeat(w - used)));
    }
    out.extend(skip_cols(old, x + w));
    line.spans = out;
}

/// The spans after the first `n` display columns. A wide character cut by
/// the edge becomes a space.
fn skip_cols(spans: Vec<Span<'static>>, n: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut col = 0usize;
    for span in spans {
        let w = UnicodeWidthStr::width(span.content.as_ref());
        if col >= n {
            out.push(span);
        } else if col + w > n {
            let mut s = String::new();
            let mut c = col;
            for ch in span.content.chars() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                if c >= n {
                    s.push(ch);
                } else if c + cw > n {
                    s.push(' ');
                }
                c += cw;
            }
            out.push(Span::styled(s, span.style));
        }
        col += w;
    }
    out
}

/// Truncate a span list so its total display width does not exceed `max_cols`.
/// Spans that fit entirely are kept unchanged; the last span that crosses the
/// boundary is split at the column limit (character-granular, not byte).
fn clamp_spans_to_width(spans: Vec<Span<'static>>, max_cols: usize) -> Vec<Span<'static>> {
    let mut out = Vec::with_capacity(spans.len());
    let mut remaining = max_cols;
    for span in spans {
        if remaining == 0 {
            break;
        }
        let w = UnicodeWidthStr::width(span.content.as_ref());
        if w <= remaining {
            remaining -= w;
            out.push(span);
        } else {
            // Span straddles the boundary — take only as many chars as fit.
            let style = span.style;
            let mut col = 0;
            let mut s = String::new();
            for ch in span.content.chars() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                if col + cw > remaining {
                    break;
                }
                col += cw;
                s.push(ch);
            }
            if !s.is_empty() {
                out.push(Span::styled(s, style));
            }
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::host::HostBackend;
    use crate::backend::TelemetryBackend;
    use ratatui::style::Style;
    use ratatui::text::Line;

    /// Regression for the reported `--host` arcade crash: Arcade embeds Memory
    /// Castle / Memory Flow, whose particle spawn used `frame % memory_channels`.
    /// The Host backend's `Architecture::Unknown` device reports 0 channels, so
    /// `ArcadeVisualization::update` panicked with a divide-by-zero. It must not.
    #[test]
    fn update_survives_zero_channel_host_device() {
        let mut backend = HostBackend::new();
        backend.init().expect("host backend init");
        backend.update().expect("host backend update");

        let mut arcade = ArcadeVisualization::new(120, 40);
        for _ in 0..10 {
            arcade.update(&backend);
        }
    }

    fn plain_line(s: &str) -> Line<'static> {
        Line::from(s.to_string())
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn splice_char_replaces_middle() {
        let mut lines = vec![plain_line("hello")];
        ArcadeVisualization::splice_char(&mut lines, 0, 2, '@', Style::default());
        assert_eq!(line_text(&lines[0]), "he@lo");
    }

    #[test]
    fn splice_char_pads_short_line() {
        let mut lines = vec![plain_line("hi")];
        // col is beyond the current line length — must not panic, must pad
        ArcadeVisualization::splice_char(&mut lines, 0, 5, '@', Style::default());
        let text = line_text(&lines[0]);
        assert_eq!(text.chars().nth(5), Some('@'));
    }

    #[test]
    fn splice_char_ignores_oob_row() {
        let mut lines = vec![plain_line("hello")];
        // Should be a no-op, not a panic
        ArcadeVisualization::splice_char(&mut lines, 99, 0, '@', Style::default());
        assert_eq!(line_text(&lines[0]), "hello");
    }

    #[test]
    fn region_boundaries_normal_height() {
        // For a reasonably tall terminal the boundaries must satisfy the
        // ordering invariant: start < end, regions don't overlap, flow ends
        // before the total line count.
        let vis = ArcadeVisualization::new(80, 40);
        assert!(vis.region(0).start < vis.region(0).end);
        assert!(vis.region(0).end <= vis.region(1).start);
        assert!(vis.region(1).start < vis.region(1).end);
        assert!(vis.region(1).end <= vis.region(2).start);
        assert!(vis.region(2).start < vis.region(2).end);
    }

    #[test]
    fn region_boundaries_tiny_height() {
        // At very small heights regions may collapse to zero size but must
        // not violate start <= end or produce overlapping ranges.
        let vis = ArcadeVisualization::new(40, 6);
        assert!(vis.region(0).start <= vis.region(0).end);
        assert!(vis.region(0).end <= vis.region(1).start);
        assert!(vis.region(1).start <= vis.region(1).end);
        assert!(vis.region(1).end <= vis.region(2).start);
        assert!(vis.region(2).start <= vis.region(2).end);
    }

    #[test]
    fn mid_sep_only_for_height_ge_2() {
        // starfield_height == 0: no mid separator → castle_start = starfield_end + 1
        let vis0 = ArcadeVisualization::new(80, 4); // content_height ≈ 0
                                                    // starfield_height == 1 (content_height ≈ 2): no mid separator
        let vis1 = ArcadeVisualization::new(80, 10);
        // Both must satisfy the no-overlap invariant
        assert!(vis0.region(0).end <= vis0.region(1).start);
        assert!(vis1.region(0).end <= vis1.region(1).start);
    }

    /// Regression for the duplicate-telemetry bug: the embedded starfield /
    /// castle / flow used to each print their own per-device W/°C header, so a
    /// single Arcade frame showed each device's readout ~3×.  Now the embedded
    /// sections render with chrome off and Arcade prints ONE shared strip, so
    /// each device's `°C` appears at most once across the whole render.
    #[test]
    fn arcade_prints_each_device_telemetry_once() {
        let mut backend = crate::backend::mock::MockBackend::new(2);
        backend.init().unwrap();
        backend.update().unwrap();
        let mut a = ArcadeVisualization::new(120, 40);
        a.update(&backend);
        let text: String = a
            .render(&backend)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        // Device 0's readout appears exactly once (the shared strip), not once
        // per embedded section (was 3x).
        let w_readouts = text.matches("°C").count();
        assert!(
            w_readouts <= backend.devices().len(),
            "expected ≤1 °C readout per device in arcade, got {w_readouts}"
        );
    }

    /// The duel strip appears on Row 1 only when a service is serving. With
    /// `set_serving(Some(..))` the render must contain the `⚔` balance marker;
    /// with `None` it must not (Row 1 stays the plain telemetry strip).
    #[test]
    fn duel_strip_shows_only_when_serving() {
        use crate::workload::inference_server::{ServingStats, VllmCounters};

        let mut backend = crate::backend::mock::MockBackend::new(2);
        backend.init().unwrap();
        backend.update().unwrap();

        let render_text = |a: &ArcadeVisualization| -> String {
            a.render(&backend)
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect()
        };

        // Serving → duel strip present.
        let stats = ServingStats::fold(None, &VllmCounters::default(), 5);
        let mut serving = ArcadeVisualization::new(120, 40);
        serving.set_serving(Some(stats));
        serving.update(&backend);
        assert!(
            render_text(&serving).contains('⚔'),
            "serving arcade must render the duel marker"
        );

        // Not serving → no duel strip.
        let mut idle = ArcadeVisualization::new(120, 40);
        idle.set_serving(None);
        idle.update(&backend);
        assert!(
            !render_text(&idle).contains('⚔'),
            "non-serving arcade must not render the duel marker"
        );
    }

    /// Guard the standalone path: with chrome at its default (`true`), a
    /// standalone Memory Castle must still print its per-device W/°C header —
    /// the dedup only applies to the embedded (chrome off) instances.
    #[test]
    fn standalone_castle_keeps_telemetry_header() {
        let mut backend = crate::backend::mock::MockBackend::new(2);
        backend.init().unwrap();
        backend.update().unwrap();
        let mut castle = MemoryCastle::new(120, 20);
        castle.update(&backend);
        let text: String = castle
            .render(&backend)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            text.contains("°C"),
            "standalone MemoryCastle (chrome default) must still show °C"
        );
    }

    fn lane_act(label: &str) -> crate::animation::Activity {
        crate::animation::Activity {
            id: format!("train:{label}"),
            kind: crate::animation::ActivityKind::Train,
            label: label.into(),
            stalled: false,
            fields: vec!["step 1/10 █░░░░░░░ 10%".into()],
            pulse_seq: Some(0),
            key_hint: 't',
        }
    }

    fn rows_of(a: &ArcadeVisualization, backend: &dyn TelemetryBackend) -> Vec<String> {
        a.render(backend)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect()
    }

    fn mock_backend() -> crate::backend::mock::MockBackend {
        let mut b = crate::backend::mock::MockBackend::new(2);
        b.init().unwrap();
        b.update().unwrap();
        b
    }

    /// With no activities nothing changes: no lanes are reserved and no lane tag
    /// appears. (Arcade's render uses randomness, so this checks the structure
    /// rather than comparing bytes with a stored frame.)
    #[test]
    fn no_activities_means_no_lanes() {
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        a.update(&backend);
        assert_eq!(a.lane_rows(), 0);
        let text = rows_of(&a, &backend).join("\n");
        assert!(!text.contains("TRAIN") && !text.contains('▎'), "{text}");
    }

    #[test]
    fn activities_replace_the_first_starfield_rows_and_keep_the_row_count() {
        let backend = mock_backend();
        let mut idle = ArcadeVisualization::new(120, 40);
        idle.update(&backend);
        let idle_rows = rows_of(&idle, &backend).len();

        let mut a = ArcadeVisualization::new(120, 40);
        a.set_activities(vec![lane_act("nano_gpt"), lane_act("second")]);
        a.update(&backend);
        assert_eq!(a.lane_rows(), 2);
        let rows = rows_of(&a, &backend);
        assert_eq!(rows.len(), idle_rows, "the render must keep its height");
        // Header, Row 1, separator, then the lanes.
        assert!(rows[3].contains("nano_gpt"), "{:?}", &rows[..6]);
        assert!(rows[4].contains("second"), "{:?}", &rows[..6]);
        // No lane row ends in a right-side border glyph.
        for r in &rows[3..5] {
            assert!(
                !"│┐┘║▕".contains(r.trim_end().chars().last().unwrap_or(' ')),
                "{r:?}"
            );
        }
    }

    #[test]
    fn a_short_starfield_gets_no_lanes() {
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 12); // starfield region < 6 rows
        assert!((a.region(0).end - a.region(0).start) < 6);
        a.set_activities(vec![lane_act("nano_gpt")]);
        a.update(&backend);
        assert_eq!(a.lane_rows(), 0);
        assert!(!rows_of(&a, &backend).join("\n").contains("nano_gpt"));
    }

    #[test]
    fn a_new_pulse_flares_the_starfield_but_a_drop_does_not() {
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        let mut act = lane_act("nano_gpt");
        act.pulse_seq = Some(10);
        a.set_activities(vec![act.clone()]);
        a.update(&backend);
        assert_eq!(
            a.starfield.as_ref().unwrap().live_flares(),
            0,
            "the first sight of a count is a baseline"
        );
        act.pulse_seq = Some(12);
        a.set_activities(vec![act.clone()]);
        a.update(&backend);
        assert!(
            a.starfield.as_ref().unwrap().live_flares() >= 1,
            "a rise flares"
        );
        let before = a.starfield.as_ref().unwrap().live_flares();
        act.pulse_seq = Some(1); // a new run: the count went down
        a.set_activities(vec![act]);
        a.update(&backend);
        assert!(
            a.starfield.as_ref().unwrap().live_flares() <= before,
            "a drop is a reset, not a burst"
        );
    }

    #[test]
    fn running_activities_tint_the_starfield_by_kind_and_idle_does_not() {
        use crate::animation::ActivityKind;
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        a.update(&backend);
        assert!(a.activity_tint().is_none());
        let mut load = lane_act("m");
        load.kind = ActivityKind::Load;
        let mut serve = lane_act("m");
        serve.kind = ActivityKind::Serve;
        let mut stalled = lane_act("m");
        stalled.stalled = true;
        let mut hue = |act: crate::animation::Activity| {
            a.set_activities(vec![act]);
            a.activity_tint().unwrap().hue
        };
        let (train, load, serve, stalled) =
            (hue(lane_act("m")), hue(load), hue(serve), hue(stalled));
        let all = [train, load, serve, stalled];
        for (i, x) in all.iter().enumerate() {
            for y in &all[i + 1..] {
                assert_ne!(x, y, "each kind has its own tint: {all:?}");
            }
        }
    }

    fn id_act(
        id: &str,
        kind: crate::animation::ActivityKind,
        pulse: Option<u64>,
    ) -> crate::animation::Activity {
        let mut a = lane_act(id);
        a.id = id.into();
        a.kind = kind;
        a.pulse_seq = pulse;
        a
    }

    /// Lanes reorder when a load finishes. Baselines follow the activity, not
    /// the lane position, so `[LOAD(0), TRAIN(100)]` becoming
    /// `[TRAIN(100), SERVE(0)]` flares nothing.
    #[test]
    fn reordering_lanes_does_not_flare_the_starfield() {
        use crate::animation::ActivityKind::{Load, Serve, Train};
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        a.set_activities(vec![
            id_act("svc:m", Load, Some(0)),
            id_act("train:7", Train, Some(100)),
        ]);
        a.update(&backend);
        a.set_activities(vec![
            id_act("train:7", Train, Some(100)),
            id_act("svc:m", Serve, Some(0)),
        ]);
        a.update(&backend);
        assert_eq!(
            a.starfield.as_ref().unwrap().live_flares(),
            0,
            "no counter advanced"
        );
    }

    /// A service that loses its stats for a scrape and comes back at the same
    /// count adds no flares. One that comes back higher flares for the rise.
    #[test]
    fn an_unavailable_count_keeps_its_baseline() {
        use crate::animation::ActivityKind::Serve;
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        a.set_activities(vec![id_act("svc:m", Serve, Some(100))]);
        a.update(&backend);
        a.set_activities(vec![id_act("svc:m", Serve, None)]);
        a.update(&backend);
        a.set_activities(vec![id_act("svc:m", Serve, Some(100))]);
        a.update(&backend);
        assert_eq!(
            a.starfield.as_ref().unwrap().live_flares(),
            0,
            "same 100 again is not new"
        );
        a.set_activities(vec![id_act("svc:m", Serve, Some(102))]);
        a.update(&backend);
        assert!(
            a.starfield.as_ref().unwrap().live_flares() >= 1,
            "a real rise still flares"
        );
    }

    /// An activity that appears in view for the first time gets a baseline, not
    /// a burst for its whole count.
    #[test]
    fn a_newly_shown_activity_is_a_baseline_not_a_burst() {
        use crate::animation::ActivityKind::{Serve, Train};
        let backend = mock_backend();
        let mut a = ArcadeVisualization::new(120, 40);
        a.set_activities(vec![id_act("train:7", Train, Some(5))]);
        a.update(&backend);
        a.set_activities(vec![
            id_act("train:7", Train, Some(5)),
            id_act("svc:m", Serve, Some(9_000)),
        ]);
        a.update(&backend);
        assert_eq!(a.starfield.as_ref().unwrap().live_flares(), 0);
    }

    /// The roaming hero is gone (0.16.5) and must not come back.
    ///
    /// A classic arcade at 140x40 over the mock backend, with a model
    /// serving (so the duel is on Row 1), mock services and a training run,
    /// is updated for 40 ticks. After each tick its frame must be exactly
    /// its parts with nothing laid over them: the title row, Row 1, and per
    /// layout row its rule and its panels, as `panel_lines` draws them. The
    /// hero and its trail were drawn over those parts (anywhere on the
    /// screen, including over the panels), so a hero or trail that comes back
    /// makes a cell differ. With no bus and no activity lanes nothing else
    /// is laid over the parts.
    ///
    /// The duel's `@` is part of Row 1, so it is allowed: it is the only `@`
    /// in the frame, and it sits in the duel strip's columns. None of the
    /// classic panels draws an `@`.
    #[test]
    fn the_roaming_hero_is_gone() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::{Paragraph, Widget};
        let (w, h) = (140usize, 40usize);
        let mut backend = mock_backend();
        let classic = crate::animation::arcade_presets::preset("classic").unwrap();
        let mut a = ArcadeVisualization::with_config(
            w,
            h,
            &ArcadeConfig {
                rows: classic.rows.clone(),
            },
        );
        a.initialize_from_devices(backend.devices());
        a.initialize_topology(&backend);
        a.set_serving(Some(crate::workload::inference_server::ServingStats {
            generation_tps: 120.0,
            requests_running: 3,
            completed_delta: 1,
            ..crate::workload::inference_server::ServingStats::zeroed()
        }));
        let run = crate::workload::train::MockTrainRun::new();
        // Cells of `lines` drawn into a `cols` x `rows` buffer.
        let grid = |lines: Vec<Line<'static>>, cols: usize, rows: usize| {
            let area = Rect::new(0, 0, cols as u16, rows as u16);
            let mut buf = Buffer::empty(area);
            Paragraph::new(lines).render(area, &mut buf);
            buf
        };
        for tick in 0..40 {
            if tick % 4 == 0 {
                backend.update().unwrap();
            }
            a.set_services(crate::workload::inference_server::mock::mock_services(
                100.0 + tick as f32,
            ));
            a.set_train_state(Some(run.state_at(tick as f32)));
            a.update(&backend);

            let frame = grid(a.render(&backend), w, h);
            // The parts, each drawn at its own place.
            let mut want: Vec<(usize, Line<'static>)> = vec![
                (0, fit_width(a.render_header(&backend), w)),
                (1, fit_width(a.render_row1(&backend), w)),
            ];
            for row in &a.layout().rows {
                want.push((row.rule_y, a.render_separator(row.label(), row.height)));
            }
            let mut checked = 0;
            for (y, mut line) in want {
                clear_right_edge(&mut line, w);
                let part = grid(vec![line], w, 1);
                for x in 0..w {
                    assert_eq!(
                        frame[(x as u16, y as u16)],
                        part[(x as u16, 0)],
                        "tick {tick}: cell ({x}, {y}) is not its row's own"
                    );
                    checked += 1;
                }
            }
            for p in a.layout().panels() {
                let r = p.rect;
                let part = grid(a.panel_lines(p, &backend), r.w, r.h);
                // The last column belongs to the right-edge guard.
                for y in 0..r.h {
                    for x in 0..r.w {
                        if r.x + x + 1 == w {
                            continue;
                        }
                        assert_eq!(
                            frame[((r.x + x) as u16, (r.y + y) as u16)].symbol(),
                            part[(x as u16, y as u16)].symbol(),
                            "tick {tick}: {:?} cell ({x}, {y}) is not the panel's own",
                            p.view
                        );
                        assert_eq!(
                            frame[((r.x + x) as u16, (r.y + y) as u16)].fg,
                            part[(x as u16, y as u16)].fg,
                            "tick {tick}: {:?} cell ({x}, {y}) is restyled",
                            p.view
                        );
                        checked += 1;
                    }
                }
            }
            assert!(checked > w * h * 3 / 4, "tick {tick}: only {checked} cells");

            // The only `@` is the duel's, in Row 1's duel strip.
            let duel_w = (w / 2).min(44);
            for y in 0..h {
                for x in 0..w {
                    if frame[(x as u16, y as u16)].symbol() == "@" {
                        assert!(y == 1 && x < duel_w, "tick {tick}: an @ at ({x}, {y})");
                    }
                }
            }
            let row1: String = (0..duel_w)
                .map(|x| frame[(x as u16, 1)].symbol().to_string())
                .collect();
            assert!(row1.contains('@'), "tick {tick}: the duel shows: {row1:?}");
        }
    }
}

#[cfg(test)]
#[path = "arcade_bus_tests.rs"]
mod bus_tests;

#[cfg(test)]
#[path = "arcade_panel_tests.rs"]
mod panel_tests;

#[cfg(test)]
#[path = "arcade_preset_frame_tests.rs"]
mod preset_frame_tests;
