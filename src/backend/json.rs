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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Instant;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelemetryJSON {
    pub voltage: Option<f32>,
    pub current: Option<f32>,
    pub power: Option<f32>,
    pub asic_temperature: Option<f32>,
    pub aiclk: Option<u32>,
    pub heartbeat: Option<u32>,
}

/// SMBUS telemetry JSON structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SmbusTelemetryJSON {
    pub ddr_speed: Option<String>,
    pub ddr_status: Option<String>,
    pub arc0_health: Option<String>,
    pub arc1_health: Option<String>,
    pub arc2_health: Option<String>,
    pub arc3_health: Option<String>,
    pub arc0_fw_version: Option<String>,
    pub board_id: Option<String>,
    pub device_id: Option<String>,
    // Add more fields as needed based on actual tt-smi JSON output
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
                let smbus = SmbusTelemetry {
                    ddr_speed: smbus_json.ddr_speed,
                    ddr_status: smbus_json.ddr_status,
                    arc0_health: smbus_json.arc0_health,
                    arc1_health: smbus_json.arc1_health,
                    arc2_health: smbus_json.arc2_health,
                    arc3_health: smbus_json.arc3_health,
                    arc0_fw_version: smbus_json.arc0_fw_version,
                    board_id: smbus_json.board_id,
                    device_id: smbus_json.device_id,
                    // Set remaining fields to None (will be filled from actual tt-smi output)
                    enum_version: None,
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
                    asic_tmon0: None,
                    asic_tmon1: None,
                    vcore: None,
                    tdp: None,
                    tdc: None,
                    throttler: None,
                    vdd_limits: None,
                    thm_limits: None,
                    input_power: None,
                    board_power_limit: None,
                    mvddq_power: None,
                    fan_speed: None,
                    faults: None,
                    pcie_status: None,
                    eth_status0: None,
                    eth_status1: None,
                    eth_debug_status0: None,
                    eth_debug_status1: None,
                    aux_status: None,
                    gddr_train_temp0: None,
                    gddr_train_temp1: None,
                    therm_trip_count: None,
                    boot_date: None,
                    rt_seconds: None,
                    wh_fw_date: None,
                };
                self.smbus_telemetry.insert(idx, smbus);
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

/// Run tt-smi -s and return SMBUS telemetry for all devices.
///
/// This is a best-effort helper for HybridBackend's background refresh thread.
/// Returns an empty map on any error — callers should handle the no-data case gracefully.
pub(crate) fn fetch_smbus_snapshot(tt_smi_path: &str) -> HashMap<usize, SmbusTelemetry> {
    use std::collections::HashMap as HM;

    let output = match Command::new(tt_smi_path)
        .args(["-s"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            log::debug!("fetch_smbus_snapshot: tt-smi exited with {}", o.status);
            return HM::new();
        }
        Err(e) => {
            log::debug!("fetch_smbus_snapshot: failed to run tt-smi: {}", e);
            return HM::new();
        }
    };

    let json_str = String::from_utf8_lossy(&output.stdout);
    let helper = JSONBackend::new(tt_smi_path);
    let devices = match helper.parse_json(&json_str) {
        Ok(d) => d,
        Err(e) => {
            log::debug!("fetch_smbus_snapshot: parse error: {}", e);
            return HM::new();
        }
    };

    let mut result = HM::new();
    for dev in devices {
        let idx = dev.index.unwrap_or(0);
        if let Some(smbus_json) = dev.smbus {
            let smbus = SmbusTelemetry {
                ddr_speed: smbus_json.ddr_speed,
                ddr_status: smbus_json.ddr_status,
                arc0_health: smbus_json.arc0_health,
                arc1_health: smbus_json.arc1_health,
                arc2_health: smbus_json.arc2_health,
                arc3_health: smbus_json.arc3_health,
                arc0_fw_version: smbus_json.arc0_fw_version,
                board_id: smbus_json.board_id,
                device_id: smbus_json.device_id,
                enum_version: None,
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
                asic_tmon0: None,
                asic_tmon1: None,
                vcore: None,
                tdp: None,
                tdc: None,
                throttler: None,
                vdd_limits: None,
                thm_limits: None,
                input_power: None,
                board_power_limit: None,
                mvddq_power: None,
                fan_speed: None,
                faults: None,
                pcie_status: None,
                eth_status0: None,
                eth_status1: None,
                eth_debug_status0: None,
                eth_debug_status1: None,
                aux_status: None,
                gddr_train_temp0: None,
                gddr_train_temp1: None,
                therm_trip_count: None,
                boot_date: None,
                rt_seconds: None,
                wh_fw_date: None,
            };
            result.insert(idx, smbus);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_backend_creation() {
        let backend = JSONBackend::new("tt-smi");
        assert_eq!(backend.tt_smi_path, "tt-smi");
        assert_eq!(backend.devices.len(), 0);
    }

    #[test]
    fn test_json_parsing_array() {
        let backend = JSONBackend::new("tt-smi");
        let json = r#"[{"index": 0, "board_type": "n150", "telemetry": {"power": 50.0}}]"#;

        let result = backend.parse_json(json);
        assert!(result.is_ok());

        let devices = result.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].index, Some(0));
        assert_eq!(devices[0].board_type, Some("n150".to_string()));
    }

    #[test]
    fn test_json_parsing_single() {
        let backend = JSONBackend::new("tt-smi");
        let json = r#"{"index": 0, "board_type": "e150", "telemetry": {"power": 40.0}}"#;

        let result = backend.parse_json(json);
        assert!(result.is_ok());

        let devices = result.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].board_type, Some("e150".to_string()));
    }

    #[test]
    fn test_json_parsing_wrapper() {
        let backend = JSONBackend::new("tt-smi");
        let json = r#"{"devices": [{"index": 0}, {"index": 1}]}"#;

        let result = backend.parse_json(json);
        assert!(result.is_ok());

        let devices = result.unwrap();
        assert_eq!(devices.len(), 2);
    }
}
