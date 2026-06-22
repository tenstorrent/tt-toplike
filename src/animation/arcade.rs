// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Arcade Mode - Unified Psychedelic Visualization
//!
//! Combines all three visualizations (Starfield, Memory Castle, Memory Flow) into a single
//! all-encompassing view with a roguelike hero character that moves based on real telemetry.
//!
//! Layout:
//! - Top 30%: Starfield (stars = Tensix cores, planets = memory hierarchy)
//! - Middle 40%: Memory Castle (DDR channels, L2 cache, L1 SRAM, particles flowing upward)
//! - Bottom 30%: Memory Flow (NoC particles, DDR perimeter, heat map center)
//!
//! Hero Character (@):
//! - Vertical position driven by power consumption (low power = bottom, high = top)
//! - Horizontal position driven by current draw
//! - Color driven by temperature (cyan → yellow → red)
//! - Trail effect showing recent positions
//! - BOLD white character for maximum visibility

use crate::animation::{
    hsv_to_rgb, lerp, temp_to_hue, AdaptiveBaseline, BoardTopology, DefragVis, HardwareStarfield,
    MemoryCastle, MemoryFlowVis,
};
use crate::backend::TelemetryBackend;
use crate::ui::colors;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Hero character trail entry
#[derive(Debug, Clone)]
struct TrailPosition {
    x: f32,
    y: f32,
    age: u32, // Frames since creation
}

/// Arcade visualization combining all three modes with hero character
pub struct ArcadeVisualization {
    width: usize,
    height: usize,

    // Sub-visualizations (public for direct access in UI)
    pub starfield: HardwareStarfield,
    pub memory_castle: MemoryCastle,
    pub memory_flow: MemoryFlowVis,
    /// Defrag block map rendered side-by-side with Memory Castle.
    defrag: DefragVis,
    /// Width of the left (castle) column in the split section.
    castle_col_w: usize,
    /// Width of the right (defrag) column in the split section.
    defrag_col_w: usize,

    // Hero character state
    hero_x: f32,
    hero_y: f32,
    hero_target_x: f32,
    hero_target_y: f32,
    hero_trail: Vec<TrailPosition>,
    /// Lerp speed, updated each frame from aiclk (throttled → slow, boosted → fast).
    hero_lerp_speed: f32,
    /// Trail length in frames, driven by live ETH link count.
    hero_trail_length: usize,
    // Telemetry-driven expression flags (updated in update(), consumed in overlay_hero())
    hero_throttled: bool,        // throttler register non-zero
    hero_faulted: bool,          // faults register non-zero
    hero_arc_stalled: bool,      // heartbeat == 0
    hero_undervolt: bool,        // vcore < 0.75 V
    hero_harvested: bool,        // harvesting_state > 0 (some cores disabled)
    hero_gddr_temp: Option<f32>, // max GDDR temp — drives trail color separately from ASIC

    // Animation state
    frame: u32,
    baseline: AdaptiveBaseline,

    // Region boundaries (in rows)
    starfield_start: usize,
    starfield_end: usize,
    castle_start: usize,
    castle_end: usize,
    flow_start: usize,
    flow_end: usize,

    /// Board topology — used to render the header topology diagram line.
    board_topology: Option<BoardTopology>,
}

impl ArcadeVisualization {
    /// Create new Arcade visualization with btop++-inspired layout
    pub fn new(width: usize, height: usize) -> Self {
        // Budget: height rows total.
        //   Row 0:         title bar
        //   Row 1:         topology diagram (or blank)
        //   Row 2:         "✧ STARFIELD" separator
        //   Rows 3…3+sf:   starfield content
        //   Row 3+sf+1:    "🏰 MEMORY CASTLE │ ▓ DEFRAG" separator
        //   Rows …:        castle/defrag content
        //   Row …+1:       "🌊 MEMORY FLOW" separator
        //   Rows …:        flow content
        //   Last row:      legend/status bar (overwritten by command-bar overlay)
        // 2 header + 3 separators + 1 footer = 6 fixed rows; rest is content.
        let content_height = height.saturating_sub(6);

        // Starfield 40%, Castle+Defrag 30%, Flow 30%.
        let starfield_height = (content_height as f32 * 0.40) as usize;
        let bottom_height = content_height.saturating_sub(starfield_height);
        let castle_height = bottom_height / 2;
        let flow_height = bottom_height - castle_height;

        // Castle column gets 55% of width, defrag gets the rest.
        // The │ divider between them costs 1 col, so both columns together
        // must be width - 1 to keep total display width == width.
        let castle_col_w = (width as f32 * 0.55) as usize;
        let defrag_col_w = width.saturating_sub(castle_col_w).saturating_sub(1);

        // Row boundaries for hero overlay (0-indexed into the line vec).
        let starfield_start = 2; // after title + topology
        let starfield_end = starfield_start + starfield_height;
        let castle_start = starfield_end + 1; // +1 separator
        let castle_end = castle_start + castle_height;
        let flow_start = castle_end + 1;
        let flow_end = flow_start + flow_height;

        Self {
            width,
            height,
            starfield: HardwareStarfield::new(width, starfield_height),
            memory_castle: MemoryCastle::new_with_density(castle_col_w, castle_height, 200, 10),
            defrag: DefragVis::new(defrag_col_w, castle_height),
            castle_col_w,
            defrag_col_w,
            memory_flow: MemoryFlowVis::new_with_density(width, flow_height, 100),
            hero_x: width as f32 / 2.0,
            hero_y: height as f32 / 2.0,
            hero_target_x: width as f32 / 2.0,
            hero_target_y: height as f32 / 2.0,
            hero_trail: Vec::with_capacity(20),
            hero_lerp_speed: 0.1,
            hero_trail_length: 20,
            hero_throttled: false,
            hero_faulted: false,
            hero_arc_stalled: false,
            hero_undervolt: false,
            hero_harvested: false,
            hero_gddr_temp: None,
            frame: 0,
            baseline: AdaptiveBaseline::new(),
            starfield_start,
            starfield_end,
            castle_start,
            castle_end,
            flow_start,
            flow_end,
            board_topology: None,
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
        self.starfield.set_sensitivity(s);
        self.memory_castle.set_sensitivity(s);
        self.memory_flow.set_sensitivity(s);
    }

    /// Initialize from devices (called once after creation)
    pub fn initialize_from_devices(&mut self, devices: &[crate::models::Device]) {
        self.starfield.initialize_from_devices(devices);
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
        self.starfield.set_topology(topo.clone());
        self.memory_castle.set_topology(topo.clone());
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

    /// Update all visualizations and hero position from telemetry.
    ///
    /// Hero speed is driven by aiclk: a throttled chip (low clock) moves sluggishly,
    /// a full-speed chip moves quickly.  This makes the hero visually reflect whether
    /// the chip is being held back.
    pub fn update(&mut self, backend: &dyn TelemetryBackend) {
        self.frame = self.frame.wrapping_add(1);

        // Update sub-visualizations
        self.starfield.update_from_telemetry(backend);
        self.memory_castle.update(backend);
        self.memory_flow.update(backend);
        self.defrag.update(backend);

        // Update hero position based on telemetry
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

                // Update baseline for relative activity detection
                self.baseline.update(
                    device.index,
                    power,
                    current,
                    telemetry.asic_temperature.unwrap_or(25.0),
                    telemetry.aiclk.unwrap_or(1000) as f32,
                );

                // Calculate target position
                let (target_x, target_y) = self.calculate_hero_target(power, current);
                self.hero_target_x = target_x;
                self.hero_target_y = target_y;

                // lerp speed driven by aiclk: throttled (< 500 MHz) → slow,
                // normal (800–1000 MHz) → default, boosted (> 1100 MHz) → fast.
                let aiclk = telemetry.aiclk.unwrap_or(1000) as f32;
                self.hero_lerp_speed = if aiclk < 500.0 {
                    0.04
                } else if aiclk < 800.0 {
                    0.04 + (aiclk - 500.0) / 300.0 * 0.06 // 0.04 → 0.10
                } else if aiclk < 1100.0 {
                    0.10
                } else {
                    0.18 // overclocked / boosted
                };

                // Trail length scales with number of live ETH links:
                // more ETH links → denser inter-chip traffic → longer visible trail.
                let eth_links = smbus
                    .and_then(|s| s.eth_live_status)
                    .map(|m| m.count_ones() as usize)
                    .unwrap_or(0);
                // Base trail = 10 frames; each 4 live links adds 5 more, up to 40.
                self.hero_trail_length = (10 + (eth_links / 4) * 5).min(40);

                // Hero status flags — read once here, used in overlay_hero().
                self.hero_throttled = smbus
                    .and_then(|s| s.throttler.as_deref())
                    .map(|t| t != "0x0" && t != "0" && !t.is_empty())
                    .unwrap_or(false);
                self.hero_faulted = smbus
                    .and_then(|s| s.faults.as_deref())
                    .map(|f| f != "0x0" && f != "0" && !f.is_empty())
                    .unwrap_or(false);
                self.hero_arc_stalled = telemetry.heartbeat.map(|h| h == 0).unwrap_or(false);
                self.hero_undervolt = telemetry.voltage.map(|v| v < 0.75).unwrap_or(false);
                self.hero_gddr_temp = smbus.and_then(|s| s.max_gddr_temp);
                self.hero_harvested = smbus
                    .and_then(|s| s.harvesting_state)
                    .map(|h| h > 0)
                    .unwrap_or(false);
            }
        }

        // Smooth interpolation toward target
        self.hero_x = lerp(self.hero_x, self.hero_target_x, self.hero_lerp_speed);
        self.hero_y = lerp(self.hero_y, self.hero_target_y, self.hero_lerp_speed);

        // Update trail
        self.update_trail();
    }

    /// Calculate hero target position based on power and current
    ///
    /// - Vertical (Y): Power consumption (0-150W mapped to height)
    ///   - Low power (0-30W): DDR region (bottom)
    ///   - Medium power (30-80W): L2/L1 region (middle)
    ///   - High power (80W+): Tensix/Starfield region (top)
    ///
    /// - Horizontal (X): Current draw (0-100A mapped to width)
    fn calculate_hero_target(&self, power: f32, current: f32) -> (f32, f32) {
        // X position: map current 0-100A to width
        let normalized_current = (current / 100.0).max(0.0).min(1.0);
        let target_x = normalized_current * (self.width as f32 - 1.0);

        // Y position: map power across all three regions top-to-bottom.
        // High power → starfield (top); medium → castle/defrag (middle); low → flow (bottom).
        let target_y = if power >= 80.0 {
            let ratio = ((power - 80.0) / 70.0).clamp(0.0, 1.0);
            self.starfield_start as f32 + ratio * (self.starfield_end - self.starfield_start) as f32
        } else if power >= 30.0 {
            let ratio = (power - 30.0) / 50.0;
            self.castle_start as f32 + ratio * (self.castle_end - self.castle_start) as f32
        } else {
            let ratio = (power / 30.0).clamp(0.0, 1.0);
            self.flow_start as f32 + ratio * (self.flow_end - self.flow_start) as f32
        };

        (target_x, target_y)
    }

    /// Update hero trail (add current position, age old positions).
    /// Length is dynamic: driven by eth_live_status popcount in update().
    fn update_trail(&mut self) {
        self.hero_trail.push(TrailPosition {
            x: self.hero_x,
            y: self.hero_y,
            age: 0,
        });
        for pos in &mut self.hero_trail {
            pos.age += 1;
        }
        let max_age = self.hero_trail_length as u32;
        self.hero_trail.retain(|pos| pos.age < max_age);
    }

    /// Render the complete Arcade visualization
    pub fn render(&self, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.height);

        // Header: exactly 2 rows — title bar + topology (or blank).
        lines.push(self.render_header(backend));
        if let Some(diagram) = self.topology_diagram_line(backend) {
            lines.push(diagram);
        } else {
            lines.push(Line::from(""));
        }

        // Render Starfield region — separator first, then content
        let starfield_lines = self.starfield.render();
        lines.push(self.render_separator("✧ STARFIELD", self.starfield_end - self.starfield_start));
        for line in &starfield_lines {
            lines.push(line.clone());
        }

        // Castle + Defrag side-by-side separator
        lines.push(self.render_separator(
            "🏰 MEMORY CASTLE  │  ▓ DEFRAG",
            self.castle_end - self.castle_start,
        ));

        // Render castle and defrag columns, zip them side by side.
        // Each produces `castle_height` lines; pad shorter side with blanks.
        let castle_lines = self.memory_castle.render(backend);
        let defrag_lines = self.defrag.render_embedded(backend);
        let split_rows = self.castle_end - self.castle_start;
        let div = Span::styled("│", Style::default().fg(colors::rgb(50, 70, 90)));
        for row in 0..split_rows {
            let left = castle_lines
                .get(row)
                .cloned()
                .unwrap_or_else(|| Line::from(""));
            let right = defrag_lines
                .get(row)
                .cloned()
                .unwrap_or_else(|| Line::from(""));
            // Clamp both columns to their budgets, then pad left to exact width.
            let left_spans = clamp_spans_to_width(left.spans, self.castle_col_w);
            let right_spans = clamp_spans_to_width(right.spans, self.defrag_col_w);
            let left_w: usize = left_spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            let pad = self.castle_col_w.saturating_sub(left_w);
            let mut spans: Vec<Span<'static>> = left_spans;
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            spans.push(div.clone());
            spans.extend(right_spans);
            lines.push(Line::from(spans));
        }

        // Separator between Castle and Flow
        lines.push(self.render_separator("🌊 MEMORY FLOW", self.flow_end - self.flow_start));

        // Render Memory Flow region
        let flow_lines = self.memory_flow.render(backend);
        for line in flow_lines {
            lines.push(line);
        }

        // Footer: 1 row — legend + hero status.
        lines.push(self.render_footer(backend));

        // Overlay hero character and trail on the composite canvas
        self.overlay_hero(lines, backend)
    }

    /// Render header for Arcade mode
    fn render_header(&self, backend: &dyn TelemetryBackend) -> Line<'static> {
        let device_count = backend.devices().len();
        let label = format!("  🎮 ARCADE MODE ");
        // "🎮" is 2 display cols wide; UnicodeWidthStr measures correctly.
        let label_w = UnicodeWidthStr::width(label.as_str());
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
    fn render_separator(&self, label: &str, _region_height: usize) -> Line<'static> {
        // Animated color cycling for separator
        let hue = (self.frame as f32 * 2.0) % 360.0;
        let separator_color = hsv_to_rgb(hue, 0.6, 0.8);

        // Use display width (columns) not byte length — emoji like 🏰 are 2 columns wide.
        let label_display_width = UnicodeWidthStr::width(label) + 2; // " label "
        let remaining = self.width.saturating_sub(label_display_width);
        let left_width = remaining / 2;
        let right_width = remaining - left_width; // absorbs odd remainder so total == self.width

        Line::from(vec![
            Span::styled(
                "─".repeat(left_width),
                Style::default()
                    .fg(separator_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", label),
                Style::default()
                    .fg(colors::rgb(220, 240, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "─".repeat(right_width),
                Style::default()
                    .fg(separator_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    }

    /// One-line legend covering all three arcade panels + hero + global keys.
    /// Matches the style of the shared `render_command_bar` overlay on other views.
    fn render_footer(&self, backend: &dyn TelemetryBackend) -> Line<'static> {
        let temp = backend
            .devices()
            .first()
            .and_then(|d| backend.telemetry(d.index))
            .map(|t| t.temp_c())
            .unwrap_or(25.0);
        let hero_color = hsv_to_rgb(temp_to_hue(temp), 1.0, 0.95);
        let dim = colors::rgb(90, 90, 110);
        let label = colors::rgb(150, 150, 170);
        let key = colors::rgb(80, 220, 200);
        let sep = colors::rgb(50, 60, 80);

        Line::from(vec![
            // Starfield legend
            Span::styled("  ✧ ", Style::default().fg(colors::rgb(100, 180, 255))),
            Span::styled("stars=cores ", Style::default().fg(dim)),
            Span::styled("○ ", Style::default().fg(colors::rgb(255, 200, 100))),
            Span::styled("planets=mem  ", Style::default().fg(dim)),
            Span::styled("│ ", Style::default().fg(sep)),
            // Castle legend
            Span::styled("🏰 ", Style::default().fg(colors::rgb(180, 140, 255))),
            Span::styled("particles: ", Style::default().fg(dim)),
            Span::styled("◦read ", Style::default().fg(colors::rgb(100, 200, 255))),
            Span::styled("·write ", Style::default().fg(colors::rgb(255, 180, 100))),
            Span::styled("◇hit ", Style::default().fg(colors::rgb(100, 255, 180))),
            Span::styled("•miss  ", Style::default().fg(colors::rgb(255, 100, 150))),
            Span::styled("│ ", Style::default().fg(sep)),
            // Defrag legend
            Span::styled("▓ ", Style::default().fg(colors::rgb(100, 220, 255))),
            Span::styled("blocks=GDDR  ", Style::default().fg(dim)),
            Span::styled("│ ", Style::default().fg(sep)),
            // Hero legend
            Span::styled(
                "@ ",
                Style::default().fg(hero_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled("x=current y=power color=temp  ", Style::default().fg(dim)),
            Span::styled("│ ", Style::default().fg(sep)),
            // Nav keys — consistent with other views
            Span::styled(
                " v ",
                Style::default()
                    .fg(key)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            Span::styled("cycle  ", Style::default().fg(label)),
            Span::styled(
                " l ",
                Style::default()
                    .fg(key)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            Span::styled("legend  ", Style::default().fg(label)),
            Span::styled(
                " ? ",
                Style::default()
                    .fg(colors::rgb(220, 180, 80))
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            Span::styled("help  ", Style::default().fg(label)),
            Span::styled(
                " / ",
                Style::default()
                    .fg(colors::rgb(220, 180, 80))
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            Span::styled("cmd", Style::default().fg(label)),
        ])
    }

    /// Splice a single styled character into an existing line at a given column.
    ///
    /// Flattens all spans to a character vector, swaps the character at `col`,
    /// then rebuilds as [before, styled_char, after].  Span styling on other
    /// characters on the same row is lost, which is acceptable — the hero and
    /// trail are meant to be the most visible elements on screen.
    /// Replace the character at display column `col` in `lines[row]` without
    /// disturbing the rest of the row's spans.  Works span-by-span so the
    /// surrounding colors (defrag blocks, castle particles, etc.) are untouched.
    fn splice_char(lines: &mut Vec<Line<'static>>, row: usize, col: usize, ch: char, style: Style) {
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
                // Hero column is outside this span — keep as-is.
                new_spans.push(span);
            } else {
                // Hero column falls inside this span.  Split around it.
                let offset = col - cursor; // character offset into the span
                                           // collect chars so we can index by display position safely
                let chars: Vec<char> = text.chars().collect();
                // Compute char index at display offset (for non-ASCII, chars and
                // display cols can differ, but all our content is ASCII-safe here).
                let before: String = chars[..offset].iter().collect();
                let after: String = chars[(offset + 1).min(chars.len())..].iter().collect();
                if !before.is_empty() {
                    new_spans.push(Span::styled(before, span.style));
                }
                new_spans.push(Span::styled(ch.to_string(), style));
                if !after.is_empty() {
                    new_spans.push(Span::styled(after, span.style));
                }
                spliced = true;
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

    /// Overlay hero character and trail onto the rendered canvas.
    ///
    /// Trail is drawn first (oldest → newest) so the hero always sits on top.
    ///
    /// ## Hero character meaning
    /// - `@` — healthy, normal operation
    /// - `%` — undervolt (vcore < 0.75 V)
    /// - `!` — throttled or faulted (thermal / power limit hit)
    /// - `?` — ARC firmware stalled (heartbeat == 0)
    /// - `(` / `)` — harvested cores (some Tensix columns disabled); alternates by frame
    ///
    /// ## Colors
    /// - Hero body: ASIC temperature → cyan (cool) → red (hot)
    /// - Trail: GDDR temperature when available (else matches body); shows GDDR
    ///   heat independently of ASIC, useful during memory-heavy model loads.
    ///
    /// ## Trail length
    /// Driven by live ETH link count (set in update()).  More ETH links → more
    /// inter-chip traffic → longer, denser trail.
    fn overlay_hero(
        &self,
        mut lines: Vec<Line<'static>>,
        backend: &dyn TelemetryBackend,
    ) -> Vec<Line<'static>> {
        let temp = backend
            .devices()
            .first()
            .and_then(|d| backend.telemetry(d.index))
            .map(|t| t.temp_c())
            .unwrap_or(25.0);

        let asic_hue = temp_to_hue(temp);
        // Trail uses GDDR temp color when available — shows memory heat separately
        // from ASIC heat.  They often diverge during model loading.
        let trail_hue = self.hero_gddr_temp.map(temp_to_hue).unwrap_or(asic_hue);

        let max_age = self.hero_trail_length as f32;

        // Draw trail (oldest → newest so hero overwrites everything)
        for pos in &self.hero_trail {
            let row = pos.y as usize;
            let col = pos.x as usize;

            let fade = 1.0 - (pos.age as f32 / max_age);
            let fade_exp = fade * fade; // exponential: rapid initial fade, lingering tail

            let trail_char = match pos.age {
                0..=4 => '○',
                5..=9 => '◦',
                10..=19 => '•',
                _ => '·',
            };

            let color = hsv_to_rgb(trail_hue, 0.7 * fade_exp, (0.55 * fade_exp).max(0.05));
            Self::splice_char(&mut lines, row, col, trail_char, Style::default().fg(color));
        }

        // ── Hero character ────────────────────────────────────────────────────
        let row = self.hero_y as usize;
        let col = self.hero_x as usize;

        // Character encodes the chip's primary condition (priority order).
        let hero_char = if self.hero_arc_stalled {
            '?' // ARC firmware unresponsive — urgent
        } else if self.hero_faulted || self.hero_throttled {
            '!' // thermal/power fault or active throttling
        } else if self.hero_undervolt {
            '%' // supply voltage too low
        } else if self.hero_harvested {
            // Alternating parens hint at "partial" chip (harvested cores disabled)
            if self.frame % 40 < 20 {
                '('
            } else {
                ')'
            }
        } else {
            '@' // healthy
        };

        // Brightness pulses — faster and more pronounced when throttled/faulted
        // to draw the eye; a healthy chip pulses gently like a heartbeat.
        let (pulse_period, pulse_amp) = if self.hero_faulted || self.hero_throttled {
            (20u32, 0.30_f32) // rapid strobe
        } else if self.hero_arc_stalled {
            (10u32, 0.40_f32) // very fast strobe — "something is wrong"
        } else {
            (60u32, 0.15_f32) // gentle heartbeat
        };
        let pulse =
            ((self.frame % pulse_period) as f32 / pulse_period as f32 * std::f32::consts::PI * 2.0)
                .sin();
        let value = (0.85 + pulse * pulse_amp).clamp(0.0, 1.0);

        // Faulted/throttled hero is red regardless of temperature
        let hero_hue = if self.hero_faulted || self.hero_throttled {
            0.0 // red
        } else if self.hero_arc_stalled {
            60.0 // yellow — "caution, not dead"
        } else {
            asic_hue
        };

        let hero_color = hsv_to_rgb(hero_hue, 1.0, value);
        let hero_style = Style::default().fg(hero_color).add_modifier(Modifier::BOLD);

        Self::splice_char(&mut lines, row, col, hero_char, hero_style);

        lines
    }
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
    use ratatui::style::Style;
    use ratatui::text::Line;

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
        assert!(vis.starfield_start < vis.starfield_end);
        assert!(vis.starfield_end <= vis.castle_start);
        assert!(vis.castle_start < vis.castle_end);
        assert!(vis.castle_end <= vis.flow_start);
        assert!(vis.flow_start < vis.flow_end);
    }

    #[test]
    fn region_boundaries_tiny_height() {
        // At very small heights regions may collapse to zero size but must
        // not violate start <= end or produce overlapping ranges.
        let vis = ArcadeVisualization::new(40, 6);
        assert!(vis.starfield_start <= vis.starfield_end);
        assert!(vis.starfield_end <= vis.castle_start);
        assert!(vis.castle_start <= vis.castle_end);
        assert!(vis.castle_end <= vis.flow_start);
        assert!(vis.flow_start <= vis.flow_end);
    }

    #[test]
    fn mid_sep_only_for_height_ge_2() {
        // starfield_height == 0: no mid separator → castle_start = starfield_end + 1
        let vis0 = ArcadeVisualization::new(80, 4); // content_height ≈ 0
                                                    // starfield_height == 1 (content_height ≈ 2): no mid separator
        let vis1 = ArcadeVisualization::new(80, 10);
        // Both must satisfy the no-overlap invariant
        assert!(vis0.starfield_end <= vis0.castle_start);
        assert!(vis1.starfield_end <= vis1.castle_start);
    }
}
