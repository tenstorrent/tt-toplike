//! Process monitoring for Tenstorrent device usage
//!
//! This module provides functionality to detect which processes are currently
//! using Tenstorrent hardware devices by scanning `/proc/[pid]/fd/` for open
//! file descriptors pointing to `/dev/tenstorrent/*` devices.
//!
//! It also detects processes using hugepages or Tenstorrent-related memory
//! mappings, which indicates shared resource usage even without direct device
//! file access.

use procfs::process::{FDTarget, Process};
use std::collections::HashMap;

/// Information about a process using Tenstorrent devices
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    /// Process ID
    pub pid: i32,
    /// Process name (from /proc/[pid]/stat)
    pub name: String,
    /// Full command line (from /proc/[pid]/cmdline)
    pub cmdline: String,
    /// List of device indices this process is using (e.g., [0, 2] for devices 0 and 2)
    pub device_indices: Vec<usize>,
    /// Count of 1GB hugepages mapped by this process
    pub hugepages_1g: usize,
    /// Count of 2MB hugepages mapped by this process
    pub hugepages_2m: usize,
    /// Whether this process has Tenstorrent-related memory mappings
    pub has_tt_mapping: bool,
}

/// Process monitor that scans for processes using Tenstorrent devices
///
/// This scanner uses the `/proc` filesystem to detect:
/// 1. Processes with open file descriptors to `/dev/tenstorrent/*` devices
/// 2. Processes using hugepages (likely for DMA/memory-mapped I/O)
/// 3. Processes with Tenstorrent-related memory mappings
///
/// # Example
///
/// ```no_run
/// use tt_toplike::workload::ProcessMonitor;
///
/// let mut monitor = ProcessMonitor::new();
/// monitor.update();
///
/// // Check which processes are using device 0
/// if let Some(processes) = monitor.get_processes_for_device(0) {
///     for proc in processes {
///         println!("PID {} ({}): {}", proc.pid, proc.name, proc.cmdline);
///     }
/// }
/// ```
pub struct ProcessMonitor {
    /// Map of device index to list of processes using that device
    device_processes: HashMap<usize, Vec<ProcessInfo>>,
    /// Processes using hugepages but no specific device file
    /// (e.g., Docker containers, shared memory workloads)
    shared_processes: Vec<ProcessInfo>,
}

impl ProcessMonitor {
    /// Create a new process monitor
    pub fn new() -> Self {
        ProcessMonitor {
            device_processes: HashMap::new(),
            shared_processes: Vec::new(),
        }
    }

    /// Update the process list by scanning `/proc`
    ///
    /// This method scans all processes in `/proc/[pid]/` and checks for:
    /// - Open file descriptors to `/dev/tenstorrent/*` devices
    /// - Hugepages usage
    /// - Tenstorrent-related memory mappings
    ///
    /// This is relatively lightweight (<10ms typically) but should be called
    /// every few seconds rather than every frame to avoid overhead.
    pub fn update(&mut self) {
        // Clear previous state
        self.device_processes.clear();
        self.shared_processes.clear();

        // Attempt to scan all processes, but don't fail on errors
        // Some processes may be inaccessible due to permissions
        match procfs::process::all_processes() {
            Ok(processes) => {
                for process_result in processes {
                    if let Ok(process) = process_result {
                        // Scan process, silently skip if permission denied
                        let _ = self.scan_process(&process);
                    }
                }
            }
            Err(e) => {
                // If /proc is completely inaccessible, log once but don't crash
                log::warn!("Failed to scan processes: {}. Process monitoring disabled.", e);
            }
        }
    }

    /// Scan a single process for Tenstorrent device usage
    fn scan_process(&mut self, process: &Process) -> Result<(), Box<dyn std::error::Error>> {
        let mut device_indices = Vec::new();
        let mut hugepages_1g = 0;
        let mut hugepages_2m = 0;
        let mut has_tt_mapping = false;

        // Check file descriptors for /dev/tenstorrent/* devices
        if let Ok(fds) = process.fd() {
            for fd_result in fds {
                if let Ok(fd_info) = fd_result {
                    // Check if FD points to /dev/tenstorrent/*
                    if let FDTarget::Path(path) = fd_info.target {
                        let path_str = path.to_string_lossy();
                        if path_str.starts_with("/dev/tenstorrent/") {
                            // Extract device index: /dev/tenstorrent/0 -> 0
                            if let Some(idx_str) = path_str.strip_prefix("/dev/tenstorrent/") {
                                if let Ok(idx) = idx_str.parse::<usize>() {
                                    if !device_indices.contains(&idx) {
                                        device_indices.push(idx);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also check memory maps for hugepages and TT mappings
        if let Ok(maps) = process.maps() {
            for map in maps {
                if let procfs::process::MMapPath::Path(path) = &map.pathname {
                    let path_str = path.to_string_lossy();

                    // Count hugepages by size
                    if path_str.contains("hugepages-1G") || path_str.contains("pagesize-1GB") {
                        hugepages_1g += 1;
                    } else if path_str.contains("hugepages") {
                        // Assume 2M if not explicitly 1G
                        hugepages_2m += 1;
                    }

                    // Check for Tenstorrent-related mappings
                    if path_str.contains("tenstorrent")
                        || path_str.contains("tt_")
                        || path_str.contains("blackhole")
                        || path_str.contains("wormhole")
                        || path_str.contains("grayskull")
                    {
                        has_tt_mapping = true;
                    }
                }
            }
        }

        // If process uses tenstorrent devices OR has hugepages/TT mappings, record it
        if !device_indices.is_empty()
            || hugepages_1g > 0
            || hugepages_2m > 0
            || has_tt_mapping
        {
            let cmdline = process
                .cmdline()
                .ok()
                .and_then(|args| {
                    if args.is_empty() {
                        None
                    } else {
                        Some(args.join(" "))
                    }
                })
                .unwrap_or_else(|| format!("[pid {}]", process.pid()));

            let name = process
                .stat()
                .ok()
                .map(|s| s.comm)
                .unwrap_or_else(|| "unknown".to_string());

            let proc_info = ProcessInfo {
                pid: process.pid(),
                name,
                cmdline: cmdline.chars().take(60).collect(), // Truncate long commands
                device_indices: device_indices.clone(),
                hugepages_1g,
                hugepages_2m,
                has_tt_mapping,
            };

            // Add to device-specific lists
            if !device_indices.is_empty() {
                for &idx in &device_indices {
                    self.device_processes
                        .entry(idx)
                        .or_insert_with(Vec::new)
                        .push(proc_info.clone());
                }
            } else if hugepages_1g > 0 || hugepages_2m > 0 || has_tt_mapping {
                // Process uses hugepages/TT resources but no specific device file
                self.shared_processes.push(proc_info);
            }
        }

        Ok(())
    }

    /// Get the list of processes using a specific device
    ///
    /// Returns `None` if no processes are using the device, or `Some(&Vec<ProcessInfo>)`
    /// if one or more processes are using it.
    pub fn get_processes_for_device(&self, device_idx: usize) -> Option<&Vec<ProcessInfo>> {
        self.device_processes.get(&device_idx)
    }

    /// Get the list of processes using shared resources (hugepages, TT mappings)
    /// but not directly accessing device files
    pub fn get_shared_processes(&self) -> &Vec<ProcessInfo> {
        &self.shared_processes
    }

    /// Check if any processes are using Tenstorrent devices or resources
    pub fn has_any_processes(&self) -> bool {
        !self.device_processes.is_empty() || !self.shared_processes.is_empty()
    }
}

impl Default for ProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}
