// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! tt-metal Inspector log-file collector: tails Inspector log files written
//! to `generated/inspector/` (relative to cwd) and
//! `$TT_METAL_HOME/generated/inspector/`, surfacing each non-empty line as a
//! `SniffEvent`. Read-only — this collector only ever opens and reads
//! existing log files already written by tt-metal's Inspector feature to
//! disk. It never subscribes to the Inspector RPC/websocket interface, never
//! reads device debug buffers, and never sets `TT_METAL_INSPECTOR` (or any
//! other env var) to turn Inspector logging on — if the feature isn't
//! already enabled and writing files, there's simply nothing here to tail.
//!
//! Reuses the established file-tail idiom from
//! [`super::log_tail::tail_file`] (open → seek to EOF → loop reading
//! appended lines, sleeping on `Ok(0)`/error, polling `shutdown`) and the
//! discover-on-a-poll shape from
//! [`super::log_tail::LogTailCollector::run`]/[`super::cache_watch`] so that
//! Inspector log files created after this collector starts (e.g. a run
//! kicked off later) still get picked up.
//!
//! FUTURE: RPC subscribe

use crate::workload::hivemind::collector::{Collector, Tx};
use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Parse one raw tt-metal Inspector log line into a `SniffEvent`, or `None`
/// for a blank line. Lines that mention "compile" or "kernel" (e.g. kernel
/// compilation lifecycle lines) are tagged `EventKind::Compile`; everything
/// else gets the generic `EventKind::Inspector`. `device` is left `None` for
/// v1 — Inspector's log line formats seen so far don't carry a device index
/// in an easily/robustly-parseable form, and guessing wrong would misattribute
/// activity to the wrong chip. A future revision can add device extraction
/// once a concrete log format needing it is in hand.
pub fn event_from_inspector_line(line: &str) -> Option<SniffEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let low = trimmed.to_lowercase();
    let kind = if low.contains("compile") || low.contains("kernel") {
        EventKind::Compile
    } else {
        EventKind::Inspector
    };
    Some(SniffEvent {
        ts: Instant::now(),
        source: Source::Inspector,
        device: None,
        severity: Severity::Info,
        kind,
        text: trimmed.to_string(),
        origin: "inspector".to_string(),
    })
}

/// Tail one Inspector log file from its live end: open, seek to EOF (history
/// written before this collector started isn't replayed), then loop reading
/// newly appended lines. Mirrors `log_tail::tail_file`'s idiom exactly — a
/// plain (blocking) file read returns `Ok(0)` at EOF rather than
/// `WouldBlock`, so we sleep and retry rather than treating that as
/// terminal.
fn tail_file(path: &Path, tx: Tx, shutdown: Arc<AtomicBool>) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return, // missing/unreadable: nothing to tail
    };
    let mut reader = BufReader::new(file);
    if reader.get_mut().seek(SeekFrom::End(0)).is_err() {
        return;
    }
    let mut line = String::new();
    while !shutdown.load(Ordering::SeqCst) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => thread::sleep(Duration::from_millis(300)), // caught up: wait for more
            Ok(_) => {
                if let Some(ev) = event_from_inspector_line(&line) {
                    if tx.try_send(ev).is_err() {
                        // channel full: drop newest (engine counts drops)
                    }
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(300)),
        }
    }
}

/// Candidate Inspector log directories: `generated/inspector` relative to
/// cwd (tt-metal's default when run from a repo checkout), plus
/// `$TT_METAL_HOME/generated/inspector` when that env var is set (mirrors
/// `cache_watch::resolve_roots`'s `TT_METAL_HOME`-relative convention).
fn resolve_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("generated/inspector")];
    if let Ok(home) = std::env::var("TT_METAL_HOME") {
        roots.push(PathBuf::from(home).join("generated/inspector"));
    }
    roots
}

/// List regular files directly under `dir` (non-recursive — Inspector's
/// default layout is a flat directory of log files, not a nested tree like
/// the compile cache `cache_watch` walks). A missing/unreadable directory
/// yields an empty list rather than an error.
fn list_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                out.push(path);
            }
        }
    }
    out
}

/// How often the Inspector directories are re-scanned for newly-created log
/// files (e.g. a new tt-metal run started after this collector began
/// tailing). Matches `log_tail::DOCKER_RESCAN_INTERVAL`'s cadence.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Discovers tt-metal Inspector log files under the default directories and
/// tails each one from its live end, re-scanning on an interval so files
/// created after startup are still picked up. Read-only: only reads
/// existing log files already on disk.
///
/// FUTURE: RPC subscribe
pub struct InspectorCollector {
    roots: Vec<PathBuf>,
}

impl InspectorCollector {
    pub fn new() -> Self {
        Self {
            roots: resolve_roots(),
        }
    }
}

impl Default for InspectorCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for InspectorCollector {
    fn name(&self) -> &'static str {
        "inspector"
    }

    fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>) {
        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        let mut tailing: HashSet<PathBuf> = HashSet::new();
        // Scan immediately on the first loop iteration rather than waiting
        // out the first interval (same trick as `log_tail::LogTailCollector::run`).
        let mut last_scan = Instant::now()
            .checked_sub(RESCAN_INTERVAL)
            .unwrap_or_else(Instant::now);

        while !shutdown.load(Ordering::SeqCst) {
            if last_scan.elapsed() >= RESCAN_INTERVAL {
                for root in &self.roots {
                    for path in list_files(root) {
                        if tailing.insert(path.clone()) {
                            let tx = tx.clone();
                            let sd = shutdown.clone();
                            handles.push(thread::spawn(move || tail_file(&path, tx, sd)));
                        }
                    }
                }
                last_scan = Instant::now();
            }
            thread::sleep(Duration::from_millis(200));
        }

        // Shutdown: join every tail thread so `stop()` doesn't return early
        // with orphaned threads still running (each thread's own loop exits
        // promptly once it re-checks `shutdown`).
        for h in handles {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::event::{EventKind, Source};
    #[test]
    fn compile_finished_maps_to_compile_kind() {
        let ev = event_from_inspector_line(
            "program 12 kernel brisc compile finished").unwrap();
        assert_eq!(ev.source, Source::Inspector);
        assert_eq!(ev.kind, EventKind::Compile);
    }

    /// The other branch of the kind decision — a line with no "compile" or
    /// "kernel" substring should get the generic `Inspector` kind rather
    /// than being misclassified as a compile event.
    #[test]
    fn non_compile_line_maps_to_inspector_kind() {
        let ev = event_from_inspector_line("program 12 launched").unwrap();
        assert_eq!(ev.source, Source::Inspector);
        assert_eq!(ev.kind, EventKind::Inspector);
    }

    #[test]
    fn blank_line_is_none() {
        assert!(event_from_inspector_line("   ").is_none());
        assert!(event_from_inspector_line("").is_none());
    }
}
