// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Process monitoring for Tenstorrent device usage
//!
//! This module provides functionality to detect which processes are currently
//! using Tenstorrent hardware devices by scanning `/proc/[pid]/fd/` for open
//! file descriptors pointing to `/dev/tenstorrent/*` devices.
//!
//! It also detects processes using hugepages or Tenstorrent-related memory
//! mappings, which indicates shared resource usage even without direct device
//! file access.
//!
//! The driver's own holder lists (`/proc/driver/tenstorrent/<N>/pids`) are
//! read first, so processes of other users and in containers are attributed
//! too, without reading their `fd/`. The device numbers are mapped to the
//! backend's indices with a [`DeviceMap`]. See
//! [`crate::workload::device_holders`] for the sources, the map and the
//! bounds.

use crate::workload::device_holders::{
    self, DeviceMap, FdRead, Holder, HolderSource, Limits, ProcHolders, ScanCounts,
};
use procfs::process::Process;
use std::collections::HashMap;
use std::path::PathBuf;

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
    /// A tt-smi snapshot run (`tt-smi -s`), decided on the full argv before
    /// `cmdline` is cut for display. See
    /// [`crate::workload::device_holders::is_snapshot_tool`].
    pub snapshot_tool: bool,
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
    /// The `/proc` to read. `/proc` itself, except in tests.
    root: PathBuf,
    /// True when the last scan read the driver's holder lists, so it saw
    /// every process holding a device, whoever owns it.
    complete: bool,
    /// Why the last scan's answer may be partial.
    counts: ScanCounts,
    /// How much one scan reads.
    limits: Limits,
}

/// Most characters of a command line kept for display.
const CMDLINE_DISPLAY_CHARS: usize = 60;

/// The display command line (cut to [`CMDLINE_DISPLAY_CHARS`]) and whether the
/// process is a tt-smi snapshot run, decided on the whole argv first: a
/// venv interpreter path can push `-s` past the cut.
fn command_line(process: &Process, comm: &str) -> (String, bool) {
    let argv = process.cmdline().unwrap_or_default();
    let full = argv.join(" ");
    let snapshot = crate::workload::device_holders::is_snapshot_tool(comm, &full);
    let shown = if argv.is_empty() {
        format!("[pid {}]", process.pid())
    } else {
        full.chars().take(CMDLINE_DISPLAY_CHARS).collect()
    };
    (shown, snapshot)
}

pub use crate::workload::device_holders::MAX_PROCESSES;

impl ProcessMonitor {
    /// Create a new process monitor
    pub fn new() -> Self {
        Self::with_root("/proc")
    }

    /// A monitor that reads `root` as its `/proc`. Tests point it at a temp
    /// directory laid out like `/proc`.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        ProcessMonitor {
            device_processes: HashMap::new(),
            shared_processes: Vec::new(),
            root: root.into(),
            complete: false,
            counts: ScanCounts::default(),
            limits: Limits::default(),
        }
    }

    /// The same monitor with other bounds (tests make them small).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Update the process list by scanning `/proc`, using each device number
    /// as its index. See [`Self::update_mapped`].
    pub fn update(&mut self) {
        self.update_mapped(&DeviceMap::identity());
    }

    /// Update the process list by scanning `/proc`.
    ///
    /// A process holds a device when the driver lists it in
    /// `/proc/driver/tenstorrent/<N>/pids` (any user) or when one of its
    /// `fd` links points at `/dev/tenstorrent/<N>` (processes this user may
    /// read). `map` turns each device number into the backend's index; see
    /// [`crate::workload::device_holders`]. Processes using hugepages or
    /// Tenstorrent-related memory mappings are also recorded, for the
    /// processes this user may read.
    ///
    /// A process that exits during the scan is skipped and counted. A
    /// process owned by another user is never read beyond the world-readable
    /// `stat` and `cmdline`, and only when the driver lists it.
    pub fn update_mapped(&mut self, map: &DeviceMap) {
        // Clear previous state
        self.device_processes.clear();
        self.shared_processes.clear();
        self.counts = ScanCounts::default();

        let driver = device_holders::read_driver_pids_with(&self.root, &self.limits);
        self.complete = driver.is_some();
        let driver = driver.unwrap_or_default();

        // Attempt to scan all processes, but don't fail on errors
        // Some processes may be inaccessible due to permissions
        match procfs::process::all_processes_with_root(&self.root) {
            Ok(mut processes) => {
                let mut looked_at = 0usize;
                while let Some(process_result) = processes.next() {
                    if looked_at >= self.limits.processes {
                        // Stop here. The rest are counted, not read: each
                        // costs one directory open and nothing else.
                        self.counts.proc_truncated = 1 + processes.count();
                        break;
                    }
                    looked_at += 1;
                    let Ok(process) = process_result else {
                        // Listed, then gone before it could be opened.
                        self.counts.vanished += 1;
                        continue;
                    };
                    let held = driver.get(&process.pid()).cloned().unwrap_or_default();
                    // Skip processes this user does not own BEFORE touching
                    // `fd()` or `maps()`. Both are denied cross-uid, and a
                    // denied open is an audited event on a STIG host while
                    // this loop runs every 2 s over every process. The old
                    // "silently skip if permission denied" was silent to us
                    // but not to auditd. See `crate::workload::proc_access`.
                    if !crate::workload::proc_access::may_read_in(&self.root, process.pid()) {
                        self.counts.foreign += 1;
                        if !held.is_empty() {
                            // The driver says it holds a device: record it from
                            // the world-readable `stat` and `cmdline` alone.
                            self.record_foreign(&process, &held, map);
                        }
                        continue;
                    }
                    // A denied open is silent to this program but not to
                    // the host: auditd can log each one. Ownership and the
                    // denial cache are checked above; the scan itself
                    // still tolerates a refusal, which it then remembers.
                    self.scan_process(&process, &held, map);
                }
            }
            Err(e) => {
                // If /proc is completely inaccessible, log once but don't crash
                log::warn!(
                    "Failed to scan processes: {}. Process monitoring disabled.",
                    e
                );
            }
        }
    }

    /// Whether the last scan saw every holder (it read the driver's lists).
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Why the last scan's answer may be partial.
    pub fn counts(&self) -> ScanCounts {
        self.counts
    }

    /// Record a process another user owns that the driver lists as a holder.
    fn record_foreign(&mut self, process: &Process, held: &[u32], map: &DeviceMap) {
        let Ok(stat) = process.stat() else {
            self.counts.vanished += 1;
            return;
        };
        let (cmdline, snapshot_tool) = command_line(process, &stat.comm);
        self.push(ProcessInfo {
            pid: process.pid(),
            name: stat.comm,
            cmdline,
            snapshot_tool,
            device_indices: map.indices_for(held),
            hugepages_1g: 0,
            hugepages_2m: 0,
            has_tt_mapping: false,
        });
    }

    /// File a process under each device it holds, or as a shared user.
    fn push(&mut self, info: ProcessInfo) {
        if info.device_indices.is_empty() {
            self.shared_processes.push(info);
            return;
        }
        for &idx in &info.device_indices {
            self.device_processes
                .entry(idx)
                .or_default()
                .push(info.clone());
        }
    }

    /// Scan a single process for Tenstorrent device usage. `held` is what
    /// the driver lists for it (device numbers).
    fn scan_process(&mut self, process: &Process, held: &[u32], map: &DeviceMap) {
        let mut numbers: Vec<u32> = held.to_vec();
        let mut hugepages_1g = 0;
        let mut hugepages_2m = 0;
        let mut has_tt_mapping = false;

        // Check file descriptors for /dev/tenstorrent/* devices. Links are
        // read with `readlink` only; nothing is opened.
        let (fds, truncated) = device_holders::fd_numbers_with(
            &self.root.join(process.pid().to_string()).join("fd"),
            self.limits.fds_per_process,
        );
        if truncated {
            self.counts.fd_truncated += 1;
        }
        match fds {
            FdRead::Denied => {
                // We own this pid but the kernel's ptrace-style access check still
                // refuses `fd/`; `maps` is refused for the same reason, so skip it
                // too and remember the pid so later scans do not retry for a while.
                crate::workload::proc_access::note_denied(process.pid());
                self.counts.denied += 1;
                if !held.is_empty() {
                    self.record_foreign(process, held, map);
                }
                return;
            }
            FdRead::Gone => {
                self.counts.vanished += 1;
                return;
            }
            FdRead::Numbers(found) => {
                for n in found {
                    if !numbers.contains(&n) {
                        numbers.push(n);
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
        if !numbers.is_empty() || hugepages_1g > 0 || hugepages_2m > 0 || has_tt_mapping {
            let Ok(stat) = process.stat() else {
                // It exited between the fd read and now.
                self.counts.vanished += 1;
                return;
            };
            let (cmdline, snapshot_tool) = command_line(process, &stat.comm);

            self.push(ProcessInfo {
                pid: process.pid(),
                name: stat.comm,
                cmdline,
                snapshot_tool,
                device_indices: map.indices_for(&numbers),
                hugepages_1g,
                hugepages_2m,
                has_tt_mapping,
            });
        }
    }

    /// Every process holding a device, for blending with tt-smi.
    pub fn holders(&self, map: &DeviceMap) -> ProcHolders {
        let mut by_pid: std::collections::BTreeMap<i32, &ProcessInfo> = Default::default();
        for list in self.device_processes.values() {
            for p in list {
                by_pid.entry(p.pid).or_insert(p);
            }
        }
        ProcHolders {
            holders: by_pid
                .into_values()
                .map(|p| Holder {
                    pid: p.pid as i64,
                    devices: p.device_indices.clone(),
                    name: p.name.clone(),
                    cmdline: p.cmdline.clone(),
                    snapshot_tool: p.snapshot_tool,
                    source: HolderSource::Proc,
                })
                .collect(),
            complete: self.complete,
            map: map.clone(),
        }
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

    /// Iterate over device indices that have at least one associated process.
    pub fn device_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.device_processes.keys().copied()
    }
}

impl Default for ProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Send a signal to a process by PID.
///
/// Returns `Ok(())` on success, or an `io::Error` with the OS error on failure.
/// Common errors: `ESRCH` (no such process), `EPERM` (permission denied).
#[cfg(unix)]
pub fn kill_pid(pid: i32, signal: libc::c_int) -> std::io::Result<()> {
    let ret = unsafe { libc::kill(pid, signal) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn kill_pid_invalid_returns_error() {
        // PID 1 (init/systemd) is owned by root.
        // Signal 0 is a pure permission check — no signal is delivered — so this is safe
        // in containers and CI regardless of whether the test runner is root.
        let result = kill_pid(1, 0);
        if unsafe { libc::getuid() } == 0 {
            // As root, signal 0 to any existing PID succeeds.
            assert!(result.is_ok(), "kill_pid(1, 0) should succeed for root");
        } else {
            assert!(
                result.is_err(),
                "kill_pid(1, 0) should fail for non-root (EPERM)"
            );
            assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EPERM));
        }
    }

    #[test]
    fn kill_pid_nonexistent_returns_error() {
        // PID i32::MAX is virtually guaranteed to not exist.
        let result = kill_pid(i32::MAX, libc::SIGTERM);
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Expected: ESRCH (no such process)
        assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
    }
}

/// Wiring tests: no guarded reader may attempt a `/proc` read on a pid this
/// user cannot read. See `proc_access::foreign_pid_for_test`.
#[cfg(all(test, target_os = "linux"))]
mod proc_guard_tests {
    use super::*;
    use crate::workload::proc_access::{foreign_pid_for_test, is_denied_for_test};

    #[test]
    fn full_scan_never_attempts_a_foreign_pid() {
        let Some(pid) = foreign_pid_for_test() else {
            return;
        };
        ProcessMonitor::new().update();
        assert!(
            !is_denied_for_test(pid),
            "a /proc read was attempted on a foreign pid"
        );
    }
}

/// The scan against a fake `/proc`: driver lists, `fd` links, a refused `fd/`
/// directory and a process that vanished during the scan. Pids are large so
/// the process-wide denial cache never meets a real pid.
#[cfg(all(test, target_os = "linux"))]
mod fake_root_tests {
    use super::*;
    use crate::workload::device_holders::Limits;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;

    /// A `/proc/<pid>` with a `stat`, a `cmdline` and `fd` links.
    fn fake_pid(root: &Path, pid: i32, comm: &str, links: &[&str]) {
        let d = root.join(pid.to_string());
        fs::create_dir_all(d.join("fd")).unwrap();
        // 52 fields, as a current kernel writes them.
        let rest: Vec<String> = (0..50)
            .map(|i| if i == 0 { "S".into() } else { "0".into() })
            .collect();
        fs::write(
            d.join("stat"),
            format!("{pid} ({comm}) {}\n", rest.join(" ")),
        )
        .unwrap();
        fs::write(d.join("cmdline"), format!("{comm}\0--serve\0")).unwrap();
        for (i, l) in links.iter().enumerate() {
            symlink(l, d.join("fd").join((i + 3).to_string())).unwrap();
        }
    }

    /// Like [`fake_pid`], with this exact argv in `cmdline`.
    fn fake_pid_argv(root: &Path, pid: i32, comm: &str, argv: &[&str], links: &[&str]) {
        fake_pid(root, pid, comm, links);
        let mut text = argv.join("\0");
        text.push('\0');
        fs::write(root.join(pid.to_string()).join("cmdline"), text).unwrap();
    }

    /// The `(pid, device)` pairs the Wafer Waffler gets from a scan of `root`.
    fn waffler_pairs(root: &Path) -> Vec<(i64, usize)> {
        let mut pm = ProcessMonitor::with_root(root);
        pm.update();
        crate::workload::device_holders::opened_pairs(&pm.holders(&DeviceMap::identity()).holders)
    }

    const ALL_FOUR: [&str; 4] = [
        "/dev/tenstorrent/0",
        "/dev/tenstorrent/1",
        "/dev/tenstorrent/2",
        "/dev/tenstorrent/3",
    ];

    /// This box's `tt-smi -s`: a Python venv interpreter running the script,
    /// 75 characters, so a 60-character cut loses the `-s`. Through the real
    /// scan it must still not be a process event, nor at any longer length.
    #[test]
    fn a_long_tt_smi_snapshot_command_line_is_still_a_snapshot_run() {
        let long_venv = format!("/opt/{}/venv/bin/python3", "x".repeat(300));
        let cases: [Vec<&str>; 3] = [
            vec![
                "/home/ttuser/.tenstorrent-venv/bin/python",
                "/home/ttuser/.local/bin/tt-smi",
                "-s",
            ],
            vec![long_venv.as_str(), "/usr/local/bin/tt-smi", "--snapshot"],
            vec![
                long_venv.as_str(),
                "/usr/local/bin/tt-smi",
                "-s",
                "-f",
                "/tmp/out.json",
            ],
        ];
        for (i, argv) in cases.iter().enumerate() {
            let td = tempfile::tempdir().unwrap();
            fake_pid_argv(td.path(), 4_100_041 + i as i32, "tt-smi", argv, &ALL_FOUR);
            assert!(
                argv.join(" ").chars().count() > 60,
                "case {i} is longer than the old cut"
            );
            assert_eq!(waffler_pairs(td.path()), vec![], "case {i}: {argv:?}");
        }
    }

    /// A long command line that is not a snapshot run still counts.
    #[test]
    fn a_long_workload_command_line_is_still_a_process_event() {
        let td = tempfile::tempdir().unwrap();
        let model = format!("--model=/models/{}", "m".repeat(200));
        fake_pid_argv(
            td.path(),
            4_100_051,
            "python3",
            &[
                "/home/ttuser/.tenstorrent-venv/bin/python",
                "-m",
                "vllm",
                "serve",
                &model,
            ],
            &["/dev/tenstorrent/1"],
        );
        assert_eq!(waffler_pairs(td.path()), vec![(4_100_051, 1)]);
    }

    fn driver(root: &Path, lists: &[(u32, &[i32])]) {
        for (n, pids) in lists {
            let d = root.join("driver/tenstorrent").join(n.to_string());
            fs::create_dir_all(&d).unwrap();
            let text: String = pids.iter().map(|p| format!("{p}\n")).collect();
            fs::write(d.join("pids"), text).unwrap();
        }
    }

    fn map_1_to_3() -> DeviceMap {
        let numbers: BTreeMap<u32, String> = [
            (0, "0000:01:00.0".to_string()),
            (1, "0000:04:00.0".to_string()),
        ]
        .into();
        DeviceMap::from_parts(
            &numbers,
            &[("0000:01:00.0".into(), 0), ("0000:04:00.0".into(), 3)],
        )
    }

    #[test]
    fn fd_links_and_driver_lists_give_mapped_devices() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        fake_pid(
            root,
            4_100_001,
            "vllm",
            &["/dev/tenstorrent/1", "/dev/null"],
        );
        fake_pid(root, 4_100_002, "bash", &["/dev/pts/0"]);
        // Only the driver knows this one holds device 0 (its fd links say
        // nothing, as for a process whose fds we could not read).
        fake_pid(root, 4_100_003, "worker", &[]);
        driver(root, &[(0, &[4_100_003]), (1, &[4_100_001])]);

        let mut pm = ProcessMonitor::with_root(root);
        pm.update_mapped(&map_1_to_3());
        assert!(pm.complete(), "the driver lists were read");
        let holders = pm.holders(&map_1_to_3()).holders;
        let got: Vec<(i64, Vec<usize>, String)> = holders
            .iter()
            .map(|h| (h.pid, h.devices.clone(), h.name.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (4_100_001, vec![3], "vllm".to_string()),
                (4_100_003, vec![0], "worker".to_string()),
            ]
        );
    }

    /// Past the process bound the scan stops, and counts every process it
    /// left unread.
    #[test]
    fn the_process_bound_stops_the_scan_and_counts_the_rest() {
        let td = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fake_pid(td.path(), 4_100_061 + i, "w", &["/dev/tenstorrent/0"]);
        }
        let mut pm = ProcessMonitor::with_root(td.path()).with_limits(Limits {
            processes: 2,
            ..Limits::default()
        });
        pm.update();
        assert_eq!(pm.holders(&DeviceMap::identity()).holders.len(), 2);
        assert_eq!(pm.counts().proc_truncated, 3);
    }

    /// The fd bound reaches the real scan: a process with more links than
    /// the bound is read up to it and counted.
    #[test]
    fn the_fd_bound_reaches_the_scan() {
        let td = tempfile::tempdir().unwrap();
        let links: Vec<String> = (0..6).map(|_| "/dev/null".to_string()).collect();
        let links: Vec<&str> = links.iter().map(String::as_str).collect();
        fake_pid(td.path(), 4_100_071, "many", &links);
        let mut pm = ProcessMonitor::with_root(td.path()).with_limits(Limits {
            fds_per_process: 4,
            ..Limits::default()
        });
        pm.update();
        assert_eq!(pm.counts().fd_truncated, 1);
    }

    #[test]
    fn without_driver_lists_the_answer_is_marked_incomplete() {
        let td = tempfile::tempdir().unwrap();
        fake_pid(td.path(), 4_100_011, "train", &["/dev/tenstorrent/0"]);
        let mut pm = ProcessMonitor::with_root(td.path());
        pm.update();
        assert!(!pm.complete());
        assert_eq!(
            pm.holders(&DeviceMap::identity()).holders[0].devices,
            vec![0]
        );
    }

    #[test]
    fn a_process_that_vanished_mid_scan_is_skipped_and_counted() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        fake_pid(root, 4_100_021, "vllm", &["/dev/tenstorrent/0"]);
        // Listed in /proc and by the driver, but its files are already gone.
        fs::create_dir_all(root.join("4100022")).unwrap();
        driver(root, &[(0, &[4_100_021, 4_100_022])]);
        let mut pm = ProcessMonitor::with_root(root);
        pm.update();
        let pids: Vec<i64> = pm
            .holders(&DeviceMap::identity())
            .holders
            .iter()
            .map(|h| h.pid)
            .collect();
        assert_eq!(pids, vec![4_100_021]);
        assert_eq!(pm.counts().vanished, 1);
    }

    #[test]
    fn a_refused_fd_directory_is_counted_and_the_driver_still_names_it() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        fake_pid(root, 4_100_031, "locked", &["/dev/tenstorrent/1"]);
        let fd = root.join("4100031/fd");
        fs::set_permissions(&fd, fs::Permissions::from_mode(0o000)).unwrap();
        driver(root, &[(1, &[4_100_031])]);
        let mut pm = ProcessMonitor::with_root(root);
        pm.update_mapped(&map_1_to_3());
        fs::set_permissions(&fd, fs::Permissions::from_mode(0o700)).unwrap();
        crate::workload::proc_access::clear_denied_for_test(4_100_031);
        let holders = pm.holders(&map_1_to_3()).holders;
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].devices, vec![3]);
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(pm.counts().denied, 1, "the refusal is counted");
        }
    }
}
