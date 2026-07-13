// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Backend adapters for telemetry data sources
//!
//! This module provides a trait-based abstraction for different telemetry backends.
//! This allows tt-toplike to support multiple data sources:
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

#[cfg(target_os = "macos")]
pub mod ane_macos; // Apple ANE power via private IOReport (no sudo)
#[cfg(feature = "remote")]
pub mod discovery; // Remote-box discovery via `tt --json discover` (--remote <name>, /remote)
pub mod factory;
#[cfg(target_os = "macos")]
pub mod gpu_macos; // Apple GPU stats via ioreg (no sudo)
pub mod host; // Host backend: any machine via sysinfo (no TT hardware required)
#[cfg(target_os = "linux")]
pub mod hybrid; // Hybrid backend: sysfs real-time + streaming JSON enrichment
pub mod json; // JSON backend for tt-smi subprocess
#[cfg(feature = "luwen-backend")]
pub mod luwen; // Luwen backend for direct hardware access
pub mod mock;
#[cfg(feature = "remote")]
mod remote_ext; // tt_toplike frame extension: processes + inference riding on /telemetry frames
#[cfg(feature = "remote")]
pub use remote_ext::*;
#[cfg(feature = "remote")]
mod serve; // TelemetryPublisher: /telemetry WebSocket server (the --serve side of remote QuietBox)
#[cfg(feature = "remote")]
pub use serve::TelemetryPublisher;
pub mod smbus_smooth; // EMA smoothing for numeric SMBUS fields
#[cfg(target_os = "linux")]
pub mod sysfs; // Sysfs backend for Linux hwmon sensors (non-invasive)
#[cfg(feature = "remote")]
pub mod ws; // WebSocket backend: remote QuietBox telemetry over the LAN (opt-in --remote, `remote` feature)

use crate::error::BackendResult;
use crate::models::{Device, SmbusTelemetry, Telemetry};

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

    /// Get the current telemetry snapshot as raw `tt-smi -s` JSON, if available.
    ///
    /// This is the passthrough/relay hook for the `--serve` telemetry publisher:
    /// a publisher broadcasts "current telemetry" by grabbing whatever the active
    /// backend last saw as `tt-smi -s` output, without re-deriving JSON from the
    /// backend's parsed `Telemetry`/`SmbusTelemetry` structs (which would be lossy
    /// and backend-specific).
    ///
    /// - [`json::JSONBackend`](crate::backend::json::JSONBackend) and
    ///   [`hybrid::HybridBackend`](crate::backend::hybrid::HybridBackend) return the
    ///   verbatim stdout of their last successful `tt-smi -s` run.
    /// - [`ws::WsBackend`](crate::backend::ws::WsBackend) relays the last frame it
    ///   received from a remote publisher (itself verbatim `tt-smi -s` stdout).
    /// - Every other backend (mock/host/sysfs/luwen) has no `tt-smi` JSON source
    ///   and keeps this default, returning `None`.
    ///
    /// Cheap by design: a getter over already-retained state, never spawns a
    /// subprocess or blocks — safe to call from a hot loop.
    fn snapshot_json(&self) -> Option<String> {
        None
    }

    /// Decoded remote process list from the `tt_toplike` frame extension, if
    /// this backend is streaming one.
    ///
    /// Only [`ws::WsBackend`](crate::backend::ws::WsBackend) overrides this
    /// (it decodes the extension from the latest `--serve` frame). Every other
    /// backend keeps this default `None`, which the UI treats identically to
    /// "extension present but this sub-key wasn't streamed": fall back to the
    /// LOCAL process scan. Cheap by contract — a clone of already-decoded
    /// state, never a probe — safe to call every render tick.
    #[cfg(feature = "remote")]
    fn remote_processes(&self) -> Option<Vec<crate::backend::RemoteProc>> {
        None
    }

    /// Decoded remote inference-workload list from the `tt_toplike` frame
    /// extension, if this backend is streaming one. See
    /// [`Self::remote_processes`] for the `None` contract and cost.
    #[cfg(feature = "remote")]
    fn remote_inference(&self) -> Option<Vec<crate::backend::RemoteInference>> {
        None
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
            update_interval_ms: 100, // 10 FPS
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

    /// Set the telemetry read timeout in milliseconds.
    ///
    /// Used by backends that wait on a data source — notably `WsBackend`, whose
    /// connect / first-frame wait derives from this value. Without wiring this,
    /// `--timeout` had no effect and the WS path was stuck at the 5000ms default.
    pub fn with_read_timeout_ms(mut self, read_timeout_ms: u64) -> Self {
        self.read_timeout_ms = read_timeout_ms;
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

    fn snapshot_json(&self) -> Option<String> {
        // Without this explicit forward, the default `{ None }` body from the
        // trait definition would apply here (it never touches `self`), silently
        // discarding whatever the boxed concrete backend (JSON/Hybrid/Ws) has
        // retained. Every app surface stores its backend as `Box<dyn
        // TelemetryBackend>` (TUI, egui, iced GUI), so this forward is what makes
        // `snapshot_json()` actually reach the real backend in practice.
        (**self).snapshot_json()
    }

    // Same rationale as `snapshot_json` above: without an explicit forward,
    // the boxed concrete backend's (WsBackend's) override would never be
    // reached through `Box<dyn TelemetryBackend>` — every app surface stores
    // its backend that way.
    #[cfg(feature = "remote")]
    fn remote_processes(&self) -> Option<Vec<crate::backend::RemoteProc>> {
        (**self).remote_processes()
    }

    #[cfg(feature = "remote")]
    fn remote_inference(&self) -> Option<Vec<crate::backend::RemoteInference>> {
        (**self).remote_inference()
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

    /// Backends that don't override `remote_processes`/`remote_inference`
    /// (i.e. everything except `WsBackend`) must keep the trait default of
    /// `None` — that's what tells the UI to fall back to LOCAL data.
    #[cfg(feature = "remote")]
    #[test]
    fn test_remote_accessors_default_to_none() {
        use crate::backend::mock::MockBackend;
        let mock = MockBackend::new(1);
        assert_eq!(mock.remote_processes(), None);
        assert_eq!(mock.remote_inference(), None);
    }

    /// `WsBackend`'s decoded extension must still be reachable through
    /// `Box<dyn TelemetryBackend>` — the shape every app surface actually
    /// stores its backend as. Without the explicit forwarding methods on
    /// `impl TelemetryBackend for Box<dyn TelemetryBackend>`, this would
    /// silently fall back to the trait's default `None` regardless of what
    /// the concrete `WsBackend` decoded.
    #[cfg(feature = "remote")]
    #[test]
    fn test_boxed_backend_forwards_remote_accessors() {
        use crate::backend::remote_ext::{
            inject_extension, RemoteInference, RemoteProc, TtToplikeExt, TT_TOPLIKE_SCHEMA,
        };
        use crate::backend::ws::WsBackend;

        const MINIMAL_FRAME: &str = r#"{
            "device_info": [{
                "board_info": { "board_type": "p150a", "bus_id": "0000:01:00.0", "coords": "(0,0)" },
                "telemetry": { "voltage": "0.85", "current": "30.0", "power": "145.0",
                    "aiclk": "800", "asic_temperature": "52.0", "heartbeat": "1234" },
                "smbus_telem": { "DDR_STATUS": "0x55555555" }
            }]
        }"#;
        let ext = TtToplikeExt {
            schema: TT_TOPLIKE_SCHEMA,
            processes: Some(vec![RemoteProc {
                pid: 1,
                name: "p".into(),
                cmd: "p".into(),
                uses_tt: true,
                cpu_pct: 1.0,
                mem_bytes: 1,
            }]),
            inference: Some(vec![RemoteInference {
                key: "k".into(),
                label: "L".into(),
                phase: "ready".into(),
                progress: None,
                serving: None,
                media: None,
            }]),
        };
        let frame = inject_extension(MINIMAL_FRAME, &ext);

        let mut ws = WsBackend::new_for_test();
        ws.test_inject(&frame);
        assert!(ws.update().is_ok());

        // Sanity: the concrete backend decoded it.
        assert!(ws.remote_processes().is_some());

        // The real assertion: boxing must not lose the override.
        let boxed: Box<dyn TelemetryBackend> = Box::new(ws);
        assert_eq!(
            boxed
                .remote_processes()
                .expect("boxed backend should forward to WsBackend's decoded extension")
                .len(),
            1
        );
        assert_eq!(
            boxed
                .remote_inference()
                .expect("boxed backend should forward to WsBackend's decoded extension")
                .len(),
            1
        );
    }
}
