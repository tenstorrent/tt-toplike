// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Terminal User Interface module
//!
//! This module provides the TUI implementation using Ratatui.
//! It displays real-time telemetry data in a beautiful terminal interface.
//!
//! Supports two display modes:
//! - Normal mode: Traditional table view with real-time telemetry
//! - Visualization mode: Hardware-responsive starfield animation

pub mod bench;
pub use bench::BenchResult;
pub mod chip_portrait;
pub mod perf;
pub use perf::PerfMeter;
pub mod throttle;
pub use throttle::ThrottleState;
pub(crate) mod inference_panel;

use crate::animation::{
    ArcadeVisualization, DefragVis, HardwareStarfield, MemoryCastle, MemoryFlowVis,
};
use crate::backend::{factory, BackendConfig, TelemetryBackend};
use crate::cli::{BackendType, Cli};
use crate::error::TTTopError;
use crate::ui::colors;
use crate::workload::{HostProcessMonitor, ProcRow};
// InferenceEngine + the /proc-based probes are only used on the Linux/TT path,
// so gate the import to match (keeps non-Linux builds warning-clean under -D warnings).
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
use crate::workload::{InferenceEngine, InferenceServerProbe, ProcessMonitor, ServingMetrics};
use crossterm::{
    event::{self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
use libc;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

/// Pending kill confirmation state.
///
/// Cross-platform so the unified Insights render path can take a
/// `Option<&KillConfirmState>` without a `cfg` gate. It is only ever populated
/// by the Linux-gated kill key handlers; off-Linux it stays `None`, so the
/// kill dialog never appears.
#[derive(Debug, Clone)]
struct KillConfirmState {
    pid: i32,
    name: String,
    device_idx: usize,
}

/// UI display mode
#[derive(Debug, Clone, Copy, PartialEq)]
enum DisplayMode {
    /// Insights screen (human-readable inference per chip + process panel)
    Insights,
    /// Full-screen grid of chip portraits for all devices.
    Grid,
    /// Hardware-responsive starfield visualization
    Starfield,
    /// Memory Castle mode (architectural memory hierarchy visualization)
    MemoryCastle,
    /// Memory Flow Topology (full-screen DRAM motion visualization)
    MemoryFlow,
    /// Arcade mode (unified visualization with all three modes + hero character)
    Arcade,
    /// Defrag view — model-loading visualization (inspired by disk defragmenters)
    Defrag,
    /// Dedicated full-screen Inference Server Monitor — entered via `i`/`I`
    /// only (deliberately excluded from the `v`-cycle rotation, see the `v`
    /// key handler below). Supersedes the old `[i]` overlay strip that used
    /// to live inside the Insights screen.
    InferenceMonitor,
}

/// Floating overlay panel type — toggled by hotkeys, auto-dismissed on any other keypress.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OverlayPanel {
    /// Symbol/color legend for the current visualization.
    Legend,
    /// Quick reference card: all keys and commands.
    Help,
    /// Prose description of what the current mode is showing and why.
    Explain,
}

/// Run the TUI application
///
/// This is the main entry point for the TUI. It creates the backend,
/// sets up the terminal, runs the event loop, and cleans up on exit.
///
/// # Arguments
///
/// * `cli` - CLI configuration
/// Max rows shown in the Insights process panel (inference-matched rows are
/// always kept; the rest are the busiest processes filling remaining slots).
const PROC_PANEL_MAX_ROWS: usize = 12;

pub fn run_tui(cli: &Cli) -> Result<(), TTTopError> {
    // Check if we have a TTY available
    if !std::io::stdout().is_terminal() {
        return Err(TTTopError::Terminal(
            "No TTY available. The TUI requires an interactive terminal.\n\
             Try running with an actual terminal (not through SSH without -t, pipes, or redirects)."
                .to_string()
        ));
    }

    // Create backend config
    let config = BackendConfig::new()
        .with_interval(cli.interval)
        .with_max_errors(cli.max_errors);

    let config = if cli.verbose {
        config.verbose()
    } else {
        config
    };

    // Create initial backend
    let backend_type = cli.effective_backend();
    let mut backend =
        factory::create_backend(backend_type, config.clone(), cli).map_err(TTTopError::Backend)?;

    log::info!("TUI started with {:?} backend", backend_type);

    // Disable stderr log output BEFORE entering raw mode. Once the terminal is
    // in raw mode, a stray `\n` from a log line doesn't return the cursor to
    // column 0, so any log emitted between raw-mode-enable and this call would
    // "staircase" diagonally across the screen. Logs still reach the in-app
    // message buffer (shown in the Insights panel); stderr is re-enabled on exit.
    crate::logging::disable_stderr();

    // Install a panic hook that restores the terminal BEFORE the panic prints.
    // Without this, a panic anywhere in the render/event loop unwinds straight
    // past the normal cleanup below, leaving the terminal in raw mode + the
    // alternate screen — which makes every subsequent shell command "staircase"
    // until the user runs `reset`. Restoring here also lets the panic message
    // print in cooked mode so the failure is actually visible.
    {
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableFocusChange);
            crate::logging::enable_stderr();
            original_hook(info);
        }));
    }

    // Setup terminal
    enable_raw_mode().map_err(|e| {
        TTTopError::Terminal(format!(
            "Failed to enable raw mode: {}. \n\
         This usually means the terminal is not properly configured.",
            e
        ))
    })?;
    let mut stdout = io::stdout();
    // Note: Mouse capture disabled - we don't use mouse events and they were causing
    // performance issues (faster animation when mousing over the terminal)
    execute!(stdout, EnterAlternateScreen, EnableFocusChange)
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    let backend_term = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend_term).map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Clear terminal before starting (helps with tmux/Terminal.app rendering)
    terminal
        .clear()
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    // Create app state and run
    let res = run_app(&mut terminal, &mut backend, backend_type, config, cli);

    // Re-enable stderr output before exiting
    crate::logging::enable_stderr();

    // Restore terminal
    disable_raw_mode().map_err(|e| TTTopError::Terminal(e.to_string()))?;
    execute!(
        terminal.backend_mut(),
        DisableFocusChange,
        LeaveAlternateScreen
    )
    .map_err(|e| TTTopError::Terminal(e.to_string()))?;
    terminal
        .show_cursor()
        .map_err(|e| TTTopError::Terminal(e.to_string()))?;

    res
}

/// Main application event loop
fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    backend: &mut Box<dyn TelemetryBackend>,
    mut backend_type: BackendType,
    config: BackendConfig,
    cli: &Cli,
) -> Result<(), TTTopError> {
    let update_interval = Duration::from_millis(cli.interval);
    let mut last_update = Instant::now();

    // Build animation config from CLI --profile flag, merged with any user config file overrides.
    // sensitivity is read at viz construction time and applied to the viz's internal AdaptiveBaseline.
    let anim_cfg = {
        use crate::config::{load_config_overrides, AnimConfig};
        AnimConfig::from_profile(cli.profile).merge(load_config_overrides())
    };

    // UI refresh rates — animation modes target 60 FPS, data modes ~10 FPS.
    // Input polling is always 16 ms so keystrokes are never delayed.
    // No focus throttle — tt-toplike is designed to run alongside other work.
    let mut ui_poll_rate_anim = Duration::from_millis(16); // ~60 FPS for animated modes
    let mut ui_poll_rate_data = Duration::from_millis(100); // ~10 FPS for data modes
    let mut last_anim_render = Instant::now();
    let mut last_data_render = Instant::now();
    // Force an immediate redraw outside the FPS cadence — set on the first frame
    // and after any input event so keystrokes feel instant even on the 10 FPS
    // data screens (where the draw is otherwise gated to the slow tick).
    let mut force_redraw = true;

    // Adaptive frame-rate throttle — both flags default to false, so the
    // default render cadence is unchanged (60 FPS anim, 10 FPS data).
    let mut throttle_state = ThrottleState::new(cli.throttle, cli.idle_on_blur);

    // Live self-instrumentation — off by default, toggled with `P`.
    let mut perf_meter = PerfMeter::new();
    let mut show_perf = false;

    // TT process attribution (Linux-only, update every 2 seconds). This reads
    // /proc to map PIDs → device fds / hugepages and is merged into the
    // cross-platform `proc_rows` by PID via `enrich_proc_rows_tt`.
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    let mut process_monitor = crate::workload::ProcessMonitor::new();

    // Cross-platform process listing for the Insights process panel. Enumerates
    // host processes via sysinfo and tags inference runtimes; on Linux it is
    // enriched by PID with TT device attribution (see the refresh tick).
    let mut host_proc_monitor = HostProcessMonitor::new();
    host_proc_monitor.update();
    // Background liveness prober: confirms server runtimes (vllm, ollama) via
    // their local API off the render path. First cycle has no verdicts yet, so
    // rows() relies on the cheap snapshot tier until the worker reports back.
    let liveness_prober = crate::workload::LivenessProber::spawn();
    liveness_prober.submit(host_proc_monitor.detected_runtimes());
    // Background inference-server monitor: probes detected TT inference-server
    // containers (docker stats + /health) off the render path. Not yet read by
    // any panel — used by the [i] panel in the next task.
    #[cfg(target_os = "linux")]
    let inference_monitor = crate::workload::InferenceServerMonitor::spawn();
    #[cfg(target_os = "linux")]
    inference_monitor.submit(host_proc_monitor.detected_inference_servers());
    // Previous inference snapshot, for detecting the model-unload edge that
    // triggers the Defrag EVICT animation.
    #[cfg(target_os = "linux")]
    let mut prev_inference_snapshot: Vec<crate::workload::inference_server::ServiceState> =
        inference_monitor.snapshot();
    // Multi-model roster lines for the `[i]` view, recomputed each monitor tick
    // (in the InferenceMonitor update branch). Held here so the render pass —
    // which must reserve the same number of rows the snake was sized against —
    // reads the exact same set. Empty for 0/1 detected service.
    let mut inference_roster: Vec<Line<'static>> = Vec::new();
    // Education footer band for the [i] loading view (composed on the inference
    // tick, like `inference_roster`). `band_rotation` advances each recompute; the
    // visible fact changes every `BAND_ROTATE_EVERY` recomputes (~8s at the ~2s
    // inference cadence).
    let mut inference_band: Vec<Line<'static>> = Vec::new();
    let mut band_rotation: usize = 0;
    // Background model-catalog refresher: curl-refreshes the compatibility
    // catalog off the render path; feeds the cold-trail starfield screensaver.
    let catalog_refresher = crate::workload::CatalogRefresher::spawn();
    let mut proc_rows: Vec<ProcRow> =
        host_proc_monitor.rows(PROC_PANEL_MAX_ROWS, &liveness_prober.fresh_verdicts());
    // Linux/TT: seed /proc attribution immediately so a TT-attributed backend
    // (Sysfs/Hybrid/Json/Luwen — anything but --host/--mock, per
    // `backend_shows_only_tt`) shows the TT-filtered process list from the
    // very first frame, not just after the first 2s refresh tick below.
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    {
        process_monitor.update();
        if crate::cli::backend_shows_only_tt(backend_type) {
            let tt_pids: std::collections::HashSet<i32> = flat_process_list(&process_monitor)
                .iter()
                .map(|p| p.pid)
                .collect();
            proc_rows = host_proc_monitor.tt_rows(PROC_PANEL_MAX_ROWS, &tt_pids);
        }
        enrich_proc_rows_tt(&mut proc_rows, &process_monitor);
    }
    let mut last_proc_rows_update = Instant::now();

    // Host CPU / RAM monitoring — sampled on the same 2s cadence as processes.
    // Cross-platform: the bars are useful on macOS too. sysinfo needs two calls
    // separated in time to compute meaningful CPU%; we call it twice at init
    // (with a tiny sleep) so the first frame shows real data rather than zeros.
    let (mut sys_monitor, mut host_cpu_pct, mut host_mem_used, mut host_mem_total) = {
        use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
        let mut s = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        );
        s.refresh_cpu_usage();
        s.refresh_memory();
        // A brief pause lets the kernel accumulate CPU ticks for a real sample.
        std::thread::sleep(Duration::from_millis(200));
        s.refresh_cpu_usage();
        s.refresh_memory();
        let cpu = s.global_cpu_usage();
        let used = s.used_memory();
        let tot = s.total_memory();
        (s, cpu, used, tot)
    };

    // UI state - initialize from CLI --mode option if provided
    let mut display_mode = if let Some(mode) = cli.mode {
        match mode {
            crate::cli::VisualizationMode::Normal => DisplayMode::Insights,
            crate::cli::VisualizationMode::Starfield => DisplayMode::Starfield,
            crate::cli::VisualizationMode::Castle => DisplayMode::MemoryCastle,
            crate::cli::VisualizationMode::Flow => DisplayMode::MemoryFlow,
            crate::cli::VisualizationMode::Arcade => DisplayMode::Arcade,
        }
    } else {
        DisplayMode::Insights // default is now Insights
    };
    let mut starfield: Option<HardwareStarfield> = None;
    let mut memory_castle: Option<MemoryCastle> = None;
    let mut memory_flow: Option<MemoryFlowVis> = None;
    let mut arcade: Option<ArcadeVisualization> = None;
    let mut defrag: Option<DefragVis> = None;
    let mut prev_display_mode = display_mode;
    // The mode to return to when `i`/`I`/`Esc` exits the dedicated
    // `DisplayMode::InferenceMonitor` view — captured at entry so `i` (or
    // `Esc`) always lands back wherever the user came from, not a hardcoded
    // Insights. Only meaningful while `display_mode == InferenceMonitor`.
    let mut prev_mode = DisplayMode::Insights;

    // ── Slash command state ────────────────────────────────────────────────
    // Press '/' from any mode to open the command bar at the bottom of the
    // screen.  Type a command, press Enter to execute, Esc to cancel.
    // The command bar is rendered as an overlay on top of the current view
    // so it works from every mode without mode-specific plumbing.
    let mut cmd_mode = false;
    let mut cmd_buf = String::new();
    let mut cmd_message: Option<(String, bool)> = None; // (text, is_error)

    // `/remote` discovery picker: boxes from the most recent `/remote` (no-arg)
    // discovery, retained so `/remote <n>` can select by list position. Empty
    // until the user runs `/remote`. Only touched by the explicit `/remote`
    // command — never by auto-detect or Tab (additive-safety).
    #[cfg(feature = "remote")]
    let mut remote_boxes: Vec<crate::backend::discovery::DiscoveredBox> = Vec::new();

    // In-flight `/remote` discovery/connect. The blocking work (mDNS discovery,
    // WebSocket connect + init) runs on a worker thread; the loop polls this
    // receiver once per tick with `try_recv()` so the render thread never
    // stalls. `None` when idle. Dropping it (Esc) cancels the wait.
    #[cfg(feature = "remote")]
    let mut pending_remote: Option<std::sync::mpsc::Receiver<RemoteOutcome>> = None;

    // ── `--serve` / `/serve` telemetry publisher (feature = "remote") ──────
    // Holds the in-app publisher, started either right here (a `--serve` flag
    // given at a real TTY — see the auto-start block immediately below) or
    // later via the `/serve` command (see the key-handler dispatch). `None`
    // means "not serving". Dropping it (`/serve off`/`/serve stop`, or simply
    // exiting the TUI) stops the publisher's background accept loop and every
    // connected client's task (see `TelemetryPublisher`'s `Drop`).
    #[cfg(feature = "remote")]
    let mut publisher: Option<crate::backend::TelemetryPublisher> = None;

    // Shared "can't serve" message: a publisher can only broadcast a backend
    // that retains verbatim `tt-smi -s` JSON — see
    // `TelemetryBackend::snapshot_json`'s doc comment for which backends
    // qualify (json/hybrid/ws) and which don't (mock/host/sysfs/luwen, which
    // have nothing to broadcast). Shared by both the `--serve` auto-start
    // below and the `/serve` command dispatch so the wording never drifts
    // between the two call sites.
    #[cfg(feature = "remote")]
    const SERVE_NEEDS_JSON_HYBRID: &str =
        "needs the json or hybrid backend (tt-smi); relaunch with --backend hybrid or --backend json";

    // `--serve` given at a real TTY: start the publisher immediately, inline
    // on this thread. Unlike `/remote`'s discovery (mDNS, seconds), a slow
    // off-thread worker is unnecessary here — `TelemetryPublisher::spawn`
    // binds synchronously and returns fast (a plain `TcpListener::bind` plus
    // spawning its own dedicated Tokio runtime), so there is nothing to hide
    // behind an mpsc worker. `--serve` given WITHOUT a TTY never reaches this
    // function at all — see `bin/tui.rs::run_headless_serve`, which handles
    // that case before the TUI is ever entered.
    #[cfg(feature = "remote")]
    if let Some(spec) = cli.serve.as_deref() {
        // Prime the backend with one `update()` before checking capability —
        // mirroring `run_headless_serve`'s Step-2 priming (bin/tui.rs). `init()`
        // alone does NOT cache a raw JSON frame for JSONBackend/HybridBackend
        // (only `update()` → `apply_raw_snapshot` does), so without this,
        // `snapshot_json()` would read `None` here even for `--backend json`,
        // failing the very check meant to distinguish JSON/Hybrid from
        // Mock/Host/Sysfs. Errors are ignored here — a failed first sample
        // just means the check below reports the same "needs json/hybrid"
        // message it always would if the backend truly lacks a raw snapshot.
        let _ = backend.update();
        if backend.snapshot_json().is_none() {
            cmd_message = Some((format!("--serve {SERVE_NEEDS_JSON_HYBRID}"), true));
        } else {
            match crate::cli::parse_serve_bind(spec).and_then(|addr| {
                crate::backend::TelemetryPublisher::spawn(addr).map_err(|e| e.to_string())
            }) {
                Ok(p) => {
                    cmd_message = Some((
                        format!(
                            "serving on {} — plaintext, unauthed, trusted-LAN only",
                            p.bind_addr()
                        ),
                        false,
                    ));
                    publisher = Some(p);
                }
                Err(e) => {
                    cmd_message = Some((e, true));
                }
            }
        }
    }

    // ── Floating overlay panel state ──────────────────────────────────────
    // Toggled by l (legend), ? (help), ! (explain).
    // Auto-dismissed on any other keypress — stays until explicit re-toggle.
    let mut overlay: Option<OverlayPanel> = None;

    // Portrait particle state — per-device particle list keyed by device index,
    // a shared AdaptiveBaseline (one entry per device), and a monotonic tick counter.
    // Particles are ticked on every Insights-mode draw frame so the animation runs
    // at the full UI rate (~60 FPS) independently of the backend update interval.
    let mut portrait_particles: std::collections::HashMap<
        usize,
        Vec<crate::ui::tui::chip_portrait::Particle>,
    > = std::collections::HashMap::new();
    let mut portrait_baseline = crate::animation::baseline::AdaptiveBaseline::new();
    portrait_baseline.sensitivity = anim_cfg.sensitivity;
    let mut portrait_tick: u64 = 0;

    // Insights mode state
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    let mut inference_engine = InferenceEngine::new();
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    let mut inference_probe = InferenceServerProbe::new();
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    let mut serving_metrics: std::collections::HashMap<i32, ServingMetrics> =
        std::collections::HashMap::new();
    // Cross-platform: the unified render path reads these. They're only mutated
    // by the Linux-gated process-navigation / kill key handlers; off-Linux they
    // stay at their defaults (cursor 0, no kill dialog).
    #[cfg_attr(
        not(all(target_os = "linux", feature = "linux-procfs")),
        allow(unused_mut)
    )]
    let mut process_cursor: usize = 0;
    #[cfg_attr(
        not(all(target_os = "linux", feature = "linux-procfs")),
        allow(unused_mut)
    )]
    let mut kill_confirm: Option<KillConfirmState> = None;

    // Fleet navigation state (Insights with 32+ devices).
    // fleet_cursor: which device cell is highlighted in the galaxy overview.
    // fleet_zoom_start: when Some(n), show portrait grid for devices [n..n+PAGE_SIZE]
    //                   instead of the compact fleet map.
    let mut fleet_cursor: usize = 0;
    let mut fleet_zoom_start: Option<usize> = None;

    // The `[i]` Inference Server Monitor view is one unified creature: it roams
    // the model-catalog starfield when nothing serves, grows the boxed-snake
    // while a model loads, and feeds on live serving telemetry once Ready. All
    // behavior selection, resizing, and the loading→ready burst live inside
    // `Snake` (see `crate::animation::snake`); the loop just feeds it a
    // `SnakeWorld` each tick and hands it the terminal at render time.
    let mut snake = crate::animation::Snake::new();

    loop {
        // Decide whether to advance animations this iteration.
        // nvtop-style trick: input always polls at INPUT_POLL_MS (16 ms) so
        // keystrokes are never delayed.  Viz updates are gated separately so
        // animation modes tick at their target FPS while data modes tick slower.
        // Defrag uses 60 FPS only when devices are active (Running/DMA/Deconstructing);
        // in Idle/Init it drops to the data rate to avoid wasting CPU.
        let defrag_is_animated = defrag.as_ref().map(|d| d.is_animated()).unwrap_or(false);
        let is_anim_mode = matches!(
            display_mode,
            DisplayMode::Starfield
                | DisplayMode::MemoryCastle
                | DisplayMode::MemoryFlow
                | DisplayMode::Arcade
                // The [i] view is a live animated dashboard (snake wave, token
                // exhaust, timeline, coiling loader) — it must redraw at anim
                // cadence, not the 10 FPS data rate, or it reads as frozen.
                | DisplayMode::InferenceMonitor
        ) || (display_mode == DisplayMode::Defrag && defrag_is_animated);
        let render_interval = if is_anim_mode {
            throttle_state.effective_anim_interval(ui_poll_rate_anim)
        } else {
            ui_poll_rate_data
        };
        let should_tick = if is_anim_mode {
            last_anim_render.elapsed() >= render_interval
        } else {
            last_data_render.elapsed() >= render_interval
        };
        if should_tick {
            if is_anim_mode {
                last_anim_render = Instant::now();
            } else {
                last_data_render = Instant::now();
            }
        }

        // Whether to redraw this iteration: on the mode's FPS tick, or once
        // immediately after input. Gating the draw (and the portrait particle
        // tick below) by this is what keeps the data screens at ~10 FPS instead
        // of redrawing the full screen on every 16 ms input-poll wakeup.
        let do_draw = should_tick || std::mem::take(&mut force_redraw);

        // Under `--remote`, prefer the box's actual process list carried on
        // the `tt_toplike` frame extension over this machine's own process
        // scan for the *process panel display*. This is deliberately never
        // substituted into `proc_rows` itself — `proc_rows` also feeds the
        // `/serve` broadcast (`build_extension` below), which must always
        // describe what THIS machine is actually running, independent of
        // what `--remote` happens to be displaying. `None` here means the
        // peer isn't streaming that sub-key (or isn't a `tt-toplike --serve`
        // publisher at all) — fall back to `proc_rows` and let
        // `remote_local_fallback_note` label it as such.
        //
        // Computed once per loop iteration (not only inside `do_draw`) so the
        // kill-dialog handlers further down — which index by `process_cursor`
        // into whichever list is currently on screen — can tell the two apart
        // and refuse to signal a local PID that merely happens to share the
        // highlighted row's index with an unrelated remote process.
        #[cfg(feature = "remote")]
        let remote_proc_rows: Option<Vec<ProcRow>> = if backend_type == BackendType::Remote {
            backend
                .remote_processes()
                .map(|procs| procs.iter().map(remote_proc_to_row).collect())
        } else {
            None
        };
        #[cfg(not(feature = "remote"))]
        let remote_proc_rows: Option<Vec<ProcRow>> = None;
        let has_remote_proc_rows = remote_proc_rows.is_some();
        // If the stream just started delivering remote process rows (e.g. a
        // local→remote panel switch mid-session), drop any kill-confirm
        // dialog that's still open — it would otherwise keep naming an
        // earlier LOCAL pid while remote rows are now on screen (see M1 in
        // the final review). The legitimate local kill path (dialog opened
        // and confirmed while `!has_remote_proc_rows` throughout) is
        // untouched — this only fires once remote rows appear.
        if has_remote_proc_rows {
            kill_confirm = None;
        }
        // Length of whatever the process panel is *actually* showing this
        // iteration — the remote list when present, else `proc_rows` — used
        // to keep cursor clamping in sync with the on-screen rows (see the
        // `Down` handler below). Only consumed by the Linux/procfs cursor
        // navigation code, hence the matching cfg (kept unused otherwise).
        #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
        let displayed_proc_row_len = remote_proc_rows
            .as_ref()
            .map(Vec::len)
            .unwrap_or(proc_rows.len());

        // Initialize or update visualizations (only on tick to match target FPS)
        let size = terminal
            .size()
            .map_err(|e| TTTopError::Terminal(e.to_string()))?;

        if should_tick {
            match display_mode {
                DisplayMode::Starfield => {
                    if starfield.is_none() {
                        let content_h = (size.height as usize).saturating_sub(8);
                        let content_w = (size.width as usize).saturating_sub(2);
                        let mut sf = HardwareStarfield::new(content_w, content_h);
                        sf.initialize_from_devices(backend.devices());
                        sf.set_sensitivity(anim_cfg.sensitivity);
                        starfield = Some(sf);
                    }
                    if let Some(ref mut sf) = starfield {
                        sf.update_from_telemetry(backend);
                    }
                }
                DisplayMode::MemoryCastle => {
                    if memory_castle.is_none() {
                        // Create new MemoryCastle with density scaled by animation profile
                        let glyph_count = ((30.0 * anim_cfg.max_particles_scale) as usize).max(5);
                        let mut mc = MemoryCastle::new_with_density(
                            size.width as usize,
                            size.height as usize,
                            anim_cfg.castle_max_particles(),
                            glyph_count,
                        );
                        mc.set_sensitivity(anim_cfg.sensitivity);
                        memory_castle = Some(mc);
                    }
                    if let Some(ref mut tg) = memory_castle {
                        tg.update(backend);
                    }
                }
                DisplayMode::MemoryFlow => {
                    if memory_flow.is_none() {
                        // Create new MemoryFlow visualization with density scaled by animation profile
                        let mut mf = MemoryFlowVis::new_with_density(
                            size.width as usize,
                            size.height as usize,
                            anim_cfg.flow_max_particles(),
                        );
                        mf.set_sensitivity(anim_cfg.sensitivity);
                        memory_flow = Some(mf);
                    }
                    if let Some(ref mut mf) = memory_flow {
                        mf.update(backend);
                    }
                }
                DisplayMode::Arcade => {
                    // Re-create if size changed — arcade pre-allocates at construction size.
                    if let Some(ref arc) = arcade {
                        if arc.width() != size.width as usize
                            || arc.height() != size.height as usize
                        {
                            arcade = None;
                        }
                    }
                    if arcade.is_none() {
                        // Create new Arcade visualization
                        let mut arc =
                            ArcadeVisualization::new(size.width as usize, size.height as usize);
                        arc.initialize_from_devices(backend.devices());
                        // Forward sensitivity so --profile paranoid (or any non-default profile)
                        // takes effect inside Arcade just like it does in the standalone modes.
                        arc.set_sensitivity(anim_cfg.sensitivity);
                        // Build board topology from SMBUS board_id data so the
                        // header diagram, stream characters, and castle separators
                        // are all topology-aware from the first frame.
                        arc.initialize_topology(backend);
                        arcade = Some(arc);
                    }
                    // Thread the first Ready inference service's serving stats
                    // into the hero ⚔ snake duel. Zero new probing: this only
                    // reads the monitor snapshot already maintained on Linux.
                    // The design is Docker/TT-only, so off-Linux there is never
                    // a serving model — hand the duel None.
                    #[cfg(target_os = "linux")]
                    let serving = {
                        use crate::workload::inference_server::Phase;
                        inference_monitor
                            .snapshot()
                            .iter()
                            .find(|s| s.phase == Phase::Ready)
                            .and_then(|s| s.serving)
                    };
                    #[cfg(not(target_os = "linux"))]
                    let serving: Option<
                        crate::workload::inference_server::ServingStats,
                    > = None;

                    // Under --remote the chips on screen belong to the REMOTE
                    // box, but the serving snapshot is a LOCAL Docker/HTTP probe
                    // of *this* machine. Pitting local serving against remote
                    // silicon in the hero ⚔ duel is incoherent, so suppress the
                    // serving feed under Remote (no-op off-Linux, where it's
                    // already None).
                    let serving = if backend_type == BackendType::Remote {
                        None
                    } else {
                        serving
                    };

                    if let Some(ref mut arc) = arcade {
                        arc.set_serving(serving);
                        arc.update(backend);
                    }
                }
                DisplayMode::Insights => {
                    // Ingestion now happens in the backend update block (runs at ~10 Hz,
                    // not 60 FPS), so nothing to do here.
                }
                DisplayMode::Grid => {
                    // Grid mode doesn't need special init
                }
                DisplayMode::InferenceMonitor => {
                    // Feed the unified snake a fresh view of the world; it owns
                    // behavior selection (roaming/growing/feeding) and the
                    // loading→ready burst internally. The live service rows only
                    // exist on Linux (the design is Docker/TT-only); off Linux we
                    // hand it an empty slice, so it simply roams the catalog.
                    #[cfg(target_os = "linux")]
                    let local_rows = inference_monitor.snapshot();
                    #[cfg(not(target_os = "linux"))]
                    let local_rows: Vec<
                        crate::workload::inference_server::ServiceState,
                    > = Vec::new();
                    // Under `--remote`, prefer the box's authoritative inference
                    // data carried on the `tt_toplike` frame extension over this
                    // machine's own Docker probe. `None` means the peer isn't
                    // streaming that sub-key (or isn't a `tt-toplike --serve`
                    // publisher at all) — fall back to `local_rows` and let
                    // `remote_local_fallback_note` below label it as such.
                    // (The non-`remote`-feature arm skips the `Option`
                    // round-trip entirely rather than hard-coding a `None` to
                    // `unwrap_or` — clippy flags an unwrap of a statically-known
                    // `None` as dead code.)
                    #[cfg(feature = "remote")]
                    let (rows, has_remote_rows): (
                        Vec<crate::workload::inference_server::ServiceState>,
                        bool,
                    ) = {
                        let remote_rows = if backend_type == BackendType::Remote {
                            backend.remote_inference().map(|list| {
                                list.iter().map(remote_inference_to_service_state).collect()
                            })
                        } else {
                            None
                        };
                        let has_remote_rows = remote_rows.is_some();
                        (remote_rows.unwrap_or(local_rows), has_remote_rows)
                    };
                    #[cfg(not(feature = "remote"))]
                    let (rows, has_remote_rows): (
                        Vec<crate::workload::inference_server::ServiceState>,
                        bool,
                    ) = (local_rows, false);
                    let catalog = catalog_refresher.snapshot();
                    let arch = backend
                        .devices()
                        .first()
                        .map(|d| d.architecture.name())
                        // Matches Architecture::name()'s "Unknown" for a
                        // detected-but-unknown device (consistent footer casing).
                        .unwrap_or("Unknown");
                    // Per-chip telemetry snapshot for the Feeding silicon strip:
                    // one reading per detected device, with power/temp/clock when
                    // the backend exposes them (all `None` otherwise).
                    let chips: Vec<crate::animation::ChipReading> = backend
                        .devices()
                        .iter()
                        .map(|d| {
                            let t = backend.telemetry(d.index);
                            crate::animation::ChipReading {
                                index: d.index,
                                arch: d.architecture.name(),
                                power_w: t.and_then(|t| t.power),
                                temp_c: t.and_then(|t| t.asic_temperature),
                                aiclk_mhz: t.and_then(|t| t.aiclk),
                            }
                        })
                        .collect();
                    // Multi-model roster (empty for <2 services). The snake gets
                    // the remaining height so `render_snake_view` can reserve the
                    // same rows for the roster and the fixed-dim loading/roaming
                    // renderers line up.
                    inference_roster = inference_roster_lines(&rows, size.width as usize);
                    // Under `--remote`, while `remote_rows` is `None` (the peer
                    // isn't streaming inference data) this view is still
                    // probing LOCAL docker, not the remote box's serving stack.
                    // Prepend a dim one-line banner so that's obvious, computed
                    // here (not in `render_snake_view`) so the extra row is
                    // counted in `roster_h` below and the snake's reserved
                    // height stays in sync with what actually gets drawn.
                    if let Some(note) = remote_local_fallback_note(backend_type, has_remote_rows) {
                        inference_roster.insert(
                            0,
                            Line::from(Span::styled(
                                note,
                                Style::default()
                                    .fg(colors::text_secondary())
                                    .add_modifier(Modifier::DIM),
                            )),
                        );
                    }
                    let roster_h = inference_roster.len();
                    // Education band: describe the featured *loading* service.
                    band_rotation = band_rotation.wrapping_add(1);
                    inference_band = {
                        use crate::workload::inference_server::{education, Phase};
                        let featured = inference_panel::featured_loading(&rows);
                        match featured {
                            Some(s)
                                if matches!(
                                    s.phase,
                                    Phase::Compiling | Phase::Loading | Phase::Alarm
                                ) =>
                            {
                                let avail = (size.height as usize)
                                    .saturating_sub(1 + roster_h + 3) // status + roster + min snake
                                    .min(INFERENCE_BAND_MAX_H);
                                education::compose_band(
                                    &s.label,
                                    s.phase,
                                    size.width as usize,
                                    avail,
                                    band_rotation / BAND_ROTATE_EVERY,
                                )
                            }
                            _ => Vec::new(),
                        }
                    };
                    let band_h = inference_band.len();
                    snake.update(&crate::animation::SnakeWorld {
                        rows: &rows,
                        catalog: &catalog,
                        arch,
                        chips: &chips,
                        cadence_secs: crate::workload::inference_server::CADENCE_SECS,
                        width: size.width as usize,
                        height: (size.height as usize).saturating_sub(1 + roster_h + band_h),
                    });
                }
                DisplayMode::Defrag => {
                    if defrag.is_none() {
                        defrag = Some(DefragVis::new(size.width as usize, size.height as usize));
                    }
                    if let Some(ref mut dv) = defrag {
                        dv.update(backend);
                    }
                }
            }
        } // end if should_tick

        // Clear terminal when switching modes to remove artifacts
        if display_mode != prev_display_mode {
            terminal.clear().ok();
            prev_display_mode = display_mode;
        }

        // Tick portrait particles once per UI frame when on the Insights screen.
        // This must happen *before* the immutable borrow inside terminal.draw().
        if do_draw && display_mode == DisplayMode::Insights {
            use crate::ui::tui::chip_portrait::tick_particles;
            portrait_tick = portrait_tick.wrapping_add(1);
            let devices_snapshot: Vec<_> = backend.devices().to_vec();
            for device in &devices_snapshot {
                let plist = portrait_particles.entry(device.index).or_default();
                if let Some(telem) = backend.telemetry(device.index) {
                    let smbus = backend.smbus_telemetry(device.index);
                    tick_particles(
                        plist,
                        telem,
                        smbus,
                        device.architecture,
                        &mut portrait_baseline,
                        device.index,
                        portrait_tick,
                        anim_cfg.portrait_max_particles(),
                    );
                }
            }
        }

        // Draw UI based on mode — only on a render tick (or just after input).
        // On data screens this is ~10 FPS; without this gate the full screen was
        // redrawn on every 16 ms input-poll wakeup (~60 FPS), wasting CPU.
        if do_draw {
            // Sample the perf meter (own CPU at most ~1/s) only when the readout
            // is toggled on, so it costs nothing by default. `perf_status` must
            // outlive the draw closure, so compute it here.
            if show_perf {
                perf_meter.maybe_sample_cpu();
            }
            let perf_status = if show_perf {
                Some(perf_meter.status_string())
            } else {
                None
            };
            let perf_arg = perf_status.as_deref();
            // Compute throttle hint before the draw closure (borrow safety: status_hint()
            // returns Option<&'static str> so no lifetime issue, but keep pattern consistent
            // with perf_arg to avoid any future borrow conflicts inside the closure).
            let throttle_hint = throttle_state.status_hint();
            // `/serve` status indicator: `◉ serving :PORT · N clients` while a
            // publisher is held, absent otherwise. Built as an owned `String`
            // (rather than borrowed inside the draw closure) for the same
            // borrow-safety/consistency reasons as `perf_arg`/`throttle_hint`
            // above — `client_count()` also changes every tick, so this is
            // recomputed fresh each draw rather than cached.
            #[cfg(feature = "remote")]
            let serve_status = publisher.as_ref().map(|p| {
                format!(
                    "◉ serving :{} · {} clients",
                    p.bind_addr().port(),
                    p.client_count()
                )
            });
            #[cfg(feature = "remote")]
            let serve_hint = serve_status.as_deref();
            #[cfg(not(feature = "remote"))]
            let serve_hint: Option<&str> = None;
            // Whether `proc_rows` above was actually built via the TT-filtered
            // path this cycle — only possible on Linux/procfs builds (see the
            // proc_rows assignments). Drives the process panel's empty-state
            // message so "no TT processes" reads differently from "no host
            // processes at all".
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            let tt_filtered = crate::cli::backend_shows_only_tt(backend_type);
            #[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
            let tt_filtered = false;
            // Honest-labeling note for the process panel: `Some` only while
            // `remote_proc_rows` (computed above) is `None` under `--remote` —
            // i.e. the list below is still LOCAL processes, not the remote
            // box's. See `remote_local_fallback_note`.
            let remote_note = remote_local_fallback_note(backend_type, has_remote_proc_rows);
            // The rows the panel actually renders this frame: the remote list
            // when present, else the local scan.
            let display_proc_rows: &[ProcRow] = remote_proc_rows.as_deref().unwrap_or(&proc_rows);
            let draw_start = Instant::now();
            terminal
                .draw(|f| {
                    // Clear frame with explicit black background for tmux compatibility
                    f.render_widget(
                        Block::default().style(Style::default().bg(colors::rgb(0, 0, 0))),
                        f.area(),
                    );

                    match display_mode {
                        DisplayMode::Insights => {
                            render_insights(
                                f,
                                backend,
                                display_proc_rows,
                                process_cursor,
                                kill_confirm.as_ref(),
                                cli,
                                &portrait_particles,
                                &portrait_baseline,
                                fleet_cursor,
                                fleet_zoom_start,
                                host_cpu_pct,
                                host_mem_used,
                                host_mem_total,
                                tt_filtered,
                                remote_note,
                                #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                                &serving_metrics,
                            );
                        }
                        DisplayMode::Grid => {
                            render_grid_mode(f, backend);
                        }
                        DisplayMode::InferenceMonitor => {
                            render_snake_view(f, &snake, &inference_roster, &inference_band);
                        }
                        DisplayMode::Starfield => {
                            if let Some(ref sf) = starfield {
                                ui_visualization(f, sf, backend);
                            }
                        }
                        DisplayMode::MemoryCastle => {
                            if let Some(ref tg) = memory_castle {
                                ui_memory_castle(f, tg, backend);
                            }
                        }
                        DisplayMode::MemoryFlow => {
                            if let Some(ref mf) = memory_flow {
                                ui_memory_flow(f, mf, backend);
                            }
                        }
                        DisplayMode::Arcade => {
                            if let Some(ref arc) = arcade {
                                ui_arcade(f, arc, backend);
                            }
                        }
                        DisplayMode::Defrag => {
                            if let Some(ref dv) = defrag {
                                ui_defrag(f, dv, backend);
                            }
                        }
                    }

                    // ── Global status bar (all modes) ─────────────────────────
                    render_global_statusbar(
                        f,
                        backend,
                        display_mode,
                        perf_arg,
                        throttle_hint,
                        serve_hint,
                    );

                    // ── Command bar overlay (all modes) ───────────────────────
                    if cmd_mode || cmd_message.is_some() {
                        let msg_ref = cmd_message.as_ref().map(|(s, e)| (s.as_str(), *e));
                        render_command_bar(f, &cmd_buf, msg_ref);
                    }

                    // ── Floating overlay panel (legend / help / explain) ───────
                    if let Some(kind) = overlay {
                        render_overlay_panel(f, kind, display_mode);
                    }
                })
                .map_err(|e| TTTopError::Terminal(e.to_string()))?;
            perf_meter.record_frame(draw_start.elapsed());
        } // end if do_draw

        // Input always polls at 16 ms so keystrokes respond immediately.
        let input_poll = Duration::from_millis(16);
        if event::poll(input_poll).map_err(|e| TTTopError::Terminal(e.to_string()))? {
            // Any input event redraws on the next iteration, regardless of the
            // FPS cadence, so the screen reflects the keystroke immediately.
            force_redraw = true;
            match event::read().map_err(|e| TTTopError::Terminal(e.to_string()))? {
                Event::FocusGained => {
                    throttle_state.focused = true;
                }
                Event::FocusLost => {
                    throttle_state.focused = false;
                }
                Event::Resize(_, _) => {
                    // Drop all size-dependent visualizations so they reinitialize
                    // at the new dimensions on the next loop iteration.
                    starfield = None;
                    memory_castle = None;
                    memory_flow = None;
                    arcade = None;
                    defrag = None;
                    terminal.clear().ok();
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // ── Command mode intercepts all keystrokes ─────────────────
                    if cmd_mode {
                        match key.code {
                            KeyCode::Esc => {
                                cmd_mode = false;
                                cmd_buf.clear();
                                cmd_message = None;
                            }
                            KeyCode::Enter => {
                                // `/remote` is dispatched here (not in
                                // execute_command) because connecting needs the
                                // live backend/backend_type, which execute_command
                                // doesn't get. It also runs OFF the render thread
                                // (see spawn_remote_worker) so mDNS discovery and
                                // the WebSocket connect never freeze the UI — the
                                // outcome is applied later when the worker reports
                                // back (see the pending_remote poll below).
                                // Everything else flows through execute_command.
                                let verb = cmd_buf
                                    .trim()
                                    .trim_start_matches('/')
                                    .split_whitespace()
                                    .next()
                                    .unwrap_or("")
                                    .to_lowercase();

                                if verb == "remote" {
                                    #[cfg(feature = "remote")]
                                    {
                                        let arg = cmd_buf
                                            .trim()
                                            .trim_start_matches('/')
                                            .split_once(char::is_whitespace)
                                            .map(|(_, rest)| rest)
                                            .unwrap_or("")
                                            .trim()
                                            .to_string();
                                        // Show progress immediately; the worker
                                        // replaces this when it finishes.
                                        cmd_message = Some((
                                            if arg.is_empty() {
                                                "discovering boxes…".to_string()
                                            } else {
                                                format!("connecting to {}…", arg)
                                            },
                                            false,
                                        ));
                                        pending_remote = Some(spawn_remote_worker(
                                            arg,
                                            remote_boxes.clone(),
                                            config.clone(),
                                        ));
                                    }
                                    #[cfg(not(feature = "remote"))]
                                    {
                                        cmd_message = Some((
                                            "remote unavailable (built without the 'remote' feature)"
                                                .to_string(),
                                            true,
                                        ));
                                    }
                                } else if verb == "serve" {
                                    // `/serve` is dispatched here (not in
                                    // execute_command) for the same reason
                                    // `/remote` is: starting/stopping needs the
                                    // live backend (for `snapshot_json()`) and
                                    // mutates the loop's `publisher` slot,
                                    // neither of which execute_command has
                                    // access to. Unlike `/remote` this runs
                                    // INLINE (no off-thread worker) because
                                    // `TelemetryPublisher::spawn` binds
                                    // synchronously and returns fast — see the
                                    // `--serve` auto-start comment above for
                                    // why that's safe on the render thread.
                                    #[cfg(feature = "remote")]
                                    {
                                        let arg = cmd_buf
                                            .trim()
                                            .trim_start_matches('/')
                                            .split_once(char::is_whitespace)
                                            .map(|(_, rest)| rest)
                                            .unwrap_or("")
                                            .trim()
                                            .to_string();
                                        match parse_serve_command(&arg) {
                                            ServeCmd::Stop => {
                                                cmd_message = Some((
                                                    if publisher.take().is_some() {
                                                        "stopped serving".to_string()
                                                    } else {
                                                        "not serving".to_string()
                                                    },
                                                    false,
                                                ));
                                            }
                                            ServeCmd::Bad(reason) => {
                                                cmd_message = Some((reason, true));
                                            }
                                            ServeCmd::Start(spec) => {
                                                if let Some(existing) = publisher.as_ref() {
                                                    cmd_message = Some((
                                                        format!(
                                                            "already serving on {}",
                                                            existing.bind_addr()
                                                        ),
                                                        false,
                                                    ));
                                                } else if backend.snapshot_json().is_none() {
                                                    cmd_message = Some((
                                                        format!(
                                                            "/serve: broadcast {SERVE_NEEDS_JSON_HYBRID}"
                                                        ),
                                                        true,
                                                    ));
                                                } else {
                                                    let bind_spec = spec.unwrap_or_else(|| {
                                                        format!(
                                                            "0.0.0.0:{}",
                                                            crate::cli::DEFAULT_SERVE_PORT
                                                        )
                                                    });
                                                    match crate::cli::parse_serve_bind(&bind_spec)
                                                        .and_then(|addr| {
                                                            crate::backend::TelemetryPublisher::spawn(
                                                                addr,
                                                            )
                                                            .map_err(|e| e.to_string())
                                                        }) {
                                                        Ok(p) => {
                                                            cmd_message = Some((
                                                                format!(
                                                                    "serving on {} — plaintext, unauthed, trusted-LAN only",
                                                                    p.bind_addr()
                                                                ),
                                                                false,
                                                            ));
                                                            publisher = Some(p);
                                                        }
                                                        Err(e) => {
                                                            cmd_message = Some((e, true));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    #[cfg(not(feature = "remote"))]
                                    {
                                        cmd_message = Some((
                                            "serve unavailable (built without the 'remote' feature)"
                                                .to_string(),
                                            true,
                                        ));
                                    }
                                } else {
                                    let (msg, is_err, should_quit) = execute_command(
                                        &cmd_buf,
                                        &mut display_mode,
                                        &mut ui_poll_rate_anim,
                                        &mut ui_poll_rate_data,
                                        anim_cfg.sensitivity,
                                        &mut overlay,
                                        &mut throttle_state,
                                    );
                                    if should_quit {
                                        return Ok(());
                                    }
                                    // Empty message (e.g. /help opens overlay) →
                                    // don't leave a blank command bar on screen.
                                    cmd_message = if msg.is_empty() {
                                        None
                                    } else {
                                        Some((msg, is_err))
                                    };
                                }
                                cmd_mode = false;
                                cmd_buf.clear();
                            }
                            KeyCode::Backspace => {
                                cmd_buf.pop();
                            }
                            KeyCode::Char(c) => {
                                cmd_buf.push(c);
                            }
                            _ => {}
                        }
                    } else {
                        // Any keypress dismisses an open overlay, except the three
                        // toggle hotkeys which flip it (handled explicitly below).
                        // We reset here so every arm below starts from a clean slate;
                        // the toggle arms then set it back to the desired panel.
                        let prior_overlay = overlay.take();

                        match key.code {
                            // ── Overlay toggle hotkeys (l / ? / !) ─────────────────
                            KeyCode::Char('l') => {
                                overlay = if prior_overlay == Some(OverlayPanel::Legend) {
                                    None
                                } else {
                                    Some(OverlayPanel::Legend)
                                };
                            }
                            KeyCode::Char('?') => {
                                overlay = if prior_overlay == Some(OverlayPanel::Help) {
                                    None
                                } else {
                                    Some(OverlayPanel::Help)
                                };
                            }
                            KeyCode::Char('!') => {
                                overlay = if prior_overlay == Some(OverlayPanel::Explain) {
                                    None
                                } else {
                                    Some(OverlayPanel::Explain)
                                };
                            }
                            KeyCode::Char('P') => {
                                show_perf = !show_perf;
                                force_redraw = true;
                            }
                            KeyCode::Char('i') | KeyCode::Char('I') => {
                                // Toggle between the dedicated Inference Server Monitor and
                                // Insights: `i` from anywhere opens the monitor (remembering
                                // where we came from), and `i` while in the monitor lands on
                                // Insights specifically — the two pair as a quick back-and-forth.
                                // (Esc, by contrast, backs out to `prev_mode` — wherever you
                                // came from — see the Esc handler.)
                                if display_mode != DisplayMode::InferenceMonitor {
                                    prev_mode = display_mode;
                                    display_mode = DisplayMode::InferenceMonitor;
                                } else {
                                    display_mode = DisplayMode::Insights;
                                }
                                force_redraw = true;
                            }
                            KeyCode::Char('/') => {
                                // Open command bar — clears any previous result message
                                cmd_mode = true;
                                cmd_buf.clear();
                                cmd_message = None;
                                // overlay already cleared above
                            }
                            KeyCode::Char('q') => {
                                return Ok(());
                            }
                            KeyCode::Esc => {
                                // Cancel an in-flight /remote wait first: drop the
                                // receiver so the worker's send fails harmlessly
                                // (it finishes on its own), then clear the
                                // "discovering…/connecting…" status line.
                                #[cfg(feature = "remote")]
                                if pending_remote.take().is_some() {
                                    cmd_message = Some(("remote cancelled".to_string(), false));
                                    // Handled — skip the dismiss chain below.
                                    force_redraw = true;
                                    continue;
                                }
                                // Dismiss command message, zoom, kill confirm (in that order).
                                // Overlays are already dismissed by the take() above.
                                if prior_overlay.is_some() {
                                    // Was showing overlay — already cleared, nothing else to do.
                                } else if cmd_message.is_some() {
                                    cmd_message = None;
                                } else if fleet_zoom_start.is_some() {
                                    // Zoom out from portrait drill-down back to galaxy overview.
                                    fleet_zoom_start = None;
                                } else if display_mode == DisplayMode::InferenceMonitor {
                                    // Back out of the dedicated Inference Server Monitor
                                    // view to wherever the user came from (see the
                                    // `i`/`I` handler above) rather than quitting.
                                    display_mode = prev_mode;
                                } else {
                                    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                                    if kill_confirm.is_some() {
                                        kill_confirm = None;
                                    } else {
                                        return Ok(());
                                    }
                                    #[cfg(not(all(
                                        target_os = "linux",
                                        feature = "linux-procfs"
                                    )))]
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('r') => {
                                // Force refresh
                                if let Err(e) = backend.update() {
                                    log::warn!("Update failed: {}", e);
                                }
                            }
                            KeyCode::Char('v') => {
                                // Cycle through visualization modes.
                                // Grid mode is intentionally excluded — the Insights screen
                                // already is the chip-portrait grid.
                                display_mode = match display_mode {
                                    DisplayMode::Insights | DisplayMode::Grid => {
                                        DisplayMode::MemoryFlow
                                    }
                                    DisplayMode::MemoryFlow => DisplayMode::Starfield,
                                    DisplayMode::Starfield => {
                                        memory_castle = None;
                                        DisplayMode::MemoryCastle
                                    }
                                    DisplayMode::MemoryCastle => {
                                        arcade = None;
                                        DisplayMode::Arcade
                                    }
                                    DisplayMode::Arcade => DisplayMode::Defrag,
                                    // Defrag and the Inference Server Monitor round out the
                                    // rotation just before it wraps back to Insights, so both
                                    // are reachable by cycling with `v` (the monitor is also
                                    // still a direct toggle via `i`).
                                    DisplayMode::Defrag => DisplayMode::InferenceMonitor,
                                    DisplayMode::InferenceMonitor => DisplayMode::Insights,
                                };
                                log::info!("Switched to {:?} mode", display_mode);
                            }
                            KeyCode::Char('d') | KeyCode::Char('D') => {
                                // Jump directly to Defrag mode
                                defrag = None; // reinit at new frame
                                display_mode = DisplayMode::Defrag;
                                log::info!("Switched directly to Defrag mode");
                            }
                            KeyCode::Char('g') => {
                                display_mode = DisplayMode::Grid;
                                log::info!("Switched to Grid mode");
                            }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                // Jump directly to Arcade mode
                                arcade = None; // Reset arcade to reinitialize
                                display_mode = DisplayMode::Arcade;
                                log::info!("Switched directly to Arcade mode");
                            }
                            KeyCode::Char('b') => {
                                // Switch to next backend
                                log::info!("Attempting to switch from {:?} backend", backend_type);

                                match factory::switch_to_next_backend(
                                    backend_type,
                                    config.clone(),
                                    cli,
                                ) {
                                    Ok((new_backend, new_type)) => {
                                        *backend = new_backend;
                                        backend_type = new_type;
                                        log::info!(
                                            "Successfully switched to {:?} backend",
                                            backend_type
                                        );

                                        // Reinitialize visualizations with new backend
                                        if let DisplayMode::Starfield = display_mode {
                                            starfield = None;
                                        }
                                        if let DisplayMode::MemoryCastle = display_mode {
                                            memory_castle = None;
                                        }
                                        if let DisplayMode::MemoryFlow = display_mode {
                                            memory_flow = None;
                                        }
                                        if let DisplayMode::Arcade = display_mode {
                                            arcade = None;
                                        }
                                        if let DisplayMode::Defrag = display_mode {
                                            defrag = None;
                                        }
                                        // Drop any fleet drill-down: the new backend
                                        // may expose a different (smaller) device count.
                                        fleet_zoom_start = None;
                                        fleet_cursor = 0;
                                    }
                                    Err(e) => {
                                        log::error!("Failed to switch backend: {}", e);
                                    }
                                }
                            }
                            // Insights-mode navigation, fleet drill-down, and kill
                            KeyCode::Up if display_mode == DisplayMode::Insights => {
                                let n = backend.devices().len();
                                if n >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
                                    // Move cursor up one row in the galaxy overview.
                                    let cells_per_row = (size.width / FLEET_CELL_W).max(1) as usize;
                                    fleet_cursor = fleet_cursor.saturating_sub(cells_per_row);
                                } else {
                                    // Normal process-list navigation.
                                    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                                    if kill_confirm.is_none() {
                                        process_cursor = process_cursor.saturating_sub(1);
                                    }
                                }
                            }
                            KeyCode::Down if display_mode == DisplayMode::Insights => {
                                let n = backend.devices().len();
                                if n >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
                                    let cells_per_row = (size.width / FLEET_CELL_W).max(1) as usize;
                                    fleet_cursor =
                                        (fleet_cursor + cells_per_row).min(n.saturating_sub(1));
                                } else {
                                    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                                    if kill_confirm.is_none() {
                                        // Clamp to `displayed_proc_row_len` — the SAME list the
                                        // panel renders this iteration (remote rows under
                                        // `--remote` when present, else `proc_rows`) — so the
                                        // cursor always matches the highlighted row.
                                        let max = displayed_proc_row_len.saturating_sub(1);
                                        process_cursor = (process_cursor + 1).min(max);
                                    }
                                }
                            }
                            KeyCode::Left if display_mode == DisplayMode::Insights => {
                                let n = backend.devices().len();
                                if n >= FLEET_DEVICE_THRESHOLD {
                                    if let Some(start) = fleet_zoom_start {
                                        // Page back in zoomed portrait view.
                                        fleet_zoom_start =
                                            Some(start.saturating_sub(FLEET_PAGE_SIZE));
                                        fleet_cursor = fleet_zoom_start.unwrap();
                                    } else {
                                        // Move cursor left one cell.
                                        fleet_cursor = fleet_cursor.saturating_sub(1);
                                    }
                                }
                            }
                            KeyCode::Right if display_mode == DisplayMode::Insights => {
                                let n = backend.devices().len();
                                if n >= FLEET_DEVICE_THRESHOLD {
                                    if let Some(start) = fleet_zoom_start {
                                        // Page forward in zoomed portrait view.
                                        let new =
                                            (start + FLEET_PAGE_SIZE).min(n.saturating_sub(1));
                                        fleet_zoom_start = Some(new);
                                        fleet_cursor = new;
                                    } else {
                                        fleet_cursor = (fleet_cursor + 1).min(n.saturating_sub(1));
                                    }
                                }
                            }
                            KeyCode::Enter if display_mode == DisplayMode::Insights => {
                                let n = backend.devices().len();
                                if n >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
                                    // Zoom into portrait view for the page containing cursor.
                                    let page_start =
                                        (fleet_cursor / FLEET_PAGE_SIZE) * FLEET_PAGE_SIZE;
                                    fleet_zoom_start = Some(page_start);
                                } else {
                                    // Kill-dialog confirmation.
                                    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                                    if let Some(ref kc) = kill_confirm {
                                        let _ = crate::workload::process_monitor::kill_pid(
                                            kc.pid,
                                            libc::SIGTERM,
                                        );
                                        kill_confirm = None;
                                    }
                                }
                            }
                            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                            KeyCode::Char('k') if display_mode == DisplayMode::Insights => {
                                // Never under a remote-sourced panel: `proc_rows` (the only
                                // list `kill_pid` can act on — it signals a LOCAL PID) would
                                // diverge from what's highlighted on screen, so `process_cursor`
                                // could pick an unrelated local process by coincidence of index.
                                if kill_confirm.is_none() && !has_remote_proc_rows {
                                    // Use proc_rows (the rendered list) so the kill target
                                    // always matches the highlighted row — not flat_process_list
                                    // (PID-ascending, uncapped) which diverges from the display.
                                    if let Some(row) = proc_rows.get(process_cursor) {
                                        kill_confirm = Some(KillConfirmState {
                                            pid: row.pid,
                                            name: row.name.clone(),
                                            device_idx: row
                                                .tt
                                                .as_ref()
                                                .and_then(|t| t.device_indices.first().copied())
                                                .unwrap_or(0),
                                        });
                                    }
                                }
                            }
                            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                            KeyCode::Char('K') if display_mode == DisplayMode::Insights => {
                                if let Some(ref kc) = kill_confirm {
                                    // Inside dialog: SIGKILL and close
                                    let _ = crate::workload::process_monitor::kill_pid(
                                        kc.pid,
                                        libc::SIGKILL,
                                    );
                                    kill_confirm = None;
                                } else if !has_remote_proc_rows {
                                    // Outside dialog: SIGKILL immediately using proc_rows
                                    // (the rendered list) so highlight == kill target. Guarded
                                    // the same way as `k` above — never act on a local PID
                                    // while a remote-sourced list is what's actually on screen.
                                    if let Some(row) = proc_rows.get(process_cursor) {
                                        let _ = crate::workload::process_monitor::kill_pid(
                                            row.pid,
                                            libc::SIGKILL,
                                        );
                                    }
                                }
                            }
                            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                            KeyCode::Char('y') if display_mode == DisplayMode::Insights => {
                                if let Some(ref kc) = kill_confirm {
                                    let _ = crate::workload::process_monitor::kill_pid(
                                        kc.pid,
                                        libc::SIGTERM,
                                    );
                                    kill_confirm = None;
                                }
                            }
                            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                            KeyCode::Char('n') if display_mode == DisplayMode::Insights => {
                                kill_confirm = None;
                            }
                            _ => {
                                // Overlay already dismissed by take() before this match.
                            }
                        } // end inner key match
                    } // end else (not cmd_mode)
                } // end Event::Key
                _ => {}
            } // end match event::read()
        }

        // ── Poll the in-flight /remote worker (non-blocking) ────────────────
        // One cheap `try_recv()` per loop tick (the 16ms input poll wakes us).
        // While pending, the "discovering…/connecting…" status line set at
        // dispatch stays up. On completion we apply the outcome here, on the
        // main thread: discovery updates the picker cache + message; connect
        // hot-swaps the (already-init()ed) backend and resets per-mode viz.
        #[cfg(feature = "remote")]
        if let Some(rx) = pending_remote.as_ref() {
            match rx.try_recv() {
                Ok(RemoteOutcome::Discovered(Ok(boxes))) if boxes.is_empty() => {
                    remote_boxes.clear();
                    cmd_message = Some((
                        "no boxes discovered — try /remote <host:port>".to_string(),
                        false,
                    ));
                    pending_remote = None;
                    force_redraw = true;
                }
                Ok(RemoteOutcome::Discovered(Ok(boxes))) => {
                    let mut lines =
                        vec!["discovered boxes — /remote <n|name> to connect:".to_string()];
                    for (i, b) in boxes.iter().enumerate() {
                        lines.push(format!("  {}. {}", i + 1, b.summary_line()));
                    }
                    remote_boxes = boxes;
                    cmd_message = Some((lines.join("\n"), false));
                    pending_remote = None;
                    force_redraw = true;
                }
                Ok(RemoteOutcome::Discovered(Err(e))) => {
                    remote_boxes.clear();
                    cmd_message = Some((format!("discover failed: {}", e), true));
                    pending_remote = None;
                    force_redraw = true;
                }
                Ok(RemoteOutcome::Connected { target, result }) => {
                    match result {
                        Ok(ws) => {
                            // Hot-swap the live backend to the WsBackend the worker
                            // already constructed + init()ed (Box<WsBackend> unsizes
                            // to Box<dyn TelemetryBackend> on assignment).
                            *backend = ws;
                            backend_type = BackendType::Remote;
                            log::info!("/remote: hot-swapped to WsBackend @ {}", target);
                            // Reset per-mode visualizations so they rebuild against
                            // the new device set (mirrors the `b` backend-cycle path).
                            starfield = None;
                            memory_castle = None;
                            memory_flow = None;
                            arcade = None;
                            defrag = None;
                            // Drop any fleet drill-down: the remote box likely has a
                            // different (smaller) device count than we were zoomed into.
                            fleet_zoom_start = None;
                            fleet_cursor = 0;
                            cmd_message = Some((format!("connected to {}", target), false));
                        }
                        Err(e) => {
                            cmd_message = Some((e, true));
                        }
                    }
                    pending_remote = None;
                    force_redraw = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still working — leave the progress message up.
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Worker vanished without sending (shouldn't happen).
                    cmd_message = Some(("remote operation failed unexpectedly".to_string(), true));
                    pending_remote = None;
                    force_redraw = true;
                }
            }
        }

        // Update backend data
        if last_update.elapsed() >= update_interval {
            if let Err(e) = backend.update() {
                log::warn!("Update failed: {}", e);
            }
            last_update = Instant::now();

            // Ingest fresh telemetry into InferenceEngine — runs at backend rate (~10 Hz),
            // not at the UI render rate (60 FPS), so ARC stall counters don't oversaturate.
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            for device in backend.devices() {
                use crate::workload::inference::parse_arc_health_counters;
                let idx = device.index;
                let power = backend.telemetry(idx).map(|t| t.power_w()).unwrap_or(0.0);
                let temp = backend.telemetry(idx).map(|t| t.temp_c()).unwrap_or(0.0);
                let current = backend.telemetry(idx).map(|t| t.current_a()).unwrap_or(0.0);
                let aiclk = backend.telemetry(idx).and_then(|t| t.aiclk);
                let arc_health = backend.smbus_telemetry(idx).map(|s| {
                    parse_arc_health_counters([
                        s.arc0_health.clone(),
                        s.arc1_health.clone(),
                        s.arc2_health.clone(),
                        s.arc3_health.clone(),
                    ])
                });
                inference_engine.ingest(idx, power, temp, current, aiclk, arc_health);
            }
        }

        // Update process list + host stats (every 2 seconds to avoid overhead).
        // Cross-platform: the host CPU/RAM bars and process panel work on macOS.
        if last_proc_rows_update.elapsed() >= Duration::from_secs(2) {
            host_proc_monitor.update();
            // Refresh the prober's target set; read back last cycle's verdicts.
            liveness_prober.submit(host_proc_monitor.detected_runtimes());
            // Refresh the inference-server monitor's target set (containers only).
            #[cfg(target_os = "linux")]
            inference_monitor.submit(host_proc_monitor.detected_inference_servers());
            // Model-unload edge → kick off the Defrag EVICT animation. The
            // power-EMA heuristic can't detect unload on Blackhole (see
            // DefragVis::trigger_evict), so we drive it from this discrete signal.
            #[cfg(target_os = "linux")]
            {
                let cur_inf = inference_monitor.snapshot();
                if inference_panel::model_unloaded(&prev_inference_snapshot, &cur_inf) {
                    if let Some(dv) = defrag.as_mut() {
                        dv.trigger_evict();
                    }
                }
                prev_inference_snapshot = cur_inf;
            }
            // Non-Linux / no-procfs: always the cross-platform host snapshot —
            // TT filtering needs the /proc device-fd attribution below, which
            // doesn't exist on these builds.
            #[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
            {
                proc_rows =
                    host_proc_monitor.rows(PROC_PANEL_MAX_ROWS, &liveness_prober.fresh_verdicts());
            }

            // Linux/TT: refresh /proc attribution + serving probes, choose the
            // TT-filtered or full host row set by backend, then merge TT device
            // info into proc_rows by PID.
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            {
                process_monitor.update();
                // Probe inference servers at the same 2s cadence to avoid
                // hammering HTTP endpoints on every backend tick.
                let flat = flat_process_list(&process_monitor);
                serving_metrics = inference_probe.update(&flat);
                proc_rows = if crate::cli::backend_shows_only_tt(backend_type) {
                    let tt_pids: std::collections::HashSet<i32> =
                        flat.iter().map(|p| p.pid).collect();
                    host_proc_monitor.tt_rows(PROC_PANEL_MAX_ROWS, &tt_pids)
                } else {
                    host_proc_monitor.rows(PROC_PANEL_MAX_ROWS, &liveness_prober.fresh_verdicts())
                };
                enrich_proc_rows_tt(&mut proc_rows, &process_monitor);
            }

            // `/serve` per-tick broadcast. Same 2s cadence as the process/
            // inference refresh above — NOT the 60fps render loop — so
            // clients get one fresh frame per refresh instead of the same
            // frame re-sent dozens of times a second for no visible benefit.
            // `proc_rows` is current as of just above; the inference snapshot
            // is re-read here (cheap: an `ArcSwap` load + clone, see
            // `InferenceServerMonitor::snapshot`) rather than threaded out of
            // the `#[cfg(target_os = "linux")]` block above, since that block
            // computes it only to detect the model-unload edge and doesn't
            // otherwise need to survive past that check.
            #[cfg(feature = "remote")]
            if let Some(pub_handle) = publisher.as_ref() {
                if let Some(frame) = backend.snapshot_json() {
                    if backend_type == BackendType::Remote {
                        // Relay: re-broadcast the origin's frame verbatim so
                        // downstream viewers see the WATCHED box's processes/
                        // inference, not this relay host's. `snapshot_json()`
                        // already IS the origin peer's raw frame (which may
                        // carry the origin's own `tt_toplike`); injecting our
                        // local `build_extension` here would overwrite that
                        // key with our laptop's scan, misattributing it to
                        // the box being watched (see I1 in the final review).
                        pub_handle.broadcast(frame);
                    } else {
                        // Local source of truth: enrich our own telemetry
                        // with our own process/inference monitors.
                        #[cfg(target_os = "linux")]
                        let inference_snapshot_for_broadcast = inference_monitor.snapshot();
                        #[cfg(not(target_os = "linux"))]
                        let inference_snapshot_for_broadcast: Vec<
                            crate::workload::inference_server::ServiceState,
                        > = Vec::new();
                        let ext = crate::backend::build_extension(
                            &proc_rows,
                            &inference_snapshot_for_broadcast,
                        );
                        pub_handle.broadcast(crate::backend::inject_extension(&frame, &ext));
                    }
                }
            }

            sys_monitor.refresh_cpu_usage();
            sys_monitor.refresh_memory();
            host_cpu_pct = sys_monitor.global_cpu_usage();
            host_mem_used = sys_monitor.used_memory();
            host_mem_total = sys_monitor.total_memory();
            throttle_state.update_load(host_cpu_pct);
            last_proc_rows_update = Instant::now();
        }
    }
}

/// Render header with app title and status
fn render_header(f: &mut Frame, area: Rect, backend: &dyn TelemetryBackend) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            "🦀 TT-TOPLIKE-RS ",
            Style::default()
                .fg(colors::rgb(102, 126, 234)) // Vibrant purple-blue
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " │ ",
            Style::default().fg(colors::rgb(118, 75, 162)), // Deep purple
        ),
        Span::styled(
            format!("{} ", backend.backend_info()),
            Style::default()
                .fg(colors::rgb(56, 178, 172)) // Teal
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(118, 75, 162))),
        Span::styled(
            format!("{} devices", backend.device_count()),
            Style::default()
                .fg(colors::rgb(56, 178, 172))
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(102, 126, 234)) // Vibrant purple-blue
                        .add_modifier(Modifier::BOLD),
                )
                .title(" ⚡ Real-Time Hardware Monitoring ⚡ ")
                .title_alignment(Alignment::Center),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render visualization mode (full-screen starfield)
fn ui_visualization(f: &mut Frame, starfield: &HardwareStarfield, backend: &dyn TelemetryBackend) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Starfield content
        ])
        .split(f.area());

    render_visualization_header(f, chunks[0], starfield, backend);

    let starfield_lines = starfield.render();

    let starfield_widget = Paragraph::new(starfield_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(100, 200, 255))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" ✧ Hardware-Responsive Starfield ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(150, 220, 255))
                        .add_modifier(Modifier::BOLD),
                )
                .style(Style::default().bg(colors::rgb(0, 0, 0))),
        );

    f.render_widget(starfield_widget, chunks[1]);
}

/// Render visualization mode header with baseline status
fn render_visualization_header(
    f: &mut Frame,
    area: Rect,
    starfield: &HardwareStarfield,
    backend: &dyn TelemetryBackend,
) {
    let status = starfield.baseline_status();
    let status_color = if starfield.is_baseline_established() {
        colors::success()
    } else {
        colors::warning()
    };

    let header_text = vec![Line::from(vec![
        Span::styled(
            " 🌌 STARFIELD ",
            Style::default()
                .fg(colors::rgb(150, 220, 255)) // Bright cyan
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", backend.backend_info()),
            Style::default().fg(colors::rgb(200, 200, 220)),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", status),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(100, 200, 255))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" ✧ Visualization Mode ")
                .title_alignment(Alignment::Left)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 200, 100)) // Orange
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render Memory Castle mode (full-screen architectural memory hierarchy)
fn ui_memory_castle(f: &mut Frame, memory_castle: &MemoryCastle, backend: &dyn TelemetryBackend) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Memory Castle content
        ])
        .split(f.area());

    render_memory_castle_header(f, chunks[0], backend);

    let castle_lines = memory_castle.render(backend);

    let castle_widget = Paragraph::new(castle_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(255, 150, 200))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🏰 Memory Castle - Hardware Memory Hierarchy ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 180, 220))
                        .add_modifier(Modifier::BOLD),
                )
                .style(Style::default().bg(colors::rgb(0, 0, 0))),
        );

    f.render_widget(castle_widget, chunks[1]);
}

/// Render Memory Flow visualization (full-screen DRAM motion)
fn ui_memory_flow(f: &mut Frame, memory_flow: &MemoryFlowVis, backend: &dyn TelemetryBackend) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Flow content
        ])
        .split(f.area());

    render_memory_flow_header(f, chunks[0], backend);

    let flow_lines = memory_flow.render(backend);

    let flow_widget = Paragraph::new(flow_lines)
        .style(Style::default().bg(colors::rgb(0, 0, 0)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(150, 255, 150))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🌊 Memory Flow - NoC & DDR Activity ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(180, 255, 180))
                        .add_modifier(Modifier::BOLD),
                )
                .style(Style::default().bg(colors::rgb(0, 0, 0))),
        );

    f.render_widget(flow_widget, chunks[1]);
}

/// Render Memory Castle mode header with device info
fn render_memory_castle_header(f: &mut Frame, area: Rect, backend: &dyn TelemetryBackend) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            " 🏰 MEMORY CASTLE ",
            Style::default()
                .fg(colors::rgb(255, 180, 220)) // Bright pink
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", backend.backend_info()),
            Style::default().fg(colors::rgb(200, 200, 220)),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} devices ", backend.device_count()),
            Style::default()
                .fg(colors::rgb(80, 220, 200))
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(255, 150, 200))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🏰 Memory Hierarchy Visualization ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 200, 100))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render Memory Flow mode header with device info
fn render_memory_flow_header(f: &mut Frame, area: Rect, backend: &dyn TelemetryBackend) {
    let header_text = vec![Line::from(vec![
        Span::styled(
            " 🌊 MEMORY FLOW ",
            Style::default()
                .fg(colors::rgb(180, 255, 180)) // Bright green
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} ", backend.backend_info()),
            Style::default().fg(colors::rgb(200, 200, 220)),
        ),
        Span::styled(" │ ", Style::default().fg(colors::rgb(80, 80, 100))),
        Span::styled(
            format!(" {} devices ", backend.device_count()),
            Style::default()
                .fg(colors::rgb(80, 220, 200))
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let header = Paragraph::new(header_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(
                    Style::default()
                        .fg(colors::rgb(150, 255, 150))
                        .add_modifier(Modifier::BOLD),
                )
                .title(" 🌊 NoC & DDR Visualization ")
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(colors::rgb(255, 200, 100))
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .alignment(Alignment::Center);

    f.render_widget(header, area);
}

/// Render Defrag mode — model-loading visualization.
///
/// Displays a grid of cells per device that fills from empty (░) → loading (▒▓)
/// → resident (█) as a model is transferred into DRAM.  Fill progress is inferred
/// from observable signals: power ramp, aiclk transition, and GDDR temperature delta.
fn ui_defrag(f: &mut Frame, dv: &DefragVis, backend: &dyn TelemetryBackend) {
    let lines = dv.render(backend);
    let widget = Paragraph::new(lines).style(Style::default().bg(colors::rgb(0, 0, 0)));
    f.render_widget(widget, f.area());
}

/// Render Arcade mode — uses the composite canvas so the hero `@` and trail
/// actually appear.  The previous ratatui-Layout approach called sub-viz methods
/// directly into separate Rects, bypassing ArcadeVisualization::render() and
/// overlay_hero() entirely — the hero was computed but never drawn.
///
/// The composite path: arcade.render(backend) → Vec<Line> → single Paragraph
/// over the full terminal area.  No border boxes around each sub-panel, but
/// the inline separators from render_separator() provide visual structure.
fn ui_arcade(f: &mut Frame, arcade: &ArcadeVisualization, backend: &dyn TelemetryBackend) {
    let lines = arcade.render(backend);
    let widget = Paragraph::new(lines).style(Style::default().bg(colors::rgb(0, 0, 0)));
    f.render_widget(widget, f.area());
}

/// Global status bar — 1 raw row at the absolute bottom of every view.
///
/// Left zone: uniform hotkey strip (all modes).  When in Insights, three
/// Insights-specific keys are appended after the separator.
/// Right zone: per-chip telemetry with fixed-width columns so that a value
/// incrementing from 99 → 100 never shifts the next chip's label.
///
/// ≤4 chips: each shown individually as `BH0  43°  820M  78W`
/// >4 chips: a single `avg  44°  822M  79W` column instead.
///
/// The command bar and overlay panels render on top of this row when active.
fn render_global_statusbar(
    f: &mut Frame,
    backend: &dyn TelemetryBackend,
    mode: DisplayMode,
    perf: Option<&str>,
    throttle_hint: Option<&str>,
    serve_hint: Option<&str>,
) {
    use crate::models::telemetry::parse_hex_or_dec;

    let area = f.area();
    if area.height < 2 {
        return;
    }
    let bar_area = Rect {
        x: 0,
        y: area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };

    // ── Colour constants ──────────────────────────────────────────────────
    let key_fg = colors::rgb(80, 220, 200);
    let label_fg = colors::rgb(160, 160, 160);
    let sep_fg = colors::rgb(80, 80, 100);
    let bg = colors::rgb(10, 14, 22);

    // ── Left zone: hotkey strip ───────────────────────────────────────────
    let key_style = Style::default()
        .fg(key_fg)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let lbl = |s: &'static str| Span::styled(s, Style::default().fg(label_fg));
    let key = |s: &'static str| Span::styled(s, key_style);
    let pipe = || Span::styled("  │  ", Style::default().fg(sep_fg));

    let mut left: Vec<Span> = vec![
        Span::raw(" "),
        key(" v "),
        lbl(" cycle"),
        pipe(),
        key(" a "),
        lbl(" arcade"),
        pipe(),
        key(" d "),
        lbl(" defrag"),
        pipe(),
        key(" l "),
        lbl(" legend"),
        pipe(),
        key(" ? "),
        lbl(" help"),
        pipe(),
        key(" ! "),
        lbl(" explain"),
        pipe(),
        key(" / "),
        lbl(" cmd"),
        pipe(),
        key(" q "),
        lbl(" quit"),
    ];

    // Insights-specific nav appended when in that mode.
    if matches!(mode, DisplayMode::Insights) {
        left.push(pipe());
        left.push(Span::styled(
            "↑↓",
            Style::default().fg(colors::rgb(220, 220, 220)),
        ));
        left.push(lbl(" nav"));
        left.push(pipe());
        left.push(Span::styled(
            "K",
            Style::default().fg(colors::rgb(255, 100, 100)),
        ));
        left.push(lbl(" destroy"));
        left.push(pipe());
        left.push(Span::styled(
            "k",
            Style::default().fg(colors::rgb(220, 180, 80)),
        ));
        left.push(lbl(" silence"));
    }

    // ── Right zone: per-chip telemetry ────────────────────────────────────
    // Collect raw values first so we can decide individual vs average.
    let devices = backend.devices();

    // Build chip telemetry: (label, temp, mhz, watts)
    struct ChipTelem {
        label: String, // "BH0", "WH1", …
        temp: f32,
        mhz: u32,
        watts: f32,
    }

    let chips: Vec<ChipTelem> = devices
        .iter()
        .map(|d| {
            let idx = d.index;
            let telem = backend.telemetry(idx);
            let smbus = backend.smbus_telemetry(idx);
            let temp = telem.map(|t| t.temp_c()).unwrap_or(0.0);
            // Prefer SMBUS aiclk (smoothed, decimal string); fall back to telemetry aiclk.
            let mhz = smbus
                .and_then(|s| s.aiclk.as_deref())
                .and_then(|v| parse_hex_or_dec(v))
                .or_else(|| telem.and_then(|t| t.aiclk))
                .unwrap_or(0);
            let watts = telem.map(|t| t.power_w()).unwrap_or(0.0);
            ChipTelem {
                label: format!("{}{}", d.architecture.abbrev(), idx),
                temp,
                mhz,
                watts,
            }
        })
        .collect();

    let mut right: Vec<Span> = Vec::new();

    if chips.is_empty() {
        // nothing to show
    } else if chips.len() <= 4 {
        // Individual columns — fixed width per field so no shift on value change.
        // Format: "BH0  43°  820M  78W" — each field right-aligned in fixed width.
        // temp: {:>3.0}° (3 digits + °), mhz: {:>4}M, watts: {:>3.0}W
        right.push(Span::styled("  │  ", Style::default().fg(sep_fg)));
        for (i, c) in chips.iter().enumerate() {
            if i > 0 {
                right.push(Span::raw("  "));
            }
            // Temp hue: <60° cool blue, <80° warm yellow, ≥80° red.
            let temp_color = if c.temp < 60.0_f32 {
                colors::rgb(100, 180, 255)
            } else if c.temp < 80.0_f32 {
                colors::rgb(255, 200, 80)
            } else {
                colors::rgb(255, 100, 80)
            };
            right.push(Span::styled(
                c.label.clone(),
                Style::default().fg(key_fg).add_modifier(Modifier::BOLD),
            ));
            right.push(Span::raw("  "));
            right.push(Span::styled(
                format!("{:>3.0}°", c.temp),
                Style::default().fg(temp_color),
            ));
            right.push(Span::raw("  "));
            right.push(Span::styled(
                format!("{:>4}M", c.mhz),
                Style::default().fg(colors::rgb(140, 200, 255)),
            ));
            right.push(Span::raw("  "));
            right.push(Span::styled(
                format!("{:>3.0}W", c.watts),
                Style::default().fg(colors::rgb(200, 160, 255)),
            ));
        }
        right.push(Span::raw(" "));
    } else {
        // Average column for fleets >4 chips.
        let n = chips.len() as f32;
        let avg_temp = chips.iter().map(|c| c.temp).sum::<f32>() / n;
        let avg_mhz = (chips.iter().map(|c| c.mhz as f32).sum::<f32>() / n).round() as u32;
        let avg_watts = chips.iter().map(|c| c.watts).sum::<f32>() / n;
        right.push(Span::styled("  │  ", Style::default().fg(sep_fg)));
        right.push(Span::styled(
            "avg",
            Style::default().fg(key_fg).add_modifier(Modifier::BOLD),
        ));
        right.push(Span::raw("  "));
        right.push(Span::styled(
            format!("{:>3.0}°", avg_temp),
            Style::default().fg(colors::rgb(180, 200, 220)),
        ));
        right.push(Span::raw("  "));
        right.push(Span::styled(
            format!("{:>4}M", avg_mhz),
            Style::default().fg(colors::rgb(140, 200, 255)),
        ));
        right.push(Span::raw("  "));
        right.push(Span::styled(
            format!("{:>3.0}W", avg_watts),
            Style::default().fg(colors::rgb(200, 160, 255)),
        ));
        right.push(Span::styled(
            format!("  ×{}", chips.len()),
            Style::default().fg(label_fg),
        ));
        right.push(Span::raw(" "));
    }

    // Optional perf readout (toggled by `P`), dim, between hotkeys and telemetry.
    if let Some(p) = perf {
        left.push(Span::styled("  │  ", Style::default().fg(sep_fg)));
        left.push(Span::styled(
            p.to_string(),
            Style::default().fg(colors::rgb(120, 120, 140)),
        ));
    }

    // Throttle status hint (e.g. "⏷30" or "⏸2") shown dim when active.
    if let Some(hint) = throttle_hint {
        left.push(Span::styled("  │  ", Style::default().fg(sep_fg)));
        left.push(Span::styled(
            hint.to_string(),
            Style::default().fg(colors::rgb(100, 100, 120)),
        ));
    }

    // `/serve` indicator (`◉ serving :PORT · N clients`), shown bright (not
    // dim like the hints above) since an active publisher is broadcasting
    // this box's telemetry over the LAN — worth staying visible at a glance.
    if let Some(hint) = serve_hint {
        left.push(Span::styled("  │  ", Style::default().fg(sep_fg)));
        left.push(Span::styled(
            hint.to_string(),
            Style::default()
                .fg(colors::rgb(120, 220, 140))
                .add_modifier(Modifier::BOLD),
        ));
    }

    left.extend(right);

    f.render_widget(Block::default().style(Style::default().bg(bg)), bar_area);
    f.render_widget(
        Paragraph::new(Line::from(left)).style(Style::default().bg(bg)),
        bar_area,
    );
}

/// Render the command bar overlay at the bottom of the terminal.
///
/// When `cmd_buf` is non-empty or command mode is active, shows:
///   `:  <cmd_buf>_`
/// When a result message is present (after Enter), shows the message for one
/// render cycle until dismissed with Esc or the next `/` press.
fn render_command_bar(f: &mut Frame, cmd_buf: &str, msg: Option<(&str, bool)>) {
    let area = f.area();
    if area.height < 2 {
        return;
    }

    // A result message may span multiple lines (the `/remote` picker list); the
    // input prompt is always a single line. Build the lines, then anchor a bar
    // of that height at the bottom of the screen.
    let (lines, bar_h): (Vec<Line>, u16) = if let Some((text, is_error)) = msg {
        let color = if is_error {
            colors::rgb(255, 100, 100)
        } else {
            colors::rgb(80, 220, 140)
        };
        // Cap the bar height so a long list never eats the whole screen.
        let max_rows = area.height.saturating_sub(1).clamp(1, 12) as usize;
        let msg_lines: Vec<&str> = text.lines().collect();
        let shown = msg_lines.len().min(max_rows);
        let lines: Vec<Line> = msg_lines
            .iter()
            .take(shown)
            .map(|l| {
                Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        (*l).to_string(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                ])
            })
            .collect();
        let h = shown.max(1) as u16;
        (lines, h)
    } else {
        let spans = vec![
            Span::styled(
                "  / ",
                Style::default()
                    .fg(colors::rgb(220, 180, 80))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                cmd_buf.to_string(),
                Style::default()
                    .fg(colors::rgb(220, 220, 220))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("_", Style::default().fg(colors::rgb(200, 200, 80))),
        ];
        (vec![Line::from(spans)], 1)
    };

    let bar_area = Rect {
        x: 0,
        y: area.height.saturating_sub(bar_h),
        width: area.width,
        height: bar_h,
    };

    // Clear the bar rows with a dark background, then render text.
    f.render_widget(
        Block::default().style(Style::default().bg(colors::rgb(15, 20, 30))),
        bar_area,
    );
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(colors::rgb(15, 20, 30))),
        bar_area,
    );
}

/// Display width (terminal columns) of a fully-styled line: the sum of every
/// span's unicode display width, so box-drawing, arrows and emoji count as the
/// columns they actually occupy rather than as byte or `char` counts.
fn line_cols(line: &Line<'_>) -> usize {
    use unicode_width::UnicodeWidthStr;
    line.spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Render the floating overlay panel (legend / help / explain) anchored to the
/// bottom-right corner.  No right-side border characters per project convention.
///
/// The panel composites over whatever view is underneath — it does not consume
/// any layout rows.  Width adapts to the widest content line (clamped to the
/// terminal) so legend/help text is never clipped; height grows with content.
fn render_overlay_panel(f: &mut Frame, kind: OverlayPanel, mode: DisplayMode) {
    let area = f.area();
    // Enforce minimum terminal size — skip if it would leave no room.
    if area.width < 24 || area.height < 6 {
        return;
    }

    let lines = overlay_panel_lines(kind, mode);

    // Title + accent per panel kind (used for the header and to size the panel).
    let (title, border_color) = match kind {
        OverlayPanel::Legend => ("LEGEND", colors::rgb(79, 209, 197)), // teal
        OverlayPanel::Help => ("HELP", colors::rgb(220, 180, 80)),     // gold
        OverlayPanel::Explain => ("EXPLAIN", colors::rgb(180, 140, 255)), // lavender
    };

    // Width adapts to the widest content line so nothing is clipped. Content
    // lines already include the "║ " left border (there is no right border per
    // project convention), so the widest line's display width is exactly the
    // width the panel needs. The header ("╔═ TITLE ═…") sets a floor. Add one
    // trailing padding column, then clamp to the terminal so a narrow screen
    // can't push the panel off-screen.
    let title_floor = title.chars().count() + 6; // "╔═ " + title + " " + a little rule
    let content_max = lines.iter().map(line_cols).max().unwrap_or(0);
    let panel_w = ((content_max.max(title_floor) + 1) as u16).min(area.width.saturating_sub(2));

    let panel_h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2)); // +2 for top/bottom border

    let x = area.width.saturating_sub(panel_w);
    let y = area.height.saturating_sub(panel_h + 1); // +1 keeps above command bar row

    let panel_rect = Rect {
        x,
        y,
        width: panel_w,
        height: panel_h,
    };

    // Clear background behind the panel.
    f.render_widget(
        Block::default().style(Style::default().bg(colors::rgb(8, 14, 22))),
        panel_rect,
    );

    // Build full display: top border + content lines + bottom border.
    // Header = "╔═ TITLE " (title.chars().count() + 4 cols) then a rule to panel_w.
    let top_rule = "═".repeat((panel_w as usize).saturating_sub(title.chars().count() + 4));
    let mut display_lines: Vec<Line> = Vec::with_capacity(panel_h as usize);

    display_lines.push(Line::from(vec![
        Span::styled(
            format!("╔═ {} ", title),
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(top_rule, Style::default().fg(colors::rgb(40, 55, 70))),
    ]));

    let content_rows = (panel_h as usize).saturating_sub(2);
    for line in lines.iter().take(content_rows) {
        display_lines.push(line.clone());
    }

    let bottom_rule = "═".repeat((panel_w as usize).saturating_sub(1));
    display_lines.push(Line::from(Span::styled(
        format!("╚{}", bottom_rule),
        Style::default().fg(colors::rgb(40, 55, 70)),
    )));

    f.render_widget(
        Paragraph::new(display_lines).style(Style::default().bg(colors::rgb(8, 14, 22))),
        panel_rect,
    );
}

/// Build the content lines for the overlay panel.
///
/// Lines must fit within PANEL_W - 2 visible columns (no right border means content
/// needs to respect the panel width itself).  Each line is prefixed with `║ ` (2 cols).
fn overlay_panel_lines(kind: OverlayPanel, mode: DisplayMode) -> Vec<Line<'static>> {
    let bg = colors::rgb(8, 14, 22);
    let bar = colors::rgb(40, 55, 70);
    let dim = colors::rgb(90, 100, 120);
    let key = colors::rgb(79, 209, 197); // teal
    let lbl = colors::rgb(180, 190, 210);
    let head = colors::rgb(220, 230, 250);

    // Helper: build a ║-prefixed content line.
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    macro_rules! sep {
        ($label:expr) => {
            ln!(vec![Span::styled(
                $label,
                Style::default().fg(head).add_modifier(Modifier::BOLD)
            )])
        };
    }
    macro_rules! row {
        ($k:expr, $v:expr, $kc:expr) => {
            ln!(vec![
                Span::styled(
                    format!("{:<12}", $k),
                    Style::default().fg($kc).add_modifier(Modifier::BOLD)
                ),
                Span::styled($v, Style::default().fg(dim)),
            ])
        };
    }

    match kind {
        OverlayPanel::Help => {
            vec![
                sep!("Keys"),
                row!(" v", "cycle views", key),
                row!(" l", "toggle legend", key),
                row!(" ?", "this help panel", key),
                row!(" !", "explain current mode", key),
                row!(" /", "open command bar", key),
                row!(" q  Esc", "quit", key),
                row!(" r", "force data refresh", key),
                row!(" a  A", "jump to Arcade", key),
                row!(" d  D", "jump to Defrag", key),
                row!(" g", "jump to Grid", key),
                row!(" i  I", "Inference Servers monitor", key),
                row!(" b", "cycle backend", key),
                ln!(vec![Span::styled(
                    "──────────────────────────────────────",
                    Style::default().fg(bar)
                )]),
                sep!("Commands  (/ to open)"),
                row!(" /mode <name>", "insights grid castle flow…", lbl),
                row!(" /fps <n>", "animation FPS  1–120", lbl),
                row!(" /datafps <n>", "data refresh FPS  1–30", lbl),
                row!(" /legend", "toggle legend panel", lbl),
                row!(" /explain", "toggle explain panel", lbl),
                row!(" /help", "toggle this panel", lbl),
                row!(
                    " /throttle",
                    "throttle anim FPS under heavy load (60→30)",
                    lbl
                ),
                row!(
                    " /idle-on-blur",
                    "drop to 2 FPS when terminal unfocused",
                    lbl
                ),
                row!(
                    " /remote [n|box]",
                    "discover + connect to a remote box",
                    lbl
                ),
                row!(
                    " /serve [bind:port]",
                    "broadcast telemetry (plaintext, trusted-LAN)",
                    lbl
                ),
                row!(" /serve off", "stop broadcasting", lbl),
            ]
        }

        OverlayPanel::Legend => {
            // Per-view symbol legend — Arcade shows all four combined.
            match mode {
                DisplayMode::Starfield => starfield_legend_lines(bar, bg, dim, key),
                DisplayMode::MemoryCastle => castle_legend_lines(bar, bg, dim),
                DisplayMode::Defrag => defrag_legend_lines(bar, bg, dim, key),
                DisplayMode::MemoryFlow => flow_legend_lines(bar, bg, dim),
                DisplayMode::InferenceMonitor => inference_legend_lines(bar, bg, dim),
                DisplayMode::Insights => insights_legend_lines(bar, bg, dim),
                DisplayMode::Grid => grid_legend_lines(bar, bg, dim),
                DisplayMode::Arcade => {
                    // All four combined.
                    let mut v = Vec::new();
                    v.push(sep!("✧ Starfield"));
                    v.extend(starfield_legend_lines(bar, bg, dim, key));
                    v.push(ln!(vec![Span::styled(
                        "──────────────────────────────────────",
                        Style::default().fg(bar)
                    )]));
                    v.push(sep!("🏰 Memory Castle"));
                    v.extend(castle_legend_lines(bar, bg, dim));
                    v.push(ln!(vec![Span::styled(
                        "──────────────────────────────────────",
                        Style::default().fg(bar)
                    )]));
                    v.push(sep!("▓ Defrag"));
                    v.extend(defrag_legend_lines(bar, bg, dim, key));
                    v.push(ln!(vec![Span::styled(
                        "──────────────────────────────────────",
                        Style::default().fg(bar)
                    )]));
                    v.push(sep!("🌊 Memory Flow"));
                    v.extend(flow_legend_lines(bar, bg, dim));
                    v.push(ln!(vec![Span::styled(
                        "──────────────────────────────────────",
                        Style::default().fg(bar)
                    )]));
                    v.push(sep!("@ Hero"));
                    v.extend(hero_legend_lines(bar, bg, dim));
                    v
                }
            }
        }

        OverlayPanel::Explain => match mode {
            DisplayMode::Starfield => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Hardware-Responsive Starfield",
                    "",
                    "Each star represents one Tensix compute",
                    "core inside a Blackhole or Wormhole ASIC.",
                    "",
                    "Star brightness encodes instantaneous",
                    "power draw. Hue cycles from cyan (cool)",
                    "through yellow to red (hot) tracking the",
                    "ASIC die temperature in real time.",
                    "",
                    "Twinkle speed accelerates when current",
                    "draw is high. Brief sparkle flashes mark",
                    "power spikes above the learned baseline.",
                    "",
                    "Round planets (○) represent DDR/L2/L1",
                    "memory hierarchy — orbit radius tracks",
                    "memory bandwidth utilization.",
                ],
            ),
            DisplayMode::MemoryCastle => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Memory Castle",
                    "",
                    "An architectural view of the Tenstorrent",
                    "memory hierarchy rendered as a layered",
                    "tower. Particles flow upward from DRAM",
                    "at the base through L2 cache, L1 SRAM,",
                    "and into the Tensix compute cores at top.",
                    "",
                    "◦ open circle  — read access",
                    "· dot          — write access",
                    "◇ diamond      — cache hit",
                    "• filled dot   — cache miss",
                    "",
                    "Particle color encodes the memory tier.",
                    "Density reflects current bandwidth.",
                ],
            ),
            DisplayMode::Defrag => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Model-Loading Defrag Display",
                    "",
                    "Inspired by classic disk defragmenters.",
                    "Each row represents one GDDR channel.",
                    "Cells fill left-to-right as model weights",
                    "are transferred into device memory.",
                    "",
                    "░ empty     ▒ loading    ▓█ resident",
                    "",
                    "EVICT flashes when the runtime unloads",
                    "a model — triggered when inference power",
                    "surges above the rolling baseline.",
                    "",
                    "Block character intensity (░▒▓█) encodes",
                    "relative activity within each cell.",
                    "Color encodes channel temperature.",
                ],
            ),
            DisplayMode::MemoryFlow => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Memory Flow — NoC & DDR Activity",
                    "",
                    "Visualizes the Network-on-Chip (NoC) and",
                    "DDR memory traffic in real time.",
                    "",
                    "Particles flowing → are read requests;",
                    "particles flowing ← are write requests.",
                    "Speed scales with NoC clock (axiclk/aiclk",
                    "ratio) and MVDDQ memory subsystem power.",
                    "",
                    "DDR channels are shown as a perimeter",
                    "ring. Channel activity brightens the",
                    "corresponding arc segment.",
                    "",
                    "Heat-map center encodes die temperature.",
                ],
            ),
            DisplayMode::Arcade => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Arcade Mode — Unified View",
                    "",
                    "Combines all four visualizations into a",
                    "single full-screen display.",
                    "",
                    "Top band: Starfield — Tensix cores",
                    "Middle-left: Memory Castle — hierarchy",
                    "Middle-right: Defrag — GDDR loading",
                    "Bottom band: Memory Flow — NoC traffic",
                    "",
                    "The @ hero character moves across the",
                    "full canvas driven by live telemetry:",
                    "  x-axis = current draw (0–100A)",
                    "  y-axis = power (low → bottom, high top)",
                    "  color  = ASIC die temperature",
                    "",
                    "Hero glyph encodes chip health state:",
                    "  @  healthy   !  throttled/faulted",
                    "  ?  ARC stall  %  undervolt",
                ],
            ),
            DisplayMode::Insights => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Insights Screen",
                    "",
                    "Real-time chip portrait grid with live",
                    "telemetry and process management.",
                    "",
                    "Each panel shows a Tensix core portrait",
                    "with power, temperature, clock, and DDR",
                    "status alongside a live process table.",
                    "",
                    "Animated particles in each portrait",
                    "reflect the current compute activity.",
                    "",
                    "Use ↑↓ to navigate the process list.",
                    "k = SIGTERM,  K = SIGKILL.",
                    "At 32+ devices a compact fleet heatmap",
                    "is shown — Enter to zoom in.",
                ],
            ),
            DisplayMode::Grid => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Grid Mode",
                    "",
                    "Full-screen grid of chip portraits for",
                    "all connected devices. No process panel.",
                    "",
                    "Color and density in each portrait cell",
                    "encode Tensix core temperature and power.",
                    "",
                    "Press v to continue cycling modes.",
                ],
            ),
            DisplayMode::InferenceMonitor => explain_lines(
                bar,
                bg,
                dim,
                lbl,
                &[
                    "Inference Server Monitor",
                    "",
                    "One creature across the whole lifecycle of",
                    "a TT inference-server (docker-detected):",
                    "",
                    "COLD  — no model up: a hungry snake roams",
                    "  the model-catalog starfield (what could",
                    "  run on your hardware).",
                    "LOADING — compiling kernels / streaming",
                    "  weights: the snake grows + coils through",
                    "  compile→load, then uncoils at ready.",
                    "SERVING — live dashboard from vLLM /metrics:",
                    "  throughput timeline, the token-exhaust",
                    "  snake, request swimlanes (queue→prefill→",
                    "  decode), and a TT silicon strip tying",
                    "  tokens/s to real chip power/temp/clock.",
                    "ALARM — stalled 5+ min: the snake reddens.",
                    "",
                    "Press l for the symbol legend · i to return.",
                ],
            ),
        },
    }
}

// ── Per-view legend line builders ─────────────────────────────────────────────

/// Legend for Grid mode: a full-screen grid of chip portraits, no process panel.
fn grid_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("cell ", Style::default().fg(colors::rgb(120, 200, 255))),
            Span::styled(
                "= one chip portrait per device (all at once)",
                Style::default().fg(dim),
            ),
        ]),
        ln!(vec![
            Span::styled(
                "particles ",
                Style::default().fg(colors::rgb(120, 200, 255)),
            ),
            Span::styled("= Tensix compute activity", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("cyan→red ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled(
                "= cool→hot (core temperature / power)",
                Style::default().fg(dim),
            ),
        ]),
        ln!(vec![
            Span::styled("v ", Style::default().fg(colors::warning())),
            Span::styled(
                "= cycle to the next visualization",
                Style::default().fg(dim)
            ),
        ]),
    ]
}

/// Legend for the Insights (default) screen: chip portraits + process panel.
fn insights_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("portrait ", Style::default().fg(colors::rgb(120, 200, 255))),
            Span::styled(
                "= one panel per TT chip (power/temp/clock/DDR)",
                Style::default().fg(dim),
            ),
        ]),
        ln!(vec![
            Span::styled(
                "particles ",
                Style::default().fg(colors::rgb(120, 200, 255)),
            ),
            Span::styled("= live compute activity", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("cyan→red ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled("= cool→hot (temperature / power)", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled(
                "proc list ",
                Style::default().fg(colors::rgb(180, 230, 180)),
            ),
            Span::styled(
                "= processes using TT devices (↑↓ to select)",
                Style::default().fg(dim),
            ),
        ]),
        ln!(vec![
            Span::styled("k / K ", Style::default().fg(colors::error())),
            Span::styled(
                "= SIGTERM / SIGKILL the selected process",
                Style::default().fg(dim),
            ),
        ]),
        ln!(vec![
            Span::styled("fleet grid ", Style::default().fg(colors::warning())),
            Span::styled(
                "= per-chip heatmap at 32+ devices (Enter=zoom)",
                Style::default().fg(dim),
            ),
        ]),
        ln!(vec![
            Span::styled("i ", Style::default().fg(colors::rgb(120, 200, 255))),
            Span::styled(
                "= open the Inference Server Monitor",
                Style::default().fg(dim),
            ),
        ]),
    ]
}

/// Legend for the `[i]` Inference Server Monitor (the unified snake dashboard).
fn inference_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("● ", Style::default().fg(colors::rgb(80, 160, 150))),
            Span::styled(
                "cold — hungry snake roams the model-star field",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("◉ ", Style::default().fg(colors::rgb(246, 188, 66))),
            Span::styled(
                "loading — coils compile→load, then uncoils",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("▶ ", Style::default().fg(colors::warning())),
            Span::styled(
                "serving — head of the live feeding snake",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("█▓▒░ ", Style::default().fg(colors::success())),
            Span::styled(
                "= body/fill (teal→amber→gold journey)",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("» · ", Style::default().fg(colors::text_secondary())),
            Span::styled(
                "= token exhaust (rate = tokens/s)",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("✦ ", Style::default().fg(colors::text_primary())),
            Span::styled("= a request completed  ", Style::default().fg(dim)),
            Span::styled("✗ ", Style::default().fg(colors::error())),
            Span::styled("= error/stall", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("swimlanes ", Style::default().fg(colors::warning())),
            Span::styled(
                "= queue→prefill→decode (avg times)",
                Style::default().fg(dim)
            ),
        ]),
        ln!(vec![
            Span::styled("timeline ", Style::default().fg(colors::success())),
            Span::styled("= tok/s over time (▁▂▃▄▅▆▇█)", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("silicon ", Style::default().fg(colors::rgb(120, 180, 200))),
            Span::styled(
                "= per-chip power/temp/AICLK (live)",
                Style::default().fg(dim)
            ),
        ]),
    ]
}

fn starfield_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
    key: ratatui::style::Color,
) -> Vec<Line<'static>> {
    let _ = key;
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("·  ○  ●", Style::default().fg(colors::rgb(100, 180, 255))),
            Span::styled("  each = one Tensix core", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "brightness",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("  = power draw", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "cyan→red  ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("  = ASIC temperature", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "twinkle   ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("  = current draw", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("○ planet  ", Style::default().fg(colors::rgb(255, 200, 80))),
            Span::styled("  = DDR/L2/L1 memory", Style::default().fg(dim))
        ]),
    ]
}

fn castle_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("◦", Style::default().fg(colors::rgb(100, 200, 255))),
            Span::styled(" = read access", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("·", Style::default().fg(colors::rgb(255, 180, 80))),
            Span::styled(" = write access", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("◇", Style::default().fg(colors::rgb(100, 255, 180))),
            Span::styled(" = cache hit", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("•", Style::default().fg(colors::rgb(255, 100, 150))),
            Span::styled(" = cache miss", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "layers    ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= DDR→L2→L1→Tensix", Style::default().fg(dim))
        ]),
    ]
}

fn defrag_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
    _key: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled(
                "each row  ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= one GDDR channel", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("░", Style::default().fg(colors::rgb(60, 80, 100))),
            Span::styled(" empty / unloaded", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("▒", Style::default().fg(colors::rgb(80, 140, 200))),
            Span::styled(" loading / in-flight DMA", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("▓", Style::default().fg(colors::rgb(79, 209, 197))),
            Span::styled(" resident / running", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("█", Style::default().fg(colors::rgb(220, 240, 255))),
            Span::styled(" peak activity", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("EVICT     ", Style::default().fg(colors::rgb(255, 100, 80))),
            Span::styled("= model unload surge", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "color     ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= channel temperature", Style::default().fg(dim))
        ]),
    ]
}

fn flow_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled(
                "→ particles ",
                Style::default().fg(colors::rgb(100, 220, 255))
            ),
            Span::styled("= DDR reads", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "← particles ",
                Style::default().fg(colors::rgb(255, 160, 80))
            ),
            Span::styled("= DDR writes", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "speed       ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= NoC clock / aiclk", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "density     ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= MVDDQ power (mem W)", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "perimeter   ",
                Style::default().fg(colors::rgb(200, 230, 255))
            ),
            Span::styled("= GDDR channel ring", Style::default().fg(dim))
        ]),
    ]
}

fn hero_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("x-axis   ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled("= current draw 0–100A", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("y-axis   ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled("= power (low→bot, hi→top)", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("color    ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled("= ASIC temperature", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "@",
                Style::default()
                    .fg(colors::rgb(79, 209, 197))
                    .add_modifier(Modifier::BOLD)
            ),
            Span::styled("  healthy", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "!",
                Style::default()
                    .fg(colors::rgb(255, 100, 80))
                    .add_modifier(Modifier::BOLD)
            ),
            Span::styled("  throttled / faulted", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "?",
                Style::default()
                    .fg(colors::rgb(255, 220, 80))
                    .add_modifier(Modifier::BOLD)
            ),
            Span::styled("  ARC firmware stalled", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "%",
                Style::default()
                    .fg(colors::rgb(255, 180, 80))
                    .add_modifier(Modifier::BOLD)
            ),
            Span::styled("  undervolt (vcore<0.75V)", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled(
                "( )",
                Style::default()
                    .fg(colors::rgb(180, 140, 255))
                    .add_modifier(Modifier::BOLD)
            ),
            Span::styled(" harvested cores disabled", Style::default().fg(dim))
        ]),
        ln!(vec![
            Span::styled("trail    ", Style::default().fg(colors::rgb(200, 230, 255))),
            Span::styled("= GDDR temp / ETH links", Style::default().fg(dim))
        ]),
    ]
}

/// Build explain lines from a static slice of text strings.
fn explain_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
    lbl: ratatui::style::Color,
    text: &[&'static str],
) -> Vec<Line<'static>> {
    text.iter()
        .map(|s| {
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            if s.is_empty() {
                // blank separator line
            } else {
                v.push(Span::styled(
                    s.to_string(),
                    Style::default().fg(if s.starts_with(' ') { dim } else { lbl }),
                ));
            }
            Line::from(v)
        })
        .collect()
}

/// Parse and execute a slash command.  Returns `(message, is_error)`.
///
/// Available commands:
/// ```
/// /fps <n>          set animation FPS (1–120)
/// /datafps <n>      set data-mode FPS (1–30)
/// /mode <name>      switch display mode (insights|grid|starfield|castle|flow|arcade|defrag)
/// /legend           toggle legend overlay      (hotkey: l)
/// /explain          toggle explain overlay     (hotkey: !)
/// /help             list commands              (hotkey: ?)
/// /throttle         toggle anim FPS throttle under heavy CPU load (60→30)
/// /idle-on-blur     toggle drop to 2 FPS when terminal loses focus
/// ```
///
/// Note: `/remote` is intentionally NOT handled here — connecting needs the live
/// backend, which this function doesn't receive, and it runs off the render
/// thread. It is dispatched in the event loop's Enter handler (see
/// `spawn_remote_worker`).
/// Returns `(message, is_error, should_quit)`.
fn execute_command(
    cmd: &str,
    display_mode: &mut DisplayMode,
    anim_poll: &mut Duration,
    data_poll: &mut Duration,
    _sensitivity: f32,
    overlay: &mut Option<OverlayPanel>,
    throttle_state: &mut ThrottleState,
) -> (String, bool, bool) {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let verb = parts[0].trim_start_matches('/').to_lowercase();
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match verb.as_str() {
        "quit" | "q" | "exit" => ("".to_string(), false, true),
        "fps" => match arg.parse::<u64>() {
            Ok(n) if (1..=120).contains(&n) => {
                *anim_poll = Duration::from_secs_f64(1.0 / n as f64);
                (format!("animation fps set to {}", n), false, false)
            }
            _ => ("fps: expected 1–120".to_string(), true, false),
        },
        "datafps" => match arg.parse::<u64>() {
            Ok(n) if (1..=30).contains(&n) => {
                *data_poll = Duration::from_secs_f64(1.0 / n as f64);
                (format!("data fps set to {}", n), false, false)
            }
            _ => ("datafps: expected 1–30".to_string(), true, false),
        },
        "mode" => match arg {
            "insights" | "normal" => {
                *display_mode = DisplayMode::Insights;
                ("→ insights".to_string(), false, false)
            }
            "grid" => {
                *display_mode = DisplayMode::Grid;
                ("→ grid".to_string(), false, false)
            }
            "starfield" => {
                *display_mode = DisplayMode::Starfield;
                ("→ starfield".to_string(), false, false)
            }
            "castle" => {
                *display_mode = DisplayMode::MemoryCastle;
                ("→ castle".to_string(), false, false)
            }
            "flow" => {
                *display_mode = DisplayMode::MemoryFlow;
                ("→ flow".to_string(), false, false)
            }
            "arcade" => {
                *display_mode = DisplayMode::Arcade;
                ("→ arcade".to_string(), false, false)
            }
            "defrag" => {
                *display_mode = DisplayMode::Defrag;
                ("→ defrag".to_string(), false, false)
            }
            _ => (
                "mode: insights|grid|starfield|castle|flow|arcade|defrag".to_string(),
                true,
                false,
            ),
        },
        // App-wide color theme. `grayskull` collapses the rainbow to a thousand
        // shades of grey (+ cyan/purple accents), with hot pink the only hot
        // color; `default` restores full color; bare `/theme` toggles.
        "theme" => match arg {
            "grayskull" | "greyskull" | "grey" | "gray" | "grayscale" | "greyscale" => {
                colors::set_theme(colors::Theme::Grayskull);
                (
                    "theme: grayskull 🩶 — a thousand shades of grey (pink runs hot)".to_string(),
                    false,
                    false,
                )
            }
            "default" | "rainbow" | "color" | "colour" | "off" => {
                colors::set_theme(colors::Theme::Default);
                ("theme: default (full color)".to_string(), false, false)
            }
            "" => match colors::current_theme() {
                colors::Theme::Default => {
                    colors::set_theme(colors::Theme::Grayskull);
                    ("theme: grayskull 🩶".to_string(), false, false)
                }
                colors::Theme::Grayskull => {
                    colors::set_theme(colors::Theme::Default);
                    ("theme: default (full color)".to_string(), false, false)
                }
            },
            _ => (
                "theme: grayskull | default  (or /theme to toggle)".to_string(),
                true,
                false,
            ),
        },
        "legend" | "l" => {
            *overlay = if *overlay == Some(OverlayPanel::Legend) {
                None
            } else {
                Some(OverlayPanel::Legend)
            };
            (
                "legend overlay toggled  (press l or /legend to hide)".to_string(),
                false,
                false,
            )
        }
        "explain" => {
            *overlay = if *overlay == Some(OverlayPanel::Explain) {
                None
            } else {
                Some(OverlayPanel::Explain)
            };
            (
                "explain overlay toggled  (press ! or /explain to hide)".to_string(),
                false,
                false,
            )
        }
        "help" | "?" | "" => {
            *overlay = if *overlay == Some(OverlayPanel::Help) {
                None
            } else {
                Some(OverlayPanel::Help)
            };
            ("".to_string(), false, false) // panel shows the full content
        }
        "throttle" => {
            throttle_state.throttle_on = !throttle_state.throttle_on;
            (
                format!(
                    "load throttle: {}",
                    if throttle_state.throttle_on {
                        "on"
                    } else {
                        "off"
                    }
                ),
                false,
                false,
            )
        }
        "idle-on-blur" | "idleonblur" => {
            throttle_state.idle_on_blur = !throttle_state.idle_on_blur;
            (
                format!(
                    "idle-on-blur: {}",
                    if throttle_state.idle_on_blur {
                        "on"
                    } else {
                        "off"
                    }
                ),
                false,
                false,
            )
        }
        _ => (
            format!("unknown command: {}  (try /help or press ?)", verb),
            true,
            false,
        ),
    }
}

/// Parsed intent of a `/serve` command's argument (the text after `/serve `).
///
/// Pure and testable without any I/O — binding a socket and mutating the
/// loop's `publisher` slot both need the live backend (for `snapshot_json()`)
/// and happen in the key-handler's special-cased `serve` dispatch (mirroring
/// `/remote`, which needs the live backend/config for the same reason —
/// `execute_command` never sees either).
#[cfg(feature = "remote")]
#[derive(Debug, Clone, PartialEq)]
enum ServeCmd {
    /// `/serve` (bare) → `None` (caller binds the default `0.0.0.0:{DEFAULT_SERVE_PORT}`).
    /// `/serve <spec>` → `Some(spec)`, a raw BIND\[:PORT\]/PORT/HOST string not
    /// yet validated — that's [`crate::cli::parse_serve_bind`]'s job, run only
    /// when the command actually dispatches (it can trigger DNS resolution
    /// for a bare hostname, so it deliberately doesn't happen in this pure
    /// parse).
    Start(Option<String>),
    /// `/serve off` | `/serve stop` (case-insensitive) — drop the publisher.
    Stop,
    /// Malformed input the dispatcher should report as an error without ever
    /// attempting to bind (e.g. trailing garbage after the bind spec).
    Bad(String),
}

/// Pure parse of a `/serve` command's argument into a [`ServeCmd`].
///
/// - Empty (or whitespace-only) → `Start(None)`.
/// - `off` / `stop` (case-insensitive) → `Stop`.
/// - A single token → `Start(Some(token))`; validity of that token as a bind
///   address is checked later by [`crate::cli::parse_serve_bind`], not here.
/// - More than one whitespace-separated token → `Bad(..)`, so `/serve 9000
///   oops` reports a clear error instead of silently binding on 9000 and
///   swallowing "oops".
#[cfg(feature = "remote")]
fn parse_serve_command(arg: &str) -> ServeCmd {
    let arg = arg.trim();
    if arg.is_empty() {
        return ServeCmd::Start(None);
    }
    if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("stop") {
        return ServeCmd::Stop;
    }
    if arg.split_whitespace().count() > 1 {
        return ServeCmd::Bad(format!("/serve: unexpected extra argument(s) in '{arg}'"));
    }
    ServeCmd::Start(Some(arg.to_string()))
}

/// Resolve a `/remote <arg>` selection against the cached picker list.
///
/// Pure (no I/O) so it is unit-testable. Resolution order:
/// 1. A 1-based **index** into the last `/remote` listing (`/remote 2`).
/// 2. A **name** matching a listed box (`/remote qb2-lab`).
/// 3. An explicit **HOST:PORT** (`/remote 10.0.0.7:8765`) — passthrough.
///
/// Anything else is an error (the caller then tries a fresh discovery-by-name
/// so `/remote <name>` still works without a prior `/remote`).
#[cfg(feature = "remote")]
fn resolve_remote_pick(
    boxes: &[crate::backend::discovery::DiscoveredBox],
    arg: &str,
) -> Result<String, String> {
    let arg = arg.trim();
    // 1-based index into the shown list.
    if let Ok(n) = arg.parse::<usize>() {
        if (1..=boxes.len()).contains(&n) {
            return Ok(boxes[n - 1].host_port());
        }
        if !boxes.is_empty() {
            return Err(format!("pick 1–{} (got {})", boxes.len(), n));
        }
    }
    // Name match against the cached list.
    if let Some(b) = boxes.iter().find(|b| b.name == arg) {
        return Ok(b.host_port());
    }
    // Explicit HOST:PORT passthrough.
    if crate::backend::discovery::looks_like_host_port(arg) {
        return Ok(arg.to_string());
    }
    Err(format!("'{}' is not a listed box or HOST:PORT", arg))
}

/// Result of an off-thread `/remote` operation, handed back to the render loop
/// over an [`std::sync::mpsc`] channel.
///
/// Both `/remote` operations (discovery and connect) run on a worker thread so
/// the render loop never blocks on mDNS (seconds) or a WebSocket connect. The
/// worker performs all the impure/blocking work; the main thread only applies
/// the outcome (update the picker cache, or hot-swap the backend).
#[cfg(feature = "remote")]
enum RemoteOutcome {
    /// `/remote` (no arg) discovery finished with either a box list or an error.
    Discovered(Result<Vec<crate::backend::discovery::DiscoveredBox>, String>),
    /// `/remote <n|name|host:port>` connect finished. On `Ok` the [`WsBackend`]
    /// has already been constructed **and `init()`ed** on the worker (boxed to
    /// keep this enum small); the main thread just needs to hot-swap it in. On
    /// `Err` the string is a ready-to-display message. `target` is the resolved
    /// `HOST:PORT` (or the raw arg when resolution itself failed) for the status
    /// line.
    Connected {
        target: String,
        result: Result<Box<crate::backend::ws::WsBackend>, String>,
    },
}

/// Spawn the worker thread that services one `/remote` command off the render
/// thread, returning the receiver the loop polls with `try_recv()`.
///
/// * `arg` empty → discovery (`tt --json discover`, blocking mDNS).
/// * `arg` present → resolve to a `HOST:PORT` (against the cached `boxes`, with
///   a fresh discovery-by-name fallback) then build + `init()` a [`WsBackend`].
///
/// The worker owns clones of everything it needs (`boxes`, `config`) so it never
/// touches main-thread state. If the loop cancels (drops the receiver on Esc),
/// the final `send` fails harmlessly and the thread just exits.
#[cfg(feature = "remote")]
fn spawn_remote_worker(
    arg: String,
    boxes: Vec<crate::backend::discovery::DiscoveredBox>,
    config: BackendConfig,
) -> std::sync::mpsc::Receiver<RemoteOutcome> {
    use crate::backend::discovery;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = if arg.is_empty() {
            // No arg → discover and let the main thread present the picker.
            RemoteOutcome::Discovered(discovery::discover_boxes())
        } else {
            // Arg present → resolve to a HOST:PORT, then build + init the backend.
            let target = match resolve_remote_pick(&boxes, &arg) {
                Ok(t) => Ok(t),
                Err(pick_err) => {
                    // Not in the cached list — try a fresh discovery-by-name so
                    // `/remote <name>` works even without a prior `/remote`.
                    discovery::run_tt_discover()
                        .and_then(|json| discovery::resolve_remote_target(&json, &arg))
                        .map_err(|e| format!("{}; {}", pick_err, e))
                }
            };
            match target {
                Ok(target) => {
                    let result = build_ws_backend(&target, &config);
                    RemoteOutcome::Connected { target, result }
                }
                // Resolution failed — report against the raw arg.
                Err(e) => RemoteOutcome::Connected {
                    target: arg,
                    result: Err(e),
                },
            }
        };
        // Send may fail if the loop cancelled the wait (Esc) and dropped the
        // receiver. That's fine — the work is done and simply discarded.
        let _ = tx.send(outcome);
    });
    rx
}

/// Construct and initialize a [`WsBackend`] for `target` (`HOST:PORT`).
///
/// Runs entirely on the `/remote` worker thread (both `from_host_port` and the
/// blocking `init()` connect/first-frame wait). Returns the live backend on
/// success or a ready-to-display error string; the caller hot-swaps it in on
/// the main thread.
#[cfg(feature = "remote")]
fn build_ws_backend(
    target: &str,
    config: &BackendConfig,
) -> Result<Box<crate::backend::ws::WsBackend>, String> {
    use crate::backend::ws::WsBackend;
    match WsBackend::from_host_port(target, config.clone()) {
        Ok(mut ws) => match ws.init() {
            Ok(()) => Ok(Box::new(ws)),
            Err(e) => Err(format!("connect to {} failed: {}", target, e)),
        },
        Err(e) => Err(format!("bad remote target {}: {}", target, e)),
    }
}

/// Truncate a string to at most `max` characters (by char count, not bytes).
fn truncate(s: &str, max: usize) -> &str {
    let mut end = 0;
    let mut count = 0;
    for c in s.chars() {
        if count >= max {
            break;
        }
        end += c.len_utf8();
        count += 1;
    }
    &s[..end]
}

/// Flatten all processes into one stable-sorted Vec for cursor navigation.
/// Uses a HashSet to dedup by PID in O(n) instead of O(n²) scan.
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
fn flat_process_list<'a>(pm: &'a ProcessMonitor) -> Vec<&'a crate::workload::ProcessInfo> {
    use std::collections::HashSet;
    let mut seen: HashSet<i32> = HashSet::new();
    let mut all: Vec<&'a crate::workload::ProcessInfo> = Vec::new();

    // Iterate only over devices that actually have processes.
    let mut device_indices: Vec<usize> = pm.device_indices().collect();
    device_indices.sort_unstable(); // deterministic order
    for idx in device_indices {
        if let Some(procs) = pm.get_processes_for_device(idx) {
            for p in procs {
                if seen.insert(p.pid) {
                    all.push(p);
                }
            }
        }
    }
    for p in pm.get_shared_processes() {
        if seen.insert(p.pid) {
            all.push(p);
        }
    }
    all.sort_unstable_by_key(|p| p.pid);
    all
}

/// Merge TT device attribution into cross-platform `proc_rows` by PID.
///
/// For every process the Linux `/proc` monitor knows about, find the matching
/// `ProcRow` (same PID) and attach its TT device indices + hugepage counts.
/// A TT process that didn't rank into the top-N rows (and wasn't inference-
/// name-matched) simply isn't shown — acceptable, as TT processes are typically
/// high-CPU and rank in.
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

/// Cell width used in fleet compact mode (see `render_device_panels`).
/// 3 chars: temperature block glyph (1) + space (1) + separator (1).
const FLEET_CELL_W: u16 = 3;

/// Column gap between adjacent device panel columns in the portrait grid.
/// Used by both `panel_layout` and `render_device_panels` — single source of truth.
const PANEL_INTER_COL_GAP: u16 = 1;

/// Fleet compact mode activates at this device count (matching the starfield galaxy mode).
/// At 32+ devices the full-portrait grid almost always overflows a standard terminal.
const FLEET_DEVICE_THRESHOLD: usize = 32;

/// How many devices to show per page in the portrait drill-down (zoom) view.
/// Kept well below FLEET_DEVICE_THRESHOLD so the zoomed slice always renders as portraits.
const FLEET_PAGE_SIZE: usize = 16;

/// Height cap for the panel area in fleet mode.
const FLEET_HEIGHT_THRESHOLD: u16 = 30; // lines; beyond this we switch to compact cells

/// Shared panel layout calculation used by both `device_panels_height` and
/// `render_device_panels`.  Returns `(stats_w, gap_col, cols_per_row)`.
///
/// Single source of truth so the two callers can never diverge on gap or
/// compact-mode threshold.
fn panel_layout(n: usize, portrait_w: u16, area_width: u16) -> (u16, u16, usize) {
    let balanced_cols = ((n as f64).sqrt().ceil() as usize).max(1);
    let full_panel_w = portrait_w + 1 + 31; // gap=1, stats_w=31
    let cols_u16 = balanced_cols as u16;
    let full_row_w = cols_u16 * full_panel_w + (cols_u16.saturating_sub(1)) * PANEL_INTER_COL_GAP;
    let (stats_w, gap_col) = if full_row_w <= area_width {
        (31_u16, 1_u16)
    } else {
        (19_u16, 0_u16)
    };
    let panel_w: u16 = portrait_w + gap_col + stats_w;
    let max_cols_fit =
        ((area_width + PANEL_INTER_COL_GAP) / (panel_w + PANEL_INTER_COL_GAP)).max(1) as usize;
    let cols_per_row = max_cols_fit.min(balanced_cols);
    (stats_w, gap_col, cols_per_row)
}

/// Compute the vertical space required for the device panel grid at the given terminal width.
///
/// When the device count is large enough that full portraits would overflow the screen
/// we switch to a compact fleet heat-map (one coloured cell per device).
fn device_panels_height(devices: &[crate::models::Device], area_width: u16) -> u16 {
    use crate::ui::tui::chip_portrait::portrait_dims;
    let n = devices.len().max(1);

    // Fleet mode at 32+ devices (same threshold as starfield galaxy mode).
    if n >= FLEET_DEVICE_THRESHOLD {
        let cells_per_row = (area_width / FLEET_CELL_W).max(1) as usize;
        let cell_rows = (n + cells_per_row - 1) / cells_per_row;
        // 2 rows per cell-row (cell line + label line), +1 for header
        return (cell_rows as u16 * 2 + 1).min(FLEET_HEIGHT_THRESHOLD);
    }

    let arch = devices
        .first()
        .map(|d| d.architecture)
        .unwrap_or(crate::models::Architecture::Blackhole);
    let (portrait_cols, _) = portrait_dims(arch);
    let portrait_w = portrait_cols as u16 + 1;

    let (_, _, cols_per_row) = panel_layout(n, portrait_w, area_width);
    let row_count = (n + cols_per_row - 1) / cols_per_row;
    (row_count as u16) * 15 // panel_h(14) + inter-row gap(1)
}

/// Render Insights screen (full layout: chip portraits + process panel).
///
/// `portrait_particles` maps device index → live particle list, threaded through
/// to `render_device_panels` → `render_chip_portrait` for the particle overlay.
fn render_insights(
    f: &mut Frame,
    backend: &dyn TelemetryBackend,
    rows: &[crate::workload::ProcRow],
    cursor: usize,
    kill_confirm: Option<&KillConfirmState>,
    _cli: &Cli,
    portrait_particles: &std::collections::HashMap<
        usize,
        Vec<crate::ui::tui::chip_portrait::Particle>,
    >,
    portrait_baseline: &crate::animation::baseline::AdaptiveBaseline,
    fleet_cursor: usize,
    fleet_zoom_start: Option<usize>,
    host_cpu_pct: f32,
    host_mem_used: u64,
    host_mem_total: u64,
    // True when `rows` was built by the TT-filtered path (Linux/procfs +
    // `backend_shows_only_tt`) — picks the process panel's empty-state
    // message. Always `false` off that path (nothing was filtered out).
    tt_filtered: bool,
    // See `remote_local_fallback_note`: `Some` under `--remote` while the
    // panel is still showing LOCAL processes rather than the remote box's.
    remote_note: Option<&'static str>,
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    serving_metrics: &std::collections::HashMap<i32, ServingMetrics>,
) {
    let area = f.area();
    let devices = backend.devices();

    // When zoomed in, the panel height is based on the slice, not the full list.
    let panel_devices: &[crate::models::Device] = if let Some(start) = fleet_zoom_start {
        // Clamp start: the device count can shrink beneath a stale zoom offset
        // (backend swap via `b`/`/remote`, or a remote fleet losing a device),
        // which would otherwise make `start > end` and panic on the slice.
        let start = start.min(devices.len());
        let end = (start + FLEET_PAGE_SIZE).min(devices.len());
        &devices[start..end]
    } else {
        devices
    };

    // Layout: header(3) | cards(exact height) | process panel (remainder)
    //
    // Cards use Constraint::Length so they always get the exact height needed to
    // show all chip panels (including a 2×2 grid when 4 chips don't fit in one row).
    // The process panel takes whatever remains; it may be zero on very short
    // terminals but the chip panels are never truncated.
    //
    // The old toggle-able `[i]` Inference Servers strip that used to live here
    // has been retired in favor of the dedicated full-screen
    // `DisplayMode::InferenceMonitor` view (the unified serving snake — see
    // `render_snake_view`), entered via `i`/`I`.
    let cards_h = device_panels_height(panel_devices, area.width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(cards_h),
            Constraint::Min(0),
        ])
        .split(area);

    render_header(f, chunks[0], backend);
    render_device_panels(
        f,
        chunks[1],
        backend,
        portrait_particles,
        &portrait_baseline,
        fleet_cursor,
        fleet_zoom_start,
    );
    render_process_panel(
        f,
        chunks[2],
        rows,
        cursor,
        kill_confirm,
        host_cpu_pct,
        host_mem_used,
        host_mem_total,
        tt_filtered,
        remote_note,
        #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
        serving_metrics,
    );
    // Kill modal overlays the full screen when active
    if let Some(ref kc) = kill_confirm {
        render_kill_dialog(f, area, kc);
    }
}

/// Full-screen render of the unified serving snake (see `crate::animation::Snake`).
fn render_snake_view(
    f: &mut Frame,
    snake: &crate::animation::Snake,
    roster: &[Line<'static>],
    band: &[Line<'static>],
) {
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(colors::rgb(0, 0, 0))),
        area,
    );
    // Layout, top to bottom: the multi-model roster (0 rows when <2 services),
    // then the featured snake, then the education band (0 rows unless a
    // service is compiling/loading/alarming), then the reserved bottom row
    // for the global status bar (drawn on top afterwards — otherwise the
    // creature's footer lands on the absolute bottom row and is painted
    // over). The snake height here must match what `SnakeWorld` was built
    // with (same `1 + roster_h + band_h` subtraction) so the fixed-dim
    // loading/roaming renderers line up.
    let roster_h = (roster.len() as u16).min(area.height);
    if roster_h > 0 {
        f.render_widget(
            Paragraph::new(roster.to_vec()),
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: roster_h,
            },
        );
    }
    // Education band sits just above the bottom status row.
    let band_h = (band.len() as u16).min(area.height.saturating_sub(roster_h + 1));
    let body = Rect {
        x: area.x,
        y: area.y + roster_h,
        width: area.width,
        height: area.height.saturating_sub(1 + roster_h + band_h),
    };
    f.render_widget(
        Paragraph::new(snake.render(body.width as usize, body.height as usize)),
        body,
    );
    if band_h > 0 {
        f.render_widget(
            Paragraph::new(band.to_vec()),
            Rect {
                x: area.x,
                y: area.y + area.height.saturating_sub(1 + band_h),
                width: area.width,
                height: band_h,
            },
        );
    }
}

/// Under `--remote` the chips/telemetry on screen belong to the remote box,
/// but (until the real remote-data plumbing lands) the process panel and the
/// `[i]` inference view still read/probe *this* machine — so without a label
/// it looks like we're fully watching the remote box when we aren't. Returns
/// the honesty note to append to those panels' headers, or `None` when it
/// isn't needed (any backend other than Remote, or once real remote data
/// exists to display instead).
///
/// `has_remote_data` reflects whether the relevant sub-key of the
/// `tt_toplike` frame extension was actually decoded from the latest frame
/// (`WsBackend::remote_processes()`/`remote_inference()` — see call sites):
/// `true` once a `--serve` peer streams that data, at which point this note
/// naturally stops firing for that panel.
fn remote_local_fallback_note(
    backend_type: crate::cli::BackendType,
    has_remote_data: bool,
) -> Option<&'static str> {
    match (backend_type, has_remote_data) {
        (crate::cli::BackendType::Remote, false) => Some("LOCAL — remote not streamed"),
        _ => None,
    }
}

/// Parse a `tt_toplike` wire phase string back into
/// [`Phase`](crate::workload::inference_server::Phase). An unrecognized
/// string (future schema bump on the producer side, or a corrupt frame) maps
/// to `Down` rather than panicking or guessing — the safest "nothing is
/// happening" reading for an unknown state.
#[cfg(feature = "remote")]
fn phase_from_str(s: &str) -> crate::workload::inference_server::Phase {
    use crate::workload::inference_server::Phase;
    match s {
        "compiling" => Phase::Compiling,
        "loading" => Phase::Loading,
        "ready" => Phase::Ready,
        "alarm" => Phase::Alarm,
        // "down" and anything unrecognized both read as Down.
        _ => Phase::Down,
    }
}

/// Map one remote process (from the `tt_toplike` frame extension) to the
/// process panel's row type. `inference`/`active` are always `None`/`false`:
/// the remote producer doesn't classify inference runtimes the way the local
/// `HostProcessMonitor` heuristic does, and `tt` is `None` for the same
/// reason — `RemoteProc::uses_tt` is a bare bool, not the detailed
/// `TtProcInfo` (device indices / hugepage counts) the local scan produces,
/// so there is nothing honest to synthesize into that field.
#[cfg(feature = "remote")]
fn remote_proc_to_row(rp: &crate::backend::RemoteProc) -> crate::workload::ProcRow {
    crate::workload::ProcRow {
        pid: rp.pid as i32,
        name: rp.name.clone(),
        cpu_pct: rp.cpu_pct,
        mem_bytes: rp.mem_bytes,
        inference: None,
        active: false,
        tt: None,
    }
}

/// Map the wire [`RemoteServing`](crate::backend::RemoteServing) shape back
/// onto the local display struct
/// [`ServingStats`](crate::workload::inference_server::metrics::ServingStats).
/// `counters` (the raw vLLM counters used only to compute local deltas) has
/// no remote equivalent — deliberately excluded from the wire shape, see
/// `remote_ext`'s module docs — so it's always `Default::default()` here.
#[cfg(feature = "remote")]
fn remote_serving_to_stats(
    rs: &crate::backend::RemoteServing,
) -> crate::workload::inference_server::metrics::ServingStats {
    crate::workload::inference_server::metrics::ServingStats {
        generation_tps: rs.generation_tps,
        prompt_tps: rs.prompt_tps,
        completed_delta: rs.completed_delta,
        errored_delta: rs.errored_delta,
        requests_running: rs.requests_running,
        requests_waiting: rs.requests_waiting,
        kv_cache_usage: rs.kv_cache_usage,
        ttft_avg_s: rs.ttft_avg_s,
        queue_avg_s: rs.queue_avg_s,
        prefill_avg_s: rs.prefill_avg_s,
        decode_avg_s: rs.decode_avg_s,
        tpot_avg_s: rs.tpot_avg_s,
        prefix_hit_rate: rs.prefix_hit_rate,
        preemptions_delta: rs.preemptions_delta,
        counters: Default::default(),
    }
}

/// Map the wire [`RemoteMedia`](crate::backend::RemoteMedia) shape back onto
/// the local [`MediaStats`](crate::workload::inference_server::metrics::MediaStats)
/// display struct. The rate/latency fields carry over directly; the cumulative
/// `completed_total`/`errored_total` are restored into
/// `counters.requests_total`/`errored_total` (the fields the roster and media
/// panel read for "N done" / "errors N") so remote clients show real totals.
/// The remaining counters exist only to compute local deltas and have no wire
/// equivalent, so they stay `Default::default()`.
#[cfg(feature = "remote")]
fn remote_media_to_stats(
    rm: &crate::backend::RemoteMedia,
) -> crate::workload::inference_server::metrics::MediaStats {
    crate::workload::inference_server::metrics::MediaStats {
        generations_per_min: rm.generations_per_min,
        jobs_in_progress: rm.jobs_in_progress,
        completed_delta: rm.completed_delta,
        errored_delta: rm.errored_delta,
        duration_avg_s: rm.duration_avg_s,
        post_avg_s: rm.post_avg_s,
        pre_avg_s: rm.pre_avg_s,
        inference_avg_s: rm.inference_avg_s,
        warmup_avg_s: rm.warmup_avg_s,
        counters: crate::workload::inference_server::MediaCounters {
            requests_total: rm.completed_total,
            errored_total: rm.errored_total,
            ..Default::default()
        },
    }
}

/// Map one remote inference workload (from the `tt_toplike` frame extension)
/// to the local [`ServiceState`](crate::workload::inference_server::ServiceState)
/// the `[i]` view's snake/roster already know how to render.
///
/// Fields the wire shape doesn't carry (`cpu_pct`, `rss_bytes`/`rss_delta`,
/// `kernel_count`/`kernel_delta`, `safetensors_fds`, `top_proc`, `last_log`,
/// `flat_ticks`) are the *local* Docker-probe internals used to derive phase
/// transitions and the Alarm escalation on THIS box — meaningless for a
/// remote workload whose phase is already authoritative on the wire, so they
/// are zeroed/`None` rather than guessed. `readiness` is reconstructed from
/// `phase` (the wire's source of truth) since `Readiness` itself isn't part
/// of the wire shape: `Ready` -> `Ready { runner: None }` (no remote runner
/// name to carry), `Down` -> `Down`, anything else -> `NotReady` (matches
/// `Phase::derive`'s own Ready/Down/otherwise shape).
#[cfg(feature = "remote")]
fn remote_inference_to_service_state(
    ri: &crate::backend::RemoteInference,
) -> crate::workload::inference_server::ServiceState {
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};

    let phase = phase_from_str(&ri.phase);
    let readiness = match phase {
        Phase::Ready => Readiness::Ready { runner: None },
        Phase::Down => Readiness::Down,
        Phase::Compiling | Phase::Loading | Phase::Alarm => Readiness::NotReady,
    };

    ServiceState {
        key: ri.key.clone(),
        label: ri.label.clone(),
        phase,
        cpu_pct: 0.0,
        rss_bytes: 0,
        rss_delta: 0,
        kernel_count: 0,
        kernel_delta: 0,
        // Not carried on the wire — a remote peer streams phase/serving, not the
        // container's raw compile/load file counts.
        loaded_count: 0,
        loaded_delta: 0,
        safetensors_fds: 0,
        readiness,
        top_proc: None,
        last_log: None,
        progress: ri.progress,
        flat_ticks: 0,
        serving: ri.serving.as_ref().map(remote_serving_to_stats),
        media: ri.media.as_ref().map(remote_media_to_stats),
    }
}

/// Max inference services listed in the `[i]` roster (see below).
const INFERENCE_ROSTER_MAX: usize = 8;

/// Recompose ticks between education-fact rotations (~8s at the ~2s inference
/// cadence). Keeps a long compile/load from showing one stale fact the whole time.
const BAND_ROTATE_EVERY: usize = 4;
/// Max rows the education band may claim in the [i] loading pane.
const INFERENCE_BAND_MAX_H: usize = 5;

/// Compact per-service roster for the `[i]` view, shown above the featured snake
/// when 2+ inference services are detected — so a multi-model box surfaces ALL
/// of them (the snake still *features* one; loading wins, else the first Ready).
/// Returns empty for 0/1 service (the single snake already says it) so the common
/// single-model view is uncluttered. One header line + up to
/// [`INFERENCE_ROSTER_MAX`] service lines; the featured row is marked `▸`.
fn inference_roster_lines(
    rows: &[crate::workload::inference_server::ServiceState],
    width: usize,
) -> Vec<Line<'static>> {
    use crate::workload::inference_server::Phase;
    if rows.len() < 2 || width < 12 {
        return Vec::new();
    }
    // Which service the snake is showing (mirror choose_behavior's priority).
    let featured_key = inference_panel::featured_loading(rows)
        .map(|s| s.key.clone())
        .or_else(|| {
            rows.iter()
                .find(|s| s.phase == Phase::Ready)
                .map(|s| s.key.clone())
        });

    let shown = rows.len().min(INFERENCE_ROSTER_MAX);
    let hidden = rows.len() - shown;
    let header = if hidden > 0 {
        format!("inference services ({}, +{hidden} more)", rows.len())
    } else {
        format!("inference services ({})", rows.len())
    };
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(shown + 1);
    lines.push(Line::from(Span::styled(
        header,
        Style::default()
            .fg(colors::text_secondary())
            .add_modifier(Modifier::BOLD),
    )));

    for s in rows.iter().take(INFERENCE_ROSTER_MAX) {
        let featured = featured_key.as_ref() == Some(&s.key);
        let (tag, tag_color) = match s.phase {
            Phase::Down => ("down   ", colors::text_secondary()),
            Phase::Compiling => ("compile", colors::info()),
            Phase::Loading => ("loading", colors::info()),
            Phase::Ready => ("serving", colors::success()),
            Phase::Alarm => ("stalled", colors::error()),
        };
        // Per-service stat: live rate when serving (tok/s for vLLM, gen/min for
        // media/diffusion), else a load %, else nothing.
        let stat = match (&s.serving, &s.media) {
            (Some(sv), _) => format!(
                "{:.0} tok/s · {} run",
                sv.generation_tps, sv.requests_running
            ),
            (None, Some(m)) => format!(
                "{} in flight · {} done",
                m.jobs_in_progress, m.counters.requests_total
            ),
            (None, None) => match s.progress {
                Some(p) => format!("{:.0}%", (p * 100.0).clamp(0.0, 100.0)),
                None => String::new(),
            },
        };
        // Truncate the label (char-safe) so marker+tag+label+stat fit `width`.
        let fixed = 2
            + 7
            + 2
            + if stat.is_empty() {
                0
            } else {
                2 + stat.chars().count()
            };
        let label_w = width.saturating_sub(fixed).max(4);
        let label: String = s.label.chars().take(label_w).collect();

        let mut spans = vec![
            Span::styled(
                if featured { "▸ " } else { "  " }.to_string(),
                Style::default().fg(colors::primary()),
            ),
            Span::styled(
                tag.to_string(),
                Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                label,
                Style::default().fg(if featured {
                    colors::text_primary()
                } else {
                    colors::text_secondary()
                }),
            ),
        ];
        if !stat.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                stat,
                Style::default().fg(colors::text_secondary()),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Render a btop-style horizontal bar: `[████████░░░░░░░░]  42%`.
///
/// `filled` is a fraction in [0, 1]; `bar_width` is the inner glyph count.
/// Returns a `Vec<Span>` ready to append to a `Line`.
fn host_bar_spans(
    label: &str,
    filled: f32,
    bar_width: usize,
    value_str: &str,
    bar_color: ratatui::style::Color,
) -> Vec<Span<'static>> {
    use ratatui::style::{Color, Style};
    use ratatui::text::Span;

    let filled_count =
        ((filled.clamp(0.0, 1.0) * bar_width as f32).round() as usize).min(bar_width);
    let empty_count = bar_width - filled_count;

    let bar_filled = "█".repeat(filled_count);
    let bar_empty = "░".repeat(empty_count);

    vec![
        Span::styled(label.to_string(), Style::default().fg(Color::Gray)),
        Span::styled(" [".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(bar_filled, Style::default().fg(bar_color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
        Span::styled("] ".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(value_str.to_string(), Style::default().fg(bar_color)),
    ]
}

/// Pick a color for a bar value in [0, 1]: teal → yellow → red.
fn bar_color(frac: f32) -> ratatui::style::Color {
    use ratatui::style::Color;
    if frac < 0.5 {
        Color::Rgb(80, 220, 200) // teal
    } else if frac < 0.8 {
        Color::Rgb(255, 200, 80) // yellow-orange
    } else {
        Color::Rgb(255, 80, 80) // red
    }
}

/// Render the process panel over cross-platform `ProcRow`s.
///
/// Always shows PID / CPU% / MEM / NAME and an inference tag. When a row carries
/// TT attribution (`row.tt`, Linux-only) it also shows the device-index column
/// and a hugepage hint. On Linux, a matching `serving_metrics` entry adds a
/// per-process serving summary row (port / health / model / tok-s).
fn render_process_panel(
    f: &mut Frame,
    area: Rect,
    rows: &[crate::workload::ProcRow],
    cursor: usize,
    _kill_confirm: Option<&KillConfirmState>,
    host_cpu_pct: f32,
    host_mem_used: u64,
    host_mem_total: u64,
    tt_filtered: bool,
    // See `remote_local_fallback_note`: `Some` under `--remote` while this
    // panel is still showing LOCAL processes rather than the remote box's.
    remote_note: Option<&'static str>,
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    serving_metrics: &std::collections::HashMap<i32, ServingMetrics>,
) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    let mut lines: Vec<Line> = Vec::new();

    // Separator rule
    let rule_width = area.width.saturating_sub(4) as usize;
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("─".repeat(rule_width), Style::default().fg(Color::DarkGray)),
    ]));

    // Host CPU / RAM bars — displayed above the process list so they're always visible
    // even when the list is empty.  Bar width is fixed at 20 glyphs to keep it compact.
    {
        const BAR_W: usize = 20;

        let cpu_frac = (host_cpu_pct / 100.0).clamp(0.0, 1.0);
        let cpu_val = format!("{:>4.1}%", host_cpu_pct.clamp(0.0, 100.0));
        let cpu_color = bar_color(cpu_frac);
        let mut cpu_spans = vec![Span::raw("  ")];
        cpu_spans.extend(host_bar_spans("CPU", cpu_frac, BAR_W, &cpu_val, cpu_color));

        let mem_frac = if host_mem_total > 0 {
            (host_mem_used as f64 / host_mem_total as f64) as f32
        } else {
            0.0
        };
        // Display in GiB with one decimal place.
        let gib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let mem_val = format!("{:.1}/{:.1}G", gib(host_mem_used), gib(host_mem_total));
        let mem_color = bar_color(mem_frac);
        let mut mem_spans: Vec<Span> = vec![Span::raw("  ")];
        mem_spans.extend(host_bar_spans("RAM", mem_frac, BAR_W, &mem_val, mem_color));

        // Lay CPU and RAM on the same row, separated by a few spaces,
        // if the area is wide enough; otherwise separate rows.
        let wide_enough = area.width >= 80;
        if wide_enough {
            let mut row_spans = vec![Span::raw("  ")];
            row_spans.extend(host_bar_spans("CPU", cpu_frac, BAR_W, &cpu_val, cpu_color));
            row_spans.push(Span::raw("     "));
            row_spans.extend(host_bar_spans("RAM", mem_frac, BAR_W, &mem_val, mem_color));
            lines.push(Line::from(row_spans));
        } else {
            lines.push(Line::from(cpu_spans));
            lines.push(Line::from(mem_spans));
        }
    }

    // Section label. Under `--remote` (until real remote data lands — see
    // `remote_local_fallback_note`) this list is still LOCAL processes, not
    // the remote box's, so the label says so — otherwise it reads as if the
    // remote box's processes were on screen.
    let mut header_spans = vec![
        Span::raw("  "),
        Span::styled("Processes", Style::default().fg(Color::Gray)),
    ];
    if let Some(note) = remote_note {
        header_spans.push(Span::styled(
            format!(" · {note}"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(ratatui::style::Modifier::DIM),
        ));
    }
    lines.push(Line::from(header_spans));

    if rows.is_empty() {
        let empty_msg = if tt_filtered {
            "No processes using TT devices detected"
        } else {
            "nothing is feeding the chips right now"
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(empty_msg, Style::default().fg(Color::DarkGray)),
        ]));
    } else {
        let clamped_cursor = cursor.min(rows.len().saturating_sub(1));

        // Human-readable memory: bytes → KMG with one decimal for ≥1 unit.
        let human_bytes = |bytes: u64| -> String {
            const KB: f64 = 1024.0;
            const MB: f64 = KB * 1024.0;
            const GB: f64 = MB * 1024.0;
            let b = bytes as f64;
            if b >= GB {
                format!("{:.1}G", b / GB)
            } else if b >= MB {
                format!("{:.1}M", b / MB)
            } else if b >= KB {
                format!("{:.1}K", b / KB)
            } else {
                format!("{}B", bytes)
            }
        };

        for (i, row) in rows.iter().enumerate() {
            let selected = i == clamped_cursor;
            // Use ">" (guaranteed 1-column width) rather than "▶" (ambiguous
            // Unicode width — some terminals render it as 2 columns, shifting
            // all subsequent spans on the selected row 1 char to the right).
            let cursor_char = if selected { ">" } else { " " };

            let pid = format!("{:>7}", row.pid);
            let cpu = format!("{:>5.1}%", row.cpu_pct);
            let mem = format!("{:>7}", human_bytes(row.mem_bytes));
            let name = format!("{:<16}", truncate(&row.name, 16));

            // Inference tag: `~ label` when actively working, `~ label idle` when
            // matched but parked (e.g. `ollama serve` with no model), else `-`.
            let inf_raw = match row.inference {
                Some(label) if row.active => format!("~ {}", label),
                Some(label) => format!("~ {} idle", label),
                None => "-".to_string(),
            };
            const INF_W: usize = 12;
            let inference = format!("{:<width$}", truncate(&inf_raw, INF_W), width = INF_W);

            let row_color = if selected { Color::White } else { Color::Gray };
            let cursor_color = if selected {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            let inf_color = match row.inference {
                Some(_) if row.active => Color::Green, // actively serving
                Some(_) => Color::Yellow,              // matched but idle/parked
                None => Color::DarkGray,
            };

            let mut row_spans = vec![
                Span::raw("  "),
                Span::styled(cursor_char, Style::default().fg(cursor_color)),
                Span::raw(" "),
                Span::styled(pid, Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled(cpu, Style::default().fg(row_color)),
                Span::raw("  "),
                Span::styled(mem, Style::default().fg(row_color)),
                Span::raw("  "),
                Span::styled(name, Style::default().fg(row_color)),
                Span::raw("  "),
                Span::styled(inference, Style::default().fg(inf_color)),
            ];

            // TT attribution (Linux-only enrichment): device indices + hugepages.
            if let Some(ref tt) = row.tt {
                let dev_raw = if tt.device_indices.is_empty() {
                    "shared".to_string()
                } else {
                    format!(
                        "dev {}",
                        tt.device_indices
                            .iter()
                            .map(|d| d.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                };
                const DEV_W: usize = 12;
                let dev = format!("{:<width$}", truncate(&dev_raw, DEV_W), width = DEV_W);
                row_spans.push(Span::raw("  "));
                row_spans.push(Span::styled(dev, Style::default().fg(Color::Blue)));

                let hp = tt.hugepages_1g + tt.hugepages_2m;
                if hp > 0 {
                    row_spans.push(Span::raw("  "));
                    row_spans.push(Span::styled(
                        format!("{}hp", hp),
                        Style::default().fg(Color::Magenta),
                    ));
                }
            }

            lines.push(Line::from(row_spans));

            // If this is a known TT inference server, append a metrics summary row.
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            if let Some(sm) = serving_metrics.get(&row.pid) {
                let mut spans: Vec<Span> = vec![
                    Span::raw("       "),
                    Span::styled(
                        format!("[{}] ", sm.flavour.label()),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        if sm.swap_in_progress {
                            "loading"
                        } else if sm.is_ready {
                            "ready"
                        } else {
                            "init"
                        },
                        Style::default().fg(if sm.is_ready {
                            Color::Green
                        } else {
                            Color::Yellow
                        }),
                    ),
                ];

                // Collect the interesting metric snippets, space-separated.
                let mut parts: Vec<String> = Vec::new();

                if let Some(ref model) = sm.model_id {
                    let short = model.rsplit('/').next().unwrap_or(model.as_str());
                    parts.push(format!("model={}", truncate(short, 24)));
                }
                if let Some(tps) = sm.generation_tps {
                    parts.push(format!("{:.0}tok/s", tps));
                }
                if let Some(rif) = sm.requests_in_flight {
                    parts.push(format!("{}req", rif));
                }
                if let Some(kv) = sm.kv_cache_utilization {
                    parts.push(format!("kv={:.0}%", kv * 100.0));
                }
                if let Some(ttft) = sm.ttft_p50 {
                    parts.push(format!("ttft={:.0}ms", ttft * 1000.0));
                }
                if let Some(ref mesh) = sm.mesh_device {
                    parts.push(format!("mesh={}", mesh));
                }

                if !parts.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", parts.join("  ")),
                        Style::default().fg(Color::Gray),
                    ));
                }

                lines.push(Line::from(spans));
            }
        }
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, area);
}

/// Render a centered kill-confirmation modal dialog over the current frame.
/// Uses `Clear` to erase the background, then draws a left-border-only dialog
/// (no right-side border characters per AGENTS.md).
///
/// Reachable from the cross-platform Insights path, but only ever invoked when
/// `kill_confirm` is `Some`, which happens solely through the Linux-gated kill
/// key handlers. Off-Linux it's compiled but never called.
fn render_kill_dialog(f: &mut Frame, area: Rect, kc: &KillConfirmState) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    const DIALOG_W: u16 = 42;
    const DIALOG_H: u16 = 8;

    let x = area
        .x
        .saturating_add((area.width.saturating_sub(DIALOG_W)) / 2);
    let y = area
        .y
        .saturating_add((area.height.saturating_sub(DIALOG_H)) / 2);
    let dialog_rect = Rect {
        x,
        y,
        width: DIALOG_W.min(area.width),
        height: DIALOG_H.min(area.height),
    };

    f.render_widget(Clear, dialog_rect);

    let content_w = (DIALOG_W.saturating_sub(1)) as usize; // ║ takes 1 col
    let name_short = truncate(&kc.name, 14);
    let dev_label = format!("Dev {}", kc.device_idx);

    let title_pad = content_w.saturating_sub("╔ Kill process? ".chars().count());
    let bottom_pad = content_w;

    let lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                "╔ Kill process? ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("═".repeat(title_pad), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                name_short.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · PID {} · {}", kc.pid, dev_label),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(Span::styled("║", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "enter",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("    silence  (SIGTERM)", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "K",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "        destroy  (SIGKILL)",
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("║  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n · esc", Style::default().fg(Color::Gray)),
            Span::styled("  cancel", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled("║", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            format!("╚{}", "═".repeat(bottom_pad)),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    f.render_widget(Paragraph::new(lines), dialog_rect);
}

/// Compact fleet heat-map for Insights when device count is large.
///
/// Each device is rendered as a single coloured block glyph (`░▒▓█`) with a
/// 2-line layout: top line = coloured blocks, bottom line = device index labels.
/// Colour encodes ASIC temperature via the same `tensix_temp_rgb` ramp used
/// by the full portrait; block density encodes relative power.
/// `cursor` is the currently highlighted device index — that cell is shown with
/// a bright white `◉` instead of the normal block glyph so the user can see
/// where Enter will zoom them in.  Press Enter to drill down to portrait view.
fn render_fleet_heatmap_panel(
    f: &mut Frame,
    area: Rect,
    backend: &dyn TelemetryBackend,
    cursor: usize,
) {
    use crate::ui::tui::chip_portrait::tensix_temp_rgb;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let devices = backend.devices();
    if devices.is_empty() {
        return;
    }

    let cells_per_row = (area.width / FLEET_CELL_W).max(1) as usize;
    let n = devices.len();

    // Clamp cursor to valid device range.
    let cursor = cursor.min(n.saturating_sub(1));
    // Which page (FLEET_PAGE_SIZE block) does the cursor land on?
    let cursor_page_start = (cursor / FLEET_PAGE_SIZE) * FLEET_PAGE_SIZE;

    // Header line — include hint for drill-down.
    let header = Line::from(vec![Span::styled(
        format!(
            " Fleet — {} devices · ←↑↓→ navigate · Enter zoom · temp=colour power=density ",
            n
        ),
        Style::default()
            .fg(colors::rgb(79, 209, 197))
            .add_modifier(Modifier::BOLD),
    )]);

    let mut lines: Vec<Line> = vec![header];
    let mut y: u16 = area.y + 1; // +1 for header

    // Highlight bar: which cells belong to the cursor's page?
    let page_end = (cursor_page_start + FLEET_PAGE_SIZE).min(n);

    let row_count = (n + cells_per_row - 1) / cells_per_row;
    for row in 0..row_count {
        if y + 1 >= area.y + area.height {
            break;
        }

        let mut block_spans: Vec<Span<'static>> = Vec::new();
        let mut label_spans: Vec<Span<'static>> = Vec::new();

        for col in 0..cells_per_row {
            let dev_idx = row * cells_per_row + col;
            if dev_idx >= n {
                break;
            }

            let device = &devices[dev_idx];
            let telem = backend.telemetry(device.index);

            let temp = telem.map(|t| t.temp_c()).unwrap_or(45.0);
            let power = telem.map(|t| t.power_w()).unwrap_or(0.0);

            let [r, g, b] = tensix_temp_rgb(temp);
            let base_color = Color::Rgb(r, g, b);

            let is_cursor = dev_idx == cursor;
            let in_page = dev_idx >= cursor_page_start && dev_idx < page_end;

            let (block_char, cell_style) = if is_cursor {
                // Cursor cell: bright marker
                (
                    '◉',
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                )
            } else if in_page {
                // Page bracket: same glyph but dimmer highlight tint
                let power_frac = (power / 150.0).min(1.0);
                let ch = match (power_frac * 4.0) as u32 {
                    0 => '░',
                    1 => '▒',
                    2 => '▓',
                    _ => '█',
                };
                (
                    ch,
                    Style::default().fg(base_color).add_modifier(Modifier::BOLD),
                )
            } else {
                // Normal cell
                let power_frac = (power / 150.0).min(1.0);
                let ch = match (power_frac * 4.0) as u32 {
                    0 => '░',
                    1 => '▒',
                    2 => '▓',
                    _ => '█',
                };
                (ch, Style::default().fg(base_color))
            };

            // Cell = block + 2 spaces (FLEET_CELL_W = 3)
            block_spans.push(Span::styled(
                format!("{:width$}", block_char, width = FLEET_CELL_W as usize),
                cell_style,
            ));

            // Index label: bright for cursor/page, dim otherwise
            let label = format!("{:<width$}", device.index, width = FLEET_CELL_W as usize);
            let label_style = if is_cursor {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if in_page {
                Style::default().fg(colors::rgb(130, 130, 160))
            } else {
                Style::default().fg(colors::rgb(80, 80, 100))
            };
            label_spans.push(Span::styled(label, label_style));
        }

        lines.push(Line::from(block_spans));
        lines.push(Line::from(label_spans));
        y += 2;
    }

    let widget = Paragraph::new(lines);
    f.render_widget(widget, area);
}

/// Render Insights device panels — one per device, each with a chip portrait on the
/// left and a stats sidebar on the right.
///
/// Width math (Blackhole):
///   Portrait outer (left border ║ + 17 content) = 18 chars
///   Gap = 1 char
///   Stats outer  (left border ║ + 30 content)  = 31 chars
///   Total per device = 18 + 1 + 31 = 50 chars
///
/// Width math (Wormhole):
///   Total = (1 + 10) + 1 + 31 = 43 chars
///
/// `portrait_particles` maps device index → live particle list.  Passed down to
/// `render_chip_portrait` so the particle overlay appears on each portrait.
/// The border color is derived from ASIC temperature via `tensix_temp_rgb`.
/// When device count is large enough that full portraits would overflow the terminal,
/// dispatches to `render_fleet_heatmap_panel` instead.
/// `fleet_cursor` and `fleet_zoom_start` control galaxy-overview cursor highlighting
/// and portrait drill-down zoom respectively.
fn render_device_panels(
    f: &mut Frame,
    area: Rect,
    backend: &dyn TelemetryBackend,
    portrait_particles: &std::collections::HashMap<
        usize,
        Vec<crate::ui::tui::chip_portrait::Particle>,
    >,
    portrait_baseline: &crate::animation::baseline::AdaptiveBaseline,
    fleet_cursor: usize,
    fleet_zoom_start: Option<usize>,
) {
    use crate::ui::tui::chip_portrait::{portrait_dims, render_chip_portrait, tensix_temp_rgb};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let all_devices = backend.devices();
    if all_devices.is_empty() {
        return;
    }

    let panel_h: u16 = 14; // portrait 12 rows + 1 title row + 1 bottom border row
    let inter_row_gap: u16 = 1;

    // Fleet galaxy mode at 32+ devices (matching starfield threshold).
    // Skip this when a zoom slice is active — zoomed view always shows portraits.
    if all_devices.len() >= FLEET_DEVICE_THRESHOLD && fleet_zoom_start.is_none() {
        render_fleet_heatmap_panel(f, area, backend, fleet_cursor);
        return;
    }

    // Determine which slice of devices to render.
    // Zoomed view: FLEET_PAGE_SIZE devices starting at fleet_zoom_start.
    // Normal view: all devices (< FLEET_DEVICE_THRESHOLD).
    let devices: &[crate::models::Device] = if let Some(start) = fleet_zoom_start {
        // Clamp start: the device count can shrink beneath a stale zoom offset
        // (backend swap via `b`/`/remote`, or a remote fleet losing a device),
        // which would otherwise make `start > end` and panic on the slice.
        let start = start.min(all_devices.len());
        let end = (start + FLEET_PAGE_SIZE).min(all_devices.len());
        &all_devices[start..end]
    } else {
        all_devices
    };

    // Zoom banner — shown above the portrait grid when a subset is displayed.
    let (area, zoom_used_rows) = if fleet_zoom_start.is_some() {
        let banner_h: u16 = 1;
        let start = fleet_zoom_start.unwrap();
        let end = (start + FLEET_PAGE_SIZE)
            .min(all_devices.len())
            .saturating_sub(1);
        let total = all_devices.len();
        let banner_text = format!(
            " ▶ Zoomed: chips {}–{} of {}   ← → page   Esc to zoom out ",
            start, end, total,
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                banner_text,
                Style::default()
                    .fg(Color::Rgb(79, 209, 197))
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: banner_h,
            },
        );
        (
            Rect {
                x: area.x,
                y: area.y + banner_h,
                width: area.width,
                height: area.height.saturating_sub(banner_h),
            },
            banner_h,
        )
    } else {
        (area, 0)
    };
    let _ = zoom_used_rows;

    // Compute panel width from first device's architecture (uniform grid).
    // All panels get the same allocated width so grid columns align.
    //
    // Compact mode: when the full-width layout (stats_w=31, gap=1) would require
    let (portrait_cols_first, _) = portrait_dims(devices[0].architecture);
    let portrait_w_first = portrait_cols_first as u16 + 1; // +1 for left border ║
    let n_usize = devices.len();

    let (stats_w, gap_col, cols_per_row) = panel_layout(n_usize, portrait_w_first, area.width);
    let compact = stats_w < 31;
    let panel_w = portrait_w_first + gap_col + stats_w;

    for (panel_idx, device) in devices.iter().enumerate() {
        let col_idx = (panel_idx % cols_per_row) as u16;
        let row_idx = (panel_idx / cols_per_row) as u16;

        let x_offset = area.x + col_idx * (panel_w + PANEL_INTER_COL_GAP);
        let y_offset = area.y + row_idx * (panel_h + inter_row_gap);

        // Skip panels that overflow the allocated area vertically or horizontally
        if y_offset + panel_h > area.y + area.height {
            break;
        }
        if x_offset + panel_w > area.x + area.width {
            continue;
        }

        let idx = device.index;
        let telemetry = backend.telemetry(idx);
        let smbus = backend.smbus_telemetry(idx);

        // Per-device portrait dims (may differ in mixed-arch systems)
        let (portrait_cols, _portrait_rows) = portrait_dims(device.architecture);
        let portrait_w = portrait_cols as u16 + 1; // +1 for left border ║

        let panel_rect = Rect {
            x: x_offset,
            y: y_offset,
            width: panel_w,
            height: panel_h,
        };

        // Derive border color from ASIC temperature — teal when cool, gold when
        // warm, red when hot — using the same `tensix_temp_rgb` heatmap as the
        // Tensix cells in the portrait itself, giving visual continuity.
        let border_color = if let Some(telem) = telemetry {
            let [r, g, b] = tensix_temp_rgb(telem.temp_c());
            Color::Rgb(r, g, b)
        } else {
            Color::DarkGray
        };

        // ── Header rule: arch name + dev index + fixed-width temp ────────────
        // {:>3} right-justifies the temperature to exactly 3 chars, so the rule
        // never changes width when temp crosses 99→100°C.
        // board_type is omitted when it duplicates the architecture name (e.g.
        // sysfs backend sets board_type="blackhole" from the hwmon driver name).
        // It is shown when it carries distinct product info (e.g. "p300c" from JSON).
        let arch_name = device.architecture.name().to_uppercase();
        let temp_i = telemetry.map(|t| t.temp_c() as i32).unwrap_or(0);
        let board_raw = device.board_type.to_lowercase();
        let arch_lower = device.architecture.name().to_lowercase();
        let label = if !board_raw.is_empty() && board_raw != arch_lower && board_raw != "unknown" {
            let board_trim = truncate(&device.board_type, 7);
            format!(
                "── {} · D{} · {} ·{:>3}°C ",
                arch_name, idx, board_trim, temp_i
            )
        } else {
            format!("── {} · D{} ·{:>3}°C ", arch_name, idx, temp_i)
        };
        let trail = (panel_w as usize).saturating_sub(label.chars().count());
        let header_text = format!("{}{}", label, "─".repeat(trail));
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                header_text,
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: panel_rect.x,
                y: panel_rect.y,
                width: panel_w,
                height: 1,
            },
        );

        // ── Portrait (1-col left margin, no box border) ───────────────────────
        let portrait_rect = Rect {
            x: panel_rect.x + 1,
            y: panel_rect.y + 1,
            width: portrait_cols as u16,
            height: panel_h.saturating_sub(2),
        };
        if let Some(telem) = telemetry {
            let particles = portrait_particles
                .get(&idx)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let power_change = portrait_baseline
                .power_change(idx, telem.power_w())
                .max(0.0);
            render_chip_portrait(
                f,
                portrait_rect,
                device,
                telem,
                smbus,
                particles,
                power_change,
            );
        }

        // ── Footer hairline ───────────────────────────────────────────────────
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(panel_w as usize),
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: panel_rect.x,
                y: panel_rect.y + panel_h - 1,
                width: panel_w,
                height: 1,
            },
        );

        // ── Right: stats sidebar (║ for interior rows; header/footer are rules) ─
        // When compact=true the sidebar is narrower (stats_w=19) so all panels
        // fit in a single row.  Core rows (power, ETH, temp, FW) are always shown;
        // secondary rows (GDDR temps, fan RPM) are omitted and label prefixes/bars
        // are shortened to fit the reduced width.
        let stats_x = panel_rect.x + portrait_w + gap_col;
        let interior_h = panel_h.saturating_sub(2); // rows between header and footer
        let stats_border_rect = Rect {
            x: stats_x,
            y: panel_rect.y + 1,
            width: 1,
            height: interior_h,
        };
        let stats_border: Vec<Line> = (0..interior_h as usize)
            .map(|_| Line::from(Span::styled("║", Style::default().fg(border_color))))
            .collect();
        f.render_widget(Paragraph::new(stats_border), stats_border_rect);

        let content_x = stats_x + 1;
        let content_w = stats_w.saturating_sub(1);
        let content_rect = Rect {
            x: content_x,
            y: panel_rect.y + 1,
            width: content_w,
            height: interior_h,
        };

        let mut stat_lines: Vec<Line> = Vec::new();
        // Arch name, device index, and temperature live in the header rule above.
        // Stats sidebar starts directly with telemetry data.

        // Power row
        let power_w_val = telemetry.map(|t| t.power_w()).unwrap_or(0.0);
        // Use device directly — avoid redundant backend.devices() lookup.
        let tdp = device
            .limits
            .as_ref()
            .and_then(|l| l.tdp_limit)
            .unwrap_or(300.0);
        let power_frac = (power_w_val / tdp).min(1.0);
        let power_color = if power_frac > 0.85 {
            Color::Rgb(255, 80, 80)
        } else if power_frac > 0.6 {
            Color::Rgb(236, 150, 184)
        } else if power_frac > 0.3 {
            Color::Rgb(160, 120, 255)
        } else {
            Color::Rgb(79, 209, 197)
        };
        if compact {
            // Compact: "⚡ bar(6) val" — fits in 18 chars total.
            let bar_filled = (power_frac * 6.0) as usize;
            let power_bar: String = (0..6)
                .map(|i| if i < bar_filled { '█' } else { '░' })
                .collect();
            stat_lines.push(Line::from(vec![
                Span::styled("⚡", Style::default().fg(Color::DarkGray)),
                Span::styled(power_bar, Style::default().fg(power_color)),
                Span::styled(
                    format!(" {:5.0}W", power_w_val),
                    Style::default().fg(Color::White),
                ),
            ]));
        } else {
            let bar_filled = (power_frac * 10.0) as usize;
            let power_bar: String = (0..10)
                .map(|i| if i < bar_filled { '█' } else { '░' })
                .collect();
            stat_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<8}", "Power"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(power_bar, Style::default().fg(power_color)),
                Span::styled(
                    format!(" {:5.0}W", power_w_val),
                    Style::default().fg(Color::White),
                ),
            ]));
        }

        // ETH row — use ENABLED_ETH as the denominator (ports provisioned by
        // firmware) rather than the architectural maximum.  On p300c cards
        // ENABLED_ETH=0x3edf gives 12 enabled ports; showing 0/24 looks broken.
        //
        // Fallback hierarchy:
        //   1. SMBUS present + ENABLED_ETH > 0  → use enabled count (accurate)
        //   2. SMBUS present + ENABLED_ETH = 0  → show live/? (transient or no ports)
        //   3. SMBUS absent entirely             → fall back to arch max (no data yet)
        let eth_live_mask = smbus.and_then(|s| s.eth_live_status).unwrap_or(0);
        let eth_enabled_mask = smbus.and_then(|s| s.enabled_eth).unwrap_or(0);
        // Count only ports that are both enabled and live to prevent impossible
        // displays like "15/14 live" if ETH_LIVE_STATUS has bits outside ENABLED_ETH.
        let eth_live_count = if eth_enabled_mask > 0 {
            (eth_live_mask & eth_enabled_mask as u64).count_ones()
        } else {
            eth_live_mask.count_ones()
        };
        let eth_total: Option<u32> = if eth_enabled_mask > 0 {
            Some(eth_enabled_mask.count_ones())
        } else if smbus.is_some() {
            None // SMBUS present but ENABLED_ETH zero — don't invent a total
        } else {
            // No SMBUS data yet — show architectural max so the row isn't empty
            Some(match device.architecture {
                crate::models::Architecture::Blackhole => 24,
                crate::models::Architecture::Wormhole => 20,
                _ => 4,
            })
        };
        let eth_total_display = eth_total
            .map(|t| t.to_string())
            .unwrap_or_else(|| "?".to_string());
        let eth_color = match eth_total {
            Some(t) if eth_live_count == t && t > 0 => Color::Rgb(79, 209, 197), // teal — all live
            _ if eth_live_count > 0 => Color::Rgb(244, 196, 113),                // yellow — partial
            _ => Color::Rgb(96, 125, 139), // muted gray — none live
        };
        if compact {
            stat_lines.push(Line::from(vec![
                Span::styled("E ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}/{}", eth_live_count, eth_total_display),
                    Style::default().fg(eth_color),
                ),
            ]));
        } else {
            const ETH_DOT_CAP: u32 = 16;
            let dots_shown = eth_total.unwrap_or(ETH_DOT_CAP).min(ETH_DOT_CAP);
            // Dot map: iterate enabled bits when available, else live bits.
            let enabled_bits: Vec<u32> = if eth_enabled_mask > 0 {
                (0..32)
                    .filter(|&b| (eth_enabled_mask >> b) & 1 == 1)
                    .collect()
            } else {
                (0..dots_shown).collect()
            };
            let eth_dots: String = enabled_bits
                .iter()
                .take(dots_shown as usize)
                .map(|&bit| {
                    if (eth_live_mask >> bit) & 1 == 1 {
                        '●'
                    } else {
                        '·'
                    }
                })
                .collect();
            let eth_suffix = match eth_total {
                Some(t) if t > ETH_DOT_CAP => format!("+{}", t - ETH_DOT_CAP),
                _ => String::new(),
            };
            stat_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<8}", "ETH"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{}{}", eth_dots, eth_suffix),
                    Style::default().fg(eth_color),
                ),
                Span::styled(
                    format!(" {}/{} live", eth_live_count, eth_total_display),
                    Style::default().fg(Color::White),
                ),
            ]));
        }

        // GDDR temp row (full mode only — too wide for compact)
        if !compact {
            if let Some(s) = smbus {
                let temps: Vec<f32> = s
                    .gddr_temps
                    .iter()
                    .filter_map(|p| p.as_ref())
                    .flat_map(|p| p.0.iter().copied())
                    .filter(|&t| t > 0.0)
                    .collect();
                if !temps.is_empty() {
                    let t_min = temps.iter().copied().fold(f32::INFINITY, f32::min);
                    let t_max = temps.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    let max_color = if t_max > 70.0 {
                        Color::Rgb(255, 80, 80)
                    } else if t_max > 50.0 {
                        Color::Rgb(236, 150, 184)
                    } else {
                        Color::Rgb(79, 209, 197)
                    };
                    stat_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:<8}", "GDDR T"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{:.0}°→{:.0}°C", t_min, t_max),
                            Style::default().fg(max_color),
                        ),
                    ]));
                }
            }
        }

        // ASIC temp row
        let asic_temp = telemetry.map(|t| t.temp_c()).unwrap_or(0.0);
        // Use device directly — avoid redundant backend.devices() lookup.
        let thm_limit = device
            .limits
            .as_ref()
            .and_then(|l| l.thm_limit)
            .unwrap_or(105.0);
        let temp_color = crate::ui::colors::temp_color(asic_temp);
        if compact {
            // Compact: "T 42.1°C" — omit limit annotation.
            stat_lines.push(Line::from(vec![
                Span::styled("T ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:.1}°C", asic_temp),
                    Style::default().fg(temp_color),
                ),
            ]));
        } else {
            stat_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<8}", "Temp"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:.1}°C", asic_temp),
                    Style::default().fg(temp_color),
                ),
                Span::styled(
                    format!(" (lim {:.0}°C)", thm_limit),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        // Fan RPM row (full mode only)
        if !compact {
            if let Some(rpm_str) = smbus.and_then(|s| s.fan_speed.as_deref()) {
                if let Ok(rpm) = rpm_str.trim().parse::<u32>() {
                    if rpm > 0 {
                        stat_lines.push(Line::from(vec![
                            Span::styled(
                                format!("{:<8}", "Fan"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(format!("{} RPM", rpm), Style::default().fg(Color::White)),
                        ]));
                    }
                }
            }
        }

        // Firmware row — shown in both full and compact mode.
        {
            let fw_ver = device
                .firmwares
                .as_ref()
                .and_then(|fw| fw.fw_bundle_version.as_deref())
                .unwrap_or("—");
            let fw_short = fw_ver.trim_start_matches("fw_pack-");
            if compact {
                // Compact: "F " + up to 16 chars (content_w=18). Truncate by
                // chars, not bytes — a byte slice would panic mid-UTF-8 (fw
                // strings are ASCII today, but this matches the rest of the UI).
                let fw_display: String = fw_short.chars().take(16).collect();
                stat_lines.push(Line::from(vec![
                    Span::styled("F ", Style::default().fg(Color::DarkGray)),
                    Span::styled(fw_display, Style::default().fg(Color::White)),
                ]));
            } else {
                // content_w is 30; "FW" label takes 8 chars, leaving 22 for the version string.
                let fw_display: String = fw_short.chars().take(22).collect();
                stat_lines.push(Line::from(vec![
                    Span::styled(format!("{:<8}", "FW"), Style::default().fg(Color::DarkGray)),
                    Span::styled(fw_display, Style::default().fg(Color::White)),
                ]));
            }
        }

        // Pad to fill all interior rows (footer hairline is the visual bottom).
        while stat_lines.len() < interior_h as usize {
            stat_lines.push(Line::raw(""));
        }

        f.render_widget(Paragraph::new(stat_lines), content_rect);
    }
}

/// Render Grid mode: all chip portraits tiled across the terminal.
///
/// Cell width = portrait_cols + 2 (left-border + 1 gap).
/// Only fully fitting cells are rendered — no partial cells or right overflow.
fn render_grid_mode(f: &mut Frame, backend: &dyn TelemetryBackend) {
    use crate::ui::tui::chip_portrait::{portrait_dims, render_chip_portrait};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let area = f.area();
    let devices = backend.devices();
    if devices.is_empty() {
        return;
    }

    // Title bar
    let title = format!(
        "tt-toplike  [Grid]  {} device{}  │  g or v to cycle views  │  q to quit",
        devices.len(),
        if devices.len() == 1 { "" } else { "s" }
    );
    let title_rect = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "{:<w$}",
                // Char-safe: the title contains a multibyte `│`, so a byte
                // slice could land mid-character and panic on a narrow grid.
                truncate(&title, area.width as usize),
                w = area.width as usize
            ),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        title_rect,
    );

    let grid_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    // Use first device's arch for uniform grid layout math. All cells must have
    // the same width and height so the tiled grid lines up correctly. Mixed-arch
    // systems will use per-device dims only for the inner rendering (label, portrait,
    // bottom border) while the grid stride stays based on the first device.
    let (grid_cell_cols, grid_cell_rows) = portrait_dims(devices[0].architecture);
    let cell_w = (grid_cell_cols as u16) + 2; // left-border + content + 1-char gap
    let cell_h = (grid_cell_rows as u16) + 2; // label row + content + bottom-border row

    let cols_per_row = (grid_area.width / cell_w).max(1) as usize;
    let rows_visible = (grid_area.height / cell_h).max(1) as usize;
    let max_cells = cols_per_row * rows_visible;

    for (cell_idx, device) in devices.iter().enumerate().take(max_cells) {
        let grid_col = (cell_idx % cols_per_row) as u16;
        let grid_row = (cell_idx / cols_per_row) as u16;

        let cell_x = grid_area.x + grid_col * cell_w;
        let cell_y = grid_area.y + grid_row * cell_h;

        let idx = device.index;
        let telemetry = backend.telemetry(idx);
        let smbus = backend.smbus_telemetry(idx);

        // Per-device portrait dimensions — may differ in mixed-arch fleets.
        // These drive the inner label, portrait, and bottom-border geometry.
        let (p_cols, p_rows) = portrait_dims(device.architecture);

        // Left border
        let border_rect = Rect {
            x: cell_x,
            y: cell_y,
            width: 1,
            height: cell_h,
        };
        let border_lines: Vec<Line> = (0..cell_h as usize)
            .map(|r| {
                let ch = if r == 0 {
                    "╔"
                } else if r == cell_h as usize - 1 {
                    "╚"
                } else {
                    "║"
                };
                Line::from(Span::styled(ch, Style::default().fg(Color::DarkGray)))
            })
            .collect();
        f.render_widget(Paragraph::new(border_lines), border_rect);

        // Device label
        let label = format!(
            "{} {}",
            device.architecture.abbrev(),
            truncate(&device.board_type, 6)
        );
        let label_rect = Rect {
            x: cell_x + 1,
            y: cell_y,
            width: p_cols as u16,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{:<w$}", truncate(&label, p_cols), w = p_cols),
                Style::default().fg(Color::Gray),
            ))),
            label_rect,
        );

        // Portrait (Grid mode does not have portrait particle state — pass empty slice)
        let portrait_rect = Rect {
            x: cell_x + 1,
            y: cell_y + 1,
            width: p_cols as u16,
            height: p_rows as u16,
        };
        if let Some(telem) = telemetry {
            render_chip_portrait(f, portrait_rect, device, telem, smbus, &[], 0.0);
        }

        // Bottom border (left-side only: ╚ + ═ repeated, no right-cap ╝)
        let bottom_rect = Rect {
            x: cell_x,
            y: cell_y + cell_h - 1,
            width: p_cols as u16 + 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("╚{}", "═".repeat(p_cols)),
                Style::default().fg(Color::DarkGray),
            ))),
            bottom_rect,
        );
    }
}

#[cfg(test)]
mod cmd_tests {
    use super::*;

    fn fresh() -> (
        DisplayMode,
        Duration,
        Duration,
        Option<OverlayPanel>,
        ThrottleState,
    ) {
        (
            DisplayMode::Insights,
            Duration::from_millis(16),
            Duration::from_millis(250),
            None,
            ThrottleState::new(false, false),
        )
    }

    #[test]
    fn quit_verbs_set_should_quit() {
        for verb in &["quit", "q", "exit", "/quit", "/q", "/exit"] {
            let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
            let (msg, is_err, should_quit) =
                execute_command(verb, &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
            assert!(should_quit, "{verb} should set should_quit");
            assert!(!is_err, "{verb} should not be an error");
            assert!(msg.is_empty(), "{verb} message should be empty");
        }
    }

    #[test]
    fn mode_command_switches_display_mode() {
        let cases = [
            ("mode insights", DisplayMode::Insights),
            ("mode grid", DisplayMode::Grid),
            ("mode starfield", DisplayMode::Starfield),
            ("mode castle", DisplayMode::MemoryCastle),
            ("mode flow", DisplayMode::MemoryFlow),
            ("mode arcade", DisplayMode::Arcade),
            ("mode defrag", DisplayMode::Defrag),
        ];
        for (cmd, expected) in cases {
            let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
            let (_, is_err, should_quit) =
                execute_command(cmd, &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
            assert_eq!(mode, expected, "/{cmd}");
            assert!(!is_err);
            assert!(!should_quit);
        }
    }

    #[test]
    fn mode_command_unknown_arg_returns_error() {
        let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
        let (_, is_err, should_quit) = execute_command(
            "mode bogus",
            &mut mode,
            &mut ap,
            &mut dp,
            1.0,
            &mut ov,
            &mut ts,
        );
        assert!(is_err);
        assert!(!should_quit);
        assert_eq!(mode, DisplayMode::Insights, "mode unchanged on bad arg");
    }

    #[test]
    fn legend_help_explain_toggle_overlays() {
        for (cmd, expected_panel) in &[
            ("legend", OverlayPanel::Legend),
            ("l", OverlayPanel::Legend),
            ("help", OverlayPanel::Help),
            ("?", OverlayPanel::Help),
            ("", OverlayPanel::Help),
            ("explain", OverlayPanel::Explain),
        ] {
            let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
            // First call: panel should appear.
            execute_command(cmd, &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
            assert_eq!(
                ov,
                Some(*expected_panel),
                "/{cmd} should open {expected_panel:?}"
            );
            // Second call: panel should dismiss (toggle).
            execute_command(cmd, &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
            assert_eq!(ov, None, "/{cmd} second call should close panel");
        }
    }

    #[test]
    fn fps_and_datafps_commands() {
        let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
        let (msg, is_err, _) =
            execute_command("fps 30", &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
        assert!(!is_err, "fps 30 should succeed");
        assert!(msg.contains("30"));
        assert_eq!(ap, Duration::from_secs_f64(1.0 / 30.0));

        let (_, is_err, _) =
            execute_command("fps 0", &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
        assert!(is_err, "fps 0 out of range");

        let (msg, is_err, _) = execute_command(
            "datafps 10",
            &mut mode,
            &mut ap,
            &mut dp,
            1.0,
            &mut ov,
            &mut ts,
        );
        assert!(!is_err);
        assert!(msg.contains("10"));
        assert_eq!(dp, Duration::from_secs_f64(1.0 / 10.0));
    }

    #[test]
    fn unknown_command_returns_error() {
        let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
        let (msg, is_err, should_quit) = execute_command(
            "boguscommand",
            &mut mode,
            &mut ap,
            &mut dp,
            1.0,
            &mut ov,
            &mut ts,
        );
        assert!(is_err);
        assert!(!should_quit);
        assert!(
            msg.contains("boguscommand"),
            "error message should echo the verb"
        );
    }

    #[test]
    fn slash_prefix_is_stripped() {
        // "/quit" should work identically to "quit" — the verb parser strips leading '/'.
        let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
        let (_, _, should_quit) =
            execute_command("/quit", &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
        assert!(should_quit);
    }

    #[test]
    fn throttle_command_toggles() {
        let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
        assert!(!ts.throttle_on, "starts off");
        let (msg, is_err, _) = execute_command(
            "throttle", &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts,
        );
        assert!(!is_err);
        assert!(ts.throttle_on, "toggled on");
        assert!(msg.contains("on"));
        // Toggle again
        execute_command(
            "throttle", &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts,
        );
        assert!(!ts.throttle_on, "toggled off");
    }

    #[test]
    fn idle_on_blur_command_toggles() {
        let (mut mode, mut ap, mut dp, mut ov, mut ts) = fresh();
        assert!(!ts.idle_on_blur, "starts off");
        let (msg, is_err, _) = execute_command(
            "idle-on-blur",
            &mut mode,
            &mut ap,
            &mut dp,
            1.0,
            &mut ov,
            &mut ts,
        );
        assert!(!is_err);
        assert!(ts.idle_on_blur, "toggled on");
        assert!(msg.contains("on"));
        // Alias also works
        execute_command(
            "idleonblur",
            &mut mode,
            &mut ap,
            &mut dp,
            1.0,
            &mut ov,
            &mut ts,
        );
        assert!(!ts.idle_on_blur, "toggled off via alias");
    }

    // ── /remote picker-selection logic (pure) ───────────────────────────────
    //
    // The full runtime hot-swap / connect path spawns a WsBackend against a live
    // socket, so it is verified manually (see REMOTE_UX_REPORT.md). Here we cover
    // the pure selection logic that decides *what* to connect to.

    #[cfg(feature = "remote")]
    mod remote {
        use super::super::resolve_remote_pick;
        use crate::backend::discovery::DiscoveredBox;

        fn boxes() -> Vec<DiscoveredBox> {
            vec![
                DiscoveredBox {
                    name: "qb2-lab".into(),
                    host: "192.168.1.42".into(),
                    ctrl_port: 8899,
                    chips: "4xBH".into(),
                    status: "idle".into(),
                    apiver: 1,
                },
                DiscoveredBox {
                    name: "qb-serving".into(),
                    host: "10.0.0.7".into(),
                    ctrl_port: 8765,
                    chips: "8xWH".into(),
                    status: "serving:llama3".into(),
                    apiver: 1,
                },
            ]
        }

        #[test]
        fn picks_by_one_based_index() {
            let b = boxes();
            assert_eq!(resolve_remote_pick(&b, "1").unwrap(), "192.168.1.42:8899");
            assert_eq!(resolve_remote_pick(&b, "2").unwrap(), "10.0.0.7:8765");
        }

        #[test]
        fn out_of_range_index_errors() {
            let b = boxes();
            assert!(resolve_remote_pick(&b, "0").is_err());
            assert!(resolve_remote_pick(&b, "3").is_err());
        }

        #[test]
        fn picks_by_name() {
            let b = boxes();
            assert_eq!(
                resolve_remote_pick(&b, "qb-serving").unwrap(),
                "10.0.0.7:8765"
            );
        }

        #[test]
        fn host_port_passes_through() {
            let b = boxes();
            assert_eq!(
                resolve_remote_pick(&b, "1.2.3.4:9000").unwrap(),
                "1.2.3.4:9000"
            );
            // Works with no cached list too (e.g. `/remote host:port` cold).
            assert_eq!(
                resolve_remote_pick(&[], "1.2.3.4:9000").unwrap(),
                "1.2.3.4:9000"
            );
        }

        #[test]
        fn unknown_name_without_list_errors() {
            // Not an index, not a listed name, not host:port → error (the caller
            // then falls back to a fresh discovery-by-name).
            assert!(resolve_remote_pick(&[], "mystery-box").is_err());
        }
    }

    // ── /serve command parsing (pure) ───────────────────────────────────────
    //
    // Binding/spawning the publisher needs the live backend (for
    // `snapshot_json()`) and mutates the loop's `publisher` slot, so — like
    // `/remote` — the actual dispatch lives in the key-handler match, not
    // `execute_command`. This only covers the pure argument→intent mapping.

    #[cfg(feature = "remote")]
    mod parse_serve_command_tests {
        use super::super::{parse_serve_command, ServeCmd};

        #[test]
        fn bare_serve_starts_with_default_bind() {
            assert_eq!(parse_serve_command(""), ServeCmd::Start(None));
        }

        #[test]
        fn whitespace_only_is_also_default_start() {
            assert_eq!(parse_serve_command("   "), ServeCmd::Start(None));
        }

        #[test]
        fn bare_port_is_a_start_spec() {
            assert_eq!(
                parse_serve_command("9000"),
                ServeCmd::Start(Some("9000".to_string()))
            );
        }

        #[test]
        fn host_port_is_a_start_spec() {
            assert_eq!(
                parse_serve_command("1.2.3.4:9000"),
                ServeCmd::Start(Some("1.2.3.4:9000".to_string()))
            );
        }

        #[test]
        fn off_stops() {
            assert_eq!(parse_serve_command("off"), ServeCmd::Stop);
        }

        #[test]
        fn stop_stops() {
            assert_eq!(parse_serve_command("stop"), ServeCmd::Stop);
        }

        #[test]
        fn stop_is_case_insensitive() {
            assert_eq!(parse_serve_command("OFF"), ServeCmd::Stop);
            assert_eq!(parse_serve_command("Stop"), ServeCmd::Stop);
        }

        #[test]
        fn unresolvable_host_is_still_a_start_spec() {
            // `parse_serve_command` is a pure string-shape parse — it never
            // does DNS resolution (that's `cli::parse_serve_bind`'s job, run
            // only once the command actually dispatches), so a bogus host
            // still comes back as a `Start` spec for the caller to validate
            // and reject.
            assert_eq!(
                parse_serve_command("wat"),
                ServeCmd::Start(Some("wat".to_string()))
            );
        }

        #[test]
        fn trailing_garbage_is_bad() {
            assert!(matches!(
                parse_serve_command("9000 extra"),
                ServeCmd::Bad(_)
            ));
        }
    }
}

#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
#[cfg(test)]
mod tests {
    use super::panel_layout;

    // BH portrait_w = 17 cols + 1 border = 18.
    const BH_PORTRAIT_W: u16 = 18;

    #[test]
    fn panel_layout_4_chips_full_mode_at_160_cols() {
        // 160-wide terminal: balanced=2, full 2-col row = 2*50+1 = 101 ≤ 160 → full mode.
        let (stats_w, gap_col, cols_per_row) = panel_layout(4, BH_PORTRAIT_W, 160);
        assert_eq!(stats_w, 31, "should be full mode");
        assert_eq!(gap_col, 1);
        assert_eq!(cols_per_row, 2, "4 chips → 2×2 grid");
    }

    #[test]
    fn panel_layout_4_chips_compact_mode_at_80_cols() {
        // 80-wide terminal: full 2-col row = 101 > 80 → compact mode.
        let (stats_w, gap_col, cols_per_row) = panel_layout(4, BH_PORTRAIT_W, 80);
        assert_eq!(stats_w, 19, "should be compact mode");
        assert_eq!(gap_col, 0);
        assert_eq!(
            cols_per_row, 2,
            "compact panels (37 cols) still fit 2 per row"
        );
    }

    #[test]
    fn panel_layout_4_chips_very_narrow_single_column() {
        // 37-wide terminal: compact panel = 37 cols; cols_per_row must be 1 (not 0).
        let (stats_w, _, cols_per_row) = panel_layout(4, BH_PORTRAIT_W, 37);
        assert_eq!(stats_w, 19, "compact at very narrow width");
        assert_eq!(cols_per_row, 1, "only one panel fits at 37 cols");
    }

    #[test]
    fn panel_layout_1_chip_always_full() {
        // Single chip: balanced=1, full row = 1*50 = 50; fits at most terminals.
        let (stats_w, _, cols_per_row) = panel_layout(1, BH_PORTRAIT_W, 120);
        assert_eq!(stats_w, 31);
        assert_eq!(cols_per_row, 1);
    }

    #[test]
    fn panel_layout_9_chips_3x3_grid() {
        // 9 chips: balanced=ceil(sqrt(9))=3; full 3-col row = 3*50+2 = 152.
        let (stats_w, _, cols_per_row) = panel_layout(9, BH_PORTRAIT_W, 160);
        assert_eq!(stats_w, 31);
        assert_eq!(cols_per_row, 3, "9 chips → 3×3 grid");
    }
}

/// Cross-platform tests for the default (Insights) screen fallback.
///
/// Unlike the Linux-gated `tests` module above, these run on every platform —
/// they guard the behaviour that matters off-Linux (e.g. `--host` on macOS).
#[cfg(test)]
mod inference_roster_tests {
    use super::inference_roster_lines;
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};

    fn svc(key: &str, label: &str, phase: Phase) -> ServiceState {
        ServiceState {
            key: key.into(),
            label: label.into(),
            phase,
            cpu_pct: 0.0,
            rss_bytes: 0,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            loaded_count: 0,
            loaded_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::Down,
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving: None,
            media: None,
        }
    }

    fn text(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn roster_empty_for_zero_or_one_service() {
        assert!(inference_roster_lines(&[], 80).is_empty());
        assert!(inference_roster_lines(&[svc("a", "A", Phase::Ready)], 80).is_empty());
    }

    #[test]
    fn roster_lists_all_with_header_and_marks_the_featured_one() {
        let rows = vec![
            svc("a", "Model-A", Phase::Ready),   // serving
            svc("b", "Model-B", Phase::Loading), // loading — featured (loading wins)
            svc("c", "Model-C", Phase::Ready),
            svc("d", "Model-D", Phase::Down),
        ];
        let lines = inference_roster_lines(&rows, 80);
        assert_eq!(lines.len(), 5, "header + 4 service rows");
        assert!(
            text(&lines[0]).contains("inference services (4)"),
            "header: {}",
            text(&lines[0])
        );
        let featured: Vec<usize> = (1..5).filter(|&i| text(&lines[i]).contains('▸')).collect();
        assert_eq!(featured.len(), 1, "exactly one featured row");
        assert!(
            text(&lines[featured[0]]).contains("Model-B"),
            "the loading model is featured"
        );
    }

    #[test]
    fn roster_caps_and_notes_hidden_count() {
        let rows: Vec<ServiceState> = (0..12)
            .map(|i| svc(&format!("k{i}"), &format!("M{i}"), Phase::Ready))
            .collect();
        let lines = inference_roster_lines(&rows, 80);
        assert_eq!(lines.len(), 1 + 8, "header + capped 8 rows");
        assert!(
            text(&lines[0]).contains("+4 more"),
            "header notes the hidden services: {}",
            text(&lines[0])
        );
    }

    #[test]
    fn roster_stat_is_gen_in_flight_for_media_tok_s_for_vllm() {
        use crate::workload::inference_server::metrics::{MediaStats, ServingStats};

        // A vLLM service reports tok/s + running; a media service reports the
        // in-flight/done generations (never tok/s, which is meaningless for it).
        let mut vllm = svc("v", "Qwen3-32B", Phase::Ready);
        vllm.serving = Some(ServingStats {
            generation_tps: 842.0,
            requests_running: 3,
            ..serving_zero()
        });
        let mut media = svc("m", "SkyReels-V2-I2V", Phase::Ready);
        media.media = Some(MediaStats {
            jobs_in_progress: 2,
            counters: crate::workload::inference_server::MediaCounters {
                requests_total: 43,
                ..Default::default()
            },
            ..media_zero()
        });

        let lines = inference_roster_lines(&[vllm, media], 100);
        let joined: String = lines.iter().map(text).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("842 tok/s · 3 run"), "vLLM row: {joined}");
        assert!(
            joined.contains("2 in flight · 43 done"),
            "media row: {joined}"
        );
        assert!(
            !joined.contains("tok/s · 43") && !joined.contains("SkyReels-V2-I2V  842"),
            "media row must not borrow the vLLM tok/s formatting"
        );
    }

    /// All-zero `ServingStats` so a test overrides only the fields it cares about.
    fn serving_zero() -> crate::workload::inference_server::metrics::ServingStats {
        crate::workload::inference_server::metrics::ServingStats {
            generation_tps: 0.0,
            prompt_tps: 0.0,
            requests_running: 0,
            requests_waiting: 0,
            kv_cache_usage: 0.0,
            ttft_avg_s: 0.0,
            queue_avg_s: 0.0,
            prefill_avg_s: 0.0,
            decode_avg_s: 0.0,
            tpot_avg_s: 0.0,
            completed_delta: 0,
            errored_delta: 0,
            prefix_hit_rate: 0.0,
            preemptions_delta: 0,
            counters: Default::default(),
        }
    }

    /// All-zero `MediaStats` so a test overrides only the fields it cares about.
    fn media_zero() -> crate::workload::inference_server::metrics::MediaStats {
        crate::workload::inference_server::metrics::MediaStats {
            generations_per_min: 0.0,
            jobs_in_progress: 0,
            completed_delta: 0,
            errored_delta: 0,
            duration_avg_s: 0.0,
            post_avg_s: 0.0,
            pre_avg_s: 0.0,
            inference_avg_s: 0.0,
            warmup_avg_s: 0.0,
            counters: Default::default(),
        }
    }
}

/// Tests for the `--remote` "LOCAL, not streamed" honesty note (see
/// `remote_local_fallback_note`) — cross-platform since the helper is pure.
#[cfg(test)]
mod remote_fallback_note_tests {
    use super::remote_local_fallback_note;
    use crate::cli::BackendType;

    #[test]
    fn remote_fallback_note_only_for_remote_without_data() {
        assert_eq!(
            remote_local_fallback_note(BackendType::Remote, false),
            Some("LOCAL — remote not streamed")
        );
        assert_eq!(remote_local_fallback_note(BackendType::Remote, true), None); // remote data present
        assert_eq!(remote_local_fallback_note(BackendType::Json, false), None); // local backend, no note
    }
}

/// Tests for the pure `RemoteProc`/`RemoteInference` -> local-type mapper
/// helpers that back the `--remote` process panel and `[i]` view (see the
/// call sites in the main loop).
#[cfg(all(test, feature = "remote"))]
mod remote_mapper_tests {
    use super::{phase_from_str, remote_inference_to_service_state, remote_proc_to_row};
    use crate::backend::{RemoteInference, RemoteProc, RemoteServing};
    use crate::workload::inference_server::{Phase, Readiness};

    #[test]
    fn phase_from_str_maps_every_known_wire_value() {
        assert_eq!(phase_from_str("down"), Phase::Down);
        assert_eq!(phase_from_str("compiling"), Phase::Compiling);
        assert_eq!(phase_from_str("loading"), Phase::Loading);
        assert_eq!(phase_from_str("ready"), Phase::Ready);
        assert_eq!(phase_from_str("alarm"), Phase::Alarm);
    }

    #[test]
    fn phase_from_str_unknown_reads_as_down() {
        // A future schema's new phase name, or a corrupt frame, must degrade
        // to the safest "nothing happening" reading rather than panic/guess.
        assert_eq!(phase_from_str("quiescent"), Phase::Down);
        assert_eq!(phase_from_str(""), Phase::Down);
        assert_eq!(phase_from_str("READY"), Phase::Down); // case-sensitive by wire contract
    }

    #[test]
    fn remote_proc_to_row_maps_fields_and_never_synthesizes_tt_info() {
        let rp = RemoteProc {
            pid: 4242,
            name: "tt-inference-server".to_string(),
            cmd: "/usr/bin/tt-inference-server --port 8080".to_string(),
            uses_tt: true,
            cpu_pct: 87.5,
            mem_bytes: 12_884_901_888,
        };
        let row = remote_proc_to_row(&rp);
        assert_eq!(row.pid, 4242);
        assert_eq!(row.name, "tt-inference-server");
        assert_eq!(row.cpu_pct, 87.5);
        assert_eq!(row.mem_bytes, 12_884_901_888);
        // Deliberately not carried: the remote wire shape has no detailed
        // TtProcInfo (device indices / hugepages), and no inference-runtime
        // classification.
        assert!(row.tt.is_none());
        assert!(row.inference.is_none());
        assert!(!row.active);
    }

    #[test]
    fn remote_inference_to_service_state_maps_ready_with_serving() {
        let serving = RemoteServing {
            generation_tps: 842.0,
            prompt_tps: 20.0,
            requests_running: 1,
            requests_waiting: 2,
            kv_cache_usage: 0.04,
            ttft_avg_s: 0.18,
            queue_avg_s: 0.01,
            prefill_avg_s: 1.0,
            decode_avg_s: 3.0,
            tpot_avg_s: 0.02,
            completed_delta: 2,
            errored_delta: 0,
            prefix_hit_rate: 0.75,
            preemptions_delta: 1,
        };
        let ri = RemoteInference {
            key: "vllm-llama3-70b".to_string(),
            label: "Llama-3 70B (vLLM)".to_string(),
            phase: "ready".to_string(),
            progress: Some(1.0),
            serving: Some(serving),
            media: None,
        };
        let state = remote_inference_to_service_state(&ri);
        assert_eq!(state.key, "vllm-llama3-70b");
        assert_eq!(state.label, "Llama-3 70B (vLLM)");
        assert_eq!(state.phase, Phase::Ready);
        assert_eq!(state.progress, Some(1.0));
        assert_eq!(state.readiness, Readiness::Ready { runner: None });
        // No remote equivalent for these locally-derived probe internals.
        assert_eq!(state.cpu_pct, 0.0);
        assert_eq!(state.rss_bytes, 0);
        assert_eq!(state.kernel_count, 0);
        assert_eq!(state.flat_ticks, 0);
        assert!(state.top_proc.is_none());
        let stats = state.serving.expect("serving should map through");
        assert_eq!(stats.generation_tps, 842.0);
        assert_eq!(stats.prompt_tps, 20.0);
        assert_eq!(stats.requests_running, 1);
        assert_eq!(stats.requests_waiting, 2);
        assert_eq!(stats.kv_cache_usage, 0.04);
        assert_eq!(stats.ttft_avg_s, 0.18);
        assert_eq!(stats.queue_avg_s, 0.01);
        assert_eq!(stats.prefill_avg_s, 1.0);
        assert_eq!(stats.decode_avg_s, 3.0);
        assert_eq!(stats.tpot_avg_s, 0.02);
        assert_eq!(stats.completed_delta, 2);
        assert_eq!(stats.errored_delta, 0);
        assert_eq!(stats.prefix_hit_rate, 0.75);
        assert_eq!(stats.preemptions_delta, 1);
        // No remote equivalent for the raw vLLM counters (deltas are computed
        // locally only) — always the zeroed default.
        assert_eq!(stats.counters, Default::default());
    }

    #[test]
    fn remote_inference_to_service_state_maps_down_and_loading_readiness() {
        let down = RemoteInference {
            key: "k".into(),
            label: "L".into(),
            phase: "down".into(),
            progress: None,
            serving: None,
            media: None,
        };
        let state = remote_inference_to_service_state(&down);
        assert_eq!(state.phase, Phase::Down);
        assert_eq!(state.readiness, Readiness::Down);
        assert!(state.serving.is_none());
        assert!(state.progress.is_none());

        let loading = RemoteInference {
            key: "k".into(),
            label: "L".into(),
            phase: "loading".into(),
            progress: Some(0.4),
            serving: None,
            media: None,
        };
        let state = remote_inference_to_service_state(&loading);
        assert_eq!(state.phase, Phase::Loading);
        assert_eq!(state.readiness, Readiness::NotReady);
        assert_eq!(state.progress, Some(0.4));
    }

    #[test]
    fn remote_inference_to_service_state_unknown_phase_reads_as_down() {
        let ri = RemoteInference {
            key: "k".into(),
            label: "L".into(),
            phase: "totally-unknown".into(),
            progress: None,
            serving: None,
            media: None,
        };
        let state = remote_inference_to_service_state(&ri);
        assert_eq!(state.phase, Phase::Down);
        assert_eq!(state.readiness, Readiness::Down);
    }

    #[test]
    fn remote_media_maps_through_with_completed_total_for_done_count() {
        // Regression (PR #21 review): a remote media workload must NOT read
        // "0 done". `completed_total` crosses the wire and is restored into
        // `counters.requests_total` (the field the roster/panel show as "done").
        let ri = RemoteInference {
            key: "sky".into(),
            label: "SkyReels-V2-I2V".into(),
            phase: "ready".into(),
            progress: None,
            serving: None,
            media: Some(crate::backend::RemoteMedia {
                generations_per_min: 2.1,
                jobs_in_progress: 2,
                completed_total: 43,
                errored_total: 2,
                completed_delta: 1,
                errored_delta: 0,
                duration_avg_s: 612.0,
                post_avg_s: 0.32,
                pre_avg_s: 0.0,
                inference_avg_s: 0.0,
                warmup_avg_s: 0.0,
            }),
        };
        let state = remote_inference_to_service_state(&ri);
        assert!(state.serving.is_none());
        let m = state.media.expect("media maps through");
        assert_eq!(m.jobs_in_progress, 2);
        assert_eq!(
            m.counters.requests_total, 43,
            "completed_total restored into counters → roster shows '43 done', not 0"
        );
        assert_eq!(
            m.counters.errored_total, 2,
            "errored_total restored into counters → media panel shows 'errors 2', not 0"
        );
    }
}

#[cfg(test)]
mod host_default_screen_tests {
    use super::render_grid_mode;
    use crate::backend::host::HostBackend;
    use crate::backend::TelemetryBackend;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Regression: the default Insights screen used to render nothing off-Linux
    /// (the `render_insights_no_procfs` stub was a no-op), so `--host` on macOS
    /// showed a blank screen. The fallback now paints the chip-portrait grid;
    /// assert it produces a non-empty buffer with the host device's telemetry.
    #[test]
    fn default_screen_paints_host_device() {
        let mut backend = HostBackend::new();
        backend.init().expect("host init");
        backend.update().expect("host update");

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        terminal
            .draw(|f| render_grid_mode(f, &backend))
            .expect("draw");

        let painted: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        let glyphs = painted.chars().filter(|c| !c.is_whitespace()).count();
        assert!(
            glyphs > 20,
            "host default screen must not be blank (only {glyphs} non-space glyphs)"
        );
    }

    /// Regression: the legend/help/explain overlay panel was a fixed 42 columns
    /// wide while its content lines are 50–66 display columns, so `Paragraph`
    /// (which clips, not wraps) silently truncated the tail of every long line
    /// — e.g. the `/serve` help line lost "(plaintext, trusted-LAN)". The panel
    /// now sizes itself to the widest content line. Render each overlay on a
    /// roomy terminal and assert the full text of the widest line survives.
    #[test]
    fn overlay_panel_does_not_truncate_wide_content() {
        use super::{
            line_cols, overlay_panel_lines, render_overlay_panel, DisplayMode, OverlayPanel,
        };

        // Terminal wide enough that the panel is never terminal-clamped.
        let (w, h) = (140u16, 44u16);
        let cases = [
            (OverlayPanel::Help, DisplayMode::Insights),
            (OverlayPanel::Legend, DisplayMode::Insights),
            (OverlayPanel::Legend, DisplayMode::InferenceMonitor),
            (OverlayPanel::Explain, DisplayMode::Insights),
        ];
        for (kind, mode) in cases {
            // The widest content line for this panel, as plain text.
            let widest: String = overlay_panel_lines(kind, mode)
                .into_iter()
                .max_by_key(|l| line_cols(l))
                .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
                .unwrap_or_default();
            // Trim the "║ " border prefix and surrounding whitespace for the search.
            let needle: String = widest.trim_start_matches('║').trim().to_string();

            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
            terminal
                .draw(|f| render_overlay_panel(f, kind, mode))
                .expect("draw");

            // Flatten the buffer row-by-row so a single on-screen line stays contiguous.
            let buf = terminal.backend().buffer().clone();
            let mut rows: Vec<String> = Vec::new();
            for y in 0..buf.area.height {
                let mut row = String::new();
                for x in 0..buf.area.width {
                    row.push_str(buf[(x, y)].symbol());
                }
                rows.push(row);
            }
            let painted = rows.join("\n");

            assert!(
                painted.contains(&needle),
                "{kind:?}/{mode:?}: widest legend line was truncated.\n\
                 expected to find: {needle:?}\nrendered:\n{painted}"
            );
        }
    }

    /// Regression: the Grid title / device labels contain a multibyte `│`, so a
    /// byte-offset truncation could land mid-character and panic on a narrow
    /// terminal. Render across small widths (including ones that would slice
    /// inside the `│`) and assert no panic.
    #[test]
    fn grid_mode_narrow_terminal_does_not_panic() {
        let mut backend = HostBackend::new();
        backend.init().expect("host init");
        backend.update().expect("host update");
        for (w, h) in [(1, 4), (5, 6), (12, 8), (40, 3), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
            terminal
                .draw(|f| render_grid_mode(f, &backend))
                .expect("draw must not panic");
        }
    }
}

/// Regression: `render_snake_view` gained a 4th `band` param (the education
/// footer). A tiny terminal where the band + status row would exceed the
/// available height must shrink/clip gracefully rather than panic.
#[cfg(test)]
mod snake_band_tests {
    use ratatui::backend::TestBackend;
    use ratatui::text::Line;
    use ratatui::Terminal;

    #[test]
    fn render_snake_view_with_band_does_not_panic() {
        let snake = crate::animation::Snake::new();
        let roster: Vec<Line<'static>> = Vec::new();
        let band = crate::workload::inference_server::education::compose_band(
            "Llama-3.1-8B-Instruct",
            crate::workload::inference_server::Phase::Loading,
            80,
            5,
            0,
        );
        // A tiny terminal must also not panic (band + status exceed height).
        for (w, h) in [(80u16, 24u16), (40, 8), (20, 3)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| super::render_snake_view(f, &snake, &roster, &band))
                .unwrap();
        }
    }
}

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
        let avg = if times.is_empty() {
            0.0
        } else {
            times.iter().sum::<f64>() / times.len() as f64
        };
        let p95 = bench::percentile_ms(&mut times, 95.0);
        bench::BenchResult {
            mode,
            frames,
            avg_ms: avg,
            p95_ms: p95,
            target_fps,
            est_cpu_pct: bench::est_cpu_pct(avg, target_fps),
        }
    }

    let mk_term = || Terminal::new(TestBackend::new(width, height)).unwrap();

    // Insights (data, 10 fps)
    {
        let mut term = mk_term();
        let mut hpm = crate::workload::HostProcessMonitor::new();
        hpm.update(); // one scan, not timed
        let rows = hpm.rows(12, &std::collections::HashMap::new()); // snapshot, not timed
        let particles: std::collections::HashMap<
            usize,
            Vec<crate::ui::tui::chip_portrait::Particle>,
        > = std::collections::HashMap::new();
        let baseline = crate::animation::baseline::AdaptiveBaseline::new();
        // Built once outside the timed loop — render_insights ignores it (`_cli`),
        // but constructing it per frame would add noise to the insights timing.
        let bench_cli = crate::cli::Cli::bench_default();
        results.push(measure("insights", 10, frames, || {
            term.draw(|f| {
                render_insights(
                    f,
                    backend,
                    &rows,
                    0,
                    None,
                    &bench_cli,
                    &particles,
                    &baseline,
                    0,
                    None,
                    0.0,
                    0,
                    0,
                    false,
                    None,
                    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
                    &std::collections::HashMap::new(),
                );
            })
            .unwrap();
        }));
    }

    // Grid (data, 10 fps)
    {
        let mut term = mk_term();
        results.push(measure("grid", 10, frames, || {
            term.draw(|f| render_grid_mode(f, backend)).unwrap();
        }));
    }

    // Inference Monitor (anim, 60 fps — the `[i]` view is in is_anim_mode). The
    // live view is the unified serving snake (`render_snake_view`). We bench two
    // states: `inference-monitor` = the cheap Roaming screensaver (empty
    // snapshot, as on non-Linux or with no containers), and `inference-serving`
    // = the heaviest realistic frame — a 4-model roster above the featured
    // serving dashboard (timeline + swimlanes + silicon strip).
    let arch = backend
        .devices()
        .first()
        .map(|d| d.architecture.name())
        .unwrap_or("Unknown");
    {
        let mut term = mk_term();
        let mut snake = crate::animation::Snake::new();
        results.push(measure("inference-monitor", 60, frames, || {
            snake.update(&crate::animation::SnakeWorld {
                rows: &[],
                catalog: &[],
                arch,
                chips: &[],
                cadence_secs: 5,
                width: width as usize,
                height: height as usize,
            });
            term.draw(|f| render_snake_view(f, &snake, &[], &[]))
                .unwrap();
        }));
    }
    {
        use crate::workload::inference_server::{
            Phase, Readiness, ServiceState, ServingStats, VllmCounters,
        };
        let counters = VllmCounters {
            generation_tokens_total: 100_000,
            prompt_tokens_total: 20_000,
            requests_running: 6,
            requests_waiting: 2,
            kv_cache_usage: 0.42,
            ttft_sum: 3.0,
            ttft_count: 20,
            ..Default::default()
        };
        let stats = ServingStats::fold(None, &counters, 5);
        let mk = |key: &str, label: &str, serving| ServiceState {
            key: key.into(),
            label: label.into(),
            phase: Phase::Ready,
            cpu_pct: 30.0,
            rss_bytes: 8_000_000_000,
            rss_delta: 0,
            kernel_count: 500,
            kernel_delta: 0,
            loaded_count: 1400,
            loaded_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::Ready { runner: None },
            top_proc: Some("python3".into()),
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving,
            media: None,
        };
        // 4 models → the featured one Feeds the full serving dashboard, and the
        // roster lists all four (the multi-model case the view now handles).
        let rows = vec![
            mk("a", "Llama-3.1-8B", Some(stats)),
            mk("b", "Qwen3-32B", Some(stats)),
            mk("c", "Mistral-7B-Instruct", None),
            mk("d", "gemma-3-1b-it", None),
        ];
        let chips: Vec<crate::animation::ChipReading> = (0..4)
            .map(|i| crate::animation::ChipReading {
                index: i,
                arch,
                power_w: Some(64.0),
                temp_c: Some(59.0),
                aiclk_mhz: Some(1000),
            })
            .collect();
        let roster = inference_roster_lines(&rows, width as usize);
        let roster_h = roster.len();
        let mut term = mk_term();
        let mut snake = crate::animation::Snake::new();
        results.push(measure("inference-serving", 60, frames, || {
            snake.update(&crate::animation::SnakeWorld {
                rows: &rows,
                catalog: &[],
                arch,
                chips: &chips,
                cadence_secs: 5,
                width: width as usize,
                height: (height as usize).saturating_sub(1 + roster_h),
            });
            term.draw(|f| render_snake_view(f, &snake, &roster, &[]))
                .unwrap();
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
