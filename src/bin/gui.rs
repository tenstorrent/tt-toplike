// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! TT-Toplike-RS - Native GUI Application
//!
//! This binary provides a native Wayland/X11 GUI for monitoring Tenstorrent hardware
//! using the iced framework.
//!
//! Features:
//! - Beautiful native window on KDE, GNOME, and other desktop environments
//! - Real-time telemetry display with historical charts
//! - GPU-accelerated starfield visualization
//! - Device selector with multiple view modes
//! - Dark mode optimized theme

use iced::{
    widget::{button, column, container, row, scrollable, text},
    Background, Color, Element, Length, Task, Theme,
};
use std::time::Duration;

use tt_toplike_rs::{
    backend::{factory, BackendConfig, TelemetryBackend, mock::MockBackend, json::JSONBackend},
    cli::{Cli, BackendType},
    init_logging,
    models::{Device, Architecture},
    ui::gui::{HistoryManager, TerminalGrid, terminal_canvas, visualization::{LineChart, DashboardVisualization}},
    animation::HardwareStarfield,
};

#[cfg(feature = "luwen-backend")]
use tt_toplike_rs::backend::luwen::LuwenBackend;

/// Display view modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// Dashboard with DDR, memory hierarchy, and animated metrics (DEFAULT)
    Dashboard,
    /// Table view with current telemetry details
    Table,
    /// Line charts showing historical data
    Charts,
    /// GPU-accelerated starfield visualization
    Starfield,
}

/// Main application state
struct TTTopGUI {
    /// Backend providing telemetry data
    backend: Box<dyn TelemetryBackend>,

    /// Current backend type
    backend_type: BackendType,

    /// Backend configuration
    config: BackendConfig,

    /// List of discovered devices
    devices: Vec<Device>,

    /// Currently selected device index
    selected_device: usize,

    /// CLI configuration
    cli: Cli,

    /// Error message (if any)
    error: Option<String>,

    /// Current view mode
    view_mode: ViewMode,

    /// Historical telemetry data
    history: HistoryManager,

    /// Starfield visualizations (TUI-style, one per device)
    starfields: Vec<HardwareStarfield>,

    /// Dashboard visualizations (one per device)
    dashboards: Vec<DashboardVisualization>,
}

/// Application messages
#[derive(Debug, Clone)]
enum Message {
    /// Periodic tick to update telemetry
    Tick,

    /// User selected a different device
    SelectDevice(usize),

    /// Refresh button clicked
    Refresh,

    /// Switch view mode
    SetViewMode(ViewMode),

    /// Switch to next backend
    SwitchBackend,
}

impl TTTopGUI {
    fn new(backend: Box<dyn TelemetryBackend>, backend_type: BackendType, config: BackendConfig, cli: Cli) -> (Self, Task<Message>) {
        let mut backend = backend;

        // Initialize backend
        let (devices, error) = match backend.init() {
            Ok(_) => {
                backend.update().ok();
                (backend.devices().to_vec(), None)
            }
            Err(e) => (vec![], Some(format!("Backend initialization failed: {}", e))),
        };

        // Create starfield visualizations for each device (TUI-style with terminal grid)
        // Use 120x40 character grid for nice large display
        // Note: Each starfield is created empty and will be populated by update_from_telemetry
        let starfields = (0..devices.len()).map(|_| HardwareStarfield::new(120, 40)).collect();

        // Create dashboard visualizations for each device
        let dashboards = devices.iter().map(|d| DashboardVisualization::new(d.clone())).collect();

        // Initialize history manager
        let mut history = HistoryManager::new();
        history.ensure_capacity(devices.len());

        (
            Self {
                backend,
                backend_type,
                config,
                devices,
                selected_device: 0,
                cli,
                error,
                view_mode: ViewMode::Dashboard, // Default to Dashboard!
                history,
                starfields,
                dashboards,
            },
            Task::none(),
        )
    }

    fn title(&self) -> String {
        format!("TT-Toplike v{} - Tenstorrent Hardware Monitor", env!("CARGO_PKG_VERSION"))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                // Update telemetry from backend
                if let Err(e) = self.backend.update() {
                    self.error = Some(format!("Update failed: {}", e));
                } else {
                    self.devices = self.backend.devices().to_vec();
                    self.error = None;

                    // Update history for all devices
                    for device in &self.devices {
                        if let Some(telem) = self.backend.telemetry(device.index) {
                            self.history.push(device.index, telem);
                        }
                    }

                    // Update visualizations
                    for starfield in self.starfields.iter_mut() {
                        starfield.update_from_telemetry(&self.backend);
                    }

                    for (i, dashboard) in self.dashboards.iter_mut().enumerate() {
                        let hist = self.history.get(i);
                        dashboard.update(hist);
                    }
                }
            }
            Message::SelectDevice(idx) => {
                if idx < self.devices.len() {
                    self.selected_device = idx;
                }
            }
            Message::Refresh => {
                if let Err(e) = self.backend.update() {
                    self.error = Some(format!("Refresh failed: {}", e));
                }
            }
            Message::SetViewMode(mode) => {
                self.view_mode = mode;
            }
            Message::SwitchBackend => {
                // Backend switching now works in all modes (terminal-based starfield doesn't use heavy GPU resources)
                // Attempt to switch to next backend
                log::info!("GUI: Attempting to switch from {:?} backend", self.backend_type);

                match factory::switch_to_next_backend(self.backend_type, self.config.clone(), &self.cli) {
                    Ok((new_backend, new_type)) => {
                        self.backend = new_backend;
                        self.backend_type = new_type;
                        log::info!("GUI: Successfully switched to {:?} backend", self.backend_type);

                        // Update devices from new backend
                        self.devices = self.backend.devices().to_vec();

                        // Reinitialize visualizations with new backend
                        self.starfields = (0..self.devices.len()).map(|_| HardwareStarfield::new(120, 40)).collect();
                        self.dashboards = self.devices.iter().map(|d| DashboardVisualization::new(d.clone())).collect();

                        // Reset history
                        self.history = HistoryManager::new();
                        self.history.ensure_capacity(self.devices.len());

                        // Reset selected device if out of bounds
                        if self.selected_device >= self.devices.len() {
                            self.selected_device = 0;
                        }

                        self.error = Some("Backend switched successfully!".to_string());
                    }
                    Err(e) => {
                        self.error = Some(format!("Failed to switch backend: {}", e));
                        log::error!("GUI: Failed to switch backend: {}", e);
                    }
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<Message> {
        // If we have an error, display it prominently
        if let Some(ref err) = self.error {
            return container(
                column![
                    text("Error").size(32),
                    text(err).size(16),
                    button(text("Retry")).on_press(Message::Refresh),
                ]
                .spacing(20)
                .padding(40),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        // If no devices, show a message
        if self.devices.is_empty() {
            return container(
                column![
                    text("No devices found").size(24),
                    text("Make sure Tenstorrent hardware is connected").size(14),
                    button(text("Refresh")).on_press(Message::Refresh),
                ]
                .spacing(20)
                .padding(40),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        // Header with backend info
        let header = container(
            row![
                text(format!("🦀 TT-Toplike v{}", env!("CARGO_PKG_VERSION"))).size(20),
                text(" | ").size(16),
                text(format!("Backend: {}", self.backend.backend_info())).size(16),
                text(" | ").size(16),
                text(format!("{} devices", self.devices.len())).size(16),
            ]
            .spacing(5)
            .padding(10),
        )
        .width(Length::Fill);

        // Device tabs
        let mut device_tabs = row![].spacing(5).padding(5);
        for (i, device) in self.devices.iter().enumerate() {
            let btn = button(text(format!("Device {}: {}", i, device.board_type)).size(14))
                .on_press(Message::SelectDevice(i));
            device_tabs = device_tabs.push(btn);
        }

        // View mode selector
        let view_selector = row![
            button(text("🎛 Dashboard")).on_press(Message::SetViewMode(ViewMode::Dashboard)),
            button(text("📋 Details")).on_press(Message::SetViewMode(ViewMode::Table)),
            button(text("📈 Charts")).on_press(Message::SetViewMode(ViewMode::Charts)),
            button(text("✨ Starfield")).on_press(Message::SetViewMode(ViewMode::Starfield)),
        ]
        .spacing(5)
        .padding(5);

        // Main content based on view mode
        let content = match self.view_mode {
            ViewMode::Dashboard => self.view_dashboard(),
            ViewMode::Table => self.view_table(),
            ViewMode::Charts => self.view_charts(),
            ViewMode::Starfield => self.view_starfield(),
        };

        // Footer
        let footer = container(
            row![
                button(text("🔄 Refresh")).on_press(Message::Refresh),
                button(text("🔀 Switch Backend")).on_press(Message::SwitchBackend),
                text(format!("Backend: {} | Interval: {}ms | {} samples",
                    self.backend.backend_info(),
                    self.cli.interval,
                    self.history.get(self.selected_device).map(|h| h.len()).unwrap_or(0)
                )).size(12),
            ]
            .spacing(10)
            .padding(10),
        )
        .width(Length::Fill);

        // Messages panel - show recent log messages
        let messages_panel = self.view_messages();

        // Main layout
        container(
            column![
                header,
                row![device_tabs, view_selector].spacing(20),
                content,
                messages_panel,
                footer,
            ]
            .spacing(10)
            .padding(10),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    /// Dashboard view with DDR, memory hierarchy, and animated metrics
    fn view_dashboard(&self) -> Element<Message> {
        if let Some(dashboard) = self.dashboards.get(self.selected_device) {
            // Cast the Element<()> to Element<Message> by mapping it
            dashboard.view().map(|_| Message::Tick)
        } else {
            container(
                text("Dashboard not available").size(18)
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    }

    /// Table view showing current telemetry
    fn view_table(&self) -> Element<Message> {
        let device = &self.devices[self.selected_device];
        let telemetry = self.backend.telemetry(device.index);

        let mut telemetry_col = column![].spacing(10).padding(20);

        // Device info
        telemetry_col = telemetry_col.push(
            text(format!("Device {}: {}", device.index, device.board_type)).size(24),
        );
        telemetry_col = telemetry_col.push(text(format!("Architecture: {:?}", device.architecture)).size(16));
        telemetry_col = telemetry_col.push(text(format!("Bus ID: {}", device.bus_id)).size(14));

        // Architecture details
        let arch_details = match device.architecture {
            Architecture::Grayskull => "Grayskull: 4 DDR channels, 10×12 Tensix grid".to_string(),
            Architecture::Wormhole => "Wormhole: 8 DDR channels, 8×10 Tensix grid".to_string(),
            Architecture::Blackhole => "Blackhole: 12 DDR channels, 14×16 Tensix grid".to_string(),
            Architecture::Unknown => "Unknown architecture".to_string(),
        };
        telemetry_col = telemetry_col.push(text(arch_details).size(12));

        // Telemetry data
        if let Some(telem) = telemetry {
            telemetry_col = telemetry_col.push(text("").size(10)); // Spacer

            let power = telem.power.unwrap_or(0.0);
            telemetry_col = telemetry_col.push(text(format!("⚡ Power: {:.1} W", power)).size(20));

            let temp = telem.asic_temperature.unwrap_or(0.0);
            telemetry_col = telemetry_col.push(text(format!("🌡 Temperature: {:.1} °C", temp)).size(20));

            let current = telem.current.unwrap_or(0.0);
            telemetry_col = telemetry_col.push(text(format!("⚙ Current: {:.2} A", current)).size(18));

            let voltage = telem.voltage.unwrap_or(0.0);
            telemetry_col = telemetry_col.push(text(format!("🔋 Voltage: {:.3} V", voltage)).size(18));

            let aiclk = telem.aiclk.unwrap_or(0);
            telemetry_col = telemetry_col.push(text(format!("⏱ AICLK: {} MHz", aiclk)).size(18));

            let heartbeat = telem.heartbeat.unwrap_or(0);
            telemetry_col = telemetry_col.push(text(format!("💓 Heartbeat: {}", heartbeat)).size(18));
        } else {
            telemetry_col = telemetry_col.push(text("No telemetry available").size(18));
        }

        scrollable(telemetry_col).height(Length::Fill).into()
    }

    /// Charts view showing historical data
    fn view_charts(&self) -> Element<Message> {
        if let Some(history) = self.history.get(self.selected_device) {
            if history.is_empty() {
                return container(
                    text("Collecting data... please wait").size(18)
                )
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
            }

            // Power chart
            let power_data: Vec<f32> = history.power.iter().copied().collect();
            let power_range = history.power_range();
            let power_chart = LineChart::new(
                format!("Power (W) - Min: {:.1}W, Max: {:.1}W", power_range.0, power_range.1),
                power_data,
                power_range,
                Color::from_rgb(0.31, 0.86, 0.78), // Teal
            );

            // Temperature chart
            let temp_data: Vec<f32> = history.temperature.iter().copied().collect();
            let temp_range = history.temp_range();
            let temp_chart = LineChart::new(
                format!("Temperature (°C) - Min: {:.1}°C, Max: {:.1}°C", temp_range.0, temp_range.1),
                temp_data,
                temp_range,
                Color::from_rgb(1.0, 0.71, 0.39), // Orange
            );

            let charts_col = column![
                container(text(format!("Device {}: {} - Historical Data",
                    self.selected_device,
                    self.devices[self.selected_device].board_type
                )).size(20)).padding(10),
                power_chart.view().map(|_| Message::Tick),
                temp_chart.view().map(|_| Message::Tick),
                container(text(format!("{} samples (last {:.1}s)",
                    history.len(),
                    history.len() as f32 * self.cli.interval as f32 / 1000.0
                )).size(12)).padding(10),
            ]
            .spacing(10);

            scrollable(charts_col).height(Length::Fill).into()
        } else {
            container(
                text("No data available").size(18)
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    }

    /// Starfield visualization view
    fn view_starfield(&self) -> Element<Message> {
        if let Some(starfield) = self.starfields.get(self.selected_device) {
            // Render starfield to terminal grid with larger size for better window filling
            // Increased from 120x40 to 160x60 for better scaling
            let mut grid = TerminalGrid::new(160, 60);
            starfield.render_to_grid(&mut grid);

            // Create terminal canvas with smaller font/cell size for better scaling
            // Reduced from 10.0x20.0 to 8.0x16.0 for more compact display
            let canvas: Element<Message> = Element::from(terminal_canvas::view(grid, 8.0, 16.0))
                .map(|_| Message::Tick);

            // Wrap in container for layout
            container(
                column![
                    // Title with baseline status
                    container(
                        text(format!("Hardware Starfield | {}", starfield.baseline_status()))
                            .size(16)
                            .color(Color::from_rgb(0.4, 0.7, 1.0))
                    )
                    .padding(10),
                    // Terminal canvas
                    canvas,
                    // Legend
                    container(
                        text("⭐ Stars = Tensix cores (brightness=power, color=temp) | ◉ Planets = Memory (L1/L2/DDR)")
                            .size(12)
                            .color(Color::from_rgb(0.6, 0.6, 0.6))
                    )
                    .padding(5),
                ]
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .into()
        } else {
            container(
                text("Visualization not available").size(18)
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    }

    /// Messages view - display recent log messages
    fn view_messages(&self) -> Element<Message> {
        use tt_toplike_rs::logging::get_recent_log_messages;

        // Get recent log messages (last 5)
        let messages = get_recent_log_messages(5);

        // Create text rows
        let mut message_rows: Vec<Element<Message>> = Vec::new();

        if messages.is_empty() {
            message_rows.push(
                text("No log messages yet")
                    .size(12)
                    .color(Color::from_rgb(0.4, 0.4, 0.4))
                    .into()
            );
        } else {
            for msg in messages.iter().rev() {
                let level_color = match msg.level {
                    log::Level::Error => Color::from_rgb(1.0, 0.4, 0.4),   // Red
                    log::Level::Warn => Color::from_rgb(1.0, 0.7, 0.4),    // Orange
                    log::Level::Info => Color::from_rgb(0.4, 0.7, 1.0),    // Blue
                    log::Level::Debug => Color::from_rgb(0.6, 0.6, 0.6),   // Gray
                    log::Level::Trace => Color::from_rgb(0.4, 0.4, 0.4),   // Dim gray
                };

                message_rows.push(
                    row![
                        text(format!("[{}]", msg.timestamp))
                            .size(12)
                            .color(Color::from_rgb(0.6, 0.6, 0.6)),
                        text(format!("{:5}", msg.level.to_string()))
                            .size(12)
                            .color(level_color),
                        text(msg.message.clone())
                            .size(12)
                            .color(Color::from_rgb(0.9, 0.9, 0.9)),
                    ]
                    .spacing(10)
                    .into()
                );
            }
        }

        container(
            column(message_rows)
                .spacing(2)
                .padding(5)
        )
        .style(|_theme: &Theme| {
            container::Style {
                background: Some(Background::Color(Color::from_rgb(0.1, 0.1, 0.15))),
                border: iced::Border {
                    color: Color::from_rgb(0.3, 0.6, 0.8),
                    width: 1.0,
                    radius: 5.0.into(),
                },
                ..Default::default()
            }
        })
        .width(Length::Fill)
        .height(Length::Fixed(120.0))
        .into()
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        // Periodic updates at configured interval
        iced::time::every(Duration::from_millis(self.cli.interval)).map(|_| Message::Tick)
    }

    fn theme(&self) -> Theme {
        Theme::Dark
    }
}

/// Create backend based on CLI configuration
fn create_backend(cli: &Cli) -> Box<dyn TelemetryBackend> {
    let config = BackendConfig::default()
        .with_interval(cli.interval)
        .with_max_errors(cli.max_errors);

    let backend_type = cli.effective_backend();

    match backend_type {
        BackendType::Mock => {
            log::info!("Creating MockBackend with {} devices", cli.mock_devices);
            Box::new(MockBackend::with_config(cli.mock_devices, config))
        }
        BackendType::Json => {
            log::info!("Creating JSONBackend with tt-smi path: {:?}", cli.tt_smi_path);
            Box::new(JSONBackend::with_config(
                cli.tt_smi_path.to_string_lossy().to_string(),
                config,
            ))
        }
        BackendType::Auto => {
            // SAFE MODE AUTO-DETECT: Never tries Luwen (invasive, requires PCI access)
            // Order: Sysfs (hwmon) → JSON (tt-smi) → Mock
            // Use --backend luwen explicitly if you need direct hardware access
            log::info!("Auto-detecting backend (safe mode - skipping Luwen)...");

            // Try Sysfs backend first (Linux hwmon sensors - SAFEST, non-invasive)
            #[cfg(target_os = "linux")]
            {
                log::info!("Trying Sysfs backend (hwmon sensors - safest, non-invasive)...");
                let mut sysfs_backend = tt_toplike_rs::backend::sysfs::SysfsBackend::with_config(config.clone());

                if sysfs_backend.init().is_ok() {
                    log::info!("Sysfs backend initialized successfully");
                    return Box::new(sysfs_backend);
                } else {
                    log::warn!("Sysfs backend failed, trying JSON backend");
                }
            }

            // Try JSON backend as second option (tt-smi subprocess - safe)
            log::info!("Trying JSON backend (tt-smi subprocess)...");
            let mut json_backend = JSONBackend::with_config(
                cli.tt_smi_path.to_string_lossy().to_string(),
                config.clone(),
            );

            if json_backend.init().is_ok() {
                log::info!("JSON backend initialized successfully");
                return Box::new(json_backend);
            }

            // Last resort: Mock backend (for testing without hardware)
            log::warn!("No hardware backends available, using mock backend");
            log::info!("Tip: Use --backend luwen for direct hardware access (requires PCI permissions)");
            Box::new(MockBackend::with_config(cli.mock_devices, config))
        }
        BackendType::Sysfs => {
            #[cfg(target_os = "linux")]
            {
                log::info!("Creating Sysfs backend");
                Box::new(tt_toplike_rs::backend::sysfs::SysfsBackend::with_config(config))
            }
            #[cfg(not(target_os = "linux"))]
            {
                eprintln!("Error: Sysfs backend only available on Linux");
                eprintln!("Use --mock or --json instead");
                std::process::exit(1);
            }
        }
        BackendType::Luwen => {
            #[cfg(feature = "luwen-backend")]
            {
                log::info!("Creating LuwenBackend");
                Box::new(LuwenBackend::with_config(config))
            }
            #[cfg(not(feature = "luwen-backend"))]
            {
                eprintln!("Error: Luwen backend not enabled");
                eprintln!("Rebuild with: cargo build --features luwen-backend,gui");
                std::process::exit(1);
            }
        }
    }
}

fn main() -> iced::Result {
    // Parse CLI arguments
    let cli = Cli::parse_args();

    // Validate arguments
    if let Err(e) = cli.validate() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    // Initialize logging
    init_logging(cli.log_level());

    // Print startup info to console
    println!("🦀 TT-Toplike-RS GUI v{}", env!("CARGO_PKG_VERSION"));
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Backend: {}", cli.backend_name());
    println!("✨ Features:");
    println!("  🎛  Dashboard: DDR channels + Memory hierarchy + Animated metrics");
    println!("  📈 Charts: Historical power & temperature");
    println!("  ✨ Starfield: GPU-accelerated psychedelic visualization");
    println!("  📋 Details: Complete telemetry table");
    println!("Launching native GUI...");
    println!();

    // Create backend configuration
    let config = BackendConfig::default()
        .with_interval(cli.interval)
        .with_max_errors(cli.max_errors);

    // Get effective backend type
    let backend_type = cli.effective_backend();

    // Create backend using factory
    let backend = match factory::create_backend(backend_type, config.clone(), &cli) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to initialize backend: {}", e);
            eprintln!("Falling back to mock backend");
            let mut mock = MockBackend::with_config(cli.mock_devices, config.clone());
            mock.init().expect("Mock backend should always succeed");
            Box::new(mock) as Box<dyn TelemetryBackend>
        }
    };

    // Run iced application
    iced::application(
        "TT-Toplike - Tenstorrent Hardware Monitor",
        TTTopGUI::update,
        TTTopGUI::view,
    )
    .subscription(TTTopGUI::subscription)
    .theme(TTTopGUI::theme)
    .run_with(move || TTTopGUI::new(backend, backend_type, config, cli))
}
