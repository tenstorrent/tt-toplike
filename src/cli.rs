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
//! tt-toplike-rs --mock
//!
//! # Use JSON backend with custom tt-smi path
//! tt-toplike-rs --json --tt-smi-path /usr/local/bin/tt-smi
//!
//! # Auto-detect backend (tries JSON, falls back to mock)
//! tt-toplike-rs --backend auto
//!
//! # Verbose output with 50ms update interval
//! tt-toplike-rs -v --interval 50
//!
//! # Monitor specific devices only
//! tt-toplike-rs --devices 0,2,4
//! ```

use clap::{Parser, ValueEnum};
use std::path::PathBuf;

/// Real-time hardware monitoring for Tenstorrent silicon
///
/// TT-Toplike-RS provides beautiful, hardware-responsive visualizations
/// for Tenstorrent AI accelerators with information density comparable to htop.
#[derive(Parser, Debug)]
#[command(name = "tt-toplike-rs")]
#[command(author = "Tenstorrent")]
#[command(version)]
#[command(about = "Real-time hardware monitoring for Tenstorrent silicon", long_about = None)]
#[command(after_help = "EXAMPLES:
    # Use mock backend for testing
    tt-toplike-rs --mock

    # Use JSON backend with custom tt-smi path
    tt-toplike-rs --json --tt-smi-path /usr/local/bin/tt-smi

    # Auto-detect backend (tries JSON first, falls back to mock)
    tt-toplike-rs --backend auto

    # Verbose logging with 50ms update interval
    tt-toplike-rs -v --interval 50

    # Monitor specific devices only
    tt-toplike-rs --devices 0,2,4

    # Quiet mode (no logs, only TUI)
    tt-toplike-rs -q
")]
pub struct Cli {
    /// Backend selection
    #[arg(short, long, value_enum, default_value = "auto")]
    pub backend: BackendType,

    /// Use mock backend (shortcut for --backend mock)
    #[arg(long, conflicts_with = "json")]
    pub mock: bool,

    /// Use JSON backend (shortcut for --backend json)
    #[arg(long, conflicts_with = "mock")]
    pub json: bool,

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
}

/// Backend selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendType {
    /// Automatically detect best backend (Luwen → JSON → Sysfs → Mock)
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
        if cli.mock {
            cli.backend = BackendType::Mock;
        } else if cli.json {
            cli.backend = BackendType::Json;
        }

        cli
    }

    /// Get the effective backend type after resolving shortcuts and auto-detection
    ///
    /// This resolves the --mock and --json shortcut flags into the actual backend type.
    pub fn effective_backend(&self) -> BackendType {
        if self.mock {
            BackendType::Mock
        } else if self.json {
            BackendType::Json
        } else {
            self.backend
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
            log::LevelFilter::Info
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
            return Err("Luwen backend not enabled. Rebuild with: cargo build --features luwen-backend".to_string());
        }

        // Warn if tt-smi-path specified with mock backend
        if self.effective_backend() == BackendType::Mock
            && self.tt_smi_path != PathBuf::from("tt-smi")
        {
            eprintln!(
                "Warning: --tt-smi-path ignored when using mock backend"
            );
        }

        // Warn if mock-devices specified with non-mock backend
        if self.effective_backend() != BackendType::Mock && self.mock_devices != 3 {
            eprintln!(
                "Warning: --mock-devices ignored when not using mock backend"
            );
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
            mock: false,
            json: false,
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
        };

        assert_eq!(cli.effective_backend(), BackendType::Auto);
        assert_eq!(cli.interval, 100);
        assert!(cli.should_monitor_device(0));
        assert!(cli.should_monitor_device(999));
        assert_eq!(cli.log_level(), log::LevelFilter::Info);
    }

    #[test]
    fn test_mock_shortcut() {
        let cli = Cli {
            backend: BackendType::Auto,
            mock: true,
            json: false,
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
        };

        assert_eq!(cli.effective_backend(), BackendType::Mock);
    }

    #[test]
    fn test_json_shortcut() {
        let cli = Cli {
            backend: BackendType::Auto,
            mock: false,
            json: true,
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
        };

        assert_eq!(cli.effective_backend(), BackendType::Json);
    }

    #[test]
    fn test_device_filtering() {
        let cli = Cli {
            backend: BackendType::Auto,
            mock: false,
            json: false,
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
            mock: false,
            json: false,
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
        };

        assert_eq!(verbose_cli.log_level(), log::LevelFilter::Debug);

        let quiet_cli = Cli {
            backend: BackendType::Auto,
            mock: false,
            json: false,
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
        };

        assert_eq!(quiet_cli.log_level(), log::LevelFilter::Off);
    }

    #[test]
    fn test_luwen_validation() {
        let cli = Cli {
            backend: BackendType::Luwen,
            mock: false,
            json: false,
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
        };

        assert!(cli.validate().is_err());
    }

    #[test]
    fn test_backend_names() {
        let auto_cli = Cli {
            backend: BackendType::Auto,
            mock: false,
            json: false,
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
        };

        assert_eq!(auto_cli.backend_name(), "Auto-detect");

        let mock_cli = Cli {
            backend: BackendType::Mock,
            mock: true,
            json: false,
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
        };

        assert_eq!(mock_cli.backend_name(), "Mock");
    }
}
