# Host Backend — Run on Any Machine

`--backend host` (or `--host`) lets you run tt-toplike on any Linux machine without Tenstorrent hardware. It reads your CPU frequency, temperature, and RAM utilisation from the OS and maps them into the same telemetry fields that drive the visualisations.

## What you see

Every visualization works identically:

| Visualization | What it shows with TT hardware | What it shows with `--host` |
|---|---|---|
| **Starfield** | Tensix cores as stars; power drives brightness; temperature drives color | CPU logical cores as stars; same mapping |
| **Memory Castle** | DDR→L2→L1→Tensix particle hierarchy | Particle density from RAM usage; temperature drives color |
| **Memory Flow** | DRAM channel particles + DDR channel bars | Per-socket RAM fill in channel bars; temperature drives color sweep |
| **Arcade** | `@` hero driven by current/power; chip topology header | `@` hero driven by CPU load/power; single "socket" topology |
| **Defrag** | GDDR block map, DMA fill, EVICT animation | RAM usage fill; blocks proportional to used/total |

## How fields are mapped

| Telemetry field | TT hardware meaning | `--host` source |
|---|---|---|
| `asic_temperature` | ASIC die temperature (°C) | CPU package temp via `/sys/class/hwmon/*/temp*` (coretemp or k10temp) |
| `aiclk` | Tensix AI clock frequency (MHz) | Max CPU core frequency from sysinfo |
| `power` | Board total power draw (W) | RAPL package energy via `/sys/class/powercap/intel-rapl/*/energy_uj` (when available), otherwise a proxy: `cpu_usage% × 45W` |
| `current` | Supply current (A) | CPU utilisation × frequency proxy |
| `voltage` | Supply voltage (V) | Not available; always `None` |
| `ddr_speed` | DDR speed in MT/s | Fixed `5600` (typical DDR5) |
| `ddr_status` | Per-channel training status | RAM utilisation percentage |

## Device topology

One `Device` entry is created per CPU socket. Most desktop systems have a single socket. The device is reported as `Architecture::Unknown` with bus ID `socket:0`.

## RAPL power sampling

On Intel and AMD CPUs with Linux RAPL support, `--host` reads:

```
/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj
```

It takes two successive samples, computes the delta in microjoules, and divides by elapsed time to get watts. The first sample produces no power reading (no delta yet); subsequent updates produce real power measurements.

If RAPL is not available (VM, older kernel, unsupported CPU), power falls back to a linear proxy: `avg_cpu_usage% × 45W`.

## Temperature detection

Temperature is read from the first hwmon device whose `name` file contains `coretemp` (Intel) or `k10temp`/`zenpower` (AMD). It finds the entry whose `temp*_label` says `Package id 0` (Intel) or `Tdie`/`Tccd1` (AMD) and reads the corresponding `temp*_input` millidegrees value.

If no hwmon temperature sensor is found, `asic_temperature` is `None` and the visualisations use a neutral color (neither hot nor cold).

## Limitations vs TT hardware

- No DDR bandwidth or per-channel training status — the Memory Flow channel bars show RAM fill rather than actual DRAM traffic
- No ARC firmware heartbeat — `heartbeat` is always `1` (alive)
- No per-core Tensix grid geometry — the starfield draws all logical cores in a square grid rather than the chip's actual 14×16 or 8×10 layout
- No SMBUS telemetry — fields like `board_id`, `ddr_speed` (real), and `tdc` are absent
- Single-socket assumption — multi-socket NUMA systems show only one device

## Usage

```bash
# Interactive TUI (auto-starts in normal table view)
tt-toplike --host

# Jump straight to a visualization
tt-toplike --host --mode arcade
tt-toplike --host --mode flow
tt-toplike --host --mode starfield

# Verbose: see what the backend is reading
tt-toplike --host -v
```
