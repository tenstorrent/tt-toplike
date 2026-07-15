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

/// Decide the `(Source, text)` for a CLOSE event given whatever was cached
/// from the matching OPEN (the process's name and the `Source` it was
/// classified as at that time), or `None` if this pid was never observed
/// opening the device in the first place (e.g. it was already holding the fd
/// when this collector started polling). Reusing the open's classification
/// — rather than reclassifying from scratch — matters because by the time a
/// CLOSE is noticed the process may already be gone, so `comm`/cmdline are no
/// longer readable; falling back to `Source::Unknown` only in the genuinely
/// unknown case (rather than always, as the old hardcoded close path did)
/// lets a downstream coalescer fold an open+close pair into one row keyed by
/// source.
///
/// Pure and OS-independent (no `/proc` access), so — like
/// `device_from_fd_target` above — it's compiled and unit-testable
/// regardless of the `linux-procfs` feature gate.
pub fn close_event_source(
    cached: Option<&(String, crate::workload::hivemind::event::Source)>,
    pid: i32,
    dev: u8,
) -> (crate::workload::hivemind::event::Source, String) {
    match cached {
        Some((name, source)) => (
            *source,
            format!("{name} (pid {pid}) closed /dev/tenstorrent/{dev}"),
        ),
        None => (
            crate::workload::hivemind::event::Source::Unknown,
            format!("pid {pid} closed /dev/tenstorrent/{dev}"),
        ),
    }
}

#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
mod real {
    use super::close_event_source;
    use crate::workload::hivemind::classify::classify;
    use crate::workload::hivemind::collector::{Collector, Tx};
    use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
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
        /// `pid -> (name, classified Source)` recorded the moment an OPEN
        /// event is emitted for that pid, so a later CLOSE for the same pid
        /// (by which point the process may already be gone, leaving `comm`/
        /// cmdline unreadable) classifies identically instead of collapsing
        /// to `Unknown`. Keyed by pid alone (not `(pid, dev)`) since a
        /// process's classification doesn't vary per device; entries are
        /// removed once that pid holds no more device fds (see `poll()`) so
        /// this can't grow unbounded.
        open_cache: HashMap<i32, (String, Source)>,
    }

    impl ProcfsCollector {
        pub fn new() -> Self {
            Self {
                monitor: ProcessMonitor::new(),
                seen: HashSet::new(),
                open_cache: HashMap::new(),
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

            // (pid, device) -> (process name, full cmdline), for every device
            // the monitor currently reports as in-use. `ProcessInfo` already
            // carries both (populated by `ProcessMonitor::update()`'s own
            // `/proc/<pid>/cmdline` read), so no second `/proc` scan is
            // needed here just to get classification signal.
            let mut current: HashMap<(i32, u8), (String, String)> = HashMap::new();
            for idx in self.monitor.device_indices() {
                let Ok(dev) = u8::try_from(idx) else {
                    continue; // device index somehow > u8::MAX: skip rather than panic
                };
                if let Some(procs) = self.monitor.get_processes_for_device(idx) {
                    for p in procs {
                        current.insert((p.pid, dev), (p.name.clone(), p.cmdline.clone()));
                    }
                }
            }
            let current_keys: HashSet<(i32, u8)> = current.keys().copied().collect();
            // Which pids still hold *some* device fd open this poll — used
            // below to decide when an `open_cache` entry can be dropped.
            let active_pids: HashSet<i32> = current_keys.iter().map(|&(pid, _)| pid).collect();

            let mut events = Vec::new();
            // Newly opened: in `current` but not `seen`. Classify from BOTH
            // `comm` and the full cmdline — `comm` alone is frequently a bare
            // interpreter name (`python`) that carries no signal, while the
            // cmdline (e.g. `tt-smi -s`, `python -m vllm...`) is where the
            // actual identity lives. Cache the result so the matching close
            // (once the process may be long gone) can reuse it verbatim.
            for (&(pid, dev), (name, cmdline)) in &current {
                if !self.seen.contains(&(pid, dev)) {
                    let source = classify(Some(name), cmdline);
                    self.open_cache.insert(pid, (name.clone(), source));
                    events.push(SniffEvent {
                        ts: Instant::now(),
                        source,
                        device: Some(dev),
                        severity: Severity::Info,
                        kind: EventKind::Fd,
                        text: format!("{name} (pid {pid}) opened /dev/tenstorrent/{dev}"),
                        origin: "procfs".into(),
                    });
                }
            }
            // Newly closed: in `seen` but not `current` any more. The process
            // may already be gone by now, so reuse whatever was cached at
            // open time (see `close_event_source`) instead of hardcoding
            // `Unknown` for every close.
            for &(pid, dev) in &self.seen {
                if !current_keys.contains(&(pid, dev)) {
                    let (source, text) = close_event_source(self.open_cache.get(&pid), pid, dev);
                    events.push(SniffEvent {
                        ts: Instant::now(),
                        source,
                        device: Some(dev),
                        severity: Severity::Info,
                        kind: EventKind::Fd,
                        text,
                        origin: "procfs".into(),
                    });
                    // Only forget this pid once it holds no more device fds
                    // at all — a pid with several devices open must keep its
                    // cached classification for the remaining closes, not
                    // just the first one.
                    if !active_pids.contains(&pid) {
                        self.open_cache.remove(&pid);
                    }
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
    use crate::workload::hivemind::event::Source;

    #[test]
    fn parses_device_fd_target() {
        assert_eq!(device_from_fd_target("/dev/tenstorrent/3"), Some(3));
        assert_eq!(device_from_fd_target("/dev/null"), None);
    }

    #[test]
    fn close_reuses_cached_open_source() {
        let cached = Some(("tt-smi".to_string(), Source::TtSmi));
        let (source, text) = close_event_source(cached.as_ref(), 4242, 0);
        assert_eq!(source, Source::TtSmi);
        assert_eq!(text, "tt-smi (pid 4242) closed /dev/tenstorrent/0");
    }

    #[test]
    fn close_falls_back_to_unknown_when_pid_was_never_cached() {
        let (source, text) = close_event_source(None, 4242, 0);
        assert_eq!(source, Source::Unknown);
        assert_eq!(text, "pid 4242 closed /dev/tenstorrent/0");
    }
}
