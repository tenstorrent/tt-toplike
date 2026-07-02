# TT-only Process Filtering + Inference Monitor View — Implementation Plan (spec #1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** On real-TT backends, show only TT-attributed processes in the Insights panel; and promote the `[i]` inference monitor to a dedicated full-screen view with a navigable service list + best-effort pre-focus.

**Architecture:** A pure backend predicate + a pure TT-filtered row selector feed a new `tt_rows` path in the process panel. A new `DisplayMode::InferenceMonitor` replaces the in-Insights `show_inference` overlay; `i` enters it (remembering the prior mode), `Esc`/`i` exits, `↑/↓` move a service cursor.

**Tech Stack:** Rust, ratatui, existing `inference_server`/`host_processes`/`process_monitor` modules.

## Global Constraints

- **Linux/TT filtering is behind `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]`** (that's where `ProcessMonitor`/`flat_process_list` live); non-Linux keeps host behavior.
- **Filter rule:** `backend_shows_only_tt(bt) = !matches!(bt, Host | Mock)` (host shows host procs; mock exempt; everything else filters). Resolve `Auto` to its effective backend before applying.
- **No new dependencies. No panics. SPDX header on any new file.**
- **The inference-server types are cross-platform** (ungated since the last feature); the monitor view renders on all platforms (empty snapshot → all `Down`), but the TT-fd filtering + `inference_monitor.snapshot()` reads stay Linux-gated.
- **Verify each task:** `cargo test --locked --lib --features tui` + `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings` + macOS cross-check `cargo clippy --target aarch64-apple-darwin --no-default-features --features tui,json-backend -- -D warnings` (check the REAL exit code; do not pipe through `tail`).

## Current-state anchors

- `src/workload/host_processes.rs`: `rows(max, verdicts)` builds all `ProcRow`s then `select_rows(all, max)` (pure, line 252) picks active-inference + busiest. `ProcRow { pid, name, cpu_pct, mem_bytes, inference: Option<&'static str>, active, tt: Option<TtProcInfo> }`.
- `src/ui/tui/mod.rs`: `flat_process_list(&ProcessMonitor) -> Vec<ProcessInfo>` (each has `.pid`) is the TT-attributed set; `enrich_proc_rows_tt(&mut rows, &pm)` sets `tt` by PID. `show_inference: bool` (line 243) toggled by `i` (line 755), passed to `render_insights` (2670, param at 2695), which adds an inference panel (2718/2744). `DisplayMode` enum has Insights/Grid/Starfield/MemoryCastle/MemoryFlow/Arcade/Defrag. The `v` key cycles modes; `i` currently flips `show_inference`.
- `src/ui/tui/inference_panel.rs`: `format_service_row`, `service_rows(snapshot)` (merges with SERVERS), `model_unloaded`.

## File Structure
- `src/cli.rs` — add `backend_shows_only_tt(BackendType) -> bool` (near `BackendType`).
- `src/workload/host_processes.rs` — add `select_tt_rows` (pure) + `tt_rows(max, &HashSet<i32>)`.
- `src/ui/tui/inference_panel.rs` — add `render_inference_monitor(...)` + `service_for_selected_process(&ProcRow) -> Option<&'static str>`.
- `src/ui/tui/mod.rs` — add `DisplayMode::InferenceMonitor`; TT-filter the panel; replace `show_inference` overlay with the view + `service_cursor` + key handling.

---

### Task 1: Pure backend predicate + TT-filtered row selection

**Files:** Modify `src/cli.rs`, `src/workload/host_processes.rs`.

**Interfaces:**
- Produces: `cli::backend_shows_only_tt(bt: BackendType) -> bool`; `host_processes::select_tt_rows(all: Vec<ProcRow>, tt_pids: &HashSet<i32>, max: usize) -> Vec<ProcRow>`; `HostProcessMonitor::tt_rows(&self, max: usize, tt_pids: &HashSet<i32>) -> Vec<ProcRow>`.

- [ ] **Step 1: Failing test for the predicate** (in `src/cli.rs` tests):

```rust
#[test]
fn tt_filter_applies_to_real_tt_backends_only() {
    use super::*;
    assert!(!backend_shows_only_tt(BackendType::Host));
    assert!(!backend_shows_only_tt(BackendType::Mock));
    assert!(backend_shows_only_tt(BackendType::Sysfs));
    assert!(backend_shows_only_tt(BackendType::Json));
    assert!(backend_shows_only_tt(BackendType::Luwen));
}
```
(Use the actual `BackendType` variant names — confirm them at the top of `cli.rs`; if `Auto`/`Hybrid` exist, add assertions matching intent: `Auto` follows its resolved backend, so test the resolved variants.)

- [ ] **Step 2: Run — fails** (`backend_shows_only_tt` undefined): `cargo test --locked --lib --features tui backend_shows_only_tt` → FAIL.

- [ ] **Step 3: Implement the predicate** in `src/cli.rs`:

```rust
/// True when the active backend represents real TT hardware, so the process
/// panel should list only TT-attributed processes. Host shows host CPU procs;
/// Mock is exempt (no real /dev/tenstorrent fds — keep --mock demos non-blank).
pub fn backend_shows_only_tt(bt: BackendType) -> bool {
    !matches!(bt, BackendType::Host | BackendType::Mock)
}
```

- [ ] **Step 4: Run — passes.**

- [ ] **Step 5: Failing test for `select_tt_rows`** (in `host_processes.rs` tests, reuse the existing `row(pid, cpu, inf, active)` helper):

```rust
#[test]
fn select_tt_rows_keeps_only_attributed_pids_sorted_by_cpu() {
    use std::collections::HashSet;
    let all = vec![
        row(1, 10.0, None, false),
        row(2, 90.0, None, false),   // high cpu but NOT a TT pid
        row(3, 50.0, Some("vllm"), true),
        row(4, 20.0, None, false),
    ];
    let tt: HashSet<i32> = [1, 3, 4].into_iter().collect();
    let out = select_tt_rows(all, &tt, 10);
    // only TT pids, sorted by cpu desc: 3 (50), 4 (20), 1 (10)
    assert_eq!(out.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![3, 4, 1]);
    // empty pid set → empty
    assert!(select_tt_rows(vec![row(1, 5.0, None, false)], &HashSet::new(), 10).is_empty());
}
```

- [ ] **Step 6: Run — fails.**

- [ ] **Step 7: Implement `select_tt_rows`** (pure) next to `select_rows` in `host_processes.rs`:

```rust
/// Pure: keep only rows whose PID is in `tt_pids` (attributed to a TT device),
/// sorted by CPU desc, capped at `max`. Used on real-TT backends so the panel
/// shows only processes contributing to TT load.
pub(crate) fn select_tt_rows(
    mut all: Vec<ProcRow>,
    tt_pids: &std::collections::HashSet<i32>,
    max: usize,
) -> Vec<ProcRow> {
    all.retain(|r| tt_pids.contains(&r.pid));
    all.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all.truncate(max);
    all
}
```

- [ ] **Step 8: Add `tt_rows`** to `impl HostProcessMonitor` (builds the raw rows exactly like `rows()` does its first pass — same `inference`/`active` tagging — then `select_tt_rows`). Mirror the raw-row construction in `rows()`; the only difference is the final selector. No separate test (its logic is `select_tt_rows`, already tested; the raw construction is shared with `rows()`).

```rust
/// Rows for TT-attributed processes only (real-TT backends). `tt_pids` is the
/// set of PIDs holding a /dev/tenstorrent fd (from `flat_process_list`). Rows are
/// not TT-enriched here — the caller runs `enrich_proc_rows_tt` afterward as usual.
pub fn tt_rows(&self, max: usize, tt_pids: &std::collections::HashSet<i32>) -> Vec<ProcRow> {
    let all = self.build_all_rows(); // extract the per-process ProcRow mapping in rows() into build_all_rows()
    select_tt_rows(all, tt_pids, max)
}
```
Refactor: extract the closure that maps sysinfo processes → `Vec<ProcRow>` (the body of `rows()` before `select_rows`) into a private `fn build_all_rows(&self) -> Vec<ProcRow>`, and have `rows()` call `select_rows(self.build_all_rows(), max)` so both paths share it (DRY).

- [ ] **Step 9: Run all host_processes + cli tests — pass. Commit.**

```bash
git add src/cli.rs src/workload/host_processes.rs
git commit -m "feat(workload): backend_shows_only_tt predicate + TT-filtered row selection"
```

---

### Task 2: Wire TT filtering into the process panel

**Files:** Modify `src/ui/tui/mod.rs` (the proc_rows build at init ~line 268 and refresh ~line 1047).

**Interfaces:** Consumes `cli::backend_shows_only_tt`, `HostProcessMonitor::tt_rows`, `flat_process_list`.

- [ ] **Step 1:** At both the init and the 2s-refresh proc_rows assignments, choose the source by backend. Inside the existing `#[cfg(all(target_os = "linux", feature = "linux-procfs"))]` block (where `process_monitor` is available), after `process_monitor.update()`:

```rust
proc_rows = if crate::cli::backend_shows_only_tt(backend_type) {
    let tt_pids: std::collections::HashSet<i32> =
        flat_process_list(&process_monitor).iter().map(|p| p.pid).collect();
    host_proc_monitor.tt_rows(PROC_PANEL_MAX_ROWS, &tt_pids)
} else {
    host_proc_monitor.rows(PROC_PANEL_MAX_ROWS, &liveness_prober.fresh_verdicts())
};
enrich_proc_rows_tt(&mut proc_rows, &process_monitor);
```
On non-Linux / no-procfs builds, keep the existing `rows(...)` call unchanged (the `tt_rows` path is Linux/procfs-gated). Resolve `Auto` to the effective backend (`backend_type` is already the resolved value in the loop — confirm; if it can still be `Auto`, map it first).

- [ ] **Step 2:** Empty-state — in `render_insights`, when the process list is empty AND the backend is TT-filtering, render one muted line: `"No processes are currently using TT devices"`. (Pass a `bool tt_filtered` into `render_insights`, or reuse the backend predicate at the call site to pick the empty message.)

- [ ] **Step 3:** Build + `-D warnings` + macOS cross-check (real exit codes) + `cargo test`. Manual: on a TT box the panel now lists only TT procs; on `--host`/`--mock` unchanged. **Commit.**

---

### Task 3: `DisplayMode::InferenceMonitor` + full-screen render

**Files:** Modify `src/ui/tui/mod.rs`, `src/ui/tui/inference_panel.rs`.

**Interfaces:** Produces `inference_panel::render_inference_monitor(f: &mut Frame, area: Rect, rows: &[ServiceState], service_cursor: usize)`.

- [ ] **Step 1:** Add `InferenceMonitor` to the `DisplayMode` enum (do NOT add it to the `v`-cycle rotation — it's entered only via `i`).
- [ ] **Step 2:** Add a `render_inference_monitor` fn in `inference_panel.rs` (or a thin ratatui wrapper in `mod.rs` calling `format_service_row`): full-screen titled list ("Inference Servers"), one line per `service_rows(snapshot)` entry via `format_service_row`, the `service_cursor`-th row highlighted, colored by phase (green Ready, yellow Compiling/Loading, red Alarm, gray Down). Add a pure test asserting the highlighted index and that all `SERVERS` appear.
- [ ] **Step 3:** In the render dispatch (`match display_mode`), add a `DisplayMode::InferenceMonitor =>` arm that reads `inference_monitor.snapshot()` (Linux) / empty (non-Linux) and calls the renderer. Remove the `show_inference` overlay from `render_insights` (delete the param + the `inference_h`/`if show_inference` blocks) — the view supersedes it.
- [ ] **Step 4:** Build + tests + clippy + cross-check. **Commit.**

---

### Task 4: `i` enter/exit + `↑/↓` navigation + best-effort pre-focus

**Files:** Modify `src/ui/tui/mod.rs`, `src/ui/tui/inference_panel.rs`.

**Interfaces:** Produces `inference_panel::service_for_selected_process(row: &ProcRow) -> Option<&'static str>`.

- [ ] **Step 1: Failing test for the pre-focus resolver** (best-effort — ProcRow carries no cmdline, so it matches on the inference label against a service key):

```rust
#[test]
fn service_for_selected_process_matches_label_else_none() {
    let mut r = crate::workload::ProcRow {
        pid: 1, name: "python3".into(), cpu_pct: 1.0, mem_bytes: 0,
        inference: Some("vllm"), active: true, tt: None,
    };
    assert_eq!(service_for_selected_process(&r), Some("vllm"));  // label is a SERVERS key
    r.inference = Some("ollama");                                 // not a container service
    assert_eq!(service_for_selected_process(&r), None);
    r.inference = None;
    assert_eq!(service_for_selected_process(&r), None);
}
```

- [ ] **Step 2: Run — fails.**

- [ ] **Step 3: Implement** in `inference_panel.rs`:

```rust
/// Best-effort: map a selected process row to a known inference service key so the
/// monitor can pre-focus it. ProcRow carries no cmdline/model, so we only match its
/// inference label against a SERVERS key; anything else → None (monitor opens
/// unfocused). Precise PID→container linking is out of scope (spec #1).
pub fn service_for_selected_process(row: &crate::workload::ProcRow) -> Option<&'static str> {
    let label = row.inference?;
    SERVERS.iter().find(|d| d.key == label).map(|d| d.key)
}
```

- [ ] **Step 4: Run — passes.**

- [ ] **Step 5: Key handling** in `mod.rs`: replace the `KeyCode::Char('i')|Char('I')` handler (currently `show_inference = !show_inference`) with:
  - If `display_mode != InferenceMonitor`: save `prev_mode = display_mode`; set `display_mode = InferenceMonitor`; set `service_cursor` from `service_for_selected_process(proc_rows.get(process_cursor))` mapped to its index in `service_rows(...)` (else 0).
  - Else (already in it): `display_mode = prev_mode`.
  - Add `KeyCode::Esc` handling in the InferenceMonitor view → `display_mode = prev_mode`.
  - Add `↑`/`↓` when `display_mode == InferenceMonitor` → move `service_cursor` clamped to `0..service_rows.len()`.
  Add `let mut prev_mode = DisplayMode::Insights;` and `let mut service_cursor: usize = 0;` near the other loop state.

- [ ] **Step 6:** Build + `-D warnings` + macOS cross-check (real exit codes) + tests. Manual: `i` enters the monitor from any screen, `↑/↓` moves the highlight, `Esc`/`i` returns; selecting a `vllm` process then `i` pre-focuses vllm. **Commit.**

---

## Self-Review

**Spec coverage:** Part A filter rule → Task 1 predicate + Task 2 wiring; select-from-TT-set (not post-filter top-N) → `select_tt_rows` + `tt_rows` (Task 1); empty state → Task 2 Step 2; mock exempt / host unchanged → predicate. Part B dedicated view → Task 3; `i` enter/exit + `↑/↓` + pre-focus → Task 4; renders on non-Linux from empty snapshot → Task 3 Step 3. ✓

**Placeholder scan:** Tasks 2/3 Steps describe the render/key wiring against exact line anchors and existing patterns (`show_inference`/`render_insights`/the `match display_mode` dispatch already in the tree) rather than pasting the large `render_insights`/key-match bodies verbatim; the pure, error-prone pieces (predicate, `select_tt_rows`, `service_for_selected_process`, `render_inference_monitor` highlight logic) have full code + tests. Acceptable per "follow existing patterns."

**Type consistency:** `backend_shows_only_tt(BackendType) -> bool`, `select_tt_rows(Vec<ProcRow>, &HashSet<i32>, usize)`, `tt_rows(usize, &HashSet<i32>)`, `build_all_rows()`, `render_inference_monitor(&mut Frame, Rect, &[ServiceState], usize)`, `service_for_selected_process(&ProcRow) -> Option<&'static str>`, `DisplayMode::InferenceMonitor`, `prev_mode`, `service_cursor` — consistent across Tasks 1–4.

**Note for the implementer:** confirm the exact `BackendType` variants in `cli.rs` (Task 1 Step 1) and whether `backend_type` in the loop is already resolved from `Auto`.
