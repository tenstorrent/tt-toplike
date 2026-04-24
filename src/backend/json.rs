// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! JSON backend for tt-smi subprocess integration
//!
//! This backend runs tt-smi in snapshot mode and parses its JSON output.
//! It provides real hardware telemetry data through the same interface as MockBackend.
//!
//! ## Architecture
//!
//! ```
//! JSONBackend → tt-smi -s (run on demand) → JSON output → Parse → Telemetry models
//! ```
//!
//! ## Usage
//!
//! ```rust,no_run
//! use tt_toplike_rs::backend::{TelemetryBackend, json::JSONBackend};
//!
//! let mut backend = JSONBackend::new("tt-smi");
//! backend.init()?;
//!
//! loop {
//!     backend.update()?;
//!     for device in backend.devices() {
//!         if let Some(telem) = backend.telemetry(device.index) {
//!             println!("Device {}: {}W", device.index, telem.power_w());
//!         }
//!     }
//! }
//! ```

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Device, SmbusTelemetry, Telemetry};
use chrono::Utc;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Instant;

// tt-smi encodes numeric telemetry values as quoted JSON strings, possibly with
// leading whitespace (e.g. `"power": " 16.0"`).  These helpers accept either a
// JSON number or a JSON string (trimmed) and return None for null / unparseable.

fn de_opt_f32_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr { Num(f32), Str(String), Null }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::Num(v))  => Some(v),
        Some(NumOrStr::Str(s))  => s.trim().parse::<f32>().ok(),
        Some(NumOrStr::Null) | None => None,
    })
}

fn de_opt_u32_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr { Num(u32), Str(String), Null }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::Num(v))  => Some(v),
        Some(NumOrStr::Str(s))  => s.trim().parse::<u32>().ok(),
        Some(NumOrStr::Null) | None => None,
    })
}

/// Actual tt-smi JSON format (from -s/--snapshot)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TTSMIDeviceRaw {
    pub board_info: Option<BoardInfoJSON>,
    pub telemetry: Option<TelemetryJSON>,
    pub smbus_telem: Option<SmbusTelemetryJSON>,
}

/// Board info from tt-smi
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoardInfoJSON {
    pub board_type: Option<String>,
    pub bus_id: Option<String>,
    pub coords: Option<String>,
}

/// Internal device representation (flattened from raw format)
///
/// This matches the structure that tt-smi produces when run with -s flag.
/// The exact structure may vary based on tt-smi version, so all fields are Option.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TTSMIDeviceJSON {
    /// Device index (0-based, derived from array position)
    pub index: Option<usize>,

    /// Board type (e75, e150, n150, n300, p150, p300, etc.)
    pub board_type: Option<String>,

    /// PCIe bus ID (e.g., "0000:01:00.0")
    pub bus_id: Option<String>,

    /// Device coordinates in grid (e.g., "(0,0)")
    pub coords: Option<String>,

    /// Core telemetry (voltage, current, power, temperature)
    pub telemetry: Option<TelemetryJSON>,

    /// SMBUS telemetry (DDR, ARC, firmware, etc.)
    pub smbus: Option<SmbusTelemetryJSON>,
}

/// Core telemetry JSON structure
///
/// tt-smi encodes all numeric values as quoted JSON strings (e.g. `"power": " 16.0"`).
/// The custom deserializer trims whitespace and parses as numeric.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelemetryJSON {
    #[serde(default, deserialize_with = "de_opt_f32_str")]
    pub voltage: Option<f32>,
    #[serde(default, deserialize_with = "de_opt_f32_str")]
    pub current: Option<f32>,
    #[serde(default, deserialize_with = "de_opt_f32_str")]
    pub power: Option<f32>,
    #[serde(default, deserialize_with = "de_opt_f32_str")]
    pub asic_temperature: Option<f32>,
    #[serde(default, deserialize_with = "de_opt_u32_str")]
    pub aiclk: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_u32_str")]
    pub heartbeat: Option<u32>,
}

/// SMBUS telemetry JSON structure — matches actual tt-smi SCREAMING_SNAKE_CASE key names.
///
/// All values arrive as hex strings (e.g. `"DDR_STATUS": "0x55555555"`).
/// Fields not present in a given tt-smi version remain None.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SmbusTelemetryJSON {
    #[serde(rename = "BOARD_ID_HIGH")]
    pub board_id_high: Option<String>,
    #[serde(rename = "BOARD_ID_LOW")]
    pub board_id_low: Option<String>,
    #[serde(rename = "DDR_STATUS")]
    pub ddr_status: Option<String>,
    #[serde(rename = "DDR_SPEED")]
    pub ddr_speed: Option<String>,
    #[serde(rename = "TIMER_HEARTBEAT")]
    pub timer_heartbeat: Option<String>,
    #[serde(rename = "AICLK")]
    pub aiclk: Option<String>,
    #[serde(rename = "AXICLK")]
    pub axiclk: Option<String>,
    #[serde(rename = "ARCCLK")]
    pub arcclk: Option<String>,
    #[serde(rename = "VCORE")]
    pub vcore: Option<String>,
    #[serde(rename = "TDP")]
    pub tdp: Option<String>,
    #[serde(rename = "TDC")]
    pub tdc: Option<String>,
    #[serde(rename = "ASIC_TEMPERATURE")]
    pub asic_temperature: Option<String>,
    #[serde(rename = "VREG_TEMPERATURE")]
    pub vreg_temperature: Option<String>,
    #[serde(rename = "BOARD_TEMPERATURE")]
    pub board_temperature: Option<String>,
    #[serde(rename = "ETH_FW_VERSION")]
    pub eth_fw_version: Option<String>,
    #[serde(rename = "DM_APP_FW_VERSION")]
    pub dm_app_fw_version: Option<String>,
    #[serde(rename = "DM_BL_FW_VERSION")]
    pub dm_bl_fw_version: Option<String>,
    #[serde(rename = "CM_FW_VERSION")]
    pub cm_fw_version: Option<String>,
    #[serde(rename = "FLASH_BUNDLE_VERSION")]
    pub flash_bundle_version: Option<String>,
    #[serde(rename = "TT_FLASH_VERSION")]
    pub tt_flash_version: Option<String>,
    #[serde(rename = "FAN_SPEED")]
    pub fan_speed: Option<String>,
    #[serde(rename = "FAN_RPM")]
    pub fan_rpm: Option<String>,
    #[serde(rename = "PCIE_USAGE")]
    pub pcie_usage: Option<String>,
    #[serde(rename = "ENABLED_TENSIX_COL")]
    pub enabled_tensix_col: Option<String>,
    #[serde(rename = "BOARD_POWER_LIMIT")]
    pub board_power_limit: Option<String>,
    #[serde(rename = "THERM_TRIP_COUNT")]
    pub therm_trip_count: Option<String>,
    #[serde(rename = "VDD_LIMITS")]
    pub vdd_limits: Option<String>,
    #[serde(rename = "THM_LIMIT_SHUTDOWN")]
    pub thm_limit_shutdown: Option<String>,
}

/// JSON backend that runs tt-smi in snapshot mode
///
/// This backend provides real hardware telemetry by running tt-smi with the -s/--snapshot
/// flag on each update. tt-smi outputs a complete JSON snapshot and exits, which we parse
/// to extract device telemetry.
///
/// ## Subprocess Management
///
/// Unlike continuous streaming backends, this runs tt-smi as a one-shot command on each
/// update() call. This matches tt-smi's snapshot mode design where it outputs complete
/// telemetry and exits.
///
/// ## JSON Parsing
///
/// tt-smi outputs a complete JSON structure containing all devices. The backend supports
/// multiple JSON formats:
/// - Array format: `[{device1}, {device2}]`
/// - Wrapper format: `{"devices": [{device1}, {device2}]}`
/// - Single device: `{device}`
///
/// Partial data is acceptable - missing fields are treated as None.
pub struct JSONBackend {
    /// Path to tt-smi executable
    tt_smi_path: String,

    /// Additional command-line arguments for tt-smi
    tt_smi_args: Vec<String>,

    /// List of discovered devices
    devices: Vec<Device>,

    /// Current telemetry for each device
    telemetry: HashMap<usize, Telemetry>,

    /// SMBUS telemetry for each device
    smbus_telemetry: HashMap<usize, SmbusTelemetry>,

    /// Configuration
    config: BackendConfig,

    /// Last successful update time
    last_update: Instant,

    /// Consecutive error count (for backoff)
    error_count: usize,
}

impl JSONBackend {
    /// Create a new JSON backend with default tt-smi path
    ///
    /// # Arguments
    ///
    /// * `tt_smi_path` - Path to tt-smi executable (e.g., "tt-smi" or "/usr/bin/tt-smi")
    ///
    /// # Example
    ///
    /// ```rust
    /// let backend = JSONBackend::new("tt-smi");
    /// ```
    pub fn new(tt_smi_path: impl Into<String>) -> Self {
        Self {
            tt_smi_path: tt_smi_path.into(),
            tt_smi_args: vec!["-s".to_string()],  // Use -s/--snapshot for JSON output
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config: BackendConfig::default(),
            last_update: Instant::now(),
            error_count: 0,
        }
    }

    /// Create JSON backend with custom configuration
    pub fn with_config(tt_smi_path: impl Into<String>, config: BackendConfig) -> Self {
        Self {
            tt_smi_path: tt_smi_path.into(),
            tt_smi_args: vec!["-s".to_string()],  // Use -s/--snapshot for JSON output
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config,
            last_update: Instant::now(),
            error_count: 0,
        }
    }

    /// Run tt-smi and capture its complete JSON output
    fn run_tt_smi(&self) -> BackendResult<String> {
        if self.config.verbose {
            log::info!("JSONBackend: Running: {} {:?}",
                self.tt_smi_path, self.tt_smi_args);
        }

        let output = Command::new(&self.tt_smi_path)
            .args(&self.tt_smi_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                BackendError::SubprocessFailed(format!("Failed to run tt-smi: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackendError::SubprocessFailed(format!(
                "tt-smi failed with status {}: {}",
                output.status,
                stderr
            )));
        }

        let json_output = String::from_utf8_lossy(&output.stdout).to_string();

        if self.config.verbose {
            log::debug!("JSONBackend: Received {} bytes of JSON output", json_output.len());
        }

        Ok(json_output)
    }

    /// Parse JSON output from tt-smi into device telemetry
    fn parse_json(&self, json_str: &str) -> BackendResult<Vec<TTSMIDeviceJSON>> {
        // Try to parse as tt-smi snapshot format with "device_info" key (modern format)
        #[derive(Deserialize)]
        struct TTSMISnapshot {
            device_info: Option<Vec<TTSMIDeviceRaw>>,
        }

        if let Ok(snapshot) = serde_json::from_str::<TTSMISnapshot>(json_str) {
            if let Some(raw_devices) = snapshot.device_info {
                // Transform raw format to flattened format with indices
                let devices: Vec<TTSMIDeviceJSON> = raw_devices
                    .into_iter()
                    .enumerate()
                    .map(|(idx, raw)| {
                        let board_info = raw.board_info.as_ref();
                        TTSMIDeviceJSON {
                            index: Some(idx),
                            board_type: board_info.and_then(|b| b.board_type.clone()),
                            bus_id: board_info.and_then(|b| b.bus_id.clone()),
                            coords: board_info.and_then(|b| b.coords.clone()),
                            telemetry: raw.telemetry,
                            smbus: raw.smbus_telem,
                        }
                    })
                    .collect();
                return Ok(devices);
            }
        }

        // Try to parse as array of devices (legacy format)
        if let Ok(devices) = serde_json::from_str::<Vec<TTSMIDeviceJSON>>(json_str) {
            return Ok(devices);
        }

        // Try to parse as object with "devices" key (legacy format)
        #[derive(Deserialize)]
        struct Wrapper {
            devices: Option<Vec<TTSMIDeviceJSON>>,
        }

        if let Ok(wrapper) = serde_json::from_str::<Wrapper>(json_str) {
            if let Some(devices) = wrapper.devices {
                return Ok(devices);
            }
        }

        // Try to parse as single device (last resort, as it's most permissive)
        if let Ok(device) = serde_json::from_str::<TTSMIDeviceJSON>(json_str) {
            return Ok(vec![device]);
        }

        Err(BackendError::ParseError(format!(
            "Failed to parse JSON output: {}",
            &json_str[..json_str.len().min(100)]
        )))
    }

    /// Update internal state from parsed JSON devices
    fn update_from_json(&mut self, json_devices: Vec<TTSMIDeviceJSON>) -> BackendResult<()> {
        for json_dev in json_devices {
            let idx = json_dev.index.unwrap_or(0);

            // Create device if not exists
            if self.devices.is_empty() || !self.devices.iter().any(|d| d.index == idx) {
                let board_type = json_dev.board_type.clone().unwrap_or_else(|| "unknown".to_string());
                let bus_id = json_dev.bus_id.clone().unwrap_or_else(|| format!("0000:0{}:00.0", idx + 1));
                let coords = json_dev.coords.clone().unwrap_or_else(|| format!("({},{})", idx / 4, idx % 4));

                let device = Device::new(idx, board_type, bus_id, coords);
                self.devices.push(device);
            }

            // Update telemetry if present
            if let Some(telem_json) = json_dev.telemetry {
                let telemetry = Telemetry {
                    voltage: telem_json.voltage,
                    current: telem_json.current,
                    power: telem_json.power,
                    asic_temperature: telem_json.asic_temperature,
                    aiclk: telem_json.aiclk,
                    heartbeat: telem_json.heartbeat,
                    timestamp: Utc::now(),
                };
                self.telemetry.insert(idx, telemetry);
            }

            // Update SMBUS telemetry if present
            if let Some(smbus_json) = json_dev.smbus {
                self.smbus_telemetry.insert(idx, smbus_from_json_fields(smbus_json));
            }
        }

        Ok(())
    }

}

impl TelemetryBackend for JSONBackend {
    fn init(&mut self) -> BackendResult<()> {
        log::info!("JSONBackend: Initializing with tt-smi path: {}", self.tt_smi_path);

        // Run tt-smi to get initial device list
        let json_output = self.run_tt_smi()?;
        let devices = self.parse_json(&json_output)?;
        self.update_from_json(devices)?;

        if self.devices.is_empty() {
            return Err(BackendError::DeviceNotFound(
                "No devices found from tt-smi output".to_string(),
            ));
        }

        log::info!("JSONBackend: Initialization complete, found {} devices", self.devices.len());
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        // Run tt-smi to get fresh telemetry
        match self.run_tt_smi() {
            Ok(json_output) => {
                match self.parse_json(&json_output) {
                    Ok(devices) => {
                        self.update_from_json(devices)?;
                        self.last_update = Instant::now();
                        self.error_count = 0; // Reset error count on success
                    }
                    Err(e) => {
                        self.error_count += 1;
                        if self.config.verbose {
                            log::debug!("JSONBackend: Parse error: {}", e);
                        }
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                self.error_count += 1;

                // Apply exponential backoff on repeated errors
                if self.error_count > 1 {
                    let backoff_ms = (100 * 2_u64.pow((self.error_count - 1).min(5) as u32)).min(5000);
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                }

                return Err(e);
            }
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
        format!("JSON ({} via {})", self.devices.len(), self.tt_smi_path)
    }
}

/// Build a `SmbusTelemetry` from the parsed JSON fields struct.
fn smbus_from_json_fields(smbus_json: SmbusTelemetryJSON) -> SmbusTelemetry {
    // Combine the two 32-bit board-id halves into one string ("0xHHHH-0xLLLLLLLL").
    let board_id = match (smbus_json.board_id_high.as_deref(), smbus_json.board_id_low.as_deref()) {
        (Some(hi), Some(lo)) => Some(format!("{}-{}", hi, lo)),
        (Some(hi), None)     => Some(hi.to_string()),
        (None,     Some(lo)) => Some(lo.to_string()),
        (None,     None)     => None,
    };

    SmbusTelemetry {
        board_id,
        ddr_status:       smbus_json.ddr_status,
        ddr_speed:        smbus_json.ddr_speed,
        // TIMER_HEARTBEAT is the ARC0 health heartbeat counter.
        arc0_health:      smbus_json.timer_heartbeat,
        aiclk:            smbus_json.aiclk,
        axiclk:           smbus_json.axiclk,
        arcclk:           smbus_json.arcclk,
        vcore:            smbus_json.vcore,
        tdp:              smbus_json.tdp,
        tdc:              smbus_json.tdc,
        asic_temperature: smbus_json.asic_temperature,
        vreg_temperature: smbus_json.vreg_temperature,
        board_temperature: smbus_json.board_temperature,
        eth_fw_version:   smbus_json.eth_fw_version,
        m3_app_fw_version: smbus_json.dm_app_fw_version,
        m3_bl_fw_version:  smbus_json.dm_bl_fw_version,
        tt_flash_version:  smbus_json.tt_flash_version,
        fan_speed:        smbus_json.fan_speed,
        pcie_status:      smbus_json.pcie_usage,
        board_power_limit: smbus_json.board_power_limit,
        therm_trip_count: smbus_json.therm_trip_count,
        vdd_limits:       smbus_json.vdd_limits,
        ..SmbusTelemetry::default()
    }
}

/// Parse a complete tt-smi JSON snapshot string into SMBUS telemetry.
///
/// Pure parsing — no subprocess spawn. Called from the HybridBackend reader
/// thread after reading a complete RS-delimited record from the pipe.
/// Returns an empty map if parsing fails or no SMBUS fields are present.
pub(crate) fn parse_smbus_from_json(json_str: &str) -> HashMap<usize, SmbusTelemetry> {
    let helper = JSONBackend::new("");
    let devices = match helper.parse_json(json_str) {
        Ok(d) => d,
        Err(e) => {
            log::debug!("parse_smbus_from_json: parse error: {}", e);
            return HashMap::new();
        }
    };
    let mut result = HashMap::new();
    for dev in devices {
        let idx = dev.index.unwrap_or(0);
        if let Some(smbus_json) = dev.smbus {
            result.insert(idx, smbus_from_json_fields(smbus_json));
        }
    }
    result
}

/// Run tt-smi -s and return SMBUS telemetry for all devices.
///
/// Blocking call — callers must run this from a background thread, NOT the
/// render loop. Falls back to `parse_smbus_from_json` for the actual parsing.
///
/// If tt-smi isn't installed or returns non-zero, returns an empty map.
pub(crate) fn fetch_smbus_snapshot(tt_smi_path: &str) -> HashMap<usize, SmbusTelemetry> {
    let output = match Command::new(tt_smi_path)
        .args(["-s"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            log::debug!("fetch_smbus_snapshot: tt-smi exited with {}", o.status);
            return HashMap::new();
        }
        Err(e) => {
            log::debug!("fetch_smbus_snapshot: failed to run tt-smi: {}", e);
            return HashMap::new();
        }
    };
    let json_str = String::from_utf8_lossy(&output.stdout);
    parse_smbus_from_json(&json_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compact real-world tt-smi snapshot (single device, all key types represented).
    const REAL_TTSMI_JSON: &str = r#"{
        "device_info": [{
            "board_info": {
                "board_type": "p300c",
                "bus_id": "0000:04:00.0",
                "coords": "N/A"
            },
            "telemetry": {
                "voltage": "0.72",
                "current": " 23.0",
                "power": " 16.0",
                "aiclk": " 800",
                "asic_temperature": "34.8",
                "fan_speed": " 38",
                "heartbeat": "11540"
            },
            "smbus_telem": {
                "BOARD_ID_HIGH": "0x461",
                "BOARD_ID_LOW": "0x31924062",
                "DDR_STATUS": "0x55555555",
                "DDR_SPEED": "0x3e80",
                "TIMER_HEARTBEAT": "0x10e7a",
                "AICLK": "0x320",
                "AXICLK": "0x3c0",
                "ARCCLK": "0x320",
                "VCORE": "0x2cf",
                "TDP": "0x10",
                "TDC": "0x17",
                "ASIC_TEMPERATURE": "0x22d522",
                "VREG_TEMPERATURE": "0x0",
                "BOARD_TEMPERATURE": "0x0",
                "ETH_FW_VERSION": "0x10900",
                "DM_APP_FW_VERSION": "0x176300",
                "DM_BL_FW_VERSION": "0x0",
                "FLASH_BUNDLE_VERSION": "0x13076300",
                "CM_FW_VERSION": "0x1d6300",
                "FAN_SPEED": "0x0",
                "FAN_RPM": "0x75a",
                "PCIE_USAGE": "0x4",
                "BOARD_POWER_LIMIT": "0x226",
                "THERM_TRIP_COUNT": "0x0",
                "VDD_LIMITS": "0x38402bc",
                "THM_LIMIT_SHUTDOWN": "0x6e",
                "TT_FLASH_VERSION": null
            }
        }]
    }"#;

    #[test]
    fn test_json_backend_creation() {
        let backend = JSONBackend::new("tt-smi");
        assert_eq!(backend.tt_smi_path, "tt-smi");
        assert_eq!(backend.devices.len(), 0);
    }

    /// Regression: tt-smi sends all telemetry values as quoted strings.
    /// TelemetryJSON must parse them even with leading whitespace.
    #[test]
    fn test_telemetry_string_values_parsed() {
        let backend = JSONBackend::new("");
        let devices = backend.parse_json(REAL_TTSMI_JSON).expect("parse failed");
        assert_eq!(devices.len(), 1);

        let telem = devices[0].telemetry.as_ref().expect("telemetry missing");
        assert_eq!(telem.power,            Some(16.0),  "power string not parsed");
        assert_eq!(telem.voltage,          Some(0.72),  "voltage string not parsed");
        assert_eq!(telem.current,          Some(23.0),  "current string not parsed");
        assert_eq!(telem.asic_temperature, Some(34.8),  "temperature string not parsed");
        assert_eq!(telem.aiclk,            Some(800),   "aiclk string not parsed");
        assert_eq!(telem.heartbeat,        Some(11540), "heartbeat string not parsed");
    }

    /// Regression: smbus_telem uses SCREAMING_SNAKE_CASE keys — must map correctly.
    #[test]
    fn test_smbus_screaming_snake_case_mapped() {
        let backend = JSONBackend::new("");
        let devices = backend.parse_json(REAL_TTSMI_JSON).expect("parse failed");

        let smbus = devices[0].smbus.as_ref().expect("smbus_telem missing");

        assert_eq!(smbus.ddr_status,   Some("0x55555555".to_string()), "DDR_STATUS not mapped");
        assert_eq!(smbus.ddr_speed,    Some("0x3e80".to_string()),      "DDR_SPEED not mapped");
        assert_eq!(smbus.timer_heartbeat, Some("0x10e7a".to_string()),  "TIMER_HEARTBEAT not mapped");
        assert_eq!(smbus.aiclk,        Some("0x320".to_string()),       "AICLK not mapped");
        assert_eq!(smbus.vcore,        Some("0x2cf".to_string()),       "VCORE not mapped");
        assert_eq!(smbus.board_id_high, Some("0x461".to_string()),      "BOARD_ID_HIGH not mapped");
        assert_eq!(smbus.board_id_low,  Some("0x31924062".to_string()), "BOARD_ID_LOW not mapped");
    }

    /// parse_smbus_from_json must produce populated SmbusTelemetry from real format.
    #[test]
    fn test_parse_smbus_from_json_real_format() {
        let result = parse_smbus_from_json(REAL_TTSMI_JSON);
        assert!(!result.is_empty(), "no SMBUS data extracted");

        let smbus = result.get(&0).expect("device 0 missing");
        assert!(smbus.ddr_status.is_some(),  "ddr_status empty after parse");
        assert!(smbus.arc0_health.is_some(), "arc0_health (TIMER_HEARTBEAT) empty after parse");
        assert!(smbus.aiclk.is_some(),       "aiclk empty after parse");
        // board_id must combine HIGH and LOW
        assert!(smbus.board_id.is_some(),    "board_id not combined");
        assert!(smbus.board_id.as_deref().unwrap().contains('-'), "board_id missing separator");
    }

    /// ddr_status_bitmask must parse the hex string "0x55555555".
    #[test]
    fn test_ddr_status_hex_bitmask() {
        let result = parse_smbus_from_json(REAL_TTSMI_JSON);
        let smbus = result.get(&0).expect("device 0 missing");
        // 0x55555555 = 8 channels each in state 0x5 (trained) when read as 4-bit groups
        // The model parses ddr_status as decimal; confirm the raw string is preserved for
        // tron_grid.rs which does its own hex parsing via u64::from_str_radix.
        let raw = smbus.ddr_status.as_deref().expect("ddr_status missing");
        assert!(raw.starts_with("0x"), "ddr_status should be hex string: {}", raw);
    }

    // Legacy format tests kept for coverage.
    #[test]
    fn test_json_parsing_array() {
        let backend = JSONBackend::new("tt-smi");
        let json = r#"[{"index": 0, "board_type": "n150", "telemetry": {"power": 50.0}}]"#;
        let devices = backend.parse_json(json).expect("parse failed");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].board_type, Some("n150".to_string()));
    }

    #[test]
    fn test_json_parsing_wrapper() {
        let backend = JSONBackend::new("tt-smi");
        let json = r#"{"devices": [{"index": 0}, {"index": 1}]}"#;
        let devices = backend.parse_json(json).expect("parse failed");
        assert_eq!(devices.len(), 2);
    }
}
