//! egui-based Dashboard Entry Point
//!
//! Lightweight real-time monitoring dashboard using egui.
//! Complements the TUI by focusing on real-time graphs and multi-device comparison.

use clap::Parser;
use eframe::egui;
use tt_toplike_rs::backend::{factory, BackendConfig};
use tt_toplike_rs::cli::{BackendType, Cli};
use tt_toplike_rs::ui::egui::DashboardApp;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse CLI arguments
    let cli = Cli::parse();

    // Initialize logging
    tt_toplike_rs::init_logging(cli.log_level());

    log::info!("🦀 TT-Toplike-RS egui Dashboard");
    log::info!("Backend: {:?}", cli.effective_backend());

    // Create backend config
    let config = BackendConfig::new()
        .with_interval(cli.interval)
        .with_max_errors(cli.max_errors);

    let config = if cli.verbose { config.verbose() } else { config };

    // Create backend
    let backend_type = cli.effective_backend();
    let backend = factory::create_backend(backend_type, config.clone(), &cli)?;

    log::info!("Backend initialized: {}", backend.backend_info());

    // Create dashboard app
    let app = DashboardApp::new(backend, cli);

    // Configure native options
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TT-Toplike-RS Dashboard")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    // Run the application
    eframe::run_native(
        "TT-Toplike-RS",
        native_options,
        Box::new(|_cc| Ok(Box::new(app))),
    )?;

    Ok(())
}
