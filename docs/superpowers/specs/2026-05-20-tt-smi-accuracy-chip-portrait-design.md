# Design: tt-smi 5.2.0 Accuracy + Chip Portrait Visualization

**Branch**: `feature/chip-portrait-and-accuracy`

---

## Overview

Four coordinated improvements to tt-toplike:

1. **Data layer**: Wire tt-smi 5.2.0's new `firmwares`, `limits`, extended `board_info`, and SMBUS fields into the data model.
2. **Chip Portrait widget**: Character-art core-grid visualization for Blackhole (17×12) and Wormhole (10×12) chips, with activity heat, harvesting state, GDDR temp coloring, and ETH live status.
3. **Insights screen refresh**: Each device gets a split panel — portrait on the left, stats sidebar on the right.
4. **Grid mode** (`DisplayMode::Grid`, key `g`): Full-screen tiled Chip Portraits for all devices.
5. **Tests**: Comprehensive coverage including quad-Galaxy (128-chip) mock fixtures.

---

## 1. Data Layer

### 1.1 New structs

**`FirmwaresInfo`** (in `src/models/telemetry.rs`):
```
fw_bundle_version: Option<String>   // "fw_pack-19.9.0"
eth_fw: Option<String>              // "6.16.0.0"
cm_fw: Option<String>               // "v80.15.0.0"
gddr_fw: Option<String>             // "1.37"
```

**`DeviceLimits`** (in `src/models/telemetry.rs`):
```
tdp_limit: Option<f32>        // watts
tdc_limit: Option<f32>        // amps
asic_fmax: Option<u32>        // MHz
thm_limit: Option<f32>        // °C
```

### 1.2 Extended `BoardInfo` (in `src/models/telemetry.rs`):
Add fields:
```
board_id: Option<String>
dram_status: Option<bool>      // from "DRAM Status: OK"
dram_speed: Option<String>     // "4266 MT/s"
pcie_speed: Option<String>     // "16.0 GT/s"
pcie_width: Option<u8>         // 16
```

### 1.3 Extended `SmbusTelemetry` (in `src/models/telemetry.rs`):
Add fields:
```
// GDDR temperature pairs (each packed as 4-byte hex: 0xAABBCCDD = [DD, CC, BB, AA] °C)
gddr_temps: Option<[Option<GddrTempPair>; 4]>   // GDDR_0_1 through GDDR_6_7
max_gddr_temp: Option<f32>

// GDDR errors
gddr_corr_errs: Option<[Option<u32>; 4]>
gddr_uncorr_errs: Option<u32>

// Core topology status
harvesting_state: Option<u32>       // HARVESTING_STATE bitmask
eth_live_status: Option<u64>        // ETH_LIVE_STATUS bitmask
enabled_eth: Option<u32>            // ENABLED_ETH bitmask
enabled_gddr: Option<u32>           // ENABLED_GDDR bitmask
enabled_l2cpu: Option<u32>          // ENABLED_L2CPU bitmask
enabled_tensix_col: Option<u32>     // 14-bit: one bit per Tensix column (BH)
```

**`GddrTempPair`**: `[f32; 4]` — four temperatures unpacked from one 4-byte hex register.

#### GDDR temp unpacking:
```
fn unpack_gddr_temps(hex_str: &str) -> Option<GddrTempPair>
  // "0x262a2c2c" → bytes [0x26, 0x2a, 0x2c, 0x2c] → [38.0, 42.0, 44.0, 44.0] °C
  // Parse as u32, extract bytes LE: b0=(v>>0)&0xFF, b1=(v>>8)&0xFF, b2=(v>>16)&0xFF, b3=(v>>24)&0xFF
  // Return None if string is missing or "N/A"
```

#### Harvesting column detection (Blackhole):
```
fn tensix_col_harvested(enabled_tensix_col: u32, col: usize) -> bool
  // col is 0-indexed among the 14 Tensix columns (not chip cols — excludes ETH/PCIe cols)
  // bit N clear → column N is harvested
  // Full 14-bit mask: all 1s = no harvesting
```

### 1.4 Device struct (in `src/models/device.rs`):
Add:
```
firmwares: Option<FirmwaresInfo>
limits: Option<DeviceLimits>
pcie_speed: Option<String>
pcie_width: Option<u8>
```

### 1.5 JSON backend wiring (`src/backend/json.rs`):
Add `TTSMIDeviceRaw` fields:
```
firmwares: Option<serde_json::Value>
limits: Option<serde_json::Value>
```
Deserialize these into `FirmwaresInfo` and `DeviceLimits` in `TTSMIDeviceRaw::to_device()`.

Map new SMBUS fields from the JSON keys:
- `GDDR_0_1_TEMP`, `GDDR_2_3_TEMP`, `GDDR_4_5_TEMP`, `GDDR_6_7_TEMP` → `gddr_temps`
- `GDDR_0_1_CORR_ERRS` ... → `gddr_corr_errs`
- `GDDR_UNCORR_ERRS` → `gddr_uncorr_errs`
- `MAX_GDDR_TEMP` → `max_gddr_temp`
- `HARVESTING_STATE` → `harvesting_state`
- `ETH_LIVE_STATUS` → `eth_live_status`
- `ENABLED_ETH`, `ENABLED_GDDR`, `ENABLED_L2CPU` → respective fields
- `ENABLED_TENSIX_COL` (parse existing `ENABLED_TENSIX_COL` into new dedicated field)

All new fields use `Option` and default to `None` when absent — backward compatible with older tt-smi output.

---

## 2. Chip Portrait Widget

### 2.1 Topology definitions (authoritative from tensix-viz `chip.js`)

**Blackhole** (17 cols × 12 rows, 0-indexed):
```
ETH:    col == 0 || col == 16
PCIe:   col == 8
DRAM:   row == 0 || row == 11
Tensix: everything else (140 cores total when unharvesteed)
```

**Wormhole** (10 cols × 12 rows, 0-indexed):
```
ETH:    row == 0 || row == 6
DRAM:   col == 0 || col == 5
Tensix: everything else (80 cores total)
```

GraySkull (10×12) is rendered the same as Wormhole for now (no ETH, no GDDR — shown as all-Tensix core grid).

### 2.2 Character assignments

| Core type | Character | Condition |
|-----------|-----------|-----------|
| Tensix (active ≥ 75%) | `▓` | activity from baseline learner |
| Tensix (active 40–75%) | `▒` | |
| Tensix (active < 40%) | `░` | |
| Tensix (harvested) | `·` | ENABLED_TENSIX_COL bit clear for that column |
| DRAM | `D` | colored by GDDR temp: teal→yellow→red (≤50°→≤70°→>70°C) |
| ETH (live) | `●` | ETH_LIVE_STATUS bit set |
| ETH (down/unknown) | `·` | ETH_LIVE_STATUS bit clear, or field absent |
| PCIe | `P` | Blackhole only |
| PCIe (active) | `P` | bright; dimmed when PCIe link not detected |

### 2.3 Portrait dimensions and character-spacing math

**CRITICAL**: All sizing must be verified to never overflow the bounding box. The portrait is purely text — no border chars on the right edge, per AGENTS.md.

**Blackhole portrait**: 17 cols × 12 rows = **17 chars wide, 12 chars tall** (no internal padding between cells).

**Wormhole portrait**: 10 cols × 12 rows = **10 chars wide, 12 chars tall**.

**With left border** (per AGENTS.md, left/bottom only):
- A bordered Blackhole portrait inner width = 17 chars.
- Outer box = `╔══` (17 `═` chars wide) + newline, then 12 body rows `║` + 17 chars + newline, then `╚══` (17 `═` chars wide) + newline.
- Total outer width = 1 (border char) + 17 (content) = **18 chars**.

**Stats sidebar** (paired with portrait on Insights screen):
- Fixed width: **30 chars** (label 12 + colon + space + value ≤ 16 chars).
- Total Insights panel width per device = 18 (portrait box) + 1 (gap) + 30 (stats) = **49 chars**.
- Minimum terminal width for Insights: 49 + 2 (outer padding) = **51 chars** — well within any modern terminal.

**Grid mode** (full-screen):
- Each portrait cell = portrait width + 2 (left border + right gap) = **20 chars** for BH, **13 chars** for WH.
- Row height = portrait rows + 1 (title row) = **13 lines** for both.
- Number of columns = `terminal_width / cell_width`, floored to a minimum of 1.
- Number of visible rows = `terminal_height / cell_height`, floored to a minimum of 1.
- No right-border characters ever emitted on portrait cells — the gap space serves as visual separation.

**Why no right border**: Terminal emulators use variable-width fonts for box-drawing chars in some configs; right-border chars at column N can wrap if the terminal is even 1 char narrower than expected. Left border only per AGENTS.md convention.

### 2.4 Activity calculation
- Use the existing `BaselineLearner` to compute a per-core activity ratio: `current_power / baseline_power` (clamped 0.0–1.0).
- In the absence of per-core power data (tt-smi provides total chip power), use the chip-level TDP utilization (`voltage * current / tdp_limit`) as a uniform scalar across all Tensix cells.
- Future: per-core granularity when available from a future tt-smi endpoint.

### 2.5 Memory particle animation
- Sparse animated chars (`·`, `╌`, `═`) stream along the NOC axes (horizontal rows first, then vertical column sweeps) at a cadence driven by estimated memory bandwidth.
- Driven by existing `MemFlowState` from the memory flow backend — reuse the same frame-tick mechanism already used in MemoryFlow mode.
- Particles are rendered as a second pass over the portrait buffer, overwriting the static core char for cells that have an active particle. This keeps the static layout as a stable baseline.

### 2.6 Implementation location
`src/ui/tui/chip_portrait.rs` — new file exporting:
```rust
pub fn render_chip_portrait(
    f: &mut Frame,
    area: Rect,
    device: &Device,
    telemetry: &Telemetry,
    tick: u64,
) -> Rect   // returns actual bounding rect used (for layout accounting)
```

---

## 3. Insights Screen

### 3.1 Layout per device
```
╔══════════════════   ╔══════════════════════════════
║   Chip Portrait     ║  Device: BH p150a [Dev 0]
║   (17×12 chars)     ║  ─────────────────────────
║                     ║  Power   ▓▓▓▓▓▓▓▓░░  145W/200W
╚══════════════════   ║  DDR     ████████░░  12.3 GB/s
                      ║  ETH     ●●●●·●·●    6/8 live
                      ║  GDDR T  38°→44°C    max 44°C
                      ║  Fan     ────────    2400 RPM
                      ║  Temp    38.2°C      (limit 105°C)
                      ║  FW      19.9.0      eth 6.16.0.0
                      ╚══════════════════════════════
```

Stats sidebar fields (in order):
1. Power: bar + watts / TDP limit
2. DDR: bandwidth bar (from existing DDR telemetry)
3. ETH: live-status dots + `N/M live` count
4. GDDR Temps: min→max range + `max NNN°C` colored (teal/yellow/red)
5. Fan: RPM (no bar — just numeric)
6. ASIC Temp: current °C + `(limit NNN°C)`
7. FW: `fw_bundle_version` + `eth fw_version`

All stat rows are padded to exactly 30 chars (left-justified value, right-padded with spaces). No right-border character.

### 3.2 Device card scroll
Multiple devices scroll vertically; each card height = max(portrait_height, stats_rows) + 2 (borders) + 1 (gap) = 15 lines per device max.

---

## 4. Grid Mode

### 4.1 Key binding
`g` toggles `DisplayMode::Grid`. Added to the existing `DisplayMode` enum in `src/ui/tui/mod.rs`.

### 4.2 Layout
- Title bar at top: `tt-toplike  [Grid]  N devices  |  press g to return`
- Device chips tiled left-to-right, top-to-bottom, wrapping when `terminal_width` is exceeded.
- Each cell:
  ```
  ╔══════════════════   (no right border)
  ║ Dev0 BH p150a
  ║ [17×12 portrait]
  ╚══════════════════
  ```
  Cell width = 1 (`║`) + max(portrait_cols, label_len) chars.
  Cell height = 1 (top border) + 1 (label row) + portrait_rows + 1 (bottom border) = 15 lines for BH.

### 4.3 Sizing guarantee
The grid renderer must compute `cell_width` and `cell_height` once per frame tick, then render only `cols * rows` cells that fully fit. No partial cells ever emitted — a cell is either fully rendered or not rendered. This prevents right-edge wrapping artifacts.

---

## 5. Mock Backend Updates (`src/backend/mock.rs`)

### 5.1 New fixture scenarios

| Scenario | Chips | Architecture | Purpose |
|----------|-------|--------------|---------|
| `MockScenario::Galaxy` | 32 | Blackhole | Single Galaxy system |
| `MockScenario::QuadGalaxy` | 128 | Blackhole | 4× Galaxy systems — max scale test |
| `MockScenario::WormholeCluster` | 32 | Wormhole | Pure-WH regression |
| `MockScenario::Mixed` | 8 | BH + WH + GS | Mixed-arch test |

### 5.2 New mock data fields

`generate_smbus_telemetry()` in mock.rs must produce:
- `gddr_temps`: 4 pairs, values `0x262a2c2c`-style hex strings cycling with `tick`
- `max_gddr_temp`: max of above values
- `eth_live_status`: random u64 with ~75% bits set (most links live)
- `harvesting_state`: 0 (no harvesting) by default; QuadGalaxy fixture randomly harvests 1–2 columns per chip for realistic variation
- `enabled_tensix_col`: `0x3FFF` (all 14 columns enabled) by default; mirrors `harvesting_state`
- `firmwares`: `fw_bundle_version = "fw_pack-19.9.0"`, realistic eth/cm/gddr versions
- `limits`: `tdp_limit = 300.0`, `tdc_limit = 40.0`, `asic_fmax = 800`, `thm_limit = 105.0`

---

## 6. Tests

### 6.1 Data layer tests (`src/backend/json_tests.rs` or inline `#[cfg(test)]`)

| Test | What it checks |
|------|---------------|
| `test_gddr_temp_unpack_normal` | `"0x262a2c2c"` → `[38.0, 42.0, 44.0, 44.0]` |
| `test_gddr_temp_unpack_zero` | `"0x00000000"` → `[0.0, 0.0, 0.0, 0.0]` |
| `test_gddr_temp_unpack_na` | `"N/A"` → `None` |
| `test_harvesting_col_detection` | mask `0x3FFE` → col 0 harvested, cols 1-13 active |
| `test_harvesting_col_all_clear` | mask `0x0000` → all 14 cols harvested |
| `test_eth_live_status_parsing` | `"0x00000000000000FF"` → first 8 ETH ports live |
| `test_firmwares_info_parse` | JSON `{"fw_bundle_version": "fw_pack-19.9.0", ...}` → `FirmwaresInfo` |
| `test_limits_parse` | JSON `{"tdp_limit": 300.0, ...}` → `DeviceLimits` |
| `test_new_smbus_fields_round_trip` | Full tt-smi 5.2.0 JSON fixture → all new SMBUS fields populated |
| `test_missing_new_fields_ok` | Older tt-smi JSON (no firmwares/limits) → parses without error, fields are None |

### 6.2 Chip Portrait tests (`src/ui/tui/chip_portrait_tests.rs`)

| Test | What it checks |
|------|---------------|
| `test_bh_portrait_dimensions` | Portrait output is exactly 17 chars wide, 12 lines tall |
| `test_wh_portrait_dimensions` | Portrait output is exactly 10 chars wide, 12 lines tall |
| `test_bh_core_types_all_positions` | Every (col,row) pair classifies correctly (ETH/PCIe/DRAM/Tensix) |
| `test_wh_core_types_all_positions` | Every (col,row) pair classifies correctly |
| `test_harvested_col_shows_dot` | Harvested column shows `·` not `▓`/`▒`/`░` |
| `test_eth_live_shows_bullet` | ETH_LIVE bit set → `●`, clear → `·` |
| `test_gddr_temp_color_teal` | GDDR temp ≤ 50°C → teal cell color |
| `test_gddr_temp_color_yellow` | GDDR temp 51–70°C → yellow cell color |
| `test_gddr_temp_color_red` | GDDR temp > 70°C → red cell color |
| `test_portrait_no_right_overflow` | No line in portrait exceeds declared width |

### 6.3 Mock backend tests (`src/backend/mock_tests.rs`)

| Test | What it checks |
|------|---------------|
| `test_galaxy_mock_has_32_devices` | `MockScenario::Galaxy` produces exactly 32 Device entries |
| `test_quad_galaxy_mock_has_128_devices` | `MockScenario::QuadGalaxy` produces exactly 128 |
| `test_quad_galaxy_all_blackhole` | All 128 devices are `Architecture::Blackhole` |
| `test_wormhole_cluster_mock` | 32 devices, all `Architecture::Wormhole` |
| `test_mock_firmwares_populated` | All devices have `firmwares.fw_bundle_version` non-None |
| `test_mock_limits_populated` | All devices have `limits.tdp_limit` non-None |
| `test_mock_gddr_temps_valid` | GDDR temps within 0–100°C range |
| `test_mock_harvesting_in_quad_galaxy` | Some chips have non-zero `harvesting_state` |

### 6.4 Rendering/layout tests (`src/ui/tui/layout_tests.rs`)

| Test | What it checks |
|------|---------------|
| `test_insights_panel_width` | Panel is exactly 49 chars wide for BH device |
| `test_insights_panel_width_wh` | Panel is exactly 42 chars wide for WH device (10+1+30=41+border=42) |
| `test_grid_mode_no_partial_cells` | Grid at 80-char terminal renders only fully fitting cells |
| `test_grid_mode_minimum_one_cell` | Grid at 20-char terminal renders exactly 1 cell (clamped) |

---

## 7. File Changes Summary

| File | Action |
|------|--------|
| `src/models/telemetry.rs` | Add `FirmwaresInfo`, `DeviceLimits`, `GddrTempPair`, extend `SmbusTelemetry` and `BoardInfo` |
| `src/models/device.rs` | Add `firmwares`, `limits`, `pcie_speed`, `pcie_width` to `Device` |
| `src/backend/json.rs` | Wire new fields from `TTSMIDeviceRaw` deserialization |
| `src/backend/mock.rs` | Add new scenarios and new field generation |
| `src/ui/tui/chip_portrait.rs` | New file: `render_chip_portrait()` |
| `src/ui/tui/mod.rs` | Add `DisplayMode::Grid`, key binding `g`, integrate portrait into Insights, add Grid render fn |
| `tests/` | New test modules for above |

---

## 8. Out of Scope

- Per-core power/activity from any future tt-smi endpoint (uniform scalar for now).
- GraySkull full topology (rendered as generic core grid, no ETH/GDDR specifics).
- NOC path visualization beyond the simplified particle animation already designed.
- Windows/macOS backend changes.
