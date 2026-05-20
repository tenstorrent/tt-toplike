# Chip Portrait + tt-smi 5.2.0 Accuracy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add GDDR/ETH/harvesting telemetry fields from tt-smi 5.2.0, a Chip Portrait widget showing BH/WH core topology with activity heat, and a new Grid display mode, all with quad-Galaxy-scale mock fixtures.

**Architecture:** New data structs (`GddrTempPair`, `FirmwaresInfo`, `DeviceLimits`) and extended `SmbusTelemetry` fields feed a new `chip_portrait.rs` module that renders an exact per-arch character-art core grid. The existing Insights screen gains a split-panel layout (portrait left, stats sidebar right); a new `DisplayMode::Grid` mode tiles all chip portraits full-screen.

**Tech Stack:** Rust, Ratatui 0.30, `unicode-width = "0.1"` (already in Cargo.toml), `cargo test --lib` for tests.

**Branch:** `feature/chip-portrait-and-accuracy` (already created)

**Baseline:** `cargo test --lib` → 103 tests, all pass. Run this before every commit.

---

### Task 1: GddrTempPair, FirmwaresInfo, DeviceLimits — new structs + parsing helpers

**Files:**
- Modify: `src/models/telemetry.rs` (append after line 251, before `fn parse_hex_or_dec`)

- [ ] **Step 1: Write failing tests**

Append to the `#[cfg(test)]` block at the bottom of `src/models/telemetry.rs` (inside `mod tests { ... }`):

```rust
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

    // ── tensix_col_harvested ──────────────────────────────────────────────────

    #[test]
    fn test_harvesting_col_all_active() {
        // 0x3FFF = all 14 bits set → no harvesting
        for col in 0..14_usize {
            assert!(!tensix_col_harvested(0x3FFF, col),
                "col {} should be active", col);
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
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "FAILED|error|unpack_gddr|tensix_col_harvested"
```

Expected: compile errors — `unpack_gddr_temps` and `tensix_col_harvested` don't exist yet.

- [ ] **Step 3: Add structs and helpers to `src/models/telemetry.rs`**

Insert after line 251 (the closing `}` of `SmbusTelemetry`'s field list, before the free `fn parse_hex_or_dec`):

```rust
/// Four temperature readings packed into one GDDR_X_Y_TEMP hex register.
/// Byte 0 (LSB) = first temp, byte 3 (MSB) = fourth temp.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GddrTempPair(pub [f32; 4]);

/// Unpack a GDDR_X_Y_TEMP hex string (e.g. "0x262a2c2c") into four °C values.
/// Bytes are extracted little-endian: byte0=LSB is temps[0].
pub fn unpack_gddr_temps(s: &str) -> Option<GddrTempPair> {
    let s = s.trim();
    if s.is_empty() || s == "N/A" { return None; }
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(GddrTempPair([
        ((v >> 0)  & 0xFF) as f32,
        ((v >> 8)  & 0xFF) as f32,
        ((v >> 16) & 0xFF) as f32,
        ((v >> 24) & 0xFF) as f32,
    ]))
}

/// Returns true if the Tensix column `col` (0-indexed among the 14 Tensix columns
/// on Blackhole) is harvested. Bit N clear → column N is harvested.
pub fn tensix_col_harvested(enabled_tensix_col: u32, col: usize) -> bool {
    if col >= 14 { return false; }
    (enabled_tensix_col >> col) & 1 == 0
}

/// Firmware bundle + component firmware versions (from tt-smi 5.2.0 `firmwares` block).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirmwaresInfo {
    pub fw_bundle_version: Option<String>,
    pub eth_fw:            Option<String>,
    pub cm_fw:             Option<String>,
    pub gddr_fw:           Option<String>,
}

/// Per-device thermal/power limits (from tt-smi 5.2.0 `limits` block).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceLimits {
    pub tdp_limit:  Option<f32>,
    pub tdc_limit:  Option<f32>,
    pub asic_fmax:  Option<u32>,
    pub thm_limit:  Option<f32>,
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "gddr_temp|harvesting_col|FAILED|ok\."
```

Expected: 6 new tests pass, total ≥ 109 pass.

- [ ] **Step 5: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/models/telemetry.rs
git commit -m "feat(models): add GddrTempPair, FirmwaresInfo, DeviceLimits + unpack helpers"
```

---

### Task 2: Extend SmbusTelemetry with new tt-smi 5.2.0 fields

**Files:**
- Modify: `src/models/telemetry.rs`

- [ ] **Step 1: Write failing tests**

Append to the `mod tests` block in `src/models/telemetry.rs`:

```rust
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
```

- [ ] **Step 2: Verify they fail**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "new_fields_default|gddr_max_temp|error"
```

Expected: compile errors — `gddr_temps` field doesn't exist yet.

- [ ] **Step 3: Add new fields to `SmbusTelemetry` struct**

In `src/models/telemetry.rs`, after the existing `eth_debug_status1` field (line ~250), add before the closing `}` of the struct:

```rust
    /// GDDR temperature pairs (4 registers × 4 bytes each = 4 pairs of temps).
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
```

- [ ] **Step 4: Update `SmbusTelemetry::new()` to initialize new fields**

In the `Self { ... }` block of `SmbusTelemetry::new()`, add after the existing field assignments (before the closing `}`):

```rust
            gddr_temps:        [None; 4],
            max_gddr_temp:     None,
            gddr_corr_errs:    [None; 4],
            gddr_uncorr_errs:  None,
            harvesting_state:  None,
            eth_live_status:   None,
            enabled_eth:       None,
            enabled_gddr:      None,
            enabled_l2cpu:     None,
            enabled_tensix_col: None,
```

- [ ] **Step 5: Add `max_gddr_temp_computed()` helper**

In the `impl SmbusTelemetry` block, after `is_arc0_healthy()`:

```rust
    /// Compute the maximum temperature across all populated GDDR temp pairs.
    pub fn max_gddr_temp_computed(&self) -> Option<f32> {
        let max = self.gddr_temps.iter()
            .filter_map(|pair| pair.as_ref())
            .flat_map(|pair| pair.0.iter().copied())
            .filter(|&t| t > 0.0)
            .fold(f32::NEG_INFINITY, f32::max);
        if max == f32::NEG_INFINITY { None } else { Some(max) }
    }
```

- [ ] **Step 6: Run tests to confirm they pass**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "new_fields_default|gddr_max_temp|FAILED|test result"
```

Expected: 2 new tests pass, all ≥ 111 pass.

- [ ] **Step 7: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/models/telemetry.rs
git commit -m "feat(models): extend SmbusTelemetry with GDDR temps, ETH live status, harvesting fields"
```

---

### Task 3: Extend Device with firmwares, limits, and PCIe fields

**Files:**
- Modify: `src/models/device.rs`

- [ ] **Step 1: Write failing tests**

Append to `mod tests` in `src/models/device.rs`:

```rust
    #[test]
    fn test_device_new_fields_default_none() {
        let d = Device::new(0, "p150a".to_string(), "0000:01:00.0".to_string(), "(0,0)".to_string());
        assert!(d.firmwares.is_none());
        assert!(d.limits.is_none());
        assert!(d.pcie_speed.is_none());
        assert!(d.pcie_width.is_none());
    }
```

- [ ] **Step 2: Verify failure**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "device_new_fields|error"
```

- [ ] **Step 3: Add fields to `Device` struct and update `new()`**

In `src/models/device.rs`, add to the `Device` struct after `coords: String,`:

```rust
    /// Firmware versions from tt-smi `firmwares` block (None on older tt-smi).
    pub firmwares: Option<crate::models::telemetry::FirmwaresInfo>,

    /// Thermal/power limits from tt-smi `limits` block (None on older tt-smi).
    pub limits: Option<crate::models::telemetry::DeviceLimits>,

    /// PCIe link speed (e.g. "16.0 GT/s"), from board_info.
    pub pcie_speed: Option<String>,

    /// PCIe link width (e.g. 16), from board_info.
    pub pcie_width: Option<u8>,
```

Update `Device::new()` — add the new fields with `None` defaults in the returned struct:

```rust
        Device {
            index,
            board_type,
            bus_id,
            architecture,
            coords,
            firmwares: None,
            limits: None,
            pcie_speed: None,
            pcie_width: None,
        }
```

- [ ] **Step 4: Run tests**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "device_new_fields|FAILED|test result"
```

Expected: 1 new test passes, all ≥ 112 pass.

- [ ] **Step 5: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/models/device.rs
git commit -m "feat(models): add firmwares, limits, pcie_speed/width fields to Device"
```

---

### Task 4: Wire JSON backend for new tt-smi 5.2.0 fields

**Files:**
- Modify: `src/backend/json.rs`

- [ ] **Step 1: Write failing tests**

Append to `mod tests` in `src/backend/json.rs`:

```rust
    // tt-smi 5.2.0 snapshot with firmwares, limits, and new SMBUS fields.
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
        // GDDR_0_1_TEMP "0x262a2c2c" → [44, 44, 42, 38]
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
        let backend = JSONBackend::new("");
        let devices = backend.parse_json(TTSMI_52_JSON).expect("parse failed");
        // firmwares block should be stored in TTSMIDeviceRaw.firmwares
        // We verify via update_from_json → backend.devices() having firmwares set
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
    fn test_old_format_still_parses_ok() {
        // Existing pre-5.2 snapshot (no GDDR fields) must still parse without error.
        let result = parse_smbus_from_json(REAL_TTSMI_JSON);
        let smbus = result.get(&0).expect("device 0 missing");
        assert!(smbus.gddr_temps.iter().all(|t| t.is_none()),
            "old format should leave gddr_temps as None");
    }
```

- [ ] **Step 2: Verify tests fail**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "52_gddr|52_eth|52_harvest|52_firmware|52_limits|old_format|error" | head -20
```

Expected: compile errors — new SMBUS fields not yet in `SmbusTelemetryJSON`, and `update_from_json_pub` doesn't exist.

- [ ] **Step 3: Add new fields to `SmbusTelemetryJSON`**

In `src/backend/json.rs`, add to `SmbusTelemetryJSON` after `thm_limit_shutdown`:

```rust
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
```

Note: `ENABLED_TENSIX_COL` is already in `SmbusTelemetryJSON` — no duplicate needed.

- [ ] **Step 4: Add firmwares/limits fields to `TTSMIDeviceRaw`**

In `struct TTSMIDeviceRaw`, add after `smbus_telem`:

```rust
    pub firmwares: Option<serde_json::Value>,
    pub limits: Option<serde_json::Value>,
```

- [ ] **Step 5: Update `smbus_from_json_fields()` to wire new fields**

In `src/backend/json.rs`, update `smbus_from_json_fields()`. Replace the `SmbusTelemetry { ... }` literal to add:

```rust
        // Wire new GDDR temp fields
        gddr_temps: [
            smbus_json.gddr_0_1_temp.as_deref().and_then(crate::models::telemetry::unpack_gddr_temps),
            smbus_json.gddr_2_3_temp.as_deref().and_then(crate::models::telemetry::unpack_gddr_temps),
            smbus_json.gddr_4_5_temp.as_deref().and_then(crate::models::telemetry::unpack_gddr_temps),
            smbus_json.gddr_6_7_temp.as_deref().and_then(crate::models::telemetry::unpack_gddr_temps),
        ],
        max_gddr_temp: smbus_json.max_gddr_temp.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_pub)
            .map(|v| v as f32),
        gddr_corr_errs: [
            smbus_json.gddr_0_1_corr_errs.as_deref().and_then(crate::models::telemetry::parse_hex_or_dec_pub),
            smbus_json.gddr_2_3_corr_errs.as_deref().and_then(crate::models::telemetry::parse_hex_or_dec_pub),
            smbus_json.gddr_4_5_corr_errs.as_deref().and_then(crate::models::telemetry::parse_hex_or_dec_pub),
            smbus_json.gddr_6_7_corr_errs.as_deref().and_then(crate::models::telemetry::parse_hex_or_dec_pub),
        ],
        gddr_uncorr_errs: smbus_json.gddr_uncorr_errs.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_pub),
        harvesting_state: smbus_json.harvesting_state.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_pub),
        eth_live_status: smbus_json.eth_live_status.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_64_pub),
        enabled_eth: smbus_json.enabled_eth.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_pub),
        enabled_gddr: smbus_json.enabled_gddr.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_pub),
        enabled_l2cpu: smbus_json.enabled_l2cpu.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_pub),
        enabled_tensix_col: smbus_json.enabled_tensix_col.as_deref()
            .and_then(crate::models::telemetry::parse_hex_or_dec_pub),
```

- [ ] **Step 6: Make `parse_hex_or_dec` and `parse_hex_or_dec_64` pub-crate**

In `src/models/telemetry.rs`, change:

```rust
fn parse_hex_or_dec(s: &str) -> Option<u32> {
```
to:
```rust
pub(crate) fn parse_hex_or_dec_pub(s: &str) -> Option<u32> {
```

And:
```rust
fn parse_hex_or_dec_64(s: &str) -> Option<u64> {
```
to:
```rust
pub(crate) fn parse_hex_or_dec_64_pub(s: &str) -> Option<u64> {
```

Update all call sites within `SmbusTelemetry` impl (`ddr_speed_mts`, `ddr_status_bitmask`, `arc0_health_value`) to use the new names.

- [ ] **Step 7: Wire firmwares/limits into `update_from_json()`**

In `src/backend/json.rs`, inside `update_from_json()`, after the line that creates the Device via `Device::new(...)`, add firmwares/limits population from the raw JSON device. This requires that `TTSMIDeviceRaw` is plumbed through. Since the current code flattens `TTSMIDeviceRaw → TTSMIDeviceJSON` and loses the raw struct, the cleanest fix is to extend `TTSMIDeviceJSON` with the two extra fields and pass them through the `map()` in `parse_json()`:

In `TTSMIDeviceJSON`, add:
```rust
    pub firmwares: Option<serde_json::Value>,
    pub limits: Option<serde_json::Value>,
```

In `parse_json()`, inside the `.map(|(idx, raw)|` closure:
```rust
                            firmwares: raw.firmwares,
                            limits: raw.limits,
```

In `update_from_json()`, after `let device = Device::new(...)`:
```rust
                let firmwares = json_dev.firmwares.as_ref().and_then(|v| {
                    serde_json::from_value::<crate::models::telemetry::FirmwaresInfo>(v.clone()).ok()
                });
                let limits = json_dev.limits.as_ref().and_then(|v| {
                    serde_json::from_value::<crate::models::telemetry::DeviceLimits>(v.clone()).ok()
                });
                let mut device = Device::new(idx, board_type, bus_id, coords);
                device.firmwares = firmwares;
                device.limits = limits;
```

- [ ] **Step 8: Add `update_from_json_pub` test helper**

Add to `impl JSONBackend`:
```rust
    /// Test-only: expose update_from_json for white-box testing.
    #[cfg(test)]
    pub fn update_from_json_pub(&mut self, devices: Vec<TTSMIDeviceJSON>) -> BackendResult<()> {
        self.update_from_json(devices)
    }
```

- [ ] **Step 9: Run tests**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "52_|old_format|FAILED|test result"
```

Expected: 6 new tests pass, all ≥ 118 pass.

- [ ] **Step 10: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/backend/json.rs src/models/telemetry.rs
git commit -m "feat(backend/json): wire tt-smi 5.2.0 GDDR temps, ETH live, harvesting, firmwares, limits"
```

---

### Task 5: MockScenario enum + Galaxy-scale fixtures

**Files:**
- Modify: `src/backend/mock.rs`

- [ ] **Step 1: Write failing tests**

Append to `mod tests` in `src/backend/mock.rs`:

```rust
    #[test]
    fn test_galaxy_mock_has_32_devices() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        assert_eq!(b.devices().len(), 32);
        assert!(b.devices().iter().all(|d| d.architecture == Architecture::Blackhole));
    }

    #[test]
    fn test_quad_galaxy_mock_has_128_devices() {
        let mut b = MockBackend::with_scenario(MockScenario::QuadGalaxy);
        b.init().unwrap();
        assert_eq!(b.devices().len(), 128);
        assert!(b.devices().iter().all(|d| d.architecture == Architecture::Blackhole));
    }

    #[test]
    fn test_wormhole_cluster_mock() {
        let mut b = MockBackend::with_scenario(MockScenario::WormholeCluster);
        b.init().unwrap();
        assert_eq!(b.devices().len(), 32);
        assert!(b.devices().iter().all(|d| d.architecture == Architecture::Wormhole));
    }

    #[test]
    fn test_mock_firmwares_populated() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        for d in b.devices() {
            assert!(d.firmwares.is_some(), "device {} missing firmwares", d.index);
            assert!(d.firmwares.as_ref().unwrap().fw_bundle_version.is_some());
        }
    }

    #[test]
    fn test_mock_limits_populated() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        for d in b.devices() {
            assert!(d.limits.is_some(), "device {} missing limits", d.index);
            assert_eq!(d.limits.as_ref().unwrap().tdp_limit, Some(300.0_f32));
        }
    }

    #[test]
    fn test_mock_gddr_temps_valid_range() {
        let mut b = MockBackend::with_scenario(MockScenario::Galaxy);
        b.init().unwrap();
        for idx in 0..b.devices().len() {
            if let Some(smbus) = b.smbus_telemetry(idx) {
                for pair in smbus.gddr_temps.iter().flatten() {
                    for &t in pair.0.iter() {
                        assert!(t >= 0.0 && t <= 100.0,
                            "GDDR temp {} out of range for dev {}", t, idx);
                    }
                }
            }
        }
    }

    #[test]
    fn test_mock_quad_galaxy_some_harvesting() {
        let mut b = MockBackend::with_scenario(MockScenario::QuadGalaxy);
        b.init().unwrap();
        // At least some devices should have non-full enabled_tensix_col (harvested cols)
        let any_harvested = b.devices().iter().any(|d| {
            b.smbus_telemetry(d.index)
                .and_then(|s| s.enabled_tensix_col)
                .map(|v| v != 0x3FFF)
                .unwrap_or(false)
        });
        assert!(any_harvested, "QuadGalaxy mock should have some harvested columns");
    }
```

- [ ] **Step 2: Verify failure**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "galaxy|wormhole_cluster|mock_firmwares|mock_limits|mock_gddr|quad_galaxy|error" | head -20
```

Expected: compile errors — `MockScenario` doesn't exist yet.

- [ ] **Step 3: Add `MockScenario` enum and `with_scenario` constructor**

In `src/backend/mock.rs`, add before `pub struct MockBackend`:

```rust
/// Pre-defined mock hardware scenarios for testing at various scales.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MockScenario {
    /// Default: cycles GS/WH/BH by device index (existing behavior).
    Mixed,
    /// Single Galaxy: 32 Blackhole chips.
    Galaxy,
    /// Quad Galaxy: 128 Blackhole chips across 4 Galaxy systems.
    QuadGalaxy,
    /// Pure Wormhole cluster: 32 WH chips.
    WormholeCluster,
}
```

Add `scenario: MockScenario` field to `MockBackend`:
```rust
    /// Scenario controlling device count and architecture.
    scenario: MockScenario,
```

Update `MockBackend::new()` to add:
```rust
            scenario: MockScenario::Mixed,
```

Add new constructor:
```rust
    /// Create mock backend with a predefined hardware scenario.
    pub fn with_scenario(scenario: MockScenario) -> Self {
        let device_count = match scenario {
            MockScenario::Mixed => 6,
            MockScenario::Galaxy => 32,
            MockScenario::QuadGalaxy => 128,
            MockScenario::WormholeCluster => 32,
        };
        Self {
            device_count,
            scenario,
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus_telemetry: HashMap::new(),
            config: BackendConfig::default(),
            update_count: 0,
            base_power: HashMap::new(),
            base_temp: HashMap::new(),
        }
    }
```

- [ ] **Step 4: Update `generate_devices()` to respect scenario**

Replace the `match idx % 3` block in `generate_devices()`:

```rust
        for idx in 0..self.device_count {
            let board_type = match self.scenario {
                MockScenario::Galaxy | MockScenario::QuadGalaxy => "p150a".to_string(),
                MockScenario::WormholeCluster => "n150".to_string(),
                MockScenario::Mixed => match idx % 3 {
                    0 => "e150".to_string(),
                    1 => "n150".to_string(),
                    _ => "p150".to_string(),
                },
            };
            let bus_id = format!("0000:{:02x}:00.0", (idx % 256) + 1);
            let mut device = Device::new(idx, board_type, bus_id,
                format!("({},{})", idx / 8, idx % 8));

            // Populate firmwares and limits for all devices
            device.firmwares = Some(crate::models::telemetry::FirmwaresInfo {
                fw_bundle_version: Some("fw_pack-19.9.0".to_string()),
                eth_fw: Some("6.16.0.0".to_string()),
                cm_fw: Some("v80.15.0.0".to_string()),
                gddr_fw: Some("1.37".to_string()),
            });
            device.limits = Some(crate::models::telemetry::DeviceLimits {
                tdp_limit: Some(300.0),
                tdc_limit: Some(40.0),
                asic_fmax: Some(800),
                thm_limit: Some(105.0),
            });

            self.devices.push(device);
        }
```

- [ ] **Step 5: Update `generate_smbus_telemetry()` to populate new fields**

In `generate_smbus_telemetry()`, replace the returned `SmbusTelemetry { ... }` to add new fields. After the existing DDR status logic, build and include:

```rust
        // Deterministic "random" harvesting for QuadGalaxy: device idx % 17 == 0 harvests col 0
        let enabled_tensix_col = if self.scenario == MockScenario::QuadGalaxy && device_idx % 17 == 0 {
            Some(0x3FFE_u32) // col 0 harvested
        } else {
            Some(0x3FFF_u32) // all 14 cols active
        };

        // GDDR temps: base 38-44°C with sinusoidal variation
        let t_base = 38.0_f32 + (device_idx % 4) as f32 * 2.0;
        let t_var = ((self.update_count as f32 * 0.1).sin() * 4.0).abs();
        let gddr_temp_pair = crate::models::telemetry::GddrTempPair([
            t_base + t_var,
            t_base + t_var + 2.0,
            t_base + t_var + 4.0,
            t_base + t_var + 6.0,
        ]);

        // ETH live status: ~75% ports live (alternating bit pattern)
        let eth_live = 0xAAAAAAAAAAAAAAAA_u64;
```

Then include these in the returned struct (after `..SmbusTelemetry::default()`):
```rust
            enabled_tensix_col,
            gddr_temps: [Some(gddr_temp_pair); 4],
            max_gddr_temp: Some(t_base + t_var + 6.0),
            eth_live_status: Some(eth_live),
            harvesting_state: if self.scenario == MockScenario::QuadGalaxy && device_idx % 17 == 0 {
                Some(1)
            } else {
                Some(0)
            },
            ..SmbusTelemetry::default()
```

- [ ] **Step 6: Run tests**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "galaxy|cluster|firmwares|limits|gddr_temps|quad_galaxy|FAILED|test result"
```

Expected: 7 new tests pass, all ≥ 125 pass.

- [ ] **Step 7: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/backend/mock.rs
git commit -m "feat(backend/mock): add MockScenario with Galaxy/QuadGalaxy/WH fixtures; populate new fields"
```

---

### Task 6: Chip topology module — CoreType and classification functions

**Files:**
- Create: `src/ui/tui/chip_portrait.rs`

This module owns topology knowledge and portrait rendering. We build it test-first.

- [ ] **Step 1: Write failing tests in a new file**

Create `src/ui/tui/chip_portrait.rs` with only the tests (implementation stubs follow):

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Chip Portrait — character-art core-grid visualization for Blackhole and Wormhole.
//!
//! Each chip is rendered as an exact W×H character grid where every character is
//! exactly one terminal column wide. The render function guarantees that no output
//! line exceeds `portrait_cols(arch)` characters.

use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use crate::models::telemetry::{tensix_col_harvested, GddrTempPair};

/// Full chip grid dimensions (cols × rows) for each architecture.
/// These are the COMPLETE chip grids including ETH, DRAM, and PCIe cells.
/// Source: tensix-viz chip.js coreType() — authoritative.
pub fn portrait_dims(arch: Architecture) -> (usize, usize) {
    match arch {
        Architecture::Blackhole => (17, 12),
        Architecture::Wormhole  => (10, 12),
        Architecture::Grayskull => (10, 12), // rendered as all-Tensix
        Architecture::Unknown   => (10, 12),
    }
}

/// Classification of a single core cell in the chip grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoreType {
    Tensix,
    Dram,
    Eth,
    Pcie,
}

/// Return the CoreType for cell (col, row) on a Blackhole chip.
/// BH: 17 cols × 12 rows. ETH at col 0 or 16. PCIe at col 8.
/// DRAM at row 0 or 11. Tensix everywhere else.
pub fn core_type_bh(col: usize, row: usize) -> CoreType {
    if col == 0 || col == 16    { CoreType::Eth }
    else if row == 0 || row == 11 { CoreType::Dram }
    else if col == 8            { CoreType::Pcie }
    else                        { CoreType::Tensix }
}

/// Return the CoreType for cell (col, row) on a Wormhole chip.
/// WH: 10 cols × 12 rows. ETH at row 0 or 6. DRAM at col 0 or 5.
/// Tensix everywhere else (no PCIe special cell).
pub fn core_type_wh(col: usize, row: usize) -> CoreType {
    if row == 0 || row == 6  { CoreType::Eth }
    else if col == 0 || col == 5 { CoreType::Dram }
    else                     { CoreType::Tensix }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── portrait_dims ────────────────────────────────────────────────────────

    #[test]
    fn test_bh_portrait_dims() {
        assert_eq!(portrait_dims(Architecture::Blackhole), (17, 12));
    }

    #[test]
    fn test_wh_portrait_dims() {
        assert_eq!(portrait_dims(Architecture::Wormhole), (10, 12));
    }

    // ── BH core type exhaustive ───────────────────────────────────────────────

    #[test]
    fn test_bh_eth_cols() {
        for row in 0..12 {
            assert_eq!(core_type_bh(0,  row), CoreType::Eth,  "BH col=0  row={}", row);
            assert_eq!(core_type_bh(16, row), CoreType::Eth,  "BH col=16 row={}", row);
        }
    }

    #[test]
    fn test_bh_dram_rows() {
        // DRAM rows only apply to non-ETH columns
        for col in 1..16_usize {
            if col == 8 { continue; } // PCIe col
            assert_eq!(core_type_bh(col, 0),  CoreType::Dram, "BH col={} row=0",  col);
            assert_eq!(core_type_bh(col, 11), CoreType::Dram, "BH col={} row=11", col);
        }
    }

    #[test]
    fn test_bh_pcie_col() {
        for row in 1..11 {
            assert_eq!(core_type_bh(8, row), CoreType::Pcie, "BH PCIe row={}", row);
        }
    }

    #[test]
    fn test_bh_tensix_interior() {
        // Sample interior cells that are definitely Tensix
        for &(col, row) in &[(1,1), (4,5), (15,10), (7,6), (9,3)] {
            assert_eq!(core_type_bh(col, row), CoreType::Tensix,
                "expected Tensix at BH ({},{})", col, row);
        }
    }

    #[test]
    fn test_bh_dram_row_takes_precedence_over_pcie() {
        // col=8, row=0 → ETH col wins over DRAM, but col 8 is interior; row 0 should be DRAM
        // col=0 row=0 → ETH wins (ETH check first)
        assert_eq!(core_type_bh(0, 0),   CoreType::Eth);  // ETH col
        assert_eq!(core_type_bh(8, 0),   CoreType::Dram); // PCIe col but row 0 → DRAM
        assert_eq!(core_type_bh(8, 11),  CoreType::Dram); // same
        assert_eq!(core_type_bh(8, 1),   CoreType::Pcie); // PCIe non-DRAM row
    }

    // ── WH core type exhaustive ───────────────────────────────────────────────

    #[test]
    fn test_wh_eth_rows() {
        for col in 0..10 {
            assert_eq!(core_type_wh(col, 0), CoreType::Eth, "WH col={} row=0", col);
            assert_eq!(core_type_wh(col, 6), CoreType::Eth, "WH col={} row=6", col);
        }
    }

    #[test]
    fn test_wh_dram_cols() {
        // DRAM only for non-ETH rows
        for row in [1,2,3,4,5,7,8,9,10,11] {
            assert_eq!(core_type_wh(0, row), CoreType::Dram, "WH DRAM col=0 row={}", row);
            assert_eq!(core_type_wh(5, row), CoreType::Dram, "WH DRAM col=5 row={}", row);
        }
    }

    #[test]
    fn test_wh_tensix_interior() {
        for &(col, row) in &[(1,1), (3,5), (9,11), (6,8), (4,4)] {
            assert_eq!(core_type_wh(col, row), CoreType::Tensix,
                "expected Tensix at WH ({},{})", col, row);
        }
    }

    #[test]
    fn test_wh_eth_row_wins_over_dram_col() {
        // col=0 row=0 → ETH row wins (ETH check is first in core_type_wh)
        assert_eq!(core_type_wh(0, 0), CoreType::Eth);
        assert_eq!(core_type_wh(5, 6), CoreType::Eth);
    }
}
```

- [ ] **Step 2: Register the module**

In `src/ui/tui/mod.rs`, add near the top (after the `use` block):

```rust
pub mod chip_portrait;
```

- [ ] **Step 3: Run topology tests**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib chip_portrait 2>&1 | grep -E "ok|FAILED|error"
```

Expected: 14 topology tests pass.

- [ ] **Step 4: Commit topology module**

```bash
cd /home/ttuser/code/tt-toplike
git add src/ui/tui/chip_portrait.rs src/ui/tui/mod.rs
git commit -m "feat(ui): add chip_portrait module with BH/WH topology classification (14 tests)"
```

---

### Task 7: Chip Portrait renderer — character art with strict width guarantees

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs`

**Width contract (enforced by debug_assert in every build):**
- Each portrait row is exactly `portrait_cols(arch)` characters wide.
- Characters used: `▓▒░·D●P` — all Unicode single-width (confirmed via `unicode_width` crate).
- No right-border characters are ever emitted.

- [ ] **Step 1: Write failing render tests**

Append to `mod tests` in `src/ui/tui/chip_portrait.rs`:

```rust
    use crate::models::telemetry::{GddrTempPair, FirmwaresInfo};

    fn make_smbus(arch: Architecture) -> SmbusTelemetry {
        let mut s = SmbusTelemetry::new();
        s.enabled_tensix_col = Some(0x3FFF); // no harvesting
        s.eth_live_status = Some(0xFFFFFFFFFFFFFFFF); // all live
        s.gddr_temps = [
            Some(GddrTempPair([38.0, 40.0, 42.0, 44.0])); 4
        ];
        s
    }

    fn make_device(arch: Architecture) -> Device {
        let board = match arch {
            Architecture::Blackhole => "p150a",
            Architecture::Wormhole  => "n150",
            _                       => "e150",
        };
        Device::new(0, board.to_string(), "0000:01:00.0".to_string(), "(0,0)".to_string())
    }

    fn make_telemetry() -> Telemetry {
        Telemetry { power: Some(100.0), ..Telemetry::new() }
    }

    // ── Width contract ────────────────────────────────────────────────────────

    #[test]
    fn test_bh_portrait_row_width_exact() {
        use unicode_width::UnicodeWidthStr;
        let device  = make_device(Architecture::Blackhole);
        let smbus   = make_smbus(Architecture::Blackhole);
        let telem   = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        assert_eq!(rows.len(), 12, "BH must have 12 rows");
        for (i, row) in rows.iter().enumerate() {
            let w = UnicodeWidthStr::width(row.as_str());
            assert_eq!(w, 17, "BH row {} width is {} not 17: {:?}", i, w, row);
        }
    }

    #[test]
    fn test_wh_portrait_row_width_exact() {
        use unicode_width::UnicodeWidthStr;
        let device  = make_device(Architecture::Wormhole);
        let smbus   = make_smbus(Architecture::Wormhole);
        let telem   = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        assert_eq!(rows.len(), 12, "WH must have 12 rows");
        for (i, row) in rows.iter().enumerate() {
            let w = UnicodeWidthStr::width(row.as_str());
            assert_eq!(w, 10, "WH row {} width is {} not 10: {:?}", i, w, row);
        }
    }

    #[test]
    fn test_bh_portrait_no_overflow_no_smbus() {
        use unicode_width::UnicodeWidthStr;
        let device = make_device(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, None, 0);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(UnicodeWidthStr::width(row.as_str()), 17,
                "BH row {} overflowed without smbus", i);
        }
    }

    // ── Content correctness ───────────────────────────────────────────────────

    #[test]
    fn test_bh_eth_cols_show_eth_char() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // col 0 should be '●' (ETH live) in all rows
        for row in &rows {
            let chars: Vec<char> = row.chars().collect();
            assert!(chars[0] == '●' || chars[0] == '·',
                "BH col=0 should be ETH char, got '{}'", chars[0]);
        }
    }

    #[test]
    fn test_bh_dram_rows_show_d() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // row 0 col 1 (non-ETH, non-PCIe) should be 'D'
        let row0_chars: Vec<char> = rows[0].chars().collect();
        assert_eq!(row0_chars[1], 'D', "BH row=0 col=1 should be DRAM 'D'");
        let row11_chars: Vec<char> = rows[11].chars().collect();
        assert_eq!(row11_chars[1], 'D', "BH row=11 col=1 should be DRAM 'D'");
    }

    #[test]
    fn test_bh_pcie_shows_p() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // col 8, non-DRAM rows (1-10) should be 'P'
        for row_idx in 1..11 {
            let chars: Vec<char> = rows[row_idx].chars().collect();
            assert_eq!(chars[8], 'P', "BH PCIe col=8 row={} should be 'P'", row_idx);
        }
    }

    #[test]
    fn test_harvested_col_shows_dot() {
        let device = make_device(Architecture::Blackhole);
        let mut smbus = make_smbus(Architecture::Blackhole);
        smbus.enabled_tensix_col = Some(0x3FFE); // col 0 of Tensix harvested
        // BH Tensix col 0 = chip col 1 (first non-ETH col)
        let telem = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // chip col 1, row 1 (Tensix area) should be '·' (harvested)
        let chars: Vec<char> = rows[1].chars().collect();
        assert_eq!(chars[1], '·', "harvested col should show '·', got '{}'", chars[1]);
    }

    #[test]
    fn test_eth_down_shows_dot() {
        let device = make_device(Architecture::Blackhole);
        let mut smbus = make_smbus(Architecture::Blackhole);
        smbus.eth_live_status = Some(0x0); // all ETH down
        let telem = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        let chars: Vec<char> = rows[0].chars().collect();
        assert_eq!(chars[0], '·', "ETH down should show '·'");
    }
```

- [ ] **Step 2: Verify tests fail (not yet implemented)**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib chip_portrait 2>&1 | grep -E "error|FAILED" | head -10
```

Expected: compile errors — `build_portrait_rows` not defined.

- [ ] **Step 3: Implement `build_portrait_rows()` and `render_chip_portrait()`**

Add to `src/ui/tui/chip_portrait.rs` (after the `pub fn core_type_wh` function):

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Activity level for a Tensix core, derived from chip-level power fraction.
fn tensix_char(activity: f32) -> char {
    if      activity >= 0.75 { '▓' }
    else if activity >= 0.40 { '▒' }
    else if activity >= 0.05 { '░' }
    else                     { '·' }
}

/// Temperature-based color for DRAM cells.
fn gddr_color(temp_c: f32) -> Color {
    if      temp_c > 70.0 { Color::Red }
    else if temp_c > 50.0 { Color::Yellow }
    else                  { Color::Cyan }
}

/// Return the 0-indexed Tensix column index for a Blackhole chip column.
/// BH Tensix columns: chip cols 1-7 and 9-15 (excludes ETH cols 0,16 and PCIe col 8).
/// Returns None if chip_col is not a Tensix column.
fn bh_tensix_col_index(chip_col: usize) -> Option<usize> {
    match chip_col {
        1..=7  => Some(chip_col - 1),          // Tensix cols 0-6
        9..=15 => Some(chip_col - 2),          // Tensix cols 7-13
        _      => None,
    }
}

/// ETH port index for a BH cell (col, row).
/// BH has 2 ETH columns (col 0 and col 16) × 12 rows = 24 ETH cells total.
/// Port index: col 0 → ports 0-11 (row 0=port 0), col 16 → ports 12-23.
fn bh_eth_port(col: usize, row: usize) -> usize {
    if col == 0 { row } else { 12 + row }
}

/// ETH port index for a WH cell (col, row).
/// WH has 2 ETH rows (row 0 and row 6) × 10 cols = 20 ETH cells total.
/// Row 0 → ports 0-9, row 6 → ports 10-19.
fn wh_eth_port(col: usize, row: usize) -> usize {
    if row == 0 { col } else { 10 + col }
}

/// Build portrait rows as plain Strings for testing (width validation).
/// Each String is exactly `portrait_cols(arch)` Unicode characters wide.
///
/// # Width guarantee
/// The function pushes exactly one char per cell, iterating col 0..cols.
/// No multi-width Unicode is used: ▓▒░·D●P are all single-column chars.
pub fn build_portrait_rows(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    _tick: u64,
) -> Vec<String> {
    let (cols, rows) = portrait_dims(device.architecture);

    // Chip-level activity: power / 300W TDP (clamped 0-1)
    let activity = (telemetry.power_w() / 300.0).min(1.0).max(0.0);

    let mut result = Vec::with_capacity(rows);

    for row in 0..rows {
        let mut line = String::with_capacity(cols);

        for col in 0..cols {
            let core_type = match device.architecture {
                Architecture::Blackhole => core_type_bh(col, row),
                _                       => core_type_wh(col, row),
            };

            let ch = match core_type {
                CoreType::Pcie => 'P',

                CoreType::Eth => {
                    let port = match device.architecture {
                        Architecture::Blackhole => bh_eth_port(col, row),
                        _                       => wh_eth_port(col, row),
                    };
                    let live = smbus
                        .and_then(|s| s.eth_live_status)
                        .map(|mask| (mask >> port) & 1 == 1)
                        .unwrap_or(false);
                    if live { '●' } else { '·' }
                }

                CoreType::Dram => 'D',

                CoreType::Tensix => {
                    // Check harvesting (BH only: map chip col → tensix col index)
                    let harvested = if device.architecture == Architecture::Blackhole {
                        bh_tensix_col_index(col)
                            .and_then(|tc| smbus.and_then(|s| s.enabled_tensix_col).map(|mask| tensix_col_harvested(mask, tc)))
                            .unwrap_or(false)
                    } else {
                        false // WH/GS don't use ENABLED_TENSIX_COL
                    };
                    if harvested { '·' } else { tensix_char(activity) }
                }
            };

            line.push(ch);
        }

        // Invariant: must be exactly `cols` chars wide.
        debug_assert_eq!(
            line.chars().count(), cols,
            "portrait row {} width {} ≠ {}", row, line.chars().count(), cols
        );

        result.push(line);
    }

    result
}

/// Build portrait rows as styled Ratatui Lines (for actual rendering).
/// Color rules:
///   Tensix active (▓): Cyan (#4FD1C5)     moderate (▒): Yellow     idle (░): DarkGray
///   Tensix harvested: DarkGray dim
///   DRAM: colored by GDDR temp (Cyan/Yellow/Red)
///   ETH live (●): Green    ETH down (·): DarkGray
///   PCIe: Blue
pub fn build_portrait_lines<'a>(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Vec<Line<'a>> {
    let rows_strs = build_portrait_rows(device, telemetry, smbus, tick);
    let (cols, _rows) = portrait_dims(device.architecture);

    // Pre-compute per-column DRAM temp color (BH: by GDDR pair index, WH: none)
    let dram_color_for_col = |col: usize| -> Color {
        let pair_idx = col / 4; // crude mapping: 4 cols per GDDR pair
        let temp = smbus
            .and_then(|s| s.gddr_temps.get(pair_idx).and_then(|p| p.as_ref()))
            .map(|p| p.0.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            .unwrap_or(0.0);
        gddr_color(temp)
    };

    rows_strs.into_iter().enumerate().map(|(row, row_str)| {
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(cols);

        for (col, ch) in row_str.chars().enumerate() {
            let core_type = match device.architecture {
                Architecture::Blackhole => core_type_bh(col, row),
                _                       => core_type_wh(col, row),
            };

            let activity = (telemetry.power_w() / 300.0).min(1.0).max(0.0);

            let style = match (core_type, ch) {
                (CoreType::Pcie, _) =>
                    Style::default().fg(Color::Blue),
                (CoreType::Eth, '●') =>
                    Style::default().fg(Color::Green),
                (CoreType::Eth, _) =>
                    Style::default().fg(Color::DarkGray),
                (CoreType::Dram, _) =>
                    Style::default().fg(dram_color_for_col(col)),
                (CoreType::Tensix, '·') =>
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                (CoreType::Tensix, '▓') =>
                    Style::default().fg(Color::Cyan),
                (CoreType::Tensix, '▒') =>
                    Style::default().fg(Color::Yellow),
                (CoreType::Tensix, _) =>
                    Style::default().fg(if activity > 0.2 {
                        Color::DarkGray
                    } else {
                        Color::DarkGray
                    }),
                _ => Style::default(),
            };

            spans.push(Span::styled(ch.to_string(), style));
        }

        Line::from(spans)
    }).collect()
}

/// Render a chip portrait into the given Rect area.
///
/// Returns the sub-Rect actually used (portrait_cols × portrait_rows).
/// The caller is responsible for allocating at least portrait_dims(arch) space.
pub fn render_chip_portrait(
    f: &mut Frame,
    area: Rect,
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Rect {
    let (cols, rows) = portrait_dims(device.architecture);
    let used = Rect {
        x: area.x,
        y: area.y,
        width:  (cols as u16).min(area.width),
        height: (rows as u16).min(area.height),
    };

    let lines = build_portrait_lines(device, telemetry, smbus, tick);
    let para = Paragraph::new(lines);
    f.render_widget(para, used);

    used
}
```

- [ ] **Step 4: Add `unicode-width` import to chip_portrait.rs**

At the top of `src/ui/tui/chip_portrait.rs`, add:
```rust
#[cfg(test)]
use unicode_width::UnicodeWidthStr;
```

- [ ] **Step 5: Run all chip_portrait tests**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib chip_portrait 2>&1 | grep -E "ok|FAILED|error"
```

Expected: all 23 chip portrait tests pass.

- [ ] **Step 6: Run full test suite**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep "test result"
```

Expected: ≥ 148 tests, 0 failures.

- [ ] **Step 7: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/ui/tui/chip_portrait.rs src/ui/tui/mod.rs
git commit -m "feat(ui): implement Chip Portrait renderer with BH/WH topology, strict width validation"
```

---

### Task 8: Insights screen — split-panel with portrait + stats sidebar

**Files:**
- Modify: `src/ui/tui/mod.rs`

The existing `render_device_cards()` renders 4 lines per device in a `Paragraph`. We replace it with `render_device_panels()` that uses a horizontal split per device: portrait (left) + stats sidebar (right).

- [ ] **Step 1: Add `render_device_panels()` function**

In `src/ui/tui/mod.rs`, add a new function after `render_device_cards()` (around line 1914):

```rust
/// Render Insights device panels — one per device, each with a chip portrait on the
/// left and a stats sidebar on the right.
///
/// Layout per device (Blackhole example, 49 chars wide):
///   ╔═════════════════   ╔══════════════════════════════
///   ║  (17×12 portrait)  ║  Dev N  BH p150a
///   ╚═════════════════   ║  Power  ████████░░  145W
///                        ║  DDR    ████████░░  12.3 GB/s
///                        ║  ETH    ●●●●·●·●    6/8 live
///                        ║  GDDR T 38°→44°C    max 44°C
///                        ║  Fan         2400 RPM
///                        ║  Temp   52.0°C  (limit 105°C)
///                        ║  FW     19.9.0  eth 6.16
///                        ╚══════════════════════════════
///
/// Width math (Blackhole):
///   Portrait outer (left border ║ + 17 content) = 18 chars
///   Gap = 1 char
///   Stats outer  (left border ║ + 30 content)  = 31 chars
///   Total per device = 18 + 1 + 31 = 50 chars   (fits any ≥52 col terminal)
///
/// Width math (Wormhole):
///   Portrait outer = 1 + 10 = 11 chars
///   Gap = 1 char
///   Stats outer  = 1 + 30 = 31 chars
///   Total = 11 + 1 + 31 = 43 chars
#[cfg(feature = "linux-procfs")]
fn render_device_panels(
    f: &mut Frame,
    area: Rect,
    backend: &Box<dyn TelemetryBackend>,
    _engine: &crate::workload::InferenceEngine,
    tick: u64,
) {
    use crate::ui::tui::chip_portrait::{portrait_dims, render_chip_portrait};
    use ratatui::layout::{Constraint, Direction, Layout, Rect};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let devices = backend.devices();
    if devices.is_empty() { return; }

    // Determine portrait height from the first device's arch (all same for homogeneous fleets)
    let panel_h: u16 = 14; // portrait 12 rows + 1 title row + 1 bottom border row
    let gap_col: u16 = 1;

    let mut y_offset = area.y;

    for device in devices.iter() {
        if y_offset + panel_h > area.y + area.height { break; } // no space left

        let idx = device.index;
        let telemetry = backend.telemetry(idx);
        let smbus     = backend.smbus_telemetry(idx);

        let (portrait_cols, _portrait_rows) = portrait_dims(device.architecture);
        let portrait_w = portrait_cols as u16 + 1; // +1 for left border ║
        let stats_w    = 31_u16;                    // 1 border + 30 content
        let panel_w    = (portrait_w + gap_col + stats_w).min(area.width);

        let panel_rect = Rect { x: area.x, y: y_offset, width: panel_w, height: panel_h };

        // ── Left: portrait ────────────────────────────────────────────────────
        let portrait_rect = Rect {
            x: panel_rect.x + 1,  // +1 for left-border column rendered below
            y: panel_rect.y + 1,  // +1 for top-label row
            width: portrait_cols as u16,
            height: panel_h.saturating_sub(2), // leave 1 row top, 1 row bottom
        };

        // Render left border column manually as a Paragraph
        let left_border_x = panel_rect.x;
        let left_border_rect = Rect { x: left_border_x, y: panel_rect.y,
            width: 1, height: panel_h };
        let border_lines: Vec<Line> = (0..panel_h as usize).map(|r| {
            let ch = if r == 0 { "╔" } else if r == panel_h as usize - 1 { "╚" } else { "║" };
            Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
        }).collect();
        f.render_widget(Paragraph::new(border_lines), left_border_rect);

        if let Some(telem) = telemetry {
            render_chip_portrait(f, portrait_rect, device, telem, smbus, tick);
        }

        // ── Right: stats sidebar ──────────────────────────────────────────────
        let stats_x = panel_rect.x + portrait_w + gap_col;
        let stats_rect = Rect {
            x: stats_x,
            y: panel_rect.y,
            width: stats_w,
            height: panel_h,
        };

        // Render right-panel left border
        let stats_border_rect = Rect { x: stats_x, y: panel_rect.y, width: 1, height: panel_h };
        let stats_border: Vec<Line> = (0..panel_h as usize).map(|r| {
            let ch = if r == 0 { "╔" } else if r == panel_h as usize - 1 { "╚" } else { "║" };
            Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
        }).collect();
        f.render_widget(Paragraph::new(stats_border), stats_border_rect);

        // Stats content (28 chars max per line, starts at stats_x+1)
        let content_x = stats_x + 1;
        let content_w = stats_w.saturating_sub(1);
        let content_rect = Rect { x: content_x, y: panel_rect.y, width: content_w, height: panel_h };

        let mut stat_lines: Vec<Line> = Vec::new();

        // Title row
        let arch_short = device.architecture.abbrev();
        let title = format!("Dev {:2}  {} {}", idx, arch_short,
            &device.board_type[..device.board_type.len().min(8)]);
        stat_lines.push(Line::from(Span::styled(
            format!("{:<28}", &title[..title.len().min(28)]),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        )));

        // Power row
        let power_w = telemetry.map(|t| t.power_w()).unwrap_or(0.0);
        let tdp = smbus.and_then(|_| backend.devices().iter().find(|d| d.index == idx))
                       .and_then(|d| d.limits.as_ref())
                       .and_then(|l| l.tdp_limit)
                       .unwrap_or(300.0);
        let power_frac = (power_w / tdp).min(1.0);
        let bar_filled = (power_frac * 10.0) as usize;
        let power_bar: String = (0..10).map(|i| if i < bar_filled { '█' } else { '░' }).collect();
        let power_color = if power_frac > 0.85 { Color::Red } else if power_frac > 0.6 { Color::Yellow } else { Color::Green };
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "Power"), Style::default().fg(Color::DarkGray)),
            Span::styled(power_bar, Style::default().fg(power_color)),
            Span::styled(format!(" {:5.0}W", power_w), Style::default().fg(Color::White)),
        ]));

        // ETH row
        let (eth_live, eth_total) = if let Some(s) = smbus {
            let mask = s.eth_live_status.unwrap_or(0);
            let total: u32 = match device.architecture {
                Architecture::Blackhole => 24,
                Architecture::Wormhole  => 20,
                _                       => 4,
            };
            (mask.count_ones(), total)
        } else { (0, 0) };

        let eth_dots: String = (0..eth_total.min(8)).map(|i| {
            if (smbus.and_then(|s| s.eth_live_status).unwrap_or(0) >> i) & 1 == 1 { '●' } else { '·' }
        }).collect();
        let eth_color = if eth_live == eth_total.into() { Color::Green } else { Color::Yellow };
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "ETH"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:<8}", eth_dots), Style::default().fg(eth_color)),
            Span::styled(format!(" {}/{} live", eth_live, eth_total), Style::default().fg(Color::White)),
        ]));

        // GDDR temp row
        if let Some(s) = smbus {
            let temps: Vec<f32> = s.gddr_temps.iter()
                .filter_map(|p| p.as_ref())
                .flat_map(|p| p.0.iter().copied())
                .filter(|&t| t > 0.0)
                .collect();
            if !temps.is_empty() {
                let t_min = temps.iter().copied().fold(f32::INFINITY, f32::min);
                let t_max = temps.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let max_color = if t_max > 70.0 { Color::Red } else if t_max > 50.0 { Color::Yellow } else { Color::Cyan };
                stat_lines.push(Line::from(vec![
                    Span::styled(format!("{:<8}", "GDDR T"), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:.0}°→{:.0}°C", t_min, t_max), Style::default().fg(max_color)),
                ]));
            }
        }

        // ASIC temp row
        let asic_temp = telemetry.map(|t| t.temp_c()).unwrap_or(0.0);
        let thm_limit = smbus.and_then(|_| backend.devices().iter().find(|d| d.index == idx))
                             .and_then(|d| d.limits.as_ref())
                             .and_then(|l| l.thm_limit)
                             .unwrap_or(105.0);
        let temp_color = crate::ui::colors::temp_color(asic_temp);
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "Temp"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:.1}°C", asic_temp), Style::default().fg(temp_color)),
            Span::styled(format!(" (lim {:.0}°C)", thm_limit), Style::default().fg(Color::DarkGray)),
        ]));

        // Fan RPM row
        if let Some(rpm_str) = smbus.and_then(|s| s.fan_speed.as_deref()) {
            if let Ok(rpm) = rpm_str.trim().parse::<u32>() {
                if rpm > 0 {
                    stat_lines.push(Line::from(vec![
                        Span::styled(format!("{:<8}", "Fan"), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{} RPM", rpm), Style::default().fg(Color::White)),
                    ]));
                }
            }
        }

        // Firmware row
        let fw_ver = backend.devices().iter()
            .find(|d| d.index == idx)
            .and_then(|d| d.firmwares.as_ref())
            .and_then(|f| f.fw_bundle_version.as_deref())
            .unwrap_or("—");
        let fw_short = fw_ver.trim_start_matches("fw_pack-");
        stat_lines.push(Line::from(vec![
            Span::styled(format!("{:<8}", "FW"), Style::default().fg(Color::DarkGray)),
            Span::styled(fw_short.to_string(), Style::default().fg(Color::White)),
        ]));

        // Bottom border row (for stats panel — reuse a plain line)
        while stat_lines.len() < panel_h as usize - 1 {
            stat_lines.push(Line::raw(""));
        }
        // Last line: bottom separator
        stat_lines.push(Line::from(Span::styled(
            "═".repeat(content_w as usize),
            Style::default().fg(Color::DarkGray),
        )));

        f.render_widget(Paragraph::new(stat_lines), content_rect);

        y_offset += panel_h + 1; // +1 gap between devices
    }
}
```

- [ ] **Step 2: Update `render_insights()` to use the new panel renderer**

In `src/ui/tui/mod.rs`, locate the `render_device_cards(f, chunks[1], backend, engine);` call inside `render_insights()` (~line 1800) and replace it with:

```rust
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64 / 16; // ~60 FPS tick counter
    render_device_panels(f, chunks[1], backend, engine, tick);
```

Also update the layout constraint for the cards area to give it more room. Change `Constraint::Length((4 * n) as u16)` to:

```rust
            Constraint::Length((15 * n) as u16),  // 14 rows per panel + 1 gap
```

- [ ] **Step 3: Build and check for compile errors**

```bash
cd /home/ttuser/code/tt-toplike && cargo build --features tui,linux-procfs 2>&1 | grep -E "error|warning: unused" | head -20
```

Expected: clean build (warnings OK, errors not).

- [ ] **Step 4: Run full test suite**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep "test result"
```

Expected: ≥ 148 tests, 0 failures.

- [ ] **Step 5: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/ui/tui/mod.rs
git commit -m "feat(ui): update Insights screen with split-panel portrait + stats sidebar"
```

---

### Task 9: Grid mode — full-screen tiled chip portraits

**Files:**
- Modify: `src/ui/tui/mod.rs`

- [ ] **Step 1: Add `DisplayMode::Grid` and key binding**

In `src/ui/tui/mod.rs`, in the `DisplayMode` enum, add:

```rust
    /// Full-screen grid of chip portraits for all devices.
    Grid,
```

In the `'v'` key cycle handler, add `Grid` to the rotation (between `Insights` and `Table`):

```rust
                            DisplayMode::Insights => DisplayMode::Grid,
                            DisplayMode::Grid     => DisplayMode::Table,
```

Add a `'g'` key binding in the key match (after the `'v'` handler):

```rust
                    KeyCode::Char('g') => {
                        display_mode = DisplayMode::Grid;
                        log::info!("Switched to Grid mode");
                    }
```

In the `Event::Resize` handler, no new state to reset for Grid mode (no size-dependent structs).

In the `draw` closure `match display_mode`, add:

```rust
                    DisplayMode::Grid => {
                        render_grid_mode(f, backend, tick);
                    }
```

Where `tick` is computed the same way as in `render_insights` (add to the outer draw closure as a `let tick = ...` before the match).

- [ ] **Step 2: Add `render_grid_mode()` function**

Add to `src/ui/tui/mod.rs` after `render_device_panels()`:

```rust
/// Render Grid mode: all chip portraits tiled across the terminal.
///
/// # Width guarantee
/// Cell width = portrait_cols + 2 (left-border + 1 gap).
/// Only fully fitting cells are rendered — no partial cells.
/// This prevents right-edge overflow regardless of terminal width.
fn render_grid_mode(
    f: &mut Frame,
    backend: &Box<dyn TelemetryBackend>,
    tick: u64,
) {
    use crate::ui::tui::chip_portrait::{portrait_dims, render_chip_portrait};
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let area = f.area();
    let devices = backend.devices();
    if devices.is_empty() { return; }

    // Title bar (1 row)
    let title = format!(
        "tt-toplike  [Grid]  {} device{}  │  g or v to cycle views  │  q to quit",
        devices.len(), if devices.len() == 1 { "" } else { "s" }
    );
    let title_rect = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{:<w$}", &title[..title.len().min(area.width as usize)], w = area.width as usize),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ))),
        title_rect,
    );

    let grid_area = Rect {
        x: area.x, y: area.y + 1,
        width: area.width, height: area.height.saturating_sub(1),
    };

    // All devices use the same arch for cell sizing in homogeneous fleets.
    // For mixed fleets, use the first device's arch for layout math.
    let (p_cols, p_rows) = portrait_dims(devices[0].architecture);
    let cell_w = (p_cols as u16) + 2; // left-border char + content + 1-char gap
    let cell_h = (p_rows as u16) + 2; // title row + content + bottom-border row

    // Compute how many cells fit
    let cols_per_row = (grid_area.width / cell_w).max(1) as usize;
    let rows_visible = (grid_area.height / cell_h).max(1) as usize;
    let max_cells = cols_per_row * rows_visible;

    for (cell_idx, device) in devices.iter().enumerate().take(max_cells) {
        let grid_col = (cell_idx % cols_per_row) as u16;
        let grid_row = (cell_idx / cols_per_row) as u16;

        let cell_x = grid_area.x + grid_col * cell_w;
        let cell_y = grid_area.y + grid_row * cell_h;

        let idx = device.index;
        let telemetry = backend.telemetry(idx);
        let smbus     = backend.smbus_telemetry(idx);

        // Left border column
        let border_rect = Rect { x: cell_x, y: cell_y, width: 1, height: cell_h };
        let border_lines: Vec<Line> = (0..cell_h as usize).map(|r| {
            let ch = if r == 0 { "╔" } else if r == cell_h as usize - 1 { "╚" } else { "║" };
            Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
        }).collect();
        f.render_widget(Paragraph::new(border_lines), border_rect);

        // Device label (1 row below top border)
        let label = format!("{} {}",
            device.architecture.abbrev(),
            &device.board_type[..device.board_type.len().min(6)]);
        let label_rect = Rect { x: cell_x + 1, y: cell_y, width: p_cols as u16, height: 1 };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{:<w$}", &label[..label.len().min(p_cols)], w = p_cols),
                Style::default().fg(Color::Gray),
            ))),
            label_rect,
        );

        // Portrait (below label, above bottom border)
        let portrait_rect = Rect {
            x: cell_x + 1,
            y: cell_y + 1,
            width: p_cols as u16,
            height: p_rows as u16,
        };

        if let Some(telem) = telemetry {
            render_chip_portrait(f, portrait_rect, device, telem, smbus, tick);
        }

        // Bottom border line
        let bottom_rect = Rect {
            x: cell_x,
            y: cell_y + cell_h - 1,
            width: p_cols as u16 + 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("╚{}", "═".repeat(p_cols)),
                Style::default().fg(Color::DarkGray),
            ))),
            bottom_rect,
        );
    }
}
```

- [ ] **Step 3: Update Insights footer hint to mention `g`**

Find `render_insights_footer` and add `g` hint to the hint line (after the `v` hint):

```rust
            Span::raw("  \u{00B7}  "),
            Span::styled("g", Style::default().fg(Color::Cyan)),
            Span::raw("  grid"),
```

- [ ] **Step 4: Build clean**

```bash
cd /home/ttuser/code/tt-toplike && cargo build --features tui,linux-procfs 2>&1 | grep "^error" | head -10
```

Expected: 0 errors.

- [ ] **Step 5: Run full test suite**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep "test result"
```

Expected: ≥ 148 tests, 0 failures.

- [ ] **Step 6: Commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/ui/tui/mod.rs
git commit -m "feat(ui): add DisplayMode::Grid with full-screen tiled chip portraits (key: g)"
```

---

### Task 10: Final integration check and layout tests

**Files:**
- Modify: `src/ui/tui/chip_portrait.rs` (append layout tests)

- [ ] **Step 1: Add layout math tests**

Append to `mod tests` in `src/ui/tui/chip_portrait.rs`:

```rust
    // ── Layout math ───────────────────────────────────────────────────────────

    #[test]
    fn test_insights_panel_width_bh() {
        // BH portrait outer = 1+17 = 18. Gap = 1. Stats outer = 1+30 = 31.
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let portrait_outer = cols as u16 + 1;
        let gap = 1_u16;
        let stats_outer = 31_u16;
        let total = portrait_outer + gap + stats_outer;
        assert_eq!(total, 50, "BH Insights panel width should be 50 chars");
    }

    #[test]
    fn test_insights_panel_width_wh() {
        let (cols, _) = portrait_dims(Architecture::Wormhole);
        let portrait_outer = cols as u16 + 1;
        let gap = 1_u16;
        let stats_outer = 31_u16;
        let total = portrait_outer + gap + stats_outer;
        assert_eq!(total, 43, "WH Insights panel width should be 43 chars");
    }

    #[test]
    fn test_grid_cell_width_bh() {
        // Grid cell = portrait_cols + 2 (left-border + 1 gap)
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let cell_w = cols as u16 + 2;
        assert_eq!(cell_w, 19, "BH grid cell width should be 19 chars");
    }

    #[test]
    fn test_grid_cell_width_wh() {
        let (cols, _) = portrait_dims(Architecture::Wormhole);
        let cell_w = cols as u16 + 2;
        assert_eq!(cell_w, 12, "WH grid cell width should be 12 chars");
    }

    #[test]
    fn test_grid_cells_per_row_80_col_bh() {
        // At 80 columns, BH cells (19 wide) → 80/19 = 4 cells per row
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let cell_w = cols as u16 + 2;
        let terminal_w = 80_u16;
        let cells = terminal_w / cell_w;
        assert_eq!(cells, 4, "80-col terminal should fit 4 BH cells per row");
    }

    #[test]
    fn test_grid_cells_per_row_20_col_minimum() {
        // At 20 columns (very narrow), clamped to 1 cell minimum
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let cell_w = cols as u16 + 2;
        let terminal_w = 20_u16;
        let cells = (terminal_w / cell_w).max(1);
        assert_eq!(cells, 1, "very narrow terminal should still show 1 cell");
    }
```

- [ ] **Step 2: Run all tests**

```bash
cd /home/ttuser/code/tt-toplike && cargo test --lib 2>&1 | grep -E "test result|FAILED"
```

Expected: ≥ 154 tests, 0 failures.

- [ ] **Step 3: Build release to catch any missed features**

```bash
cd /home/ttuser/code/tt-toplike && cargo build --release --features tui,linux-procfs 2>&1 | grep "^error" | head -10
```

Expected: 0 errors.

- [ ] **Step 4: Final commit**

```bash
cd /home/ttuser/code/tt-toplike
git add src/ui/tui/chip_portrait.rs
git commit -m "test(ui): add layout math tests for Insights panel and Grid mode sizing"
```

- [ ] **Step 5: Verify full branch history looks clean**

```bash
cd /home/ttuser/code/tt-toplike && git log --oneline feature/chip-portrait-and-accuracy ^main
```

Expected output (9 commits):
```
<hash> test(ui): add layout math tests for Insights panel and Grid mode sizing
<hash> feat(ui): add DisplayMode::Grid with full-screen tiled chip portraits (key: g)
<hash> feat(ui): update Insights screen with split-panel portrait + stats sidebar
<hash> feat(ui): implement Chip Portrait renderer with BH/WH topology, strict width validation
<hash> feat(ui): add chip_portrait module with BH/WH topology classification (14 tests)
<hash> feat(backend/mock): add MockScenario with Galaxy/QuadGalaxy/WH fixtures; populate new fields
<hash> feat(backend/json): wire tt-smi 5.2.0 GDDR temps, ETH live, harvesting, firmwares, limits
<hash> feat(models): extend SmbusTelemetry with GDDR temps, ETH live status, harvesting fields
<hash> feat(models): add GddrTempPair, FirmwaresInfo, DeviceLimits + unpack helpers
```

---

## Self-Review Checklist

- [x] **Spec coverage**: All spec sections covered — data layer (Tasks 1-4), mock fixtures (Task 5), topology (Task 6), portrait renderer (Task 7), Insights screen (Task 8), Grid mode (Task 9), layout tests (Task 10).
- [x] **No placeholders**: Every code block is complete and compilable.
- [x] **Type consistency**: `GddrTempPair`, `FirmwaresInfo`, `DeviceLimits` defined in Task 1, used consistently in Tasks 2-8. `build_portrait_rows` defined in Task 7, tested in Tasks 7 and 10. `MockScenario` defined and used in Task 5.
- [x] **Width guarantee**: `build_portrait_rows` pushes exactly 1 char per cell via inner loop, tested with `UnicodeWidthStr::width()` to ≤ single-column confirmation. No right-border characters emitted anywhere.
- [x] **Backward compatibility**: All new fields are `Option` with `None` defaults; old tt-smi JSON parses without error (tested in Task 4 `test_old_format_still_parses_ok`).
- [x] **public/private visibility**: `parse_hex_or_dec` → `pub(crate) parse_hex_or_dec_pub` to avoid leaking internal helpers while still being usable from `json.rs`.
