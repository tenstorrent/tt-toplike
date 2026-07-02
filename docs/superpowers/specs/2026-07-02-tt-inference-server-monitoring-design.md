# TT inference-server monitoring (design, v2 — resource-probe model)

**Status:** revised after real-world ops input (AGENTS.md Phase 22); pending re-approval.
**Date:** 2026-07-02
**Supersedes:** the v1 log-lifecycle approach below the line. Follow-on to the
liveness-probe work (`2026-06-30-inference-liveness-probes-design.md`).

## Problem (revised)

An operator waiting for a model to come up (e.g. Z-Image-Turbo first-run TTNN
kernel compilation + weight load on P150X4) gets **no signal for 90+ minutes**:
the server log goes silent during weight loading, and the only heartbeat is
`405 Method Not Allowed` liveness polls. There's no way to tell "progressing" from
"hung/OOM." tt-toplike should fill that gap with a live inference-server panel.

**This invalidates the v1 assumption that logs carry lifecycle.** Verified live on
the QuietBox (2026-07-02): the log is silent during load; the real progress signals
are resource/process-based.

## Goal

A toggle-able **`[i]` Inference Server panel** showing, per service: **phase**
(compiling kernels / loading weights / ready / failed-or-hung), **concrete progress
evidence** (not just elapsed time), a **hang/OOM alarm**, and a services list. The
discriminating requirement: distinguish "working" from "stuck" — CPU% cannot
(a tight loop and g++ look identical).

## Grounded facts (verified live, 2026-07-02)

- **Readiness is per-service** (from `tt-local-generator/app/server_manager.py`'s
  `SERVERS` dict): media services on **:8000 → `GET /tt-liveness`**; prompt-server on
  **:8001 → `/health`**; artgen LLMs on **:8002 → `/v1/models`**. Each media service
  has a `runner_key` (e.g. `tt-flux.1-schnell`).
- **`/tt-liveness` while loading → HTTP 405, body `{"detail":"Model is not ready"}`**;
  when ready → 200 with a `runner_in_use` field matching `runner_key`. So the
  readiness ladder is: connection-refused (down) → 405 "Model is not ready"
  (up, not ready) → 200 + runner_in_use (ready).
- **`docker stats --no-stream`** gives container CPU% and MEM (RSS). **`docker exec`**
  works (`ps`, `find`, `ls /proc/<pid>/fd`). No daemon socket needed.
- Phase signals observed: during load, top process is `python3` at ~100% CPU with
  RSS climbing and **no `g++`**; during compile, `g++` is alive.

## Signal model (the redesign)

**Phase** is derived from process + resource + liveness signals, not logs:

| Phase | Signal |
|---|---|
| `Down` | no container / connection refused on liveness port |
| `Compiling` | **kernel-artifact count under `built/` growing** tick-over-tick (`g++` alive = coarse corroborating hint) |
| `Loading` | kernel count flat, Python alive, **RSS growing** tick-over-tick |
| `Ready` | liveness endpoint returns 200 (media: `runner_in_use` matches `runner_key`) |
| `Alarm` | not Ready and **all progress probes flat** for ≥ 5 min (hang/OOM) |

**Cache-dir discovery (do NOT hardcode).** The runtime kernel cache is
`$TT_METAL_HOME/built` — verified live: kernels JIT-compile into
`$TT_METAL_HOME/built/<dev_ids>/tt-metal-cache<hash>/kernels/<name>/<hash>/{brisc,
ncrisc,trisc…}` as `.elf`/`.o`/`.dephash`/`.build_state`. The monitor reads
`TT_METAL_HOME` (and `CACHE_ROOT`) from the **container's environment** at runtime
(`docker inspect -f '{{range .Config.Env}}…' <c>`, or `docker exec <c> printenv
TT_METAL_HOME`). Never hardcode the path — it varies by image/user. (Do NOT count
`.o` under `build_Release/`: that's tt-metal's static C++ build, a constant ~4233,
not per-run progress.)

**Three independent progress probes** (a hang is all three flat; the key insight):
1. **Compiled-kernel artifact count** under `$TT_METAL_HOME/built` (compile progress)
   — e.g. `docker exec <c> sh -c 'find "$TT_METAL_HOME/built" -name "*.dephash" |
   wc -l'` (one per compiled kernel). Tracked as a **delta per tick** — a nonzero
   delta means kernels are still compiling. (`g++` presence is only a coarse hint:
   the compiler subprocesses are transient and often missed by a `ps` snapshot, as
   observed live while kernels were actively building.)
2. **RSS growing** (load progress) — from `docker stats` MemUsage delta per tick.
3. **open `.safetensors` / weight FDs** (load cross-check) —
   `docker exec <c> sh -c 'ls -l /proc/<pid>/fd 2>/dev/null | grep -c safetensors'`.

**Progress % (best-effort, optional):** compile = `.o count / baseline` (baseline
profiled once per model, hardcoded); load = `RSS / model_disk_footprint` (footprint
known per model). Absent a baseline, show the raw evidence (counts/rate) without a %.

**Metrics surfaced per service:** phase, CPU% + RSS (docker stats), RSS delta/tick
(GB/min), `.o` count + delta, `.safetensors` FD count, liveness status (000/405/200),
top process name, and the **last non-health-check log line** (`docker logs --tail`,
filtering out the `/tt-liveness … 405` spam) as a human breadcrumb.

## Services registry

Mirror the `SERVERS` dict: `key`, `label`, `health_url` (→ port + path), optional
`runner_key`. v1 hardcodes the current set (wan2.2, mochi, flux, sdxl,
z-image-turbo, motif, animate, skyreels @ :8000/`/tt-liveness`; prompt-server
@ :8001/`/health`; artgen-* @ :8002/`/v1/models`) in one Rust table, with a note that
it could later be read from a shared config file. The panel shows every known
service with its state; a service with no running container shows `Down` gracefully.

## Architecture

A background `InferenceServerMonitor` (Linux-gated), same shape as `LivenessProber`:
- **Detect** the running container(s) from the process snapshot (Task 1's cmdline
  parse — reused) and/or `docker ps`, mapping to a service `key`.
- On a slow cadence (~5s), for each known service: read `docker stats --no-stream`,
  run the `docker exec` probes (`.o` count, top process, `.safetensors` FDs), and
  probe the per-service `health_url`. Fold into a `ServiceState` (phase + metrics +
  RSS/`.o` history for delta and the flat-line alarm timer).
- Publish `Vec<ServiceState>` via `arc-swap`; the render path reads it lock-free.
- **All docker/HTTP I/O is on the background thread**; nothing on the render path.
- New Insights sub-view toggled with **`i`**, rendered from the snapshot.

**Pure, unit-tested cores** (the docker calls are thin shells around these):
- `parse_docker_stats(line) -> (cpu_pct, rss_bytes)`
- `parse_liveness(status, body) -> Readiness` (Down/NotReady/Ready{runner})
- `parse_env_var(env_output, key) -> Option<String>` (discover `TT_METAL_HOME`)
- `count_lines(find_output) -> usize` (kernel-artifact count; `.safetensors` FD count)
- `top_process(ps_output) -> Option<(name, cpu, rss)>`
- `Phase::derive(kernel_delta, python_alive, rss_delta, readiness)` + the flat-line
  `Alarm` timer reducer
- `estimate_progress(phase, kernel_count, rss, model_profile) -> Option<f32>`

## What survives from v1 (already committed on the branch)

- **Task 1 — cmdline → container/model/mesh/port identity:** reused for container
  detection and service mapping. ✅
- **Monitor pattern** (background thread + `arc-swap` snapshot), `Source` enum: reused.
- **Task 2 — `/v1/models` probe:** superseded. Readiness is per-service; media uses
  `/tt-liveness`. Rework into `parse_liveness` + the per-service `health_url` table.
  (`/v1/models` remains correct for the artgen :8002 services.)
- **Task 3 — log-line classifier:** demoted. The lifecycle is resource-driven; the
  only log use now is "last non-health-check line" as a breadcrumb. `classify_log_line`
  and the log-marker set are dropped from the critical path (kept only if trivially
  useful for the breadcrumb filter).

## Extension trails

- **Config-driven services:** the registry is one table now; later read the shared
  `SERVERS` config so the two tools can't drift.
- **Per-model profiles:** `estimate_progress` takes a `ModelProfile { o_baseline,
  disk_footprint }`; v1 hardcodes the few known models, more added as data-only rows.
- **Richer telemetry:** `ServiceState.metrics` stays a growing struct (tokens/sec etc.).
- **Non-Docker/host installs:** container access is behind a small `ContainerProbe`
  trait; a host/systemd impl slots in later.

## Error handling / degradation

- No docker / `docker exec` denied / container gone → that service shows `Down` or
  the last-known phase with a stale marker; never panic, never block the UI.
- Missing `.o`/FD data → that probe reports `None`; phase falls back to the signals
  that are available (RSS + liveness).
- Everything off the render path; failures degrade a service to a lesser signal.

## Scope

Linux/TT only (`cfg(target_os = "linux")`); no-op elsewhere. No new dependencies
(shell out to `docker` + the existing `std::net` HTTP helper). Off the hot path.
No panics on external output.

## Testing

Host-runnable pure functions over captured fixtures: `docker stats` line,
`/tt-liveness` 405/200 bodies, `find … *.o` output, `ls /proc/<pid>/fd` output,
`ps` output, `Phase::derive` truth table (incl. the all-flat → Alarm case with a
timer), and `estimate_progress`. The docker/HTTP shells stay thin and are exercised
via injected fakes (no real docker in unit tests), mirroring the LogSource fake used
before.

## Out of scope (v1)

- Non-Docker/host-native installs (behind the `ContainerProbe` trait seam).
- Deep serving metrics (tokens/sec, latency) — `metrics` struct reserved.
- Profiling the `.o`/footprint baselines automatically (v1 hardcodes known models;
  unknown models show raw evidence, no %).

---

# v1 (superseded) — log-lifecycle approach

The original design assumed the server log carried lifecycle (parse "Loading
model", progress %) with a Docker `LogSource`, a `classify_log_line` classifier, and
a `/v1/models` readiness probe. Real-world ops (Phase 22) showed the log is silent
during the painful load phase, so the signal model moved to resource/process probes
above. Retained here for provenance; the `LogSource` trait, `LifecycleEvent`
classifier, and `LifecycleState` reducer from Tasks 3–4 are not the v2 critical path.
