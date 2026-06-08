// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Sysfs sensor backend for reading hardware telemetry via Linux hwmon
//!
//! This backend provides a non-invasive fallback by reading sensor data directly
//! from `/sys/class/hwmon/` without requiring PCI access or special permissions.
//!
//! # When to Use
//!
//! - Hardware is actively running workloads (LLMs, training, etc.)
//! - Luwen backend fails due to BAR0 mapping conflicts
//! - No special permissions available (no sudo, no ttkmd module)
//! - Read-only telemetry monitoring desired
//!
//! # Limitations
//!
//! - Only provides basic metrics (temperature, voltage, power if available)
//! - No SMBUS telemetry (firmware versions, DDR status, etc.)
//! - Sensor naming/availability varies by kernel driver
//! - May not detect all devices if driver doesn't expose hwmon

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Valid sysfs file paths for each sensor type on one device.
/// Populated once during `init()` so `update()` never retries missing paths.
#[derive(Default)]
struct SensorPaths {
    temperature: Option<PathBuf>, // tempN_input (millicelsius)
    voltage: Option<PathBuf>,     // inN_input   (millivolts)
    power: Option<PathBuf>,       // powerN_input (microwatts)
    current: Option<PathBuf>,     // currN_input  (milliamperes)
}

/// Sysfs sensor backend implementation
///
/// Reads telemetry from `/sys/class/hwmon/hwmon*` entries
pub struct SysfsBackend {
    /// Backend configuration (reserved for future use)
    #[allow(dead_code)]
    config: BackendConfig,

    /// Detected devices with their hwmon paths
    devices: Vec<Device>,

    /// Mapping of device index to hwmon directory path
    hwmon_paths: HashMap<usize, PathBuf>,

    /// Cached valid sensor file paths per device — populated in init().
    sensor_paths: HashMap<usize, SensorPaths>,

    /// Cached telemetry data (per device index)
    telemetry_cache: HashMap<usize, Telemetry>,
}

impl SysfsBackend {
    /// Create a new Sysfs backend with default configuration
    pub fn new() -> Self {
        Self::with_config(BackendConfig::default())
    }

    /// Create a new Sysfs backend with custom configuration
    pub fn with_config(config: BackendConfig) -> Self {
        Self {
            config,
            devices: Vec::new(),
            hwmon_paths: HashMap::new(),
            sensor_paths: HashMap::new(),
            telemetry_cache: HashMap::new(),
        }
    }

    /// Scan /sys/class/hwmon/ for Tenstorrent devices
    ///
    /// Clears existing state before scanning so that calling init() twice does not
    /// double-count devices. This happens in practice when the tui.rs binary pre-probes
    /// the backend and then run_with_backend() calls init() a second time.
    fn detect_devices(&mut self) -> BackendResult<()> {
        log::info!("SysfsBackend: Scanning /sys/class/hwmon/");

        // Clear any previously discovered state so repeated init() calls are idempotent.
        self.devices.clear();
        self.hwmon_paths.clear();
        self.sensor_paths.clear();
        self.telemetry_cache.clear();

        let hwmon_base = Path::new("/sys/class/hwmon");
        if !hwmon_base.exists() {
            return Err(BackendError::Initialization(
                "Hwmon sysfs not available (Linux-specific)".to_string(),
            ));
        }

        let entries = fs::read_dir(hwmon_base).map_err(|e| {
            BackendError::Initialization(format!("Failed to read hwmon dir: {}", e))
        })?;

        let mut device_idx = 0;
        for entry in entries.flatten() {
            let path = entry.path();

            // Check if this is a Tenstorrent device by reading name
            let name_path = path.join("name");
            if let Ok(name) = fs::read_to_string(&name_path) {
                let name = name.trim();

                // Look for Tenstorrent-related hwmon names
                // Common patterns: "tenstorrent", "tt_*", "grayskull", "wormhole", "blackhole"
                if name.contains("tenstorrent")
                    || name.starts_with("tt_")
                    || name.contains("grayskull")
                    || name.contains("wormhole")
                    || name.contains("blackhole")
                {
                    log::info!(
                        "SysfsBackend: Found Tenstorrent device: {} at {:?}",
                        name,
                        path
                    );

                    // Try to determine architecture from name
                    let architecture = if name.contains("grayskull") {
                        Architecture::Grayskull
                    } else if name.contains("wormhole") {
                        Architecture::Wormhole
                    } else if name.contains("blackhole") {
                        Architecture::Blackhole
                    } else {
                        Architecture::Unknown
                    };

                    // Try to extract PCI address from device path
                    let bus_id = self
                        .extract_pci_address(&path)
                        .unwrap_or_else(|| format!("hwmon{}", device_idx));

                    let device = Device {
                        index: device_idx,
                        board_type: name.to_string(),
                        bus_id: bus_id.clone(),
                        coords: String::new(),
                        architecture,
                        firmwares: None,
                        limits: None,
                        pcie_speed: None,
                        pcie_width: None,
                    };

                    self.devices.push(device);
                    self.hwmon_paths.insert(device_idx, path);
                    device_idx += 1;
                }
            }
        }

        if self.devices.is_empty() {
            return Err(BackendError::Initialization(
                "No Tenstorrent devices found in hwmon".to_string(),
            ));
        }

        log::info!("SysfsBackend: Found {} devices", self.devices.len());
        Ok(())
    }

    /// Try to enrich device board_type from `tt-smi -s` output.
    ///
    /// The hwmon driver only exposes the chip architecture name (e.g. "blackhole"),
    /// not the board SKU (e.g. "p300c"). This runs tt-smi once at init and matches
    /// devices by PCI bus_id to fill in the real product name. Fails silently if
    /// tt-smi is unavailable or its output cannot be parsed.
    ///
    /// Only tt-smi v4+ is used. Earlier versions access hardware invasively via
    /// direct PCI reads, which can disrupt running workloads. v4+ switched to a
    /// safe snapshot mode that reads from the kernel driver.
    fn try_enrich_board_types_from_tt_smi(&mut self) {
        // Gate on tt-smi major version >= 4 before calling -s.
        // `tt-smi --version` prints just the version string, e.g. "5.2.0".
        let ver_out = match Command::new("tt-smi").arg("--version").output() {
            Ok(o) if o.status.success() => o,
            _ => return, // tt-smi not found or --version failed
        };
        let ver_str = String::from_utf8_lossy(&ver_out.stdout);
        let major: u32 = ver_str
            .trim()
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if major < 4 {
            log::warn!(
                "SysfsBackend: tt-smi version {} is < 4 — skipping board-type enrichment \
                 to avoid invasive PCI access",
                ver_str.trim()
            );
            return;
        }

        #[derive(serde::Deserialize)]
        struct BoardInfo {
            bus_id: Option<String>,
            board_type: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct RawDev {
            board_info: Option<BoardInfo>,
        }
        #[derive(serde::Deserialize)]
        struct Snapshot {
            device_info: Option<Vec<RawDev>>,
        }

        let output = Command::new("tt-smi").arg("-s").output();
        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return, // tt-smi not available — silently skip
        };
        let json = match std::str::from_utf8(&output.stdout) {
            Ok(s) => s,
            Err(_) => return,
        };
        let snapshot: Snapshot = match serde_json::from_str(json) {
            Ok(s) => s,
            Err(_) => return,
        };
        let raw_devs = match snapshot.device_info {
            Some(d) => d,
            None => return,
        };

        // Build a map: normalized bus_id → board_type
        let mut sku_by_bus: HashMap<String, String> = HashMap::new();
        for raw in raw_devs {
            if let Some(bi) = raw.board_info {
                if let (Some(bus), Some(bt)) = (bi.bus_id, bi.board_type) {
                    let bus_norm = bus.trim().to_lowercase();
                    if !bus_norm.is_empty() && !bt.is_empty() {
                        sku_by_bus.insert(bus_norm, bt);
                    }
                }
            }
        }
        if sku_by_bus.is_empty() {
            return;
        }

        for device in &mut self.devices {
            let bus_norm = device.bus_id.trim().to_lowercase();
            if let Some(sku) = sku_by_bus.get(&bus_norm) {
                device.board_type = sku.clone();
            }
        }
        log::info!("SysfsBackend: enriched board types from tt-smi");
    }

    /// Extract PCI address from hwmon device path
    /// Example: /sys/class/hwmon/hwmon3 -> /sys/devices/pci0000:00/0000:00:01.0/...
    fn extract_pci_address(&self, hwmon_path: &Path) -> Option<String> {
        // Read the device link to find real device path
        let device_link = hwmon_path.join("device");
        if let Ok(real_path) = fs::read_link(&device_link) {
            let path_str = real_path.to_string_lossy();

            // Look for PCI address pattern: 0000:00:00.0
            for component in path_str.split('/') {
                if component.len() >= 12
                    && component.chars().nth(4) == Some(':')
                    && component.chars().nth(7) == Some(':')
                {
                    return Some(component.to_string());
                }
            }
        }
        None
    }

    /// Find valid sensor file paths for one hwmon device.
    /// Called once in `init()` so `update()` can read directly without retrying.
    fn discover_sensor_paths(hwmon_path: &Path) -> SensorPaths {
        let mut paths = SensorPaths::default();

        for i in 1..=8 {
            if paths.temperature.is_none() {
                let p = hwmon_path.join(format!("temp{}_input", i));
                if p.exists() {
                    paths.temperature = Some(p);
                }
            }
            if paths.power.is_none() {
                let p = hwmon_path.join(format!("power{}_input", i));
                if p.exists() {
                    paths.power = Some(p);
                }
            }
            if paths.current.is_none() {
                let p = hwmon_path.join(format!("curr{}_input", i));
                if p.exists() {
                    paths.current = Some(p);
                }
            }
        }
        for i in 0..=8 {
            if paths.voltage.is_none() {
                let p = hwmon_path.join(format!("in{}_input", i));
                if p.exists() {
                    paths.voltage = Some(p);
                }
            }
        }

        paths
    }

    /// Read a millicelsius sysfs file and convert to Celsius.
    fn read_temperature_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok().map(|mc| mc as f32 / 1000.0))
    }

    /// Read a millivolt sysfs file and convert to Volts.
    fn read_voltage_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok().map(|mv| mv as f32 / 1000.0))
    }

    /// Read a microwatt sysfs file and convert to Watts.
    fn read_power_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path).ok().and_then(|s| {
            s.trim()
                .parse::<i64>()
                .ok()
                .map(|uw| uw as f32 / 1_000_000.0)
        })
    }

    /// Read a milliampere sysfs file and convert to Amperes.
    fn read_current_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok().map(|ma| ma as f32 / 1000.0))
    }
}

impl Default for SysfsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SysfsBackend {
    /// Mutable access to the device list — used by HybridBackend to enrich
    /// sysfs devices with firmware/limits data obtained from the JSON snapshot.
    pub(crate) fn devices_mut(&mut self) -> &mut [Device] {
        &mut self.devices
    }

    /// Mutable access to a cached telemetry entry — used by HybridBackend to
    /// overlay firmware-reported TDP/TDC values on top of the hwmon reading.
    pub(crate) fn telemetry_cache_mut(&mut self, device_idx: usize) -> Option<&mut Telemetry> {
        self.telemetry_cache.get_mut(&device_idx)
    }

    /// Insert a telemetry entry directly into the cache — test helper only.
    #[cfg(test)]
    pub(crate) fn insert_telemetry_for_test(&mut self, device_idx: usize, telem: Telemetry) {
        self.telemetry_cache.insert(device_idx, telem);
    }
}

impl TelemetryBackend for SysfsBackend {
    fn init(&mut self) -> BackendResult<()> {
        self.detect_devices()?;
        // Attempt to replace generic arch names (e.g. "blackhole") with real
        // board SKUs (e.g. "p300c") using a one-shot tt-smi -s call.
        self.try_enrich_board_types_from_tt_smi();
        // Cache valid sensor paths so update() never retries missing indices.
        for (device_idx, hwmon_path) in &self.hwmon_paths {
            let paths = Self::discover_sensor_paths(hwmon_path);
            self.sensor_paths.insert(*device_idx, paths);
        }
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        for device_idx in 0..self.devices.len() {
            let paths = match self.sensor_paths.get(&device_idx) {
                Some(p) => p,
                None => continue,
            };

            let temperature = paths
                .temperature
                .as_deref()
                .and_then(Self::read_temperature_path);
            let voltage = paths.voltage.as_deref().and_then(Self::read_voltage_path);
            let power = paths.power.as_deref().and_then(Self::read_power_path);
            let current = paths.current.as_deref().and_then(Self::read_current_path);

            // If power and voltage available but not current, calculate it.
            let calculated_current = match (power, current, voltage) {
                (Some(p), None, Some(v)) if v > 0.0 => Some(p / v),
                _ => current,
            };

            let telemetry = Telemetry {
                timestamp: chrono::Utc::now(),
                voltage,
                current: calculated_current,
                power,
                asic_temperature: temperature,
                aiclk: None,
                heartbeat: None,
            };

            self.telemetry_cache.insert(device_idx, telemetry);
        }

        Ok(())
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn device_count(&self) -> usize {
        self.devices.len()
    }

    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry> {
        self.telemetry_cache.get(&device_idx)
    }

    fn smbus_telemetry(&self, _device_idx: usize) -> Option<&SmbusTelemetry> {
        // SMBUS telemetry not available via sysfs hwmon
        None
    }

    fn backend_info(&self) -> String {
        "Sysfs (hwmon sensors)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sysfs_backend_creation() {
        let backend = SysfsBackend::new();
        assert_eq!(backend.backend_info(), "Sysfs (hwmon sensors)");
        assert_eq!(backend.device_count(), 0);
    }

    #[test]
    fn test_sysfs_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = SysfsBackend::with_config(config);
        assert_eq!(backend.config.update_interval_ms, 50);
    }

    // Note: Actual device detection tests require real hardware or mocked filesystem
}
