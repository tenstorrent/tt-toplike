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
use crate::ui::colors;
use ratatui::style::Color;
use std::f32;

/// Character progression for star brightness levels
const STAR_CHARS: [char; 5] = ['·', '∘', '○', '◉', '●'];

/// Character progression for memory planet intensity
const PLANET_CHARS: [char; 5] = ['·', '░', '▒', '▓', '█'];

/// Character progression for data flow intensity
const FLOW_CHARS: [char; 4] = ['▹', '▸', '▷', '▶'];

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
            0 => colors::INFO,        // L1 - blue
            1 => colors::WARNING,     // L2 - yellow/orange
            2 => colors::ERROR,       // DDR - red/magenta
            _ => colors::TEXT_SECONDARY,
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

    /// Flow intensity (0.0 to 1.0)
    pub intensity: f32,

    /// Animation offset
    pub offset: f32,
}

impl DataStream {
    /// Get the character to render based on intensity
    pub fn get_char(&self) -> char {
        let idx = (self.intensity * (FLOW_CHARS.len() - 1) as f32) as usize;
        FLOW_CHARS[idx.min(FLOW_CHARS.len() - 1)]
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
        }
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
            // Calculate center position for this device
            let device_center_x = (device_idx * device_spacing) + (device_spacing / 2);
            let device_center_y = self.height / 2;

            // Get Tensix grid dimensions for this architecture
            let (grid_rows, grid_cols) = device.architecture.tensix_grid();

            // Calculate star spacing to fit grid within device region
            let star_spacing_x = (device_spacing.saturating_sub(10)) / grid_cols.max(1);
            let star_spacing_y = (self.height.saturating_sub(10)) / grid_rows.max(1);

            // Create stars for each Tensix core
            for row in 0..grid_rows {
                for col in 0..grid_cols {
                    let x = device_center_x.saturating_sub(grid_cols * star_spacing_x / 2)
                        + col * star_spacing_x;
                    let y = device_center_y.saturating_sub(grid_rows * star_spacing_y / 2)
                        + row * star_spacing_y;

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

                    // Create streams along the path
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
        // Update baseline learning
        for device in backend.devices() {
            if let Some(telem) = backend.telemetry(device.index) {
                let power = telem.power_w();
                let current = telem.current_a();
                let temp = telem.temp_c();
                let aiclk = telem.aiclk_mhz() as f32;  // Convert u32 to f32

                self.baseline.update(device.index, power, current, temp, aiclk);
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

                // Color from temperature
                star.color = colors::temp_color(temp);
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

                planet.color = planet.get_color();

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

        // Update data flow streams based on power differentials
        for stream in &mut self.streams {
            let from_power = backend.telemetry(stream.from_device)
                .map(|t| t.power_w())
                .unwrap_or(0.0);
            let to_power = backend.telemetry(stream.to_device)
                .map(|t| t.power_w())
                .unwrap_or(0.0);

            // Flow intensity based on power differential
            let power_diff = (from_power - to_power).abs();
            stream.intensity = (power_diff / 50.0).min(1.0);

            // Animate flow
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

        // Create blank canvas
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

        // Render data streams
        for stream in &self.streams {
            if stream.intensity > 0.1 {
                let show = ((stream.offset * 10.0) as u32 + self.frame / 2) % 3 == 0;
                if show && stream.x < self.width && stream.y < self.height {
                    canvas[stream.y][stream.x] = (stream.get_char(), colors::PRIMARY);
                }
            }
        }

        // Convert canvas to Ratatui Lines with Spans
        let mut lines = Vec::new();

        for row in canvas {
            let mut spans = Vec::new();
            let mut current_text = String::new();
            let mut current_color = colors::TEXT_SECONDARY;

            for (ch, color) in row {
                if color != current_color {
                    // Push accumulated text with previous color
                    if !current_text.is_empty() {
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
                Color::Rgb(r, g, b) => IcedColor::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0),
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

        // Render data streams
        for stream in &self.streams {
            if stream.intensity > 0.1 {
                let show = ((stream.offset * 10.0) as u32 + self.frame / 2) % 3 == 0;
                if show && stream.x < self.width && stream.y < self.height {
                    canvas[stream.y][stream.x] = (stream.get_char(), colors::PRIMARY);
                }
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
        };
        assert_eq!(l2_planet.get_char(), '◇'); // L2 outline diamond
    }
}
