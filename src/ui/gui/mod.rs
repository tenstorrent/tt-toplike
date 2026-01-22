//! Native GUI module
//!
//! This module provides GUI-specific utilities and components.
//! The main GUI application is in src/bin/gui.rs.

pub mod history;
pub mod visualization;
pub mod terminal_grid;
pub mod terminal_canvas;

pub use history::HistoryManager;
pub use terminal_grid::TerminalGrid;

/// Run the native GUI application
///
/// Note: The actual GUI implementation is in src/bin/gui.rs.
/// This function is provided for consistency with the TUI module structure.
pub fn run_gui() {
    eprintln!("Error: run_gui() should not be called directly.");
    eprintln!("Use the tt-toplike-gui binary instead:");
    eprintln!("  cargo run --bin tt-toplike-gui --features gui");
    std::process::exit(1);
}
