# TT-Toplike-RS 🦀

Real-time hardware monitoring for Tenstorrent silicon (Grayskull, Wormhole, Blackhole). Written in Rust.

![3 visualizers](/assets/tt-toplike-rs.png "Screenshot of 3 simultaneous visualizers")

## Installation

### Debian / Ubuntu (recommended)

Pre-built `.deb` packages are produced by `build-deb.sh` and can be installed directly:

```bash
# Build packages (requires cargo, debhelper, rustc ≥ 1.75)
./build-deb.sh

# Install
sudo dpkg -i ../tt-toplike_0.1.0_amd64.deb         # TUI monitor
sudo dpkg -i ../tt-toplike-egui_0.1.0_amd64.deb    # egui dashboard (optional)

# Verify
tt-toplike --mock --mock-devices 4
tt-toplike --mode arcade
```

**Prerequisites** (one-time):
```bash
sudo apt install devscripts debhelper rustc cargo
```

The build vendors all crate dependencies into `vendor/` so the package builds offline (no network access needed at build time). Run `./build-deb.sh --quick` to skip re-vendoring when `vendor/` is already present.

### Build from Source

```bash
# TUI only (safe defaults — no Luwen, no GUI)
cargo build --release --bin tt-toplike-tui --features tui,json-backend,linux-procfs

# egui dashboard
cargo build --release --bin tt-toplike-egui --features egui,json-backend

# Everything
cargo build --release --all-features
```

## Usage

```bash
# Auto-detect backend (safe: Sysfs → JSON → Mock; never tries Luwen)
tt-toplike

# Explicit backends
tt-toplike --backend sysfs    # hwmon sensors — zero interference with running workloads
tt-toplike --backend json     # tt-smi subprocess
tt-toplike --mock --mock-devices 4

# Luwen (direct PCI access) — explicit only, never auto-detected
# WARNING: may disrupt running workloads (LLMs, training)
tt-toplike --backend luwen

# Visualization modes
tt-toplike --mode arcade      # unified split-screen (default: normal table)
tt-toplike --mode castle      # Memory Castle roguelike
tt-toplike --mode starfield   # Tensix starfield
tt-toplike --mode flow        # NoC memory flow

# Filter to specific devices
tt-toplike --devices 0,2
```

### Keyboard Controls (TUI)

| Key | Action |
|-----|--------|
| `q` / `ESC` | Quit |
| `r` | Force refresh |
| `v` | Cycle visualization mode |
| `b` | Cycle backend (live switching) |

## Features

### Multiple Visualization Modes

- **Normal** — live telemetry table with color-coded power/temp
- **Starfield** — stars = Tensix cores; brightness = power, color = temperature, twinkle = current
- **Memory Castle** — roguelike dungeon with 600 particles representing DDR→L2→L1→Tensix memory hierarchy; 4 particle types (Read/Write/CacheHit/Miss) with trails
- **Memory Flow** — NoC particles flowing across DDR channels
- **Arcade** — all three visualizations simultaneously, with a `@` hero character driven by real telemetry (X = current, Y = power, color = temperature)
- **egui Dashboard** — GPU-accelerated psychedelic dashboard with animated charts, TRON grid overlay, and cyberpunk aesthetic

### Backend System (Safe by Default)

Auto-detect order: **Sysfs → JSON → Mock** (Luwen excluded from auto-detect).

| Backend | Method | Safe on active HW? | Permissions |
|---------|--------|--------------------|-------------|
| Sysfs   | Linux hwmon (`/sys/class/hwmon/`) | ✅ Yes | None |
| JSON    | `tt-smi -s` subprocess | ✅ Yes | None |
| Mock    | Simulated telemetry | ✅ N/A | None |
| Luwen   | Direct PCI BAR0 access | ⚠️ May disrupt | root / ttkmd |

Luwen is only accessible with `--backend luwen` and never used in auto-detect, preventing accidental interference with running LLMs or training jobs.

### Architecture Support

- **Grayskull**: 10×12 Tensix grid, 4 DDR channels
- **Wormhole**: 8×10 Tensix grid, 8 DDR channels
- **Blackhole**: 14×16 Tensix grid, 12 DDR channels

### Multi-Chip Visualization

Memory Castle and Arcade modes automatically detect multiple devices and render side-by-side with per-device color coding (hue shift per device). Particle density reflects real power differentials (e.g. 12W vs 18W across 4 Blackhole chips).

## Tenstorrent PPA Integration

The `tt-toplike` package integrates with the Tenstorrent PPA ecosystem:

```
tt-toplike Recommends → tt-smi          (required for JSON backend)
tt-toplike Recommends → tenstorrent-dkms (required for sysfs hwmon driver)
tt-toplike Suggests   → tt-toplike-egui  (optional GPU dashboard)
tt-toplike Suggests   → tenstorrent-tools
```

Install the full stack:
```bash
sudo apt install tt-toplike tt-smi tenstorrent-dkms
```

## Building .deb Packages

```bash
# Full build (vendors crates, builds both packages)
./build-deb.sh

# Skip re-vendoring (vendor/ already present and current)
./build-deb.sh --quick

# Inspect the packages
dpkg-deb --info ../tt-toplike_0.1.0_amd64.deb
dpkg-deb --contents ../tt-toplike_0.1.0_amd64.deb

# Or use cargo-deb for quick developer iteration
cargo install cargo-deb
cargo deb                                          # builds tt-toplike TUI
cargo deb --bin tt-toplike-egui --features egui   # builds egui package
```

The `vendor/` directory (~80 MB) is committed to git for reproducible offline builds. The `debian/rules` uses `--frozen` to enforce no network fetches at build time, matching Debian build daemon behavior.

## Architecture

```
┌─────────────────────────────────┐
│   tt-toplike (TUI / egui)       │
└───────────────┬─────────────────┘
                │
        ┌───────┴───────┐
        │  Backend Trait │
        └──┬──┬──┬──┬───┘
           │  │  │  │
       Sysfs JSON Mock Luwen
      (hwmon)(tt-smi)   (PCI†)

† explicit --backend luwen only
```

## Testing

```bash
cargo test
cargo build --bin tt-toplike-tui --features tui    # zero warnings
```

## License

Apache-2.0 — Tenstorrent
