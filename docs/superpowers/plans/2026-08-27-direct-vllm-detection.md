# Direct (non-Docker) vLLM-on-TT Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a bare (non-Docker) `vllm serve <model> ...` / `... server_example_tt.py --model <model> ...` launch on TT hardware show up in the `[i]` Inference Server panel with the same phase/progress/metrics richness a Docker-wrapped tt-inference-server container already gets.

**Architecture:** Add a `Source::Host { pid }` variant alongside the existing `Source::Docker { container }`; a new pure `parse_direct_vllm` recognizer feeds it from the same process scan that already finds Docker launches; a new `SystemProbe` (wrapping the existing `DockerProbe`) implements the unchanged `ContainerProbe` trait, dispatching per-call on the identity key's prefix so Docker calls behave exactly as today and Host calls read `/proc` directly and shell out to local `ps`/`sh` scoped to the launched pid's whole process tree (no cgroup to scope by for free, unlike Docker).

**Tech Stack:** Rust, `sysinfo` 0.38 (`Process::environ()`, already populated by the existing `ProcessRefreshKind::everything()` call), std `Command`/`/proc` reads. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-27-direct-vllm-detection-design.md`

## Global Constraints

- Identity key for a host process: `format!("host-vllm-{pid}")` (constant prefix `"host-vllm-"`).
- TT-confirmation gate: require `MESH_DEVICE` or `TT_METAL_HOME` present in the process's environment before treating it as a TT inference server.
- Default port when unspecified: `8000`.
- `ContainerProbe`'s trait *signature* does not change — only a new default method (`top_proc_cmd`) is added, and a new implementor (`SystemProbe`) is introduced. Existing `DockerProbe` behavior is byte-for-byte unchanged.
- Every new I/O helper degrades to empty/`None` on any failure (spawn error, missing `/proc` entry, exited pid) — never panics. This matches every existing method in `probe.rs`.
- `#[cfg(target_os = "linux")]` gating: the same gate `detected_inference_servers()` already has; the new probe-side `/proc` code is Linux-only too (the crate already only builds `luwen`/TT-hardware paths on Linux, and `ContainerProbe`/`DockerProbe` are already implicitly Linux/macOS-with-docker-only in practice — no new cfg needed beyond what `detected_inference_servers()` already has, since `SystemProbe`'s host branch is only ever reached via a `Source::Host` value, which only `parse_direct_vllm` — itself only called from the `cfg(target_os = "linux")` function — ever produces).

---

## File Structure

- `src/workload/inference_server/detect.rs` — add `Source::Host`, `service_key`, `parse_direct_vllm`.
- `src/workload/inference_server/mod.rs` — re-export the two new `detect` items.
- `src/workload/inference_server/monitor.rs` — use `service_key` instead of the now-invalid irrefutable `Source::Docker` destructure; call `probe.top_proc_cmd(...)` instead of the hardcoded `TOP_PROC_CMD` constant; construct `SystemProbe` in `spawn()`.
- `src/workload/inference_server/probe.rs` — refactor `docker()`'s bounded-subprocess plumbing into a shared `run_bounded`/`run_local`; add `ContainerProbe::top_proc_cmd` (defaulted); add `process_tree_pids`/`parse_children`; add `SystemProbe`.
- `src/workload/host_processes.rs` — `detected_inference_servers()` gains a second per-process check using `parse_direct_vllm`.

---

### Task 1: `Source::Host` variant + `service_key`

**Files:**
- Modify: `src/workload/inference_server/detect.rs:9-13` (the `Source` enum)
- Modify: `src/workload/inference_server/mod.rs:21` (re-export list)
- Modify: `src/workload/inference_server/monitor.rs:268-269,322-329` (`rebuild_snapshot`, `merge_detections`'s `container_of`)
- Modify: `src/workload/host_processes.rs:150,163-164` (`detected_inference_servers`)
- Test: inline `#[cfg(test)]` in `detect.rs`

**Interfaces:**
- Produces: `pub enum Source { Docker { container: String }, Host { pid: i32 } }`; `pub fn service_key(source: &Source) -> String`; `pub(crate) const HOST_KEY_PREFIX: &str = "host-vllm-";`

- [ ] **Step 1: Write the failing test for `service_key`**

Add to `src/workload/inference_server/detect.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn service_key_docker_uses_container_name_host_uses_pid() {
    let docker = Source::Docker {
        container: "tt-inference-server-abc123".into(),
    };
    assert_eq!(service_key(&docker), "tt-inference-server-abc123");

    let host = Source::Host { pid: 4242 };
    assert_eq!(service_key(&host), "host-vllm-4242");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib service_key_docker_uses_container_name_host_uses_pid`
Expected: FAIL to compile — `service_key` and `Source::Host` don't exist yet.

- [ ] **Step 3: Add the `Host` variant and `service_key`**

In `src/workload/inference_server/detect.rs`, replace:

```rust
/// Where the server runs. v1 handles Docker; the `Host` trail (non-container
/// installs) slots in behind this enum without changing consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Docker { container: String },
    // Trail: Host { unit_or_pid: String },
}
```

with:

```rust
/// Where the server runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Docker { container: String },
    /// A bare (non-Docker) process, e.g. a direct `vllm serve`/
    /// `server_example_tt.py` launch — see `parse_direct_vllm`.
    Host { pid: i32 },
}

/// Prefix of the identity key `service_key` derives for a `Source::Host` —
/// also used by `probe::SystemProbe` to recognize a host-keyed call.
pub(crate) const HOST_KEY_PREFIX: &str = "host-vllm-";

/// Stable identity key for a detected server, used for prev-state lookup,
/// dedup, and the monitor's change-signature. Docker keys by container name
/// (survives the monitor's own restarts, stable across ticks); a bare host
/// process has no such name, so it keys by pid — a restart gets a fresh key
/// and starts from `fresh_state`, which is correct: the old process's
/// kernel/RSS history is not the new process's history.
pub fn service_key(source: &Source) -> String {
    match source {
        Source::Docker { container } => container.clone(),
        Source::Host { pid } => format!("{HOST_KEY_PREFIX}{pid}"),
    }
}
```

- [ ] **Step 4: Fix the two call sites this makes non-exhaustive**

In `src/workload/inference_server/monitor.rs`, `rebuild_snapshot` currently has:

```rust
    for server in detected {
        let Source::Docker { container } = &server.source;
```

Replace with:

```rust
    for server in detected {
        let key0 = service_key(&server.source);
        let container = key0.as_str();
```

(Every later use of `container` in this loop — `service_for`, the fallback
label/port branch, `build_sample`, `prev.iter().find(|s| s.key == key)` —
is unchanged; `container` is now the general identity key, which is exactly
what those calls already treated it as.)

And `merge_detections`'s `container_of` closure:

```rust
    let container_of = |s: &InferenceServer| -> String {
        let Source::Docker { container } = &s.source;
        container.clone()
    };
```

Replace with:

```rust
    let container_of = |s: &InferenceServer| -> String { service_key(&s.source) };
```

Add `service_key` to `monitor.rs`'s existing import:

```rust
use crate::workload::inference_server::detect::{InferenceServer, Source};
```

becomes:

```rust
use crate::workload::inference_server::detect::{service_key, InferenceServer, Source};
```

In `src/workload/host_processes.rs`, `detected_inference_servers` currently has:

```rust
        use crate::workload::inference_server::{parse_inference_server, Source};

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for p in self.sys.processes().values() {
            let name = p.name().to_string_lossy().to_string();
            let cmdline = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(server) = parse_inference_server(&name, &cmdline) {
                let Source::Docker { container } = &server.source;
                if seen.insert(container.clone()) {
                    out.push(server);
                }
            }
        }
        out
```

Replace with:

```rust
        use crate::workload::inference_server::{parse_inference_server, service_key};

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for p in self.sys.processes().values() {
            let name = p.name().to_string_lossy().to_string();
            let cmdline = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(server) = parse_inference_server(&name, &cmdline) {
                if seen.insert(service_key(&server.source)) {
                    out.push(server);
                }
            }
        }
        out
```

(`Source` is no longer referenced directly in this function, so it's dropped
from the `use` line; `service_key` replaces it.)

In `src/workload/inference_server/mod.rs`, change:

```rust
pub use detect::{parse_inference_server, InferenceServer, Source};
```

to:

```rust
pub use detect::{parse_inference_server, service_key, InferenceServer, Source};
```

- [ ] **Step 5: Run test to verify it passes, and the crate still builds**

Run: `cargo build --features tui && cargo test --lib service_key_docker_uses_container_name_host_uses_pid`
Expected: builds cleanly, test PASSes.

- [ ] **Step 6: Run the full test suite (nothing else should have moved)**

Run: `cargo test --lib --features tui`
Expected: all previously-passing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/workload/inference_server/detect.rs src/workload/inference_server/mod.rs src/workload/inference_server/monitor.rs src/workload/host_processes.rs
git commit -m "feat(inference_server): add Source::Host + service_key identity helper"
```

---

### Task 2: `parse_direct_vllm` (pure detection)

**Files:**
- Modify: `src/workload/inference_server/detect.rs` (add the function + tests, after `parse_inspect`)

**Interfaces:**
- Consumes: `env_value`/`model_arg` (already private to this file, unchanged); `probe::parse_env_var` (existing, `pub fn parse_env_var(env_output: &str, key: &str) -> Option<String>` in `probe.rs`).
- Produces: `pub fn parse_direct_vllm(name: &str, cmdline: &str, environ: &str, pid: i32) -> Option<InferenceServer>`

- [ ] **Step 1: Write the failing tests**

Add to `detect.rs`'s test module:

```rust
// Real shapes from tt-tnt's docs/serving-with-tt-kernel.md and AUTOFIX.md,
// trimmed to the parts parse_direct_vllm reads.
const CMDLINE_VLLM_SERVE: &str = "vllm serve episod/tt-tnt-1024 --max_model_len 512 \
    --max_num_seqs 32 --port 8000 --additional-config {\"tt\":{\"fabric_config\":\"FABRIC_2D_TORUS_XY\"}}";
const ENVIRON_VLLM_SERVE: &str = "TT_METAL_HOME=/home/ttuser/tt-metal-src-vllm-home\n\
    MESH_DEVICE=P300x2\nHF_MODEL=episod/tt-tnt-1024\nPATH=/usr/bin\n";

const CMDLINE_EXAMPLE_SCRIPT: &str =
    "python3 server_example_tt.py --model episod/tt-tnt --max_model_len 2048 --max_num_seqs 8";
const ENVIRON_EXAMPLE_SCRIPT: &str =
    "MESH_DEVICE=P150\nHF_MODEL=episod/tt-tnt\nVLLM_USE_V1=1\n";

#[test]
fn parses_vllm_serve_shape() {
    let s = parse_direct_vllm("vllm", CMDLINE_VLLM_SERVE, ENVIRON_VLLM_SERVE, 4242)
        .expect("should detect vllm serve shape");
    assert_eq!(s.source, Source::Host { pid: 4242 });
    assert_eq!(s.model.as_deref(), Some("episod/tt-tnt-1024"));
    assert_eq!(s.mesh.as_deref(), Some("P300x2"));
    assert_eq!(s.port, Some(8000));
    assert!(s.uses_tt_device);
}

#[test]
fn parses_example_script_shape() {
    let s = parse_direct_vllm("python3", CMDLINE_EXAMPLE_SCRIPT, ENVIRON_EXAMPLE_SCRIPT, 99)
        .expect("should detect server_example_tt.py shape");
    assert_eq!(s.source, Source::Host { pid: 99 });
    assert_eq!(s.model.as_deref(), Some("episod/tt-tnt"));
    assert_eq!(s.mesh.as_deref(), Some("P150"));
    // no --port in this shape → default
    assert_eq!(s.port, Some(8000));
}

#[test]
fn rejects_vllm_serve_without_tt_evidence() {
    // Same cmdline, but neither MESH_DEVICE nor TT_METAL_HOME set — must not
    // be misclassified as TT-backed (could be plain upstream vLLM on GPU).
    let no_tt_env = "HOME=/root\nPATH=/usr/bin\n";
    assert!(parse_direct_vllm("vllm", CMDLINE_VLLM_SERVE, no_tt_env, 1).is_none());
}

#[test]
fn rejects_unrelated_processes() {
    assert!(parse_direct_vllm("bash", "bash -c ls", ENVIRON_VLLM_SERVE, 1).is_none());
    assert!(parse_direct_vllm("python3", "python3 train.py", ENVIRON_VLLM_SERVE, 1).is_none());
}

#[test]
fn parses_explicit_port_override() {
    let cmd = "vllm serve some/model --port 9001";
    let s = parse_direct_vllm("vllm", cmd, ENVIRON_VLLM_SERVE, 1).unwrap();
    assert_eq!(s.port, Some(9001));
}

#[test]
fn recognizes_a_full_path_vllm_binary_not_just_the_bare_name() {
    // A venv console-script's argv[0] is commonly a full path
    // (e.g. `/home/ttuser/venv-vllm-standalone/bin/vllm serve ...`), not the
    // bare token "vllm" — real deployments look like this, not like the
    // other tests' bare-name fixtures.
    let cmd = "/home/ttuser/venv-vllm-standalone/bin/vllm serve episod/tt-tnt-1024 --port 8000";
    let s = parse_direct_vllm("vllm", cmd, ENVIRON_VLLM_SERVE, 1)
        .expect("full-path vllm binary must still be recognized");
    assert_eq!(s.model.as_deref(), Some("episod/tt-tnt-1024"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib parses_vllm_serve_shape parses_example_script_shape rejects_vllm_serve_without_tt_evidence rejects_unrelated_processes parses_explicit_port_override`
Expected: FAIL to compile — `parse_direct_vllm` doesn't exist yet.

- [ ] **Step 3: Implement `parse_direct_vllm`**

Add near the top of `detect.rs`, with the file's other `use` statements (it
currently has none before its first function — this is the first):

```rust
use crate::workload::inference_server::probe::parse_env_var;
```

Add to `detect.rs`, after `parse_inspect` and before the `#[cfg(test)]` module:

```rust
/// Recognize a direct (non-Docker) vLLM-on-TT launch from `name` + `cmdline`
/// + the process's own `environ` (`KEY=VALUE`-per-line text — same shape
/// `probe::parse_env_var` already expects), building a `Source::Host { pid }`
/// record. Two real shapes (from `tt-tnt`'s `tt-model serve`, which execs one
/// of these directly): `vllm serve <model> ...`, or a cmdline containing
/// `server_example_tt.py` with a `--model <X>` arg.
///
/// Requires `MESH_DEVICE` or `TT_METAL_HOME` present in `environ` to confirm
/// this is genuinely TT-backed — mirrors `parse_inference_server`'s
/// `uses_tt_device` check, which has no `/dev/tenstorrent` cmdline
/// equivalent to key off for a bare host process. Without this gate, plain
/// upstream vLLM (e.g. running against a GPU on the same box for comparison)
/// would be misclassified as a TT inference server.
pub fn parse_direct_vllm(
    name: &str,
    cmdline: &str,
    environ: &str,
    pid: i32,
) -> Option<InferenceServer> {
    let _ = name; // recognized from cmdline shape alone; kept for API symmetry with parse_inference_server
    let toks: Vec<&str> = cmdline.split_whitespace().collect();

    // Match by `ends_with("vllm")`, not `== "vllm"`: a venv console-script's
    // argv[0] is commonly a full path (e.g.
    // `/home/ttuser/venv-vllm-standalone/bin/vllm`), not the bare token.
    let model = toks
        .iter()
        .position(|t| t.ends_with("vllm"))
        .filter(|&i| toks.get(i + 1) == Some(&"serve"))
        .and_then(|i| toks.get(i + 2))
        .filter(|t| !t.starts_with('-'))
        .map(|s| s.to_string())
        .or_else(|| {
            cmdline
                .contains("server_example_tt.py")
                .then(|| model_arg(&toks))
                .flatten()
        })?;

    let mesh = parse_env_var(environ, "MESH_DEVICE");
    let tt_metal_home = parse_env_var(environ, "TT_METAL_HOME");
    if mesh.is_none() && tt_metal_home.is_none() {
        return None;
    }

    let port = toks
        .iter()
        .position(|t| *t == "--port")
        .and_then(|i| toks.get(i + 1))
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8000);

    Some(InferenceServer {
        source: Source::Host { pid },
        // No container image for a bare process; a stable label is enough —
        // only used for the monitor's display/signature formatting.
        image: "vllm-direct".to_string(),
        model: Some(model),
        mesh,
        arch: None,
        device: None,
        port: Some(port),
        uses_tt_device: true,
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib parses_vllm_serve_shape parses_example_script_shape rejects_vllm_serve_without_tt_evidence rejects_unrelated_processes parses_explicit_port_override`
Expected: all 5 PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workload/inference_server/detect.rs
git commit -m "feat(inference_server): recognize direct vLLM-on-TT launches"
```

---

### Task 3: Wire detection into `host_processes.rs`

**Files:**
- Modify: `src/workload/host_processes.rs` (`detected_inference_servers`)
- Modify: `src/workload/inference_server/mod.rs` (re-export `parse_direct_vllm`)

**Interfaces:**
- Consumes: `parse_direct_vllm` (Task 2), `service_key` (Task 1), `sysinfo::Process::environ() -> &[OsString]` (already populated — `update()` already calls `ProcessRefreshKind::everything()`, whose `environ` field defaults to `UpdateKind::OnlyIfNotSet`, i.e. populated).
- Produces: `detected_inference_servers()` now also finds direct-vLLM launches.

- [ ] **Step 1: Write the failing test**

`detected_inference_servers` does real process-table I/O, so it isn't unit-tested directly today (matches the file's existing pattern — no test currently covers it). Instead, add a focused test at the boundary you're changing, in `host_processes.rs`'s own test module, that exercises the *decision* this step adds without needing a real process table — by testing that `parse_direct_vllm` (already unit-tested in Task 2) is what gates inclusion, via a tiny module-level (not `impl HostProcesses`-associated — so it's callable bare, the same way both `detected_inference_servers` and the test below need to call it) harness function extracted for testability:

```rust
/// Testable core of the per-process decision `detected_inference_servers`
/// makes: given one process's (name, cmdline, environ, pid), which
/// `InferenceServer` (if any) it contributes. A plain module-level function
/// (not a `HostProcesses` method) so both the real scan and this file's
/// tests can call it directly without an instance. Docker detection is
/// tried first, direct-vLLM second.
#[cfg(target_os = "linux")]
fn classify_one(
    name: &str,
    cmdline: &str,
    environ: &str,
    pid: i32,
) -> Option<crate::workload::InferenceServer> {
    use crate::workload::inference_server::{parse_direct_vllm, parse_inference_server};
    parse_inference_server(name, cmdline).or_else(|| parse_direct_vllm(name, cmdline, environ, pid))
}
```

Place this above `impl HostProcesses { ... }` (module level, not inside the
`impl` block). The test:

```rust
    #[cfg(target_os = "linux")]
    #[test]
    fn classify_one_prefers_docker_then_falls_back_to_direct_vllm() {
        let docker_cmd = "docker run --rm --name tt-inference-server-x \
            --device /dev/tenstorrent:/dev/tenstorrent --publish 0.0.0.0:8002:8002 \
            ghcr.io/tenstorrent/tt-inference-server/vllm-tt-metal-src-release:0.14.0 --model M";
        assert!(matches!(
            classify_one("docker", docker_cmd, "", 1).unwrap().source,
            crate::workload::Source::Docker { .. }
        ));

        let direct = classify_one(
            "vllm",
            "vllm serve episod/tt-tnt-1024 --port 8000",
            "MESH_DEVICE=P300x2\n",
            4242,
        )
        .unwrap();
        assert!(matches!(direct.source, crate::workload::Source::Host { pid: 4242 }));

        assert!(classify_one("bash", "bash -c ls", "", 1).is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib classify_one_prefers_docker_then_falls_back_to_direct_vllm`
Expected: FAIL to compile — `classify_one` doesn't exist in `detected_inference_servers`'s implementation yet, and `parse_direct_vllm` isn't re-exported from `inference_server` yet.

- [ ] **Step 3: Re-export `parse_direct_vllm` and wire it in**

In `src/workload/inference_server/mod.rs`:

```rust
pub use detect::{parse_inference_server, service_key, InferenceServer, Source};
```

becomes:

```rust
pub use detect::{parse_direct_vllm, parse_inference_server, service_key, InferenceServer, Source};
```

In `src/workload/host_processes.rs`, replace the whole `detected_inference_servers` body:

```rust
    #[cfg(target_os = "linux")]
    pub fn detected_inference_servers(&self) -> Vec<crate::workload::InferenceServer> {
        use crate::workload::inference_server::{parse_inference_server, service_key};

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for p in self.sys.processes().values() {
            let name = p.name().to_string_lossy().to_string();
            let cmdline = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(server) = parse_inference_server(&name, &cmdline) {
                if seen.insert(service_key(&server.source)) {
                    out.push(server);
                }
            }
        }
        out
    }
```

with (note `classify_one` is added at **module level**, above `impl
HostProcesses`, not inside it — see Step 1's harness):

```rust
/// Testable core of the per-process decision `detected_inference_servers`
/// makes: given one process's (name, cmdline, environ, pid), which
/// `InferenceServer` (if any) it contributes. Docker detection is tried
/// first — a TT inference-server container's foreground `docker run`
/// process would also, in principle, cmdline-match nothing
/// vLLM-direct-shaped, so the ordering is defensive rather than
/// load-bearing today.
#[cfg(target_os = "linux")]
fn classify_one(
    name: &str,
    cmdline: &str,
    environ: &str,
    pid: i32,
) -> Option<crate::workload::InferenceServer> {
    use crate::workload::inference_server::{parse_direct_vllm, parse_inference_server};
    parse_inference_server(name, cmdline).or_else(|| parse_direct_vllm(name, cmdline, environ, pid))
}
```

And, inside `impl HostProcesses { ... }`:

```rust
    /// The TT inference-server containers *and* direct (non-Docker) vLLM-on-TT
    /// processes detected in the current snapshot (deduped by identity key),
    /// as structured [`crate::workload::InferenceServer`] records — no
    /// docker/proc I/O beyond what `sysinfo` already collected this refresh.
    /// Feed these to a [`crate::workload::InferenceServerMonitor`] each
    /// refresh.
    #[cfg(target_os = "linux")]
    pub fn detected_inference_servers(&self) -> Vec<crate::workload::InferenceServer> {
        use crate::workload::inference_server::service_key;

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (pid, p) in self.sys.processes() {
            let name = p.name().to_string_lossy().to_string();
            let cmdline = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            let environ = p
                .environ()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n");
            let pid_i32 = i32::try_from(pid.as_u32()).unwrap_or(i32::MAX);
            if let Some(server) = classify_one(&name, &cmdline, &environ, pid_i32) {
                if seen.insert(service_key(&server.source)) {
                    out.push(server);
                }
            }
        }
        out
    }
```

(Note the loop changes from `self.sys.processes().values()` to
`self.sys.processes()` — a `(pid, process)` iterator — because
`classify_one` needs the pid for the `Source::Host` case; the docstring's
doc-comment about "no docker/proc I/O" is updated to reflect that `environ`
now gets read too, which `ProcessRefreshKind::everything()` already
populated, so it's not new I/O on top of what `update()` already did.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib classify_one_prefers_docker_then_falls_back_to_direct_vllm`
Expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all tests pass, including the existing (untouched) `detected_inference_servers`-adjacent behavior.

- [ ] **Step 6: Commit**

```bash
git add src/workload/host_processes.rs src/workload/inference_server/mod.rs
git commit -m "feat(inference_server): detect direct vLLM-on-TT processes in the host scan"
```

---

### Task 4: Process-tree pid walking

**Files:**
- Modify: `src/workload/inference_server/probe.rs` (add near the bottom, before the `ContainerProbe`/`DockerProbe` section)

**Interfaces:**
- Produces: `fn parse_children(text: &str) -> Vec<i32>` (private, pure); `fn process_tree_pids(root: i32) -> Vec<i32>` (private, Linux-only real fs walk, built on `parse_children`).

- [ ] **Step 1: Write the failing test**

Add to `probe.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn parse_children_splits_whitespace_separated_pids() {
    assert_eq!(parse_children("123 456 789\n"), vec![123, 456, 789]);
    assert_eq!(parse_children(""), Vec::<i32>::new());
    assert_eq!(parse_children("  42  "), vec![42]);
    // A non-numeric token (shouldn't happen in a real /proc file, but the
    // parser must degrade rather than panic) is silently skipped.
    assert_eq!(parse_children("1 abc 3"), vec![1, 3]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib parse_children_splits_whitespace_separated_pids`
Expected: FAIL to compile — `parse_children` doesn't exist yet.

- [ ] **Step 3: Implement `parse_children` and `process_tree_pids`**

Add to `probe.rs`, after the existing pure parsers (`top_process`) and before
the `// ── Container access abstraction ──` section comment:

```rust
/// Parse one `/proc/<pid>/task/<pid>/children` file's contents
/// (whitespace-separated child pids) into a `Vec<i32>`. Pure — a malformed
/// or non-numeric token is silently skipped rather than failing the whole
/// parse, matching every other parser in this module.
fn parse_children(text: &str) -> Vec<i32> {
    text.split_whitespace().filter_map(|t| t.parse().ok()).collect()
}

/// All pids in the process tree rooted at `root` (inclusive), found by
/// recursively reading `/proc/<pid>/task/<pid>/children`. Docker gets this
/// scoping for free from the container's own PID namespace (`docker
/// exec`/`docker stats` only ever see the container's own processes); a bare
/// host-launched vLLM has no such boundary, and it does fork real children
/// (a `g++` compile child during kernel compilation, TT device worker
/// subprocess(es)) whose CPU/RSS must count toward the same service's
/// signals. Best-effort: a pid whose `children` file is already gone (it
/// exited between reads) simply isn't descended further — same
/// degrade-gracefully contract as every other probe helper.
#[cfg(target_os = "linux")]
fn process_tree_pids(root: i32) -> Vec<i32> {
    let mut out = vec![root];
    let mut frontier = vec![root];
    while let Some(pid) = frontier.pop() {
        let path = format!("/proc/{pid}/task/{pid}/children");
        if let Ok(text) = std::fs::read_to_string(&path) {
            for child in parse_children(&text) {
                out.push(child);
                frontier.push(child);
            }
        }
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib parse_children_splits_whitespace_separated_pids`
Expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass. (`process_tree_pids` isn't called yet — `#[allow(dead_code)]`
would trigger a warning; that's expected and resolved in Task 6 when it's
wired up. If CI treats warnings as errors, add `#[allow(dead_code)]` above
`process_tree_pids` for this task only, and remove it in Task 6 once it's used.)

- [ ] **Step 6: Commit**

```bash
git add src/workload/inference_server/probe.rs
git commit -m "feat(inference_server): add process-tree pid walking for host probing"
```

---

### Task 5: Shared bounded-subprocess runner + `top_proc_cmd` trait method

**Files:**
- Modify: `src/workload/inference_server/probe.rs` (`docker()`, `ContainerProbe` trait, `DockerProbe`) — `monitor.rs` is untouched in this task; see the ordering note in Step 3.

**Interfaces:**
- Consumes: nothing new.
- Produces: `fn run_bounded(cmd: std::process::Command) -> String` (private); `fn run_local(args: &[&str]) -> String` (private); `ContainerProbe::top_proc_cmd(&self, key: &str) -> String` (defaulted to `TOP_PROC_CMD`); `pub(crate) const TOP_PROC_CMD: &str` (moved here from `monitor.rs`, which no longer needs its own copy after Task 7).

- [ ] **Step 1: Write the failing test**

This task is a refactor of already-tested behavior (`docker()`'s existing
tests exercise its callers indirectly via `parse_docker_stats`, etc.) plus
one new pure default-method assertion. Add to `probe.rs`'s tests:

```rust
#[test]
fn default_top_proc_cmd_is_the_docker_global_ps() {
    struct Dummy;
    impl ContainerProbe for Dummy {
        fn env(&self, _c: &str) -> String { String::new() }
        fn stats(&self, _c: &str) -> String { String::new() }
        fn exec(&self, _c: &str, _sh: &str) -> String { String::new() }
        fn http(&self, _port: u16, _path: &str) -> (u16, String) { (0, String::new()) }
    }
    assert_eq!(Dummy.top_proc_cmd("anything"), TOP_PROC_CMD);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib default_top_proc_cmd_is_the_docker_global_ps`
Expected: FAIL to compile — `top_proc_cmd` and `TOP_PROC_CMD` don't exist in
`probe.rs` yet.

- [ ] **Step 3: Add a `TOP_PROC_CMD` constant + the trait method in `probe.rs`**

Note: `monitor.rs` keeps its own private `const TOP_PROC_CMD` for now (still
used by `build_sample`, unchanged in this task) — the two constants
temporarily coexist in separate modules with identical literal values; the
duplication is removed in Task 7, Step 3, when `build_sample` switches to
`probe.top_proc_cmd(...)` and `monitor.rs`'s copy is deleted. Don't remove
`monitor.rs`'s constant yet, or `build_sample` won't compile until Task 7.

In `src/workload/inference_server/probe.rs`, add near the top (after the
module doc comment, before `parse_docker_stats`):

```rust
/// `ps` invocation whose first data row is the service's top-CPU consumer.
/// Correct as a *global* `ps` when run via `docker exec` (the container's own
/// PID namespace already scopes it to that container's processes);
/// `SystemProbe` overrides `ContainerProbe::top_proc_cmd` for a host-keyed
/// service to scope this the same way `process_tree_pids` does.
pub(crate) const TOP_PROC_CMD: &str = "ps -eo pcpu,rss,comm --sort=-pcpu";
```

Then, in the `ContainerProbe` trait definition, add a new method with a
default implementation (after `http`, before `list_servers`):

```rust
    /// The `ps`-style command whose first data row is the service's
    /// top-CPU process (see [`top_process`]/[`contains_python`]'s expected
    /// 3-column, header-then-rows shape). Default: [`TOP_PROC_CMD`] — correct
    /// for `DockerProbe`, since `docker exec` already scopes to the
    /// container's own PID namespace. `key` is the same identity string
    /// passed to `env`/`stats`/`exec`.
    fn top_proc_cmd(&self, key: &str) -> String {
        let _ = key;
        TOP_PROC_CMD.to_string()
    }
```

- [ ] **Step 4: Refactor `docker()` into shared `run_bounded`/`run_local`**

Replace the existing `docker()` function body:

```rust
pub(crate) fn docker(args: &[&str]) -> String {
    use std::io::Read;
    use std::process::Stdio;

    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut child = match Command::new("docker")
        .args(&owned)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    // Only the pipe moves into the reader thread; `child` stays here so a
    // timeout can kill it.
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut so) = stdout {
            let _ = so.by_ref().take(MAX_DOCKER_OUTPUT).read_to_end(&mut buf);
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    match rx.recv_timeout(DOCKER_TIMEOUT) {
        Ok(s) => {
            let _ = child.wait();
            s
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            String::new()
        }
    }
}
```

with:

```rust
/// Run a pre-built `Command`, bounded by [`DOCKER_TIMEOUT`] (renamed in
/// spirit only — the bound applies to any local subprocess this module
/// spawns, not just `docker`) and [`MAX_DOCKER_OUTPUT`]. Any spawn/exec
/// failure, or exceeding the timeout, yields `""` rather than panicking or
/// blocking the monitor thread indefinitely; on timeout the child is killed
/// so the reader thread's pipe hits EOF and the thread exits (no orphan).
fn run_bounded(mut cmd: Command) -> String {
    use std::io::Read;
    use std::process::Stdio;

    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut so) = stdout {
            let _ = so.by_ref().take(MAX_DOCKER_OUTPUT).read_to_end(&mut buf);
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    match rx.recv_timeout(DOCKER_TIMEOUT) {
        Ok(s) => {
            let _ = child.wait();
            s
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            String::new()
        }
    }
}

/// Run `docker <args>`, returning stdout as a lossy UTF-8 string — see
/// [`run_bounded`] for the timeout/output-cap contract.
pub(crate) fn docker(args: &[&str]) -> String {
    let mut cmd = Command::new("docker");
    cmd.args(args);
    run_bounded(cmd)
}

/// Run `args[0] args[1..]` locally (no `docker` wrapper) — used by
/// `SystemProbe`'s host-process probing (`ps`, `sh -c <find/ps command>`)
/// where `docker exec`'s namespace scoping doesn't apply. Same bounds as
/// [`docker`]. Empty `args` yields `""` without spawning anything.
fn run_local(args: &[&str]) -> String {
    let Some((program, rest)) = args.split_first() else {
        return String::new();
    };
    let mut cmd = Command::new(program);
    cmd.args(rest);
    run_bounded(cmd)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib default_top_proc_cmd_is_the_docker_global_ps`
Expected: PASS.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass, including every existing `docker()`-dependent test
(`DockerProbe`'s methods are unchanged, just routed through `run_bounded`).
`run_local` is unused until Task 6 — add `#[allow(dead_code)]` above it for
this task only if the build denies warnings; remove in Task 6.

- [ ] **Step 7: Commit**

```bash
git add src/workload/inference_server/probe.rs
git commit -m "refactor(inference_server): share bounded-subprocess plumbing, add top_proc_cmd hook"
```

---

### Task 6: `SystemProbe`

**Files:**
- Modify: `src/workload/inference_server/probe.rs` (add `SystemProbe` and its host-side helpers, after `DockerProbe`'s `impl ContainerProbe for DockerProbe` block)

**Interfaces:**
- Consumes: `process_tree_pids` (Task 4), `run_local` (Task 5), `parse_docker_stats` (existing), `HOST_KEY_PREFIX` (Task 1, `detect.rs`).
- Produces: `pub(crate) struct SystemProbe { docker: DockerProbe }` with `pub(crate) fn new(docker: DockerProbe) -> Self`, implementing `ContainerProbe`.

- [ ] **Step 1: Write the failing tests**

Add to `probe.rs`'s tests:

```rust
#[test]
fn system_probe_dispatches_docker_keys_to_the_inner_docker_probe() {
    // A docker-shaped key (no "host-vllm-" prefix) must behave exactly like
    // the wrapped DockerProbe — we can't run a real docker daemon in tests,
    // so this asserts the *dispatch*, not DockerProbe's own I/O (already
    // covered by DockerProbe's existing tests).
    assert!(pid_from_key("tt-inference-server-abc123").is_none());
    assert_eq!(pid_from_key("host-vllm-4242"), Some(4242));
    assert!(pid_from_key("host-vllm-not-a-pid").is_none());
}

#[test]
fn host_stats_text_matches_parse_docker_stats_shape() {
    // Doesn't spawn a real ps — exercises the formatting contract directly:
    // whatever host_stats_text produces must round-trip through
    // parse_docker_stats the same way a real docker stats line does.
    let formatted = format!("{:.2}%|{}B / 0B", 12.5_f32, 1024_u64 * 1024);
    let (cpu, rss) = parse_docker_stats(&formatted).expect("must parse");
    assert!((cpu - 12.5).abs() < 0.01);
    assert_eq!(rss, 1024 * 1024);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib system_probe_dispatches_docker_keys_to_the_inner_docker_probe host_stats_text_matches_parse_docker_stats_shape`
Expected: FAIL to compile — `pid_from_key` doesn't exist yet (the second
test only exercises existing `parse_docker_stats`, so it should already
compile and pass on its own — it's here to pin the format contract before
`host_stats_text` is written against it).

- [ ] **Step 3: Implement `SystemProbe` and its host-side helpers**

Add to `probe.rs`, after the `impl ContainerProbe for DockerProbe { ... }`
block and before the `docker()`/`run_bounded`/`run_local` functions (which
`SystemProbe`'s methods call):

```rust
/// Parse the pid out of a `service_key`-produced host key
/// (`"host-vllm-<pid>"`), or `None` for a Docker-shaped key (a plain
/// container name). `SystemProbe` uses this to decide, per call, which
/// backing implementation handles a given identity key.
fn pid_from_key(key: &str) -> Option<i32> {
    key.strip_prefix(super::detect::HOST_KEY_PREFIX)?.parse().ok()
}

/// Read `/proc/<pid>/environ` (NUL-separated `KEY=VALUE` records) and
/// reformat as newline-separated `KEY=VALUE` lines — the shape
/// [`parse_env_var`] already expects (matches the Docker path's
/// `env`/`printenv` dump shape). Missing/unreadable (process exited,
/// permission denied) degrades to `""`, same as every other probe method.
fn host_environ_text(pid: i32) -> String {
    match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => String::new(),
    }
}

/// `"{cpu}%|{rss_bytes}B / 0B"` — matches [`parse_docker_stats`]'s expected
/// shape (it only reads the "used" side of the `/`, so the "0B" denominator
/// is a placeholder, not a real memory limit; documented so it isn't
/// mistaken for one). CPU% and RSS are summed over `process_tree_pids(pid)`
/// via a single tree-scoped `ps` call — a bare host process has no cgroup to
/// aggregate by the way `docker stats` does for a container.
fn host_stats_text(pid: i32) -> String {
    let pids = process_tree_pids(pid);
    let pid_list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let out = run_local(&["ps", "-o", "pcpu=,rss=", "-p", &pid_list]);
    let (mut cpu_sum, mut rss_kib_sum) = (0.0f32, 0u64);
    for line in out.lines() {
        let mut it = line.split_whitespace();
        let cpu = it.next().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
        let rss = it.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        cpu_sum += cpu;
        rss_kib_sum = rss_kib_sum.saturating_add(rss);
    }
    let rss_bytes = rss_kib_sum.saturating_mul(1024);
    format!("{cpu_sum:.2}%|{rss_bytes}B / 0B")
}

/// The tree-scoped `ps` command a host-keyed [`ContainerProbe::top_proc_cmd`]
/// returns — same 3-column, header-then-rows shape [`TOP_PROC_CMD`] produces
/// (so [`top_process`]/[`contains_python`] parse it unchanged), scoped to
/// `process_tree_pids(pid)` instead of relying on a container PID namespace.
fn host_top_proc_cmd(pid: i32) -> String {
    let pids = process_tree_pids(pid);
    let pid_list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    format!("ps -o pcpu,rss,comm --sort=-pcpu -p {pid_list}")
}

/// Run `sh -c <sh>` locally with the target pid's own environment variables
/// (read via [`host_environ_text`]) injected explicitly — a docker-exec'd
/// shell inherits the container's env for free; a locally-spawned one does
/// not, and the `KERNEL_FIND_CMD`/`LOADED_FIND_CMD` shell text (in
/// `monitor.rs`) depends on `$TT_METAL_HOME`/`$CACHE_ROOT` being set.
fn host_exec(pid: i32, sh: &str) -> String {
    let env_text = host_environ_text(pid);
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(sh);
    for line in env_text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            cmd.env(k, v);
        }
    }
    run_bounded(cmd)
}

/// Wraps a [`DockerProbe`] and dispatches each `ContainerProbe` call by the
/// identity key's shape: a `"host-vllm-<pid>"` key (see
/// [`crate::workload::inference_server::detect::service_key`]) is handled by
/// reading `/proc`/running local `ps`/`sh` scoped to that pid's whole process
/// tree; any other key (a Docker container name) is forwarded to the wrapped
/// `DockerProbe` unchanged. This is the probe `InferenceServerMonitor::spawn`
/// constructs, so both deployment shapes are monitored simultaneously.
pub(crate) struct SystemProbe {
    docker: DockerProbe,
}

impl SystemProbe {
    pub(crate) fn new(docker: DockerProbe) -> Self {
        Self { docker }
    }
}

impl ContainerProbe for SystemProbe {
    fn env(&self, key: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => host_environ_text(pid),
            None => self.docker.env(key),
        }
    }
    fn stats(&self, key: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => host_stats_text(pid),
            None => self.docker.stats(key),
        }
    }
    fn exec(&self, key: &str, sh: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => host_exec(pid, sh),
            None => self.docker.exec(key, sh),
        }
    }
    fn http(&self, port: u16, path: &str) -> (u16, String) {
        // Container-agnostic already — plain localhost HTTP either way.
        self.docker.http(port, path)
    }
    fn top_proc_cmd(&self, key: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => host_top_proc_cmd(pid),
            None => self.docker.top_proc_cmd(key),
        }
    }
    fn list_servers(&self) -> Vec<super::InferenceServer> {
        // Detached host processes are already found by the periodic
        // `HostProcesses::detected_inference_servers()` re-scan (there's no
        // "detached vLLM process" concept analogous to a detached container
        // needing separate enumeration) — only forward Docker's.
        self.docker.list_servers()
    }
}
```

Remove the now-unneeded `#[allow(dead_code)]` from `process_tree_pids` and
`run_local` if you added them in Tasks 4/5.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib system_probe_dispatches_docker_keys_to_the_inner_docker_probe host_stats_text_matches_parse_docker_stats_shape`
Expected: both PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --lib --features tui`
Expected: all pass. `SystemProbe` isn't constructed anywhere yet (Task 7) —
if the build denies warnings for the now-unused `SystemProbe`/`new`, that's
expected and resolved in Task 7.

- [ ] **Step 6: Commit**

```bash
git add src/workload/inference_server/probe.rs
git commit -m "feat(inference_server): add SystemProbe for host-process probing"
```

---

### Task 7: Wire `SystemProbe` in; use `top_proc_cmd` in `build_sample`

**Files:**
- Modify: `src/workload/inference_server/monitor.rs` (`build_sample`, `InferenceServerMonitor::spawn`)

**Interfaces:**
- Consumes: `SystemProbe` (Task 6), `ContainerProbe::top_proc_cmd` (Task 5).

- [ ] **Step 1: Write the failing test**

Add to `monitor.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn rebuild_snapshot_tracks_a_host_source_by_its_pid_key() {
    let srv = InferenceServer {
        source: Source::Host { pid: 4242 },
        image: "vllm-direct".into(),
        model: Some("episod/tt-tnt-1024".into()),
        mesh: Some("P300x2".into()),
        arch: None,
        device: None,
        port: Some(8000),
        uses_tt_device: true,
    };
    let snap = rebuild_snapshot(std::slice::from_ref(&srv), &[], &FakeProbe, 5);
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].key, "host-vllm-4242");
    assert!(
        snap[0].label.contains("tt-tnt-1024"),
        "label derived from model basename, got {:?}",
        snap[0].label
    );
}
```

(`FakeProbe` already implements the unchanged `ContainerProbe` trait, so it
needs no changes — a `Source::Host` value is handled entirely by
`rebuild_snapshot`'s use of `service_key`, which Task 1 already made
source-agnostic.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib rebuild_snapshot_tracks_a_host_source_by_its_pid_key`
Expected: this should actually already PASS after Task 1 (since
`rebuild_snapshot` was made source-agnostic there) — run it now to confirm,
as a regression guard before this task's real change (switching
`build_sample`'s `ps` command source).

- [ ] **Step 3: Use `probe.top_proc_cmd(...)` in `build_sample`, and drop `monitor.rs`'s now-redundant constant**

In `src/workload/inference_server/monitor.rs`, `build_sample` currently has:

```rust
    let ps_output = probe.exec(container, TOP_PROC_CMD);
```

Replace with:

```rust
    let ps_output = probe.exec(container, &probe.top_proc_cmd(container));
```

Then delete `monitor.rs`'s own copy of the constant (Task 5 added the real
one to `probe.rs`; this was the temporary duplicate kept only so the tree
compiled between tasks):

```rust
/// `ps` invocation whose first data row is the container's top CPU consumer.
const TOP_PROC_CMD: &str = "ps -eo pcpu,rss,comm --sort=-pcpu";
```

- [ ] **Step 4: Construct `SystemProbe` in `spawn()`**

In `src/workload/inference_server/monitor.rs`:

```rust
    pub fn spawn() -> Self {
        Self::spawn_with_probe(Box::new(DockerProbe))
    }
```

becomes:

```rust
    pub fn spawn() -> Self {
        Self::spawn_with_probe(Box::new(SystemProbe::new(DockerProbe)))
    }
```

Add `SystemProbe` to the existing `probe` import:

```rust
use crate::workload::inference_server::probe::{
    contains_python, count_lines, parse_docker_stats, parse_env_var, parse_liveness, top_process,
    ContainerProbe, DockerProbe, Readiness, TickSample,
};
```

becomes:

```rust
use crate::workload::inference_server::probe::{
    contains_python, count_lines, parse_docker_stats, parse_env_var, parse_liveness, top_process,
    ContainerProbe, DockerProbe, Readiness, SystemProbe, TickSample,
};
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib rebuild_snapshot_tracks_a_host_source_by_its_pid_key`
Expected: PASS.

- [ ] **Step 6: Run the full test suite and clippy**

Run: `cargo test --lib --features tui && cargo clippy --features tui --all-targets -- -D warnings`
Expected: all tests pass; clippy clean (this is when any leftover
`#[allow(dead_code)]` from Tasks 4–6 should be removed, since everything is
now reachable — `process_tree_pids`, `run_local`, `SystemProbe` are all used
via `spawn()`).

- [ ] **Step 7: Commit**

```bash
git add src/workload/inference_server/monitor.rs
git commit -m "feat(inference_server): probe direct vLLM launches via SystemProbe"
```

---

## Manual verification (not automated — no TT hardware in CI)

Once merged, verify against a real `tt-model serve` launch on a box with TT
hardware (per the spec's testing section, this mirrors how the rest of the
`[i]` panel is verified — fakes in CI, manual on real hardware):

1. Launch a direct vLLM serve (e.g. `tt-model serve <bundle>` from `tt-tnt`,
   or the raw `vllm serve <model> --port 8000` command from the spec).
2. Run `tt-toplike-tui` (or `tt-toplike --mode ...` — whatever launches the
   `[i]` panel) alongside it.
3. Confirm the service appears in the `[i]` panel labeled from the model's
   basename, with `Compiling`/`Loading`/`Ready` phase transitions as the
   server comes up, and that CPU%/RSS reflect the process's activity
   (including any `g++` compile child, if compilation is still warm).
4. Note the result (pass/fail, what was checked) in `AGENTS.md`'s dev log,
   consistent with how every other backend/monitoring change in that log
   records its hardware-verification status.
