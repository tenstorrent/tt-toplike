// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Process + file-descriptor collector: surfaces which processes are actively
//! opening/closing Tenstorrent device nodes, by polling
//! `workload::process_monitor::ProcessMonitor` (already-established `/proc`
//! scanning — this collector never re-walks `/proc` itself). Read-only.
//!
//! `ProcessMonitor` (and the `procfs` crate it wraps) is only buildable on
//! Linux with the `linux-procfs` feature (see `Cargo.toml` / `workload/mod.rs`),
//! so the real collector is gated the same way. A no-op stand-in is provided
//! for every other target/feature combination so callers — in particular
//! Task 9's collector-registration engine — can construct a `ProcfsCollector`
//! unconditionally without their own cfg-gating.

/// Parse a `/proc/<pid>/fd/<n>` symlink target (as reported by
/// `procfs::process::FDTarget::Path`) into the Tenstorrent device index it
/// points at, e.g. `/dev/tenstorrent/3` -> `Some(3)`. Any other target —
/// `/dev/null`, a different device node, a regular file, a socket, a malformed
/// or out-of-range index — maps to `None` rather than guessing.
///
/// Pure and OS-independent (plain string parsing), so it is compiled and
/// usable unconditionally — in particular `collectors::log_tail` reuses it to
/// recognize a docker container "pinned" to one specific device (as opposed to
/// the whole `/dev/tenstorrent` class) from that container's inspected
/// `HostConfig.Devices[].PathOnHost`, on every target/feature combination.
pub fn device_from_fd_target(link: &str) -> Option<u8> {
    link.strip_prefix("/dev/tenstorrent/")?.parse::<u8>().ok()
}

#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
mod real {
    use crate::workload::hivemind::classify::classify;
    use crate::workload::hivemind::collector::{Collector, Tx};
    use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent};
    use crate::workload::process_monitor::ProcessMonitor;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Polls `ProcessMonitor::update()` about once a second and diffs the set
    /// of `(pid, device index)` pairs it reports against the previous poll,
    /// emitting an `Fd` `SniffEvent` for each newly-opened or newly-closed
    /// pairing. Diffing on change (rather than re-reporting the full live set
    /// every tick) keeps steady state quiet, matching `kmsg`/`cache_watch`'s
    /// "only surface what changed" idiom.
    pub struct ProcfsCollector {
        monitor: ProcessMonitor,
        /// Last poll's `(pid, device index)` pairs the monitor reported open,
        /// carried across polls purely to diff against — never read outside
        /// `run()`.
        seen: HashSet<(i32, u8)>,
    }

    impl ProcfsCollector {
        pub fn new() -> Self {
            Self {
                monitor: ProcessMonitor::new(),
                seen: HashSet::new(),
            }
        }

        /// One poll: refresh the monitor, compute the current `(pid, device,
        /// name)` set, diff against `self.seen`, update `self.seen`, and
        /// return the open/close events to emit. Kept separate from `run()`
        /// so the diffing logic itself doesn't require a live channel/thread
        /// to exercise (Task 9 drives `run()` end-to-end; this method is the
        /// natural seam for a future unit test against a fake process list).
        fn poll(&mut self) -> Vec<SniffEvent> {
            self.monitor.update();

            // (pid, device) -> process name, for every device the monitor
            // currently reports as in-use.
            let mut current: HashMap<(i32, u8), String> = HashMap::new();
            for idx in self.monitor.device_indices() {
                let Ok(dev) = u8::try_from(idx) else {
                    continue; // device index somehow > u8::MAX: skip rather than panic
                };
                if let Some(procs) = self.monitor.get_processes_for_device(idx) {
                    for p in procs {
                        current.insert((p.pid, dev), p.name.clone());
                    }
                }
            }
            let current_keys: HashSet<(i32, u8)> = current.keys().copied().collect();

            let mut events = Vec::new();
            // Newly opened: in `current` but not `seen`.
            for (&(pid, dev), name) in &current {
                if !self.seen.contains(&(pid, dev)) {
                    events.push(SniffEvent {
                        ts: Instant::now(),
                        source: classify(Some(name), ""),
                        device: Some(dev),
                        severity: Severity::Info,
                        kind: EventKind::Fd,
                        text: format!("{name} (pid {pid}) opened /dev/tenstorrent/{dev}"),
                        origin: "procfs".into(),
                    });
                }
            }
            // Newly closed: in `seen` but not `current` any more. Name is
            // unknown at this point (the process may have exited), so the
            // event is attributed generically rather than guessing a stale name.
            for &(pid, dev) in &self.seen {
                if !current_keys.contains(&(pid, dev)) {
                    events.push(SniffEvent {
                        ts: Instant::now(),
                        source: crate::workload::hivemind::event::Source::Unknown,
                        device: Some(dev),
                        severity: Severity::Info,
                        kind: EventKind::Fd,
                        text: format!("pid {pid} closed /dev/tenstorrent/{dev}"),
                        origin: "procfs".into(),
                    });
                }
            }

            self.seen = current_keys;
            events
        }
    }

    impl Default for ProcfsCollector {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Collector for ProcfsCollector {
        fn name(&self) -> &'static str {
            "procfs"
        }

        fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>) {
            while !shutdown.load(Ordering::SeqCst) {
                for ev in self.poll() {
                    if tx.try_send(ev).is_err() {
                        // channel full: drop newest (engine counts drops)
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
pub use real::ProcfsCollector;

/// No-op stand-in used whenever the real, `/proc`-backed collector can't be
/// built (non-Linux hosts, or Linux builds with `linux-procfs` disabled).
/// Implements `Collector` but `run()` returns immediately, emitting nothing —
/// this lets Task 9's engine construct and register a `ProcfsCollector`
/// unconditionally, with no cfg-gating of its own, on every target.
#[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
pub struct ProcfsCollector;

#[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
impl ProcfsCollector {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
impl Default for ProcfsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
impl crate::workload::hivemind::collector::Collector for ProcfsCollector {
    fn name(&self) -> &'static str {
        "procfs"
    }

    fn run(
        &mut self,
        _tx: crate::workload::hivemind::collector::Tx,
        _shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        // No-op: `/proc`-backed device-fd scanning is unavailable on this
        // target/build. Nothing to poll, nothing to emit.
    }
}

#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_device_fd_target() {
        assert_eq!(device_from_fd_target("/dev/tenstorrent/3"), Some(3));
        assert_eq!(device_from_fd_target("/dev/null"), None);
    }
}
