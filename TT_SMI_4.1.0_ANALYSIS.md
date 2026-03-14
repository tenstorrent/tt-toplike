# tt-smi 4.1.0+ Enhanced Data Stream Analysis

**Date**: March 13, 2026  
**tt-smi Version**: 4.1.0  
**Status**: Safe to run during load (confirmed by Tenstorrent)

## Overview

tt-smi 4.1.0+ provides significantly richer data than previous versions and is now **safe to run concurrently with active workloads** (LLMs, training, inference). This enables combining sysfs (safe, always available) with tt-smi (rich, detailed) for the best of both worlds.

## Data Structure Comparison

### What We Currently Use (Sysfs)
- Temperature (ASIC only)
- Voltage
- Power (if exposed by hwmon driver)
- Current (calculated from P/V or direct)
- Device count and basic identification

### What tt-smi 4.1.0 Adds

#### 1. Host Information (NEW)
```json
"host_info": {
    "OS": "Linux",
    "Distro": "Ubuntu 24.04.4 LTS",
    "Kernel": "6.17.0-14-generic",
    "Hostname": "tt-quietbox",
    "Platform": "x86_64",
    "Python": "3.12.3",
    "Memory": "249.32 GB",
    "Driver": "TT-KMD 2.7.0"
}
```
**Use Cases:**
- Display system info in header
- Detect driver version for compatibility warnings
- Show total system memory context

#### 2. Software Versions (NEW)
```json
"host_sw_vers": {
    "tt_smi": "4.1.0",
    "pyluwen": "0.8.1",
    "tt_umd": "0.9.1"
}
```
**Use Cases:**
- Version compatibility checks
- Display in about/info screen
- Warn if versions are mismatched

#### 3. Enhanced Board Info
```json
"board_info": {
    "bus_id": "0000:04:00.0",
    "board_type": "p300c",
    "board_id": "0000046131924055",
    "coords": "N/A",
    "dram_status": true,          // ← NEW: Boolean health
    "dram_speed": "16G",          // ← NEW: Human-readable
    "pcie_speed": 5,              // ← NEW: PCIe Gen
    "pcie_width": "4"             // ← NEW: PCIe lanes
}
```
**Use Cases:**
- DRAM health indicator (red if false)
- PCIe bottleneck detection (should be Gen5 x4)
- Display link speed in status bar

#### 4. Enhanced Telemetry (Clean Decimals)
```json
"telemetry": {
    "voltage": "0.86",            // Volts (decimal, not hex)
    "current": " 94.0",           // Amps
    "power": " 80.0",             // Watts
    "aiclk": "1350",              // MHz
    "asic_temperature": "73.2",   // Celsius
    "fan_speed": " 38",           // Percentage
    "heartbeat": "1156"           // Timer value
}
```
**Use Cases:**
- Direct parsing (no hex conversion)
- Fan speed visualization
- Heartbeat liveness indicator

#### 5. Firmware Versions (NEW - Critical!)
```json
"firmwares": {
    "fw_bundle_version": "19.6.0.0",
    "tt_flash_version": "N/A",
    "cm_fw": "0.28.0.0",
    "cm_fw_date": "2020-00-28",
    "eth_fw": "0.0.0",
    "dm_bl_fw": "0.0.0.0",
    "dm_app_fw": "0.22.0.0"
}
```
**Use Cases:**
- Display in device details
- Warn if firmware is outdated
- Match against known good versions

#### 6. Limits and Thresholds (NEW - Critical!)
```json
"limits": {
    "vdd_min": "0.70",           // Minimum safe voltage
    "vdd_max": "0.90",           // Maximum safe voltage
    "tdp_limit": "125",          // Thermal Design Power (Watts)
    "tdc_limit": "500",          // Thermal Design Current (Amps)
    "asic_fmax": "1350",         // Maximum frequency (MHz)
    "therm_trip_l1_limit": 0,
    "thm_limit": "110",          // Thermal limit (°C)
    "bus_peak_limit": 0
}
```
**Use Cases:**
- **WARNING ZONES**: Color-code telemetry approaching limits
  - Power approaching TDP (125W) → orange → red
  - Current approaching TDC (500A) → orange → red
  - Temp approaching thm_limit (110°C) → orange → red
- Display limits in gauges/meters
- Safety margin calculations

#### 7. Enhanced SMBUS (New Fields)
```json
"ENABLED_TENSIX_COL": "0x3fdd",  // Bitmask of active Tensix columns
"ENABLED_ETH": "0x3edf",         // Bitmask of active Ethernet cores
"ENABLED_GDDR": "0xff",          // Bitmask of active GDDR channels
"ENABLED_L2CPU": "0xf",          // Bitmask of active L2 CPUs
"PCIE_USAGE": "0x1",             // PCIe utilization indicator
"FAN_RPM": "0x75a",              // Actual fan RPM (not just %)
"ASIC_ID_HIGH": "0x89e991bd",    // Unique chip ID (high)
"ASIC_ID_LOW": "0xb13e022e",     // Unique chip ID (low)
```
**Use Cases:**
- **Memory Castle Enhancement**: Show only enabled Tensix columns
- Harvesting visualization (disabled cores shown as dark)
- Unique chip identification
- Fan RPM alongside percentage

## Visualization Enhancements Enabled

### 1. Warning Zones in All Views
Current telemetry displays can now show safety margins:
- **Green**: Well below limits (<70% of limit)
- **Yellow**: Approaching limits (70-90% of limit)
- **Orange**: Near limits (90-95% of limit)
- **Red**: At or exceeding limits (>95% of limit)

Example: Power display
- 50W of 125W TDP = Green (40%)
- 90W of 125W TDP = Yellow (72%)
- 115W of 125W TDP = Orange (92%)
- 120W of 125W TDP = Red (96%)

### 2. Enhanced Memory Castle
**Current**: Shows all Tensix grid positions
**Enhanced**: Show only enabled cores (from ENABLED_TENSIX_COL bitmask)
- Disabled cores: dim/dark
- Enabled cores: bright with activity

**Blackhole Example**: 14×16 grid (224 cores)
- ENABLED_TENSIX_COL = 0x3fdd
- Binary: 0011 1111 1101 1101 (columns 0,2,3,4,5,6,7,8,9,10,11,12,13 enabled)
- Column 1 disabled (harvested)

### 3. PCIe Link Status
Display in header/footer:
```
PCIe: Gen5 x4 ✓  (expected for P300C)
PCIe: Gen3 x4 ⚠  (bottleneck warning)
PCIe: Gen5 x1 ⚠  (lane degradation warning)
```

### 4. DRAM Health Indicator
```
DRAM: ✓ 16G  (healthy)
DRAM: ✗ 16G  (failed - requires attention)
```

### 5. Firmware Display
Show in expanded device view or footer:
```
FW: 19.6.0.0 | CM: 0.28.0.0 | DM: 0.22.0.0
```

### 6. Fan Monitoring
Current: Not displayed
Enhanced:
```
Fan: 38% (1882 RPM)
```

### 7. System Info Bar
Top header can now show:
```
tt-quietbox | KMD 2.7.0 | tt-smi 4.1.0 | 4 devices
```

## Implementation Strategy

### Phase 1: Enhanced JSON Backend
Update `src/backend/json.rs` to parse new 4.1.0 format:
1. Parse `host_info` and `host_sw_vers` (store in backend struct)
2. Parse enhanced `board_info` (PCIe, DRAM status)
3. Parse `firmwares` section
4. Parse `limits` section
5. Parse new SMBUS fields (ENABLED_*, FAN_RPM, ASIC_ID)

### Phase 2: Extended Data Models
Update `src/models/`:
1. Add `HostInfo` struct
2. Add `SoftwareVersions` struct
3. Add `Firmware` struct
4. Add `Limits` struct
5. Extend `BoardInfo` with PCIe and DRAM fields
6. Extend `SmbusTelemetry` with new fields

### Phase 3: Hybrid Backend (Sysfs + JSON)
Create new `HybridBackend`:
- Primary: Sysfs (always safe, fast, low overhead)
- Secondary: tt-smi 4.1.0 (when available, for enhanced data)
- Merge strategy:
  - Core telemetry from Sysfs (lower latency)
  - Enhanced fields from tt-smi (limits, firmware, etc.)
  - Detect tt-smi version on init
  - Graceful degradation if tt-smi unavailable

### Phase 4: Enhanced Visualizations
1. **TUI Table**: Add columns for PCIe status, DRAM health, limits
2. **Memory Castle**: 
   - Dim disabled Tensix columns (from bitmask)
   - Show limit warnings with color coding
   - Display firmware version in castle header
3. **Starfield**: Color-code stars by limit proximity (not just temp)
4. **GUI Dashboard**: Add limit gauges, PCIe status, firmware info

## Version Detection Logic

```rust
// Detect tt-smi version on init
fn detect_tt_smi_version() -> Option<(u32, u32, u32)> {
    let output = Command::new("tt-smi")
        .arg("--version")
        .output()
        .ok()?;
    
    let version_str = String::from_utf8_lossy(&output.stdout);
    // Parse "4.1.0" → (4, 1, 0)
    parse_version(&version_str)
}

// Enable enhanced features if 4.1.0+
let enhanced = detect_tt_smi_version()
    .map(|(major, minor, _)| major >= 4 && minor >= 1)
    .unwrap_or(false);
```

## Backward Compatibility

Must support:
- **tt-smi < 4.1.0**: Use existing JSON backend (legacy format)
- **tt-smi 4.1.0+**: Use enhanced JSON backend (new format)
- **No tt-smi**: Fall back to Sysfs or Mock

## Safety Guarantees

tt-smi 4.1.0+ is **confirmed safe** for:
- Concurrent LLM inference
- Active training workloads
- Multiple simultaneous readers
- High-frequency polling (10 Hz recommended, 100 Hz possible)

This removes the previous limitation where we needed Luwen for detailed data but couldn't use it during active workloads.

## Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Update latency | <50ms | tt-smi subprocess + JSON parse |
| CPU overhead | <2% | Per device at 10 Hz polling |
| Memory | <5MB | Parsed JSON structures |
| Interference | Zero | Confirmed by Tenstorrent |

## Next Steps

1. ✅ Analyze tt-smi 4.1.0 output (this document)
2. ⏳ Update JSON backend for 4.1.0 format
3. ⏳ Create extended data models
4. ⏳ Implement hybrid Sysfs+JSON backend
5. ⏳ Add limit-aware visualizations
6. ⏳ Add PCIe/DRAM status displays
7. ⏳ Add firmware version displays
8. ⏳ Test with active LLM workload

## Files to Modify

| File | Changes |
|------|---------|
| `src/backend/json.rs` | Parse 4.1.0 format with new sections |
| `src/models/device.rs` | Add HostInfo, SoftwareVersions, extended BoardInfo |
| `src/models/telemetry.rs` | Add Firmware, Limits structs |
| `src/backend/hybrid.rs` | New file: Sysfs+JSON combined backend |
| `src/animation/tron_grid.rs` | Use ENABLED_TENSIX_COL for Memory Castle |
| `src/ui/tui/mod.rs` | Add PCIe, DRAM, limit columns |
| `src/ui/gui/visualization.rs` | Add limit gauges, firmware display |

