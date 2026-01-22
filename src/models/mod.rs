//! Data models for Tenstorrent hardware telemetry
//!
//! This module contains all data structures representing hardware information,
//! telemetry data, and device state. These models are designed to be compatible
//! with both luwen (direct hardware) and JSON (subprocess) backends.

pub mod telemetry;
pub mod device;

// Re-export commonly used types
pub use telemetry::{Telemetry, SmbusTelemetry};
pub use device::{Device, Architecture};
