# Direct (non-Docker) vLLM-on-TT detection (design)

**Status:** proposed, approved in chat 2026-08-27; pending written-spec review.
**Date:** 2026-08-27
**Extends:** `2026-07-02-tt-inference-server-monitoring-design.md` (the `[i]`
Inference Server panel, `InferenceServer`/`ServiceState`/`InferenceServerMonitor`
pipeline). This document only covers what's new; see that spec for the
panel's phase model, alarm logic, and rendering.

## Problem

The `[i]` panel currently only detects TT inference servers launched via
`docker run` (`parse_inference_server`/`parse_inspect` in `detect.rs`, keyed
by container name, probed with `docker exec`/`docker stats`). A direct vLLM
launch — `tt-model serve` (which itself execs `vllm serve <model> ...` or
`python3 server_example_tt.py --model <model> ...`, per `tt-tnt`'s
`docs/serving-with-tt-kernel.md`) — is invisible to it. This deployment shape
is expected to become more common than the Docker-wrapped one. The generic
`inference_match()` classifier already tags such a process as `"vllm"` for
the plain process table + liveness ping, but it never reaches the rich
phase/progress/metrics panel.

The code already anticipated this gap: `Source` carries a commented-out
`Host { unit_or_pid: String }` trail variant, `ContainerProbe`'s doc comment
says "a host/systemd impl for non-Docker installs would implement this same
trait," and `services.rs`'s `prompt-server` entry notes it's a bare `python3`
process the current detector can't see.

## Grounded facts (from `~/code/tt-tnt`, 2026-08-27)

Two real launch shapes, both from `tt-model serve` (a separate CLI,
`tt-kernel-package-manager`, that reads a bundle's `vllm_metadata.json` and
execs the shape below directly — not a sidecar):

```
# Canonical CLI form (AUTOFIX.md:207-212)
TT_METAL_HOME=... PYTHONPATH=... LD_LIBRARY_PATH=.../build/lib \
EXTRA_MODELS_DIR=... VLLM_USE_V1=1 MESH_DEVICE=P300x2 HF_MODEL=episod/tt-tnt-1024 \
vllm serve episod/tt-tnt-1024 --max_model_len 512 --max_num_seqs 32 --port 8000 \
  --additional-config '{"tt":{"fabric_config":"FABRIC_2D_TORUS_XY"}}'

# Example-script form (docs/serving-with-tt-kernel.md:243-246)
TT_METAL_HOME=... PYTHONPATH=... LD_LIBRARY_PATH=.../build/lib \
EXTRA_MODELS_DIR=... VLLM_USE_V1=1 MESH_DEVICE=P150 HF_MODEL=episod/tt-tnt \
.../bin/python3 server_example_tt.py --model episod/tt-tnt --max_model_len 2048 --max_num_seqs 8
```

- Env vars observed: `MESH_DEVICE`, `TT_METAL_HOME`, `HF_MODEL`, `PYTHONPATH`,
  `LD_LIBRARY_PATH`, `EXTRA_MODELS_DIR`, `VLLM_USE_V1`. No `ARCH_NAME`/`DEVICE`
  equivalent found.
- Port: both shapes default to **8000**; `vllm serve` accepts `--port`.
- No distinctive `argv[0]`/`setproctitle` rename — recognize by argv shape:
  `vllm serve <model> ...` (tokens `vllm`, `serve`), or a cmdline containing
  `server_example_tt.py` with a `--model` arg.
- `/health` (plain vLLM, no custom liveness endpoint) is the right
  `health_path` — and is already the fallback the monitor uses today for any
  detected-but-uncatalogued service (`services::service_for` miss branch).

## Design

### 1. `Source::Host` and identity

```rust
pub enum Source {
    Docker { container: String },
    Host { pid: i32 },
}
```

A stable identity/key string is needed wherever the code currently
destructures `Source::Docker { container }` and uses `container` as a key
(prev-state lookup, dedup, signature). New helper in `detect.rs`:

```rust
/// Stable identity key for a detected server, used for prev-state lookup,
/// dedup, and the monitor's change-signature. Docker keys by container name
/// (survives the monitor's own restarts, stable across ticks); a bare host
/// process has no such name, so it keys by pid — a restart gets a fresh key
/// and starts `fresh_state`, which is correct: the old process's kernel/RSS
/// history is not the new process's history.
pub fn service_key(source: &Source) -> String {
    match source {
        Source::Docker { container } => container.clone(),
        Source::Host { pid } => format!("host-vllm-{pid}"),
    }
}
```

This replaces the `let Source::Docker { container } = &server.source;`
irrefutable destructure in `monitor.rs::rebuild_snapshot`,
`monitor.rs::merge_detections`/`detected_sig`, and
`host_processes.rs::detected_inference_servers` — each becomes either a call
to `service_key()` or an explicit `match` where behavior genuinely differs
(dedup in `merge_detections` is by this same key).

### 2. Detection: `parse_direct_vllm`

New pure function in `detect.rs`, next to `parse_inference_server` (reuses
its `value_after`/`env_value`/`model_arg` helpers):

```rust
/// Recognize a direct (non-Docker) vLLM-on-TT launch from `name` + `cmdline`
/// + the process's own `environ` (`KEY=VALUE\n`-per-line, same shape
/// `parse_env_var` already expects). Two shapes: `vllm serve <model> ...`,
/// or a cmdline containing `server_example_tt.py` with a `--model` arg.
/// Requires MESH_DEVICE or TT_METAL_HOME present in `environ` to confirm
/// this is genuinely TT-backed (mirrors `parse_inference_server`'s
/// `uses_tt_device` check, which has no `/dev/tenstorrent` cmdline
/// equivalent to key off for a bare host process).
pub fn parse_direct_vllm(name: &str, cmdline: &str, environ: &str) -> Option<InferenceServer>
```

Behavior:
- `toks = cmdline.split_whitespace()`.
- Shape A: `toks` contains `vllm` immediately followed by `serve`; model is
  the next positional token (first token after `serve` not starting with
  `-`).
- Shape B: `cmdline` contains `server_example_tt.py`; model from
  `model_arg(&toks)`.
- Neither shape → `None`.
- Gate: `env_value(environ, "MESH_DEVICE").is_some() || env_value(environ, "TT_METAL_HOME").is_some()`,
  else `None`.
- `port`: `--port <N>` from `toks`, else `8000`.
- `mesh`: `env_value(environ, "MESH_DEVICE")`.
- `arch`, `device`: `None` (no source observed).
- `uses_tt_device: true` (implied by the gate).
- `source: Source::Host { pid }` — `pid` passed in by the caller (the
  process-table scanner), not derived here; this function stays pure/testable
  against a literal cmdline+environ+name with no process-table dependency.

Signature: unlike `parse_inference_server(name, cmdline)`, `parse_direct_vllm`
takes `pid: i32` as a fourth parameter (alongside `name`, `cmdline`,
`environ`) and bakes it directly into the returned `Source::Host { pid }` —
one extra `i32` costs nothing for testability (tests just pass a fixed pid),
and it avoids a placeholder-then-overwrite dance.

### 3. Wiring: `host_processes.rs`

`detected_inference_servers()` (already `#[cfg(target_os = "linux")]`, already
iterates `self.sys.processes()` with name+cmdline) gains a second check per
process using `p.environ()` (populated for free — the existing
`ProcessRefreshKind::everything()` call already sets `environ:
UpdateKind::OnlyIfNotSet`, no new refresh cost):

```rust
let environ = p.environ().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join("\n");
if let Some(server) = parse_direct_vllm(&name, &cmdline, &environ, pid_i32) {
    if seen.insert(service_key(&server.source)) {
        out.push(server);
    }
}
```

(`seen` becomes keyed by `service_key(&server.source)` instead of the
Docker-only `container.clone()`, so Docker and Host entries dedup against the
same set — irrelevant in practice since they're never the same key, but keeps
one dedup set instead of two.)

Both existing `.submit(host_proc_monitor.detected_inference_servers())` call
sites in `ui/tui/mod.rs` need no change.

### 4. Process-tree scoping

Docker gets "everything belonging to this service" for free (container PID
namespace). A bare host process doesn't, and vLLM-on-TT genuinely forks: a
`g++` compile child during kernel compilation, and worker subprocess(es) for
device execution. Per your call: **walk the tree from the launched pid and
scope every signal to it + all descendants.**

New helper (`probe.rs`, Linux-only):

```rust
/// All pids in the process tree rooted at `root` (inclusive), via
/// `/proc/<pid>/task/<pid>/children` (direct children; recurse). A pid that
/// exits mid-walk (file gone) is simply not descended further — best-effort,
/// matches every other probe method's "degrade to less data, never panic"
/// contract.
fn process_tree_pids(root: i32) -> Vec<i32>
```

### 5. `SystemProbe`: the second `ContainerProbe`

`ContainerProbe`'s trait signature is unchanged. New `SystemProbe` wraps a
`DockerProbe` and dispatches per-call on the key string
(`"host-vllm-"`-prefixed → host logic; else → delegate to the inner
`DockerProbe`):

- `env(key)`: parse pid from key, read `/proc/<pid>/environ` directly (NUL-separated
  → reformat to `K=V\n` lines matching `parse_env_var`'s expected shape) — no
  shell-out, unlike Docker's `docker exec env`.
- `stats(key)`: walk the tree, run `ps -o rss= -p <comma-joined pids>` (or
  equivalent), sum RSS; sum `%cpu` the same way; format as
  `"{cpu}%|{rss}KiB / 0KiB"` so the existing `parse_docker_stats` parses it
  unchanged (the denominator is discarded by that parser, so a dummy is fine —
  documented inline so it isn't mistaken for a real limit).
- `exec(key, sh)`: for the two `find` commands (`KERNEL_FIND_CMD`,
  `LOADED_FIND_CMD`), run `sh -c <sh>` **with the target's own env vars
  (`TT_METAL_HOME`/`CACHE_ROOT`, read via the same `/proc/<pid>/environ`)
  injected explicitly** — a docker-exec'd shell inherits the container's env
  for free; a locally-spawned one does not. For the `ps`-based top-process
  probe, `ContainerProbe` gains a new method with a default so `DockerProbe`
  is unaffected:

  ```rust
  /// The `ps`-style command whose first data row is the service's top-CPU
  /// process. Default: the docker-exec'd global `ps` (correct there because
  /// `docker exec` already scopes to the container's own PID namespace).
  /// `SystemProbe` overrides this for a host key to scope by the walked pid
  /// tree instead (`ps -o pcpu,rss,comm --no-headers -p <pids>`, pre-sorted
  /// by cpu descending to match `--sort=-pcpu`'s row order that
  /// `top_process`/`contains_python` assume).
  fn top_proc_cmd(&self, key: &str) -> String { TOP_PROC_CMD.to_string() }
  ```

  `monitor.rs::build_sample` calls `probe.top_proc_cmd(container)` instead of
  the hardcoded `TOP_PROC_CMD` constant when building the `ps` exec.
- `http(port, path)`: unchanged (already container-agnostic — plain localhost
  HTTP).
- `list_servers()`: unchanged/not overridden — detached host processes are
  already found by the periodic `detected_inference_servers()` re-scan, same
  as a foreground `docker run`'s equivalent path; there's no "detached vLLM
  process" concept analogous to a detached container needing separate
  enumeration.

`InferenceServerMonitor::spawn()` constructs `SystemProbe::new(DockerProbe)`
instead of a bare `DockerProbe`. Tests are unaffected — they construct their
own fakes against the unchanged `ContainerProbe` trait.

### 6. Everything downstream is untouched

`fold_tick`, `Phase::derive`, `is_alarm`, `estimate_progress`,
`parse_vllm_metrics`/`ServingStats`, `service_for` (a custom HF model id like
`episod/tt-tnt-1024` won't match the curated `SERVERS` table, so it takes the
existing "track anyway, label from model basename" branch — the same one
already used for uncatalogued Docker vLLM deployments), and all `[i]` panel
rendering operate purely on `ServiceState`/`TickSample` with no knowledge of
`Source`. No changes needed there.

## Testing

- `detect.rs`: unit tests for `parse_direct_vllm` — both real shapes (from
  the grounded-facts examples above, trimmed like `DOCKER_RUN`/`DOCKER_RUN_VLLM`
  already are), the TT-confirmation gate rejecting a `vllm serve` with neither
  `MESH_DEVICE` nor `TT_METAL_HOME` set, port default vs. explicit `--port`,
  and `service_key` for both `Source` variants.
- `probe.rs`: unit test `process_tree_pids`-adjacent parsing (the
  `/proc/.../children` line-splitting) against literal strings; the real
  recursive fs walk isn't unit-tested (matches this module's existing
  pattern — `docker()`'s subprocess plumbing isn't directly unit-tested
  either, only its pure parsers are).
- `monitor.rs`: `rebuild_snapshot`/`merge_detections` tests get a
  `Source::Host` case alongside the existing `Source::Docker` ones (a fake
  `SystemProbe`-equivalent isn't needed — `FakeProbe` already implements the
  unchanged trait; add one assertion that a `Source::Host` entry's key comes
  out as `host-vllm-<pid>`).
- No integration test against a real `tt-model serve` process in CI (no TT
  hardware there); this stays consistent with the rest of the `[i]` panel,
  which is Docker-probe-tested the same way (fakes only, real-hardware
  verification is manual and noted in the PR/AGENTS.md).

## Open items intentionally deferred

- `ModelProfile` (compile/load-progress % baselines) has no entries for
  `tt-tnt`-style custom models — `progress` stays `None` for these,
  same as every uncatalogued service today.
- If a direct-vLLM process is ever launched with `TT_VISIBLE_DEVICES` set to
  multiple chips (the tt-tnt docs flag this as a trap for single-device
  serving), detection doesn't need to care — it only reads `MESH_DEVICE`,
  which is independent of that trap.
