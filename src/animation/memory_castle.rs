// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Memory Dungeon - Roguelike visualization of memory hierarchy
//!
//! A dynamic, full-screen ASCII visualization showing memory flowing through the hardware hierarchy:
//! - DDR channels at bottom (external memory gates)
//! - L2 cache in middle (staging rooms)
//! - L1 SRAM above (fast cache vaults)
//! - Tensix cores at top (processing chambers)
//!
//! Memory particles (@◉◎•○·) spawn at DDR, flow upward through L2 → L1 → Tensix,
//! with real-time animation driven by actual telemetry (power, current, temperature).
//!
//! Inspired by roguelike dungeons (NetHack, DCSS), where every character and color
//! has meaning, and the dungeon is alive with activity.

use crate::animation::{AdaptiveBaseline, hsv_to_rgb, rgb_to_hsv, temp_to_hue};
use crate::animation::topology::BoardTopology;
use crate::backend::TelemetryBackend;
use crate::ui::colors;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Particle type distinguishes different memory operations
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParticleType {
    /// Memory read operation (fast, bright)
    Read,
    /// Memory write operation (slower, warmer)
    Write,
    /// Cache hit (very fast, cool)
    CacheHit,
    /// Cache miss (slower, hot)
    CacheMiss,
}

/// A memory particle representing a memory operation flowing through the hierarchy
#[derive(Debug, Clone)]
pub struct MemoryParticle {
    /// X position (column)
    pub x: f32,
    /// Y position (row, 0=bottom)
    pub y: f32,
    /// Velocity X
    pub vx: f32,
    /// Velocity Y
    pub vy: f32,
    /// Current layer (0=DDR, 1=L2, 2=L1, 3=Tensix)
    pub layer: usize,
    /// Target layer
    pub target_layer: usize,
    /// Intensity (0.0-1.0, driven by power)
    pub intensity: f32,
    /// Color hue (0-360, driven by temperature)
    pub hue: f32,
    /// Time to live (frames)
    pub ttl: u32,
    /// Particle type
    pub particle_type: ParticleType,
    /// Trail positions (last N positions for trail effect)
    pub trail: Vec<(f32, f32)>,
    /// Source device index (for multi-chip visualization)
    pub source_device: usize,
}

impl MemoryParticle {
    /// Create new particle at DDR entry
    pub fn new(ddr_channel: usize, power_change: f32, temp: f32, frame: u32, source_device: usize) -> Self {
        // Randomize particle type based on frame counter
        let particle_type = match frame % 4 {
            0 => ParticleType::Read,
            1 => ParticleType::Write,
            2 => ParticleType::CacheHit,
            _ => ParticleType::CacheMiss,
        };

        // Vary spawn position with some randomness
        let x_offset = ((frame * 7 + ddr_channel as u32 * 3) % 10) as f32 * 0.5;

        // Different velocity based on type
        let (vy, ttl) = match particle_type {
            ParticleType::CacheHit => (0.8, 40),   // Very fast
            ParticleType::Read => (0.6, 50),       // Fast
            ParticleType::Write => (0.4, 60),      // Medium
            ParticleType::CacheMiss => (0.3, 70),  // Slow
        };

        Self {
            x: (ddr_channel * 10) as f32 + x_offset,
            y: 0.0,
            vx: ((frame * 13) % 20) as f32 * 0.02 - 0.2,  // Slight horizontal drift
            vy,
            layer: 0,
            target_layer: 3,  // All particles aim for Tensix
            intensity: power_change.max(0.2).min(1.0),
            // Full 360° rainbow sweep: temp biases the starting hue, frame drives rapid
            // cycling so particles born at different times span the whole spectrum.
            // At 10 FPS, frame * 5.0 completes a full rainbow every ~72 frames (~7s).
            hue: (temp_to_hue(temp) + frame as f32 * 5.0) % 360.0,
            ttl,
            particle_type,
            trail: Vec::with_capacity(8),
            source_device,
        }
    }

    /// Get character representing this particle
    pub fn get_char(&self) -> char {
        match self.particle_type {
            ParticleType::Read => {
                let idx = (self.intensity * 2.0) as usize;
                ['◌', '○', '◉'][idx.min(2)]
            }
            ParticleType::Write => {
                let idx = (self.intensity * 2.0) as usize;
                ['□', '▣', '■'][idx.min(2)]
            }
            ParticleType::CacheHit => {
                let idx = (self.intensity * 2.0) as usize;
                ['◇', '◈', '◆'][idx.min(2)]
            }
            ParticleType::CacheMiss => {
                let idx = (self.intensity * 2.0) as usize;
                ['∘', '●', '⬤'][idx.min(2)]
            }
        }
    }

    /// Get color for this particle
    pub fn get_color(&self) -> Color {
        // All four types use self.hue (which sweeps the full rainbow over time), just
        // offset by 90° increments so the four types always form a tetrad — distinct
        // but all cycling together. Saturation is pinned at 1.0 for maximum vividness.
        let base_hue = match self.particle_type {
            ParticleType::Read     => self.hue,
            ParticleType::Write    => (self.hue +  90.0) % 360.0,
            ParticleType::CacheHit => (self.hue + 180.0) % 360.0,
            ParticleType::CacheMiss => (self.hue + 270.0) % 360.0,
        };

        hsv_to_rgb(
            base_hue,
            1.0,
            (0.72 + self.intensity * 0.28).min(1.0),
        )
    }

    /// Get trail character (dimmer version)
    pub fn get_trail_char(&self) -> char {
        match self.particle_type {
            ParticleType::Read => '·',
            ParticleType::Write => '▪',
            ParticleType::CacheHit => '⋅',
            ParticleType::CacheMiss => '•',
        }
    }

    /// Get trail color (dimmer, same hue family as particle head)
    pub fn get_trail_color(&self, age: usize) -> Color {
        // Mirror the same 90° offsets used in get_color so trails read as the same hue
        let base_hue = match self.particle_type {
            ParticleType::Read     => self.hue,
            ParticleType::Write    => (self.hue +  90.0) % 360.0,
            ParticleType::CacheHit => (self.hue + 180.0) % 360.0,
            ParticleType::CacheMiss => (self.hue + 270.0) % 360.0,
        };

        // Trails were near-black (val ≤ 0.3). Raise floor so they're visible ribbons.
        let fade = (8 - age.min(8)) as f32 / 8.0;
        hsv_to_rgb(
            base_hue,
            0.85,
            (0.62 * fade * self.intensity).max(0.08),
        )
    }

    /// Update particle position (move upward through layers)
    pub fn update(&mut self) {
        // Store trail position
        if self.trail.len() >= 8 {
            self.trail.remove(0);
        }
        self.trail.push((self.x, self.y));

        // Apply velocity
        self.x += self.vx;
        self.y += self.vy;

        // Advance through layers based on Y position
        if self.y > 45.0 && self.layer < 3 {
            self.layer = 3;  // Reached Tensix
        } else if self.y > 30.0 && self.layer < 2 {
            self.layer = 2;  // Reached L1
        } else if self.y > 15.0 && self.layer < 1 {
            self.layer = 1;  // Reached L2
        }

        self.ttl = self.ttl.saturating_sub(1);
    }

    /// Check if particle is still alive
    pub fn is_alive(&self) -> bool {
        self.ttl > 0 && self.y < 60.0
    }
}

/// Memory Dungeon visualization
pub struct MemoryCastle {
    /// Terminal width
    width: usize,
    /// Terminal height
    height: usize,
    /// Adaptive baseline for relative activity
    baseline: AdaptiveBaseline,
    /// Animation frame counter
    frame: u32,
    /// Active memory particles
    particles: Vec<MemoryParticle>,
    /// Maximum particles
    max_particles: usize,
    /// Environmental glyphs (x, y, char, hue)
    environment: Vec<(usize, usize, char, f32)>,
    /// Board topology for topology-aware column separators and board labels.
    /// `║` is used at board boundaries; `│` between chips on the same board.
    board_topology: Option<BoardTopology>,
}

impl MemoryCastle {
    /// Create new Memory Dungeon with full density (600 particles)
    pub fn new(width: usize, height: usize) -> Self {
        Self::new_with_density(width, height, 600, 30)
    }

    /// Create new Memory Dungeon with custom density
    /// For Arcade mode, use lower values (300 particles, 15 glyphs) for better performance
    pub fn new_with_density(width: usize, height: usize, max_particles: usize, glyph_count: usize) -> Self {
        // Generate environmental glyphs (torches, runes, etc.)
        let mut environment = Vec::new();
        let glyph_chars = ['⚡', '※', '☼', '♦', '◊', '▲', '▼', '◄', '►', '⚬', '⊙', '⊕'];

        // Place glyphs pseudo-randomly
        for i in 0..glyph_count {
            let x = (i * 17 + 7) % (width.saturating_sub(4).max(1));
            let y = (i * 23 + 13) % (height.saturating_sub(6).max(1));
            let char_idx = (i * 11) % glyph_chars.len();
            let hue = (i * 37) as f32 % 360.0;
            environment.push((x + 2, y + 3, glyph_chars[char_idx], hue));
        }

        Self {
            width,
            height,
            baseline: AdaptiveBaseline::new(),
            frame: 0,
            particles: Vec::new(),
            max_particles,
            environment,
            board_topology: None,
        }
    }

    /// Install board topology for topology-aware multi-device rendering.
    ///
    /// Once set, `render_multi_device` uses `║` between chips on different
    /// boards and `│` between chips that share a board, and adds a board
    /// label row above the per-device header.
    pub fn set_topology(&mut self, topology: BoardTopology) {
        self.board_topology = Some(topology);
    }

    /// Update animation state
    pub fn update<B: TelemetryBackend>(&mut self, backend: &B) {
        self.frame = self.frame.wrapping_add(1);

        // Update baseline for each device
        for (idx, device) in backend.devices().iter().enumerate() {
            if let Some(telem) = backend.telemetry(idx) {
                self.baseline.update(
                    device.index,
                    telem.power_w(),
                    telem.current_a(),
                    telem.temp_c(),
                    telem.aiclk_mhz() as f32,
                );
            }
        }

        // Spawn new particles based on activity (spawn MANY more particles)
        for (_idx, device) in backend.devices().iter().enumerate() {
            if let Some(telem) = backend.telemetry(device.index) {
                let power_change = self.baseline.power_change(device.index, telem.power_w());
                let temp = telem.temp_c();

                // Spawn rate based on activity (much more aggressive)
                let spawn_count = if power_change > 0.5 {
                    4  // High activity = 4 particles per frame
                } else if power_change > 0.3 {
                    2  // Medium activity = 2 particles per frame
                } else {
                    1  // Low activity = 1 particle per frame
                };

                for _ in 0..spawn_count {
                    if self.particles.len() < self.max_particles {
                        let num_channels = device.architecture.memory_channels();
                        let channel = (self.frame as usize * 7 + device.index * 3 + self.particles.len()) % num_channels;
                        self.particles.push(MemoryParticle::new(channel, power_change, temp, self.frame, device.index));
                    }
                }
            }
        }

        // Update all particles
        for particle in &mut self.particles {
            particle.update();
        }

        // Remove dead particles
        self.particles.retain(|p| p.is_alive());
    }

    /// Render the Memory Dungeon
    pub fn render<B: TelemetryBackend>(&self, backend: &B) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let devices = backend.devices();
        if devices.is_empty() {
            return lines;
        }

        // Multi-device mode: render side-by-side columns
        let num_devices = devices.len();
        if num_devices > 1 {
            return self.render_multi_device(backend);
        }

        // Single device mode: use full width (original behavior)
        let device = &devices[0];
        let telem = backend.telemetry(0);
        let smbus = backend.smbus_telemetry(0);

        // Get metrics
        let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
        let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);
        let current = telem.map(|t| t.current_a()).unwrap_or(0.0);
        let power_change = self.baseline.power_change(device.index, power);
        let current_change = self.baseline.current_change(device.index, current);

        // === HEADER ===
        lines.push(self.render_header(device, telem, smbus));
        lines.push(self.render_separator());

        // Calculate total canvas height
        let canvas_height = self.height.saturating_sub(4);  // Reserve for header/footer

        // Create a canvas for particle overlay
        let canvas_width = self.width.min(120);

        // Render full-screen canvas with all layers and particles
        for row in 0..canvas_height {
            let mut spans = Vec::new();
            spans.push(Span::raw("  "));  // Left padding

            for col in 0..canvas_width {
                // Determine which layer this position belongs to
                let y_ratio = row as f32 / canvas_height as f32;
                let layer = if y_ratio < 0.15 {
                    // Bottom 15%: DDR
                    (col, row, 0)
                } else if y_ratio < 0.40 {
                    // 15-40%: L2 Cache
                    (col, row, 1)
                } else if y_ratio < 0.70 {
                    // 40-70%: L1 SRAM
                    (col, row, 2)
                } else {
                    // 70-100%: Tensix
                    (col, row, 3)
                };

                // Check for particles at this position
                let particle_here: Vec<_> = self.particles.iter()
                    .filter(|p| {
                        let px = p.x as usize;
                        let py = (canvas_height as f32 - p.y).max(0.0) as usize;
                        px == col && py == row
                    })
                    .collect();

                // Check for trails at this position
                let mut trail_here = None;
                for p in &self.particles {
                    for (age, (tx, ty)) in p.trail.iter().enumerate() {
                        let px = *tx as usize;
                        let py = (canvas_height as f32 - ty).max(0.0) as usize;
                        if px == col && py == row {
                            trail_here = Some((p.get_trail_char(), p.get_trail_color(age)));
                            break;
                        }
                    }
                    if trail_here.is_some() {
                        break;
                    }
                }

                // Check for environment glyphs
                let glyph_here = self.environment.iter()
                    .find(|(x, y, _, _)| *x == col && *y == row);

                // Render priority: particles > trails > environment > background
                if let Some(p) = particle_here.first() {
                    spans.push(Span::styled(
                        p.get_char().to_string(),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(p.get_color()).add_modifier(Modifier::BOLD),
                    ));
                } else if let Some((trail_char, trail_color)) = trail_here {
                    spans.push(Span::styled(
                        trail_char.to_string(),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(trail_color),
                    ));
                } else if let Some((_, _, ch, hue)) = glyph_here {
                    // Glyphs were near-invisible (sat 0.4, val 0.3). Raise both and
                    // let their hue drift with frame so they shimmer as background accents.
                    let glyph_color = hsv_to_rgb((*hue + self.frame as f32 * 1.8) % 360.0, 0.9, 0.62);
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(glyph_color),
                    ));
                } else {
                    // Background based on layer
                    spans.push(self.render_background(layer.2, col, row, power_change, current_change, temp));
                }
            }

            lines.push(Line::from(spans));
        }

        // === FOOTER ===
        lines.push(self.render_separator());
        lines.push(self.render_footer());

        lines
    }

    /// Render multi-device side-by-side view
    fn render_multi_device<B: TelemetryBackend>(&self, backend: &B) -> Vec<Line<'static>> {
        let devices = backend.devices();

        // Each side-by-side column needs at least 20 chars to be readable.
        // Beyond that threshold we switch to a compact fleet-grid view that
        // scales to any number of chips without wrapping off-screen.
        const MIN_CHIP_COL_WIDTH: usize = 20;
        let max_side_by_side = ((self.width.saturating_sub(2)) / MIN_CHIP_COL_WIDTH).max(1);
        if devices.len() > max_side_by_side {
            return self.render_fleet_grid(backend);
        }

        let mut lines = Vec::new();
        let num_devices = devices.len();

        // Calculate column width for each device
        let col_width = (self.width.saturating_sub(2)) / num_devices;  // Leave 2 chars padding

        // === GLOBAL HEADER ===

        // Top separator.
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(100, 100, 120))),
        ]));

        // Board-label row: one address label per multi-chip carrier board, left-aligned
        // inside each board's column span.  No box-drawing, no cross character — those
        // all had unicode-width math issues and made the row look broken.
        // Suppressed for standalone single-chip cards (has_multi_chip_boards() == false).
        if let Some(ref topo) = self.board_topology {
            if topo.has_multi_chip_boards() {
                let mut board_label_spans = vec![Span::raw("  ")];
                for board in topo.boards.iter() {
                    let board_chips_in_view = board.chips.iter()
                        .filter(|&&c| c < num_devices)
                        .count();
                    if board_chips_in_view == 0 {
                        continue;
                    }
                    let board_col_w = col_width * board_chips_in_view;
                    let board_color = hsv_to_rgb(board.hue, 0.7, 0.75);
                    // Show address if non-empty; pad the rest of the column with spaces.
                    let label = if board.label.is_empty() { String::new() } else { format!(" {}", board.label) };
                    let padding = board_col_w.saturating_sub(label.len());
                    board_label_spans.push(Span::styled(
                        format!("{}{}", label, " ".repeat(padding)),
                        Style::default().bg(colors::rgb(0, 0, 0)).fg(board_color),
                    ));
                }
                lines.push(Line::from(board_label_spans));
            }
        }

        // Per-device header row.
        let header_spans: Vec<Span> = devices.iter().take(num_devices).enumerate().map(|(idx, device)| {
            let telem = backend.telemetry(device.index);
            let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
            let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);

            // Use board hue if topology known, else 90° per-device hue.
            let hue = self.board_topology.as_ref()
                .map(|t| t.board_hue(device.index))
                .unwrap_or((idx as f32 * 90.0) % 360.0);
            let color = hsv_to_rgb(hue, 0.8, 0.9);

            let device_info = format!(" Dev{} {:.0}W {:.0}°C ", idx, power, temp);
            let padding_needed = col_width.saturating_sub(device_info.len());
            let padding = " ".repeat(padding_needed / 2);

            vec![
                Span::styled(padding.clone(), Style::default()),
                Span::styled(device_info, Style::default().bg(colors::rgb(0, 0, 0)).fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(padding, Style::default()),
            ]
        }).flatten().collect();

        lines.push(Line::from(header_spans));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(100, 100, 120))),
        ]));

        // === CANVAS ===
        let canvas_height = self.height.saturating_sub(6);  // Reserve for header/footer

        for row in 0..canvas_height {
            let mut spans = Vec::new();
            spans.push(Span::raw("  "));  // Left padding

            // Render each device column
            for (dev_idx, device) in devices.iter().take(num_devices).enumerate() {
                let x_offset = dev_idx * col_width;
                let hue_shift = (dev_idx as f32 * 90.0) % 360.0;

                let telem = backend.telemetry(device.index);
                let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
                let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);
                let current = telem.map(|t| t.current_a()).unwrap_or(0.0);
                let power_change = self.baseline.power_change(device.index, power);
                let current_change = self.baseline.current_change(device.index, current);

                // Render this device's column
                for col in 0..col_width {
                    let global_col = x_offset + col;

                    // Determine layer
                    let y_ratio = row as f32 / canvas_height as f32;
                    let layer = if y_ratio < 0.15 { 0 } else if y_ratio < 0.40 { 1 } else if y_ratio < 0.70 { 2 } else { 3 };

                    // Check for particles from this device in this position
                    let mut found_particle = false;
                    for particle in &self.particles {
                        if particle.source_device != device.index {
                            continue;  // Skip particles from other devices
                        }

                        let px = particle.x as usize;
                        let py = (canvas_height as f32 - particle.y) as usize;

                        if px == global_col && py == row && py < canvas_height {
                            let particle_char = particle.get_char();
                            let mut particle_hue = particle.hue + hue_shift;  // Apply device hue shift
                            if particle_hue > 360.0 { particle_hue -= 360.0; }
                            let particle_color = hsv_to_rgb(particle_hue, 0.9, particle.intensity);
                            spans.push(Span::styled(
                                particle_char.to_string(),
                                Style::default().bg(colors::rgb(0, 0, 0)).fg(particle_color).add_modifier(Modifier::BOLD)
                            ));
                            found_particle = true;
                            break;
                        }

                        // Check trail
                        if !found_particle {
                            for (tx, ty) in &particle.trail {
                                let trail_x = *tx as usize;
                                let trail_y = (canvas_height as f32 - ty) as usize;
                                if trail_x == global_col && trail_y == row && trail_y < canvas_height {
                                    let mut trail_hue = particle.hue + hue_shift;
                                    if trail_hue > 360.0 { trail_hue -= 360.0; }
                                    let trail_color = hsv_to_rgb(trail_hue, 0.85, (particle.intensity * 0.65).max(0.08));
                                    spans.push(Span::styled("·", Style::default().bg(colors::rgb(0, 0, 0)).fg(trail_color)));
                                    found_particle = true;
                                    break;
                                }
                            }
                        }
                        if found_particle { break; }
                    }

                    if !found_particle {
                        // No particle, render background with device hue shift
                        let bg_span = self.render_background(layer, global_col, row, power_change, current_change, temp);

                        // Apply hue shift to background colors
                        if let Some(fg) = bg_span.style.fg {
                            if let Color::Rgb(r, g, b) = fg {
                                // Convert to HSV, shift hue, convert back
                                let hsv = rgb_to_hsv(r, g, b);
                                let mut new_hue = hsv.0 + hue_shift;
                                if new_hue > 360.0 { new_hue -= 360.0; }
                                let shifted_color = hsv_to_rgb(new_hue, hsv.1, hsv.2);
                                spans.push(Span::styled(bg_span.content, Style::default().bg(colors::rgb(0, 0, 0)).fg(shifted_color)));
                            } else {
                                spans.push(bg_span);
                            }
                        } else {
                            spans.push(bg_span);
                        }
                    }
                }

                // Column separator between adjacent chip columns.
                // ║ (amber) at multi-chip board boundaries, │ (dim) otherwise.
                // For standalone single-chip cards, all separators are thin │.
                if dev_idx < num_devices - 1 {
                    let next_device_idx = devices.get(dev_idx + 1).map(|d| d.index).unwrap_or(dev_idx + 1);
                    let is_board_boundary = self.board_topology.as_ref()
                        .map(|t| t.has_multi_chip_boards() && !t.same_board(device.index, next_device_idx))
                        .unwrap_or(false);
                    if is_board_boundary {
                        // Inter-board boundary on a multi-chip carrier: amber thick separator.
                        spans.push(Span::styled("║", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(200, 160, 60))));
                    } else {
                        // Intra-board or standalone cards: muted thin separator.
                        spans.push(Span::styled("│", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(80, 80, 100))));
                    }
                }
            }

            lines.push(Line::from(spans));
        }

        // === FOOTER ===
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(100, 100, 120))),
        ]));
        let footer_text = format!("Showing {} devices side-by-side │ Particles color-coded by device", num_devices);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(footer_text, Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(160, 160, 160))),
        ]));

        lines
    }

    /// Fleet-grid view for large chip counts (> max_side_by_side).
    ///
    /// Lays chips out in a compact two-dimensional grid — each cell shows a
    /// short power bar and live metrics.  Column count scales with terminal
    /// width so the display is useful on anything from 80 to 320 columns.
    ///
    /// Each cell is 35 chars wide; columns are separated by " │ " (3 chars).
    /// Grid column count: `max(1, min(4, (width - 4) / 38))`.
    fn render_fleet_grid<B: TelemetryBackend>(&self, backend: &B) -> Vec<Line<'static>> {
        use crate::animation::common::temp_to_hue;

        let devices = backend.devices();
        let n = devices.len();

        // Dynamic column count: each cell + separator ≈ 38 chars.
        let grid_cols = ((self.width.saturating_sub(4)) / 38).max(1).min(4);

        // Arch summary for the header.
        let arch_summary = {
            let bh = devices.iter().filter(|d| matches!(d.architecture, crate::models::Architecture::Blackhole)).count();
            let wh = devices.iter().filter(|d| matches!(d.architecture, crate::models::Architecture::Wormhole)).count();
            let gs = devices.iter().filter(|d| matches!(d.architecture, crate::models::Architecture::Grayskull)).count();
            let mut parts: Vec<String> = Vec::new();
            if bh > 0 { parts.push(format!("{}× Blackhole", bh)); }
            if wh > 0 { parts.push(format!("{}× Wormhole", wh)); }
            if gs > 0 { parts.push(format!("{}× Grayskull", gs)); }
            parts.join(", ")
        };

        let sep_style  = Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(100, 100, 120));
        let hdr_style  = Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(220, 240, 255)).add_modifier(Modifier::BOLD);
        let col_sep    = Span::styled(" │ ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(80, 80, 100)));

        let mut lines = Vec::new();

        // Header
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), sep_style),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("🏰 Fleet View  ·  {} chips ({})", n, arch_summary), hdr_style),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), sep_style),
        ]));

        // Grid rows (row-major)
        let grid_rows = (n + grid_cols - 1) / grid_cols;
        for row in 0..grid_rows {
            let mut spans = vec![Span::raw("  ")];
            for col in 0..grid_cols {
                let chip_idx = row * grid_cols + col;
                if chip_idx >= n { break; }

                let device = &devices[chip_idx];
                let telem  = backend.telemetry(device.index);
                let power  = telem.map(|t| t.power_w()).unwrap_or(0.0);
                let temp   = telem.map(|t| t.temp_c()).unwrap_or(25.0);

                // 12-char power bar (each block ≈ 6.7 W up to 80 W)
                let filled = ((power / 80.0) * 12.0).clamp(0.0, 12.0) as usize;
                let bar: String = (0..12).map(|i| if i < filled { '█' } else { '░' }).collect();

                let hue       = temp_to_hue(temp);
                let bar_color = hsv_to_rgb(hue, 0.9, 0.85);
                let idx_color = hsv_to_rgb((chip_idx as f32 * 40.0) % 360.0, 0.6, 0.75);
                let arch_str  = device.architecture.abbrev();

                // "Dev  0 BH ████████░░░░ 16.1W 43.2°C"
                spans.push(Span::styled(
                    format!("Dev {:2} {} ", device.index, arch_str),
                    Style::default().bg(colors::rgb(0, 0, 0)).fg(idx_color),
                ));
                spans.push(Span::styled(
                    bar,
                    Style::default().bg(colors::rgb(0, 0, 0)).fg(bar_color).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!(" {:5.1}W {:5.1}°C", power, temp),
                    Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(180, 180, 180)),
                ));

                // Column separator (not after the last column in a row)
                if col + 1 < grid_cols && (row * grid_cols + col + 1) < n {
                    spans.push(col_sep.clone());
                }
            }
            lines.push(Line::from(spans));
        }

        // Footer
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), sep_style),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{} chips total │ switch to normal view with 'v'", n),
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(140, 140, 140)),
            ),
        ]));

        lines
    }

    /// Render background character for a layer
    ///
    /// Each layer's hue now drifts continuously with the frame counter so the
    /// background itself cycles through the full spectrum rather than being
    /// pinned to a static palette.  Saturation and value are also raised so
    /// the structural elements are visible without competing with particles.
    fn render_background(&self, layer: usize, col: usize, row: usize, power_change: f32, current_change: f32, temp: f32) -> Span<'static> {
        let f = self.frame as f32;
        match layer {
            0 => {
                // DDR: vertical walls — hue drifts through blues/purples/pinks
                if col % 12 == 0 {
                    let activity = ((f * 0.1 + col as f32 * 0.5).sin() + 1.0) / 2.0 * current_change;
                    let hue = (210.0 + f * 0.9) % 360.0;
                    let color = hsv_to_rgb(hue, 0.9, 0.45 + activity * 0.45);
                    Span::styled("║".to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                } else if row % 3 == 0 {
                    let hue = (200.0 + f * 0.7) % 360.0;
                    let color = hsv_to_rgb(hue, 0.75, 0.38);
                    Span::styled("═".to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                } else {
                    Span::raw(" ")
                }
            }
            1 => {
                // L2: staging rooms — hue sweeps the full spectrum slowly (was stuck at 45°)
                if (col % 15 == 0 || col % 15 == 14) && row % 4 < 3 {
                    let activity = ((f * 0.08 + col as f32 * 0.3).cos() + 1.0) / 2.0 * current_change;
                    let hue = (f * 0.6) % 360.0;
                    let color = hsv_to_rgb(hue, 0.9, 0.5 + activity * 0.42);
                    Span::styled("│".to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                } else if row % 4 == 0 && col % 15 < 14 {
                    let hue = (f * 0.6 + 30.0) % 360.0;
                    let color = hsv_to_rgb(hue, 0.8, 0.38);
                    Span::styled("─".to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                } else {
                    Span::raw(" ")
                }
            }
            2 => {
                // L1: cache vaults — hue cycles through greens/teals/blues
                if (col + row) % 8 == 0 {
                    let activity = ((f * 0.12 + col as f32 * 0.4 + row as f32 * 0.3).sin() + 1.0) / 2.0 * power_change;
                    let hue = (130.0 + f * 0.7) % 360.0;
                    let color = hsv_to_rgb(hue, 0.9, 0.5 + activity * 0.42);
                    Span::styled("◇".to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                } else if col % 10 == 0 {
                    let hue = (130.0 + f * 0.7 + 15.0) % 360.0;
                    let color = hsv_to_rgb(hue, 0.75, 0.38);
                    Span::styled("│".to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                } else {
                    Span::raw(" ")
                }
            }
            3 => {
                // Tensix: compute cores — temp biases hue but frame still cycles it
                if (col % 4 == 0 || col % 4 == 3) && row % 3 < 2 {
                    let wave = ((f * 0.1 + col as f32 * 0.5 + row as f32 * 0.4).sin() + 1.0) / 2.0;
                    let activity = (power_change * 0.7 + wave * 0.3).max(0.0).min(1.0);
                    // temp_to_hue gives 0-180°; adding frame cycle extends it to full 360°
                    let hue = (temp_to_hue(temp) + f * 2.5) % 360.0;
                    let color = hsv_to_rgb(hue, (0.88 + activity * 0.12).min(1.0), (0.58 + activity * 0.42).min(1.0));
                    let ch = if activity > 0.7 { '▓' } else if activity > 0.4 { '▒' } else { '░' };
                    Span::styled(ch.to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(color))
                } else {
                    Span::raw(" ")
                }
            }
            _ => Span::raw(" "),
        }
    }

    /// Render header with device info
    fn render_header(
        &self,
        device: &crate::models::Device,
        telem: Option<&crate::models::Telemetry>,
        smbus: Option<&crate::models::SmbusTelemetry>,
    ) -> Line<'static> {
        let mut spans = Vec::new();

        spans.push(Span::raw("  "));

        // Title
        spans.push(Span::styled(
            " 🏰 MEMORY DUNGEON ",
            Style::default()
                .fg(colors::rgb(220, 180, 255))
                .add_modifier(Modifier::BOLD),
        ));

        spans.push(Span::raw(" │ "));

        // Device info
        spans.push(Span::styled(
            format!("Device {}: {} ", device.index, device.architecture.abbrev()),
            Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(180, 200, 255)),
        ));

        spans.push(Span::raw("│ "));

        // Temperature
        if let Some(t) = telem {
            let temp = t.temp_c();
            let temp_color = if temp > 80.0 {
                colors::rgb(255, 100, 100)
            } else if temp > 65.0 {
                colors::rgb(255, 180, 100)
            } else {
                colors::rgb(100, 220, 100)
            };
            spans.push(Span::styled(
                format!("🌡 {:.1}°C ", temp),
                Style::default().bg(colors::rgb(0, 0, 0)).fg(temp_color),
            ));

            spans.push(Span::raw("│ "));

            // Power
            spans.push(Span::styled(
                format!("⚡ {:.1}W ", t.power_w()),
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(255, 220, 100)),
            ));

            spans.push(Span::raw("│ "));

            // Current
            spans.push(Span::styled(
                format!("⚙ {:.1}A ", t.current_a()),
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(100, 180, 255)),
            ));
        }

        // ARC health
        if let Some(s) = smbus {
            let healthy = s.is_arc0_healthy();
            let arc_color = if healthy {
                colors::rgb(100, 255, 100)
            } else {
                colors::rgb(255, 100, 100)
            };
            spans.push(Span::raw("│ ARC: "));
            spans.push(Span::styled(
                if healthy { "●" } else { "○" },
                Style::default().bg(colors::rgb(0, 0, 0)).fg(arc_color),
            ));
        }

        // Particle count
        spans.push(Span::raw(format!(" │ Particles: {} ", self.particles.len())));

        Line::from(spans)
    }

    /// Render separator line - just a nice visual break, no exact alignment needed
    fn render_separator(&self) -> Line<'static> {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "─".repeat(100),  // Simple, looks good at any width
                Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(80, 80, 100)),
            ),
        ])
    }

    /// Render footer with legend
    fn render_footer(&self) -> Line<'static> {
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Particles: ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(150, 150, 150))),
            Span::styled("○◉ ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(100, 200, 255))),
            Span::raw("Read │ "),
            Span::styled("□■ ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(255, 180, 100))),
            Span::raw("Write │ "),
            Span::styled("◇◆ ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(100, 255, 200))),
            Span::raw("CacheHit │ "),
            Span::styled("●⬤ ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(255, 100, 100))),
            Span::raw("Miss │ "),
            Span::styled("·•▪ ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(120, 120, 120))),
            Span::raw("Trails │ "),
            Span::styled("⚡※☼♦◊ ", Style::default().bg(colors::rgb(0, 0, 0)).fg(colors::rgb(180, 150, 200))),
            Span::raw("Glyphs"),
        ])
    }
}
