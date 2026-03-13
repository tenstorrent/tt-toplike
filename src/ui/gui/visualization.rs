//! GPU-accelerated visualizations for the GUI
//!
//! This module provides Canvas-based hardware-responsive visualizations
//! similar to the TUI starfield and TRON Grid modes.

use iced::{
    mouse, Element,
    widget::canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke, Text},
    Color, Point, Rectangle, Size, Theme,
};

use crate::models::{Architecture, Device};
use crate::ui::colors;
use crate::ui::gui::history::TelemetryHistory;

/// Hardware-responsive starfield visualization
pub struct StarfieldVisualization {
    /// Device being visualized
    device: Device,

    /// Animation frame counter
    frame: u64,

    /// Star positions (based on Tensix topology)
    stars: Vec<Star>,

    /// Cache for rendering optimization
    cache: Cache,
}

/// A single star representing a Tensix core
#[derive(Debug, Clone)]
struct Star {
    /// X position (0.0 to 1.0, normalized)
    x: f32,

    /// Y position (0.0 to 1.0, normalized)
    y: f32,

    /// Brightness (0.0 to 1.0)
    brightness: f32,

    /// Animation phase
    phase: f32,

    /// Base hue (for color cycling)
    hue: f32,
}

impl StarfieldVisualization {
    /// Create a new starfield for a device
    pub fn new(device: Device) -> Self {
        let stars = Self::create_stars_for_device(&device);

        Self {
            device,
            frame: 0,
            stars,
            cache: Cache::new(),
        }
    }

    /// Create stars based on device architecture's Tensix grid
    fn create_stars_for_device(device: &Device) -> Vec<Star> {
        let (rows, cols) = device.architecture.tensix_grid();
        let mut stars = Vec::with_capacity(rows * cols);

        for row in 0..rows {
            for col in 0..cols {
                // Normalize positions to 0.0-1.0 range
                let x = (col as f32 + 0.5) / cols as f32;
                let y = (row as f32 + 0.5) / rows as f32;

                // Initial phase and hue based on position
                let phase = (row * cols + col) as f32 * 0.1;
                let hue = ((col * 30 + row * 20) % 360) as f32;

                stars.push(Star {
                    x,
                    y,
                    brightness: 0.5,
                    phase,
                    hue,
                });
            }
        }

        stars
    }

    /// Update animation and hardware state
    pub fn update(&mut self, history: Option<&TelemetryHistory>) {
        self.frame += 1;

        // Update star brightness and colors based on hardware telemetry
        if let Some(hist) = history {
            let power = hist.latest_power();
            let temp = hist.latest_temp();
            let current = hist.latest_current();

            // Calculate activity level (normalized)
            let power_activity = (power / 200.0).min(1.0).max(0.0);
            let temp_hue_shift = colors::temp_to_hue(temp);

            // Update each star
            for (i, star) in self.stars.iter_mut().enumerate() {
                // Twinkle animation
                star.phase += 0.05 + current / 100.0;
                let twinkle = (star.phase.sin() * 0.5 + 0.5) * 0.3;

                // Brightness driven by power
                star.brightness = 0.3 + power_activity * 0.5 + twinkle;

                // Color cycling with temperature influence
                star.hue = ((star.hue + 2.0) % 360.0) + temp_hue_shift * 0.3;
            }
        } else {
            // No telemetry - just animate gently
            for star in &mut self.stars {
                star.phase += 0.02;
                let twinkle = (star.phase.sin() * 0.5 + 0.5) * 0.3;
                star.brightness = 0.4 + twinkle;
                star.hue = (star.hue + 1.0) % 360.0;
            }
        }

        // Invalidate cache to trigger redraw
        self.cache.clear();
    }

    /// Draw the starfield
    pub fn view(&self) -> Element<'static, ()> {
        Canvas::new(self.clone())
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .into()
    }
}

impl Clone for StarfieldVisualization {
    fn clone(&self) -> Self {
        Self {
            device: self.device.clone(),
            frame: self.frame,
            stars: self.stars.clone(),
            cache: Cache::new(),
        }
    }
}

impl canvas::Program<(), Theme> for StarfieldVisualization {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            // Dark background
            frame.fill_rectangle(
                Point::ORIGIN,
                bounds.size(),
                Color::from_rgb(0.02, 0.02, 0.05),
            );

            // Draw device info text
            let title_text = Text {
                content: format!("{} - {:?}", self.device.board_type, self.device.architecture),
                position: Point::new(10.0, 10.0),
                color: Color::from_rgb(0.7, 0.7, 0.9),
                size: 16.0.into(),
                ..Text::default()
            };
            frame.fill_text(title_text);

            // Draw architecture info
            let (rows, cols) = self.device.architecture.tensix_grid();
            let arch_text = Text {
                content: format!("{}×{} Tensix Grid - {} Cores", cols, rows, rows * cols),
                position: Point::new(10.0, 30.0),
                color: Color::from_rgb(0.6, 0.6, 0.8),
                size: 12.0.into(),
                ..Text::default()
            };
            frame.fill_text(arch_text);

            // Draw stars
            for star in &self.stars {
                // Convert normalized coordinates to screen space
                let x = star.x * bounds.width;
                let y = star.y * bounds.height;

                // Convert HSV to RGB
                let color = hsv_to_rgb(star.hue, 0.8, star.brightness);

                // Draw star as a circle with glow
                let radius = 3.0 + star.brightness * 2.0;

                // Outer glow (larger, transparent)
                let glow_path = Path::circle(Point::new(x, y), radius * 2.0);
                frame.fill(
                    &glow_path,
                    Color::from_rgba(color.r, color.g, color.b, star.brightness * 0.3),
                );

                // Inner bright core
                let core_path = Path::circle(Point::new(x, y), radius);
                frame.fill(&core_path, color);
            }

            // Draw connecting lines between nearby stars (topology visualization)
            for (i, star1) in self.stars.iter().enumerate() {
                for star2 in self.stars.iter().skip(i + 1) {
                    let dx = star1.x - star2.x;
                    let dy = star1.y - star2.y;
                    let distance = (dx * dx + dy * dy).sqrt();

                    // Only connect nearby stars
                    if distance < 0.15 {
                        let x1 = star1.x * bounds.width;
                        let y1 = star1.y * bounds.height;
                        let x2 = star2.x * bounds.width;
                        let y2 = star2.y * bounds.height;

                        let line_path = Path::line(Point::new(x1, y1), Point::new(x2, y2));

                        let alpha = (1.0 - distance / 0.15) * 0.2;
                        let avg_brightness = (star1.brightness + star2.brightness) / 2.0;

                        frame.stroke(
                            &line_path,
                            Stroke::default()
                                .with_width(1.0)
                                .with_color(Color::from_rgba(
                                    0.4,
                                    0.6,
                                    1.0,
                                    alpha * avg_brightness,
                                )),
                        );
                    }
                }
            }
        });

        vec![geometry]
    }
}

/// Convert HSV color to RGB (simplified)
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color {
    let c = v * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - ((h_prime % 2.0) - 1.0).abs());
    let m = v - c;

    let (r, g, b) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        5 => (c, 0.0, x),
        _ => (c, x, 0.0),
    };

    Color::from_rgb(r + m, g + m, b + m)
}

/// Dashboard visualization - unified chip art with DDR, memory hierarchy, and metrics
pub struct DashboardVisualization {
    /// Device being visualized
    device: Device,

    /// Animation frame counter
    frame: u64,

    /// Cache for rendering
    cache: Cache,
}

impl DashboardVisualization {
    /// Create a new dashboard for a device
    pub fn new(device: Device) -> Self {
        Self {
            device,
            frame: 0,
            cache: Cache::new(),
        }
    }

    /// Update animation
    pub fn update(&mut self, _history: Option<&TelemetryHistory>) {
        self.frame += 1;
        self.cache.clear();
    }

    /// Draw the dashboard
    pub fn view(&self) -> Element<'static, ()> {
        Canvas::new(self.clone())
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .into()
    }
}

impl Clone for DashboardVisualization {
    fn clone(&self) -> Self {
        Self {
            device: self.device.clone(),
            frame: self.frame,
            cache: Cache::new(),
        }
    }
}

impl canvas::Program<(), Theme> for DashboardVisualization {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            // Dark background
            frame.fill_rectangle(
                Point::ORIGIN,
                bounds.size(),
                Color::from_rgb(0.02, 0.02, 0.05),
            );

            let width = bounds.width;
            let height = bounds.height;

            // Calculate layout sections using percentage-based heights for better scaling
            let header_height = height * 0.10;      // 10% for header
            let ddr_section_height = height * 0.25;  // 25% for DDR channels
            let memory_section_height = height * 0.35; // 35% for memory hierarchy
            let metrics_section_height = height * 0.25; // 25% for metrics
            // 5% left for spacing

            let ddr_y = header_height;
            let memory_y = ddr_y + ddr_section_height;
            let metrics_y = memory_y + memory_section_height;

            // === HEADER: Device Info ===
            self.draw_header(frame, width, header_height);

            // === DDR CHANNELS ===
            self.draw_ddr_channels(frame, width, ddr_y, ddr_section_height);

            // === MEMORY HIERARCHY ===
            self.draw_memory_hierarchy(frame, width, memory_y, memory_section_height);

            // === METRICS GAUGES ===
            self.draw_metrics_gauges(frame, width, metrics_y, metrics_section_height);

            // === ANIMATED BORDER ===
            self.draw_animated_border(frame, bounds);
        });

        vec![geometry]
    }
}

impl DashboardVisualization {
    /// Draw header with device info
    fn draw_header(&self, frame: &mut Frame, width: f32, height: f32) {
        // Title background
        frame.fill_rectangle(
            Point::new(0.0, 0.0),
            Size::new(width, height),
            Color::from_rgba(0.2, 0.3, 0.5, 0.2),
        );

        // Device name
        let title = Text {
            content: format!("⚡ {} - {:?}", self.device.board_type, self.device.architecture),
            position: Point::new(20.0, 15.0),
            color: Color::from_rgb(0.7, 0.85, 1.0),
            size: 24.0.into(),
            ..Text::default()
        };
        frame.fill_text(title);

        // Architecture details
        let (rows, cols) = self.device.architecture.tensix_grid();
        let ddr_channels = match self.device.architecture {
            Architecture::Grayskull => 4,
            Architecture::Wormhole => 8,
            Architecture::Blackhole => 12,
            Architecture::Unknown => 0,
        };

        let details = Text {
            content: format!("{}×{} Tensix Grid │ {} DDR Channels │ {} Cores",
                cols, rows, ddr_channels, rows * cols),
            position: Point::new(20.0, 42.0),
            color: Color::from_rgb(0.5, 0.7, 0.9),
            size: 14.0.into(),
            ..Text::default()
        };
        frame.fill_text(details);
    }

    /// Draw DDR channels with training status and utilization
    fn draw_ddr_channels(&self, frame: &mut Frame, width: f32, y_offset: f32, section_height: f32) {
        // Section title
        let title = Text {
            content: "═══ DDR MEMORY CHANNELS ═══".to_string(),
            position: Point::new(20.0, y_offset + 10.0),
            color: Color::from_rgb(0.8, 0.9, 1.0),
            size: 16.0.into(),
            ..Text::default()
        };
        frame.fill_text(title);

        let num_channels = match self.device.architecture {
            Architecture::Grayskull => 4,
            Architecture::Wormhole => 8,
            Architecture::Blackhole => 12,
            Architecture::Unknown => 0,
        };

        if num_channels == 0 {
            return;
        }

        let channel_width = (width - 60.0) / num_channels as f32;
        let bar_height = 40.0;
        let y_pos = y_offset + 40.0;

        for i in 0..num_channels {
            let x = 30.0 + i as f32 * channel_width;

            // Animated training status (for demo - cycles through states)
            let anim_state = ((self.frame / 20) + i as u64) % 3;
            let (status_char, status_color, status_text) = match anim_state {
                0 => ('○', Color::from_rgb(0.4, 0.4, 0.5), "Idle"),
                1 => ('◐', Color::from_rgb(0.3, 0.8, 0.9), "Training"),
                _ => ('●', Color::from_rgb(0.3, 0.9, 0.5), "Trained"),
            };

            // Channel label
            let label = Text {
                content: format!("CH{}", i),
                position: Point::new(x + 5.0, y_pos - 18.0),
                color: Color::from_rgb(0.6, 0.7, 0.8),
                size: 12.0.into(),
                ..Text::default()
            };
            frame.fill_text(label);

            // Status indicator
            let status_indicator = Text {
                content: status_char.to_string(),
                position: Point::new(x + channel_width / 2.0 - 10.0, y_pos),
                color: status_color,
                size: 20.0.into(),
                ..Text::default()
            };
            frame.fill_text(status_indicator);

            // Utilization bar (animated)
            let utilization = ((self.frame as f32 * 0.05 + i as f32 * 0.5).sin() * 0.5 + 0.5) * 0.8;
            let bar_fill_height = bar_height * utilization;

            // Bar background
            frame.fill_rectangle(
                Point::new(x, y_pos + 30.0),
                Size::new(channel_width - 10.0, bar_height),
                Color::from_rgba(0.2, 0.2, 0.3, 0.5),
            );

            // Bar fill (gradient based on utilization)
            let bar_color = if utilization > 0.7 {
                Color::from_rgb(1.0, 0.4, 0.4) // Red (high)
            } else if utilization > 0.4 {
                Color::from_rgb(1.0, 0.7, 0.3) // Orange (medium)
            } else {
                Color::from_rgb(0.3, 0.9, 0.6) // Green (low)
            };

            frame.fill_rectangle(
                Point::new(x, y_pos + 30.0 + (bar_height - bar_fill_height)),
                Size::new(channel_width - 10.0, bar_fill_height),
                bar_color,
            );

            // Utilization percentage
            let util_text = Text {
                content: format!("{:.0}%", utilization * 100.0),
                position: Point::new(x + 5.0, y_pos + 32.0 + bar_height + 5.0),
                color: Color::from_rgb(0.7, 0.8, 0.9),
                size: 10.0.into(),
                ..Text::default()
            };
            frame.fill_text(util_text);

            // Status text
            let status_label = Text {
                content: status_text.to_string(),
                position: Point::new(x + 2.0, y_pos + 32.0 + bar_height + 18.0),
                color: status_color,
                size: 9.0.into(),
                ..Text::default()
            };
            frame.fill_text(status_label);
        }
    }

    /// Draw memory hierarchy visualization (L1/L2/DDR)
    fn draw_memory_hierarchy(&self, frame: &mut Frame, width: f32, y_offset: f32, section_height: f32) {
        // Section title
        let title = Text {
            content: "═══ MEMORY HIERARCHY ═══".to_string(),
            position: Point::new(20.0, y_offset + 10.0),
            color: Color::from_rgb(0.8, 0.9, 1.0),
            size: 16.0.into(),
            ..Text::default()
        };
        frame.fill_text(title);

        let layer_height = 50.0;
        let layer_spacing = 10.0;
        let start_y = y_offset + 40.0;

        // === L1 SRAM (per-core cache) ===
        self.draw_memory_layer(
            frame,
            "L1 SRAM (Per-Core)",
            Point::new(40.0, start_y),
            width - 80.0,
            layer_height,
            Color::from_rgb(0.3, 0.8, 1.0), // Cyan
            0.6 + ((self.frame as f32 * 0.1).sin() * 0.2),
            "Fast",
        );

        // === L2 Cache (8 banks) ===
        self.draw_memory_layer(
            frame,
            "L2 Cache (8 Banks)",
            Point::new(40.0, start_y + layer_height + layer_spacing),
            width - 80.0,
            layer_height,
            Color::from_rgb(1.0, 0.8, 0.3), // Yellow
            0.5 + ((self.frame as f32 * 0.08 + 1.0).sin() * 0.2),
            "Shared",
        );

        // === DDR (off-chip) ===
        self.draw_memory_layer(
            frame,
            "DDR (Off-Chip)",
            Point::new(40.0, start_y + (layer_height + layer_spacing) * 2.0),
            width - 80.0,
            layer_height,
            Color::from_rgb(0.9, 0.4, 0.9), // Purple
            0.4 + ((self.frame as f32 * 0.06 + 2.0).sin() * 0.15),
            "Large",
        );
    }

    /// Draw a single memory layer
    fn draw_memory_layer(
        &self,
        frame: &mut Frame,
        name: &str,
        position: Point,
        width: f32,
        height: f32,
        color: Color,
        activity: f32,
        speed_label: &str,
    ) {
        // Background
        frame.fill_rectangle(
            position,
            Size::new(width, height),
            Color::from_rgba(0.1, 0.1, 0.2, 0.5),
        );

        // Activity bar
        let activity_width = width * activity.max(0.0).min(1.0);
        frame.fill_rectangle(
            position,
            Size::new(activity_width, height),
            Color::from_rgba(color.r, color.g, color.b, 0.3),
        );

        // Name label
        let name_text = Text {
            content: name.to_string(),
            position: Point::new(position.x + 10.0, position.y + 10.0),
            color: Color::from_rgb(0.9, 0.9, 1.0),
            size: 14.0.into(),
            ..Text::default()
        };
        frame.fill_text(name_text);

        // Activity percentage
        let activity_text = Text {
            content: format!("{:.0}%", activity * 100.0),
            position: Point::new(position.x + 10.0, position.y + 28.0),
            color,
            size: 12.0.into(),
            ..Text::default()
        };
        frame.fill_text(activity_text);

        // Speed label
        let speed_text = Text {
            content: speed_label.to_string(),
            position: Point::new(position.x + width - 50.0, position.y + 18.0),
            color: Color::from_rgb(0.6, 0.7, 0.8),
            size: 11.0.into(),
            ..Text::default()
        };
        frame.fill_text(speed_text);

        // Border
        let border_path = Path::rectangle(position, Size::new(width, height));
        frame.stroke(
            &border_path,
            Stroke::default()
                .with_width(2.0)
                .with_color(color),
        );
    }

    /// Draw real-time metrics with gauges
    fn draw_metrics_gauges(&self, frame: &mut Frame, width: f32, y_offset: f32, _available_height: f32) {
        // Section title
        let title = Text {
            content: "═══ REAL-TIME METRICS ═══".to_string(),
            position: Point::new(20.0, y_offset),
            color: Color::from_rgb(0.8, 0.9, 1.0),
            size: 16.0.into(),
            ..Text::default()
        };
        frame.fill_text(title);

        let gauge_y = y_offset + 30.0;
        let gauge_width = (width - 80.0) / 3.0;

        // Power gauge (animated for demo)
        let power = 50.0 + ((self.frame as f32 * 0.05).sin() * 30.0);
        self.draw_gauge(
            frame,
            "⚡ Power",
            Point::new(40.0, gauge_y),
            gauge_width - 20.0,
            power,
            200.0,
            "W",
            Color::from_rgb(0.3, 0.9, 0.6),
        );

        // Temperature gauge (animated for demo)
        let temp = 55.0 + ((self.frame as f32 * 0.04 + 1.0).sin() * 15.0);
        self.draw_gauge(
            frame,
            "🌡 Temp",
            Point::new(40.0 + gauge_width, gauge_y),
            gauge_width - 20.0,
            temp,
            100.0,
            "°C",
            Color::from_rgb(1.0, 0.6, 0.3),
        );

        // Current gauge (animated for demo)
        let current = 30.0 + ((self.frame as f32 * 0.06 + 2.0).sin() * 15.0);
        self.draw_gauge(
            frame,
            "⚙ Current",
            Point::new(40.0 + gauge_width * 2.0, gauge_y),
            gauge_width - 20.0,
            current,
            100.0,
            "A",
            Color::from_rgb(0.5, 0.7, 1.0),
        );
    }

    /// Draw a single gauge
    fn draw_gauge(
        &self,
        frame: &mut Frame,
        label: &str,
        position: Point,
        width: f32,
        value: f32,
        max_value: f32,
        unit: &str,
        color: Color,
    ) {
        let height = 80.0;

        // Background
        frame.fill_rectangle(
            position,
            Size::new(width, height),
            Color::from_rgba(0.1, 0.1, 0.2, 0.5),
        );

        // Label
        let label_text = Text {
            content: label.to_string(),
            position: Point::new(position.x + 10.0, position.y + 10.0),
            color: Color::from_rgb(0.8, 0.8, 0.9),
            size: 13.0.into(),
            ..Text::default()
        };
        frame.fill_text(label_text);

        // Value text
        let value_text = Text {
            content: format!("{:.1} {}", value, unit),
            position: Point::new(position.x + 10.0, position.y + 30.0),
            color,
            size: 18.0.into(),
            ..Text::default()
        };
        frame.fill_text(value_text);

        // Progress bar
        let bar_y = position.y + 55.0;
        let bar_height = 15.0;
        let fill_width = (width - 20.0) * (value / max_value).min(1.0);

        // Bar background
        frame.fill_rectangle(
            Point::new(position.x + 10.0, bar_y),
            Size::new(width - 20.0, bar_height),
            Color::from_rgba(0.2, 0.2, 0.3, 0.8),
        );

        // Bar fill
        frame.fill_rectangle(
            Point::new(position.x + 10.0, bar_y),
            Size::new(fill_width, bar_height),
            Color::from_rgba(color.r, color.g, color.b, 0.8),
        );

        // Border
        let border_path = Path::rectangle(position, Size::new(width, height));
        frame.stroke(
            &border_path,
            Stroke::default()
                .with_width(1.5)
                .with_color(Color::from_rgba(color.r, color.g, color.b, 0.5)),
        );
    }

    /// Draw animated border around entire canvas
    fn draw_animated_border(&self, frame: &mut Frame, bounds: Rectangle) {
        let phase = (self.frame as f32 * 0.02) % 1.0;

        // Gradient colors that cycle
        let hue = phase * 360.0;
        let color1 = hsv_to_rgb(hue, 0.6, 0.8);
        let color2 = hsv_to_rgb((hue + 180.0) % 360.0, 0.6, 0.8);

        // Top edge
        let top_path = Path::line(
            Point::new(0.0, 0.0),
            Point::new(bounds.width, 0.0),
        );
        frame.stroke(
            &top_path,
            Stroke::default()
                .with_width(3.0)
                .with_color(color1),
        );

        // Bottom edge
        let bottom_path = Path::line(
            Point::new(0.0, bounds.height),
            Point::new(bounds.width, bounds.height),
        );
        frame.stroke(
            &bottom_path,
            Stroke::default()
                .with_width(3.0)
                .with_color(color2),
        );
    }
}

/// Simple line chart for telemetry data
pub struct LineChart {
    /// Chart title
    title: String,

    /// Data points (Y values)
    data: Vec<f32>,

    /// Value range (min, max)
    range: (f32, f32),

    /// Line color
    color: Color,

    /// Cache for rendering
    cache: Cache,
}

impl LineChart {
    /// Create a new line chart
    pub fn new(title: String, data: Vec<f32>, range: (f32, f32), color: Color) -> Self {
        Self {
            title,
            data,
            range,
            color,
            cache: Cache::new(),
        }
    }

    /// Update chart data
    pub fn update(&mut self, data: Vec<f32>, range: (f32, f32)) {
        self.data = data;
        self.range = range;
        self.cache.clear();
    }

    /// Draw the chart
    pub fn view(&self) -> Element<'static, ()> {
        Canvas::new(self.clone())
            .width(iced::Length::Fill)
            .height(iced::Length::Fixed(150.0))
            .into()
    }
}

impl Clone for LineChart {
    fn clone(&self) -> Self {
        Self {
            title: self.title.clone(),
            data: self.data.clone(),
            range: self.range,
            color: self.color,
            cache: Cache::new(),
        }
    }
}

impl canvas::Program<(), Theme> for LineChart {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            // Background
            frame.fill_rectangle(
                Point::ORIGIN,
                bounds.size(),
                Color::from_rgb(0.05, 0.05, 0.08),
            );

            // Title
            let title_text = Text {
                content: self.title.clone(),
                position: Point::new(10.0, 5.0),
                color: Color::from_rgb(0.8, 0.8, 0.9),
                size: 14.0.into(),
                ..Text::default()
            };
            frame.fill_text(title_text);

            // Chart area (leave margins for labels)
            let margin_top = 30.0;
            let margin_bottom = 20.0;
            let margin_left = 50.0;
            let margin_right = 10.0;

            let chart_width = bounds.width - margin_left - margin_right;
            let chart_height = bounds.height - margin_top - margin_bottom;

            if self.data.is_empty() || chart_width <= 0.0 || chart_height <= 0.0 {
                return;
            }

            // Grid lines
            for i in 0..5 {
                let y = margin_top + (i as f32 / 4.0) * chart_height;
                let grid_path = Path::line(
                    Point::new(margin_left, y),
                    Point::new(margin_left + chart_width, y),
                );
                frame.stroke(
                    &grid_path,
                    Stroke::default()
                        .with_width(1.0)
                        .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.1)),
                );

                // Y-axis labels
                let value = self.range.1 - (i as f32 / 4.0) * (self.range.1 - self.range.0);
                let label = Text {
                    content: format!("{:.1}", value),
                    position: Point::new(5.0, y - 6.0),
                    color: Color::from_rgb(0.6, 0.6, 0.7),
                    size: 10.0.into(),
                    ..Text::default()
                };
                frame.fill_text(label);
            }

            // Draw line chart
            if self.data.len() > 1 {
                let mut path_builder = canvas::path::Builder::new();

                for (i, &value) in self.data.iter().enumerate() {
                    let x = margin_left + (i as f32 / (self.data.len() - 1) as f32) * chart_width;

                    // Normalize value to 0.0-1.0 range
                    let normalized = if self.range.1 > self.range.0 {
                        ((value - self.range.0) / (self.range.1 - self.range.0))
                            .max(0.0)
                            .min(1.0)
                    } else {
                        0.5
                    };

                    let y = margin_top + chart_height - (normalized * chart_height);

                    if i == 0 {
                        path_builder.move_to(Point::new(x, y));
                    } else {
                        path_builder.line_to(Point::new(x, y));
                    }
                }

                let path = path_builder.build();
                frame.stroke(
                    &path,
                    Stroke::default().with_width(2.0).with_color(self.color),
                );
            }
        });

        vec![geometry]
    }
}
