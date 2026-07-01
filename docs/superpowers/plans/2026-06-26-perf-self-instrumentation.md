# Perf Self-Instrumentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure the TUI's render cost per screen (headless benchmark) and surface the tool's own live FPS + CPU% (status-bar readout), so perf tuning is data-driven instead of guessed.

**Architecture:** Component A is a `--bench` run mode that renders each `DisplayMode` into a ratatui `TestBackend` for N frames and reports avg/p95 frame time + estimated CPU. Component B is a `PerfMeter` (FPS counter + own-PID CPU via `sysinfo`) shown compactly in the global status bar, toggled by `P`. No new dependencies (TestBackend + sysinfo already present); everything cross-platform.

**Tech Stack:** Rust, ratatui `TestBackend`, `sysinfo` (own-process CPU), existing `src/ui/tui` render fns + `src/animation` visualizations.

## Global Constraints

- No new dependencies (`ratatui` TestBackend and `sysinfo` are already deps).
- Cross-platform (macOS/Linux/Windows); no `#[cfg]`-gated types in the new public surface.
- `--bench` must NOT enter raw mode / require a TTY (it's the headless path).
- Component B adds zero cost when toggled off (don't refresh sysinfo unless enabled).
- `est_cpu_pct = avg_ms * target_fps / 10.0` (percent of one core). Target FPS: Insights & Grid = 10; Starfield, MemoryCastle, MemoryFlow, Arcade = 60; Defrag = 60.
- TDD, frequent commits, zero new warnings.
- Bench frame times are advisory (machine/load-dependent) — tests assert harness structure (a row per mode, no panic), never specific timings.
- Repo facts: visualization constructors/updaters — `HardwareStarfield::new(w,h)` + `initialize_from_devices(&[Device])` + `update_from_telemetry(&dyn TelemetryBackend)`; `MemoryCastle::new(w,h)` + `update(&dyn TelemetryBackend)`; `MemoryFlowVis::new(w,h)` + `update(&dyn TelemetryBackend)`; `ArcadeVisualization::new(w,h)` + `initialize_from_devices` + `initialize_topology(&dyn TelemetryBackend)` + `update(&dyn TelemetryBackend)`; `DefragVis::new(w,h)` + `update(&dyn TelemetryBackend)`. Private render fns in `src/ui/tui/mod.rs`: `ui_visualization(f,&HardwareStarfield,backend)`, `ui_memory_castle(f,&MemoryCastle,backend)`, `ui_memory_flow(f,&MemoryFlowVis,backend)`, `ui_arcade(f,&ArcadeVisualization,backend)`, `ui_defrag(f,&DefragVis,backend)`, `render_grid_mode(f,backend)`, `render_insights(f,backend,&[ProcRow],cursor,Option<&KillConfirmState>,cli,&particles,&baseline,fleet_cursor,fleet_zoom,host_cpu,host_used,host_total,[cfg]&serving)`. `DisplayMode` enum is private to `mod.rs`. Build: `cargo build --bin tt-toplike-tui --features tui`. Tests: `cargo test --lib --tests --features tui`.

---

### Task 1: `--bench` flag + `BenchResult` + pure perf math

**Files:**
- Modify: `src/cli.rs` (add `bench` flag)
- Create: `src/ui/tui/bench.rs` (BenchResult + pure helpers + table formatting)
- Modify: `src/ui/tui/mod.rs` (add `pub mod bench;` under the existing `#[cfg(feature = "tui")]` ui wiring — or in the tui ui module tree)
- Test: inline `#[cfg(test)] mod tests` in `bench.rs`

**Interfaces:**
- Produces:
  - `pub struct BenchResult { pub mode: &'static str, pub frames: usize, pub avg_ms: f64, pub p95_ms: f64, pub target_fps: u32, pub est_cpu_pct: f64 }`
  - `pub fn est_cpu_pct(avg_ms: f64, target_fps: u32) -> f64`
  - `pub fn percentile_ms(samples: &mut [f64], pct: f64) -> f64`
  - `pub fn format_table(results: &[BenchResult]) -> String`

- [ ] **Step 1: Add the `bench` CLI flag**

In `src/cli.rs`, add to the `Cli` struct (near the other bool flags like `print`):
```rust
    /// Headless render benchmark: render each screen into an off-screen buffer
    /// for a fixed number of frames and print per-screen timing + estimated CPU.
    /// Does not enter the TUI. Respects --mock / --host for the backend.
    #[arg(long)]
    pub bench: bool,
```
If `src/cli.rs` has a `tests` module constructing `Cli { ... }` literals, add `bench: false,` to each so they still compile.

- [ ] **Step 2: Write the failing test**

Create `src/ui/tui/bench.rs` with the test module first:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn est_cpu_is_avg_times_fps_over_ten() {
        // 1.0 ms/frame at 60 fps = 60 ms of work per 1000 ms = 6% of one core.
        assert!((est_cpu_pct(1.0, 60) - 6.0).abs() < 1e-9);
        // 0.4 ms at 10 fps = 0.4% .
        assert!((est_cpu_pct(0.4, 10) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn percentile_picks_high_sample() {
        let mut s = vec![1.0, 2.0, 3.0, 4.0, 100.0];
        let p95 = percentile_ms(&mut s, 95.0);
        assert!(p95 >= 4.0, "p95 should be near the top: {p95}");
    }

    #[test]
    fn table_has_a_row_per_result() {
        let rows = vec![
            BenchResult { mode: "insights", frames: 10, avg_ms: 0.4, p95_ms: 0.6, target_fps: 10, est_cpu_pct: 0.4 },
            BenchResult { mode: "arcade", frames: 10, avg_ms: 3.6, p95_ms: 5.0, target_fps: 60, est_cpu_pct: 21.6 },
        ];
        let t = format_table(&rows);
        assert!(t.contains("insights"));
        assert!(t.contains("arcade"));
        assert!(t.lines().count() >= 3); // header + 2 rows
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib --features tui bench`
Expected: FAIL — `cannot find type BenchResult` / functions.

- [ ] **Step 4: Implement the module**

Prepend to `src/ui/tui/bench.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Headless render-cost benchmark types + pure helpers.
//!
//! The harness that actually renders each screen lives in `super`
//! (`run_render_bench`); this module holds the result type and the pure math so
//! it can be unit-tested without a backend or a terminal.

/// One screen's measured render cost.
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub mode: &'static str,
    pub frames: usize,
    pub avg_ms: f64,
    pub p95_ms: f64,
    pub target_fps: u32,
    pub est_cpu_pct: f64,
}

/// Estimated CPU as a percent of one core: ms-per-frame * frames-per-second,
/// expressed as a percentage (ms*fps / 1000 * 100 == ms*fps/10).
pub fn est_cpu_pct(avg_ms: f64, target_fps: u32) -> f64 {
    avg_ms * target_fps as f64 / 10.0
}

/// Linear-interpolation-free percentile: nearest-rank on a sorted copy.
pub fn percentile_ms(samples: &mut [f64], pct: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((pct / 100.0) * (samples.len() as f64 - 1.0)).round() as usize;
    samples[rank.min(samples.len() - 1)]
}

/// Render a fixed-width table of results for stdout.
pub fn format_table(results: &[BenchResult]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<16}{:>8}{:>14}{:>10}{:>12}{:>18}\n",
        "mode", "frames", "avg ms/frame", "p95 ms", "target fps", "est cpu (1 core)"
    ));
    for r in results {
        out.push_str(&format!(
            "{:<16}{:>8}{:>14.3}{:>10.3}{:>12}{:>17.1}%\n",
            r.mode, r.frames, r.avg_ms, r.p95_ms, r.target_fps, r.est_cpu_pct
        ));
    }
    out
}
```
Register the module: in `src/ui/tui/mod.rs`, add near the top `mod bench;` and `pub use bench::BenchResult;` (so `bin/tui.rs` can name the type). If the `ui::tui` module is feature-gated behind `tui`, the `mod bench;` lives inside that gate — match the existing module's gating.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib --features tui bench`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**
```bash
git add src/cli.rs src/ui/tui/bench.rs src/ui/tui/mod.rs
git commit -m "feat(bench): --bench flag, BenchResult, and pure perf math"
```

---

### Task 2: `run_render_bench` harness + wire `--bench`

**Files:**
- Modify: `src/ui/tui/mod.rs` (add `pub fn run_render_bench`)
- Modify: `src/ui/mod.rs` (re-export `run_render_bench` if `run_tui` is re-exported there — match the existing pattern)
- Modify: `src/bin/tui.rs` (dispatch `--bench` before the TUI path)
- Test: integration test in `tests/backend_isolation.rs` (or a new `tests/bench.rs`)

**Interfaces:**
- Consumes: `BenchResult` (Task 1); the private `ui_*`/`render_*` fns; the viz constructors; `ProcRow`/`HostProcessMonitor` (already in crate).
- Produces: `pub fn run_render_bench(backend: &dyn crate::backend::TelemetryBackend, frames: usize, width: u16, height: u16) -> Vec<BenchResult>`

- [ ] **Step 1: Write the failing integration test**

Append to `tests/backend_isolation.rs`:
```rust
#[test]
fn render_bench_produces_a_row_per_mode() {
    use tt_toplike::backend::mock::MockBackend;
    use tt_toplike::backend::TelemetryBackend;
    let mut b = MockBackend::new(3);
    b.init().unwrap();
    b.update().unwrap();

    let results = tt_toplike::ui::run_render_bench(&b, 5, 120, 40);
    // One row for each screen the run loop can show.
    let modes: Vec<&str> = results.iter().map(|r| r.mode).collect();
    for expected in ["insights", "grid", "starfield", "memory-castle", "memory-flow", "arcade", "defrag"] {
        assert!(modes.contains(&expected), "missing bench row for {expected}: {modes:?}");
    }
    // Every row rendered the requested frame count and produced finite timings.
    for r in &results {
        assert_eq!(r.frames, 5);
        assert!(r.avg_ms.is_finite() && r.avg_ms >= 0.0);
        assert!(r.est_cpu_pct.is_finite() && r.est_cpu_pct >= 0.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test backend_isolation --features tui render_bench`
Expected: FAIL — `run_render_bench` not found.

- [ ] **Step 3: Implement `run_render_bench` in `src/ui/tui/mod.rs`**

Add this function (it can call the private `ui_*`/`render_*` fns because it lives in the same module). It mirrors the run loop's construct→update→render per mode:
```rust
/// Headless render benchmark: render each screen into a `TestBackend` for
/// `frames` frames, timing the per-frame update+draw, and return per-mode cost.
/// No raw mode, no TTY — safe to call from `--bench` or tests.
pub fn run_render_bench(
    backend: &dyn TelemetryBackend,
    frames: usize,
    width: u16,
    height: u16,
) -> Vec<bench::BenchResult> {
    use crate::animation::{
        ArcadeVisualization, DefragVis, HardwareStarfield, MemoryCastle, MemoryFlowVis,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::Instant;

    // (label, target_fps, render-closure). Each closure ticks its own viz state
    // and draws one frame into the provided Terminal.
    let mut results = Vec::new();

    // Helper: run `frames` timed iterations of a per-frame closure, build a result.
    fn measure<F: FnMut()>(
        mode: &'static str,
        target_fps: u32,
        frames: usize,
        mut frame: F,
    ) -> bench::BenchResult {
        let mut times = Vec::with_capacity(frames);
        for _ in 0..frames {
            let t = Instant::now();
            frame();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let avg = if times.is_empty() { 0.0 } else { times.iter().sum::<f64>() / times.len() as f64 };
        let p95 = bench::percentile_ms(&mut times.clone(), 95.0);
        bench::BenchResult { mode, frames, avg_ms: avg, p95_ms: p95, target_fps, est_cpu_pct: bench::est_cpu_pct(avg, target_fps) }
    }

    let mk_term = || Terminal::new(TestBackend::new(width, height)).unwrap();

    // Insights (data, 10 fps)
    {
        let mut term = mk_term();
        let mut hpm = crate::workload::HostProcessMonitor::new();
        let particles: std::collections::HashMap<usize, Vec<crate::ui::tui::chip_portrait::Particle>> =
            std::collections::HashMap::new();
        let baseline = crate::animation::baseline::AdaptiveBaseline::new();
        results.push(measure("insights", 10, frames, || {
            hpm.update();
            let rows = hpm.rows(12);
            term.draw(|f| {
                render_insights(
                    f, backend, &rows, 0, None, /*cli*/ &crate::cli::Cli::bench_default(),
                    &particles, &baseline, 0, None, 0.0, 0, 0,
                    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                    &std::collections::HashMap::new(),
                );
            }).unwrap();
        }));
    }

    // Grid (data, 10 fps)
    {
        let mut term = mk_term();
        results.push(measure("grid", 10, frames, || {
            term.draw(|f| render_grid_mode(f, backend)).unwrap();
        }));
    }

    // Starfield (anim, 60 fps)
    {
        let mut term = mk_term();
        let mut sf = HardwareStarfield::new(width as usize, height as usize);
        sf.initialize_from_devices(backend.devices());
        results.push(measure("starfield", 60, frames, || {
            sf.update_from_telemetry(backend);
            term.draw(|f| ui_visualization(f, &sf, backend)).unwrap();
        }));
    }

    // Memory Castle (anim, 60 fps)
    {
        let mut term = mk_term();
        let mut mc = MemoryCastle::new(width as usize, height as usize);
        results.push(measure("memory-castle", 60, frames, || {
            mc.update(backend);
            term.draw(|f| ui_memory_castle(f, &mc, backend)).unwrap();
        }));
    }

    // Memory Flow (anim, 60 fps)
    {
        let mut term = mk_term();
        let mut mf = MemoryFlowVis::new(width as usize, height as usize);
        results.push(measure("memory-flow", 60, frames, || {
            mf.update(backend);
            term.draw(|f| ui_memory_flow(f, &mf, backend)).unwrap();
        }));
    }

    // Arcade (anim, 60 fps)
    {
        let mut term = mk_term();
        let mut arc = ArcadeVisualization::new(width as usize, height as usize);
        arc.initialize_from_devices(backend.devices());
        arc.initialize_topology(backend);
        results.push(measure("arcade", 60, frames, || {
            arc.update(backend);
            term.draw(|f| ui_arcade(f, &arc, backend)).unwrap();
        }));
    }

    // Defrag (60 fps when active)
    {
        let mut term = mk_term();
        let mut dv = DefragVis::new(width as usize, height as usize);
        results.push(measure("defrag", 60, frames, || {
            dv.update(backend);
            term.draw(|f| ui_defrag(f, &dv, backend)).unwrap();
        }));
    }

    results
}
```
Add a tiny test-only/bench-only CLI constructor so `render_insights` can be called without parsing args — in `src/cli.rs`:
```rust
impl Cli {
    /// Minimal defaults for headless rendering (bench / tests). Not for normal use.
    pub fn bench_default() -> Self {
        Cli {
            backend: BackendType::Mock,
            mock: Some(0),
            json: false,
            host: false,
            tt_smi_path: std::path::PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: true,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            bench: true,
            mode: None,
            profile: crate::config::AnimationProfile::Normal,
        }
    }
}
```
(Match the exact current field set of `Cli`; copy it from the `test_default_cli` literal in `src/cli.rs` and set `bench: true`.)

If `src/ui/mod.rs` re-exports `run_tui`, add `pub use tui::run_render_bench;` alongside it so `tt_toplike::ui::run_render_bench` resolves (the test uses that path).

- [ ] **Step 4: Wire `--bench` in `src/bin/tui.rs`**

Near the top of the run logic (after CLI parse + logging init, before the backend/TTY/TUI path), add:
```rust
    if cli.bench {
        // Build the requested backend (respects --mock / --host), sample once.
        let config = /* same BackendConfig the normal path builds */ Default::default();
        let backend_type = cli.effective_backend();
        match tt_toplike::backend::factory::create_backend(backend_type, config, &cli) {
            Ok(mut backend) => {
                let _ = backend.update();
                let results = tt_toplike::ui::run_render_bench(backend.as_ref(), 120, 120, 40);
                print!("{}", tt_toplike::ui::tui::bench::format_table(&results));
            }
            Err(e) => eprintln!("bench: backend init failed: {e}"),
        }
        return;
    }
```
Adapt `config`/imports to match how `src/bin/tui.rs` already builds `BackendConfig` and calls the factory (copy from the existing backend-creation code in that file). Place this dispatch before any `IsTerminal`/raw-mode logic so `--bench` never needs a TTY.

- [ ] **Step 5: Run the integration test**

Run: `cargo test --test backend_isolation --features tui render_bench`
Expected: PASS — a row per mode, 5 frames each.

- [ ] **Step 6: Run the bench for real (sanity) + full build**

Run: `cargo run --bin tt-toplike-tui --features tui -- --bench --mock 4`
Expected: prints the table with finite numbers for all 7 modes.
Run: `cargo build --bin tt-toplike-tui --features tui` → zero warnings.

- [ ] **Step 7: Commit**
```bash
git add src/ui/tui/mod.rs src/ui/mod.rs src/bin/tui.rs src/cli.rs tests/backend_isolation.rs
git commit -m "feat(bench): headless per-screen render benchmark via --bench"
```

---

### Task 3: `PerfMeter` (live FPS + own-process CPU)

**Files:**
- Create: `src/ui/tui/perf.rs`
- Modify: `src/ui/tui/mod.rs` (`mod perf;`)
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct PerfMeter { /* private */ }`
  - `PerfMeter::new() -> Self`
  - `PerfMeter::record_frame(&mut self, render_dur: std::time::Duration)` — call once per drawn frame
  - `PerfMeter::maybe_sample_cpu(&mut self)` — refresh own-PID CPU at most ~1/sec (no-op otherwise)
  - `PerfMeter::status_string(&self) -> String` — e.g. `"9.8fps · 0.4ms · self 1.3%"`
  - `pub(crate) fn fps_from(frames: u32, elapsed_secs: f64) -> f64` (pure, tested)

- [ ] **Step 1: Write the failing test**

Create `src/ui/tui/perf.rs` with the test module first:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_is_frames_over_elapsed() {
        assert!((fps_from(10, 1.0) - 10.0).abs() < 1e-9);
        assert_eq!(fps_from(5, 0.0), 0.0); // guard divide-by-zero
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui perf`
Expected: FAIL — `fps_from` not found.

- [ ] **Step 3: Implement `PerfMeter`**

Prepend to `src/ui/tui/perf.rs`:
```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Live self-instrumentation: actual draw FPS, last frame render time, and the
//! tool's own process CPU%. Off until toggled on, so it costs nothing by default.

use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

pub(crate) fn fps_from(frames: u32, elapsed_secs: f64) -> f64 {
    if elapsed_secs <= 0.0 {
        return 0.0;
    }
    frames as f64 / elapsed_secs
}

pub struct PerfMeter {
    sys: System,
    pid: Option<sysinfo::Pid>,
    window_start: Instant,
    frames_in_window: u32,
    fps: f64,
    last_frame_ms: f64,
    cpu_pct: Option<f32>,
    last_cpu_sample: Instant,
}

impl PerfMeter {
    pub fn new() -> Self {
        PerfMeter {
            sys: System::new(),
            pid: sysinfo::get_current_pid().ok(),
            window_start: Instant::now(),
            frames_in_window: 0,
            fps: 0.0,
            last_frame_ms: 0.0,
            cpu_pct: None,
            last_cpu_sample: Instant::now(),
        }
    }

    /// Record one drawn frame and its render duration; recompute FPS each ~1s.
    pub fn record_frame(&mut self, render_dur: Duration) {
        self.last_frame_ms = render_dur.as_secs_f64() * 1000.0;
        self.frames_in_window += 1;
        let elapsed = self.window_start.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.fps = fps_from(self.frames_in_window, elapsed);
            self.frames_in_window = 0;
            self.window_start = Instant::now();
        }
    }

    /// Refresh own-PID CPU at most ~1/sec. Call each loop iteration when enabled.
    pub fn maybe_sample_cpu(&mut self) {
        if self.last_cpu_sample.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_cpu_sample = Instant::now();
        if let Some(pid) = self.pid {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu(),
            );
            self.cpu_pct = self.sys.process(pid).map(|p| p.cpu_usage());
        }
    }

    pub fn status_string(&self) -> String {
        let cpu = match self.cpu_pct {
            Some(c) => format!("self {:.1}%", c),
            None => "self —".to_string(),
        };
        format!("{:.1}fps · {:.1}ms · {}", self.fps, self.last_frame_ms, cpu)
    }
}

impl Default for PerfMeter {
    fn default() -> Self {
        Self::new()
    }
}
```
Register: in `src/ui/tui/mod.rs`, add `mod perf;` and `pub use perf::PerfMeter;` (match the module's gating).

> Note: confirm the `sysinfo` 0.38 process-refresh + `cpu_usage`/`get_current_pid` API against the installed version (same family used in `workload::host_processes`); adjust calls if signatures differ. Public interface above must not change.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui perf`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/ui/tui/perf.rs src/ui/tui/mod.rs
git commit -m "feat(perf): PerfMeter — live FPS + own-process CPU sampler"
```

---

### Task 4: Wire `PerfMeter` into the run loop + `P` toggle + status bar

**Files:**
- Modify: `src/ui/tui/mod.rs` (own a `PerfMeter`, time the gated draw, `P` hotkey, status-bar string)

**Interfaces:**
- Consumes: `PerfMeter` (Task 3); the existing `render_global_statusbar` (line ~1316) and the gated draw (the `if do_draw { terminal.draw(...) }` block).

- [ ] **Step 1: Add meter state + toggle**

In `run_app`, near the other `let mut` state, add:
```rust
    let mut perf_meter = PerfMeter::new();
    let mut show_perf = false;
```
In the key-event match (Insights/global keys), add a branch (place beside other global toggles like the legend/help keys):
```rust
                            KeyCode::Char('P') => {
                                show_perf = !show_perf;
                                force_redraw = true;
                            }
```

- [ ] **Step 2: Time the draw and feed the meter**

Wrap the gated draw so its duration is recorded. Replace:
```rust
        if do_draw {
        terminal
            .draw(|f| { /* ... */ })
            .map_err(|e| TTTopError::Terminal(e.to_string()))?;
        } // end if do_draw
```
with a timed version:
```rust
        if do_draw {
            let draw_start = Instant::now();
            terminal
                .draw(|f| { /* ...unchanged closure... */ })
                .map_err(|e| TTTopError::Terminal(e.to_string()))?;
            perf_meter.record_frame(draw_start.elapsed());
        } // end if do_draw
        if show_perf {
            perf_meter.maybe_sample_cpu();
        }
```
(Keep the existing draw closure body exactly as-is; only add the timing + the post-draw cpu sample.)

- [ ] **Step 3: Show the meter in the status bar**

`render_global_statusbar(f, backend, display_mode)` can't see `perf_meter`. Pass an optional string: change its signature to
`fn render_global_statusbar(f: &mut Frame, backend: &dyn TelemetryBackend, mode: DisplayMode, perf: Option<&str>)`
and at the right edge of the bar, when `perf` is `Some(s)`, render `s` (dim style). Update the call site inside the draw closure to pass `if show_perf { Some(perf_status.as_str()) } else { None }`, where just before the draw you compute `let perf_status = perf_meter.status_string();` (compute outside the closure to avoid borrow issues; it's a cheap String).

- [ ] **Step 4: Build + manual check**

Run: `cargo build --bin tt-toplike-tui --features tui` → zero warnings.
Run: `cargo test --lib --tests --features tui` → all pass.
Manual (real terminal): `tt-toplike-tui --host`, press `P` → status bar shows `Nfps · N.Nms · self N.N%`; press `P` again → hidden.

- [ ] **Step 5: Commit**
```bash
git add src/ui/tui/mod.rs
git commit -m "feat(perf): P toggles live FPS/self-CPU readout in the status bar"
```

---

## Self-Review

**1. Spec coverage:**
- A headless bench rendering each mode into TestBackend, avg/p95 + est CPU → Tasks 1 (math/result/table) + 2 (harness + `--bench` + dispatch) ✅
- est_cpu formula and per-mode target FPS → Task 1 `est_cpu_pct` + Task 2 per-mode `measure(...)` calls ✅
- TestBackend, no TTY → Task 2 (`run_render_bench` + `--bench` dispatched before TTY path) ✅
- Advisory timings, tests assert structure → Task 2 test asserts rows/frames/finiteness, not values ✅
- B PerfMeter: FPS + own-PID CPU + frame time, off by default, `P` toggle, status bar → Tasks 3 + 4 ✅
- No new deps; cross-platform → uses ratatui TestBackend + sysinfo (both present), no cfg-gated public types ✅
- Testing (bench harness, PerfMeter math) → Tasks 1, 2, 3 ✅

**2. Placeholder scan:** No TBD/TODO. The "adapt config/imports to match bin/tui.rs" and "confirm sysinfo API" notes are concrete reuse/verification steps with the exact target named, not deferred design.

**3. Type consistency:** `BenchResult` fields (Task 1) are used identically in `run_render_bench` and `format_table` (Tasks 1/2). `run_render_bench` signature matches the integration test's call (Task 2). `PerfMeter::{new,record_frame,maybe_sample_cpu,status_string}` (Task 3) are used exactly in Task 4. `Cli::bench_default()` mirrors the real `Cli` field set (must be copied verbatim from the current struct at implementation time — flagged in Task 2 Step 3).

**Known risk (flagged for execution):** Task 2's `run_render_bench` mirrors the run loop's per-mode construct/update/render; the implementer must use the actual current viz constructor calls (e.g. whether the run loop uses `new` vs `new_with_density`) — using `::new(w,h)` measures full standalone density, which is the right "per-mode standalone cost" to report. `Cli::bench_default()` must match the exact `Cli` field set in `src/cli.rs` (copy from `test_default_cli`).
