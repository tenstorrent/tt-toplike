// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Command-line argument parsing
//!
//! This module handles all CLI argument parsing using clap.
//! It provides a clean interface for configuring the application
//! through command-line flags and options.
//!
//! ## Usage Examples
//!
//! ```bash
//! # Use mock backend for testing
//! tt-toplike --mock
//!
//! # Use JSON backend with custom tt-smi path
//! tt-toplike --json --tt-smi-path /usr/local/bin/tt-smi
//!
//! # Auto-detect backend (tries JSON, falls back to mock)
//! tt-toplike --backend auto
//!
//! # Verbose output with 50ms update interval
//! tt-toplike -v --interval 50
//!
//! # Monitor specific devices only
//! tt-toplike --devices 0,2,4
//! ```

use clap::{Parser, ValueEnum};
use std::path::PathBuf;

/// Real-time hardware monitoring for Tenstorrent silicon
///
/// tt-toplike provides beautiful, hardware-responsive visualizations
/// for Tenstorrent AI accelerators with information density comparable to htop.
#[derive(Parser, Debug)]
#[command(name = "tt-toplike")]
#[command(author = "Tenstorrent")]
#[command(version)]
#[command(about = "Real-time hardware monitoring for Tenstorrent silicon", long_about = None)]
#[command(after_help = "EXAMPLES:
    # Use mock backend for testing
    tt-toplike --mock

    # Use JSON backend with custom tt-smi path
    tt-toplike --json --tt-smi-path /usr/local/bin/tt-smi

    # Launch directly in Arcade mode
    tt-toplike --mode arcade

    # Launch in Memory Castle mode with verbose logging
    tt-toplike --mode castle -v

    # Starfield visualization with fast refresh
    tt-toplike --mode starfield --interval 50

    # Memory Flow topology view
    tt-toplike --mode flow

    # Monitor specific devices only
    tt-toplike --devices 0,2,4

    # Quiet mode (no logs, only TUI)
    tt-toplike -q
")]
pub struct Cli {
    /// Backend selection
    #[arg(short, long, value_enum, default_value = "auto")]
    pub backend: BackendType,

    /// Use mock backend with an optional chip count (shortcut for --backend mock)
    ///
    /// `--mock` alone uses the --mock-devices count (default 3).
    /// `--mock N` is shorthand for `--mock --mock-devices N` — e.g. `--mock 32`.
    #[arg(long, conflicts_with = "json", num_args(0..=1), default_missing_value = "0")]
    pub mock: Option<usize>,

    /// Use JSON backend (shortcut for --backend json)
    #[arg(long, conflicts_with = "mock")]
    pub json: bool,

    /// Use Host backend — any machine, no TT hardware required (shortcut for --backend host)
    ///
    /// Reads CPU, RAM, and temperature from the host OS.  All visualisations
    /// work unchanged and describe your CPU instead of a TT accelerator.
    #[arg(long, conflicts_with = "mock", conflicts_with = "json")]
    pub host: bool,

    /// Connect to a remote Tenstorrent box over WebSocket (remote QuietBox).
    ///
    /// Reads telemetry from `ws://<HOST:PORT>/telemetry`, where a
    /// tt-station-agentd publisher pushes frames that are the verbatim stdout of
    /// `tt-smi -s` — consumed exactly like local telemetry. Opt-in only: this
    /// backend is never entered by auto-detect or Tab-cycling.
    ///
    /// Accepts `HOST:PORT` (e.g. `192.168.1.42:8765`) or a bare `HOST`
    /// (defaults to port 8000, the agentd control port). IPv6: `[::1]:8765`.
    ///
    /// Transport is plaintext `ws://` with no authentication — frames are
    /// trusted verbatim. Use only on a network you trust (a lab LAN), not across
    /// the public internet.
    #[arg(
        long,
        value_name = "HOST:PORT",
        conflicts_with = "mock",
        conflicts_with = "json",
        conflicts_with = "host"
    )]
    pub remote: Option<String>,

    /// Publish local telemetry over WebSocket for other tt-toplike instances to watch.
    ///
    /// `--serve` alone binds all interfaces on the default port
    /// (`0.0.0.0:8770`). `--serve BIND:PORT` binds a specific address/port,
    /// `--serve PORT` binds all interfaces on that port, and `--serve HOST`
    /// binds the default port on that host. IPv6: `[::1]:8770`.
    ///
    /// A box can `--serve` while also watching a `--remote` peer — that's the
    /// relay case, so the two flags do not conflict.
    #[arg(long, value_name = "BIND:PORT", num_args = 0..=1, default_missing_value = "0.0.0.0:8770")]
    pub serve: Option<String>,

    /// Path to tt-smi executable
    ///
    /// Only used with JSON backend. Defaults to "tt-smi" in PATH.
    #[arg(long, default_value = "tt-smi")]
    pub tt_smi_path: PathBuf,

    /// Update interval in milliseconds
    ///
    /// How frequently to poll telemetry data. Lower values provide
    /// smoother animations but increase CPU usage.
    /// Range: 10-1000ms
    #[arg(short, long, default_value = "100")]
    pub interval: u64,

    /// Device indices to monitor (comma-separated)
    ///
    /// If not specified, all devices are monitored.
    /// Example: --devices 0,2,4
    #[arg(short, long, value_delimiter = ',')]
    pub devices: Option<Vec<usize>>,

    /// Verbose logging
    ///
    /// Show detailed backend logs. Useful for debugging.
    #[arg(short, long, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Quiet mode (suppress all logs)
    ///
    /// Only show the TUI interface, no log output.
    #[arg(short, long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Number of mock devices (only for mock backend)
    ///
    /// How many virtual devices to create when using mock backend.
    #[arg(long, default_value = "3")]
    pub mock_devices: usize,

    /// Maximum consecutive errors before giving up
    ///
    /// Backend will attempt this many retries before failing.
    #[arg(long, default_value = "10")]
    pub max_errors: usize,

    /// Telemetry read timeout in milliseconds
    ///
    /// How long to wait for telemetry before timing out.
    #[arg(long, default_value = "5000")]
    pub timeout: u64,

    // Help text built with `concat!` so it carries the view's current display
    // name and alias (see `crate::die3d_display_name!`).
    #[arg(
        short = 'm',
        long,
        value_enum,
        help = "Launch directly into specific visualization mode",
        long_help = concat!(
            "Launch directly into specific visualization mode\n\n",
            "Skip the normal table view and start in a specific mode. ",
            "Available modes: normal, starfield, castle, flow, arcade, hivemind, ",
            "training, die3d (or ",
            crate::die3d_mode_alias!(),
            ", the ",
            crate::die3d_display_name!(),
            "), saga (a hero's quest through a dungeon built from the machine's ",
            "signals, with a chronicle that names each line's cause; also `S` in-app)"
        )
    )]
    pub mode: Option<VisualizationMode>,

    // Help text built with `concat!` so it carries the view's display name.
    #[arg(
        long,
        help = "Rotate through every view unattended (kiosk / wall-display mode)",
        long_help = concat!(
            "Rotate through every view unattended (kiosk / wall-display mode)\n\n",
            "Cycles every view except Grid and Saga, including the ones reachable ",
            "only by their own key (`i`, `~`, `t`, `3`), 30s on each and 45s on arcade. ",
            "Training and the inference monitor are visited only while a training ",
            "run exists or a model server is starting or running. The ",
            crate::die3d_display_name!(),
            " (`3`) is visited only on a box with a Blackhole chip. Toggle in-app ",
            "with `R`. Combines with `--mode` to choose where the rotation starts."
        )
    )]
    pub rotate: bool,

    /// Animation sensitivity preset
    ///
    /// Controls how responsive visualizations are to hardware activity.
    /// anomalies-only = nearly still except spikes; paranoid = every fluctuation visible.
    /// User can further tune thresholds in ~/.config/tt-toplike/config.toml.
    #[arg(long, value_enum, default_value = "normal")]
    pub profile: crate::config::AnimationProfile,

    /// Launch directly into visualization mode
    ///
    /// Skip the main monitor and show hardware-responsive animations.
    #[arg(long)]
    pub visualize: bool,

    /// Launch directly into workload detection mode
    ///
    /// Show ML framework and process detection interface.
    #[arg(long)]
    pub workload: bool,

    /// Print telemetry to stdout and exit (no TUI)
    ///
    /// Useful for debugging or piping to other tools.
    #[arg(long)]
    pub print: bool,

    /// Headless render benchmark: render each screen into an off-screen buffer
    /// for a fixed number of frames and print per-screen timing + estimated CPU.
    /// Does not enter the TUI. Respects --mock / --host for the backend.
    #[arg(long)]
    pub bench: bool,

    /// Check the config file and exit: print every problem with its line, and
    /// exit non-zero when there is one.
    ///
    /// Reads `~/.config/tt-toplike/config.toml`, the only config file. The
    /// `[arcade]` section sets the arcade's own layouts and the layout to start
    /// with (docs/arcade-layout.md); the report lists every arcade layout with
    /// its panels. The `[backdrop]` section sets the backdrop of the Inference
    /// view, the Training view and the Wafer Waffler: off, scene, sky or
    /// clouds (docs/backdrop.md); other views carry none. In the TUI, `w`
    /// cycles the backdrop of those views and `/backdrop` shows or sets it; in
    /// the arcade, `}` and `{` step through the layouts. Does not enter the TUI
    /// and opens no device.
    #[arg(long)]
    pub check_config: bool,

    /// Start the arcade with the layout NAME.
    ///
    /// A built-in layout (classic, now-serving, defrag-wide, waffler,
    /// workload, deck, skyline, memory-lane) or one from the config file (`config` for its
    /// `[[arcade.row]]` tables, or the name of an `[[arcade.layouts]]`
    /// table). Wins over `[arcade] layout` in the config. An unknown name is
    /// reported on the status line and the arcade starts with the first
    /// layout. In the arcade, `}` and `{` step to the next and previous
    /// layout, and `/arcade layout next|prev|<name>|list` does the same from
    /// the command bar. See docs/arcade-layout.md.
    #[arg(long, value_name = "NAME")]
    pub arcade_layout: Option<String>,

    /// Throttle animation FPS when the host is under heavy load (opt-in).
    ///
    /// When set, animated modes drop from 60 to 30 FPS while system CPU is high
    /// (≥85%, restored below 80%), yielding CPU to the monitored workload.
    /// Toggle live with the `/throttle` command. Off by default.
    #[arg(long)]
    pub throttle: bool,

    /// Drop animation to ~2 FPS while the terminal is unfocused (opt-in).
    ///
    /// Toggle live with the `/idle-on-blur` command. Off by default.
    #[arg(long)]
    pub idle_on_blur: bool,

    /// What to do when a `tt-smi -r` reset is detected on this box
    /// (default: inform).
    ///
    /// Can also be set with `tt_smi_reset_behavior` in
    /// ~/.config/tt-toplike/config.toml. The flag wins over the file.
    #[arg(long, value_enum, value_name = "BEHAVIOR")]
    pub tt_smi_reset_behavior: Option<ResetBehavior>,

    /// Record everything the signal bus sees into segment files in DIR.
    ///
    /// The log is a directory of `bus-<n>.jsonl[.gz]` files, kept for 24 hours
    /// or 5 GB (see `--bus-retain`). Read it back with `--replay DIR` or
    /// `--bus-inspect DIR`. The same as `--bus-dir DIR`. Works in the TUI
    /// and with `--headless`. Without it the TUI keeps its bus in memory.
    #[arg(long, value_name = "DIR")]
    pub record: Option<PathBuf>,

    /// Play a recorded log into a headless bus host. No hardware is read.
    ///
    /// PATH is a segment file (plain or `.gz`) or a directory of segments.
    /// The records are republished on the bus with their original spacing
    /// divided by `--speed`, and the bus is marked as a replay. Needs
    /// `--headless` in this release: run
    /// `tt-toplike-tui --headless --replay PATH --bus-socket SOCKET` and read
    /// it in a TUI with `--bus-attach SOCKET`. Cannot be combined with a backend or live flag
    /// (`--mock`, `--json`, `--host`, `--backend`, `--serve`, `--remote`).
    #[arg(long, value_name = "PATH")]
    pub replay: Option<PathBuf>,

    /// Replay speed: 1 is real time, 4 is four times faster, 0.5 is half speed.
    /// Only valid with `--replay`. Must be a number above zero.
    #[arg(long, value_name = "N", requires = "replay")]
    pub speed: Option<f64>,

    /// Print a summary of a recorded log and exit (no TUI).
    ///
    /// Shows the header, record counts by kind, the time span, the entities,
    /// the events and each signal's sample count, min, max and last value.
    /// PATH is a segment file (plain or `.gz`) or a directory of segments.
    /// Narrow the output with `--entity` and `--signal`.
    ///
    /// Holes in the log (a deleted middle segment, an unreadable segment,
    /// corrupt lines) are listed under `holes:`. A deleted LAST segment
    /// cannot be detected; only an unreadable last segment is reported.
    #[arg(long, value_name = "PATH")]
    pub bus_inspect: Option<PathBuf>,

    /// Run with no screen: host the signal bus, record it, and serve it on a
    /// Unix socket, until SIGTERM or SIGINT.
    ///
    /// The log goes to `--bus-dir` (default `~/.local/state/tt-toplike/bus`,
    /// or `$XDG_STATE_HOME/tt-toplike/bus`). The socket is `--bus-socket`
    /// (default `$XDG_RUNTIME_DIR/tt-toplike/bus.sock`, else
    /// `/tmp/tt-toplike-<uid>/bus.sock`). With `--serve`, also broadcasts
    /// `/telemetry` frames as the no-TTY `--serve` mode does. With `--replay`,
    /// plays a recorded log and reads no backend. For running under
    /// systemd.
    #[arg(long, conflicts_with_all = ["print", "bench", "bus_inspect"])]
    pub headless: bool,

    /// Directory for the signal-bus log. Default with `--headless`:
    /// `~/.local/state/tt-toplike/bus`. The TUI keeps its bus in memory
    /// unless this (or `--record`) is given. One process at a time may write
    /// a directory; a second is refused.
    #[arg(long, value_name = "DIR")]
    pub bus_dir: Option<PathBuf>,

    /// Path of the signal-bus Unix socket. Default:
    /// `$XDG_RUNTIME_DIR/tt-toplike/bus.sock`, else
    /// `/tmp/tt-toplike-<uid>/bus.sock`. Its directory must belong to you
    /// and not be writable by group or others, so a path directly in `/tmp`
    /// is refused.
    ///
    /// `--headless` always serves the socket. The TUI serves it only when
    /// this flag is given, and then hosts its own bus.
    #[arg(long, value_name = "PATH")]
    pub bus_socket: Option<PathBuf>,

    /// TUI: do not look for a headless bus host at start-up.
    ///
    /// By default the TUI tries the default socket path once, with a short
    /// timeout. When a headless host (`--headless`) answers there, the TUI
    /// reads that host's bus and does not host its own; chip telemetry
    /// still comes from the TUI's own backend. The probe is skipped with
    /// `--bus-socket`, `--record`/`--bus-dir`, `--mock` and `--remote`.
    #[arg(long, conflicts_with_all = ["headless", "bus_attach"])]
    pub no_bus_attach: bool,

    /// TUI: read the bus of the headless host serving the socket at PATH,
    /// and fail at start-up if it does not answer.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["headless", "bus_socket", "record", "bus_dir", "bus_retain"]
    )]
    pub bus_attach: Option<PathBuf>,

    /// How long to keep the signal-bus log: HOURS, or HOURS,GB for a size
    /// limit too (whichever is reached first). Default 24,5.
    #[arg(long, value_name = "HOURS[,GB]", value_parser = parse_bus_retain)]
    pub bus_retain: Option<BusRetain>,

    /// With `--headless --mock`: also scan this box's real processes and
    /// model servers and record them against the mock chips.
    ///
    /// Off by default, so a mock run records only simulated data. A mock
    /// run records nothing to disk unless `--bus-dir` (or `--record`) is
    /// given, and its log says `backend mock, mode mock`, with the
    /// simulated readings marked `Estimated`.
    #[arg(long, requires = "mock")]
    pub mock_workloads: bool,

    /// With `--bus-inspect`: print the Saga digest of the log in place of
    /// the summary, then exit.
    ///
    /// Feeds every record to a fresh saga (replay label, real names, the
    /// default 20-minute season) and prints `saga digest: 0x<16 hex digits>`
    /// with the worlds born and beats written. The same records in the same
    /// order give the same digest, whatever their sequence numbers and
    /// times, so a log and a replay of it print the same line. See
    /// docs/saga.md.
    #[arg(
        long,
        requires = "bus_inspect",
        conflicts_with_all = ["entity", "signal"]
    )]
    pub saga_digest: bool,

    /// With `--bus-inspect`: show only this entity, for example `chip:0000:01:00.0`.
    #[arg(long, value_name = "KEY", requires = "bus_inspect")]
    pub entity: Option<String>,

    /// With `--bus-inspect`: show only this signal, for example `tt_asic_temperature_celsius`.
    #[arg(long, value_name = "NAME", requires = "bus_inspect")]
    pub signal: Option<String>,
}

/// `--bus-retain HOURS[,GB]`: the log's age limit and optional size limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BusRetain {
    /// Closed segments older than this are deleted.
    pub secs: u64,
    /// While the log is larger than this, the oldest segment is deleted.
    /// `None` keeps the default (5 GB).
    pub bytes: Option<u64>,
}

/// Parse `HOURS` or `HOURS,GB`. Both are numbers above zero; fractions are
/// allowed (`0.5` hours is 30 minutes). A GB is 10^9 bytes.
pub fn parse_bus_retain(s: &str) -> Result<BusRetain, String> {
    let (h, gb) = match s.split_once(',') {
        Some((h, gb)) => (h, Some(gb)),
        None => (s, None),
    };
    let num = |what: &str, v: &str| -> Result<f64, String> {
        match v.trim().parse::<f64>() {
            Ok(x) if x.is_finite() && x > 0.0 => Ok(x),
            _ => Err(format!(
                "--bus-retain wants HOURS or HOURS,GB with numbers above zero; \
                 the {what} part was '{v}'"
            )),
        }
    };
    let hours = num("HOURS", h)?;
    let bytes = gb.map(|g| num("GB", g)).transpose()?;
    Ok(BusRetain {
        secs: (hours * 3600.0).round().max(1.0) as u64,
        bytes: bytes.map(|g| (g * 1e9).round().max(1.0) as u64),
    })
}

/// Where a headless host keeps its log when `--bus-dir` is not given:
/// `$XDG_STATE_HOME/tt-toplike/bus`, else `~/.local/state/tt-toplike/bus`.
/// `None` with neither a usable `XDG_STATE_HOME` nor a home directory.
pub fn default_bus_dir() -> Option<PathBuf> {
    resolve_bus_dir(std::env::var_os("XDG_STATE_HOME"), dirs::home_dir())
}

/// [`default_bus_dir`] with its inputs passed in. An `XDG_STATE_HOME` that
/// is empty or relative is ignored, as the XDG spec says.
pub fn resolve_bus_dir(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let base = match xdg_state_home.map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => home?.join(".local").join("state"),
    };
    Some(base.join("tt-toplike").join("bus"))
}

/// The bus socket path when `--bus-socket` is not given:
/// `$XDG_RUNTIME_DIR/tt-toplike/bus.sock`, else
/// `/tmp/tt-toplike-<uid>/bus.sock`. Both are in a directory of their own,
/// which the socket server creates with mode 0700: the server refuses a
/// directory others can write to, so `/tmp` itself cannot hold it.
#[cfg(unix)]
pub fn default_bus_socket() -> PathBuf {
    // SAFETY: `getuid` takes no arguments, cannot fail and touches no
    // memory. The same call is made in workload::process_monitor.
    let uid = unsafe { libc::getuid() };
    resolve_bus_socket(std::env::var_os("XDG_RUNTIME_DIR"), uid)
}

/// [`default_bus_socket`] with its inputs passed in. An `XDG_RUNTIME_DIR`
/// that is empty or relative is ignored.
pub fn resolve_bus_socket(xdg_runtime_dir: Option<std::ffi::OsString>, uid: u32) -> PathBuf {
    match xdg_runtime_dir.map(PathBuf::from) {
        Some(p) if p.is_absolute() => p.join("tt-toplike").join("bus.sock"),
        _ => PathBuf::from(format!("/tmp/tt-toplike-{uid}")).join("bus.sock"),
    }
}

/// What toplike does when it sees a `tt-smi -r` reset (`--tt-smi-reset-behavior`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ResetBehavior {
    /// Never look for resets.
    Ignore,
    /// Show a `tt-smi -r` segment in the status bar of every view while a
    /// reset runs and for 10 seconds after it ends. This is the default.
    Inform,
    /// Inform, plus a takeover animation for each reset, drawn in a
    /// centered box over a tinted screen. HivemindSweeper gets a feed event
    /// in its place.
    Dazzle,
    /// Play all five reset animations in a row (8 seconds each) at start
    /// and again on each real reset, with DEMO in the box title. Any key
    /// skips to the next animation. Esc, q or Q ends the demo. A real reset
    /// also gets the status segment, and a feed event in HivemindSweeper.
    Demo,
}

impl ResetBehavior {
    /// True when the reset detector should run (every value but `Ignore`).
    pub fn detects(self) -> bool {
        !matches!(self, ResetBehavior::Ignore)
    }

    /// True when a detected reset starts a takeover animation.
    pub fn animates(self) -> bool {
        matches!(self, ResetBehavior::Dazzle | ResetBehavior::Demo)
    }

    /// True for the demo behavior (the `Takeover::Demo` sequencer).
    pub fn is_demo(self) -> bool {
        matches!(self, ResetBehavior::Demo)
    }
}

/// Picks the effective behavior: the CLI flag first, then the config-file
/// string, then `inform`. An unknown config string logs a warning (inside
/// [`crate::config::parse_reset_behavior`]) and counts as absent.
pub fn resolve_reset_behavior(cli: Option<ResetBehavior>, config: Option<&str>) -> ResetBehavior {
    cli.or_else(|| config.and_then(crate::config::parse_reset_behavior))
        .unwrap_or(ResetBehavior::Inform)
}

/// Backend selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendType {
    /// Automatically detect best backend (SAFE MODE: Sysfs → JSON → Mock)
    /// Note: Auto-detect NEVER tries Luwen (Luwen/UMD arbitration unresolved upstream).
    /// Use --backend luwen explicitly; the live `b` cycle also skips it.
    Auto,

    /// Use mock backend (no hardware required)
    Mock,

    /// Use JSON backend (tt-smi subprocess)
    Json,

    /// Use Luwen backend (direct hardware access)
    #[value(alias = "luwen")]
    Luwen,

    /// Use Sysfs backend (Linux hwmon sensors, non-invasive)
    #[cfg(target_os = "linux")]
    Sysfs,

    /// Use Hybrid backend (sysfs real-time + background JSON enrichment from tt-smi)
    ///
    /// Best default for Linux systems: sysfs provides fast non-invasive core metrics
    /// (temp, power, voltage) while tt-smi is polled every 1.5s in the background for
    /// richer SMBUS data (DDR status, ARC health, board IDs).
    /// Falls back to sysfs-only if tt-smi is unavailable.
    #[cfg(target_os = "linux")]
    Hybrid,

    /// Use Host backend (any machine, no TT hardware required)
    ///
    /// Reads CPU, RAM, and temperature from the host OS via sysinfo + Linux sysfs.
    /// All visualisations work — they describe your CPU instead of a TT accelerator.
    /// Great for demos, screenshots, or exploring tt-toplike before you have hardware.
    Host,

    /// Use Remote WebSocket backend (remote QuietBox telemetry over the LAN)
    ///
    /// Connects to `ws://<host:port>/telemetry` and consumes the streamed
    /// `tt-smi -s` frames as if they were local telemetry. Selected only via the
    /// explicit `--remote <HOST:PORT>` flag; never auto-detected or Tab-cycled.
    Remote,
}

/// True when the active backend represents real *local* TT hardware, so the
/// process panel should list only TT-attributed processes. Host shows host CPU
/// procs; Mock is exempt (no real /dev/tenstorrent fds — keep --mock demos
/// non-blank); Remote is exempt too — the process list is read from the LOCAL
/// machine and cannot correspond to the remote box's chips, so filtering it to
/// "TT processes" would wrongly blank out the local processes we can actually
/// see. Remote therefore shows unfiltered local processes, like Host/Mock.
pub fn backend_shows_only_tt(bt: BackendType) -> bool {
    !matches!(
        bt,
        BackendType::Host | BackendType::Mock | BackendType::Remote
    )
}

/// Default port for `--serve` / `/serve` telemetry publishing.
///
/// Distinct from `--remote`'s default of 8000 (the tt-station-agentd control
/// port) so a `--serve`d tt-toplike and a `tt-station-agentd` can coexist on
/// the same box without a port clash.
pub const DEFAULT_SERVE_PORT: u16 = 8770;

/// Records the TUI's own bus keeps in memory when it serves no socket (see
/// `Cli::tui_bus_config`).
pub const TUI_BUS_WINDOW: usize = 4_096;

/// The TUI's disk writer queue when it serves no socket. Smaller than
/// [`TUI_BUS_WINDOW`], as the bus requires.
pub const TUI_BUS_QUEUE: usize = 2_048;

/// Parse a `--serve`/`/serve` bind-address spec into a `SocketAddr`.
///
/// Accepts, in order of preference:
/// - `BIND:PORT` (e.g. `192.168.1.5:9000`) — used as-is.
/// - `[IPV6]:PORT` (e.g. `[::1]:8770`) — bracketed IPv6 with an explicit port.
/// - `PORT` alone (e.g. `8770`) — binds all interfaces (`0.0.0.0`) on that port.
/// - `BIND` alone (e.g. `192.168.1.5` or a hostname) — binds
///   [`DEFAULT_SERVE_PORT`] on that address.
///
/// Resolution is delegated to [`std::net::ToSocketAddrs`], so bare IP
/// literals resolve synchronously (no DNS) while hostnames go through normal
/// system resolution; the first resolved address is returned. Never panics —
/// garbage input (empty string, non-numeric port, unresolvable host) yields
/// `Err` with a human-readable message.
pub fn parse_serve_bind(spec: &str) -> Result<std::net::SocketAddr, String> {
    use std::net::ToSocketAddrs;

    let spec = spec.trim();
    if spec.is_empty() {
        return Err("--serve requires a BIND[:PORT] (or a bare PORT)".to_string());
    }

    // Bracketed IPv6: [addr] or [addr]:port
    let host_port = if let Some(rest) = spec.strip_prefix('[') {
        let (addr, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("malformed IPv6 bind address: {spec}"))?;
        match tail.strip_prefix(':') {
            Some(port) => format!("[{addr}]:{port}"),
            None if tail.is_empty() => format!("[{addr}]:{DEFAULT_SERVE_PORT}"),
            None => return Err(format!("malformed bind address: {spec}")),
        }
    } else if spec.parse::<u16>().is_ok() {
        // Bare PORT: bind all interfaces.
        format!("0.0.0.0:{spec}")
    } else {
        match spec.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() => {
                // Validate the port before handing off to resolution, so a
                // typo like "nonsense:x" fails fast with a clear message
                // instead of triggering a (possibly slow) DNS lookup on the
                // bogus "host" half.
                port.parse::<u16>()
                    .map_err(|_| format!("invalid port in bind address: {spec}"))?;
                spec.to_string()
            }
            // Bare HOST (no colon): default the port.
            _ => format!("{spec}:{DEFAULT_SERVE_PORT}"),
        }
    };

    host_port
        .to_socket_addrs()
        .map_err(|e| format!("failed to resolve bind address '{spec}': {e}"))?
        .next()
        .ok_or_else(|| format!("no address resolved for bind address '{spec}'"))
}

/// Visualization mode selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VisualizationMode {
    /// Normal table view with telemetry
    Normal,

    /// Hardware-responsive starfield (Tensix cores as stars)
    Starfield,

    /// Memory Castle (DDR → L2 → L1 → Tensix hierarchy)
    #[value(alias = "memory")]
    Castle,

    /// Memory Flow Topology (full-screen DRAM motion)
    #[value(alias = "topology")]
    Flow,

    /// Arcade mode (unified visualization with hero character)
    Arcade,

    /// HivemindSweeper — opt-in passive activity sniffer (source × device
    /// board + coalesced event feed). Also reachable in-app with `~`.
    #[value(alias = "hive", alias = "sniff", alias = "hivemindsweeper")]
    Hivemind,

    /// Training — attaches to a live tt-train run (loss mountains, network
    /// sweeps, checkpoint comets). Also reachable in-app with `t`.
    ///
    /// Takes no target: the view finds the run itself, so this is purely a
    /// "start here" alias for scripted/kiosk launches.
    #[value(alias = "train", alias = "tt-train")]
    Training,

    // The display name and alias come from `crate::die3d_display_name!` and
    // `crate::die3d_mode_alias!`, so a rename reaches `--help`.
    #[value(
        alias = crate::die3d_mode_alias!(),
        help = concat!(
            crate::die3d_display_name!(),
            " (or ",
            crate::die3d_mode_alias!(),
            "): each Blackhole board in isometric 3D from live telemetry. ",
            "Also reachable in-app with `3`."
        )
    )]
    Die3d,

    /// Saga: a hero's quest through a dungeon built from the machine's own
    /// signals, with a chronicle of what happens. Also reachable in-app with
    /// `S`. Needs `[saga] enabled` (on by default).
    Saga,
}

impl Cli {
    /// Parse command-line arguments
    ///
    /// This is the main entry point for CLI parsing.
    /// Returns a configured Cli struct or exits on error.
    ///
    /// # Example
    ///
    /// ```rust
    /// let cli = Cli::parse();
    /// println!("Using backend: {:?}", cli.backend);
    /// ```
    pub fn parse_args() -> Self {
        let mut cli = Self::parse();

        // Handle shortcut flags
        if cli.mock.is_some() {
            cli.backend = BackendType::Mock;
        } else if cli.json {
            cli.backend = BackendType::Json;
        } else if cli.host {
            cli.backend = BackendType::Host;
        } else if cli.remote.is_some() {
            cli.backend = BackendType::Remote;
        }

        cli
    }

    /// Get the effective backend type after resolving shortcuts and auto-detection
    ///
    /// This resolves the --mock and --json shortcut flags into the actual backend type.
    pub fn effective_backend(&self) -> BackendType {
        if self.mock.is_some() {
            BackendType::Mock
        } else if self.json {
            BackendType::Json
        } else if self.host {
            BackendType::Host
        } else if self.remote.is_some() {
            BackendType::Remote
        } else {
            self.backend
        }
    }

    /// Get the number of mock devices to create.
    ///
    /// `--mock N` takes precedence over `--mock-devices N`.
    /// `--mock` alone (or not specified) falls back to `--mock-devices` (default 3).
    pub fn effective_mock_devices(&self) -> usize {
        match self.mock {
            Some(n) if n > 0 => n,  // --mock 32  → use the inline count
            _ => self.mock_devices, // --mock / --mock-devices / default
        }
    }

    /// Get log level filter based on verbose/quiet flags
    ///
    /// Returns appropriate log::LevelFilter for env_logger.
    pub fn log_level(&self) -> log::LevelFilter {
        if self.quiet {
            log::LevelFilter::Off
        } else if self.verbose {
            log::LevelFilter::Debug
        } else {
            // Quiet by default: only surface problems (e.g. a requested backend's
            // hardware/config being unavailable). Routine startup progress is
            // Info/Debug and hidden unless `-v` is passed. `-q` silences entirely.
            log::LevelFilter::Warn
        }
    }

    /// Check if a specific device should be monitored
    ///
    /// Returns true if the device index is in the filter list,
    /// or if no filter is specified (monitor all devices).
    ///
    /// # Arguments
    ///
    /// * `device_idx` - Device index to check
    ///
    /// # Example
    ///
    /// ```rust
    /// let cli = Cli::parse();
    /// if cli.should_monitor_device(0) {
    ///     println!("Monitoring device 0");
    /// }
    /// ```
    pub fn should_monitor_device(&self, device_idx: usize) -> bool {
        match &self.devices {
            Some(devices) => devices.contains(&device_idx),
            None => true, // No filter = monitor all
        }
    }

    /// Get a human-readable backend name for display
    ///
    /// Returns a string describing the selected backend.
    pub fn backend_name(&self) -> &'static str {
        match self.effective_backend() {
            BackendType::Auto => "Auto-detect",
            BackendType::Mock => "Mock",
            BackendType::Json => "JSON (tt-smi)",
            BackendType::Luwen => "Luwen (Direct HW)",
            #[cfg(target_os = "linux")]
            BackendType::Sysfs => "Sysfs (hwmon sensors)",
            #[cfg(target_os = "linux")]
            BackendType::Hybrid => "Hybrid (sysfs + tt-smi cache)",
            BackendType::Host => "Host (CPU/RAM via sysinfo)",
            BackendType::Remote => "Remote (WebSocket QuietBox)",
        }
    }

    /// Minimal defaults for headless rendering (bench / tests). Not for normal use.
    ///
    /// Constructs a `Cli` with safe quiet-mock defaults so callers like
    /// `run_render_bench` can invoke render functions that need a `&Cli` reference
    /// without having to parse actual command-line arguments.
    pub fn bench_default() -> Self {
        Cli {
            backend: BackendType::Mock,
            mock: Some(0),
            json: false,
            host: false,
            remote: None,
            serve: None,
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
            check_config: false,
            arcade_layout: None,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        }
    }

    /// Validate CLI arguments
    ///
    /// Checks for invalid combinations and returns error messages if found.
    /// This is called after parsing to catch semantic errors.
    ///
    /// # Returns
    ///
    /// Ok(()) if valid, Err(message) if invalid.
    pub fn validate(&self) -> Result<(), String> {
        // Check if luwen backend is enabled (at compile time)
        #[cfg(not(feature = "luwen-backend"))]
        if self.effective_backend() == BackendType::Luwen {
            return Err(
                "Luwen backend not enabled. Rebuild with: cargo build --features luwen-backend"
                    .to_string(),
            );
        }

        // Warn if tt-smi-path specified with mock backend
        if self.effective_backend() == BackendType::Mock
            && self.tt_smi_path != PathBuf::from("tt-smi")
        {
            eprintln!("Warning: --tt-smi-path ignored when using mock backend");
        }

        // Warn if mock-devices specified with non-mock backend
        if self.effective_backend() != BackendType::Mock
            && self.mock_devices != 3
            && self.mock.is_none()
        {
            eprintln!("Warning: --mock-devices ignored when not using mock backend");
        }

        self.validate_bus()
    }

    /// The replay speed: `--speed`, or 1 (real time) when it was not given.
    pub fn effective_speed(&self) -> f64 {
        self.speed.unwrap_or(1.0)
    }

    /// The signal-bus settings for a headless host: the log directory
    /// (`--bus-dir`, `--record`, else [`default_bus_dir`]) and the retention
    /// from `--bus-retain`. A replay, and a `synthetic` backend (`--mock`,
    /// or auto-detect falling back to mock), record only into a directory
    /// given on the command line, never the default one, so neither is
    /// mixed into the log of the real chips.
    pub fn headless_bus_config(&self, synthetic: bool) -> tt_toplike_bus::BusConfig {
        let explicit = self.bus_dir.clone().or_else(|| self.record.clone());
        let dir = if self.replay.is_some() || synthetic {
            explicit
        } else {
            explicit.or_else(default_bus_dir)
        };
        let mut cfg = tt_toplike_bus::BusConfig {
            dir,
            ..tt_toplike_bus::BusConfig::default()
        };
        if let Some(r) = self.bus_retain {
            cfg.retain_secs = r.secs;
            if let Some(b) = r.bytes {
                cfg.retain_bytes = b;
            }
        }
        cfg
    }

    /// The bus socket path: `--bus-socket`, else [`default_bus_socket`].
    #[cfg(unix)]
    pub fn bus_socket_path(&self) -> PathBuf {
        self.bus_socket.clone().unwrap_or_else(default_bus_socket)
    }

    /// The signal-bus settings for the TUI's own bus: memory only unless
    /// `--record`/`--bus-dir` names a directory (the TUI never writes to the
    /// default directory, which belongs to a headless host), with the
    /// retention from `--bus-retain`.
    ///
    /// Without `--bus-socket` nothing reads the in-memory record window (the
    /// views read the world the reducer keeps), so it is cut to
    /// [`TUI_BUS_WINDOW`] records, about 1.5 MB in place of about 20 MB for
    /// the default 65,536. The writer queue is cut to [`TUI_BUS_QUEUE`] so
    /// the window stays larger than the queue, which the bus needs (a
    /// window at or below the queue can report spurious `dropped` errors to
    /// a subscriber; see `docs/bus.md`). With `--bus-socket` a client can
    /// subscribe from an older sequence number, so the defaults stay.
    pub fn tui_bus_config(&self) -> tt_toplike_bus::BusConfig {
        let mut cfg = tt_toplike_bus::BusConfig {
            dir: self.bus_dir.clone().or_else(|| self.record.clone()),
            ..tt_toplike_bus::BusConfig::default()
        };
        if self.bus_socket.is_none() {
            cfg.window_records = TUI_BUS_WINDOW;
            cfg.writer_queue = TUI_BUS_QUEUE;
        }
        if let Some(r) = self.bus_retain {
            cfg.retain_secs = r.secs;
            if let Some(b) = r.bytes {
                cfg.retain_bytes = b;
            }
        }
        cfg
    }

    /// Checks for the signal-bus flags (`--record`, `--replay`, `--speed`,
    /// `--bus-inspect`, `--headless`, `--bus-dir`). The messages say which
    /// flags clash and why.
    fn validate_bus(&self) -> Result<(), String> {
        if let Some(speed) = self.speed {
            if !(speed.is_finite() && speed > 0.0) {
                return Err(format!(
                    "--speed must be a number above zero (1 is real time), got {speed}"
                ));
            }
        }
        if self.bus_inspect.is_some() {
            let other = [
                ("--replay", self.replay.is_some()),
                ("--record", self.record.is_some()),
                ("--serve", self.serve.is_some()),
                ("--remote", self.remote.is_some()),
                ("--print", self.print),
                ("--bench", self.bench),
                ("--headless", self.headless),
            ]
            .into_iter()
            .find_map(|(name, set)| set.then_some(name));
            if let Some(name) = other {
                return Err(format!(
                    "--bus-inspect prints a summary and exits, so it cannot be combined \
                     with {name}. Only --entity, --signal and --saga-digest go with it"
                ));
            }
        }
        if self.replay.is_some() {
            if self.serve.is_some() {
                return Err(
                    "--replay cannot be combined with --serve: --serve publishes this \
                     machine's live telemetry to other instances, and a replay plays a \
                     recorded log"
                        .to_string(),
                );
            }
            if self.remote.is_some() {
                return Err(
                    "--replay cannot be combined with --remote: a replay is read from a \
                     file and a remote box is a second source. Use one or the other"
                        .to_string(),
                );
            }
            let backend_flag = [
                ("--mock", self.mock.is_some()),
                ("--json", self.json),
                ("--host", self.host),
                ("--backend", self.backend != BackendType::Auto),
            ]
            .into_iter()
            .find_map(|(name, set)| set.then_some(name));
            if let Some(name) = backend_flag {
                return Err(format!(
                    "--replay cannot be combined with {name}: a replay reads a recorded \
                     log and no backend. Drop {name}"
                ));
            }
            if !self.headless {
                return Err(
                    "--replay needs --headless in this release: the TUI shows chips from a \
                     live backend. Run `tt-toplike-tui --headless --replay PATH --bus-socket \
                     SOCKET` and read it in a TUI with `--bus-attach SOCKET`"
                        .to_string(),
                );
            }
            if let (Some(rep), Some(dir)) = (
                &self.replay,
                &self.bus_dir.as_ref().or(self.record.as_ref()),
            ) {
                if is_within(rep, dir) {
                    return Err(
                        "--replay names the --bus-dir/--record directory or a file inside it: \
                         a replay cannot record into the log it reads (retention there could \
                         delete the file being played)"
                            .to_string(),
                    );
                }
            }
        }
        if let (Some(r), Some(d)) = (&self.record, &self.bus_dir) {
            if !same_path(r, d) {
                return Err(
                    "--record and --bus-dir name different directories; --record DIR is the \
                     same as --bus-dir DIR, so give one"
                        .to_string(),
                );
            }
        }
        Ok(())
    }
}

/// True when `path` is `dir` or inside it, comparing canonical paths (so
/// `..`, `.` and symlinks are resolved). A path that does not exist yet is
/// compared as written.
fn is_within(path: &std::path::Path, dir: &std::path::Path) -> bool {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let d = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    p.starts_with(&d)
}

/// Short backend name for a log header's `backend` entry. For auto-detect,
/// the first word of `info` (the resolved backend's `backend_info()`),
/// lower-cased: `Mock (3 devices)` gives `mock`.
pub fn backend_tag(bt: BackendType, info: &str) -> String {
    let fixed = match bt {
        BackendType::Auto => None,
        BackendType::Mock => Some("mock"),
        BackendType::Json => Some("json"),
        BackendType::Luwen => Some("luwen"),
        #[cfg(target_os = "linux")]
        BackendType::Sysfs => Some("sysfs"),
        #[cfg(target_os = "linux")]
        BackendType::Hybrid => Some("hybrid"),
        BackendType::Host => Some("host"),
        BackendType::Remote => Some("remote"),
    };
    match fixed {
        Some(n) => n.to_string(),
        None => info
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_lowercase(),
    }
}

/// True when `a` and `b` name the same place: equal after
/// canonicalising, or equal as written when either does not exist yet.
fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_cli() {
        // Simulate default args
        let cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert_eq!(cli.effective_backend(), BackendType::Auto);
        assert_eq!(cli.interval, 100);
        assert!(cli.should_monitor_device(0));
        assert!(cli.should_monitor_device(999));
        // Default is quiet: Warn (only surfaces problems; -v raises to Debug).
        assert_eq!(cli.log_level(), log::LevelFilter::Warn);
    }

    #[test]
    fn test_mock_shortcut() {
        let cli = Cli {
            backend: BackendType::Auto,
            mock: Some(0),
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert_eq!(cli.effective_backend(), BackendType::Mock);
    }

    #[test]
    fn test_json_shortcut() {
        let cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: true,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert_eq!(cli.effective_backend(), BackendType::Json);
    }

    #[test]
    fn test_device_filtering() {
        let cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: Some(vec![0, 2, 4]),
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert!(cli.should_monitor_device(0));
        assert!(!cli.should_monitor_device(1));
        assert!(cli.should_monitor_device(2));
        assert!(!cli.should_monitor_device(3));
        assert!(cli.should_monitor_device(4));
    }

    #[test]
    fn test_verbose_quiet() {
        let verbose_cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: true,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert_eq!(verbose_cli.log_level(), log::LevelFilter::Debug);

        let quiet_cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
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
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert_eq!(quiet_cli.log_level(), log::LevelFilter::Off);
    }

    #[test]
    fn test_luwen_validation() {
        let cli = Cli {
            backend: BackendType::Luwen,
            mock: None,
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        // `validate()` only rejects `--backend luwen` when the crate was built
        // WITHOUT the `luwen-backend` feature (see `Cli::validate()` above) —
        // when the feature is compiled in, luwen is a legitimate, if invasive,
        // choice and validate() has nothing to say about it. Assert the
        // behavior that actually matches each compile-time configuration
        // rather than assuming rejection unconditionally.
        #[cfg(not(feature = "luwen-backend"))]
        assert!(cli.validate().is_err());
        #[cfg(feature = "luwen-backend")]
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn test_backend_names() {
        let auto_cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert_eq!(auto_cli.backend_name(), "Auto-detect");

        let mock_cli = Cli {
            backend: BackendType::Mock,
            mock: Some(0),
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        };

        assert_eq!(mock_cli.backend_name(), "Mock");
    }

    fn mock_cli_with(mock: Option<usize>, mock_devices: usize) -> Cli {
        Cli {
            backend: BackendType::Auto,
            mock,
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            check_config: false,
            arcade_layout: None,
            throttle: false,
            idle_on_blur: false,
            tt_smi_reset_behavior: None,
            record: None,
            replay: None,
            speed: None,
            bus_inspect: None,
            headless: false,
            bus_dir: None,
            bus_socket: None,
            no_bus_attach: false,
            bus_attach: None,
            bus_retain: None,
            mock_workloads: false,
            saga_digest: false,
            entity: None,
            signal: None,
        }
    }

    #[test]
    fn tt_filter_applies_to_real_tt_backends_only() {
        use super::*;
        assert!(!backend_shows_only_tt(BackendType::Host));
        assert!(!backend_shows_only_tt(BackendType::Mock));
        // Remote: process list is the LOCAL machine's, not the remote chips' —
        // show it unfiltered like Host/Mock.
        assert!(!backend_shows_only_tt(BackendType::Remote));
        assert!(backend_shows_only_tt(BackendType::Json));
        assert!(backend_shows_only_tt(BackendType::Luwen));

        #[cfg(target_os = "linux")]
        {
            assert!(backend_shows_only_tt(BackendType::Sysfs));
            assert!(backend_shows_only_tt(BackendType::Hybrid));
        }
    }

    #[test]
    fn test_effective_mock_devices() {
        // mock=None: falls back to mock_devices default
        assert_eq!(mock_cli_with(None, 3).effective_mock_devices(), 3);

        // mock=Some(0): sentinel for bare --mock, falls back to mock_devices
        assert_eq!(mock_cli_with(Some(0), 3).effective_mock_devices(), 3);

        // mock=Some(0) with custom mock_devices: still falls back to that value
        assert_eq!(mock_cli_with(Some(0), 8).effective_mock_devices(), 8);

        // mock=Some(N > 0): inline count takes precedence over mock_devices
        assert_eq!(mock_cli_with(Some(32), 3).effective_mock_devices(), 32);

        // mock=Some(1): edge — 1 > 0 so inline count wins
        assert_eq!(mock_cli_with(Some(1), 5).effective_mock_devices(), 1);
    }

    #[test]
    fn parse_serve_bind_bare_port_binds_all_interfaces() {
        assert_eq!(
            parse_serve_bind("8770").unwrap(),
            "0.0.0.0:8770".parse().unwrap()
        );
    }

    #[test]
    fn parse_serve_bind_host_and_port_used_as_is() {
        assert_eq!(
            parse_serve_bind("127.0.0.1:9000").unwrap(),
            "127.0.0.1:9000".parse().unwrap()
        );
    }

    #[test]
    fn parse_serve_bind_bare_host_defaults_port() {
        assert_eq!(
            parse_serve_bind("192.168.1.5").unwrap(),
            "192.168.1.5:8770".parse().unwrap()
        );
    }

    #[test]
    fn parse_serve_bind_bracketed_ipv6() {
        let addr = parse_serve_bind("[::1]:8770").unwrap();
        assert!(addr.is_ipv6());
        assert_eq!(addr.port(), 8770);
        assert_eq!(addr.ip(), "::1".parse::<std::net::IpAddr>().unwrap());
    }

    #[test]
    fn parse_serve_bind_rejects_garbage() {
        assert!(parse_serve_bind("nonsense:x").is_err());
        assert!(parse_serve_bind("").is_err());
    }

    #[test]
    fn parse_serve_bind_default_port_constant() {
        assert_eq!(DEFAULT_SERVE_PORT, 8770);
    }

    /// `--mode training` and its aliases are the scriptable entry point to a
    /// view that otherwise needs an interactive `t`, so the accepted spellings
    /// are load-bearing: a launcher, kiosk unit, or CI script hard-codes one.
    #[test]
    fn mode_training_and_its_aliases_parse() {
        for spelling in ["training", "train", "tt-train"] {
            let cli = Cli::try_parse_from(["tt-toplike", "--mode", spelling])
                .unwrap_or_else(|e| panic!("`--mode {spelling}` must parse: {e}"));
            assert_eq!(
                cli.mode,
                Some(VisualizationMode::Training),
                "`--mode {spelling}` must select the Training view"
            );
        }
    }

    /// The Wafer Waffler answers to its code name and to its display name.
    #[test]
    fn mode_die3d_and_waffler_parse() {
        for spelling in ["die3d", crate::animation::DIE3D_MODE_ALIAS] {
            let cli = Cli::try_parse_from(["tt-toplike", "--mode", spelling])
                .unwrap_or_else(|e| panic!("`--mode {spelling}` must parse: {e}"));
            assert_eq!(
                cli.mode,
                Some(VisualizationMode::Die3d),
                "--mode {spelling}"
            );
        }
    }

    /// `--mode saga` selects the Saga view.
    #[test]
    fn mode_saga_parses() {
        let cli = Cli::try_parse_from(["tt-toplike", "--mode", "saga"])
            .unwrap_or_else(|e| panic!("`--mode saga` must parse: {e}"));
        assert_eq!(cli.mode, Some(VisualizationMode::Saga));
    }

    /// The neighbouring modes must keep their own spellings — an alias list is
    /// easy to widen by accident until two modes answer to the same word.
    #[test]
    fn mode_training_does_not_swallow_neighbouring_spellings() {
        let cases = [
            ("hivemind", VisualizationMode::Hivemind),
            ("arcade", VisualizationMode::Arcade),
            ("normal", VisualizationMode::Normal),
        ];
        for (spelling, want) in cases {
            let cli = Cli::try_parse_from(["tt-toplike", "--mode", spelling]).unwrap();
            assert_eq!(cli.mode, Some(want), "`--mode {spelling}` regressed");
        }
        assert!(
            Cli::try_parse_from(["tt-toplike", "--mode", "trainingg"]).is_err(),
            "an unknown mode must be rejected, not silently defaulted"
        );
    }

    #[test]
    fn reset_behavior_flag_absent_is_none_and_resolves_to_inform() {
        let cli = Cli::try_parse_from(["tt-toplike"]).unwrap();
        assert_eq!(cli.tt_smi_reset_behavior, None);
        assert_eq!(
            resolve_reset_behavior(cli.tt_smi_reset_behavior, None),
            ResetBehavior::Inform
        );
    }

    #[test]
    fn reset_behavior_all_four_values_parse() {
        for (arg, want) in [
            ("ignore", ResetBehavior::Ignore),
            ("inform", ResetBehavior::Inform),
            ("dazzle", ResetBehavior::Dazzle),
            ("demo", ResetBehavior::Demo),
        ] {
            let cli = Cli::try_parse_from(["tt-toplike", "--tt-smi-reset-behavior", arg]).unwrap();
            assert_eq!(cli.tt_smi_reset_behavior, Some(want), "value {arg}");
        }
    }

    #[test]
    fn reset_behavior_unknown_value_is_rejected() {
        assert!(Cli::try_parse_from(["tt-toplike", "--tt-smi-reset-behavior", "sparkle"]).is_err());
        // The value names are lowercase on the command line.
        assert!(Cli::try_parse_from(["tt-toplike", "--tt-smi-reset-behavior", "Dazzle"]).is_err());
    }

    #[test]
    fn old_reset_takeover_flag_is_gone() {
        assert!(Cli::try_parse_from(["tt-toplike", "--reset-takeover"]).is_err());
    }

    #[test]
    fn reset_behavior_predicates() {
        use ResetBehavior::*;
        // (behavior, detects, animates, is_demo)
        for (b, d, a, m) in [
            (Ignore, false, false, false),
            (Inform, true, false, false),
            (Dazzle, true, true, false),
            (Demo, true, true, true),
        ] {
            assert_eq!(b.detects(), d, "{b:?}.detects()");
            assert_eq!(b.animates(), a, "{b:?}.animates()");
            assert_eq!(b.is_demo(), m, "{b:?}.is_demo()");
        }
    }

    #[test]
    fn reset_behavior_precedence_cli_then_config_then_inform() {
        use ResetBehavior::*;
        // CLI beats config.
        assert_eq!(resolve_reset_behavior(Some(Ignore), Some("dazzle")), Ignore);
        // Config beats the default.
        assert_eq!(resolve_reset_behavior(None, Some("dazzle")), Dazzle);
        assert_eq!(resolve_reset_behavior(None, Some("DEMO")), Demo);
        // Neither set gives inform.
        assert_eq!(resolve_reset_behavior(None, None), Inform);
        // An unknown config value falls back to inform.
        assert_eq!(resolve_reset_behavior(None, Some("sparkle")), Inform);
    }

    #[test]
    fn reset_behavior_help_lists_the_four_values_and_default() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        for name in ["ignore", "inform", "dazzle", "demo"] {
            assert!(help.contains(name), "help is missing {name}");
        }
        assert!(help.contains("--tt-smi-reset-behavior"));
        assert!(help.contains("default: inform"));
    }

    /// The flag's block of the long help: from its name to the next flag.
    fn reset_behavior_help_block() -> String {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        let start = help
            .find("--tt-smi-reset-behavior")
            .expect("flag missing from --help");
        let rest = &help[start..];
        let end = rest[1..].find("\n  -").map_or(rest.len(), |i| i + 1);
        rest[..end].to_string()
    }

    #[test]
    fn reset_behavior_help_describes_each_value_once() {
        let block = reset_behavior_help_block();
        let lower = block.to_lowercase();
        // Each value is listed once, with its description on the same line.
        for (name, description) in [
            ("ignore", "never look for resets"),
            ("inform", "segment in the status bar"),
            ("dazzle", "centered box over a tinted screen"),
            ("demo", "all five reset animations in a row"),
        ] {
            let item = format!("- {name}:");
            assert_eq!(block.matches(&item).count(), 1, "{item}\n{block}");
            let line = block.lines().find(|l| l.contains(&item)).unwrap();
            assert!(line.to_lowercase().contains(description), "{line}");
            assert_eq!(lower.matches(description).count(), 1, "{description}");
        }
        // inform is named as the default, and demo says how to end it.
        assert!(lower.contains("default: inform"), "{block}");
        let inform = block.lines().find(|l| l.contains("- inform:")).unwrap();
        assert!(inform.contains("This is the default"), "{inform}");
        let demo = block.lines().find(|l| l.contains("- demo:")).unwrap();
        assert!(demo.contains("Esc, q or Q ends the demo"), "{demo}");
    }

    // ---- signal bus flags ---------------------------------------------------

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut v = vec!["tt-toplike"];
        v.extend_from_slice(args);
        Cli::try_parse_from(v)
    }

    #[test]
    fn bus_flags_default_to_off() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.record, None);
        assert_eq!(cli.replay, None);
        assert_eq!(cli.speed, None);
        assert_eq!(cli.effective_speed(), 1.0);
        assert_eq!(cli.bus_inspect, None);
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn record_replay_and_speed_parse() {
        let cli = parse(&["--record", "/tmp/rec"]).unwrap();
        assert_eq!(cli.record, Some(PathBuf::from("/tmp/rec")));
        assert!(cli.validate().is_ok());

        let cli = parse(&["--headless", "--replay", "/tmp/rec", "--speed", "4"]).unwrap();
        assert_eq!(cli.replay, Some(PathBuf::from("/tmp/rec")));
        assert_eq!(cli.effective_speed(), 4.0);
        assert!(cli.validate().is_ok());

        let cli = parse(&["--replay", "x.jsonl.gz", "--speed", "0.5"]).unwrap();
        assert_eq!(cli.effective_speed(), 0.5);
    }

    #[test]
    fn speed_needs_replay_and_must_be_positive() {
        assert!(parse(&["--speed", "2"]).is_err(), "--speed alone");
        for bad in ["0", "-1", "nan", "inf"] {
            let cli = parse(&["--replay", "x", "--speed", bad]);
            // "-1" is read by clap as a flag, which is also a rejection.
            if let Ok(cli) = cli {
                let e = cli.validate().unwrap_err();
                assert!(e.contains("above zero"), "{bad}: {e}");
            }
        }
        let cli = parse(&["--replay", "x", "--speed", "0"]).unwrap();
        assert!(cli.validate().unwrap_err().contains("--speed"));
    }

    #[test]
    fn replay_is_rejected_with_serve_or_remote() {
        let cli = parse(&["--replay", "x", "--serve"]).unwrap();
        let e = cli.validate().unwrap_err();
        assert!(e.contains("--replay") && e.contains("--serve"), "{e}");
        assert!(e.contains("live telemetry"), "{e}");

        let cli = parse(&["--replay", "x", "--remote", "10.0.0.5:8000"]).unwrap();
        let e = cli.validate().unwrap_err();
        assert!(e.contains("--replay") && e.contains("--remote"), "{e}");
        assert!(e.contains("second source"), "{e}");
    }

    #[test]
    fn bus_inspect_parses_with_filters_and_stands_alone() {
        let cli = parse(&[
            "--bus-inspect",
            "/tmp/rec",
            "--entity",
            "model:llama-8b",
            "--signal",
            "model.progress",
        ])
        .unwrap();
        assert_eq!(cli.bus_inspect, Some(PathBuf::from("/tmp/rec")));
        assert_eq!(cli.entity.as_deref(), Some("model:llama-8b"));
        assert_eq!(cli.signal.as_deref(), Some("model.progress"));
        assert!(cli.validate().is_ok());

        assert!(parse(&["--entity", "chip:x"]).is_err(), "--entity alone");
        assert!(parse(&["--signal", "s"]).is_err(), "--signal alone");

        let cli = parse(&["--bus-inspect", "x", "--replay", "y"]).unwrap();
        let e = cli.validate().unwrap_err();
        assert!(e.contains("--bus-inspect") && e.contains("--replay"), "{e}");
    }

    /// `--saga-digest` goes with `--bus-inspect` only, so it is refused with
    /// the same live-mode flags `--bus-inspect` refuses. It replaces the
    /// summary, so the summary's filters do not go with it.
    #[test]
    fn saga_digest_needs_bus_inspect_and_no_filters() {
        let cli = parse(&["--bus-inspect", "/tmp/rec", "--saga-digest"]).unwrap();
        assert!(cli.saga_digest);
        assert!(cli.validate().is_ok());
        assert!(!parse(&["--bus-inspect", "x"]).unwrap().saga_digest);

        assert!(parse(&["--saga-digest"]).is_err(), "--saga-digest alone");
        for filter in [&["--entity", "chip:x"][..], &["--signal", "s"]] {
            let mut args = vec!["--bus-inspect", "x", "--saga-digest"];
            args.extend_from_slice(filter);
            assert!(parse(&args).is_err(), "{filter:?}");
        }
        for live in [
            &["--replay", "y"][..],
            &["--record", "y"],
            &["--remote", "10.0.0.5:8000"],
            &["--print"],
            &["--bench"],
        ] {
            let mut args = vec!["--bus-inspect", "x", "--saga-digest"];
            args.extend_from_slice(live);
            let refused = parse(&args).map_or(true, |c| c.validate().is_err());
            assert!(refused, "{live:?}");
        }
        assert!(parse(&["--bus-inspect", "x", "--saga-digest", "--headless"]).is_err());
    }

    // ---- headless and bus host flags (Task 10a) -----------------------------

    #[test]
    fn headless_parses_and_does_not_mix_with_print_bench_or_inspect() {
        let cli = parse(&["--headless", "--mock", "2"]).unwrap();
        assert!(cli.headless);
        assert!(cli.validate().is_ok());
        for other in [&["--print"][..], &["--bench"], &["--bus-inspect", "x"]] {
            let mut args = vec!["--headless"];
            args.extend_from_slice(other);
            assert!(parse(&args).is_err(), "{other:?}");
        }
    }

    #[test]
    fn bus_socket_default_is_in_a_private_directory() {
        assert_eq!(
            resolve_bus_socket(Some("/run/user/1000".into()), 1000),
            PathBuf::from("/run/user/1000/tt-toplike/bus.sock")
        );
        assert_eq!(
            resolve_bus_socket(None, 1000),
            PathBuf::from("/tmp/tt-toplike-1000/bus.sock")
        );
        // Empty or relative values are ignored (XDG spec).
        for bad in ["", "run/user"] {
            assert_eq!(
                resolve_bus_socket(Some(bad.into()), 7),
                PathBuf::from("/tmp/tt-toplike-7/bus.sock"),
                "{bad:?}"
            );
        }
        let cli = parse(&["--headless", "--bus-socket", "/x/s.sock"]).unwrap();
        #[cfg(unix)]
        assert_eq!(cli.bus_socket_path(), PathBuf::from("/x/s.sock"));
        let _ = cli;
    }

    #[test]
    fn bus_dir_default_follows_xdg_state_home_then_home() {
        assert_eq!(
            resolve_bus_dir(Some("/state".into()), Some("/home/u".into())),
            Some(PathBuf::from("/state/tt-toplike/bus"))
        );
        assert_eq!(
            resolve_bus_dir(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.local/state/tt-toplike/bus"))
        );
        assert_eq!(
            resolve_bus_dir(Some("rel".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.local/state/tt-toplike/bus"))
        );
        assert_eq!(resolve_bus_dir(None, None), None);
    }

    #[test]
    fn headless_bus_config_takes_bus_dir_record_or_the_default() {
        let cli = parse(&["--headless", "--bus-dir", "/d"]).unwrap();
        assert_eq!(
            cli.headless_bus_config(false).dir,
            Some(PathBuf::from("/d"))
        );
        let cli = parse(&["--headless", "--record", "/r"]).unwrap();
        assert_eq!(
            cli.headless_bus_config(false).dir,
            Some(PathBuf::from("/r"))
        );
        let cli = parse(&["--headless"]).unwrap();
        assert_eq!(cli.headless_bus_config(false).dir, default_bus_dir());
        // A replay never records into the default live log.
        let cli = parse(&["--headless", "--replay", "/log"]).unwrap();
        assert_eq!(cli.headless_bus_config(false).dir, None);
    }

    #[test]
    fn bus_retain_parses_hours_and_an_optional_size() {
        let cli = parse(&["--headless", "--bus-retain", "2"]).unwrap();
        assert_eq!(
            cli.bus_retain,
            Some(BusRetain {
                secs: 7200,
                bytes: None
            })
        );
        let c = cli.headless_bus_config(false);
        assert_eq!((c.retain_secs, c.retain_bytes), (7200, 5_000_000_000));
        let cli = parse(&["--headless", "--bus-retain", "0.5,1.5"]).unwrap();
        let c = cli.headless_bus_config(false);
        assert_eq!((c.retain_secs, c.retain_bytes), (1800, 1_500_000_000));
        for bad in ["0", "x", "1,", "1,0", "-3", "nan", "1,2,3"] {
            assert!(parse_bus_retain(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn replay_with_a_live_backend_flag_is_rejected() {
        for flag in [
            &["--mock"][..],
            &["--mock", "2"],
            &["--json"],
            &["--host"],
            &["--backend", "json"],
        ] {
            let mut args = vec!["--headless", "--replay", "log"];
            args.extend_from_slice(flag);
            let e = parse(&args).unwrap().validate().unwrap_err();
            assert!(
                e.contains("--replay") && e.contains(flag[0]),
                "{flag:?}: {e}"
            );
        }
        let e = parse(&["--headless", "--replay", "log", "--serve"])
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(e.contains("--serve"), "{e}");
    }

    #[test]
    fn replay_without_headless_points_at_the_headless_command() {
        let e = parse(&["--replay", "log"]).unwrap().validate().unwrap_err();
        assert!(
            e.contains("tt-toplike-tui --headless --replay PATH")
                && e.contains("--bus-socket")
                && e.contains("--bus-attach SOCKET"),
            "{e}"
        );
    }

    #[test]
    fn replay_cannot_record_into_the_log_it_reads() {
        let e = parse(&["--headless", "--replay", "/log", "--bus-dir", "/log"])
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(e.contains("inside it"), "{e}");
        assert!(
            parse(&["--headless", "--replay", "/log", "--bus-dir", "/other"])
                .unwrap()
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn record_and_bus_dir_must_agree() {
        assert!(parse(&["--headless", "--record", "/a", "--bus-dir", "/b"])
            .unwrap()
            .validate()
            .is_err());
        assert!(parse(&["--headless", "--record", "/a", "--bus-dir", "/a"])
            .unwrap()
            .validate()
            .is_ok());
    }

    /// The TUI takes the bus flags. Its bus is memory only unless a
    /// directory is named; it never takes the headless default directory.
    #[test]
    fn the_tui_takes_the_bus_flags() {
        for args in [
            &["--record", "/a"][..],
            &["--bus-dir", "/a"],
            &["--bus-socket", "/a/s"],
            &["--bus-retain", "2,1"],
            &["--no-bus-attach"],
            &["--bus-attach", "/a/s"],
        ] {
            let cli = parse(args).unwrap();
            assert!(cli.validate().is_ok(), "{args:?}");
        }
        assert_eq!(parse(&[]).unwrap().tui_bus_config().dir, None);
        let cli = parse(&["--record", "/r"]).unwrap();
        assert_eq!(cli.tui_bus_config().dir, Some(PathBuf::from("/r")));
        let cli = parse(&["--bus-dir", "/d", "--bus-retain", "2,1"]).unwrap();
        let c = cli.tui_bus_config();
        assert_eq!(c.dir, Some(PathBuf::from("/d")));
        assert_eq!(c.retain_secs, 7_200);
    }

    /// Final fix wave M5: without `--bus-socket` the TUI's bus keeps a
    /// 4,096-record window (and a smaller writer queue); with it, the
    /// defaults.
    #[test]
    fn the_tui_bus_window_is_small_unless_a_socket_is_served() {
        let d = tt_toplike_bus::BusConfig::default();
        for args in [&[][..], &["--record", "/r"][..]] {
            let c = parse(args).unwrap().tui_bus_config();
            assert_eq!(c.window_records, 4_096, "{args:?}");
            assert_eq!(c.writer_queue, 2_048, "{args:?}");
            assert!(c.window_records > c.writer_queue);
        }
        let c = parse(&["--bus-socket", "/a/s"]).unwrap().tui_bus_config();
        assert_eq!(c.window_records, d.window_records);
        assert_eq!(c.writer_queue, d.writer_queue);
        assert!(c.window_records > c.writer_queue);
    }

    /// `--bus-attach` reads another host's bus, so it cannot be combined
    /// with flags that make the TUI host its own.
    #[test]
    fn bus_attach_conflicts_with_hosting_flags() {
        for other in [
            &["--bus-socket", "/s"][..],
            &["--record", "/r"],
            &["--bus-dir", "/d"],
            &["--headless"],
            &["--no-bus-attach"],
            &["--bus-retain", "2"],
        ] {
            let mut args = vec!["--bus-attach", "/a"];
            args.extend_from_slice(other);
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    // ---- fix round 1 --------------------------------------------------------

    #[test]
    fn a_synthetic_headless_run_is_memory_only_unless_a_dir_is_given() {
        let cli = parse(&["--headless", "--mock", "2"]).unwrap();
        assert_eq!(cli.headless_bus_config(true).dir, None);
        let cli = parse(&["--headless", "--mock", "--bus-dir", "/m"]).unwrap();
        assert_eq!(cli.headless_bus_config(true).dir, Some(PathBuf::from("/m")));
        let cli = parse(&["--headless", "--mock", "--record", "/r"]).unwrap();
        assert_eq!(cli.headless_bus_config(true).dir, Some(PathBuf::from("/r")));
        // A real backend still gets the default directory.
        let cli = parse(&["--headless"]).unwrap();
        assert_eq!(cli.headless_bus_config(false).dir, default_bus_dir());
    }

    #[test]
    fn mock_workloads_is_parsed_and_opt_in() {
        let cli = parse(&["--headless", "--mock"]).unwrap();
        assert!(!cli.mock_workloads);
        let cli = parse(&["--headless", "--mock", "--mock-workloads"]).unwrap();
        assert!(cli.mock_workloads);
        assert!(
            parse(&["--headless", "--mock-workloads"]).is_err(),
            "needs --mock"
        );
    }

    #[test]
    fn backend_tag_names_the_backend() {
        assert_eq!(backend_tag(BackendType::Mock, "Mock (2 devices)"), "mock");
        assert_eq!(backend_tag(BackendType::Json, "whatever"), "json");
        // Auto names what it resolved to.
        assert_eq!(backend_tag(BackendType::Auto, "Mock (3 devices)"), "mock");
        assert_eq!(
            backend_tag(BackendType::Auto, "Hybrid (sysfs + tt-smi)"),
            "hybrid"
        );
    }

    /// A replay of one segment inside the bus directory is refused too:
    /// retention there could delete the file being read.
    #[test]
    fn replay_of_a_file_inside_the_bus_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bus");
        std::fs::create_dir(&dir).unwrap();
        let seg = dir.join("bus-3.jsonl");
        std::fs::write(&seg, b"").unwrap();
        let d = dir.to_str().unwrap();
        let dotted = format!("{}/../bus", d);
        for bus_dir in [d, dotted.as_str()] {
            let e = parse(&[
                "--headless",
                "--replay",
                seg.to_str().unwrap(),
                "--bus-dir",
                bus_dir,
            ])
            .unwrap()
            .validate()
            .unwrap_err();
            assert!(e.contains("inside it"), "{e}");
        }
        let other = tmp.path().join("other");
        assert!(parse(&[
            "--headless",
            "--replay",
            seg.to_str().unwrap(),
            "--bus-dir",
            other.to_str().unwrap()
        ])
        .unwrap()
        .validate()
        .is_ok());
    }
}
