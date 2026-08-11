# Sysfs Backend - Non-Invasive Hardware Monitoring

## Overview

The Sysfs backend provides **non-invasive hardware monitoring** by reading tt-kmd's sysfs surfaces. This backend is specifically designed for monitoring Tenstorrent hardware that's actively running workloads (LLMs, training, inference) without interfering with operations.

tt-kmd exposes **two** sysfs surfaces, and this backend reads both:

1. **hwmon** — `/sys/class/hwmon/hwmonN/`: the standard Linux sensor interface (temperature, voltage, power, current, fan, and `*_max` limits).
2. **The tt-kmd class-attribute directory** — `/sys/class/tenstorrent/tenstorrent!N/`: Tenstorrent-specific attributes (`tt_aiclk`, `tt_heartbeat`, `tt_card_type`, …) plus a `pcie_perf_counters/` subdirectory of PCIe link counters. It is located by resolving each hwmon directory's `device` symlink to the PCI device directory and looking for `<pci-dir>/tenstorrent/<subdir>` — resolving through the PCI device pins the hwmon↔class-device correlation to the same physical card.

Reading the second surface is what closed most of this backend's historical telemetry gap: clocks, ARC firmware heartbeat, board SKU, firmware bundle version, thermal-trip count, and live PCIe bandwidth all come from there. Both surfaces are world-readable ordinary files — no device open, no ARC message, no PCI mapping.

## Why Sysfs?

### The Problem
- **Luwen backend** requires direct PCI BAR0 memory mapping
- Active workloads (LLM serving, training) lock PCI resources exclusively
- Even `noc_safe` mode fails with BAR0 mapping errors
- Direct hardware access conflicts with running processes

### The Solution
- **Kernel-mediated access** through Linux hwmon subsystem
- **Zero PCI interference** - reads from kernel-maintained buffers
- **Multiple concurrent readers** supported by kernel
- **No special permissions** required (world-readable sysfs files)

## Usage

### Quick Start

```bash
# Explicitly use sysfs backend (fastest)
./target/debug/tt-toplike-gui --backend sysfs
./target/debug/tt-toplike-tui --backend sysfs

# Auto-detect starts with the Hybrid backend, which *is* this backend plus
# background tt-smi enrichment (Luwen is never auto-detected)
./target/debug/tt-toplike-gui
```

### When to Use Sysfs

✅ **Use Sysfs When**:
- Hardware is running active workloads (LLMs, training, inference)
- You don't have root access
- Luwen backend panics with BAR0 mapping errors
- You need guaranteed non-invasive monitoring
- Multiple monitoring tools need to run simultaneously

❌ **Don't Use Sysfs When**:
- Hardware is idle and you want the *full* SMBUS block (see "Telemetry NOT Available" below)
- You require DDR training status or per-channel memory data
- You need GDDR temperatures, ECC counters, or per-ARC firmware health
- Your driver predates tt-kmd 2.7 (no class attributes — the backend falls back to hwmon-only)

## What's Available

### Telemetry Provided ✅

| Metric | Source | Notes |
|--------|--------|-------|
| **Temperature** | hwmon `temp*_input` (prefers `temp*_label` = `asic_temp`) | ASIC temperature in °C |
| **Voltage** | hwmon `in*_input` (prefers label `vcore`) | VCore voltage in V |
| **Power** | hwmon `power*_input` (prefers label `power`) | Power consumption in W |
| **Current** | hwmon `curr*_input` (prefers label `current`), else calculated | Amperes; falls back to P/V when the driver has no current sensor |
| **Fan speed** | hwmon `fan*_input` (prefers label `fan_rpm`) | RPM; the all-ones sentinel (`0xFFFFFFFF`) on fanless cards is filtered downstream |
| **Power / current / thermal limits** | hwmon `power1_max`, `curr1_max`, `temp1_max` | Real per-board ceilings (a p300c reports 125 W / 500 A / 90 °C) instead of the UI's hardcoded 300 W / 105 °C fallbacks. Requires **tt-kmd ≥ 2.9.0** on Blackhole |
| **AICLK** | class attr `tt_aiclk` | Read every tick into `Telemetry::aiclk` |
| **AXICLK / ARCCLK** | class attrs `tt_axiclk`, `tt_arcclk` | Into the synthesized SMBUS block |
| **ARC firmware heartbeat** | class attr `tt_heartbeat` | Into `Telemetry::heartbeat` *and* `SmbusTelemetry::arc0_health` — the same firmware counter tt-smi reports as `TIMER_HEARTBEAT` |
| **Board SKU** | class attr `tt_card_type` | The real product name (e.g. `p300c`), not just the hwmon arch name. **Replaces** the old ~1.2 s `tt-smi -s` startup probe on modern kmd |
| **Firmware bundle version** | class attr `tt_fw_bundle_ver` | Fills `Device::firmwares.fw_bundle_version`, so the Insights FW row works in sysfs-only mode |
| **Board serial** | class attr `tt_serial` | Into `SmbusTelemetry::board_id` |
| **Thermal-trip count** | class attr `tt_therm_trip_count` | Lifetime hardware-shutdown counter; Insights shows a row when non-zero |
| **M3 app firmware version** | class attr `tt_m3app_fw_ver` | Best-effort; absent on some cards |
| **PCIe bandwidth** | class subdir `pcie_perf_counters/` | Twelve data-word counters folded into in/out directions and differentiated between ticks → bytes/sec each way |
| **Device count** | hwmon discovery | All Tenstorrent devices, **ordered by PCI bus id** |
| **Architecture** | hwmon `name` | Grayskull / Wormhole / Blackhole from the name string |

`smbus_telemetry()` therefore no longer returns `None` on a modern driver — it returns a **partial**
`SmbusTelemetry` synthesized from the class attributes plus the hwmon fan reading. On a driver without
the class-attribute directory it stays `None`, exactly as before.

### Telemetry NOT Available ❌

These fields are absent from *both* sysfs surfaces, so `synthesize_smbus` leaves them `None`. Use the
JSON (`tt-smi`) or hybrid backend if you need them:

| Metric | Why Not Available |
|--------|-------------------|
| **DDR training status** | The per-channel `DDR_STATUS` bitmask is an SMBUS register; the driver doesn't publish it |
| **GDDR temperatures & ECC counters** | SMBUS/ARC telemetry tags only |
| **ARC1–3 health** | Only the single aggregate `tt_heartbeat` counter is exposed |
| **Per-component firmware versions** | Only the bundle version (`tt_fw_bundle_ver`) is exposed — not the individual ARC / Ethernet / SPI versions |
| **Board & VReg temperatures** | Only the ASIC sensor is published via hwmon |
| **PCIe link geometry (GenN ×width)** | Not in sysfs — the Insights PCIe row gets this from `tt-smi`'s `board_info`, so sysfs-only mode shows live rates without the geometry line |
| **Ethernet link status / harvesting masks** | ARC telemetry only |
| **`asic_fmax`** | Not an hwmon limit attribute; the JSON limits block has it |

## How It Works

### Device Detection

1. Scans `/sys/class/hwmon/` for all hwmon devices
2. Reads `name` file from each hwmon directory
3. Looks for Tenstorrent-related names:
   - `"tenstorrent"` / `"tt_*"`
   - `"grayskull"`
   - `"wormhole"`
   - `"blackhole"`
4. **Sorts the candidates by PCI bus id**, then assigns device indices in that order
5. Creates device entries with hwmon path mappings, and resolves each device's tt-kmd class-attribute directory

Step 4 matters more than it looks. `readdir` returns hwmon entries in filesystem order, which is
neither hwmon-number nor bus-id order and isn't stable across boots (measured on a 4× Blackhole box:
04:00.0, 03:00.0, 01:00.0, 02:00.0). The hybrid backend joins `tt-smi` metadata and SMBUS telemetry
onto these devices **by index**, and `tt-smi -s` emits `device_info[]` ascending by bus id — so
assigning indices in readdir order made every card display another card's data. Candidates whose bus
id can't be resolved sort last, by hwmon number, so they still get a stable index.

### Sensor Reading

For each hwmon device, sensors are discovered **once** at init and then read directly each tick.
Discovery prefers the input file whose `*_label` sibling matches the expected sensor, and falls back
to the lowest-numbered existing input for older/unlabeled drivers:

| Sensor | Files scanned | Preferred label | Unit conversion |
|--------|---------------|-----------------|-----------------|
| Temperature | `temp1_input` … `temp8_input` | `asic_temp` | m°C → °C |
| Voltage | `in0_input` … `in8_input` | `vcore` | mV → V |
| Power | `power1_input` … `power8_input` | `power` | µW → W |
| Current | `curr1_input` … `curr8_input` | `current` | mA → A |
| Fan | `fan1_input` … `fan4_input` | `fan_rpm` | RPM (verbatim) |

Label-awareness fixes a real failure mode: a card can expose several sensors of the same class at
different indices (e.g. `temp1 = vreg_temp`, `temp2 = asic_temp`), and "lowest index wins" silently
picked the wrong one.

Limits come from the matching `*_max` files (`power1_max`, `curr1_max`, `temp1_max`) at init.

### tt-kmd Class Attributes

Each hwmon directory's `device` symlink is canonicalized to the PCI device directory, and the
`tenstorrent/<subdir>` beneath it is the class-attribute directory. Attributes are read in two rhythms:

```bash
# Static — read once at init
tt_card_type        # "p300c"            → Device::board_type (replaces the tt-smi probe)
tt_fw_bundle_ver    # "19.11.0.0"        → Device::firmwares.fw_bundle_version

# Dynamic — read every tick
tt_aiclk            # "800"   (MHz)      → Telemetry::aiclk
tt_heartbeat        # "43874"            → Telemetry::heartbeat + SmbusTelemetry::arc0_health
tt_axiclk           # "960"   (MHz)      → SmbusTelemetry::axiclk
tt_arcclk           # "800"   (MHz)      → SmbusTelemetry::arcclk
tt_therm_trip_count # "0"                → SmbusTelemetry::therm_trip_count
tt_serial           # "0000046131924062" → SmbusTelemetry::board_id
tt_m3app_fw_ver     #                    → SmbusTelemetry::m3_app_fw_version
```

If **any** device is missing `tt_card_type` (an older driver), the backend falls back to the bounded
`tt-smi -s` board-type probe it used before — the same graceful degradation as when tt-smi is absent.

### PCIe Bandwidth

`<class-dir>/pcie_perf_counters/` holds twelve monotonically-increasing counters of 32-bit data words
crossing the PCIe link, split by direction and initiator. They are folded into two totals:

- **Into the chip** — `slv_{posted,nonposted}_wr_data_word_received*` (host writes to the device) plus
  `mst_rd_data_word_received*` (device reading host memory).
- **Out of the chip** — `slv_rd_data_word_sent*` (host reads from the device) plus
  `mst_{posted,nonposted}_wr_data_word_sent*` (device writing host memory).

Each name has a `0` and `1` suffix (two PCIe controllers); both are summed. Bandwidth is
`Δwords × 4 bytes ÷ Δt` between successive ticks — so the *first* tick after startup primes the
tracker and reports nothing, and a counter that goes backwards (device reset) clamps its delta to
zero rather than spiking. Reading these files is passive: no device open, no ARC message.

This is surfaced through `TelemetryBackend::pcie_bandwidth()`, which defaults to `None` and is
implemented only by the sysfs and hybrid backends — the JSON backend has no access to the counter files.

### PCI Address Extraction

Attempts to extract PCI bus address from sysfs device symlinks:

```bash
# Read device symlink
/sys/class/hwmon/hwmon6/device → ../../../0000:04:00.0

# Parse PCI address pattern: 0000:04:00.0
```

The **last** PCI-looking component wins, not the first: a longer link such as
`.../pci0000:00/0000:00:01.5/0000:04:00.0/hwmon/hwmon6` walks down through the upstream bridge
(`0000:00:01.5`) before reaching the card, and it's the leaf that identifies the device. Taking the
first component would also collapse several cards onto one bus id and break the bus-id ordering above.

## Manual Inspection

### Find Tenstorrent Devices

```bash
# List all hwmon devices
ls -la /sys/class/hwmon/

# Check device names
for dir in /sys/class/hwmon/hwmon*/; do
    echo "$dir: $(cat $dir/name 2>/dev/null || echo 'unknown')"
done

# Example output:
# /sys/class/hwmon/hwmon1/: blackhole
# /sys/class/hwmon/hwmon3/: blackhole
```

### Read Sensor Values

```bash
# Temperature (millicelsius)
cat /sys/class/hwmon/hwmon1/temp1_input
# Example: 45000 (= 45.0°C)

# Voltage (millivolts)
cat /sys/class/hwmon/hwmon1/in0_input
# Example: 850 (= 0.85V)

# Power (microwatts, if available)
cat /sys/class/hwmon/hwmon1/power1_input 2>/dev/null || echo "Not available"
# Example: 125000000 (= 125W)

# Current (milliamps, if available)
cat /sys/class/hwmon/hwmon1/curr1_input 2>/dev/null || echo "Not available"
# Example: 85000 (= 85A)

# Firmware limits (the real per-board ceilings, tt-kmd >= 2.9.0)
cat /sys/class/hwmon/hwmon1/power1_max   # 125000000 (= 125W)
cat /sys/class/hwmon/hwmon1/curr1_max    # 500000    (= 500A)
cat /sys/class/hwmon/hwmon1/temp1_max    # 90000     (= 90C)
```

### Read tt-kmd Class Attributes

```bash
# Every device the driver has registered
ls -d /sys/class/tenstorrent/tenstorrent!*

# Or reach the one belonging to a given hwmon node (what the backend does)
ls "$(readlink -f /sys/class/hwmon/hwmon1/device)"/tenstorrent/*/

# Board SKU, serial and firmware bundle (static)
cat /sys/class/tenstorrent/tenstorrent\!0/tt_card_type      # p300c
cat /sys/class/tenstorrent/tenstorrent\!0/tt_serial         # 0000046131924062
cat /sys/class/tenstorrent/tenstorrent\!0/tt_fw_bundle_ver  # 19.11.0.0

# Clocks, ARC heartbeat and thermal trips (change every tick)
cat /sys/class/tenstorrent/tenstorrent\!0/tt_aiclk            # 800
cat /sys/class/tenstorrent/tenstorrent\!0/tt_axiclk           # 960
cat /sys/class/tenstorrent/tenstorrent\!0/tt_arcclk           # 800
cat /sys/class/tenstorrent/tenstorrent\!0/tt_heartbeat        # 43874 (increments while ARC is alive)
cat /sys/class/tenstorrent/tenstorrent\!0/tt_therm_trip_count # 0

# PCIe data-word counters (monotonic; differentiate to get bandwidth)
ls /sys/class/tenstorrent/tenstorrent\!0/pcie_perf_counters/
```

### Check Available Attributes

```bash
# List all sensor files for a device
ls -la /sys/class/hwmon/hwmon1/

# Common files:
# - name           : Device name
# - temp*_input    : Temperature sensors
# - temp*_label    : Sensor labels (used to pick the ASIC sensor)
# - temp*_max      : Firmware thermal limit (tt-kmd >= 2.9.0)
# - temp*_crit     : Critical temperature threshold (optional)
# - in*_input      : Voltage sensors
# - power*_input   : Power sensors (if available)
# - curr*_input    : Current sensors (if available)
# - fan*_input     : Fan RPM (0xFFFFFFFF on fanless cards)
```

## Performance

### Latency
- **Read time**: <1ms per device (simple file reads)
- **Update rate**: Configurable (default 100ms = 10 Hz)
- **Sensor count**: Typically 1-4 sensors per device

### CPU Usage
- **Idle**: <0.5% (minimal overhead)
- **Active**: <1% with 10 Hz updates
- **Scalability**: Linear with device count

### Memory
- **Per-device**: ~2KB (paths + telemetry cache)
- **Total**: <10KB for typical systems

### Comparison

| Backend | Latency | CPU  | Permissions | Works on Active HW |
|---------|---------|------|-------------|--------------------|
| Sysfs   | <1ms    | <1%  | None        | ✅ Yes             |
| Luwen   | <1ms    | <1%  | root/ttkmd  | ❌ No (panics)     |
| JSON    | ~50ms   | ~3%  | None        | ✅ Yes (if tt-smi) |
| Mock    | <1ms    | <1%  | None        | N/A (simulated)    |

## Troubleshooting

### No Devices Found

```bash
$ ./target/debug/tt-toplike-gui --backend sysfs
Error: No Tenstorrent devices found in hwmon
```

**Causes**:
1. Kernel driver not loaded
2. Hwmon support not enabled in driver
3. Device naming doesn't match patterns

**Solutions**:
```bash
# Check if hwmon entries exist
ls -la /sys/class/hwmon/

# Check device names
cat /sys/class/hwmon/hwmon*/name

# Look for PCI devices
lspci | grep -i tenstorrent

# Check kernel modules
lsmod | grep -i tt
```

### Sensor Values Missing

```bash
# Temperature shows but power doesn't
```

**Cause**: Driver doesn't expose power sensors via hwmon

**Solutions**:
- Use Luwen backend (requires idle hardware + permissions)
- Use JSON backend (requires tt-smi)
- Check driver documentation for available sensors

### Incorrect Values

```bash
# Temperature shows 0.0°C or unrealistic values
```

**Causes**:
1. Sensor not calibrated
2. Driver reporting error state as zero
3. Sensor file format unexpected

**Solutions**:
```bash
# Check raw sensor value
cat /sys/class/hwmon/hwmon1/temp1_input

# Check if sensor has label
cat /sys/class/hwmon/hwmon1/temp1_label 2>/dev/null

# Try different temp sensor indices (temp2, temp3, etc.)
for i in {1..8}; do
    echo -n "temp${i}: "
    cat /sys/class/hwmon/hwmon1/temp${i}_input 2>/dev/null || echo "N/A"
done
```

## Architecture-Specific Notes

### Grayskull
- 4 DDR channels (not visible via sysfs)
- 10×12 Tensix grid (not detailed in hwmon)
- Typical sensors: 1-2 temp, 1-2 voltage

### Wormhole
- 8 DDR channels (not visible via sysfs)
- 8×10 Tensix grid (not detailed in hwmon)
- Typical sensors: 2-4 temp, 2-3 voltage, power if available

### Blackhole
- 12 DDR channels (not visible via sysfs)
- 14×16 Tensix grid (not detailed in hwmon)
- Typical sensors: 4+ temp, 2-4 voltage, power if available

## Kernel Requirements

### Minimum Kernel Version
- **Linux 2.6.26+**: Basic hwmon support
- **Linux 3.0+**: Modern hwmon interface
- **Linux 5.0+**: Recommended for full feature support

### Required Kernel Options
```
CONFIG_HWMON=y           # Hardware monitoring support
CONFIG_SYSFS=y           # Sysfs filesystem
CONFIG_I2C=y             # I2C bus support (for sensor chips)
```

### Driver Dependencies
The Tenstorrent kernel driver must expose the hwmon interface. Check with:
```bash
modinfo tenstorrent | grep -i version
ls /sys/class/hwmon/*/name | xargs -I{} sh -c 'printf "%s: " {}; cat {}'
```

| Feature | Minimum tt-kmd |
|---------|----------------|
| hwmon sensors (temp / voltage / power / current) | any version with hwmon support |
| Class attributes (`tt_aiclk`, `tt_heartbeat`, `tt_card_type`, …) and `pcie_perf_counters/` | **2.7** |
| `*_max` firmware limits on Blackhole | **2.9.0** |

Everything degrades gracefully: an absent attribute reads as `None` and the corresponding row simply
doesn't render.

## Limitations

### Partial SMBUS Telemetry
The synthesized `SmbusTelemetry` covers clocks, the ARC heartbeat, thermal trips, the board serial and
the fan — not the full block `tt-smi` reports. Still missing: **DDR training status**, **GDDR
temperatures and ECC counters**, **per-ARC (1–3) health**, **per-component firmware versions**,
**board/VReg temperatures**, **Ethernet link status** and **harvesting masks**. Run the JSON or hybrid
backend when you need those.

### PCIe
- **Bandwidth only, no geometry**: the counters give bytes/sec each way, but the link's generation and
  width come from `tt-smi`'s `board_info` — sysfs-only mode shows rates without the `GenN ×W` line.
- **No reading on the first tick**: bandwidth is a difference between two counter samples.

### Driver Version Dependence
- **tt-kmd < 2.7**: no class-attribute directory at all — no clocks, no heartbeat, no `tt_card_type`
  (the `tt-smi` board-type probe comes back), no PCIe counters. The backend still works from hwmon alone.
- **tt-kmd < 2.9.0 on Blackhole**: no `*_max` limit files, so the UI falls back to its generic
  300 W / 105 °C reference values.
- Attribute *shape* is driver-specific; every read is best-effort and a missing file degrades to `None`
  rather than failing the backend.

### Update Rate Constraints
- **Kernel buffering**: Sensors may update slower than query rate
- **Driver refresh**: Some drivers update sensors at fixed intervals (e.g., 1 Hz)
- **File I/O overhead**: Each read requires system call

### Multi-Device Support
- **Device ordering**: hwmon indices don't match PCI bus order, so the backend sorts by bus id before
  assigning device indices — this is what keeps the hybrid backend's index-keyed join against
  `tt-smi -s` pointing at the right card
- **Dynamic hotplug**: devices appearing after init are not picked up until the backend re-initializes
- **No coordination**: Hwmon doesn't guarantee consistent multi-device reads

## Best Practices

### For Production Monitoring

1. **Use Auto-Detect**: on Linux the application tries Hybrid (this backend + background tt-smi) → JSON → Mock
   ```bash
   ./target/debug/tt-toplike-gui
   ```

2. **Explicit Sysfs for Active Hardware**: Skip failed attempts
   ```bash
   ./target/debug/tt-toplike-gui --backend sysfs
   ```

3. **Appropriate Update Rate**: Don't exceed driver refresh rate
   ```bash
   ./target/debug/tt-toplike-gui --backend sysfs --interval 1000  # 1 Hz
   ```

4. **Log Analysis**: Monitor for sensor read failures
   ```bash
   ./target/debug/tt-toplike-gui --backend sysfs -v 2>&1 | grep -i error
   ```

### For Development

1. **Verify Sensor Availability**: Check manually first
   ```bash
   ls -la /sys/class/hwmon/hwmon*/
   cat /sys/class/hwmon/hwmon*/name
   ```

2. **Test Sensor Reads**: Ensure values are sane
   ```bash
   watch -n 1 'cat /sys/class/hwmon/hwmon1/temp1_input'
   ```

3. **Compare Backends**: Cross-check with Luwen on idle hardware
   ```bash
   # With hardware idle:
   sudo ./target/debug/tt-toplike-gui --backend luwen
   # vs.
   ./target/debug/tt-toplike-gui --backend sysfs
   ```

## Future Enhancements

### Already Landed (v0.8.0)
- Label-aware sensor discovery (`*_label`), fan sensor, and real `*_max` limits
- Clocks (`tt_aiclk` / `tt_axiclk` / `tt_arcclk`), ARC heartbeat, board SKU, firmware bundle version,
  board serial and thermal-trip count from the tt-kmd class-attribute directory
- Live PCIe bandwidth from `pcie_perf_counters/`
- Bus-id-ordered device discovery

### Planned Improvements
1. **Multi-sensor aggregation**: average or report multiple temp sensors instead of picking one
2. **Threshold monitoring**: read `temp*_crit` for alerts (distinct from the `temp*_max` limit already used)
3. **Historical tracking**: min/max sensor values over a session

### Driver Wishlist
- **DDR training status**: the per-channel bitmask as a class attribute, so sysfs-only mode can render
  the boot-time training sequence
- **GDDR temperature and ECC counters**: currently ARC/SMBUS-only
- **PCIe link geometry**: generation and width alongside the counters, so the PCIe row is complete
  without `tt-smi`
- **Power rail details**: per-component power breakdown

## References

### Linux Hwmon Documentation
- [Hwmon sysfs interface](https://www.kernel.org/doc/Documentation/hwmon/sysfs-interface)
- [Hwmon kernel documentation](https://www.kernel.org/doc/html/latest/hwmon/hwmon-kernel-api.html)

### Sensor Units (from hwmon spec)
- Temperature: millidegrees Celsius (mC)
- Voltage: millivolts (mV)
- Current: milliamperes (mA)
- Power: microwatts (µW)

### Example Drivers
- `coretemp` - Intel CPU temperature
- `k10temp` - AMD CPU temperature
- `radeon` - AMD GPU sensors
- `tenstorrent` - Tenstorrent accelerator (custom)

---

*Last Updated: August 10, 2026 (tt-toplike v0.8.0 — tt-kmd class attributes + PCIe counters)*
*Status: Production Ready ✅*
