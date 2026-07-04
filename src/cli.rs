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
    #[arg(
        long,
        value_name = "HOST:PORT",
        conflicts_with = "mock",
        conflicts_with = "json",
        conflicts_with = "host"
    )]
    pub remote: Option<String>,

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

    /// Launch directly into specific visualization mode
    ///
    /// Skip the normal table view and start in a specific mode.
    /// Available modes: normal, starfield, castle, flow, arcade
    #[arg(short = 'm', long, value_enum)]
    pub mode: Option<VisualizationMode>,

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
    /// (temp, power, voltage) while tt-smi is polled every 5s in the background for
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
            throttle: false,
            idle_on_blur: false,
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

        Ok(())
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
        };

        assert_eq!(verbose_cli.log_level(), log::LevelFilter::Debug);

        let quiet_cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: false,
            host: false,
            remote: None,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
        };

        assert!(cli.validate().is_err());
    }

    #[test]
    fn test_backend_names() {
        let auto_cli = Cli {
            backend: BackendType::Auto,
            mock: None,
            json: false,
            host: false,
            remote: None,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
        };

        assert_eq!(auto_cli.backend_name(), "Auto-detect");

        let mock_cli = Cli {
            backend: BackendType::Mock,
            mock: Some(0),
            json: false,
            host: false,
            remote: None,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
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
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
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
}
