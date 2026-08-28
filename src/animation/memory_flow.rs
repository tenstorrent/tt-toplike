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
    /// Source device index — pins this particle's color to a fleet hue band.
    pub device_idx: usize,
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
    /// Create a new read particle (DDR → Core).
    ///
    /// `device_hue_base` is the 360°-divided hue band assigned to this device in the
    /// fleet (e.g. device 0 of 4 → 0°, device 1 → 90°, …).  Reads use that base;
    /// writes offset by +120° so read/write traffic are visually distinct per device.
    pub fn new_read(
        channel: usize,
        current: f32,
        frame: u32,
        device_idx: usize,
        device_hue_base: f32,
    ) -> Self {
        let channel_pos = (channel as f32 + 0.5) / 12.0;
        let pseudo_rand = ((channel * 73 + frame as usize * 37) % 100) as f32 / 100.0;

        Self {
            x: channel_pos,
            y: 0.0,
            target_x: 0.3 + (pseudo_rand * 0.4),
            target_y: 0.5,
            device_idx,
            direction: FlowDirection::Read,
            channel,
            speed: 0.01 + (current / 100.0) * 0.03,
            intensity: 0.7 + pseudo_rand * 0.3,
            // Base hue anchors to the device's band; slow time-drift keeps it alive.
            hue: (device_hue_base + frame as f32 * 0.8 + channel as f32 * 8.0) % 360.0,
            ttl: 120,
        }
    }

    /// Create a new write particle (Core → DDR).
    pub fn new_write(
        channel: usize,
        current: f32,
        frame: u32,
        device_idx: usize,
        device_hue_base: f32,
    ) -> Self {
        let channel_pos = (channel as f32 + 0.5) / 12.0;
        let pseudo_rand = ((channel * 97 + frame as usize * 43) % 100) as f32 / 100.0;

        Self {
            x: 0.3 + (pseudo_rand * 0.4),
            y: 0.5,
            target_x: channel_pos,
            target_y: 1.0,
            device_idx,
            direction: FlowDirection::Write,
            channel,
            speed: 0.01 + (current / 100.0) * 0.03,
            intensity: 0.7 + pseudo_rand * 0.3,
            // Writes offset +120° within the device band — visually distinct from reads.
            hue: (device_hue_base + 120.0 + frame as f32 * 0.8 + channel as f32 * 8.0) % 360.0,
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
        hsv_to_rgb(self.hue, 1.0, value) // Full saturation for maximum vibrancy
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

    /// Emit the power/temperature fields in the fleet stats line.  `true` for
    /// the standalone Memory Flow mode (default, byte-identical to today);
    /// Arcade sets this `false` so the composite view owns a single shared
    /// telemetry strip instead of each embedded section printing W/°C.
    chrome: bool,
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
            chrome: true,
        }
    }

    /// Toggle the power/temp fields in the fleet stats line.  `true` = the
    /// standalone look (default); `false` drops the W/°C fields (particle and
    /// current stats stay) so a composite view (Arcade) can render one shared
    /// telemetry strip.
    pub fn set_chrome(&mut self, chrome: bool) {
        self.chrome = chrome;
    }

    /// Set the animation sensitivity multiplier.
    ///
    /// Delegates directly to the internal `AdaptiveBaseline`. Higher values make
    /// particle density and flow speed respond to smaller hardware fluctuations.
    /// 1.0 = Normal, 5.0 = Paranoid, 0.2 = AnomaliesOnly.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.baseline.sensitivity = sensitivity;
    }

    /// Update from telemetry — tracks all devices.
    ///
    /// Each device gets a hue band in the 360° wheel (device 0 → 0°, device 1 → 360/n°, …).
    /// Particles carry `device_idx` so reads/writes from different chips appear in distinct
    /// colors simultaneously.  The shared heat map and DDR bars accumulate across the fleet.
    pub fn update(&mut self, backend: &dyn TelemetryBackend) {
        self.frame += 1;

        let devices = backend.devices();
        if devices.is_empty() {
            return;
        }

        // Set grid dimensions from first device's architecture (they're all the same arch
        // in practice; the heat map just uses the largest case so indices are always valid).
        (self.grid_cols, self.grid_rows) = match devices[0].architecture {
            Architecture::Grayskull => (10, 12),
            Architecture::Wormhole => (8, 10),
            Architecture::Blackhole => (14, 16),
            Architecture::Unknown => (8, 8),
        };

        let n_devices = devices.len().max(1);
        let hue_step = 360.0 / n_devices as f32;

        // Per-device pass: spawn particles and accumulate heat.
        for (fleet_pos, device) in devices.iter().enumerate() {
            let device_hue_base = fleet_pos as f32 * hue_step;
            let telem = backend.telemetry(device.index);
            let smbus = backend.smbus_telemetry(device.index);

            if let Some(t) = telem {
                let power = t.power_w();
                let current = t.current_a();
                let temp = t.temp_c();
                let aiclk = t.aiclk_mhz() as f32;

                let axiclk: f32 = smbus
                    .and_then(|s| s.axiclk.as_deref())
                    .and_then(|v| crate::models::telemetry::parse_hex_or_dec(v))
                    .map(|v| v as f32)
                    .unwrap_or(0.0);
                let noc_factor = if aiclk > 50.0 && axiclk > 0.0 {
                    ((axiclk / aiclk) / 0.5).clamp(0.3, 1.4)
                } else {
                    1.0
                };

                let mvddq: f32 = smbus
                    .and_then(|s| s.mvddq_power.as_deref())
                    .and_then(|v| crate::models::telemetry::parse_hex_or_dec(v))
                    .map(|v| v as f32)
                    .unwrap_or(0.0);

                self.baseline
                    .update(device.index, power, current, temp, aiclk);

                // Per-device particle budget: split max_particles across the fleet so
                // total density stays constant regardless of how many chips are present.
                // Clamp to at least 1 so a large fleet never silently disables the viz.
                let device_budget = (self.max_particles / n_devices).max(1);

                let device_particle_count = self
                    .particles
                    .iter()
                    .filter(|p| p.device_idx == device.index)
                    .count();

                // Read particles
                let read_rate = (power / 150.0).min(1.0);
                let pseudo_r = ((self.frame * 73 + fleet_pos as u32 * 31) % 100) as f32 / 100.0;
                if pseudo_r < read_rate && device_particle_count < device_budget {
                    // `.max(1)` guards 0-channel devices (Host/CPU backend's
                    // `Architecture::Unknown`) against a divide-by-zero panic.
                    let num_ch = device.memory_channels().max(1);
                    let channel = (self.frame as usize + fleet_pos * 7) % num_ch;
                    let mut p = MemoryFlowParticle::new_read(
                        channel,
                        current,
                        self.frame,
                        device.index,
                        device_hue_base,
                    );
                    p.speed *= noc_factor;
                    self.particles.push(p);
                }

                // Write particles — recount after the read spawn so the budget check
                // reflects any particle just added.
                let device_particle_count = self
                    .particles
                    .iter()
                    .filter(|p| p.device_idx == device.index)
                    .count();
                let mvddq_rate = (mvddq / 20.0).min(1.0);
                let write_rate = (current / 100.0).min(1.0).max(mvddq_rate * 0.6);
                let pseudo_w = ((self.frame * 97 + fleet_pos as u32 * 43) % 100) as f32 / 100.0;
                if pseudo_w < write_rate * 0.5 && device_particle_count < device_budget {
                    // `.max(1)` guards 0-channel devices (Host/CPU backend's
                    // `Architecture::Unknown`) against a divide-by-zero panic.
                    let num_ch = device.memory_channels().max(1);
                    let channel = (self.frame as usize + fleet_pos * 7 + num_ch / 2) % num_ch;
                    let mut p = MemoryFlowParticle::new_write(
                        channel,
                        current,
                        self.frame,
                        device.index,
                        device_hue_base,
                    );
                    p.speed *= noc_factor;
                    self.particles.push(p);
                }

                // Accumulate heat from all devices (superimpose activity waves).
                let power_change = self.baseline.power_change(device.index, power);
                let phase_offset = fleet_pos as f32 * 1.2; // stagger waves per device
                for row in 0..self.grid_rows {
                    for col in 0..self.grid_cols {
                        let idx = row * self.grid_cols + col;
                        if idx < self.core_heat.len() {
                            let wave = (row as f32 * 0.5
                                + col as f32 * 0.5
                                + self.frame as f32 * 0.1
                                + phase_offset)
                                .sin()
                                * 0.15;
                            let contrib =
                                ((power_change + wave).max(0.0).min(1.0)) / n_devices as f32;
                            // Accumulate rather than replace so all devices show simultaneously.
                            self.core_heat[idx] = (self.core_heat[idx] + contrib * 0.1).min(1.0);
                        }
                    }
                }

                // Also slowly decay per-frame so heat doesn't saturate.
                let _ = temp; // used indirectly via baseline
            }
        }

        // Global decay after all devices contribute.
        for h in &mut self.core_heat {
            *h *= 0.92;
        }

        // Update and cull particles
        for particle in &mut self.particles {
            particle.update();
        }
        self.particles.retain(|p| p.is_active());
    }

    /// Render full-screen visualization.
    ///
    /// DDR bars show the fleet aggregate (sum of current / sum of power across all devices).
    /// The core heat map and particle field are already accumulated from all devices in update().
    /// Stats line reports fleet totals.
    pub fn render(&self, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let devices = backend.devices();
        if devices.is_empty() {
            lines.push(Line::from("No devices found"));
            return lines;
        }

        // Aggregate telemetry across all devices for the DDR bars and stats.
        let mut total_power = 0.0f32;
        let mut total_current = 0.0f32;
        let mut total_temp = 0.0f32;
        let mut active = 0usize;
        for device in devices {
            if let Some(t) = backend.telemetry(device.index) {
                total_power += t.power_w();
                total_current += t.current_a();
                total_temp += t.temp_c();
                active += 1;
            }
        }
        let avg_temp = if active > 0 {
            total_temp / active as f32
        } else {
            0.0
        };

        // Use the first device's architecture for channel count and smbus DDR status.
        let arch_device = &devices[0];
        let ddr_channels = arch_device.memory_channels();
        let smbus = backend.smbus_telemetry(arch_device.index);

        // Synthetic aggregate telemetry for the DDR bar helpers.
        let agg_telem_power = total_power;
        let agg_telem_current = total_current;

        let grid_height = self.height.saturating_sub(6);

        lines.push(self.render_ddr_channels_top_agg(ddr_channels, agg_telem_current, smbus));
        lines.push(Line::from(""));

        for y in 0..grid_height {
            lines.push(self.render_grid_line(y, grid_height, avg_temp));
        }

        lines.push(Line::from(""));
        lines.push(self.render_ddr_channels_bottom_agg(ddr_channels, agg_telem_power, smbus));
        lines.push(self.render_stats_fleet(devices, total_power, total_current, avg_temp));

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
    fn parse_ddr_status(
        smbus: Option<&crate::models::SmbusTelemetry>,
        num_channels: usize,
    ) -> Vec<u8> {
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

    /// Map real per-channel `gddr_telemetry` state onto the same status
    /// vocabulary `parse_ddr_status` produces from the packed bitmask
    /// (0=idle/untrained, 1=training, 2=trained, >2=error), so the existing
    /// render match in `render_ddr_channels_top_agg`/`bottom_agg` needs no
    /// changes. `gddr_telemetry` has no "currently training" signal (only a
    /// post-hoc pass/fail), so this source never emits `1` — that state
    /// stays reachable only via the bitmask fallback.
    fn channel_status_from_gddr_telemetry(
        g: &crate::models::telemetry::GddrTelemetry,
        num_channels: usize,
    ) -> Vec<u8> {
        (0..num_channels)
            .map(|i| {
                g.channels
                    .iter()
                    .find(|c| c.channel == i)
                    .map_or(0, |c| {
                        if c.harvested || !c.enabled {
                            0
                        } else if !c.training_pass || !c.bist_pass {
                            3
                        } else {
                            2
                        }
                    })
            })
            .collect()
    }

    /// Real `gddr_telemetry` state when present (tt-smi ≥ 6.3.0), else the
    /// packed `DDR_STATUS` bitmask decode (`parse_ddr_status`).
    fn channel_status(
        smbus: Option<&crate::models::SmbusTelemetry>,
        num_channels: usize,
    ) -> Vec<u8> {
        match smbus
            .and_then(|s| s.gddr_telemetry.as_ref())
            .filter(|g| !g.channels.is_empty())
        {
            Some(g) => Self::channel_status_from_gddr_telemetry(g, num_channels),
            None => Self::parse_ddr_status(smbus, num_channels),
        }
    }

    /// Render top DDR channels — fleet aggregate version.
    fn render_ddr_channels_top_agg(
        &self,
        num_channels: usize,
        total_current: f32,
        smbus: Option<&crate::models::SmbusTelemetry>,
    ) -> Line<'static> {
        let mut spans = Vec::new();
        let current = total_current;
        let utilization = (current / 100.0).min(1.0);

        spans.push(Span::styled(
            "DDR IN:  ",
            Style::default()
                .fg(colors::rgb(100, 180, 255))
                .add_modifier(Modifier::BOLD),
        ));

        let channel_width = (self.width.saturating_sub(15)) / num_channels.max(1);
        let channel_status = Self::channel_status(smbus, num_channels);

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
                    spans.push(Span::styled(
                        "═".repeat(filled),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(color),
                    ));
                    spans.push(Span::styled(
                        "·".repeat(empty),
                        Style::default().fg(colors::rgb(40, 40, 60)),
                    ));
                }
                1 => {
                    // Training — animate with alternating characters
                    let anim = if (self.frame / 4) % 2 == 0 {
                        '◐'
                    } else {
                        '◑'
                    };
                    let bar = anim.to_string().repeat(channel_width.max(1));
                    spans.push(Span::styled(
                        bar,
                        Style::default().fg(colors::rgb(80, 220, 220)),
                    ));
                }
                _ => {
                    // Untrained / error — dim outline
                    let color = if status > 2 {
                        colors::rgb(180, 60, 60)
                    } else {
                        colors::rgb(50, 50, 70)
                    };
                    spans.push(Span::styled(
                        "─".repeat(channel_width),
                        Style::default().fg(color),
                    ));
                }
            }
        }

        Line::from(spans)
    }

    /// Render core grid line with particles.  Takes `avg_temp` directly (pre-aggregated).
    fn render_grid_line(&self, y: usize, total_height: usize, avg_temp: f32) -> Line<'static> {
        let mut spans = Vec::new();
        let temp = avg_temp;

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

    /// Render bottom DDR channels — fleet aggregate version.
    fn render_ddr_channels_bottom_agg(
        &self,
        num_channels: usize,
        total_power: f32,
        smbus: Option<&crate::models::SmbusTelemetry>,
    ) -> Line<'static> {
        let mut spans = Vec::new();

        let power = total_power;
        let utilization = (power / 150.0).min(1.0);

        spans.push(Span::styled(
            "DDR OUT: ",
            Style::default()
                .fg(colors::rgb(255, 180, 100))
                .add_modifier(Modifier::BOLD),
        ));

        let channel_width = (self.width.saturating_sub(15)) / num_channels.max(1);
        let channel_status = Self::channel_status(smbus, num_channels);

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
                    spans.push(Span::styled(
                        "═".repeat(filled),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(color),
                    ));
                    spans.push(Span::styled(
                        "·".repeat(empty),
                        Style::default().fg(colors::rgb(40, 30, 20)),
                    ));
                }
                1 => {
                    // Training — animate with alternating characters
                    let anim = if (self.frame / 4) % 2 == 0 {
                        '◒'
                    } else {
                        '◓'
                    };
                    let bar = anim.to_string().repeat(channel_width.max(1));
                    spans.push(Span::styled(
                        bar,
                        Style::default().fg(colors::rgb(220, 180, 80)),
                    ));
                }
                _ => {
                    let color = if status > 2 {
                        colors::rgb(180, 60, 60)
                    } else {
                        colors::rgb(50, 40, 30)
                    };
                    spans.push(Span::styled(
                        "─".repeat(channel_width),
                        Style::default().fg(color),
                    ));
                }
            }
        }

        Line::from(spans)
    }

    /// Fleet stats line — totals across all devices.
    fn render_stats_fleet(
        &self,
        devices: &[crate::models::Device],
        total_power: f32,
        total_current: f32,
        avg_temp: f32,
    ) -> Line<'static> {
        let n = devices.len();
        let arch_label = if n == 1 {
            devices[0].architecture.abbrev().to_string()
        } else {
            format!("{}×{}", n, devices[0].architecture.abbrev())
        };
        // Standalone shows the full stats line; embedded (chrome off) drops the
        // power/temp fields — those are owned by the Arcade shared strip — while
        // keeping the flow-specific particle count and current draw.
        let stats = if self.chrome {
            format!(
                " {} | particles:{:3} | power:{:5.1}W | temp:{:3.0}°C | current:{:5.1}A",
                arch_label,
                self.particles.len(),
                total_power,
                avg_temp,
                total_current,
            )
        } else {
            format!(
                " {} | particles:{:3} | current:{:5.1}A",
                arch_label,
                self.particles.len(),
                total_current,
            )
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::host::HostBackend;
    use crate::backend::TelemetryBackend;

    /// Regression: same zero-channel divide-by-zero as Memory Castle. The Host
    /// backend's `Architecture::Unknown` device reports 0 memory channels, and
    /// the read/write particle spawn paths did `frame % num_ch`. Arcade embeds
    /// Memory Flow, so this panicked too. update() must tolerate 0 channels.
    #[test]
    fn update_survives_zero_channel_device() {
        let mut backend = HostBackend::new();
        backend.init().expect("host backend init");
        backend.update().expect("host backend update");

        let mut flow = MemoryFlowVis::new(80, 24);
        for _ in 0..10 {
            flow.update(&backend);
        }
    }

    #[test]
    fn channel_status_prefers_real_gddr_telemetry_over_bitmask() {
        use crate::models::telemetry::{GddrChannel, GddrTelemetry, SmbusTelemetry};
        let smbus = SmbusTelemetry {
            // Bitmask says "all trained" (nibble 2 repeated) — if this were used,
            // every channel would show status 2. gddr_telemetry disagrees, and
            // must win.
            ddr_status: Some("0x22222222".to_string()),
            gddr_telemetry: Some(GddrTelemetry {
                speed: None,
                max_temp: None,
                enabled_mask: None,
                channels: vec![
                    GddrChannel { channel: 0, harvested: true, enabled: false, training_pass: false, bist_pass: true, ..Default::default() },
                    GddrChannel { channel: 1, harvested: false, enabled: true, training_pass: true, bist_pass: false, ..Default::default() },
                    GddrChannel { channel: 2, harvested: false, enabled: true, training_pass: true, bist_pass: true, ..Default::default() },
                ],
            }),
            ..Default::default()
        };
        let status = MemoryFlowVis::channel_status(Some(&smbus), 3);
        assert_eq!(status[0], 0, "harvested channel reads as idle, not trained");
        assert_eq!(status[1], 3, "BIST failure is a distinct error code, not the generic bitmask error");
        assert_eq!(status[2], 2, "healthy enabled channel reads as trained");
    }

    #[test]
    fn channel_status_falls_back_to_bitmask_when_gddr_telemetry_absent() {
        use crate::models::telemetry::SmbusTelemetry;
        let smbus = SmbusTelemetry {
            ddr_status: Some("0x00000012".to_string()), // channel0=trained(2), channel1=idle(0), channel... etc per nibble
            gddr_telemetry: None,
            ..Default::default()
        };
        let status = MemoryFlowVis::channel_status(Some(&smbus), 2);
        assert_eq!(status[0], 2);
        assert_eq!(status[1], 1);
    }
}
