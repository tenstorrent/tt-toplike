// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Host system backend — runs on any machine, no TT hardware required
//!
//! Maps the host's CPU sockets, logical cores, and RAM bandwidth proxies
//! into the same `TelemetryBackend` trait as the hardware backends.  Every
//! visualization (Starfield, Memory Castle, Memory Flow, Arcade) works
//! unchanged — they just describe your CPU instead of an AI accelerator.
//!
//! ## Field mapping
//!
//! | Telemetry field   | TT hardware meaning          | Host mapping                         |
//! |-------------------|------------------------------|--------------------------------------|
//! | `asic_temperature`| ASIC die temperature (°C)    | CPU package temperature (°C)         |
//! | `aiclk`           | Tensix clock frequency (MHz) | CPU max-frequency across all cores   |
//! | `power`           | Board power draw (W)         | CPU package power via RAPL (W)       |
//! | `current`         | Supply current (A)           | CPU utilisation (%) used as proxy    |
//! | `voltage`         | Supply voltage (V)           | Not available — `None`               |
//!
//! ## SMBUS / DDR channels
//!
//! The host backend synthesises DDR-channel strings from RAM stats so that
//! the Memory Flow visualisation has something to render.  Each "channel"
//! represents one NUMA node's memory bandwidth share.
//!
//! ## Device topology
//!
//! One `Device` is created per CPU socket.  If the sysinfo crate cannot
//! distinguish sockets it produces a single logical "CPU-0" device.
//! GPU devices via NVML/ROCm are not yet supported but can be added later.
//!
//! ## Usage
//!
//! ```bash
//! tt-toplike --backend host
//! tt-toplike --host          # shortcut flag
//! ```

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use chrono::Utc;
use std::collections::HashMap;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

// ── RAPL power via sysfs (Linux only) ────────────────────────────────────────
// On Linux, Intel and AMD CPUs expose package power through
// /sys/class/powercap/intel-rapl/intel-rapl:N/energy_uj.  We read successive
// samples and divide by the elapsed time to get watts.

#[cfg(target_os = "linux")]
fn rapl_energy_uj(socket: usize) -> Option<u64> {
    // Try both AMD and Intel RAPL paths.
    let paths = [
        format!(
            "/sys/class/powercap/intel-rapl/intel-rapl:{}/energy_uj",
            socket
        ),
        format!(
            "/sys/class/powercap/intel-rapl:{}:{}/energy_uj",
            socket, socket
        ),
    ];
    for path in &paths {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Ok(v) = s.trim().parse::<u64>() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn rapl_energy_uj(_socket: usize) -> Option<u64> {
    None
}

// ── CPU temperature via hwmon (Linux only) ────────────────────────────────────
// Look for a coretemp or k10temp hwmon entry that reports a "Package id N"
// or "Tdie" label — that is the socket-level temperature.

#[cfg(target_os = "linux")]
fn cpu_package_temp_c(socket: usize) -> Option<f32> {
    let hwmon_root = std::path::Path::new("/sys/class/hwmon");
    let entries = std::fs::read_dir(hwmon_root).ok()?;
    for entry in entries.flatten() {
        let base = entry.path();
        // Only coretemp and k10temp expose package temperatures
        let name_path = base.join("name");
        let name = std::fs::read_to_string(&name_path).ok()?;
        let name = name.trim();
        if name != "coretemp" && name != "k10temp" && name != "zenpower" {
            continue;
        }
        // Walk tempN_label files looking for "Package id N" (Intel) or "Tdie"/"Tccd" (AMD)
        let label_target_intel = format!("Package id {}", socket);
        for n in 1..=64u32 {
            let label_path = base.join(format!("temp{}_label", n));
            if let Ok(label) = std::fs::read_to_string(&label_path) {
                let label = label.trim();
                let is_package = label == label_target_intel
                    || (socket == 0 && (label == "Tdie" || label == "Tccd1"));
                if is_package {
                    let input_path = base.join(format!("temp{}_input", n));
                    if let Ok(s) = std::fs::read_to_string(input_path) {
                        if let Ok(millideg) = s.trim().parse::<f32>() {
                            return Some(millideg / 1000.0);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn cpu_package_temp_c(_socket: usize) -> Option<f32> {
    None
}

// ── HostBackend ───────────────────────────────────────────────────────────────

/// Telemetry backend that reads the host CPU/RAM using sysinfo + Linux sysfs.
///
/// Works on any machine — no Tenstorrent hardware needed.  All visualisations
/// (Starfield, Memory Castle, Memory Flow, Arcade) will describe your CPU
/// instead of an AI accelerator.
pub struct HostBackend {
    /// sysinfo system handle
    sys: System,

    /// Discovered host devices (one per socket)
    devices: Vec<Device>,

    /// Current telemetry per device index
    telemetry: HashMap<usize, Telemetry>,

    /// Synthesised SMBUS-like telemetry per device (carries DDR channel stubs)
    smbus: HashMap<usize, SmbusTelemetry>,

    /// RAPL: previous energy samples (socket → microjoules)
    prev_rapl_uj: HashMap<usize, u64>,

    /// RAPL: timestamp of previous energy samples
    prev_rapl_time: std::time::Instant,

    /// Number of CPU sockets (1 if indeterminate)
    socket_count: usize,

    /// Total logical CPU core count
    core_count: usize,

    #[allow(dead_code)]
    config: BackendConfig,
}

impl HostBackend {
    pub fn new() -> Self {
        Self::with_config(BackendConfig::default())
    }

    pub fn with_config(config: BackendConfig) -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        HostBackend {
            sys,
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus: HashMap::new(),
            prev_rapl_uj: HashMap::new(),
            prev_rapl_time: std::time::Instant::now(),
            socket_count: 1,
            core_count: 0,
            config,
        }
    }

    // ── private helpers ────────────────────────────────────────────────────

    /// Guess socket count from CPU brand / physical IDs heuristic.
    ///
    /// sysinfo doesn't expose socket topology directly.  We default to 1
    /// socket and let callers override via the device list length.
    fn detect_socket_count(sys: &System) -> usize {
        // sysinfo 0.38 doesn't have a direct socket API — use 1 as safe default.
        // On a NUMA system you might want to expand this, but that's a future improvement.
        let _ = sys;
        1
    }

    /// Build a SmbusTelemetry that mimics DDR-channel fields from RAM stats.
    ///
    /// The Memory Flow visualisation reads `ddr_speed_mts` and per-channel
    /// health from SmbusTelemetry.  We synthesise plausible values from the
    /// host's total RAM so the vis has something to work with.
    fn make_smbus_from_ram(sys: &System, socket_idx: usize) -> SmbusTelemetry {
        let total_gib = sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0);
        // Each socket "owns" an equal share of total RAM
        let socket_gib = total_gib; // single-socket assumption
        let _ = socket_idx;

        // Synthesise a plausible DDR5 speed label (most modern CPUs are DDR5-5600)
        let ddr_speed = "5600".to_string();

        // Build channel status strings.  Wormhole has 8 channels; we'll
        // use 4 as a reasonable default for a CPU (matching Grayskull DDR count).
        let used_gib = sys.used_memory() as f64 / (1024.0 * 1024.0 * 1024.0);
        let fill_pct = (used_gib / socket_gib * 100.0).min(100.0) as u32;
        let ch_status = format!("{}%", fill_pct);

        SmbusTelemetry {
            // Synthesised DDR fields
            ddr_speed: Some(ddr_speed),
            ddr_status: Some(ch_status),
            // Heartbeat — always alive on host
            arc0_health: Some("1".to_string()),
            // Leave everything else None — host doesn't have SMBUS
            ..SmbusTelemetry::default()
        }
    }

    /// Sample telemetry for one socket and update internal maps.
    fn sample_socket(&mut self, socket_idx: usize) {
        let cpus = self.sys.cpus();
        if cpus.is_empty() {
            return;
        }

        // Average CPU usage across all cores (proxy for "current" / load %)
        let core_count = cpus.len();
        let avg_usage: f32 = cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / core_count as f32;

        // Max frequency across all cores (proxy for "aiclk")
        let max_freq_mhz = cpus.iter().map(|c| c.frequency()).max().unwrap_or(0) as u32;

        // Temperature — try hwmon sysfs first, fall back to sysinfo component temp
        let temp_c = cpu_package_temp_c(socket_idx);

        // RAPL power
        let now = std::time::Instant::now();
        let power_w = rapl_energy_uj(socket_idx).and_then(|uj| {
            let prev = self.prev_rapl_uj.get(&socket_idx).copied();
            let dt = now.duration_since(self.prev_rapl_time).as_secs_f64();
            self.prev_rapl_uj.insert(socket_idx, uj);
            if let Some(prev_uj) = prev {
                if dt > 0.0 && uj >= prev_uj {
                    let delta_uj = uj - prev_uj;
                    Some((delta_uj as f64 / 1_000_000.0 / dt) as f32)
                } else {
                    None
                }
            } else {
                None
            }
        });
        self.prev_rapl_time = now;

        // RAM — expose as current proxy when RAPL not available
        let mem_used_gib = self.sys.used_memory() as f32 / (1024.0 * 1024.0 * 1024.0);
        let mem_total_gib = self.sys.total_memory() as f32 / (1024.0 * 1024.0 * 1024.0);
        // Use memory bandwidth proxy: load pct as amperes, total RAM as a scale
        let current_proxy = avg_usage / 100.0 * mem_total_gib;

        let telem = Telemetry {
            voltage: None,
            current: Some(if power_w.is_some() {
                // When we have real power, use usage% * max_freq as current proxy
                avg_usage / 100.0 * max_freq_mhz as f32 / 1000.0
            } else {
                current_proxy
            }),
            power: power_w.or_else(|| {
                // Rough TDP proxy from load: assume max 65W desktop / 15W laptop
                // Use 45W as a middle ground for unknown system
                Some(avg_usage / 100.0 * 45.0)
            }),
            asic_temperature: temp_c,
            aiclk: Some(max_freq_mhz),
            heartbeat: Some(1),
            timestamp: Utc::now(),
        };

        self.telemetry.insert(socket_idx, telem);

        let smbus = Self::make_smbus_from_ram(&self.sys, socket_idx);
        self.smbus.insert(socket_idx, smbus);

        let _ = mem_used_gib;
    }
}

impl TelemetryBackend for HostBackend {
    fn init(&mut self) -> BackendResult<()> {
        // Two refreshes with a small gap so sysinfo can compute meaningful CPU%.
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        std::thread::sleep(std::time::Duration::from_millis(150));
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        self.core_count = self.sys.cpus().len();
        self.socket_count = Self::detect_socket_count(&self.sys);

        if self.core_count == 0 {
            return Err(BackendError::Initialization(
                "sysinfo: no CPU cores found".to_string(),
            ));
        }

        // Build device list — one entry per socket
        let cpu_brand = self
            .sys
            .cpus()
            .first()
            .map(|c| c.brand().to_string())
            .unwrap_or_else(|| "CPU".to_string());

        // Map this socket's logical cores into a near-square star grid so the
        // Starfield visualization has one "core" per CPU thread to render
        // (Architecture::Unknown would otherwise yield a 0×0 grid → empty view).
        let cores_per_socket = (self.core_count / self.socket_count.max(1)).max(1);
        let grid_cols = (cores_per_socket as f64).sqrt().ceil() as usize;
        let grid_cols = grid_cols.max(1);
        let grid_rows = cores_per_socket.div_ceil(grid_cols).max(1);

        for sock in 0..self.socket_count {
            let device = Device {
                index: sock,
                // Use "cpu-N" as board_type so name() formats as "Unknown-N"
                // (Architecture::Unknown is the right category for host CPUs)
                board_type: format!("cpu-{}", sock),
                bus_id: format!("socket:{}", sock),
                architecture: Architecture::Unknown,
                coords: format!("({},0)", sock),
                firmwares: None,
                limits: None,
                pcie_speed: None,
                pcie_width: None,
                // CPU cores → star grid; 4 synthesised DDR channels (see
                // make_smbus_from_ram) so Memory Castle/Flow render channels.
                grid_override: Some((grid_rows, grid_cols)),
                channels_override: Some(4),
            };
            self.devices.push(device);
            log::info!(
                "HostBackend: socket {} — {} ({} logical cores)",
                sock,
                cpu_brand,
                self.core_count / self.socket_count
            );
        }

        // Seed RAPL baseline for first real power sample
        for sock in 0..self.socket_count {
            if let Some(uj) = rapl_energy_uj(sock) {
                self.prev_rapl_uj.insert(sock, uj);
            }
        }

        // Initial telemetry sample
        for sock in 0..self.socket_count {
            self.sample_socket(sock);
        }

        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        for sock in 0..self.socket_count {
            self.sample_socket(sock);
        }
        Ok(())
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry> {
        self.telemetry.get(&device_idx)
    }

    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        self.smbus.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        let brand = self
            .sys
            .cpus()
            .first()
            .map(|c| c.brand().to_string())
            .unwrap_or_else(|| "CPU".to_string());
        format!(
            "Host ({} — {} cores, {} sockets)",
            brand, self.core_count, self.socket_count
        )
    }
}

impl Default for HostBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_backend_init_and_update() {
        let mut backend = HostBackend::new();
        assert!(backend.init().is_ok());
        assert!(
            !backend.devices().is_empty(),
            "should discover at least one device"
        );

        let dev = &backend.devices()[0];
        assert_eq!(dev.architecture, Architecture::Unknown);
        assert!(dev.bus_id.starts_with("socket:"));

        assert!(backend.telemetry(0).is_some());

        let t = backend.telemetry(0).unwrap();
        assert!(t.aiclk.unwrap_or(0) > 0, "CPU frequency should be non-zero");
        assert!(
            t.power.is_some(),
            "power (RAPL or proxy) should be available"
        );

        assert!(backend.update().is_ok());
    }

    #[test]
    fn test_host_device_has_renderable_geometry() {
        // The Host backend must give its CPU "device" a non-zero star grid and
        // channel count (via the overrides) so Starfield/Memory Castle render
        // something instead of an empty 0×0 Architecture::Unknown grid.
        let mut backend = HostBackend::new();
        backend.init().unwrap();
        let dev = &backend.devices()[0];

        let (rows, cols) = dev.tensix_grid();
        assert!(
            rows > 0 && cols > 0,
            "host grid must be non-empty (was {rows}x{cols}) so Starfield renders"
        );
        assert!(
            dev.memory_channels() >= 1,
            "host must report >=1 memory channel for the DDR visualizations"
        );
    }

    #[test]
    fn test_host_backend_info() {
        let mut backend = HostBackend::new();
        backend.init().unwrap();
        let info = backend.backend_info();
        assert!(
            info.starts_with("Host ("),
            "backend_info should start with 'Host (': {}",
            info
        );
    }
}
