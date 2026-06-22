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

pub mod chip_portrait;

use crate::animation::{ArcadeVisualization, DefragVis, HardwareStarfield, MemoryCastle, MemoryFlowVis};
use crate::backend::{factory, BackendConfig, TelemetryBackend};
use crate::cli::{BackendType, Cli};
use crate::error::TTTopError;
use crate::ui::colors;
#[cfg(feature = "linux-procfs")]
use crate::workload::{InferenceEngine, InferenceServerProbe, ProcessMonitor, ServingMetrics};
use crossterm::{
    event::{self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
#[cfg(feature = "linux-procfs")]
use libc;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

/// Pending kill confirmation state.
#[cfg(feature = "linux-procfs")]
#[derive(Debug, Clone)]
struct KillConfirmState {
    pid: i32,
    name: String,
    device_idx: usize,
}

/// UI display mode
#[derive(Debug, Clone, Copy, PartialEq)]
enum DisplayMode {
    /// Insights screen (human-readable inference per chip + process panel)
    Insights,
    /// Full-screen grid of chip portraits for all devices.
    Grid,
    /// Hardware-responsive starfield visualization
    Starfield,
    /// Memory Castle mode (architectural memory hierarchy visualization)
    MemoryCastle,
    /// Memory Flow Topology (full-screen DRAM motion visualization)
    MemoryFlow,
    /// Arcade mode (unified visualization with all three modes + hero character)
    Arcade,
    /// Defrag view — model-loading visualization (inspired by disk defragmenters)
    Defrag,
}

/// Floating overlay panel type — toggled by hotkeys, auto-dismissed on any other keypress.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OverlayPanel {
    /// Symbol/color legend for the current visualization.
    Legend,
    /// Quick reference card: all keys and commands.
    Help,
    /// Prose description of what the current mode is showing and why.
    Explain,
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

    let config = if cli.verbose {
        config.verbose()
    } else {
        config
    };

    // Create initial backend
    let backend_type = cli.effective_backend();
    let mut backend =
        factory::create_backend(backend_type, config.clone(), cli).map_err(TTTopError::Backend)?;

    log::info!("TUI started with {:?} backend", backend_type);

    // Setup terminal
    enable_raw_mode().map_err(|e| {
        TTTopError::Terminal(format!(
            "Failed to enable raw mode: {}. \n\
         This usually means the terminal is not properly configured.",
            e
        ))
    })?;
    let mut stdout = io::stdout();
    // Note: Mouse capture disabled - we don't use mouse events and they were causing
    // performance issues (faster animation when mousing over the terminal)
    execute!(stdout, EnterAlternateScreen, EnableFocusChange)
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Disable stderr output to prevent log corruption in TUI
    crate::logging::disable_stderr();

    let backend_term = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend_term).map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Clear terminal before starting (helps with tmux/Terminal.app rendering)
    terminal
        .clear()
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Create app state and run
    let res = run_app(&mut terminal, &mut backend, backend_type, config, cli);

    // Re-enable stderr output before exiting
    crate::logging::enable_stderr();

    // Restore terminal
    disable_raw_mode().map_err(|e| TTTopError::Terminal(e.to_string()))?;
    execute!(terminal.backend_mut(), DisableFocusChange, LeaveAlternateScreen)
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

    // Build animation config from CLI --profile flag, merged with any user config file overrides.
    // sensitivity is read at viz construction time and applied to the viz's internal AdaptiveBaseline.
    let anim_cfg = {
        use crate::config::{load_config_overrides, AnimConfig};
        AnimConfig::from_profile(cli.profile).merge(load_config_overrides())
    };

    // UI refresh rates — animation modes target 60 FPS, data modes ~10 FPS.
    // Input polling is always 16 ms so keystrokes are never delayed.
    // No focus throttle — tt-toplike is designed to run alongside other work.
    let mut ui_poll_rate_anim = Duration::from_millis(16); // ~60 FPS for animated modes
    let mut ui_poll_rate_data = Duration::from_millis(100); // ~10 FPS for data modes
    let mut last_anim_render = Instant::now();
    let mut last_data_render = Instant::now();

    // Process monitoring (Linux-only, update every 2 seconds)
    #[cfg(feature = "linux-procfs")]
    let mut process_monitor = crate::workload::ProcessMonitor::new();
    #[cfg(feature = "linux-procfs")]
    let mut last_process_update = Instant::now();
    #[cfg(feature = "linux-procfs")]
    let process_update_interval = Duration::from_secs(2);

    // Host CPU / RAM monitoring — sampled alongside the process monitor.
    // sysinfo needs two calls separated in time to compute meaningful CPU%;
    // we call it twice at init (with a tiny sleep) so the first frame shows
    // real data rather than zeros.
    #[cfg(feature = "linux-procfs")]
    let (mut sys_monitor, mut host_cpu_pct, mut host_mem_used, mut host_mem_total) = {
        use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
        let mut s = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        );
        s.refresh_cpu_usage();
        s.refresh_memory();
        // A brief pause lets the kernel accumulate CPU ticks for a real sample.
        std::thread::sleep(Duration::from_millis(200));
        s.refresh_cpu_usage();
        s.refresh_memory();
        let cpu = s.global_cpu_usage();
        let used = s.used_memory();
        let tot = s.total_memory();
        (s, cpu, used, tot)
    };

    // UI state - initialize from CLI --mode option if provided
    let mut display_mode = if let Some(mode) = cli.mode {
        match mode {
            crate::cli::VisualizationMode::Normal => DisplayMode::Insights,
            crate::cli::VisualizationMode::Starfield => DisplayMode::Starfield,
            crate::cli::VisualizationMode::Castle => DisplayMode::MemoryCastle,
            crate::cli::VisualizationMode::Flow => DisplayMode::MemoryFlow,
            crate::cli::VisualizationMode::Arcade => DisplayMode::Arcade,
        }
    } else {
        DisplayMode::Insights // default is now Insights
    };
    let mut starfield: Option<HardwareStarfield> = None;
    let mut memory_castle: Option<MemoryCastle> = None;
    let mut memory_flow: Option<MemoryFlowVis> = None;
    let mut arcade: Option<ArcadeVisualization> = None;
    let mut defrag: Option<DefragVis> = None;
    let mut prev_display_mode = display_mode;

    // ── Slash command state ────────────────────────────────────────────────
    // Press '/' from any mode to open the command bar at the bottom of the
    // screen.  Type a command, press Enter to execute, Esc to cancel.
    // The command bar is rendered as an overlay on top of the current view
    // so it works from every mode without mode-specific plumbing.
    let mut cmd_mode = false;
    let mut cmd_buf = String::new();
    let mut cmd_message: Option<(String, bool)> = None; // (text, is_error)

    // ── Floating overlay panel state ──────────────────────────────────────
    // Toggled by l (legend), ? (help), ! (explain).
    // Auto-dismissed on any other keypress — stays until explicit re-toggle.
    let mut overlay: Option<OverlayPanel> = None;

    // Portrait particle state — per-device particle list keyed by device index,
    // a shared AdaptiveBaseline (one entry per device), and a monotonic tick counter.
    // Particles are ticked on every Insights-mode draw frame so the animation runs
    // at the full UI rate (~60 FPS) independently of the backend update interval.
    let mut portrait_particles: std::collections::HashMap<
        usize,
        Vec<crate::ui::tui::chip_portrait::Particle>,
    > = std::collections::HashMap::new();
    let mut portrait_baseline = crate::animation::baseline::AdaptiveBaseline::new();
    portrait_baseline.sensitivity = anim_cfg.sensitivity;
    let mut portrait_tick: u64 = 0;

    // Insights mode state
    #[cfg(feature = "linux-procfs")]
    let mut inference_engine = InferenceEngine::new();
    #[cfg(feature = "linux-procfs")]
    let mut inference_probe = InferenceServerProbe::new();
    #[cfg(feature = "linux-procfs")]
    let mut serving_metrics: std::collections::HashMap<i32, ServingMetrics> =
        std::collections::HashMap::new();
    #[cfg(feature = "linux-procfs")]
    let mut process_cursor: usize = 0;
    #[cfg(feature = "linux-procfs")]
    let mut kill_confirm: Option<KillConfirmState> = None;

    // Fleet navigation state (Insights with 32+ devices).
    // fleet_cursor: which device cell is highlighted in the galaxy overview.
    // fleet_zoom_start: when Some(n), show portrait grid for devices [n..n+PAGE_SIZE]
    //                   instead of the compact fleet map.
    let mut fleet_cursor: usize = 0;
    let mut fleet_zoom_start: Option<usize> = None;

    loop {
        // Decide whether to advance animations this iteration.
        // nvtop-style trick: input always polls at INPUT_POLL_MS (16 ms) so
        // keystrokes are never delayed.  Viz updates are gated separately so
        // animation modes tick at their target FPS while data modes tick slower.
        // Defrag uses 60 FPS only when devices are active (Running/DMA/Deconstructing);
        // in Idle/Init it drops to the data rate to avoid wasting CPU.
        let defrag_is_animated = defrag.as_ref().map(|d| d.is_animated()).unwrap_or(false);
        let is_anim_mode = matches!(
            display_mode,
            DisplayMode::Starfield
                | DisplayMode::MemoryCastle
                | DisplayMode::MemoryFlow
                | DisplayMode::Arcade
        ) || (display_mode == DisplayMode::Defrag && defrag_is_animated);
        let render_interval = if is_anim_mode {
            ui_poll_rate_anim
        } else {
            ui_poll_rate_data
        };
        let should_tick = if is_anim_mode {
            last_anim_render.elapsed() >= render_interval
        } else {
            last_data_render.elapsed() >= render_interval
        };
        if should_tick {
            if is_anim_mode { last_anim_render = Instant::now(); }
            else { last_data_render = Instant::now(); }
        }

        // Initialize or update visualizations (only on tick to match target FPS)
        let size = terminal
            .size()
            .map_err(|e| TTTopError::Terminal(e.to_string()))?;

        if should_tick {
        match display_mode {
            DisplayMode::Starfield => {
                if starfield.is_none() {
                    let content_h = (size.height as usize).saturating_sub(8);
                    let content_w = (size.width as usize).saturating_sub(2);
                    let mut sf = HardwareStarfield::new(content_w, content_h);
                    sf.initialize_from_devices(backend.devices());
                    sf.set_sensitivity(anim_cfg.sensitivity);
                    starfield = Some(sf);
                }
                if let Some(ref mut sf) = starfield {
                    sf.update_from_telemetry(backend);
                }
            }
            DisplayMode::MemoryCastle => {
                if memory_castle.is_none() {
                    // Create new MemoryCastle with density scaled by animation profile
                    let glyph_count = ((30.0 * anim_cfg.max_particles_scale) as usize).max(5);
                    let mut mc = MemoryCastle::new_with_density(
                        size.width as usize,
                        size.height as usize,
                        anim_cfg.castle_max_particles(),
                        glyph_count,
                    );
                    mc.set_sensitivity(anim_cfg.sensitivity);
                    memory_castle = Some(mc);
                }
                if let Some(ref mut tg) = memory_castle {
                    tg.update(backend);
                }
            }
            DisplayMode::MemoryFlow => {
                if memory_flow.is_none() {
                    // Create new MemoryFlow visualization with density scaled by animation profile
                    let mut mf = MemoryFlowVis::new_with_density(
                        size.width as usize,
                        size.height as usize,
                        anim_cfg.flow_max_particles(),
                    );
                    mf.set_sensitivity(anim_cfg.sensitivity);
                    memory_flow = Some(mf);
                }
                if let Some(ref mut mf) = memory_flow {
                    mf.update(backend);
                }
            }
            DisplayMode::Arcade => {
                // Re-create if size changed — arcade pre-allocates at construction size.
                if let Some(ref arc) = arcade {
                    if arc.width() != size.width as usize || arc.height() != size.height as usize {
                        arcade = None;
                    }
                }
                if arcade.is_none() {
                    // Create new Arcade visualization
                    let mut arc =
                        ArcadeVisualization::new(size.width as usize, size.height as usize);
                    arc.initialize_from_devices(backend.devices());
                    // Forward sensitivity so --profile paranoid (or any non-default profile)
                    // takes effect inside Arcade just like it does in the standalone modes.
                    arc.set_sensitivity(anim_cfg.sensitivity);
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
            DisplayMode::Insights => {
                // Ingestion now happens in the backend update block (runs at ~10 Hz,
                // not 60 FPS), so nothing to do here.
            }
            DisplayMode::Grid => {
                // Grid mode doesn't need special init
            }
            DisplayMode::Defrag => {
                if defrag.is_none() {
                    defrag = Some(DefragVis::new(
                        size.width as usize,
                        size.height as usize,
                    ));
                }
                if let Some(ref mut dv) = defrag {
                    dv.update(backend);
                }
            }
        }
        } // end if should_tick

        // Clear terminal when switching modes to remove artifacts
        if display_mode != prev_display_mode {
            terminal.clear().ok();
            prev_display_mode = display_mode;
        }

        // Tick portrait particles once per UI frame when on the Insights screen.
        // This must happen *before* the immutable borrow inside terminal.draw().
        if display_mode == DisplayMode::Insights {
            use crate::ui::tui::chip_portrait::tick_particles;
            portrait_tick = portrait_tick.wrapping_add(1);
            let devices_snapshot: Vec<_> = backend.devices().to_vec();
            for device in &devices_snapshot {
                let plist = portrait_particles.entry(device.index).or_default();
                if let Some(telem) = backend.telemetry(device.index) {
                    let smbus = backend.smbus_telemetry(device.index);
                    tick_particles(
                        plist,
                        telem,
                        smbus,
                        device.architecture,
                        &mut portrait_baseline,
                        device.index,
                        portrait_tick,
                        anim_cfg.portrait_max_particles(),
                    );
                }
            }
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
                    DisplayMode::Insights => {
                        #[cfg(feature = "linux-procfs")]
                        render_insights(
                            f,
                            backend,
                            &inference_engine,
                            &process_monitor,
                            process_cursor,
                            kill_confirm.as_ref(),
                            cli,
                            &portrait_particles,
                            &portrait_baseline,
                            fleet_cursor,
                            fleet_zoom_start,
                            host_cpu_pct,
                            host_mem_used,
                            host_mem_total,
                            &serving_metrics,
                        );
                        #[cfg(not(feature = "linux-procfs"))]
                        render_insights_no_procfs(
                            f,
                            backend,
                            &crate::workload::InferenceEngine::new(),
                        );
                    }
                    DisplayMode::Grid => {
                        render_grid_mode(f, backend);
                    }
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
                    DisplayMode::Defrag => {
                        if let Some(ref dv) = defrag {
                            ui_defrag(f, dv, backend);
                        }
                    }
                }

                // ── Command bar overlay (all modes) ───────────────────────
                if cmd_mode || cmd_message.is_some() {
                    let msg_ref = cmd_message.as_ref().map(|(s, e)| (s.as_str(), *e));
                    render_command_bar(f, &cmd_buf, msg_ref);
                }

                // ── Floating overlay panel (legend / help / explain) ───────
                if let Some(kind) = overlay {
                    render_overlay_panel(f, kind, display_mode);
                }
            })
            .map_err(|e| TTTopError::Terminal(e.to_string()))?;

        // Input always polls at 16 ms so keystrokes respond immediately.
        let input_poll = Duration::from_millis(16);
        if event::poll(input_poll).map_err(|e| TTTopError::Terminal(e.to_string()))? {
            match event::read().map_err(|e| TTTopError::Terminal(e.to_string()))? {
                Event::FocusGained | Event::FocusLost => {}
                Event::Resize(_, _) => {
                    // Drop all size-dependent visualizations so they reinitialize
                    // at the new dimensions on the next loop iteration.
                    starfield = None;
                    memory_castle = None;
                    memory_flow = None;
                    arcade = None;
                    defrag = None;
                    terminal.clear().ok();
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                // ── Command mode intercepts all keystrokes ─────────────────
                if cmd_mode {
                    match key.code {
                        KeyCode::Esc => {
                            cmd_mode = false;
                            cmd_buf.clear();
                            cmd_message = None;
                        }
                        KeyCode::Enter => {
                            let (msg, is_err) = execute_command(
                                &cmd_buf,
                                &mut display_mode,
                                &mut ui_poll_rate_anim,
                                &mut ui_poll_rate_data,
                                anim_cfg.sensitivity,
                                &mut overlay,
                            );
                            cmd_message = Some((msg, is_err));
                            cmd_mode = false;
                            cmd_buf.clear();
                        }
                        KeyCode::Backspace => { cmd_buf.pop(); }
                        KeyCode::Char(c) => { cmd_buf.push(c); }
                        _ => {}
                    }
                } else { match key.code {
                    // ── Overlay toggle hotkeys (l / ? / !) ─────────────────
                    KeyCode::Char('l') => {
                        overlay = if overlay == Some(OverlayPanel::Legend) { None } else { Some(OverlayPanel::Legend) };
                    }
                    KeyCode::Char('?') => {
                        overlay = if overlay == Some(OverlayPanel::Help) { None } else { Some(OverlayPanel::Help) };
                    }
                    KeyCode::Char('!') => {
                        overlay = if overlay == Some(OverlayPanel::Explain) { None } else { Some(OverlayPanel::Explain) };
                    }
                    KeyCode::Char('/') => {
                        // Open command bar — clears any previous result message
                        cmd_mode = true;
                        cmd_buf.clear();
                        cmd_message = None;
                        overlay = None; // dismiss overlay when entering command mode
                    }
                    KeyCode::Char('q') => {
                        return Ok(());
                    }
                    KeyCode::Esc => {
                        // Dismiss overlay, command message, zoom, kill confirm (in that order).
                        if overlay.is_some() {
                            overlay = None;
                        } else if cmd_message.is_some() {
                            cmd_message = None;
                        } else
                        if fleet_zoom_start.is_some() {
                            // Zoom out from portrait drill-down back to galaxy overview.
                            fleet_zoom_start = None;
                        } else {
                            #[cfg(feature = "linux-procfs")]
                            if kill_confirm.is_some() {
                                kill_confirm = None;
                            } else {
                                return Ok(());
                            }
                            #[cfg(not(feature = "linux-procfs"))]
                            return Ok(());
                        }
                    }
                    KeyCode::Char('r') => {
                        // Force refresh
                        if let Err(e) = backend.update() {
                            log::warn!("Update failed: {}", e);
                        }
                    }
                    KeyCode::Char('v') => {
                        // Cycle through visualization modes.
                        // Grid mode is intentionally excluded — the Insights screen
                        // already is the chip-portrait grid.
                        display_mode = match display_mode {
                            DisplayMode::Insights | DisplayMode::Grid => DisplayMode::MemoryFlow,
                            DisplayMode::MemoryFlow => DisplayMode::Starfield,
                            DisplayMode::Starfield => {
                                memory_castle = None;
                                DisplayMode::MemoryCastle
                            }
                            DisplayMode::MemoryCastle => {
                                arcade = None;
                                DisplayMode::Arcade
                            }
                            DisplayMode::Arcade => DisplayMode::Defrag,
                            DisplayMode::Defrag => DisplayMode::Insights,
                        };
                        log::info!("Switched to {:?} mode", display_mode);
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        // Jump directly to Defrag mode
                        defrag = None; // reinit at new frame
                        display_mode = DisplayMode::Defrag;
                        log::info!("Switched directly to Defrag mode");
                    }
                    KeyCode::Char('g') => {
                        display_mode = DisplayMode::Grid;
                        log::info!("Switched to Grid mode");
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
                                if let DisplayMode::Defrag = display_mode {
                                    defrag = None;
                                }
                            }
                            Err(e) => {
                                log::error!("Failed to switch backend: {}", e);
                            }
                        }
                    }
                    // Insights-mode navigation, fleet drill-down, and kill
                    KeyCode::Up if display_mode == DisplayMode::Insights => {
                        let n = backend.devices().len();
                        if n >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
                            // Move cursor up one row in the galaxy overview.
                            let cells_per_row = (size.width / FLEET_CELL_W).max(1) as usize;
                            fleet_cursor = fleet_cursor.saturating_sub(cells_per_row);
                        } else {
                            // Normal process-list navigation.
                            #[cfg(feature = "linux-procfs")]
                            if kill_confirm.is_none() {
                                process_cursor = process_cursor.saturating_sub(1);
                            }
                        }
                    }
                    KeyCode::Down if display_mode == DisplayMode::Insights => {
                        let n = backend.devices().len();
                        if n >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
                            let cells_per_row = (size.width / FLEET_CELL_W).max(1) as usize;
                            fleet_cursor = (fleet_cursor + cells_per_row).min(n.saturating_sub(1));
                        } else {
                            #[cfg(feature = "linux-procfs")]
                            if kill_confirm.is_none() {
                                let max =
                                    flat_process_list(&process_monitor).len().saturating_sub(1);
                                process_cursor = (process_cursor + 1).min(max);
                            }
                        }
                    }
                    KeyCode::Left if display_mode == DisplayMode::Insights => {
                        let n = backend.devices().len();
                        if n >= FLEET_DEVICE_THRESHOLD {
                            if let Some(start) = fleet_zoom_start {
                                // Page back in zoomed portrait view.
                                fleet_zoom_start = Some(start.saturating_sub(FLEET_PAGE_SIZE));
                                fleet_cursor = fleet_zoom_start.unwrap();
                            } else {
                                // Move cursor left one cell.
                                fleet_cursor = fleet_cursor.saturating_sub(1);
                            }
                        }
                    }
                    KeyCode::Right if display_mode == DisplayMode::Insights => {
                        let n = backend.devices().len();
                        if n >= FLEET_DEVICE_THRESHOLD {
                            if let Some(start) = fleet_zoom_start {
                                // Page forward in zoomed portrait view.
                                let new = (start + FLEET_PAGE_SIZE).min(n.saturating_sub(1));
                                fleet_zoom_start = Some(new);
                                fleet_cursor = new;
                            } else {
                                fleet_cursor = (fleet_cursor + 1).min(n.saturating_sub(1));
                            }
                        }
                    }
                    KeyCode::Enter if display_mode == DisplayMode::Insights => {
                        let n = backend.devices().len();
                        if n >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
                            // Zoom into portrait view for the page containing cursor.
                            let page_start = (fleet_cursor / FLEET_PAGE_SIZE) * FLEET_PAGE_SIZE;
                            fleet_zoom_start = Some(page_start);
                        } else {
                            // Kill-dialog confirmation.
                            #[cfg(feature = "linux-procfs")]
                            if let Some(ref kc) = kill_confirm {
                                let _ = crate::workload::process_monitor::kill_pid(
                                    kc.pid,
                                    libc::SIGTERM,
                                );
                                kill_confirm = None;
                            }
                        }
                    }
                    #[cfg(feature = "linux-procfs")]
                    KeyCode::Char('k') if display_mode == DisplayMode::Insights => {
                        if kill_confirm.is_none() {
                            if let Some(proc) =
                                flat_process_list(&process_monitor).get(process_cursor)
                            {
                                kill_confirm = Some(KillConfirmState {
                                    pid: proc.pid,
                                    name: proc.name.clone(),
                                    device_idx: proc.device_indices.first().copied().unwrap_or(0),
                                });
                            }
                        }
                    }
                    #[cfg(feature = "linux-procfs")]
                    KeyCode::Char('K') if display_mode == DisplayMode::Insights => {
                        if let Some(ref kc) = kill_confirm {
                            // Inside dialog: SIGKILL and close
                            let _ =
                                crate::workload::process_monitor::kill_pid(kc.pid, libc::SIGKILL);
                            kill_confirm = None;
                        } else if let Some(proc) =
                            flat_process_list(&process_monitor).get(process_cursor)
                        {
                            // Outside dialog: SIGKILL immediately
                            let _ =
                                crate::workload::process_monitor::kill_pid(proc.pid, libc::SIGKILL);
                        }
                    }
                    #[cfg(feature = "linux-procfs")]
                    KeyCode::Char('y') if display_mode == DisplayMode::Insights => {
                        if let Some(ref kc) = kill_confirm {
                            let _ =
                                crate::workload::process_monitor::kill_pid(kc.pid, libc::SIGTERM);
                            kill_confirm = None;
                        }
                    }
                    #[cfg(feature = "linux-procfs")]
                    KeyCode::Char('n') if display_mode == DisplayMode::Insights => {
                        kill_confirm = None;
                    }
                    _ => {
                        // Any unrecognized key dismisses the overlay (auto-hide).
                        overlay = None;
                    }
                } // end inner key match
                } // end else (not cmd_mode)
                } // end Event::Key
                _ => {}
            } // end match event::read()
        }

        // Update backend data
        if last_update.elapsed() >= update_interval {
            if let Err(e) = backend.update() {
                log::warn!("Update failed: {}", e);
            }
            last_update = Instant::now();

            // Ingest fresh telemetry into InferenceEngine — runs at backend rate (~10 Hz),
            // not at the UI render rate (60 FPS), so ARC stall counters don't oversaturate.
            #[cfg(feature = "linux-procfs")]
            for device in backend.devices() {
                use crate::workload::inference::parse_arc_health_counters;
                let idx = device.index;
                let power = backend.telemetry(idx).map(|t| t.power_w()).unwrap_or(0.0);
                let temp = backend.telemetry(idx).map(|t| t.temp_c()).unwrap_or(0.0);
                let current = backend.telemetry(idx).map(|t| t.current_a()).unwrap_or(0.0);
                let aiclk = backend.telemetry(idx).and_then(|t| t.aiclk);
                let arc_health = backend.smbus_telemetry(idx).map(|s| {
                    parse_arc_health_counters([
                        s.arc0_health.clone(),
                        s.arc1_health.clone(),
                        s.arc2_health.clone(),
                        s.arc3_health.clone(),
                    ])
                });
                inference_engine.ingest(idx, power, temp, current, aiclk, arc_health);
            }
        }

        // Update process monitor + host stats (every 2 seconds to avoid overhead)
        #[cfg(feature = "linux-procfs")]
        if last_process_update.elapsed() >= process_update_interval {
            process_monitor.update();
            // Probe inference servers — runs at same 2s cadence to avoid hammering
            // HTTP endpoints on every backend tick.
            let flat = flat_process_list(&process_monitor);
            serving_metrics = inference_probe.update(&flat);
            sys_monitor.refresh_cpu_usage();
            sys_monitor.refresh_memory();
            host_cpu_pct = sys_monitor.global_cpu_usage();
            host_mem_used = sys_monitor.used_memory();
            host_mem_total = sys_monitor.total_memory();
            last_process_update = Instant::now();
        }
    }
}

/// Render header with app title and status
fn render_header(f: &mut Frame, area: Rect, backend: &dyn TelemetryBackend) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            "🦀 TT-TOPLIKE-RS ",
            Style::default()
                .fg(colors::rgb(102, 126, 234)) // Vibrant purple-blue
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " │ ",
            Style::default().fg(colors::rgb(118, 75, 162)), // Deep purple
        ),
        Span::styled(
            format!("{} ", backend.backend_info()),
            Style::default()
                .fg(colors::rgb(56, 178, 172)) // Teal
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(118, 75, 162))),
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
                .border_style(
                    Style::default()
                        .fg(colors::rgb(102, 126, 234)) // Vibrant purple-blue
                        .add_modifier(Modifier::BOLD),
                )
                .title(" ⚡ Real-Time Hardware Monitoring ⚡ ")
                .title_alignment(Alignment::Center),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render visualization mode (full-screen starfield)
fn ui_visualization(f: &mut Frame, starfield: &HardwareStarfield, backend: &dyn TelemetryBackend) {
    // Create layout with header and content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Starfield content
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Render visualization header
    render_visualization_header(f, chunks[0], starfield, backend);

    // Render starfield content
    let starfield_lines = starfield.render();

    let starfield_widget = Paragraph::new(starfield_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0))) // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(100, 200, 255)) // Bright cyan
                        .add_modifier(Modifier::BOLD),
                )
                .title(" ✧ Hardware-Responsive Starfield ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(150, 220, 255))
                        .add_modifier(Modifier::BOLD),
                )
                .style(Style::default().bg(colors::rgb(0, 0, 0))), // Transparent block background
        );

    f.render_widget(starfield_widget, chunks[1]);

    // Render visualization footer
    render_visualization_footer(f, chunks[2]);
}

/// Render visualization mode header with baseline status
fn render_visualization_header(
    f: &mut Frame,
    area: Rect,
    starfield: &HardwareStarfield,
    backend: &dyn TelemetryBackend,
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
                .fg(colors::rgb(150, 220, 255)) // Bright cyan
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
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(100, 200, 255))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" ✧ Visualization Mode ")
                .title_alignment(Alignment::Left)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 200, 100)) // Orange
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render visualization mode footer with legend
fn render_visualization_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("stars=cores ", Style::default().fg(colors::rgb(100, 200, 255))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("○ planets=mem ", Style::default().fg(colors::rgb(255, 200, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("color=temp ", Style::default().fg(colors::rgb(255, 100, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" v ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("cycle  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" l ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("legend  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" ? ", Style::default().fg(colors::rgb(220, 180, 80)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("help  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" ! ", Style::default().fg(colors::rgb(180, 140, 255)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("explain", Style::default().fg(colors::rgb(160, 160, 160))),
    ])];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::rgb(100, 100, 120)))
                .title(" ⌨  Controls ")
                .title_alignment(Alignment::Left)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(150, 120, 180))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render Memory Castle mode (full-screen architectural memory hierarchy)
fn ui_memory_castle(f: &mut Frame, memory_castle: &MemoryCastle, backend: &dyn TelemetryBackend) {
    // Create layout with header and content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Memory Castle content
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Render Memory Castle header
    render_memory_castle_header(f, chunks[0], backend);

    // Render Memory Castle content
    let castle_lines = memory_castle.render(backend);

    let castle_widget = Paragraph::new(castle_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0))) // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(255, 150, 200)) // Bright pink
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🏰 Memory Castle - Hardware Memory Hierarchy ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 180, 220))
                        .add_modifier(Modifier::BOLD),
                )
                .style(Style::default().bg(colors::rgb(0, 0, 0))), // Transparent block background
        );

    f.render_widget(castle_widget, chunks[1]);

    // Render Memory Castle footer
    render_memory_castle_footer(f, chunks[2]);
}

/// Render Memory Flow visualization (full-screen DRAM motion)
fn ui_memory_flow(f: &mut Frame, memory_flow: &MemoryFlowVis, backend: &dyn TelemetryBackend) {
    // Create layout with header, content, and footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Flow content
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Render Memory Flow header
    render_memory_flow_header(f, chunks[0], backend);

    // Render Memory Flow content
    let flow_lines = memory_flow.render(backend);

    let flow_widget = Paragraph::new(flow_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0))) // Transparent background for tmux
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(150, 255, 150)) // Bright green
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🌊 Memory Flow - NoC & DDR Activity ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(180, 255, 180))
                        .add_modifier(Modifier::BOLD),
                )
                .style(Style::default().bg(colors::rgb(0, 0, 0))), // Transparent block background
        );

    f.render_widget(flow_widget, chunks[1]);

    // Render Memory Flow footer
    render_memory_flow_footer(f, chunks[2]);
}

/// Render Memory Castle mode header with device info
fn render_memory_castle_header(f: &mut Frame, area: Rect, backend: &dyn TelemetryBackend) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            " 🏰 MEMORY CASTLE ",
            Style::default()
                .fg(colors::rgb(255, 180, 220)) // Bright pink
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
            Style::default()
                .fg(colors::rgb(80, 220, 200))
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(255, 150, 200))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🏰 Memory Hierarchy Visualization ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 200, 100))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render Memory Castle mode footer with controls
fn render_memory_castle_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("◦read ", Style::default().fg(colors::rgb(100, 200, 255))),
        Span::styled("·write ", Style::default().fg(colors::rgb(255, 180, 80))),
        Span::styled("◇hit ", Style::default().fg(colors::rgb(100, 255, 180))),
        Span::styled("•miss ", Style::default().fg(colors::rgb(255, 100, 150))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("DDR→L2→L1→Tensix ", Style::default().fg(colors::rgb(255, 200, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" v ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("cycle  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" l ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("legend  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" ? ", Style::default().fg(colors::rgb(220, 180, 80)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("help", Style::default().fg(colors::rgb(160, 160, 160))),
    ])];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::rgb(100, 100, 120)))
                .title(" ⌨  Controls ")
                .title_alignment(Alignment::Left)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(150, 120, 180))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render Memory Flow mode header with device info
fn render_memory_flow_header(f: &mut Frame, area: Rect, backend: &dyn TelemetryBackend) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            " 🌊 MEMORY FLOW ",
            Style::default()
                .fg(colors::rgb(180, 255, 180)) // Bright green
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
            Style::default()
                .fg(colors::rgb(80, 220, 200))
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(150, 255, 150))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🌊 NoC & DDR Visualization ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 200, 100))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render Memory Flow mode footer with controls
fn render_memory_flow_footer(f: &mut Frame, area: Rect) {
    let footer_text = vec![Line::from(vec![
        Span::styled("→ reads  ← writes ", Style::default().fg(colors::rgb(180, 255, 180))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled("speed=NoC  density=memW ", Style::default().fg(colors::rgb(255, 200, 100))),
        Span::styled("│ ", Style::default().fg(colors::rgb(100, 100, 120))),
        Span::styled(" v ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("cycle  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" l ", Style::default().fg(colors::rgb(80, 220, 200)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("legend  ", Style::default().fg(colors::rgb(160, 160, 160))),
        Span::styled(" ? ", Style::default().fg(colors::rgb(220, 180, 80)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::styled("help", Style::default().fg(colors::rgb(160, 160, 160))),
    ])];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(colors::rgb(100, 100, 120)))
                .title(" ⌨  Controls ")
                .title_alignment(Alignment::Left)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(150, 120, 180))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(footer, area);
}

/// Render Defrag mode — model-loading visualization.
///
/// Displays a grid of cells per device that fills from empty (░) → loading (▒▓)
/// → resident (█) as a model is transferred into DRAM.  Fill progress is inferred
/// from observable signals: power ramp, aiclk transition, and GDDR temperature delta.
fn ui_defrag(f: &mut Frame, dv: &DefragVis, backend: &dyn TelemetryBackend) {
    let lines = dv.render(backend);
    let widget = Paragraph::new(lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)));
    f.render_widget(widget, f.area());
}

/// Render Arcade mode — uses the composite canvas so the hero `@` and trail
/// actually appear.  The previous ratatui-Layout approach called sub-viz methods
/// directly into separate Rects, bypassing ArcadeVisualization::render() and
/// overlay_hero() entirely — the hero was computed but never drawn.
///
/// The composite path: arcade.render(backend) → Vec<Line> → single Paragraph
/// over the full terminal area.  No border boxes around each sub-panel, but
/// the inline separators from render_separator() provide visual structure.
fn ui_arcade(f: &mut Frame, arcade: &ArcadeVisualization, backend: &dyn TelemetryBackend) {
    let lines = arcade.render(backend);
    let widget = Paragraph::new(lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)));
    f.render_widget(widget, f.area());
}



/// Render the command bar overlay at the bottom of the terminal.
///
/// When `cmd_buf` is non-empty or command mode is active, shows:
///   `:  <cmd_buf>_`
/// When a result message is present (after Enter), shows the message for one
/// render cycle until dismissed with Esc or the next `/` press.
fn render_command_bar(f: &mut Frame, cmd_buf: &str, msg: Option<(&str, bool)>) {
    let area = f.area();
    if area.height < 2 {
        return;
    }
    // Place the bar at the very bottom row.
    let bar_area = Rect {
        x: 0,
        y: area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };

    let spans = if let Some((text, is_error)) = msg {
        let color = if is_error {
            colors::rgb(255, 100, 100)
        } else {
            colors::rgb(80, 220, 140)
        };
        vec![
            Span::styled("  ", Style::default()),
            Span::styled(text.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]
    } else {
        vec![
            Span::styled(
                "  / ",
                Style::default()
                    .fg(colors::rgb(220, 180, 80))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                cmd_buf.to_string(),
                Style::default()
                    .fg(colors::rgb(220, 220, 220))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("_", Style::default().fg(colors::rgb(200, 200, 80))),
        ]
    };

    // Clear the bar row with a dark background, then render text.
    f.render_widget(
        Block::default().style(Style::default().bg(colors::rgb(15, 20, 30))),
        bar_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(colors::rgb(15, 20, 30))),
        bar_area,
    );
}

/// Render the floating overlay panel (legend / help / explain) anchored to the
/// bottom-right corner.  No right-side border characters per project convention.
///
/// The panel composites over whatever view is underneath — it does not consume
/// any layout rows.  Width is fixed at 40 columns; height grows with content.
fn render_overlay_panel(f: &mut Frame, kind: OverlayPanel, mode: DisplayMode) {
    let area = f.area();
    const PANEL_W: u16 = 42;
    // Enforce minimum terminal size — skip if it would leave no room.
    if area.width < PANEL_W + 4 || area.height < 6 {
        return;
    }

    let lines = overlay_panel_lines(kind, mode);
    let panel_h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2)); // +2 for top/bottom border

    let x = area.width.saturating_sub(PANEL_W);
    let y = area.height.saturating_sub(panel_h + 1); // +1 keeps above command bar row

    let panel_rect = Rect { x, y, width: PANEL_W, height: panel_h };

    // Clear background behind the panel.
    f.render_widget(
        Block::default().style(Style::default().bg(colors::rgb(8, 14, 22))),
        panel_rect,
    );

    // Build full display: top border + content lines + bottom border.
    let (title, border_color) = match kind {
        OverlayPanel::Legend  => ("LEGEND",  colors::rgb(79, 209, 197)),   // teal
        OverlayPanel::Help    => ("HELP",    colors::rgb(220, 180, 80)),    // gold
        OverlayPanel::Explain => ("EXPLAIN", colors::rgb(180, 140, 255)),   // lavender
    };
    let inner_w = PANEL_W.saturating_sub(2) as usize; // ╔ takes 1 col, no right border
    let top_rule = "═".repeat(inner_w.saturating_sub(title.len() + 2));
    let mut display_lines: Vec<Line> = Vec::with_capacity(panel_h as usize);

    display_lines.push(Line::from(vec![
        Span::styled(format!("╔═ {} ", title), Style::default().fg(border_color).add_modifier(Modifier::BOLD)),
        Span::styled(top_rule, Style::default().fg(colors::rgb(40, 55, 70))),
    ]));

    let content_rows = (panel_h as usize).saturating_sub(2);
    for line in lines.iter().take(content_rows) {
        display_lines.push(line.clone());
    }

    let bottom_rule = "═".repeat(PANEL_W.saturating_sub(1) as usize);
    display_lines.push(Line::from(
        Span::styled(format!("╚{}", bottom_rule), Style::default().fg(colors::rgb(40, 55, 70)))
    ));

    f.render_widget(
        Paragraph::new(display_lines)
            .style(Style::default().bg(colors::rgb(8, 14, 22))),
        panel_rect,
    );
}

/// Build the content lines for the overlay panel.
///
/// Lines must fit within PANEL_W - 2 visible columns (no right border means content
/// needs to respect the panel width itself).  Each line is prefixed with `║ ` (2 cols).
fn overlay_panel_lines(kind: OverlayPanel, mode: DisplayMode) -> Vec<Line<'static>> {
    let bg   = colors::rgb(8, 14, 22);
    let bar  = colors::rgb(40, 55, 70);
    let dim  = colors::rgb(90, 100, 120);
    let key  = colors::rgb(79, 209, 197);   // teal
    let lbl  = colors::rgb(180, 190, 210);
    let head = colors::rgb(220, 230, 250);

    // Helper: build a ║-prefixed content line.
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    macro_rules! sep {
        ($label:expr) => {
            ln!(vec![Span::styled($label, Style::default().fg(head).add_modifier(Modifier::BOLD))])
        };
    }
    macro_rules! row {
        ($k:expr, $v:expr, $kc:expr) => {
            ln!(vec![
                Span::styled(format!("{:<12}", $k), Style::default().fg($kc).add_modifier(Modifier::BOLD)),
                Span::styled($v, Style::default().fg(dim)),
            ])
        };
    }

    match kind {
        OverlayPanel::Help => {
            vec![
                sep!("Keys"),
                row!(" v",           "cycle views",                  key),
                row!(" l",           "toggle legend",                 key),
                row!(" ?",           "this help panel",               key),
                row!(" !",           "explain current mode",          key),
                row!(" /",           "open command bar",              key),
                row!(" q  Esc",      "quit",                          key),
                row!(" r",           "force data refresh",            key),
                row!(" a  A",        "jump to Arcade",                key),
                row!(" d  D",        "jump to Defrag",                key),
                row!(" g",           "jump to Grid",                  key),
                row!(" b",           "cycle backend",                 key),
                ln!(vec![Span::styled("──────────────────────────────────────", Style::default().fg(bar))]),
                sep!("Commands  (/ to open)"),
                row!(" /mode <name>","insights grid castle flow…",    lbl),
                row!(" /fps <n>",    "animation FPS  1–120",          lbl),
                row!(" /datafps <n>","data refresh FPS  1–30",        lbl),
                row!(" /legend",     "toggle legend panel",           lbl),
                row!(" /explain",    "toggle explain panel",          lbl),
                row!(" /help",       "toggle this panel",             lbl),
            ]
        }

        OverlayPanel::Legend => {
            // Per-view symbol legend — Arcade shows all four combined.
            match mode {
                DisplayMode::Starfield => starfield_legend_lines(bar, bg, dim, key),
                DisplayMode::MemoryCastle => castle_legend_lines(bar, bg, dim),
                DisplayMode::Defrag => defrag_legend_lines(bar, bg, dim, key),
                DisplayMode::MemoryFlow => flow_legend_lines(bar, bg, dim),
                DisplayMode::Arcade => {
                    // All four combined.
                    let mut v = Vec::new();
                    v.push(sep!("✧ Starfield"));
                    v.extend(starfield_legend_lines(bar, bg, dim, key));
                    v.push(ln!(vec![Span::styled("──────────────────────────────────────", Style::default().fg(bar))]));
                    v.push(sep!("🏰 Memory Castle"));
                    v.extend(castle_legend_lines(bar, bg, dim));
                    v.push(ln!(vec![Span::styled("──────────────────────────────────────", Style::default().fg(bar))]));
                    v.push(sep!("▓ Defrag"));
                    v.extend(defrag_legend_lines(bar, bg, dim, key));
                    v.push(ln!(vec![Span::styled("──────────────────────────────────────", Style::default().fg(bar))]));
                    v.push(sep!("🌊 Memory Flow"));
                    v.extend(flow_legend_lines(bar, bg, dim));
                    v.push(ln!(vec![Span::styled("──────────────────────────────────────", Style::default().fg(bar))]));
                    v.push(sep!("@ Hero"));
                    v.extend(hero_legend_lines(bar, bg, dim));
                    v
                }
                _ => {
                    vec![
                        ln!(vec![Span::styled("no legend for this view", Style::default().fg(dim))]),
                        ln!(vec![Span::styled("try v to switch to a viz mode", Style::default().fg(dim))]),
                    ]
                }
            }
        }

        OverlayPanel::Explain => {
            match mode {
                DisplayMode::Starfield => explain_lines(bar, bg, dim, lbl, &[
                    "Hardware-Responsive Starfield",
                    "",
                    "Each star represents one Tensix compute",
                    "core inside a Blackhole or Wormhole ASIC.",
                    "",
                    "Star brightness encodes instantaneous",
                    "power draw. Hue cycles from cyan (cool)",
                    "through yellow to red (hot) tracking the",
                    "ASIC die temperature in real time.",
                    "",
                    "Twinkle speed accelerates when current",
                    "draw is high. Brief sparkle flashes mark",
                    "power spikes above the learned baseline.",
                    "",
                    "Round planets (○) represent DDR/L2/L1",
                    "memory hierarchy — orbit radius tracks",
                    "memory bandwidth utilization.",
                ]),
                DisplayMode::MemoryCastle => explain_lines(bar, bg, dim, lbl, &[
                    "Memory Castle",
                    "",
                    "An architectural view of the Tenstorrent",
                    "memory hierarchy rendered as a layered",
                    "tower. Particles flow upward from DRAM",
                    "at the base through L2 cache, L1 SRAM,",
                    "and into the Tensix compute cores at top.",
                    "",
                    "◦ open circle  — read access",
                    "· dot          — write access",
                    "◇ diamond      — cache hit",
                    "• filled dot   — cache miss",
                    "",
                    "Particle color encodes the memory tier.",
                    "Density reflects current bandwidth.",
                ]),
                DisplayMode::Defrag => explain_lines(bar, bg, dim, lbl, &[
                    "Model-Loading Defrag Display",
                    "",
                    "Inspired by classic disk defragmenters.",
                    "Each row represents one GDDR channel.",
                    "Cells fill left-to-right as model weights",
                    "are transferred into device memory.",
                    "",
                    "░ empty     ▒ loading    ▓█ resident",
                    "",
                    "EVICT flashes when the runtime unloads",
                    "a model — triggered when inference power",
                    "surges above the rolling baseline.",
                    "",
                    "Block character intensity (░▒▓█) encodes",
                    "relative activity within each cell.",
                    "Color encodes channel temperature.",
                ]),
                DisplayMode::MemoryFlow => explain_lines(bar, bg, dim, lbl, &[
                    "Memory Flow — NoC & DDR Activity",
                    "",
                    "Visualizes the Network-on-Chip (NoC) and",
                    "DDR memory traffic in real time.",
                    "",
                    "Particles flowing → are read requests;",
                    "particles flowing ← are write requests.",
                    "Speed scales with NoC clock (axiclk/aiclk",
                    "ratio) and MVDDQ memory subsystem power.",
                    "",
                    "DDR channels are shown as a perimeter",
                    "ring. Channel activity brightens the",
                    "corresponding arc segment.",
                    "",
                    "Heat-map center encodes die temperature.",
                ]),
                DisplayMode::Arcade => explain_lines(bar, bg, dim, lbl, &[
                    "Arcade Mode — Unified View",
                    "",
                    "Combines all four visualizations into a",
                    "single full-screen display.",
                    "",
                    "Top band: Starfield — Tensix cores",
                    "Middle-left: Memory Castle — hierarchy",
                    "Middle-right: Defrag — GDDR loading",
                    "Bottom band: Memory Flow — NoC traffic",
                    "",
                    "The @ hero character moves across the",
                    "full canvas driven by live telemetry:",
                    "  x-axis = current draw (0–100A)",
                    "  y-axis = power (low → bottom, high top)",
                    "  color  = ASIC die temperature",
                    "",
                    "Hero glyph encodes chip health state:",
                    "  @  healthy   !  throttled/faulted",
                    "  ?  ARC stall  %  undervolt",
                ]),
                DisplayMode::Insights => explain_lines(bar, bg, dim, lbl, &[
                    "Insights Screen",
                    "",
                    "Real-time chip portrait grid with live",
                    "telemetry and process management.",
                    "",
                    "Each panel shows a Tensix core portrait",
                    "with power, temperature, clock, and DDR",
                    "status alongside a live process table.",
                    "",
                    "Animated particles in each portrait",
                    "reflect the current compute activity.",
                    "",
                    "Use ↑↓ to navigate the process list.",
                    "k = SIGTERM,  K = SIGKILL.",
                    "At 32+ devices a compact fleet heatmap",
                    "is shown — Enter to zoom in.",
                ]),
                DisplayMode::Grid => explain_lines(bar, bg, dim, lbl, &[
                    "Grid Mode",
                    "",
                    "Full-screen grid of chip portraits for",
                    "all connected devices. No process panel.",
                    "",
                    "Color and density in each portrait cell",
                    "encode Tensix core temperature and power.",
                    "",
                    "Press v to continue cycling modes.",
                ]),
            }
        }
    }
}

// ── Per-view legend line builders ─────────────────────────────────────────────

fn starfield_legend_lines(bar: ratatui::style::Color, bg: ratatui::style::Color, dim: ratatui::style::Color, key: ratatui::style::Color) -> Vec<Line<'static>> {
    let _ = key;
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![Span::styled("·  ○  ●", Style::default().fg(colors::rgb(100, 180, 255))), Span::styled("  each = one Tensix core", Style::default().fg(dim))]),
        ln!(vec![Span::styled("brightness", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("  = power draw", Style::default().fg(dim))]),
        ln!(vec![Span::styled("cyan→red  ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("  = ASIC temperature", Style::default().fg(dim))]),
        ln!(vec![Span::styled("twinkle   ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("  = current draw", Style::default().fg(dim))]),
        ln!(vec![Span::styled("○ planet  ", Style::default().fg(colors::rgb(255, 200, 80))), Span::styled("  = DDR/L2/L1 memory", Style::default().fg(dim))]),
    ]
}

fn castle_legend_lines(bar: ratatui::style::Color, bg: ratatui::style::Color, dim: ratatui::style::Color) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![Span::styled("◦", Style::default().fg(colors::rgb(100, 200, 255))), Span::styled(" = read access", Style::default().fg(dim))]),
        ln!(vec![Span::styled("·", Style::default().fg(colors::rgb(255, 180, 80))),  Span::styled(" = write access", Style::default().fg(dim))]),
        ln!(vec![Span::styled("◇", Style::default().fg(colors::rgb(100, 255, 180))), Span::styled(" = cache hit", Style::default().fg(dim))]),
        ln!(vec![Span::styled("•", Style::default().fg(colors::rgb(255, 100, 150))), Span::styled(" = cache miss", Style::default().fg(dim))]),
        ln!(vec![Span::styled("layers    ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= DDR→L2→L1→Tensix", Style::default().fg(dim))]),
    ]
}

fn defrag_legend_lines(bar: ratatui::style::Color, bg: ratatui::style::Color, dim: ratatui::style::Color, _key: ratatui::style::Color) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![Span::styled("each row  ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= one GDDR channel", Style::default().fg(dim))]),
        ln!(vec![Span::styled("░", Style::default().fg(colors::rgb(60, 80, 100))),  Span::styled(" empty / unloaded", Style::default().fg(dim))]),
        ln!(vec![Span::styled("▒", Style::default().fg(colors::rgb(80, 140, 200))), Span::styled(" loading / in-flight DMA", Style::default().fg(dim))]),
        ln!(vec![Span::styled("▓", Style::default().fg(colors::rgb(79, 209, 197))), Span::styled(" resident / running", Style::default().fg(dim))]),
        ln!(vec![Span::styled("█", Style::default().fg(colors::rgb(220, 240, 255))), Span::styled(" peak activity", Style::default().fg(dim))]),
        ln!(vec![Span::styled("EVICT     ", Style::default().fg(colors::rgb(255, 100, 80))), Span::styled("= model unload surge", Style::default().fg(dim))]),
        ln!(vec![Span::styled("color     ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= channel temperature", Style::default().fg(dim))]),
    ]
}

fn flow_legend_lines(bar: ratatui::style::Color, bg: ratatui::style::Color, dim: ratatui::style::Color) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![Span::styled("→ particles ", Style::default().fg(colors::rgb(100, 220, 255))), Span::styled("= DDR reads", Style::default().fg(dim))]),
        ln!(vec![Span::styled("← particles ", Style::default().fg(colors::rgb(255, 160, 80))), Span::styled("= DDR writes", Style::default().fg(dim))]),
        ln!(vec![Span::styled("speed       ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= NoC clock / aiclk", Style::default().fg(dim))]),
        ln!(vec![Span::styled("density     ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= MVDDQ power (mem W)", Style::default().fg(dim))]),
        ln!(vec![Span::styled("perimeter   ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= GDDR channel ring", Style::default().fg(dim))]),
    ]
}

fn hero_legend_lines(bar: ratatui::style::Color, bg: ratatui::style::Color, dim: ratatui::style::Color) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![Span::styled("x-axis   ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= current draw 0–100A", Style::default().fg(dim))]),
        ln!(vec![Span::styled("y-axis   ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= power (low→bot, hi→top)", Style::default().fg(dim))]),
        ln!(vec![Span::styled("color    ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= ASIC temperature", Style::default().fg(dim))]),
        ln!(vec![Span::styled("@", Style::default().fg(colors::rgb(79, 209, 197)).add_modifier(Modifier::BOLD)), Span::styled("  healthy", Style::default().fg(dim))]),
        ln!(vec![Span::styled("!", Style::default().fg(colors::rgb(255, 100, 80)).add_modifier(Modifier::BOLD)),  Span::styled("  throttled / faulted", Style::default().fg(dim))]),
        ln!(vec![Span::styled("?", Style::default().fg(colors::rgb(255, 220, 80)).add_modifier(Modifier::BOLD)), Span::styled("  ARC firmware stalled", Style::default().fg(dim))]),
        ln!(vec![Span::styled("%", Style::default().fg(colors::rgb(255, 180, 80)).add_modifier(Modifier::BOLD)), Span::styled("  undervolt (vcore<0.75V)", Style::default().fg(dim))]),
        ln!(vec![Span::styled("( )", Style::default().fg(colors::rgb(180, 140, 255)).add_modifier(Modifier::BOLD)), Span::styled(" harvested cores disabled", Style::default().fg(dim))]),
        ln!(vec![Span::styled("trail    ", Style::default().fg(colors::rgb(200, 230, 255))), Span::styled("= GDDR temp / ETH links", Style::default().fg(dim))]),
    ]
}

/// Build explain lines from a static slice of text strings.
fn explain_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
    lbl: ratatui::style::Color,
    text: &[&'static str],
) -> Vec<Line<'static>> {
    text.iter().map(|s| {
        let mut v: Vec<Span<'static>> = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
        if s.is_empty() {
            // blank separator line
        } else {
            v.push(Span::styled(s.to_string(), Style::default().fg(if s.starts_with(' ') { dim } else { lbl })));
        }
        Line::from(v)
    }).collect()
}

/// Parse and execute a slash command.  Returns `(message, is_error)`.
///
/// Available commands:
/// ```
/// /fps <n>          set animation FPS (1–120)
/// /datafps <n>      set data-mode FPS (1–30)
/// /mode <name>      switch display mode (insights|grid|starfield|castle|flow|arcade|defrag)
/// /legend           toggle legend overlay      (hotkey: l)
/// /explain          toggle explain overlay     (hotkey: !)
/// /help             list commands              (hotkey: ?)
/// ```
fn execute_command(
    cmd: &str,
    display_mode: &mut DisplayMode,
    anim_poll: &mut Duration,
    data_poll: &mut Duration,
    _sensitivity: f32,
    overlay: &mut Option<OverlayPanel>,
) -> (String, bool) {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let verb = parts[0].trim_start_matches('/').to_lowercase();
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match verb.as_str() {
        "fps" => {
            match arg.parse::<u64>() {
                Ok(n) if (1..=120).contains(&n) => {
                    *anim_poll = Duration::from_millis(1000 / n);
                    (format!("animation fps set to {}", n), false)
                }
                _ => ("fps: expected 1–120".to_string(), true),
            }
        }
        "datafps" => {
            match arg.parse::<u64>() {
                Ok(n) if (1..=30).contains(&n) => {
                    *data_poll = Duration::from_millis(1000 / n);
                    (format!("data fps set to {}", n), false)
                }
                _ => ("datafps: expected 1–30".to_string(), true),
            }
        }
        "mode" => {
            match arg {
                "insights" | "normal" => { *display_mode = DisplayMode::Insights; ("→ insights".to_string(), false) }
                "grid"     => { *display_mode = DisplayMode::Grid;         ("→ grid".to_string(), false) }
                "starfield"=> { *display_mode = DisplayMode::Starfield;    ("→ starfield".to_string(), false) }
                "castle"   => { *display_mode = DisplayMode::MemoryCastle; ("→ castle".to_string(), false) }
                "flow"     => { *display_mode = DisplayMode::MemoryFlow;   ("→ flow".to_string(), false) }
                "arcade"   => { *display_mode = DisplayMode::Arcade;       ("→ arcade".to_string(), false) }
                "defrag"   => { *display_mode = DisplayMode::Defrag;       ("→ defrag".to_string(), false) }
                _ => ("mode: insights|grid|starfield|castle|flow|arcade|defrag".to_string(), true),
            }
        }
        "legend" | "l" => {
            *overlay = if *overlay == Some(OverlayPanel::Legend) { None } else { Some(OverlayPanel::Legend) };
            ("legend overlay toggled  (press l or /legend to hide)".to_string(), false)
        }
        "explain" => {
            *overlay = if *overlay == Some(OverlayPanel::Explain) { None } else { Some(OverlayPanel::Explain) };
            ("explain overlay toggled  (press ! or /explain to hide)".to_string(), false)
        }
        "help" | "?" | "" => {
            *overlay = if *overlay == Some(OverlayPanel::Help) { None } else { Some(OverlayPanel::Help) };
            ("".to_string(), false) // panel shows the full content
        }
        _ => (format!("unknown command: {}  (try /help or press ?)", verb), true),
    }
}

/// Truncate a string to at most `max` characters (by char count, not bytes).
fn truncate(s: &str, max: usize) -> &str {
    let mut end = 0;
    let mut count = 0;
    for c in s.chars() {
        if count >= max {
            break;
        }
        end += c.len_utf8();
        count += 1;
    }
    &s[..end]
}

/// Flatten all processes into one stable-sorted Vec for cursor navigation.
/// Uses a HashSet to dedup by PID in O(n) instead of O(n²) scan.
#[cfg(feature = "linux-procfs")]
fn flat_process_list<'a>(pm: &'a ProcessMonitor) -> Vec<&'a crate::workload::ProcessInfo> {
    use std::collections::HashSet;
    let mut seen: HashSet<i32> = HashSet::new();
    let mut all: Vec<&'a crate::workload::ProcessInfo> = Vec::new();

    // Iterate only over devices that actually have processes.
    let mut device_indices: Vec<usize> = pm.device_indices().collect();
    device_indices.sort_unstable(); // deterministic order
    for idx in device_indices {
        if let Some(procs) = pm.get_processes_for_device(idx) {
            for p in procs {
                if seen.insert(p.pid) {
                    all.push(p);
                }
            }
        }
    }
    for p in pm.get_shared_processes() {
        if seen.insert(p.pid) {
            all.push(p);
        }
    }
    all.sort_unstable_by_key(|p| p.pid);
    all
}

/// Cell width used in fleet compact mode (see `render_device_panels`).
/// 3 chars: temperature block glyph (1) + space (1) + separator (1).
const FLEET_CELL_W: u16 = 3;

/// Column gap between adjacent device panel columns in the portrait grid.
/// Used by both `panel_layout` and `render_device_panels` — single source of truth.
const PANEL_INTER_COL_GAP: u16 = 1;

/// Fleet compact mode activates at this device count (matching the starfield galaxy mode).
/// At 32+ devices the full-portrait grid almost always overflows a standard terminal.
const FLEET_DEVICE_THRESHOLD: usize = 32;

/// How many devices to show per page in the portrait drill-down (zoom) view.
/// Kept well below FLEET_DEVICE_THRESHOLD so the zoomed slice always renders as portraits.
const FLEET_PAGE_SIZE: usize = 16;

/// Height cap for the panel area in fleet mode.
const FLEET_HEIGHT_THRESHOLD: u16 = 30; // lines; beyond this we switch to compact cells

/// Shared panel layout calculation used by both `device_panels_height` and
/// `render_device_panels`.  Returns `(stats_w, gap_col, cols_per_row)`.
///
/// Single source of truth so the two callers can never diverge on gap or
/// compact-mode threshold.
#[cfg(feature = "linux-procfs")]
fn panel_layout(n: usize, portrait_w: u16, area_width: u16) -> (u16, u16, usize) {
    let balanced_cols = ((n as f64).sqrt().ceil() as usize).max(1);
    let full_panel_w = portrait_w + 1 + 31; // gap=1, stats_w=31
    let cols_u16 = balanced_cols as u16;
    let full_row_w = cols_u16 * full_panel_w + (cols_u16.saturating_sub(1)) * PANEL_INTER_COL_GAP;
    let (stats_w, gap_col) = if full_row_w <= area_width {
        (31_u16, 1_u16)
    } else {
        (19_u16, 0_u16)
    };
    let panel_w: u16 = portrait_w + gap_col + stats_w;
    let max_cols_fit =
        ((area_width + PANEL_INTER_COL_GAP) / (panel_w + PANEL_INTER_COL_GAP)).max(1) as usize;
    let cols_per_row = max_cols_fit.min(balanced_cols);
    (stats_w, gap_col, cols_per_row)
}

/// Compute the vertical space required for the device panel grid at the given terminal width.
///
/// When the device count is large enough that full portraits would overflow the screen
/// we switch to a compact fleet heat-map (one coloured cell per device).
#[cfg(feature = "linux-procfs")]
fn device_panels_height(devices: &[crate::models::Device], area_width: u16) -> u16 {
    use crate::ui::tui::chip_portrait::portrait_dims;
    let n = devices.len().max(1);

    // Fleet mode at 32+ devices (same threshold as starfield galaxy mode).
    if n >= FLEET_DEVICE_THRESHOLD {
        let cells_per_row = (area_width / FLEET_CELL_W).max(1) as usize;
        let cell_rows = (n + cells_per_row - 1) / cells_per_row;
        // 2 rows per cell-row (cell line + label line), +1 for header
        return (cell_rows as u16 * 2 + 1).min(FLEET_HEIGHT_THRESHOLD);
    }

    let arch = devices
        .first()
        .map(|d| d.architecture)
        .unwrap_or(crate::models::Architecture::Blackhole);
    let (portrait_cols, _) = portrait_dims(arch);
    let portrait_w = portrait_cols as u16 + 1;

    let (_, _, cols_per_row) = panel_layout(n, portrait_w, area_width);
    let row_count = (n + cols_per_row - 1) / cols_per_row;
    (row_count as u16) * 15 // panel_h(14) + inter-row gap(1)
}

/// Render Insights screen (full layout — implemented by Task 8/9 helpers below).
///
/// `portrait_particles` maps device index → live particle list, threaded through
/// to `render_device_panels` → `render_chip_portrait` for the particle overlay.
#[cfg(feature = "linux-procfs")]
fn render_insights(
    f: &mut Frame,
    backend: &dyn TelemetryBackend,
    engine: &InferenceEngine,
    pm: &ProcessMonitor,
    cursor: usize,
    kill_confirm: Option<&KillConfirmState>,
    _cli: &Cli,
    portrait_particles: &std::collections::HashMap<
        usize,
        Vec<crate::ui::tui::chip_portrait::Particle>,
    >,
    portrait_baseline: &crate::animation::baseline::AdaptiveBaseline,
    fleet_cursor: usize,
    fleet_zoom_start: Option<usize>,
    host_cpu_pct: f32,
    host_mem_used: u64,
    host_mem_total: u64,
    serving_metrics: &std::collections::HashMap<i32, ServingMetrics>,
) {
    let area = f.area();
    let devices = backend.devices();

    // When zoomed in, the panel height is based on the slice, not the full list.
    let panel_devices: &[crate::models::Device] = if let Some(start) = fleet_zoom_start {
        let end = (start + FLEET_PAGE_SIZE).min(devices.len());
        &devices[start..end]
    } else {
        devices
    };

    // Layout: header(3) | cards(exact height) | process panel (remainder) | footer(1)
    //
    // Cards use Constraint::Length so they always get the exact height needed to
    // show all chip panels (including a 2×2 grid when 4 chips don't fit in one row).
    // The process panel takes whatever remains; it may be zero on very short terminals
    // but the chip panels are never truncated.
    let cards_h = device_panels_height(panel_devices, area.width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(cards_h),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(f, chunks[0], backend);
    render_device_panels(
        f,
        chunks[1],
        backend,
        engine,
        portrait_particles,
        &portrait_baseline,
        fleet_cursor,
        fleet_zoom_start,
    );
    render_process_panel(
        f,
        chunks[2],
        pm,
        &flat_process_list(pm),
        cursor,
        kill_confirm,
        host_cpu_pct,
        host_mem_used,
        host_mem_total,
        serving_metrics,
    );
    render_insights_footer(
        f,
        chunks[3],
        kill_confirm,
        devices.len() >= FLEET_DEVICE_THRESHOLD,
        fleet_zoom_start.is_some(),
    );
    // Kill modal overlays the full screen when active
    if let Some(ref kc) = kill_confirm {
        render_kill_dialog(f, area, kc);
    }
}

/// Stub Insights render for non-linux-procfs builds.
#[cfg(not(feature = "linux-procfs"))]
fn render_insights_no_procfs(
    f: &mut Frame,
    backend: &dyn TelemetryBackend,
    engine: &crate::workload::InferenceEngine,
) {
    let _ = (f, backend, engine);
}

/// Render a btop-style horizontal bar: `[████████░░░░░░░░]  42%`.
///
/// `filled` is a fraction in [0, 1]; `bar_width` is the inner glyph count.
/// Returns a `Vec<Span>` ready to append to a `Line`.
#[cfg(feature = "linux-procfs")]
fn host_bar_spans(
    label: &str,
    filled: f32,
    bar_width: usize,
    value_str: &str,
    bar_color: ratatui::style::Color,
) -> Vec<Span<'static>> {
    use ratatui::style::{Color, Style};
    use ratatui::text::Span;

    let filled_count =
        ((filled.clamp(0.0, 1.0) * bar_width as f32).round() as usize).min(bar_width);
    let empty_count = bar_width - filled_count;

    let bar_filled = "█".repeat(filled_count);
    let bar_empty = "░".repeat(empty_count);

    vec![
        Span::styled(label.to_string(), Style::default().fg(Color::Gray)),
        Span::styled(" [".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(bar_filled, Style::default().fg(bar_color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
        Span::styled("] ".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(value_str.to_string(), Style::default().fg(bar_color)),
    ]
}

/// Pick a color for a bar value in [0, 1]: teal → yellow → red.
#[cfg(feature = "linux-procfs")]
fn bar_color(frac: f32) -> ratatui::style::Color {
    use ratatui::style::Color;
    if frac < 0.5 {
        Color::Rgb(80, 220, 200) // teal
    } else if frac < 0.8 {
        Color::Rgb(255, 200, 80) // yellow-orange
    } else {
        Color::Rgb(255, 80, 80) // red
    }
}

/// Render the process panel with cursor, device mapping, and kill confirmation.
#[cfg(feature = "linux-procfs")]
fn render_process_panel(
    f: &mut Frame,
    area: Rect,
    pm: &ProcessMonitor,
    procs: &[&crate::workload::ProcessInfo],
    cursor: usize,
    _kill_confirm: Option<&KillConfirmState>,
    host_cpu_pct: f32,
    host_mem_used: u64,
    host_mem_total: u64,
    serving_metrics: &std::collections::HashMap<i32, ServingMetrics>,
) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    let _ = pm; // pm available for future expansion
    let mut lines: Vec<Line> = Vec::new();

    // Separator rule
    let rule_width = area.width.saturating_sub(4) as usize;
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("─".repeat(rule_width), Style::default().fg(Color::DarkGray)),
    ]));

    // Host CPU / RAM bars — displayed above the process list so they're always visible
    // even when the list is empty.  Bar width is fixed at 20 glyphs to keep it compact.
    {
        const BAR_W: usize = 20;

        let cpu_frac = (host_cpu_pct / 100.0).clamp(0.0, 1.0);
        let cpu_val = format!("{:>4.1}%", host_cpu_pct.clamp(0.0, 100.0));
        let cpu_color = bar_color(cpu_frac);
        let mut cpu_spans = vec![Span::raw("  ")];
        cpu_spans.extend(host_bar_spans("CPU", cpu_frac, BAR_W, &cpu_val, cpu_color));

        let mem_frac = if host_mem_total > 0 {
            (host_mem_used as f64 / host_mem_total as f64) as f32
        } else {
            0.0
        };
        // Display in GiB with one decimal place.
        let gib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let mem_val = format!("{:.1}/{:.1}G", gib(host_mem_used), gib(host_mem_total));
        let mem_color = bar_color(mem_frac);
        let mut mem_spans: Vec<Span> = vec![Span::raw("  ")];
        mem_spans.extend(host_bar_spans("RAM", mem_frac, BAR_W, &mem_val, mem_color));

        // Lay CPU and RAM on the same row, separated by a few spaces,
        // if the area is wide enough; otherwise separate rows.
        let wide_enough = area.width >= 80;
        if wide_enough {
            let mut row_spans = vec![Span::raw("  ")];
            row_spans.extend(host_bar_spans("CPU", cpu_frac, BAR_W, &cpu_val, cpu_color));
            row_spans.push(Span::raw("     "));
            row_spans.extend(host_bar_spans("RAM", mem_frac, BAR_W, &mem_val, mem_color));
            lines.push(Line::from(row_spans));
        } else {
            lines.push(Line::from(cpu_spans));
            lines.push(Line::from(mem_spans));
        }
    }

    // Section label
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("Processes", Style::default().fg(Color::Gray)),
    ]));

    if procs.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "nothing is feeding the chips right now",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    } else {
        let clamped_cursor = cursor.min(procs.len().saturating_sub(1));

        for (i, proc) in procs.iter().enumerate() {
            let selected = i == clamped_cursor;
            // Use ">" (guaranteed 1-column width) rather than "▶" (ambiguous
            // Unicode width — some terminals render it as 2 columns, shifting
            // all subsequent spans on the selected row 1 char to the right).
            let cursor_char = if selected { ">" } else { " " };

            let name = format!("{:<12}", truncate(&proc.name, 12));
            let cmdline = format!("{:<32}", truncate(&proc.cmdline, 32));
            let pid = format!("{:>7}", proc.pid);

            // Device column: build raw string then *truncate* to column width.
            // format!("{:<N}", s) pads but never truncates; without an explicit
            // truncation step a process holding 4+ devices overflows the column
            // and misaligns everything to the right.
            const DEV_W: usize = 10;
            let dev_raw = if proc.device_indices.is_empty() {
                "shared".to_string()
            } else {
                let dev_str = proc
                    .device_indices
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                format!("D{}", dev_str)
            };
            let devices = format!("{:<width$}", truncate(&dev_raw, DEV_W), width = DEV_W);

            let row_color = if selected { Color::White } else { Color::Gray };
            let cursor_color = if selected {
                Color::Yellow
            } else {
                Color::DarkGray
            };

            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(cursor_char, Style::default().fg(cursor_color)),
                Span::raw(" "),
                Span::styled(name, Style::default().fg(row_color)),
                Span::raw("  "),
                Span::styled(cmdline, Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(pid, Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled(devices, Style::default().fg(Color::Blue)),
            ]));

            // If this is a known TT inference server, append a metrics summary row.
            if let Some(sm) = serving_metrics.get(&proc.pid) {
                let mut spans: Vec<Span> = vec![
                    Span::raw("       "),
                    Span::styled(
                        format!("[{}] ", sm.flavour.label()),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        if sm.swap_in_progress {
                            "loading"
                        } else if sm.is_ready {
                            "ready"
                        } else {
                            "init"
                        },
                        Style::default().fg(if sm.is_ready {
                            Color::Green
                        } else {
                            Color::Yellow
                        }),
                    ),
                ];

                // Collect the interesting metric snippets, space-separated.
                let mut parts: Vec<String> = Vec::new();

                if let Some(ref model) = sm.model_id {
                    let short = model.rsplit('/').next().unwrap_or(model.as_str());
                    parts.push(format!("model={}", truncate(short, 24)));
                }
                if let Some(tps) = sm.generation_tps {
                    parts.push(format!("{:.0}tok/s", tps));
                }
                if let Some(rif) = sm.requests_in_flight {
                    parts.push(format!("{}req", rif));
                }
                if let Some(kv) = sm.kv_cache_utilization {
                    parts.push(format!("kv={:.0}%", kv * 100.0));
                }
                if let Some(ttft) = sm.ttft_p50 {
                    parts.push(format!("ttft={:.0}ms", ttft * 1000.0));
                }
                if let Some(ref mesh) = sm.mesh_device {
                    parts.push(format!("mesh={}", mesh));
                }

                if !parts.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", parts.join("  ")),
                        Style::default().fg(Color::Gray),
                    ));
                }

                lines.push(Line::from(spans));
            }
        }
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, area);
}

/// One-line footer for Insights mode: always shows nav hints.
/// The kill confirmation is now handled by the modal dialog (`render_kill_dialog`).
#[cfg(feature = "linux-procfs")]
fn render_insights_footer(
    f: &mut Frame,
    area: Rect,
    _kill_confirm: Option<&KillConfirmState>,
    in_fleet: bool,
    in_zoom: bool,
) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    let dot = Span::raw("  \u{00B7}  ");

    let line = if in_zoom {
        // Zoomed portrait view — show paging hints.
        Line::from(vec![
            Span::raw("  "),
            Span::styled("← →", Style::default().fg(Color::White)),
            Span::raw("  page"),
            dot.clone(),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw("  back to overview"),
            dot.clone(),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw("  next view"),
            dot.clone(),
            Span::styled("q", Style::default().fg(Color::DarkGray)),
            Span::raw("  leave"),
        ])
    } else if in_fleet {
        // Galaxy overview — show navigation + drill-down hints.
        Line::from(vec![
            Span::raw("  "),
            Span::styled("←↑↓→", Style::default().fg(Color::White)),
            Span::raw("  navigate"),
            dot.clone(),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw("  zoom in"),
            dot.clone(),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw("  next view"),
            dot.clone(),
            Span::styled("q", Style::default().fg(Color::DarkGray)),
            Span::raw("  leave"),
        ])
    } else {
        // Normal portrait grid.
        Line::from(vec![
            Span::raw("  "),
            Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::White)),
            Span::raw("  move"),
            dot.clone(),
            Span::styled("k", Style::default().fg(Color::Yellow)),
            Span::raw("  silence"),
            dot.clone(),
            Span::styled("K", Style::default().fg(Color::Red)),
            Span::raw("  destroy now"),
            dot.clone(),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw("  next view"),
            dot.clone(),
            Span::styled("g", Style::default().fg(Color::Cyan)),
            Span::raw("  grid"),
            dot.clone(),
            Span::styled("?", Style::default().fg(Color::Rgb(220, 180, 80))),
            Span::raw("  help"),
            dot.clone(),
            Span::styled("q", Style::default().fg(Color::DarkGray)),
            Span::raw("  leave"),
        ])
    };

    f.render_widget(Paragraph::new(line), area);
}

/// Render a centered kill-confirmation modal dialog over the current frame.
/// Uses `Clear` to erase the background, then draws a left-border-only dialog
/// (no right-side border characters per AGENTS.md).
#[cfg(feature = "linux-procfs")]
fn render_kill_dialog(f: &mut Frame, area: Rect, kc: &KillConfirmState) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    const DIALOG_W: u16 = 42;
    const DIALOG_H: u16 = 8;

    let x = area
        .x
        .saturating_add((area.width.saturating_sub(DIALOG_W)) / 2);
    let y = area
        .y
        .saturating_add((area.height.saturating_sub(DIALOG_H)) / 2);
    let dialog_rect = Rect {
        x,
        y,
        width: DIALOG_W.min(area.width),
        height: DIALOG_H.min(area.height),
    };

    f.render_widget(Clear, dialog_rect);

    let content_w = (DIALOG_W.saturating_sub(1)) as usize; // ║ takes 1 col
    let name_short = truncate(&kc.name, 14);
    let dev_label = format!("Dev {}", kc.device_idx);

    let title_pad = content_w.saturating_sub("╔ Kill process? ".chars().count());
    let bottom_pad = content_w;

    let lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                "╔ Kill process? ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("═".repeat(title_pad), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                name_short.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · PID {} · {}", kc.pid, dev_label),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(Span::styled("║", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "enter",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("    silence  (SIGTERM)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "K",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "        destroy  (SIGKILL)",
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n · esc", Style::default().fg(Color::Gray)),
            Span::styled("  cancel", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled("║", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            format!("╚{}", "═".repeat(bottom_pad)),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    f.render_widget(Paragraph::new(lines), dialog_rect);
}

/// Compact fleet heat-map for Insights when device count is large.
///
/// Each device is rendered as a single coloured block glyph (`░▒▓█`) with a
/// 2-line layout: top line = coloured blocks, bottom line = device index labels.
/// Colour encodes ASIC temperature via the same `tensix_temp_rgb` ramp used
/// by the full portrait; block density encodes relative power.
/// `cursor` is the currently highlighted device index — that cell is shown with
/// a bright white `◉` instead of the normal block glyph so the user can see
/// where Enter will zoom them in.  Press Enter to drill down to portrait view.
#[cfg(feature = "linux-procfs")]
fn render_fleet_heatmap_panel(
    f: &mut Frame,
    area: Rect,
    backend: &dyn TelemetryBackend,
    cursor: usize,
) {
    use crate::ui::tui::chip_portrait::tensix_temp_rgb;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let devices = backend.devices();
    if devices.is_empty() {
        return;
    }

    let cells_per_row = (area.width / FLEET_CELL_W).max(1) as usize;
    let n = devices.len();

    // Clamp cursor to valid device range.
    let cursor = cursor.min(n.saturating_sub(1));
    // Which page (FLEET_PAGE_SIZE block) does the cursor land on?
    let cursor_page_start = (cursor / FLEET_PAGE_SIZE) * FLEET_PAGE_SIZE;

    // Header line — include hint for drill-down.
    let header = Line::from(vec![Span::styled(
        format!(
            " Fleet — {} devices · ←↑↓→ navigate · Enter zoom · temp=colour power=density ",
            n
        ),
        Style::default()
            .fg(colors::rgb(79, 209, 197))
            .add_modifier(Modifier::BOLD),
    )]);

    let mut lines: Vec<Line> = vec![header];
    let mut y: u16 = area.y + 1; // +1 for header

    // Highlight bar: which cells belong to the cursor's page?
    let page_end = (cursor_page_start + FLEET_PAGE_SIZE).min(n);

    let row_count = (n + cells_per_row - 1) / cells_per_row;
    for row in 0..row_count {
        if y + 1 >= area.y + area.height {
            break;
        }

        let mut block_spans: Vec<Span<'static>> = Vec::new();
        let mut label_spans: Vec<Span<'static>> = Vec::new();

        for col in 0..cells_per_row {
            let dev_idx = row * cells_per_row + col;
            if dev_idx >= n {
                break;
            }

            let device = &devices[dev_idx];
            let telem = backend.telemetry(device.index);

            let temp = telem.map(|t| t.temp_c()).unwrap_or(45.0);
            let power = telem.map(|t| t.power_w()).unwrap_or(0.0);

            let [r, g, b] = tensix_temp_rgb(temp);
            let base_color = Color::Rgb(r, g, b);

            let is_cursor = dev_idx == cursor;
            let in_page = dev_idx >= cursor_page_start && dev_idx < page_end;

            let (block_char, cell_style) = if is_cursor {
                // Cursor cell: bright marker
                (
                    '◉',
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                )
            } else if in_page {
                // Page bracket: same glyph but dimmer highlight tint
                let power_frac = (power / 150.0).min(1.0);
                let ch = match (power_frac * 4.0) as u32 {
                    0 => '░',
                    1 => '▒',
                    2 => '▓',
                    _ => '█',
                };
                (
                    ch,
                    Style::default().fg(base_color).add_modifier(Modifier::BOLD),
                )
            } else {
                // Normal cell
                let power_frac = (power / 150.0).min(1.0);
                let ch = match (power_frac * 4.0) as u32 {
                    0 => '░',
                    1 => '▒',
                    2 => '▓',
                    _ => '█',
                };
                (ch, Style::default().fg(base_color))
            };

            // Cell = block + 2 spaces (FLEET_CELL_W = 3)
            block_spans.push(Span::styled(
                format!("{:width$}", block_char, width = FLEET_CELL_W as usize),
                cell_style,
            ));

            // Index label: bright for cursor/page, dim otherwise
            let label = format!("{:<width$}", device.index, width = FLEET_CELL_W as usize);
            let label_style = if is_cursor {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if in_page {
                Style::default().fg(colors::rgb(130, 130, 160))
            } else {
                Style::default().fg(colors::rgb(80, 80, 100))
            };
            label_spans.push(Span::styled(label, label_style));
        }

        lines.push(Line::from(block_spans));
        lines.push(Line::from(label_spans));
        y += 2;
    }

    let widget = Paragraph::new(lines);
    f.render_widget(widget, area);
}

/// Render Insights device panels — one per device, each with a chip portrait on the
/// left and a stats sidebar on the right.
///
/// Width math (Blackhole):
///   Portrait outer (left border ║ + 17 content) = 18 chars
///   Gap = 1 char
///   Stats outer  (left border ║ + 30 content)  = 31 chars
///   Total per device = 18 + 1 + 31 = 50 chars
///
/// Width math (Wormhole):
///   Total = (1 + 10) + 1 + 31 = 43 chars
///
/// `portrait_particles` maps device index → live particle list.  Passed down to
/// `render_chip_portrait` so the particle overlay appears on each portrait.
/// The border color is derived from ASIC temperature via `tensix_temp_rgb`.
/// When device count is large enough that full portraits would overflow the terminal,
/// dispatches to `render_fleet_heatmap_panel` instead.
/// `fleet_cursor` and `fleet_zoom_start` control galaxy-overview cursor highlighting
/// and portrait drill-down zoom respectively.
#[cfg(feature = "linux-procfs")]
fn render_device_panels(
    f: &mut Frame,
    area: Rect,
    backend: &dyn TelemetryBackend,
    _engine: &crate::workload::InferenceEngine,
    portrait_particles: &std::collections::HashMap<
        usize,
        Vec<crate::ui::tui::chip_portrait::Particle>,
    >,
    portrait_baseline: &crate::animation::baseline::AdaptiveBaseline,
    fleet_cursor: usize,
    fleet_zoom_start: Option<usize>,
) {
    use crate::ui::tui::chip_portrait::{portrait_dims, render_chip_portrait, tensix_temp_rgb};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let all_devices = backend.devices();
    if all_devices.is_empty() {
        return;
    }

    let panel_h: u16 = 14; // portrait 12 rows + 1 title row + 1 bottom border row
    let inter_row_gap: u16 = 1;

    // Fleet galaxy mode at 32+ devices (matching starfield threshold).
    // Skip this when a zoom slice is active — zoomed view always shows portraits.
    if all_devices.len() >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
        render_fleet_heatmap_panel(f, area, backend, fleet_cursor);
        return;
    }

    // Determine which slice of devices to render.
    // Zoomed view: FLEET_PAGE_SIZE devices starting at fleet_zoom_start.
    // Normal view: all devices (< FLEET_DEVICE_THRESHOLD).
    let devices: &[crate::models::Device] = if let Some(start) = fleet_zoom_start {
        let end = (start + FLEET_PAGE_SIZE).min(all_devices.len());
        &all_devices[start..end]
    } else {
        all_devices
    };

    // Zoom banner — shown above the portrait grid when a subset is displayed.
    let (area, zoom_used_rows) = if fleet_zoom_start.is_some() {
        let banner_h: u16 = 1;
        let start = fleet_zoom_start.unwrap();
        let end = (start + FLEET_PAGE_SIZE)
            .min(all_devices.len())
            .saturating_sub(1);
        let total = all_devices.len();
        let banner_text = format!(
            " ▶ Zoomed: chips {}–{} of {}   ← → page   Esc to zoom out ",
            start, end, total,
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                banner_text,
                Style::default()
                    .fg(Color::Rgb(79, 209, 197))
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: banner_h,
            },
        );
        (
            Rect {
                x: area.x,
                y: area.y + banner_h,
                width: area.width,
                height: area.height.saturating_sub(banner_h),
            },
            banner_h,
        )
    } else {
        (area, 0)
    };
    let _ = zoom_used_rows;

    // Compute panel width from first device's architecture (uniform grid).
    // All panels get the same allocated width so grid columns align.
    //
    // Compact mode: when the full-width layout (stats_w=31, gap=1) would require
    let (portrait_cols_first, _) = portrait_dims(devices[0].architecture);
    let portrait_w_first = portrait_cols_first as u16 + 1; // +1 for left border ║
    let n_usize = devices.len();

    let (stats_w, gap_col, cols_per_row) = panel_layout(n_usize, portrait_w_first, area.width);
    let compact = stats_w < 31;
    let panel_w = portrait_w_first + gap_col + stats_w;

    for (panel_idx, device) in devices.iter().enumerate() {
        let col_idx = (panel_idx % cols_per_row) as u16;
        let row_idx = (panel_idx / cols_per_row) as u16;

        let x_offset = area.x + col_idx * (panel_w + PANEL_INTER_COL_GAP);
        let y_offset = area.y + row_idx * (panel_h + inter_row_gap);

        // Skip panels that overflow the allocated area vertically or horizontally
        if y_offset + panel_h > area.y + area.height {
            break;
        }
        if x_offset + panel_w > area.x + area.width {
            continue;
        }

        let idx = device.index;
        let telemetry = backend.telemetry(idx);
        let smbus = backend.smbus_telemetry(idx);

        // Per-device portrait dims (may differ in mixed-arch systems)
        let (portrait_cols, _portrait_rows) = portrait_dims(device.architecture);
        let portrait_w = portrait_cols as u16 + 1; // +1 for left border ║

        let panel_rect = Rect {
            x: x_offset,
            y: y_offset,
            width: panel_w,
            height: panel_h,
        };

        // Derive border color from ASIC temperature — teal when cool, gold when
        // warm, red when hot — using the same `tensix_temp_rgb` heatmap as the
        // Tensix cells in the portrait itself, giving visual continuity.
        let border_color = if let Some(telem) = telemetry {
            let [r, g, b] = tensix_temp_rgb(telem.temp_c());
            Color::Rgb(r, g, b)
        } else {
            Color::DarkGray
        };

        // ── Header rule: arch name + dev index + fixed-width temp ────────────
        // {:>3} right-justifies the temperature to exactly 3 chars, so the rule
        // never changes width when temp crosses 99→100°C.
        // board_type is omitted when it duplicates the architecture name (e.g.
        // sysfs backend sets board_type="blackhole" from the hwmon driver name).
        // It is shown when it carries distinct product info (e.g. "p300c" from JSON).
        let arch_name = device.architecture.name().to_uppercase();
        let temp_i = telemetry.map(|t| t.temp_c() as i32).unwrap_or(0);
        let board_raw = device.board_type.to_lowercase();
        let arch_lower = device.architecture.name().to_lowercase();
        let label = if !board_raw.is_empty() && board_raw != arch_lower && board_raw != "unknown" {
            let board_trim = &device.board_type[..device.board_type.len().min(7)];
            format!(
                "── {} · D{} · {} ·{:>3}°C ",
                arch_name, idx, board_trim, temp_i
            )
        } else {
            format!("── {} · D{} ·{:>3}°C ", arch_name, idx, temp_i)
        };
        let trail = (panel_w as usize).saturating_sub(label.chars().count());
        let header_text = format!("{}{}", label, "─".repeat(trail));
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                header_text,
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: panel_rect.x,
                y: panel_rect.y,
                width: panel_w,
                height: 1,
            },
        );

        // ── Portrait (1-col left margin, no box border) ───────────────────────
        let portrait_rect = Rect {
            x: panel_rect.x + 1,
            y: panel_rect.y + 1,
            width: portrait_cols as u16,
            height: panel_h.saturating_sub(2),
        };
        if let Some(telem) = telemetry {
            let particles = portrait_particles
                .get(&idx)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let power_change = portrait_baseline
                .power_change(idx, telem.power_w())
                .max(0.0);
            render_chip_portrait(
                f,
                portrait_rect,
                device,
                telem,
                smbus,
                particles,
                power_change,
            );
        }

        // ── Footer hairline ───────────────────────────────────────────────────
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(panel_w as usize),
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: panel_rect.x,
                y: panel_rect.y + panel_h - 1,
                width: panel_w,
                height: 1,
            },
        );

        // ── Right: stats sidebar (║ for interior rows; header/footer are rules) ─
        // When compact=true the sidebar is narrower (stats_w=19) so all panels
        // fit in a single row.  Core rows (power, ETH, temp, FW) are always shown;
        // secondary rows (GDDR temps, fan RPM) are omitted and label prefixes/bars
        // are shortened to fit the reduced width.
        let stats_x = panel_rect.x + portrait_w + gap_col;
        let interior_h = panel_h.saturating_sub(2); // rows between header and footer
        let stats_border_rect = Rect {
            x: stats_x,
            y: panel_rect.y + 1,
            width: 1,
            height: interior_h,
        };
        let stats_border: Vec<Line> = (0..interior_h as usize)
            .map(|_| Line::from(Span::styled("║", Style::default().fg(border_color))))
            .collect();
        f.render_widget(Paragraph::new(stats_border), stats_border_rect);

        let content_x = stats_x + 1;
        let content_w = stats_w.saturating_sub(1);
        let content_rect = Rect {
            x: content_x,
            y: panel_rect.y + 1,
            width: content_w,
            height: interior_h,
        };

        let mut stat_lines: Vec<Line> = Vec::new();
        // Arch name, device index, and temperature live in the header rule above.
        // Stats sidebar starts directly with telemetry data.

        // Power row
        let power_w_val = telemetry.map(|t| t.power_w()).unwrap_or(0.0);
        // Use device directly — avoid redundant backend.devices() lookup.
        let tdp = device
            .limits
            .as_ref()
            .and_then(|l| l.tdp_limit)
            .unwrap_or(300.0);
        let power_frac = (power_w_val / tdp).min(1.0);
        let power_color = if power_frac > 0.85 {
            Color::Rgb(255, 80, 80)
        } else if power_frac > 0.6 {
            Color::Rgb(236, 150, 184)
        } else if power_frac > 0.3 {
            Color::Rgb(160, 120, 255)
        } else {
            Color::Rgb(79, 209, 197)
        };
        if compact {
            // Compact: "⚡ bar(6) val" — fits in 18 chars total.
            let bar_filled = (power_frac * 6.0) as usize;
            let power_bar: String = (0..6)
                .map(|i| if i < bar_filled { '█' } else { '░' })
                .collect();
            stat_lines.push(Line::from(vec![
                Span::styled("⚡", Style::default().fg(Color::DarkGray)),
                Span::styled(power_bar, Style::default().fg(power_color)),
                Span::styled(
                    format!(" {:5.0}W", power_w_val),
                    Style::default().fg(Color::White),
                ),
            ]));
        } else {
            let bar_filled = (power_frac * 10.0) as usize;
            let power_bar: String = (0..10)
                .map(|i| if i < bar_filled { '█' } else { '░' })
                .collect();
            stat_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<8}", "Power"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(power_bar, Style::default().fg(power_color)),
                Span::styled(
                    format!(" {:5.0}W", power_w_val),
                    Style::default().fg(Color::White),
                ),
            ]));
        }

        // ETH row — use ENABLED_ETH as the denominator (ports provisioned by
        // firmware) rather than the architectural maximum.  On p300c cards
        // ENABLED_ETH=0x3edf gives 12 enabled ports; showing 0/24 looks broken.
        //
        // Fallback hierarchy:
        //   1. SMBUS present + ENABLED_ETH > 0  → use enabled count (accurate)
        //   2. SMBUS present + ENABLED_ETH = 0  → show live/? (transient or no ports)
        //   3. SMBUS absent entirely             → fall back to arch max (no data yet)
        let eth_live_mask = smbus.and_then(|s| s.eth_live_status).unwrap_or(0);
        let eth_enabled_mask = smbus.and_then(|s| s.enabled_eth).unwrap_or(0);
        // Count only ports that are both enabled and live to prevent impossible
        // displays like "15/14 live" if ETH_LIVE_STATUS has bits outside ENABLED_ETH.
        let eth_live_count = if eth_enabled_mask > 0 {
            (eth_live_mask & eth_enabled_mask as u64).count_ones()
        } else {
            eth_live_mask.count_ones()
        };
        let eth_total: Option<u32> = if eth_enabled_mask > 0 {
            Some(eth_enabled_mask.count_ones())
        } else if smbus.is_some() {
            None // SMBUS present but ENABLED_ETH zero — don't invent a total
        } else {
            // No SMBUS data yet — show architectural max so the row isn't empty
            Some(match device.architecture {
                crate::models::Architecture::Blackhole => 24,
                crate::models::Architecture::Wormhole => 20,
                _ => 4,
            })
        };
        let eth_total_display = eth_total
            .map(|t| t.to_string())
            .unwrap_or_else(|| "?".to_string());
        let eth_color = match eth_total {
            Some(t) if eth_live_count == t && t > 0 => Color::Rgb(79, 209, 197), // teal — all live
            _ if eth_live_count > 0 => Color::Rgb(244, 196, 113),                // yellow — partial
            _ => Color::Rgb(96, 125, 139), // muted gray — none live
        };
        if compact {
            stat_lines.push(Line::from(vec![
                Span::styled("E ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}/{}", eth_live_count, eth_total_display),
                    Style::default().fg(eth_color),
                ),
            ]));
        } else {
            const ETH_DOT_CAP: u32 = 16;
            let dots_shown = eth_total.unwrap_or(ETH_DOT_CAP).min(ETH_DOT_CAP);
            // Dot map: iterate enabled bits when available, else live bits.
            let enabled_bits: Vec<u32> = if eth_enabled_mask > 0 {
                (0..32)
                    .filter(|&b| (eth_enabled_mask >> b) & 1 == 1)
                    .collect()
            } else {
                (0..dots_shown).collect()
            };
            let eth_dots: String = enabled_bits
                .iter()
                .take(dots_shown as usize)
                .map(|&bit| {
                    if (eth_live_mask >> bit) & 1 == 1 {
                        '●'
                    } else {
                        '·'
                    }
                })
                .collect();
            let eth_suffix = match eth_total {
                Some(t) if t > ETH_DOT_CAP => format!("+{}", t - ETH_DOT_CAP),
                _ => String::new(),
            };
            stat_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<8}", "ETH"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{}{}", eth_dots, eth_suffix),
                    Style::default().fg(eth_color),
                ),
                Span::styled(
                    format!(" {}/{} live", eth_live_count, eth_total_display),
                    Style::default().fg(Color::White),
                ),
            ]));
        }

        // GDDR temp row (full mode only — too wide for compact)
        if !compact {
            if let Some(s) = smbus {
                let temps: Vec<f32> = s
                    .gddr_temps
                    .iter()
                    .filter_map(|p| p.as_ref())
                    .flat_map(|p| p.0.iter().copied())
                    .filter(|&t| t > 0.0)
                    .collect();
                if !temps.is_empty() {
                    let t_min = temps.iter().copied().fold(f32::INFINITY, f32::min);
                    let t_max = temps.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    let max_color = if t_max > 70.0 {
                        Color::Rgb(255, 80, 80)
                    } else if t_max > 50.0 {
                        Color::Rgb(236, 150, 184)
                    } else {
                        Color::Rgb(79, 209, 197)
                    };
                    stat_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:<8}", "GDDR T"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{:.0}°→{:.0}°C", t_min, t_max),
                            Style::default().fg(max_color),
                        ),
                    ]));
                }
            }
        }

        // ASIC temp row
        let asic_temp = telemetry.map(|t| t.temp_c()).unwrap_or(0.0);
        // Use device directly — avoid redundant backend.devices() lookup.
        let thm_limit = device
            .limits
            .as_ref()
            .and_then(|l| l.thm_limit)
            .unwrap_or(105.0);
        let temp_color = crate::ui::colors::temp_color(asic_temp);
        if compact {
            // Compact: "T 42.1°C" — omit limit annotation.
            stat_lines.push(Line::from(vec![
                Span::styled("T ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:.1}°C", asic_temp),
                    Style::default().fg(temp_color),
                ),
            ]));
        } else {
            stat_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<8}", "Temp"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:.1}°C", asic_temp),
                    Style::default().fg(temp_color),
                ),
                Span::styled(
                    format!(" (lim {:.0}°C)", thm_limit),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        // Fan RPM row (full mode only)
        if !compact {
            if let Some(rpm_str) = smbus.and_then(|s| s.fan_speed.as_deref()) {
                if let Ok(rpm) = rpm_str.trim().parse::<u32>() {
                    if rpm > 0 {
                        stat_lines.push(Line::from(vec![
                            Span::styled(
                                format!("{:<8}", "Fan"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(format!("{} RPM", rpm), Style::default().fg(Color::White)),
                        ]));
                    }
                }
            }
        }

        // Firmware row — shown in both full and compact mode.
        {
            let fw_ver = device
                .firmwares
                .as_ref()
                .and_then(|fw| fw.fw_bundle_version.as_deref())
                .unwrap_or("—");
            let fw_short = fw_ver.trim_start_matches("fw_pack-");
            if compact {
                // Compact: "F " + up to 16 chars (content_w=18).
                let fw_display = &fw_short[..fw_short.len().min(16)];
                stat_lines.push(Line::from(vec![
                    Span::styled("F ", Style::default().fg(Color::DarkGray)),
                    Span::styled(fw_display.to_string(), Style::default().fg(Color::White)),
                ]));
            } else {
                // content_w is 30; "FW" label takes 8 chars, leaving 22 for the version string.
                let fw_display = &fw_short[..fw_short.len().min(22)];
                stat_lines.push(Line::from(vec![
                    Span::styled(format!("{:<8}", "FW"), Style::default().fg(Color::DarkGray)),
                    Span::styled(fw_display.to_string(), Style::default().fg(Color::White)),
                ]));
            }
        }

        // Pad to fill all interior rows (footer hairline is the visual bottom).
        while stat_lines.len() < interior_h as usize {
            stat_lines.push(Line::raw(""));
        }

        f.render_widget(Paragraph::new(stat_lines), content_rect);
    }
}

/// Render Grid mode: all chip portraits tiled across the terminal.
///
/// Cell width = portrait_cols + 2 (left-border + 1 gap).
/// Only fully fitting cells are rendered — no partial cells or right overflow.
fn render_grid_mode(f: &mut Frame, backend: &dyn TelemetryBackend) {
    use crate::ui::tui::chip_portrait::{portrait_dims, render_chip_portrait};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let area = f.area();
    let devices = backend.devices();
    if devices.is_empty() {
        return;
    }

    // Title bar
    let title = format!(
        "tt-toplike  [Grid]  {} device{}  │  g or v to cycle views  │  q to quit",
        devices.len(),
        if devices.len() == 1 { "" } else { "s" }
    );
    let title_rect = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "{:<w$}",
                &title[..title.len().min(area.width as usize)],
                w = area.width as usize
            ),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        title_rect,
    );

    let grid_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    // Use first device's arch for uniform grid layout math. All cells must have
    // the same width and height so the tiled grid lines up correctly. Mixed-arch
    // systems will use per-device dims only for the inner rendering (label, portrait,
    // bottom border) while the grid stride stays based on the first device.
    let (grid_cell_cols, grid_cell_rows) = portrait_dims(devices[0].architecture);
    let cell_w = (grid_cell_cols as u16) + 2; // left-border + content + 1-char gap
    let cell_h = (grid_cell_rows as u16) + 2; // label row + content + bottom-border row

    let cols_per_row = (grid_area.width / cell_w).max(1) as usize;
    let rows_visible = (grid_area.height / cell_h).max(1) as usize;
    let max_cells = cols_per_row * rows_visible;

    for (cell_idx, device) in devices.iter().enumerate().take(max_cells) {
        let grid_col = (cell_idx % cols_per_row) as u16;
        let grid_row = (cell_idx / cols_per_row) as u16;

        let cell_x = grid_area.x + grid_col * cell_w;
        let cell_y = grid_area.y + grid_row * cell_h;

        let idx = device.index;
        let telemetry = backend.telemetry(idx);
        let smbus = backend.smbus_telemetry(idx);

        // Per-device portrait dimensions — may differ in mixed-arch fleets.
        // These drive the inner label, portrait, and bottom-border geometry.
        let (p_cols, p_rows) = portrait_dims(device.architecture);

        // Left border
        let border_rect = Rect {
            x: cell_x,
            y: cell_y,
            width: 1,
            height: cell_h,
        };
        let border_lines: Vec<Line> = (0..cell_h as usize)
            .map(|r| {
                let ch = if r == 0 {
                    "╔"
                } else if r == cell_h as usize - 1 {
                    "╚"
                } else {
                    "║"
                };
                Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
            })
            .collect();
        f.render_widget(Paragraph::new(border_lines), border_rect);

        // Device label
        let label = format!(
            "{} {}",
            device.architecture.abbrev(),
            &device.board_type[..device.board_type.len().min(6)]
        );
        let label_rect = Rect {
            x: cell_x + 1,
            y: cell_y,
            width: p_cols as u16,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{:<w$}", &label[..label.len().min(p_cols)], w = p_cols),
                Style::default().fg(Color::Gray),
            ))),
            label_rect,
        );

        // Portrait (Grid mode does not have portrait particle state — pass empty slice)
        let portrait_rect = Rect {
            x: cell_x + 1,
            y: cell_y + 1,
            width: p_cols as u16,
            height: p_rows as u16,
        };
        if let Some(telem) = telemetry {
            render_chip_portrait(f, portrait_rect, device, telem, smbus, &[], 0.0);
        }

        // Bottom border (left-side only: ╚ + ═ repeated, no right-cap ╝)
        let bottom_rect = Rect {
            x: cell_x,
            y: cell_y + cell_h - 1,
            width: p_cols as u16 + 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("╚{}", "═".repeat(p_cols)),
                Style::default().fg(Color::DarkGray),
            ))),
            bottom_rect,
        );
    }
}

#[cfg(feature = "linux-procfs")]
#[cfg(test)]
mod tests {
    use super::panel_layout;

    // BH portrait_w = 17 cols + 1 border = 18.
    const BH_PORTRAIT_W: u16 = 18;

    #[test]
    fn panel_layout_4_chips_full_mode_at_160_cols() {
        // 160-wide terminal: balanced=2, full 2-col row = 2*50+1 = 101 ≤ 160 → full mode.
        let (stats_w, gap_col, cols_per_row) = panel_layout(4, BH_PORTRAIT_W, 160);
        assert_eq!(stats_w, 31, "should be full mode");
        assert_eq!(gap_col, 1);
        assert_eq!(cols_per_row, 2, "4 chips → 2×2 grid");
    }

    #[test]
    fn panel_layout_4_chips_compact_mode_at_80_cols() {
        // 80-wide terminal: full 2-col row = 101 > 80 → compact mode.
        let (stats_w, gap_col, cols_per_row) = panel_layout(4, BH_PORTRAIT_W, 80);
        assert_eq!(stats_w, 19, "should be compact mode");
        assert_eq!(gap_col, 0);
        assert_eq!(
            cols_per_row, 2,
            "compact panels (37 cols) still fit 2 per row"
        );
    }

    #[test]
    fn panel_layout_4_chips_very_narrow_single_column() {
        // 37-wide terminal: compact panel = 37 cols; cols_per_row must be 1 (not 0).
        let (stats_w, _, cols_per_row) = panel_layout(4, BH_PORTRAIT_W, 37);
        assert_eq!(stats_w, 19, "compact at very narrow width");
        assert_eq!(cols_per_row, 1, "only one panel fits at 37 cols");
    }

    #[test]
    fn panel_layout_1_chip_always_full() {
        // Single chip: balanced=1, full row = 1*50 = 50; fits at most terminals.
        let (stats_w, _, cols_per_row) = panel_layout(1, BH_PORTRAIT_W, 120);
        assert_eq!(stats_w, 31);
        assert_eq!(cols_per_row, 1);
    }

    #[test]
    fn panel_layout_9_chips_3x3_grid() {
        // 9 chips: balanced=ceil(sqrt(9))=3; full 3-col row = 3*50+2 = 152.
        let (stats_w, _, cols_per_row) = panel_layout(9, BH_PORTRAIT_W, 160);
        assert_eq!(stats_w, 31);
        assert_eq!(cols_per_row, 3, "9 chips → 3×3 grid");
    }
}
