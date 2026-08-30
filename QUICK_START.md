# tt-toplike Quick Start

**Version**: 0.11.1
**Last Updated**: August 29, 2026

---

## Install

The quickest path on Ubuntu is the **Tenstorrent apt repository** (`ppa.tenstorrent.com`) — always the newest version, updated with `apt upgrade`:

```bash
# Add Tenstorrent's signing key + apt repository (one time)
sudo install -m 0755 -d /etc/apt/keyrings
sudo wget -qO /etc/apt/keyrings/tt-pkg-key.asc https://ppa.tenstorrent.com/tt-pkg-key.asc
echo "deb [signed-by=/etc/apt/keyrings/tt-pkg-key.asc] https://ppa.tenstorrent.com/ubuntu/ $(. /etc/os-release && echo "$VERSION_CODENAME") main" \
  | sudo tee /etc/apt/sources.list.d/tenstorrent.list
sudo apt update

# Install
sudo apt install tt-toplike        # TUI monitor
sudo apt install tt-toplike-app    # optional native window host
```

Prefer a pinned `.deb`, macOS, or building from source? See the [README](README.md#installation).

---

## Launch Modes

### Arcade Mode (Unified Visualization)
```bash
tt-toplike --mode arcade
tt-toplike -m arcade
```
- All three visualizations stacked: Starfield (top) / Memory Castle (middle) / Memory Flow (bottom)
- Hero character (`@`) that moves with live telemetry (X = current, Y = power, color = temperature)
- Topology diagram in the header: chip-to-chip links shown for carrier boards (p300/n300), suppressed for independent PCIe cards (p150a/n150)
- >8 chips: compact mini-bar (one character per chip) instead of the detailed diagram

### Memory Castle (DDR Hierarchy)
```bash
tt-toplike --mode castle
```
- Side-by-side per-device columns (scales dynamically with terminal width)
- Fleet grid automatically switches in for large chip counts (32+)
- Board grouping (`║` separators) only shown for dual-chip carrier boards; p150a and similar single-chip cards show as independent columns with `│`
- 600 particles per device: Read ○◉ / Write □■ / CacheHit ◇◆ / Miss ●⬤, with trails

### Starfield (Tensix Cores)
```bash
tt-toplike --mode starfield
```
- Stars = Tensix cores (brightness = power, color = temperature, twinkle = current)
- Memory hierarchy planets orbiting each device cluster

### Memory Flow (NoC)
```bash
tt-toplike --mode flow
```
- Particles stream between the DDR perimeter and Tensix core grid
- Density = traffic, color = temperature, speed = current draw

### Insights (Default)
```bash
tt-toplike               # default
tt-toplike --mode normal # alias — "normal" now maps to Insights
```
- Split-panel view: a **chip portrait** per processor (CPU/GPU/ANE/TT as a device card) with live power, temperature, DDR/training status, and an accuracy/activity trend
- The per-chip sidebar also shows **current** (against the board's TDC limit), **board power** (tt-smi ≥ 6.0.0), **PCIe** link geometry with live ▼/▲ bandwidth, and — only when they're non-zero, because zero is the healthy answer — **GDDR ECC** errors (uncorrectable in red) and the lifetime **thermal-trip** count. Power and temperature are compared against the board's *real* firmware limits, not generic reference values
- A **TT-process panel** lists processes by resource use and tags any that match a known inference runtime (ollama, vLLM, llama.cpp, MLX, ComfyUI, …); on TT hardware it also attributes per-process device usage and serving metrics
- `↑`/`↓` navigate the process list; `k` silences the selected process's alerts, `K` kills it (Linux)
- `Enter` zooms into the selected chip portrait; `Esc` zooms back out
- At **32+ devices** the portraits collapse into a fleet heatmap grid; `←`/`→` page through the fleet

### Inference Server Monitor (`[i]`)
```bash
# from any view, just press:  i
```
- The flagship unified serving **snake** with three telemetry-true states: **cold** (a hungry snake roams the model-catalog starfield — footer "N of M models run on your <arch>"), **loading** (a coiling boxed snake through compile → load → ready in a synthwave palette + hot-pink burst on Ready, with a footer band explaining the phase and showing **compiled** kernels vs **loaded** weight shards), and **serving** (a live dashboard from the server's vLLM `/metrics`: green **READY** header, throughput timeline, token-exhaust snake, request swimlanes, stats panel, TT silicon strip)
- `i` while in it jumps to Insights; `Esc` backs out to where you came from. It also sits at the tail of the `v` cycle. Press `l` for the legend, `/explain` for the mapping overlay

### HivemindSweeper (`~`)
```bash
tt-toplike --mode hivemind    # or press ~ in any mode
```
- Opt-in, **read-only** activity sniffer: *"what's touching my TT hardware right now — and is it making progress?"*
- Built for **finding signs of life** when the interesting work isn't logging where you're watching — silent kernel compiles, a model holding the device, or serving throughput buried in docker DEBUG logs
- Correlates five passive sources — `/dev/kmsg` driver messages, tt-metal compile-cache churn, `/proc` process + device-fd activity, log tails (incl. `docker logs`), and the tt-metal Inspector — into a **source × device** heat board + a **coalesced feed**: repeats fold into one row with a live count · rate · sparkline (e.g. `metal · ncrisc.elf ×136`, `vLLM · holding /dev/tenstorrent/#`, `vLLM · Avg generation throughput …`)
- Identifies interpreter-hosted workloads by cmdline **and loaded TT libraries** (`/proc/<pid>/maps`), so a `python -m pytest …` shows as `metal`/`ttnn`, never a bare "unknown"
- Point it at a target: `/watch <path>`, `/watch pid <n>`, `/wrap <cmd…>`. `f` unified feed · `s` severity floor · arrows/`hjk` cursor · `l` legend
- The red **KITT scanner** along the bottom slows, focuses, and brightens as total activity climbs
- Safe: collectors spawn only while the mode is active and stop on exit; never sets a debug env var or reads a device buffer

### Training (`t`)
```bash
# from any view, just press:  t
```
- Auto-attaches to a live [tt-train](https://github.com/tenstorrent/tt-metal/tree/main/tt-train) run — no flags, no target to name. Scans for a `nano_gpt`/`mnist_mlp`/`linear_regression` process, resolves `/proc/<pid>/fd/1` to its log, and starts tailing it
- The model as a character-grid network (columns = transformer blocks, nodes = attention heads) fed by token particles, amber forward sweeps, violet backward/gradient sweeps
- A loss "mountain range" descends as the model converges (magenta = high loss → teal = low loss, each column keeping its own loss's hue as a run-history timeline), under an aurora/starfield nightscape that opens up as loss drops; a comet crosses the sky on each checkpoint save
- **Requires stdout redirected to a file** (`> train.log`) at launch — if fd 1 is a pipe or tty the per-step stream can't be tailed after the fact (an OS property, not a gap), and the view says so instead of drawing a fake curve
- Shows only what tt-train actually emits live (step, loss, step time, cache entries) — no gradient norms or MFU live, but derives its own tokens/sec and ETA from what it can read. `l` for the legend, `/explain` for the mapping overlay
- `Esc` backs out to where you came from. Deliberately excluded from the `v` cycle, like `i`

---

## Backend Options

### Auto-detect (Safe Mode — default)
Tries: **Hybrid (sysfs + background JSON) → JSON → Mock** on Linux (Luwen is never auto-detected)

### Sysfs (Non-invasive)
```bash
tt-toplike --backend sysfs
```
- Reads the **two** sysfs surfaces the Tenstorrent driver exposes, both world-readable ordinary files:
  - Linux hwmon (`/sys/class/hwmon/`) — temperature, voltage, power, current, fan, and the board's real
    `*_max` limits (a p300c reports 125 W / 500 A / 90 °C). Sensors are picked by their `*_label`, so
    the ASIC temp sensor is used rather than whichever happens to be `temp1`
  - tt-kmd class attributes (`/sys/class/tenstorrent/tenstorrent!N/`) — AICLK/AXICLK/ARCCLK, ARC
    firmware heartbeat, board SKU, firmware bundle version, serial, thermal-trip count, and the
    `pcie_perf_counters/` that give **live PCIe bandwidth**
- Zero interference with running workloads — safe during LLM inference; no subprocess, no device open
- Needs **tt-kmd ≥ 2.7** for the class attributes and **≥ 2.9** for the `*_max` limits on Blackhole.
  Older drivers just expose less; nothing breaks
- Still needs `tt-smi` (i.e. the JSON or hybrid backend) for the deeper SMBUS block: DDR training
  status, GDDR temperatures and ECC counters, per-component firmware versions, and the PCIe link's
  generation/width

### JSON (tt-smi)
```bash
tt-toplike --backend json
```
- Runs `tt-smi -s` as a subprocess
- Requires `tt-smi` installed
- Adds the full SMBUS block, plus (on tt-smi ≥ 6.0.0) per-device process attribution and board power.
  No PCIe bandwidth — that comes from the driver counters, which only sysfs/hybrid read

### Hybrid (the default on Linux)
```bash
tt-toplike --backend hybrid
```
- Sysfs on the fast path plus a background `tt-smi` stream for enrichment — the union of the two above
- Runs sysfs-only when `tt-smi` isn't installed

### Host (CPU/RAM — any machine, no TT hardware)
```bash
tt-toplike --host                 # or: tt-toplike --backend host
tt-toplike --host --mode arcade
```
- **Runs on Linux, macOS, and Windows** — the way to try tt-toplike on a laptop with no Tenstorrent hardware
- Maps your CPU (frequency → AICLK, utilization → current), RAM (→ DDR channels), and — on Linux — package temperature (hwmon) and power (RAPL) into the normal telemetry fields
- On macOS/Windows, temperature and power aren't exposed by the OS, so those read as 0; everything else is live
- Real data (unlike `--mock`), just describing your CPU instead of a TT accelerator

### Mock (Testing)
```bash
tt-toplike --mock --mock-devices 4
```
- No hardware required; fully simulated telemetry (use `--host` instead if you want your real CPU/RAM)

### Luwen (Direct PCI — explicit only)
```bash
tt-toplike --backend luwen
```
- Kernel-mediated (tt-kmd) reads; mostly passive, but Luwen/UMD arbitration is
  unresolved upstream — ⚠️ avoid during workloads, especially multi-chip/galaxy
- Never used in auto-detect; must be requested explicitly

---

## Common Commands

```bash
tt-toplike --mode arcade --backend sysfs    # arcade on real hardware (safe)
tt-toplike --mode castle --interval 50      # castle at 50ms refresh
tt-toplike --mode starfield -v              # starfield with verbose log
tt-toplike --mode flow --devices 0,2       # flow, devices 0 and 2 only
tt-toplike --mock --mock-devices 8          # 8-device mock (shows fleet grid)
tt-toplike --mock --mock-devices 32         # 32-device mock (fleet grid + mini-bar)
tt-toplike --host --mode arcade             # your real CPU/RAM in arcade mode (macOS/Linux/Windows)
```

---

## Keyboard Shortcuts (in TUI)

| Key | Action |
|-----|--------|
| `v` | Cycle visualization modes: Insights → Flow → Starfield → Castle → Arcade → Defrag → Inference → Insights |
| `i` | Open the Inference Server Monitor from any view; `i` while in it jumps to Insights, `Esc` backs out to where you came from. Also at the tail of the `v` cycle |
| `t` | Open the Training view from any view — auto-attaches to a running tt-train process with no further input; `Esc` backs out. Excluded from the `v` cycle, like `i` |
| `l` | Toggle legend overlay for the current view |
| `b` | Cycle backend live: Hybrid → Sysfs → JSON → Mock → Host → Hybrid. Luwen and Remote are launch-only — the cycle never steps onto them |
| `r` | Force refresh |
| `q` / `ESC` | Quit |

### Slash-commands (press `/` then type)

| Command | Action |
|---------|--------|
| `/fps <1–120>` | Set animation frame rate |
| `/datafps <1–30>` | Set telemetry poll rate |
| `/mode <name>` | Jump to a mode: `insights`, `grid`, `starfield`, `castle`, `flow`, `arcade`, `defrag` |
| `/theme grayskull` | App-wide grayscale — a thousand shades of grey with cyan/purple accents and hot pink as the only hot color (`/theme default` restores, bare `/theme` toggles) |
| `/legend` | Toggle the legend overlay (same as `l`) |
| `/explain` | Toggle the "how it maps to hardware" overlay |

---

## Native Window App

```bash
tt-toplike-app            # PTY-hosted TUI in a native eframe window
tt-toplike-app --mock --mock-devices 4
```

---

## Troubleshooting

### No hardware detected
```bash
ls /sys/class/hwmon/       # check hwmon entries (Linux)
tt-toplike --host          # real CPU/RAM telemetry, any OS — no TT hardware needed
tt-toplike --mock --mock-devices 2   # or fully simulated, to verify the binary works
```

### tmux / SSH terminal colors
tt-toplike uses `Color::Reset` backgrounds throughout — no grey boxes.
For best results, ensure your terminal emulator supports 24-bit color.

### Board labels look wrong
If you have independent PCIe cards (p150a, n150) and see "Board 0 → [Dev0, Dev1]"
groupings, make sure you're running v0.6.0 or later. The fix auto-detects
`chips_per_board` from the hardware's `board_type` field.

---

## Build from Source

```bash
cd ~/code/tt-toplike

# TUI (safe defaults — no Luwen, no GUI)
cargo build --release --bin tt-toplike-tui --features tui,json-backend,linux-procfs

# Native window app
cargo build --release --bin tt-toplike-app --features app,json-backend
```
