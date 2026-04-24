// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Backend adapters for telemetry data sources
//!
//! This module provides a trait-based abstraction for different telemetry backends.
//! This allows tt-toplike-rs to support multiple data sources:
//!
//! - **MockBackend**: Generates fake data for testing and development
//! - **JSONBackend**: Spawns tt-smi subprocess and parses JSON snapshots
//! - **LuwenBackend**: (Future) Direct hardware access via luwen library
//!
//! ## Architecture
//!
//! All backends implement the `TelemetryBackend` trait, which provides a
//! common interface for device discovery, telemetry updates, and data access.
//! This allows the UI layer to be backend-agnostic.
//!
//! ```
//! ┌─────────────┐
//! │  UI Layer   │
//! └──────┬──────┘
//!        │
//!        ▼ (uses trait)
//! ┌──────────────────┐
//! │ TelemetryBackend │ (trait)
//! └──────────────────┘
//!        △
//!        │ (implements)
//!   ┌────┴────┬──────────┐
//!   │         │          │
//! ┌─▼──┐  ┌──▼───┐  ┌───▼────┐
//! │Mock│  │JSON  │  │Luwen   │
//! └────┘  └──────┘  └────────┘
//! ```

pub mod mock;
pub mod json;          // JSON backend for tt-smi subprocess
pub mod smbus_smooth;  // EMA smoothing for numeric SMBUS fields
#[cfg(feature = "luwen-backend")]
pub mod luwen;  // Luwen backend for direct hardware access
#[cfg(target_os = "linux")]
pub mod sysfs;  // Sysfs backend for Linux hwmon sensors (non-invasive)
#[cfg(target_os = "linux")]
pub mod hybrid;  // Hybrid backend: sysfs real-time + streaming JSON enrichment
pub mod factory;  // Backend factory for dynamic creation and switching

use crate::error::BackendResult;
use crate::models::{Device, Telemetry, SmbusTelemetry};

/// Common interface for all telemetry backends
///
/// This trait defines the contract that all backend implementations must fulfill.
/// It provides methods for initialization, telemetry updates, and data access.
///
/// ## Lifecycle
///
/// 1. **Creation**: Backend is created with configuration
/// 2. **Initialization**: `init()` is called to set up connections/devices
/// 3. **Update Loop**: `update()` is called periodically to refresh telemetry
/// 4. **Data Access**: `devices()`, `telemetry()`, etc. retrieve current data
/// 5. **Cleanup**: Backend is dropped, performing any necessary cleanup
///
/// ## Example
///
/// ```rust,no_run
/// use tt_toplike::backend::{TelemetryBackend, mock::MockBackend};
///
/// let mut backend = MockBackend::new(2); // 2 mock devices
/// backend.init()?;
///
/// loop {
///     backend.update()?;
///
///     for device in backend.devices() {
///         if let Some(telem) = backend.telemetry(device.index) {
///             println!("Device {}: {}W, {}°C",
///                 device.name(), telem.power_w(), telem.temp_c());
///         }
///     }
///
///     std::thread::sleep(std::time::Duration::from_millis(100));
/// }
/// ```
pub trait TelemetryBackend: Send + Sync {
    /// Initialize the backend
    ///
    /// This is called once at startup to establish connections, discover devices,
    /// and prepare for telemetry updates. This method should:
    /// - Discover/enumerate devices
    /// - Establish any necessary connections
    /// - Perform initial telemetry read
    /// - Validate backend requirements (drivers, permissions, etc.)
    ///
    /// # Errors
    ///
    /// Returns `BackendError` if:
    /// - No devices found
    /// - Driver not available
    /// - Permissions insufficient
    /// - Initial telemetry read fails
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// let mut backend = MockBackend::new(1);
    /// backend.init()?; // Discovers devices, performs initial read
    /// ```
    fn init(&mut self) -> BackendResult<()>;

    /// Update telemetry data for all devices
    ///
    /// This is called periodically (typically every 100ms) to refresh telemetry
    /// data from the hardware. This method should:
    /// - Query current telemetry from all devices
    /// - Update internal cache
    /// - Handle transient errors gracefully
    ///
    /// ## Performance Considerations
    ///
    /// This method is called in a hot loop, so it should be efficient:
    /// - Avoid unnecessary allocations
    /// - Use caching where appropriate
    /// - Return quickly (< 50ms target)
    ///
    /// # Errors
    ///
    /// Returns `BackendError` if:
    /// - Telemetry read fails for all devices
    /// - Backend connection lost
    /// - Timeout occurred
    ///
    /// Note: Transient errors for individual devices should be handled gracefully
    /// (e.g., by keeping stale data) rather than failing the entire update.
    fn update(&mut self) -> BackendResult<()>;

    /// Get list of discovered devices
    ///
    /// Returns a slice of all devices discovered during initialization.
    /// The list is stable across updates (devices don't appear/disappear).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// for device in backend.devices() {
    ///     println!("{}: {} ({})",
    ///         device.index, device.name(), device.bus_id);
    /// }
    /// ```
    fn devices(&self) -> &[Device];

    /// Get current telemetry for a specific device
    ///
    /// Returns the most recent telemetry data for the device at the given index.
    /// Returns `None` if:
    /// - Index is out of bounds
    /// - Telemetry not yet available
    /// - Device telemetry read failed
    ///
    /// # Arguments
    ///
    /// * `device_idx` - Zero-based device index (from `devices()`)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// if let Some(telem) = backend.telemetry(0) {
    ///     println!("Power: {}W", telem.power_w());
    ///     println!("Temp: {}°C", telem.temp_c());
    /// }
    /// ```
    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry>;

    /// Get SMBUS telemetry for a specific device
    ///
    /// Returns detailed SMBUS telemetry including DDR status, firmware versions,
    /// ARC health, and other low-level hardware information.
    ///
    /// Returns `None` if:
    /// - Index is out of bounds
    /// - SMBUS data not available for this device
    /// - Backend doesn't support SMBUS telemetry
    ///
    /// # Arguments
    ///
    /// * `device_idx` - Zero-based device index
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// if let Some(smbus) = backend.smbus_telemetry(0) {
    ///     if let Some(speed) = smbus.ddr_speed_mts() {
    ///         println!("DDR Speed: {} MT/s", speed);
    ///     }
    ///     if smbus.is_arc0_healthy() {
    ///         println!("ARC0 firmware: healthy");
    ///     }
    /// }
    /// ```
    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry>;

    /// Get backend name/type for debugging
    ///
    /// Returns a human-readable string identifying the backend implementation.
    /// Used for logging, debugging, and UI display.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// println!("Using backend: {}", backend.backend_info());
    /// // Output: "Using backend: Mock (2 devices)"
    /// ```
    fn backend_info(&self) -> String;

    /// Get number of devices
    ///
    /// Convenience method that returns the number of discovered devices.
    /// Equivalent to `self.devices().len()`.
    fn device_count(&self) -> usize {
        self.devices().len()
    }

    /// Check if telemetry is available for a device
    ///
    /// Returns `true` if telemetry data exists for the given device index.
    /// This is a quick check before calling `telemetry()`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// if backend.has_telemetry(0) {
    ///     let telem = backend.telemetry(0).unwrap();
    ///     // Safe to unwrap because we checked
    /// }
    /// ```
    fn has_telemetry(&self, device_idx: usize) -> bool {
        self.telemetry(device_idx).is_some()
    }

    /// Check if SMBUS telemetry is available for a device
    ///
    /// Returns `true` if SMBUS data exists for the given device index.
    fn has_smbus_telemetry(&self, device_idx: usize) -> bool {
        self.smbus_telemetry(device_idx).is_some()
    }
}

/// Backend configuration options
///
/// Common configuration that can be shared across different backend implementations.
#[derive(Debug, Clone)]
pub struct BackendConfig {
    /// Update interval in milliseconds (default: 100ms = 10 FPS)
    pub update_interval_ms: u64,

    /// Maximum number of consecutive errors before giving up (default: 10)
    pub max_consecutive_errors: usize,

    /// Timeout for telemetry reads in milliseconds (default: 5000ms)
    pub read_timeout_ms: u64,

    /// Enable verbose logging
    pub verbose: bool,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            update_interval_ms: 100,  // 10 FPS
            max_consecutive_errors: 10,
            read_timeout_ms: 5000,
            verbose: false,
        }
    }
}

impl BackendConfig {
    /// Create a new backend config with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set update interval
    pub fn with_interval(mut self, interval_ms: u64) -> Self {
        self.update_interval_ms = interval_ms;
        self
    }

    /// Set max errors
    pub fn with_max_errors(mut self, max_errors: usize) -> Self {
        self.max_consecutive_errors = max_errors;
        self
    }

    /// Enable verbose logging
    pub fn verbose(mut self) -> Self {
        self.verbose = true;
        self
    }
}

// Implement TelemetryBackend for Box<dyn TelemetryBackend> to allow easier use
impl TelemetryBackend for Box<dyn TelemetryBackend> {
    fn init(&mut self) -> BackendResult<()> {
        (**self).init()
    }

    fn update(&mut self) -> BackendResult<()> {
        (**self).update()
    }

    fn devices(&self) -> &[Device] {
        (**self).devices()
    }

    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry> {
        (**self).telemetry(device_idx)
    }

    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        (**self).smbus_telemetry(device_idx)
    }

    fn backend_info(&self) -> String {
        (**self).backend_info()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_config_default() {
        let config = BackendConfig::default();
        assert_eq!(config.update_interval_ms, 100);
        assert_eq!(config.max_consecutive_errors, 10);
        assert_eq!(config.read_timeout_ms, 5000);
        assert!(!config.verbose);
    }

    #[test]
    fn test_backend_config_builder() {
        let config = BackendConfig::new()
            .with_interval(50)
            .with_max_errors(20)
            .verbose();

        assert_eq!(config.update_interval_ms, 50);
        assert_eq!(config.max_consecutive_errors, 20);
        assert!(config.verbose);
    }
}
