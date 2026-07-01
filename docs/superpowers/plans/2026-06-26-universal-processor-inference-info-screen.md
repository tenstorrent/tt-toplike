# Universal Processor + Inference-Process Info Screen — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the default Insights screen cross-platform — rich per-device panels for any processor (CPU/Apple GPU/ANE/TT) plus a unified process panel that lists processes by resource use and tags those matching known ML/inference runtimes, with TT device-fd attribution and serving metrics layered in by PID where available.

**Architecture:** Add a cross-platform `sysinfo`-based process collector (`workload::host_processes`) and a pure `inference_match` classifier (`workload::inference_match`) producing a unified `ProcRow`. Relax the `cfg(all(target_os = "linux", feature = "linux-procfs"))` gates on the Insights renderers so they run everywhere, pushing only the genuinely TT/procfs-specific bits (device-fd scan, serving probes, hugepages) behind `cfg`. The Linux `ProcessMonitor` + `serving` data is merged into `ProcRow` by PID.

**Tech Stack:** Rust, `sysinfo` (existing dep, process enumeration), ratatui (TUI), existing `workload::{inference, process_monitor, serving}`.

## Global Constraints

- **Cross-platform default screen:** new code is ungated (compiles on macOS/Linux/Windows). Only TT/procfs-specific data and rendering stay behind `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]`.
- **`ProcRow` is gate-free:** it must not embed any `cfg`-gated type. TT enrichment on it is primitives only (`device_indices`, hugepage counts). Serving metrics (the gated `ServingMetrics` type) stay passed separately and rendered behind `cfg`.
- **Preserve TT behavior:** the existing Linux `ProcessMonitor` and `serving` detection code is unchanged — only merged in by PID. TT Insights output (columns, data) stays equivalent; guarded by existing + new tests.
- **Detection = hybrid:** show union of top-N-by-CPU/mem and all name-matched processes; matched first, then CPU desc.
- **Graceful degradation:** `sysinfo` failure → empty process panel, device panels still render. No panics; everything `Option`.
- **No new dependencies.** `sysinfo` is already present.
- **Out of scope:** per-process GPU/ANE attribution; cross-platform process *kill* (kill UI stays Linux-gated); NVIDIA/AMD discovery; the Phase-2 perf (TPS/TTFT) layer.
- **Repo facts:** Insights renderers + helpers are gated and live in `src/ui/tui/mod.rs`: `render_insights` (2410), `render_insights_no_procfs` (2496), `render_device_panels` (2997) + helpers `panel_layout` (2359)/`device_panels_height` (2381)/`render_fleet_heatmap_panel` (2847), `render_process_panel` (2552). `run_app` process/inference state is gated at lines 195-219; refresh tick at 901-912; the Insights match arm at 461-482. The gated `use` is line 22-23. `InferenceEngine` is cross-platform; `ProcessMonitor`/`InferenceServerProbe`/`ServingMetrics`/`ProcessInfo` are Linux+procfs-gated (see `src/workload/mod.rs`). Build: `cargo build --bin tt-toplike-tui --features tui`. Tests: `cargo test --lib --tests --features tui`.

---

### Task 1: `inference_match` classifier (cross-platform, pure)

**Files:**
- Create: `src/workload/inference_match.rs`
- Modify: `src/workload/mod.rs` (register module + re-export, ungated)
- Test: inline `#[cfg(test)] mod tests` in `inference_match.rs`

**Interfaces:**
- Produces: `pub fn inference_match(name: &str, cmdline: &str) -> Option<&'static str>` — returns a static runtime label or `None`.

- [ ] **Step 1: Register the module (ungated)**

In `src/workload/mod.rs`, add near the top with the other `pub mod` lines (NOT under any `cfg`):
```rust
pub mod inference_match;
pub use inference_match::inference_match;
```

- [ ] **Step 2: Write the failing test**

Create `src/workload/inference_match.rs` with the test module first:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_runtimes() {
        assert_eq!(inference_match("ollama", "ollama serve"), Some("ollama"));
        assert_eq!(inference_match("python3", "python3 -m vllm.entrypoints.openai.api_server"), Some("vllm"));
        assert_eq!(inference_match("python", "python train.py --use torch"), Some("torch"));
        assert_eq!(inference_match("llama-server", "/usr/bin/llama-server -m m.gguf"), Some("llama.cpp"));
        assert_eq!(inference_match("mlx_lm.server", "mlx_lm.server --model x"), Some("mlx"));
    }

    #[test]
    fn ignores_non_inference() {
        assert_eq!(inference_match("WindowServer", "/System/.../WindowServer"), None);
        assert_eq!(inference_match("node", "node server.js"), None);
        assert_eq!(inference_match("", ""), None);
    }

    #[test]
    fn name_or_cmdline_either_matches() {
        // matches on cmdline even when the process name is a generic interpreter
        assert_eq!(inference_match("python3.11", "comfyui/main.py"), Some("ComfyUI"));
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib --features tui inference_match`
Expected: FAIL — `cannot find function inference_match`.

- [ ] **Step 4: Write the implementation**

Prepend to `src/workload/inference_match.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Heuristic classifier: does a process look like an ML/inference runtime?
//!
//! Pure, cross-platform name/cmdline matching against a curated, easily-extended
//! list. Returns a stable runtime label for display, or `None`. This is a "might
//! be doing inference" hint, not proof — it does not attribute device usage.

/// (label, needles) — if any needle appears (case-insensitive) in the process
/// name or full cmdline, the process is tagged with `label`. Ordered most- to
/// least-specific so the first hit wins.
const RUNTIMES: &[(&str, &[&str])] = &[
    ("ollama", &["ollama"]),
    ("vllm", &["vllm"]),
    ("llama.cpp", &["llama-server", "llama-cli", "llama.cpp", "llama_cpp", "llamacpp"]),
    ("mlx", &["mlx_lm", "mlx-lm", "mlx.server"]),
    ("ComfyUI", &["comfyui"]),
    ("stable-diffusion", &["stable-diffusion", "sdwebui", "automatic1111"]),
    ("koboldcpp", &["koboldcpp", "koboldai"]),
    ("whisper", &["whisper.cpp", "whisper-server", "faster-whisper"]),
    ("lm-studio", &["lm studio", "lmstudio", "lms server"]),
    ("text-generation", &["text-generation-inference", "text-generation-webui", "tgi"]),
    ("vllm", &["vllm"]),
    ("torch", &["torchrun", "torch.distributed", " torch", "pytorch"]),
    ("transformers", &["transformers", "huggingface", "hf_", "accelerate launch"]),
    ("diffusers", &["diffusers"]),
    ("jax", &["jax", "flax"]),
    ("tensorflow", &["tensorflow", "tf.keras"]),
    ("triton", &["tritonserver", "triton_python"]),
];

/// Classify a process by name + cmdline. Returns the matched runtime label, or
/// `None` if nothing matches.
pub fn inference_match(name: &str, cmdline: &str) -> Option<&'static str> {
    if name.is_empty() && cmdline.is_empty() {
        return None;
    }
    let hay = format!("{} {}", name, cmdline).to_lowercase();
    for (label, needles) in RUNTIMES {
        for needle in *needles {
            if hay.contains(&needle.to_lowercase()) {
                return Some(label);
            }
        }
    }
    None
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib --features tui inference_match`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**
```bash
git add src/workload/inference_match.rs src/workload/mod.rs
git commit -m "feat(workload): cross-platform inference-runtime classifier"
```

---

### Task 2: `host_processes` collector + `ProcRow` (cross-platform)

**Files:**
- Create: `src/workload/host_processes.rs`
- Modify: `src/workload/mod.rs` (register + re-export, ungated)
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `inference_match` (Task 1).
- Produces:
  - `pub struct TtProcInfo { pub device_indices: Vec<usize>, pub hugepages_1g: usize, pub hugepages_2m: usize }`
  - `pub struct ProcRow { pub pid: i32, pub name: String, pub cpu_pct: f32, pub mem_bytes: u64, pub inference: Option<&'static str>, pub tt: Option<TtProcInfo> }`
  - `pub struct HostProcessMonitor { /* private System */ }`
  - `HostProcessMonitor::new() -> Self`, `HostProcessMonitor::update(&mut self)`, `HostProcessMonitor::rows(&self, max: usize) -> Vec<ProcRow>`
  - `pub(crate) fn select_rows(all: Vec<ProcRow>, max: usize) -> Vec<ProcRow>` (pure, tested)

- [ ] **Step 1: Register the module (ungated)**

In `src/workload/mod.rs` (NOT under `cfg`):
```rust
pub mod host_processes;
pub use host_processes::{HostProcessMonitor, ProcRow, TtProcInfo};
```

- [ ] **Step 2: Write the failing test (pure selection/sort)**

Create `src/workload/host_processes.rs` with the test module first:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i32, cpu: f32, inf: Option<&'static str>) -> ProcRow {
        ProcRow { pid, name: format!("p{pid}"), cpu_pct: cpu, mem_bytes: 0, inference: inf, tt: None }
    }

    #[test]
    fn matched_rows_sort_first_then_by_cpu() {
        let all = vec![
            row(1, 90.0, None),
            row(2, 10.0, Some("ollama")),
            row(3, 50.0, None),
            row(4, 5.0, Some("vllm")),
        ];
        let out = select_rows(all, 3);
        // matched (2,4) first, each group by cpu desc; then top non-matched by cpu
        assert_eq!(out.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![2, 4, 1]);
    }

    #[test]
    fn matched_idle_process_still_included_over_cap() {
        // a matched process with tiny cpu must survive even when many busy procs exist
        let mut all: Vec<ProcRow> = (10..20).map(|p| row(p, 80.0, None)).collect();
        all.push(row(99, 0.1, Some("mlx")));
        let out = select_rows(all, 3);
        assert!(out.iter().any(|r| r.pid == 99), "matched process must be kept");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib --features tui host_processes`
Expected: FAIL — `cannot find function select_rows` / types.

- [ ] **Step 4: Write the implementation**

Prepend to `src/workload/host_processes.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Cross-platform process collector for the Insights screen.
//!
//! Enumerates host processes via `sysinfo` and tags likely ML/inference
//! runtimes (see `inference_match`). Produces gate-free `ProcRow`s that the
//! Linux/TT path may enrich by PID with device-fd attribution.

use crate::workload::inference_match;
use sysinfo::{ProcessRefreshKind, RefreshKind, System};

/// TT-specific per-process info, merged in by PID on TT hardware. Primitives
/// only, so `ProcRow` stays free of any `cfg`-gated type.
#[derive(Debug, Clone, Default)]
pub struct TtProcInfo {
    pub device_indices: Vec<usize>,
    pub hugepages_1g: usize,
    pub hugepages_2m: usize,
}

/// One row in the Insights process panel.
#[derive(Debug, Clone)]
pub struct ProcRow {
    pub pid: i32,
    pub name: String,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
    pub inference: Option<&'static str>,
    pub tt: Option<TtProcInfo>,
}

/// Owns a `sysinfo::System` refreshed for process listing.
pub struct HostProcessMonitor {
    sys: System,
}

impl HostProcessMonitor {
    pub fn new() -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
        );
        HostProcessMonitor { sys }
    }

    /// Refresh the process list (call on the ~2s cadence).
    pub fn update(&mut self) {
        self.sys
            .refresh_processes_specifics(ProcessRefreshKind::everything());
    }

    /// Build selected rows: union of top-`max` by CPU and all inference-matched.
    pub fn rows(&self, max: usize) -> Vec<ProcRow> {
        let all: Vec<ProcRow> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let name = p.name().to_string_lossy().to_string();
                let cmdline = p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                ProcRow {
                    pid: pid.as_u32() as i32,
                    inference: inference_match(&name, &cmdline),
                    name,
                    cpu_pct: p.cpu_usage(),
                    mem_bytes: p.memory(),
                    tt: None,
                }
            })
            .collect();
        select_rows(all, max)
    }
}

impl Default for HostProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure selection: all inference-matched rows first (CPU desc), then the
/// highest-CPU non-matched rows, capped so the total is at least `max` matched
/// plus fills remaining slots. Matched rows are never dropped.
pub(crate) fn select_rows(mut all: Vec<ProcRow>, max: usize) -> Vec<ProcRow> {
    all.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap_or(std::cmp::Ordering::Equal));
    let (mut matched, non_matched): (Vec<ProcRow>, Vec<ProcRow>) =
        all.into_iter().partition(|r| r.inference.is_some());
    let remaining = max.saturating_sub(matched.len());
    matched.extend(non_matched.into_iter().take(remaining));
    matched
}
```

> Note: confirm the `sysinfo` 0.38 API names while implementing (`refresh_processes_specifics`, `ProcessRefreshKind::everything`, `Process::cmd`/`name` returning `OsStr`, `Pid::as_u32`). Adjust calls to the installed version if they differ; the public interface above must not change.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib --features tui host_processes`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**
```bash
git add src/workload/host_processes.rs src/workload/mod.rs
git commit -m "feat(workload): cross-platform host process collector + ProcRow"
```

---

### Task 3: Un-gate the device-panel renderers

Make the rich per-device cards render on every platform (they are procfs-free; confirmed in the Apple GPU/ANE review).

**Files:**
- Modify: `src/ui/tui/mod.rs` — remove the `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]` attribute immediately above each of: `render_device_panels` (line 2997), `panel_layout` (2359), `device_panels_height` (2381), `render_fleet_heatmap_panel` (2847).

**Interfaces:**
- Produces: these four functions become callable on all targets with unchanged signatures.

- [ ] **Step 1: Remove the four cfg attributes**

Delete only the single `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]` line directly above each of the four `fn` definitions named above. Do not change the function bodies or signatures.

- [ ] **Step 2: Move the gated `tests` module's `panel_layout` import if needed**

The `panel_layout` unit tests live in the Linux-gated `tests` module (`mod.rs:3760` `#[cfg(all(target_os = "linux", feature = "linux-procfs"))] #[cfg(test)] mod tests`). Leave that module gated (its other tests may use gated items) — `panel_layout` is still in scope for it. No change needed unless the build says otherwise.

- [ ] **Step 3: Build on macOS to verify cross-platform compile**

Run: `cargo build --bin tt-toplike-tui --features tui`
Expected: builds with zero warnings. (If `render_fleet_heatmap_panel` references any still-gated symbol, gate just that reference or report DONE_WITH_CONCERNS — but the review confirmed it is procfs-free.)

- [ ] **Step 4: Commit**
```bash
git add src/ui/tui/mod.rs
git commit -m "refactor(tui): un-gate device-panel renderers for cross-platform use"
```

---

### Task 4: Cross-platform `render_process_panel` over `ProcRow`

**Files:**
- Modify: `src/ui/tui/mod.rs` — `render_process_panel` (line 2552): remove its `cfg` gate, change its signature to consume `&[ProcRow]`, render the inference tag always, TT columns when present, serving columns behind `cfg`.

**Interfaces:**
- Consumes: `crate::workload::ProcRow` (Task 2); on Linux also the gated `ServingMetrics` map.
- Produces: `fn render_process_panel(f, area, rows: &[crate::workload::ProcRow], cursor: usize, kill_confirm: Option<&KillConfirmState>, host_cpu_pct: f32, host_mem_used: u64, host_mem_total: u64, #[cfg(all(target_os="linux", feature="linux-procfs"))] serving_metrics: &std::collections::HashMap<i32, ServingMetrics>)`.

- [ ] **Step 1: Replace the signature and gate**

Remove the `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]` above `render_process_panel`. Replace the parameter list: drop `pm: &ProcessMonitor` and `procs: &[&crate::workload::ProcessInfo]`; add `rows: &[crate::workload::ProcRow]`. Keep `cursor`, `kill_confirm`, `host_cpu_pct/used/total`. Make the `serving_metrics` parameter `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]`-attributed (per-parameter cfg attribute is valid Rust).

- [ ] **Step 2: Rewrite the row-rendering loop**

Replace the body that iterated `procs` (ProcessInfo) with one iterating `rows` (ProcRow). For each row render: `PID`, `CPU%`, `MEM` (human bytes), `NAME`, and an `INFERENCE?` column = `~ {label}` when `row.inference.is_some()` else `-`. When `row.tt.is_some()`, also render the TT device-index column (`dev {indices}`) and hugepage hint. Behind `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]`, if `serving_metrics.get(&row.pid)` is `Some`, append the serving cell (port/health) exactly as the current code formats it. Keep the existing host CPU/RAM bar block above the list and the cursor-highlight logic (now indexing `rows`).

- [ ] **Step 3: Build**

Run: `cargo build --bin tt-toplike-tui --features tui`
Expected: compile errors at the call site (`render_insights`) — expected; fixed in Task 5.

- [ ] **Step 4: Commit (compiles after Task 5; commit the panel change with its caller in Task 5)**

Defer the commit to Task 5 Step 5 so the tree builds. (This task's deliverable is verified together with Task 5.)

---

### Task 5: Unify `render_insights` (one cross-platform path)

**Files:**
- Modify: `src/ui/tui/mod.rs` — un-gate `render_insights` (2410), delete `render_insights_no_procfs` (2496), update the Insights match arm (461-482) to call the single `render_insights` with `&[ProcRow]`.

**Interfaces:**
- Consumes: `render_device_panels` (Task 3), `render_process_panel` (Task 4), `&[ProcRow]` (Task 2).
- Produces: `fn render_insights(f, backend, rows: &[ProcRow], cursor, kill_confirm, cli, portrait_particles, portrait_baseline, fleet_cursor, fleet_zoom_start, #[cfg(linux+procfs)] serving_metrics)`. (Drops the `_engine`/`pm` params; `host_cpu/mem` still passed through to the process panel — keep those params.)

- [ ] **Step 1: Un-gate and re-signature `render_insights`**

Remove its `cfg` gate. Replace the `pm: &ProcessMonitor` parameter with `rows: &[crate::workload::ProcRow]`; drop `_engine` (unused). Keep `host_cpu_pct/used/total`. Gate the `serving_metrics` parameter with `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]`. In the body, the call to `render_process_panel(...)` now passes `rows` and (behind the same cfg) `serving_metrics`.

- [ ] **Step 2: Delete `render_insights_no_procfs`**

Remove the whole `#[cfg(not(...))] fn render_insights_no_procfs(...)` function (line 2496). `render_grid_mode` stays (used by the `Grid` view mode).

- [ ] **Step 3: Update the Insights match arm in `run_app`**

At `mod.rs:461-482`, replace the two-branch `#[cfg]` dispatch (`render_insights` / `render_insights_no_procfs`) with a single call to `render_insights(f, backend, &proc_rows, process_cursor, kill_confirm.as_ref(), cli, &portrait_particles, &portrait_baseline, fleet_cursor, fleet_zoom_start)` plus, behind `#[cfg(all(target_os="linux", feature="linux-procfs"))]`, the trailing `&serving_metrics` argument. `proc_rows: Vec<ProcRow>` comes from Task 6.

- [ ] **Step 4: Build**

Run: `cargo build --bin tt-toplike-tui --features tui`
Expected: errors only where `proc_rows`/`process_cursor`/`fleet_cursor` are defined in `run_app` — fixed in Task 6.

- [ ] **Step 5: Commit (with Task 4's panel change)**

Defer to Task 6 Step 6 (the tree builds once `run_app` provides `proc_rows`). Tasks 4–6 land as one buildable commit set.

---

### Task 6: Cross-platform process state in `run_app`

Replace the Linux-gated process/host state with a cross-platform `HostProcessMonitor` producing `proc_rows`, enriched on Linux by PID.

**Files:**
- Modify: `src/ui/tui/mod.rs` — imports (22-23), state decls (195-219), the refresh tick (901-912), and provide `proc_rows` to the render arm.

**Interfaces:**
- Consumes: `HostProcessMonitor`/`ProcRow`/`TtProcInfo` (Task 2); on Linux the gated `ProcessMonitor`/`InferenceServerProbe`/`ServingMetrics` (unchanged).
- Produces: a `proc_rows: Vec<ProcRow>` in `run_app` scope, refreshed every 2 s, consumed by `render_insights`.

- [ ] **Step 1: Make `InferenceEngine` import cross-platform; keep TT imports gated**

At `mod.rs:22-23`, split the import: `use crate::workload::InferenceEngine;` ungated, and keep `#[cfg(all(target_os = "linux", feature = "linux-procfs"))] use crate::workload::{InferenceServerProbe, ProcessMonitor, ServingMetrics};`. Add `use crate::workload::{HostProcessMonitor, ProcRow};` ungated.

- [ ] **Step 2: Add cross-platform process state; keep TT state gated**

After the gated `process_monitor` block (195-200), add ungated:
```rust
let mut host_proc_monitor = HostProcessMonitor::new();
host_proc_monitor.update();
let mut proc_rows: Vec<ProcRow> = host_proc_monitor.rows(PROC_PANEL_MAX_ROWS);
let mut last_proc_rows_update = Instant::now();
```
Add `const PROC_PANEL_MAX_ROWS: usize = 12;` near the top of the function or module. Keep the gated `process_monitor`/`sys_monitor`/`host_cpu_pct` state as-is (still used for host bars + TT enrichment).

Make `process_cursor`/`fleet_cursor`/`fleet_zoom_start`/`portrait_particles`/`portrait_baseline` ungated if any are currently gated (they're referenced by the now-ungated render path). Verify each is declared ungated; un-gate any that aren't.

- [ ] **Step 3: Refresh `proc_rows` on the 2s tick + enrich on Linux**

In the refresh block (901-912), add (ungated):
```rust
if last_proc_rows_update.elapsed() >= Duration::from_secs(2) {
    host_proc_monitor.update();
    proc_rows = host_proc_monitor.rows(PROC_PANEL_MAX_ROWS);
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    enrich_proc_rows_tt(&mut proc_rows, &process_monitor);
    last_proc_rows_update = Instant::now();
}
```
Add a Linux-only helper near `flat_process_list`:
```rust
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
fn enrich_proc_rows_tt(rows: &mut [crate::workload::ProcRow], pm: &ProcessMonitor) {
    for info in flat_process_list(pm) {
        if let Some(r) = rows.iter_mut().find(|r| r.pid == info.pid) {
            r.tt = Some(crate::workload::TtProcInfo {
                device_indices: info.device_indices.clone(),
                hugepages_1g: info.hugepages_1g,
                hugepages_2m: info.hugepages_2m,
            });
        }
    }
}
```
(If a TT process is not in `proc_rows` because it didn't rank in top-N and wasn't name-matched, it simply isn't shown — acceptable; TT processes are typically high-CPU. Optional: also push missing TT PIDs as rows. Keep YAGNI: skip unless a test requires it.)

- [ ] **Step 4: Pass `proc_rows` to the render arm**

Ensure the Task-5 match-arm call passes `&proc_rows`. Keep the existing host CPU/RAM bar values (`host_cpu_pct/used/total`) flowing to the process panel (they're computed in the gated block today; if the host bars should show off-Linux too, compute them from `host_proc_monitor`'s system or a small ungated sysinfo CPU read — otherwise pass `0.0`/`0` off-Linux and render bars only when non-zero). Decision for this task: compute host CPU/mem cross-platform from a small ungated sysinfo read so the bars work on macOS; remove the `cfg` gate on that state block.

- [ ] **Step 5: Build + run**

Run: `cargo build --bin tt-toplike-tui --features tui` (zero warnings), then `cargo test --lib --tests --features tui` (all pass), then `./target/debug/tt-toplike-tui --host --print` (still works).

- [ ] **Step 6: Commit Tasks 4–6 together**
```bash
git add src/ui/tui/mod.rs
git commit -m "feat(tui): cross-platform Insights — unified process panel over ProcRow"
```

---

### Task 7: Integration test + README

**Files:**
- Modify: `tests/backend_isolation.rs` (cross-platform Insights render test)
- Modify: `README.md` (Insights/host section)

**Interfaces:**
- Consumes: `HostProcessMonitor`/`ProcRow`/`inference_match` (Tasks 1-2); `render`-level behavior (Tasks 4-6).

- [ ] **Step 1: Write the integration test**

Append to `tests/backend_isolation.rs`:
```rust
#[test]
fn inference_match_tags_known_runtimes_via_public_api() {
    use tt_toplike::workload::inference_match;
    assert_eq!(inference_match("ollama", "ollama serve"), Some("ollama"));
    assert_eq!(inference_match("WindowServer", "/System/WindowServer"), None);
}

#[test]
fn host_process_monitor_lists_this_process() {
    use tt_toplike::workload::HostProcessMonitor;
    let mut m = HostProcessMonitor::new();
    m.update();
    // The test binary itself should appear among the rows.
    let rows = m.rows(64);
    assert!(!rows.is_empty(), "should enumerate at least one process");
    assert!(rows.iter().all(|r| r.pid > 0));
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --test backend_isolation --features tui`
Expected: PASS (existing + 2 new).

- [ ] **Step 3: Update README**

In `README.md`, in the Insights/`--host` area, add a sentence: the Insights screen works on any machine — it shows each processor (CPU/GPU/ANE/TT) as a device card and lists processes by resource use, tagging those that match known inference runtimes (ollama, vLLM, llama.cpp, MLX, ComfyUI, …); on TT hardware it additionally attributes device usage and serving metrics per process.

- [ ] **Step 4: Full verification**

Run: `cargo test --lib --tests --features tui` (all pass) and `cargo build --bin tt-toplike-tui --features tui` (zero warnings).

- [ ] **Step 5: Commit**
```bash
git add tests/backend_isolation.rs README.md
git commit -m "test+docs: cross-platform Insights process detection"
```

---

## Self-Review

**1. Spec coverage:**
- Rich device panels on host (retire grid fallback) → Tasks 3, 5 ✅
- Unified process panel, hybrid detection → Tasks 1, 2, 4 ✅
- Universal scope + TT enrichment by PID → Task 6 (`enrich_proc_rows_tt`) ✅
- `ProcRow` gate-free; serving stays gated/separate → Task 2 (TtProcInfo primitives) + Task 4 (gated serving param) ✅
- Degradation/no panic → `Option`/empty rows throughout ✅
- Testing (matcher, selection, integration) → Tasks 1, 2, 7 ✅
- Preserve TT behavior → Task 6 keeps `process_monitor`/`serving` unchanged, merges by PID ✅
- README → Task 7 ✅

**2. Placeholder scan:** No TBD/TODO. Two "confirm against installed version" notes (sysinfo API, fleet helper procfs-freedom) are concrete verification steps with defined fallbacks, not deferred decisions.

**3. Type consistency:** `ProcRow`/`TtProcInfo`/`inference_match`/`select_rows`/`HostProcessMonitor.rows(max)` are defined in Tasks 1-2 and used identically in Tasks 4-7. `render_process_panel`/`render_insights` new signatures match between Tasks 4, 5, 6. `enrich_proc_rows_tt` (Task 6) consumes the `TtProcInfo` shape from Task 2.

**Known risk (flagged for execution):** Tasks 4-6 refactor large existing gated functions; their steps give targeted edit instructions (exact functions, cfg lines, signatures, merge logic) rather than full body reproductions. The implementer must read the current bodies and apply the changes — the per-parameter `#[cfg]` on `serving_metrics` and un-gating the cursor/particle state are the fiddly parts; the build errors in Tasks 4-5 Step 3/4 are expected and resolve at Task 6.
