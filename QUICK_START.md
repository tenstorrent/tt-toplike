# TT-Toplike-RS Quick Start

**Version**: 0.1.0
**Installed**: March 23, 2026
**Location**: `~/.local/bin/`

---

## Launch Modes

### 🎮 Arcade Mode (Unified Visualization)
```bash
tt-toplike-tui --mode arcade
tt-toplike-tui -m arcade
```
- Hero character (@) that moves with telemetry
- All three visualizations in split screen
- 30/40/30 layout (Starfield/Castle/Flow)

### 🏰 Memory Castle (DDR Hierarchy)
```bash
tt-toplike-tui --mode castle
tt-toplike-tui -m memory      # alias
```
- DDR channels with training status
- L2 cache visualization (8 banks)
- L1 SRAM compressed grid
- Tensix core activity

### 🌌 Starfield (Tensix Cores)
```bash
tt-toplike-tui --mode starfield
```
- Stars = Tensix cores (brightness = power)
- Color = temperature
- Twinkle rate = current draw
- Memory hierarchy planets

### 🌊 Memory Flow (DRAM Motion)
```bash
tt-toplike-tui --mode flow
tt-toplike-tui -m topology    # alias
```
- Full-screen DRAM visualization
- NoC particles
- DDR channel perimeter
- Heat map center

### 📋 Normal (Table View)
```bash
tt-toplike-tui --mode normal
tt-toplike-tui               # default
```
- Traditional table view
- Real-time telemetry
- Process monitoring (Linux)

---

## Backend Options

### Auto-detect (Safe Mode)
```bash
tt-toplike-tui --mode arcade
```
Tries: Sysfs → JSON → Mock (never Luwen)

### Sysfs (Non-invasive)
```bash
tt-toplike-tui --mode arcade --backend sysfs
```
- Linux hwmon sensors
- Zero interference with running workloads
- Works during LLM inference

### Mock (Testing)
```bash
tt-toplike-tui --mode arcade --mock --mock-devices 4
```
- No hardware required
- Simulated telemetry

### JSON (tt-smi)
```bash
tt-toplike-tui --mode arcade --backend json
```
- Subprocess wrapper
- Requires tt-smi installed

---

## Common Commands

### Arcade with Real Hardware
```bash
tt-toplike-tui --mode arcade --backend sysfs
```

### Memory Castle with Fast Refresh
```bash
tt-toplike-tui --mode castle --interval 50
```

### Starfield with Verbose Logging
```bash
tt-toplike-tui --mode starfield -v
```

### Memory Flow with Specific Devices
```bash
tt-toplike-tui --mode flow --devices 0,2
```

### Normal Mode (Default)
```bash
tt-toplike-tui
```

---

## Keyboard Shortcuts (In TUI)

- `v` - Cycle visualization modes
- `b` - Switch backend (Sysfs → JSON → Luwen → Mock)
- `q` or `ESC` - Quit
- `r` - Force refresh

---

## Psychedelic egui Dashboard

### Launch GUI
```bash
tt-toplike-egui --mock --mock-devices 4
```

### Features
- 150 animated particles with glow
- TRON grid overlay
- Cyberpunk theme (neon colors)
- Rainbow title bar
- 4 psychedelic graphs
- Process monitoring panel
- 60 FPS animation

---

## Troubleshooting

### "Text file busy"
```bash
pkill tt-toplike-tui
# Then retry command
```

### tmux Background Colors
Already fixed in v0.1.0! Uses transparent backgrounds.

### No Hardware Detected
```bash
# Check hwmon
ls /sys/class/hwmon/

# Try mock backend
tt-toplike-tui --mock --mock-devices 2
```

### Temperature Readings Off
Report issue with:
- Backend used: `--backend sysfs|json|luwen`
- Expected vs actual values
- Visualization mode

---

## Files

**Binaries**:
- `~/.local/bin/tt-toplike-tui` (2.3 MB)
- `~/.local/bin/tt-toplike-egui` (16 MB)

**Source**: `~/code/tt-toplike-rs/`

**Test Scripts**:
- `test-modes.sh` - Test all 5 modes
- `test-egui.sh` - Test GUI dashboard

---

## Build from Source

```bash
cd ~/code/tt-toplike-rs

# TUI
cargo build --release --bin tt-toplike-tui --features tui
cargo install --path . --bin tt-toplike-tui --features tui --force

# GUI
cargo build --release --bin tt-toplike-egui --features egui
cargo install --path . --bin tt-toplike-egui --features egui --force

# Copy to .local/bin
cp ~/.cargo/bin/tt-toplike-tui ~/.local/bin/
cp ~/.cargo/bin/tt-toplike-egui ~/.local/bin/
```

---

**Last Updated**: March 23, 2026
**Status**: ✅ Installed and ready to use!
