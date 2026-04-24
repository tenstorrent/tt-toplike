// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Luwen backend for direct hardware access
//!
//! This backend uses the official Tenstorrent luwen library to communicate
//! directly with hardware via PCIe, providing the fastest and most efficient
//! telemetry access.
//!
//! # Architecture
//!
//! - Uses `luwen-if` for high-level chip detection and communication
//! - Bypasses subprocess overhead (unlike JSON backend)
//! - Direct memory-mapped I/O for telemetry reads
//! - Supports all Tenstorrent architectures (Grayskull, Wormhole, Blackhole)

#[cfg(feature = "luwen-backend")]
use all_smi_luwen_if::chip::{Chip, ChipImpl};

#[cfg(feature = "luwen-backend")]
use all_smi_luwen_core::Arch;

#[cfg(feature = "luwen-backend")]
use all_smi_luwen_if::ChipDetectOptions;

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use std::collections::HashMap;

/// Luwen backend implementation for direct hardware access
pub struct LuwenBackend {
    /// Backend configuration
    config: BackendConfig,

    /// Detected devices
    devices: Vec<Device>,

    /// Cached telemetry data (per device index)
    telemetry_cache: HashMap<usize, Telemetry>,

    /// Cached SMBUS telemetry (per device index)
    smbus_cache: HashMap<usize, SmbusTelemetry>,

    /// Luwen chip handles
    #[cfg(feature = "luwen-backend")]
    chips: Vec<Chip>,
}

impl LuwenBackend {
    /// Create a new Luwen backend with default configuration
    pub fn new() -> Self {
        Self::with_config(BackendConfig::default())
    }

    /// Create a new Luwen backend with custom configuration
    pub fn with_config(config: BackendConfig) -> Self {
        Self {
            config,
            devices: Vec::new(),
            telemetry_cache: HashMap::new(),
            smbus_cache: HashMap::new(),
            #[cfg(feature = "luwen-backend")]
            chips: Vec::new(),
        }
    }

    /// Detect and initialize hardware devices
    #[cfg(feature = "luwen-backend")]
    fn detect_devices(&mut self) -> BackendResult<()> {
        log::info!("LuwenBackend: Detecting devices...");

        // Try with noc_safe first (safer for active hardware)
        let options = ChipDetectOptions {
            local_only: true,       // Only detect locally attached devices
            noc_safe: true,         // Use safer NoC access (won't interfere with workloads)
            continue_on_failure: true,  // Try all devices even if some fail
            ..Default::default()
        };

        log::info!("LuwenBackend: Trying noc_safe mode (non-invasive for active workloads)");
        // Detect chips using all-smi-luwen-ref (returns UninitChip objects)
        let uninit_chips = all_smi_luwen_ref::detect_chips_silent(options)
            .map_err(|e| BackendError::Initialization(format!("Device detection failed: {}", e)))?;

        if uninit_chips.is_empty() {
            return Err(BackendError::Initialization("No devices found".to_string()));
        }

        log::info!("LuwenBackend: Found {} uninitialized devices", uninit_chips.len());

        // Initialize each chip
        for (idx, uninit_chip) in uninit_chips.into_iter().enumerate() {
            // Initialize the chip with a dummy callback (all-smi pattern)
            let chip = uninit_chip.init(&mut |_| Ok::<(), std::convert::Infallible>(()))
                .map_err(|_| BackendError::Initialization("Chip initialization failed".to_string()))?;

            // Get architecture
            let arch = chip.get_arch();
            let architecture = match arch {
                Arch::Grayskull => Architecture::Grayskull,
                Arch::Wormhole => Architecture::Wormhole,
                Arch::Blackhole => Architecture::Blackhole,
                _ => Architecture::Unknown,
            };

            // Get device info for better identification
            let device_info = chip.get_device_info().ok().flatten();
            let bus_id = if let Some(info) = &device_info {
                format!("{:?}", info)  // Will include PCI address
            } else {
                format!("pci:{}", idx)
            };

            // Try to get telemetry to determine board type
            let board_type = if let Ok(telem) = chip.get_telemetry() {
                telem.board_type().to_string()
            } else {
                format!("{:?}", arch)
            };

            // Create device
            let device = Device {
                index: idx,
                board_type,
                bus_id,
                coords: String::new(),  // Coordinates not provided by luwen-if
                architecture,
            };

            self.devices.push(device);

            // Store chip handle for telemetry reads
            self.chips.push(chip);
        }

        log::info!("LuwenBackend: Successfully initialized {} devices", self.devices.len());
        Ok(())
    }

    #[cfg(not(feature = "luwen-backend"))]
    fn detect_devices(&mut self) -> BackendResult<()> {
        Err(BackendError::Initialization(
            "Luwen backend not enabled. Rebuild with --features luwen-backend".to_string()
        ))
    }
}

impl Default for LuwenBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryBackend for LuwenBackend {
    fn init(&mut self) -> BackendResult<()> {
        self.detect_devices()?;
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        #[cfg(feature = "luwen-backend")]
        {
            use all_smi_luwen_if::chip::ChipImpl;

            // Read telemetry from each chip
            for (idx, chip) in self.chips.iter().enumerate() {
                match chip.get_telemetry() {
                    Ok(luwen_telem) => {
                        // Map all-smi Telemetry to our Telemetry model
                        // Note: luwen returns f64, but our model uses f32
                        let telemetry = Telemetry {
                            timestamp: chrono::Utc::now(),
                            voltage: Some(luwen_telem.voltage() as f32),
                            current: Some(luwen_telem.current() as f32),
                            power: Some(luwen_telem.power() as f32),
                            asic_temperature: Some(luwen_telem.asic_temperature() as f32),
                            aiclk: Some(luwen_telem.ai_clk()),
                            heartbeat: Some(luwen_telem.arc0_health),
                        };

                        // Map all-smi Telemetry to our SmbusTelemetry model
                        // Note: all-smi fields don't have smbus_tx_ prefix
                        let smbus = SmbusTelemetry {
                            board_id: Some(luwen_telem.board_id.to_string()),
                            ddr_status: Some(luwen_telem.ddr_status.to_string()),
                            ddr_speed: luwen_telem.ddr_speed.map(|v| v.to_string()),
                            eth_status0: Some(luwen_telem.eth_status0.to_string()),
                            eth_status1: Some(luwen_telem.eth_status1.to_string()),
                            pcie_status: Some(luwen_telem.pcie_status.to_string()),
                            faults: Some(luwen_telem.faults.to_string()),
                            arc0_fw_version: Some(luwen_telem.arc0_fw_version.to_string()),
                            arc1_fw_version: Some(luwen_telem.arc1_fw_version.to_string()),
                            arc2_fw_version: Some(luwen_telem.arc2_fw_version.to_string()),
                            arc3_fw_version: Some(luwen_telem.arc3_fw_version.to_string()),
                            eth_fw_version: Some(luwen_telem.eth_fw_version.to_string()),
                            m3_bl_fw_version: Some(luwen_telem.m3_bl_fw_version.to_string()),
                            m3_app_fw_version: Some(luwen_telem.m3_app_fw_version.to_string()),
                            arc0_health: Some(luwen_telem.arc0_health.to_string()),
                            arc1_health: Some(luwen_telem.arc1_health.to_string()),
                            arc2_health: Some(luwen_telem.arc2_health.to_string()),
                            arc3_health: Some(luwen_telem.arc3_health.to_string()),
                            aiclk: Some(luwen_telem.aiclk.to_string()),
                            axiclk: Some(luwen_telem.axiclk.to_string()),
                            arcclk: Some(luwen_telem.arcclk.to_string()),
                            vcore: Some(luwen_telem.vcore.to_string()),
                            asic_temperature: Some(luwen_telem.asic_temperature.to_string()),
                            vreg_temperature: Some(luwen_telem.vreg_temperature.to_string()),
                            board_temperature: Some(luwen_telem.board_temperature.to_string()),
                            tdp: Some(luwen_telem.tdp.to_string()),
                            tdc: Some(luwen_telem.tdc.to_string()),
                            throttler: Some(luwen_telem.throttler.to_string()),
                            fan_speed: Some(luwen_telem.fan_speed.to_string()),
                            vdd_limits: Some(luwen_telem.vdd_limits.to_string()),
                            thm_limits: Some(luwen_telem.thm_limits.to_string()),
                            asic_tmon0: Some(luwen_telem.asic_tmon0.to_string()),
                            asic_tmon1: Some(luwen_telem.asic_tmon1.to_string()),
                            mvddq_power: Some(luwen_telem.mvddq_power.to_string()),
                            gddr_train_temp0: Some(luwen_telem.gddr_train_temp0.to_string()),
                            gddr_train_temp1: Some(luwen_telem.gddr_train_temp1.to_string()),
                            boot_date: Some(luwen_telem.boot_date.to_string()),
                            rt_seconds: Some(luwen_telem.rt_seconds.to_string()),
                            eth_debug_status0: Some(luwen_telem.eth_debug_status0.to_string()),
                            eth_debug_status1: Some(luwen_telem.eth_debug_status1.to_string()),
                            tt_flash_version: Some(luwen_telem.tt_flash_version.to_string()),
                            enum_version: Some(luwen_telem.enum_version.to_string()),
                            device_id: Some(luwen_telem.device_id.to_string()),
                            spibootrom_fw_version: Some(luwen_telem.spibootrom_fw_version.to_string()),
                            wh_fw_date: Some(luwen_telem.wh_fw_date.to_string()),
                            aux_status: luwen_telem.aux_status.map(|v| v.to_string()),
                            // Fields not in all-smi Telemetry
                            input_power: None,
                            board_power_limit: None,
                            therm_trip_count: None,
                        };

                        // Cache the telemetry data
                        self.telemetry_cache.insert(idx, telemetry);
                        self.smbus_cache.insert(idx, smbus);
                    }
                    Err(e) => {
                        log::warn!("LuwenBackend: Failed to read telemetry for device {}: {:?}", idx, e);
                        // Keep existing cached data on error (don't remove from cache)
                    }
                }
            }

            Ok(())
        }

        #[cfg(not(feature = "luwen-backend"))]
        {
            Err(BackendError::Update("Luwen backend not enabled".to_string()))
        }
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

    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        self.smbus_cache.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        "Luwen (Direct Hardware)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_luwen_backend_creation() {
        let backend = LuwenBackend::new();
        assert_eq!(backend.backend_info(), "Luwen (Direct Hardware)");
        assert_eq!(backend.device_count(), 0);
    }

    #[test]
    fn test_luwen_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = LuwenBackend::with_config(config);
        assert_eq!(backend.config.interval_ms, 50);
    }
}
