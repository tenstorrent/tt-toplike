// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Telemetry data structures
//!
//! Contains structures for representing hardware telemetry data from
//! Tenstorrent devices. These models are designed to deserialize from
//! tt-smi JSON output and to be populated from luwen API calls.

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// Main telemetry data for a device
///
/// Contains core metrics like power, temperature, clock speeds.
/// All fields are Option<T> to handle missing/unavailable data gracefully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Telemetry {
    /// Supply voltage in volts (e.g., 0.85V)
    pub voltage: Option<f32>,

    /// Current draw in amperes (e.g., 25.5A)
    pub current: Option<f32>,

    /// Power consumption in watts (e.g., 45.2W)
    pub power: Option<f32>,

    /// ASIC temperature in Celsius (e.g., 52.3°C)
    pub asic_temperature: Option<f32>,

    /// AI clock frequency in MHz (e.g., 1000MHz)
    pub aiclk: Option<u32>,

    /// ARC firmware heartbeat (1 = healthy, 0 = stalled)
    pub heartbeat: Option<u32>,

    /// Timestamp when telemetry was captured
    #[serde(default = "chrono::Utc::now")]
    pub timestamp: DateTime<Utc>,
}

impl Telemetry {
    /// Create a new empty Telemetry instance
    pub fn new() -> Self {
        Self {
            voltage: None,
            current: None,
            power: None,
            asic_temperature: None,
            aiclk: None,
            heartbeat: None,
            timestamp: Utc::now(),
        }
    }

    /// Check if telemetry data is valid (has at least some measurements)
    pub fn is_valid(&self) -> bool {
        self.power.is_some() || self.asic_temperature.is_some() || self.current.is_some()
    }

    /// Get power consumption in watts (0.0 if not available)
    pub fn power_w(&self) -> f32 {
        self.power.unwrap_or(0.0)
    }

    /// Get temperature in Celsius (0.0 if not available)
    pub fn temp_c(&self) -> f32 {
        self.asic_temperature.unwrap_or(0.0)
    }

    /// Get current draw in amperes (0.0 if not available)
    pub fn current_a(&self) -> f32 {
        self.current.unwrap_or(0.0)
    }

    /// Get AICLK frequency in MHz (0 if not available)
    pub fn aiclk_mhz(&self) -> u32 {
        self.aiclk.unwrap_or(0)
    }

    /// Check if ARC firmware heartbeat is healthy
    pub fn arc_healthy(&self) -> bool {
        self.heartbeat.unwrap_or(0) > 0
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

/// SMBUS telemetry (low-level hardware status)
///
/// Contains detailed hardware status information from the System Management Bus.
/// This includes DDR status, firmware versions, health indicators, and more.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmbusTelemetry {
    /// Board ID
    pub board_id: Option<String>,

    /// Enumeration version
    pub enum_version: Option<String>,

    /// Device ID
    pub device_id: Option<String>,

    /// DDR memory speed (e.g., "6400" for 6400 MT/s)
    pub ddr_speed: Option<String>,

    /// DDR training status bitmask
    /// Each bit represents a DDR channel:
    /// - 0 = untrained
    /// - 1 = training
    /// - 2 = trained
    /// - 3 = error
    pub ddr_status: Option<String>,

    /// ARC0 firmware health (heartbeat counter)
    pub arc0_health: Option<String>,

    /// ARC1 firmware health (heartbeat counter)
    pub arc1_health: Option<String>,

    /// ARC2 firmware health (heartbeat counter)
    pub arc2_health: Option<String>,

    /// ARC3 firmware health (heartbeat counter)
    pub arc3_health: Option<String>,

    /// ARC0 firmware version
    pub arc0_fw_version: Option<String>,

    /// ARC1 firmware version
    pub arc1_fw_version: Option<String>,

    /// ARC2 firmware version
    pub arc2_fw_version: Option<String>,

    /// ARC3 firmware version
    pub arc3_fw_version: Option<String>,

    /// Ethernet firmware version
    pub eth_fw_version: Option<String>,

    /// M3 bootloader firmware version
    pub m3_bl_fw_version: Option<String>,

    /// M3 application firmware version
    pub m3_app_fw_version: Option<String>,

    /// SPI boot ROM firmware version
    pub spibootrom_fw_version: Option<String>,

    /// TT-Flash version
    pub tt_flash_version: Option<String>,

    /// AI clock frequency (MHz)
    pub aiclk: Option<String>,

    /// AXI clock frequency (MHz)
    pub axiclk: Option<String>,

    /// ARC clock frequency (MHz)
    pub arcclk: Option<String>,

    /// ASIC temperature from SMBUS
    pub asic_temperature: Option<String>,

    /// Voltage regulator temperature
    pub vreg_temperature: Option<String>,

    /// Board temperature
    pub board_temperature: Option<String>,

    /// Core voltage (VCore)
    pub vcore: Option<String>,

    /// Thermal Design Power (TDP) limit
    pub tdp: Option<String>,

    /// Thermal Design Current (TDC) limit
    pub tdc: Option<String>,

    /// Throttler status (thermal/power throttling)
    pub throttler: Option<String>,

    /// VDD limits
    pub vdd_limits: Option<String>,

    /// Thermal limits
    pub thm_limits: Option<String>,

    /// Fan speed (if applicable)
    pub fan_speed: Option<String>,

    /// Faults register
    pub faults: Option<String>,

    /// PCIe status
    pub pcie_status: Option<String>,

    /// Ethernet status 0
    pub eth_status0: Option<String>,

    /// Ethernet status 1
    pub eth_status1: Option<String>,

    /// Input power
    pub input_power: Option<String>,

    /// Board power limit
    pub board_power_limit: Option<String>,

    /// Thermal trip count
    pub therm_trip_count: Option<String>,

    /// Boot date
    pub boot_date: Option<String>,

    /// Runtime seconds
    pub rt_seconds: Option<String>,

    /// Wormhole firmware date
    pub wh_fw_date: Option<String>,

    /// ASIC thermal monitor 0
    pub asic_tmon0: Option<String>,

    /// ASIC thermal monitor 1
    pub asic_tmon1: Option<String>,

    /// MVDDQ power
    pub mvddq_power: Option<String>,

    /// GDDR training temperature 0
    pub gddr_train_temp0: Option<String>,

    /// GDDR training temperature 1
    pub gddr_train_temp1: Option<String>,

    /// Auxiliary status
    pub aux_status: Option<String>,

    /// Ethernet debug status 0
    pub eth_debug_status0: Option<String>,

    /// Ethernet debug status 1
    pub eth_debug_status1: Option<String>,
}

/// Parse a string as u32, accepting both "0x1A2B" hex and plain decimal.
fn parse_hex_or_dec(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Same as `parse_hex_or_dec` but for u64 (DDR_STATUS is 32+ bits wide).
fn parse_hex_or_dec_64(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

impl SmbusTelemetry {
    /// Create a new empty SmbusTelemetry instance
    pub fn new() -> Self {
        Self {
            board_id: None,
            enum_version: None,
            device_id: None,
            ddr_speed: None,
            ddr_status: None,
            arc0_health: None,
            arc1_health: None,
            arc2_health: None,
            arc3_health: None,
            arc0_fw_version: None,
            arc1_fw_version: None,
            arc2_fw_version: None,
            arc3_fw_version: None,
            eth_fw_version: None,
            m3_bl_fw_version: None,
            m3_app_fw_version: None,
            spibootrom_fw_version: None,
            tt_flash_version: None,
            aiclk: None,
            axiclk: None,
            arcclk: None,
            asic_temperature: None,
            vreg_temperature: None,
            board_temperature: None,
            vcore: None,
            tdp: None,
            tdc: None,
            throttler: None,
            vdd_limits: None,
            thm_limits: None,
            fan_speed: None,
            faults: None,
            pcie_status: None,
            eth_status0: None,
            eth_status1: None,
            input_power: None,
            board_power_limit: None,
            therm_trip_count: None,
            boot_date: None,
            rt_seconds: None,
            wh_fw_date: None,
            asic_tmon0: None,
            asic_tmon1: None,
            mvddq_power: None,
            gddr_train_temp0: None,
            gddr_train_temp1: None,
            aux_status: None,
            eth_debug_status0: None,
            eth_debug_status1: None,
        }
    }

    /// Parse DDR speed as integer (MT/s).
    /// Accepts both decimal ("16000") and hex ("0x3e80") strings.
    pub fn ddr_speed_mts(&self) -> Option<u32> {
        parse_hex_or_dec(self.ddr_speed.as_deref()?)
    }

    /// Parse DDR status as bitmask.
    /// Each 4-bit nibble represents one channel's state:
    /// - 0: untrained, 1: training, 2/5: trained, F: error
    /// Accepts both decimal and hex ("0x55555555") strings.
    pub fn ddr_status_bitmask(&self) -> Option<u64> {
        parse_hex_or_dec_64(self.ddr_status.as_deref()?)
    }

    /// Check if specific DDR channel is trained.
    ///
    /// tt-smi DDR_STATUS is a 32-bit value where each 4-bit nibble encodes one
    /// channel: 0x5 means trained on Blackhole hardware.  Channels 0-7 occupy
    /// nibbles 0-7 (bits 3:0, 7:4, … 31:28).
    pub fn is_ddr_channel_trained(&self, channel: usize) -> bool {
        if let Some(status) = self.ddr_status_bitmask() {
            let nibble = (status >> (channel * 4)) & 0xF;
            // 0x5 = trained on Blackhole; legacy 2-bit encoding: 2 = trained
            nibble == 0x5 || nibble == 2
        } else {
            false
        }
    }

    /// Get ARC0 health as integer (heartbeat counter).
    /// Accepts both decimal and hex ("0x10e7a") strings.
    pub fn arc0_health_value(&self) -> Option<u32> {
        parse_hex_or_dec(self.arc0_health.as_deref()?)
    }

    /// Check if ARC0 firmware is healthy (heartbeat > 0)
    pub fn is_arc0_healthy(&self) -> bool {
        self.arc0_health_value().unwrap_or(0) > 0
    }
}

impl Default for SmbusTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_creation() {
        let telem = Telemetry::new();
        assert!(!telem.is_valid());
        assert_eq!(telem.power_w(), 0.0);
    }

    #[test]
    fn test_telemetry_with_data() {
        let telem = Telemetry {
            voltage: Some(0.85),
            current: Some(25.5),
            power: Some(45.2),
            asic_temperature: Some(52.3),
            aiclk: Some(1000),
            heartbeat: Some(1),
            timestamp: Utc::now(),
        };

        assert!(telem.is_valid());
        assert_eq!(telem.power_w(), 45.2);
        assert_eq!(telem.temp_c(), 52.3);
        assert_eq!(telem.current_a(), 25.5);
        assert_eq!(telem.aiclk_mhz(), 1000);
        assert!(telem.arc_healthy());
    }

    #[test]
    fn test_smbus_telemetry() {
        let mut smbus = SmbusTelemetry::new();
        // Real tt-smi values: DDR_SPEED is hex, arc0_health (TIMER_HEARTBEAT) is hex
        smbus.ddr_speed   = Some("0x3e80".to_string()); // 0x3e80 = 16000 MT/s
        smbus.ddr_status  = Some("0x55555555".to_string());
        smbus.arc0_health = Some("0x10e7a".to_string()); // non-zero → healthy

        assert_eq!(smbus.ddr_speed_mts(), Some(16000));
        assert_eq!(smbus.ddr_status_bitmask(), Some(0x55555555_u64));
        assert_eq!(smbus.arc0_health_value(), Some(0x10e7a));
        assert!(smbus.is_arc0_healthy());
    }

    #[test]
    fn test_ddr_channel_status() {
        let mut smbus = SmbusTelemetry::new();

        // Blackhole DDR_STATUS: 4-bit nibble per channel, 0x5 = trained.
        // "0x55555555" → 8 channels, all trained.
        smbus.ddr_status = Some("0x55555555".to_string());
        for ch in 0..8 {
            assert!(smbus.is_ddr_channel_trained(ch), "ch {} should be trained", ch);
        }

        // Channel 0 nibble = 0x0 (untrained), rest 0x5 (trained) → 0x55555550
        smbus.ddr_status = Some("0x55555550".to_string());
        assert!(!smbus.is_ddr_channel_trained(0), "ch 0 should be untrained");
        for ch in 1..8 {
            assert!(smbus.is_ddr_channel_trained(ch), "ch {} should be trained", ch);
        }

        // Legacy decimal string with nibble value 2 still counts as trained.
        smbus.ddr_status = Some("2".to_string()); // decimal 2 → nibble 0 = 2 = trained
        assert!(smbus.is_ddr_channel_trained(0));
    }
}
