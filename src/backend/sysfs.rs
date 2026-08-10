// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Sysfs sensor backend for reading hardware telemetry via Linux hwmon
//!
//! This backend provides a non-invasive fallback by reading sensor data directly
//! from `/sys/class/hwmon/` without requiring PCI access or special permissions.
//!
//! # When to Use
//!
//! - Hardware is actively running workloads (LLMs, training, etc.)
//! - Luwen backend fails due to BAR0 mapping conflicts
//! - No special permissions available (no sudo, no ttkmd module)
//! - Read-only telemetry monitoring desired
//!
//! # Limitations
//!
//! - Only provides basic metrics (temperature, voltage, power if available)
//! - SMBUS telemetry is *partial*: on kmd ≥ 2.7, `smbus_telemetry()` returns a
//!   `SmbusTelemetry` synthesized from tt-kmd's `tt_*` class attrs (AICLK,
//!   AXICLK, ARCCLK, ARC heartbeat, therm trip count, board serial, M3 app FW
//!   version) plus the hwmon fan reading — not the full SMBUS block tt-smi
//!   exposes (no DDR training status, no per-ARC FW versions, no board/vreg
//!   temps). On older kmd without the `tenstorrent` class dir, it's `None`
//!   exactly as before.
//! - Sensor naming/availability varies by kernel driver
//! - May not detect all devices if driver doesn't expose hwmon

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Valid sysfs file paths for each sensor type on one device.
/// Populated once during `init()` so `update()` never retries missing paths.
#[derive(Default)]
struct SensorPaths {
    temperature: Option<PathBuf>, // tempN_input (millicelsius)
    voltage: Option<PathBuf>,     // inN_input   (millivolts)
    power: Option<PathBuf>,       // powerN_input (microwatts)
    current: Option<PathBuf>,     // currN_input  (milliamperes)
    fan: Option<PathBuf>,         // fanN_input   (RPM; 0xFFFFFFFF sentinel = no fan present)
}

/// Sysfs sensor backend implementation
///
/// Reads telemetry from `/sys/class/hwmon/hwmon*` entries
pub struct SysfsBackend {
    /// Backend configuration (reserved for future use)
    #[allow(dead_code)]
    config: BackendConfig,

    /// Detected devices with their hwmon paths
    devices: Vec<Device>,

    /// Mapping of device index to hwmon directory path
    hwmon_paths: HashMap<usize, PathBuf>,

    /// Cached valid sensor file paths per device — populated in init().
    sensor_paths: HashMap<usize, SensorPaths>,

    /// tt-kmd class-attribute dir per device (tt_aiclk, tt_heartbeat,
    /// pcie_perf_counters/, …). Absent on kmd < 2.7.
    tt_class_dirs: HashMap<usize, PathBuf>,

    /// Cached telemetry data (per device index)
    telemetry_cache: HashMap<usize, Telemetry>,

    /// SMBUS-shaped telemetry synthesized from tt-kmd sysfs attrs (partial —
    /// only the fields the driver exposes). Serves the Insights fan/clock rows
    /// in sysfs-only mode; HybridBackend ignores it (it blends its own from
    /// tt-smi JSON).
    smbus_cache: HashMap<usize, SmbusTelemetry>,
}

impl SysfsBackend {
    /// Create a new Sysfs backend with default configuration
    pub fn new() -> Self {
        Self::with_config(BackendConfig::default())
    }

    /// Create a new Sysfs backend with custom configuration
    pub fn with_config(config: BackendConfig) -> Self {
        Self {
            config,
            devices: Vec::new(),
            hwmon_paths: HashMap::new(),
            sensor_paths: HashMap::new(),
            tt_class_dirs: HashMap::new(),
            telemetry_cache: HashMap::new(),
            smbus_cache: HashMap::new(),
        }
    }

    /// Scan /sys/class/hwmon/ for Tenstorrent devices
    ///
    /// Clears existing state before scanning so that calling init() twice does not
    /// double-count devices. This happens in practice when the tui.rs binary pre-probes
    /// the backend and then run_with_backend() calls init() a second time.
    fn detect_devices(&mut self) -> BackendResult<()> {
        log::info!("SysfsBackend: Scanning /sys/class/hwmon/");

        // Clear any previously discovered state so repeated init() calls are idempotent.
        self.devices.clear();
        self.hwmon_paths.clear();
        self.sensor_paths.clear();
        self.tt_class_dirs.clear();
        self.telemetry_cache.clear();
        self.smbus_cache.clear();

        let hwmon_base = Path::new("/sys/class/hwmon");
        if !hwmon_base.exists() {
            return Err(BackendError::Initialization(
                "Hwmon sysfs not available (Linux-specific)".to_string(),
            ));
        }

        let entries = fs::read_dir(hwmon_base).map_err(|e| {
            BackendError::Initialization(format!("Failed to read hwmon dir: {}", e))
        })?;

        let mut device_idx = 0;
        for entry in entries.flatten() {
            let path = entry.path();

            // Check if this is a Tenstorrent device by reading name
            let name_path = path.join("name");
            if let Ok(name) = fs::read_to_string(&name_path) {
                let name = name.trim();

                // Look for Tenstorrent-related hwmon names
                // Common patterns: "tenstorrent", "tt_*", "grayskull", "wormhole", "blackhole"
                if name.contains("tenstorrent")
                    || name.starts_with("tt_")
                    || name.contains("grayskull")
                    || name.contains("wormhole")
                    || name.contains("blackhole")
                {
                    log::info!(
                        "SysfsBackend: Found Tenstorrent device: {} at {:?}",
                        name,
                        path
                    );

                    // Try to determine architecture from name
                    let architecture = if name.contains("grayskull") {
                        Architecture::Grayskull
                    } else if name.contains("wormhole") {
                        Architecture::Wormhole
                    } else if name.contains("blackhole") {
                        Architecture::Blackhole
                    } else {
                        Architecture::Unknown
                    };

                    // Try to extract PCI address from device path
                    let bus_id = self
                        .extract_pci_address(&path)
                        .unwrap_or_else(|| format!("hwmon{}", device_idx));

                    let mut device = Device {
                        index: device_idx,
                        board_type: name.to_string(),
                        bus_id: bus_id.clone(),
                        coords: String::new(),
                        architecture,
                        firmwares: None,
                        limits: None,
                        pcie_speed: None,
                        pcie_width: None,
                        grid_override: None,
                        channels_override: None,
                    };
                    // Real firmware limits from hwmon *_max files (kmd ≥ 2.9.0).
                    // HybridBackend later overwrites with the richer JSON limits
                    // block when tt-smi is available; this covers sysfs-only mode.
                    device.limits = Self::read_hwmon_limits(&path);

                    self.devices.push(device);
                    self.hwmon_paths.insert(device_idx, path);
                    device_idx += 1;
                }
            }
        }

        if self.devices.is_empty() {
            return Err(BackendError::Initialization(
                "No Tenstorrent devices found in hwmon".to_string(),
            ));
        }

        log::info!("SysfsBackend: Found {} devices", self.devices.len());
        Ok(())
    }

    /// Try to enrich device board_type from `tt-smi -s` output.
    ///
    /// The hwmon driver only exposes the chip architecture name (e.g. "blackhole"),
    /// not the board SKU (e.g. "p300c"). This runs tt-smi once at init and matches
    /// devices by PCI bus_id to fill in the real product name. Fails silently if
    /// tt-smi is unavailable or its output cannot be parsed.
    ///
    /// **Bounded.** tt-smi shells out and, while a model is compiling/loading, can
    /// take several seconds to respond (ARC + CPU contention with the workload) —
    /// and this runs on the startup path, before the first frame. So the whole
    /// (version-gated) probe runs on a helper thread and we wait only briefly; on
    /// timeout we skip enrichment and the device keeps its hwmon arch name, the
    /// same graceful degradation as when tt-smi is absent, rather than stalling
    /// the entire TUI from painting for ~10 s. The orphaned probe (rare) finishes
    /// and exits on its own; this is a one-shot init call, not a per-tick poll.
    ///
    /// Only tt-smi v4+ is used. Earlier versions access hardware invasively via
    /// direct PCI reads, which can disrupt running workloads. v4+ switched to a
    /// safe snapshot mode that reads from the kernel driver.
    fn try_enrich_board_types_from_tt_smi(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(sku_map_from_tt_smi());
        });
        let sku_by_bus = match rx.recv_timeout(std::time::Duration::from_millis(1200)) {
            Ok(Some(m)) if !m.is_empty() => m,
            _ => return, // timed out, tt-smi absent/<v4, or nothing parsed → skip
        };
        for device in &mut self.devices {
            let bus_norm = device.bus_id.trim().to_lowercase();
            if let Some(sku) = sku_by_bus.get(&bus_norm) {
                device.board_type = sku.clone();
            }
        }
        log::info!("SysfsBackend: enriched board types from tt-smi");
    }

    /// Extract PCI address from hwmon device path
    /// Example: /sys/class/hwmon/hwmon3 -> /sys/devices/pci0000:00/0000:00:01.0/...
    fn extract_pci_address(&self, hwmon_path: &Path) -> Option<String> {
        // Read the device link to find real device path
        let device_link = hwmon_path.join("device");
        if let Ok(real_path) = fs::read_link(&device_link) {
            let path_str = real_path.to_string_lossy();

            // Look for PCI address pattern: 0000:00:00.0
            for component in path_str.split('/') {
                if component.len() >= 12
                    && component.chars().nth(4) == Some(':')
                    && component.chars().nth(7) == Some(':')
                {
                    return Some(component.to_string());
                }
            }
        }
        None
    }

    /// Pick the input file for one sensor class: prefer the candidate whose
    /// `_label` file matches `preferred_label` (tt-kmd labels its sensors, e.g.
    /// temp1_label=asic_temp), else the lowest-numbered existing input file.
    ///
    /// This matters because a device can expose *multiple* sensors of the same
    /// class at different indices (e.g. temp1=vreg_temp, temp2=asic_temp) — the
    /// old "lowest index wins" logic would silently pick the wrong one. Falling
    /// back to lowest-index-existing preserves behavior for older/unlabeled
    /// drivers that only expose a single sensor per class.
    fn pick_sensor(
        hwmon_path: &Path,
        prefix: &str, // "temp" | "in" | "power" | "curr" | "fan"
        range: std::ops::RangeInclusive<u32>,
        preferred_label: &str,
    ) -> Option<PathBuf> {
        let mut first_existing: Option<PathBuf> = None;
        for i in range {
            let input = hwmon_path.join(format!("{}{}_input", prefix, i));
            if !input.exists() {
                continue;
            }
            let label_path = hwmon_path.join(format!("{}{}_label", prefix, i));
            if let Ok(label) = fs::read_to_string(&label_path) {
                if label.trim() == preferred_label {
                    return Some(input); // exact label match wins immediately
                }
            }
            if first_existing.is_none() {
                first_existing = Some(input);
            }
        }
        first_existing
    }

    /// Find valid sensor file paths for one hwmon device.
    /// Called once in `init()` so `update()` can read directly without retrying.
    fn discover_sensor_paths(hwmon_path: &Path) -> SensorPaths {
        SensorPaths {
            temperature: Self::pick_sensor(hwmon_path, "temp", 1..=8, "asic_temp"),
            voltage: Self::pick_sensor(hwmon_path, "in", 0..=8, "vcore"),
            power: Self::pick_sensor(hwmon_path, "power", 1..=8, "power"),
            current: Self::pick_sensor(hwmon_path, "curr", 1..=8, "current"),
            fan: Self::pick_sensor(hwmon_path, "fan", 1..=4, "fan_rpm"),
        }
    }

    /// Read a whole-number sysfs attribute file (plain decimal, e.g. "125000000").
    fn read_u64_file(path: &Path) -> Option<u64> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    }

    /// Locate the tt-kmd class-attribute directory for a hwmon device.
    ///
    /// tt-kmd registers a second sysfs surface alongside hwmon:
    /// `/sys/class/tenstorrent/tenstorrent!N/` with `tt_aiclk`, `tt_heartbeat`,
    /// `tt_card_type`, `pcie_perf_counters/`, … . It lives under the SAME PCI
    /// device dir the hwmon `device` symlink points at, as
    /// `<pci-dir>/tenstorrent/<single subdir>`. Resolving through the PCI dir
    /// (rather than globbing /sys/class/tenstorrent) pins the hwmon↔class-dev
    /// correlation to the same physical card. None on kmd < 2.7 (attrs absent).
    fn find_tt_class_dir(hwmon_path: &Path) -> Option<PathBuf> {
        let pci_dir = fs::canonicalize(hwmon_path.join("device")).ok()?;
        let tt_parent = pci_dir.join("tenstorrent");
        let mut entries = fs::read_dir(&tt_parent).ok()?;
        // Exactly one tenstorrent!N subdir exists per PCI function.
        entries.find_map(|e| {
            let p = e.ok()?.path();
            p.is_dir().then_some(p)
        })
    }

    /// Read a trimmed string attribute (e.g. tt_card_type = "p300c\n" → "p300c").
    /// None for missing/empty files.
    fn read_string_attr(dir: &Path, name: &str) -> Option<String> {
        let s = fs::read_to_string(dir.join(name)).ok()?;
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// Read the hwmon `*_max` limit files into a DeviceLimits.
    ///
    /// tt-kmd ≥ 2.9.0 exposes firmware-provided limits on Blackhole too:
    /// power1_max (µW), curr1_max (mA), temp1_max (m°C). These are the real
    /// per-board ceilings (e.g. 125 W / 500 A / 90 °C on p300c) — far better
    /// than the UI's hardcoded 300 W / 105 °C fallbacks. asic_fmax is not
    /// exposed by hwmon; it stays None (the JSON limits block has it).
    /// Returns None when no limit file exists (older driver) so callers can
    /// leave `Device::limits` untouched.
    fn read_hwmon_limits(hwmon_path: &Path) -> Option<crate::models::telemetry::DeviceLimits> {
        let tdp_limit = Self::read_u64_file(&hwmon_path.join("power1_max"))
            .map(|uw| uw as f32 / 1_000_000.0);
        let tdc_limit =
            Self::read_u64_file(&hwmon_path.join("curr1_max")).map(|ma| ma as f32 / 1000.0);
        let thm_limit =
            Self::read_u64_file(&hwmon_path.join("temp1_max")).map(|mc| mc as f32 / 1000.0);
        if tdp_limit.is_none() && tdc_limit.is_none() && thm_limit.is_none() {
            return None;
        }
        Some(crate::models::telemetry::DeviceLimits {
            tdp_limit,
            tdc_limit,
            asic_fmax: None,
            thm_limit,
        })
    }

    /// Read a millicelsius sysfs file and convert to Celsius.
    fn read_temperature_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok().map(|mc| mc as f32 / 1000.0))
    }

    /// Read a millivolt sysfs file and convert to Volts.
    fn read_voltage_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok().map(|mv| mv as f32 / 1000.0))
    }

    /// Read a microwatt sysfs file and convert to Watts.
    fn read_power_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path).ok().and_then(|s| {
            s.trim()
                .parse::<i64>()
                .ok()
                .map(|uw| uw as f32 / 1_000_000.0)
        })
    }

    /// Read a milliampere sysfs file and convert to Amperes.
    fn read_current_path(path: &Path) -> Option<f32> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok().map(|ma| ma as f32 / 1000.0))
    }

    /// Build a partial SmbusTelemetry from the tt-kmd class attrs + hwmon fan.
    ///
    /// Everything is stored as the same string shapes the JSON backend uses so
    /// the existing accessors (fan_rpm(), arc0_health_value(), …) work
    /// unchanged. tt_heartbeat maps to arc0_health: it's the same firmware
    /// heartbeat counter TIMER_HEARTBEAT carries in tt-smi output.
    fn synthesize_smbus(tt_dir: Option<&Path>, fan_path: Option<&Path>) -> SmbusTelemetry {
        let mut s = SmbusTelemetry::new();
        if let Some(dir) = tt_dir {
            s.aiclk = Self::read_string_attr(dir, "tt_aiclk");
            s.axiclk = Self::read_string_attr(dir, "tt_axiclk");
            s.arcclk = Self::read_string_attr(dir, "tt_arcclk");
            s.arc0_health = Self::read_string_attr(dir, "tt_heartbeat");
            s.therm_trip_count = Self::read_string_attr(dir, "tt_therm_trip_count");
            s.board_id = Self::read_string_attr(dir, "tt_serial");
            s.m3_app_fw_version = Self::read_string_attr(dir, "tt_m3app_fw_ver");
        }
        if let Some(fan) = fan_path {
            s.fan_speed = Self::read_u64_file(fan).map(|v| v.to_string());
        }
        s
    }
}

impl Default for SysfsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SysfsBackend {
    /// Mutable access to the device list — used by HybridBackend to enrich
    /// sysfs devices with firmware/limits data obtained from the JSON snapshot.
    pub(crate) fn devices_mut(&mut self) -> &mut [Device] {
        &mut self.devices
    }

    /// Mutable access to a cached telemetry entry — used by HybridBackend to
    /// overlay firmware-reported TDP/TDC values on top of the hwmon reading.
    pub(crate) fn telemetry_cache_mut(&mut self, device_idx: usize) -> Option<&mut Telemetry> {
        self.telemetry_cache.get_mut(&device_idx)
    }

    /// Insert a telemetry entry directly into the cache — test helper only.
    #[cfg(test)]
    pub(crate) fn insert_telemetry_for_test(&mut self, device_idx: usize, telem: Telemetry) {
        self.telemetry_cache.insert(device_idx, telem);
    }
}

impl TelemetryBackend for SysfsBackend {
    fn init(&mut self) -> BackendResult<()> {
        self.detect_devices()?;
        // Cache valid sensor paths so update() never retries missing indices.
        for (device_idx, hwmon_path) in &self.hwmon_paths {
            let paths = Self::discover_sensor_paths(hwmon_path);
            self.sensor_paths.insert(*device_idx, paths);
        }

        // Discover the tt-kmd class-attribute dirs and use their static attrs
        // for device enrichment (no subprocess needed).
        // detect_devices() above already errors out on an empty device list,
        // so we always start from "all devices have it" and flip to false the
        // first time one doesn't.
        let mut all_have_card_type = true;
        for device in &mut self.devices {
            let Some(hwmon_path) = self.hwmon_paths.get(&device.index) else {
                all_have_card_type = false;
                continue;
            };
            let Some(tt_dir) = Self::find_tt_class_dir(hwmon_path) else {
                all_have_card_type = false;
                continue;
            };
            // Board SKU straight from the driver (tt_card_type = "p300c") —
            // supersedes the generic hwmon arch name AND the tt-smi probe.
            if let Some(card) = Self::read_string_attr(&tt_dir, "tt_card_type") {
                device.board_type = card;
            } else {
                all_have_card_type = false;
            }
            // FW bundle version → same Device.firmwares slot the JSON limits
            // block fills, so the Insights FW row works in sysfs-only mode.
            if device.firmwares.is_none() {
                if let Some(fw) = Self::read_string_attr(&tt_dir, "tt_fw_bundle_ver") {
                    device.firmwares = Some(crate::models::telemetry::FirmwaresInfo {
                        fw_bundle_version: Some(fw),
                        ..Default::default()
                    });
                }
            }
            self.tt_class_dirs.insert(device.index, tt_dir);
        }
        if !all_have_card_type {
            // Older kmd without tt_card_type — fall back to the bounded
            // tt-smi -s probe (unchanged behavior).
            self.try_enrich_board_types_from_tt_smi();
        }

        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        for device_idx in 0..self.devices.len() {
            let paths = match self.sensor_paths.get(&device_idx) {
                Some(p) => p,
                None => continue,
            };

            let temperature = paths
                .temperature
                .as_deref()
                .and_then(Self::read_temperature_path);
            let voltage = paths.voltage.as_deref().and_then(Self::read_voltage_path);
            let power = paths.power.as_deref().and_then(Self::read_power_path);
            let current = paths.current.as_deref().and_then(Self::read_current_path);

            // If power and voltage available but not current, calculate it.
            let calculated_current = match (power, current, voltage) {
                (Some(p), None, Some(v)) if v > 0.0 => Some(p / v),
                _ => current,
            };

            // Dynamic tt-kmd class attrs: AICLK + ARC heartbeat feed the core
            // Telemetry (they were hardcoded None before kmd exposed them);
            // the rest lands in the synthesized SMBUS block.
            let tt_dir = self.tt_class_dirs.get(&device_idx);
            let aiclk = tt_dir
                .and_then(|d| Self::read_u64_file(&d.join("tt_aiclk")))
                .map(|v| v as u32);
            let heartbeat = tt_dir
                .and_then(|d| Self::read_u64_file(&d.join("tt_heartbeat")))
                .map(|v| v as u32);

            let telemetry = Telemetry {
                timestamp: chrono::Utc::now(),
                voltage,
                current: calculated_current,
                power,
                asic_temperature: temperature,
                aiclk,
                heartbeat,
            };

            let smbus = Self::synthesize_smbus(tt_dir.map(|p| p.as_path()), paths.fan.as_deref());
            self.smbus_cache.insert(device_idx, smbus);

            self.telemetry_cache.insert(device_idx, telemetry);
        }

        Ok(())
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
        // Partial SMBUS synthesized from tt-kmd sysfs attrs (kmd ≥ 2.7).
        self.smbus_cache.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        "Sysfs (hwmon sensors)".to_string()
    }
}

/// Version-gated `tt-smi -s` → `{ normalized bus_id → board_type }`. Does all the
/// subprocess I/O + JSON parse and takes no `self`, so it can run on a helper
/// thread under a timeout (see [`SysfsBackend::try_enrich_board_types_from_tt_smi`]).
/// `None` when tt-smi is absent, older than v4 (invasive PCI reads), or its
/// output can't be parsed.
fn sku_map_from_tt_smi() -> Option<HashMap<String, String>> {
    // Gate on tt-smi major version >= 4 before calling -s. `--version` prints
    // just the version string, e.g. "5.2.0".
    let ver_out = Command::new("tt-smi").arg("--version").output().ok()?;
    if !ver_out.status.success() {
        return None;
    }
    let ver_str = String::from_utf8_lossy(&ver_out.stdout);
    let major: u32 = ver_str
        .trim()
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if major < 4 {
        log::warn!(
            "SysfsBackend: tt-smi version {} is < 4 — skipping board-type enrichment \
             to avoid invasive PCI access",
            ver_str.trim()
        );
        return None;
    }
    let output = Command::new("tt-smi").arg("-s").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let json = std::str::from_utf8(&output.stdout).ok()?;
    Some(parse_sku_map(json))
}

/// Parse a `tt-smi -s` snapshot into `{ normalized bus_id → board_type }`. Pure
/// (no I/O), so it's unit-testable against canned JSON. Unknown/empty fields are
/// skipped; a malformed document yields an empty map.
fn parse_sku_map(json: &str) -> HashMap<String, String> {
    #[derive(serde::Deserialize)]
    struct BoardInfo {
        bus_id: Option<String>,
        board_type: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct RawDev {
        board_info: Option<BoardInfo>,
    }
    #[derive(serde::Deserialize)]
    struct Snapshot {
        device_info: Option<Vec<RawDev>>,
    }

    let mut sku_by_bus: HashMap<String, String> = HashMap::new();
    let Ok(snapshot) = serde_json::from_str::<Snapshot>(json) else {
        return sku_by_bus;
    };
    for raw in snapshot.device_info.unwrap_or_default() {
        if let Some(bi) = raw.board_info {
            if let (Some(bus), Some(bt)) = (bi.bus_id, bi.board_type) {
                let bus_norm = bus.trim().to_lowercase();
                if !bus_norm.is_empty() && !bt.is_empty() {
                    sku_by_bus.insert(bus_norm, bt);
                }
            }
        }
    }
    sku_by_bus
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sysfs_backend_creation() {
        let backend = SysfsBackend::new();
        assert_eq!(backend.backend_info(), "Sysfs (hwmon sensors)");
        assert_eq!(backend.device_count(), 0);
    }

    #[test]
    fn test_sysfs_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = SysfsBackend::with_config(config);
        assert_eq!(backend.config.update_interval_ms, 50);
    }

    // Note: Actual device detection tests require real hardware or mocked filesystem

    #[test]
    fn parse_sku_map_extracts_board_types_by_bus() {
        let json = r#"{
            "device_info": [
                {"board_info": {"bus_id": "0000:07:00.0", "board_type": "p300c"}},
                {"board_info": {"bus_id": "0000:0A:00.0", "board_type": "p150a"}},
                {"board_info": {"bus_id": "  ", "board_type": "ignored-empty-bus"}},
                {"board_info": {"bus_id": "0000:0B:00.0", "board_type": ""}},
                {"board_info": null}
            ]
        }"#;
        let m = parse_sku_map(json);
        // bus_id is normalized (trimmed + lowercased) so uppercase hex matches.
        assert_eq!(m.get("0000:07:00.0").map(String::as_str), Some("p300c"));
        assert_eq!(m.get("0000:0a:00.0").map(String::as_str), Some("p150a"));
        // Empty bus_id or board_type entries are skipped; null board_info too.
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn parse_sku_map_tolerates_garbage_and_empty() {
        assert!(parse_sku_map("not json").is_empty());
        assert!(parse_sku_map("{}").is_empty());
        assert!(parse_sku_map(r#"{"device_info": []}"#).is_empty());
    }

    use std::fs as stdfs;

    /// Build a fake hwmon dir with the exact attribute set tt-kmd 2.9.0
    /// exposes on Blackhole (values from a live p300c).
    fn fake_hwmon(dir: &Path) {
        for (f, v) in [
            ("name", "blackhole"),
            ("temp1_input", "38036"), ("temp1_label", "asic_temp"), ("temp1_max", "90000"),
            ("in0_input", "718"), ("in0_label", "vcore"), ("in0_max", "900"),
            ("power1_input", "16000000"), ("power1_label", "power"), ("power1_max", "125000000"),
            ("curr1_input", "23000"), ("curr1_label", "current"), ("curr1_max", "500000"),
            ("fan1_input", "4294967295"), ("fan1_label", "fan_rpm"),
        ] {
            stdfs::write(dir.join(f), v).unwrap();
        }
    }

    #[test]
    fn discover_sensor_paths_finds_fan() {
        let td = tempfile::tempdir().unwrap();
        fake_hwmon(td.path());
        let paths = SysfsBackend::discover_sensor_paths(td.path());
        assert!(paths.fan.is_some(), "fan1_input should be discovered");
        assert!(paths.temperature.is_some());
    }

    #[test]
    fn discover_sensor_paths_prefers_asic_temp_label() {
        let td = tempfile::tempdir().unwrap();
        fake_hwmon(td.path());
        // Add a decoy lower-numbered sensor with a non-ASIC label. Label-aware
        // discovery must skip it in favor of the one labelled asic_temp.
        stdfs::write(td.path().join("temp1_label"), "vreg_temp").unwrap();
        stdfs::write(td.path().join("temp2_input"), "40000").unwrap();
        stdfs::write(td.path().join("temp2_label"), "asic_temp").unwrap();
        let paths = SysfsBackend::discover_sensor_paths(td.path());
        assert_eq!(
            paths.temperature.as_deref(),
            Some(td.path().join("temp2_input").as_path())
        );
    }

    #[test]
    fn read_hwmon_limits_converts_units() {
        let td = tempfile::tempdir().unwrap();
        fake_hwmon(td.path());
        let lim = SysfsBackend::read_hwmon_limits(td.path()).unwrap();
        assert_eq!(lim.tdp_limit, Some(125.0)); // 125000000 µW
        assert_eq!(lim.tdc_limit, Some(500.0)); // 500000 mA
        assert_eq!(lim.thm_limit, Some(90.0));  // 90000 m°C
        assert_eq!(lim.asic_fmax, None);        // not exposed by hwmon
    }

    #[test]
    fn read_hwmon_limits_absent_files_yield_none() {
        let td = tempfile::tempdir().unwrap();
        stdfs::write(td.path().join("name"), "blackhole").unwrap();
        assert!(SysfsBackend::read_hwmon_limits(td.path()).is_none());
    }

    /// Build the tenstorrent class-attribute dir the way tt-kmd lays it out:
    /// <pci-dir>/tenstorrent/tenstorrent!0/tt_* — reached from a hwmon dir via
    /// its `device` symlink.
    fn fake_tt_class(hwmon_dir: &Path) -> PathBuf {
        let pci_dir = hwmon_dir.parent().unwrap().join("pcidev");
        let tt_dir = pci_dir.join("tenstorrent").join("tenstorrent!0");
        stdfs::create_dir_all(&tt_dir).unwrap();
        std::os::unix::fs::symlink(&pci_dir, hwmon_dir.join("device")).unwrap();
        for (f, v) in [
            ("tt_card_type", "p300c"),
            ("tt_serial", "0000046131924062"),
            ("tt_fw_bundle_ver", "19.11.0.0"),
            ("tt_aiclk", "800"),
            ("tt_axiclk", "960"),
            ("tt_arcclk", "800"),
            ("tt_heartbeat", "43874"),
            ("tt_therm_trip_count", "0"),
        ] {
            stdfs::write(tt_dir.join(f), format!("{}\n", v)).unwrap();
        }
        tt_dir
    }

    #[test]
    fn find_tt_class_dir_via_device_symlink() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        fake_hwmon(&hwmon);
        let expected = fake_tt_class(&hwmon);
        assert_eq!(SysfsBackend::find_tt_class_dir(&hwmon), Some(expected));
    }

    #[test]
    fn find_tt_class_dir_absent_returns_none() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        fake_hwmon(&hwmon); // no device symlink at all
        assert_eq!(SysfsBackend::find_tt_class_dir(&hwmon), None);
    }

    #[test]
    fn read_string_attr_trims() {
        let td = tempfile::tempdir().unwrap();
        stdfs::write(td.path().join("tt_card_type"), "p300c\n").unwrap();
        assert_eq!(
            SysfsBackend::read_string_attr(td.path(), "tt_card_type"),
            Some("p300c".to_string())
        );
        assert_eq!(SysfsBackend::read_string_attr(td.path(), "tt_missing"), None);
    }

    #[test]
    fn synthesize_smbus_from_tt_attrs() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        fake_hwmon(&hwmon);
        let tt_dir = fake_tt_class(&hwmon);
        let fan_path = hwmon.join("fan1_input");
        let smbus = SysfsBackend::synthesize_smbus(Some(&tt_dir), Some(fan_path.as_path()));
        // Clocks come through as plain decimal strings (SmbusTelemetry stores strings).
        assert_eq!(smbus.aiclk.as_deref(), Some("800"));
        assert_eq!(smbus.axiclk.as_deref(), Some("960"));
        assert_eq!(smbus.arcclk.as_deref(), Some("800"));
        assert_eq!(smbus.therm_trip_count.as_deref(), Some("0"));
        assert_eq!(smbus.arc0_health.as_deref(), Some("43874")); // tt_heartbeat
        assert_eq!(smbus.board_id.as_deref(), Some("0000046131924062")); // tt_serial
        // fan1_input = 4294967295 (no-fan sentinel) is stored verbatim; the
        // existing fan_rpm() accessor maps it to None downstream.
        assert_eq!(smbus.fan_speed.as_deref(), Some("4294967295"));
        assert_eq!(smbus.fan_rpm(), None);
    }

    #[test]
    fn synthesize_smbus_no_sources_is_all_none() {
        let smbus = SysfsBackend::synthesize_smbus(None, None);
        assert!(smbus.aiclk.is_none() && smbus.fan_speed.is_none());
    }
}
