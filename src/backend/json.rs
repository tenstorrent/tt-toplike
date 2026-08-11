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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Instant;

// tt-smi encodes numeric telemetry values as quoted JSON strings, possibly with
// leading whitespace (e.g. `"power": " 16.0"`). These helpers accept either a
// JSON number or a JSON string (trimmed) and return None for null /
// unparseable. They live in `models::serde_num` because `DeviceLimits` — a
// model, which must not depend on this backend — needs exactly the same
// tolerance for tt-smi's string-valued `limits` block.
use crate::models::serde_num::{de_opt_f32_str, de_opt_u32_str};

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
    /// PCIe generation — a bare number (4) in tt-smi 5.3.0, but tolerate a
    /// quoted string too (tt-smi has flip-flopped on number-vs-string before).
    #[serde(default, deserialize_with = "de_opt_u32_str")]
    pub pcie_speed: Option<u32>,
    /// PCIe lane width — a STRING ("4") in tt-smi 5.3.0.
    #[serde(default, deserialize_with = "de_opt_u32_str")]
    pub pcie_width: Option<u32>,
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
    /// PCIe generation from board_info (tt-smi 5.x).
    #[serde(default)]
    pub pcie_speed: Option<u32>,
    /// PCIe lane width from board_info (tt-smi 5.x).
    #[serde(default)]
    pub pcie_width: Option<u32>,
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
    /// Total board input power in watts (tt-smi ≥ 6.0.0). See
    /// [`Telemetry::board_power`](crate::models::Telemetry) for the
    /// per-asic-vs-whole-board distinction.
    #[serde(default, deserialize_with = "de_opt_f32_str")]
    pub board_power: Option<f32>,
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

    /// Top-level per-device process attribution (tt-smi ≥ 6.0.0 `processes[]`).
    /// Empty for older tt-smi snapshots that predate the field.
    processes: Vec<crate::models::DeviceProcess>,

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
            processes: Vec::new(),
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
            processes: Vec::new(),
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
    /// `backend.parse_json(..)` keep working unchanged (they don't care about
    /// process attribution, so it's discarded here). Test-only: the runtime
    /// paths (`init`/`update`) call `parse_tt_smi_snapshot` directly.
    #[cfg(test)]
    fn parse_json(&self, json_str: &str) -> BackendResult<Vec<TTSMIDeviceJSON>> {
        parse_json_devices(json_str).map(|(devices, _processes)| devices)
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
        // Unlike telemetry/smbus (keyed maps merged per-device), `processes[]`
        // is a whole-system snapshot each tick — replace wholesale rather than
        // accumulate, so an exited process doesn't linger forever. A snapshot
        // shape that predates the field parses to an empty Vec, which would
        // otherwise wipe a previously-seen process list on a single old-format
        // frame; in practice this never happens mid-stream (the tt-smi version
        // is fixed for the process's lifetime), so this stays a plain replace.
        self.processes = parsed.processes;
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

/// Best-effort decode of the top-level `processes[]` block into typed rows.
///
/// Decoding is **per row**: a row tt-smi wrote in an unexpected shape is
/// skipped, the rest still land, and nothing here can ever affect the device
/// list (the caller has already parsed that). Anything that isn't a JSON array
/// — `null`, an object, a number — yields an empty list, matching what older
/// tt-smi versions (which don't emit the key at all) produce.
///
/// `DeviceProcess`'s own fields use tolerant number-or-string deserializers, so
/// `"pid": "4242"` decodes rather than being skipped.
fn decode_processes(value: Option<serde_json::Value>) -> Vec<crate::models::DeviceProcess> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    let total = rows.len();
    let decoded: Vec<crate::models::DeviceProcess> = rows
        .into_iter()
        .filter_map(|row| serde_json::from_value::<crate::models::DeviceProcess>(row).ok())
        .collect();
    if decoded.len() != total {
        log::warn!(
            "tt-smi processes[]: decoded {} of {} rows; the rest had unexpected value shapes",
            decoded.len(),
            total
        );
    }
    decoded
}

/// Salvage a document that carries `device_info` but failed the strict
/// modern-shape parse, by decoding each `device_info[]` entry independently.
///
/// Returns `None` when the document isn't the modern shape at all (no
/// `device_info` array) — the caller then continues down the legacy branches.
/// Returns `Some` (possibly with fewer devices than the array had) when it is:
/// one card whose telemetry block tt-smi emitted in an unexpected shape then
/// costs only that card, instead of the whole document falling through to the
/// single-device branch and reporting 1 phantom device for the entire box.
///
/// Index attribution is preserved by *array position*, not by "position among
/// the entries that decoded": a skipped entry leaves its index unused rather
/// than shifting every later card's SMBUS/limits onto its neighbour.
fn salvage_modern_snapshot(
    json_str: &str,
) -> Option<(Vec<TTSMIDeviceJSON>, Vec<crate::models::DeviceProcess>)> {
    let mut doc: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = doc.as_object_mut()?;
    let processes = decode_processes(obj.remove("processes"));
    let entries = match obj.remove("device_info") {
        Some(serde_json::Value::Array(entries)) => entries,
        _ => return None,
    };

    let devices = entries
        .into_iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let raw = match serde_json::from_value::<TTSMIDeviceRaw>(entry) {
                Ok(raw) => raw,
                Err(e) => {
                    log::warn!("tt-smi device_info[{idx}] could not be decoded ({e}); skipping");
                    return None;
                }
            };
            let board_info = raw.board_info.as_ref();
            Some(TTSMIDeviceJSON {
                index: Some(idx),
                board_type: board_info.and_then(|b| b.board_type.clone()),
                bus_id: board_info.and_then(|b| b.bus_id.clone()),
                coords: board_info.and_then(|b| b.coords.clone()),
                telemetry: raw.telemetry,
                smbus: raw.smbus_telem,
                firmwares: raw.firmwares,
                limits: raw.limits,
                pcie_speed: board_info.and_then(|b| b.pcie_speed),
                pcie_width: board_info.and_then(|b| b.pcie_width),
            })
        })
        .collect();

    Some((devices, processes))
}

/// Pure parse of tt-smi snapshot JSON into the intermediate device list, plus
/// any top-level per-device process attribution (tt-smi ≥ 6.0.0 `processes[]`).
///
/// Supports every format `tt-smi` (and its legacy variants) can emit:
/// - modern snapshot: `{"device_info": [ {board_info, telemetry, smbus_telem, ..} ], "processes": [..]}`
/// - legacy array:     `[{device1}, {device2}]`
/// - legacy wrapper:   `{"devices": [..]}`
/// - single device:    `{device}`
///
/// Only the modern snapshot shape carries `processes[]` — every legacy branch
/// returns an empty process list (those tt-smi versions predate the field).
fn parse_json_devices(
    json_str: &str,
) -> BackendResult<(Vec<TTSMIDeviceJSON>, Vec<crate::models::DeviceProcess>)> {
    // Try to parse as tt-smi snapshot format with "device_info" key (modern format)
    #[derive(Deserialize)]
    struct TTSMISnapshot {
        device_info: Option<Vec<TTSMIDeviceRaw>>,
        /// tt-smi ≥ 6.0.0: top-level per-device process attribution.
        ///
        /// **Deliberately a raw `Value`, not `Vec<DeviceProcess>`.** This struct
        /// is the gate for the entire modern-snapshot branch: if *any* value in
        /// the document fails to fit its declared type, `from_str` fails, this
        /// branch is skipped, and the permissive single-device fallthrough at the
        /// bottom of this function succeeds instead — silently collapsing an
        /// N-device box to one phantom device with a fabricated bus id and no
        /// telemetry. `processes[]` is the least load-bearing block in the
        /// snapshot but was the only one typed strictly (`pid: i64`,
        /// `device: usize`), and tt-smi has repeatedly flip-flopped between bare
        /// numbers and quoted strings for numeric fields (hence the
        /// `de_opt_*_str` helpers above). Keeping it opaque here makes that
        /// impossible; it is decoded best-effort, per row, afterwards.
        #[serde(default)]
        processes: Option<serde_json::Value>,
    }

    match serde_json::from_str::<TTSMISnapshot>(json_str) {
        Ok(snapshot) => {
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
                            pcie_speed: board_info.and_then(|b| b.pcie_speed),
                            pcie_width: board_info.and_then(|b| b.pcie_width),
                        }
                    })
                    .collect();
                return Ok((devices, decode_processes(snapshot.processes)));
            }
        }
        Err(e) => {
            // A document that *has* a `device_info` key must never slide quietly
            // into the permissive single-device fallthrough at the bottom of this
            // function: every `TTSMIDeviceJSON` field is `Option`, so `{...}`
            // parses as "one device" no matter what it actually contains, and an
            // N-device box would silently render as 1 phantom device with a
            // fabricated bus id and no telemetry. Instead, salvage the modern
            // shape element-by-element (see `salvage_modern_snapshot`) so one
            // malformed card costs only that card, and say so at `warn` — the
            // operator needs to know why a device went missing.
            if let Some((devices, processes)) = salvage_modern_snapshot(json_str) {
                log::warn!(
                    "tt-smi snapshot did not fit the modern snapshot shape ({e}); \
                     salvaged {} of its device_info[] entries element-wise",
                    devices.len()
                );
                return Ok((devices, processes));
            }
        }
    }

    // Try to parse as array of devices (legacy format)
    if let Ok(devices) = serde_json::from_str::<Vec<TTSMIDeviceJSON>>(json_str) {
        return Ok((devices, Vec::new()));
    }

    // Try to parse as object with "devices" key (legacy format)
    #[derive(Deserialize)]
    struct Wrapper {
        devices: Option<Vec<TTSMIDeviceJSON>>,
    }

    if let Ok(wrapper) = serde_json::from_str::<Wrapper>(json_str) {
        if let Some(devices) = wrapper.devices {
            return Ok((devices, Vec::new()));
        }
    }

    // Try to parse as single device (last resort, as it's most permissive)
    if let Ok(device) = serde_json::from_str::<TTSMIDeviceJSON>(json_str) {
        return Ok((vec![device], Vec::new()));
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
    // PCIe link info (board_info, tt-smi 5.x): speed is the PCIe generation
    // number; render as "Gen4" to match tt-smi's own display.
    device.pcie_speed = json_dev.pcie_speed.map(|g| format!("Gen{}", g));
    device.pcie_width = json_dev.pcie_width.and_then(|w| u8::try_from(w).ok());
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
        board_power: telem_json.board_power,
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
    /// Top-level per-device process attribution (tt-smi ≥ 6.0.0 `processes[]`).
    /// Empty for snapshot shapes that predate the field.
    pub processes: Vec<crate::models::DeviceProcess>,
}

/// Build a `ParsedSnapshot` from an already-parsed intermediate device list.
///
/// `processes[]` is a top-level sibling of `device_info` in the snapshot, not
/// something derived from the per-device list, so this always yields an empty
/// process list — `parse_tt_smi_snapshot` fills it in from what
/// `parse_json_devices` returned alongside the device list.
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
        processes: Vec::new(),
    }
}

/// Pure parsing entry point: verbatim `tt-smi -s` stdout → the `Device` /
/// `Telemetry` / `SmbusTelemetry` triad (plus top-level `processes[]`) every
/// backend serves.
///
/// This is the single shared parser. `JSONBackend` feeds it the stdout of the
/// `tt-smi` subprocess; `WsBackend` feeds it each WebSocket frame (which is,
/// by contract, that same verbatim stdout). Returns a `ParseError` if none of
/// the supported JSON shapes match.
pub(crate) fn parse_tt_smi_snapshot(json_str: &str) -> BackendResult<ParsedSnapshot> {
    let (json_devices, processes) = parse_json_devices(json_str)?;
    Ok(ParsedSnapshot {
        processes,
        ..parsed_from_json_devices(json_devices)
    })
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

    fn device_processes(&self) -> &[crate::models::DeviceProcess] {
        &self.processes
    }
}

/// Choose between tt-smi's two fan fields, preferring the first that carries a
/// real (non-sentinel) RPM.
///
/// Returns the winning field's **original string** so downstream parsing and
/// EMA smoothing behave exactly as they do for any other SMBUS string field.
/// Returns `None` when neither field holds a real reading (fanless card), so
/// `SmbusTelemetry::fan_rpm()` reports "no fan" rather than a sentinel.
fn pick_fan_field(fan_speed: Option<&str>, fan_rpm: Option<&str>) -> Option<String> {
    [fan_speed, fan_rpm]
        .into_iter()
        .flatten()
        .find(|s| crate::models::telemetry::real_fan_rpm(s).is_some())
        .map(|s| s.to_string())
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
        // FAN_SPEED is the legacy percentage-or-RPM field; tt-smi ≥ 5.3.0
        // reports RPM in FAN_RPM. Both keys are normally *present*, so an
        // `Option::or` chain never falls through — and on Blackhole FAN_SPEED
        // is the `0x0` sentinel while FAN_RPM carries the real reading, which
        // is exactly the case that left the Fan row blank. Pick whichever field
        // yields a non-sentinel RPM, FAN_SPEED first for back-compat; when
        // neither does, stay None rather than storing a sentinel.
        fan_speed: pick_fan_field(
            smbus_json.fan_speed.as_deref(),
            smbus_json.fan_rpm.as_deref(),
        ),
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

/// Per-device metadata carried alongside SMBUS telemetry in a [`SnapshotParts`]
/// — everything `HybridBackend` needs to enrich its sysfs-sourced `Device`s
/// and telemetry with fields sysfs/hwmon can't see.
///
/// `firmwares`/`limits`/`pcie_speed`/`pcie_width` are static-ish device
/// metadata (copied onto `Device` once per snapshot); `board_power` is a
/// live *measurement* (tt-smi's whole-card telemetry reading) and gets
/// overlaid onto `Telemetry` every tick instead — see the two different
/// call sites in `HybridBackend::update()`.
///
/// Replaces a bare `(Option<FirmwaresInfo>, Option<DeviceLimits>)` tuple —
/// that was already an awkward shape before PCIe link geometry and board
/// power needed to ride along too; a 5-tuple would have been worse.
#[derive(Debug, Clone, Default)]
pub(crate) struct DeviceMeta {
    pub firmwares: Option<crate::models::telemetry::FirmwaresInfo>,
    pub limits: Option<crate::models::telemetry::DeviceLimits>,
    /// PCIe generation, pre-formatted as "Gen4" — matches `device_from_json`
    /// so the hybrid and pure-JSON backends render identical link text.
    pub pcie_speed: Option<String>,
    pub pcie_width: Option<u8>,
    /// Whole-card input power (tt-smi ≥ 6.0.0 `telemetry.board_power`).
    pub board_power: Option<f32>,
}

/// Parse result from a single tt-smi JSON snapshot, extracting both SMBUS
/// telemetry and device metadata (firmwares, limits, PCIe link, board power)
/// in one pass.
///
/// Used by `HybridBackend`, whose background reader only needs the SMBUS map
/// plus this metadata (it gets its live device list from sysfs).
/// Distinct from [`ParsedSnapshot`], which carries the full device triad.
pub(crate) struct SnapshotParts {
    pub smbus: HashMap<usize, SmbusTelemetry>,
    pub meta: HashMap<usize, DeviceMeta>,
    /// tt-smi **array position** → that entry's normalized PCI bus id, for the
    /// entries that carry one.
    ///
    /// `smbus`/`meta` above are keyed by array position, which is only a valid
    /// join key against another list if both lists enumerate exactly the same
    /// cards in the same order. `HybridBackend` gets its device list from sysfs,
    /// which can legitimately see cards tt-smi doesn't (a card busy/failed to
    /// enumerate, `--devices` filtering upstream, hotplug), so it re-keys these
    /// maps onto its own device indices by bus id — the one identifier both
    /// sides carry. See `hybrid::rekey_snapshot_by_bus_id`.
    pub bus_ids: HashMap<usize, String>,
}

/// Canonical form of a PCI bus id for use as a join key: trimmed and lowercased
/// (tt-smi has emitted uppercase hex, e.g. `0000:0A:00.0`, where sysfs uses
/// lowercase). Every producer and consumer of a bus-id join key must go through
/// this so the two sides can't disagree on case or padding.
pub(crate) fn normalize_bus_id(bus_id: &str) -> String {
    bus_id.trim().to_lowercase()
}

/// Parse a tt-smi JSON snapshot once and extract both SMBUS telemetry and
/// device metadata.  Replaces the previous two-function pattern that parsed
/// the same JSON document twice per background poll cycle.
pub(crate) fn parse_snapshot(json_str: &str) -> SnapshotParts {
    let (devices, _processes) = match parse_json_devices(json_str) {
        Ok(d) => d,
        Err(e) => {
            log::debug!("parse_snapshot: parse error: {}", e);
            return SnapshotParts {
                smbus: HashMap::new(),
                meta: HashMap::new(),
                bus_ids: HashMap::new(),
            };
        }
    };
    let mut smbus_map = HashMap::new();
    let mut meta_map = HashMap::new();
    let mut bus_ids = HashMap::new();
    for dev in devices {
        let idx = dev.index.unwrap_or(0);
        // Recorded before the per-block extraction below so a device that
        // carries only a bus id (no smbus/meta) still contributes its join key.
        if let Some(bus) = dev.bus_id.as_deref() {
            let norm = normalize_bus_id(bus);
            if !norm.is_empty() {
                bus_ids.insert(idx, norm);
            }
        }
        if let Some(smbus_json) = dev.smbus {
            smbus_map.insert(idx, smbus_from_json_fields(smbus_json));
        }
        let firmwares = dev.firmwares.as_ref().and_then(|v| {
            serde_json::from_value::<crate::models::telemetry::FirmwaresInfo>(v.clone()).ok()
        });
        let limits = dev.limits.as_ref().and_then(|v| {
            serde_json::from_value::<crate::models::telemetry::DeviceLimits>(v.clone()).ok()
        });
        // PCIe link info (board_info, tt-smi 5.x): speed is the PCIe
        // generation number; render as "Gen4" to match tt-smi's own display
        // and `device_from_json` (the pure-JSON backend's equivalent path),
        // so hybrid and json render identically.
        let pcie_speed = dev.pcie_speed.map(|g| format!("Gen{}", g));
        let pcie_width = dev.pcie_width.and_then(|w| u8::try_from(w).ok());
        // Whole-card power lives in the telemetry block, not board_info —
        // `.as_ref()` avoids fighting the `dev.smbus` partial move above.
        let board_power = dev.telemetry.as_ref().and_then(|t| t.board_power);
        if firmwares.is_some()
            || limits.is_some()
            || pcie_speed.is_some()
            || pcie_width.is_some()
            || board_power.is_some()
        {
            meta_map.insert(
                idx,
                DeviceMeta {
                    firmwares,
                    limits,
                    pcie_speed,
                    pcie_width,
                    board_power,
                },
            );
        }
    }
    SnapshotParts {
        smbus: smbus_map,
        meta: meta_map,
        bus_ids,
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
        let meta = snap.meta.get(&0).expect("meta for device 0 missing");
        let fw = meta.firmwares.as_ref().expect("firmwares should be Some");
        assert_eq!(
            fw.fw_bundle_version.as_deref(),
            Some("fw_pack-19.9.0"),
            "parse_snapshot fw_bundle_version mismatch"
        );
        let lim = meta.limits.as_ref().expect("limits should be Some");
        assert_eq!(
            lim.tdp_limit,
            Some(300.0_f32),
            "parse_snapshot tdp_limit mismatch"
        );
    }

    /// Regression for the finding that PCIe link geometry and board power
    /// never reached the Hybrid backend: `parse_snapshot()` — the function
    /// `HybridBackend`'s reader thread actually calls — must extract
    /// `pcie_speed`/`pcie_width` (from `board_info`) and `board_power`
    /// (from `telemetry`) into `DeviceMeta`, formatted identically to the
    /// pure-JSON path (`device_from_json`'s "Gen4" rendering).
    #[test]
    fn parse_snapshot_meta_carries_pcie_link_and_board_power() {
        let json = r#"{
            "device_info": [{
                "board_info": {
                    "bus_id": "0000:01:00.0", "board_type": "p300c",
                    "pcie_speed": 4, "pcie_width": "4"
                },
                "telemetry": {"power": " 16.0", "board_power": " 42.0"}
            }]
        }"#;
        let snap = parse_snapshot(json);
        let meta = snap.meta.get(&0).expect("meta for device 0 missing");
        assert_eq!(
            meta.pcie_speed.as_deref(),
            Some("Gen4"),
            "pcie_speed must be pre-formatted as tt-smi's own \"GenN\" display"
        );
        assert_eq!(meta.pcie_width, Some(4));
        assert_eq!(meta.board_power, Some(42.0));
    }

    /// A `limits` block exactly as tt-smi 5.3.0 emits it on a live p300c:
    /// numeric values as **quoted strings**, except `bus_peak_limit` /
    /// `fan_rpm_limit` which are bare numbers.
    ///
    /// Regression for the finding that `--backend json` never got real limits:
    /// `Option<f32>` fields rejected the string form, serde failed the whole
    /// block, and `Device::limits` stayed `None` — so the Insights rows fell
    /// back to the UI's generic 300 W / 105 °C. The hybrid path masked this
    /// because hwmon `*_max` supplies its limits instead.
    const TTSMI_53_STRING_LIMITS_JSON: &str = r#"{
        "device_info": [{
            "board_info": {
                "board_type": "p300c",
                "bus_id": "0000:01:00.0",
                "coords": "(0,0)"
            },
            "telemetry": {"power": " 16.0", "asic_temperature": "38.4"},
            "firmwares": {
                "fw_bundle_version": "19.11.0.0",
                "tt_flash_version": "N/A",
                "cm_fw": "0.33.0.0",
                "cm_fw_date": "2020-00-33",
                "eth_fw": "1.11.0",
                "dm_bl_fw": "0.0.0.0",
                "dm_app_fw": "0.27.0.0",
                "gddr_fw": "2.16"
            },
            "limits": {
                "vdd_min": "0.70",
                "vdd_max": "0.90",
                "tdp_limit": "125",
                "tdc_limit": "500",
                "asic_fmax": "1350",
                "therm_trip_l1_limit": "90",
                "thm_limit": "110",
                "bus_peak_limit": 0,
                "fan_rpm_limit": 0
            }
        }]
    }"#;

    /// End-to-end through the pure-JSON parser: the string-valued limits must
    /// land on `Device::limits`, with `thm_limit` carrying the 90 °C throttle
    /// point (not tt-smi's 110 °C shutdown value) so the number printed next to
    /// Temp means the same thing here as it does on sysfs/hybrid/luwen.
    #[test]
    fn json_path_parses_tt_smi_53_string_limits() {
        let parsed =
            parse_tt_smi_snapshot(TTSMI_53_STRING_LIMITS_JSON).expect("live snapshot must parse");
        let dev = &parsed.devices[0];
        let lim = dev
            .limits
            .as_ref()
            .expect("string-valued limits block must reach Device::limits");
        assert_eq!(lim.tdp_limit, Some(125.0));
        assert_eq!(lim.tdc_limit, Some(500.0));
        assert_eq!(lim.asic_fmax, Some(1350));
        assert_eq!(lim.thm_limit, Some(90.0));
        // The firmwares block (all-string on real hardware) must survive too.
        assert_eq!(
            dev.firmwares
                .as_ref()
                .and_then(|f| f.fw_bundle_version.as_deref()),
            Some("19.11.0.0")
        );
    }

    /// The hybrid backend's reader takes the `parse_snapshot` path instead, so
    /// pin the same expectation there — a fix in only one of the two parse
    /// sites would leave the other silently dropping limits.
    #[test]
    fn parse_snapshot_meta_parses_tt_smi_53_string_limits() {
        let snap = parse_snapshot(TTSMI_53_STRING_LIMITS_JSON);
        let meta = snap.meta.get(&0).expect("meta for device 0 missing");
        let lim = meta
            .limits
            .as_ref()
            .expect("string-valued limits block must reach DeviceMeta::limits");
        assert_eq!(lim.tdp_limit, Some(125.0));
        assert_eq!(lim.tdc_limit, Some(500.0));
        assert_eq!(lim.asic_fmax, Some(1350));
        assert_eq!(lim.thm_limit, Some(90.0));
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
    fn board_info_pcie_and_dram_fields_parse() {
        // pcie_speed is a bare NUMBER and pcie_width a STRING in real
        // tt-smi 5.3.0 output — both shapes must parse.
        let json = r#"{
            "device_info": [{
                "board_info": {
                    "bus_id": "0000:01:00.0", "board_type": "p300c", "coords": "N/A",
                    "dram_status": true, "dram_speed": "16G",
                    "pcie_speed": 4, "pcie_width": "4"
                },
                "telemetry": {"power": " 16.0"}
            }]
        }"#;
        let parsed = parse_tt_smi_snapshot(json).unwrap();
        let dev = &parsed.devices[0];
        assert_eq!(dev.pcie_speed.as_deref(), Some("Gen4"));
        assert_eq!(dev.pcie_width, Some(4));
    }

    #[test]
    fn fan_rpm_used_when_fan_speed_absent() {
        // tt-smi 5.3.0 changed fan semantics to RPM; a source that only
        // populates FAN_RPM must still drive the fan row.
        let json = r#"{
            "device_info": [{
                "board_info": {"bus_id": "0000:01:00.0", "board_type": "p150a"},
                "smbus_telem": {"FAN_RPM": "0xbb8"}
            }]
        }"#;
        let parsed = parse_tt_smi_snapshot(json).unwrap();
        let smbus = parsed.smbus.get(&0).unwrap();
        assert_eq!(smbus.fan_rpm(), Some(3000)); // 0xbb8
    }

    /// Regression: the committed real-world fixture has `FAN_SPEED = "0x0"`
    /// alongside `FAN_RPM = "0x75a"` (1882 RPM) — the shape a live Blackhole
    /// card actually emits. An `Option::or` chain kept the present-but-sentinel
    /// FAN_SPEED and left the Fan row blank; the field must be chosen by whether
    /// it holds a *real* reading.
    #[test]
    fn fan_rpm_wins_when_fan_speed_is_sentinel_zero() {
        let result = parse_smbus_from_json(REAL_TTSMI_JSON);
        let smbus = result.get(&0).expect("device 0 missing");
        assert_eq!(
            smbus.fan_rpm(),
            Some(1882),
            "FAN_RPM=0x75a must win over the FAN_SPEED=0x0 sentinel"
        );
    }

    /// FAN_SPEED keeps priority when it holds a real reading (older tt-smi,
    /// where FAN_SPEED is the only fan field).
    #[test]
    fn fan_speed_wins_when_real_and_fan_rpm_absent() {
        let json = r#"{
            "device_info": [{
                "board_info": {"bus_id": "0000:01:00.0", "board_type": "n300"},
                "smbus_telem": {"FAN_SPEED": "0x9c4"}
            }]
        }"#;
        let parsed = parse_tt_smi_snapshot(json).unwrap();
        let smbus = parsed.smbus.get(&0).unwrap();
        assert_eq!(smbus.fan_rpm(), Some(2500)); // 0x9c4
    }

    /// Both fields sentinel (fanless p150-class card) → no fan reading at all,
    /// and no sentinel string stored to be mis-rendered later.
    #[test]
    fn both_fan_fields_sentinel_yields_none() {
        let json = r#"{
            "device_info": [{
                "board_info": {"bus_id": "0000:01:00.0", "board_type": "p150a"},
                "smbus_telem": {"FAN_SPEED": "0x0", "FAN_RPM": "0xffffffff"}
            }]
        }"#;
        let parsed = parse_tt_smi_snapshot(json).unwrap();
        let smbus = parsed.smbus.get(&0).unwrap();
        assert_eq!(smbus.fan_speed, None, "sentinels must not be stored");
        assert_eq!(smbus.fan_rpm(), None);
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

    #[test]
    fn tt_smi_6_processes_and_board_power_parse() {
        let json = r#"{
            "time": "2026-08-10T15:00:00",
            "processes": [
                {"pid": 4242, "user": "ttuser", "device": 0, "cmdline": "python -m vllm serve"},
                {"pid": 4243, "device": 1}
            ],
            "device_info": [{
                "board_info": {"bus_id": "0000:01:00.0", "board_type": "p300c"},
                "telemetry": {"power": " 16.0", "board_power": " 42.0"}
            }]
        }"#;
        let parsed = parse_tt_smi_snapshot(json).unwrap();
        assert_eq!(parsed.processes.len(), 2);
        assert_eq!(parsed.processes[0].pid, Some(4242));
        assert_eq!(
            parsed.processes[0].cmdline.as_deref(),
            Some("python -m vllm serve")
        );
        assert_eq!(parsed.processes[1].user, None); // exclude_none omits fields
        let t = parsed.telemetry.get(&0).unwrap();
        assert_eq!(t.power, Some(16.0));
        assert_eq!(t.board_power, Some(42.0));
    }

    #[test]
    fn snapshots_without_processes_parse_as_empty() {
        let json = r#"{"device_info": [{"board_info": {"bus_id": "x", "board_type": "p150a"}}]}"#;
        let parsed = parse_tt_smi_snapshot(json).unwrap();
        assert!(parsed.processes.is_empty());
    }

    /// Two-device snapshot whose `processes[]` uses tt-smi's *other* habit for
    /// numeric values — a quoted string `"pid": "4242"`.
    ///
    /// Regression: `processes` used to be typed `Option<Vec<DeviceProcess>>`
    /// with `pid: Option<i64>`, so this document failed the modern-snapshot
    /// parse outright, fell through to the permissive single-device branch (every
    /// `TTSMIDeviceJSON` field is `Option`, so *any* object parses as one
    /// device), and a 2-device box silently reported **1 phantom device** with a
    /// fabricated bus id and no telemetry at all. The process list must never
    /// cost the device list.
    #[test]
    fn string_typed_pid_in_processes_does_not_collapse_device_list() {
        let json = r#"{
            "device_info": [
                {
                    "board_info": {"bus_id": "0000:01:00.0", "board_type": "p150a"},
                    "telemetry": {"power": " 16.0", "asic_temperature": " 41.5"}
                },
                {
                    "board_info": {"bus_id": "0000:02:00.0", "board_type": "p150a"},
                    "telemetry": {"power": " 22.0", "asic_temperature": " 44.0"}
                }
            ],
            "processes": [
                {"pid": "4242", "user": "ttuser", "device": "1", "cmdline": "python -m vllm serve"}
            ]
        }"#;
        let parsed = parse_tt_smi_snapshot(json).expect("snapshot must parse");

        assert_eq!(parsed.devices.len(), 2, "both devices must survive");
        assert_eq!(parsed.devices[0].bus_id, "0000:01:00.0");
        assert_eq!(parsed.devices[1].bus_id, "0000:02:00.0");
        assert_eq!(parsed.telemetry.get(&0).and_then(|t| t.power), Some(16.0));
        assert_eq!(parsed.telemetry.get(&1).and_then(|t| t.power), Some(22.0));

        // The tolerant DeviceProcess deserializers also let the row itself
        // through, so nothing is lost — but even an empty list would be fine
        // here; what matters is that the devices are intact.
        assert_eq!(parsed.processes.len(), 1);
        assert_eq!(parsed.processes[0].pid, Some(4242));
        assert_eq!(parsed.processes[0].device, Some(1));
    }

    /// Same guarantee for a `processes` block that isn't even an array (an object
    /// keyed by pid, say): devices are untouched and the process list is empty.
    #[test]
    fn object_shaped_processes_block_does_not_harm_devices() {
        let json = r#"{
            "device_info": [
                {"board_info": {"bus_id": "0000:01:00.0"}, "telemetry": {"power": " 16.0"}},
                {"board_info": {"bus_id": "0000:02:00.0"}, "telemetry": {"power": " 22.0"}}
            ],
            "processes": {"4242": {"device": 0}}
        }"#;
        let parsed = parse_tt_smi_snapshot(json).expect("snapshot must parse");
        assert_eq!(parsed.devices.len(), 2);
        assert_eq!(parsed.telemetry.get(&1).and_then(|t| t.power), Some(22.0));
        assert!(
            parsed.processes.is_empty(),
            "a non-array processes block yields no rows"
        );
    }

    /// A `device_info[]` entry tt-smi wrote in an unexpected shape costs only
    /// that entry: the other card keeps its telemetry, and — crucially — the
    /// document does NOT fall through to the single-device branch. Array
    /// positions are preserved, so the survivor keeps index 1 rather than
    /// sliding into the missing card's slot (that slide is exactly the
    /// mis-attribution this release set out to fix).
    #[test]
    fn malformed_device_entry_is_skipped_not_collapsed() {
        let json = r#"{
            "device_info": [
                {"board_info": {"bus_id": "0000:01:00.0"}, "telemetry": true},
                {"board_info": {"bus_id": "0000:02:00.0"}, "telemetry": {"power": " 22.0"}}
            ]
        }"#;
        let parsed = parse_tt_smi_snapshot(json).expect("snapshot must salvage");
        assert_eq!(parsed.devices.len(), 1, "only the bad entry is dropped");
        assert_eq!(parsed.devices[0].index, 1, "array position is preserved");
        assert_eq!(parsed.devices[0].bus_id, "0000:02:00.0");
        assert_eq!(parsed.telemetry.get(&1).and_then(|t| t.power), Some(22.0));
        assert!(
            parsed.telemetry.get(&0).is_none(),
            "no telemetry may be attributed to the dropped card's index"
        );
    }
}
