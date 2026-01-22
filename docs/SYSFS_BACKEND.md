# Sysfs Backend - Non-Invasive Hardware Monitoring

## Overview

The Sysfs backend provides **non-invasive hardware monitoring** by reading from Linux's hwmon subsystem (`/sys/class/hwmon/`). This backend is specifically designed for monitoring Tenstorrent hardware that's actively running workloads (LLMs, training, inference) without interfering with operations.

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

# Auto-detect will try sysfs after Luwen and JSON fail
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
- Hardware is idle and you want full SMBUS telemetry
- You need ARC firmware health monitoring
- You require DDR training status and channel-specific data
- Clock frequency (AICLK) monitoring is critical

## What's Available

### Telemetry Provided ✅

| Metric | Source | Notes |
|--------|--------|-------|
| **Temperature** | `temp*_input` | ASIC temperature in °C |
| **Voltage** | `in*_input` | VCore voltage in V |
| **Power** | `power*_input` | Power consumption in W (if driver exposes) |
| **Current** | `curr*_input` or calculated | Amperes (may be calculated from P/V) |
| **Device Count** | hwmon discovery | All Tenstorrent devices detected |
| **Architecture** | hwmon `name` | Detected from name string |

### Telemetry NOT Available ❌

| Metric | Why Not Available |
|--------|-------------------|
| **SMBUS Data** | Requires direct hardware access, not exposed by hwmon |
| **AICLK** | Clock frequency not standard hwmon attribute |
| **ARC Heartbeat** | Firmware-specific, requires SMBUS access |
| **DDR Status** | Detailed memory info requires direct access |
| **Firmware Versions** | SMBUS-only information |

## How It Works

### Device Detection

1. Scans `/sys/class/hwmon/` for all hwmon devices
2. Reads `name` file from each hwmon directory
3. Looks for Tenstorrent-related names:
   - `"tenstorrent"`
   - `"grayskull"`
   - `"wormhole"`
   - `"blackhole"`
4. Creates device entries with hwmon path mappings

### Sensor Reading

For each hwmon device, reads sensor files:

```bash
# Temperature (converts millicelsius → Celsius)
/sys/class/hwmon/hwmon*/temp1_input
/sys/class/hwmon/hwmon*/temp2_input
# ... up to temp8_input

# Voltage (converts millivolts → Volts)
/sys/class/hwmon/hwmon*/in0_input
/sys/class/hwmon/hwmon*/in1_input
# ... up to in8_input

# Power (converts microwatts → Watts, if available)
/sys/class/hwmon/hwmon*/power1_input
/sys/class/hwmon/hwmon*/power2_input
# ... up to power8_input

# Current (milliamps → Amperes, or calculated from P/V)
/sys/class/hwmon/hwmon*/curr1_input
```

### PCI Address Extraction

Attempts to extract PCI bus address from sysfs device symlinks:

```bash
# Read device symlink
/sys/class/hwmon/hwmon3/device → /sys/devices/pci0000:00/0000:00:01.0/...

# Parse PCI address pattern: 0000:00:01.0
```

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
```

### Check Available Attributes

```bash
# List all sensor files for a device
ls -la /sys/class/hwmon/hwmon1/

# Common files:
# - name           : Device name
# - temp*_input    : Temperature sensors
# - temp*_label    : Sensor labels (optional)
# - temp*_crit     : Critical temperature threshold (optional)
# - in*_input      : Voltage sensors
# - power*_input   : Power sensors (if available)
# - curr*_input    : Current sensors (if available)
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
The Tenstorrent kernel driver must expose hwmon interface. Check with:
```bash
modinfo ttkmd | grep hwmon
```

## Limitations

### Detailed Telemetry
- **No SMBUS access**: Firmware versions, ARC health unavailable
- **No DDR details**: Training status, per-channel info missing
- **No clock frequencies**: AICLK not exposed by hwmon

### Update Rate Constraints
- **Kernel buffering**: Sensors may update slower than query rate
- **Driver refresh**: Some drivers update sensors at fixed intervals (e.g., 1 Hz)
- **File I/O overhead**: Each read requires system call

### Multi-Device Support
- **Device ordering**: Hwmon indices may not match PCI bus order
- **Dynamic hotplug**: New devices may appear with different indices
- **No coordination**: Hwmon doesn't guarantee consistent multi-device reads

## Best Practices

### For Production Monitoring

1. **Use Auto-Detect**: Let the application try Luwen → JSON → Sysfs → Mock
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

### Planned Improvements
1. **Extended sensor discovery**: Fan speed, frequency (if driver adds)
2. **Multi-sensor aggregation**: Average multiple temp sensors
3. **Label parsing**: Use `temp*_label` for identification
4. **Threshold monitoring**: Read `temp*_crit` for alerts
5. **Historical tracking**: Min/max sensor values

### Driver Wishlist
- **AICLK exposure**: Clock frequency as hwmon attribute
- **Power rail details**: Per-component power breakdown
- **DDR temperature**: Individual channel temperatures
- **Fan control**: If hardware has fans

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

*Last Updated: January 15, 2026*
*Backend Version: v1.0*
*Status: Production Ready ✅*
