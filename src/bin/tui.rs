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

    // --bus-inspect: print a summary of a recorded signal-bus log and exit.
    // Needs no backend and no TTY, and prints no banner, so the output can be
    // piped. Exit 1 when the log cannot be read or the output fails, other
    // than a closed pipe.
    if let Some(path) = &cli.bus_inspect {
        let filter = tt_toplike_bus::inspect::Filter {
            entity: cli.entity.clone(),
            signal: cli.signal.clone(),
        };
        // A closed pipe on stdout (`| head`) is success, and an error
        // message is written without `eprintln!`, which would panic when
        // stderr is the same closed pipe (`2>&1 | head`).
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        let code = tt_toplike_bus::inspect::inspect_command(
            path,
            &filter,
            &mut out,
            &mut std::io::stderr(),
        );
        // Dropping a BufWriter flushes again and ignores the error, so a
        // second broken pipe here is silent.
        drop(out);
        if code != 0 {
            std::process::exit(code);
        }
        return;
    }

    // The TUI hosts the signal bus itself (`--record`, `--bus-dir`,
    // `--bus-retain`, `--bus-socket`) or attaches to a headless host's
    // (`--bus-attach`, or the start-up probe of the default socket); see
    // `tt_toplike::bus::tui_bus`. `Cli::validate` already refused `--replay`
    // without `--headless`: a replay is served by a headless host and read
    // with `--bus-attach`.

    // Initialize logging with appropriate level
    init_logging(cli.log_level());

    // Print startup banner (suppressed under --bench to keep output
    // machine-parseable, and for a replay, which reads no backend)
    if !cli.bench && cli.replay.is_none() {
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

    #[cfg(not(feature = "remote"))]
    if cli.serve.is_some() {
        eprintln!("Error: --serve not available (build with --features remote)");
        std::process::exit(1);
    }

    // `--headless`, and `--serve` without a TTY (piped output, backgrounded,
    // run under a process supervisor): no TUI at all. Both run the same loop
    // (`tt_toplike::bus::headless`). `--headless` records the bus and serves
    // its socket; the no-TTY `--serve` keeps its old behaviour, a memory-only
    // bus that feeds the `/telemetry` broadcast. Contrast with `--serve` at a
    // real TTY, which starts the TUI AND a publisher together (wired inside
    // `run_tui`). Dispatch here, before backend selection, since the headless
    // path builds and drives its own backend.
    if cli.headless || (cli.serve.is_some() && !std::io::stdout().is_terminal()) {
        run_headless_main(&cli, config.clone());
        return;
    }

    // Select and initialize backend based on CLI arguments
    let backend_type = cli.effective_backend();

    // Interactive TUI is the common path, and `run_tui` creates and owns its
    // backend exactly once (via `factory::create_backend`, which does its own
    // Auto cascade). Building a backend HERE too — as the per-type arms below
    // do — would initialize it two or three times before the first frame, and
    // each Sysfs/Hybrid init shells out to `tt-smi` (~0.4s), so the old flow
    // paid ~1.3s (and 3× the tt-smi load under concurrent instances) before
    // painting. Route straight to `run_tui` and skip the throwaway
    // construction. Two cases still fall through to the match below: `--print`
    // (needs a populated backend in hand to dump) and `Remote` (its arm resolves
    // the `--remote` host spec via `tt --json discover` and rewrites `cli.remote`
    // to the explicit address before `run_tui`, so the factory doesn't
    // re-discover — see that arm).
    if !cli.print && backend_type != BackendType::Remote {
        if let Err(e) = tt_toplike::ui::run_tui(&cli) {
            eprintln!("TUI error: {}", e);
            std::process::exit(1);
        }
        return;
    }

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

/// The headless loop: `--headless`, and `--serve` with no TTY.
///
/// Runs until SIGTERM or SIGINT, then flushes the bus, removes the socket
/// file and exits 0. Every failure before the loop starts (the backend, the
/// `--serve` bind, a log directory or socket another process holds, a socket
/// directory that is not private) prints one line and exits 1. Inside the
/// loop a failed `backend.update()` is logged and the loop goes on, as the
/// old `--serve` loop did: other clients may still rely on the last frame.
fn run_headless_main(cli: &Cli, config: BackendConfig) {
    use tt_toplike::bus::headless::{
        install_stop_signals, run_headless, run_headless_replay, HeadlessConfig, LocalWorkloads,
        NoWorkloads, Workloads,
    };

    #[cfg(unix)]
    let socket = cli.headless.then(|| cli.bus_socket_path());
    #[cfg(not(unix))]
    let socket: Option<std::path::PathBuf> = None;
    // First signal: stop the loop. Second: remove the socket and exit now.
    let stop = match install_stop_signals(socket.clone()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: cannot watch for SIGTERM/SIGINT: {e}");
            std::process::exit(1);
        }
    };
    // `synthetic`: the backend makes its values up (mock). Such a run
    // writes to disk only into a directory named on the command line.
    let bus_for = |synthetic: bool| {
        if cli.headless {
            cli.headless_bus_config(synthetic)
        } else {
            // The no-TTY `--serve` keeps its old footprint: nothing on disk.
            tt_toplike_bus::BusConfig::default()
        }
    };
    let describe = |bus: &tt_toplike_bus::BusConfig| {
        let log = bus
            .dir
            .as_ref()
            .map_or("memory only".to_string(), |d| d.display().to_string());
        let sock = socket
            .as_ref()
            .map_or("none".to_string(), |p| p.display().to_string());
        format!("bus log: {log}; bus socket: {sock}")
    };
    let interval = std::time::Duration::from_millis(cli.interval);

    // --replay: play a recorded log and serve it. No backend.
    if let Some(path) = &cli.replay {
        let bus = bus_for(false);
        println!(
            "Headless replay of {} at speed {} ({}). Stop with SIGTERM or Ctrl-C.",
            path.display(),
            cli.effective_speed(),
            describe(&bus)
        );
        let cfg = HeadlessConfig {
            bus,
            socket,
            interval,
            relay: false,
            gozer: false,
            synthetic: false,
            backend_name: "replay".into(),
        };
        let done = |r: &Result<u64, String>| match r {
            Ok(n) => println!("replay: played {n} records; serving until stopped"),
            Err(e) => eprintln!("replay: stopped early: {e}"),
        };
        match run_headless_replay(path, cli.effective_speed(), cfg, &stop, done) {
            Ok(r) => println!("stopped: bus head {}, flushed: {}", r.head, r.flushed),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Build + init the backend exactly like the TUI path does via the
    // factory, then prime it with one `update()`, so a raw-JSON-capable
    // backend (JSON/Hybrid/Ws) has a frame cached before the `--serve`
    // capability check below runs (`snapshot_json()` is filled by
    // `update()`; `init()` leaves it empty).
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
    let relay = backend_type == BackendType::Remote;
    // Auto-detect can fall back to mock: name what was actually built.
    let backend_name = tt_toplike::cli::backend_tag(backend_type, &backend.backend_info());
    let synthetic = backend_name == "mock";
    let bus = bus_for(synthetic);

    // `--serve`: only a backend that hands back verbatim `tt-smi -s` JSON
    // (JSON, Hybrid, Ws) can be broadcast, because the wire format IS that
    // JSON plus the additive `tt_toplike` extension. Fail fast with a remedy.
    #[cfg(feature = "remote")]
    let publisher = match cli.serve.as_deref() {
        None => None,
        Some(spec) => {
            let bind = match tt_toplike::cli::parse_serve_bind(spec) {
                Ok(addr) => addr,
                Err(e) => {
                    eprintln!("Error: invalid --serve address '{}': {}", spec, e);
                    std::process::exit(1);
                }
            };
            if backend.snapshot_json().is_none() {
                eprintln!(
                    "Error: --serve needs the json or hybrid backend (tt-smi); \
                     relaunch with --backend hybrid or --backend json"
                );
                std::process::exit(1);
            }
            match tt_toplike::backend::TelemetryPublisher::spawn(bind) {
                Ok(p) => {
                    println!(
                        "Serving telemetry on ws://{}/telemetry (headless, no TUI)",
                        p.bind_addr()
                    );
                    Some(p)
                }
                Err(e) => {
                    eprintln!("Error: failed to start --serve publisher: {}", e);
                    std::process::exit(1);
                }
            }
        }
    };

    if cli.headless {
        println!(
            "Headless bus host on {} ({}). Stop with SIGTERM or Ctrl-C.",
            backend.backend_info(),
            describe(&bus)
        );
    }

    // A relay's processes and models come from the remote frame; local
    // monitors would name this box's processes against the remote chips.
    // A mock run scans nothing real unless --mock-workloads asks for it.
    let local =
        tt_toplike::bus::headless::uses_local_workloads(relay, synthetic, cli.mock_workloads);
    let mut workloads: Box<dyn Workloads> = if local {
        Box::new(LocalWorkloads::new())
    } else {
        Box::new(NoWorkloads)
    };
    // gozer describes this box's real chips: not for mock, host or remote.
    let gozer = cli.headless
        && !matches!(
            backend_type,
            BackendType::Mock | BackendType::Host | BackendType::Remote
        );
    let cfg = HeadlessConfig {
        bus,
        socket,
        interval,
        relay,
        gozer,
        synthetic,
        backend_name,
    };

    #[cfg(feature = "remote")]
    let mut last_client_log = std::time::Instant::now();
    #[cfg(feature = "remote")]
    let mut send = |frame: String| {
        if let Some(p) = &publisher {
            p.broadcast(frame);
            // Log the client count every 10 s, to confirm a peer attached.
            if last_client_log.elapsed() >= std::time::Duration::from_secs(10) {
                log::info!("--serve: {} client(s) connected", p.client_count());
                last_client_log = std::time::Instant::now();
            }
        }
    };
    #[cfg(feature = "remote")]
    let broadcast: Option<&mut dyn FnMut(String)> = if cli.serve.is_some() {
        Some(&mut send)
    } else {
        None
    };
    #[cfg(not(feature = "remote"))]
    let broadcast: Option<&mut dyn FnMut(String)> = None;

    match run_headless(backend.as_mut(), workloads.as_mut(), cfg, broadcast, &stop) {
        Ok(r) => {
            if cli.headless {
                println!(
                    "stopped after {} ticks: bus head {}, flushed: {}",
                    r.ticks, r.head, r.flushed
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
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
