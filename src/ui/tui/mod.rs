// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Terminal User Interface module
//!
//! This module provides the TUI implementation using Ratatui.
//! It displays real-time telemetry data in a beautiful terminal interface.
//!
//! Supports two display modes:
//! - Normal mode: Traditional table view with real-time telemetry
//! - Visualization mode: Hardware-responsive starfield animation

use crate::animation::{ArcadeVisualization, HardwareStarfield, MemoryFlowVis, MemoryCastle};
use crate::backend::{factory, BackendConfig, TelemetryBackend};
use crate::cli::{BackendType, Cli};
use crate::error::TTTopError;
use crate::ui::colors;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
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
    /// Memory Castle mode (architectural memory hierarchy visualization)
    MemoryCastle,
    /// Memory Flow Topology (full-screen DRAM motion visualization)
    MemoryFlow,
    /// Arcade mode (unified visualization with all three modes + hero character)
    Arcade,
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
    // Note: Mouse capture disabled - we don't use mouse events and they were causing
    // performance issues (faster animation when mousing over the terminal)
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Disable stderr output to prevent log corruption in TUI
    crate::logging::disable_stderr();

    let backend_term = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend_term).map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Clear terminal before starting (helps with tmux/Terminal.app rendering)
    terminal.clear().map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Create app state and run
    let res = run_app(&mut terminal, &mut backend, backend_type, config, cli);

    // Re-enable stderr output before exiting
    crate::logging::enable_stderr();

    // Restore terminal
    disable_raw_mode().map_err(|e| TTTopError::Terminal(e.to_string()))?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen
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

    // UI refresh rate: 60 FPS for smooth animations and responsive input
    // This is independent of backend update rate
    let ui_poll_rate = Duration::from_millis(16); // ~60 FPS

    // Process monitoring (Linux-only, update every 2 seconds)
    #[cfg(feature = "linux-procfs")]
    let mut process_monitor = crate::workload::ProcessMonitor::new();
    #[cfg(feature = "linux-procfs")]
    let mut last_process_update = Instant::now();
    #[cfg(feature = "linux-procfs")]
    let process_update_interval = Duration::from_secs(2);

    // Detect if we're in tmux (helps with rendering issues on macOS Terminal.app)
    let _in_tmux = std::env::var("TMUX").is_ok() ||
                   std::env::var("TERM").unwrap_or_default().contains("screen");

    // UI state - initialize from CLI --mode option if provided
    let mut display_mode = if let Some(mode) = cli.mode {
        match mode {
            crate::cli::VisualizationMode::Normal => DisplayMode::Normal,
            crate::cli::VisualizationMode::Starfield => DisplayMode::Starfield,
            crate::cli::VisualizationMode::Castle => DisplayMode::MemoryCastle,
            crate::cli::VisualizationMode::Flow => DisplayMode::MemoryFlow,
            crate::cli::VisualizationMode::Arcade => DisplayMode::Arcade,
        }
    } else {
        DisplayMode::Normal
    };
    let mut starfield: Option<HardwareStarfield> = None;
    let mut memory_castle: Option<MemoryCastle> = None;
    let mut memory_flow: Option<MemoryFlowVis> = None;
    let mut arcade: Option<ArcadeVisualization> = None;
    let mut prev_display_mode = display_mode;

    loop {
        // Initialize or update visualizations
        let size = terminal.size().map_err(|e| TTTopError::Terminal(e.to_string()))?;

        match display_mode {
            DisplayMode::Starfield => {
                if starfield.is_none() {
                    let content_h = (size.height as usize).saturating_sub(8);
                    let content_w = (size.width as usize).saturating_sub(2);
                    let mut sf = HardwareStarfield::new(content_w, content_h);
                    sf.initialize_from_devices(backend.devices());
                    starfield = Some(sf);
                }
                if let Some(ref mut sf) = starfield {
                    sf.update_from_telemetry(backend);
                }
            }
            DisplayMode::MemoryCastle => {
                if memory_castle.is_none() {
                    // Create new MemoryCastle with random parameters
                    memory_castle = Some(MemoryCastle::new(size.width as usize, size.height as usize));
                }
                if let Some(ref mut tg) = memory_castle {
                    tg.update(backend);
                }
            }
            DisplayMode::MemoryFlow => {
                if memory_flow.is_none() {
                    // Create new MemoryFlow visualization
                    memory_flow = Some(MemoryFlowVis::new(size.width as usize, size.height as usize));
                }
                if let Some(ref mut mf) = memory_flow {
                    mf.update(backend);
                }
            }
            DisplayMode::Arcade => {
                if arcade.is_none() {
                    // Create new Arcade visualization
                    let mut arc = ArcadeVisualization::new(size.width as usize, size.height as usize);
                    arc.initialize_from_devices(backend.devices());
                    // Build board topology from SMBUS board_id data so the
                    // header diagram, stream characters, and castle separators
                    // are all topology-aware from the first frame.
                    arc.initialize_topology(backend);
                    arcade = Some(arc);
                }
                if let Some(ref mut arc) = arcade {
                    arc.update(backend);
                }
            }
            DisplayMode::Normal => {
                // Normal mode doesn't need special init
            }
        }

        // Clear terminal when switching modes to remove artifacts
        if display_mode != prev_display_mode {
            terminal.clear().ok();
            prev_display_mode = display_mode;
        }

        // Draw UI based on mode
        terminal
            .draw(|f| {
                // Clear frame with explicit black background for tmux compatibility
                f.render_widget(
                    Block::default().style(Style::default().bg(colors::rgb(0, 0, 0))),
                    f.area(),
                );

                match display_mode {
                    #[cfg(feature = "linux-procfs")]
                    DisplayMode::Normal => ui(f, backend, cli, &process_monitor),
                    #[cfg(not(feature = "linux-procfs"))]
                    DisplayMode::Normal => ui(f, backend, cli),
                    DisplayMode::Starfield => {
                        if let Some(ref sf) = starfield {
                            ui_visualization(f, sf, backend);
                        }
                    }
                    DisplayMode::MemoryCastle => {
                        if let Some(ref tg) = memory_castle {
                            ui_memory_castle(f, tg, backend);
                        }
                    }
                    DisplayMode::MemoryFlow => {
                        if let Some(ref mf) = memory_flow {
                            ui_memory_flow(f, mf, backend);
                        }
                    }
                    DisplayMode::Arcade => {
                        if let Some(ref arc) = arcade {
                            ui_arcade(f, arc, backend);
                        }
                    }
                }
            })
            .map_err(|e| TTTopError::Terminal(e.to_string()))?;

        // Handle input with fixed UI poll rate (60 FPS)
        // This provides smooth animations and responsive keyboard input
        // regardless of backend update interval
        if event::poll(ui_poll_rate).map_err(|e| TTTopError::Terminal(e.to_string()))? {
            match event::read().map_err(|e| TTTopError::Terminal(e.to_string()))? {
            Event::Resize(_, _) => {
                // Drop all size-dependent visualizations so they reinitialize
                // at the new dimensions on the next loop iteration.
                starfield = None;
                memory_castle = None;
                memory_flow = None;
                arcade = None;
                terminal.clear().ok();
            }
            Event::Key(key) => match key.code {
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
                        // Cycle through visualization modes
                        display_mode = match display_mode {
                            DisplayMode::Normal => DisplayMode::MemoryFlow,
                            DisplayMode::MemoryFlow => DisplayMode::Starfield,
                            DisplayMode::Starfield => {
                                // Randomize Memory Castle on each activation
                                memory_castle = None;
                                DisplayMode::MemoryCastle
                            }
                            DisplayMode::MemoryCastle => {
                                // Reset Arcade on each activation
                                arcade = None;
                                DisplayMode::Arcade
                            }
                            DisplayMode::Arcade => DisplayMode::Normal,
                        };
                        log::info!("Switched to {:?} mode", display_mode);
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        // Jump directly to Arcade mode
                        arcade = None; // Reset arcade to reinitialize
                        display_mode = DisplayMode::Arcade;
                        log::info!("Switched directly to Arcade mode");
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
                                if let DisplayMode::MemoryCastle = display_mode {
                                    memory_castle = None;
                                }
                                if let DisplayMode::MemoryFlow = display_mode {
                                    memory_flow = None;
                                }
                                if let DisplayMode::Arcade = display_mode {
                                    arcade = None;
                                }
                            }
                            Err(e) => {
                                log::error!("Failed to switch backend: {}", e);
                            }
                        }
                    }
                    _ => {}
                }
            _ => {}
            } // end match event::read()
        }

        // Update backend data
        if last_update.elapsed() >= update_interval {
            if let Err(e) = backend.update() {
                log::warn!("Update failed: {}", e);
            }
            last_update = Instant::now();
        }

        // Update process monitor (every 2 seconds to avoid overhead)
        #[cfg(feature = "linux-procfs")]
        if last_process_update.elapsed() >= process_update_interval {
            process_monitor.update();
            last_process_update = Instant::now();
        }
    }
}

/// Render the UI (with process monitoring on Linux)
#[cfg(feature = "linux-procfs")]
fn ui(
    f: &mut Frame,
    backend: &Box<dyn TelemetryBackend>,
    cli: &Cli,
    process_monitor: &crate::workload::ProcessMonitor,
) {
    // Adapt layout based on whether we have processes to display
    let chunks = if process_monitor.has_any_processes() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),      // Header
                Constraint::Min(8),         // Device list
                Constraint::Length(6),      // Process list (NEW)
                Constraint::Length(6),      // Messages
                Constraint::Length(3),      // Footer
            ])
            .split(f.area())
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),      // Header
                Constraint::Min(10),        // Device list (more space when no processes)
                Constraint::Length(6),      // Messages
                Constraint::Length(3),      // Footer
            ])
            .split(f.area())
    };

    // Render header
    render_header(f, chunks[0], backend);

    // Render device list
    render_devices(f, chunks[1], backend, cli);

    // Render process list if we have processes
    if process_monitor.has_any_processes() {
        render_processes(f, chunks[2], backend, process_monitor);
        render_messages(f, chunks[3]);
        render_footer(f, chunks[4], &backend.backend_info());
    } else {
        render_messages(f, chunks[2]);
        render_footer(f, chunks[3], &backend.backend_info());
    }
}

/// Render the UI (without process monitoring on non-Linux platforms)
#[cfg(not(feature = "linux-procfs"))]
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
                .fg(colors::rgb(102, 126, 234))  // Vibrant purple-blue
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " │ ",
            Style::default().fg(colors::rgb(118, 75, 162)),  // Deep purple
        ),
        Span::styled(
            format!("{} ", backend.backend_info()),
            Style::default()
                .fg(colors::rgb(56, 178, 172))  // Teal
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " │ ",
            Style::default().fg(colors::rgb(118, 75, 162)),
        ),
        Span::styled(
            format!("{} devices", backend.device_count()),
            Style::default()
                .fg(colors::rgb(56, 178, 172))
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(102, 126, 234))  // Vibrant purple-blue
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
        let (power_str, _power_style) = if let Some(t) = telem {
            let power = t.power_w();
            (
                format!("{:.1}W", power),
                Style::default().fg(colors::power_color(power)),
            )
        } else {
            ("N/A".to_string(), Style::default().fg(colors::TEXT_SECONDARY))
        };

        // Temperature
        let (temp_str, _temp_style) = if let Some(t) = telem {
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
        let (health_str, _health_style) = if let Some(s) = smbus {
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
                    .fg(colors::rgb(56, 178, 172))  // Teal borders
                    .add_modifier(Modifier::BOLD))
                .title(" ⚡ Hardware Telemetry ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(102, 126, 234))  // Purple-blue title
                    .add_modifier(Modifier::BOLD)),
        )
        .column_spacing(2);  // More spacing for readability

    f.render_widget(table, area);
}

/// Render process list showing which processes are using Tenstorrent devices
#[cfg(feature = "linux-procfs")]
fn render_processes(
    f: &mut Frame,
    area: Rect,
    backend: &Box<dyn TelemetryBackend>,
    process_monitor: &crate::workload::ProcessMonitor,
) {
    let mut process_lines = Vec::new();

    // Iterate through devices and show processes
    for device in backend.devices() {
        if let Some(processes) = process_monitor.get_processes_for_device(device.index) {
            // Device header
            let device_line = Line::from(vec![
                Span::styled(
                    format!("Device {}: ", device.index),
                    Style::default()
                        .fg(colors::rgb(120, 150, 255))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "{} process{}",
                        processes.len(),
                        if processes.len() == 1 { "" } else { "es" }
                    ),
                    Style::default().fg(colors::TEXT_SECONDARY),
                ),
            ]);
            process_lines.push(device_line);

            // Process list (show up to 2 per device to save space)
            for (i, proc) in processes.iter().take(2).enumerate() {
                let is_last = i == processes.len().min(2) - 1 && processes.len() <= 2;
                let prefix = if is_last { "└─" } else { "├─" };

                let proc_line = Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(prefix, Style::default().fg(colors::rgb(100, 100, 120))),
                    Span::styled(
                        format!(" {} ", proc.name),
                        Style::default()
                            .fg(colors::rgb(80, 220, 200))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("[{}] ", proc.pid),
                        Style::default().fg(colors::TEXT_SECONDARY),
                    ),
                    Span::styled(
                        &proc.cmdline,
                        Style::default().fg(colors::rgb(200, 200, 220)),
                    ),
                ]);
                process_lines.push(proc_line);

                // Show hugepages info if present
                if proc.hugepages_1g > 0 || proc.hugepages_2m > 0 {
                    let hugepages_str = if proc.hugepages_1g > 0 && proc.hugepages_2m > 0 {
                        format!(
                            "(hugepages: {} x 1GB, {} x 2MB)",
                            proc.hugepages_1g, proc.hugepages_2m
                        )
                    } else if proc.hugepages_1g > 0 {
                        format!("(hugepages: {} x 1GB)", proc.hugepages_1g)
                    } else {
                        format!("(hugepages: {} x 2MB)", proc.hugepages_2m)
                    };

                    let hugepages_line = Line::from(vec![
                        Span::styled("     ", Style::default()),
                        Span::styled(
                            hugepages_str,
                            Style::default()
                                .fg(colors::rgb(150, 120, 180))
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]);
                    process_lines.push(hugepages_line);
                }
            }

            // Show "and N more" if there are more processes
            if processes.len() > 2 {
                let more_line = Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(
                        format!("└─ and {} more...", processes.len() - 2),
                        Style::default()
                            .fg(colors::TEXT_SECONDARY)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]);
                process_lines.push(more_line);
            }
        }
    }

    // Show shared processes (using hugepages but no specific device file)
    let shared = process_monitor.get_shared_processes();
    if !shared.is_empty() {
        let shared_line = Line::from(vec![
            Span::styled(
                "Shared: ",
                Style::default()
                    .fg(colors::rgb(150, 120, 180))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{} process{}",
                    shared.len(),
                    if shared.len() == 1 { "" } else { "es" }
                ),
                Style::default().fg(colors::TEXT_SECONDARY),
            ),
        ]);
        process_lines.push(shared_line);

        for (i, proc) in shared.iter().take(2).enumerate() {
            let is_last = i == shared.len().min(2) - 1 && shared.len() <= 2;
            let prefix = if is_last { "└─" } else { "├─" };

            let proc_line = Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(prefix, Style::default().fg(colors::rgb(100, 100, 120))),
                Span::styled(
                    format!(" {} ", proc.name),
                    Style::default()
                        .fg(colors::rgb(150, 120, 180))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{}] ", proc.pid),
                    Style::default().fg(colors::TEXT_SECONDARY),
                ),
                Span::styled(&proc.cmdline, Style::default().fg(colors::rgb(200, 200, 220))),
            ]);
            process_lines.push(proc_line);

            // Show hugepages info
            if proc.hugepages_1g > 0 || proc.hugepages_2m > 0 {
                let hugepages_str = if proc.hugepages_1g > 0 && proc.hugepages_2m > 0 {
                    format!(
                        "(hugepages: {} x 1GB, {} x 2MB)",
                        proc.hugepages_1g, proc.hugepages_2m
                    )
                } else if proc.hugepages_1g > 0 {
                    format!("(hugepages: {} x 1GB)", proc.hugepages_1g)
                } else {
                    format!("(hugepages: {} x 2MB)", proc.hugepages_2m)
                };

                let hugepages_line = Line::from(vec![
                    Span::styled("     ", Style::default()),
                    Span::styled(
                        hugepages_str,
                        Style::default()
                            .fg(colors::rgb(150, 120, 180))
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]);
                process_lines.push(hugepages_line);
            }
        }

        if shared.len() > 2 {
            let more_line = Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled(
                    format!("└─ and {} more...", shared.len() - 2),
                    Style::default()
                        .fg(colors::TEXT_SECONDARY)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]);
            process_lines.push(more_line);
        }
    }

    // Render as paragraph
    let paragraph = Paragraph::new(process_lines)
        .block(
            Block::default()
                .title(" 🔧 Hardware Usage ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(255, 200, 100))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .style(Style::default().fg(colors::TEXT_PRIMARY));

    f.render_widget(paragraph, area);
}

/// Render footer with keyboard shortcuts and backend info
fn render_footer(f: &mut Frame, area: Rect, backend_info: &str) {
    let footer_text = vec![Line::from(vec![
        Span::styled(" q ", Style::default()
            .fg(colors::rgb(255, 100, 100))  // Bright red
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" quit  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(colors::rgb(150, 120, 180))),  // Purple separator
        Span::styled(" r ", Style::default()
            .fg(colors::rgb(100, 180, 255))  // Bright blue
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" refresh  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(colors::rgb(150, 120, 180))),  // Purple separator
        Span::styled(" v ", Style::default()
            .fg(colors::rgb(80, 220, 200))  // Bright teal
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" visualize  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(colors::rgb(150, 120, 180))),  // Purple separator
        Span::styled(" A ", Style::default()
            .fg(colors::rgb(255, 100, 255))  // Bright magenta
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" arcade  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(colors::rgb(150, 120, 180))),  // Purple separator
        Span::styled(" b ", Style::default()
            .fg(colors::rgb(150, 220, 100))  // Bright green
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" backend  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(colors::rgb(150, 120, 180))),  // Purple separator
        Span::styled(" ESC ", Style::default()
            .fg(colors::rgb(255, 200, 100))  // Bright orange
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" exit", Style::default().fg(colors::rgb(160, 160, 160))),
    ])];

    let title = format!(" ⌨  Keyboard Controls │ Backend: {} ", backend_info);

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)  // Rounded borders
                .border_style(Style::default()
                    .fg(colors::rgb(102, 126, 234))  // Purple-blue borders
                    .add_modifier(Modifier::BOLD))
                .title(title)
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(56, 178, 172))  // Teal title
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
            log::Level::Error => colors::rgb(255, 100, 100),   // Bright red
            log::Level::Warn => colors::rgb(255, 180, 100),    // Bright orange
            log::Level::Info => colors::rgb(100, 180, 255),    // Bright blue
            log::Level::Debug => colors::rgb(150, 150, 150),   // Gray
            log::Level::Trace => colors::rgb(100, 100, 100),   // Dim gray
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("[{}] ", msg.timestamp),
                Style::default().fg(colors::rgb(160, 160, 160)),
            ),
            Span::styled(
                format!("{:5} ", msg.level.to_string()),
                Style::default().fg(level_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &msg.message,
                Style::default().fg(colors::rgb(220, 220, 220)),
            ),
        ]));
    }

    // Show helpful text if no messages
    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "No log messages yet",
                Style::default().fg(colors::rgb(100, 100, 100)).add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    let messages_widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(80, 220, 200))  // Teal borders
                    .add_modifier(Modifier::BOLD))
                .title(" 📋 Recent Messages ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(102, 126, 234))  // Purple-blue title
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
        .style(Style::default().bg(colors::rgb(0, 0, 0)))  // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(100, 200, 255))  // Bright cyan
                    .add_modifier(Modifier::BOLD))
                .title(" ✧ Hardware-Responsive Starfield ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(150, 220, 255))
                    .add_modifier(Modifier::BOLD))
                .style(Style::default().bg(colors::rgb(0, 0, 0))),  // Transparent block background
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
            " 🌌 STARFIELD ",
            Style::default()
                .fg(colors::rgb(150, 220, 255))  // Bright cyan
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", backend.backend_info()),
            Style::default().fg(colors::rgb(200, 200, 220)),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", status),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(100, 200, 255))
                    .add_modifier(Modifier::BOLD))
                .title(" ✧ Visualization Mode ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(255, 200, 100))  // Orange
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render visualization mode footer with legend
fn render_visualization_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("Stars: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("Tensix Cores ", Style::default().fg(colors::rgb(100, 200, 255))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("Planets: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("Memory (L1/L2/DDR) ", Style::default().fg(colors::rgb(255, 200, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("Color: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("Temperature ", Style::default().fg(colors::rgb(255, 100, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" v ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("cycle", Style::default().fg(colors::rgb(160, 160, 160))),
    ])];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(100, 100, 120)))
                .title(" ⌨  Controls ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(150, 120, 180))
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render Memory Castle mode (full-screen architectural memory hierarchy)
fn ui_memory_castle(
    f: &mut Frame,
    memory_castle: &MemoryCastle,
    backend: &Box<dyn TelemetryBackend>,
) {
    // Create layout with header and content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(0),     // Memory Castle content
            Constraint::Length(3),  // Footer
        ])
        .split(f.area());

    // Render Memory Castle header
    render_memory_castle_header(f, chunks[0], backend);

    // Render Memory Castle content
    let castle_lines = memory_castle.render(backend);

    let castle_widget = Paragraph::new(castle_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))  // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(255, 150, 200))  // Bright pink
                    .add_modifier(Modifier::BOLD))
                .title(" 🏰 Memory Castle - Hardware Memory Hierarchy ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(255, 180, 220))
                    .add_modifier(Modifier::BOLD))
                .style(Style::default().bg(colors::rgb(0, 0, 0))),  // Transparent block background
        );

    f.render_widget(castle_widget, chunks[1]);

    // Render Memory Castle footer
    render_memory_castle_footer(f, chunks[2]);
}

/// Render Memory Flow visualization (full-screen DRAM motion)
fn ui_memory_flow(
    f: &mut Frame,
    memory_flow: &MemoryFlowVis,
    backend: &Box<dyn TelemetryBackend>,
) {
    // Create layout with header, content, and footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(0),     // Flow content
            Constraint::Length(3),  // Footer
        ])
        .split(f.area());

    // Render Memory Flow header
    render_memory_flow_header(f, chunks[0], backend);

    // Render Memory Flow content
    let flow_lines = memory_flow.render(backend);

    let flow_widget = Paragraph::new(flow_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))  // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(150, 255, 150))  // Bright green
                    .add_modifier(Modifier::BOLD))
                .title(" 🌊 Memory Flow - NoC & DDR Activity ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(180, 255, 180))
                    .add_modifier(Modifier::BOLD))
                .style(Style::default().bg(colors::rgb(0, 0, 0))),  // Transparent block background
        );

    f.render_widget(flow_widget, chunks[1]);

    // Render Memory Flow footer
    render_memory_flow_footer(f, chunks[2]);
}

/// Render Memory Castle mode header with device info
fn render_memory_castle_header<B: TelemetryBackend>(
    f: &mut Frame,
    area: Rect,
    backend: &B,
) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            " 🏰 MEMORY CASTLE ",
            Style::default()
                .fg(colors::rgb(255, 180, 220))  // Bright pink
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", backend.backend_info()),
            Style::default().fg(colors::rgb(200, 200, 220)),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} devices ", backend.device_count()),
            Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(255, 150, 200))
                    .add_modifier(Modifier::BOLD))
                .title(" 🏰 Memory Hierarchy Visualization ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(255, 200, 100))
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render Memory Castle mode footer with controls
fn render_memory_castle_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("Particles: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("○◉ Read □■ Write ◇◆ Hit ●⬤ Miss ", Style::default().fg(colors::rgb(255, 180, 220))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("Layers: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("DDR→L2→L1→Tensix ", Style::default().fg(colors::rgb(255, 200, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" v ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("cycle", Style::default().fg(colors::rgb(160, 160, 160))),
    ])];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(100, 100, 120)))
                .title(" ⌨  Controls ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(150, 120, 180))
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render Memory Flow mode header with device info
fn render_memory_flow_header<B: TelemetryBackend>(
    f: &mut Frame,
    area: Rect,
    backend: &B,
) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            " 🌊 MEMORY FLOW ",
            Style::default()
                .fg(colors::rgb(180, 255, 180))  // Bright green
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", backend.backend_info()),
            Style::default().fg(colors::rgb(200, 200, 220)),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} devices ", backend.device_count()),
            Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(150, 255, 150))
                    .add_modifier(Modifier::BOLD))
                .title(" 🌊 NoC & DDR Visualization ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(255, 200, 100))
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render Memory Flow mode footer with controls
fn render_memory_flow_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("Flow: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("NoC Particles ", Style::default().fg(colors::rgb(180, 255, 180))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("DDR: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("Channel Activity ", Style::default().fg(colors::rgb(255, 200, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("Color: ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled("Temperature + Traffic ", Style::default().fg(colors::rgb(255, 100, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" v ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("cycle", Style::default().fg(colors::rgb(160, 160, 160))),
    ])];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(100, 100, 120)))
                .title(" ⌨  Controls ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(150, 120, 180))
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render Arcade mode with btop++-inspired layout
fn ui_arcade(
    f: &mut Frame,
    arcade: &ArcadeVisualization,
    backend: &Box<dyn TelemetryBackend>,
) {
    // Create main layout: Header (4 lines incl. topology diagram) | Content | Footer.
    // Guard: keep header at 3 when device_count < 2 (no topology row needed).
    let header_height = if backend.devices().len() >= 2 { 4 } else { 3 };
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // Header
            Constraint::Min(10),               // Content
            Constraint::Length(3),             // Footer
        ])
        .split(f.area());

    // Render header (passes arcade for topology diagram)
    render_arcade_header(f, main_chunks[0], arcade, backend);

    // Split content: Top (Starfield) | Bottom (Castle+Flow + Table)
    let content_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),  // Starfield (top 40%)
            Constraint::Percentage(60),  // Bottom area
        ])
        .split(main_chunks[1]);

    // Render starfield (full width)
    render_arcade_starfield(f, content_chunks[0], arcade, backend);

    // Split bottom: Castle (top) | Flow (bottom) - both full width
    let viz_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),  // Memory Castle
            Constraint::Percentage(50),  // Memory Flow
        ])
        .split(content_chunks[1]);

    // Render visualizations (full width now!)
    render_arcade_castle(f, viz_chunks[0], arcade, backend);
    render_arcade_flow(f, viz_chunks[1], arcade, backend);

    // Render footer
    render_arcade_footer(f, main_chunks[2], backend);
}

/// Render Arcade mode header
///
/// When `arcade` has topology set (device_count ≥ 2), the header block gets an
/// extra line with the topology diagram: `[BH0 ██░ 16W 43°C] ←→ [BH1 …] ═══ [BH2 …]`
fn render_arcade_header(f: &mut Frame, area: Rect, arcade: &ArcadeVisualization, backend: &Box<dyn TelemetryBackend>) {
    let device_count = backend.devices().len();

    let mut header_text = vec![Line::from(vec![
        Span::styled(
            " 🎮 ARCADE MODE ",
            Style::default()
                .fg(colors::rgb(255, 100, 255))  // Bright magenta (btop++ style)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} Device{} ", device_count, if device_count == 1 { "" } else { "s" }),
            Style::default().fg(colors::rgb(100, 220, 255)),  // Bright cyan
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", backend.backend_info()),
            Style::default().fg(colors::rgb(150, 220, 100)),  // Bright green
        ),
    ])];

    // Topology diagram — only when ≥ 2 devices.
    if device_count >= 2 {
        if let Some(diagram) = arcade.topology_diagram_line(backend) {
            header_text.push(diagram);
        }
    }

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default()
                    .fg(colors::rgb(100, 150, 255))  // Bright blue border
                    .add_modifier(Modifier::BOLD))
                .title(" ⚡ Enhanced Layout ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(255, 200, 100))  // Orange title
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Left);

    f.render_widget(header, area);
}

/// Render starfield section
fn render_arcade_starfield(
    f: &mut Frame,
    area: Rect,
    arcade: &ArcadeVisualization,
    _backend: &Box<dyn TelemetryBackend>,
) {
    let starfield_lines = arcade.starfield.render();

    let starfield_widget = Paragraph::new(starfield_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))  // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::rgb(100, 200, 255)))  // Cyan
                .title(" ✧ STARFIELD ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(150, 220, 255))
                    .add_modifier(Modifier::BOLD))
                .style(Style::default().bg(colors::rgb(0, 0, 0))),  // Transparent block background
        );

    f.render_widget(starfield_widget, area);
}

/// Render memory castle section
fn render_arcade_castle(
    f: &mut Frame,
    area: Rect,
    arcade: &ArcadeVisualization,
    backend: &Box<dyn TelemetryBackend>,
) {
    let castle_lines = arcade.memory_castle.render(backend);

    let castle_widget = Paragraph::new(castle_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))  // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::rgb(255, 150, 200)))  // Pink
                .title(" 🏰 MEMORY CASTLE ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(255, 180, 220))
                    .add_modifier(Modifier::BOLD))
                .style(Style::default().bg(colors::rgb(0, 0, 0))),  // Transparent block background
        );

    f.render_widget(castle_widget, area);
}

/// Render memory flow section
fn render_arcade_flow(
    f: &mut Frame,
    area: Rect,
    arcade: &ArcadeVisualization,
    backend: &Box<dyn TelemetryBackend>,
) {
    let flow_lines = arcade.memory_flow.render(backend);

    let flow_widget = Paragraph::new(flow_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))  // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::rgb(150, 255, 150)))  // Green
                .title(" 🌊 MEMORY FLOW ")
                .title_alignment(Alignment::Center)
                .title_style(Style::default()
                    .fg(colors::rgb(180, 255, 180))
                    .add_modifier(Modifier::BOLD))
                .style(Style::default().bg(colors::rgb(0, 0, 0))),  // Transparent block background
        );

    f.render_widget(flow_widget, area);
}

/// Render device table (standard toplike display)
/// Currently unused - kept for potential future use
#[allow(dead_code)]
fn render_arcade_devices(f: &mut Frame, area: Rect, backend: &Box<dyn TelemetryBackend>) {
    let devices = backend.devices();

    // Build device rows
    let mut rows = Vec::new();
    for device in devices {
        if let Some(telemetry) = backend.telemetry(device.index) {
            let power = telemetry.power.unwrap_or(0.0);
            let temp = telemetry.asic_temperature.unwrap_or(0.0);
            let current = telemetry.current.unwrap_or(0.0);
            let voltage = telemetry.voltage.unwrap_or(0.0);
            let aiclk = telemetry.aiclk.unwrap_or(0);

            rows.push(Row::new(vec![
                format!("{}", device.index),
                format!("{:?}", device.architecture).chars().take(2).collect::<String>(),  // GS/WH/BH
                format!("{:.1}W", power),
                format!("{:.0}°C", temp),
                format!("{:.1}A", current),
                format!("{:.2}V", voltage),
                format!("{}MHz", aiclk),
            ])
            .style(Style::default().fg(colors::rgb(200, 200, 220)))
            .height(1));
        }
    }

    // Create table
    let table = Table::new(
        rows,
        [
            Constraint::Length(3),   // ID
            Constraint::Length(3),   // Arch
            Constraint::Length(8),   // Power
            Constraint::Length(7),   // Temp
            Constraint::Length(7),   // Current
            Constraint::Length(7),   // Voltage
            Constraint::Length(8),   // AICLK
        ],
    )
    .header(
        Row::new(vec!["ID", "Arc", "Power", "Temp", "Curr", "Volt", "AICLK"])
            .style(Style::default()
                .fg(colors::rgb(150, 220, 255))
                .add_modifier(Modifier::BOLD))
            .height(1),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default()
                .fg(colors::rgb(255, 200, 100))  // Orange
                .add_modifier(Modifier::BOLD))
            .title(" 📊 DEVICES ")
            .title_alignment(Alignment::Center)
            .title_style(Style::default()
                .fg(colors::rgb(255, 220, 150))
                .add_modifier(Modifier::BOLD)),
    )
    .column_spacing(1);

    f.render_widget(table, area);
}

/// Render arcade footer
fn render_arcade_footer(f: &mut Frame, area: Rect, backend: &Box<dyn TelemetryBackend>) {
    // Get hero stats from first device
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

    let temp_hue = crate::animation::temp_to_hue(temp);
    let hero_color = crate::animation::hsv_to_rgb(temp_hue, 1.0, 1.0);

    let footer_text = vec![Line::from(vec![
        Span::styled(" A ", Style::default()
            .fg(colors::rgb(255, 100, 255))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" arcade ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" v ", Style::default()
            .fg(colors::rgb(100, 220, 255))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled(" cycle ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" │ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" Hero: ", Style::default().fg(colors::rgb(180, 180, 180))),
        Span::styled("@", Style::default()
            .fg(hero_color)
            .add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" │ P:{:.1}W T:{:.0}°C I:{:.1}A ", power, temp, current),
            Style::default().fg(colors::rgb(150, 220, 200)),
        ),
    ])];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::rgb(100, 150, 255)))
                .title(" ⌨  Controls ")
                .title_alignment(Alignment::Left)
                .title_style(Style::default()
                    .fg(colors::rgb(150, 200, 255))
                    .add_modifier(Modifier::BOLD)),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}
