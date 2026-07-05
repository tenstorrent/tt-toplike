// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! tt-toplike - Terminal User Interface
//!
//! This binary provides a beautiful terminal-based interface for monitoring
//! Tenstorrent hardware using Ratatui and Crossterm.
//!
//! Features:
//! - Real-time telemetry display
//! - Hardware-responsive psychedelic visualizations
//! - Multiple visualization modes (Starfield, TRON Grid, etc.)
//! - Adaptive baseline learning for universal hardware sensitivity
//! - Dark-mode optimized color palette

#[cfg(feature = "remote")]
use std::io::IsTerminal;

use tt_toplike::{
    backend::{json::JSONBackend, mock::MockBackend, BackendConfig, TelemetryBackend},
    cli::{BackendType, Cli},
    init_logging,
};

#[cfg(feature = "luwen-backend")]
use tt_toplike::backend::luwen::LuwenBackend;

fn main() {
    // Parse command-line arguments.
    //
    // `mut` only in the `remote` build: the `--remote` arm rewrites `cli.remote`
    // to the resolved HOST:PORT so the TUI's factory pass doesn't re-run
    // discovery (see the BackendType::Remote arm). Split by cfg so the non-remote
    // build doesn't trip the `unused_mut` lint under `-D warnings`.
    #[cfg(feature = "remote")]
    let mut cli = Cli::parse_args();
    #[cfg(not(feature = "remote"))]
    let cli = Cli::parse_args();

    // Validate arguments
    if let Err(e) = cli.validate() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    // Initialize logging with appropriate level
    init_logging(cli.log_level());

    // Print startup banner (suppressed under --bench to keep output machine-parseable)
    if !cli.bench {
        println!("🦀 tt-toplike v{}", env!("CARGO_PKG_VERSION"));
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Backend: {}", cli.backend_name());
        println!("Update Interval: {}ms", cli.interval);
        if let Some(ref devices) = cli.devices {
            println!("Device Filter: {:?}", devices);
        }
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!();
    }

    // Create backend configuration. `--timeout` (milliseconds) drives the read
    // timeout, which the WsBackend also uses for its connect/first-frame wait —
    // without this it was stuck at the 5000ms default.
    let config = BackendConfig::default()
        .with_interval(cli.interval)
        .with_max_errors(cli.max_errors)
        .with_read_timeout_ms(cli.timeout);

    // --bench: headless render benchmark — does NOT require a TTY. Dispatch here,
    // before the backend selection / TUI path, so it works in CI and scripts.
    if cli.bench {
        let backend_type = cli.effective_backend();
        match tt_toplike::backend::factory::create_backend(backend_type, config, &cli) {
            Ok(mut backend) => {
                // A failed first sample means the bench renders against no data,
                // which is a broken run — fail loudly so CI/scripts catch it
                // rather than reporting a green benchmark over empty telemetry.
                if let Err(e) = backend.update() {
                    eprintln!("bench: backend update failed: {e}");
                    std::process::exit(1);
                }
                let results = tt_toplike::ui::run_render_bench(backend.as_ref(), 120, 120, 40);
                print!("{}", tt_toplike::ui::tui::bench::format_table(&results));
            }
            Err(e) => {
                // Exit non-zero so `--bench` is usable as a CI/scripting gate —
                // a failed backend init should fail the job, not pass silently.
                eprintln!("bench: backend init failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `--serve` without a TTY (piped output, backgrounded, run under a process
    // supervisor, etc.): a headless telemetry publisher, no TUI at all. This is
    // the "run as a service" path — contrast with `--serve` at a real TTY,
    // which starts the TUI AND a publisher together (a later task, wired inside
    // `run_tui`). Dispatch here, before backend selection, since the headless
    // path builds and drives its own backend rather than handing off to
    // `run_with_backend`/`run_tui`.
    #[cfg(feature = "remote")]
    if cli.serve.is_some() && !std::io::stdout().is_terminal() {
        run_headless_serve(&cli, config.clone());
        return;
    }
    #[cfg(not(feature = "remote"))]
    if cli.serve.is_some() {
        eprintln!("Error: --serve not available (build with --features remote)");
        std::process::exit(1);
    }

    // Select and initialize backend based on CLI arguments
    let backend_type = cli.effective_backend();

    match backend_type {
        BackendType::Mock => {
            log::info!(
                "Initializing MockBackend with {} devices",
                cli.effective_mock_devices()
            );
            let mut backend = MockBackend::with_config(cli.effective_mock_devices(), config);
            run_with_backend(&mut backend, &cli);
        }
        BackendType::Json => {
            log::info!(
                "Initializing JSONBackend with tt-smi path: {:?}",
                cli.tt_smi_path
            );
            let mut backend =
                JSONBackend::with_config(cli.tt_smi_path.to_string_lossy().to_string(), config);
            run_with_backend(&mut backend, &cli);
        }
        BackendType::Auto => {
            // SAFE MODE AUTO-DETECT: Never tries Luwen (invasive, requires PCI access)
            // Order: Sysfs (hwmon) → JSON (tt-smi) → Mock
            // Use --backend luwen explicitly if you need direct hardware access
            log::info!("Auto-detecting backend (safe mode - skipping Luwen)...");

            // Try Sysfs backend first (Linux hwmon sensors - SAFEST, non-invasive)
            #[cfg(target_os = "linux")]
            {
                println!("🔍 Trying Sysfs backend (hwmon sensors - safest, non-invasive)...");
                let mut sysfs_backend =
                    tt_toplike::backend::sysfs::SysfsBackend::with_config(config.clone());

                match sysfs_backend.init() {
                    Ok(_) => {
                        println!("✓ Sysfs backend initialized successfully");
                        run_with_backend(&mut sysfs_backend, &cli);
                        return;
                    }
                    Err(e) => {
                        log::warn!("Sysfs backend failed: {}", e);
                        println!("⚠ Sysfs backend unavailable, trying JSON backend...");
                    }
                }
            }

            // Try JSON backend as second option (tt-smi subprocess - safe)
            println!("🔍 Trying JSON backend (tt-smi subprocess)...");
            let mut json_backend = JSONBackend::with_config(
                cli.tt_smi_path.to_string_lossy().to_string(),
                config.clone(),
            );

            match json_backend.init() {
                Ok(_) => {
                    println!("✓ JSON backend initialized successfully");
                    run_with_backend(&mut json_backend, &cli);
                    return;
                }
                Err(e) => {
                    log::warn!("JSON backend failed: {}", e);
                    println!("⚠ JSON backend unavailable, falling back to mock...");
                }
            }

            // Last resort: Mock backend (for testing without hardware)
            println!("⚠ No hardware backends available, using mock backend");
            println!(
                "💡 Tip: Use --backend luwen for direct hardware access (requires PCI permissions)"
            );
            let mut mock_backend = MockBackend::with_config(cli.effective_mock_devices(), config);
            run_with_backend(&mut mock_backend, &cli);
        }
        // Sysfs is a Linux-only variant (see cli::BackendType); the arm only exists
        // when compiling for Linux, so no non-Linux fallback arm is required.
        #[cfg(target_os = "linux")]
        BackendType::Sysfs => {
            log::info!("Initializing Sysfs backend");
            let mut backend = tt_toplike::backend::sysfs::SysfsBackend::with_config(config);
            run_with_backend(&mut backend, &cli);
        }
        BackendType::Luwen => {
            #[cfg(feature = "luwen-backend")]
            {
                log::info!("Initializing LuwenBackend (direct hardware access)");
                let mut backend = LuwenBackend::with_config(config);
                run_with_backend(&mut backend, &cli);
            }

            #[cfg(not(feature = "luwen-backend"))]
            {
                eprintln!("Error: Luwen backend not enabled");
                eprintln!("Rebuild with: cargo build --features luwen-backend");
                std::process::exit(1);
            }
        }
        #[cfg(target_os = "linux")]
        BackendType::Hybrid => {
            log::info!("Initializing HybridBackend (sysfs + background tt-smi cache)");
            let mut backend = tt_toplike::backend::hybrid::HybridBackend::with_config(
                cli.tt_smi_path.to_string_lossy().to_string(),
                config,
            );
            run_with_backend(&mut backend, &cli);
        }
        BackendType::Host => {
            log::info!("Initializing HostBackend (CPU/RAM via sysinfo — no TT hardware required)");
            let mut backend = tt_toplike::backend::host::HostBackend::with_config(config);
            run_with_backend(&mut backend, &cli);
        }
        // Remote QuietBox — opt-in only via --remote <HOST:PORT>. Consumes the
        // box's streamed `tt-smi -s` frames over WebSocket exactly like local
        // telemetry. Never entered by auto-detect or Tab-cycling. Gated behind
        // the `remote` feature (in default); when off, the variant still exists
        // but fails cleanly rather than referencing the un-compiled ws module.
        #[cfg(feature = "remote")]
        BackendType::Remote => {
            let spec = cli.remote.clone().unwrap_or_default();
            // Resolve a bare box name (`--remote qb2-lab`) via `tt --json
            // discover`; HOST:PORT passes straight through.
            let resolved = match tt_toplike::backend::discovery::resolve_remote_spec(&spec) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: could not resolve --remote '{}': {}", spec, e);
                    std::process::exit(1);
                }
            };
            // Collapse the double startup discovery: `run_with_backend` → `run_tui`
            // rebuilds the backend via the factory (the accepted, backend-agnostic
            // double-build pattern — see sysfs.rs), and for Remote that factory
            // pass would shell `tt --json discover` a SECOND time. Rewrite
            // `cli.remote` to the already-resolved HOST:PORT so the factory's
            // `resolve_remote_spec` sees an explicit address and passes it through
            // without another mDNS browse.
            cli.remote = Some(resolved.clone());
            log::info!("Initializing WsBackend (remote QuietBox @ {})", resolved);
            // Unlike the local backends, a WsBackend is expensive to duplicate:
            // each one owns a tokio runtime and a live WebSocket subscription to
            // the box. `run_tui` builds its own via the factory, so for the TUI
            // path we go straight there (one connection). Only `--print`, which
            // needs a concrete backend to dump and then exits, builds a
            // standalone one here.
            if cli.print {
                match tt_toplike::backend::ws::WsBackend::from_host_port(&resolved, config) {
                    Ok(mut backend) => run_with_backend(&mut backend, &cli),
                    Err(e) => {
                        eprintln!("Error: invalid --remote target '{}': {}", resolved, e);
                        std::process::exit(1);
                    }
                }
            } else if let Err(e) = tt_toplike::ui::run_tui(&cli) {
                eprintln!("TUI error: {}", e);
                std::process::exit(1);
            }
        }
        #[cfg(not(feature = "remote"))]
        BackendType::Remote => {
            eprintln!("Error: --remote not available (built without the 'remote' feature)");
            eprintln!("Rebuild with: cargo build --features remote");
            std::process::exit(1);
        }
    }
}

/// Run the application with a given backend
///
/// This initializes the backend and launches the TUI (or prints telemetry if --print).
fn run_with_backend<B: TelemetryBackend>(backend: &mut B, cli: &Cli) {
    // Initialize backend
    match backend.init() {
        Ok(_) => {
            log::info!("Backend initialized: {}", backend.backend_info());
            log::info!("Discovered {} devices", backend.device_count());
        }
        Err(e) => {
            eprintln!("✗ Backend initialization failed: {}", e);
            std::process::exit(1);
        }
    }

    // Read initial telemetry
    if let Err(e) = backend.update() {
        eprintln!("✗ Failed to read telemetry: {}", e);
        std::process::exit(1);
    }

    // Print mode - dump telemetry and exit
    if cli.print {
        print_telemetry(backend);
        return;
    }

    // Launch TUI (TUI will create its own backend)
    if let Err(e) = tt_toplike::ui::run_tui(cli) {
        eprintln!("TUI error: {}", e);
        std::process::exit(1);
    }
}

/// Headless `--serve`: no TUI, no terminal — just poll the backend and fan
/// its telemetry out over `/telemetry` until the process is killed.
///
/// This is the "run as a background service" counterpart to the TTY path
/// (which starts the TUI *and* a publisher together — wired inside `run_tui`
/// in a later task). Every panic-risk edge here degrades to a logged warning
/// and "keep looping" rather than crashing the service: a single failed
/// `backend.update()` (e.g. a transient `tt-smi` hiccup) shouldn't take the
/// whole publisher down, since other clients may still be relying on the
/// last-broadcast frame.
///
/// Ctrl-C handling: deliberately a plain loop with no `SIGINT` handler. There
/// is nothing to flush or persist on shutdown — the publisher's `Drop` (never
/// reached on a hard kill) only matters for a graceful in-process shutdown,
/// and the OS reclaiming the socket on process exit is sufficient for this
/// v1. A future revision can add a signal handler if a clean "N clients said
/// goodbye" log on shutdown becomes worth the extra dependency.
#[cfg(feature = "remote")]
fn run_headless_serve(cli: &Cli, config: BackendConfig) {
    // Step 1: resolve the bind address. `cli.serve.is_some()` was already
    // checked by the caller, but `parse_serve_bind` still owns validating the
    // BIND[:PORT]/PORT/HOST shape (see cli::parse_serve_bind's doc comment).
    let spec = cli.serve.as_deref().unwrap_or_default();
    let bind = match tt_toplike::cli::parse_serve_bind(spec) {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("Error: invalid --serve address '{}': {}", spec, e);
            std::process::exit(1);
        }
    };

    // Step 2: build + init the backend exactly like the normal (TUI) path
    // does via the factory, then prime it with one `update()` — mirroring
    // `run_with_backend`'s init-then-update sequence — so a raw-JSON-capable
    // backend (JSON/Hybrid/Ws) actually has a frame cached before the
    // capability check below runs. Without this priming update, `snapshot_json()`
    // would read `None` even for `--backend json` (its raw frame is only
    // cached by `update()`, not `init()`), which would make every headless
    // launch fail the very check meant to distinguish JSON/Hybrid from
    // Mock/Host/Sysfs.
    let backend_type = cli.effective_backend();
    let mut backend = match tt_toplike::backend::factory::create_backend(backend_type, config, cli)
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("✗ Backend initialization failed: {}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = backend.update() {
        eprintln!("✗ Failed to read telemetry: {}", e);
        std::process::exit(1);
    }

    // Step 3: only a backend that can hand back verbatim `tt-smi -s` JSON
    // (JSON, Hybrid, Ws) can be broadcast — the wire format IS that JSON, plus
    // the additive `tt_toplike` extension. Mock/Host/Sysfs never populate
    // `snapshot_json()` (see their `TelemetryBackend` impls / the trait's
    // `None` default), so fail fast with a clear remedy rather than silently
    // publishing nothing useful.
    if backend.snapshot_json().is_none() {
        eprintln!(
            "Error: --serve needs the json or hybrid backend (tt-smi); \
             relaunch with --backend hybrid or --backend json"
        );
        std::process::exit(1);
    }

    // Step 4: bind and start serving `/telemetry`.
    let publisher = match tt_toplike::backend::TelemetryPublisher::spawn(bind) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: failed to start --serve publisher: {}", e);
            std::process::exit(1);
        }
    };
    log::info!(
        "--serve: telemetry publisher listening on ws://{}/telemetry",
        publisher.bind_addr()
    );
    println!(
        "Serving telemetry on ws://{}/telemetry (headless — no TTY detected)",
        publisher.bind_addr()
    );

    // Step 5: local process + inference-server monitors, mirroring how
    // `run_tui` (src/ui/tui/mod.rs) constructs `host_proc_monitor` +
    // `inference_monitor` — these feed the richer `tt_toplike` extension
    // riding along on each broadcast frame. `InferenceServerMonitor` mirrors
    // the TTY path's `#[cfg(target_os = "linux")]` gate (its `DockerProbe`
    // backend targets Linux container hosts); non-Linux headless servers
    // still broadcast processes, just with an always-empty inference list.
    let mut host_proc_monitor = tt_toplike::workload::HostProcessMonitor::new();
    host_proc_monitor.update();
    let liveness_prober = tt_toplike::workload::LivenessProber::spawn();
    liveness_prober.submit(host_proc_monitor.detected_runtimes());
    #[cfg(target_os = "linux")]
    let inference_monitor = tt_toplike::workload::InferenceServerMonitor::spawn();
    #[cfg(target_os = "linux")]
    inference_monitor.submit(host_proc_monitor.detected_inference_servers());

    // Headless has no fixed panel height (no TUI to size rows for), so cap
    // generously rather than reusing the TUI's `PROC_PANEL_MAX_ROWS`.
    const HEADLESS_PROC_ROWS_MAX: usize = 64;
    let mut last_client_log = std::time::Instant::now();

    // Step 6: loop until killed. Each tick: refresh backend + monitors, build
    // the enriched frame, broadcast it, and periodically log the connected
    // client count (useful for confirming a remote peer actually attached
    // without needing a debugger).
    loop {
        if let Err(e) = backend.update() {
            // A transient read failure shouldn't take the publisher down —
            // clients just keep seeing the last-broadcast frame until the
            // next successful update.
            log::warn!("--serve: backend update failed: {}", e);
        }

        host_proc_monitor.update();
        liveness_prober.submit(host_proc_monitor.detected_runtimes());
        #[cfg(target_os = "linux")]
        inference_monitor.submit(host_proc_monitor.detected_inference_servers());

        let proc_rows =
            host_proc_monitor.rows(HEADLESS_PROC_ROWS_MAX, &liveness_prober.fresh_verdicts());
        #[cfg(target_os = "linux")]
        let inference_snapshot = inference_monitor.snapshot();
        #[cfg(not(target_os = "linux"))]
        let inference_snapshot: Vec<tt_toplike::workload::inference_server::ServiceState> =
            Vec::new();

        let ext = tt_toplike::backend::build_extension(&proc_rows, &inference_snapshot);
        let base_json = backend.snapshot_json().unwrap_or_default();
        let frame = tt_toplike::backend::inject_extension(&base_json, &ext);
        publisher.broadcast(frame);

        if last_client_log.elapsed() >= std::time::Duration::from_secs(10) {
            log::info!("--serve: {} client(s) connected", publisher.client_count());
            last_client_log = std::time::Instant::now();
        }

        std::thread::sleep(std::time::Duration::from_millis(cli.interval));
    }
}

/// Print telemetry to stdout (for --print mode)
fn print_telemetry<B: TelemetryBackend>(backend: &B) {
    println!("Backend: {}", backend.backend_info());
    println!("Devices: {}\n", backend.device_count());

    for device in backend.devices() {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!(
            "Device {}: {} ({:?})",
            device.index, device.board_type, device.architecture
        );
        println!("Bus ID: {}", device.bus_id);

        if let Some(telem) = backend.telemetry(device.index) {
            println!("\nCore Telemetry:");
            println!("  Voltage:     {:.3} V", telem.voltage.unwrap_or(0.0));
            println!("  Current:     {:.2} A", telem.current.unwrap_or(0.0));
            println!("  Power:       {:.2} W", telem.power.unwrap_or(0.0));
            println!(
                "  Temperature: {:.1} °C",
                telem.asic_temperature.unwrap_or(0.0)
            );
            println!("  AICLK:       {} MHz", telem.aiclk.unwrap_or(0));
            println!("  Heartbeat:   {}", telem.heartbeat.unwrap_or(0));
        }

        if let Some(smbus) = backend.smbus_telemetry(device.index) {
            println!("\nSMBUS Telemetry:");
            if let Some(ref ddr_status) = smbus.ddr_status {
                println!("  DDR Status:  {}", ddr_status);
            }
            if let Some(ref ddr_speed) = smbus.ddr_speed {
                println!("  DDR Speed:   {} MT/s", ddr_speed);
            }
            if let Some(ref arc0) = smbus.arc0_health {
                println!("  ARC0 Health: {}", arc0);
            }
        }
        println!();
    }
}
