//! egui-based K-RAD dashboard 🎸
//!
//! This module provides a PSYCHEDELIC real-time monitoring dashboard using egui.
//!
//! Features:
//! - 🌈 Cyberpunk theme with neon colors
//! - ✨ Particle effects and glowing visualizations
//! - 📊 Real-time telemetry graphs with rainbow gradients
//! - 🎆 Power surge animations and temperature heat maps
//! - 🔧 Process monitoring with visual flair
//!
//! Philosophy: "K-RAD from top to bottom inside out and back up your wazoo again"

use crate::backend::TelemetryBackend;
use crate::cli::Cli;
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Maximum number of data points to keep in history (2 minutes at 1 sample/sec)
const MAX_HISTORY: usize = 120;

/// Maximum number of background particles
const MAX_PARTICLES: usize = 150;

/// Background particle for psychedelic effect
#[derive(Clone)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    hue: f32,        // 0.0-360.0
    brightness: f32, // 0.0-1.0
    size: f32,       // radius in pixels
}

impl Particle {
    fn new(width: f32, height: f32) -> Self {
        use std::f32::consts::PI;
        let angle = (rand::random::<f32>() * 2.0 * PI);
        Self {
            x: rand::random::<f32>() * width,
            y: rand::random::<f32>() * height,
            vx: angle.cos() * (0.5 + rand::random::<f32>() * 1.5),
            vy: angle.sin() * (0.5 + rand::random::<f32>() * 1.5),
            hue: rand::random::<f32>() * 360.0,
            brightness: 0.3 + rand::random::<f32>() * 0.7,
            size: 1.0 + rand::random::<f32>() * 3.0,
        }
    }

    fn update(&mut self, width: f32, height: f32, dt: f32) {
        // Move particle
        self.x += self.vx * dt;
        self.y += self.vy * dt;

        // Wrap around edges
        if self.x < 0.0 {
            self.x += width;
        }
        if self.x > width {
            self.x -= width;
        }
        if self.y < 0.0 {
            self.y += height;
        }
        if self.y > height {
            self.y -= height;
        }

        // Cycle hue
        self.hue = (self.hue + dt * 30.0) % 360.0;
    }

    fn to_color(&self) -> egui::Color32 {
        hsv_to_rgb(self.hue, 0.8, self.brightness)
    }
}

/// Convert HSV to RGB (for psychedelic colors)
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> egui::Color32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    egui::Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Cyberpunk color palette
struct CyberpunkTheme {
    neon_cyan: egui::Color32,
    neon_magenta: egui::Color32,
    neon_yellow: egui::Color32,
    neon_green: egui::Color32,
    dark_bg: egui::Color32,
    darker_bg: egui::Color32,
    glow: egui::Color32,
}

impl CyberpunkTheme {
    fn new() -> Self {
        Self {
            neon_cyan: egui::Color32::from_rgb(0, 255, 255),
            neon_magenta: egui::Color32::from_rgb(255, 0, 255),
            neon_yellow: egui::Color32::from_rgb(255, 255, 0),
            neon_green: egui::Color32::from_rgb(0, 255, 100),
            dark_bg: egui::Color32::from_rgb(10, 10, 26),
            darker_bg: egui::Color32::from_rgb(5, 5, 15),
            glow: egui::Color32::from_rgba_premultiplied(100, 200, 255, 80),
        }
    }
}

/// Telemetry history for a single device
#[derive(Clone)]
struct DeviceHistory {
    /// Device index
    device_idx: usize,
    /// Device name
    name: String,
    /// Timestamps (relative to start, in seconds)
    timestamps: VecDeque<f64>,
    /// Power consumption history (Watts)
    power: VecDeque<f64>,
    /// Temperature history (Celsius)
    temperature: VecDeque<f64>,
    /// Current draw history (Amperes)
    current: VecDeque<f64>,
    /// Voltage history (Volts)
    voltage: VecDeque<f64>,
}

impl DeviceHistory {
    fn new(device_idx: usize, name: String) -> Self {
        Self {
            device_idx,
            name,
            timestamps: VecDeque::with_capacity(MAX_HISTORY),
            power: VecDeque::with_capacity(MAX_HISTORY),
            temperature: VecDeque::with_capacity(MAX_HISTORY),
            current: VecDeque::with_capacity(MAX_HISTORY),
            voltage: VecDeque::with_capacity(MAX_HISTORY),
        }
    }

    fn add_sample(&mut self, timestamp: f64, power: f64, temp: f64, current: f64, voltage: f64) {
        // Add new sample
        self.timestamps.push_back(timestamp);
        self.power.push_back(power);
        self.temperature.push_back(temp);
        self.current.push_back(current);
        self.voltage.push_back(voltage);

        // Remove old samples if we exceed max history
        if self.timestamps.len() > MAX_HISTORY {
            self.timestamps.pop_front();
            self.power.pop_front();
            self.temperature.pop_front();
            self.current.pop_front();
            self.voltage.pop_front();
        }
    }

    fn power_points(&self) -> PlotPoints {
        self.timestamps
            .iter()
            .zip(self.power.iter())
            .map(|(t, p)| [*t, *p])
            .collect()
    }

    fn temp_points(&self) -> PlotPoints {
        self.timestamps
            .iter()
            .zip(self.temperature.iter())
            .map(|(t, temp)| [*t, *temp])
            .collect()
    }

    fn current_points(&self) -> PlotPoints {
        self.timestamps
            .iter()
            .zip(self.current.iter())
            .map(|(t, i)| [*t, *i])
            .collect()
    }

    fn voltage_points(&self) -> PlotPoints {
        self.timestamps
            .iter()
            .zip(self.voltage.iter())
            .map(|(t, v)| [*t, *v])
            .collect()
    }
}

/// Main egui dashboard application
pub struct DashboardApp {
    /// Backend for telemetry data
    backend: Box<dyn TelemetryBackend>,
    /// CLI configuration
    _cli: Cli,
    /// Device history
    device_history: Vec<DeviceHistory>,
    /// Start time for relative timestamps
    start_time: Instant,
    /// Last update time
    last_update: Instant,
    /// Update interval
    update_interval: Duration,
    /// Process monitor (Linux only)
    #[cfg(feature = "linux-procfs")]
    process_monitor: crate::workload::ProcessMonitor,
    /// Last process scan time
    #[cfg(feature = "linux-procfs")]
    last_process_scan: Instant,
    /// Background particles for psychedelic effect
    particles: Vec<Particle>,
    /// Cyberpunk theme
    theme: CyberpunkTheme,
    /// Frame counter for animations
    frame: u64,
    /// Last frame time for delta calculations
    last_frame_time: Instant,
}

impl DashboardApp {
    pub fn new(backend: Box<dyn TelemetryBackend>, cli: Cli) -> Self {
        let update_interval = Duration::from_millis(cli.interval);

        // Initialize device history
        let device_history = backend
            .devices()
            .iter()
            .map(|d| DeviceHistory::new(d.index, d.name()))
            .collect();

        let start_time = Instant::now();

        // Initialize particles (will be repositioned on first frame)
        let particles = (0..MAX_PARTICLES)
            .map(|_| Particle::new(1280.0, 800.0))
            .collect();

        Self {
            backend,
            _cli: cli,
            device_history,
            start_time,
            last_update: start_time,
            update_interval,
            #[cfg(feature = "linux-procfs")]
            process_monitor: crate::workload::ProcessMonitor::new(),
            #[cfg(feature = "linux-procfs")]
            last_process_scan: start_time,
            particles,
            theme: CyberpunkTheme::new(),
            frame: 0,
            last_frame_time: start_time,
        }
    }

    fn update_telemetry(&mut self) {
        // Check if it's time to update
        if self.last_update.elapsed() < self.update_interval {
            return;
        }

        // Update backend
        if let Err(e) = self.backend.update() {
            log::warn!("Backend update failed: {}", e);
        }

        // Get current timestamp (relative to start)
        let timestamp = self.start_time.elapsed().as_secs_f64();

        // Update device history
        for history in &mut self.device_history {
            if let Some(telem) = self.backend.telemetry(history.device_idx) {
                let power = telem.power_w() as f64;
                let temp = telem.temp_c() as f64;
                let current = telem.current_a() as f64;
                let voltage = telem.voltage.unwrap_or(0.0) as f64;

                history.add_sample(timestamp, power, temp, current, voltage);
            }
        }

        self.last_update = Instant::now();

        // Update process monitor (every 2 seconds)
        #[cfg(feature = "linux-procfs")]
        if self.last_process_scan.elapsed() >= Duration::from_secs(2) {
            self.process_monitor.update();
            self.last_process_scan = Instant::now();
        }
    }
}

impl eframe::App for DashboardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ═══════════════════════════════════════════════════════════════
        // 🎸 K-RAD PSYCHEDELIC SETUP 🎸
        // ═══════════════════════════════════════════════════════════════

        // Apply cyberpunk theme
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = self.theme.darker_bg;
        visuals.panel_fill = self.theme.dark_bg;
        visuals.window_stroke = egui::Stroke::new(2.0, self.theme.neon_cyan);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, self.theme.neon_cyan);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, self.theme.neon_magenta);
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(2.0, self.theme.neon_yellow);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0, self.theme.neon_green);
        ctx.set_visuals(visuals);

        // Calculate delta time for animations
        let now = Instant::now();
        let dt = (now - self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        self.frame += 1;

        // Update particles
        let screen_rect = ctx.screen_rect();
        for particle in &mut self.particles {
            particle.update(screen_rect.width(), screen_rect.height(), dt * 60.0); // 60 FPS reference
        }

        // Update telemetry
        self.update_telemetry();

        // Request continuous repaint for real-time updates
        ctx.request_repaint();

        // ═══════════════════════════════════════════════════════════════
        // 🌈 PARTICLE BACKGROUND RENDERING 🌈
        // ═══════════════════════════════════════════════════════════════

        // Render particles behind all UI
        let painter = ctx.layer_painter(egui::LayerId::background());
        for particle in &self.particles {
            let pos = egui::pos2(particle.x, particle.y);
            painter.circle_filled(pos, particle.size, particle.to_color());

            // Add glow effect (outer circles with decreasing opacity)
            for i in 1..=3 {
                let glow_size = particle.size + (i as f32 * 2.0);
                let mut glow_color = particle.to_color();
                glow_color[3] = (particle.brightness * 50.0 / (i as f32)) as u8;
                painter.circle_filled(pos, glow_size, glow_color);
            }
        }

        // Draw grid pattern (TRON-style)
        let grid_spacing = 50.0;
        let grid_color = egui::Color32::from_rgba_premultiplied(0, 100, 150, 30);
        for x in (0..(screen_rect.width() as i32)).step_by(grid_spacing as usize) {
            painter.line_segment(
                [
                    egui::pos2(x as f32, 0.0),
                    egui::pos2(x as f32, screen_rect.height()),
                ],
                egui::Stroke::new(0.5, grid_color),
            );
        }
        for y in (0..(screen_rect.height() as i32)).step_by(grid_spacing as usize) {
            painter.line_segment(
                [
                    egui::pos2(0.0, y as f32),
                    egui::pos2(screen_rect.width(), y as f32),
                ],
                egui::Stroke::new(0.5, grid_color),
            );
        }

        // ═══════════════════════════════════════════════════════════════
        // 🎆 RAINBOW GRADIENT TITLE BAR 🎆
        // ═══════════════════════════════════════════════════════════════

        // Top panel: Title and status with rainbow gradient
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::none()
                .fill(self.theme.darker_bg)
                .stroke(egui::Stroke::new(3.0, self.theme.neon_cyan))
                .inner_margin(10.0))
            .show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Psychedelic title with cycling colors
                let title_hue = (self.frame as f32 * 2.0) % 360.0;
                ui.label(
                    egui::RichText::new("🦀 TT-TOPLIKE-RS 🎸")
                        .size(24.0)
                        .strong()
                        .color(hsv_to_rgb(title_hue, 1.0, 1.0))
                );

                ui.add_space(20.0);

                // Neon separator
                ui.label(
                    egui::RichText::new("│")
                        .size(20.0)
                        .color(self.theme.neon_magenta)
                );

                ui.add_space(10.0);

                // Backend info with glow
                ui.label(
                    egui::RichText::new(format!("Backend: {}", self.backend.backend_info()))
                        .color(self.theme.neon_cyan)
                        .strong()
                );

                ui.add_space(10.0);

                ui.label(
                    egui::RichText::new("│")
                        .size(20.0)
                        .color(self.theme.neon_magenta)
                );

                ui.add_space(10.0);

                // Device count
                ui.label(
                    egui::RichText::new(format!("⚡ {} devices", self.backend.devices().len()))
                        .color(self.theme.neon_yellow)
                        .strong()
                );
            });
        });

        // ═══════════════════════════════════════════════════════════════
        // 🔧 PROCESS MONITORING PANEL (K-RAD Edition) 🔧
        // ═══════════════════════════════════════════════════════════════

        // Bottom panel: Process monitoring (if available)
        #[cfg(feature = "linux-procfs")]
        if self.process_monitor.has_any_processes() {
            egui::TopBottomPanel::bottom("process_panel")
                .min_height(150.0)
                .frame(egui::Frame::none()
                    .fill(self.theme.darker_bg)
                    .stroke(egui::Stroke::new(2.0, self.theme.neon_yellow))
                    .inner_margin(10.0))
                .show(ctx, |ui| {
                    // Glowing title
                    ui.label(
                        egui::RichText::new("🔧 HARDWARE USAGE 🔧")
                            .size(18.0)
                            .strong()
                            .color(self.theme.neon_yellow)
                    );
                    ui.add_space(5.0);

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        // Show processes per device
                        for device in self.backend.devices() {
                            if let Some(processes) =
                                self.process_monitor.get_processes_for_device(device.index)
                            {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Device {}: {} process{}",
                                        device.index,
                                        processes.len(),
                                        if processes.len() == 1 { "" } else { "es" }
                                    ))
                                    .strong()
                                    .color(egui::Color32::from_rgb(120, 150, 255)),
                                );

                                for proc in processes.iter().take(5) {
                                    ui.horizontal(|ui| {
                                        ui.label("  •");
                                        ui.label(
                                            egui::RichText::new(&proc.name)
                                                .color(egui::Color32::from_rgb(80, 220, 200)),
                                        );
                                        ui.label(format!("[{}]", proc.pid));
                                        ui.label(&proc.cmdline);
                                    });

                                    if proc.hugepages_1g > 0 || proc.hugepages_2m > 0 {
                                        ui.horizontal(|ui| {
                                            ui.label("     ");
                                            let hp_text = if proc.hugepages_1g > 0
                                                && proc.hugepages_2m > 0
                                            {
                                                format!(
                                                    "(hugepages: {} × 1GB, {} × 2MB)",
                                                    proc.hugepages_1g, proc.hugepages_2m
                                                )
                                            } else if proc.hugepages_1g > 0 {
                                                format!("(hugepages: {} × 1GB)", proc.hugepages_1g)
                                            } else {
                                                format!("(hugepages: {} × 2MB)", proc.hugepages_2m)
                                            };
                                            ui.label(
                                                egui::RichText::new(hp_text)
                                                    .italics()
                                                    .color(egui::Color32::from_rgb(150, 120, 180)),
                                            );
                                        });
                                    }
                                }

                                if processes.len() > 5 {
                                    ui.label(format!("     ... and {} more", processes.len() - 5));
                                }

                                ui.add_space(5.0);
                            }
                        }

                        // Show shared processes
                        let shared = self.process_monitor.get_shared_processes();
                        if !shared.is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Shared: {} process{}",
                                    shared.len(),
                                    if shared.len() == 1 { "" } else { "es" }
                                ))
                                .strong()
                                .color(egui::Color32::from_rgb(150, 120, 180)),
                            );

                            for proc in shared.iter().take(3) {
                                ui.horizontal(|ui| {
                                    ui.label("  •");
                                    ui.label(
                                        egui::RichText::new(&proc.name)
                                            .color(egui::Color32::from_rgb(150, 120, 180)),
                                    );
                                    ui.label(format!("[{}]", proc.pid));
                                    ui.label(&proc.cmdline);
                                });
                            }

                            if shared.len() > 3 {
                                ui.label(format!("     ... and {} more", shared.len() - 3));
                            }
                        }
                    });
                });
        }

        // ═══════════════════════════════════════════════════════════════
        // 📊 PSYCHEDELIC TELEMETRY GRAPHS 📊
        // ═══════════════════════════════════════════════════════════════

        // Central panel: Graphs with rainbow styling
        egui::CentralPanel::default()
            .frame(egui::Frame::none()
                .fill(self.theme.dark_bg)
                .inner_margin(10.0))
            .show(ctx, |ui| {
            // Animated title
            let title_hue = ((self.frame as f32 * 1.5) + 120.0) % 360.0;
            ui.label(
                egui::RichText::new("📊 REAL-TIME TELEMETRY 📊")
                    .size(20.0)
                    .strong()
                    .color(hsv_to_rgb(title_hue, 0.9, 1.0))
            );
            ui.add_space(10.0);

            // Create a 2×2 grid of plots
            let available_height = ui.available_height();
            let plot_height = (available_height - 40.0) / 2.0; // 2 rows

            // Row 1: Power and Temperature
            ui.horizontal(|ui| {
                let plot_width = (ui.available_width() - 10.0) / 2.0;

                // Power plot with rainbow colors
                ui.vertical(|ui| {
                    ui.set_width(plot_width);
                    ui.set_height(plot_height);

                    // Frame for plot with neon border
                    egui::Frame::none()
                        .stroke(egui::Stroke::new(2.0, self.theme.neon_cyan))
                        .inner_margin(5.0)
                        .show(ui, |ui| {
                            Plot::new("power_plot")
                                .legend(egui_plot::Legend::default())
                                .height(plot_height - 30.0)
                                .show_axes([true, true])
                                .y_axis_label("⚡ Power (W)")
                                .x_axis_label("Time (s)")
                                .show(ui, |plot_ui| {
                                    for history in &self.device_history {
                                        // Rainbow colors cycling per device
                                        let hue = (history.device_idx as f32 * 90.0 + self.frame as f32 * 0.5) % 360.0;
                                        let color = hsv_to_rgb(hue, 0.9, 1.0);

                                        plot_ui.line(
                                            Line::new(&history.name, history.power_points())
                                                .color(color)
                                                .width(2.5),
                                        );
                                    }
                                });
                        });
                });

                // Temperature plot with fire colors
                ui.vertical(|ui| {
                    ui.set_width(plot_width);
                    ui.set_height(plot_height);

                    egui::Frame::none()
                        .stroke(egui::Stroke::new(2.0, self.theme.neon_magenta))
                        .inner_margin(5.0)
                        .show(ui, |ui| {
                            Plot::new("temp_plot")
                                .legend(egui_plot::Legend::default())
                                .height(plot_height - 30.0)
                                .show_axes([true, true])
                                .y_axis_label("🔥 Temperature (°C)")
                                .x_axis_label("Time (s)")
                                .show(ui, |plot_ui| {
                                    for history in &self.device_history {
                                        // Fire gradient (red-orange-yellow cycle)
                                        let hue = (history.device_idx as f32 * 90.0 + self.frame as f32 * 0.5 + 180.0) % 360.0;
                                        let color = hsv_to_rgb(hue, 0.9, 1.0);

                                        plot_ui.line(
                                            Line::new(&history.name, history.temp_points())
                                                .color(color)
                                                .width(2.5),
                                        );
                                    }
                                });
                        });
                });
            });

            ui.add_space(10.0);

            // Row 2: Current and Voltage
            ui.horizontal(|ui| {
                let plot_width = (ui.available_width() - 10.0) / 2.0;

                // Current plot with electric colors
                ui.vertical(|ui| {
                    ui.set_width(plot_width);
                    ui.set_height(plot_height);

                    egui::Frame::none()
                        .stroke(egui::Stroke::new(2.0, self.theme.neon_yellow))
                        .inner_margin(5.0)
                        .show(ui, |ui| {
                            Plot::new("current_plot")
                                .legend(egui_plot::Legend::default())
                                .height(plot_height - 30.0)
                                .show_axes([true, true])
                                .y_axis_label("⚡ Current (A)")
                                .x_axis_label("Time (s)")
                                .show(ui, |plot_ui| {
                                    for history in &self.device_history {
                                        // Electric blue-yellow gradient
                                        let hue = (history.device_idx as f32 * 90.0 + self.frame as f32 * 0.5 + 240.0) % 360.0;
                                        let color = hsv_to_rgb(hue, 0.9, 1.0);

                                        plot_ui.line(
                                            Line::new(&history.name, history.current_points())
                                                .color(color)
                                                .width(2.5),
                                        );
                                    }
                                });
                        });
                });

                // Voltage plot with plasma colors
                ui.vertical(|ui| {
                    ui.set_width(plot_width);
                    ui.set_height(plot_height);

                    egui::Frame::none()
                        .stroke(egui::Stroke::new(2.0, self.theme.neon_green))
                        .inner_margin(5.0)
                        .show(ui, |ui| {
                            Plot::new("voltage_plot")
                                .legend(egui_plot::Legend::default())
                                .height(plot_height - 30.0)
                                .show_axes([true, true])
                                .y_axis_label("⚡ Voltage (V)")
                                .x_axis_label("Time (s)")
                                .show(ui, |plot_ui| {
                                    for history in &self.device_history {
                                        // Plasma magenta-green gradient
                                        let hue = (history.device_idx as f32 * 90.0 + self.frame as f32 * 0.5 + 300.0) % 360.0;
                                        let color = hsv_to_rgb(hue, 0.9, 1.0);

                                        plot_ui.line(
                                            Line::new(&history.name, history.voltage_points())
                                                .color(color)
                                                .width(2.5),
                                        );
                                    }
                                });
                        });
                });
            });
        });
    }
}
