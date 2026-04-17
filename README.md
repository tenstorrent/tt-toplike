# TT-Toplike-RS 🦀

Real-time hardware monitoring for Tenstorrent silicon (Grayskull, Wormhole, Blackhole). Written in Rust.

![3 visualizers](/assets/tt-toplike-rs.png "Screenshot of 3 simultaneous visualizers")

## How visualizations are grounded in hardware activity

The visualizations aren't decorative. Every particle, star, color shift, and brightness change maps to a real signal coming off the hardware. Here's what you're actually looking at.

### The signals

tt-toplike reads a small set of telemetry values from the chip — through the Linux hwmon kernel interface (sysfs), through `tt-smi`, or directly through Luwen — and drives everything from those:

- **Power (W)** — total chip power draw, measured continuously
- **Temperature (°C)** — die temperature from the ASIC thermal sensor
- **Current (A)** — current draw; a fast-moving proxy for compute intensity
- **DDR training status** — per-channel bitmask from SMBUS: whether each DDR channel is idle, training, trained, or faulted
- **ARC heartbeat** — the RISC-V management firmware pulses this register to signal it's alive

There's also an **adaptive baseline**: for the first ~20 samples the tool learns your chip's idle state. After that, everything is shown as *relative change* from baseline rather than absolute values. A chip drawing 20W shows the same visual intensity as a chip drawing 80W at the same fraction above its idle state. This makes the tool work equally well across hardware generations.

### How colors are chosen

Every color is computed from a single function: `hsv_to_rgb(hue, saturation, value)`.

**Hue** (which part of the spectrum) is a combination of:
- A *temperature anchor* — `temp_to_hue()` maps 0–100 °C to 180°–0° (cyan at cold, red at hot). This is the baseline.
- A *time drift* — the frame counter slowly rotates the hue through the full 360° wheel over ~7 seconds. This is what produces the rainbow sweep during LLM inference: each inference burst arrives in a different part of the spectrum.
- A *position offset* — in the starfield, each Tensix core has its own phase, so the grid shows a color wave rather than all cores flipping simultaneously. In Memory Castle, the four particle types are spaced 90° apart (a tetrad), so Read/Write/CacheHit/CacheMiss are always visually distinct regardless of where the sweep currently is.
- A *channel spread* — in Memory Flow, each DDR channel adds 30° of offset, so 12 channels fan out across the entire color wheel.

**Saturation** (vividness) is pinned at or near 1.0 everywhere except dim background elements. There's no muting.

**Value** (brightness) is driven by activity: low power → dim characters and low value; high power → bright characters and high value. In the starfield, the character itself also encodes brightness (`·∘○◉●`), so you get two independent brightness cues — color value and character weight — which together create good depth contrast at different activity levels.

The result is that idle hardware shows a slowly-rotating pastel palette, and active hardware shows saturated, vivid colors cycling rapidly through the spectrum.

### What an LLM thinking looks like

Autoregressive inference (token-by-token generation) has a characteristic rhythm. Each token is a sequential forward pass — attention over a growing KV cache, then a matrix multiply through the FFN. Between tokens there's almost nothing happening. This creates a **pulsed pattern**: brief compute bursts spaced by the model's generation cadence, typically a few hundred milliseconds each. The power trace looks like a comb.

In Memory Castle, this shows up as waves of particles that swell and thin. In the starfield, stars pulse brighter during each token's compute burst then settle back. The particle hue sweeps through the rainbow over about 7 seconds — so during a long thinking pause you'll see one color family, and the burst of the next token arrives in a different hue band.

Temperature lags power by several seconds (thermal mass of the package), so the color signal always trails the activity signal. You can see the chip "remembering" the heat from three tokens ago.

### What WAN 2.2 generation looks like

Video diffusion is a different animal. Each denoising step is a full forward pass of a large model — not a quick autoregressive decode but a sustained, high-memory-bandwidth computation that runs for hundreds of milliseconds. Steps happen sequentially through the diffusion schedule.

The result is **sustained high power with structured plateaus**: you'll see the visualization stay dense and bright for the full duration of a step, then briefly relax between steps as the scheduler loads the next noise level. The Memory Castle particles stop thinning out between bursts — the dungeon stays full. In Arcade mode, the `@` hero sits high and right (high power, high current) and barely moves, which is its own kind of signal.

Temperature climbs higher and holds there. The color of everything — stars, particles, backgrounds — shifts warmer because `temp_to_hue()` biases toward red as the die heats up.

### Why idle still has a lot of activity

A "quiet" Blackhole is never actually quiet. Several things generate continuous background power:

- **ARC firmware** — the four RISC-V management cores run continuously handling thermals, SMBUS communication, PCIe link monitoring, and power regulation. This costs a few watts of baseline power.
- **DDR refresh** — LPDDR keeps all its trained channels alive with periodic refresh cycles. The DDR channels show as trained (solid bars) even with no user workload.
- **SRAM retention** — L1 and L2 SRAM need continuous power to hold state. The tensix grid never fully powers down.
- **PLL lock** — the clock network (AICLK, AXICLK, ARCCLK) runs continuously.

The adaptive baseline captures all of this and treats it as zero-point. What you see in the visualization at idle is the true floor: particles spawning slowly and evenly, stars dim but present, the hero character drifting in the lower-left of the Arcade canvas. That floor has meaning — it's the hardware telling you it's alive and maintained.

### Running `tt-smi -r` while watching

`tt-smi -r` triggers a hard reset of the TT device: PCIe link goes down, the ARC firmware restarts from scratch, and all DDR channels retrain from zero. If you have tt-toplike running and do this in another terminal, you get a genuine light show backed by real hardware events:

1. **Power drop** — as the chip resets, power briefly collapses toward zero. Particles stop spawning. The starfield dims out. The dungeon goes quiet.
2. **DDR retraining** — the SMBUS DDR status bitmask flips channel-by-channel from *trained* → *idle* → *training* → *trained* as each channel comes back online. In Memory Flow's channel bars, you watch the channels relight one at a time. This takes a few seconds and the order is deterministic per chip.
3. **ARC restart** — the heartbeat goes dark and comes back. ARC health indicators flicker red then green as the firmware finishes booting.
4. **Power renormalization** — once the chip is back, the adaptive baseline has to re-learn idle state over the next 20 samples. During this window the visualization is slightly over-reactive — everything looks more active than it is while the baseline recalibrates. This produces the most visually intense few seconds of the whole sequence.

The full reset-to-stable cycle typically takes 10–15 seconds. tt-toplike's safe backends (sysfs, JSON) survive the reset without crashing because they're just reading kernel files — they just see a brief gap in data.

---

## Installation

### Debian / Ubuntu (recommended)

Pre-built `.deb` packages are produced by `build-deb.sh` and can be installed directly:

```bash
# Build packages (requires cargo, debhelper, rustc ≥ 1.75)
./build-deb.sh

# Install
sudo dpkg -i ../tt-toplike_0.3.0_amd64.deb         # TUI monitor
sudo dpkg -i ../tt-toplike-egui_0.3.0_amd64.deb    # egui dashboard (optional)

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
dpkg-deb --info ../tt-toplike_0.3.0_amd64.deb
dpkg-deb --contents ../tt-toplike_0.3.0_amd64.deb

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
