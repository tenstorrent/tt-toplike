# TT-Toplike GUI Features

## Overview

The native GUI application provides a beautiful Wayland/X11 interface for monitoring Tenstorrent hardware with real-time updates, historical charts, and GPU-accelerated visualizations.

## Features

### 1. **Multiple View Modes**

The GUI offers four distinct visualization modes, selectable via buttons:

#### 🎛 Dashboard View (Default)
- **DDR Channel Visualization**: Real-time training status for all channels
  - Architecture-specific counts: Grayskull (4), Wormhole (8), Blackhole (12)
  - Training status indicators:
    - ○ Idle (gray)
    - ◐ Training (animated cyan, alternates ◐◑)
    - ● Trained (bright green)
    - ✗ Error (bright red)
  - Utilization bars showing current draw per channel (█▓▒░·)
- **Memory Hierarchy**: Three-tier visualization
  - **L1 SRAM** (cyan): Per-core cache, fast access
  - **L2 Cache** (yellow): 8 shared banks, medium access
  - **DDR** (purple): Off-chip memory, large capacity
  - Animated activity bars driven by hardware telemetry
- **Real-Time Metrics Gauges**:
  - Power consumption (W) with color-coded progress bar
  - ASIC temperature (°C) with thermal gradient
  - Current draw (A) with activity indicator
- **Color-Cycling Animated Border**: Rainbow HSV animation
- **Architecture Details**: Chip type, Tensix grid dimensions

#### 📋 Details View
- Detailed telemetry table display
- Device information (architecture, bus ID, board type)
- Current metrics:
  - ⚡ Power (W)
  - 🌡 Temperature (°C)
  - ⚙ Current (A)
  - 🔋 Voltage (V)
  - ⏱ AICLK (MHz)
  - 💓 Heartbeat
- Architecture-specific details (DDR channels, Tensix grid)

#### 📈 Charts View
- **Historical Power Chart** (last 30 seconds)
  - Real-time line graph
  - Min/Max value display
  - Teal color coding
- **Historical Temperature Chart** (last 30 seconds)
  - Real-time line graph
  - Min/Max value display
  - Orange color coding
- Sample count and time window display
- Auto-scaling Y-axis based on data range

#### ✨ Starfield Visualization
- **GPU-Accelerated Canvas rendering**
- **Topology-Accurate Stars**: Each star represents an actual Tensix core
  - Grayskull: 10×12 grid (120 stars)
  - Wormhole: 8×10 grid (80 stars)
  - Blackhole: 14×16 grid (224 stars)
- **Hardware-Responsive Animation**:
  - Star **brightness** = Power consumption (relative to baseline)
  - Star **color** = Temperature (cyan→green→yellow→orange→red)
  - Star **twinkle rate** = Current draw
- **Topology Lines**: Connecting lines between nearby cores show chip layout
- **Adaptive Baseline Learning**: Learns idle state, shows relative activity

### 2. **Multi-Device Support**

- Device selector tabs at the top
- Switch between devices seamlessly
- Each device has independent:
  - Telemetry history (300 samples = 30s @ 100ms)
  - Starfield visualization
  - Real-time metrics

### 3. **Real-Time Updates**

- Configurable update interval (default: 100ms = 10 FPS)
- Continuous telemetry polling from backend
- Smooth animation in starfield mode
- Live chart updates with circular buffer

### 4. **Dark Mode Optimization**

- Native dark theme
- Color palette optimized for dark backgrounds
- Consistent with TUI color scheme
- Professional appearance on KDE, GNOME, and other DEs

### 5. **Backend Flexibility**

Same backend system as TUI:
- **Auto**: Try Luwen → JSON → Mock (default)
- **Luwen**: Direct hardware access (best performance)
- **JSON**: tt-smi subprocess (compatibility)
- **Mock**: Simulated devices (testing)

## Usage

### Basic Launch

```bash
# With mock backend (2 devices)
cargo run --bin tt-toplike-gui --features gui -- --mock --mock-devices 2

# With auto-detection (tries real hardware)
cargo run --bin tt-toplike-gui --features gui

# With JSON backend
cargo run --bin tt-toplike-gui --features gui -- --backend json

# With Luwen backend (real hardware)
cargo run --bin tt-toplike-gui --features gui --features luwen-backend
```

### CLI Options

```bash
# Custom update interval (50ms = 20 FPS)
tt-toplike-gui --mock --interval 50

# Filter specific devices
tt-toplike-gui --devices 0,2

# Verbose logging
tt-toplike-gui -v --mock
```

## Mouse Controls

- **Device Selection**: Click device tabs (Device 0, Device 1, etc.)
- **View Mode**: Click view buttons
  - 🎛 **Dashboard** - DDR channels, memory hierarchy, animated metrics (default)
  - 📋 **Details** - Detailed telemetry table
  - 📈 **Charts** - Historical power and temperature graphs
  - ✨ **Starfield** - GPU-accelerated psychedelic visualization
- **Refresh**: Click 🔄 button (force immediate update)
- **Window**: Standard window controls (close, minimize, maximize)

## Technical Details

### Performance

- **Update Rate**: 10 Hz (100ms) default, configurable
- **Memory**: ~15-20MB typical (with history)
- **CPU**: <3% idle, <8% active (with visualizations)
- **GPU**: Vulkan/OpenGL ES acceleration for Canvas

### History Management

- **Circular Buffer**: 300 samples per device (30s @ 100ms)
- **Efficient Storage**: VecDeque for O(1) push/pop
- **Auto-Cleanup**: Old samples removed automatically
- **Per-Device**: Independent histories for multi-device systems

### Visualization Engine

- **iced Canvas**: GPU-accelerated 2D rendering
- **Geometry Caching**: Optimized frame rendering
- **Adaptive Baseline**: Learns hardware idle state
- **HSV Color Space**: Smooth color cycling
- **Anti-Aliasing**: Smooth star rendering with glow effects

### Architecture

```
TTTopGUI
├── Backend (shared with TUI)
│   ├── Mock
│   ├── JSON
│   └── Luwen
├── History Manager
│   └── TelemetryHistory (per device)
├── Visualizations
│   ├── DashboardVisualization (NEW)
│   ├── StarfieldVisualization
│   └── LineChart
└── View Modes
    ├── Dashboard (default)
    ├── Details (table)
    ├── Charts
    └── Starfield
```

## Future Enhancements

Potential additions for future versions:

- [ ] TRON Grid visualization mode (available in TUI, could be ported to GUI)
- [ ] System tray integration
- [ ] Desktop notifications (temperature alerts)
- [ ] Export historical data (CSV, JSON)
- [ ] Multi-device grid view (4×4 tile layout)
- [ ] Custom color themes
- [ ] Pause/Resume data collection
- [ ] Screenshot/recording capability
- [ ] Workload detection integration

## Troubleshooting

### GUI won't launch
```bash
# Check if GUI feature is enabled
cargo build --bin tt-toplike-gui --features gui

# Check for Wayland/X11 support
echo $XDG_SESSION_TYPE  # Should be 'wayland' or 'x11'

# Try with verbose logging
tt-toplike-gui -v --mock
```

### No GPU acceleration
The GUI will automatically fall back to CPU rendering if GPU isn't available. Check logs for:
```
[INFO] Selected: AdapterInfo { ..., backend: Vulkan }
```

### Mock backend overflow panic (FIXED)
Fixed in current version. If you see "attempt to add with overflow", update to latest code.

### Luwen backend hardware access error (RESOLVED ✅)
**Status**: This issue is now **automatically handled** by the application. The panic is caught and the app gracefully falls back to JSON or Mock backend.

**Symptom**: You may see panic messages in the log, but the application continues running:
```
WARNING: Failed to map bar0_wc for 0 with error Invalid argument (os error 22)
thread 'main' panicked at .../all-smi-ttkmd-if-0.2.2/src/lib.rs:294:17:
Failed to map bar0_uc for 0 with error Invalid argument (os error 22)
[WARN] Luwen backend panicked (likely hardware access issue), trying JSON backend
```

**What Happens Now**: The application automatically tries backends in order:
1. Luwen (direct hardware) - panic caught if permissions fail
2. JSON (tt-smi subprocess) - tried automatically
3. Mock (simulated devices) - used as last resort

**To Use Specific Backend** (if auto-detect isn't working as expected):

1. **Use Mock Backend** (recommended for testing):
   ```bash
   tt-toplike-gui --mock --mock-devices 2
   ```

2. **Use JSON Backend** (for real hardware without PCI access):
   ```bash
   tt-toplike-gui --backend json
   ```
   This uses tt-smi subprocess instead of direct hardware access.

3. **Run with Sudo** (if you need Luwen):
   ```bash
   sudo tt-toplike-gui
   ```
   ⚠️ Only use if ttkmd kernel module is loaded and you specifically need Luwen backend.

4. **Check Hardware Access**:
   ```bash
   lsmod | grep ttkmd              # Verify kernel module loaded
   ls -l /dev/tenstorrent*         # Check device permissions
   ```

## Platform Support

- **KDE Plasma**: Excellent (tested on 6.0+)
- **GNOME**: Excellent (tested on 46+)
- **Wayland**: Primary target, full support
- **X11**: Full support (fallback)
- **Other DEs**: Should work (Cinnamon, XFCE, etc.)

## Performance Tips

1. **Reduce Update Interval** for lower CPU usage:
   ```bash
   tt-toplike-gui --interval 200  # 5 Hz instead of 10 Hz
   ```

2. **Stay in Details View** if visualizations cause performance issues (Dashboard uses GPU acceleration)

3. **Filter Devices** to reduce memory usage:
   ```bash
   tt-toplike-gui --devices 0  # Monitor only first device
   ```

4. **Disable History** by reducing interval dramatically:
   ```bash
   tt-toplike-gui --interval 1000  # Only stores 300 seconds = 5 minutes
   ```

## Code Structure

```
src/
├── bin/
│   └── gui.rs              # Main GUI application (540+ lines)
├── ui/
│   └── gui/
│       ├── history.rs      # Historical telemetry tracking (200+ lines)
│       └── visualization.rs # Canvas visualizations (900+ lines)
│           ├── DashboardVisualization (520 lines)
│           ├── StarfieldVisualization (200 lines)
│           └── LineChart (180 lines)
└── backend/               # Shared with TUI (3000+ lines)
```

## License

Apache 2.0 - Same as tt-toplike project

## Credits

- **iced**: Cross-platform GUI framework
- **wgpu**: GPU acceleration
- **Tenstorrent**: Hardware and luwen library
- **tt-toplike**: Original Python implementation inspiration
