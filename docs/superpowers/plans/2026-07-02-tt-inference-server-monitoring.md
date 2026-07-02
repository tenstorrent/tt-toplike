# TT Inference-Server Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reliably detect a TT inference server on the box and show its lifecycle (Starting → Loading → Ready → Serving → Error) in Insights, instead of unrelated processes.

**Architecture:** Cmdline-first identity (parse the container's `docker run` args for model/mesh/arch/port/device), a Docker-log lifecycle engine (polled `docker logs --since`, version-tolerant line classifier, state reducer), and the existing liveness HTTP probe as readiness corroboration. A background thread folds logs+probe into a lock-free `arc-swap` cache read by the render path. Linux/TT-gated.

**Tech Stack:** Rust, `sysinfo` (process cmdlines), `arc-swap` (lock-free cache), `std::process::Command` (docker subprocess), `std::thread` + `std::sync::mpsc`. No new dependencies.

## Global Constraints

- **Linux/TT only.** All new modules and their wiring are `#[cfg(target_os = "linux")]`-gated; the feature compiles to a no-op elsewhere (matches `process_monitor`/`serving`).
- **No new dependencies.** Reuse `arc-swap`, `std::process::Command`, `std::thread`.
- **Off the hot path.** Zero subprocess/network I/O in the render loop or in any `rows()`-style call; all docker/probe I/O runs on the background thread, results read via `arc-swap`.
- **No panics.** Every failure path returns `None`/keeps last state; never unwrap external data.
- **SPDX header** on every new file: `// SPDX-License-Identifier: Apache-2.0` / `// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.`
- **Extension trails (build the seam even though v1 fills one impl):** `Source` enum (Docker now, Host later); `LogSource` trait (DockerLogSource now); variant `registry` (media/LLM now); `LifecycleEvent` enum + `LifecycleState.metrics: Option<ServingMetrics>` (always `None` in v1) for future serving telemetry.
- **Verify each task** with: `cargo test --locked --lib --features tui <module>` and `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings`.

---

## File Structure

- `src/workload/inference_server/mod.rs` — module root: re-exports + shared types (`Source`, `InferenceServer`, `Phase`, `LifecycleEvent`, `LifecycleState`, `ServingMetrics`).
- `src/workload/inference_server/detect.rs` — `parse_inference_server` (cmdline → `InferenceServer`) + variant registry.
- `src/workload/inference_server/logs.rs` — `LogSource` trait, `DockerLogSource`, `classify_log_line`, `LifecycleState::fold`.
- `src/workload/inference_server/monitor.rs` — `InferenceServerMonitor` (background thread + `arc-swap` cache).
- Modify `src/workload/mod.rs` — gate + declare the submodule, re-export public types.
- Modify `src/workload/liveness_probe.rs` — probe fix: `tt-inference-server` readiness via `/v1/models`.
- Modify `src/ui/tui/mod.rs` — build detected servers each refresh, feed the monitor, render the "Inference Servers" card.

A submodule directory (not a flat file) because this feature has four distinct responsibilities; keeping each in a focused file matches the skill's file-boundary guidance while the existing flat `workload/*.rs` files stay untouched.

---

### Task 1: Inference-server record + cmdline detection

**Files:**
- Create: `src/workload/inference_server/mod.rs`
- Create: `src/workload/inference_server/detect.rs`
- Modify: `src/workload/mod.rs`

**Interfaces:**
- Produces: `Source` (enum), `InferenceServer` (struct), `parse_inference_server(name: &str, cmdline: &str) -> Option<InferenceServer>`.

- [ ] **Step 1: Declare the gated submodule in `src/workload/mod.rs`**

Add near the other workload modules:

```rust
#[cfg(target_os = "linux")]
pub mod inference_server;
#[cfg(target_os = "linux")]
pub use inference_server::{InferenceServer, InferenceServerMonitor, LifecycleState, Phase, Source};
```

(The `InferenceServerMonitor`/`LifecycleState`/`Phase` re-exports resolve in Task 4/6; declaring them now keeps one edit to `mod.rs`. If the crate is built between tasks, temporarily trim the `pub use` to only the types defined so far.)

- [ ] **Step 2: Write `mod.rs` shared types**

`src/workload/inference_server/mod.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Detect and monitor TT inference servers (Docker-first), reporting a lifecycle
//! state to the Insights screen. See
//! `docs/superpowers/specs/2026-07-02-tt-inference-server-monitoring-design.md`.

mod detect;
mod logs;
mod monitor;

pub use detect::{parse_inference_server, InferenceServer, Source};
pub use logs::{classify_log_line, LifecycleEvent, LifecycleState, Phase, ServingMetrics};
pub use monitor::InferenceServerMonitor;
```

- [ ] **Step 3: Write the failing detection test**

`src/workload/inference_server/detect.rs` (test module first):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a real `docker run` on the QuietBox (media inference server).
    const DOCKER_RUN: &str = "docker run --rm --name tt-inference-server-5edd00ce \
        --device /dev/tenstorrent:/dev/tenstorrent --publish 0.0.0.0:8000:8000 \
        -e MODEL=FLUX.1-schnell -e MESH_DEVICE=P300x2 -e ARCH_NAME=blackhole \
        -e DEVICE=p300x2 -e NO_AUTH=1 ghcr.io/tenstorrent/tt-media-inference-server:0.17.0-8c48a10";

    #[test]
    fn parses_tt_inference_docker_run() {
        let s = parse_inference_server("docker", DOCKER_RUN).expect("should detect");
        assert!(matches!(s.source, Source::Docker { .. }));
        assert_eq!(s.model.as_deref(), Some("FLUX.1-schnell"));
        assert_eq!(s.mesh.as_deref(), Some("P300x2"));
        assert_eq!(s.arch.as_deref(), Some("blackhole"));
        assert_eq!(s.device.as_deref(), Some("p300x2"));
        assert_eq!(s.port, Some(8000));
        assert!(s.uses_tt_device);
        assert!(s.image.contains("tt-media-inference-server"));
    }

    #[test]
    fn ignores_unrelated_processes() {
        assert!(parse_inference_server("uvicorn", "uvicorn --host 0.0.0.0 main:app").is_none());
        assert!(parse_inference_server("bash", "docker ps").is_none());
        assert!(parse_inference_server("node", "node server.js").is_none());
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test --locked --lib --features tui inference_server::detect`
Expected: FAIL — `parse_inference_server`/`Source`/`InferenceServer` not found.

- [ ] **Step 5: Implement `detect.rs`**

Prepend to `src/workload/inference_server/detect.rs` (above the test module):

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Recognize a TT inference server from a process name + cmdline and extract a
//! structured record. Pure and cross-platform-compilable; only invoked on Linux.

/// Where the server runs. v1 handles Docker; the `Host` trail (non-container
/// installs) slots in behind this enum without changing consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Docker { container: String },
    // Trail: Host { unit_or_pid: String },
}

/// Identity of a detected inference server, parsed from its launch cmdline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceServer {
    pub source: Source,
    pub image: String,
    pub model: Option<String>,
    pub mesh: Option<String>,
    pub arch: Option<String>,
    pub device: Option<String>,
    pub port: Option<u16>,
    pub uses_tt_device: bool,
}

/// True if `image_or_name` names a TT inference server. Central recognizer so
/// new variants (vLLM-native, Triton, …) are a one-line addition here.
fn is_tt_inference_image(token: &str) -> bool {
    let t = token.to_lowercase();
    t.contains("inference-server")
        || t.contains("tt-media-inference")
        || (t.contains("ghcr.io/tenstorrent/tt-") && t.contains("server"))
}

/// Value following a flag token, e.g. `--name X` → `X`. Returns the next token.
fn value_after<'a>(toks: &[&'a str], i: usize) -> Option<&'a str> {
    toks.get(i + 1).copied()
}

/// Parse `-e KEY=VAL` env pairs (KEY is the token after `-e`/`--env`).
fn env_value(toks: &[&str], key: &str) -> Option<String> {
    for (i, t) in toks.iter().enumerate() {
        if (*t == "-e" || *t == "--env") {
            if let Some(kv) = value_after(toks, i) {
                if let Some((k, v)) = kv.split_once('=') {
                    if k == key {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Extract the container port from `--publish [host:]ctr[/proto]`, e.g.
/// `--publish 0.0.0.0:8000:8000` → 8000 (last numeric segment before any `/`).
fn published_port(toks: &[&str]) -> Option<u16> {
    for (i, t) in toks.iter().enumerate() {
        if *t == "--publish" || *t == "-p" {
            if let Some(spec) = value_after(toks, i) {
                let spec = spec.split('/').next().unwrap_or(spec);
                if let Some(seg) = spec.rsplit(':').next() {
                    if let Ok(p) = seg.parse::<u16>() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

/// Recognize a TT inference server from `name` + full `cmdline`, or `None`.
/// v1 only recognizes the `docker run` form.
pub fn parse_inference_server(name: &str, cmdline: &str) -> Option<InferenceServer> {
    let toks: Vec<&str> = cmdline.split_whitespace().collect();
    // Must be a docker run invocation.
    let is_docker_run = (name == "docker" || toks.first() == Some(&"docker"))
        && toks.iter().any(|t| *t == "run");
    if !is_docker_run {
        return None;
    }
    // Find the image token (recognizer) and the --name.
    let image = toks.iter().find(|t| is_tt_inference_image(t))?.to_string();
    let container = toks
        .iter()
        .position(|t| *t == "--name")
        .and_then(|i| value_after(&toks, i))
        .unwrap_or("unknown")
        .to_string();

    Some(InferenceServer {
        source: Source::Docker { container },
        image,
        model: env_value(&toks, "MODEL"),
        mesh: env_value(&toks, "MESH_DEVICE"),
        arch: env_value(&toks, "ARCH_NAME"),
        device: env_value(&toks, "DEVICE"),
        port: published_port(&toks),
        uses_tt_device: cmdline.contains("/dev/tenstorrent"),
    })
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --locked --lib --features tui inference_server::detect`
Expected: PASS (2 tests). Then `cargo clippy --locked --lib --features tui -- -D warnings` clean.

- [ ] **Step 7: Commit**

```bash
git add src/workload/mod.rs src/workload/inference_server/mod.rs src/workload/inference_server/detect.rs
git commit -m "feat(workload): detect TT inference server from docker run cmdline"
```

Note: `mod.rs` references `logs`/`monitor` (Tasks 3–6). If building now, comment out the `mod logs;`/`mod monitor;` lines and their re-exports until those tasks land, or implement Tasks 1,3,4,6 before the first full build.

---

### Task 2: Fix the liveness probe for the media variant

**Files:**
- Modify: `src/workload/liveness_probe.rs` (the `tt-inference-server` arm of `probe_spec`, and add a test)

**Interfaces:**
- Consumes: existing `probe_spec`, `judge_data_nonempty`, `judge_http_ok`.

- [ ] **Step 1: Write the failing test**

Add to `liveness_probe.rs` tests:

```rust
#[test]
fn tt_inference_server_probes_v1_models_not_health() {
    // The media variant returns 405 on /health but 200 on /v1/models, and the
    // LLM variant is OpenAI-compatible (also serves /v1/models), so /v1/models
    // is the common readiness endpoint.
    let spec = probe_spec("tt-inference-server").expect("has a probe spec");
    assert_eq!(spec.path, "/v1/models");
    // 200 with a served model ⇒ up; empty/again ⇒ not.
    assert!((spec.judge)(200, br#"{"object":"list","data":[{"id":"m"}]}"#));
    assert!(!(spec.judge)(405, b""));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui liveness_probe::tests::tt_inference_server_probes_v1_models`
Expected: FAIL — `spec.path` is currently `/health`.

- [ ] **Step 3: Change the probe spec**

In `probe_spec`, replace the `tt-inference-server` arm:

```rust
        // OpenAI-compatible across variants: /v1/models returns 200 when the API
        // is up. (The media variant returns 405 on /health, so we don't use it.)
        // Note: 200 here means "server reachable"; true model-ready comes from the
        // log lifecycle (LifecycleState), not this probe.
        "tt-inference-server" => Some(ProbeSpec {
            default_port: 8000,
            path: "/v1/models",
            judge: judge_data_nonempty,
        }),
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test --locked --lib --features tui liveness_probe`
Expected: PASS (existing probe tests + the new one).

- [ ] **Step 5: Commit**

```bash
git add src/workload/liveness_probe.rs
git commit -m "fix(workload): tt-inference-server readiness via /v1/models (media variant 405s on /health)"
```

---

### Task 3: Lifecycle types + log-line classifier

**Files:**
- Create: `src/workload/inference_server/logs.rs`

**Interfaces:**
- Produces: `Phase` (enum), `LifecycleEvent` (enum), `ServingMetrics` (struct), `classify_log_line(line: &str) -> Option<LifecycleEvent>`.

- [ ] **Step 1: Write the failing classifier test**

`src/workload/inference_server/logs.rs` (test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_log_markers() {
        assert!(matches!(
            classify_log_line("Loading weights: 42%"),
            Some(LifecycleEvent::Loading { progress: Some(p), .. }) if (p - 0.42).abs() < 1e-3
        ));
        assert!(matches!(
            classify_log_line("INFO: Application startup complete."),
            Some(LifecycleEvent::Ready)
        ));
        assert!(matches!(
            classify_log_line("Traceback (most recent call last):"),
            Some(LifecycleEvent::Error(_))
        ));
        assert!(matches!(
            classify_log_line(r#"172.17.0.1 - "POST /v1/images/generations HTTP/1.1" 200"#),
            Some(LifecycleEvent::Serving)
        ));
        // Health-check spam and unknown lines are ignored.
        assert!(classify_log_line(r#""GET /health HTTP/1.0" 405 Method Not Allowed"#).is_none());
        assert!(classify_log_line("some unrelated chatter").is_none());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui inference_server::logs`
Expected: FAIL — `classify_log_line`/`LifecycleEvent` not found.

- [ ] **Step 3: Implement the types + classifier**

Prepend to `logs.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Turn inference-server log lines into lifecycle events, and fold events (plus
//! the readiness probe) into a `LifecycleState`. Pure classifier + reducer;
//! the log *source* lives alongside as a trait (see `LogSource`).

use std::time::{Duration, Instant};

/// Coarse lifecycle phase surfaced on the Insights card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Loading,
    Ready,
    Serving,
    Error,
}

/// A single classified log observation. Trail: add variants (e.g.
/// `Tokens { per_sec }`, `DiffusionStep { i, n }`) for richer telemetry later.
#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleEvent {
    Loading { progress: Option<f32>, detail: String },
    Ready,
    Serving,
    Error(String),
}

/// Placeholder for future serving telemetry (tokens/sec, latency, …). Always
/// `None` on `LifecycleState` in v1; the field + type reserve the card slot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServingMetrics {}

/// Best-effort, version-tolerant. Returns `None` for lines we don't recognize
/// (state then holds). Case-insensitive; ordered most- to least-specific.
pub fn classify_log_line(line: &str) -> Option<LifecycleEvent> {
    let l = line.to_lowercase();

    // Errors first.
    if l.contains("traceback (most recent call last)")
        || l.contains("failed to load")
        || l.contains("fatal")
        || l.contains(" error:")
    {
        return Some(LifecycleEvent::Error(line.trim().to_string()));
    }
    // Readiness markers (uvicorn/vLLM/tt-metal).
    if l.contains("application startup complete")
        || l.contains("uvicorn running on")
        || l.contains("model ready")
        || l.contains("compilation complete")
    {
        return Some(LifecycleEvent::Ready);
    }
    // Serving: a successful inference request (not a health poll).
    if (l.contains("/v1/") || l.contains("/generate"))
        && !l.contains("/v1/models")
        && (l.contains(" 200") || l.contains(" 201"))
    {
        return Some(LifecycleEvent::Serving);
    }
    // Loading, optionally with a percentage.
    if l.contains("loading") || l.contains("downloading") || l.contains("compiling") {
        let progress = parse_percent(&l);
        return Some(LifecycleEvent::Loading {
            progress,
            detail: line.trim().to_string(),
        });
    }
    None
}

/// Extract the first `NN%` as a 0..1 fraction, if present.
fn parse_percent(l: &str) -> Option<f32> {
    let idx = l.find('%')?;
    let start = l[..idx].rfind(|c: char| !c.is_ascii_digit() && c != '.').map_or(0, |p| p + 1);
    l[start..idx].parse::<f32>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0))
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test --locked --lib --features tui inference_server::logs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workload/inference_server/logs.rs
git commit -m "feat(workload): inference-server log-line classifier + lifecycle types"
```

---

### Task 4: LifecycleState + reducer (precedence + staleness)

**Files:**
- Modify: `src/workload/inference_server/logs.rs`

**Interfaces:**
- Consumes: `Phase`, `LifecycleEvent`, `ServingMetrics`.
- Produces: `LifecycleState { phase, progress, detail, metrics, observed_at }`, `LifecycleState::starting()`, `LifecycleState::fold(&mut self, events: &[LifecycleEvent], probe_ok: bool, now: Instant)`, `LifecycleState::is_stale(now, max_age) -> bool`.

- [ ] **Step 1: Write the failing reducer test**

Add to `logs.rs` tests:

```rust
    #[test]
    fn fold_precedence_and_staleness() {
        let t0 = Instant::now();
        let mut st = LifecycleState::starting(t0);
        assert_eq!(st.phase, Phase::Starting);

        // Probe up (reachable) with no log events ⇒ still not Ready (probe only
        // proves "reachable"); phase advances to Loading at most.
        st.fold(&[], true, t0);
        assert_eq!(st.phase, Phase::Loading);

        // A Ready log event wins over probe/loading.
        st.fold(&[LifecycleEvent::Ready], true, t0);
        assert_eq!(st.phase, Phase::Ready);

        // An Error event beats everything in the same batch.
        st.fold(&[LifecycleEvent::Ready, LifecycleEvent::Error("boom".into())], true, t0);
        assert_eq!(st.phase, Phase::Error);

        // Staleness: older than 2× the poll cadence.
        assert!(!st.is_stale(t0, Duration::from_secs(12)));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui inference_server::logs::tests::fold_precedence`
Expected: FAIL — `LifecycleState` not found.

- [ ] **Step 3: Implement `LifecycleState`**

Add to `logs.rs` (above the test module):

```rust
/// The reduced lifecycle state surfaced on the card. `metrics` is always `None`
/// in v1 (reserved for the serving-telemetry trail).
#[derive(Debug, Clone)]
pub struct LifecycleState {
    pub phase: Phase,
    pub progress: Option<f32>,
    pub detail: String,
    pub metrics: Option<ServingMetrics>,
    pub observed_at: Instant,
}

impl LifecycleState {
    /// Initial state for a freshly-detected server (container present, nothing
    /// observed yet).
    pub fn starting(now: Instant) -> Self {
        LifecycleState {
            phase: Phase::Starting,
            progress: None,
            detail: String::new(),
            metrics: None,
            observed_at: now,
        }
    }

    /// Fold a batch of log events plus the readiness-probe result into the state.
    /// Precedence: log Error > log Ready/Serving > probe-reachable (⇒ Loading)
    /// > current phase. `now` timestamps the observation.
    pub fn fold(&mut self, events: &[LifecycleEvent], probe_ok: bool, now: Instant) {
        self.observed_at = now;

        // Highest-precedence event in the batch wins.
        if let Some(err) = events.iter().find_map(|e| match e {
            LifecycleEvent::Error(d) => Some(d.clone()),
            _ => None,
        }) {
            self.phase = Phase::Error;
            self.detail = err;
            return;
        }
        if events.iter().any(|e| matches!(e, LifecycleEvent::Serving)) {
            self.phase = Phase::Serving;
            return;
        }
        if events.iter().any(|e| matches!(e, LifecycleEvent::Ready)) {
            self.phase = Phase::Ready;
            self.progress = None;
            return;
        }
        if let Some((prog, detail)) = events.iter().rev().find_map(|e| match e {
            LifecycleEvent::Loading { progress, detail } => Some((*progress, detail.clone())),
            _ => None,
        }) {
            self.phase = Phase::Loading;
            self.progress = prog;
            self.detail = detail;
            return;
        }
        // No log events this batch: the probe being reachable means at least
        // "up/loading" — but never asserts Ready (that's a log-only signal).
        if probe_ok && matches!(self.phase, Phase::Starting) {
            self.phase = Phase::Loading;
        }
    }

    /// True if this state hasn't been refreshed within `max_age` (so a dead
    /// server doesn't read "Ready" forever).
    pub fn is_stale(&self, now: Instant, max_age: Duration) -> bool {
        now.duration_since(self.observed_at) > max_age
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test --locked --lib --features tui inference_server::logs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workload/inference_server/logs.rs
git commit -m "feat(workload): lifecycle state reducer (log Error > Ready/Serving > probe > phase)"
```

---

### Task 5: LogSource trait + DockerLogSource

**Files:**
- Modify: `src/workload/inference_server/logs.rs`

**Interfaces:**
- Consumes: `Source`.
- Produces: `trait LogSource { fn poll_lines(&mut self) -> Vec<String>; }`, `DockerLogSource::new(container: String) -> Self`, and a pure helper `docker_logs_args(container, since_secs) -> Vec<String>`.

- [ ] **Step 1: Write the failing test for the pure arg builder**

Add to `logs.rs` tests:

```rust
    #[test]
    fn docker_logs_args_are_bounded_and_since_scoped() {
        let args = docker_logs_args("tt-inference-server-5edd00ce", 6);
        assert_eq!(args[0], "logs");
        assert!(args.iter().any(|a| a == "--since"));
        assert!(args.iter().any(|a| a == "6s"));
        assert!(args.iter().any(|a| a == "tt-inference-server-5edd00ce"));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui inference_server::logs::tests::docker_logs_args`
Expected: FAIL — `docker_logs_args` not found.

- [ ] **Step 3: Implement the trait + Docker source**

Add to `logs.rs`:

```rust
use std::process::Command;

/// A source of recent log lines for one server. Trail: `FileLogSource` /
/// `JournaldLogSource` implement this for non-Docker installs.
pub trait LogSource: Send {
    /// Return log lines produced since the previous call (best-effort; empty on
    /// error). Must not block longer than a couple of seconds.
    fn poll_lines(&mut self) -> Vec<String>;
}

/// Build `docker logs --since <N>s <container>` args. Pure, unit-tested.
pub fn docker_logs_args(container: &str, since_secs: u64) -> Vec<String> {
    vec![
        "logs".to_string(),
        "--since".to_string(),
        format!("{since_secs}s"),
        container.to_string(),
    ]
}

/// Polls `docker logs --since` for one container. Each poll reads only lines
/// since the previous poll's window (`since_secs`). Never panics; a missing
/// docker / stopped container yields an empty batch.
pub struct DockerLogSource {
    container: String,
    since_secs: u64,
}

impl DockerLogSource {
    pub fn new(container: String, since_secs: u64) -> Self {
        DockerLogSource { container, since_secs }
    }
}

impl LogSource for DockerLogSource {
    fn poll_lines(&mut self) -> Vec<String> {
        let out = match Command::new("docker")
            .args(docker_logs_args(&self.container, self.since_secs))
            .output()
        {
            Ok(o) => o,
            Err(_) => return Vec::new(),
        };
        // docker writes logs to both stdout and stderr; classify both.
        let mut lines = Vec::new();
        for stream in [&out.stdout, &out.stderr] {
            for line in String::from_utf8_lossy(stream).lines() {
                lines.push(line.to_string());
            }
        }
        lines
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test --locked --lib --features tui inference_server::logs`
Expected: PASS. Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/workload/inference_server/logs.rs
git commit -m "feat(workload): LogSource trait + DockerLogSource (polled docker logs --since)"
```

---

### Task 6: InferenceServerMonitor (background thread + arc-swap)

**Files:**
- Create: `src/workload/inference_server/monitor.rs`

**Interfaces:**
- Consumes: `InferenceServer`, `Source`, `LifecycleState`, `LifecycleEvent`, `classify_log_line`, `DockerLogSource`, `LogSource`, and `crate::workload::liveness_probe` for the readiness probe.
- Produces: `InferenceServerMonitor::spawn() -> Self`, `submit(&self, servers: Vec<InferenceServer>)`, `snapshot(&self) -> Vec<(InferenceServer, LifecycleState)>`.

- [ ] **Step 1: Write the failing test for the pure fold-over-source path**

`src/workload/inference_server/monitor.rs` (test module) — tests the reduction logic with a fake source, no real docker:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::logs::{LifecycleEvent, LogSource, Phase};
    use std::time::Instant;

    struct FakeSource(Vec<String>);
    impl LogSource for FakeSource {
        fn poll_lines(&mut self) -> Vec<String> {
            std::mem::take(&mut self.0)
        }
    }

    #[test]
    fn reduce_from_source_marks_ready_on_startup_complete() {
        let mut src = FakeSource(vec![
            "Loading weights: 10%".into(),
            "INFO: Application startup complete.".into(),
        ]);
        let mut state = LifecycleState::starting(Instant::now());
        reduce_from_source(&mut state, &mut src, true, Instant::now());
        assert_eq!(state.phase, Phase::Ready);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui inference_server::monitor`
Expected: FAIL — module/`reduce_from_source` not found.

- [ ] **Step 3: Implement `monitor.rs`**

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Background monitor: given the inference servers detected each refresh, polls
//! their logs + readiness probe on a slow cadence and publishes a lock-free
//! snapshot of `(InferenceServer, LifecycleState)` for the render path.

use super::detect::{InferenceServer, Source};
use super::logs::{classify_log_line, DockerLogSource, LifecycleState, LogSource};
use crate::workload::liveness_probe;
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Poll cadence (matches LivenessProber::PROBE_INTERVAL).
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// A state not refreshed within this window is dropped from the snapshot.
const MAX_AGE: Duration = Duration::from_secs(12);

type Snapshot = Vec<(InferenceServer, LifecycleState)>;

/// Pure step: pull a source's lines, classify them, fold into `state`. Extracted
/// so the reduction is testable with a fake source (no docker).
pub(crate) fn reduce_from_source(
    state: &mut LifecycleState,
    source: &mut dyn LogSource,
    probe_ok: bool,
    now: Instant,
) {
    let events: Vec<_> = source
        .poll_lines()
        .iter()
        .filter_map(|l| classify_log_line(l))
        .collect();
    state.fold(&events, probe_ok, now);
}

pub struct InferenceServerMonitor {
    cache: Arc<ArcSwap<Snapshot>>,
    tx: Sender<Vec<InferenceServer>>,
}

impl InferenceServerMonitor {
    pub fn spawn() -> Self {
        let cache: Arc<ArcSwap<Snapshot>> = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let (tx, rx) = mpsc::channel::<Vec<InferenceServer>>();
        let cache_bg = Arc::clone(&cache);

        let _ = std::thread::Builder::new()
            .name("tt-inference-monitor".into())
            .spawn(move || {
                let mut servers: Vec<InferenceServer> = Vec::new();
                let mut states: HashMap<String, LifecycleState> = HashMap::new();
                let mut sources: HashMap<String, DockerLogSource> = HashMap::new();
                loop {
                    match rx.recv_timeout(POLL_INTERVAL) {
                        Ok(mut s) => {
                            while let Ok(newer) = rx.try_recv() {
                                s = newer;
                            }
                            servers = s;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }

                    let now = Instant::now();
                    let mut snapshot: Snapshot = Vec::new();
                    let mut seen = Vec::new();
                    for srv in &servers {
                        let Source::Docker { container } = &srv.source;
                        let key = container.clone();
                        seen.push(key.clone());
                        let src = sources
                            .entry(key.clone())
                            .or_insert_with(|| DockerLogSource::new(container.clone(), 6));
                        let state = states
                            .entry(key.clone())
                            .or_insert_with(|| LifecycleState::starting(now));
                        let probe_ok = srv
                            .port
                            .map(|p| liveness_probe::probe_reachable("tt-inference-server", p))
                            .unwrap_or(false);
                        reduce_from_source(state, src, probe_ok, now);
                        snapshot.push((srv.clone(), state.clone()));
                    }
                    // Drop states/sources for servers that disappeared.
                    states.retain(|k, st| seen.contains(k) && !st.is_stale(now, MAX_AGE));
                    sources.retain(|k, _| seen.contains(k));
                    cache_bg.store(Arc::new(snapshot));
                }
            });

        InferenceServerMonitor { cache, tx }
    }

    /// Hand the monitor the servers detected this refresh (non-blocking).
    pub fn submit(&self, servers: Vec<InferenceServer>) {
        let _ = self.tx.send(servers);
    }

    /// Lock-free read of the latest `(server, state)` snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.cache.load().as_ref().clone()
    }
}
```

- [ ] **Step 4: Add the probe helper to `liveness_probe.rs`**

`InferenceServerMonitor` needs a one-shot readiness check. Expose a thin wrapper over the existing `probe_once` (Task 2's `/v1/models` spec):

```rust
/// One-shot readiness probe for `label` on `port` (localhost). `true` only on a
/// clean positive; errors/timeouts ⇒ `false`. Used by InferenceServerMonitor.
pub fn probe_reachable(label: &str, port: u16) -> bool {
    probe_once(label, port).unwrap_or(false)
}
```

(If `probe_once` is private, mark it `pub(crate)`; add `probe_reachable` next to it.)

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test --locked --lib --features tui inference_server`
Expected: PASS (detect + logs + monitor). Clippy `-D warnings` clean. Confirm the `mod.rs` `pub use` now resolves all names.

- [ ] **Step 6: Commit**

```bash
git add src/workload/inference_server/monitor.rs src/workload/liveness_probe.rs
git commit -m "feat(workload): InferenceServerMonitor — background log+probe fold, arc-swap snapshot"
```

---

### Task 7: Detect servers from the process snapshot + feed the monitor

**Files:**
- Modify: `src/workload/host_processes.rs` (add `detected_inference_servers`)
- Modify: `src/ui/tui/mod.rs` (spawn monitor, submit each refresh)

**Interfaces:**
- Consumes: `parse_inference_server`, `InferenceServer`, `InferenceServerMonitor`.
- Produces: `HostProcessMonitor::detected_inference_servers(&self) -> Vec<InferenceServer>`.

- [ ] **Step 1: Write the failing test**

Add to `host_processes.rs` tests (it already has a tests module):

```rust
    #[test]
    fn detected_inference_servers_is_gated_and_returns_vec() {
        // Smoke: on a machine with no TT inference container this is empty, but
        // the call must exist and not panic.
        let mut m = HostProcessMonitor::new();
        m.update();
        #[cfg(target_os = "linux")]
        {
            let _servers = m.detected_inference_servers();
        }
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui host_processes::tests::detected_inference_servers`
Expected: FAIL — method not found.

- [ ] **Step 3: Implement `detected_inference_servers`**

Add to `impl HostProcessMonitor` (Linux-gated):

```rust
    /// TT inference servers detected in the current snapshot (deduped by
    /// container). Cheap: cmdline parse only, no I/O. Linux-only.
    #[cfg(target_os = "linux")]
    pub fn detected_inference_servers(&self) -> Vec<crate::workload::InferenceServer> {
        use crate::workload::inference_server::parse_inference_server;
        use crate::workload::inference_server::Source;
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
            if let Some(srv) = parse_inference_server(&name, &cmdline) {
                let Source::Docker { container } = &srv.source;
                if seen.insert(container.clone()) {
                    out.push(srv);
                }
            }
        }
        out
    }
```

- [ ] **Step 4: Run test to verify pass**

Run: `cargo test --locked --lib --features tui host_processes`
Expected: PASS.

- [ ] **Step 5: Spawn + feed the monitor in the TUI loop**

In `src/ui/tui/mod.rs`, next to the `LivenessProber` setup (init block ~line 249), Linux-gated:

```rust
    #[cfg(target_os = "linux")]
    let inference_monitor = crate::workload::InferenceServerMonitor::spawn();
    #[cfg(target_os = "linux")]
    inference_monitor.submit(host_proc_monitor.detected_inference_servers());
```

In the 2s refresh block (next to the existing `liveness_prober.submit(...)`):

```rust
            #[cfg(target_os = "linux")]
            inference_monitor.submit(host_proc_monitor.detected_inference_servers());
```

- [ ] **Step 6: Build + commit**

Run: `cargo build --locked --bin tt-toplike-tui --features tui` then `RUSTFLAGS="-D warnings" cargo build --locked --bin tt-toplike-tui --features tui`
Expected: clean.

```bash
git add src/workload/host_processes.rs src/ui/tui/mod.rs
git commit -m "feat(workload): detect inference servers from snapshot; feed the monitor from the TUI loop"
```

---

### Task 8: Render the "Inference Servers" card in Insights

**Files:**
- Modify: `src/ui/tui/mod.rs` (read `inference_monitor.snapshot()`, render a card in the Insights layout)

**Interfaces:**
- Consumes: `InferenceServerMonitor::snapshot()`, `LifecycleState`, `Phase`.
- Produces: a rendered card; a pure helper `format_inference_row(server, state) -> String` (unit-tested).

- [ ] **Step 1: Write the failing formatter test**

Add a small test module near the Insights render code (or in a new `src/ui/tui/inference_card.rs` with `mod inference_card;` in `mod.rs`):

```rust
#[cfg(test)]
mod inference_card_tests {
    use super::*;
    use crate::workload::{InferenceServer, LifecycleState, Phase, Source};
    use std::time::Instant;

    #[test]
    fn formats_loading_with_model_and_mesh() {
        let srv = InferenceServer {
            source: Source::Docker { container: "tt-inference-server-x".into() },
            image: "…/tt-media-inference-server:0.17.0".into(),
            model: Some("FLUX.1-schnell".into()),
            mesh: Some("P300x2".into()),
            arch: Some("blackhole".into()),
            device: Some("p300x2".into()),
            port: Some(8000),
            uses_tt_device: true,
        };
        let mut st = LifecycleState::starting(Instant::now());
        st.phase = Phase::Loading;
        st.progress = Some(0.42);
        let row = format_inference_row(&srv, &st);
        assert!(row.contains("FLUX.1-schnell"));
        assert!(row.contains("P300x2"));
        assert!(row.contains("LOADING"));
        assert!(row.contains("42%"));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --locked --lib --features tui inference_card`
Expected: FAIL — `format_inference_row` not found.

- [ ] **Step 3: Implement the formatter**

```rust
/// One-line summary for the Inference Servers card. Pure + unit-tested.
pub(crate) fn format_inference_row(
    server: &crate::workload::InferenceServer,
    state: &crate::workload::LifecycleState,
) -> String {
    use crate::workload::Phase;
    let phase = match state.phase {
        Phase::Starting => "STARTING",
        Phase::Loading => "LOADING",
        Phase::Ready => "READY",
        Phase::Serving => "SERVING",
        Phase::Error => "ERROR",
    };
    let model = server.model.as_deref().unwrap_or("?");
    let mesh = server.mesh.as_deref().unwrap_or("?");
    let pct = match state.progress {
        Some(p) => format!(" {}%", (p * 100.0).round() as u32),
        None => String::new(),
    };
    format!("{model}  [{mesh}]  {phase}{pct}")
}
```

- [ ] **Step 4: Render the card in the Insights layout**

Where the Insights screen composes panels, add a block (Linux-gated) that reads the snapshot and renders one styled line per server (green=Ready/Serving, yellow=Loading/Starting, red=Error), using `format_inference_row`. Follow the existing panel-rendering style in the file (Paragraph/Line/Span with `colors`). Keep it above or beside the process list.

```rust
    #[cfg(target_os = "linux")]
    let inference_rows: Vec<(crate::workload::InferenceServer, crate::workload::LifecycleState)> =
        inference_monitor.snapshot();
    // …in the Insights render fn, iterate inference_rows and push a titled block
    // "Inference Servers" with one colored line per row via format_inference_row.
```

- [ ] **Step 5: Run tests + build**

Run: `cargo test --locked --lib --features tui inference_card` (PASS), then `cargo test --locked --lib --features tui` (all pass), `RUSTFLAGS="-D warnings" cargo build --locked --bin tt-toplike-tui --features tui` (clean), `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings` (clean).

- [ ] **Step 6: Manual check on the live box**

Run `tt-toplike` while the tt-inference-server container is up; confirm the "Inference Servers" card shows the model/mesh and a LOADING→READY transition.

- [ ] **Step 7: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(tui): Inference Servers card — model/mesh + lifecycle state in Insights"
```

---

## Self-Review

**Spec coverage:**
- Detection from cmdline (model/mesh/arch/port/device/tt-device) → Task 1. ✓
- Probe fix (`/v1/models`) → Task 2. ✓
- Log classifier + lifecycle types → Task 3. ✓
- State reducer (precedence + staleness) → Task 4. ✓
- LogSource trait + DockerLogSource (polled `--since`) → Task 5. ✓
- Background monitor, off hot path, arc-swap → Task 6. ✓
- Detect-from-snapshot + TUI feed → Task 7. ✓
- Insights "Inference Servers" card → Task 8. ✓
- Extension trails (Source enum, LogSource trait, variant recognizer, LifecycleEvent + metrics=None) → present in Tasks 1,3,4,5. ✓
- Linux/TT gating + no new deps + off-hot-path + no panics → Global Constraints, enforced per task. ✓

**Placeholder scan:** Task 8 Step 4 describes the card wiring at the render site rather than pasting the file's exact panel code (the Insights render fn is large and its exact insertion point depends on the current layout); the pure `format_inference_row` it depends on is fully specified and tested. All other steps contain complete code.

**Type consistency:** `InferenceServer`, `Source::Docker { container }`, `LifecycleState { phase, progress, detail, metrics, observed_at }`, `Phase`, `LifecycleEvent`, `classify_log_line`, `LogSource::poll_lines`, `docker_logs_args`, `reduce_from_source`, `InferenceServerMonitor::{spawn,submit,snapshot}`, `liveness_probe::probe_reachable` — names/signatures consistent across Tasks 1–8.
