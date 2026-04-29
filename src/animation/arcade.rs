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
        // Mid-starfield separator is only emitted when there are >= 2 starfield
        // lines (the insertion fires at i == len/2, which is i==0 for len==1,
        // placing the separator *before* the only content line and shifting all
        // subsequent indices by 1).  Skip it for height < 2 to keep boundaries
        // consistent with what render() actually produces.
        let mid_sep = if starfield_height >= 2 { 1 } else { 0 };
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
    /// **≤ 8 chips** — detailed format:
    /// `[BH0 ██░ 16W 43°C] ←→ [BH1 ██░ 14W 42°C]  ═══  [BH2 ██░ 12W 45°C] …`
    ///
    /// **> 8 chips** — compact mini-bar (one character per chip, color = temp):
    /// `32× BH  [░▒▓█░░░░▒▓█░░▒▓█░░▒▓█▒░░░░▒▓█░▒]`
    ///
    /// Returns `None` when device count < 2 or no topology is available.
    pub fn topology_diagram_line<B: TelemetryBackend>(&self, backend: &B) -> Option<Line<'static>> {
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
        // Layout for multi-chip boards:   [BH2 ██░ 16W ←→ BH3 ██░ 14W]  ═══  [BH0 ██░ 12W ←→ BH1 ██░ 18W]
        // Layout for single-chip cards:   [BH0 ██░ 15W]  [BH1 ██░ 12W]  [BH2 ██░ 13W]  [BH3 ██░ 18W]
        //
        // The [ ] now wraps the whole board so the ←→ intra-chip link sits visually
        // inside the board container and ═══ clearly connects two distinct units.
        let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
        let num_boards = topo.boards.len();

        for (b_idx, board) in topo.boards.iter().enumerate() {
            let board_color = hsv_to_rgb(board.hue, 0.85, 0.9);

            // Opening board bracket in board color.
            spans.push(Span::styled("[", Style::default().fg(board_color)));

            for (c_idx, &chip_idx) in board.chips.iter().enumerate() {
                let device = devices.get(chip_idx);
                let telem  = backend.telemetry(chip_idx);
                let power  = telem.map(|t| t.power_w()).unwrap_or(0.0);
                let temp   = telem.map(|t| t.temp_c()).unwrap_or(25.0);

                let act = (power / 80.0).clamp(0.0, 1.0);
                let bar: String = (0..3).map(|i| {
                    let threshold = (i + 1) as f32 / 3.0;
                    if act >= threshold { '█' } else if act >= threshold - 0.17 { '▓' } else { '░' }
                }).collect();

                let arch_label = device.map(|d| d.architecture.abbrev()).unwrap_or("?");
                // No individual brackets — the chip is inside the board's [ ].
                let chip_text = format!("{}{} {} {:.0}W {:.0}°C",
                    arch_label, chip_idx, bar, power, temp);
                let chip_color = hsv_to_rgb(
                    (board.hue + chip_idx as f32 * 15.0) % 360.0, 0.9, 0.9,
                );
                spans.push(Span::styled(chip_text, Style::default().fg(chip_color)));

                // Intra-board link sits inside the board bracket.
                if c_idx + 1 < board.chips.len() {
                    spans.push(Span::styled(
                        " ←→ ",
                        Style::default().fg(board_color).add_modifier(Modifier::BOLD),
                    ));
                }
            }

            // Closing board bracket.
            spans.push(Span::styled("]", Style::default().fg(board_color)));

            // Inter-board separator.
            if b_idx + 1 < num_boards {
                if has_multi {
                    // Multi-chip carrier boards: Ethernet/PCIe bridge between units.
                    spans.push(Span::styled(
                        "  ═══  ",
                        Style::default().fg(colors::rgb(200, 160, 60)).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    // Independent PCIe cards: two spaces, no structural link implied.
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
    fn topology_minibar_line<B: TelemetryBackend>(&self, backend: &B, topo: &crate::animation::topology::BoardTopology) -> Line<'static> {
        use crate::animation::common::temp_to_hue;

        let devices = backend.devices();
        let n = devices.len();

        // Architecture summary for the label.
        let bh = devices.iter().filter(|d| matches!(d.architecture, crate::models::Architecture::Blackhole)).count();
        let wh = devices.iter().filter(|d| matches!(d.architecture, crate::models::Architecture::Wormhole)).count();
        let gs = devices.iter().filter(|d| matches!(d.architecture, crate::models::Architecture::Grayskull)).count();
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
                Style::default().fg(colors::rgb(180, 180, 200)).add_modifier(Modifier::BOLD),
            ),
        ];

        // One char per chip.  Cap display at 64 to avoid overflowing very wide
        // terminals; chips beyond that get a "+N" suffix.
        const MAX_BAR_CHIPS: usize = 64;
        let bar_n = n.min(MAX_BAR_CHIPS);

        for chip_idx in 0..bar_n {
            let device = &devices[chip_idx];
            let telem  = backend.telemetry(device.index);
            let power  = telem.map(|t| t.power_w()).unwrap_or(0.0);
            let temp   = telem.map(|t| t.temp_c()).unwrap_or(25.0);

            let act = (power / 80.0).clamp(0.0, 1.0);
            let ch  = if act > 0.75 { '█' } else if act > 0.50 { '▓' } else if act > 0.25 { '▒' } else { '░' };

            let hue   = temp_to_hue(temp);
            let color = hsv_to_rgb(hue, 0.85, 0.85 + act * 0.15);
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));

            // Faint board-boundary marker for multi-chip boards only.
            if topo.has_multi_chip_boards() && chip_idx + 1 < bar_n {
                let next_idx = chip_idx + 1;
                if !topo.same_board(chip_idx, next_idx) {
                    spans.push(Span::styled("|", Style::default().fg(colors::rgb(100, 90, 60))));
                }
            }
        }

        spans.push(Span::styled("]", Style::default().fg(colors::rgb(180, 180, 200)).add_modifier(Modifier::BOLD)));

        if n > MAX_BAR_CHIPS {
            spans.push(Span::styled(
                format!(" +{}", n - MAX_BAR_CHIPS),
                Style::default().fg(colors::rgb(140, 140, 140)),
            ));
        }

        Line::from(spans)
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
            if starfield_lines.len() >= 2 && i == starfield_lines.len() / 2 {
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
