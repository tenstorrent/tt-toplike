# TT Inference-Server Monitoring Implementation Plan (v2 — resource-probe model)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A toggle-able Insights `[i]` panel showing each inference service's phase (compiling / loading / ready / hung) with concrete progress evidence, so an operator can tell "working" from "stuck" during long first-run kernel compiles + weight loads.

**Architecture:** A Linux-gated background `InferenceServerMonitor` shells out to `docker stats`/`docker exec`/`docker inspect` and the per-service liveness HTTP endpoint on a ~5s cadence, folds the signals into per-service `ServiceState` (phase + progress deltas + hang timer) via pure functions, and publishes a snapshot via `arc-swap` that the render path reads lock-free.

**Tech Stack:** Rust; `std::process::Command` (docker), the existing `std::net` HTTP helper in `liveness_probe`, `arc-swap`, `std::thread`/`mpsc`. No new deps.

## Global Constraints

- **Linux/TT only** (`#[cfg(target_os = "linux")]`); no-op elsewhere.
- **No new dependencies.**
- **Off the hot path:** all `docker`/HTTP I/O on the background thread; render reads `arc-swap` only.
- **No panics** on external output (docker/HTTP text): parse with `Option`/`Result`, never index/unwrap untrusted data.
- **Discover, don't hardcode:** the kernel-cache dir is `$TT_METAL_HOME/built`, with `TT_METAL_HOME` read from the container env at runtime. Never hardcode a container path.
- **SPDX header** on every new file (`Apache-2.0` / `2026 Tenstorrent USA, Inc.`).
- **Verify each task:** `cargo test --locked --lib --features tui inference_server` and `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings`.

## Context: what already exists on the branch (v1, partially superseded)

- `inference_server/detect.rs` — `parse_inference_server` (cmdline → `InferenceServer{source,image,model,mesh,arch,device,port,uses_tt_device}`). **KEEP + reuse.**
- `inference_server/logs.rs` — `Phase`(Starting/Loading/Ready/Serving/Error), `LifecycleEvent`, `LifecycleState`, `classify_log_line`, `ServingMetrics`. **Task A retires the lifecycle bits** (v2's `Phase` is different and lives in `state.rs`); only a log breadcrumb survives.
- `liveness_probe.rs` — `probe_spec("tt-inference-server")` now `/v1/models` (Task 2). Leave as-is; that module is the *generic runtime* liveness feature. v2's per-service probing is separate, in `probe.rs`.

## File Structure (v2)

- `inference_server/services.rs` (new) — `ServiceDef` + the `SERVERS` table (mirrors tt-local-generator) + `service_for(model, image) -> Option<&'static ServiceDef>`.
- `inference_server/probe.rs` (new) — pure parsers (`parse_docker_stats`, `parse_liveness`, `parse_env_var`, `count_lines`, `top_process`) + thin docker/HTTP shells behind a `ContainerProbe` trait.
- `inference_server/state.rs` (new) — `Phase`, `ModelProfile`, `ServiceState`, `Phase::derive`, alarm-timer reducer, `estimate_progress`.
- `inference_server/monitor.rs` (new) — `InferenceServerMonitor` (bg thread + `arc-swap` snapshot).
- `inference_server/logs.rs` (modify) — retire lifecycle; keep `last_non_health_line`.
- `inference_server/mod.rs` (modify) — module decls + re-exports.
- `src/ui/tui/mod.rs` (modify) — `i` toggle + panel render + `format_service_row` (pure).

---

### Task A: Retire the log-lifecycle bits; keep a breadcrumb

**Files:** Modify `src/workload/inference_server/logs.rs`, `src/workload/inference_server/mod.rs`.

**Interfaces:** Produces `last_non_health_line(lines: &[String]) -> Option<String>`. Removes `Phase`, `LifecycleEvent`, `LifecycleState` (none created yet beyond Task 3's `Phase`/`LifecycleEvent`/`ServingMetrics`), `classify_log_line`.

- [ ] **Step 1: Write the failing test** in `logs.rs` (replace the old test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn breadcrumb_skips_health_poll_spam() {
        let lines = vec![
            "INFO: compiling kernels for z-image".to_string(),
            r#"172.17.0.1 - "GET /tt-liveness HTTP/1.1" 405 Method Not Allowed"#.to_string(),
            r#"172.17.0.1 - "GET /health HTTP/1.0" 405"#.to_string(),
        ];
        assert_eq!(last_non_health_line(&lines).as_deref(), Some("INFO: compiling kernels for z-image"));
        assert_eq!(last_non_health_line(&[]), None);
    }
}
```

- [ ] **Step 2: Run it — fails** (`last_non_health_line` undefined, old items still present).

Run: `cargo test --locked --lib --features tui inference_server::logs`

- [ ] **Step 3: Replace `logs.rs` body** (keep SPDX header) with just the breadcrumb:

```rust
//! Log breadcrumb for the inference-server panel. v2 derives lifecycle from
//! resource/process probes (see state.rs), not log parsing — the server log is
//! silent during the long load phase. The only log use is surfacing the last
//! human-meaningful line, filtering out liveness/health-poll spam.

/// Last log line that isn't a health/liveness poll, if any.
pub fn last_non_health_line(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .rev()
        .find(|l| {
            let low = l.to_lowercase();
            !(low.contains("/tt-liveness") || low.contains("/health") || low.contains("/v1/models"))
        })
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}
```

- [ ] **Step 4: Update `inference_server/mod.rs`** — replace the `logs` re-export line:

```rust
pub use logs::last_non_health_line;
```

Remove any `pub use logs::{classify_log_line, LifecycleEvent, Phase, ServingMetrics};`. (Later tasks add `services`, `probe`, `state`, `monitor` mod lines + re-exports.)

- [ ] **Step 5: Run tests + clippy + build** → green; `cargo build --locked --bin tt-toplike-tui --features tui` clean.

- [ ] **Step 6: Commit**

```bash
git add src/workload/inference_server/logs.rs src/workload/inference_server/mod.rs
git commit -m "refactor(workload): retire log-lifecycle; keep last_non_health_line breadcrumb (v2 pivot)"
```

---

### Task B: Services registry

**Files:** Create `src/workload/inference_server/services.rs`; modify `inference_server/mod.rs` (`mod services; pub use services::{ServiceDef, service_for, SERVERS};`).

**Interfaces:** Produces `ServiceDef { key, label, port, health_path, runner_key: Option<&'static str> }`, `SERVERS: &[ServiceDef]`, `service_for(model: Option<&str>, image: &str) -> Option<&'static ServiceDef>`.

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_model_to_service_and_carries_endpoint() {
        let s = service_for(Some("Z-Image-Turbo"), "ghcr.io/tenstorrent/tt-media-inference-server:x").expect("known");
        assert_eq!(s.key, "z-image-turbo");
        assert_eq!(s.port, 8000);
        assert_eq!(s.health_path, "/tt-liveness");
        assert_eq!(s.runner_key, Some("tt-z-image-turbo"));
        // prompt-server uses a different port + path
        let p = SERVERS.iter().find(|d| d.key == "prompt-server").unwrap();
        assert_eq!((p.port, p.health_path), (8001, "/health"));
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `services.rs`** (SPDX header + table mirroring `tt-local-generator/app/server_manager.py`):

```rust
/// One managed inference service (mirrors tt-local-generator's SERVERS dict).
#[derive(Debug, Clone, Copy)]
pub struct ServiceDef {
    pub key: &'static str,
    pub label: &'static str,
    pub port: u16,
    pub health_path: &'static str,
    /// `runner_in_use` value reported by /tt-liveness when this model is loaded.
    pub runner_key: Option<&'static str>,
}

/// Known services. Trail: later read from a shared config so the two tools can't drift.
pub const SERVERS: &[ServiceDef] = &[
    ServiceDef { key: "wan2.2", label: "Wan2.2-T2V-A14B (P300X2)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-wan2.2") },
    ServiceDef { key: "mochi", label: "Mochi-1", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-mochi-1") },
    ServiceDef { key: "flux", label: "FLUX.1-schnell", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-flux.1-schnell") },
    ServiceDef { key: "sdxl", label: "SDXL (cpp_server)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-sdxl-generate") },
    ServiceDef { key: "z-image-turbo", label: "Z-Image-Turbo (P150X4)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-z-image-turbo") },
    ServiceDef { key: "motif", label: "Motif-Image-6B-Preview (P300X2)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-motif-image-6b-preview") },
    ServiceDef { key: "animate", label: "Wan2.2-Animate-14B", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-wan2.2-animate") },
    ServiceDef { key: "skyreels", label: "SkyReels-V2-I2V-14B-540P (Blackhole)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-skyreels-v2-i2v") },
    ServiceDef { key: "prompt-server", label: "Prompt Generator (Qwen3-0.6B)", port: 8001, health_path: "/health", runner_key: None },
];

/// Map a detected container's MODEL env (preferred) or image to a service.
/// Case-insensitive contains match on the model against each key's tokens.
pub fn service_for(model: Option<&str>, _image: &str) -> Option<&'static ServiceDef> {
    let m = model?.to_lowercase();
    SERVERS.iter().find(|d| {
        // e.g. model "Z-Image-Turbo" → key "z-image-turbo"
        m.replace(['.', '_'], "-").contains(d.key) || d.key.contains(&m.replace(['.', '_'], "-"))
    })
}
```

- [ ] **Step 4: tests + clippy + build green. Commit**

```bash
git add src/workload/inference_server/services.rs src/workload/inference_server/mod.rs
git commit -m "feat(workload): inference-server services registry (mirrors tt-local-generator SERVERS)"
```

---

### Task C: Pure probe parsers

**Files:** Create `src/workload/inference_server/probe.rs`; modify `mod.rs`.

**Interfaces:** Produces `parse_docker_stats(&str) -> Option<(f32, u64)>` (cpu%, rss bytes), `Readiness` (`Down`/`NotReady`/`Ready{runner: Option<String>}`), `parse_liveness(status: u16, body: &str) -> Readiness`, `parse_env_var(env_output: &str, key: &str) -> Option<String>`, `count_lines(&str) -> usize`, `top_process(ps_output: &str) -> Option<(String, f32, u64)>`.

- [ ] **Step 1: Failing tests** (fixtures captured live):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_docker_stats_line() {
        // `docker stats --no-stream --format '{{.CPUPerc}}|{{.MemUsage}}'`
        let (cpu, rss) = parse_docker_stats("102.17%|39.96GiB / 249.3GiB").unwrap();
        assert!((cpu - 102.17).abs() < 0.01);
        assert_eq!(rss, (39.96 * 1024.0 * 1024.0 * 1024.0) as u64);
    }
    #[test]
    fn liveness_ladder() {
        assert!(matches!(parse_liveness(0, ""), Readiness::Down));
        assert!(matches!(parse_liveness(405, r#"{"detail":"Model is not ready"}"#), Readiness::NotReady));
        match parse_liveness(200, r#"{"runner_in_use":"tt-z-image-turbo"}"#) {
            Readiness::Ready { runner } => assert_eq!(runner.as_deref(), Some("tt-z-image-turbo")),
            _ => panic!("expected Ready"),
        }
    }
    #[test]
    fn parses_env_and_counts() {
        assert_eq!(parse_env_var("HOME=/root\nTT_METAL_HOME=/x/tt-metal\n", "TT_METAL_HOME").as_deref(), Some("/x/tt-metal"));
        assert_eq!(count_lines("a\nb\nc\n"), 3);
        assert_eq!(count_lines(""), 0);
    }
    #[test]
    fn top_process_from_ps() {
        // `ps -eo pcpu,rss,comm --sort=-pcpu` (rss in KiB)
        let out = "%CPU   RSS COMMAND\n33.7 9043136 python3\n 5.2 815612 python3\n";
        let (name, cpu, rss) = top_process(out).unwrap();
        assert_eq!(name, "python3");
        assert!((cpu - 33.7).abs() < 0.01);
        assert_eq!(rss, 9043136 * 1024);
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `probe.rs`** (SPDX header). Parsers first:

```rust
/// Readiness ladder from a liveness probe.
#[derive(Debug, Clone, PartialEq)]
pub enum Readiness {
    Down,                              // connection refused / no response
    NotReady,                          // up but model not loaded (e.g. 405 "Model is not ready")
    Ready { runner: Option<String> },  // 200; runner = runner_in_use if present
}

/// Parse `{{.CPUPerc}}|{{.MemUsage}}` → (cpu%, rss bytes). MemUsage is "USED / LIMIT".
pub fn parse_docker_stats(line: &str) -> Option<(f32, u64)> {
    let (cpu_s, mem_s) = line.split_once('|')?;
    let cpu = cpu_s.trim().trim_end_matches('%').parse::<f32>().ok()?;
    let used = mem_s.split('/').next()?.trim();
    Some((cpu, parse_size_bytes(used)?))
}

/// "39.96GiB" / "812916KiB" / "700MiB" → bytes.
fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_alphabetic())?;
    let (num, unit) = s.split_at(split);
    let v = num.trim().parse::<f64>().ok()?;
    let mult = match unit.trim().to_lowercase().as_str() {
        "b" => 1.0,
        "kib" | "kb" => 1024.0,
        "mib" | "mb" => 1024.0 * 1024.0,
        "gib" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "tib" | "tb" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((v * mult) as u64)
}

pub fn parse_liveness(status: u16, body: &str) -> Readiness {
    match status {
        0 => Readiness::Down,
        200 => {
            // pull runner_in_use if present (tolerant substring parse, no serde dep needed)
            let runner = body
                .split_once("\"runner_in_use\"")
                .and_then(|(_, rest)| rest.split_once(':'))
                .and_then(|(_, rest)| rest.split('"').nth(1))
                .map(|s| s.to_string());
            Readiness::Ready { runner }
        }
        _ => Readiness::NotReady, // 405 "Model is not ready", 503, etc.
    }
}

/// Extract `KEY=VALUE` from `env`/`printenv`-style output.
pub fn parse_env_var(env_output: &str, key: &str) -> Option<String> {
    env_output.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// Count non-empty lines (kernel-artifact count, FD count).
pub fn count_lines(s: &str) -> usize {
    s.lines().filter(|l| !l.trim().is_empty()).count()
}

/// First data row of `ps -eo pcpu,rss,comm --sort=-pcpu` → (comm, cpu%, rss bytes).
/// rss is KiB in ps output. Skips the header line.
pub fn top_process(ps_output: &str) -> Option<(String, f32, u64)> {
    for line in ps_output.lines().skip(1) {
        let mut it = line.split_whitespace();
        let cpu = it.next()?.parse::<f32>().ok()?;
        let rss_kib = it.next()?.parse::<u64>().ok()?;
        let comm = it.next()?.to_string();
        return Some((comm, cpu, rss_kib * 1024));
    }
    None
}
```

- [ ] **Step 4: tests + clippy + build green. Commit**

```bash
git add src/workload/inference_server/probe.rs src/workload/inference_server/mod.rs
git commit -m "feat(workload): pure probe parsers (docker stats, /tt-liveness, env, ps, counts)"
```

---

### Task D: Phase, ServiceState, reducer + progress estimate

**Files:** Create `src/workload/inference_server/state.rs`; modify `mod.rs`.

**Interfaces:** Consumes `Readiness`. Produces `Phase` (Down/Compiling/Loading/Ready/Alarm), `ModelProfile { o_baseline: Option<usize>, footprint_bytes: Option<u64> }`, `ServiceState { key, label, phase, cpu_pct, rss_bytes, rss_delta, kernel_count, kernel_delta, safetensors_fds, readiness, top_proc, last_log, progress, flat_ticks }`, `Phase::derive(kernel_delta: i64, python_alive: bool, rss_delta: i64, readiness: &Readiness) -> Phase`, `estimate_progress(phase: Phase, kernel_count: usize, rss: u64, profile: &ModelProfile) -> Option<f32>`, and an `Alarm` rule: `flat_ticks * cadence >= 5min` when not Ready.

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::probe::Readiness;
    #[test]
    fn phase_derivation() {
        assert_eq!(Phase::derive(0, false, 0, &Readiness::Down), Phase::Down);
        assert_eq!(Phase::derive(12, true, 0, &Readiness::NotReady), Phase::Compiling); // kernels growing
        assert_eq!(Phase::derive(0, true, 700_000_000, &Readiness::NotReady), Phase::Loading); // RSS growing
        assert_eq!(Phase::derive(0, true, 0, &Readiness::Ready { runner: None }), Phase::Ready);
    }
    #[test]
    fn all_flat_not_ready_is_alarm_after_5min() {
        // 5s cadence → 60 flat ticks = 5 min.
        assert!(is_alarm(60, 5, &Readiness::NotReady));
        assert!(!is_alarm(59, 5, &Readiness::NotReady));
        assert!(!is_alarm(120, 5, &Readiness::Ready { runner: None }));
    }
    #[test]
    fn progress_uses_profile_when_present() {
        let prof = ModelProfile { o_baseline: Some(1000), footprint_bytes: None };
        assert_eq!(estimate_progress(Phase::Compiling, 500, 0, &prof), Some(0.5));
        let none = ModelProfile { o_baseline: None, footprint_bytes: None };
        assert_eq!(estimate_progress(Phase::Compiling, 500, 0, &none), None);
    }
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `state.rs`** (SPDX header):

```rust
use crate::workload::inference_server::probe::Readiness;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { Down, Compiling, Loading, Ready, Alarm }

/// Per-model baselines for the optional % estimate. v1 hardcodes a few; unknown → all None.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelProfile {
    pub o_baseline: Option<usize>,
    pub footprint_bytes: Option<u64>,
}

/// Derive phase from the tick's deltas + readiness. `Alarm` is layered on top by
/// the reducer (needs the flat-tick history), so this returns the momentary phase.
impl Phase {
    pub fn derive(kernel_delta: i64, python_alive: bool, rss_delta: i64, readiness: &Readiness) -> Phase {
        if let Readiness::Ready { .. } = readiness { return Phase::Ready; }
        if let Readiness::Down = readiness {
            if !python_alive { return Phase::Down; }
        }
        if kernel_delta > 0 { return Phase::Compiling; }
        if rss_delta > 0 { return Phase::Loading; }
        // not ready, nothing moving, but process alive → provisional Loading;
        // the reducer escalates to Alarm after enough flat ticks.
        if python_alive { Phase::Loading } else { Phase::Down }
    }
}

/// True when nothing has progressed for >= 5 minutes and the service isn't Ready.
pub fn is_alarm(flat_ticks: u32, cadence_secs: u32, readiness: &Readiness) -> bool {
    !matches!(readiness, Readiness::Ready { .. }) && flat_ticks.saturating_mul(cadence_secs) >= 300
}

/// Best-effort % (compile: kernel_count/baseline; load: rss/footprint). None w/o profile.
pub fn estimate_progress(phase: Phase, kernel_count: usize, rss: u64, profile: &ModelProfile) -> Option<f32> {
    match phase {
        Phase::Compiling => profile.o_baseline.filter(|b| *b > 0).map(|b| (kernel_count as f32 / b as f32).clamp(0.0, 1.0)),
        Phase::Loading => profile.footprint_bytes.filter(|b| *b > 0).map(|b| (rss as f32 / b as f32).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// Full per-service state published to the render path.
#[derive(Debug, Clone)]
pub struct ServiceState {
    pub key: String,
    pub label: String,
    pub phase: Phase,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub rss_delta: i64,
    pub kernel_count: usize,
    pub kernel_delta: i64,
    pub safetensors_fds: usize,
    pub readiness: Readiness,
    pub top_proc: Option<String>,
    pub last_log: Option<String>,
    pub progress: Option<f32>,
    pub flat_ticks: u32,
}
```

- [ ] **Step 4: tests + clippy + build green. Commit**

```bash
git add src/workload/inference_server/state.rs src/workload/inference_server/mod.rs
git commit -m "feat(workload): Phase derivation, ServiceState, alarm timer, progress estimate"
```

---

### Task E: ContainerProbe trait + Docker impl + InferenceServerMonitor

**Files:** add the `ContainerProbe` trait + `DockerProbe` to `probe.rs`; create `src/workload/inference_server/monitor.rs`; modify `mod.rs`.

**Interfaces:** Consumes everything above. Produces `trait ContainerProbe` (env/stats/exec/http methods returning owned strings/ints, empty on error), `DockerProbe`, `InferenceServerMonitor::{spawn, submit(Vec<InferenceServer>), snapshot() -> Vec<ServiceState>}`, and a pure `fold_tick(prev: &ServiceState, sample: TickSample) -> ServiceState` that the fake-probe test drives.

- [ ] **Step 1: Failing test** for the pure `fold_tick` with a synthetic sample (no docker):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::probe::Readiness;
    #[test]
    fn fold_tick_marks_loading_and_tracks_flat_ticks() {
        let prev = ServiceState_zeroed("z-image-turbo", "Z-Image-Turbo");
        let sample = TickSample {
            cpu_pct: 33.0, rss_bytes: 9_000_000_000, kernel_count: 500,
            safetensors_fds: 2, readiness: Readiness::NotReady,
            top_proc: Some("python3".into()), python_alive: true, last_log: None,
        };
        let next = fold_tick(&prev, sample, &ModelProfile::default(), 5);
        assert_eq!(next.phase, crate::workload::inference_server::state::Phase::Loading);
        assert_eq!(next.rss_delta, 9_000_000_000);   // grew from 0
        // second identical tick → nothing moved → flat_ticks increments
        let sample2 = TickSample { rss_bytes: 9_000_000_000, kernel_count: 500, ..sample_like(&next) };
        let next2 = fold_tick(&next, sample2, &ModelProfile::default(), 5);
        assert_eq!(next2.flat_ticks, 1);
    }
}
```

(The implementer writes the small `ServiceState_zeroed`/`sample_like` test helpers.)

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement `ContainerProbe` + `DockerProbe` in `probe.rs`:**

```rust
use std::process::Command;

/// One tick's raw sample for a service (already parsed from probe output).
pub struct TickSample {
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub kernel_count: usize,
    pub safetensors_fds: usize,
    pub readiness: Readiness,
    pub top_proc: Option<String>,
    pub python_alive: bool,
    pub last_log: Option<String>,
}

/// Abstract container access so the monitor is testable with a fake. Trail: a
/// host/systemd impl for non-Docker installs.
pub trait ContainerProbe: Send {
    fn env(&self, container: &str) -> String;                 // `printenv`-style dump
    fn stats(&self, container: &str) -> String;               // "cpu%|memusage"
    fn exec(&self, container: &str, sh: &str) -> String;      // `sh -c`, stdout
    fn http(&self, port: u16, path: &str) -> (u16, String);   // (status, body); status 0 = down
}

pub struct DockerProbe;
impl ContainerProbe for DockerProbe {
    fn env(&self, c: &str) -> String { docker(&["exec", c, "env"]) }
    fn stats(&self, c: &str) -> String {
        docker(&["stats", "--no-stream", "--format", "{{.CPUPerc}}|{{.MemUsage}}", c])
    }
    fn exec(&self, c: &str, sh: &str) -> String { docker(&["exec", c, "sh", "-c", sh]) }
    fn http(&self, port: u16, path: &str) -> (u16, String) {
        // Reuse the crate's localhost HTTP helper (liveness_probe) for status+body.
        crate::workload::liveness_probe::http_get_status_body(port, path)
    }
}

fn docker(args: &[&str]) -> String {
    Command::new("docker").args(args).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}
```

(Add a small `pub fn http_get_status_body(port, path) -> (u16, String)` to `liveness_probe.rs` reusing its existing `http_get_localhost`; status 0 on connect error.)

- [ ] **Step 4: Implement `fold_tick` + `InferenceServerMonitor` in `monitor.rs`:**

`fold_tick(prev, sample, profile, cadence_secs)`:
- `rss_delta = sample.rss_bytes as i64 - prev.rss_bytes as i64`
- `kernel_delta = sample.kernel_count as i64 - prev.kernel_count as i64`
- momentary `phase = Phase::derive(kernel_delta, sample.python_alive, rss_delta, &sample.readiness)`
- moved this tick = `kernel_delta > 0 || rss_delta > 0`; `flat_ticks = if moved {0} else {prev.flat_ticks + 1}`
- if `is_alarm(flat_ticks, cadence_secs, &sample.readiness)` → `phase = Phase::Alarm`
- `progress = estimate_progress(phase, sample.kernel_count, sample.rss_bytes, profile)`
- return the updated `ServiceState`.

`InferenceServerMonitor::spawn()`: background thread (mirror `LivenessProber`): receives detected `Vec<InferenceServer>`, maps each to a `ServiceDef` via `service_for`; per cadence tick, for each service builds a `TickSample` from the `ContainerProbe` calls (discover `TT_METAL_HOME` via `parse_env_var(probe.env(c), "TT_METAL_HOME")`; kernel_count = `count_lines(probe.exec(c, "find \"$TT_METAL_HOME/built\" -name '*.dephash' 2>/dev/null"))`; stats via `parse_docker_stats(probe.stats(c))`; top proc via `top_process`; python_alive from top_proc/ps; readiness via `parse_liveness(probe.http(port, path))`; last_log via `last_non_health_line`), folds with `fold_tick`, stores `Vec<ServiceState>` in `arc-swap`. `snapshot()` clones the cache.

Full code follows the `liveness_probe.rs` monitor pattern (channel + `recv_timeout(cadence)` + `ArcSwap`). The implementer mirrors that file.

- [ ] **Step 5: tests + clippy + build green. Commit**

```bash
git add src/workload/inference_server/probe.rs src/workload/inference_server/monitor.rs src/workload/inference_server/mod.rs src/workload/liveness_probe.rs
git commit -m "feat(workload): ContainerProbe/DockerProbe + InferenceServerMonitor (fold_tick + arc-swap)"
```

---

### Task F: Detect + feed the monitor from the TUI loop

**Files:** Modify `src/workload/host_processes.rs` (`detected_inference_servers` — reuse Task 1 `parse_inference_server`), `src/ui/tui/mod.rs` (spawn monitor + submit each refresh). Same shape as the existing `LivenessProber` wiring.

- [ ] **Step 1:** Add `HostProcessMonitor::detected_inference_servers() -> Vec<InferenceServer>` (Linux-gated; dedupe by container) — as in the v1 plan Task 7. Test: exists, returns Vec, no panic.
- [ ] **Step 2:** In the TUI init + 2s refresh blocks (next to `liveness_prober`), Linux-gated: `let inference_monitor = InferenceServerMonitor::spawn();` and `inference_monitor.submit(host_proc_monitor.detected_inference_servers());`.
- [ ] **Step 3:** build (`-D warnings`) + tests green. **Commit.**

---

### Task G: `[i]` Inference Servers panel

**Files:** Modify `src/ui/tui/mod.rs` — add an `i` key toggle for a new Insights sub-view; `format_service_row(&ServiceState) -> String` (pure, tested); render one colored row per `inference_monitor.snapshot()` entry (green Ready, yellow Compiling/Loading, red Alarm, gray Down), plus the progress evidence line (RSS, +rate, kernel count/delta, liveness code).

- [ ] **Step 1: Failing test** for `format_service_row` (loading z-image, RSS shown, phase LOADING).
- [ ] **Step 2: Run — fails.**
- [ ] **Step 3: Implement `format_service_row`** (phase label, model, RSS in GiB, `+MB/tick` rate, liveness code; `✓ progressing` when a delta moved, `⚠ stalled` at Alarm).
- [ ] **Step 4: Wire the `i` toggle + panel render** into the Insights layout (follow the existing key-handling + panel style in the file). Show all `SERVERS`, not just running ones (Down for absent).
- [ ] **Step 5:** tests + clippy + `-D warnings` build green.
- [ ] **Step 6: Manual check** on the live box: `tt-toplike`, press `i`; while Z-Image-Turbo loads, confirm phase + growing RSS/kernel evidence and the alarm path when idle.
- [ ] **Step 7: Commit.**

---

## Self-Review

**Spec coverage:** phase model (Down/Compiling/Loading/Ready/Alarm) → Task D; three progress probes (kernel-artifact count via discovered `$TT_METAL_HOME/built`, RSS, `.safetensors` FDs) → Tasks C/E; per-service liveness endpoints + `runner_in_use` → Tasks B/C; docker stats/exec/inspect access → Task E; hang alarm (all-flat ≥5min) → Task D; services registry → Task B; `[i]` panel + services list → Task G; log breadcrumb → Task A; runtime cache discovery (no hardcode) → Task E; extension trails (ContainerProbe trait, ModelProfile, config-driven services, metrics) → Tasks B/D/E. ✓

**Placeholder scan:** Task E Step 4 (fold_tick + monitor) and Task F/G describe the monitor loop and panel wiring in prose rather than full code, because they mirror the existing `liveness_probe.rs` monitor and the Insights render code already in the tree; the pure pieces they depend on (`fold_tick` rule, `Phase::derive`, all parsers, `format_service_row`) are fully specified with code + tests. Acceptable per "follow existing patterns."

**Type consistency:** `Readiness`, `ServiceDef`/`service_for`/`SERVERS`, `Phase`/`ModelProfile`/`ServiceState`/`is_alarm`/`estimate_progress`/`Phase::derive`, `ContainerProbe`/`DockerProbe`/`TickSample`, `fold_tick`, `InferenceServerMonitor::{spawn,submit,snapshot}`, `last_non_health_line`, `parse_*`/`count_lines`/`top_process` — consistent across Tasks A–G.
