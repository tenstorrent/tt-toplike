// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Compile-cache / generated-kernel-artifact collector (polling). Watches
//! `tt-metal`/`tt-dit` compile-cache and `generated/` directories for newly
//! written kernel build artifacts (`.o`/`.so`/`.hex`/`.elf`) and surfaces each
//! new one as a `Compile` SniffEvent. No filesystem-notification crate is
//! used — this is a plain, bounded-depth directory scan against a `seen` set,
//! run on an interval. Read-only: only directory listings and file names are
//! inspected, never file contents or device state.

use crate::workload::hivemind::collector::{Collector, Tx};
use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Maximum recursion depth for the directory walk, to bound worst-case work
/// and guarantee termination even in unexpectedly deep trees. Symlinks are
/// never followed (see `walk`), so this is also our defense against symlink
/// cycles.
const MAX_DEPTH: usize = 6;

/// Kernel build artifact extensions we consider "compile output."
const ARTIFACT_EXTS: [&str; 4] = ["o", "so", "hex", "elf"];

/// Walk `roots` recursively (bounded depth, no symlink following), find
/// kernel build artifacts not already present in `seen`, add them to `seen`,
/// and return a `Compile` SniffEvent for each newly-seen artifact.
///
/// Only the paths that match a known artifact extension are ever inserted
/// into `seen` — directories and non-artifact files are traversed but not
/// tracked, so `seen`'s growth is bounded by the number of distinct
/// artifacts that have ever existed on disk, not by total directory churn.
pub fn scan_new_artifacts(roots: &[PathBuf], seen: &mut HashSet<PathBuf>) -> Vec<SniffEvent> {
    let mut out = Vec::new();
    for root in roots {
        walk(root, 0, seen, &mut out);
    }
    out
}

fn walk(dir: &Path, depth: usize, seen: &mut HashSet<PathBuf>, out: &mut Vec<SniffEvent>) {
    if depth > MAX_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // missing/unreadable root: nothing to report
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Never follow symlinks: symlink_metadata reports the link itself,
        // so a symlinked directory is skipped rather than recursed into
        // (avoids symlink cycles without needing a visited-inode set).
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk(&path, depth + 1, seen, out);
            continue;
        }
        if !is_artifact(&path) {
            continue;
        }
        if seen.contains(&path) {
            continue;
        }
        seen.insert(path.clone());
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(SniffEvent {
            ts: Instant::now(),
            source: Source::TtMetal,
            device: device_col_from_path(&path),
            severity: Severity::Info,
            kind: EventKind::Compile,
            text: filename,
            origin: "cache".into(),
        });
    }
}

fn is_artifact(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ARTIFACT_EXTS.contains(&ext))
}

/// Best-effort device index extraction from a path segment of the form
/// `device_<N>`. A bare board-name segment (e.g. `p150`) carries no numeric
/// index, so it maps to `None` rather than guessing.
pub fn device_col_from_path(p: &Path) -> Option<u8> {
    for component in p.components() {
        let seg = component.as_os_str().to_str()?;
        if let Some(rest) = seg.strip_prefix("device_") {
            if let Ok(n) = rest.parse::<u8>() {
                return Some(n);
            }
        }
    }
    None
}

/// Resolves candidate compile-cache/generated-artifact roots from the
/// environment (falling back to `cwd` when `TT_METAL_HOME` is unset) and
/// polls them once per second for newly-written kernel artifacts.
pub struct CacheWatchCollector {
    roots: Vec<PathBuf>,
}

impl CacheWatchCollector {
    pub fn new() -> Self {
        Self {
            roots: resolve_roots(),
        }
    }
}

impl Default for CacheWatchCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the list of directories to poll. Explicit cache-dir env vars are
/// taken as-is; the remaining candidates are conventional subdirectory names
/// resolved relative to `TT_METAL_HOME` (or `cwd` if that's unset). Missing
/// directories are kept in the list — `walk` treats an unreadable root as
/// simply having nothing to report, so no filesystem probing happens here.
fn resolve_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(p) = std::env::var("TT_METAL_CACHE") {
        roots.push(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("TT_DIT_CACHE_DIR") {
        roots.push(PathBuf::from(p));
    }

    let base = std::env::var("TT_METAL_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    for sub in ["tt_metal_cache", "tt_dit_cache", "generated"] {
        roots.push(base.join(sub));
    }

    roots
}

impl Collector for CacheWatchCollector {
    fn name(&self) -> &'static str {
        "cache_watch"
    }

    fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>) {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        while !shutdown.load(Ordering::SeqCst) {
            for ev in scan_new_artifacts(&self.roots, &mut seen) {
                if tx.try_send(ev).is_err() {
                    // channel full: drop newest (engine counts drops)
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn reports_only_newly_seen_artifacts() {
        let dir = std::env::temp_dir().join(format!("hm-cache-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let f1 = dir.join("brisc.o");
        fs::write(&f1, b"x").unwrap();

        let mut seen = HashSet::new();
        let roots = vec![dir.clone()];
        let first = scan_new_artifacts(&roots, &mut seen);
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].kind,
            crate::workload::hivemind::event::EventKind::Compile
        );

        // Second scan: nothing new.
        let second = scan_new_artifacts(&roots, &mut seen);
        assert!(second.is_empty());

        // Add another; only it is reported.
        fs::write(dir.join("ncrisc.o"), b"y").unwrap();
        let third = scan_new_artifacts(&roots, &mut seen);
        assert_eq!(third.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_device_index_segment() {
        use std::path::PathBuf;
        let p = PathBuf::from("/x/cache_llama/device_2/brisc.o");
        assert_eq!(device_col_from_path(&p), Some(2));
        let p2 = PathBuf::from("/x/cache_llama/p150/brisc.o");
        assert_eq!(device_col_from_path(&p2), None);
    }
}
