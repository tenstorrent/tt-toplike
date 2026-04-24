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
    AdaptiveBaseline, HardwareStarfield, MemoryCastle, MemoryFlowVis, BoardTopology,
    hsv_to_rgb, temp_to_hue, lerp, PARTICLE_CHARS,
};
use crate::backend::TelemetryBackend;
use crate::ui::colors;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

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

    // Hero character state
    hero_x: f32,
    hero_y: f32,
    hero_target_x: f32,
    hero_target_y: f32,
    hero_trail: Vec<TrailPosition>,

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
        // Calculate region boundaries
        // Reserve 3 lines for header, 3 for footer
        let content_height = height.saturating_sub(6);

        // New layout:
        // - Starfield: 40% of height, full width
        // - Bottom: 60% split into left (60% width) and right (40% width)
        //   - Left: Memory Castle + Memory Flow stacked
        //   - Right: Device table
        let starfield_height = (content_height as f32 * 0.4) as usize;
        let bottom_height = content_height.saturating_sub(starfield_height);

        // Castle and Flow share the bottom-left (each gets 50% of bottom_height)
        let castle_height = bottom_height / 2;
        let flow_height = bottom_height - castle_height;

        // Region boundaries must match the actual render() output.
        // render() pushes 4 lines before starfield content (header + topology
        // placeholder + 2 spacing lines).  It also inserts separators:
        //   +1  mid-starfield label (only when starfield_height > 0)
        //   +1  starfield → castle separator
        //   +1  castle → flow separator
        // overlay_hero() indexes into lines[] by hero_y, so these must match.
        let starfield_start = 4;
        let starfield_end = starfield_start + starfield_height;
        // Mid-starfield separator is only emitted when the starfield loop runs.
        let mid_sep = if starfield_height > 0 { 1 } else { 0 };
        let castle_start = starfield_end + mid_sep + 1; // +1 = starfield→castle sep
        let castle_end = castle_start + castle_height;
        let flow_start = castle_end + 1;      // castle→flow sep
        let flow_end = flow_start + flow_height;

        Self {
            width,
            height,
            starfield: HardwareStarfield::new(width, starfield_height),
            // Use lighter density for Arcade mode for better performance
            memory_castle: MemoryCastle::new_with_density(width, castle_height, 300, 15),  // 50% particles, 50% glyphs
            memory_flow: MemoryFlowVis::new_with_density(width, flow_height, 100),  // 50% particles
            hero_x: width as f32 / 2.0,
            hero_y: height as f32 / 2.0,
            hero_target_x: width as f32 / 2.0,
            hero_target_y: height as f32 / 2.0,
            hero_trail: Vec::with_capacity(20),
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

    /// Initialize from devices (called once after creation)
    pub fn initialize_from_devices(&mut self, devices: &[crate::models::Device]) {
        self.starfield.initialize_from_devices(devices);
    }

    /// Build and install board topology from live SMBUS data.
    ///
    /// Call this once after backend init.  Propagates topology to all
    /// sub-visualizations so the header diagram, stream characters, and
    /// column separators are all topology-aware in the same frame.
    pub fn initialize_topology<B: TelemetryBackend>(&mut self, backend: &B) {
        use crate::animation::topology::BoardTopology;
        let board_ids: Vec<Option<String>> = backend.devices().iter()
            .map(|d| backend.smbus_telemetry(d.index)
                .and_then(|s| s.board_id.clone()))
            .collect();
        let topo = BoardTopology::from_devices_with_ids(backend.devices(), &board_ids);

        // Push topology to sub-visualizations.
        self.starfield.set_topology(topo.clone());
        self.memory_castle.set_topology(topo.clone());
        self.board_topology = Some(topo);
    }

    /// Render a one-line topology diagram for the arcade header.
    ///
    /// Example output:
    /// `[BH0 ██░ 16W 43°C] ←→ [BH1 ██░ 14W 42°C]  ═══  [BH2 ██░ 12W 45°C] ←→ [BH3 ██░ 18W 42°C]`
    ///
    /// Returns `None` when device count < 2 or no topology is available.
    pub fn topology_diagram_line<B: TelemetryBackend>(&self, backend: &B) -> Option<Line<'static>> {
        if backend.devices().len() < 2 {
            return None;
        }

        let topo = self.board_topology.as_ref()?;
        let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
        let num_boards = topo.boards.len();

        for (b_idx, board) in topo.boards.iter().enumerate() {
            let board_color = hsv_to_rgb(board.hue, 0.85, 0.9);

            // Render each chip in this board.
            for (c_idx, &chip_idx) in board.chips.iter().enumerate() {
                let device = backend.devices().get(chip_idx);
                let telem = backend.telemetry(chip_idx);

                let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
                let temp  = telem.map(|t| t.temp_c()).unwrap_or(25.0);

                // Activity bar (3 chars): ██░, ▓▒░, etc.
                let act = (power / 80.0).clamp(0.0, 1.0);
                let bar: String = (0..3).map(|i| {
                    let threshold = (i + 1) as f32 / 3.0;
                    if act >= threshold { '█' } else if act >= threshold - 0.17 { '▓' } else { '░' }
                }).collect();

                let arch_label = device.map(|d| {
                    match d.architecture {
                        crate::models::Architecture::Blackhole  => "BH",
                        crate::models::Architecture::Wormhole   => "WH",
                        crate::models::Architecture::Grayskull  => "GS",
                        crate::models::Architecture::Unknown    => "?",
                    }
                }).unwrap_or("?");

                let chip_text = format!("[{}{} {} {:.0}W {:.0}°C]",
                    arch_label, chip_idx, bar, power, temp);

                let chip_color = hsv_to_rgb(
                    (board.hue + chip_idx as f32 * 15.0) % 360.0, 0.9, 0.9,
                );
                spans.push(Span::styled(chip_text, Style::default().fg(chip_color)));

                // Intra-board link between chips on the same board.
                if c_idx + 1 < board.chips.len() {
                    spans.push(Span::styled(
                        " ←→ ",
                        Style::default().fg(board_color).add_modifier(Modifier::BOLD),
                    ));
                }
            }

            // Inter-board link between boards.
            if b_idx + 1 < num_boards {
                spans.push(Span::styled(
                    "  ═══  ",
                    Style::default().fg(colors::rgb(200, 160, 60)).add_modifier(Modifier::BOLD),
                ));
            }
        }

        Some(Line::from(spans))
    }

    /// Update all visualizations and hero position from telemetry
    pub fn update<B: TelemetryBackend>(&mut self, backend: &B) {
        self.frame = self.frame.wrapping_add(1);

        // Update sub-visualizations
        self.starfield.update_from_telemetry(backend);
        self.memory_castle.update(backend);
        self.memory_flow.update(backend);

        // Update hero position based on telemetry
        if let Some(device) = backend.devices().first() {
            if let Some(telemetry) = backend.telemetry(device.index) {
                let power = telemetry.power.unwrap_or(0.0);
                let current = telemetry.current.unwrap_or(0.0);

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
            }
        }

        // Smooth interpolation toward target (10-frame lerp)
        let lerp_speed = 0.1; // 10% per frame = ~10 frames to reach target
        self.hero_x = lerp(self.hero_x, self.hero_target_x, lerp_speed);
        self.hero_y = lerp(self.hero_y, self.hero_target_y, lerp_speed);

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

        // Y position: map power to regions
        // 0-30W → Flow region (bottom)
        // 30-80W → Castle region (middle)
        // 80W+ → Starfield region (top)
        let target_y = if power < 30.0 {
            // Low power: bottom third (Flow region)
            let ratio = power / 30.0;
            self.flow_start as f32 + ratio * (self.flow_end - self.flow_start) as f32
        } else if power < 80.0 {
            // Medium power: middle third (Castle region)
            let ratio = (power - 30.0) / 50.0;
            self.castle_start as f32 + ratio * (self.castle_end - self.castle_start) as f32
        } else {
            // High power: top third (Starfield region)
            let ratio = ((power - 80.0) / 70.0).min(1.0); // Cap at 150W
            self.starfield_start as f32 + ratio * (self.starfield_end - self.starfield_start) as f32
        };

        (target_x, target_y)
    }

    /// Update hero trail (add current position, age old positions)
    fn update_trail(&mut self) {
        // Add current position to trail
        self.hero_trail.push(TrailPosition {
            x: self.hero_x,
            y: self.hero_y,
            age: 0,
        });

        // Age all trail positions
        for pos in &mut self.hero_trail {
            pos.age += 1;
        }

        // Remove old trail positions (keep last 20 frames)
        self.hero_trail.retain(|pos| pos.age < 20);
    }

    /// Render the complete Arcade visualization
    pub fn render<B: TelemetryBackend>(&self, backend: &B) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.height);

        // Render header (3 lines always, topology diagram as 4th when available).
        lines.push(self.render_header(backend));
        if let Some(diagram) = self.topology_diagram_line(backend) {
            lines.push(diagram);
        } else {
            lines.push(Line::from("")); // Placeholder keeps layout stable.
        }
        lines.push(Line::from("")); // Spacing
        lines.push(Line::from("")); // Spacing

        // Render Starfield region
        let starfield_lines = self.starfield.render();
        let separator_1 = self.render_separator("✧ STARFIELD", self.starfield_end - self.starfield_start);

        for (i, line) in starfield_lines.iter().enumerate() {
            if i == starfield_lines.len() / 2 {
                // Insert separator in middle for visual clarity
                lines.push(separator_1.clone());
            }
            lines.push(line.clone());
        }

        // Separator between Starfield and Castle
        lines.push(self.render_separator("🏰 MEMORY CASTLE", self.castle_end - self.castle_start));

        // Render Memory Castle region
        let castle_lines = self.memory_castle.render(backend);
        for line in castle_lines {
            lines.push(line);
        }

        // Separator between Castle and Flow
        lines.push(self.render_separator("🌊 MEMORY FLOW", self.flow_end - self.flow_start));

        // Render Memory Flow region
        let flow_lines = self.memory_flow.render(backend);
        for line in flow_lines {
            lines.push(line);
        }

        // Render footer
        lines.push(Line::from("")); // Spacing
        lines.push(self.render_footer(backend));

        // Overlay hero character and trail on the composite canvas
        self.overlay_hero(lines, backend)
    }

    /// Render header for Arcade mode
    fn render_header<B: TelemetryBackend>(&self, backend: &B) -> Line<'static> {
        let device_count = backend.devices().len();

        Line::from(vec![
            Span::styled(
                "  🎮 ARCADE MODE ",
                Style::default()
                    .fg(colors::rgb(220, 240, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "│",
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(150, 120, 180)),
            ),
            Span::styled(
                format!(" {} Device{} ", device_count, if device_count == 1 { "" } else { "s" }),
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(80, 220, 200)),
            ),
            Span::styled(
                "│",
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(150, 120, 180)),
            ),
            Span::styled(
                " Press 'v' to cycle modes ",
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(160, 160, 160)),
            ),
        ])
    }

    /// Render region separator
    fn render_separator(&self, label: &str, _region_height: usize) -> Line<'static> {
        // Animated color cycling for separator
        let hue = (self.frame as f32 * 2.0) % 360.0;
        let separator_color = hsv_to_rgb(hue, 0.6, 0.8);

        let label_width = label.len() + 2; // " label "
        let line_width = (self.width.saturating_sub(label_width)) / 2;

        Line::from(vec![
            Span::styled(
                "─".repeat(line_width),
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
                "─".repeat(line_width),
                Style::default()
                    .fg(separator_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    }

    /// Render footer with legend and hero status
    fn render_footer<B: TelemetryBackend>(&self, backend: &B) -> Line<'static> {
        // Get telemetry for hero status
        let (power, temp, current) = if let Some(device) = backend.devices().first() {
            if let Some(telem) = backend.telemetry(device.index) {
                (
                    telem.power.unwrap_or(0.0),
                    telem.asic_temperature.unwrap_or(25.0),
                    telem.current.unwrap_or(0.0),
                )
            } else {
                (0.0, 25.0, 0.0)
            }
        } else {
            (0.0, 25.0, 0.0)
        };

        Line::from(vec![
            Span::styled(
                "  Hero: ",
                Style::default()
                    .fg(colors::rgb(160, 160, 160)),
            ),
            Span::styled(
                "@",
                Style::default()
                    .fg(hsv_to_rgb(temp_to_hue(temp), 1.0, 1.0))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" │ P:{:.1}W T:{:.1}°C I:{:.1}A ", power, temp, current),
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(160, 160, 160)),
            ),
            Span::styled(
                "│ Trail: ",
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(160, 160, 160)),
            ),
            Span::styled(
                PARTICLE_CHARS.iter().collect::<String>(),
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(80, 220, 200)),
            ),
        ])
    }

    /// Splice a single styled character into an existing line at a given column.
    ///
    /// Flattens all spans to a character vector, swaps the character at `col`,
    /// then rebuilds as [before, styled_char, after].  Span styling on other
    /// characters on the same row is lost, which is acceptable — the hero and
    /// trail are meant to be the most visible elements on screen.
    fn splice_char(lines: &mut Vec<Line<'static>>, row: usize, col: usize, ch: char, style: Style) {
        if row >= lines.len() { return; }
        let content: String = lines[row].spans.iter().map(|s| s.content.as_ref()).collect();
        let mut chars: Vec<char> = content.chars().collect();
        // Pad with spaces so the hero is always visible even when the rendered
        // line is shorter than col (common under high-current load where
        // target_x approaches width-1 but content lines are not right-padded).
        while chars.len() <= col {
            chars.push(' ');
        }
        let before: String = chars[..col].iter().collect();
        let after: String = chars[col + 1..].iter().collect();
        lines[row] = Line::from(vec![
            Span::raw(before),
            Span::styled(ch.to_string(), style),
            Span::raw(after),
        ]);
    }

    /// Overlay hero character and trail onto the rendered canvas.
    ///
    /// Trail is drawn first (oldest positions first) so the hero `@` always
    /// sits on top.  Both trail and hero are spliced character-by-character
    /// into the already-rendered lines.
    fn overlay_hero<B: TelemetryBackend>(
        &self,
        mut lines: Vec<Line<'static>>,
        backend: &B,
    ) -> Vec<Line<'static>> {
        let temp = backend
            .devices()
            .first()
            .and_then(|d| backend.telemetry(d.index))
            .map(|t| t.temp_c())
            .unwrap_or(25.0);

        let hue = temp_to_hue(temp);

        // Draw trail (oldest → newest so hero overwrites everything)
        for pos in &self.hero_trail {
            let row = pos.y as usize;
            let col = pos.x as usize;

            let fade = 1.0 - (pos.age as f32 / 20.0);
            let fade_exp = fade * fade; // Exponential: rapid initial fade, lingering tail

            let trail_char = match pos.age {
                0..=4  => '○',
                5..=9  => '◦',
                10..=14 => '•',
                _      => '·',
            };

            let color = hsv_to_rgb(hue, 0.7 * fade_exp, (0.55 * fade_exp).max(0.05));
            let style = Style::default().fg(color);
            Self::splice_char(&mut lines, row, col, trail_char, style);
        }

        // Draw hero character (overwrites trail at current position)
        let row = self.hero_y as usize;
        let col = self.hero_x as usize;

        // Brightness pulses with the frame counter (heartbeat effect)
        let pulse = ((self.frame % 60) as f32 / 60.0 * std::f32::consts::PI * 2.0).sin();
        let value = 0.85 + pulse * 0.15;
        let hero_color = hsv_to_rgb(hue, 1.0, value);
        let hero_style = Style::default()
            .fg(hero_color)
            .add_modifier(Modifier::BOLD);

        Self::splice_char(&mut lines, row, col, '@', hero_style);

        lines
    }
}
