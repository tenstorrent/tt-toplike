# Telemetry Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Read every telemetry signal the current driver/tooling stack exposes: tt-kmd's `tt_*` sysfs attributes + hwmon limits/labels/fan, live PCIe bandwidth counters, tt-smi 5.x/6.x snapshot fields we currently drop (full board_info, `processes[]`, `board_power`, `FAN_RPM`), fix the KV-cache metric-name split, surface parsed-but-hidden diagnostics (GDDR uncorrectable ECC, thermal trips, current w/ TDC limit), and migrate the luwen backend off the abandoned `all-smi-luwen-*` forks to official `luwen-api`/`luwen-def` 0.8.5.

**Architecture:** All additions are read-only file reads or serde-struct extensions layered onto existing backends. The sysfs backend gains a second attribute source (`/sys/class/tenstorrent/tenstorrent!N/`, reached via each hwmon dir's `device` symlink → `<pci>/tenstorrent/<subdir>`) and starts synthesizing a partial `SmbusTelemetry`. PCIe bandwidth is a new small module with a rate tracker, exposed via a defaulted `TelemetryBackend` trait method (avoids touching every backend). JSON-backend work is pure parser/model extension. Luwen migration is a dependency + field-mapping swap, compile-verified only (never run against this box's live workloads).

**Tech Stack:** Rust 1.93, serde, tempfile (tests). New deps: `luwen-api = 0.8.5`, `luwen-def = 0.8.5` (optional, replacing `all-smi-luwen-*`).

## Global Constraints

- **Build gotcha:** an untracked `.cargo/config.toml` redirects crates to an incomplete `vendor/`. Before ANY cargo command: `mv /home/ttuser/code/tt-toplike/.cargo/config.toml /home/ttuser/code/tt-toplike/.cargo/config.toml.bak 2>/dev/null || true`. Restore it in the final task.
- Run tests with `cargo test --features tui` (matches CI). `cargo build --bin tt-toplike-tui --features tui` for compile checks.
- **NEVER run any binary with `--backend luwen` on this machine** — live inference workloads may be running. Luwen changes are compile-verified only (`cargo build --features luwen-backend`).
- Adding a field to `Telemetry` is a compile error at **every** struct literal (17 sites incl. tests) — fix all sites the compiler flags with `<field>: None`.
- TUI: left-side/bottom borders only, never right-side border characters.
- Comment style: deep comments explaining hardware semantics (match existing files).
- All sysfs reads must tolerate missing files/dirs (older tt-kmd) by returning `None` — never error.
- Commit per task with conventional-commit messages. Do not push.

## Ground truth (verified on this box, tt-kmd 2.9.0, 4× Blackhole p300c)

```
/sys/class/hwmon/hwmonN/            name=blackhole
  temp1_input=38036 temp1_label=asic_temp temp1_max=90000        (m°C)
  in0_input=718     in0_label=vcore      in0_max=900             (mV)
  power1_input=16000000 power1_label=power power1_max=125000000  (µW)
  curr1_input=23000 curr1_label=current  curr1_max=500000        (mA)
  fan1_input=4294967295 fan1_label=fan_rpm                       (RPM; 0xFFFFFFFF = no fan)
  device -> ../../<pci addr dir>
/sys/devices/pci.../0000:01:00.0/tenstorrent/tenstorrent!0/      (reachable as <hwmon>/device/tenstorrent/<only subdir>)
  tt_aiclk=800 tt_axiclk=960 tt_arcclk=800 (MHz, plain decimal)
  tt_serial=0000046131924062  tt_card_type=p300c  tt_fw_bundle_ver=19.11.0.0
  tt_heartbeat=43874  tt_therm_trip_count=0  tt_asic_id=FCF9BCF9E3C8B89E  tt_m3app_fw_ver=0.27.0.0
  pcie_perf_counters/   12 files, u64 decimal, monotonically increasing 32-bit-word counts:
    mst_nonposted_wr_data_word_sent{0,1}  mst_posted_wr_data_word_sent{0,1}
    mst_rd_data_word_received{0,1}        slv_nonposted_wr_data_word_received{0,1}
    slv_posted_wr_data_word_received{0,1} slv_rd_data_word_sent{0,1}
```
Counter semantics: `mst_*` = device-initiated (device is PCIe master), `slv_*` = host-initiated (device is target). Data **into** the chip = `slv_*_wr_*_received* + mst_rd_data_word_received*`. Data **out of** the chip = `slv_rd_data_word_sent* + mst_*_wr_*_sent*`. Words are 32-bit → bytes = words × 4.

tt-smi ≥ 6.0.0 snapshot additions: top-level `"processes": [{"pid": int, "user": str, "device": int, "cmdline": str}]` (fields optional, serialized with exclude_none) and `telemetry.board_power`. board_info (already in 5.x, we drop it): `"dram_status": true, "dram_speed": "16G", "pcie_speed": 4, "pcie_width": "4"` — note pcie_speed is a NUMBER and pcie_width is a STRING in 5.3.0 output.

luwen-api 0.8.5 API (verified on docs.rs):
- `luwen_api::detect_chips_silent(root_chips: Vec<Chip>, options: ChipDetectOptions) -> Result<Vec<Chip>, PlatformError>` — returns initialized chips, no init-callback dance.
- `luwen_api::ChipDetectOptions { continue_on_failure: bool, local_only: bool, chip_filter: Vec<Arch>, noc_safe: bool }` + `Default`.
- `luwen_api::chip::{Chip, ChipImpl, Telemetry}`; `luwen_def::Arch::{Grayskull, Wormhole, Blackhole}`.
- `Telemetry` public fields (all `u32` unless noted): `board_id: u64`, `ddr_status`, `ddr_speed: Option<u32>`, `eth_status0/1`, `pcie_status`, `faults`, `arc0..3_fw_version`, `eth_fw_version`, `m3_bl/app_fw_version`, `arc0..3_health`, `aiclk`, `axiclk`, `arcclk`, `vcore`, `asic_temperature`, `vreg_temperature`, `board_temperature`, `tdp`, `tdc`, `throttler`, `fan_speed`, `fan_rpm`, `vdd_limits`, `thm_limits`, `asic_tmon0/1`, `mvddq_power`, `gddr_train_temp0/1`, `boot_date`, `rt_seconds`, `eth_debug_status0/1`, `tt_flash_version`, `enum_version`, `device_id`, `spibootrom_fw_version`, `wh_fw_date`, `aux_status: Option<u32>`, `timer_heartbeat`, `gddr01_temp`, `gddr23_temp`, `gddr45_temp`, `gddr67_temp`, `max_gddr_temp`, `gddr01_corr_errs`, `gddr23_corr_errs`, `gddr45_corr_errs`, `gddr67_corr_errs`, `gddr_uncorr_errs`, `therm_trip_count`, `harvesting_state`, `tensix_enabled_col`, `enabled_eth`, `enabled_gddr`, `enabled_l2cpu`, `board_power_limit`, `input_power`, `asic_power: Option<u32>`. Methods: `voltage() -> f64`, `current() -> f64`, `power() -> f64`, `asic_temperature() -> f64` (handles 16.16 fixed-point), `ai_clk() -> u32`, `telemetry_heartbeat() -> u32`, `board_type() -> &'static str`.
- NOT in luwen Telemetry: `eth_live_status` (leave None).

---

### Task 1: Sysfs — label-aware hwmon discovery, fan sensor, and `*_max` limits

**Files:**
- Modify: `src/backend/sysfs.rs` (SensorPaths struct :33-39, discover_sensor_paths :234-267, detect_devices :84-172, update :341-376)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `SensorPaths { temperature, voltage, power, current, fan: Option<PathBuf> }`; `fn discover_sensor_paths(hwmon_path: &Path) -> SensorPaths` (now label-aware); `fn read_hwmon_limits(hwmon_path: &Path) -> Option<crate::models::telemetry::DeviceLimits>`; `fn read_u64_file(path: &Path) -> Option<u64>`. Task 3 consumes `paths.fan`.

- [ ] **Step 1: Write failing tests** (in `mod tests`, using `tempfile::tempdir()` to fabricate a fake hwmon dir):

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features tui backend::sysfs -- --nocapture`
Expected: FAIL — no `fan` field, no `read_hwmon_limits`, label test fails (first-match picks temp1).

- [ ] **Step 3: Implement.**

Extend `SensorPaths`:
```rust
#[derive(Default)]
struct SensorPaths {
    temperature: Option<PathBuf>, // tempN_input (millicelsius)
    voltage: Option<PathBuf>,     // inN_input   (millivolts)
    power: Option<PathBuf>,       // powerN_input (microwatts)
    current: Option<PathBuf>,     // currN_input  (milliamperes)
    fan: Option<PathBuf>,         // fanN_input   (RPM; 0xFFFFFFFF = no fan)
}
```

Rewrite `discover_sensor_paths` to be label-aware. For each sensor class, scan all candidate indices; if any candidate's `*_label` matches the preferred label for that class (`asic_temp`, `vcore`, `power`, `current`, `fan_rpm`), pick it; otherwise fall back to the lowest-numbered existing file (previous behavior, covers older drivers without labels):

```rust
    /// Pick the input file for one sensor class: prefer the candidate whose
    /// `_label` file matches `preferred_label` (tt-kmd labels its sensors, e.g.
    /// temp1_label=asic_temp), else the lowest-numbered existing input file.
    fn pick_sensor(
        hwmon_path: &Path,
        prefix: &str,          // "temp" | "in" | "power" | "curr" | "fan"
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

    fn discover_sensor_paths(hwmon_path: &Path) -> SensorPaths {
        SensorPaths {
            temperature: Self::pick_sensor(hwmon_path, "temp", 1..=8, "asic_temp"),
            voltage: Self::pick_sensor(hwmon_path, "in", 0..=8, "vcore"),
            power: Self::pick_sensor(hwmon_path, "power", 1..=8, "power"),
            current: Self::pick_sensor(hwmon_path, "curr", 1..=8, "current"),
            fan: Self::pick_sensor(hwmon_path, "fan", 1..=4, "fan_rpm"),
        }
    }
```

Add limit reading + a shared u64 reader (used again in Tasks 2–4):

```rust
    /// Read a whole-number sysfs attribute file (plain decimal, e.g. "125000000").
    fn read_u64_file(path: &Path) -> Option<u64> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
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
```

In `detect_devices()`, after constructing each `Device` (before `self.devices.push(device)`), populate limits:
```rust
                    let mut device = Device { /* existing literal unchanged */ };
                    // Real firmware limits from hwmon *_max files (kmd ≥ 2.9.0).
                    // HybridBackend later overwrites with the richer JSON limits
                    // block when tt-smi is available; this covers sysfs-only mode.
                    device.limits = Self::read_hwmon_limits(&path);
```
(Change `let device = Device {...}` to `let mut device = ...`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features tui backend::sysfs`
Expected: PASS (all new + existing).

- [ ] **Step 5: Commit**

```bash
git add src/backend/sysfs.rs
git commit -m "feat(sysfs): label-aware hwmon discovery, fan sensor, real *_max limits"
```

---

### Task 2: Sysfs — tenstorrent class dir discovery + static device enrichment (card type, serial, FW bundle)

**Files:**
- Modify: `src/backend/sysfs.rs`
- Test: same file

**Interfaces:**
- Consumes: `read_u64_file` from Task 1.
- Produces: `fn find_tt_class_dir(hwmon_path: &Path) -> Option<PathBuf>`; `fn read_string_attr(dir: &Path, name: &str) -> Option<String>`; field `tt_class_dirs: HashMap<usize, PathBuf>` on `SysfsBackend`. Tasks 3–5 consume `tt_class_dirs`.

- [ ] **Step 1: Write failing tests:**

```rust
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
```

- [ ] **Step 2: Run to verify FAIL** — `cargo test --features tui backend::sysfs` → unresolved names.

- [ ] **Step 3: Implement.**

```rust
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
```

Add field to `SysfsBackend` struct + clear it in `detect_devices()` alongside the other `.clear()` calls:
```rust
    /// tt-kmd class-attribute dir per device (tt_aiclk, tt_heartbeat,
    /// pcie_perf_counters/, …). Absent on kmd < 2.7.
    tt_class_dirs: HashMap<usize, PathBuf>,
```
(init in both constructors: `tt_class_dirs: HashMap::new(),`)

In `init()` (after the `discover_sensor_paths` loop), discover class dirs and enrich devices; skip the tt-smi subprocess probe entirely when every device got a card type from sysfs:

```rust
        // Discover the tt-kmd class-attribute dirs and use their static attrs
        // for device enrichment (no subprocess needed).
        let mut all_have_card_type = !self.devices.is_empty();
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
```
**Ordering note:** this block REPLACES the existing unconditional `self.try_enrich_board_types_from_tt_smi();` call in `init()` — move that call into the `if !all_have_card_type` arm as shown. This removes the 1.2 s startup probe on modern kmd.

Borrow-checker note: the loop iterates `&mut self.devices` while reading `self.hwmon_paths` and inserting into `self.tt_class_dirs`. Rust allows disjoint field borrows within a method only if accessed through `self` directly per statement — if the compiler objects, restructure as: first collect `Vec<(usize, PathBuf)>` of `(index, tt_dir)` from `self.hwmon_paths`, then loop over `&mut self.devices`.

- [ ] **Step 4: Run tests** — `cargo test --features tui backend::sysfs` → PASS. Also run the full suite once (`cargo test --features tui`) since `init()` behavior changed.

- [ ] **Step 5: Manual verification on this box** (safe, read-only):
```bash
cargo run --bin tt-toplike-tui --features tui -- --backend sysfs --print 2>/dev/null | head -30
```
Expected: devices report `board_type: p300c` (not "blackhole"), and startup does NOT log "enriched board types from tt-smi".

- [ ] **Step 6: Commit**

```bash
git add src/backend/sysfs.rs
git commit -m "feat(sysfs): read tt-kmd class attrs — card type, serial, FW bundle (drops tt-smi probe on modern kmd)"
```

---

### Task 3: Sysfs — per-tick dynamic tt_* reads + synthesized SmbusTelemetry (aiclk, heartbeat, fan, clocks, therm trips)

**Files:**
- Modify: `src/backend/sysfs.rs` (update() :341-376, smbus_telemetry() :390-393, struct fields)
- Test: same file

**Interfaces:**
- Consumes: `tt_class_dirs`, `SensorPaths.fan`, `read_u64_file`, `read_string_attr` (Tasks 1–2).
- Produces: `SysfsBackend::smbus_telemetry()` now returns `Some(&SmbusTelemetry)` when any synthesized field is present; `Telemetry.aiclk`/`heartbeat` populated. New struct field `smbus_cache: HashMap<usize, SmbusTelemetry>`.

- [ ] **Step 1: Write failing test** — a pure function test for the synthesis, plus an update-path test using the fakes from Tasks 1–2:

```rust
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
```

- [ ] **Step 2: Run to verify FAIL.**

- [ ] **Step 3: Implement.**

Add struct field (+ constructors + clear in detect_devices):
```rust
    /// SMBUS-shaped telemetry synthesized from tt-kmd sysfs attrs (partial —
    /// only the fields the driver exposes). Serves the Insights fan/clock rows
    /// in sysfs-only mode; HybridBackend ignores it (it blends its own from
    /// tt-smi JSON).
    smbus_cache: HashMap<usize, SmbusTelemetry>,
```

Synthesis fn:
```rust
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
```

In `update()`, after building `telemetry` and before `self.telemetry_cache.insert(...)`:
```rust
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

            let smbus = Self::synthesize_smbus(
                tt_dir.map(|p| p.as_path()),
                paths.fan.as_deref(),
            );
            self.smbus_cache.insert(device_idx, smbus);
```

Replace `smbus_telemetry()`:
```rust
    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        // Partial SMBUS synthesized from tt-kmd sysfs attrs (kmd ≥ 2.7).
        self.smbus_cache.get(&device_idx)
    }
```
Also update the module doc comment at the top of the file (`//! # Limitations`) — "No SMBUS telemetry" is no longer accurate; describe the partial synthesis.

Borrow note: `paths` is borrowed from `self.sensor_paths` while `self.tt_class_dirs` is read and `self.smbus_cache` is written — same disjoint-field caveat as Task 2; if needed, clone the two `Option<PathBuf>`s out of `paths`/`tt_class_dirs` before the inserts.

- [ ] **Step 4: Run tests** — `cargo test --features tui backend::sysfs` then full `cargo test --features tui` → PASS.

- [ ] **Step 5: Manual verification** (read-only): run `cargo run --bin tt-toplike-tui --features tui -- --backend sysfs --print 2>/dev/null | head -40` — expect aiclk ≈ 800 and a heartbeat value on each device.

- [ ] **Step 6: Commit**

```bash
git add src/backend/sysfs.rs
git commit -m "feat(sysfs): per-tick tt_* attrs — aiclk, ARC heartbeat, fan RPM, clocks, therm trips"
```

---

### Task 4: PCIe perf counters module — pure reader + bandwidth rate tracker

**Files:**
- Create: `src/backend/pcie_counters.rs`
- Modify: `src/backend/mod.rs` (add `pub mod pcie_counters;` next to the other module decls)
- Test: inside the new file

**Interfaces:**
- Produces (all `pub`, consumed by Task 5 and the UI in Task 8):
  - `pub struct PcieCounterSnapshot { pub words_in: u64, pub words_out: u64 }`
  - `pub fn read_counters(dir: &Path) -> Option<PcieCounterSnapshot>`
  - `pub struct PcieBandwidth { pub rx_bytes_per_sec: f64, pub tx_bytes_per_sec: f64 }` (rx = into chip, tx = out of chip)
  - `pub struct PcieRateTracker` with `pub fn new() -> Self` and `pub fn sample(&mut self, snap: PcieCounterSnapshot, now: std::time::Instant) -> Option<PcieBandwidth>`
  - `pub fn format_bandwidth(bytes_per_sec: f64) -> String` (e.g. `"1.2 GB/s"`, `"340 MB/s"`, `"12 kB/s"`, `"0 B/s"`)

- [ ] **Step 1: Write the failing tests** (bottom of the new file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn fake_counters(dir: &std::path::Path, base: u64) {
        // The 12 files tt-kmd exposes; controller-1 files are usually ~0 on
        // single-controller cards but must still be summed.
        for (f, v) in [
            ("mst_nonposted_wr_data_word_sent0", base + 100),
            ("mst_nonposted_wr_data_word_sent1", 0),
            ("mst_posted_wr_data_word_sent0", base + 200),
            ("mst_posted_wr_data_word_sent1", 0),
            ("mst_rd_data_word_received0", base + 300),
            ("mst_rd_data_word_received1", 0),
            ("slv_nonposted_wr_data_word_received0", base + 400),
            ("slv_nonposted_wr_data_word_received1", 7),
            ("slv_posted_wr_data_word_received0", base + 500),
            ("slv_posted_wr_data_word_received1", 0),
            ("slv_rd_data_word_sent0", base + 600),
            ("slv_rd_data_word_sent1", 3),
        ] {
            fs::write(dir.join(f), v.to_string()).unwrap();
        }
    }

    #[test]
    fn read_counters_sums_directions() {
        let td = tempfile::tempdir().unwrap();
        fake_counters(td.path(), 1000);
        let snap = read_counters(td.path()).unwrap();
        // in  = slv posted+nonposted wr received + mst rd received
        //     = (1400+7) + 1500 + 1300 = 4207
        assert_eq!(snap.words_in, 4207);
        // out = slv rd sent + mst posted+nonposted wr sent
        //     = (1600+3) + 1200 + 1100 = 3903
        assert_eq!(snap.words_out, 3903);
    }

    #[test]
    fn read_counters_missing_dir_is_none() {
        assert!(read_counters(std::path::Path::new("/nonexistent-xyz")).is_none());
    }

    #[test]
    fn rate_tracker_computes_bytes_per_sec() {
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        // First sample only primes the tracker.
        assert!(tracker
            .sample(PcieCounterSnapshot { words_in: 1000, words_out: 500 }, t0)
            .is_none());
        // 2 s later: +2000 words in, +1000 words out → ×4 bytes / 2 s.
        let bw = tracker
            .sample(
                PcieCounterSnapshot { words_in: 3000, words_out: 1500 },
                t0 + Duration::from_secs(2),
            )
            .unwrap();
        assert!((bw.rx_bytes_per_sec - 4000.0).abs() < 1e-6);
        assert!((bw.tx_bytes_per_sec - 2000.0).abs() < 1e-6);
    }

    #[test]
    fn rate_tracker_counter_reset_yields_zero_not_negative() {
        let mut tracker = PcieRateTracker::new();
        let t0 = Instant::now();
        tracker.sample(PcieCounterSnapshot { words_in: 5000, words_out: 5000 }, t0);
        // Counters went backwards (device reset) → clamp to 0, don't underflow.
        let bw = tracker
            .sample(
                PcieCounterSnapshot { words_in: 10, words_out: 10 },
                t0 + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(bw.rx_bytes_per_sec, 0.0);
        assert_eq!(bw.tx_bytes_per_sec, 0.0);
    }

    #[test]
    fn format_bandwidth_picks_sane_units() {
        assert_eq!(format_bandwidth(0.0), "0 B/s");
        assert_eq!(format_bandwidth(950.0), "950 B/s");
        assert_eq!(format_bandwidth(12_300.0), "12.3 kB/s");
        assert_eq!(format_bandwidth(340_000_000.0), "340 MB/s");
        assert_eq!(format_bandwidth(1_230_000_000.0), "1.23 GB/s");
    }
}
```

- [ ] **Step 2: Run to verify FAIL** — `cargo test --features tui pcie_counters` → module doesn't exist.

- [ ] **Step 3: Implement** (`src/backend/pcie_counters.rs`):

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! PCIe data-word counters from tt-kmd's sysfs class attributes.
//!
//! tt-kmd exposes `/sys/class/tenstorrent/tenstorrent!N/pcie_perf_counters/`
//! — 12 world-readable, monotonically increasing counters of 32-bit data
//! words crossing the PCIe link, split by direction and initiator:
//!
//! * `mst_*` — the DEVICE is the PCIe master (device-initiated DMA):
//!   `mst_rd_data_word_received*` = device reading host memory (→ into chip),
//!   `mst_{posted,nonposted}_wr_data_word_sent*` = device writing host memory
//!   (→ out of chip).
//! * `slv_*` — the device is the PCIe target (host-initiated MMIO/DMA):
//!   `slv_{posted,nonposted}_wr_data_word_received*` = host writing to the
//!   device (→ into chip), `slv_rd_data_word_sent*` = host reading from the
//!   device (→ out of chip).
//!
//! Each name has a `0` and `1` suffix (two PCIe controllers); both are summed.
//! Reading these files is passive — no device open, no ARC message.

use std::fs;
use std::path::Path;
use std::time::Instant;

/// Bytes per 32-bit PCIe data word.
const BYTES_PER_WORD: u64 = 4;

/// Raw cumulative word counts, folded down to the two directions we render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieCounterSnapshot {
    /// Words that entered the chip (host writes + device reads of host memory).
    pub words_in: u64,
    /// Words that left the chip (host reads + device writes to host memory).
    pub words_out: u64,
}

/// Derived link bandwidth between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PcieBandwidth {
    /// Bytes/second flowing INTO the chip.
    pub rx_bytes_per_sec: f64,
    /// Bytes/second flowing OUT of the chip.
    pub tx_bytes_per_sec: f64,
}

/// Counter files whose sum is "data into the chip".
const IN_COUNTERS: [&str; 6] = [
    "slv_posted_wr_data_word_received0",
    "slv_posted_wr_data_word_received1",
    "slv_nonposted_wr_data_word_received0",
    "slv_nonposted_wr_data_word_received1",
    "mst_rd_data_word_received0",
    "mst_rd_data_word_received1",
];

/// Counter files whose sum is "data out of the chip".
const OUT_COUNTERS: [&str; 6] = [
    "slv_rd_data_word_sent0",
    "slv_rd_data_word_sent1",
    "mst_posted_wr_data_word_sent0",
    "mst_posted_wr_data_word_sent1",
    "mst_nonposted_wr_data_word_sent0",
    "mst_nonposted_wr_data_word_sent1",
];

/// Read and fold the counter directory. `None` if the directory (or every
/// file in it) is unreadable — i.e. tt-kmd < 2.x without the attribute group.
/// Individual missing files count as 0 so a partial set still reports.
pub fn read_counters(dir: &Path) -> Option<PcieCounterSnapshot> {
    if !dir.is_dir() {
        return None;
    }
    let sum = |names: &[&str]| -> u64 {
        names
            .iter()
            .filter_map(|n| {
                fs::read_to_string(dir.join(n))
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
            })
            .sum()
    };
    Some(PcieCounterSnapshot {
        words_in: sum(&IN_COUNTERS),
        words_out: sum(&OUT_COUNTERS),
    })
}

/// Turns successive snapshots into bytes/sec. The first sample primes the
/// tracker and yields `None`; a counter that goes backwards (device reset)
/// clamps its delta to 0 rather than underflowing.
#[derive(Debug, Default)]
pub struct PcieRateTracker {
    prev: Option<(PcieCounterSnapshot, Instant)>,
}

impl PcieRateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self, snap: PcieCounterSnapshot, now: Instant) -> Option<PcieBandwidth> {
        let result = self.prev.and_then(|(prev, prev_t)| {
            let dt = now.duration_since(prev_t).as_secs_f64();
            if dt <= 0.0 {
                return None;
            }
            let d_in = snap.words_in.saturating_sub(prev.words_in);
            let d_out = snap.words_out.saturating_sub(prev.words_out);
            Some(PcieBandwidth {
                rx_bytes_per_sec: (d_in * BYTES_PER_WORD) as f64 / dt,
                tx_bytes_per_sec: (d_out * BYTES_PER_WORD) as f64 / dt,
            })
        });
        self.prev = Some((snap, now));
        result
    }
}

/// Human-readable rate with SI decimal units, ≤ 3 significant digits.
pub fn format_bandwidth(bytes_per_sec: f64) -> String {
    let b = bytes_per_sec.max(0.0);
    if b < 1000.0 {
        format!("{:.0} B/s", b)
    } else if b < 1_000_000.0 {
        format!("{:.1} kB/s", b / 1000.0)
    } else if b < 1_000_000_000.0 {
        format!("{:.0} MB/s", b / 1_000_000.0)
    } else {
        format!("{:.2} GB/s", b / 1_000_000_000.0)
    }
}
```
Adjust `format_bandwidth` decimals to exactly satisfy the test expectations ("12.3 kB/s", "340 MB/s", "1.23 GB/s").

- [ ] **Step 4: Run tests** — `cargo test --features tui pcie_counters` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/pcie_counters.rs src/backend/mod.rs
git commit -m "feat(pcie): read tt-kmd pcie_perf_counters and derive per-device link bandwidth"
```

---

### Task 5: Wire PCIe bandwidth into SysfsBackend + trait method + Hybrid delegation

**Files:**
- Modify: `src/backend/mod.rs` (TelemetryBackend trait — add defaulted method; also add it to the `Box<dyn TelemetryBackend>` blanket impl if that impl forwards methods explicitly)
- Modify: `src/backend/sysfs.rs` (update() computes bandwidth; store trackers)
- Modify: `src/backend/hybrid.rs` (delegate to inner sysfs)
- Test: `src/backend/sysfs.rs`

**Interfaces:**
- Consumes: `pcie_counters::{read_counters, PcieRateTracker, PcieBandwidth}` (Task 4), `tt_class_dirs` (Task 2).
- Produces: trait method `fn pcie_bandwidth(&self, _device_idx: usize) -> Option<crate::backend::pcie_counters::PcieBandwidth> { None }` — the UI (Task 8) calls `backend.pcie_bandwidth(idx)`.

- [ ] **Step 1: Add the trait method** (in `src/backend/mod.rs`, inside `pub trait TelemetryBackend`):

```rust
    /// Instantaneous PCIe link bandwidth for one device, if the backend can
    /// measure it. Only the sysfs/hybrid path (tt-kmd `pcie_perf_counters/`)
    /// implements this; every other backend inherits the `None` default.
    fn pcie_bandwidth(&self, _device_idx: usize) -> Option<crate::backend::pcie_counters::PcieBandwidth> {
        None
    }
```
Check the `impl TelemetryBackend for Box<dyn TelemetryBackend>` blanket impl in the same file: if it forwards each method explicitly (it does — Phase 13), add a forwarding arm:
```rust
    fn pcie_bandwidth(&self, device_idx: usize) -> Option<crate::backend::pcie_counters::PcieBandwidth> {
        (**self).pcie_bandwidth(device_idx)
    }
```

- [ ] **Step 2: Write failing test** (sysfs.rs tests; uses fakes from Tasks 1–2 + 4):

```rust
    #[test]
    fn pcie_bandwidth_none_without_counter_dir() {
        let backend = SysfsBackend::new();
        assert!(backend.pcie_bandwidth(0).is_none());
    }
```
(The rate math itself is already covered by Task 4's unit tests; here we only verify the plumbing default. Full-path coverage happens in the manual step.)

- [ ] **Step 3: Implement in SysfsBackend.** Add fields (+ constructors + clear in `detect_devices()`):

```rust
    /// Per-device PCIe counter rate trackers (needs two samples to produce a rate).
    pcie_trackers: HashMap<usize, crate::backend::pcie_counters::PcieRateTracker>,

    /// Last computed per-device PCIe bandwidth.
    pcie_bandwidth: HashMap<usize, crate::backend::pcie_counters::PcieBandwidth>,
```

At the end of the per-device loop body in `update()`:
```rust
            // PCIe link bandwidth from the kmd counter files (passive reads).
            if let Some(tt_dir) = self.tt_class_dirs.get(&device_idx) {
                let counter_dir = tt_dir.join("pcie_perf_counters");
                if let Some(snap) = crate::backend::pcie_counters::read_counters(&counter_dir) {
                    let tracker = self.pcie_trackers.entry(device_idx).or_default();
                    if let Some(bw) = tracker.sample(snap, std::time::Instant::now()) {
                        self.pcie_bandwidth.insert(device_idx, bw);
                    }
                }
            }
```
And the accessor:
```rust
    fn pcie_bandwidth(&self, device_idx: usize) -> Option<crate::backend::pcie_counters::PcieBandwidth> {
        self.pcie_bandwidth.get(&device_idx).copied()
    }
```

In `hybrid.rs` (`impl TelemetryBackend for HybridBackend`):
```rust
    fn pcie_bandwidth(&self, device_idx: usize) -> Option<crate::backend::pcie_counters::PcieBandwidth> {
        self.sysfs.pcie_bandwidth(device_idx)
    }
```

- [ ] **Step 4: Build + test** — `cargo test --features tui` → PASS, zero new warnings.

- [ ] **Step 5: Manual verification**: `cargo run --bin tt-toplike-tui --features tui -- --backend sysfs --print` twice a few seconds apart, or temporarily log — simplest: run the TUI after Task 8 lands the row. For now assert compile + tests.

- [ ] **Step 6: Commit**

```bash
git add src/backend/mod.rs src/backend/sysfs.rs src/backend/hybrid.rs
git commit -m "feat(pcie): expose per-device PCIe bandwidth through the backend trait (sysfs + hybrid)"
```

---

### Task 6: JSON backend — parse full board_info (pcie speed/width, dram) + FAN_RPM fallback

**Files:**
- Modify: `src/backend/json.rs` (BoardInfoJSON :89-95, parse_json_devices :463-523, device_from_json :528-556, TTSMIDeviceJSON :100-123, smbus_from_json_fields :722)
- Test: same file (existing test JSON fixtures around :900-1100 show the pattern)

**Interfaces:**
- Consumes: `de_opt_u32_str` (existing, json.rs:61).
- Produces: `Device.pcie_speed: Option<String>` (e.g. `"Gen4"`), `Device.pcie_width: Option<u8>` populated from snapshots; `SmbusTelemetry.fan_speed` falls back to FAN_RPM. Task 8's UI reads `device.pcie_speed`/`pcie_width`.

- [ ] **Step 1: Write failing tests:**

```rust
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
```

- [ ] **Step 2: Run to verify FAIL** — `cargo test --features tui backend::json`.

- [ ] **Step 3: Implement.**

Extend `BoardInfoJSON`:
```rust
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
```

In `parse_json_devices`'s device_info mapping closure, thread the two new values through `TTSMIDeviceJSON`. Add fields to `TTSMIDeviceJSON`:
```rust
    /// PCIe generation from board_info (tt-smi 5.x).
    pub pcie_speed: Option<u32>,
    /// PCIe lane width from board_info (tt-smi 5.x).
    pub pcie_width: Option<u32>,
```
and in the closure:
```rust
                        pcie_speed: board_info.and_then(|b| b.pcie_speed),
                        pcie_width: board_info.and_then(|b| b.pcie_width),
```
(Every other construction site of `TTSMIDeviceJSON` — the legacy-format paths deserialize it directly, so `#[serde(default)]` is needed on both new fields to keep legacy JSON parsing; add `#[serde(default)]`.)

In `device_from_json`, after `device.limits = limits;`:
```rust
    // PCIe link info (board_info, tt-smi 5.x): speed is the PCIe generation
    // number; render as "Gen4" to match tt-smi's own display.
    device.pcie_speed = json_dev.pcie_speed.map(|g| format!("Gen{}", g));
    device.pcie_width = json_dev.pcie_width.and_then(|w| u8::try_from(w).ok());
```

In `smbus_from_json_fields` change the fan mapping:
```rust
        // FAN_SPEED is the legacy percentage-or-RPM field; tt-smi ≥ 5.3.0
        // reports RPM in FAN_RPM. Prefer FAN_SPEED when present (back-compat),
        // fall back to FAN_RPM so RPM-only sources still drive the fan row.
        fan_speed: smbus_json.fan_speed.or(smbus_json.fan_rpm),
```

- [ ] **Step 4: Run tests** — `cargo test --features tui backend::json` then full suite → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/json.rs
git commit -m "feat(json): parse board_info pcie/dram fields and fall back to FAN_RPM"
```

---

### Task 7: JSON backend — tt-smi 6.x `processes[]` + `telemetry.board_power`

**Files:**
- Modify: `src/backend/json.rs` (TTSMISnapshot :465-468, TelemetryJSON :129-143, telemetry_from_json :559-569, ParsedSnapshot :576-583, parsed_from_json_devices, JSONBackend struct + merge_parsed)
- Modify: `src/models/telemetry.rs` (Telemetry struct :17-40 + `new()` :44-54)
- Modify: `src/backend/mod.rs` (trait: defaulted `device_processes()`; Box blanket impl forwarding)
- Modify: every `Telemetry { … }` literal the compiler flags (~17 sites: mock.rs, sysfs.rs, luwen.rs, hybrid.rs, host.rs, smbus_smooth.rs, chip_portrait.rs, tests) — add `board_power: None` (mock may synthesize a value).
- Test: `src/backend/json.rs`

**Interfaces:**
- Produces:
  - `pub struct DeviceProcess { pub pid: Option<i64>, pub user: Option<String>, pub device: Option<usize>, pub cmdline: Option<String> }` in `src/models/telemetry.rs` (exported via `src/models/mod.rs` — add to its `pub use telemetry::{...}` list).
  - `Telemetry.board_power: Option<f32>` — total board input power (p300 = both asics + peripherals), vs `power` which is the per-asic TDP measurement.
  - Trait method `fn device_processes(&self) -> &[crate::models::DeviceProcess] { &[] }`.
  - `ParsedSnapshot.processes: Vec<DeviceProcess>`.

- [ ] **Step 1: Write failing tests:**

```rust
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
        assert_eq!(parsed.processes[0].cmdline.as_deref(), Some("python -m vllm serve"));
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
```

- [ ] **Step 2: Run to verify FAIL.**

- [ ] **Step 3: Implement.**

`src/models/telemetry.rs` — add after the `Telemetry` impl block:
```rust
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
```
Add to `Telemetry`:
```rust
    /// Total board input power in watts (tt-smi ≥ 6.0.0 `board_power`).
    /// Distinct from `power`: on dual-asic boards (p300) `power` is this
    /// asic's TDP measurement while `board_power` covers the whole card.
    pub board_power: Option<f32>,
```
and `board_power: None,` in `Telemetry::new()`. Export `DeviceProcess` from `src/models/mod.rs`.

`src/backend/json.rs`:
- `TelemetryJSON` += `#[serde(default, deserialize_with = "de_opt_f32_str")] pub board_power: Option<f32>,`; `telemetry_from_json` maps it.
- `TTSMISnapshot` += `processes: Option<Vec<crate::models::DeviceProcess>>` — note: this struct is local to `parse_json_devices`, which returns only devices. Refactor: move the `TTSMISnapshot` struct + first-branch parse up into `parse_tt_smi_snapshot`, OR (simpler, less churn) change `parse_json_devices` to return `(Vec<TTSMIDeviceJSON>, Vec<DeviceProcess>)`. Take the simpler route:
  - `fn parse_json_devices(json_str) -> BackendResult<(Vec<TTSMIDeviceJSON>, Vec<crate::models::DeviceProcess>)>` — the modern-snapshot branch returns `snapshot.processes.unwrap_or_default()`, every legacy branch returns `Vec::new()`.
  - `parse_tt_smi_snapshot` threads it into `ParsedSnapshot { processes, .. }`.
  - `parse_snapshot` (hybrid's helper, :830) destructures and ignores processes: `let (devices, _processes) = ...`.
- `ParsedSnapshot` += `pub processes: Vec<crate::models::DeviceProcess>`.
- `JSONBackend` += field `processes: Vec<crate::models::DeviceProcess>` (init `Vec::new()`); `merge_parsed` stores it; trait impl:
```rust
    fn device_processes(&self) -> &[crate::models::DeviceProcess] {
        &self.processes
    }
```

`src/backend/mod.rs` trait:
```rust
    /// Per-device process attribution from the telemetry source, when the
    /// source provides it (tt-smi ≥ 6.0.0 snapshot `processes[]`). Backends
    /// without process data inherit the empty default.
    fn device_processes(&self) -> &[crate::models::DeviceProcess] {
        &[]
    }
```
plus Box-forwarding arm. `WsBackend` (src/backend/ws.rs) also consumes `ParsedSnapshot` — store + serve `processes` there the same way if its merge path is shared; if it constructs state field-by-field, add the same one-line field + accessor.

- Fix every `Telemetry { … }` literal the compiler flags: add `board_power: None,` (in `mock.rs`, give one device a synthetic `board_power: Some(power * 1.3)` so the field is exercised in mock mode — mock literal sites will make this obvious).

- [ ] **Step 4: Run** `cargo build --bin tt-toplike-tui --features tui` repeatedly, fixing each flagged literal, then `cargo test --features tui` → PASS.

- [ ] **Step 5: Commit**

```bash
git add -A src/
git commit -m "feat(json): parse tt-smi 6.x processes[] and telemetry.board_power"
```

---

### Task 8: Insights sidebar — PCIe row, Current row, GDDR ECC row, thermal-trips row, board power

**Files:**
- Modify: `src/ui/tui/mod.rs` (full-mode stat rows; insert after the Fan row block ending :5762, before the Firmware row block starting :5764)
- Test: `cargo test --features tui` (rendering helpers only; rows follow existing untested-row precedent — no new unit test file, but keep the pure formatting in `pcie_counters::format_bandwidth`, already tested in Task 4)

**Interfaces:**
- Consumes: `backend.pcie_bandwidth(idx)` (Task 5), `device.pcie_speed`/`pcie_width` (Task 6), `telemetry.board_power` (Task 7), `smbus.gddr_corr_errs`/`gddr_uncorr_errs`/`therm_trip_count` (already parsed), `device.limits.tdc_limit` (already parsed), `crate::backend::pcie_counters::format_bandwidth`.
- The enclosing function already has in scope: `backend`, `idx`, `device: &Device`, `telemetry: Option<&Telemetry>`, `smbus: Option<&SmbusTelemetry>`, `stat_lines: Vec<Line>`, `compact: bool`. Full-mode label column is `format!("{:<8}", "Label")` in DarkGray; values White; content width is 30.

- [ ] **Step 1: Implement the rows** (all full-mode only — `if !compact`), inserted between the Fan row and Firmware row:

```rust
        // Current row (full mode only): firmware-measured amps vs. the TDC
        // ceiling from the limits block / hwmon curr1_max.
        if !compact {
            if let Some(amps) = telemetry.and_then(|t| t.current) {
                let tdc = device.limits.as_ref().and_then(|l| l.tdc_limit);
                let lim_txt = tdc
                    .map(|a| format!(" (lim {:.0}A)", a))
                    .unwrap_or_default();
                stat_lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:<8}", "Current"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{:.0}A", amps), Style::default().fg(Color::White)),
                    Span::styled(lim_txt, Style::default().fg(Color::DarkGray)),
                ]));
            }
        }

        // Board power row (full mode only): whole-card input power (tt-smi ≥ 6,
        // dual-asic p300 boards) — only shown when it differs from asic power.
        if !compact {
            if let Some(bw) = telemetry.and_then(|t| t.board_power) {
                stat_lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:<8}", "Board"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{:.0}W", bw), Style::default().fg(Color::White)),
                    Span::styled(" total", Style::default().fg(Color::DarkGray)),
                ]));
            }
        }

        // PCIe row (full mode only): link geometry from board_info plus live
        // bandwidth from the kmd counters (sysfs/hybrid backends only).
        if !compact {
            let link = match (device.pcie_speed.as_deref(), device.pcie_width) {
                (Some(gen), Some(w)) => Some(format!("{} x{}", gen, w)),
                (Some(gen), None) => Some(gen.to_string()),
                _ => None,
            };
            let bw = backend.pcie_bandwidth(idx);
            if link.is_some() || bw.is_some() {
                let mut spans = vec![Span::styled(
                    format!("{:<8}", "PCIe"),
                    Style::default().fg(Color::DarkGray),
                )];
                if let Some(l) = link {
                    spans.push(Span::styled(l, Style::default().fg(Color::White)));
                }
                if let Some(bw) = bw {
                    use crate::backend::pcie_counters::format_bandwidth;
                    spans.push(Span::styled(
                        format!(
                            " ▼{} ▲{}",
                            format_bandwidth(bw.rx_bytes_per_sec),
                            format_bandwidth(bw.tx_bytes_per_sec)
                        ),
                        Style::default().fg(Color::Rgb(79, 209, 197)),
                    ));
                }
                stat_lines.push(Line::from(spans));
            }
        }

        // GDDR ECC row (full mode only): shown only when any error counter is
        // non-zero — uncorrectable errors are the headline diagnostic and go red.
        if !compact {
            if let Some(s) = smbus {
                let corr: u32 = s.gddr_corr_errs.iter().flatten().sum();
                let uncorr = s.gddr_uncorr_errs.unwrap_or(0);
                if corr > 0 || uncorr > 0 {
                    let color = if uncorr > 0 {
                        Color::Rgb(255, 80, 80) // uncorrectable — data corruption risk
                    } else {
                        Color::Rgb(244, 196, 113) // correctable only — watch it
                    };
                    stat_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:<8}", "ECC"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{} uncorr · {} corr", uncorr, corr),
                            Style::default().fg(color),
                        ),
                    ]));
                }
            }
        }

        // Thermal-trip row (full mode only): lifetime trip counter — any
        // non-zero value means the card hit hardware thermal shutdown at least
        // once and deserves attention.
        if !compact {
            if let Some(trips) = smbus
                .and_then(|s| s.therm_trip_count.as_deref())
                .and_then(crate::models::telemetry::parse_hex_or_dec)
            {
                if trips > 0 {
                    stat_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:<8}", "Trips"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{} thermal", trips),
                            Style::default().fg(Color::Rgb(255, 80, 80)),
                        ),
                    ]));
                }
            }
        }
```
Note: `parse_hex_or_dec` is `pub(crate)` in `crate::models::telemetry` — the call above works from within the crate. If the sidebar area is tight vertically, these rows only appear when data exists (Current is the only always-on addition when telemetry has amps), matching the sidebar's existing conditional-row style.

- [ ] **Step 2: Build + full test suite** — `cargo build --bin tt-toplike-tui --features tui && cargo test --features tui` → PASS, zero new warnings.

- [ ] **Step 3: Manual smoke on this box** (safe — sysfs/hybrid only): run `cargo run --bin tt-toplike-tui --features tui` in a tmux pane, open Insights, verify: Current row shows ~20-30A with "(lim 500A)", PCIe row shows live ▼/▲ rates that move when a workload runs, no ECC/Trips rows (counters are 0 here — correct), fan row still absent (fanless sentinel). Quit cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(insights): PCIe link+bandwidth, current, board power, GDDR ECC, thermal-trip rows"
```

---

### Task 9: Metrics scrapers — accept both vLLM KV-cache metric names

**Files:**
- Modify: `src/workload/inference_server/metrics.rs:73` and `src/workload/serving.rs:480`
- Test: `src/workload/inference_server/metrics.rs` (existing test fixture at :496 uses `vllm:kv_cache_usage_perc`)

**Interfaces:** none new — both scrapers set the field they already set.

- [ ] **Step 1: Write failing tests** (one per scraper, next to each file's existing parser tests):

metrics.rs:
```rust
    #[test]
    fn kv_cache_accepts_both_metric_names() {
        // vLLM renamed gpu_cache_usage_perc → kv_cache_usage_perc; TT builds
        // in the field have shipped both. Accept either.
        let old_name = "vllm:gpu_cache_usage_perc{engine=\"0\"} 0.42\n";
        let stats = parse_vllm_metrics(old_name);
        assert_eq!(stats.kv_cache_usage, Some(0.42));
    }
```
(Adapt the exact constructor/return-field names to the file's existing test helpers at :480-520 — the existing fixture test shows the calling convention; keep it identical.)

serving.rs:
```rust
    #[test]
    fn kv_cache_accepts_new_metric_name() {
        let mut m = ServingMetrics::default();
        // parse path exercised through the same line-matching helper
        let line = "vllm:kv_cache_usage_perc{engine=\"0\"} 0.37";
        assert_eq!(parse_metric_line(line, "vllm:kv_cache_usage_perc"), Some(0.37));
    }
```
(If `ServingMetrics::default()` isn't a thing, drop that line — the assertion on `parse_metric_line` is the real check, plus the branch change below.)

- [ ] **Step 2: Run to verify FAIL** (metrics.rs one fails; serving's parse_metric_line check passes but the branch fix below is what wires it).

- [ ] **Step 3: Implement.**

metrics.rs:73 — change:
```rust
        } else if is_metric(line, "vllm:kv_cache_usage_perc")
            || is_metric(line, "vllm:gpu_cache_usage_perc")
        {
```
serving.rs:480 — change:
```rust
        } else if let Some(val) = parse_metric_line(line, "vllm:gpu_cache_usage_perc")
            .or_else(|| parse_metric_line(line, "vllm:kv_cache_usage_perc"))
        {
            m.kv_cache_utilization = Some(val);
        }
```

- [ ] **Step 4: Run tests** — `cargo test --features tui workload` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workload/inference_server/metrics.rs src/workload/serving.rs
git commit -m "fix(metrics): accept both vLLM KV-cache metric names in both scrapers"
```

---

### Task 10: Luwen backend — migrate to official luwen-api/luwen-def 0.8.5

**Files:**
- Modify: `Cargo.toml` (:44-51 deps, :159 feature)
- Modify: `src/backend/luwen.rs` (imports :17-24, detect_devices :69-154, update :176-282)
- Modify: `src/models/telemetry.rs` (add `unpack_gddr_temps_u32` helper)
- Test: compile-only for luwen paths (`cargo build --features luwen-backend`); unit test for the new unpack helper.

**Interfaces:**
- Consumes: `luwen_api::{detect_chips_silent, ChipDetectOptions}`, `luwen_api::chip::{Chip, ChipImpl}`, `luwen_def::Arch` (verified above).
- Produces: `pub fn unpack_gddr_temps_u32(v: u32) -> GddrTempPair` in models/telemetry.rs (refactor: `unpack_gddr_temps` string version calls it).

**⚠ NEVER execute the luwen backend on this machine. Compile verification only.**

- [ ] **Step 1: Failing test for the unpack helper** (models/telemetry.rs):

```rust
    #[test]
    fn test_gddr_temp_unpack_u32() {
        // Same packing as the hex-string form: byte0 (LSB) = temps[0].
        let pair = unpack_gddr_temps_u32(0x262a2c2c);
        assert_eq!(pair.0, [44.0_f32, 44.0, 42.0, 38.0]);
    }
```
Implement by extracting the body of `unpack_gddr_temps` (:292-305):
```rust
/// Unpack a raw GDDR_X_Y_TEMP register (4 packed temperature bytes) into °C.
pub fn unpack_gddr_temps_u32(v: u32) -> GddrTempPair {
    GddrTempPair([
        (v & 0xFF) as f32,
        ((v >> 8) & 0xFF) as f32,
        ((v >> 16) & 0xFF) as f32,
        ((v >> 24) & 0xFF) as f32,
    ])
}
```
and have the string version end with `Some(unpack_gddr_temps_u32(v))`. Run the telemetry tests → PASS.

- [ ] **Step 2: Cargo.toml swap.** Replace the three `all-smi-*` lines (:49-51) with:

```toml
# Official Tenstorrent luwen crates (crates.io, published from tenstorrent/luwen).
# Replaces the abandoned all-smi-luwen-* forks (stale since 2025-07: missing
# Blackhole telemetry tags and the negative 16.16-FP temperature fix).
# NOTE (kmd ≥ 2.9/2.10): holding /dev/tenstorrent/N open participates in
# driver-managed power-state aggregation and blocks O_EXCL openers (tt-flash);
# one more reason luwen stays explicit-only and out of auto-detect.
luwen-api = { version = "0.8.5", optional = true }
luwen-def = { version = "0.8.5", optional = true }
```
and the feature (:159):
```toml
luwen-backend = ["dep:luwen-api", "dep:luwen-def"]   # Direct hardware access (EXPLICIT ONLY)
```

- [ ] **Step 3: Rewrite `src/backend/luwen.rs`.** Imports:

```rust
#[cfg(feature = "luwen-backend")]
use luwen_api::chip::{Chip, ChipImpl};
#[cfg(feature = "luwen-backend")]
use luwen_api::ChipDetectOptions;
#[cfg(feature = "luwen-backend")]
use luwen_def::Arch;
```

`detect_devices` — official `detect_chips_silent` returns initialized `Chip`s (no UninitChip/init-callback dance):
```rust
        let options = ChipDetectOptions {
            local_only: true,          // Only detect locally attached devices
            noc_safe: true,            // Use safer NoC access (won't interfere with workloads)
            continue_on_failure: true, // Try all devices even if some fail
            ..Default::default()
        };

        log::info!("LuwenBackend: Trying noc_safe mode (non-invasive for active workloads)");
        let chips = luwen_api::detect_chips_silent(Vec::new(), options)
            .map_err(|e| BackendError::Initialization(format!("Device detection failed: {}", e)))?;

        if chips.is_empty() {
            return Err(BackendError::Initialization("No devices found".to_string()));
        }

        for (idx, chip) in chips.into_iter().enumerate() {
            let arch = chip.get_arch();
            let architecture = match arch {
                Arch::Grayskull => Architecture::Grayskull,
                Arch::Wormhole => Architecture::Wormhole,
                Arch::Blackhole => Architecture::Blackhole,
            };
            // (…existing bus_id / board_type / Device construction unchanged,
            //  except `format!("{:?}", arch)` still works — Arch: Debug.)
```
If `Arch` is `#[non_exhaustive]` the compiler will demand a wildcard — add `_ => Architecture::Unknown,` only if it does.

`update()` telemetry mapping — core struct via the accessor methods (they handle 16.16 fixed-point and unit scaling):
```rust
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
                            board_power: None, // not in luwen Telemetry
                        };
```
SMBUS mapping: keep every existing field line (the official struct has the same field names the fork had — `board_id: u64`, `ddr_status`, `arc0_health`, etc. — so the `.to_string()` lines carry over), and REPLACE the trailing "not available" block (:248-255) with the now-available fields:
```rust
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
```
with `use crate::models::telemetry::unpack_gddr_temps_u32;` added under the feature gate. Type notes: fork fields that were `Option` (`ddr_speed: Option<u32>`, `aux_status: Option<u32>`) keep their `.map(|v| v.to_string())` handling; if the compiler flags a type mismatch on any field (u64 vs u32), follow the compiler — the plan's field list above is the authority for what exists.

Also update the module doc comment (:4-15) and the Cargo.toml safety comments to name the official crates.

- [ ] **Step 4: Compile-verify both ways:**

Run: `cargo build --bin tt-toplike-tui --features tui` (luwen OFF — default) → success.
Run: `cargo build --bin tt-toplike-tui --features tui,luwen-backend` → success. Fix any field/type mismatches the compiler reports against the real crate (docs.rs was the source; the compiler is ground truth).
Run: `cargo test --features tui` → PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/backend/luwen.rs src/models/telemetry.rs
git commit -m "feat(luwen): migrate to official luwen-api/luwen-def 0.8.5 with full Blackhole telemetry"
```

---

### Task 11: Version bump, changelog, dev-log, final verification

**Files:**
- Modify: version via `scripts/bump-version.sh 0.8.0` (rewrites Cargo.toml, QUICK_START.md, site/index.html, debian/changelog version token)
- Modify: `debian/changelog` (hand-edit top stanza body), `AGENTS.md` (new Phase entry), `Cargo.lock` (regenerate)

- [ ] **Step 1: Bump + lock sync** (CI runs `--locked`; a stale lock fails every job):

```bash
./scripts/bump-version.sh 0.8.0
cargo update -w   # regenerates Cargo.lock for the new version + luwen deps
```

- [ ] **Step 2: Hand-edit the `debian/changelog` top stanza** body to describe: tt-kmd class-attr telemetry (aiclk/heartbeat/card-type/fan/limits without tt-smi), live PCIe bandwidth rows, tt-smi 6.x processes[]/board_power parsing, new Insights diagnostics rows (ECC/trips/current/board), KV-cache metric-name fix, luwen migration to official 0.8.5 crates.

- [ ] **Step 3: Append a Phase entry to `AGENTS.md`** ("Phase 25 — Telemetry expansion (August 2026, v0.8.0)") summarizing the four work items, the `/sys/class/tenstorrent` + `pcie_perf_counters` discovery, and the luwen fork→official migration rationale (forks 13 months stale; kmd 2.10 O_EXCL note).

- [ ] **Step 4: Full verification:**

```bash
cargo test --features tui
cargo build --release --bin tt-toplike-tui --features tui
cargo build --bin tt-toplike-tui --features tui,luwen-backend  # compile-only
cargo clippy --features tui -- -D warnings 2>/dev/null || cargo clippy --features tui  # match repo convention; don't introduce new warnings
```
Manual smoke (safe): run the release TUI on this box (default hybrid backend), check Insights shows PCIe ▼/▲ moving, Current row, correct SKU, aiclk in sysfs mode (`--backend sysfs`).

- [ ] **Step 5: Install + restore + commit:**

```bash
cp target/release/tt-toplike-tui ~/.local/bin/tt-toplike-tui
# restore the vendored-build redirect moved aside at the start:
mv .cargo/config.toml.bak .cargo/config.toml 2>/dev/null || true
git add -A
git commit -m "chore: release v0.8.0 — telemetry expansion (sysfs tt_* attrs, PCIe bandwidth, tt-smi 6.x, luwen 0.8.5)"
```

---

## Self-review notes

- Spec coverage: item 1 → Tasks 1–3; item 2 → Tasks 4–5 + row in 8; item 3 → Tasks 6–9 (board_info, processes/board_power, UI surfacing, KV-cache + FAN_RPM); item 4 → Task 10. Release hygiene → Task 11.
- Type consistency: `PcieBandwidth`/`format_bandwidth` defined Task 4, consumed 5 & 8; `DeviceProcess` defined Task 7 consumed by trait; `unpack_gddr_temps_u32` defined Task 10 Step 1 before use in Step 3; `read_u64_file`/`read_string_attr`/`tt_class_dirs` defined Tasks 1–2, consumed 3 & 5.
- Known uncertainty, handled in-plan: exact luwen-api field types (compiler is ground truth, Step 4 of Task 10); serde shape drift for pcie_speed/width (flexible deserializer); borrow-checker restructuring fallbacks documented where loops mix field borrows.
