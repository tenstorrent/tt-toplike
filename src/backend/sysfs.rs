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

/// A Tenstorrent-looking hwmon directory found during discovery, before a
/// device index has been assigned to it.
///
/// Discovery is deliberately two-phase (collect → sort → assign) because the
/// device index is the join key `HybridBackend` uses against tt-smi's
/// bus-id-ordered `device_info[]`; see
/// [`SysfsBackend::collect_hwmon_candidates`].
struct HwmonCandidate {
    /// The hwmon directory itself (`/sys/class/hwmon/hwmon3`).
    path: PathBuf,
    /// The hwmon `name` attribute (e.g. `"blackhole"`); becomes the initial
    /// `Device::board_type` until a tt_card_type / tt-smi SKU supersedes it.
    name: String,
    /// Architecture inferred from `name`.
    architecture: Architecture,
    /// PCI bus id from the `device` symlink; `None` when unresolvable.
    bus_id: Option<String>,
    /// Directory basename (`"hwmon3"`) — stable `bus_id` fallback.
    dir_name: String,
    /// Numeric part of `dir_name` (`u32::MAX` when unparseable) so unresolved
    /// candidates sort numerically instead of lexicographically.
    hwmon_num: u32,
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

    /// Per-device PCIe counter rate trackers (needs two samples to produce a rate).
    pcie_trackers: HashMap<usize, crate::backend::pcie_counters::PcieRateTracker>,

    /// Last computed per-device PCIe bandwidth.
    pcie_bandwidth: HashMap<usize, crate::backend::pcie_counters::PcieBandwidth>,

    /// When the slow lane (tt-kmd class attrs, synthesized SMBUS, PCIe
    /// counters) last ran. `None` until the first `update()`, so the first tick
    /// always populates everything. See [`slow_lane_due`].
    last_slow_refresh: Option<std::time::Instant>,
}

/// How often the slow lane of [`SysfsBackend::update`] runs.
///
/// The fast lane (4 hwmon inputs per device: temp, volt, power, curr) runs every
/// tick — at the UI's ~10 Hz that's what makes the power/temperature readouts
/// feel live. The slow lane adds ~21 more reads per device (2 tt-kmd class
/// attrs + 7 in `synthesize_smbus` + 12 PCIe counters); at 10 Hz on a 32-chip
/// box that was ~8k synchronous `open`/`read`/`close` per second on the render
/// thread, for values that are static (serial, FW version), coarse (AICLK), or
/// better measured over a longer window (PCIe rates). 1 Hz is well inside the
/// resolution any of these rows can convey.
const SLOW_LANE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

/// Is the slow lane due to run?
///
/// Pure and timestamp-injectable so the cadence can be unit-tested without
/// sleeping. `last == None` (first tick ever) is always due, so a single
/// `update()` call — as `--print` and the tests do — still populates the
/// SMBUS/PCIe/class-attr values.
fn slow_lane_due(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    interval: std::time::Duration,
) -> bool {
    match last {
        None => true,
        // saturating_duration_since: a `now` that predates `last` (clock
        // weirdness, an out-of-order caller) yields 0 rather than panicking,
        // which simply defers the refresh to the next tick.
        Some(prev) => now.saturating_duration_since(prev) >= interval,
    }
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
            pcie_trackers: HashMap::new(),
            pcie_bandwidth: HashMap::new(),
            last_slow_refresh: None,
        }
    }

    /// One hwmon directory that looks like a Tenstorrent device, captured
    /// *before* any device index is assigned so that ordering can be decided
    /// first — see [`SysfsBackend::collect_hwmon_candidates`].
    fn hwmon_candidate(path: PathBuf) -> Option<HwmonCandidate> {
        let name = fs::read_to_string(path.join("name")).ok()?;
        let name = name.trim();

        // Look for Tenstorrent-related hwmon names
        // Common patterns: "tenstorrent", "tt_*", "grayskull", "wormhole", "blackhole"
        if !(name.contains("tenstorrent")
            || name.starts_with("tt_")
            || name.contains("grayskull")
            || name.contains("wormhole")
            || name.contains("blackhole"))
        {
            return None;
        }

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

        let dir_name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Numeric hwmon index ("hwmon10" → 10) so unresolved-bus_id candidates
        // sort numerically rather than lexicographically ("hwmon10" < "hwmon3").
        let hwmon_num = dir_name
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .parse::<u32>()
            .unwrap_or(u32::MAX);

        Some(HwmonCandidate {
            bus_id: Self::extract_pci_address(&path),
            path,
            name: name.to_string(),
            architecture,
            dir_name,
            hwmon_num,
        })
    }

    /// Scan `hwmon_base` for Tenstorrent hwmon directories and return the
    /// candidates in a **deterministic, tt-smi-compatible order**.
    ///
    /// `fs::read_dir` yields entries in filesystem order, which is neither
    /// hwmon-number order nor PCI bus-id order, and is not stable across boots.
    /// (Measured on a 4× Blackhole box: readdir gave 04:00.0, 03:00.0, 01:00.0,
    /// 02:00.0.) [`crate::backend::hybrid::HybridBackend`] joins tt-smi metadata
    /// and SMBUS telemetry onto these devices **by index**, and `tt-smi -s`
    /// emits `device_info[]` sorted ascending by PCI bus id — so assigning
    /// indices in readdir order made every device display another card's
    /// DDR/ARC/GDDR/ECC/PCIe/board-power data. Sorting by bus id here is what
    /// keeps the two lists in agreement.
    ///
    /// Candidates whose bus id can't be resolved from the `device` symlink sort
    /// last, ordered by hwmon number, so they still get a stable index.
    fn collect_hwmon_candidates(hwmon_base: &Path) -> BackendResult<Vec<HwmonCandidate>> {
        if !hwmon_base.exists() {
            return Err(BackendError::Initialization(
                "Hwmon sysfs not available (Linux-specific)".to_string(),
            ));
        }

        let entries = fs::read_dir(hwmon_base).map_err(|e| {
            BackendError::Initialization(format!("Failed to read hwmon dir: {}", e))
        })?;

        let mut candidates: Vec<HwmonCandidate> = entries
            .flatten()
            .filter_map(|entry| Self::hwmon_candidate(entry.path()))
            .collect();

        candidates.sort_by(|a, b| {
            (
                a.bus_id.is_none(),
                a.bus_id.as_deref().unwrap_or(""),
                a.hwmon_num,
                a.dir_name.as_str(),
            )
                .cmp(&(
                    b.bus_id.is_none(),
                    b.bus_id.as_deref().unwrap_or(""),
                    b.hwmon_num,
                    b.dir_name.as_str(),
                ))
        });

        Ok(candidates)
    }

    /// Scan /sys/class/hwmon/ for Tenstorrent devices
    ///
    /// Clears existing state before scanning so that calling init() twice does not
    /// double-count devices. This happens in practice when the tui.rs binary pre-probes
    /// the backend and then run_with_backend() calls init() a second time.
    fn detect_devices(&mut self) -> BackendResult<()> {
        self.detect_devices_in(Path::new("/sys/class/hwmon"))
    }

    /// [`Self::detect_devices`] with an injectable scan root (tests point it at
    /// a temp dir of fake hwmon directories).
    fn detect_devices_in(&mut self, hwmon_base: &Path) -> BackendResult<()> {
        log::info!("SysfsBackend: Scanning {}", hwmon_base.display());

        // Clear any previously discovered state so repeated init() calls are idempotent.
        self.devices.clear();
        self.hwmon_paths.clear();
        self.sensor_paths.clear();
        self.tt_class_dirs.clear();
        self.telemetry_cache.clear();
        self.smbus_cache.clear();
        self.pcie_trackers.clear();
        self.pcie_bandwidth.clear();
        // Re-detection must re-read the slow lane on the next tick, not wait out
        // a stale interval against the previous device set.
        self.last_slow_refresh = None;

        // Collect *then* sort *then* assign indices — every per-index map below
        // is populated from the same sorted sequence, so they stay consistent.
        let candidates = Self::collect_hwmon_candidates(hwmon_base)?;

        for (device_idx, candidate) in candidates.into_iter().enumerate() {
            log::info!(
                "SysfsBackend: Found Tenstorrent device: {} at {:?} (bus {}) → index {}",
                candidate.name,
                candidate.path,
                candidate.bus_id.as_deref().unwrap_or("unknown"),
                device_idx
            );

            let mut device = Device {
                index: device_idx,
                board_type: candidate.name,
                // Fall back to the hwmon directory name rather than the device
                // index: it's stable across boots and doesn't imply a PCI address.
                bus_id: candidate.bus_id.unwrap_or(candidate.dir_name),
                coords: String::new(),
                architecture: candidate.architecture,
                firmwares: None,
                limits: None,
                pcie_speed: None,
                pcie_width: None,
                grid_override: None,
                channels_override: None,
            };
            // Sensor discovery happens here (not in `init()`) because the
            // `*_max` limits below must come from the *same* sensor indices
            // `pick_sensor` selected — see `read_hwmon_limits`.
            let paths = Self::discover_sensor_paths(&candidate.path);
            // Real firmware limits from hwmon *_max files (kmd ≥ 2.9.0).
            // HybridBackend later overwrites with the richer JSON limits
            // block when tt-smi is available; this covers sysfs-only mode.
            device.limits = Self::read_hwmon_limits(&paths);

            self.devices.push(device);
            self.sensor_paths.insert(device_idx, paths);
            self.hwmon_paths.insert(device_idx, candidate.path);
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

    /// Extract the device's PCI address from its hwmon `device` symlink.
    ///
    /// On tt-kmd the link is a single relative hop, e.g.
    /// `/sys/class/hwmon/hwmon6/device -> ../../../0000:04:00.0`.
    ///
    /// The **last** PCI-looking path component is taken, not the first: a longer
    /// link such as `…/pci0000:00/0000:00:01.5/0000:04:00.0/hwmon/hwmon6` walks
    /// down through the upstream bridge (`0000:00:01.5`) before reaching the card
    /// (`0000:04:00.0`), and it's the leaf that identifies the device. Getting
    /// this wrong would also collapse several cards onto the same bus id and so
    /// break the bus-id ordering that `collect_hwmon_candidates` depends on.
    fn extract_pci_address(hwmon_path: &Path) -> Option<String> {
        // Read the device link to find real device path
        let device_link = hwmon_path.join("device");
        let real_path = fs::read_link(&device_link).ok()?;
        let path_str = real_path.to_string_lossy();

        // Look for PCI address pattern: 0000:00:00.0
        path_str
            .split('/')
            .rfind(|component| {
                component.len() >= 12
                    && component.chars().nth(4) == Some(':')
                    && component.chars().nth(7) == Some(':')
            })
            .map(|c| c.to_string())
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

    /// The `*_max` ceiling that belongs to a specific `*_input` file
    /// (`temp2_input` → `temp2_max`).
    ///
    /// Deriving the path from the *selected* input rather than hardcoding index 1
    /// is what keeps a limit paired with the reading it bounds — see
    /// [`Self::read_hwmon_limits`]. `None` when that sensor has no `_max`
    /// attribute, so the caller reports "no limit" instead of borrowing another
    /// sensor's ceiling.
    fn limit_path_for(input: Option<&Path>) -> Option<PathBuf> {
        let input = input?;
        let stem = input.file_name()?.to_str()?.strip_suffix("_input")?;
        Some(input.with_file_name(format!("{}_max", stem)))
    }

    /// Read the hwmon `*_max` limit files into a DeviceLimits.
    ///
    /// tt-kmd ≥ 2.9.0 exposes firmware-provided limits on Blackhole too:
    /// `power<N>_max` (µW), `curr<N>_max` (mA), `temp<N>_max` (m°C). These are
    /// the real per-board ceilings (e.g. 125 W / 500 A / 90 °C on p300c) — far
    /// better than the UI's hardcoded 300 W / 105 °C fallbacks. asic_fmax is not
    /// exposed by hwmon; it stays None (the JSON limits block has it).
    /// Returns None when no limit file exists (older driver) so callers can
    /// leave `Device::limits` untouched.
    ///
    /// **The limits follow the sensors `pick_sensor` chose**, they are not read
    /// from index 1. A device can expose several sensors of one class — tt-kmd
    /// labels them, and `discover_sensor_paths` is label-aware precisely because
    /// `temp1_label=vreg_temp` can sit next to `temp2_label=asic_temp`. Reading
    /// `temp1_max` unconditionally on such a driver compared the ASIC temperature
    /// against the *VReg* ceiling (125 °C instead of 90 °C), so the Insights
    /// limit annotation and the starfield thermal-headroom cue were both wrong in
    /// the non-alarming direction — the worst way to be wrong about a thermal
    /// limit.
    fn read_hwmon_limits(paths: &SensorPaths) -> Option<crate::models::telemetry::DeviceLimits> {
        let tdp_limit = Self::limit_path_for(paths.power.as_deref())
            .and_then(|p| Self::read_u64_file(&p))
            .map(|uw| uw as f32 / 1_000_000.0);
        let tdc_limit = Self::limit_path_for(paths.current.as_deref())
            .and_then(|p| Self::read_u64_file(&p))
            .map(|ma| ma as f32 / 1000.0);
        let thm_limit = Self::limit_path_for(paths.temperature.as_deref())
            .and_then(|p| Self::read_u64_file(&p))
            .map(|mc| mc as f32 / 1000.0);
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

    /// Did [`Self::synthesize_smbus`] actually find anything?
    ///
    /// **Why this gate exists.** `smbus_telemetry()` returning `Some(block)` is
    /// read across the UI as "SMBUS data is available for this device", and
    /// several call sites branch on the *presence* of the block rather than on
    /// the specific field they want:
    ///
    /// * `animation::memory_castle` paints the ARC dot from
    ///   `is_arc0_healthy()`, which is `arc0_health.unwrap_or(0) > 0` — an
    ///   all-`None` block reads as "ARC dead" and puts a red failure dot on a
    ///   perfectly healthy card, where previously (`None`) the indicator was
    ///   simply hidden.
    /// * the Insights ETH row treats `Some` as "firmware said 0 ports enabled"
    ///   (`0/? live`) and reserves its architectural-maximum fallback
    ///   (`0/24 live`) for `None`.
    ///
    /// So on a driver that exposes neither the `tenstorrent` class dir (kmd <
    /// 2.7) nor a fan sensor, this must stay `None` — exactly the pre-0.8.0
    /// semantics. Only the fields `synthesize_smbus` fills are checked; the
    /// remaining ~40 SMBUS fields are unreachable from sysfs by construction.
    fn synthesized_smbus_has_data(s: &SmbusTelemetry) -> bool {
        s.aiclk.is_some()
            || s.axiclk.is_some()
            || s.arcclk.is_some()
            || s.arc0_health.is_some()
            || s.therm_trip_count.is_some()
            || s.board_id.is_some()
            || s.m3_app_fw_version.is_some()
            // `fan_rpm()`, not `fan_speed.is_some()`: hwmon exposes `fan1_input`
            // on cards that have no readable fan, where it reads the firmware
            // all-ones sentinel (0xFFFFFFFF — the value this file's own
            // `fake_hwmon` fixture uses as the realistic p300c reading). A raw
            // presence check counts that as data and publishes a block whose
            // only populated field is a non-reading, which is exactly the
            // all-`None`-block failure described above. The shared
            // `real_fan_rpm` predicate (models::telemetry) filters it.
            || s.fan_rpm().is_some()
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

    /// Insert a synthesized SMBUS entry directly into the cache — test helper
    /// only. Lets `HybridBackend` tests exercise the no-tt-smi fallback path
    /// without a real hwmon tree.
    #[cfg(test)]
    pub(crate) fn insert_smbus_for_test(&mut self, device_idx: usize, smbus: SmbusTelemetry) {
        self.smbus_cache.insert(device_idx, smbus);
    }

    /// Append a device to the list without touching the filesystem — test helper
    /// only. Lets `HybridBackend` tests build a multi-card device list (with real
    /// bus ids, which its tt-smi join keys on) without a fake hwmon tree.
    /// `update()` on such a device is a no-op: it has no cached sensor paths.
    #[cfg(test)]
    pub(crate) fn push_device_for_test(&mut self, device: Device) {
        self.devices.push(device);
    }
}

impl TelemetryBackend for SysfsBackend {
    fn init(&mut self) -> BackendResult<()> {
        // `detect_devices` also caches the per-device sensor paths (so `update()`
        // never retries missing indices) — it has to, because the hwmon `*_max`
        // limits it reads must come from the sensors discovery selected.
        self.detect_devices()?;

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
        // Decide once per tick whether the slow lane runs — see
        // `slow_lane_due` for the cadence rationale.
        let now = std::time::Instant::now();
        let slow_lane = slow_lane_due(self.last_slow_refresh, now, SLOW_LANE_INTERVAL);
        if slow_lane {
            self.last_slow_refresh = Some(now);
        }

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
            //
            // Slow lane: these are two more file opens per device per tick on
            // top of the hwmon reads above, and neither is worth ~10 Hz — AICLK
            // is a coarse DVFS clock and tt_heartbeat is a liveness counter.
            // Between refreshes the previous values are carried forward so no
            // row blinks out.
            let tt_dir = self.tt_class_dirs.get(&device_idx);
            let (aiclk, heartbeat) = if slow_lane {
                (
                    tt_dir
                        .and_then(|d| Self::read_u64_file(&d.join("tt_aiclk")))
                        .map(|v| v as u32),
                    tt_dir
                        .and_then(|d| Self::read_u64_file(&d.join("tt_heartbeat")))
                        .map(|v| v as u32),
                )
            } else {
                let prev = self.telemetry_cache.get(&device_idx);
                (prev.and_then(|t| t.aiclk), prev.and_then(|t| t.heartbeat))
            };

            let telemetry = Telemetry {
                timestamp: chrono::Utc::now(),
                voltage,
                current: calculated_current,
                power,
                asic_temperature: temperature,
                aiclk,
                heartbeat,
                // hwmon/tt-kmd doesn't expose a separate whole-board power
                // rail distinct from the per-asic reading above.
                board_power: None,
            };

            // Slow lane: 7 more file reads per device (clocks, heartbeat, therm
            // trip count, serial, M3 FW, fan) for fields that are static or
            // slow-moving. The cached block is left untouched in between.
            if slow_lane {
                let smbus =
                    Self::synthesize_smbus(tt_dir.map(|p| p.as_path()), paths.fan.as_deref());
                // Only publish a block that actually carries data — an
                // all-`None` block is read elsewhere as "SMBUS present, ARC
                // dead"; see `synthesized_smbus_has_data`.
                if Self::synthesized_smbus_has_data(&smbus) {
                    self.smbus_cache.insert(device_idx, smbus);
                } else {
                    self.smbus_cache.remove(&device_idx);
                }
            }

            self.telemetry_cache.insert(device_idx, telemetry);

            // PCIe link bandwidth from the kmd counter files (passive reads).
            //
            // Slow lane: 12 counter files per device — by far the heaviest part
            // of this loop. Sampling at ~1 Hz also *improves* the derived rate:
            // the tracker computes its own dt, and a 1 s window smooths the
            // quantization noise a 100 ms window shows on a bursty link.
            if slow_lane {
                let snap = self.tt_class_dirs.get(&device_idx).and_then(|tt_dir| {
                    crate::backend::pcie_counters::read_counters(&tt_dir.join("pcie_perf_counters"))
                });
                match snap {
                    Some(snap) => {
                        let tracker = self.pcie_trackers.entry(device_idx).or_default();
                        if let Some(bw) = tracker.sample(snap, now) {
                            self.pcie_bandwidth.insert(device_idx, bw);
                        }
                    }
                    // Counter set no longer readable — the driver was reloaded,
                    // the device was reset or surprise-removed, or the attribute
                    // group's permissions changed. Drop the cached rate rather
                    // than letting it render on as if it were live; that is the
                    // same "confident number for an unsampled link" failure
                    // `read_counters` returns `None` to avoid, just one layer up.
                    // The tracker goes too, so a link that comes back re-baselines
                    // instead of differencing across the outage.
                    None => {
                        self.pcie_bandwidth.remove(&device_idx);
                        self.pcie_trackers.remove(&device_idx);
                    }
                }
            }
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

    fn pcie_bandwidth(
        &self,
        device_idx: usize,
    ) -> Option<crate::backend::pcie_counters::PcieBandwidth> {
        self.pcie_bandwidth.get(&device_idx).copied()
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

    #[test]
    fn pcie_bandwidth_none_without_counter_dir() {
        let backend = SysfsBackend::new();
        assert!(backend.pcie_bandwidth(0).is_none());
    }

    /// The cached rate must be dropped when the counter set stops being
    /// readable — a driver reload, a device reset/surprise-removal, or a
    /// permissions change on the attribute group.
    ///
    /// `read_counters` deliberately returns `None` there rather than a
    /// zero-valued snapshot, precisely so the UI never shows a confident
    /// `▼0 B/s ▲0 B/s` for a link nobody sampled. Keeping the *last* rate in
    /// the cache is the same lie one layer up: the sidebar keeps rendering a
    /// live-looking rate for a link that is no longer being measured.
    #[test]
    fn pcie_bandwidth_is_cleared_when_the_counter_dir_disappears() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        fake_hwmon(&hwmon);
        let tt_dir = fake_tt_class(&hwmon);
        let counters = tt_dir.join("pcie_perf_counters");
        stdfs::create_dir_all(&counters).unwrap();

        let write_counters = |base: u64| {
            for f in [
                "mst_nonposted_wr_data_word_sent0",
                "mst_posted_wr_data_word_sent0",
                "mst_rd_data_word_received0",
                "slv_nonposted_wr_data_word_received0",
                "slv_posted_wr_data_word_received0",
                "slv_rd_data_word_sent0",
            ] {
                stdfs::write(counters.join(f), base.to_string()).unwrap();
            }
        };

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();
        // detect_devices_in doesn't resolve class dirs (init() does), so do it
        // the way init() would — without the tt-smi enrichment subprocess.
        backend
            .tt_class_dirs
            .insert(0, SysfsBackend::find_tt_class_dir(&hwmon).unwrap());

        // Two slow-lane passes over a moving counter set → a real rate.
        write_counters(1_000);
        backend.update().unwrap();
        write_counters(2_000);
        backend.last_slow_refresh = None; // force the slow lane again
        backend.update().unwrap();
        assert!(
            backend.pcie_bandwidth(0).is_some(),
            "two samples of a readable counter set must yield a rate"
        );

        // The attribute group goes away (driver reload / device reset).
        stdfs::remove_dir_all(&counters).unwrap();
        backend.last_slow_refresh = None;
        backend.update().unwrap();
        assert!(
            backend.pcie_bandwidth(0).is_none(),
            "an unreadable counter set must clear the cached rate, not preserve it"
        );
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
            ("temp1_input", "38036"),
            ("temp1_label", "asic_temp"),
            ("temp1_max", "90000"),
            ("in0_input", "718"),
            ("in0_label", "vcore"),
            ("in0_max", "900"),
            ("power1_input", "16000000"),
            ("power1_label", "power"),
            ("power1_max", "125000000"),
            ("curr1_input", "23000"),
            ("curr1_label", "current"),
            ("curr1_max", "500000"),
            ("fan1_input", "4294967295"),
            ("fan1_label", "fan_rpm"),
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
        let paths = SysfsBackend::discover_sensor_paths(td.path());
        let lim = SysfsBackend::read_hwmon_limits(&paths).unwrap();
        assert_eq!(lim.tdp_limit, Some(125.0)); // 125000000 µW
        assert_eq!(lim.tdc_limit, Some(500.0)); // 500000 mA
        assert_eq!(lim.thm_limit, Some(90.0)); // 90000 m°C
        assert_eq!(lim.asic_fmax, None); // not exposed by hwmon
    }

    #[test]
    fn read_hwmon_limits_absent_files_yield_none() {
        let td = tempfile::tempdir().unwrap();
        stdfs::write(td.path().join("name"), "blackhole").unwrap();
        let paths = SysfsBackend::discover_sensor_paths(td.path());
        assert!(SysfsBackend::read_hwmon_limits(&paths).is_none());
    }

    /// Limits must come from the sensor `pick_sensor` selected, not from index 1.
    ///
    /// Regression: `read_hwmon_limits` read `temp1_max` unconditionally while the
    /// *reading* came from the label-matched `temp2_input`. On a driver that
    /// exposes `temp1_label=vreg_temp` alongside `temp2_label=asic_temp` (the
    /// decoy layout `discover_sensor_paths_prefers_asic_temp_label` builds) the
    /// ASIC temperature was therefore compared against the VReg ceiling —
    /// 125 °C instead of 90 °C — making the Insights limit annotation and the
    /// starfield thermal-headroom cue wrong in the *non-alarming* direction.
    #[test]
    fn read_hwmon_limits_follows_the_selected_sensor_not_index_1() {
        let td = tempfile::tempdir().unwrap();
        fake_hwmon(td.path());
        // temp1 is the VReg decoy with a much higher ceiling; temp2 is the ASIC.
        stdfs::write(td.path().join("temp1_label"), "vreg_temp").unwrap();
        stdfs::write(td.path().join("temp1_max"), "125000").unwrap();
        stdfs::write(td.path().join("temp2_input"), "40000").unwrap();
        stdfs::write(td.path().join("temp2_label"), "asic_temp").unwrap();
        stdfs::write(td.path().join("temp2_max"), "90000").unwrap();

        let paths = SysfsBackend::discover_sensor_paths(td.path());
        let lim = SysfsBackend::read_hwmon_limits(&paths).unwrap();
        assert_eq!(
            lim.thm_limit,
            Some(90.0),
            "the ASIC sensor's own ceiling must win over the VReg sensor's"
        );
    }

    /// A selected sensor with no `_max` sibling reports no limit rather than
    /// borrowing a different sensor's ceiling.
    #[test]
    fn read_hwmon_limits_missing_max_for_selected_sensor_is_none() {
        let td = tempfile::tempdir().unwrap();
        fake_hwmon(td.path());
        // Same decoy layout, but the ASIC sensor has no ceiling of its own.
        stdfs::write(td.path().join("temp1_label"), "vreg_temp").unwrap();
        stdfs::write(td.path().join("temp1_max"), "125000").unwrap();
        stdfs::write(td.path().join("temp2_input"), "40000").unwrap();
        stdfs::write(td.path().join("temp2_label"), "asic_temp").unwrap();

        let paths = SysfsBackend::discover_sensor_paths(td.path());
        let lim = SysfsBackend::read_hwmon_limits(&paths).unwrap();
        assert_eq!(lim.thm_limit, None, "must not fall back to temp1_max");
        // The other classes still report (single unambiguous sensor each).
        assert_eq!(lim.tdp_limit, Some(125.0));
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
        assert_eq!(
            SysfsBackend::read_string_attr(td.path(), "tt_missing"),
            None
        );
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
        assert!(
            !SysfsBackend::synthesized_smbus_has_data(&smbus),
            "an all-None block must never be published as SMBUS data"
        );
    }

    /// `smbus_telemetry()` must stay `None` on a driver that exposes neither the
    /// tt-kmd class dir nor a fan — the pre-0.8.0 semantics.
    ///
    /// Regression: `update()` inserted the synthesized block unconditionally, so
    /// this returned `Some(all-None)`. Two concrete consequences, both visible on
    /// a healthy card: `animation::memory_castle` paints its ARC dot from
    /// `is_arc0_healthy()` (`arc0_health.unwrap_or(0) > 0`) and so showed a red
    /// "ARC ○" failure indicator where it used to show nothing, and the Insights
    /// ETH row's documented "SMBUS absent entirely → architectural max" fallback
    /// became unreachable (`0/? live` instead of `0/24 live`). Via
    /// `HybridBackend::smbus_telemetry`'s `.or_else()` this reached the *default*
    /// Linux backend on any box without tt-smi installed.
    #[test]
    fn smbus_telemetry_is_none_without_class_dir_or_fan() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        // hwmon sensors only: no `device` symlink (→ no tt class dir) and no fan.
        for (f, v) in [
            ("name", "blackhole"),
            ("temp1_input", "38036"),
            ("temp1_label", "asic_temp"),
            ("in0_input", "718"),
            ("power1_input", "16000000"),
            ("curr1_input", "23000"),
        ] {
            stdfs::write(hwmon.join(f), v).unwrap();
        }

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();
        backend.update().unwrap();

        assert!(
            backend.telemetry(0).is_some(),
            "core hwmon telemetry must still be served"
        );
        assert_eq!(backend.telemetry(0).unwrap().asic_temperature, Some(38.036));
        assert!(
            backend.smbus_telemetry(0).is_none(),
            "no class dir and no fan → no SMBUS block at all"
        );
    }

    /// Same gate, but with a fan file whose value is the firmware "no reading"
    /// sentinel — the realistic p300c case, where hwmon exposes `fan1_input`
    /// and the fanless card reports all-ones.
    ///
    /// A raw `fan_speed.is_some()` presence check treats that sentinel as data
    /// and publishes an otherwise all-`None` SMBUS block, which is precisely
    /// what the gate exists to prevent (red "ARC dead" dot, `0/? live` ETH).
    #[test]
    fn smbus_telemetry_is_none_when_only_fan_is_a_sentinel() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        // hwmon sensors only (no `device` symlink → no tt class dir), plus a
        // fan sensor reporting the all-ones sentinel.
        for (f, v) in [
            ("name", "blackhole"),
            ("temp1_input", "38036"),
            ("temp1_label", "asic_temp"),
            ("in0_input", "718"),
            ("power1_input", "16000000"),
            ("curr1_input", "23000"),
            ("fan1_input", "4294967295"),
        ] {
            stdfs::write(hwmon.join(f), v).unwrap();
        }

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();
        backend.update().unwrap();

        assert!(
            backend.smbus_telemetry(0).is_none(),
            "a sentinel fan reading is not data — the block must stay absent"
        );
    }

    /// A *real* fan reading, on the other hand, is enough on its own.
    #[test]
    fn smbus_telemetry_is_some_for_a_real_fan_reading() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        for (f, v) in [
            ("name", "blackhole"),
            ("temp1_input", "38036"),
            ("temp1_label", "asic_temp"),
            ("fan1_input", "1882"),
        ] {
            stdfs::write(hwmon.join(f), v).unwrap();
        }

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();
        backend.update().unwrap();

        let smbus = backend
            .smbus_telemetry(0)
            .expect("a real fan RPM is real SMBUS data");
        assert_eq!(smbus.fan_rpm(), Some(1882));
    }

    /// With the tt-kmd class dir present, the synthesized block *is* served.
    #[test]
    fn smbus_telemetry_is_some_with_class_dir() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        fake_hwmon(&hwmon);
        fake_tt_class(&hwmon);

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();
        // detect_devices_in doesn't resolve class dirs (init() does), so do it
        // the way init() would — without the tt-smi enrichment subprocess.
        let tt_dir = SysfsBackend::find_tt_class_dir(&hwmon).unwrap();
        backend.tt_class_dirs.insert(0, tt_dir);
        backend.update().unwrap();

        let smbus = backend
            .smbus_telemetry(0)
            .expect("class-dir attrs must be published");
        assert_eq!(smbus.aiclk.as_deref(), Some("800"));
        assert_eq!(smbus.arc0_health.as_deref(), Some("43874"));
        // And the core telemetry picked up the same attrs.
        assert_eq!(backend.telemetry(0).unwrap().aiclk, Some(800));
    }

    /// Slow-lane cadence, asserted on injected timestamps — no sleeping.
    #[test]
    fn slow_lane_due_respects_the_interval() {
        let t0 = std::time::Instant::now();
        let interval = std::time::Duration::from_millis(1000);

        // First tick ever: always due, so one-shot callers (`--print`) get a
        // fully populated snapshot.
        assert!(slow_lane_due(None, t0, interval));
        // Inside the window: skipped (this is the ~10 Hz render path).
        assert!(!slow_lane_due(
            Some(t0),
            t0 + std::time::Duration::from_millis(100),
            interval
        ));
        assert!(!slow_lane_due(
            Some(t0),
            t0 + std::time::Duration::from_millis(999),
            interval
        ));
        // Exactly at, and past, the interval: due again.
        assert!(slow_lane_due(Some(t0), t0 + interval, interval));
        assert!(slow_lane_due(
            Some(t0),
            t0 + std::time::Duration::from_millis(2500),
            interval
        ));
        // A `now` that predates `last` defers rather than panicking.
        assert!(!slow_lane_due(
            Some(t0 + std::time::Duration::from_millis(500)),
            t0,
            interval
        ));
    }

    /// The slow lane must not re-read on back-to-back ticks, and the values it
    /// produced must survive the ticks that skip it (no flicker).
    #[test]
    fn update_retains_slow_lane_values_between_refreshes() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon2");
        stdfs::create_dir_all(&hwmon).unwrap();
        fake_hwmon(&hwmon);
        let tt_dir = fake_tt_class(&hwmon);

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();
        backend
            .tt_class_dirs
            .insert(0, SysfsBackend::find_tt_class_dir(&hwmon).unwrap());

        backend.update().unwrap(); // first tick: slow lane runs
        assert_eq!(backend.telemetry(0).unwrap().aiclk, Some(800));

        // Change the attr underneath us, then tick again immediately. The slow
        // lane is not due, so the *previous* values must be carried forward —
        // proving the reads were skipped, and that nothing blanks out.
        stdfs::write(tt_dir.join("tt_aiclk"), "1350\n").unwrap();
        backend.update().unwrap();
        assert_eq!(
            backend.telemetry(0).unwrap().aiclk,
            Some(800),
            "slow-lane values are retained between refreshes"
        );
        assert_eq!(
            backend.smbus_telemetry(0).unwrap().aiclk.as_deref(),
            Some("800"),
            "the synthesized SMBUS block is retained too"
        );

        // Force the interval to have elapsed and the new value comes through.
        backend.last_slow_refresh =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
        backend.update().unwrap();
        assert_eq!(backend.telemetry(0).unwrap().aiclk, Some(1350));
    }

    /// Create a fake hwmon dir whose `device` symlink resolves to `bus_id`,
    /// mimicking the real tt-kmd layout
    /// (`/sys/class/hwmon/hwmon6/device -> ../../../0000:04:00.0`).
    /// The target need not exist — `extract_pci_address` only reads the link.
    fn fake_hwmon_with_bus(base: &Path, dir_name: &str, bus_id: Option<&str>) {
        let hwmon = base.join(dir_name);
        stdfs::create_dir_all(&hwmon).unwrap();
        stdfs::write(hwmon.join("name"), "blackhole\n").unwrap();
        if let Some(bus) = bus_id {
            std::os::unix::fs::symlink(format!("../../../{}", bus), hwmon.join("device")).unwrap();
        }
    }

    /// A longer link that walks through the upstream bridge must still yield the
    /// leaf card address, not the bridge's.
    #[test]
    fn extract_pci_address_prefers_leaf_over_bridge() {
        let td = tempfile::tempdir().unwrap();
        let hwmon = td.path().join("hwmon6");
        stdfs::create_dir_all(&hwmon).unwrap();
        std::os::unix::fs::symlink(
            "../../devices/pci0000:00/0000:00:01.5/0000:04:00.0/hwmon/hwmon6",
            hwmon.join("device"),
        )
        .unwrap();
        assert_eq!(
            SysfsBackend::extract_pci_address(&hwmon).as_deref(),
            Some("0000:04:00.0")
        );
    }

    /// Device indices must follow ascending PCI bus id, NOT `read_dir` order.
    ///
    /// `HybridBackend` joins tt-smi metadata/SMBUS onto sysfs devices by index,
    /// and `tt-smi -s` orders `device_info[]` by bus id; readdir order on a real
    /// 4× Blackhole box was 04, 03, 01, 02, so index-order joins mismatched every
    /// card. The fake dirs below are named so that hwmon-number order (2,3,5,7),
    /// readdir order, and bus-id order all disagree.
    #[test]
    fn detect_devices_sorted_by_bus_id_not_readdir_order() {
        let td = tempfile::tempdir().unwrap();
        fake_hwmon_with_bus(td.path(), "hwmon5", Some("0000:04:00.0"));
        fake_hwmon_with_bus(td.path(), "hwmon2", Some("0000:03:00.0"));
        fake_hwmon_with_bus(td.path(), "hwmon7", Some("0000:01:00.0"));
        fake_hwmon_with_bus(td.path(), "hwmon3", Some("0000:02:00.0"));
        // A non-Tenstorrent hwmon node must be ignored entirely.
        let other = td.path().join("hwmon0");
        stdfs::create_dir_all(&other).unwrap();
        stdfs::write(other.join("name"), "coretemp\n").unwrap();

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();

        let buses: Vec<&str> = backend.devices.iter().map(|d| d.bus_id.as_str()).collect();
        assert_eq!(
            buses,
            vec![
                "0000:01:00.0",
                "0000:02:00.0",
                "0000:03:00.0",
                "0000:04:00.0"
            ],
            "devices must be ordered ascending by PCI bus id"
        );
        // Indices must match position, and hwmon_paths must agree with them.
        for (pos, device) in backend.devices.iter().enumerate() {
            assert_eq!(device.index, pos, "index must match ordering position");
            let path = backend
                .hwmon_paths
                .get(&pos)
                .expect("hwmon_paths must have an entry per index");
            assert_eq!(
                SysfsBackend::extract_pci_address(path).as_deref(),
                Some(device.bus_id.as_str()),
                "hwmon_paths[{}] must point at the same card as devices[{}]",
                pos,
                pos
            );
        }
    }

    /// Candidates without a resolvable bus id sort last, in hwmon-number order,
    /// and fall back to the hwmon directory name as their `bus_id`.
    #[test]
    fn detect_devices_unresolved_bus_ids_sort_last_deterministically() {
        let td = tempfile::tempdir().unwrap();
        fake_hwmon_with_bus(td.path(), "hwmon10", None);
        fake_hwmon_with_bus(td.path(), "hwmon4", Some("0000:05:00.0"));
        fake_hwmon_with_bus(td.path(), "hwmon2", None);

        let mut backend = SysfsBackend::new();
        backend.detect_devices_in(td.path()).unwrap();

        let buses: Vec<&str> = backend.devices.iter().map(|d| d.bus_id.as_str()).collect();
        assert_eq!(
            buses,
            // Numeric (not lexicographic) hwmon ordering: hwmon2 before hwmon10.
            vec!["0000:05:00.0", "hwmon2", "hwmon10"],
        );
    }

    #[test]
    fn detect_devices_errors_when_no_tt_hwmon_present() {
        let td = tempfile::tempdir().unwrap();
        let other = td.path().join("hwmon0");
        stdfs::create_dir_all(&other).unwrap();
        stdfs::write(other.join("name"), "nvme\n").unwrap();
        let mut backend = SysfsBackend::new();
        assert!(backend.detect_devices_in(td.path()).is_err());
    }
}
