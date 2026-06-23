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
use crate::models::telemetry::{DeviceLimits, FirmwaresInfo, GddrTempPair};
use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use chrono::Utc;
use std::collections::HashMap;

/// Pre-defined mock hardware scenarios for testing at various scales.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MockScenario {
    /// Default: cycles GS/WH/BH by device index (existing behavior).
    Mixed,
    /// Single Galaxy: 32 Blackhole chips.
    Galaxy,
    /// Quad Galaxy: 128 Blackhole chips across 4 Galaxy systems.
    QuadGalaxy,
    /// Pure Wormhole cluster: 32 WH chips.
    WormholeCluster,
}

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

    /// Scenario controlling device count and architecture.
    scenario: MockScenario,

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
            scenario: MockScenario::Mixed,
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
            scenario: MockScenario::Mixed,
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config,
            update_count: 0,
            base_power: HashMap::new(),
            base_temp: HashMap::new(),
        }
    }

    /// Create mock backend with a predefined hardware scenario.
    ///
    /// # Example
    ///
    /// ```rust
    /// use tt_toplike::backend::mock::{MockBackend, MockScenario};
    /// use tt_toplike::backend::TelemetryBackend;
    ///
    /// let mut backend = MockBackend::with_scenario(MockScenario::Galaxy);
    /// backend.init().unwrap(); // still required
    /// assert_eq!(backend.devices().len(), 32);
    /// ```
    pub fn with_scenario(scenario: MockScenario) -> Self {
        let device_count = match scenario {
            MockScenario::Mixed => 6,
            MockScenario::Galaxy => 32,
            MockScenario::QuadGalaxy => 128,
            MockScenario::WormholeCluster => 32,
        };
        Self {
            device_count,
            scenario,
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config: BackendConfig::default(),
            update_count: 0,
            base_power: HashMap::new(),
            base_temp: HashMap::new(),
        }
    }

    /// Generate mock devices with different architectures
    fn generate_devices(&mut self) {
        self.devices.clear();

        for idx in 0..self.device_count {
            let board_type = match self.scenario {
                MockScenario::Galaxy | MockScenario::QuadGalaxy => "p150a".to_string(),
                MockScenario::WormholeCluster => "n150".to_string(),
                MockScenario::Mixed => match idx % 3 {
                    0 => "e150".to_string(),
                    1 => "n150".to_string(),
                    _ => "p150".to_string(),
                },
            };
            let bus_id = format!("0000:{:02x}:00.0", (idx % 256) + 1);
            let mut device = Device::new(
                idx,
                board_type,
                bus_id,
                format!("({},{})", idx / 8, idx % 8),
            );

            // Populate firmwares and limits for all scenarios
            device.firmwares = Some(FirmwaresInfo {
                fw_bundle_version: Some("fw_pack-19.9.0".to_string()),
                eth_fw: Some("6.16.0.0".to_string()),
                cm_fw: Some("v80.15.0.0".to_string()),
                gddr_fw: Some("1.37".to_string()),
            });
            device.limits = Some(DeviceLimits {
                tdp_limit: Some(300.0),
                tdc_limit: Some(40.0),
                asic_fmax: Some(800),
                thm_limit: Some(105.0),
            });

            self.devices.push(device);
        }

        if self.config.verbose {
            log::info!("MockBackend: Generated {} devices", self.devices.len());
            for device in &self.devices {
                log::debug!(
                    "  - {}: {} ({})",
                    device.index,
                    device.name(),
                    device.bus_id
                );
            }
        }
    }

    /// Initialize base power and temperature for variation
    fn initialize_base_values(&mut self) {
        for idx in 0..self.device_count {
            // Base power varies by architecture
            let base_power = match self.devices[idx].architecture {
                Architecture::Grayskull => 40.0, // Lower power
                Architecture::Wormhole => 55.0,  // Medium power
                Architecture::Blackhole => 70.0, // Higher power
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

        // AICLK varies 900-1200 MHz; use modulo to keep base in range for any device count
        let aiclk_base = 900 + ((device_idx % 7) * 43) as i32; // 900..1100 range, cycles per 7 devices
        let aiclk_variation = (time_factor.cos() * 100.0) as i32;
        let aiclk = (aiclk_base + aiclk_variation).clamp(900, 1200) as u32;

        Telemetry {
            voltage: Some(voltage),
            current: Some(current),
            power: Some(power),
            asic_temperature: Some(temperature),
            aiclk: Some(aiclk),
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

        // Deterministic "random" harvesting for QuadGalaxy: device idx % 17 == 0 harvests col 0
        let is_harvested = self.scenario == MockScenario::QuadGalaxy && device_idx % 17 == 0;
        let enabled_tensix_col = if is_harvested {
            Some(0x3FFE_u32) // col 0 harvested
        } else {
            Some(0x3FFF_u32) // all 14 cols active
        };

        // GDDR temps: base 38-44°C with sinusoidal variation
        let t_base = 38.0_f32 + (device_idx % 4) as f32 * 2.0;
        let t_var = ((self.update_count as f32 * 0.1).sin() * 4.0).abs();

        // ETH live status: ~75% ports live (alternating bit pattern)
        let eth_live = 0xAAAAAAAAAAAAAAAA_u64;

        let harvesting_state = if is_harvested {
            Some(1_u32)
        } else {
            Some(0_u32)
        };

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

            // tt-smi 5.2.0 fields
            gddr_temps: [
                Some(GddrTempPair([
                    t_base + t_var,
                    t_base + t_var + 2.0,
                    t_base + t_var + 4.0,
                    t_base + t_var + 6.0,
                ])),
                Some(GddrTempPair([
                    t_base + t_var,
                    t_base + t_var + 2.0,
                    t_base + t_var + 4.0,
                    t_base + t_var + 6.0,
                ])),
                Some(GddrTempPair([
                    t_base + t_var,
                    t_base + t_var + 2.0,
                    t_base + t_var + 4.0,
                    t_base + t_var + 6.0,
                ])),
                Some(GddrTempPair([
                    t_base + t_var,
                    t_base + t_var + 2.0,
                    t_base + t_var + 4.0,
                    t_base + t_var + 6.0,
                ])),
            ],
            max_gddr_temp: Some(t_base + t_var + 6.0),
            gddr_corr_errs: [None; 4],
            gddr_uncorr_errs: None,
            harvesting_state,
            eth_live_status: Some(eth_live),
            enabled_eth: None,
            enabled_gddr: None,
            enabled_l2cpu: None,
            enabled_tensix_col,
        }
    }

    /// Generate random noise in range [-magnitude, +magnitude]
    fn random_noise(&self, magnitude: f32) -> f32 {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hash, Hasher};

        // Non-deterministic noise via OS-seeded hasher; update_count seeds additional spread
        let mut hasher = RandomState::new().build_hasher();
        self.update_count.hash(&mut hasher);
        let hash = hasher.finish();

        let normalized = (hash % 1000) as f32 / 500.0 - 1.0; // -1.0 to +1.0
        normalized * magnitude
    }
}

impl TelemetryBackend for MockBackend {
    fn init(&mut self) -> BackendResult<()> {
        log::info!(
            "MockBackend: Initializing with {} devices",
            self.device_count
        );

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

    #[test]
    fn test_galaxy_mock_has_32_devices() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        assert_eq!(b.devices().len(), 32);
        assert!(b
            .devices()
            .iter()
            .all(|d| d.architecture == Architecture::Blackhole));
    }

    #[test]
    fn test_quad_galaxy_mock_has_128_devices() {
        let mut b = MockBackend::with_scenario(MockScenario::QuadGalaxy);
        b.init().unwrap();
        assert_eq!(b.devices().len(), 128);
        assert!(b
            .devices()
            .iter()
            .all(|d| d.architecture == Architecture::Blackhole));
    }

    #[test]
    fn test_wormhole_cluster_mock() {
        let mut b = MockBackend::with_scenario(MockScenario::WormholeCluster);
        b.init().unwrap();
        assert_eq!(b.devices().len(), 32);
        assert!(b
            .devices()
            .iter()
            .all(|d| d.architecture == Architecture::Wormhole));
    }

    #[test]
    fn test_mock_firmwares_populated() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        for d in b.devices() {
            assert!(
                d.firmwares.is_some(),
                "device {} missing firmwares",
                d.index
            );
            assert!(d.firmwares.as_ref().unwrap().fw_bundle_version.is_some());
        }
    }

    #[test]
    fn test_mock_limits_populated() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        for d in b.devices() {
            assert!(d.limits.is_some(), "device {} missing limits", d.index);
            assert_eq!(d.limits.as_ref().unwrap().tdp_limit, Some(300.0_f32));
        }
    }

    #[test]
    fn test_mock_gddr_temps_valid_range() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        for idx in 0..b.devices().len() {
            if let Some(smbus) = b.smbus_telemetry(idx) {
                for pair in smbus.gddr_temps.iter().flatten() {
                    for &t in pair.0.iter() {
                        assert!(
                            t >= 0.0 && t <= 100.0,
                            "GDDR temp {} out of range for dev {}",
                            t,
                            idx
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_mock_quad_galaxy_some_harvesting() {
        let mut b = MockBackend::with_scenario(MockScenario::QuadGalaxy);
        b.init().unwrap();
        // At least some devices should have non-full enabled_tensix_col (harvested cols)
        let any_harvested = b.devices().iter().any(|d| {
            b.smbus_telemetry(d.index)
                .and_then(|s| s.enabled_tensix_col)
                .map(|v| v != 0x3FFF)
                .unwrap_or(false)
        });
        assert!(
            any_harvested,
            "QuadGalaxy mock should have some harvested columns"
        );
    }
}
