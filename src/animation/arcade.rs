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
    AdaptiveBaseline, HardwareStarfield, MemoryCastle, MemoryFlowVis,
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

        let starfield_start = 3; // After header
        let starfield_end = starfield_start + starfield_height;
        let castle_start = starfield_end;
        let castle_end = castle_start + castle_height;
        let flow_start = castle_end;
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
        }
    }

    /// Initialize from devices (called once after creation)
    pub fn initialize_from_devices(&mut self, devices: &[crate::models::Device]) {
        self.starfield.initialize_from_devices(devices);
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

        // Render header
        lines.push(self.render_header(backend));
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

    /// Overlay hero character and trail onto the rendered canvas
    fn overlay_hero<B: TelemetryBackend>(
        &self,
        lines: Vec<Line<'static>>,
        _backend: &B,
    ) -> Vec<Line<'static>> {
        // Hero overlay is complex and can cause panics when trying to modify
        // arbitrary Line structures with multiple Spans.
        // For now, skip overlay and just return the composite canvas.
        // The hero status is shown in the footer instead.
        // TODO: Implement proper overlay by rendering to a character grid first,
        // then converting to styled Lines.
        lines
    }

    /// Render a single trail character
    fn render_trail_char(&self, lines: &mut [Line<'static>], pos: &TrailPosition, temp: f32) {
        let row = pos.y as usize;
        let col = pos.x as usize;

        if row >= lines.len() || col >= self.width {
            return;
        }

        // Fade out based on age (exponential fade)
        let fade = 1.0 - (pos.age as f32 / 20.0);
        let fade_exp = fade * fade; // Exponential fade

        // Trail character based on age
        let trail_char = match pos.age {
            0..=4 => '○',
            5..=9 => '◦',
            10..=14 => '•',
            _ => '·',
        };

        // Trail color (temperature-based but dimmed)
        let hue = temp_to_hue(temp);
        let saturation = 0.5 * fade_exp;
        let value = 0.4 * fade_exp;
        let trail_color = hsv_to_rgb(hue, saturation, value);

        // Replace character at position
        let line = &lines[row];
        let spans: Vec<Span> = line
            .spans
            .iter()
            .enumerate()
            .flat_map(|(span_idx, span)| {
                let chars: Vec<char> = span.content.chars().collect();
                chars
                    .iter()
                    .enumerate()
                    .map(|(char_idx, &ch)| {
                        let abs_col = span_idx * 10 + char_idx; // Approximate column
                        if abs_col == col {
                            Span::styled(trail_char.to_string(), Style::default().bg(colors::rgb(0, 0, 0)).fg(trail_color))
                        } else {
                            Span::styled(ch.to_string(), span.style)
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        lines[row] = Line::from(spans);
    }

    /// Render hero character
    fn render_hero_char(&self, lines: &mut [Line<'static>], temp: f32, heartbeat: u32) {
        let row = self.hero_y as usize;
        let col = self.hero_x as usize;

        if row >= lines.len() || col >= self.width {
            return;
        }

        // Hero character (classic roguelike '@')
        let hero_char = '@';

        // Hero color (temperature-based, full saturation, pulsing brightness)
        let hue = temp_to_hue(temp);
        let saturation = 1.0;
        let pulse = ((heartbeat % 60) as f32 / 60.0 * std::f32::consts::PI * 2.0).sin();
        let value = 0.85 + pulse * 0.15; // Pulse between 0.85 and 1.0
        let hero_color = hsv_to_rgb(hue, saturation, value);

        // Hero has BOLD modifier for maximum visibility
        let hero_style = Style::default()
            .fg(hero_color)
            .add_modifier(Modifier::BOLD);

        // Replace character at hero position (simplified - just prepend/append)
        // Note: This is a simplified overlay. For pixel-perfect positioning,
        // we'd need to track exact column offsets across all spans.
        let line = &lines[row];
        let content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        if col < content.len() {
            let mut new_content = content.chars().collect::<Vec<_>>();
            new_content[col] = hero_char;
            let content_str: String = new_content.iter().collect();

            // Reconstruct line with hero character highlighted
            let before: String = content_str.chars().take(col).collect();
            let after: String = content_str.chars().skip(col + 1).collect();

            lines[row] = Line::from(vec![
                Span::raw(before),
                Span::styled(hero_char.to_string(), hero_style),
                Span::raw(after),
            ]);
        }
    }
}
