# Inference liveness — authoritative probes (design)

**Status:** planned follow-on to PR #19 (do NOT add to the v0.7.0 release PR).
**Date:** 2026-06-30
**Builds on:** `ProcRow.active` (the cheap, pure liveness tier shipped in
`feat(workload): don't surface idle inference daemons as active`).

## Problem

The snapshot-only liveness tier (`runtime_active` in `src/workload/host_processes.rs`)
answers "is this runtime working?" from CPU + a known subprocess shape. That is
authoritative for **ollama** (a loaded model spawns an `ollama runner` worker)
but weak for **server** runtimes:

- A **vllm** / **tt-inference-server** process can hold a model loaded and sit at
  ~0% CPU *between requests*. The CPU heuristic reads that as idle, but it is
  really "loaded and ready" — a different state from idle ollama (no model at all).

When we *know* how to ask a runtime directly, we should: "model loaded / server
ready" is the notion of "connected to or running a model" the user actually cares
about.

## Decisions (locked with user 2026-06-30)

1. **Follow-on PR**, not part of #19.
2. **Dep-free transport**: hand-rolled minimal HTTP GET. *Implementation note:* the
   TUI run loop is synchronous — there is no tokio runtime to spawn onto — so the
   probe runs on a plain `std::thread` using blocking `std::net::TcpStream` with
   connect/read timeouts, rather than `tokio::net`. This keeps it truly
   dependency-free (no runtime to stand up) and simpler. No new crates.
3. **Confirm-only, default-on**: only probe a runtime we have *already detected as
   a running process*. Never scan ports blindly. Slow background cadence, short
   timeout, cached. Never on the render path.

## Design

### Probe model — `RuntimeProbe`

A small registry mapping a detected runtime label → how to confirm it:

```text
label            host:port (default)   endpoint        "active" when
---------------  --------------------  --------------  -----------------------------
ollama           127.0.0.1:11434       GET /api/ps     JSON .models[] non-empty (200)
vllm             127.0.0.1:8000        GET /v1/models  JSON .data[] non-empty (200)
tt-inference-... 127.0.0.1:<discovered> GET /health    200 (503 = "Model not ready")
```

`tt-inference-server` exposes `/health` (200 when the model is ready, 503 otherwise,
no auth) — confirmed in `tt-media-server/open_ai_api/tt_maintenance_api.py`. We use
that rather than `/tt-liveness`, which reports "alive" even before a model loads.

- Port discovery (most-trusted first): the port the PID is **actually listening
  on**, read from the OS socket table (`/proc/net/tcp{,6}` joined to
  `/proc/<pid>/fd` socket inodes — the same join `netstat`/`ss -ltnp` do, but no
  subprocess); then a `--port`/`host:port` from the cmdline; then the runtime's
  conventional default. Socket discovery is Linux-only (other platforms fall back
  to cmdline/default) and runs on the background thread, not the render path.
  Confirm-only means we only ever hit a port belonging to a process we already saw.
- Each probe is **pure where possible**: a free function `parse_<runtime>_active(&[u8]) -> bool`
  over the raw response body, unit-tested with captured fixtures. Only the socket
  read/write is impure.

### Execution — off the hot path

- A `LivenessProber` owns a background `tokio` task (the app already runs a tokio
  runtime). The render loop and `HostProcessMonitor::rows()` **never block on it**.
- Cadence: probe at most once per `PROBE_INTERVAL` (start ~5s), independent of the
  ~10 FPS render and ~2s process refresh. Per-probe timeout ~250ms; a timed-out or
  errored probe is treated as "unknown" and falls back to the snapshot tier.
- Result is a cached `HashMap<label_or_pid, ProbeVerdict { active, observed_at }>`
  read via `arc-swap` (already a dep) so the read path is lock-free.

### Wiring into liveness

`runtime_active` gains a third input: an optional probe verdict.

```
fn runtime_active(label, cpu_pct, ctx, probe: Option<bool>) -> bool {
    if let Some(ready) = probe { return ready; }   // authoritative when fresh
    // else fall back to the existing cheap tier (CPU + ollama runner subprocess)
}
```

Precedence: **fresh probe verdict > subprocess signal > CPU threshold.** A stale
verdict (older than ~2× cadence) is ignored so a crashed server doesn't read as
ready forever.

## Perf guarantees

- No new dependencies.
- Zero network I/O on the render path or in `rows()`; all probing is on the
  background task.
- Bounded work: at most one probe per detected server-runtime per cadence tick,
  each with a hard timeout. Confirm-only means no port scanning.

## Test plan

- Pure: `parse_ollama_active`, `parse_vllm_active`, `parse_tt_inference_active`
  over captured-byte fixtures (loaded vs empty vs malformed → unknown).
- Port parse from cmdline (`--port`, `--host`, default fallback).
- `runtime_active` precedence: fresh probe wins; stale probe ignored; no probe →
  cheap tier unchanged.
- Prober cadence/timeout logic exercised with an injected fake transport (no real
  socket in unit tests).

## Out of scope

- Per-request throughput / tokens-per-second (that's `serving.rs` territory).
- Remote/non-localhost servers.
