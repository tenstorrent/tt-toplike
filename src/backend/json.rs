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
//! use tt_toplike::backend::{TelemetryBackend, json::JSONBackend};
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
    enum NumOrStr {
        Num(f32),
        Str(String),
        Null,
    }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::Num(v)) => Some(v),
        Some(NumOrStr::Str(s)) => s.trim().parse::<f32>().ok(),
        Some(NumOrStr::Null) | None => None,
    })
}

fn de_opt_u32_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(u32),
        Str(String),
        Null,
    }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::Num(v)) => Some(v),
        Some(NumOrStr::Str(s)) => s.trim().parse::<u32>().ok(),
        Some(NumOrStr::Null) | None => None,
    })
}

/// Actual tt-smi JSON format (from -s/--snapshot)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TTSMIDeviceRaw {
    pub board_info: Option<BoardInfoJSON>,
    pub telemetry: Option<TelemetryJSON>,
    pub smbus_telem: Option<SmbusTelemetryJSON>,
    /// Firmware bundle + component versions (tt-smi 5.2.0+).
    pub firmwares: Option<serde_json::Value>,
    /// Thermal/power limits (tt-smi 5.2.0+).
    pub limits: Option<serde_json::Value>,
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
    /// Firmware bundle + component versions (tt-smi 5.2.0+).
    pub firmwares: Option<serde_json::Value>,
    /// Thermal/power limits (tt-smi 5.2.0+).
    pub limits: Option<serde_json::Value>,
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
    #[serde(rename = "GDDR_0_1_TEMP")]
    pub gddr_0_1_temp: Option<String>,
    #[serde(rename = "GDDR_2_3_TEMP")]
    pub gddr_2_3_temp: Option<String>,
    #[serde(rename = "GDDR_4_5_TEMP")]
    pub gddr_4_5_temp: Option<String>,
    #[serde(rename = "GDDR_6_7_TEMP")]
    pub gddr_6_7_temp: Option<String>,
    #[serde(rename = "MAX_GDDR_TEMP")]
    pub max_gddr_temp: Option<String>,
    #[serde(rename = "GDDR_0_1_CORR_ERRS")]
    pub gddr_0_1_corr_errs: Option<String>,
    #[serde(rename = "GDDR_2_3_CORR_ERRS")]
    pub gddr_2_3_corr_errs: Option<String>,
    #[serde(rename = "GDDR_4_5_CORR_ERRS")]
    pub gddr_4_5_corr_errs: Option<String>,
    #[serde(rename = "GDDR_6_7_CORR_ERRS")]
    pub gddr_6_7_corr_errs: Option<String>,
    #[serde(rename = "GDDR_UNCORR_ERRS")]
    pub gddr_uncorr_errs: Option<String>,
    #[serde(rename = "HARVESTING_STATE")]
    pub harvesting_state: Option<String>,
    #[serde(rename = "ETH_LIVE_STATUS")]
    pub eth_live_status: Option<String>,
    #[serde(rename = "ENABLED_ETH")]
    pub enabled_eth: Option<String>,
    #[serde(rename = "ENABLED_GDDR")]
    pub enabled_gddr: Option<String>,
    #[serde(rename = "ENABLED_L2CPU")]
    pub enabled_l2cpu: Option<String>,
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

    /// Verbatim stdout of the last *successfully parsed* `tt-smi -s` run.
    ///
    /// Retained for the `--serve` telemetry publisher: it needs to broadcast
    /// "current telemetry" as raw tt-smi JSON, byte-identical to what tt-smi
    /// produced, rather than re-serializing our parsed `Telemetry`/`SmbusTelemetry`
    /// structs (lossy — we don't round-trip every tt-smi field). Left untouched on
    /// a parse error so a single bad tick doesn't blank out the last known-good
    /// snapshot.
    last_raw: Option<String>,
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
            tt_smi_args: vec!["-s".to_string()], // Use -s/--snapshot for JSON output
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config: BackendConfig::default(),
            last_update: Instant::now(),
            error_count: 0,
            last_raw: None,
        }
    }

    /// Create JSON backend with custom configuration
    pub fn with_config(tt_smi_path: impl Into<String>, config: BackendConfig) -> Self {
        Self {
            tt_smi_path: tt_smi_path.into(),
            tt_smi_args: vec!["-s".to_string()], // Use -s/--snapshot for JSON output
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config,
            last_update: Instant::now(),
            error_count: 0,
            last_raw: None,
        }
    }

    /// Run tt-smi and capture its complete JSON output
    fn run_tt_smi(&self) -> BackendResult<String> {
        if self.config.verbose {
            log::info!(
                "JSONBackend: Running: {} {:?}",
                self.tt_smi_path,
                self.tt_smi_args
            );
        }

        let output = Command::new(&self.tt_smi_path)
            .args(&self.tt_smi_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| BackendError::SubprocessFailed(format!("Failed to run tt-smi: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackendError::SubprocessFailed(format!(
                "tt-smi failed with status {}: {}",
                output.status, stderr
            )));
        }

        let json_output = String::from_utf8_lossy(&output.stdout).to_string();

        if self.config.verbose {
            log::debug!(
                "JSONBackend: Received {} bytes of JSON output",
                json_output.len()
            );
        }

        Ok(json_output)
    }

    /// Parse JSON output from tt-smi into the intermediate device list.
    ///
    /// Thin instance-method wrapper around the pure free function
    /// [`parse_json_devices`] so existing white-box tests that call
    /// `backend.parse_json(..)` keep working unchanged. Test-only: the runtime
    /// paths (`init`/`update`) call `parse_tt_smi_snapshot` directly.
    #[cfg(test)]
    fn parse_json(&self, json_str: &str) -> BackendResult<Vec<TTSMIDeviceJSON>> {
        parse_json_devices(json_str)
    }

    /// Test-only: expose update_from_json for white-box testing.
    #[cfg(test)]
    pub fn update_from_json_pub(&mut self, devices: Vec<TTSMIDeviceJSON>) -> BackendResult<()> {
        self.update_from_json(devices)
    }

    /// Test-only: build a `ParsedSnapshot` from an already-parsed device list and
    /// merge it into this backend, preserving the historical append-only device
    /// semantics that `init()`/`update()` rely on.
    #[cfg(test)]
    fn update_from_json(&mut self, json_devices: Vec<TTSMIDeviceJSON>) -> BackendResult<()> {
        self.merge_parsed(parsed_from_json_devices(json_devices));
        Ok(())
    }

    /// Merge a parsed snapshot into the backend's cached state.
    ///
    /// Device list is append-only and keyed by index (devices never disappear
    /// across updates); telemetry and SMBUS maps are replaced per-index. This
    /// reproduces the exact behaviour of the previous `update_from_json`, so the
    /// subprocess path is unchanged by the refactor to a shared parser.
    fn merge_parsed(&mut self, parsed: ParsedSnapshot) {
        for device in parsed.devices {
            if !self.devices.iter().any(|d| d.index == device.index) {
                self.devices.push(device);
            }
        }
        for (idx, telem) in parsed.telemetry {
            self.telemetry.insert(idx, telem);
        }
        for (idx, smbus) in parsed.smbus {
            self.smbus_telemetry.insert(idx, smbus);
        }
    }

    /// Parse a raw `tt-smi -s` output string, merge it into cached state, and —
    /// only on successful parse — retain it verbatim for `snapshot_json()`.
    ///
    /// This is the shared core of `update()`: the subprocess path
    /// (`run_tt_smi()` → this) and the `#[cfg(test)]` seam below both funnel
    /// through here, so a test driving the seam exercises the exact same
    /// parse/merge/retain logic that a live `update()` runs — the only thing the
    /// seam skips is actually spawning `tt-smi`.
    fn apply_raw_snapshot(&mut self, json_output: String) -> BackendResult<()> {
        match parse_tt_smi_snapshot(&json_output) {
            Ok(parsed) => {
                self.merge_parsed(parsed);
                self.last_raw = Some(json_output);
                self.last_update = Instant::now();
                self.error_count = 0; // Reset error count on success
                Ok(())
            }
            Err(e) => {
                self.error_count += 1;
                if self.config.verbose {
                    log::debug!("JSONBackend: Parse error: {}", e);
                }
                Err(e)
            }
        }
    }

    /// Test-only: drive `apply_raw_snapshot` with canned `tt-smi -s` output,
    /// without spawning a real subprocess. See [`apply_raw_snapshot`] for why
    /// this is "the real update() path" minus the subprocess boundary.
    #[cfg(test)]
    pub(crate) fn apply_raw_snapshot_pub(&mut self, json_output: &str) -> BackendResult<()> {
        self.apply_raw_snapshot(json_output.to_string())
    }
}

/// Pure parse of tt-smi snapshot JSON into the intermediate device list.
///
/// Supports every format `tt-smi` (and its legacy variants) can emit:
/// - modern snapshot: `{"device_info": [ {board_info, telemetry, smbus_telem, ..} ]}`
/// - legacy array:     `[{device1}, {device2}]`
/// - legacy wrapper:   `{"devices": [..]}`
/// - single device:    `{device}`
fn parse_json_devices(json_str: &str) -> BackendResult<Vec<TTSMIDeviceJSON>> {
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
                        firmwares: raw.firmwares,
                        limits: raw.limits,
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

    // Truncate for the error message by *characters*, not bytes: `json_str`
    // can be untrusted remote input (a WsBackend frame), and byte-slicing at a
    // fixed offset panics if it lands inside a multi-byte UTF-8 sequence.
    Err(BackendError::ParseError(format!(
        "Failed to parse JSON output: {}",
        json_str.chars().take(100).collect::<String>()
    )))
}

/// Build a `Device` from a single parsed JSON entry, applying the same
/// bus-id / coords fallbacks and firmware/limits decoding the JSON backend has
/// always used.
fn device_from_json(json_dev: &TTSMIDeviceJSON) -> Device {
    let idx = json_dev.index.unwrap_or(0);
    let board_type = json_dev
        .board_type
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let bus_id = json_dev
        .bus_id
        .clone()
        .unwrap_or_else(|| format!("0000:0{}:00.0", idx + 1));
    let coords = json_dev
        .coords
        .clone()
        .unwrap_or_else(|| format!("({},{})", idx / 4, idx % 4));

    // Parse firmwares and limits blocks (tt-smi 5.2.0+), falling back to
    // None for older snapshots that don't include these keys.
    let firmwares = json_dev.firmwares.as_ref().and_then(|v| {
        serde_json::from_value::<crate::models::telemetry::FirmwaresInfo>(v.clone()).ok()
    });
    let limits = json_dev.limits.as_ref().and_then(|v| {
        serde_json::from_value::<crate::models::telemetry::DeviceLimits>(v.clone()).ok()
    });

    let mut device = Device::new(idx, board_type, bus_id, coords);
    device.firmwares = firmwares;
    device.limits = limits;
    device
}

/// Convert a parsed telemetry block into the `Telemetry` model, stamping "now".
fn telemetry_from_json(telem_json: &TelemetryJSON) -> Telemetry {
    Telemetry {
        voltage: telem_json.voltage,
        current: telem_json.current,
        power: telem_json.power,
        asic_temperature: telem_json.asic_temperature,
        aiclk: telem_json.aiclk,
        heartbeat: telem_json.heartbeat,
        timestamp: Utc::now(),
    }
}

/// Fully parsed tt-smi snapshot: the exact triad every backend serves.
///
/// This is the shared parse result that both `JSONBackend` (subprocess path)
/// and `WsBackend` (WebSocket path) build their state from — one schema, one
/// parser, zero mapping.
pub(crate) struct ParsedSnapshot {
    /// Devices in array order (index == position), with firmwares/limits decoded.
    pub devices: Vec<Device>,
    /// Per-device core telemetry (only present devices are keyed).
    pub telemetry: HashMap<usize, Telemetry>,
    /// Per-device SMBUS telemetry (only present devices are keyed).
    pub smbus: HashMap<usize, SmbusTelemetry>,
}

/// Build a `ParsedSnapshot` from an already-parsed intermediate device list.
fn parsed_from_json_devices(json_devices: Vec<TTSMIDeviceJSON>) -> ParsedSnapshot {
    let mut devices = Vec::with_capacity(json_devices.len());
    let mut telemetry = HashMap::new();
    let mut smbus = HashMap::new();

    for json_dev in json_devices {
        let idx = json_dev.index.unwrap_or(0);
        devices.push(device_from_json(&json_dev));
        if let Some(telem_json) = json_dev.telemetry {
            telemetry.insert(idx, telemetry_from_json(&telem_json));
        }
        if let Some(smbus_json) = json_dev.smbus {
            smbus.insert(idx, smbus_from_json_fields(smbus_json));
        }
    }

    ParsedSnapshot {
        devices,
        telemetry,
        smbus,
    }
}

/// Pure parsing entry point: verbatim `tt-smi -s` stdout → the `Device` /
/// `Telemetry` / `SmbusTelemetry` triad every backend serves.
///
/// This is the single shared parser. `JSONBackend` feeds it the stdout of the
/// `tt-smi` subprocess; `WsBackend` feeds it each WebSocket frame (which is,
/// by contract, that same verbatim stdout). Returns a `ParseError` if none of
/// the supported JSON shapes match.
pub(crate) fn parse_tt_smi_snapshot(json_str: &str) -> BackendResult<ParsedSnapshot> {
    let json_devices = parse_json_devices(json_str)?;
    Ok(parsed_from_json_devices(json_devices))
}

impl TelemetryBackend for JSONBackend {
    fn init(&mut self) -> BackendResult<()> {
        log::info!(
            "JSONBackend: Initializing with tt-smi path: {}",
            self.tt_smi_path
        );

        // Run tt-smi to get initial device list
        let json_output = self.run_tt_smi()?;
        let parsed = parse_tt_smi_snapshot(&json_output)?;
        self.merge_parsed(parsed);

        if self.devices.is_empty() {
            return Err(BackendError::DeviceNotFound(
                "No devices found from tt-smi output".to_string(),
            ));
        }

        log::info!(
            "JSONBackend: Initialization complete, found {} devices",
            self.devices.len()
        );
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        // Run tt-smi to get fresh telemetry
        match self.run_tt_smi() {
            Ok(json_output) => {
                self.apply_raw_snapshot(json_output)?;
            }
            Err(e) => {
                self.error_count += 1;

                // Apply exponential backoff on repeated errors
                if self.error_count > 1 {
                    let backoff_ms =
                        (100 * 2_u64.pow((self.error_count - 1).min(5) as u32)).min(5000);
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

    fn snapshot_json(&self) -> Option<String> {
        self.last_raw.clone()
    }
}

/// Build a `SmbusTelemetry` from the parsed JSON fields struct.
fn smbus_from_json_fields(smbus_json: SmbusTelemetryJSON) -> SmbusTelemetry {
    // Combine the two 32-bit board-id halves into one string ("0xHHHH-0xLLLLLLLL").
    let board_id = match (
        smbus_json.board_id_high.as_deref(),
        smbus_json.board_id_low.as_deref(),
    ) {
        (Some(hi), Some(lo)) => Some(format!("{}-{}", hi, lo)),
        (Some(hi), None) => Some(hi.to_string()),
        (None, Some(lo)) => Some(lo.to_string()),
        (None, None) => None,
    };

    SmbusTelemetry {
        board_id,
        ddr_status: smbus_json.ddr_status,
        ddr_speed: smbus_json.ddr_speed,
        // TIMER_HEARTBEAT is the ARC0 health heartbeat counter.
        arc0_health: smbus_json.timer_heartbeat,
        aiclk: smbus_json.aiclk,
        axiclk: smbus_json.axiclk,
        arcclk: smbus_json.arcclk,
        vcore: smbus_json.vcore,
        tdp: smbus_json.tdp,
        tdc: smbus_json.tdc,
        asic_temperature: smbus_json.asic_temperature,
        vreg_temperature: smbus_json.vreg_temperature,
        board_temperature: smbus_json.board_temperature,
        eth_fw_version: smbus_json.eth_fw_version,
        m3_app_fw_version: smbus_json.dm_app_fw_version,
        m3_bl_fw_version: smbus_json.dm_bl_fw_version,
        tt_flash_version: smbus_json.tt_flash_version,
        fan_speed: smbus_json.fan_speed,
        pcie_status: smbus_json.pcie_usage,
        board_power_limit: smbus_json.board_power_limit,
        therm_trip_count: smbus_json.therm_trip_count,
        vdd_limits: smbus_json.vdd_limits,
        // ── GDDR temperatures (tt-smi 5.2.0+) ──────────────────────────────────
        gddr_temps: [
            smbus_json
                .gddr_0_1_temp
                .as_deref()
                .and_then(crate::models::telemetry::unpack_gddr_temps),
            smbus_json
                .gddr_2_3_temp
                .as_deref()
                .and_then(crate::models::telemetry::unpack_gddr_temps),
            smbus_json
                .gddr_4_5_temp
                .as_deref()
                .and_then(crate::models::telemetry::unpack_gddr_temps),
            smbus_json
                .gddr_6_7_temp
                .as_deref()
                .and_then(crate::models::telemetry::unpack_gddr_temps),
        ],
        max_gddr_temp: smbus_json
            .max_gddr_temp
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s))
            .map(|v| v as f32),
        // ── GDDR error counters (tt-smi 5.2.0+) ────────────────────────────────
        gddr_corr_errs: [
            smbus_json
                .gddr_0_1_corr_errs
                .as_deref()
                .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
            smbus_json
                .gddr_2_3_corr_errs
                .as_deref()
                .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
            smbus_json
                .gddr_4_5_corr_errs
                .as_deref()
                .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
            smbus_json
                .gddr_6_7_corr_errs
                .as_deref()
                .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
        ],
        gddr_uncorr_errs: smbus_json
            .gddr_uncorr_errs
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
        // ── Harvesting / enabled bitmasks (tt-smi 5.2.0+) ──────────────────────
        harvesting_state: smbus_json
            .harvesting_state
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
        eth_live_status: smbus_json
            .eth_live_status
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec_64(s)),
        enabled_eth: smbus_json
            .enabled_eth
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
        enabled_gddr: smbus_json
            .enabled_gddr
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
        enabled_l2cpu: smbus_json
            .enabled_l2cpu
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
        enabled_tensix_col: smbus_json
            .enabled_tensix_col
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s)),
        // THM_LIMIT_SHUTDOWN is the thermal shutdown trip point in °C (may be hex-encoded).
        // Map it to thm_limits so starfield's thermal-headroom logic can use it.
        thm_limits: smbus_json
            .thm_limit_shutdown
            .as_deref()
            .and_then(|s| crate::models::telemetry::parse_hex_or_dec(s))
            .map(|v| v.to_string()),
        ..SmbusTelemetry::default()
    }
}

/// Parse result from a single tt-smi JSON snapshot, extracting both SMBUS
/// telemetry and device metadata (firmwares, limits) in one pass.
///
/// Used by `HybridBackend`, whose background reader only needs the SMBUS map
/// plus firmware/limits metadata (it gets its live device list from sysfs).
/// Distinct from [`ParsedSnapshot`], which carries the full device triad.
pub(crate) struct SnapshotParts {
    pub smbus: HashMap<usize, SmbusTelemetry>,
    pub meta: HashMap<
        usize,
        (
            Option<crate::models::telemetry::FirmwaresInfo>,
            Option<crate::models::telemetry::DeviceLimits>,
        ),
    >,
}

/// Parse a tt-smi JSON snapshot once and extract both SMBUS telemetry and
/// device metadata.  Replaces the previous two-function pattern that parsed
/// the same JSON document twice per background poll cycle.
pub(crate) fn parse_snapshot(json_str: &str) -> SnapshotParts {
    let devices = match parse_json_devices(json_str) {
        Ok(d) => d,
        Err(e) => {
            log::debug!("parse_snapshot: parse error: {}", e);
            return SnapshotParts {
                smbus: HashMap::new(),
                meta: HashMap::new(),
            };
        }
    };
    let mut smbus_map = HashMap::new();
    let mut meta_map = HashMap::new();
    for dev in devices {
        let idx = dev.index.unwrap_or(0);
        if let Some(smbus_json) = dev.smbus {
            smbus_map.insert(idx, smbus_from_json_fields(smbus_json));
        }
        let firmwares = dev.firmwares.as_ref().and_then(|v| {
            serde_json::from_value::<crate::models::telemetry::FirmwaresInfo>(v.clone()).ok()
        });
        let limits = dev.limits.as_ref().and_then(|v| {
            serde_json::from_value::<crate::models::telemetry::DeviceLimits>(v.clone()).ok()
        });
        if firmwares.is_some() || limits.is_some() {
            meta_map.insert(idx, (firmwares, limits));
        }
    }
    SnapshotParts {
        smbus: smbus_map,
        meta: meta_map,
    }
}

#[cfg(test)]
pub(crate) fn parse_smbus_from_json(json_str: &str) -> HashMap<usize, SmbusTelemetry> {
    parse_snapshot(json_str).smbus
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
        assert_eq!(telem.power, Some(16.0), "power string not parsed");
        assert_eq!(telem.voltage, Some(0.72), "voltage string not parsed");
        assert_eq!(telem.current, Some(23.0), "current string not parsed");
        assert_eq!(
            telem.asic_temperature,
            Some(34.8),
            "temperature string not parsed"
        );
        assert_eq!(telem.aiclk, Some(800), "aiclk string not parsed");
        assert_eq!(telem.heartbeat, Some(11540), "heartbeat string not parsed");
    }

    /// Regression: smbus_telem uses SCREAMING_SNAKE_CASE keys — must map correctly.
    #[test]
    fn test_smbus_screaming_snake_case_mapped() {
        let backend = JSONBackend::new("");
        let devices = backend.parse_json(REAL_TTSMI_JSON).expect("parse failed");

        let smbus = devices[0].smbus.as_ref().expect("smbus_telem missing");

        assert_eq!(
            smbus.ddr_status,
            Some("0x55555555".to_string()),
            "DDR_STATUS not mapped"
        );
        assert_eq!(
            smbus.ddr_speed,
            Some("0x3e80".to_string()),
            "DDR_SPEED not mapped"
        );
        assert_eq!(
            smbus.timer_heartbeat,
            Some("0x10e7a".to_string()),
            "TIMER_HEARTBEAT not mapped"
        );
        assert_eq!(smbus.aiclk, Some("0x320".to_string()), "AICLK not mapped");
        assert_eq!(smbus.vcore, Some("0x2cf".to_string()), "VCORE not mapped");
        assert_eq!(
            smbus.board_id_high,
            Some("0x461".to_string()),
            "BOARD_ID_HIGH not mapped"
        );
        assert_eq!(
            smbus.board_id_low,
            Some("0x31924062".to_string()),
            "BOARD_ID_LOW not mapped"
        );
    }

    /// parse_smbus_from_json must produce populated SmbusTelemetry from real format.
    #[test]
    fn test_parse_smbus_from_json_real_format() {
        let result = parse_smbus_from_json(REAL_TTSMI_JSON);
        assert!(!result.is_empty(), "no SMBUS data extracted");

        let smbus = result.get(&0).expect("device 0 missing");
        assert!(smbus.ddr_status.is_some(), "ddr_status empty after parse");
        assert!(
            smbus.arc0_health.is_some(),
            "arc0_health (TIMER_HEARTBEAT) empty after parse"
        );
        assert!(smbus.aiclk.is_some(), "aiclk empty after parse");
        // board_id must combine HIGH and LOW
        assert!(smbus.board_id.is_some(), "board_id not combined");
        assert!(
            smbus.board_id.as_deref().unwrap().contains('-'),
            "board_id missing separator"
        );
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
        assert!(
            raw.starts_with("0x"),
            "ddr_status should be hex string: {}",
            raw
        );
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

    // ── tt-smi 5.2.0 snapshot with firmwares, limits, and new SMBUS fields ────

    const TTSMI_52_JSON: &str = r#"{
        "device_info": [{
            "board_info": {
                "board_type": "p150a",
                "bus_id": "0000:01:00.0",
                "coords": "(0,0)"
            },
            "telemetry": {
                "voltage": "0.85",
                "current": "30.0",
                "power": "145.0",
                "aiclk": "800",
                "asic_temperature": "52.0",
                "heartbeat": "1234"
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
                "ASIC_TEMPERATURE": "0x22",
                "VREG_TEMPERATURE": "0x1e",
                "BOARD_TEMPERATURE": "0x1a",
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
                "TT_FLASH_VERSION": null,
                "GDDR_0_1_TEMP": "0x262a2c2c",
                "GDDR_2_3_TEMP": "0x28282a2a",
                "GDDR_4_5_TEMP": "0x24262828",
                "GDDR_6_7_TEMP": "0x22242626",
                "MAX_GDDR_TEMP": "0x2c",
                "HARVESTING_STATE": "0x0",
                "ETH_LIVE_STATUS": "0x00000000000000FF",
                "ENABLED_ETH": "0xFF",
                "ENABLED_GDDR": "0xFF",
                "ENABLED_L2CPU": "0x0",
                "ENABLED_TENSIX_COL": "0x3FFF"
            },
            "firmwares": {
                "fw_bundle_version": "fw_pack-19.9.0",
                "eth_fw": "6.16.0.0",
                "cm_fw": "v80.15.0.0",
                "gddr_fw": "1.37"
            },
            "limits": {
                "tdp_limit": 300.0,
                "tdc_limit": 40.0,
                "asic_fmax": 800,
                "thm_limit": 105.0
            }
        }]
    }"#;

    #[test]
    fn test_52_gddr_temps_parsed() {
        let result = parse_smbus_from_json(TTSMI_52_JSON);
        let smbus = result.get(&0).expect("device 0 missing");
        // GDDR_0_1_TEMP "0x262a2c2c" → bytes LE: 0x2c, 0x2c, 0x2a, 0x26 → [44, 44, 42, 38]
        let pair = smbus.gddr_temps[0].expect("gddr_temps[0] missing");
        assert_eq!(pair.0, [44.0_f32, 44.0, 42.0, 38.0]);
        assert!(smbus.max_gddr_temp.is_some());
    }

    #[test]
    fn test_52_eth_live_status_parsed() {
        let result = parse_smbus_from_json(TTSMI_52_JSON);
        let smbus = result.get(&0).expect("device 0 missing");
        assert_eq!(smbus.eth_live_status, Some(0xFF_u64));
    }

    #[test]
    fn test_52_harvesting_state_parsed() {
        let result = parse_smbus_from_json(TTSMI_52_JSON);
        let smbus = result.get(&0).expect("device 0 missing");
        assert_eq!(smbus.harvesting_state, Some(0_u32));
        assert_eq!(smbus.enabled_tensix_col, Some(0x3FFF_u32));
    }

    #[test]
    fn test_52_firmwares_block_parsed() {
        let mut b = JSONBackend::new("");
        let json_devs = b.parse_json(TTSMI_52_JSON).unwrap();
        b.update_from_json_pub(json_devs).unwrap();
        let dev = &b.devices()[0];
        let fw = dev.firmwares.as_ref().expect("firmwares missing");
        assert_eq!(fw.fw_bundle_version.as_deref(), Some("fw_pack-19.9.0"));
        assert_eq!(fw.eth_fw.as_deref(), Some("6.16.0.0"));
    }

    #[test]
    fn test_52_limits_block_parsed() {
        let mut b = JSONBackend::new("");
        let json_devs = b.parse_json(TTSMI_52_JSON).unwrap();
        b.update_from_json_pub(json_devs).unwrap();
        let dev = &b.devices()[0];
        let lim = dev.limits.as_ref().expect("limits missing");
        assert_eq!(lim.tdp_limit, Some(300.0_f32));
        assert_eq!(lim.asic_fmax, Some(800_u32));
    }

    #[test]
    fn test_parse_snapshot_meta() {
        // Verify the HybridBackend code path: parse_snapshot() must extract
        // both SMBUS telemetry and firmware/limits metadata in one pass.
        let snap = parse_snapshot(TTSMI_52_JSON);
        // SMBUS path unchanged
        let smbus = snap.smbus.get(&0).expect("SMBUS for device 0 missing");
        assert!(
            smbus.eth_live_status.is_some(),
            "eth_live_status should be populated"
        );
        // Meta path (used by HybridBackend, not covered by other tests)
        let (fw, lim) = snap.meta.get(&0).expect("meta for device 0 missing");
        let fw = fw.as_ref().expect("firmwares should be Some");
        assert_eq!(
            fw.fw_bundle_version.as_deref(),
            Some("fw_pack-19.9.0"),
            "parse_snapshot fw_bundle_version mismatch"
        );
        let lim = lim.as_ref().expect("limits should be Some");
        assert_eq!(
            lim.tdp_limit,
            Some(300.0_f32),
            "parse_snapshot tdp_limit mismatch"
        );
    }

    /// The shared pure parser must yield the full device triad — the exact
    /// state `JSONBackend` and `WsBackend` both serve. Protects the refactor
    /// that lifted parsing out of the subprocess backend.
    #[test]
    fn test_parse_tt_smi_snapshot_full_triad() {
        let parsed = parse_tt_smi_snapshot(TTSMI_52_JSON).expect("parse failed");

        // One device, with architecture derived from board_type.
        assert_eq!(parsed.devices.len(), 1);
        let dev = &parsed.devices[0];
        assert_eq!(dev.index, 0);
        assert_eq!(dev.board_type, "p150a");
        assert_eq!(dev.architecture, crate::models::Architecture::Blackhole);
        // Firmwares/limits decoded onto the device.
        assert_eq!(
            dev.firmwares.as_ref().and_then(|f| f.eth_fw.as_deref()),
            Some("6.16.0.0")
        );
        assert_eq!(dev.limits.as_ref().and_then(|l| l.asic_fmax), Some(800));

        // Core telemetry populated (values arrive as quoted strings in tt-smi).
        let telem = parsed.telemetry.get(&0).expect("telemetry missing");
        assert_eq!(telem.power, Some(145.0));
        assert_eq!(telem.asic_temperature, Some(52.0));
        assert_eq!(telem.aiclk, Some(800));

        // SMBUS telemetry populated.
        let smbus = parsed.smbus.get(&0).expect("smbus missing");
        assert_eq!(smbus.eth_live_status, Some(0xFF_u64));
        assert_eq!(smbus.enabled_tensix_col, Some(0x3FFF_u32));
    }

    /// Malformed JSON must surface as an error, not a panic.
    #[test]
    fn test_parse_tt_smi_snapshot_bad_json_errors() {
        assert!(parse_tt_smi_snapshot("not json at all").is_err());
    }

    /// `snapshot_json()` must be `None` before any successful update, and must
    /// return the exact raw `tt-smi -s` output after one. Drives the same
    /// parse+merge+store-raw logic `update()` uses (via the `apply_raw_snapshot`
    /// test seam), just without spawning a real `tt-smi` subprocess — mirroring
    /// the existing `update_from_json_pub` white-box pattern in this file.
    #[test]
    fn test_snapshot_json_returns_last_raw_after_update() {
        let mut backend = JSONBackend::new("tt-smi");
        assert_eq!(
            backend.snapshot_json(),
            None,
            "snapshot_json should be None before any successful update"
        );

        backend
            .apply_raw_snapshot_pub(REAL_TTSMI_JSON)
            .expect("apply_raw_snapshot should succeed on well-formed JSON");

        assert_eq!(
            backend.snapshot_json(),
            Some(REAL_TTSMI_JSON.to_string()),
            "snapshot_json must return the exact raw tt-smi JSON byte-identical"
        );
    }

    /// A parse failure must not clobber a previously stored good snapshot, and
    /// must not fabricate a snapshot out of unparseable output.
    #[test]
    fn test_snapshot_json_unchanged_on_parse_error() {
        let mut backend = JSONBackend::new("tt-smi");

        backend
            .apply_raw_snapshot_pub(REAL_TTSMI_JSON)
            .expect("first apply should succeed");
        assert_eq!(backend.snapshot_json(), Some(REAL_TTSMI_JSON.to_string()));

        // A bad frame errors and must not overwrite the last good snapshot.
        assert!(backend.apply_raw_snapshot_pub("not json at all").is_err());
        assert_eq!(
            backend.snapshot_json(),
            Some(REAL_TTSMI_JSON.to_string()),
            "a failed parse must not clobber the last known-good snapshot"
        );
    }

    #[test]
    fn test_old_format_still_parses_ok() {
        // Existing pre-5.2 snapshot (no GDDR fields) must still parse without error.
        let result = parse_smbus_from_json(REAL_TTSMI_JSON);
        let smbus = result.get(&0).expect("device 0 missing");
        assert!(
            smbus.gddr_temps.iter().all(|t| t.is_none()),
            "old format should leave gddr_temps as None"
        );
    }

    /// Regression: unparseable input whose 100-byte boundary lands inside a
    /// multi-byte UTF-8 sequence must not panic. `parse_json_devices` truncates
    /// the error message by *characters*, not bytes — a byte-slice at offset 100
    /// would panic ("byte index 100 is not a char boundary") on frames like the
    /// untrusted ones a remote `WsBackend` peer can send.
    #[test]
    fn test_parse_error_truncation_is_char_boundary_safe() {
        // 99 ASCII bytes, then a 4-byte emoji straddling byte offset 100 — the
        // old `&json_str[..100]` slice landed mid-codepoint here.
        let mut junk = "x".repeat(99);
        junk.push('🚀'); // bytes 99..103; a fixed byte-slice at 100 splits it
        junk.push_str(" not json");

        let err = parse_json_devices(&junk).expect_err("garbage must fail to parse");
        // Message is produced without panicking and carries the char-truncated tail.
        assert!(matches!(err, BackendError::ParseError(_)));
    }
}
