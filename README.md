# TT-Toplike-RS 🦀

Real-time hardware monitoring for Tenstorrent silicon, written in Rust.

## Project Status

**Status**: 🎉 **Production Ready** (12 phases complete)
**Latest**: Sysfs backend for non-invasive monitoring of active hardware

### Completed ✅
- ✅ **Dual Frontend Architecture**: TUI + Native GUI with shared core library
- ✅ **Four Backend System**: Mock, JSON (tt-smi), Luwen (direct hardware), Sysfs (hwmon sensors)
- ✅ **Non-Invasive Monitoring**: Sysfs backend works on hardware running active workloads (LLMs, etc.)
- ✅ **Graceful Error Handling**: Panic catching with automatic backend fallback
- ✅ **Native GUI** (iced): Wayland/X11 with Vulkan/OpenGL acceleration
  - Dashboard view with DDR channels, memory hierarchy, animated metrics
  - Details view with complete telemetry table
  - Charts view with historical power/temperature graphs
  - Starfield view with GPU-accelerated visualization
- ✅ **Terminal TUI** (ratatui): Dark mode optimized with multiple views
  - TRON Grid mode with DDR/memory visualization
  - Hardware-responsive starfield
  - Psychedelic color cycling
- ✅ **Adaptive Baseline System**: Learns idle state, shows relative activity
- ✅ **Architecture Support**: Grayskull, Wormhole, Blackhole
- ✅ All 30 tests passing
- ✅ Comprehensive documentation (CLAUDE.md, GUI_FEATURES.md, TESTING.md)

### Future Enhancements 🎯
- ⏳ ML workload detection integration
- ⏳ System tray integration for GUI
- ⏳ Desktop notifications for temperature alerts
- ⏳ Export historical data (CSV, JSON)

## Features

### Real-Time Monitoring TUI
- 🎨 **Beautiful Color Palette**: Purple-blue-teal theme from tt-vscode-toolkit
- 📊 **Live Telemetry**: Power, temperature, current, voltage, clock speeds
- 🎯 **Color-Coded Status**: Temperature and power use traffic light colors
- 🏥 **Health Monitoring**: ARC firmware heartbeat tracking
- ⌨️ **Interactive Controls**: Keyboard shortcuts for quit, refresh, and visualization toggle
- 🔄 **Configurable Refresh**: 10ms to 1000ms update intervals (10-100 FPS)
- 📱 **Responsive Design**: Adapts to terminal size, handles resize
- ✨ **Clean Exit**: Alternate screen preserves your terminal state

### Hardware-Responsive Visualizations
- 🌌 **Starfield Mode**: Press `v` to toggle full-screen visualization
- ⭐ **Tensix Cores as Stars**: Positioned by actual chip topology (GS: 10×12, WH: 8×10, BH: 14×16)
- 🎨 **Temperature Colors**: Cyan (cool) → Green → Yellow → Orange → Red (hot)
- 💡 **Power Brightness**: Star brightness driven by real power consumption
- ✨ **Current Twinkle**: Animation speed reflects current draw intensity
- 🪐 **Memory Planets**: Three-tier hierarchy (L1/L2/DDR) with distinct behaviors
- 🌊 **Data Streams**: Animated flow between devices based on power differentials
- 📊 **Adaptive Baseline**: Learns idle state over 20 samples, shows relative activity
- 🎯 **Universal Sensitivity**: Works on any hardware (5W-200W), 10% change = visible

### Backend Support
- 🤖 **Mock Backend**: Test and develop without hardware (generates realistic telemetry)
- 📡 **JSON Backend**: Integrates with tt-smi subprocess (compatible fallback)
- ⚡ **Luwen Backend**: Direct hardware access via PCI (best performance, requires idle hardware)
- 🔬 **Sysfs Backend**: Linux hwmon sensors (non-invasive, works on active hardware!)
- 🔍 **Auto-Detection**: Tries Luwen → JSON → Sysfs → Mock with graceful fallback
- 🛡️ **Panic Recovery**: Catches hardware access panics, continues with fallback
- 🎛️ **Device Filtering**: Monitor specific devices with `--devices`

**✨ New in Phase 12**: Sysfs backend enables monitoring of hardware running active workloads (LLMs, training, inference) without any invasiveness or special permissions!

### CLI Features
- 📖 **Comprehensive Help**: Detailed examples and usage info
- 🛠️ **Flexible Configuration**: Control every aspect via CLI flags
- 📝 **Logging Control**: Verbose, normal, or quiet modes
- 🎯 **Backend Selection**: Choose mock, JSON, or auto-detect

## Architecture

TT-Toplike-RS uses a hybrid backend approach:

1. **Luwen Backend** (Primary): Direct hardware access via official Tenstorrent Rust library
2. **JSON Backend** (Fallback): Subprocess tt-smi + JSON parsing for compatibility

```
┌─────────────────┐
│   TT-Toplike    │
│    (Ratatui)    │
└────────┬────────┘
         │
    ┌────┴────┐
    │ Backend │ (Trait)
    │  Layer  │
    └─┬────┬──┘
      │    │
┌─────┴┐  ┌┴──────┐
│Luwen │  │ JSON  │
│(HW)  │  │(Proc) │
└──────┘  └───────┘
```

## Dependencies

- **ratatui**: Terminal UI framework
- **crossterm**: Cross-platform terminal manipulation
- **tokio**: Async runtime
- **serde/serde_json**: JSON parsing
- **clap**: CLI argument parsing
- **sysinfo**: Process monitoring
- **thiserror/anyhow**: Error handling
- **chrono**: Time utilities

For complete list, see `Cargo.toml`.

## Usage

### GUI Application (Native Wayland/X11)
```bash
# Auto-detect backend (tries Luwen → JSON → Sysfs → Mock)
./target/debug/tt-toplike-gui

# Use sysfs backend for active hardware (recommended for production!)
./target/debug/tt-toplike-gui --backend sysfs

# Use mock backend for testing (no hardware required)
./target/debug/tt-toplike-gui --mock --mock-devices 3

# Use JSON backend with tt-smi
./target/debug/tt-toplike-gui --backend json

# With Luwen backend (requires permissions + idle hardware)
sudo ./target/debug/tt-toplike-gui --backend luwen

# Custom refresh rate and verbose logging
./target/debug/tt-toplike-gui --interval 50 -v
```

### TUI Application (Terminal)
```bash
# Auto-detect backend (tries Luwen → JSON → Sysfs → Mock)
./target/debug/tt-toplike-tui

# Use sysfs backend for active hardware (recommended!)
./target/debug/tt-toplike-tui --backend sysfs

# Use mock backend for testing
./target/debug/tt-toplike-tui --mock --mock-devices 3

# Customize update interval and device filter
./target/debug/tt-toplike-tui --interval 50 --devices 0,2,4

# Show help
./target/debug/tt-toplike-tui --help
```

**Note**: Auto-detect gracefully handles hardware access issues. If Luwen fails (hardware active), it tries JSON, then Sysfs (works on active hardware!), then Mock.

### Keyboard Controls

**Normal Mode (Table View)**:
- `q` or `ESC`: Quit application
- `r`: Force refresh telemetry
- `v`: Toggle to visualization mode

**Visualization Mode (Starfield)**:
- `v`: Return to normal table view
- `q` or `ESC`: Quit application
- `r`: Force refresh (updates baseline)

### Visualization Features

The starfield visualization shows:
- **Stars**: Each star represents a Tensix core at its actual grid position
  - Brightness increases with power consumption (relative to baseline)
  - Color changes with temperature (cyan = cool, red = hot)
  - Twinkle speed reflects current draw
- **Planets**: Memory hierarchy orbiting each device
  - L1 cache (◆ blue): Responds to compute activity
  - L2 cache (◇ yellow): Responds to memory traffic
  - DDR channels (blocks): Responds to combined load
- **Streams**: Data flow between devices (when multiple devices present)
- **Baseline Learning**: First 20 samples establish idle state
  - Shows "LEARNING BASELINE (N/20)" during learning phase
  - Once established, all activity shown relative to baseline
  - Makes visualization sensitive on any hardware (5W-200W ranges)

## Building

### Build Both Frontends
```bash
# Build all features (TUI + GUI + all backends)
cargo build --all-features

# Or build individually
cargo build --bin tt-toplike-gui --features gui
cargo build --bin tt-toplike-tui --features tui
```

### Release Build (Optimized)
```bash
cargo build --release --bin tt-toplike-gui --features gui
cargo build --release --bin tt-toplike-tui --features tui
```

### Run Without Building
```bash
# GUI with mock backend
cargo run --bin tt-toplike-gui --features gui -- --mock --mock-devices 2

# TUI with mock backend
cargo run --bin tt-toplike-tui --features tui -- --mock --mock-devices 2
```

## Testing

```bash
# Run all tests
cargo test

# Lint and format check
cargo clippy --all-features
cargo fmt --check

# See TESTING.md for comprehensive test scenarios
```

## Data Models

### Device (`src/models/device.rs`)
- Represents Tenstorrent hardware devices
- Architecture detection (Grayskull, Wormhole, Blackhole)
- Board type parsing and capabilities

### Telemetry (`src/models/telemetry.rs`)
- Core metrics: power, temperature, current, clock speeds
- SMBUS telemetry: DDR status, ARC health, firmware versions
- Comprehensive hardware status

### Error Handling (`src/error.rs`)
- Type-safe error handling with `thiserror`
- Backend-specific and application-level errors
- Result type aliases for convenience

## Contributing

This project follows Rust best practices:
- Liberal comments and documentation
- Comprehensive error handling
- Type safety throughout
- Unit tests for all modules

## License

Apache-2.0

## Authors

Tenstorrent
