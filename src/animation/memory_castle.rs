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
use crate::backend::TelemetryBackend;
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
            hue: temp_to_hue(temp) + (frame % 60) as f32,  // Vary hue slightly
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
        let (base_hue, saturation_boost) = match self.particle_type {
            ParticleType::Read => (self.hue, 0.2),       // Original hue
            ParticleType::Write => (self.hue + 60.0, 0.3),  // Shift toward orange
            ParticleType::CacheHit => (180.0, 0.4),      // Cyan
            ParticleType::CacheMiss => (0.0, 0.5),       // Red
        };

        hsv_to_rgb(
            base_hue % 360.0,
            0.7 + saturation_boost,
            0.6 + self.intensity * 0.4
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

    /// Get trail color (dimmer version)
    pub fn get_trail_color(&self, age: usize) -> Color {
        let (base_hue, _) = match self.particle_type {
            ParticleType::Read => (self.hue, 0.0),
            ParticleType::Write => (self.hue + 60.0, 0.0),
            ParticleType::CacheHit => (180.0, 0.0),
            ParticleType::CacheMiss => (0.0, 0.0),
        };

        let fade = (8 - age.min(8)) as f32 / 8.0;
        hsv_to_rgb(
            base_hue % 360.0,
            0.5,
            0.3 * fade * self.intensity
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
            let x = (i * 17 + 7) % (width.saturating_sub(4));
            let y = (i * 23 + 13) % (height.saturating_sub(6));
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
        }
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
                        Style::default().fg(p.get_color()).add_modifier(Modifier::BOLD),
                    ));
                } else if let Some((trail_char, trail_color)) = trail_here {
                    spans.push(Span::styled(
                        trail_char.to_string(),
                        Style::default().fg(trail_color),
                    ));
                } else if let Some((_, _, ch, hue)) = glyph_here {
                    let glyph_color = hsv_to_rgb(*hue, 0.4, 0.3);
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(glyph_color),
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
        let mut lines = Vec::new();
        let devices = backend.devices();
        let num_devices = devices.len().min(4);  // Max 4 devices

        // Calculate column width for each device
        let col_width = (self.width.saturating_sub(2)) / num_devices;  // Leave 2 chars padding

        // === GLOBAL HEADER ===
        let header_spans: Vec<Span> = devices.iter().take(num_devices).enumerate().map(|(idx, device)| {
            let telem = backend.telemetry(device.index);
            let power = telem.map(|t| t.power_w()).unwrap_or(0.0);
            let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);

            // Color-code each device by hue shift
            let hue = (idx as f32 * 90.0) % 360.0;
            let color = hsv_to_rgb(hue, 0.8, 0.9);

            let device_info = format!(" Dev{} {:.0}W {:.0}°C ", idx, power, temp);
            let padding_needed = col_width.saturating_sub(device_info.len());
            let padding = " ".repeat(padding_needed / 2);

            vec![
                Span::styled(padding.clone(), Style::default()),
                Span::styled(device_info, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(padding, Style::default()),
            ]
        }).flatten().collect();

        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), Style::default().fg(Color::Rgb(100, 100, 120))),
        ]));
        lines.push(Line::from(header_spans));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), Style::default().fg(Color::Rgb(100, 100, 120))),
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
                                Style::default().fg(particle_color).add_modifier(Modifier::BOLD)
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
                                    let trail_color = hsv_to_rgb(trail_hue, 0.5, particle.intensity * 0.5);
                                    spans.push(Span::styled("·", Style::default().fg(trail_color)));
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
                                spans.push(Span::styled(bg_span.content, Style::default().fg(shifted_color)));
                            } else {
                                spans.push(bg_span);
                            }
                        } else {
                            spans.push(bg_span);
                        }
                    }
                }

                // Column separator
                if dev_idx < num_devices - 1 {
                    spans.push(Span::styled("│", Style::default().fg(Color::Rgb(80, 80, 100))));
                }
            }

            lines.push(Line::from(spans));
        }

        // === FOOTER ===
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("─".repeat(self.width.saturating_sub(2)), Style::default().fg(Color::Rgb(100, 100, 120))),
        ]));
        let footer_text = format!("Showing {} devices side-by-side │ Particles color-coded by device", num_devices);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(footer_text, Style::default().fg(Color::Rgb(160, 160, 160))),
        ]));

        lines
    }

    /// Render background character for a layer
    fn render_background(&self, layer: usize, col: usize, row: usize, power_change: f32, current_change: f32, temp: f32) -> Span<'static> {
        match layer {
            0 => {
                // DDR: vertical walls with gates
                if col % 12 == 0 {
                    let activity = ((self.frame as f32 * 0.1 + col as f32 * 0.5).sin() + 1.0) / 2.0 * current_change;
                    let color = hsv_to_rgb(270.0, 0.6, 0.3 + activity * 0.3);
                    Span::styled("║".to_string(), Style::default().fg(color))
                } else if row % 3 == 0 {
                    Span::styled("═".to_string(), Style::default().fg(Color::Rgb(80, 60, 100)))
                } else {
                    Span::raw(" ")
                }
            }
            1 => {
                // L2: staging rooms with boxes
                if (col % 15 == 0 || col % 15 == 14) && row % 4 < 3 {
                    let activity = ((self.frame as f32 * 0.08 + col as f32 * 0.3).cos() + 1.0) / 2.0 * current_change;
                    let color = hsv_to_rgb(45.0, 0.7, 0.4 + activity * 0.4);
                    Span::styled("│".to_string(), Style::default().fg(color))
                } else if row % 4 == 0 && col % 15 < 14 {
                    let color = hsv_to_rgb(45.0, 0.5, 0.3);
                    Span::styled("─".to_string(), Style::default().fg(color))
                } else {
                    Span::raw(" ")
                }
            }
            2 => {
                // L1: cache vaults with diamonds
                if (col + row) % 8 == 0 {
                    let activity = ((self.frame as f32 * 0.12 + col as f32 * 0.4 + row as f32 * 0.3).sin() + 1.0) / 2.0 * power_change;
                    let color = hsv_to_rgb(180.0, 0.6, 0.4 + activity * 0.4);
                    Span::styled("◇".to_string(), Style::default().fg(color))
                } else if col % 10 == 0 {
                    let color = hsv_to_rgb(180.0, 0.4, 0.3);
                    Span::styled("│".to_string(), Style::default().fg(color))
                } else {
                    Span::raw(" ")
                }
            }
            3 => {
                // Tensix: compute cores with blocks
                if (col % 4 == 0 || col % 4 == 3) && row % 3 < 2 {
                    let wave = ((self.frame as f32 * 0.1 + col as f32 * 0.5 + row as f32 * 0.4).sin() + 1.0) / 2.0;
                    let activity = (power_change * 0.7 + wave * 0.3).max(0.0).min(1.0);
                    let hue = temp_to_hue(temp);
                    let color = hsv_to_rgb(hue, 0.7 + activity * 0.3, 0.5 + activity * 0.5);
                    let ch = if activity > 0.7 { '▓' } else if activity > 0.4 { '▒' } else { '░' };
                    Span::styled(ch.to_string(), Style::default().fg(color))
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
                .fg(Color::Rgb(220, 180, 255))
                .add_modifier(Modifier::BOLD),
        ));

        spans.push(Span::raw(" │ "));

        // Device info
        spans.push(Span::styled(
            format!("Device {}: {} ", device.index, device.architecture.abbrev()),
            Style::default().fg(Color::Rgb(180, 200, 255)),
        ));

        spans.push(Span::raw("│ "));

        // Temperature
        if let Some(t) = telem {
            let temp = t.temp_c();
            let temp_color = if temp > 80.0 {
                Color::Rgb(255, 100, 100)
            } else if temp > 65.0 {
                Color::Rgb(255, 180, 100)
            } else {
                Color::Rgb(100, 220, 100)
            };
            spans.push(Span::styled(
                format!("🌡 {:.1}°C ", temp),
                Style::default().fg(temp_color),
            ));

            spans.push(Span::raw("│ "));

            // Power
            spans.push(Span::styled(
                format!("⚡ {:.1}W ", t.power_w()),
                Style::default().fg(Color::Rgb(255, 220, 100)),
            ));

            spans.push(Span::raw("│ "));

            // Current
            spans.push(Span::styled(
                format!("⚙ {:.1}A ", t.current_a()),
                Style::default().fg(Color::Rgb(100, 180, 255)),
            ));
        }

        // ARC health
        if let Some(s) = smbus {
            let healthy = s.is_arc0_healthy();
            let arc_color = if healthy {
                Color::Rgb(100, 255, 100)
            } else {
                Color::Rgb(255, 100, 100)
            };
            spans.push(Span::raw("│ ARC: "));
            spans.push(Span::styled(
                if healthy { "●" } else { "○" },
                Style::default().fg(arc_color),
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
                Style::default().fg(Color::Rgb(80, 80, 100)),
            ),
        ])
    }

    /// Render footer with legend
    fn render_footer(&self) -> Line<'static> {
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Particles: ", Style::default().fg(Color::Rgb(150, 150, 150))),
            Span::styled("○◉ ", Style::default().fg(Color::Rgb(100, 200, 255))),
            Span::raw("Read │ "),
            Span::styled("□■ ", Style::default().fg(Color::Rgb(255, 180, 100))),
            Span::raw("Write │ "),
            Span::styled("◇◆ ", Style::default().fg(Color::Rgb(100, 255, 200))),
            Span::raw("CacheHit │ "),
            Span::styled("●⬤ ", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::raw("Miss │ "),
            Span::styled("·•▪ ", Style::default().fg(Color::Rgb(120, 120, 120))),
            Span::raw("Trails │ "),
            Span::styled("⚡※☼♦◊ ", Style::default().fg(Color::Rgb(180, 150, 200))),
            Span::raw("Glyphs"),
        ])
    }
}
