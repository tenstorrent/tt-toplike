// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Hardware-responsive starfield visualization
//!
//! This module implements the core visualization where Tensix cores become stars,
//! memory channels become planets, and all motion is driven by real hardware telemetry.
//!
//! Key principles:
//! - Star positions match actual chip topology (GS: 10×12, WH: 8×10, BH: 14×16)
//! - Color = Temperature (cyan→green→yellow→orange→red)
//! - Brightness = Power consumption
//! - Twinkle rate = Current draw
//! - No fake animations - everything driven by real hardware state

use crate::backend::TelemetryBackend;
use crate::models::Device;
use crate::animation::AdaptiveBaseline;
use crate::animation::topology::{BoardTopology, sync_score};
use crate::ui::colors;
use ratatui::style::Color;
use std::collections::HashMap;
use std::f32;

/// Character progression for star brightness levels
const STAR_CHARS: [char; 5] = ['·', '∘', '○', '◉', '●'];

/// Character progression for memory planet intensity
const PLANET_CHARS: [char; 5] = ['·', '░', '▒', '▓', '█'];

// FLOW_CHARS removed: DataStream now uses topology-aware '─' (intra-board) / '═' (inter-board).

/// A single star representing a Tensix core
#[derive(Debug, Clone)]
pub struct Star {
    /// X position in terminal coordinates
    pub x: usize,

    /// Y position in terminal coordinates
    pub y: usize,

    /// Device index this star belongs to
    pub device_idx: usize,

    /// Core index within the device
    pub core_idx: usize,

    /// Current brightness (0.0 = dim, 1.0 = bright)
    pub brightness: f32,

    /// Current color (temperature-based)
    pub color: Color,

    /// Animation phase (for twinkling)
    pub phase: f32,

    /// Z-depth layer (0.0 = far background, 1.0 = foreground)
    /// Used for parallax effect and size variation
    pub depth: f32,

    /// Secondary twinkle phase (different frequency for complexity)
    pub phase2: f32,

    /// Sparkle counter (for occasional bright flashes)
    pub sparkle: f32,
}

impl Star {
    /// Get the character to render based on brightness and depth
    ///
    /// Foreground stars (depth close to 1.0) render larger/brighter
    /// Background stars (depth close to 0.0) render smaller/dimmer
    pub fn get_char(&self) -> char {
        // Sparkle effect - occasionally show as brightest character
        if self.sparkle > 0.8 {
            return '✦'; // Bright sparkle
        }

        // Depth affects perceived brightness (foreground = brighter)
        let depth_brightness = self.brightness * (0.4 + self.depth * 0.6);

        let idx = (depth_brightness * (STAR_CHARS.len() - 1) as f32) as usize;
        STAR_CHARS[idx.min(STAR_CHARS.len() - 1)]
    }
}

/// A memory planet representing L1/L2/DDR hierarchy
#[derive(Debug, Clone)]
pub struct MemoryPlanet {
    /// X position in terminal coordinates
    pub x: usize,

    /// Y position in terminal coordinates
    pub y: usize,

    /// Device index this planet belongs to
    pub device_idx: usize,

    /// Memory level (0=L1, 1=L2, 2=DDR)
    pub level: usize,

    /// Channel index for DDR, cache index for L1/L2
    pub channel_idx: usize,

    /// Current activity level (0.0 to 1.0)
    pub activity: f32,

    /// Current color
    pub color: Color,

    /// Orbital angle (0.0 to 2π) for circular motion around device
    pub angle: f32,

    /// Orbital radius from device center
    pub radius: f32,

    /// Pulsing phase for size animation
    pub pulse: f32,
}

impl MemoryPlanet {
    /// Get the character to render based on activity
    pub fn get_char(&self) -> char {
        match self.level {
            0 => '◆', // L1 - diamond
            1 => '◇', // L2 - outline diamond
            2 => {
                // DDR - intensity-based blocks
                let idx = (self.activity * (PLANET_CHARS.len() - 1) as f32) as usize;
                PLANET_CHARS[idx.min(PLANET_CHARS.len() - 1)]
            }
            _ => '·',
        }
    }

    /// Get color for this memory level
    pub fn get_color(&self) -> Color {
        match self.level {
            0 => colors::rgb(100, 180, 255),   // L1 - blue (was colors::INFO)
            1 => colors::rgb(255, 180, 100),   // L2 - orange (was colors::WARNING)
            2 => colors::rgb(255, 100, 100),   // DDR - red (was colors::ERROR)
            _ => colors::rgb(160, 160, 160),   // Fallback grey
        }
    }
}

/// A data flow stream between devices
#[derive(Debug, Clone)]
pub struct DataStream {
    /// X position in terminal coordinates
    pub x: usize,

    /// Y position in terminal coordinates
    pub y: usize,

    /// Source device index
    pub from_device: usize,

    /// Target device index
    pub to_device: usize,

    /// Flow intensity / sync score (0.0 to 1.0)
    pub intensity: f32,

    /// Animation offset
    pub offset: f32,

    /// Whether both endpoints are on the same physical board.
    /// Intra-board streams use `─` and are always-on (floor 0.2).
    /// Inter-board streams use `═` and only appear when both sides are active.
    pub intra_board: bool,
}

impl DataStream {
    /// Get the character to render based on topology.
    ///
    /// `─` for intra-board links (always visible, even when idle).
    /// `═` for inter-board links (heavier, only shown when active).
    pub fn get_char(&self) -> char {
        if self.intra_board { '─' } else { '═' }
    }
}

/// Hardware-responsive starfield visualization system
pub struct HardwareStarfield {
    /// All stars (Tensix cores)
    stars: Vec<Star>,

    /// All memory planets (L1/L2/DDR)
    planets: Vec<MemoryPlanet>,

    /// Data flow streams between devices
    streams: Vec<DataStream>,

    /// Adaptive baseline for relative activity detection
    baseline: AdaptiveBaseline,

    /// Animation frame counter
    frame: u32,

    /// Display width in characters
    width: usize,

    /// Display height in characters
    height: usize,

    /// Board topology (set once after backend init).
    /// Drives stream character selection and intra-board floor.
    board_topology: Option<BoardTopology>,

    /// Snapshot of per-device baseline-relative power change,
    /// updated each frame by `update_from_telemetry`.
    /// Used by stream rendering to compute `sync_score`.
    device_activity: HashMap<usize, f32>,
}

impl HardwareStarfield {
    /// Create a new hardware starfield
    ///
    /// # Arguments
    ///
    /// * `width` - Display width in terminal characters
    /// * `height` - Display height in terminal characters
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            stars: Vec::new(),
            planets: Vec::new(),
            streams: Vec::new(),
            baseline: AdaptiveBaseline::new(),
            frame: 0,
            width,
            height,
            board_topology: None,
            device_activity: HashMap::new(),
        }
    }

    /// Install board topology for topology-aware stream rendering.
    ///
    /// Call this once after backend initialisation when SMBUS `board_id`
    /// data may be available.  Safe to call at any point — the change
    /// takes effect on the next `update_from_telemetry` call.
    pub fn set_topology(&mut self, topology: BoardTopology) {
        self.board_topology = Some(topology);
        // Re-tag all existing streams so they don't need a full reinit.
        if let Some(ref topo) = self.board_topology {
            for stream in &mut self.streams {
                stream.intra_board = topo.same_board(stream.from_device, stream.to_device);
            }
        }
    }

    /// Access the current board topology, if set.
    pub fn topology(&self) -> Option<&BoardTopology> {
        self.board_topology.as_ref()
    }

    /// Initialize stars and planets from device topology
    ///
    /// Creates star positions based on actual Tensix grid layout for each architecture.
    ///
    /// # Arguments
    ///
    /// * `devices` - Slice of detected devices
    pub fn initialize_from_devices(&mut self, devices: &[Device]) {
        self.stars.clear();
        self.planets.clear();

        let num_devices = devices.len();
        if num_devices == 0 {
            return;
        }

        // Calculate device spacing across screen width
        let device_spacing = self.width / num_devices.max(1);

        for (device_idx, device) in devices.iter().enumerate() {
            // Calculate center position for this device (horizontal only)
            let device_center_x = (device_idx * device_spacing) + (device_spacing / 2);

            // Get Tensix grid dimensions for this architecture
            let (grid_rows, grid_cols) = device.architecture.tensix_grid();

            // Cell spacing: at least 1 so no rows/cols collapse in small panels
            // (arcade starfield can be as few as 8 rows).
            let star_spacing_x = ((device_spacing.saturating_sub(2)) / grid_cols.max(1)).max(1);
            let star_spacing_y = (self.height / grid_rows.max(1)).max(1);

            // Span = distance from the first dot to the last dot (dot-center to dot-center).
            // Centering the span gives equal whitespace above/below and left/right of the grid.
            let grid_span_x = (grid_cols.saturating_sub(1)) * star_spacing_x;
            let grid_span_y = (grid_rows.saturating_sub(1)) * star_spacing_y;

            let x_start = device_center_x.saturating_sub(grid_span_x / 2);
            let y_start = (self.height.saturating_sub(grid_span_y)) / 2;

            // Planets orbit the geometric centre of the star grid.
            let device_center_y = y_start + grid_span_y / 2;

            // Create stars for each Tensix core
            for row in 0..grid_rows {
                for col in 0..grid_cols {
                    let x = x_start + col * star_spacing_x;
                    let y = y_start + row * star_spacing_y;

                    // Ensure within bounds
                    if x < self.width && y < self.height {
                        // Assign varied z-depth for parallax effect
                        // Core position influences depth (edge cores = background, center = foreground)
                        let dist_from_center_x = (col as f32 - grid_cols as f32 / 2.0).abs();
                        let dist_from_center_y = (row as f32 - grid_rows as f32 / 2.0).abs();
                        let depth = 1.0 - ((dist_from_center_x + dist_from_center_y) /
                                         (grid_cols as f32 + grid_rows as f32)) * 0.6;

                        self.stars.push(Star {
                            x,
                            y,
                            device_idx,
                            core_idx: row * grid_cols + col,
                            brightness: 0.3, // Start dim
                            color: colors::PRIMARY,
                            phase: (row + col) as f32 * 0.5, // Varied animation phases
                            depth: depth.clamp(0.3, 1.0),
                            phase2: (row * 3 + col * 7) as f32 * 0.3, // Different frequency
                            sparkle: 0.0,
                        });
                    }
                }
            }

            // Create memory planets around device perimeter
            let num_channels = device.architecture.memory_channels();

            // DDR planets (outer ring)
            for i in 0..num_channels {
                let angle = (i as f32 / num_channels as f32) * 2.0 * f32::consts::PI;
                let radius = (device_spacing / 2).min(self.height / 3) as f32;
                let px = device_center_x as f32 + angle.cos() * radius;
                let py = device_center_y as f32 + angle.sin() * radius;

                if px >= 0.0 && (px as usize) < self.width && py >= 0.0 && (py as usize) < self.height {
                    self.planets.push(MemoryPlanet {
                        x: px as usize,
                        y: py as usize,
                        device_idx,
                        level: 2, // DDR
                        channel_idx: i,
                        activity: 0.0,
                        color: colors::ERROR,
                        angle,
                        radius,
                        pulse: i as f32 * 0.4, // Varied pulsing phases
                    });
                }
            }

            // L2 planets (middle ring) - 8 cache banks
            for i in 0..8 {
                let angle = (i as f32 / 8.0) * 2.0 * f32::consts::PI + 0.4; // Offset from DDR
                let radius = (device_spacing / 3).min(self.height / 4) as f32;
                let px = device_center_x as f32 + angle.cos() * radius;
                let py = device_center_y as f32 + angle.sin() * radius;

                if px >= 0.0 && (px as usize) < self.width && py >= 0.0 && (py as usize) < self.height {
                    self.planets.push(MemoryPlanet {
                        x: px as usize,
                        y: py as usize,
                        device_idx,
                        level: 1, // L2
                        channel_idx: i,
                        activity: 0.0,
                        color: colors::WARNING,
                        angle,
                        radius,
                        pulse: i as f32 * 0.5 + 0.3, // Varied pulsing phases
                    });
                }
            }

            // L1 planets (inner ring) - 4 SRAM banks
            for i in 0..4 {
                let angle = (i as f32 / 4.0) * 2.0 * f32::consts::PI + 0.8; // Offset from L2
                let radius = (device_spacing / 5).min(self.height / 6) as f32;
                let px = device_center_x as f32 + angle.cos() * radius;
                let py = device_center_y as f32 + angle.sin() * radius;

                if px >= 0.0 && (px as usize) < self.width && py >= 0.0 && (py as usize) < self.height {
                    self.planets.push(MemoryPlanet {
                        x: px as usize,
                        y: py as usize,
                        device_idx,
                        level: 0, // L1
                        channel_idx: i,
                        activity: 0.0,
                        color: colors::INFO,
                        angle,
                        radius,
                        pulse: i as f32 * 0.7 + 0.6, // Varied pulsing phases
                    });
                }
            }
        }

        // Initialize data streams between devices (if multiple devices)
        if num_devices > 1 {
            let device_spacing = self.width / num_devices;
            for from_idx in 0..num_devices {
                for to_idx in (from_idx + 1)..num_devices {
                    let from_x = (from_idx * device_spacing) + (device_spacing / 2);
                    let to_x = (to_idx * device_spacing) + (device_spacing / 2);

                    // Determine topology relationship for this stream pair.
                    let intra = self.board_topology.as_ref()
                        .map(|t| t.same_board(from_idx, to_idx))
                        .unwrap_or(false);

                    // Create stream segments along the horizontal path.
                    let num_steps = ((to_x - from_x) / 5).max(1);
                    for step in 0..num_steps {
                        let x = from_x + (to_x - from_x) * step / num_steps;
                        let y = self.height / 2;

                        if x < self.width && y < self.height {
                            self.streams.push(DataStream {
                                x,
                                y,
                                from_device: from_idx,
                                to_device: to_idx,
                                intensity: 0.0,
                                offset: step as f32 / num_steps as f32,
                                intra_board: intra,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Update visualization from current hardware telemetry
    ///
    /// This is where the magic happens - every visual element gets updated
    /// based on real hardware state.
    ///
    /// # Arguments
    ///
    /// * `backend` - Backend providing telemetry data
    pub fn update_from_telemetry<B: TelemetryBackend>(&mut self, backend: &B) {
        // Update baseline learning and snapshot per-device activity.
        for device in backend.devices() {
            if let Some(telem) = backend.telemetry(device.index) {
                let power = telem.power_w();
                let current = telem.current_a();
                let temp = telem.temp_c();
                let aiclk = telem.aiclk_mhz() as f32;

                self.baseline.update(device.index, power, current, temp, aiclk);

                // Cache baseline-relative power change for stream sync_score.
                let activity = self.baseline.power_change(device.index, power)
                    .max(0.0).min(1.0);
                self.device_activity.insert(device.index, activity);
            }
        }

        // Update star properties based on telemetry
        for star in &mut self.stars {
            if let Some(telem) = backend.telemetry(star.device_idx) {
                let power = telem.power_w();
                let current = telem.current_a();
                let temp = telem.temp_c();

                // Brightness from power (relative to baseline)
                let power_change = self.baseline.power_change(star.device_idx, power);
                let base_brightness = 0.3 + power_change.max(0.0).min(1.0) * 0.7;

                // Multi-frequency twinkling for more organic star behavior
                let current_change = self.baseline.current_change(star.device_idx, current);
                let twinkle_speed = 0.1 + current_change.max(0.0).min(1.0) * 0.3;
                star.phase += twinkle_speed;
                star.phase2 += twinkle_speed * 1.7; // Different frequency

                // Combine two sine waves for complex twinkling
                let twinkle1 = (star.phase.sin() * 0.5 + 0.5) * 0.15;
                let twinkle2 = (star.phase2.cos() * 0.5 + 0.5) * 0.10;
                let twinkle = twinkle1 + twinkle2; // ±25% variation total

                // Sparkle effect - occasional bright flash when power spikes
                if power_change > 0.7 && (star.sparkle == 0.0 || star.sparkle > 0.95) {
                    // Trigger sparkle with random chance
                    if (self.frame + star.core_idx as u32) % 50 == 0 {
                        star.sparkle = 1.0;
                    }
                }
                // Decay sparkle
                if star.sparkle > 0.0 {
                    star.sparkle -= 0.05;
                    if star.sparkle < 0.0 {
                        star.sparkle = 0.0;
                    }
                }

                star.brightness = (base_brightness + twinkle).clamp(0.0, 1.0);

                // Color: full 360° rainbow cycling driven by temp + time + core position.
                // Each core starts at a different phase so the grid shows a colour wave.
                use crate::animation::{hsv_to_rgb, temp_to_hue};
                let hue = (temp_to_hue(temp)
                    + self.frame as f32 * 2.0
                    + star.core_idx as f32 * 2.7) % 360.0;
                star.color = hsv_to_rgb(hue, 1.0, 1.0);
            }
        }

        // Update memory planet activity
        for planet in &mut self.planets {
            if let Some(telem) = backend.telemetry(planet.device_idx) {
                let current = telem.current_a();
                let current_change = self.baseline.current_change(planet.device_idx, current);

                // Different memory levels respond to different metrics
                planet.activity = match planet.level {
                    0 => {
                        // L1: Responds to power (compute activity)
                        let power = telem.power_w();
                        let power_change = self.baseline.power_change(planet.device_idx, power);
                        power_change.max(0.0).min(1.0)
                    }
                    1 => {
                        // L2: Responds to current (memory traffic)
                        current_change.max(0.0).min(1.0)
                    }
                    2 => {
                        // DDR: Responds to combined activity
                        let power = telem.power_w();
                        let power_change = self.baseline.power_change(planet.device_idx, power);
                        ((power_change + current_change) / 2.0).max(0.0).min(1.0)
                    }
                    _ => 0.0,
                };

                // Planets: each memory level cycles through its own hue range,
                // per-channel offset spreads the colours so they aren't all in sync.
                use crate::animation::hsv_to_rgb as _hsv;
                let planet_hue = match planet.level {
                    0 => (240.0 + self.frame as f32 * 1.5 + planet.channel_idx as f32 * 90.0) % 360.0,
                    1 => (120.0 + self.frame as f32 * 1.0 + planet.channel_idx as f32 * 45.0) % 360.0,
                    _ => (self.frame as f32 * 2.5 + planet.channel_idx as f32 * 30.0) % 360.0,
                };
                let planet_value = 0.6 + planet.activity * 0.4;
                planet.color = _hsv(planet_hue, 1.0, planet_value);

                // Orbital motion - planets orbit around device center
                // Different levels orbit at different speeds
                let orbit_speed = match planet.level {
                    0 => 0.05,  // L1 fastest (inner ring)
                    1 => 0.03,  // L2 medium
                    2 => 0.02,  // DDR slowest (outer ring)
                    _ => 0.0,
                };

                // Speed increases with activity
                planet.angle += orbit_speed * (1.0 + planet.activity * 0.5);

                // Calculate new orbital position
                let device_spacing = self.width / backend.devices().len().max(1);
                let device_center_x = (planet.device_idx * device_spacing) + (device_spacing / 2);
                let device_center_y = self.height / 2;

                let px = device_center_x as f32 + planet.angle.cos() * planet.radius;
                let py = device_center_y as f32 + planet.angle.sin() * planet.radius;

                if px >= 0.0 && (px as usize) < self.width && py >= 0.0 && (py as usize) < self.height {
                    planet.x = px as usize;
                    planet.y = py as usize;
                }

                // Pulsing animation for size variation (visual effect)
                planet.pulse += 0.08 * (1.0 + planet.activity);
            }
        }

        // Update data flow streams using sync_score (geometric mean of both
        // chips' activity).  This remains non-zero even when both chips are
        // equally loaded — solving the "equal-load streams go dark" problem
        // that the old abs(power_diff)/50 formula had.
        for stream in &mut self.streams {
            let act_a = *self.device_activity.get(&stream.from_device).unwrap_or(&0.0);
            let act_b = *self.device_activity.get(&stream.to_device).unwrap_or(&0.0);

            stream.intensity = sync_score(act_a, act_b, stream.intra_board);

            // Animate flow offset.
            stream.offset = (stream.offset + 0.1) % 1.0;
        }

        // Increment animation frame
        self.frame = self.frame.wrapping_add(1);
    }

    /// Render the starfield to Ratatui Lines
    ///
    /// Returns a vector of Ratatui Line structures with proper styling.
    pub fn render(&self) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::text::{Line, Span};
        use ratatui::style::Style;

        // Create blank canvas with explicit black background
        let mut canvas: Vec<Vec<(char, Color)>> = vec![vec![(' ', colors::rgb(0, 0, 0)); self.width]; self.height];

        // Render stars
        for star in &self.stars {
            if star.x < self.width && star.y < self.height {
                canvas[star.y][star.x] = (star.get_char(), star.color);
            }
        }

        // Render memory planets
        for planet in &self.planets {
            if planet.x < self.width && planet.y < self.height {
                canvas[planet.y][planet.x] = (planet.get_char(), planet.color);
            }
        }

        // Render data streams using topology-aware thresholds and colors.
        //
        // Intra-board (─): always-on floor of 0.2 from sync_score, use board hue.
        // Inter-board (═): visible when intensity ≥ 0.05, use rainbow sweep.
        for stream in &self.streams {
            // Threshold: intra-board always shown (floor ≥ 0.2), inter-board only when active.
            let threshold = if stream.intra_board { 0.05 } else { 0.05 };
            if stream.intensity < threshold {
                continue;
            }
            let show = ((stream.offset * 10.0) as u32 + self.frame / 2) % 3 == 0;
            if show && stream.x < self.width && stream.y < self.height {
                use crate::animation::hsv_to_rgb;
                let stream_hue = if stream.intra_board {
                    // Use the board hue for intra-board links.
                    self.board_topology.as_ref()
                        .map(|t| t.board_hue(stream.from_device))
                        .unwrap_or(200.0)
                } else {
                    // Full rainbow sweep for inter-board links.
                    (self.frame as f32 * 3.0 + stream.offset * 360.0) % 360.0
                };
                let value = 0.5 + stream.intensity * 0.5;
                let stream_color = hsv_to_rgb(stream_hue, 0.9, value);
                canvas[stream.y][stream.x] = (stream.get_char(), stream_color);
            }
        }

        // Convert canvas to Ratatui Lines with Spans
        let mut lines = Vec::new();

        for row in canvas {
            let mut spans = Vec::new();
            let mut current_text = String::new();
            let mut current_color = colors::rgb(0, 0, 0);

            for (ch, color) in row {
                if color != current_color {
                    // Push accumulated text with previous color
                    if !current_text.is_empty() {
                        // Only set foreground color - let widget background show through
                        spans.push(Span::styled(
                            current_text.clone(),
                            Style::default().fg(current_color)
                        ));
                        current_text.clear();
                    }
                    current_color = color;
                }
                current_text.push(ch);
            }

            // Push final span
            if !current_text.is_empty() {
                // Only set foreground color - let widget background show through
                spans.push(Span::styled(
                    current_text,
                    Style::default().fg(current_color)
                ));
            }

            lines.push(Line::from(spans));
        }

        lines
    }

    /// Render the starfield to a TerminalGrid (for GUI)
    ///
    /// This method produces the same visual output as render() but writes to
    /// a TerminalGrid instead of Ratatui Lines, allowing the GUI to display
    /// the same terminal-style visualization.
    #[cfg(feature = "gui")]
    pub fn render_to_grid(&self, grid: &mut crate::ui::gui::TerminalGrid) {
        use iced::Color as IcedColor;

        // Helper function to convert Ratatui Color to Iced Color
        fn ratatui_to_iced(color: Color) -> IcedColor {
            match color {
                Color::Rgb(r, g, b) | Color::Indexed(_) => {
                    // For Indexed colors, we'd need a lookup table, but for simplicity
                    // just use a default color. The pattern match here is mainly for Rgb.
                    if let Color::Rgb(r, g, b) = color {
                        IcedColor::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
                    } else {
                        IcedColor::from_rgb(0.8, 0.8, 0.8)
                    }
                }
                Color::Reset => IcedColor::from_rgb(0.8, 0.8, 0.8),
                _ => IcedColor::from_rgb(0.8, 0.8, 0.8),
            }
        }

        // Clear the grid
        grid.clear();

        // Create internal canvas (same as render())
        let mut canvas: Vec<Vec<(char, Color)>> = vec![vec![(' ', colors::TEXT_SECONDARY); self.width]; self.height];

        // Render stars
        for star in &self.stars {
            if star.x < self.width && star.y < self.height {
                canvas[star.y][star.x] = (star.get_char(), star.color);
            }
        }

        // Render memory planets
        for planet in &self.planets {
            if planet.x < self.width && planet.y < self.height {
                canvas[planet.y][planet.x] = (planet.get_char(), planet.color);
            }
        }

        // Render data streams (topology-aware, same logic as render()).
        for stream in &self.streams {
            if stream.intensity < 0.05 {
                continue;
            }
            let show = ((stream.offset * 10.0) as u32 + self.frame / 2) % 3 == 0;
            if show && stream.x < self.width && stream.y < self.height {
                use crate::animation::hsv_to_rgb;
                let stream_hue = if stream.intra_board {
                    self.board_topology.as_ref()
                        .map(|t| t.board_hue(stream.from_device))
                        .unwrap_or(200.0)
                } else {
                    (self.frame as f32 * 3.0 + stream.offset * 360.0) % 360.0
                };
                canvas[stream.y][stream.x] = (stream.get_char(), hsv_to_rgb(stream_hue, 0.9, 0.8));
            }
        }

        // Write canvas to terminal grid
        for (row_idx, row) in canvas.iter().enumerate() {
            if row_idx >= grid.height() {
                break;
            }
            for (col_idx, &(ch, color)) in row.iter().enumerate() {
                if col_idx >= grid.width() {
                    break;
                }
                let iced_color = ratatui_to_iced(color);
                grid.set_char(row_idx, col_idx, ch, iced_color);
            }
        }
    }

    /// Get baseline learning status message
    pub fn baseline_status(&self) -> String {
        if self.baseline.is_established() {
            "BASELINE ESTABLISHED".to_string()
        } else {
            let progress = (self.baseline.progress() * 20.0) as usize;
            format!("LEARNING BASELINE ({}/20)", progress)
        }
    }

    /// Check if baseline is established
    pub fn is_baseline_established(&self) -> bool {
        self.baseline.is_established()
    }

    /// Resize starfield display
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        // Stars/planets will be reinitialized on next device update
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_starfield_creation() {
        let starfield = HardwareStarfield::new(80, 24);
        assert_eq!(starfield.stars.len(), 0); // No stars until initialized
        assert_eq!(starfield.width, 80);
        assert_eq!(starfield.height, 24);
    }

    #[test]
    fn test_star_brightness_char() {
        let mut star = Star {
            x: 0,
            y: 0,
            device_idx: 0,
            core_idx: 0,
            brightness: 0.0,
            color: colors::PRIMARY,
            phase: 0.0,
            depth: 1.0,
            phase2: 0.0,
            sparkle: 0.0,
        };

        assert_eq!(star.get_char(), '·'); // Dimmest

        star.brightness = 0.5;
        assert_eq!(star.get_char(), '○'); // Medium

        star.brightness = 1.0;
        assert_eq!(star.get_char(), '●'); // Brightest
    }

    #[test]
    fn test_memory_planet_chars() {
        let l1_planet = MemoryPlanet {
            x: 0,
            y: 0,
            device_idx: 0,
            level: 0,
            channel_idx: 0,
            activity: 0.5,
            color: colors::INFO,
            angle: 0.0,
            radius: 10.0,
            pulse: 0.0,
        };
        assert_eq!(l1_planet.get_char(), '◆'); // L1 diamond

        let l2_planet = MemoryPlanet {
            x: 0,
            y: 0,
            device_idx: 0,
            level: 1,
            channel_idx: 0,
            activity: 0.5,
            color: colors::WARNING,
            angle: 0.0,
            radius: 10.0,
            pulse: 0.0,
        };
        assert_eq!(l2_planet.get_char(), '◇'); // L2 outline diamond
    }

    #[test]
    fn test_data_stream_chars() {
        let intra = DataStream { x: 0, y: 0, from_device: 0, to_device: 1,
            intensity: 0.5, offset: 0.0, intra_board: true };
        assert_eq!(intra.get_char(), '─');

        let inter = DataStream { x: 0, y: 0, from_device: 0, to_device: 2,
            intensity: 0.5, offset: 0.0, intra_board: false };
        assert_eq!(inter.get_char(), '═');
    }
}
