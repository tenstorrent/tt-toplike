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
            telemetry_cache: HashMap::new(),
        }
    }

    /// Scan /sys/class/hwmon/ for Tenstorrent devices
    fn detect_devices(&mut self) -> BackendResult<()> {
        log::info!("SysfsBackend: Scanning /sys/class/hwmon/");

        let hwmon_base = Path::new("/sys/class/hwmon");
        if !hwmon_base.exists() {
            return Err(BackendError::Initialization(
                "Hwmon sysfs not available (Linux-specific)".to_string(),
            ));
        }

        let entries = fs::read_dir(hwmon_base)
            .map_err(|e| BackendError::Initialization(format!("Failed to read hwmon dir: {}", e)))?;

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
                    log::info!("SysfsBackend: Found Tenstorrent device: {} at {:?}", name, path);

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
                    let bus_id = self.extract_pci_address(&path)
                        .unwrap_or_else(|| format!("hwmon{}", device_idx));

                    let device = Device {
                        index: device_idx,
                        board_type: name.to_string(),
                        bus_id: bus_id.clone(),
                        coords: String::new(),
                        architecture,
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

    /// Read temperature from hwmon sensor (returns Celsius)
    fn read_temperature(&self, hwmon_path: &Path) -> Option<f32> {
        // Try common temperature input patterns: temp1_input, temp2_input, etc.
        for i in 1..=8 {
            let temp_path = hwmon_path.join(format!("temp{}_input", i));
            if let Ok(content) = fs::read_to_string(&temp_path) {
                if let Ok(millicelsius) = content.trim().parse::<i32>() {
                    return Some(millicelsius as f32 / 1000.0); // Convert mC to C
                }
            }
        }
        None
    }

    /// Read voltage from hwmon sensor (returns Volts)
    fn read_voltage(&self, hwmon_path: &Path) -> Option<f32> {
        // Try common voltage input patterns: in0_input, in1_input, etc.
        for i in 0..=8 {
            let volt_path = hwmon_path.join(format!("in{}_input", i));
            if let Ok(content) = fs::read_to_string(&volt_path) {
                if let Ok(millivolts) = content.trim().parse::<i32>() {
                    return Some(millivolts as f32 / 1000.0); // Convert mV to V
                }
            }
        }
        None
    }

    /// Read power from hwmon sensor (returns Watts)
    fn read_power(&self, hwmon_path: &Path) -> Option<f32> {
        // Try common power input patterns: power1_input, power2_input, etc.
        for i in 1..=8 {
            let power_path = hwmon_path.join(format!("power{}_input", i));
            if let Ok(content) = fs::read_to_string(&power_path) {
                if let Ok(microwatts) = content.trim().parse::<i64>() {
                    return Some(microwatts as f32 / 1_000_000.0); // Convert µW to W
                }
            }
        }
        None
    }

    /// Read current from hwmon sensor (returns Amperes)
    fn read_current(&self, hwmon_path: &Path) -> Option<f32> {
        // Try common current input patterns: curr1_input, curr2_input, etc.
        for i in 1..=8 {
            let curr_path = hwmon_path.join(format!("curr{}_input", i));
            if let Ok(content) = fs::read_to_string(&curr_path) {
                if let Ok(milliamps) = content.trim().parse::<i32>() {
                    return Some(milliamps as f32 / 1000.0); // Convert mA to A
                }
            }
        }
        None
    }
}

impl Default for SysfsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryBackend for SysfsBackend {
    fn init(&mut self) -> BackendResult<()> {
        self.detect_devices()
    }

    fn update(&mut self) -> BackendResult<()> {
        for (device_idx, hwmon_path) in &self.hwmon_paths {
            let temperature = self.read_temperature(hwmon_path);
            let voltage = self.read_voltage(hwmon_path);
            let power = self.read_power(hwmon_path);
            let current = self.read_current(hwmon_path);

            // If power and voltage available but not current, calculate it
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
                aiclk: None, // Not available via hwmon
                heartbeat: None, // Not available via hwmon
            };

            self.telemetry_cache.insert(*device_idx, telemetry);
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
