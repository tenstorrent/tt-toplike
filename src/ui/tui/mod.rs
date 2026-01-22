//! Terminal User Interface module
//!
//! This module provides the TUI implementation using Ratatui.
//! It displays real-time telemetry data in a beautiful terminal interface.
//!
//! Supports two display modes:
//! - Normal mode: Traditional table view with real-time telemetry
//! - Visualization mode: Hardware-responsive starfield animation

use crate::animation::{HardwareStarfield, TronGrid};
use crate::backend::{factory, BackendConfig, TelemetryBackend};
use crate::cli::{BackendType, Cli};
use crate::error::TTTopError;
use crate::ui::colors;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Row, Table},
    Frame, Terminal,
};
use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

/// UI display mode
#[derive(Debug, Clone, Copy, PartialEq)]
enum DisplayMode {
    /// Normal table view with telemetry
    Normal,
    /// Hardware-responsive starfield visualization (original)
    Starfield,
    /// TRON Grid mode (neon topology with randomization)
    TronGrid,
}

/// Run the TUI application
///
/// This is the main entry point for the TUI. It creates the backend,
/// sets up the terminal, runs the event loop, and cleans up on exit.
///
/// # Arguments
///
/// * `cli` - CLI configuration
pub fn run_tui(cli: &Cli) -> Result<(), TTTopError> {
    // Check if we have a TTY available
    if !std::io::stdout().is_terminal() {
        return Err(TTTopError::Terminal(
            "No TTY available. The TUI requires an interactive terminal.\n\
             Try running with an actual terminal (not through SSH without -t, pipes, or redirects)."
                .to_string()
        ));
    }

    // Create backend config
    let config = BackendConfig::new()
        .with_interval(cli.interval)
        .with_max_errors(cli.max_errors);

    let config = if cli.verbose { config.verbose() } else { config };

    // Create initial backend
    let backend_type = cli.effective_backend();
    let mut backend = factory::create_backend(backend_type, config.clone(), cli)
        .map_err(TTTopError::Backend)?;

    log::info!("TUI started with {:?} backend", backend_type);

    // Setup terminal
    enable_raw_mode().map_err(|e| TTTopError::Terminal(format!(
        "Failed to enable raw mode: {}. \n\
         This usually means the terminal is not properly configured.",
        e
    )))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Disable stderr output to prevent log corruption in TUI
    crate::logging::disable_stderr();

    let backend_term = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend_term).map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Create app state and run
    let res = run_app(&mut terminal, &mut backend, backend_type, config, cli);

    // Re-enable stderr output before exiting
    crate::logging::enable_stderr();

    // Restore terminal
    disable_raw_mode().map_err(|e| TTTopError::Terminal(e.to_string()))?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .map_err(|e| TTTopError::Terminal(e.to_string()))?;
    terminal
        .show_cursor()
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    res
}

/// Main application event loop
fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    backend: &mut Box<dyn TelemetryBackend>,
    mut backend_type: BackendType,
    config: BackendConfig,
    cli: &Cli,
) -> Result<(), TTTopError> {
    let update_interval = Duration::from_millis(cli.interval);
    let mut last_update = Instant::now();

    // UI state
    let mut display_mode = DisplayMode::Normal;
    let mut starfield: Option<HardwareStarfield> = None;
    let mut tron_grid: Option<TronGrid> = None;

    loop {
        // Initialize or update visualizations
        let size = terminal.size().map_err(|e| TTTopError::Terminal(e.to_string()))?;

        match display_mode {
            DisplayMode::Starfield => {
                if starfield.is_none() {
                    let mut sf = HardwareStarfield::new(size.width as usize, size.height as usize);
                    sf.initialize_from_devices(backend.devices());
                    starfield = Some(sf);
                }
                if let Some(ref mut sf) = starfield {
                    sf.update_from_telemetry(backend);
                }
            }
            DisplayMode::TronGrid => {
                if tron_grid.is_none() {
                    // Create new TronGrid with random parameters
                    tron_grid = Some(TronGrid::new(size.width as usize, size.height as usize));
                }
                if let Some(ref mut tg) = tron_grid {
                    tg.update(backend);
                }
            }
            DisplayMode::Normal => {
                // Normal mode doesn't need special init
            }
        }

        // Draw UI based on mode
        terminal
            .draw(|f| {
                match display_mode {
                    DisplayMode::Normal => ui(f, backend, cli),
                    DisplayMode::Starfield => {
                        if let Some(ref sf) = starfield {
                            ui_visualization(f, sf, backend);
                        }
                    }
                    DisplayMode::TronGrid => {
                        if let Some(ref tg) = tron_grid {
                            ui_tron_grid(f, tg, backend);
                        }
                    }
                }
            })
            .map_err(|e| TTTopError::Terminal(e.to_string()))?;

        // Handle input with timeout
        let timeout = update_interval
            .checked_sub(last_update.elapsed())
            .unwrap_or(Duration::from_millis(0));

        if event::poll(timeout).map_err(|e| TTTopError::Terminal(e.to_string()))? {
            if let Event::Key(key) = event::read().map_err(|e| TTTopError::Terminal(e.to_string()))? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        return Ok(());
                    }
                    KeyCode::Char('r') => {
                        // Force refresh
                        if let Err(e) = backend.update() {
                            log::warn!("Update failed: {}", e);
                        }
                    }
                    KeyCode::Char('v') => {
                        // Cycle through visualization modes (with randomization)
                        display_mode = match display_mode {
                            DisplayMode::Normal => DisplayMode::Starfield,
                            DisplayMode::Starfield => {
                                // Randomize TRON Grid on each activation
                                tron_grid = None;
                                DisplayMode::TronGrid
                            }
                            DisplayMode::TronGrid => DisplayMode::Normal,
                        };
                        log::info!("Switched to {:?} mode (randomized)", display_mode);
                    }
                    KeyCode::Char('b') => {
                        // Switch to next backend
                        log::info!("Attempting to switch from {:?} backend", backend_type);

                        match factory::switch_to_next_backend(backend_type, config.clone(), cli) {
                            Ok((new_backend, new_type)) => {
                                *backend = new_backend;
                                backend_type = new_type;
                                log::info!("Successfully switched to {:?} backend", backend_type);

                                // Reinitialize visualizations with new backend
                                if let DisplayMode::Starfield = display_mode {
                                    starfield = None;
                                }
                                if let DisplayMode::TronGrid = display_mode {
                                    tron_grid = None;
                                }
                            }
                            Err(e) => {
                                log::error!("Failed to switch backend: {}", e);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Update backend data
        if last_update.elapsed() >= update_interval {
            if let Err(e) = backend.update() {
                log::warn!("Update failed: {}", e);
            }
            last_update = Instant::now();
        }
    }
}

/// Render the UI
fn ui(f: &mut Frame, backend: &Box<dyn TelemetryBackend>, cli: &Cli) {
    // Create layout
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),      // Header
            Constraint::Min(10),        // Content (device list)
            Constraint::Length(6),      // Messages (log display)
            Constraint::Length(3),      // Footer
        ])
        .split(f.area());

    // Render header
    render_header(f, chunks[0], backend);

    // Render device list
    render_devices(f, chunks[1], backend, cli);

    // Render messages
    render_messages(f, chunks[2]);

    // Render footer
    let backend_info = backend.backend_info();
    render_footer(f, chunks[3], &backend_info);
}

/// Render header with app title and status
fn render_header(f: &mut Frame, area: Rect, backend: &Box<dyn TelemetryBackend>) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            "🦀 TT-TOPLIKE-RS ",
            Style::default()
                .fg(Color::Rgb(102, 126, 234))  // Vibrant purple-blue
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " │ ",
            Style::default().fg(Color::Rgb(118, 75, 162)),  // Deep purple
        ),
        Span::styled(
            format!("{} ", backend.backend_info()),
            Style::default()
                .fg(Color::Rgb(56, 178, 172))  // Teal
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " │ ",
            Style::default().fg(Color::Rgb(118, 75, 162)),
        ),
        Span::styled(
            format!("{} devices", backend.device_count()),
            Style::default()
                .fg(Color::Rgb(56, 178, 172))
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(Color::Rgb(102, 126, 234))  // Vibrant purple-blue
                    .add_modifier(Modifier::BOLD))
                .title(" ⚡ Real-Time Hardware Monitoring ⚡ ")
                .title_alignment(Alignment::Center),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render device list with telemetry
fn render_devices(f: &mut Frame, area: Rect, backend: &Box<dyn TelemetryBackend>, cli: &Cli) {
    let mut rows = Vec::new();

    // Header row
    let header_style = Style::default()
        .fg(colors::PRIMARY)
        .add_modifier(Modifier::BOLD);

    // Data rows
    for device in backend.devices() {
        // Apply device filter
        if !cli.should_monitor_device(device.index) {
            continue;
        }

        let telem = backend.telemetry(device.index);
        let smbus = backend.smbus_telemetry(device.index);

        // Device name
        let name = format!("{}", device.name());

        // Architecture
        let arch = device.architecture.abbrev().to_string();

        // Power
        let (power_str, power_style) = if let Some(t) = telem {
            let power = t.power_w();
            (
                format!("{:.1}W", power),
                Style::default().fg(colors::power_color(power)),
            )
        } else {
            ("N/A".to_string(), Style::default().fg(colors::TEXT_SECONDARY))
        };

        // Temperature
        let (temp_str, temp_style) = if let Some(t) = telem {
            let temp = t.temp_c();
            (
                format!("{:.1}°C", temp),
                Style::default().fg(colors::temp_color(temp)),
            )
        } else {
            ("N/A".to_string(), Style::default().fg(colors::TEXT_SECONDARY))
        };

        // Current
        let current_str = if let Some(t) = telem {
            format!("{:.1}A", t.current_a())
        } else {
            "N/A".to_string()
        };

        // Voltage
        let voltage_str = if let Some(t) = telem {
            format!("{:.2}V", t.voltage.unwrap_or(0.0))
        } else {
            "N/A".to_string()
        };

        // AICLK
        let aiclk_str = if let Some(t) = telem {
            format!("{}MHz", t.aiclk_mhz())
        } else {
            "N/A".to_string()
        };

        // ARC Health
        let (health_str, health_style) = if let Some(s) = smbus {
            let healthy = s.is_arc0_healthy();
            (
                if healthy { "✓ Healthy" } else { "✗ Failed" },
                Style::default().fg(colors::health_color(healthy)),
            )
        } else {
            ("Unknown", Style::default().fg(colors::TEXT_SECONDARY))
        };

        rows.push(
            Row::new(vec![
                name,
                arch,
                power_str,
                temp_str,
                current_str,
                voltage_str,
                aiclk_str,
                health_str.to_string(),
            ])
            .style(Style::default().fg(colors::TEXT_PRIMARY))
            .height(1),
        );

        // Apply color to specific cells
        // (Ratatui doesn't support per-cell styling in the simple way, so we use styled text)
    }

    let widths = [
        Constraint::Length(15), // Name
        Constraint::Length(6),  // Arch
        Constraint::Length(8),  // Power
        Constraint::Length(8),  // Temp
        Constraint::Length(8),  // Current
        Constraint::Length(8),  // Voltage
        Constraint::Length(10), // AICLK
        Constraint::Length(12), // Health
    ];

    let table = Table::new(rows, widths)
        .header(
            Row::new(vec![
                "Device",
                "Arch",
                "Power",
                "Temp",
                "Current",
                "Voltage",
                "AICLK",
                "ARC Health",
            ])
            .style(header_style)
            .height(1),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)  // Rounded borders
                .border_style(Style::default()
                    .fg(Color::Rgb(56, 178, 172))  // Teal borders
                    .add_modifier(Modifier::BOLD))
                .title(" ⚡ Hardware Telemetry ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(Color::Rgb(102, 126, 234))  // Purple-blue title
                    .add_modifier(Modifier::BOLD)),
        )
        .column_spacing(2);  // More spacing for readability

    f.render_widget(table, area);
}

/// Render footer with keyboard shortcuts and backend info
fn render_footer(f: &mut Frame, area: Rect, backend_info: &str) {
    let footer_text = vec![Line::from(vec![
        Span::styled(" q ", Style::default()
            .fg(Color::Rgb(255, 100, 100))  // Bright red
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" quit  ", Style::default().fg(Color::Rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(Color::Rgb(150, 120, 180))),  // Purple separator
        Span::styled(" r ", Style::default()
            .fg(Color::Rgb(100, 180, 255))  // Bright blue
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" refresh  ", Style::default().fg(Color::Rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(Color::Rgb(150, 120, 180))),  // Purple separator
        Span::styled(" v ", Style::default()
            .fg(Color::Rgb(80, 220, 200))  // Bright teal
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" visualize  ", Style::default().fg(Color::Rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(Color::Rgb(150, 120, 180))),  // Purple separator
        Span::styled(" b ", Style::default()
            .fg(Color::Rgb(150, 220, 100))  // Bright green
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" backend  ", Style::default().fg(Color::Rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(Color::Rgb(150, 120, 180))),  // Purple separator
        Span::styled(" ESC ", Style::default()
            .fg(Color::Rgb(255, 200, 100))  // Bright orange
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" exit", Style::default().fg(Color::Rgb(160, 160, 160))),
    ])];

    let title = format!(" ⌨  Keyboard Controls │ Backend: {} ", backend_info);

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)  // Rounded borders
                .border_style(Style::default()
                    .fg(Color::Rgb(102, 126, 234))  // Purple-blue borders
                    .add_modifier(Modifier::BOLD))
                .title(title)
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(Color::Rgb(56, 178, 172))  // Teal title
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render recent log messages
fn render_messages(f: &mut Frame, area: Rect) {
    use crate::logging::get_recent_log_messages;

    // Get recent log messages (last 5)
    let messages = get_recent_log_messages(5);

    // Create text lines with color-coded log levels
    let mut lines = Vec::new();
    for msg in messages.iter().rev() {
        let level_color = match msg.level {
            log::Level::Error => Color::Rgb(255, 100, 100),   // Bright red
            log::Level::Warn => Color::Rgb(255, 180, 100),    // Bright orange
            log::Level::Info => Color::Rgb(100, 180, 255),    // Bright blue
            log::Level::Debug => Color::Rgb(150, 150, 150),   // Gray
            log::Level::Trace => Color::Rgb(100, 100, 100),   // Dim gray
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("[{}] ", msg.timestamp),
                Style::default().fg(Color::Rgb(160, 160, 160)),
            ),
            Span::styled(
                format!("{:5} ", msg.level.to_string()),
                Style::default().fg(level_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &msg.message,
                Style::default().fg(Color::Rgb(220, 220, 220)),
            ),
        ]));
    }

    // Show helpful text if no messages
    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "No log messages yet",
                Style::default().fg(Color::Rgb(100, 100, 100)).add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    let messages_widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(Color::Rgb(80, 220, 200))  // Teal borders
                    .add_modifier(Modifier::BOLD))
                .title(" 📋 Recent Messages ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(Color::Rgb(102, 126, 234))  // Purple-blue title
                    .add_modifier(Modifier::BOLD)),
        )
        .wrap(ratatui::widgets::Wrap { trim: true });

    f.render_widget(messages_widget, area);
}

/// Render visualization mode (full-screen starfield)
fn ui_visualization(
    f: &mut Frame,
    starfield: &HardwareStarfield,
    backend: &Box<dyn TelemetryBackend>,
) {
    // Create layout with header and content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(0),     // Starfield content
            Constraint::Length(3),  // Footer
        ])
        .split(f.area());

    // Render visualization header
    render_visualization_header(f, chunks[0], starfield, backend);

    // Render starfield content
    let starfield_lines = starfield.render();

    let starfield_widget = Paragraph::new(starfield_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY))
                .title(" Hardware-Responsive Starfield ")
                .title_alignment(Alignment::Center),
        );

    f.render_widget(starfield_widget, chunks[1]);

    // Render visualization footer
    render_visualization_footer(f, chunks[2]);
}

/// Render visualization mode header with baseline status
fn render_visualization_header<B: TelemetryBackend>(
    f: &mut Frame,
    area: Rect,
    starfield: &HardwareStarfield,
    backend: &B,
) {
    let status = starfield.baseline_status();
    let status_color = if starfield.is_baseline_established() {
        colors::SUCCESS
    } else {
        colors::WARNING
    };

    let header_text = vec![Line::from(vec![
        Span::styled(
            " TT-Toplike-RS ",
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("│ {} ", backend.backend_info()),
            Style::default().fg(colors::TEXT_SECONDARY),
        ),
        Span::styled(
            format!("│ {} ", status),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .style(Style::default().bg(colors::BACKGROUND))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY))
                .title(" 🌌 Visualization Mode ")
                .title_alignment(Alignment::Left),
        )
        .alignment(Alignment::Left);

    f.render_widget(header, area);
}

/// Render visualization mode footer with legend
fn render_visualization_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("Stars: ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Tensix Cores ", Style::default().fg(colors::PRIMARY)),
        Span::styled("│ ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Planets: ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Memory (L1/L2/DDR) ", Style::default().fg(colors::WARNING)),
        Span::styled("│ ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Color: ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Temperature ", Style::default().fg(colors::ERROR)),
        Span::styled("│ ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled(" v ", Style::default().fg(colors::SUCCESS).add_modifier(Modifier::BOLD)),
        Span::styled("exit viz", Style::default().fg(colors::TEXT_SECONDARY)),
    ])];

    let footer = Paragraph::new(footer_text)
        .style(Style::default().bg(colors::BACKGROUND))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::BORDER))
                .title(" Legend ")
                .title_alignment(Alignment::Left),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render TRON Grid mode (full-screen neon topology)
fn ui_tron_grid(
    f: &mut Frame,
    tron_grid: &TronGrid,
    backend: &Box<dyn TelemetryBackend>,
) {
    // Create layout with header and content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(0),     // TRON Grid content
            Constraint::Length(3),  // Footer
        ])
        .split(f.area());

    // Render TRON Grid header
    render_tron_grid_header(f, chunks[0], backend);

    // Render TRON Grid content
    let tron_lines = tron_grid.render(backend);

    let tron_widget = Paragraph::new(tron_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::PRIMARY))
                .title(" TRON Grid - Neon Hardware Topology ")
                .title_alignment(Alignment::Center),
        );

    f.render_widget(tron_widget, chunks[1]);

    // Render TRON Grid footer
    render_tron_grid_footer(f, chunks[2]);
}

/// Render TRON Grid mode header with device info
fn render_tron_grid_header<B: TelemetryBackend>(
    f: &mut Frame,
    area: Rect,
    backend: &B,
) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            " TT-Toplike-RS ",
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("│ {} ", backend.backend_info()),
            Style::default().fg(colors::TEXT_SECONDARY),
        ),
        Span::styled(
            format!("│ {} devices ", backend.device_count()),
            Style::default().fg(colors::SUCCESS).add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .style(Style::default().bg(colors::BACKGROUND))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::PRIMARY))
                .title(" ⚡ TRON Grid Mode ⚡ ")
                .title_alignment(Alignment::Center),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render TRON Grid mode footer with controls
fn render_tron_grid_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("Grid: ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Neon Topology ", Style::default().fg(colors::PRIMARY)),
        Span::styled("│ ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Nodes: ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Tensix Cores ", Style::default().fg(colors::WARNING)),
        Span::styled("│ ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Color: ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled("Activity + Temp ", Style::default().fg(colors::ERROR)),
        Span::styled("│ ", Style::default().fg(colors::TEXT_SECONDARY)),
        Span::styled(" v ", Style::default().fg(colors::SUCCESS).add_modifier(Modifier::BOLD)),
        Span::styled("randomize", Style::default().fg(colors::TEXT_SECONDARY)),
    ])];

    let footer = Paragraph::new(footer_text)
        .style(Style::default().bg(colors::BACKGROUND))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::BORDER))
                .title(" Legend ")
                .title_alignment(Alignment::Left),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}
