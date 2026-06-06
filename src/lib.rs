// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! tt-toplike - Core Library
//!
//! This library provides the core functionality for monitoring Tenstorrent hardware,
//! including backend abstractions, telemetry models, and visualization utilities.
//!
//! The library is designed to support multiple frontend interfaces:
//! - Terminal User Interface (TUI) using Ratatui
//! - Native GUI using iced
//! - Future: Web interface, REST API, etc.
//!
//! # Architecture
//!
//! The core is built around the `TelemetryBackend` trait, which abstracts over
//! different hardware access methods:
//! - **MockBackend**: Generates realistic mock data for testing
//! - **JSONBackend**: Communicates with tt-smi via JSON subprocess
//! - **LuwenBackend**: Direct hardware access via luwen library
//!
//! # Example
//!
//! ```no_run
//! use tt_toplike::backend::{BackendConfig, mock::MockBackend, TelemetryBackend};
//!
//! let mut backend = MockBackend::with_config(2, BackendConfig::default());
//! backend.init().expect("Failed to initialize backend");
//! backend.update().expect("Failed to update telemetry");
//!
//! for device in backend.devices() {
//!     println!("Device {}: {}", device.index, device.board_type);
//!     if let Some(telem) = backend.telemetry(device.index) {
//!         println!("  Power: {:.1}W", telem.power.unwrap_or(0.0));
//!         println!("  Temp: {:.1}°C", telem.asic_temperature.unwrap_or(0.0));
//!     }
//! }
//! ```

// Pre-existing style and complexity lints suppressed crate-wide.
// New code should prefer the idiomatic forms; these allows prevent CI
// failures from legacy code while the codebase is gradually modernised.
#![allow(
    clippy::cmp_owned,
    clippy::collapsible_else_if,
    clippy::collapsible_match,
    clippy::derivable_impls,
    clippy::doc_lazy_continuation,
    clippy::empty_line_after_outer_attr,
    clippy::explicit_counter_loop,
    clippy::identity_op,
    clippy::if_same_then_else,
    clippy::iter_nth,
    clippy::lines_filter_map_ok,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_flatten,
    clippy::manual_hash_one,
    clippy::manual_is_multiple_of,
    clippy::map_flatten,
    clippy::needless_borrow,
    clippy::new_without_default,
    clippy::ptr_arg,
    clippy::redundant_closure,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_map_or,
    clippy::unnecessary_unwrap,
    clippy::unused_enumerate_index,
    clippy::unwrap_or_default
)]

// Public modules - these are the stable API
pub mod animation;
pub mod backend;
pub mod error;
pub mod logging;
pub mod models;

// Process monitoring (Linux-only)
#[cfg(feature = "linux-procfs")]
pub mod workload;

// CLI module - shared by both TUI and GUI
pub mod cli;

// Animation configuration: profiles and user-tunable thresholds
pub mod config;

// UI module - contains both TUI and GUI implementations
pub mod ui;

// Re-exports for convenience
pub use backend::{BackendConfig, TelemetryBackend};
pub use error::{Result, TTTopError};
pub use models::{Architecture, Device, SmbusTelemetry, Telemetry};

/// Library version string
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Library name string
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Full version banner
pub fn version_banner() -> String {
    format!("🦀 {} v{}", NAME, VERSION)
}

/// Initialize logging with the specified level filter
///
/// This sets up logging to both stderr and an internal message buffer
/// that can be accessed via the `logging` module functions.
///
/// This is a convenience function for frontends to set up logging consistently.
pub fn init_logging(level: log::LevelFilter) {
    logging::init_logging_with_buffer(level);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_info() {
        assert!(!VERSION.is_empty());
        assert!(!NAME.is_empty());
        let banner = version_banner();
        assert!(banner.contains(NAME));
        assert!(banner.contains(VERSION));
    }
}
