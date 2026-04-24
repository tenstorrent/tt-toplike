// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Mock backend for testing and development
//!
//! Generates realistic fake telemetry data without requiring actual hardware.
//! Useful for:
//! - Testing UI components
//! - Development without hardware access
//! - Demos and screenshots
//! - CI/CD testing
//!
//! The mock backend simulates realistic hardware behavior:
//! - Variable power consumption (with random fluctuations)
//! - Temperature changes correlated with power
//! - DDR training status progression
//! - ARC firmware heartbeats
//! - Multiple device architectures

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use chrono::Utc;
use std::collections::HashMap;

/// Mock backend that generates fake telemetry data
///
/// Creates a configurable number of virtual devices and generates
/// realistic telemetry data with temporal variation.
///
/// # Example
///
/// ```rust,no_run
/// use tt_toplike::backend::{TelemetryBackend, mock::MockBackend};
///
/// // Create mock backend with 2 devices
/// let mut backend = MockBackend::new(2);
/// backend.init()?;
///
/// // Telemetry varies on each update
/// backend.update()?;
/// println!("Power: {}W", backend.telemetry(0).unwrap().power_w());
///
/// backend.update()?;
/// println!("Power: {}W", backend.telemetry(0).unwrap().power_w()); // Different!
/// ```
pub struct MockBackend {
    /// Number of mock devices to create
    device_count: usize,

    /// List of mock devices
    devices: Vec<Device>,

    /// Current telemetry for each device
    telemetry: HashMap<usize, Telemetry>,

    /// SMBUS telemetry for each device
    smbus_telemetry: HashMap<usize, SmbusTelemetry>,

    /// Configuration
    config: BackendConfig,

    /// Internal state: update counter (for temporal variation)
    update_count: usize,

    /// Base power values for each device (varied from update to update)
    base_power: HashMap<usize, f32>,

    /// Base temperature values for each device
    base_temp: HashMap<usize, f32>,
}

impl MockBackend {
    /// Create a new mock backend with specified number of devices
    ///
    /// # Arguments
    ///
    /// * `device_count` - Number of virtual devices to create (1-16 recommended)
    ///
    /// # Example
    ///
    /// ```rust
    /// let backend = MockBackend::new(3); // 3 mock devices
    /// ```
    pub fn new(device_count: usize) -> Self {
        Self {
            device_count,
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config: BackendConfig::default(),
            update_count: 0,
            base_power: HashMap::new(),
            base_temp: HashMap::new(),
        }
    }

    /// Create mock backend with custom configuration
    pub fn with_config(device_count: usize, config: BackendConfig) -> Self {
        Self {
            device_count,
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config,
            update_count: 0,
            base_power: HashMap::new(),
            base_temp: HashMap::new(),
        }
    }

    /// Generate mock devices with different architectures
    fn generate_devices(&mut self) {
        self.devices.clear();

        for idx in 0..self.device_count {
            // Cycle through architectures: GS, WH, BH, GS, WH, BH, ...
            let (board_type, bus_id) = match idx % 3 {
                0 => (
                    "e150".to_string(),
                    format!("0000:0{}:00.0", idx + 1),
                ), // Grayskull
                1 => (
                    "n150".to_string(),
                    format!("0000:0{}:00.0", idx + 1),
                ), // Wormhole
                _ => (
                    "p150".to_string(),
                    format!("0000:0{}:00.0", idx + 1),
                ), // Blackhole
            };

            let device = Device::new(idx, board_type, bus_id, format!("({},{})", idx / 4, idx % 4));

            self.devices.push(device);
        }

        if self.config.verbose {
            log::info!("MockBackend: Generated {} devices", self.devices.len());
            for device in &self.devices {
                log::debug!("  - {}: {} ({})", device.index, device.name(), device.bus_id);
            }
        }
    }

    /// Initialize base power and temperature for variation
    fn initialize_base_values(&mut self) {
        for idx in 0..self.device_count {
            // Base power varies by architecture
            let base_power = match self.devices[idx].architecture {
                Architecture::Grayskull => 40.0,  // Lower power
                Architecture::Wormhole => 55.0,   // Medium power
                Architecture::Blackhole => 70.0,  // Higher power
                Architecture::Unknown => 50.0,
            };

            // Base temperature correlates with power
            let base_temp = 45.0 + (base_power - 40.0) * 0.5;

            self.base_power.insert(idx, base_power);
            self.base_temp.insert(idx, base_temp);
        }
    }

    /// Generate realistic telemetry for a device
    ///
    /// Creates telemetry with temporal variation:
    /// - Power fluctuates ±10W from base
    /// - Temperature tracks power with lag
    /// - Current derived from power/voltage
    /// - AICLK varies 900-1200 MHz
    fn generate_telemetry(&self, device_idx: usize) -> Telemetry {
        let base_power = self.base_power.get(&device_idx).copied().unwrap_or(50.0);
        let base_temp = self.base_temp.get(&device_idx).copied().unwrap_or(50.0);

        // Add sinusoidal variation + random noise
        let time_factor = (self.update_count as f32) * 0.1;
        let power_variation = (time_factor.sin() * 8.0) + self.random_noise(3.0);
        let temp_variation = (time_factor.sin() * 5.0) + self.random_noise(2.0);

        let power = (base_power + power_variation).max(10.0).min(150.0);
        let temperature = (base_temp + temp_variation).max(25.0).min(95.0);

        // Calculate current from power (assuming ~0.85V nominal)
        let voltage = 0.85 + self.random_noise(0.02);
        let current = power / voltage;

        // AICLK varies 900-1200 MHz
        let aiclk_base = 1000 + (device_idx * 50) as i32;
        let aiclk_variation = (time_factor.cos() * 100.0) as i32;
        let aiclk = (aiclk_base + aiclk_variation).max(0) as u32;

        Telemetry {
            voltage: Some(voltage),
            current: Some(current),
            power: Some(power),
            asic_temperature: Some(temperature),
            aiclk: Some(aiclk.clamp(900, 1200)),
            heartbeat: Some(1), // Always healthy in mock
            timestamp: Utc::now(),
        }
    }

    /// Generate realistic SMBUS telemetry
    fn generate_smbus_telemetry(&self, device_idx: usize) -> SmbusTelemetry {
        let device = &self.devices[device_idx];
        let num_channels = device.memory_channels();

        // Generate DDR status: all channels trained (2 = trained per channel, 2 bits each)
        // Example for 8 channels: 10 10 10 10 10 10 10 10 = 0b10101010_10101010 = 43690
        let ddr_status_value: u32 = (0..num_channels)
            .map(|ch| 2u32 << (ch * 2)) // 2 = trained state
            .sum();

        SmbusTelemetry {
            board_id: Some(format!("BOARD_{:04}", device_idx)),
            enum_version: Some("1.0".to_string()),
            device_id: Some(format!("DEV_{}", device_idx)),
            ddr_speed: Some("6400".to_string()), // 6400 MT/s
            ddr_status: Some(ddr_status_value.to_string()),

            // ARC firmware health (incrementing heartbeats, always > 0)
            arc0_health: Some((((self.update_count + device_idx * 7) % 255) + 1).to_string()),
            arc1_health: Some((((self.update_count + device_idx * 11) % 255) + 1).to_string()),
            arc2_health: Some((((self.update_count + device_idx * 13) % 255) + 1).to_string()),
            arc3_health: Some((((self.update_count + device_idx * 17) % 255) + 1).to_string()),

            // Firmware versions
            arc0_fw_version: Some("1.2.3".to_string()),
            arc1_fw_version: Some("1.2.3".to_string()),
            arc2_fw_version: Some("1.2.3".to_string()),
            arc3_fw_version: Some("1.2.3".to_string()),
            eth_fw_version: Some("2.1.0".to_string()),
            m3_bl_fw_version: Some("0.9.1".to_string()),
            m3_app_fw_version: Some("1.0.5".to_string()),
            spibootrom_fw_version: Some("1.1.0".to_string()),
            tt_flash_version: Some("3.2.1".to_string()),

            // Clock frequencies
            aiclk: Some("1000".to_string()),
            axiclk: Some("500".to_string()),
            arcclk: Some("800".to_string()),

            // Temperatures
            asic_temperature: Some("52".to_string()),
            vreg_temperature: Some("48".to_string()),
            board_temperature: Some("42".to_string()),
            asic_tmon0: Some("51".to_string()),
            asic_tmon1: Some("53".to_string()),

            // Power and limits
            vcore: Some("0.85".to_string()),
            tdp: Some("120".to_string()),
            tdc: Some("150".to_string()),
            throttler: Some("0".to_string()), // Not throttling
            vdd_limits: Some("0.75-0.95".to_string()),
            thm_limits: Some("95".to_string()),
            input_power: Some("50".to_string()),
            board_power_limit: Some("120".to_string()),
            mvddq_power: Some("12".to_string()),

            // Status registers
            fan_speed: Some("3000".to_string()),
            faults: Some("0".to_string()), // No faults
            pcie_status: Some("Gen4x16".to_string()),
            eth_status0: Some("LinkUp".to_string()),
            eth_status1: Some("LinkUp".to_string()),
            eth_debug_status0: Some("0".to_string()),
            eth_debug_status1: Some("0".to_string()),
            aux_status: Some("OK".to_string()),

            // Training and boot info
            gddr_train_temp0: Some("45".to_string()),
            gddr_train_temp1: Some("46".to_string()),
            therm_trip_count: Some("0".to_string()),
            boot_date: Some("2026-01-11".to_string()),
            rt_seconds: Some((self.update_count * 100).to_string()), // Runtime in seconds
            wh_fw_date: Some("2026-01-01".to_string()),
        }
    }

    /// Generate random noise in range [-magnitude, +magnitude]
    fn random_noise(&self, magnitude: f32) -> f32 {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hash, Hasher};

        // Simple deterministic "random" based on update count
        // (not cryptographically secure, just for variation)
        let mut hasher = RandomState::new().build_hasher();
        self.update_count.hash(&mut hasher);
        let hash = hasher.finish();

        let normalized = (hash % 1000) as f32 / 500.0 - 1.0; // -1.0 to +1.0
        normalized * magnitude
    }
}

impl TelemetryBackend for MockBackend {
    fn init(&mut self) -> BackendResult<()> {
        log::info!("MockBackend: Initializing with {} devices", self.device_count);

        if self.device_count == 0 {
            return Err(BackendError::DeviceNotFound(
                "Cannot create mock backend with 0 devices".to_string(),
            ));
        }

        // Generate mock devices
        self.generate_devices();
        self.initialize_base_values();

        // Generate initial telemetry
        for idx in 0..self.device_count {
            let telemetry = self.generate_telemetry(idx);
            let smbus = self.generate_smbus_telemetry(idx);

            self.telemetry.insert(idx, telemetry);
            self.smbus_telemetry.insert(idx, smbus);
        }

        log::info!("MockBackend: Initialization complete");
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        self.update_count += 1;

        // Update telemetry for all devices
        for idx in 0..self.device_count {
            let telemetry = self.generate_telemetry(idx);
            let smbus = self.generate_smbus_telemetry(idx);

            self.telemetry.insert(idx, telemetry);
            self.smbus_telemetry.insert(idx, smbus);
        }

        if self.config.verbose && self.update_count % 10 == 0 {
            log::debug!("MockBackend: Update #{}", self.update_count);
        }

        Ok(())
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry> {
        self.telemetry.get(&device_idx)
    }

    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        self.smbus_telemetry.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        format!("Mock ({} devices)", self.device_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_backend_creation() {
        let backend = MockBackend::new(3);
        assert_eq!(backend.device_count, 3);
        assert_eq!(backend.devices.len(), 0); // Not initialized yet
    }

    #[test]
    fn test_mock_backend_init() {
        let mut backend = MockBackend::new(2);
        assert!(backend.init().is_ok());
        assert_eq!(backend.devices().len(), 2);
        assert!(backend.has_telemetry(0));
        assert!(backend.has_telemetry(1));
        assert!(!backend.has_telemetry(2)); // Out of bounds
    }

    #[test]
    fn test_mock_backend_architectures() {
        let mut backend = MockBackend::new(6);
        backend.init().unwrap();

        // Should cycle through architectures: GS, WH, BH, GS, WH, BH
        assert_eq!(backend.devices()[0].architecture, Architecture::Grayskull);
        assert_eq!(backend.devices()[1].architecture, Architecture::Wormhole);
        assert_eq!(backend.devices()[2].architecture, Architecture::Blackhole);
        assert_eq!(backend.devices()[3].architecture, Architecture::Grayskull);
        assert_eq!(backend.devices()[4].architecture, Architecture::Wormhole);
        assert_eq!(backend.devices()[5].architecture, Architecture::Blackhole);
    }

    #[test]
    fn test_mock_backend_telemetry_variation() {
        let mut backend = MockBackend::new(1);
        backend.init().unwrap();

        let power1 = backend.telemetry(0).unwrap().power_w();

        backend.update().unwrap();
        let power2 = backend.telemetry(0).unwrap().power_w();

        backend.update().unwrap();
        let power3 = backend.telemetry(0).unwrap().power_w();

        // Power should vary between updates
        assert!(power1 != power2 || power2 != power3);
        assert!(power1 > 0.0 && power1 < 200.0);
    }

    #[test]
    fn test_mock_backend_smbus() {
        let mut backend = MockBackend::new(1);
        backend.init().unwrap();

        let smbus = backend.smbus_telemetry(0).unwrap();

        assert!(smbus.ddr_speed.is_some());
        assert!(smbus.ddr_status.is_some());
        assert!(smbus.arc0_health.is_some());
        assert!(smbus.is_arc0_healthy()); // Should be healthy in mock
    }

    #[test]
    fn test_mock_backend_ddr_channels() {
        let mut backend = MockBackend::new(3);
        backend.init().unwrap();

        // Device 0: Grayskull (4 channels)
        assert_eq!(backend.devices()[0].memory_channels(), 4);

        // Device 1: Wormhole (8 channels)
        assert_eq!(backend.devices()[1].memory_channels(), 8);

        // Device 2: Blackhole (12 channels)
        assert_eq!(backend.devices()[2].memory_channels(), 12);
    }

    #[test]
    fn test_mock_backend_zero_devices() {
        let mut backend = MockBackend::new(0);
        assert!(backend.init().is_err()); // Should fail
    }
}
