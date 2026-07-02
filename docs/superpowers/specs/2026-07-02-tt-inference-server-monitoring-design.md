# TT inference-server monitoring (design)

**Status:** approved design, pending spec review. Follow-on to the liveness-probe
work (see `2026-06-30-inference-liveness-probes-design.md`).
**Date:** 2026-07-02

## Problem

While a `tt-inference-server` is loading a model on the box, the Insights panel
lists unrelated top-CPU apps and never surfaces the server. Root causes observed
live (QuietBox, 2026-07-02):

- The server runs as a **Docker container** (`ghcr.io/tenstorrent/tt-media-inference-server`);
  the real workers are `uvicorn … main:app` **inside** the container, which match
  none of `inference_match`'s needles.
- The only matching host process is the **idle `docker run` client** (0% CPU),
  which the liveness gate demotes.
- The HTTP probe is wrong for this variant: `GET /health` → **405**, while
  `GET /v1/models` → **200**.

Process-name and single-endpoint HTTP heuristics are too fragile across variants
(LLM vs media, container vs host, differing endpoints, loading vs ready). We need
a signal that is (a) reliable for identity and (b) able to report lifecycle.

## Goal

Both, **state-first**: reliably detect a TT inference server and show its
lifecycle — `Starting → Loading(+progress) → Ready → Serving → Error` — so it is
surfaced correctly instead of unrelated processes. Richer serving telemetry
(tokens/sec, per-step diffusion progress) is layered on opportunistically as logs
expose it, not required for v1.

## Key insight

The most reliable signal is the **`docker run` cmdline we already read** from the
process table. It carries full identity with zero parsing fragility:

```
--name tt-inference-server-5edd00ce
ghcr.io/tenstorrent/tt-media-inference-server:0.17.0-8c48a10
-e MODEL=FLUX.1-schnell  -e MESH_DEVICE=P300x2  -e DEVICE=p300x2
-e ARCH_NAME=blackhole   --publish 0.0.0.0:8000:8000
--device /dev/tenstorrent
```

→ identity, model, mesh, arch, port, and TT-device association. **Logs** add the
*lifecycle* (load progress / ready / errors) the cmdline can't; an **HTTP probe**
corroborates readiness. Each layer degrades independently (approach #1, chosen
over log-only and probe-first).

## Scope

**Linux/TT only.** Docker and `/dev/tenstorrent` are Linux concepts; the whole
feature is `cfg(target_os = "linux")`-gated and compiles to a no-op elsewhere,
matching how `process_monitor`/`serving` are already gated.

## Design — two tracks

### Track A — detection from the container cmdline (foundation)

A pure parser over a process's name + cmdline that recognizes a TT inference
container and extracts a structured record:

```
InferenceServer {
    container_name: String,       // tt-inference-server-5edd00ce
    image: String,                // …/tt-media-inference-server:0.17.0-…
    model: Option<String>,        // FLUX.1-schnell   (-e MODEL=)
    mesh: Option<String>,         // P300x2           (-e MESH_DEVICE=)
    arch: Option<String>,         // blackhole        (-e ARCH_NAME=)
    device: Option<String>,       // p300x2           (-e DEVICE=)
    port: Option<u16>,            // 8000             (--publish host:ctr)
    uses_tt_device: bool,         // --device /dev/tenstorrent present
}
```

Recognition: image contains `inference-server` or matches `ghcr.io/tenstorrent/tt-*`,
**or** `--name` contains `tt-inference-server`. Pure and unit-tested against the
captured cmdline fixture. This alone fixes today's "junk in the panel" problem —
we can label and surface the server without any logs.

**Probe fix (relates to the liveness-probe module):** for `tt-inference-server`,
readiness = a **200 from `/v1/models`** (media variant) **or `/health`** (LLM
variant, which returns 200/503). The probe tries `/v1/models` first, then
`/health`; ready if either returns 200. Uses Track A's discovered/known port.

### Track B — log lifecycle engine

- **Log source abstraction** (`trait LogSource`): first impl shells out to
  `docker logs --since <last_poll> <name>` **by polling** on the background
  cadence (not a long-lived `-f` follow — polling is bounded, needs no child-
  process lifecycle management, and matches how we already spawn `tt-smi`). Each
  poll reads only lines since the previous poll. Unavailable/errored source →
  `None` (degrade to "present/up").
- **Version-tolerant parser** (`fn classify_log_line(&str) -> Option<LifecycleEvent>`):
  a small, centralized set of markers → `Loading{progress}`, `Ready`, `Serving`,
  `Error`. Unknown lines are ignored (state holds). Pure, fixture-tested.
- **State reducer**: folds events into
  `LifecycleState { phase, progress: Option<f32>, detail: String, observed_at }`.
  Precedence when combining signals: **log Error > log Ready/Serving > probe 200
  (Ready) > container present (Starting/Loading)**. A stale log state (older than
  ~2× cadence) is not trusted as authoritative.

### Execution

A `InferenceServerMonitor` owns a background thread and a lock-free cache
(`arc-swap`), mirroring `LivenessProber`:

1. Each ~2s process refresh, the TUI hands it the detected `InferenceServer`
   records (cheap; from Track A over the snapshot).
2. The background thread, on a slow cadence (~5s, matching `LivenessProber`'s
   `PROBE_INTERVAL`), pulls recent `docker logs` per detected server and folds
   them into `LifecycleState`; on the same tick it triggers the HTTP probe.
3. The render path reads the cached `Vec<(InferenceServer, LifecycleState)>`
   lock-free. **No subprocess or network I/O on the render path.**

### Surfacing

A dedicated Insights **"Inference Servers"** card, one row per detected server:
`model · arch/mesh · phase (+progress bar while Loading) · port/health`. The
existing process list stays but is no longer the primary inference signal. TT
device association (via `--device`) lets the card sit next to the chip it runs on.

## Error handling / degradation

- No docker / `docker logs` fails → keep Track A identity, phase = best-effort
  from probe/container presence; never panic, never block the UI.
- Log format drift → unknown lines ignored; card still shows identity + probe
  readiness.
- Multiple servers → one record/card each (dedup by container name).
- Everything off the hot path; failures yield `None` and fall back a layer.

## Testing

- **Pure, host-runnable** (Linux): cmdline parser over the captured `docker run`
  fixture (asserts model/mesh/arch/port/device); `classify_log_line` over captured
  log fixtures (loading/ready/error/unknown); state reducer precedence
  (log Error beats probe Ready; stale state ignored); probe endpoint choice
  (`/v1/models` for the media variant).
- Background thread kept a thin shell; logic lives in the pure functions.

## Out of scope for v1 — and the trail to each

v1 ships the Docker path, state-first. Each deferred piece has a designed-in seam
so it's an additive change, not a rewrite. The plan must build these seams even
though v1 leaves one implementation behind each.

- **Host-native / non-Docker installs** → the `LogSource` **trait** is the seam.
  v1 ships `DockerLogSource`; a later `FileLogSource` / `JournaldLogSource` slots
  in behind the same trait, and detection grows a non-container branch that emits
  the same `InferenceServer` record (leave the record's source field an enum:
  `Source::Docker { container } | Source::Host { … }`).
- **More runtime variants** → detection recognizes servers through a small
  **registry** (image/name patterns → variant), parallel to `liveness_probe`'s
  `probe_spec`. Adding vLLM-native, Triton, etc. is a registry entry + optional
  parser markers, no structural change.
- **Deep serving telemetry** (tokens/sec, latency, per-step diffusion progress)
  → the **`LifecycleEvent` enum + state reducer** are the seam. v1 defines
  `Loading/Ready/Serving/Error`; richer signals are new `LifecycleEvent` variants
  and fields on `LifecycleState`, consumed by the same card. Wire a
  `metrics: Option<ServingMetrics>` field now (always `None` in v1) so the type
  and card slot already exist.
- **Feeding telemetry into the visualizations** (e.g. Defrag inference energy from
  real per-step progress) → downstream consumers read `LifecycleState`/`metrics`;
  no producer change needed later.
- **Non-Linux platforms** → stays `cfg`-gated; the trait/registry seams are
  platform-neutral, so a future non-Docker source could even be cross-platform.

Each trail is called out again as an explicit "extension point" note in the
implementation plan so the v1 code leaves the door open by construction.
