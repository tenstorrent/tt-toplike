// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! TT-Toplike-RS - Terminal User Interface
//!
//! This binary provides a beautiful terminal-based interface for monitoring
//! Tenstorrent hardware using Ratatui and Crossterm.
//!
//! Features:
//! - Real-time telemetry display
//! - Hardware-responsive psychedelic visualizations
//! - Multiple visualization modes (Starfield, TRON Grid, etc.)
//! - Adaptive baseline learning for universal hardware sensitivity
//! - Dark-mode optimized color palette

use tt_toplike_rs::{
    backend::{BackendConfig, TelemetryBackend, mock::MockBackend, json::JSONBackend},
    cli::{Cli, BackendType},
    init_logging,
};

#[cfg(feature = "luwen-backend")]
use tt_toplike_rs::backend::luwen::LuwenBackend;

fn main() {
    // Parse command-line arguments
    let cli = Cli::parse_args();

    // Validate arguments
    if let Err(e) = cli.validate() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    // Initialize logging with appropriate level
    init_logging(cli.log_level());

    // Print startup banner
    println!("🦀 TT-Toplike-RS v{}", env!("CARGO_PKG_VERSION"));
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Backend: {}", cli.backend_name());
    println!("Update Interval: {}ms", cli.interval);
    if let Some(ref devices) = cli.devices {
        println!("Device Filter: {:?}", devices);
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Create backend configuration
    let config = BackendConfig::default()
        .with_interval(cli.interval)
        .with_max_errors(cli.max_errors);

    // Select and initialize backend based on CLI arguments
    let backend_type = cli.effective_backend();

    match backend_type {
        BackendType::Mock => {
            log::info!("Initializing MockBackend with {} devices", cli.mock_devices);
            let mut backend = MockBackend::with_config(cli.mock_devices, config);
            run_with_backend(&mut backend, &cli);
        }
        BackendType::Json => {
            log::info!("Initializing JSONBackend with tt-smi path: {:?}", cli.tt_smi_path);
            let mut backend = JSONBackend::with_config(
                cli.tt_smi_path.to_string_lossy().to_string(),
                config,
            );
            run_with_backend(&mut backend, &cli);
        }
        BackendType::Auto => {
            // SAFE MODE AUTO-DETECT: Never tries Luwen (invasive, requires PCI access)
            // Order: Sysfs (hwmon) → JSON (tt-smi) → Mock
            // Use --backend luwen explicitly if you need direct hardware access
            log::info!("Auto-detecting backend (safe mode - skipping Luwen)...");

            // Try Sysfs backend first (Linux hwmon sensors - SAFEST, non-invasive)
            #[cfg(target_os = "linux")]
            {
                println!("🔍 Trying Sysfs backend (hwmon sensors - safest, non-invasive)...");
                let mut sysfs_backend = tt_toplike_rs::backend::sysfs::SysfsBackend::with_config(config.clone());

                match sysfs_backend.init() {
                    Ok(_) => {
                        println!("✓ Sysfs backend initialized successfully");
                        run_with_backend(&mut sysfs_backend, &cli);
                        return;
                    }
                    Err(e) => {
                        log::warn!("Sysfs backend failed: {}", e);
                        println!("⚠ Sysfs backend unavailable, trying JSON backend...");
                    }
                }
            }

            // Try JSON backend as second option (tt-smi subprocess - safe)
            println!("🔍 Trying JSON backend (tt-smi subprocess)...");
            let mut json_backend = JSONBackend::with_config(
                cli.tt_smi_path.to_string_lossy().to_string(),
                config.clone(),
            );

            match json_backend.init() {
                Ok(_) => {
                    println!("✓ JSON backend initialized successfully");
                    run_with_backend(&mut json_backend, &cli);
                    return;
                }
                Err(e) => {
                    log::warn!("JSON backend failed: {}", e);
                    println!("⚠ JSON backend unavailable, falling back to mock...");
                }
            }

            // Last resort: Mock backend (for testing without hardware)
            println!("⚠ No hardware backends available, using mock backend");
            println!("💡 Tip: Use --backend luwen for direct hardware access (requires PCI permissions)");
            let mut mock_backend = MockBackend::with_config(cli.mock_devices, config);
            run_with_backend(&mut mock_backend, &cli);
        }
        BackendType::Sysfs => {
            #[cfg(target_os = "linux")]
            {
                log::info!("Initializing Sysfs backend");
                let mut backend = tt_toplike_rs::backend::sysfs::SysfsBackend::with_config(config);
                run_with_backend(&mut backend, &cli);
            }
            #[cfg(not(target_os = "linux"))]
            {
                eprintln!("Error: Sysfs backend only available on Linux");
                eprintln!("Use --mock, --json, or --backend luwen instead");
                std::process::exit(1);
            }
        }
        BackendType::Luwen => {
            #[cfg(feature = "luwen-backend")]
            {
                log::info!("Initializing LuwenBackend (direct hardware access)");
                let mut backend = LuwenBackend::with_config(config);
                run_with_backend(&mut backend, &cli);
            }

            #[cfg(not(feature = "luwen-backend"))]
            {
                eprintln!("Error: Luwen backend not enabled");
                eprintln!("Rebuild with: cargo build --features luwen-backend");
                std::process::exit(1);
            }
        }
        #[cfg(target_os = "linux")]
        BackendType::Hybrid => {
            log::info!("Initializing HybridBackend (sysfs + background tt-smi cache)");
            let mut backend = tt_toplike_rs::backend::hybrid::HybridBackend::with_config(
                cli.tt_smi_path.to_string_lossy().to_string(),
                config,
            );
            run_with_backend(&mut backend, &cli);
        }
        #[cfg(not(target_os = "linux"))]
        BackendType::Hybrid => {
            eprintln!("Error: Hybrid backend only available on Linux");
            std::process::exit(1);
        }
    }
}

/// Run the application with a given backend
///
/// This initializes the backend and launches the TUI (or prints telemetry if --print).
fn run_with_backend<B: TelemetryBackend>(backend: &mut B, cli: &Cli) {
    // Initialize backend
    match backend.init() {
        Ok(_) => {
            log::info!("Backend initialized: {}", backend.backend_info());
            log::info!("Discovered {} devices", backend.device_count());
        }
        Err(e) => {
            eprintln!("✗ Backend initialization failed: {}", e);
            std::process::exit(1);
        }
    }

    // Read initial telemetry
    if let Err(e) = backend.update() {
        eprintln!("✗ Failed to read telemetry: {}", e);
        std::process::exit(1);
    }

    // Print mode - dump telemetry and exit
    if cli.print {
        print_telemetry(backend);
        return;
    }

    // Launch TUI (TUI will create its own backend)
    if let Err(e) = tt_toplike_rs::ui::run_tui(cli) {
        eprintln!("TUI error: {}", e);
        std::process::exit(1);
    }
}

/// Print telemetry to stdout (for --print mode)
fn print_telemetry<B: TelemetryBackend>(backend: &B) {
    println!("Backend: {}", backend.backend_info());
    println!("Devices: {}\n", backend.device_count());

    for device in backend.devices() {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Device {}: {} ({:?})", device.index, device.board_type, device.architecture);
        println!("Bus ID: {}", device.bus_id);

        if let Some(telem) = backend.telemetry(device.index) {
            println!("\nCore Telemetry:");
            println!("  Voltage:     {:.3} V", telem.voltage.unwrap_or(0.0));
            println!("  Current:     {:.2} A", telem.current.unwrap_or(0.0));
            println!("  Power:       {:.2} W", telem.power.unwrap_or(0.0));
            println!("  Temperature: {:.1} °C", telem.asic_temperature.unwrap_or(0.0));
            println!("  AICLK:       {} MHz", telem.aiclk.unwrap_or(0));
            println!("  Heartbeat:   {}", telem.heartbeat.unwrap_or(0));
        }

        if let Some(smbus) = backend.smbus_telemetry(device.index) {
            println!("\nSMBUS Telemetry:");
            if let Some(ref ddr_status) = smbus.ddr_status {
                println!("  DDR Status:  {}", ddr_status);
            }
            if let Some(ref ddr_speed) = smbus.ddr_speed {
                println!("  DDR Speed:   {} MT/s", ddr_speed);
            }
            if let Some(ref arc0) = smbus.arc0_health {
                println!("  ARC0 Health: {}", arc0);
            }
        }
        println!();
    }
}
