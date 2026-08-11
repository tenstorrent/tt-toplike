// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Telemetry data structures
//!
//! Contains structures for representing hardware telemetry data from
//! Tenstorrent devices. These models are designed to deserialize from
//! tt-smi JSON output and to be populated from luwen API calls.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

    /// Total board input power in watts (tt-smi ≥ 6.0.0 `board_power`).
    /// Distinct from `power`: on dual-asic boards (p300) `power` is this
    /// asic's TDP measurement while `board_power` covers the whole card.
    ///
    /// `#[serde(default)]` keeps this back-compat on the wire (remote frames
    /// and cached snapshots from older tt-smi/tt-toplike don't carry the key).
    #[serde(default)]
    pub board_power: Option<f32>,
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
            board_power: None,
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

/// One row of tt-smi ≥ 6.0.0's per-device process attribution
/// (top-level `processes[]` in the snapshot). All fields optional —
/// tt-smi serializes with exclude_none.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceProcess {
    pub pid: Option<i64>,
    pub user: Option<String>,
    /// Index of the device this process has open.
    pub device: Option<usize>,
    pub cmdline: Option<String>,
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

    /// GDDR temperature pairs (4 registers × 4 bytes each).
    /// Index 0 = GDDR_0_1_TEMP, 1 = GDDR_2_3_TEMP, 2 = GDDR_4_5_TEMP, 3 = GDDR_6_7_TEMP.
    pub gddr_temps: [Option<GddrTempPair>; 4],

    /// Maximum GDDR temperature across all channels (°C), from MAX_GDDR_TEMP field.
    pub max_gddr_temp: Option<f32>,

    /// GDDR correctable error counts per pair register.
    pub gddr_corr_errs: [Option<u32>; 4],

    /// GDDR uncorrectable error count.
    pub gddr_uncorr_errs: Option<u32>,

    /// HARVESTING_STATE bitmask (non-zero means some cores harvested).
    pub harvesting_state: Option<u32>,

    /// ETH_LIVE_STATUS bitmask: bit N set → ETH port N has a live link.
    pub eth_live_status: Option<u64>,

    /// ENABLED_ETH bitmask.
    pub enabled_eth: Option<u32>,

    /// ENABLED_GDDR bitmask.
    pub enabled_gddr: Option<u32>,

    /// ENABLED_L2CPU bitmask.
    pub enabled_l2cpu: Option<u32>,

    /// ENABLED_TENSIX_COL: 14-bit mask, one bit per Tensix column (Blackhole).
    /// Bit N clear → Tensix column N is harvested.
    pub enabled_tensix_col: Option<u32>,
}

/// Four temperature readings packed into one GDDR_X_Y_TEMP hex register.
/// Byte 0 (LSB) = first temp, byte 3 (MSB) = fourth temp.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GddrTempPair(pub [f32; 4]);

/// Unpack a raw GDDR_X_Y_TEMP register (4 packed temperature bytes) into °C.
/// Bytes are extracted little-endian: byte0=LSB is temps[0].
pub fn unpack_gddr_temps_u32(v: u32) -> GddrTempPair {
    GddrTempPair([
        (v & 0xFF) as f32,
        ((v >> 8) & 0xFF) as f32,
        ((v >> 16) & 0xFF) as f32,
        ((v >> 24) & 0xFF) as f32,
    ])
}

/// Unpack a GDDR_X_Y_TEMP hex string (e.g. "0x262a2c2c") into four °C values.
/// Bytes are extracted little-endian: byte0=LSB is temps[0].
/// Returns None if the string is empty, `"N/A"`, or not a valid hex value.
pub fn unpack_gddr_temps(s: &str) -> Option<GddrTempPair> {
    let s = s.trim();
    if s.is_empty() || s == "N/A" {
        return None;
    }
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(unpack_gddr_temps_u32(v))
}

/// Number of Tensix columns in the Blackhole chip grid that can be harvested.
/// This bitmask field (ENABLED_TENSIX_COL) is Blackhole-specific.
pub const BH_TENSIX_COL_COUNT: usize = 14;

/// Returns true if the Tensix column `col` (0-indexed among the BH_TENSIX_COL_COUNT
/// Blackhole Tensix columns) is harvested. Bit N clear → column N is harvested.
/// Always returns false for col >= BH_TENSIX_COL_COUNT (Blackhole-specific field;
/// Wormhole uses a different topology mechanism handled in chip_portrait.rs).
pub fn tensix_col_harvested(enabled_tensix_col: u32, col: usize) -> bool {
    if col >= BH_TENSIX_COL_COUNT {
        return false;
    }
    (enabled_tensix_col >> col) & 1 == 0
}

/// Firmware bundle + component firmware versions (from tt-smi 5.2.0 `firmwares` block).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirmwaresInfo {
    pub fw_bundle_version: Option<String>,
    pub eth_fw: Option<String>,
    pub cm_fw: Option<String>,
    pub gddr_fw: Option<String>,
}

/// Per-device thermal/power limits (from tt-smi 5.2.0 `limits` block).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceLimits {
    pub tdp_limit: Option<f32>,
    pub tdc_limit: Option<f32>,
    pub asic_fmax: Option<u32>,
    pub thm_limit: Option<f32>,
}

/// Parse a string as u32, accepting both "0x1A2B" hex and plain decimal.
pub(crate) fn parse_hex_or_dec(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Same as `parse_hex_or_dec` but for u64 (DDR_STATUS is 32+ bits wide).
pub(crate) fn parse_hex_or_dec_64(s: &str) -> Option<u64> {
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
            gddr_temps: [None; 4],
            max_gddr_temp: None,
            gddr_corr_errs: [None; 4],
            gddr_uncorr_errs: None,
            harvesting_state: None,
            eth_live_status: None,
            enabled_eth: None,
            enabled_gddr: None,
            enabled_l2cpu: None,
            enabled_tensix_col: None,
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

    /// Fan speed in RPM, or `None` when there is no valid reading.
    ///
    /// Cards with no controllable/readable fan (e.g. p150-class Blackhole)
    /// report the firmware all-ones sentinel `0xFFFFFFFF`, which tt-smi masks
    /// to `65535` (`0xFFFF`) in its decimal `fan_speed` field and sysfs exposes
    /// raw as `4294967295`. None of those — nor `0` — is a real RPM, so we map
    /// them to `None` instead of rendering a nonsensical "65535 RPM". Accepts
    /// both decimal and hex ("0xffffffff") strings.
    pub fn fan_rpm(&self) -> Option<u32> {
        let rpm = parse_hex_or_dec(self.fan_speed.as_deref()?)?;
        if rpm == 0 || rpm == 0xFFFF || rpm == 0xFFFF_FFFF {
            None
        } else {
            Some(rpm)
        }
    }

    /// Firmware-reported measured power in Watts from the SMBUS TDP register.
    ///
    /// Despite the name ("Thermal Design Power"), SMBUS TDP on BH/WH is a
    /// real-time measurement, not a limit.  tt-smi exposes it verbatim as
    /// `telemetry.power`.  Use this in preference to sysfs `power1_input`
    /// when available — the firmware-reported value covers all power domains
    /// (Tensix VDD + GDDR), whereas the hwmon driver may only expose vcore.
    ///
    /// Parses as f32 (hex or decimal) so EMA-blended values like "16.0" are
    /// handled correctly.  Returns `None` for zero or negative readings —
    /// firmware never reports ≤ 0 W for a live device, so those are treated
    /// as "not available" to avoid overwriting a valid sysfs reading with 0.
    pub fn tdp_watts(&self) -> Option<f32> {
        let s = self.tdp.as_deref()?.trim();
        let v: f32 = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u32::from_str_radix(hex, 16).ok()? as f32
        } else {
            s.parse().ok()?
        };
        if v > 0.0 {
            Some(v)
        } else {
            None
        }
    }

    /// Firmware-reported measured current in Amperes from the SMBUS TDC register.
    ///
    /// Same caveat as `tdp_watts` — real-time measurement, not a limit.
    /// Parses as f32 and returns `None` for non-positive values.
    pub fn tdc_amperes(&self) -> Option<f32> {
        let s = self.tdc.as_deref()?.trim();
        let v: f32 = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u32::from_str_radix(hex, 16).ok()? as f32
        } else {
            s.parse().ok()?
        };
        if v > 0.0 {
            Some(v)
        } else {
            None
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

    /// Compute the maximum temperature across all populated GDDR temp pairs.
    /// Prefer this over the raw `max_gddr_temp` field when that field is `None`
    /// (older tt-smi versions don't expose MAX_GDDR_TEMP directly).
    pub fn max_gddr_temp_computed(&self) -> Option<f32> {
        let max = self
            .gddr_temps
            .iter()
            .filter_map(|pair| pair.as_ref())
            .flat_map(|pair| pair.0.iter().copied())
            .filter(|&t| t > 0.0) // 0.0 = unpopulated slot; hardware never reports 0°C for live GDDR
            .fold(f32::NEG_INFINITY, f32::max);
        if max == f32::NEG_INFINITY {
            None
        } else {
            Some(max)
        }
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
            board_power: None,
        };

        assert!(telem.is_valid());
        assert_eq!(telem.power_w(), 45.2);
        assert_eq!(telem.temp_c(), 52.3);
        assert_eq!(telem.current_a(), 25.5);
        assert_eq!(telem.aiclk_mhz(), 1000);
        assert!(telem.arc_healthy());
    }

    #[test]
    fn fan_rpm_filters_all_ones_sentinel() {
        let mut smbus = SmbusTelemetry::new();
        // A real reading passes through unchanged.
        smbus.fan_speed = Some("3000".to_string());
        assert_eq!(smbus.fan_rpm(), Some(3000));
        // Cards with no readable fan (e.g. p150 Blackhole) report the firmware
        // all-ones sentinel 0xFFFFFFFF; tt-smi masks it to 65535 (0xFFFF) in its
        // decimal fan_speed field. Neither is a real RPM.
        smbus.fan_speed = Some("65535".to_string());
        assert_eq!(smbus.fan_rpm(), None);
        // Raw u32 all-ones (e.g. straight from sysfs fan1_input = 4294967295).
        smbus.fan_speed = Some("4294967295".to_string());
        assert_eq!(smbus.fan_rpm(), None);
        // Hex all-ones form (tt-smi FAN_RPM = "0xffffffff").
        smbus.fan_speed = Some("0xffffffff".to_string());
        assert_eq!(smbus.fan_rpm(), None);
        // Zero / absent → no reading.
        smbus.fan_speed = Some("0".to_string());
        assert_eq!(smbus.fan_rpm(), None);
        smbus.fan_speed = None;
        assert_eq!(smbus.fan_rpm(), None);
    }

    #[test]
    fn test_smbus_telemetry() {
        let mut smbus = SmbusTelemetry::new();
        // Real tt-smi values: DDR_SPEED is hex, arc0_health (TIMER_HEARTBEAT) is hex
        smbus.ddr_speed = Some("0x3e80".to_string()); // 0x3e80 = 16000 MT/s
        smbus.ddr_status = Some("0x55555555".to_string());
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
            assert!(
                smbus.is_ddr_channel_trained(ch),
                "ch {} should be trained",
                ch
            );
        }

        // Channel 0 nibble = 0x0 (untrained), rest 0x5 (trained) → 0x55555550
        smbus.ddr_status = Some("0x55555550".to_string());
        assert!(!smbus.is_ddr_channel_trained(0), "ch 0 should be untrained");
        for ch in 1..8 {
            assert!(
                smbus.is_ddr_channel_trained(ch),
                "ch {} should be trained",
                ch
            );
        }

        // Legacy decimal string with nibble value 2 still counts as trained.
        smbus.ddr_status = Some("2".to_string()); // decimal 2 → nibble 0 = 2 = trained
        assert!(smbus.is_ddr_channel_trained(0));
    }

    // ── GddrTempPair ─────────────────────────────────────────────────────────

    #[test]
    fn test_gddr_temp_unpack_normal() {
        // 0x262a2c2c → bytes LE: 0x2c, 0x2c, 0x2a, 0x26 → [44.0, 44.0, 42.0, 38.0]
        let pair = unpack_gddr_temps("0x262a2c2c").unwrap();
        assert_eq!(pair.0, [44.0_f32, 44.0, 42.0, 38.0]);
    }

    #[test]
    fn test_gddr_temp_unpack_zero() {
        let pair = unpack_gddr_temps("0x00000000").unwrap();
        assert_eq!(pair.0, [0.0_f32, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_gddr_temp_unpack_na() {
        assert!(unpack_gddr_temps("N/A").is_none());
        assert!(unpack_gddr_temps("").is_none());
    }

    #[test]
    fn test_gddr_temp_unpack_u32() {
        // Same packing as the hex-string form: byte0 (LSB) = temps[0].
        let pair = unpack_gddr_temps_u32(0x262a2c2c);
        assert_eq!(pair.0, [44.0_f32, 44.0, 42.0, 38.0]);
    }

    // ── tensix_col_harvested ──────────────────────────────────────────────────

    #[test]
    fn test_harvesting_col_all_active() {
        // 0x3FFF = all 14 bits set → no harvesting
        for col in 0..14_usize {
            assert!(
                !tensix_col_harvested(0x3FFF, col),
                "col {} should be active",
                col
            );
        }
    }

    #[test]
    fn test_harvesting_col_col0_harvested() {
        // bit 0 clear → col 0 harvested
        assert!(tensix_col_harvested(0x3FFE, 0));
        assert!(!tensix_col_harvested(0x3FFE, 1));
    }

    #[test]
    fn test_harvesting_col_all_harvested() {
        for col in 0..14_usize {
            assert!(tensix_col_harvested(0x0000, col));
        }
    }

    #[test]
    fn test_smbus_telemetry_new_fields_default_none() {
        let s = SmbusTelemetry::new();
        assert!(s.gddr_temps.iter().all(|t| t.is_none()));
        assert!(s.max_gddr_temp.is_none());
        assert!(s.harvesting_state.is_none());
        assert!(s.eth_live_status.is_none());
        assert!(s.enabled_tensix_col.is_none());
    }

    #[test]
    fn test_smbus_gddr_max_temp() {
        let mut s = SmbusTelemetry::new();
        s.gddr_temps[0] = Some(GddrTempPair([38.0, 42.0, 44.0, 44.0]));
        s.gddr_temps[1] = Some(GddrTempPair([50.0, 52.0, 48.0, 46.0]));
        let max = s.max_gddr_temp_computed();
        assert_eq!(max, Some(52.0_f32));
    }
}
