# Apple GPU + ANE telemetry for the Host backend (macOS, no-sudo hybrid)

**Status:** Design — awaiting review
**Branch:** `macos-cpu-mode`
**Date:** 2026-06-25

## Problem

`tt-toplike --host` on macOS reads only CPU (utilization, frequency) and RAM via
`sysinfo`. Models run on a Mac actually execute on the **GPU** (Metal / MPS /
MLX / Core ML) and the **Apple Neural Engine (ANE)** — neither of which the CPU
sampler can see. So an accelerator-bound inference run leaves the visualization
looking idle. We want those AI chips to show up.

## Goal

Surface Apple GPU and ANE activity as additional devices in the Host backend, so
the existing multi-device visualizations render them automatically:

- **GPU**: utilization-driven particle activity, a star grid sized to GPU cores,
  GPU memory driving the DDR-channel bars.
- **ANE**: power-derived activity so a Core ML / ANE workload lights up.

All **without sudo**, macOS-gated so Linux/TT builds are untouched, and degrading
gracefully to today's CPU-only behavior if any source is unavailable.

## Chosen approach: hybrid data sources

Two sources, each used for exactly what it does cleanly:

| Chip | Source | Access | Why |
|------|--------|--------|-----|
| GPU  | `ioreg -r -d 1 -c IOAccelerator` → `PerformanceStatistics` | subprocess, **no sudo**, no unsafe | utilization % + memory are plain public-ish fields; a string parse is robust and fixture-testable |
| ANE  | Apple's private `IOReport` framework (energy channels) | FFI, **no sudo** | the **only** no-sudo way to see ANE; reports ANE *power*, not a utilization % |

Rejected alternatives (from scoping):
- **ioreg-only**: simplest, but ANE stays invisible — and ANE is the headline "AI chip."
- **Full IOReport** (GPU+ANE+power via private API): reimplements GPU stats we can already get cleanly via ioreg, for a larger unsafe surface and more macOS-drift risk.
- **powermetrics**: complete, but requires **sudo** — breaks the project's safe-by-default ethos.

The hybrid confines the unsafe/private surface to the one capability that
genuinely requires it (ANE power), while GPU stays a plain, testable parse.

## Verified ground truth (Apple M4 Pro, this machine, no sudo)

`ioreg -rc IOAccelerator`:
- `model = "Apple M4 Pro"`, `gpu-core-count = 20`
- `PerformanceStatistics`: `Device Utilization %`, `Renderer Utilization %`,
  `Tiler Utilization %`, `In use system memory`, `Alloc system memory`
- GPU **frequency/power are NOT** plain fields (only in the private IOReport
  "GPU Performance States" subgroup) → GPU power/temp/clock stay `None`.

ANE: no `ane`/`neural` node in `ioreg`; ANE energy is only in IOReport's
"Energy Model"/PMP channels (the channel `macmon` reads for ANE power).

## Architecture

Three small, independently-testable units. No visualization changes — the viz is
already multi-device, and the `Device` geometry overrides (`grid_override`,
`channels_override`) added earlier on this branch are exactly the hook the GPU/ANE
"devices" need.

### 1. `src/backend/gpu_macos.rs` (`#[cfg(target_os = "macos")]`)
- `fn parse_ioreg(output: &str) -> Option<GpuSample>` — **pure**, fixture-testable.
- `fn sample() -> Option<GpuSample>` — thin `Command::new("ioreg")` wrapper calling `parse_ioreg`.
- `struct GpuSample { model: String, core_count: usize, util_pct: f32, mem_in_use_bytes: u64, mem_alloc_bytes: u64 }`

### 2. `src/backend/ane_macos.rs` (`#[cfg(target_os = "macos")]`)
- Encapsulates **all** unsafe FFI + Core Foundation interop behind a safe API:
  - `struct AneSampler { /* IOReport subscription + previous sample */ }`
  - `AneSampler::new() -> Option<Self>` — subscribe to the energy channels; `None` if symbols/channels are unavailable (future macOS).
  - `AneSampler::sample(&mut self) -> Option<f64>` — returns **ANE watts** by taking an IOReport delta against the previous sample over elapsed time.
- FFI surface (declared `extern "C"`, linked from `libIOReport`, the pattern `macmon` uses; MIT reference only, not a dependency): `IOReportCopyChannelsInGroup`, `IOReportCreateSubscription`, `IOReportCreateSamples`, `IOReportCreateSamplesDelta`, `IOReportIterate` + channel/value accessors.
- New **macOS-target-only** deps (mirroring the `procfs` Linux-target gating so Linux/TT never pulls them): `core-foundation`, and `block` for the iterate callback.

### 3. `HostBackend` integration (`src/backend/host.rs`)
- `init()`: after the CPU device, conditionally append:
  - **GPU device** (index 1) if `gpu_macos::sample()` succeeds — `board_type: "apple-gpu"`, `bus_id: "gpu:0"`, `grid_override` from `gpu-core-count` (20 → near-square grid via the helper already used for CPU cores), `channels_override: Some(4)`.
  - **ANE device** (index 2) if `AneSampler::new()` succeeds — `board_type: "apple-ane"`, `bus_id: "ane:0"`, `grid_override: Some((4, 4))` (Apple's stated 16-core ANE — a nominal figure, not from an API; documented as such), `channels_override: Some(1)`.
- `update()`: sample GPU (ioreg) and ANE (IOReport) **throttled to ~1s** and cached between ticks, so we don't spawn `ioreg` or re-sample at the 100 ms UI rate (mirrors HybridBackend's background-poll cadence).

### Field mapping

| Telemetry field | GPU | ANE |
|-----------------|-----|-----|
| utilization → `current` | `Device Utilization %` | `power / nominal_max_ane_w` (synthetic %, so the viz animates) |
| `power` (W) | `None` (not no-sudo via ioreg) | ANE watts from IOReport |
| star grid | `gpu-core-count` (20) | nominal 16 → 4×4 |
| DDR bars | GPU `In use`/`Alloc system memory` | n/a (1 synthetic channel) |
| `aiclk` / `temp` | `None` | `None` |

## Data flow

```
HostBackend::update()  (throttled ~1s)
  ├─ sysinfo            → CPU device telemetry        (device 0, existing)
  ├─ gpu_macos::sample  → ioreg parse → GPU telemetry  (device 1, if present)
  └─ AneSampler::sample → IOReport delta → ANE watts   (device 2, if present)
            │
            ▼
   existing multi-device visualizations (no changes)
   render one chip-portrait / starfield column / particle stream per device
```

## Error handling — everything degrades gracefully

- `ioreg` missing / errors / unparseable dict → **GPU device not added**.
- IOReport symbols or ANE channels unavailable (e.g. future macOS) → `AneSampler::new()` returns `None` → **ANE device not added**; a `log::debug!` notes why.
- CPU-only `--host` therefore always works, exactly as today. No panics; FFI is wrapped so a `None`/missing channel never aborts.
- All new code is `#[cfg(target_os = "macos")]`; non-macOS builds compile and behave identically to now (guaranteed by the cross-backend isolation tests already on this branch).

## Testing

- **`gpu_macos`**: unit-test `parse_ioreg` against a captured real fixture (the
  output from this M4 Pro) — assert util/mem/core-count extraction; plus a
  malformed-input case returning `None`.
- **`ane_macos`**: unit-test the pure energy-delta → watts math with injected
  numbers (the FFI boundary itself can't be fixtured; it needs the live
  framework). Wrap FFI so failures are `None`, not panics.
- **Integration (macOS-gated)**: extend `tests/backend_isolation.rs` — a Host
  backend with GPU/ANE devices present has non-empty grids, ≥1 channel each, and
  survives all four visualizations (reusing the existing harness).
- **Linux/TT untouched**: macOS-target deps + `cfg(target_os = "macos")` code
  don't compile off-mac; the existing isolation suite continues to pass.

## Limitations (to document in README under `--host`)

- **ANE is power-derived activity, not a true utilization %** (no API exposes ANE %).
- ANE **"16 cores" is Apple's marketing figure**, used only to size the grid — not from any API.
- **GPU power/temp/frequency remain absent** (ioreg can't provide them no-sudo).
  *Cheap future add-on:* since IOReport is already linked for ANE, GPU power is a
  small extension — deliberately left out to honor the chosen hybrid split / YAGNI.
- GPU utilization is **whole-device**, not per-process (includes WindowServer, other apps).
- **Private-API drift**: ANE IOReport channel names/groups are undocumented and may
  change across macOS/chip generations — contained to `ane_macos.rs` with `None` fallback.
- **Linux GPUs (NVIDIA/AMD) still unaddressed** — separate NVML/ROCm effort.

## Effort & risk

- **~3–4 days.** GPU/ioreg side is ~half a day; the bulk is the IOReport FFI + CF
  interop for ANE and per-macOS channel validation.
- **Risk: low–medium.** Additive, macOS-gated, no sudo, no new deps off-mac,
  graceful fallback. The only real fragility is private-API drift, isolated to one file.

## Out of scope

- ANE/GPU **power on Linux**, NVIDIA/AMD GPUs, sudo-based powermetrics, per-process
  GPU attribution, GPU frequency, and any visualization redesign.
