//! User Interface Module
//!
//! This module provides multiple frontend implementations:
//! - **TUI**: Terminal-based interface using Ratatui (feature: "tui")
//! - **GUI**: Native GUI using iced (feature: "gui")
//!
//! Both frontends share the same backend abstraction and core functionality,
//! allowing users to choose their preferred interface.

// Color utilities (shared by both frontends)
pub mod colors;

// TUI implementation (requires "tui" feature)
#[cfg(feature = "tui")]
pub mod tui;

// GUI implementation (requires "gui" feature)
#[cfg(feature = "gui")]
pub mod gui;

// Re-export run_tui for convenience
#[cfg(feature = "tui")]
pub use tui::run_tui;

// Re-export run_gui for convenience
#[cfg(feature = "gui")]
pub use gui::run_gui;
