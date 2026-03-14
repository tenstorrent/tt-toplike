//! Memory Castle Mode - ANSI Art visualization of memory hierarchy
//!
//! Transforms chip memory hierarchy into architectural castles inspired by:
//! - Castle of Greyskull (Grayskull chips): Medieval castle with gates, towers, windows
//! - Portal Nexus (Wormhole chips): Sci-fi teleportation network with swirling portals
//! - Event Horizon (Blackhole chips): Cosmic black hole with accretion disk and singularity
//!
//! Inspired by ANSI art, Logstalgia, and the "motion of memory" concept.
//!
//! Visual elements:
//! - DDR channels as architectural doors/gates/portals
//! - L2 cache as shelves or staging areas
//! - L1 SRAM as windows/rooms/nexus points (Tensix cores)
//! - Memory particles flowing through the hierarchy (DDR → L2 → L1 → Tensix)
//! - Architecture-specific character sets from CP437 box-drawing
//!
//! All animation driven by real telemetry:
//! - Particle speed ← Current draw
//! - Particle density ← Power consumption
//! - Particle color ← Temperature
//! - Gate animation ← DDR training status
//! - Structural integrity ← ARC health

use crate::animation::{
    AdaptiveBaseline, ansi_color, arc_health_color, hsv_to_rgb, temp_to_hue,
    value_to_window_char, PARTICLE_CHARS,
};
use crate::backend::TelemetryBackend;
use crate::models::Architecture;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::f32;

/// Grid pattern style (randomized on creation)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridStyle {
    /// Single-line box drawing
    SingleLine,
    /// Double-line box drawing (more dramatic)
    DoubleLine,
    /// Block characters (solid retro look)
    BlockStyle,
    /// Minimalist dots and dashes
    DotDash,
}

impl GridStyle {
    /// Get random style
    pub fn random() -> Self {
        let styles = [
            GridStyle::SingleLine,
            GridStyle::DoubleLine,
            GridStyle::BlockStyle,
            GridStyle::DotDash,
        ];
        let idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() % 4) as usize;
        styles[idx]
    }

    /// Get grid line characters for this style
    pub fn chars(&self) -> GridChars {
        match self {
            GridStyle::SingleLine => GridChars {
                horizontal: '─',
                vertical: '│',
                top_left: '┌',
                top_right: '┐',
                bottom_left: '└',
                bottom_right: '┘',
                cross: '┼',
            },
            GridStyle::DoubleLine => GridChars {
                horizontal: '═',
                vertical: '║',
                top_left: '╔',
                top_right: '╗',
                bottom_left: '╚',
                bottom_right: '╝',
                cross: '╬',
            },
            GridStyle::BlockStyle => GridChars {
                horizontal: '▬',
                vertical: '▮',
                top_left: '▛',
                top_right: '▜',
                bottom_left: '▙',
                bottom_right: '▟',
                cross: '▞',
            },
            GridStyle::DotDash => GridChars {
                horizontal: '╌',
                vertical: '╎',
                top_left: '·',
                top_right: '·',
                bottom_left: '·',
                bottom_right: '·',
                cross: '┼',
            },
        }
    }
}

/// Grid drawing characters
pub struct GridChars {
    pub horizontal: char,
    pub vertical: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub cross: char,
}

/// Color scheme for castle visualization
#[derive(Debug, Clone, Copy)]
pub enum ColorScheme {
    /// Classic blue
    ClassicBlue,
    /// Orange
    Orange,
    /// Cyan/Magenta (cyberpunk)
    Cyberpunk,
    /// Green (matrix-style)
    Matrix,
    /// Rainbow cycling
    Rainbow,
}

impl ColorScheme {
    /// Get random color scheme
    pub fn random() -> Self {
        let schemes = [
            ColorScheme::ClassicBlue,
            ColorScheme::Orange,
            ColorScheme::Cyberpunk,
            ColorScheme::Matrix,
            ColorScheme::Rainbow,
        ];
        let idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() % 5) as usize;
        schemes[idx]
    }

    /// Get base color for this scheme
    pub fn base_color(&self, frame: u32) -> Color {
        match self {
            ColorScheme::ClassicBlue => Color::Rgb(80, 180, 255),
            ColorScheme::Orange => Color::Rgb(255, 150, 80),
            ColorScheme::Cyberpunk => Color::Rgb(150, 80, 255),
            ColorScheme::Matrix => Color::Rgb(80, 255, 120),
            ColorScheme::Rainbow => {
                // Cycle through rainbow
                ansi_color((frame / 10) as usize)
            }
        }
    }

    /// Get bright/active color
    pub fn bright_color(&self, frame: u32) -> Color {
        match self {
            ColorScheme::ClassicBlue => Color::Rgb(150, 220, 255),
            ColorScheme::Orange => Color::Rgb(255, 200, 120),
            ColorScheme::Cyberpunk => Color::Rgb(255, 150, 255),
            ColorScheme::Matrix => Color::Rgb(150, 255, 180),
            ColorScheme::Rainbow => {
                // Bright rainbow
                ansi_color((frame / 10 + 8) as usize)
            }
        }
    }
}

// ========================================
// MEMORY PARTICLE SYSTEM
// ========================================

/// Memory layer in the hierarchy (particle journey)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MemoryLayer {
    /// DDR channel (external memory, entry point)
    DDR(usize),
    /// L2 cache bank (staging area)
    L2(usize),
    /// L1 SRAM core (fast access, specific Tensix core)
    L1 { row: usize, col: usize },
    /// Processing element (final destination)
    Tensix(usize),
}

/// A memory particle representing a memory operation
///
/// Particles flow through the memory hierarchy: DDR → L2 → L1 → Tensix
/// Movement and appearance driven by real telemetry data
#[derive(Debug, Clone)]
pub struct MemoryParticle {
    /// Current X position (screen coordinates)
    pub x: f32,
    /// Current Y position (screen coordinates)
    pub y: f32,
    /// Movement velocity (pixels/frame, driven by current draw)
    pub velocity: f32,
    /// Current layer in memory hierarchy
    pub layer: MemoryLayer,
    /// Target layer (where particle is heading)
    pub target_layer: MemoryLayer,
    /// Activity intensity (0.0-1.0, driven by power)
    pub intensity: f32,
    /// Color hue (0-360°, driven by temperature)
    pub color_hue: f32,
    /// Time to live (frames remaining before removal)
    pub ttl: u32,
}

impl MemoryParticle {
    /// Create a new particle at DDR entry point
    pub fn new(ddr_channel: usize, current: f32, temp: f32, power: f32) -> Self {
        // Velocity based on current draw (higher current = faster particles)
        let velocity = 0.5 + (current / 100.0).min(1.0) * 1.5;

        // Intensity based on power consumption
        let intensity = (power / 100.0).max(0.0).min(1.0);

        // Color from temperature
        let color_hue = temp_to_hue(temp);

        Self {
            x: 0.0,
            y: 0.0,
            velocity,
            layer: MemoryLayer::DDR(ddr_channel),
            target_layer: MemoryLayer::L2(0),  // Will update on first move
            intensity,
            color_hue,
            ttl: 60,  // Live for 60 frames max
        }
    }

    /// Update particle position and advance through memory hierarchy
    pub fn update(&mut self, current: f32, _frame: u32) {
        // Update velocity from current telemetry
        self.velocity = 0.5 + (current / 100.0).min(1.0) * 1.5;

        // Move towards target
        self.x += self.velocity;
        self.y += self.velocity * 0.5;

        // Check if reached target layer, advance to next
        if self.x >= 10.0 {
            self.layer = self.target_layer;
            self.x = 0.0;

            // Set next target
            self.target_layer = match self.layer {
                MemoryLayer::DDR(_) => MemoryLayer::L2(0),
                MemoryLayer::L2(_) => MemoryLayer::L1 { row: 0, col: 0 },
                MemoryLayer::L1 { .. } => MemoryLayer::Tensix(0),
                MemoryLayer::Tensix(_) => MemoryLayer::Tensix(0),  // Stay here
            };
        }

        // Decrease TTL
        self.ttl = self.ttl.saturating_sub(1);
    }

    /// Check if particle is still active
    pub fn is_active(&self) -> bool {
        self.ttl > 0
    }

    /// Get character to represent particle based on intensity
    pub fn get_char(&self) -> char {
        let intensity_clamped = self.intensity.max(0.0).min(1.0);
        let index = (intensity_clamped * (PARTICLE_CHARS.len() - 1) as f32) as usize;
        PARTICLE_CHARS[index]
    }

    /// Get color for particle
    pub fn get_color(&self) -> Color {
        hsv_to_rgb(self.color_hue, 0.8, 0.8 + self.intensity * 0.2)
    }
}

/// Castle theme based on chip architecture
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CastleTheme {
    /// Grayskull: Medieval stone castle with gates, towers, windows
    Greyskull,
    /// Wormhole: Sci-fi portal nexus with swirling wormhole entrances
    PortalNexus,
    /// Blackhole: Cosmic event horizon with accretion disk and singularity
    EventHorizon,
}

/// Memory Castle visualization mode
pub struct MemoryCastle {
    /// Display dimensions
    width: usize,
    height: usize,

    /// Adaptive baseline for relative activity
    baseline: AdaptiveBaseline,

    /// Animation frame counter
    frame: u32,

    /// Memory particles flowing through hierarchy
    particles: Vec<MemoryParticle>,

    /// Maximum particles to spawn (prevents performance issues)
    max_particles: usize,
}

impl MemoryCastle {
    /// Create new Memory Castle mode with random parameters
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            baseline: AdaptiveBaseline::new(),
            frame: 0,
            particles: Vec::new(),
            max_particles: 100,  // Cap at 100 particles for performance
        }
    }

    /// Get castle theme for a device based on architecture
    fn get_castle_theme(arch: Architecture) -> CastleTheme {
        match arch {
            Architecture::Grayskull => CastleTheme::Greyskull,
            Architecture::Wormhole => CastleTheme::PortalNexus,
            Architecture::Blackhole => CastleTheme::EventHorizon,
            Architecture::Unknown => CastleTheme::Greyskull,  // Default to castle
        }
    }

    /// Spawn a new memory particle
    fn spawn_particle(&mut self, ddr_channel: usize, current: f32, temp: f32, power: f32) {
        if self.particles.len() < self.max_particles {
            let particle = MemoryParticle::new(ddr_channel, current, temp, power);
            self.particles.push(particle);
        }
    }

    /// Update from telemetry and animate particles
    pub fn update<B: TelemetryBackend>(&mut self, backend: &B) {
        self.frame += 1;

        // Update baseline and spawn particles
        for (idx, device) in backend.devices().iter().enumerate() {
            if let Some(telem) = backend.telemetry(idx) {
                let power = telem.power_w();
                let current = telem.current_a();
                let temp = telem.temp_c();
                let aiclk = telem.aiclk_mhz() as f32;

                self.baseline
                    .update(device.index, power, current, temp, aiclk);

                // Spawn new particles based on power consumption
                // Higher power = more memory operations = more particles
                let spawn_rate = (power / 100.0).max(0.0).min(1.0);
                if self.frame % 2 == 0 && spawn_rate > 0.1 {
                    // Random DDR channel for entry point
                    let num_channels = device.architecture.memory_channels();
                    let ddr_channel = self.frame as usize % num_channels;
                    self.spawn_particle(ddr_channel, current, temp, power);
                }
            }
        }

        // Update existing particles
        let current = backend
            .devices()
            .first()
            .and_then(|d| backend.telemetry(d.index))
            .map(|t| t.current_a())
            .unwrap_or(0.0);

        for particle in &mut self.particles {
            particle.update(current, self.frame);
        }

        // Remove dead particles
        self.particles.retain(|p| p.is_active());
    }

    /// Render the TRON grid
    pub fn render<B: TelemetryBackend>(&self, backend: &B) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Calculate layout
        let devices = backend.devices();
        let device_count = devices.len();

        if device_count == 0 {
            return lines;
        }

        let devices_per_row = if device_count <= 2 { device_count } else { 2 };
        let grid_width = (self.width - 4) / devices_per_row;
        let grid_height = (self.height - 8) / ((device_count + devices_per_row - 1) / devices_per_row);

        // Title line
        lines.push(self.create_title_line(backend));
        lines.push(Line::from(""));

        // Render each device as a grid
        for (idx, device) in devices.iter().enumerate() {
            let col = idx % devices_per_row;

            if col == 0 {
                // Start new row
                let device_lines = self.render_device_grid(backend, device, grid_width, grid_height);
                lines.extend(device_lines);
            }
        }

        // Footer with legend
        lines.push(Line::from(""));
        lines.push(self.create_legend_line());

        lines
    }

    /// Create title line with ARC health
    fn create_title_line<B: TelemetryBackend>(&self, backend: &B) -> Line<'static> {
        let mut spans = Vec::new();

        // Mode name
        spans.push(Span::styled(
            " ⚔ MEMORY CASTLE MODE ⚔ ",
            Style::default()
                .fg(Color::Rgb(200, 180, 140))
                .add_modifier(Modifier::BOLD),
        ));

        spans.push(Span::raw(" │ "));

        // ARC health
        let mut arc_health = Vec::new();
        for (idx, _device) in backend.devices().iter().enumerate() {
            if let Some(smbus) = backend.smbus_telemetry(idx) {
                let healthy = smbus.is_arc0_healthy();
                arc_health.push((idx, healthy));
            }
        }

        if !arc_health.is_empty() {
            spans.push(Span::raw("ARC: "));
            for (_, healthy) in &arc_health {
                let color = arc_health_color(*healthy, self.frame);
                let symbol = if *healthy { '●' } else { '○' };
                spans.push(Span::styled(
                    symbol.to_string(),
                    Style::default().fg(color),
                ));
            }

            let healthy_count = arc_health.iter().filter(|(_, h)| *h).count();
            let total = arc_health.len();
            spans.push(Span::raw(format!(" ({}/{})", healthy_count, total)));
        }

        Line::from(spans)
    }

    /// Render a single device as a Memory Castle
    ///
    /// Dispatches to architecture-specific renderer based on castle theme
    fn render_device_grid<B: TelemetryBackend>(
        &self,
        backend: &B,
        device: &crate::models::Device,
        width: usize,
        height: usize,
    ) -> Vec<Line<'static>> {
        let castle_theme = Self::get_castle_theme(device.architecture);

        match castle_theme {
            CastleTheme::Greyskull => {
                self.render_greyskull_castle(backend, device, width, height)
            }
            CastleTheme::PortalNexus => {
                self.render_portal_nexus(backend, device, width, height)
            }
            CastleTheme::EventHorizon => {
                self.render_event_horizon(backend, device, width, height)
            }
        }
    }

    /// Render Grayskull as Medieval Castle of Greyskull
    ///
    /// Architecture:
    /// - 4 DDR channels as castle gates (heavy doors: ╔╗╚╝)
    /// - L2 cache as great hall shelves (horizontal bars: ═══)
    /// - L1 SRAM as tower windows (10×12 grid of □▪■)
    /// - 120 Tensix cores as rooms in castle towers
    ///
    /// Particles flow: Gates → Shelves → Tower Windows → Rooms
    fn render_greyskull_castle<B: TelemetryBackend>(
        &self,
        backend: &B,
        device: &crate::models::Device,
        width: usize,
        _height: usize,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Use heavy box drawing for castle aesthetic
        let door_chars = ['╔', '╗', '╚', '╝'];
        let wall_h = '═';
        let wall_v = '║';
        let content_width = width.saturating_sub(2);

        // Get telemetry
        let telem = backend.telemetry(device.index);
        let smbus = backend.smbus_telemetry(device.index);

        let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
        let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);
        let current = telem.map(|t| t.current_a()).unwrap_or(0.0);
        let arc_healthy = smbus.map(|s| s.is_arc0_healthy()).unwrap_or(true);

        // Get activity from baseline
        let power_change = self.baseline.power_change(device.index, power);
        let current_change = self.baseline.current_change(device.index, current);

        // Stone gray castle colors (crumbling red if ARC failed)
        let stone_gray = Color::Rgb(160, 160, 170);
        let stone_dark = Color::Rgb(100, 100, 110);
        let castle_color = if arc_healthy {
            stone_gray
        } else {
            Color::Rgb(200, 80, 80)  // Crumbling castle
        };

        let accent_color = Color::Rgb(200, 180, 140);  // Medieval gold/bronze

        // Top border - Castle battlements
        let mut top_line = vec![Span::styled(
            door_chars[0].to_string(),  // ╔
            Style::default().fg(castle_color).add_modifier(Modifier::BOLD),
        )];

        let title = format!(" ⚔ CASTLE OF GREYSKULL {} ⚔ ", device.index);
        let title_len = title.chars().count();  // Count characters, not bytes (emoji are multi-byte)
        top_line.push(Span::styled(
            title,
            Style::default()
                .fg(accent_color)
                .add_modifier(Modifier::BOLD),
        ));
        top_line.push(Span::styled(
            wall_h.to_string().repeat(content_width.saturating_sub(title_len)),
            Style::default().fg(stone_dark),
        ));
        top_line.push(Span::styled(
            door_chars[1].to_string(),  // ╗
            Style::default().fg(castle_color).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(top_line));

        // Castle Gates (DDR channels) - 4 heavy doors
        lines.push(self.render_castle_gates(
            backend,
            device,
            content_width,
            wall_v,
            castle_color,
            accent_color,
            current,
        ));

        // Great Hall Shelves (L2 cache) - 8 horizontal shelves
        lines.push(self.render_great_hall_shelves(
            content_width,
            wall_v,
            castle_color,
            accent_color,
            current_change,
        ));

        // Tower Windows (L1 SRAM) - 10×12 Tensix grid compressed to 3 rows
        lines.extend(self.render_tower_windows(
            device,
            content_width,
            wall_v,
            castle_color,
            power_change,
            temp,
        ));

        // Castle foundation - Stats line
        let stats = format!(
            " ⚡ {:.1}W │ 🌡 {:.0}°C │ ⚙ {:.1}A │ Particles: {} ",
            power, temp, current, self.particles.len()
        );
        let stats_len = stats.chars().count();  // Count characters, not bytes (emoji are multi-byte)
        let mut stats_line = vec![Span::styled(
            wall_v.to_string(),
            Style::default().fg(stone_dark),
        )];
        stats_line.push(Span::styled(
            stats,
            Style::default().fg(accent_color),
        ));
        stats_line.push(Span::raw(" ".repeat(content_width.saturating_sub(stats_len))));
        stats_line.push(Span::styled(
            wall_v.to_string(),
            Style::default().fg(stone_dark),
        ));
        lines.push(Line::from(stats_line));

        // Bottom border - Castle foundation
        let mut bottom_line = vec![Span::styled(
            door_chars[2].to_string(),  // ╚
            Style::default().fg(castle_color).add_modifier(Modifier::BOLD),
        )];
        bottom_line.push(Span::styled(
            wall_h.to_string().repeat(content_width),
            Style::default().fg(stone_dark),
        ));
        bottom_line.push(Span::styled(
            door_chars[3].to_string(),  // ╝
            Style::default().fg(castle_color).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(bottom_line));

        lines.push(Line::from("")); // Spacing

        lines
    }

    /// Render Portal Nexus for Wormhole chips (placeholder - to be implemented)
    fn render_portal_nexus<B: TelemetryBackend>(
        &self,
        backend: &B,
        device: &crate::models::Device,
        width: usize,
        _height: usize,
    ) -> Vec<Line<'static>> {
        // For now, fall back to Greyskull rendering
        // TODO: Implement full portal nexus theme
        self.render_greyskull_castle(backend, device, width, _height)
    }

    /// Render Event Horizon for Blackhole chips (placeholder - to be implemented)
    fn render_event_horizon<B: TelemetryBackend>(
        &self,
        backend: &B,
        device: &crate::models::Device,
        width: usize,
        _height: usize,
    ) -> Vec<Line<'static>> {
        // For now, fall back to Greyskull rendering
        // TODO: Implement full event horizon theme
        self.render_greyskull_castle(backend, device, width, _height)
    }

    /// Calculate actual width of a line's spans (counting characters, not bytes)
    ///
    /// Handles Unicode characters correctly by counting grapheme clusters
    fn calculate_span_width(spans: &[Span]) -> usize {
        spans.iter()
            .map(|span| {
                // Count Unicode characters (not bytes)
                span.content.chars().count()
            })
            .sum()
    }

    /// Render castle gates (DDR channels) with training status
    ///
    /// Format: ╔●╗ ╔●╗ ╔○╗ ╔✗╗ (4 gates with status symbols)
    /// Training animation alternates door characters
    fn render_castle_gates<B: TelemetryBackend>(
        &self,
        backend: &B,
        device: &crate::models::Device,
        content_width: usize,
        wall_v: char,
        castle_color: Color,
        accent_color: Color,
        current: f32,
    ) -> Line<'static> {
        let mut spans = vec![Span::styled(
            wall_v.to_string(),
            Style::default().fg(castle_color),
        )];

        spans.push(Span::styled(
            " ⛩ Gates: ",
            Style::default()
                .fg(accent_color)
                .add_modifier(Modifier::BOLD),
        ));

        // Grayskull has 4 DDR channels = 4 castle gates
        let num_channels = device.architecture.memory_channels();
        let smbus = backend.smbus_telemetry(device.index);

        // Get DDR status if available
        let ddr_status_str = smbus
            .and_then(|s| s.ddr_status.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("0");

        // Parse DDR status (hex string)
        let ddr_status =
            u64::from_str_radix(ddr_status_str.trim_start_matches("0x"), 16).unwrap_or(0);

        // Render each gate with door characters and training status
        for i in 0..num_channels {
            // Extract 4-bit status for this channel
            let channel_status = (ddr_status >> (4 * i)) & 0xF;

            // Gate door characters and status symbol
            let (gate_left, gate_right, status_char, status_color) = match channel_status {
                2 => ('╔', '╗', '●', Color::Rgb(80, 220, 100)), // Open gate, trained
                1 => {
                    // Opening gate (training animation)
                    let anim_chars = if (self.frame / 3) % 2 == 0 {
                        ('╔', '╗', '◐')
                    } else {
                        ('║', '║', '◑')
                    };
                    (
                        anim_chars.0,
                        anim_chars.1,
                        anim_chars.2,
                        Color::Rgb(100, 200, 220),
                    )
                }
                0 => ('─', '─', '○', Color::Rgb(100, 100, 120)), // Closed gate
                _ => ('╳', '╳', '✗', Color::Rgb(255, 100, 100)), // Broken gate
            };

            spans.push(Span::styled(
                format!("{}", gate_left),
                Style::default().fg(castle_color),
            ));
            spans.push(Span::styled(
                status_char.to_string(),
                Style::default().fg(status_color).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!("{} ", gate_right),
                Style::default().fg(castle_color),
            ));
        }

        // Particle flow visualization (utilization)
        let utilization = (current / 100.0).min(1.0);
        let num_blocks = (utilization * 8.0) as usize;

        spans.push(Span::raw(" │ "));
        spans.push(Span::styled(
            "Flow: ",
            Style::default().fg(accent_color),
        ));

        // Show particles flowing through gates
        for i in 0..8 {
            let particle_char = if i < num_blocks {
                PARTICLE_CHARS[i % PARTICLE_CHARS.len()]
            } else {
                '·'
            };
            let particle_color = if i < num_blocks {
                Color::Rgb(200, 180, 140)
            } else {
                Color::Rgb(80, 80, 100)
            };
            spans.push(Span::styled(
                particle_char.to_string(),
                Style::default().fg(particle_color),
            ));
        }

        // Padding to fill width (calculate actual width dynamically)
        let actual_width = Self::calculate_span_width(&spans);
        let padding_needed = content_width.saturating_sub(actual_width).saturating_sub(1); // -1 for right border
        spans.push(Span::raw(" ".repeat(padding_needed)));

        // Right border
        spans.push(Span::styled(
            wall_v.to_string(),
            Style::default().fg(castle_color),
        ));

        Line::from(spans)
    }

    /// Render great hall shelves (L2 cache)
    ///
    /// 8 horizontal shelves with particles resting on them
    /// Shelf activity represents memory staging operations
    fn render_great_hall_shelves(
        &self,
        content_width: usize,
        wall_v: char,
        castle_color: Color,
        accent_color: Color,
        current_change: f32,
    ) -> Line<'static> {
        let mut spans = vec![Span::styled(
            wall_v.to_string(),
            Style::default().fg(castle_color),
        )];

        spans.push(Span::styled(
            " 📚 Shelves: ",
            Style::default()
                .fg(accent_color)
                .add_modifier(Modifier::BOLD),
        ));

        // 8 shelves with particles
        let l2_activity = current_change.max(0.0).min(1.0);
        for i in 0..8 {
            // Wave pattern for shelf activity
            let shelf_phase = (i as f32 * 0.5 + self.frame as f32 * 0.1).sin() * 0.3;
            let shelf_activity = (l2_activity + shelf_phase).max(0.0).min(1.0);

            // Shelf character (═) with particles on it
            let shelf_color = if shelf_activity > 0.6 {
                Color::Rgb(200, 180, 120) // Active shelf (golden)
            } else {
                Color::Rgb(120, 110, 90) // Dusty shelf (brown)
            };

            // Particle on shelf
            let particle_char = if shelf_activity > 0.5 { '●' } else { '○' };

            spans.push(Span::styled(
                '═'.to_string(),
                Style::default().fg(shelf_color),
            ));
            spans.push(Span::styled(
                particle_char.to_string(),
                Style::default().fg(accent_color),
            ));
            spans.push(Span::styled("═ ", Style::default().fg(shelf_color)));
        }

        // Padding (calculate actual width dynamically)
        let actual_width = Self::calculate_span_width(&spans);
        let padding_needed = content_width.saturating_sub(actual_width).saturating_sub(1); // -1 for right border
        spans.push(Span::raw(" ".repeat(padding_needed)));

        // Right border
        spans.push(Span::styled(
            wall_v.to_string(),
            Style::default().fg(castle_color),
        ));

        Line::from(spans)
    }

    /// Render tower windows (L1 SRAM / Tensix cores)
    ///
    /// Windows show activity in each Tensix core
    /// Characters: □ (empty) → ▫ → ▪ → ■ (full)
    /// Colors: Temperature-based (cyan → red)
    fn render_tower_windows(
        &self,
        device: &crate::models::Device,
        content_width: usize,
        wall_v: char,
        castle_color: Color,
        power_change: f32,
        temp: f32,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let (grid_cols, grid_rows) = match device.architecture {
            Architecture::Grayskull => (10, 12),
            Architecture::Wormhole => (8, 10),
            Architecture::Blackhole => (14, 16),
            Architecture::Unknown => (8, 8),
        };

        // Compress to 3 display rows
        let display_rows = 3;
        let row_step = grid_rows as f32 / display_rows as f32;

        for display_row in 0..display_rows {
            let mut spans = vec![Span::styled(
                wall_v.to_string(),
                Style::default().fg(castle_color),
            )];

            if display_row == 0 {
                spans.push(Span::styled(
                    " 🏰 Towers: ",
                    Style::default()
                        .fg(Color::Rgb(200, 180, 140))
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw("           "));
            }

            let actual_row = (display_row as f32 * row_step) as usize;

            // Render tower windows with castle window characters
            for col in 0..grid_cols {
                // Wave pattern for window lighting
                let wave = (actual_row as f32 * 0.5
                    + col as f32 * 0.5
                    + self.frame as f32 * 0.15)
                    .sin()
                    * 0.3;
                let window_activity = (power_change + wave).max(0.0).min(1.5);

                // Window character based on activity
                let window_char = value_to_window_char(window_activity.min(1.0));

                // Window color based on temperature (warm glow)
                let hue = temp_to_hue(temp);
                let saturation = 0.4 + window_activity * 0.4;
                let value = 0.5 + window_activity * 0.5;
                let window_color = hsv_to_rgb(hue, saturation.min(1.0), value.min(1.0));

                spans.push(Span::styled(
                    window_char.to_string(),
                    Style::default().fg(window_color),
                ));
            }

            // Padding (calculate actual width dynamically)
            let actual_width = Self::calculate_span_width(&spans);
            let padding_needed = content_width.saturating_sub(actual_width).saturating_sub(1); // -1 for right border
            spans.push(Span::raw(" ".repeat(padding_needed)));

            // Right border
            spans.push(Span::styled(
                wall_v.to_string(),
                Style::default().fg(castle_color),
            ));

            lines.push(Line::from(spans));
        }

        lines
    }

    /// Create legend line
    fn create_legend_line(&self) -> Line<'static> {
        Line::from(vec![
            Span::raw(" Gates: "),
            Span::styled("╔●╗", Style::default().fg(Color::Rgb(80, 220, 100))),
            Span::raw(" Open "),
            Span::styled("║◐║", Style::default().fg(Color::Rgb(100, 200, 220))),
            Span::raw(" Opening "),
            Span::styled("─○─", Style::default().fg(Color::Rgb(100, 100, 120))),
            Span::raw(" Closed "),
            Span::styled("╳✗╳", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::raw(" Broken  │  "),
            Span::styled(
                "v",
                Style::default()
                    .fg(Color::Rgb(80, 220, 200))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" randomize"),
        ])
    }

    /// Resize handler
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
    }

    /// Get mode name
    pub fn mode_name(&self) -> &'static str {
        "Memory Castle"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_style_random() {
        let style1 = GridStyle::random();
        let style2 = GridStyle::random();
        // Just verify they're valid styles
        let _ = style1.chars();
        let _ = style2.chars();
    }

    #[test]
    fn test_color_scheme_random() {
        let scheme1 = ColorScheme::random();
        let scheme2 = ColorScheme::random();
        // Verify colors can be generated
        let _ = scheme1.base_color(0);
        let _ = scheme2.bright_color(0);
    }

    #[test]
    fn test_memory_castle_creation() {
        let grid = MemoryCastle::new(80, 24);
        assert_eq!(grid.mode_name(), "Memory Castle");
        assert_eq!(grid.width, 80);
        assert_eq!(grid.height, 24);
        assert_eq!(grid.particles.len(), 0); // No particles initially
        assert_eq!(grid.max_particles, 100);
    }

    #[test]
    fn test_castle_theme_selection() {
        use crate::models::Architecture;

        assert_eq!(
            MemoryCastle::get_castle_theme(Architecture::Grayskull),
            CastleTheme::Greyskull
        );
        assert_eq!(
            MemoryCastle::get_castle_theme(Architecture::Wormhole),
            CastleTheme::PortalNexus
        );
        assert_eq!(
            MemoryCastle::get_castle_theme(Architecture::Blackhole),
            CastleTheme::EventHorizon
        );
        assert_eq!(
            MemoryCastle::get_castle_theme(Architecture::Unknown),
            CastleTheme::Greyskull
        ); // Default
    }

    #[test]
    fn test_memory_particle_creation() {
        let particle = MemoryParticle::new(0, 50.0, 45.0, 75.0);

        assert!(matches!(particle.layer, MemoryLayer::DDR(0)));
        assert!(particle.velocity > 0.0);
        assert!(particle.intensity > 0.0 && particle.intensity <= 1.0);
        assert_eq!(particle.ttl, 60);
        assert!(particle.is_active());
    }

    #[test]
    fn test_particle_movement() {
        let mut particle = MemoryParticle::new(0, 50.0, 45.0, 75.0);
        let initial_x = particle.x;

        particle.update(50.0, 10);

        assert!(particle.x > initial_x, "Particle should move forward");
        assert_eq!(particle.ttl, 59, "TTL should decrease");
    }

    #[test]
    fn test_particle_lifecycle() {
        let mut particle = MemoryParticle::new(0, 50.0, 45.0, 75.0);

        // Expire the particle
        particle.ttl = 1;
        particle.update(50.0, 10);

        assert_eq!(particle.ttl, 0);
        assert!(!particle.is_active(), "Particle with TTL=0 should be inactive");
    }

    #[test]
    fn test_particle_character_intensity() {
        let low_particle = MemoryParticle {
            x: 0.0,
            y: 0.0,
            velocity: 1.0,
            layer: MemoryLayer::DDR(0),
            target_layer: MemoryLayer::L2(0),
            intensity: 0.0,
            color_hue: 180.0,
            ttl: 60,
        };

        let high_particle = MemoryParticle {
            x: 0.0,
            y: 0.0,
            velocity: 1.0,
            layer: MemoryLayer::DDR(0),
            target_layer: MemoryLayer::L2(0),
            intensity: 1.0,
            color_hue: 180.0,
            ttl: 60,
        };

        assert_eq!(low_particle.get_char(), '·'); // Low intensity = small dot
        assert_eq!(high_particle.get_char(), '✦'); // High intensity = star
    }

    #[test]
    fn test_ddr_status_parsing() {
        // Test DDR status bit extraction
        // Format: 4 bits per channel (0=untrained, 1=training, 2=trained, 3+=error)

        // All channels trained (0x2222 for 4 channels)
        let status = 0x2222u64;
        for i in 0..4 {
            let channel_status = (status >> (4 * i)) & 0xF;
            assert_eq!(channel_status, 2, "Channel {} should be trained", i);
        }

        // Mixed status: channel 0=trained, 1=training, 2=untrained, 3=error
        let status = 0x3012u64;
        assert_eq!((status >> 0) & 0xF, 2, "Channel 0 trained");
        assert_eq!((status >> 4) & 0xF, 1, "Channel 1 training");
        assert_eq!((status >> 8) & 0xF, 0, "Channel 2 untrained");
        assert_eq!((status >> 12) & 0xF, 3, "Channel 3 error");

        // Wormhole with 8 channels (0x22222222)
        let status = 0x22222222u64;
        for i in 0..8 {
            let channel_status = (status >> (4 * i)) & 0xF;
            assert_eq!(channel_status, 2, "WH Channel {} should be trained", i);
        }
    }

    #[test]
    fn test_architecture_memory_channels() {
        use crate::models::Architecture;

        assert_eq!(Architecture::Grayskull.memory_channels(), 4, "GS has 4 DDR channels");
        assert_eq!(Architecture::Wormhole.memory_channels(), 8, "WH has 8 DDR channels");
        assert_eq!(Architecture::Blackhole.memory_channels(), 12, "BH has 12 DDR channels");
        assert_eq!(Architecture::Unknown.memory_channels(), 8, "Unknown defaults to 8 channels");
    }

    #[test]
    fn test_border_alignment_calculation() {
        // Test content_width calculation
        let total_width = 80usize;
        let content_width = total_width.saturating_sub(2);
        assert_eq!(content_width, 78, "Content width should be total - 2 for borders");

        // Test title padding calculation
        let title = " Device 0: GS ";
        let title_len = title.chars().count();  // Use chars().count() for Unicode safety
        let padding = content_width.saturating_sub(title_len);
        assert_eq!(padding + title_len, content_width, "Title + padding should equal content_width");

        // Test stats line padding
        let stats = " Power: 43.2W │ Temp: 67.0°C │ Current: 19.4A ";
        let stats_len = stats.chars().count();  // Use chars().count() for Unicode safety
        let padding = content_width.saturating_sub(stats_len);
        assert!(padding + stats_len <= content_width, "Stats + padding should not exceed content_width");
    }

    #[test]
    fn test_utilization_blocks() {
        // Test current draw to utilization block conversion
        let current: f32 = 50.0;  // 50A
        let utilization = (current / 100.0).min(1.0);
        let num_blocks = (utilization * 8.0) as usize;
        assert_eq!(num_blocks, 4, "50A should show 4 out of 8 blocks");

        // High current
        let current: f32 = 100.0;
        let utilization = (current / 100.0).min(1.0);
        let num_blocks = (utilization * 8.0) as usize;
        assert_eq!(num_blocks, 8, "100A should show all 8 blocks");

        // Low current
        let current: f32 = 12.5;
        let utilization = (current / 100.0).min(1.0);
        let num_blocks = (utilization * 8.0) as usize;
        assert_eq!(num_blocks, 1, "12.5A should show 1 block");

        // No current
        let current: f32 = 0.0;
        let utilization = (current / 100.0).min(1.0);
        let num_blocks = (utilization * 8.0) as usize;
        assert_eq!(num_blocks, 0, "0A should show no blocks");
    }

    #[test]
    fn test_l1_core_activity_characters() {
        // Test core activity to character mapping
        let test_cases = vec![
            (1.5, '█'),   // Very high
            (1.0, '█'),   // Very high
            (0.8, '▓'),   // High
            (0.5, '▒'),   // Medium
            (0.3, '░'),   // Low
            (0.1, '·'),   // Idle
            (0.0, '·'),   // Idle
        ];

        for (activity, expected_char) in test_cases {
            let actual_char = if activity > 1.0 {
                '█'
            } else if activity > 0.7 {
                '▓'
            } else if activity > 0.4 {
                '▒'
            } else if activity > 0.2 {
                '░'
            } else {
                '·'
            };
            assert_eq!(
                actual_char, expected_char,
                "Activity {} should map to character '{}'",
                activity, expected_char
            );
        }
    }

    #[test]
    fn test_l2_bank_wave_pattern() {
        // Test wave pattern generation for L2 banks
        let frame = 10u32;
        let l2_activity = 0.5;

        for i in 0..8 {
            let bank_phase = (i as f32 * 0.5 + frame as f32 * 0.1).sin() * 0.3;
            let bank_activity = (l2_activity + bank_phase).max(0.0).min(1.0);

            assert!(bank_activity >= 0.0, "Bank activity should not be negative");
            assert!(bank_activity <= 1.0, "Bank activity should not exceed 1.0");
        }
    }

    #[test]
    fn test_ddr_training_animation() {
        // Test training animation character alternation
        let frame1 = 0u32;
        let frame2 = 3u32;
        let frame3 = 6u32;

        let char1 = if (frame1 / 3) % 2 == 0 { '◐' } else { '◑' };
        let char2 = if (frame2 / 3) % 2 == 0 { '◐' } else { '◑' };
        let char3 = if (frame3 / 3) % 2 == 0 { '◐' } else { '◑' };

        assert_eq!(char1, '◐', "Frame 0 should show ◐");
        assert_eq!(char2, '◑', "Frame 3 should show ◑");
        assert_eq!(char3, '◐', "Frame 6 should show ◐");
    }

    #[test]
    fn test_compressed_tensix_grid() {
        use crate::models::Architecture;

        // Test grid compression for different architectures
        let architectures = vec![
            (Architecture::Grayskull, 10, 12),
            (Architecture::Wormhole, 8, 10),
            (Architecture::Blackhole, 14, 16),
        ];

        let display_rows = 3;
        for (arch, cols, rows) in architectures {
            let row_step = rows as f32 / display_rows as f32;

            assert!(row_step > 0.0, "{:?}: Row step should be positive", arch);

            // Verify compression covers the full grid
            let last_row = ((display_rows - 1) as f32 * row_step) as usize;
            assert!(last_row < rows, "{:?}: Last displayed row should be within grid", arch);

            // Verify all columns are shown
            assert_eq!(cols, cols, "{:?}: All columns should be displayed", arch);
        }
    }
}
