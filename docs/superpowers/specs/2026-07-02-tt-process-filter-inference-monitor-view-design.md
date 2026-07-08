# TT-only process filtering + dedicated Inference Server Monitor view (design)

**Status:** approved design, pending spec review.
**Date:** 2026-07-02
**Sequence:** Spec #1 of 3 in the "inference observability" arc. Follow-ons (their
own spec→plan cycles), both rendering *inside* the view this spec creates:
- Spec #2 — logstalgia-style visualization of inference endpoint hits.
- Spec #3 — snake-game model-load-progress visualization.

Builds on the inference-server monitoring feature (`inference_server` module +
the `[i]` panel added in `2026-07-02-tt-inference-server-monitoring-design.md`).

## Problem

Two gaps in the current Insights experience on TT hardware:

1. **Noisy process list.** On real TT backends the process panel shows the busiest
   *host* processes and merely annotates which hold a TT device fd
   (`enrich_proc_rows_tt`). Processes that aren't contributing to TT load
   (browsers, editors, unrelated daemons) crowd out what the operator cares about.
2. **The inference monitor is a cramped in-Insights panel toggle**, not a place you
   navigate into. There's no way to select an inference-server process and jump to
   its monitor, and there's no room for the richer visualizations coming next.

## Goal

- On non-`--host` real-TT backends, the process panel lists **only processes
  attributed to a TT device**.
- Promote the inference monitor to a **dedicated full-screen view** entered with
  `i`, with a navigable service list and a pre-focus hook for the selected process
  — the surface the logstalgia/snake visualizations will render into.

## Part A — TT-only process filtering

### Rule
The process panel filters to TT-attributed processes when the active backend
represents **real TT hardware** — defined by intent as "everything except Host and
Mock", so it's robust to the exact `BackendType` variants:

```
fn backend_shows_only_tt(bt: BackendType) -> bool = !matches!(bt, Host | Mock)
    // Host -> false: show host CPU processes (unchanged)
    // Mock -> false: exempt (no real /dev/tenstorrent fds; keep --mock demos non-blank)
    // everything else (sysfs/hybrid/json/luwen/…) -> true: real TT → filter
```

The plan must confirm the actual `BackendType` variants (e.g. resolve `Auto` to
its effective backend before applying the predicate, matching how the TUI already
resolves the active backend).

"Attributed to a TT device" = the process holds a `/dev/tenstorrent` fd, which
`ProcessMonitor` already discovers (the same data `enrich_proc_rows_tt` merges as
`ProcRow.tt = Some(TtProcInfo)`).

### Selection, not post-filtering (important)
Today the flow is: `HostProcessMonitor::rows(max, …)` picks the top-N by CPU →
`enrich_proc_rows_tt` annotates TT attribution on those N. Filtering *after* that
would drop low-CPU-but-TT-attached processes before we ever learn they're TT
processes. Instead, when `backend_shows_only_tt` is true, **selection is driven by
the TT-attributed PID set**:

- `ProcessMonitor` exposes the set of PIDs holding TT device fds (it already
  computes this for enrichment).
- A new `HostProcessMonitor::tt_rows(max, tt_pids: &HashSet<i32>) -> Vec<ProcRow>`
  builds rows for exactly those PIDs (name/cpu/mem via sysinfo), sorted by CPU
  desc, capped at `max`. Each row still gets its `inference`/`active` tags via the
  existing per-row logic, and is enriched with `TtProcInfo` afterward as today.
- The TUI calls `tt_rows(...)` on real-TT backends and the existing
  `rows(...)` on host/mock.

### Empty state
When the TT-attributed set is empty (idle box, nothing using the devices), the
panel renders a single muted line: **"No processes are currently using TT
devices."** Selection/kill logic sees an empty `proc_rows` and no-ops safely
(cursor clamps to 0, kill guarded by `proc_rows.get(cursor)` — already the case).

## Part B — Dedicated Inference Server Monitor view

### View model
A distinct full-screen screen state (not part of the `v` mode rotation):

- `i` / `I` toggles into the monitor from any screen; `Esc` or `i` returns to the
  prior screen. (Replaces the current `show_inference` in-Insights panel toggle.)
- `↑`/`↓` move a `service_cursor` over the service list; the focused service is
  highlighted. (This is where specs #2/#3 render logstalgia/snake for the focused
  service.)
- The list is the Task-G content promoted to full screen: every known `SERVERS`
  entry, merged with the live `snapshot()`, one row via `format_service_row`
  (phase · model · RSS · progress · liveness), colored by phase.

### Context-aware entry (pre-focus)
When `i` is pressed while a process is selected in the (TT-filtered) list, the
monitor pre-focuses the matching service, best-effort:

```
fn service_for_selected_process(row: &ProcRow) -> Option<&'static str>  // service key
```
Resolution order: (1) if the row's cmdline/name maps to a model via
`inference_server::parse_inference_server` + `service_for`, use that key;
(2) else `None` → the monitor opens with the cursor at the first service.
Precise PID→container linking is explicitly out of scope (best-effort only);
opening unfocused is an acceptable fallback.

### Rendering
`render_inference_monitor(f, area, &snapshot, service_cursor)` draws the titled
full-screen list. On non-Linux the monitor still renders (from an empty snapshot →
all services `Down`), since the `inference_server` types are now cross-platform.

## Components / isolation

- `src/ui/tui/inference_panel.rs` → becomes the monitor-view module: keeps
  `format_service_row`/`service_rows`; adds `render_inference_monitor` +
  `service_for_selected_process`.
- `src/workload/host_processes.rs`: add `tt_rows(max, &HashSet<i32>)`; expose the
  TT-fd PID set from `ProcessMonitor` (a getter if not already accessible).
- `src/cli.rs` (or the TUI): `backend_shows_only_tt(BackendType) -> bool`.
- `src/ui/tui/mod.rs`: swap `show_inference` panel toggle for the full-screen view
  state + `service_cursor`; call `tt_rows` vs `rows` by backend; `i`/`Esc`/`↑`/`↓`
  handling; empty-state line.

## Error handling / degradation
- No TT hardware / empty attribution → empty-state line; no panic.
- Backend switched live (`b` key): the filter follows `backend_type`, so switching
  to `--host` restores host processes and away from it re-filters.
- Pre-focus resolution failing → open unfocused. Never blocks entry.
- All existing off-hot-path guarantees for the monitor snapshot are unchanged.

## Testing
Pure/unit-testable, host-runnable:
- `backend_shows_only_tt` truth table (Host/Mock false; Sysfs/Hybrid/Luwen/Json true).
- `tt_rows`: given a synthetic process set + a TT-fd PID set, returns only those
  PIDs' rows, sorted by CPU desc, capped at `max`; empty set → empty vec.
- `service_for_selected_process`: a ProcRow whose cmdline carries a known model →
  the right service key; an unrelated row → `None`.
- Render: `render_inference_monitor` over a synthetic snapshot via the existing
  `TestBackend`/buffer harness (title present, one line per service, empty-state
  line when no rows).

## Out of scope (this spec)
- The logstalgia endpoint-hit visualization (spec #2) and the snake load-progress
  visualization (spec #3) — this spec only builds the view they render into.
- Precise selected-PID → container/service linking (best-effort pre-focus only).
- Changing `--host` process behavior (unchanged).
