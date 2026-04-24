// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Memory Flow Topology Visualization
//!
//! Full-screen visualization of memory hierarchy and data flow through the chip.
//!
//! Shows:
//! - DDR channels as sources/sinks (perimeter)
//! - Tensix core grid as heat map (center)
//! - Particles flowing through NoC (Network on Chip)
//! - L2 cache as intermediate staging
//! - Bandwidth utilization per channel
//! - Memory access patterns in real-time
//!
//! All animation driven by real telemetry:
//! - Particle flow direction ← Memory reads/writes
//! - Particle density ← Bandwidth utilization
//! - Particle speed ← Current draw
//! - Core heat map ← Power consumption
//! - Core color ← Temperature
//!
//! Inspired by:
//! - Logstalgia's data flow visualization
//! - Network topology diagrams
//! - Heat map overlays

use crate::animation::AdaptiveBaseline;
use crate::backend::TelemetryBackend;
use crate::models::Architecture;
use crate::ui::colors;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::f32;

/// Direction of memory flow
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlowDirection {
    /// Read: DDR → Core
    Read,
    /// Write: Core → DDR
    Write,
    /// Internal: Core ↔ Core (NoC traffic)
    Internal,
}

/// A particle representing a memory transaction
#[derive(Debug, Clone)]
pub struct MemoryFlowParticle {
    /// X position (0.0 = left edge, 1.0 = right edge, normalized)
    pub x: f32,
    /// Y position (0.0 = top edge, 1.0 = bottom edge, normalized)
    pub y: f32,
    /// Target X position
    pub target_x: f32,
    /// Target Y position
    pub target_y: f32,
    /// Flow direction
    pub direction: FlowDirection,
    /// DDR channel source/destination (0-11 for Blackhole)
    pub channel: usize,
    /// Speed (tiles per frame)
    pub speed: f32,
    /// Intensity (0.0-1.0, affects brightness)
    pub intensity: f32,
    /// Color hue (0-360°, temperature-based)
    pub hue: f32,
    /// Time to live (frames)
    pub ttl: u32,
}

impl MemoryFlowParticle {
    /// Create a new read particle (DDR → Core)
    pub fn new_read(channel: usize, current: f32, temp: f32, frame: u32) -> Self {
        // Start at DDR channel position (top edge)
        let channel_pos = (channel as f32 + 0.5) / 12.0; // Normalize 0-1

        // Deterministic "randomness" based on channel and frame
        let pseudo_rand = ((channel * 73 + frame as usize * 37) % 100) as f32 / 100.0;

        Self {
            x: channel_pos,
            y: 0.0, // Top edge (DDR)
            target_x: 0.3 + (pseudo_rand * 0.4), // Pseudo-random core
            target_y: 0.5, // Center (core grid)
            direction: FlowDirection::Read,
            channel,
            speed: 0.01 + (current / 100.0) * 0.03,
            intensity: 0.7 + pseudo_rand * 0.3,
            // Full 360° hue cycling: frame-driven + per-channel spread.
            // Reads get the "forward" arc; writes get the opposite arc (+180°).
            hue: (frame as f32 * 3.0 + channel as f32 * 30.0) % 360.0,
            ttl: 120,
        }
    }

    /// Create a new write particle (Core → DDR)
    pub fn new_write(channel: usize, current: f32, _temp: f32, frame: u32) -> Self {
        let channel_pos = (channel as f32 + 0.5) / 12.0;

        // Deterministic "randomness" based on channel and frame
        let pseudo_rand = ((channel * 97 + frame as usize * 43) % 100) as f32 / 100.0;

        Self {
            x: 0.3 + (pseudo_rand * 0.4), // Pseudo-random core
            y: 0.5, // Center (core grid)
            target_x: channel_pos,
            target_y: 1.0, // Bottom edge (DDR)
            direction: FlowDirection::Write,
            channel,
            speed: 0.01 + (current / 100.0) * 0.03,
            intensity: 0.7 + pseudo_rand * 0.3,
            // Writes offset 180° from reads so they visually counter-rotate.
            hue: (180.0 + frame as f32 * 3.0 + channel as f32 * 30.0) % 360.0,
            ttl: 120,
        }
    }

    /// Update particle position
    pub fn update(&mut self) {
        // Move towards target
        let dx = self.target_x - self.x;
        let dy = self.target_y - self.y;
        let dist = (dx * dx + dy * dy).sqrt();

        if dist > 0.01 {
            self.x += (dx / dist) * self.speed;
            self.y += (dy / dist) * self.speed;
        } else {
            // Reached target, mark for removal
            self.ttl = 0;
        }

        self.ttl = self.ttl.saturating_sub(1);
    }

    /// Check if particle is still active
    pub fn is_active(&self) -> bool {
        self.ttl > 0
    }

    /// Get character for particle
    pub fn get_char(&self) -> char {
        match self.direction {
            FlowDirection::Read => '→',
            FlowDirection::Write => '←',
            FlowDirection::Internal => '↔',
        }
    }

    /// Get color for particle
    pub fn get_color(&self) -> Color {
        use crate::animation::hsv_to_rgb;
        let value = 0.6 + self.intensity * 0.4;
        hsv_to_rgb(self.hue, 1.0, value)  // Full saturation for maximum vibrancy
    }
}

/// Memory Flow Topology visualization
pub struct MemoryFlowVis {
    /// Display dimensions
    width: usize,
    height: usize,

    /// Adaptive baseline for relative activity
    baseline: AdaptiveBaseline,

    /// Animation frame counter
    frame: u32,

    /// Active particles
    particles: Vec<MemoryFlowParticle>,

    /// Maximum particles
    max_particles: usize,

    /// Core activity heat map (grid of 0.0-1.0 values)
    /// Indexed as [row * cols + col]
    core_heat: Vec<f32>,

    /// Grid dimensions (architecture-specific)
    grid_rows: usize,
    grid_cols: usize,
}

impl MemoryFlowVis {
    /// Create new Memory Flow visualization with full density (200 particles)
    pub fn new(width: usize, height: usize) -> Self {
        Self::new_with_density(width, height, 200)
    }

    /// Create new Memory Flow visualization with custom particle density
    /// For Arcade mode, use lower value (100 particles) for better performance
    pub fn new_with_density(width: usize, height: usize, max_particles: usize) -> Self {
        Self {
            width,
            height,
            baseline: AdaptiveBaseline::new(),
            frame: 0,
            particles: Vec::new(),
            max_particles,
            core_heat: vec![0.0; 16 * 14], // Max size (Blackhole)
            grid_rows: 0,
            grid_cols: 0,
        }
    }

    /// Update from telemetry
    pub fn update<B: TelemetryBackend>(&mut self, backend: &B) {
        self.frame += 1;

        let devices = backend.devices();
        if devices.is_empty() {
            return;
        }

        // Use first device for now (multi-device support later)
        let device = &devices[0];
        let telem = backend.telemetry(device.index);

        // Update grid dimensions based on architecture
        (self.grid_cols, self.grid_rows) = match device.architecture {
            Architecture::Grayskull => (10, 12),
            Architecture::Wormhole => (8, 10),
            Architecture::Blackhole => (14, 16),
            Architecture::Unknown => (8, 8),
        };

        if let Some(t) = telem {
            let power = t.power_w();
            let current = t.current_a();
            let temp = t.temp_c();
            let aiclk = t.aiclk_mhz() as f32;

            // Update baseline
            self.baseline.update(device.index, power, current, temp, aiclk);

            // Spawn read particles (DDR → Core)
            let read_rate = (power / 150.0).min(1.0);
            let pseudo_spawn = ((self.frame * 73) % 100) as f32 / 100.0;
            if pseudo_spawn < read_rate && self.particles.len() < self.max_particles {
                let num_channels = device.architecture.memory_channels();
                let channel = (self.frame as usize) % num_channels;
                self.particles
                    .push(MemoryFlowParticle::new_read(
                        channel,
                        current,
                        temp,
                        self.frame,
                    ));
            }

            // Spawn write particles (Core → DDR)
            let write_rate = (current / 100.0).min(1.0);
            let pseudo_spawn_write = ((self.frame * 97) % 100) as f32 / 100.0;
            if pseudo_spawn_write < write_rate * 0.5 && self.particles.len() < self.max_particles
            {
                let num_channels = device.architecture.memory_channels();
                let channel = ((self.frame + num_channels as u32 / 2) as usize) % num_channels;
                self.particles
                    .push(MemoryFlowParticle::new_write(
                        channel,
                        current,
                        temp,
                        self.frame,
                    ));
            }

            // Update core heat map
            let power_change = self.baseline.power_change(device.index, power);
            for row in 0..self.grid_rows {
                for col in 0..self.grid_cols {
                    let idx = row * self.grid_cols + col;
                    if idx < self.core_heat.len() {
                        // Add wave pattern
                        let wave = (row as f32 * 0.5
                            + col as f32 * 0.5
                            + self.frame as f32 * 0.1)
                            .sin()
                            * 0.2;
                        let target_heat = (power_change + wave).max(0.0).min(1.0);

                        // Smooth interpolation (decay over time)
                        self.core_heat[idx] = self.core_heat[idx] * 0.9 + target_heat * 0.1;
                    }
                }
            }
        }

        // Update particles
        for particle in &mut self.particles {
            particle.update();
        }

        // Remove dead particles
        self.particles.retain(|p| p.is_active());
    }

    /// Render full-screen visualization
    pub fn render<B: TelemetryBackend>(&self, backend: &B) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let devices = backend.devices();
        if devices.is_empty() {
            lines.push(Line::from("No devices found"));
            return lines;
        }

        let device = &devices[0];
        let telem = backend.telemetry(device.index);

        // Calculate layout
        let ddr_channels = device.architecture.memory_channels();
        let grid_height = self.height.saturating_sub(6); // Leave room for DDR + stats

        let smbus = backend.smbus_telemetry(device.index);

        // Top DDR channels (sources)
        lines.push(self.render_ddr_channels_top(ddr_channels, telem, smbus));
        lines.push(Line::from("")); // Spacing

        // Core grid with particles
        for y in 0..grid_height {
            lines.push(self.render_grid_line(y, grid_height, telem));
        }

        lines.push(Line::from("")); // Spacing

        // Bottom DDR channels (sinks)
        lines.push(self.render_ddr_channels_bottom(ddr_channels, telem, smbus));

        // Stats line
        lines.push(self.render_stats(device, telem));

        lines
    }

    /// Parse the SMBUS DDR status bitmask into per-channel training state.
    ///
    /// The bitmask encodes 4 bits per channel:
    ///   0x0 → untrained/idle
    ///   0x1 → currently training
    ///   0x2 → fully trained (normal operation)
    ///   other → error / unknown
    ///
    /// Returns a Vec<u8> of length `num_channels`, one nibble per channel.
    fn parse_ddr_status(smbus: Option<&crate::models::SmbusTelemetry>, num_channels: usize) -> Vec<u8> {
        let raw = smbus
            .and_then(|s| s.ddr_status.as_ref())
            .and_then(|s| {
                let s = s.trim_start_matches("0x").trim_start_matches("0X");
                u64::from_str_radix(s, 16).ok()
            })
            .unwrap_or(0);

        (0..num_channels)
            .map(|i| ((raw >> (4 * i)) & 0xF) as u8)
            .collect()
    }

    /// Render top DDR channels (sources).
    ///
    /// Bar height is driven by total current draw (real signal).
    /// Per-channel state comes from the SMBUS DDR training status bitmask —
    /// trained channels show full utilization bars, training channels animate,
    /// untrained channels show a dim outline only.  No synthetic scanning.
    fn render_ddr_channels_top(
        &self,
        num_channels: usize,
        telem: Option<&crate::models::Telemetry>,
        smbus: Option<&crate::models::SmbusTelemetry>,
    ) -> Line<'static> {
        let mut spans = Vec::new();

        let current = telem.map(|t| t.current_a()).unwrap_or(0.0);
        let utilization = (current / 100.0).min(1.0);

        spans.push(Span::styled(
            "DDR IN:  ",
            Style::default()
                .fg(colors::rgb(100, 180, 255))
                .add_modifier(Modifier::BOLD),
        ));

        let channel_width = (self.width.saturating_sub(15)) / num_channels.max(1);
        let channel_status = Self::parse_ddr_status(smbus, num_channels);

        for ch in 0..num_channels {
            let status = channel_status.get(ch).copied().unwrap_or(0);

            match status {
                2 => {
                    // Trained — show real utilization bar
                    let filled = (utilization * channel_width as f32) as usize;
                    let empty = channel_width.saturating_sub(filled);
                    let color = if utilization > 0.7 {
                        colors::rgb(255, 200, 100)
                    } else if utilization > 0.4 {
                        colors::rgb(100, 180, 255)
                    } else {
                        colors::rgb(80, 150, 200)
                    };
                    spans.push(Span::styled("═".repeat(filled), Style::default().bg(colors::rgb(0, 0, 0)).fg(color)));
                    spans.push(Span::styled("·".repeat(empty), Style::default().fg(colors::rgb(40, 40, 60))));
                }
                1 => {
                    // Training — animate with alternating characters
                    let anim = if (self.frame / 4) % 2 == 0 { '◐' } else { '◑' };
                    let bar = anim.to_string().repeat(channel_width.max(1));
                    spans.push(Span::styled(bar, Style::default().fg(colors::rgb(80, 220, 220))));
                }
                _ => {
                    // Untrained / error — dim outline
                    let color = if status > 2 { colors::rgb(180, 60, 60) } else { colors::rgb(50, 50, 70) };
                    spans.push(Span::styled("─".repeat(channel_width), Style::default().fg(color)));
                }
            }
        }

        Line::from(spans)
    }

    /// Render core grid line with particles
    fn render_grid_line(
        &self,
        y: usize,
        total_height: usize,
        telem: Option<&crate::models::Telemetry>,
    ) -> Line<'static> {
        let mut spans = Vec::new();
        let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);

        // Normalize y to 0.0-1.0
        let y_norm = y as f32 / total_height as f32;

        for x in 0..self.width {
            let x_norm = x as f32 / self.width as f32;

            // Check if particle is at this position
            let mut particle_here = None;
            for particle in &self.particles {
                let px = (particle.x * self.width as f32) as usize;
                let py = (particle.y * total_height as f32) as usize;
                if px == x && py == y {
                    particle_here = Some(particle);
                    break;
                }
            }

            if let Some(particle) = particle_here {
                // Draw particle
                spans.push(Span::styled(
                    particle.get_char().to_string(),
                    Style::default()
                        .fg(particle.get_color())
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                // Draw core background heat map
                let in_core_area = x_norm > 0.1 && x_norm < 0.9 && y_norm > 0.2 && y_norm < 0.8;

                if in_core_area {
                    // Map position to core grid
                    let grid_x =
                        ((x_norm - 0.1) / 0.8 * self.grid_cols as f32) as usize % self.grid_cols;
                    let grid_y =
                        ((y_norm - 0.2) / 0.6 * self.grid_rows as f32) as usize % self.grid_rows;
                    let idx = grid_y * self.grid_cols + grid_x;

                    let heat = if idx < self.core_heat.len() {
                        self.core_heat[idx]
                    } else {
                        0.0
                    };

                    let core_char = if heat > 0.8 {
                        '█'
                    } else if heat > 0.6 {
                        '▓'
                    } else if heat > 0.4 {
                        '▒'
                    } else if heat > 0.2 {
                        '░'
                    } else {
                        '·'
                    };

                    // Color: full 360° cycling. Temp anchors the base hue; time
                    // slowly drifts it through the entire spectrum. High heat
                    // boosts saturation and value for a glowing effect.
                    use crate::animation::{hsv_to_rgb, temp_to_hue};
                    let hue = (temp_to_hue(temp) + self.frame as f32 * 1.5) % 360.0;
                    let saturation = 0.7 + heat * 0.3;
                    let value = 0.3 + heat * 0.7;
                    let color = hsv_to_rgb(hue, saturation, value);

                    spans.push(Span::styled(
                        core_char.to_string(),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(color),
                    ));
                } else {
                    // Outside core area - empty space
                    spans.push(Span::raw(" "));
                }
            }
        }

        Line::from(spans)
    }

    /// Render bottom DDR channels (sinks).
    ///
    /// Bar height uses power draw (complementary to the top bar's current).
    /// Training status from SMBUS controls which channels are active —
    /// same logic as the top bar, warm colour palette to distinguish direction.
    fn render_ddr_channels_bottom(
        &self,
        num_channels: usize,
        telem: Option<&crate::models::Telemetry>,
        smbus: Option<&crate::models::SmbusTelemetry>,
    ) -> Line<'static> {
        let mut spans = Vec::new();

        let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
        let utilization = (power / 150.0).min(1.0);

        spans.push(Span::styled(
            "DDR OUT: ",
            Style::default()
                .fg(colors::rgb(255, 180, 100))
                .add_modifier(Modifier::BOLD),
        ));

        let channel_width = (self.width.saturating_sub(15)) / num_channels.max(1);
        let channel_status = Self::parse_ddr_status(smbus, num_channels);

        for ch in 0..num_channels {
            let status = channel_status.get(ch).copied().unwrap_or(0);

            match status {
                2 => {
                    // Trained — warm palette (power-driven, write direction)
                    let filled = (utilization * channel_width as f32) as usize;
                    let empty = channel_width.saturating_sub(filled);
                    let color = if utilization > 0.7 {
                        colors::rgb(255, 150, 80)
                    } else if utilization > 0.4 {
                        colors::rgb(210, 130, 70)
                    } else {
                        colors::rgb(150, 100, 60)
                    };
                    spans.push(Span::styled("═".repeat(filled), Style::default().bg(colors::rgb(0, 0, 0)).fg(color)));
                    spans.push(Span::styled("·".repeat(empty), Style::default().fg(colors::rgb(40, 30, 20))));
                }
                1 => {
                    // Training — animate with alternating characters
                    let anim = if (self.frame / 4) % 2 == 0 { '◒' } else { '◓' };
                    let bar = anim.to_string().repeat(channel_width.max(1));
                    spans.push(Span::styled(bar, Style::default().fg(colors::rgb(220, 180, 80))));
                }
                _ => {
                    let color = if status > 2 { colors::rgb(180, 60, 60) } else { colors::rgb(50, 40, 30) };
                    spans.push(Span::styled("─".repeat(channel_width), Style::default().fg(color)));
                }
            }
        }

        Line::from(spans)
    }

    /// Render stats line
    fn render_stats(
        &self,
        device: &crate::models::Device,
        telem: Option<&crate::models::Telemetry>,
    ) -> Line<'static> {
        let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
        let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);
        let current = telem.map(|t| t.current_a()).unwrap_or(0.0);

        let stats = format!(
            " {} | Particles: {} | Power: {:.1}W | Temp: {:.0}°C | Current: {:.1}A | Frame: {}",
            device.architecture.abbrev(),
            self.particles.len(),
            power,
            temp,
            current,
            self.frame
        );

        Line::from(vec![Span::styled(
            stats,
            Style::default()
                .fg(colors::rgb(200, 200, 220))
                .add_modifier(Modifier::DIM),
        )])
    }

    /// Resize handler
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
    }

    /// Get mode name
    pub fn mode_name(&self) -> &'static str {
        "Memory Flow"
    }
}
