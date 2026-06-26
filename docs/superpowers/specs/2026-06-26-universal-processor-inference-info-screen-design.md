# Universal processor + inference-process info screen

**Status:** Design — awaiting review
**Branch:** `macos-cpu-mode`
**Date:** 2026-06-26

## Problem

The default screen (Insights) pairs per-device chip portraits with a process
panel, but both are gated `cfg(all(target_os = "linux", feature = "linux-procfs"))`
and are entirely TT-specific: the process panel scans `/proc` for processes
holding `/dev/tenstorrent` handles and probes known TT inference servers, and
`ProcessInfo` carries TT-only fields (device indices, hugepages, TT mappings).
On a host — macOS especially — none of that applies, so the screen falls back to
a bare chip-portrait grid with **no process information at all**.

We want the default screen to (a) present the host's processors — CPU, Apple
GPU, ANE, and later NVIDIA/AMD — with the same rich cards used for TT devices,
and (b) show processes that **might be using inference of any kind**, on any OS.

## Goal

One cross-platform Insights screen:
- **Rich per-device panels** for whatever processors exist (retiring the macOS
  grid fallback for the default screen).
- A **unified process panel** that lists processes by resource use and **tags
  the ones that match known ML/inference runtimes**, on every backend.
- On TT hardware, the existing device-fd attribution and serving-server metrics
  are **layered back in** by PID — the proven TT detection is preserved, not
  replaced.

## Decisions (from brainstorming)

- **Detection = hybrid:** list top processes by CPU/memory (cross-platform via
  `sysinfo`) **and** tag those whose name/cmdline matches a curated ML-runtime
  list. Matched processes sort first. (Not name-only; not resource-only.)
- **Scope = universal:** the cross-platform process panel applies to all
  backends; TT-specific data enriches it where available.
- **Device rendering:** the host uses the same rich panels as TT (un-gate the
  existing renderers), not the grid fallback.
- **Enrich-by-PID:** keep the existing Linux `process_monitor` + `serving`
  detection unchanged; merge their data into the unified records by PID.

## Architecture

### Relax the Insights gates (one cross-platform path)
Today `render_insights`, `render_device_panels`, `render_process_panel`, and the
helper chain `panel_layout` / `device_panels_height` / `render_fleet_heatmap_panel`
are all `cfg(all(target_os = "linux", feature = "linux-procfs"))`, with a
`render_insights_no_procfs` → `render_grid_mode` fallback. We:
- Un-gate `render_device_panels` and its helper chain (confirmed procfs-free in
  the GPU/ANE review) so the host shows rich per-device cards.
- Make `render_process_panel` cross-platform, rendering a unified record.
- Collapse `render_insights` + `render_insights_no_procfs` into one
  cross-platform `render_insights` (header + device panels + process panel).
  `render_grid_mode` remains, but only as the explicit `Grid` view mode.
- Push only the genuinely procfs/TT-specific bits (device-fd attribution,
  serving probes, hugepages) behind `cfg`.

### New component: `workload::host_processes` (cross-platform)
A `HostProcessMonitor` built on `sysinfo` (already a dependency), owned by the
TUI run loop and refreshed on the existing ~2 s cadence (alongside the Linux
`ProcessMonitor`):
- Enumerates processes → `pid, name, cmdline, cpu_pct, mem_bytes`.
- A pure classifier `inference_match(name: &str, cmdline: &str) -> Option<&'static str>`
  matches a curated, easily-extended ML-runtime list (e.g. `ollama`,
  `llama.cpp`/`llama-server`, `mlx_lm`, python with
  `torch`/`vllm`/`transformers`/`diffusers`/`jax`/`tensorflow`, `ComfyUI`,
  `stable-diffusion`, `koboldcpp`, `whisper`, `lm-studio`). Returns the matched
  runtime label or `None`.
- Selection: the union of **top-N by CPU/mem** and **all name-matched**
  processes; matched first, then by CPU descending. N is chosen to fill the
  panel height.

### Unified record + TT enrichment
```text
ProcRow {
    pid: i32,
    name: String,
    cpu_pct: f32,
    mem_bytes: u64,
    inference: Option<&'static str>,   // matched runtime label, cross-platform
    tt: Option<TtProcInfo>,            // Some only on TT/Linux
}
TtProcInfo { device_indices: Vec<usize>, hugepages_1g, hugepages_2m, serving: Option<ServingMetrics> }
```
On Linux/TT, the existing `process_monitor` records and `serving` metrics are
merged into `ProcRow.tt` **by PID**. Off-TT, `tt` is `None` and those columns
are omitted from the panel.

## Data flow

```
TUI run loop (~2 s tick)
  ├─ HostProcessMonitor (sysinfo)  → ProcRow{pid,name,cpu,mem,inference}  [all OS]
  └─ [Linux+procfs] ProcessMonitor + serving probes → enrich ProcRow.tt by PID
           │
           ▼
  render_insights (cross-platform)
    header + render_device_panels(devices)  + render_process_panel(ProcRows)
```

## Panel layout

```
PROCESSES
  PID     CPU%   MEM     NAME            INFERENCE?     [TT DEV]   [SERVER]
  4821    312%   18 GB   python3         ~ mlx_lm                            (host)
  7734     96%    2 GB   ollama          ~ ollama
  1042     74%    9 GB   vllm            ~ vllm-tt        dev 0,1   :8000 ✓   (TT, enriched)
  512      44%  900 MB   WindowServer    -
```
`~` marks a name/cmdline match. The `TT DEV` / `SERVER` columns render only when
any row has `tt = Some` (i.e. on TT hardware).

## Cross-platform behavior

| | Device panels | Process panel | Inference tag | TT dev attribution / serving |
|---|---|---|---|---|
| TT (Linux) | ✅ (unchanged) | ✅ unified | ✅ | ✅ enriched by PID |
| Host (Linux) | ✅ rich panels | ✅ unified | ✅ | — |
| Host (macOS) | ✅ rich panels (CPU/GPU/ANE) | ✅ unified | ✅ | — |

## Error handling & degradation
- `sysinfo` process enumeration failing → empty process panel; device panels
  still render. No panics; everything `Option`.
- TT enrichment only attempted when the Linux `ProcessMonitor` is present.
- Linux/TT visual output stays equivalent (same data and columns as today),
  guarded by the existing isolation tests plus new ones.

## Testing
- Unit-test `inference_match`: matches `ollama`, `python … vllm …`, `mlx_lm`;
  ignores `WindowServer`, `node`, an empty cmdline.
- Unit-test the selection/sort: matched rows first, then top-N by CPU; a
  matched-but-idle process still appears.
- Cross-platform integration (extends `tests/backend_isolation.rs`): render the
  Insights screen with a Host backend into a `TestBackend` buffer; assert the
  device panels and process panel both paint, and a seeded inference-matched
  process name appears with its tag.
- Existing TT-path unit tests must still pass (the enrichment merge does not
  change TT columns or behavior).

## Out of scope
- Per-process GPU/ANE attribution (no no-sudo API on macOS) — the inference tag
  is name/resource-based, not "this PID used the ANE".
- Cross-platform process **kill** panel — `libc::kill` works on macOS, but the
  kill UI stays Linux-gated initially to bound scope.
- NVIDIA/AMD device discovery — a separate effort; this screen is simply ready
  to render them once they appear as `Device`s.

## Future direction: inference-perf telemetry (Phase 2, separate spec)

This screen detects *which* processes are doing inference. A natural follow-on
is reporting *how well* they perform — **tokens/sec, time-to-first-token, tokens
generated, model, context length** — so users can compare machine-to-machine
(a Mac vs a TT box vs a GPU box). It generalizes the existing TT-only
`workload::serving` (which already scrapes `/metrics` and tails logs) to a
cross-engine, cross-platform perf layer, surfaced in a **details pane** for the
selected inference process.

Sources per engine (Phase 2 research):
- **vLLM / tt-inference-server** — Prometheus `/metrics` (TTFT, time-per-output-token, throughput); already partly handled by `serving.rs`.
- **llama.cpp `llama-server`** — `/metrics` when started with `--metrics`, else per-request timing in its log/stderr.
- **Ollama** — per-request `eval_count`/`eval_duration` from its API, or its `server.log`; compute TPS/TTFT.
- **MLX (`mlx_lm.server`) / LM Studio** — tokens/sec in their logs (log-tail parsing).

Two harvest strategies, both read-only and bounded: **endpoint scrape** (precise,
needs a metrics-friendly server) and **log tailing** (passive, universal-ish,
matches engines without a metrics endpoint — the main path on macOS). The
`ProcRow` record from this spec is the seam: Phase 2 adds an optional
`perf: Option<InferencePerf>` field, populated per detected engine. Kept out of
this spec to bound scope; this screen is the substrate Phase 2 builds on.

## Effort & risk
~3–5 days. Most of the work is the unification refactor of the gated Insights
renderers and the enrich-by-PID merge layer; the `sysinfo` collector and the
`inference_match` classifier are small and easily tested. Risk is moderate
because it touches the proven TT Insights path — mitigated by keeping TT
detection code unchanged (only merged in) and by the existing + new tests.
