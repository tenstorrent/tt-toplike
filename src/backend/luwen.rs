// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Luwen backend for direct hardware access
//!
//! This backend uses the official Tenstorrent `luwen-api`/`luwen-def` crates
//! (crates.io, published from tenstorrent/luwen) to communicate directly with
//! hardware via PCIe, providing the fastest and most efficient telemetry
//! access.
//!
//! Previously this backend was built on the third-party `all-smi-luwen-*`
//! forks, which have been stale since 2025-07 (missing Blackhole telemetry
//! tags and a negative 16.16-fixed-point temperature decode fix). Migrated
//! to the official 0.8.5 crates to pick up full Blackhole telemetry (GDDR
//! temps/ECC, harvesting, enabled masks, therm trips, input/board power).
//!
//! # Architecture
//!
//! - Uses `luwen-api` for high-level chip detection and communication
//! - Bypasses subprocess overhead (unlike JSON backend)
//! - Direct memory-mapped I/O for telemetry reads
//! - Supports all Tenstorrent architectures (Grayskull, Wormhole, Blackhole)

#[cfg(feature = "luwen-backend")]
use luwen_api::chip::{Chip, ChipImpl};

#[cfg(feature = "luwen-backend")]
use luwen_api::ChipDetectOptions;

#[cfg(feature = "luwen-backend")]
use luwen_def::Arch;

#[cfg(feature = "luwen-backend")]
use crate::models::telemetry::unpack_gddr_temps_u32;

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
            local_only: true,          // Only detect locally attached devices
            noc_safe: true,            // Use safer NoC access (won't interfere with workloads)
            continue_on_failure: true, // Try all devices even if some fail
            ..Default::default()
        };

        log::info!("LuwenBackend: Trying noc_safe mode (non-invasive for active workloads)");
        // Detect chips using the official luwen-api. Unlike the old all-smi-luwen-ref
        // path, detect_chips_silent() returns fully-initialized Chip objects directly —
        // no separate UninitChip/init-callback dance is needed.
        let chips = luwen_api::detect_chips_silent(Vec::new(), options)
            .map_err(|e| BackendError::Initialization(format!("Device detection failed: {}", e)))?;

        if chips.is_empty() {
            return Err(BackendError::Initialization("No devices found".to_string()));
        }

        log::info!("LuwenBackend: Found {} devices", chips.len());

        // Register each already-initialized chip
        for (idx, chip) in chips.into_iter().enumerate() {
            // Get architecture
            let arch = chip.get_arch();
            // Grayskull is #[deprecated] in luwen_def::Arch ("legacy architecture
            // that is no longer supported") but the variant still exists — keep
            // mapping it for completeness rather than silently dropping support.
            // The allow is scoped to just this arm (upstream only deprecated
            // that one variant; Wormhole/Blackhole shouldn't be silently
            // exempted from future deprecation warnings too).
            let architecture = match arch {
                #[allow(deprecated)]
                Arch::Grayskull => Architecture::Grayskull,
                Arch::Wormhole => Architecture::Wormhole,
                Arch::Blackhole => Architecture::Blackhole,
            };

            // Get device info for better identification
            let device_info = chip.get_device_info().ok().flatten();
            let bus_id = if let Some(info) = &device_info {
                format!("{:?}", info) // Will include PCI address
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
                coords: String::new(), // Coordinates not provided by luwen-if
                architecture,
                firmwares: None,
                limits: None,
                pcie_speed: None,
                pcie_width: None,
                grid_override: None,
                channels_override: None,
            };

            self.devices.push(device);

            // Store chip handle for telemetry reads
            self.chips.push(chip);
        }

        log::info!(
            "LuwenBackend: Successfully initialized {} devices",
            self.devices.len()
        );
        Ok(())
    }

    #[cfg(not(feature = "luwen-backend"))]
    fn detect_devices(&mut self) -> BackendResult<()> {
        Err(BackendError::Initialization(
            "Luwen backend not enabled. Rebuild with --features luwen-backend".to_string(),
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
            use luwen_api::chip::ChipImpl;

            // Read telemetry from each chip
            for (idx, chip) in self.chips.iter().enumerate() {
                match chip.get_telemetry() {
                    Ok(luwen_telem) => {
                        // Map official luwen Telemetry to our Telemetry model.
                        // Note: luwen returns f64, but our model uses f32.
                        let telemetry = Telemetry {
                            timestamp: chrono::Utc::now(),
                            voltage: Some(luwen_telem.voltage() as f32),
                            current: Some(luwen_telem.current() as f32),
                            power: Some(luwen_telem.power() as f32),
                            asic_temperature: Some(luwen_telem.asic_temperature() as f32),
                            aiclk: Some(luwen_telem.ai_clk()),
                            // telemetry_heartbeat() falls back correctly on BH
                            // where the per-ARC health registers don't exist.
                            heartbeat: Some(luwen_telem.telemetry_heartbeat()),
                            // The official luwen Telemetry has no distinct whole-board
                            // power rail (it does have asic_power, but that's not the
                            // same measurement as tt-smi's board_power) — leave None.
                            board_power: None,
                        };

                        // Map official luwen Telemetry to our SmbusTelemetry model.
                        // Field names carry over unchanged from the all-smi fork.
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
                            spibootrom_fw_version: Some(
                                luwen_telem.spibootrom_fw_version.to_string(),
                            ),
                            wh_fw_date: Some(luwen_telem.wh_fw_date.to_string()),
                            aux_status: luwen_telem.aux_status.map(|v| v.to_string()),
                            input_power: Some(luwen_telem.input_power.to_string()),
                            board_power_limit: Some(luwen_telem.board_power_limit.to_string()),
                            therm_trip_count: Some(luwen_telem.therm_trip_count.to_string()),
                            // ── GDDR temps / ECC / harvesting — new in official 0.8.x ──
                            gddr_temps: [
                                Some(unpack_gddr_temps_u32(luwen_telem.gddr01_temp)),
                                Some(unpack_gddr_temps_u32(luwen_telem.gddr23_temp)),
                                Some(unpack_gddr_temps_u32(luwen_telem.gddr45_temp)),
                                Some(unpack_gddr_temps_u32(luwen_telem.gddr67_temp)),
                            ],
                            max_gddr_temp: Some(luwen_telem.max_gddr_temp as f32),
                            gddr_corr_errs: [
                                Some(luwen_telem.gddr01_corr_errs),
                                Some(luwen_telem.gddr23_corr_errs),
                                Some(luwen_telem.gddr45_corr_errs),
                                Some(luwen_telem.gddr67_corr_errs),
                            ],
                            gddr_uncorr_errs: Some(luwen_telem.gddr_uncorr_errs),
                            harvesting_state: Some(luwen_telem.harvesting_state),
                            enabled_eth: Some(luwen_telem.enabled_eth),
                            enabled_gddr: Some(luwen_telem.enabled_gddr),
                            enabled_l2cpu: Some(luwen_telem.enabled_l2cpu),
                            enabled_tensix_col: Some(luwen_telem.tensix_enabled_col),
                            // eth_live_status is NOT in luwen Telemetry — tt-smi
                            // derives it separately; stays None on this path.
                            ..Default::default()
                        };

                        // Cache the telemetry data
                        self.telemetry_cache.insert(idx, telemetry);
                        self.smbus_cache.insert(idx, smbus);
                    }
                    Err(e) => {
                        log::warn!(
                            "LuwenBackend: Failed to read telemetry for device {}: {:?}",
                            idx,
                            e
                        );
                        // Keep existing cached data on error (don't remove from cache)
                    }
                }
            }

            Ok(())
        }

        #[cfg(not(feature = "luwen-backend"))]
        {
            Err(BackendError::Update(
                "Luwen backend not enabled".to_string(),
            ))
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
        assert_eq!(backend.config.update_interval_ms, 50);
    }
}
