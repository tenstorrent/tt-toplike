// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Point-at-target collector: the design doc's `/watch <path>`,
//! `/watch pid <n>`, and `/wrap <command…>` commands all resolve to a
//! `WrapCollector` instance, registered by `Hivemind::add_watch_path` /
//! `add_watch_pid` / `add_wrap` respectively.
//!
//! - `WrapKind::File` and `WrapKind::Pid` are strictly read-only: a file tail
//!   (reusing `collectors::log_tail`'s tail idiom and its `event_from_log`
//!   classifier) and a `/proc/<pid>/fd` + cmdline scan (reusing
//!   `collectors::procfs::device_from_fd_target`), mirroring the patterns
//!   those two sibling collectors already established.
//! - `WrapKind::Command` is the one deliberate side effect in the whole
//!   hivemindsweeper subsystem: it spawns a user-supplied argv directly (no
//!   shell, no env injection) and kills the child on shutdown so nothing is
//!   ever orphaned. The UI confirmation gate for this lives in a later task
//!   (see the design doc's "`:wrap` confirmation" note) — this collector's
//!   job is only to run correctly once asked to.

use crate::workload::hivemind::classify::classify;
use crate::workload::hivemind::collector::{Collector, Tx};
use crate::workload::hivemind::collectors::log_tail::event_from_log;
use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// What a `WrapCollector` points at. See the module doc for the read-only
/// vs. side-effecting split between variants.
#[derive(Debug, Clone)]
pub enum WrapKind {
    /// Tail a host file into a dedicated lane (`/watch <path>`).
    File(PathBuf),
    /// Attach to a PID: read its `/proc/<pid>/fd` + cmdline, best-effort
    /// following any open log-like files (`/watch pid <n>`).
    Pid(i32),
    /// Spawn `argv` directly (no shell) and capture stdout+stderr as
    /// `Emission` events (`/wrap <command…>`).
    Command(Vec<String>),
}

/// Collector for one point-at-target: file tail, pid attach, or wrapped
/// subprocess. Construct with [`WrapCollector::new`]; the `kind` field
/// itself stays private so callers go through the constructor (matching
/// every sibling collector's `::new()` convention) — a descendant test
/// module can still build the struct literal directly, which is how the
/// brief's own test does it.
pub struct WrapCollector {
    kind: WrapKind,
}

impl WrapCollector {
    pub fn new(kind: WrapKind) -> Self {
        Self { kind }
    }
}

/// How long the file-tail loop sleeps after catching up to EOF before
/// retrying the read. Matches `collectors::log_tail`'s tail cadence.
const TAIL_IDLE_SLEEP: Duration = Duration::from_millis(300);

/// Tail one file from its live end, in the same idiom as
/// `collectors::log_tail`'s private `tail_file`: open, seek to EOF (no
/// history replay — only newly appended lines count), then loop reading
/// until `shutdown`. Reuses `log_tail::event_from_log` for the
/// line -> `SniffEvent` classification (kind `Log`, severity inferred from
/// ERROR/WARN tokens, source from `classify`) rather than re-implementing
/// it. `device` overrides the returned event's device column — `None` for
/// the plain `/watch <path>` case (host-attributed, like every other log
/// line); a pid-attach caller that already knows which device fd owns this
/// file could pass `Some(n)` (v1 always passes `None` — see `pid_attach`).
fn tail_file_into(
    origin: &str,
    path: &Path,
    proc_name: Option<&str>,
    device: Option<u8>,
    tx: &Tx,
    shutdown: &Arc<AtomicBool>,
) {
    let file = match std::fs::File::open(path) {
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
            Ok(0) => thread::sleep(TAIL_IDLE_SLEEP), // caught up: wait for more
            Ok(_) => {
                if let Some(mut ev) = event_from_log(origin, proc_name, &line) {
                    ev.device = device;
                    if tx.try_send(ev).is_err() {
                        // channel full: drop newest (engine counts drops)
                    }
                }
            }
            Err(_) => thread::sleep(TAIL_IDLE_SLEEP),
        }
    }
}

/// `WrapKind::File` entry point: tail the given path until shutdown.
fn run_file(path: PathBuf, tx: Tx, shutdown: Arc<AtomicBool>) {
    let origin = format!("wrap:{}", path.display());
    tail_file_into(&origin, &path, None, None, &tx, &shutdown);
}

/// Spawn one piped stdout/stderr reader thread: read lines until EOF (the
/// pipe closes when the child exits or is killed) or `shutdown`, emitting an
/// `Emission` `SniffEvent` per non-empty line. Mirrors
/// `collectors::log_tail::spawn_docker_logs_tail`'s reader-thread idiom.
fn spawn_pipe_reader<R: std::io::Read + Send + 'static>(
    pipe: R,
    origin: String,
    proc_name: String,
    tx: Tx,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF: pipe closed (child exited or was killed)
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let ev = SniffEvent {
                        ts: Instant::now(),
                        source: classify(Some(&proc_name), trimmed),
                        device: None,
                        severity: Severity::Info,
                        kind: EventKind::Emission,
                        text: trimmed.to_string(),
                        origin: origin.clone(),
                    };
                    if tx.try_send(ev).is_err() {
                        // channel full: drop newest (engine counts drops)
                    }
                }
                Err(_) => break,
            }
        }
    })
}

/// `WrapKind::Command` entry point: spawn `argv` directly (no shell — argv[0]
/// is passed straight to `Command::new`, the rest as `.args(..)`, exactly as
/// the caller supplied them) with stdout and stderr piped, read both on
/// dedicated helper threads into `Emission` events, and kill the child on
/// shutdown.
///
/// Shutdown sequence: the outer loop below polls `shutdown` (and the child's
/// own exit status) every 100ms rather than blocking on the child indefinitely,
/// so a `Handle::stop()` is noticed promptly even for a long-running child.
/// Once shutdown is observed (or the child already exited on its own), the
/// child is killed (a no-op if it already exited — `ESRCH` is swallowed) and
/// waited on; killing it closes the write end of both pipes, which unblocks
/// the reader threads' blocking `read_line` calls with an EOF, so they return
/// and get joined here rather than leaking as detached threads.
fn run_command(argv: Vec<String>, tx: Tx, shutdown: Arc<AtomicBool>) {
    // `Hivemind::add_wrap` already rejects an empty argv before a
    // `WrapCollector` is ever constructed, but a defensive check here means
    // this function is still safe if something ever constructs one directly.
    let Some(program) = argv.first() else {
        return;
    };
    let origin = format!("wrap:{program}");
    let proc_name = program.clone();

    let mut child = match Command::new(program)
        .args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return, // couldn't spawn (missing binary, no permission, ...)
    };

    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(spawn_pipe_reader(
            out,
            origin.clone(),
            proc_name.clone(),
            tx.clone(),
            shutdown.clone(),
        ));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(spawn_pipe_reader(
            err,
            origin.clone(),
            proc_name.clone(),
            tx.clone(),
            shutdown.clone(),
        ));
    }

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match child.try_wait() {
            Ok(Some(_status)) => break, // child exited on its own
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break, // can't determine status any more; fall through to cleanup
        }
    }

    // Kill (harmless if already exited) + wait so the child is fully reaped —
    // never left as a zombie or, worse, still running past `stop()`.
    let _ = child.kill();
    let _ = child.wait();
    for r in readers {
        let _ = r.join();
    }
}

/// PID-attach implementation, gated to Linux + the `linux-procfs` feature —
/// the same gating (and the same `procfs` crate) `collectors::procfs` uses,
/// for the same reason: unavailable elsewhere. See the no-op arm below for
/// every other target/feature combination.
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
mod pid_attach {
    use super::{tail_file_into, Tx};
    use crate::workload::hivemind::classify::classify;
    use crate::workload::hivemind::collectors::procfs::close_event_source;
    use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
    use procfs::process::{FDTarget, Process};
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    /// How often the attach loop re-reads `/proc/<pid>/fd`. Matches
    /// `collectors::procfs::ProcfsCollector`'s ~1s polling cadence.
    const POLL_INTERVAL: Duration = Duration::from_secs(1);

    /// Attach to `pid`: each poll, re-read its `/proc/<pid>/fd` table (via
    /// the already-established `procfs` crate, same as
    /// `collectors::procfs::ProcfsCollector`) and diff the set of open
    /// Tenstorrent device fds against the previous poll, emitting an `Fd`
    /// event on open/close — the "read /proc/<n>/fd" half of
    /// `/watch pid <n>`. The "best-effort follow its open log files" half:
    /// any other open regular file whose path looks log-like (see
    /// `looks_like_log_path`) gets its own tail thread the first time it's
    /// seen (reusing `tail_file_into`, the same idiom `WrapKind::File`
    /// uses), and is never re-spawned once tailing (deduped by path,
    /// mirroring `log_tail`'s docker-container dedup).
    ///
    /// Returns as soon as the pid disappears (`Process::new` starts failing —
    /// nothing left to attach to) or `shutdown` is set. Note that any file
    /// tail threads already spawned keep running until `shutdown` fires
    /// (they share the same flag) rather than being torn down the instant the
    /// pid exits — deliberately: the file may still be worth watching (e.g.
    /// still being appended by a sibling/child process), and there is no risk
    /// of hanging `stop()` since both this loop and every tail thread poll
    /// the identical `shutdown` flag.
    pub(super) fn run(pid: i32, tx: Tx, shutdown: Arc<AtomicBool>) {
        let origin = format!("wrap:pid:{pid}");
        let mut seen_devices: HashSet<u8> = HashSet::new();
        let mut tailing: HashSet<PathBuf> = HashSet::new();
        let mut tail_handles: Vec<JoinHandle<()>> = Vec::new();
        // `pid` is fixed for this attach loop's whole lifetime, so this cache
        // never holds more than the one entry — it's a `HashMap` (rather than
        // a bare `Option`) purely to match `collectors::procfs::open_cache`'s
        // shape and let both collectors share `close_event_source`. Populated
        // the moment an OPEN is emitted for a device fd; looked up (not
        // removed) when that fd later closes, so the close event reuses the
        // OPEN's classification even if `stat()`/`cmdline()` would now return
        // different or empty data (e.g. the process has gone zombie between
        // polls). Evicted only *after* a poll's closes are fully processed —
        // and only once this pid holds no device fd at all any more — so a
        // pid closing several device fds in the very same poll (the same
        // eviction-timing bug `collectors::procfs` already hit and fixed)
        // doesn't lose its cached classification partway through.
        let mut open_cache: HashMap<i32, (String, Source)> = HashMap::new();

        while !shutdown.load(Ordering::SeqCst) {
            let Ok(process) = Process::new(pid) else {
                break; // pid gone: nothing left to attach to
            };
            let proc_name = process
                .stat()
                .ok()
                .map(|s| s.comm)
                .unwrap_or_else(|| format!("pid{pid}"));
            // `comm` is frequently a generic interpreter name (`python3`,
            // `node`) that carries no classification signal on its own —
            // `cmdline` (e.g. `python3 -m vllm.entrypoints.openai.api_server`)
            // is where the actual framework/server identity lives. Same idiom
            // `workload::process_monitor` already uses for its own cmdline
            // read (`cmdline()` -> `Result<Vec<String>>`, joined with spaces).
            let cmdline = process
                .cmdline()
                .ok()
                .map(|v| v.join(" "))
                .unwrap_or_default();

            let mut current_devices: HashSet<u8> = HashSet::new();
            if let Ok(fds) = process.fd() {
                for fd in fds.flatten() {
                    let FDTarget::Path(path) = fd.target else {
                        continue; // socket/pipe/anon fd: nothing to attribute
                    };
                    if let Some(dev) =
                        crate::workload::hivemind::collectors::procfs::device_from_fd_target(
                            &path.to_string_lossy(),
                        )
                    {
                        current_devices.insert(dev);
                    } else if looks_like_log_path(&path)
                        && path.is_file()
                        && tailing.insert(path.clone())
                    {
                        let tx = tx.clone();
                        let sd = shutdown.clone();
                        let file_origin = format!("{origin}:{}", path.display());
                        let name = proc_name.clone();
                        tail_handles.push(thread::spawn(move || {
                            tail_file_into(&file_origin, &path, Some(&name), None, &tx, &sd);
                        }));
                    }
                }
            }

            for &dev in &current_devices {
                if seen_devices.insert(dev) {
                    let source = classify(Some(&proc_name), &cmdline);
                    // Cache this pid's classification so a later close (by
                    // which point `stat()`/`cmdline()` may no longer be
                    // trustworthy) reuses it verbatim — see `open_cache`'s
                    // doc comment above.
                    open_cache.insert(pid, (proc_name.clone(), source));
                    let _ = tx.try_send(SniffEvent {
                        ts: Instant::now(),
                        source,
                        device: Some(dev),
                        severity: Severity::Info,
                        kind: EventKind::Fd,
                        text: format!("{proc_name} (pid {pid}) opened /dev/tenstorrent/{dev}"),
                        origin: origin.clone(),
                    });
                }
            }
            // Devices that were open last poll but aren't any more: closed.
            // Delegated to the pure `close_events_and_eviction` helper below
            // — see its docs for why the cache eviction decision must be
            // acted on only *after* every close this poll has been produced.
            let (close_events, should_evict) = close_events_and_eviction(
                &seen_devices,
                &current_devices,
                &open_cache,
                pid,
                &origin,
            );
            for ev in close_events {
                let _ = tx.try_send(ev);
                // channel full: drop newest (engine counts drops)
            }
            seen_devices.retain(|dev| current_devices.contains(dev));
            if should_evict {
                open_cache.remove(&pid);
            }

            thread::sleep(POLL_INTERVAL);
        }

        for h in tail_handles {
            let _ = h.join();
        }
    }

    /// Pure close-event generation for one `/watch pid` poll: given the
    /// device indices this pid held open last poll (`seen_devices`), the
    /// ones still open now (`current_devices`), and the `(name, Source)`
    /// cached at OPEN time (`open_cache`, keyed by `pid` — see `run`'s
    /// `open_cache` doc comment), produce the CLOSE `SniffEvent`s for this
    /// poll, plus whether `open_cache`'s entry for `pid` should now be
    /// evicted.
    ///
    /// Mirrors `collectors::procfs::close_events_and_evictions` (see its docs
    /// for the full rationale, which applies identically here): a pid closing
    /// SEVERAL device fds in the very same poll must see the same cached
    /// classification for every one of those closes, so `open_cache` is only
    /// ever looked up here, never removed mid-loop — eviction is returned as
    /// a bool (true once `current_devices` is empty, i.e. this pid holds no
    /// device fd at all any more) for the caller to act on only after every
    /// close this poll has already been generated.
    ///
    /// Pure (no `/proc` access, no `&mut self`), so — like its `procfs`
    /// sibling — this poll-boundary interaction is unit-testable with
    /// synthetic `seen_devices`/`current_devices`/`open_cache` and no live
    /// process required.
    ///
    /// `pub(super)` (rather than private) purely so a sibling test module in
    /// this file can call it directly with synthetic data — mirroring
    /// `collectors::procfs::close_events_and_evictions`'s own `pub(super)`.
    pub(super) fn close_events_and_eviction(
        seen_devices: &HashSet<u8>,
        current_devices: &HashSet<u8>,
        open_cache: &HashMap<i32, (String, Source)>,
        pid: i32,
        origin: &str,
    ) -> (Vec<SniffEvent>, bool) {
        let mut events = Vec::new();
        for &dev in seen_devices {
            if current_devices.contains(&dev) {
                continue;
            }
            let (source, text) = close_event_source(open_cache.get(&pid), pid, dev);
            events.push(SniffEvent {
                ts: Instant::now(),
                source,
                device: Some(dev),
                severity: Severity::Info,
                kind: EventKind::Fd,
                text,
                origin: origin.to_string(),
            });
        }
        (events, current_devices.is_empty())
    }

    /// Heuristic for "looks like a log file" among a pid's other open
    /// regular files (this branch only runs once `device_from_fd_target`
    /// already returned `None`, so device nodes never reach here): a common
    /// log extension, or "log" anywhere in the path. Deliberately
    /// conservative — the goal is "don't spend a thread tailing every shared
    /// library or data file a process happens to have open", not exhaustive
    /// log detection.
    fn looks_like_log_path(path: &std::path::Path) -> bool {
        let s = path.to_string_lossy().to_lowercase();
        s.ends_with(".log") || s.ends_with(".txt") || s.contains("log")
    }
}

/// No-op stand-in for pid attach, used whenever the real `/proc`-backed
/// implementation can't be built (non-Linux hosts, or Linux builds with
/// `linux-procfs` disabled) — mirrors `collectors::procfs`'s own no-op arm.
/// `run()` returns immediately, emitting nothing: "Linux-gated; no-op/
/// graceful elsewhere" per the design doc.
#[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
mod pid_attach {
    pub(super) fn run(
        _pid: i32,
        _tx: crate::workload::hivemind::collector::Tx,
        _shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        // No-op: `/proc`-backed pid attach is unavailable on this
        // target/build. Nothing to poll, nothing to emit.
    }
}

impl Collector for WrapCollector {
    fn name(&self) -> &'static str {
        match &self.kind {
            WrapKind::File(_) => "wrap:file",
            WrapKind::Pid(_) => "wrap:pid",
            WrapKind::Command(_) => "wrap:command",
        }
    }

    fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>) {
        match &self.kind {
            WrapKind::File(path) => run_file(path.clone(), tx, shutdown),
            WrapKind::Pid(pid) => pid_attach::run(*pid, tx, shutdown),
            WrapKind::Command(argv) => run_command(argv.clone(), tx, shutdown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Note: `Tx` is intentionally not imported here (unlike the brief's
    // literal Step-1 snippet) — it's already in scope via `use super::*`
    // (wrap.rs's own top-level `use collector::{Collector, Tx}`), so a
    // second explicit import of the same name is dead weight that trips the
    // zero-warning build gate. The test's behavior is unchanged.
    use crate::workload::hivemind::collector::spawn;
    use crate::workload::hivemind::event::EventKind;
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    #[test]
    fn wrap_command_captures_stdout_as_emission() {
        let (tx, rx) = sync_channel::<crate::workload::hivemind::event::SniffEvent>(16);
        let c = WrapCollector {
            kind: WrapKind::Command(vec!["echo".into(), "emitting-cpp".into()]),
        };
        let h = spawn(Box::new(c), tx);
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(ev.kind, EventKind::Emission);
        assert!(ev.text.contains("emitting-cpp"));
        h.stop();
    }

    // --- Additional coverage beyond the brief's mandated test -------------

    use crate::workload::hivemind::event::Source;

    #[test]
    fn wrap_command_captures_stderr_as_emission_too() {
        // `cat` on a nonexistent path writes its error to stderr and exits
        // nonzero; asserting only kind/non-emptiness (not the exact message
        // text) keeps this robust across coreutils/locale differences.
        let (tx, rx) = sync_channel(16);
        let c = WrapCollector::new(WrapKind::Command(vec![
            "cat".into(),
            "/nonexistent-hivemind-wrap-test-path".into(),
        ]));
        let h = spawn(Box::new(c), tx);
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(ev.kind, EventKind::Emission);
        assert!(!ev.text.is_empty());
        h.stop();
    }

    #[test]
    fn wrap_command_bad_binary_is_graceful() {
        // Command::spawn fails (ENOENT) rather than panicking; nothing
        // should ever be emitted, and stop() must still complete promptly.
        let (tx, rx) = sync_channel(16);
        let c = WrapCollector::new(WrapKind::Command(vec![
            "/no/such/hivemind-wrap-test-binary".into(),
        ]));
        let h = spawn(Box::new(c), tx);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        h.stop();
    }

    #[test]
    fn wrap_file_tails_appended_lines() {
        use std::io::Write;
        let mut path = std::env::temp_dir();
        path.push(format!("hivemind-wrap-test-{}.log", std::process::id()));
        std::fs::write(&path, "").expect("create empty test file");

        let (tx, rx) = sync_channel(16);
        let c = WrapCollector::new(WrapKind::File(path.clone()));
        let h = spawn(Box::new(c), tx);
        // Give the tail thread a moment to open the file and seek to EOF
        // before appending — mirrors the "no history replay" contract.
        thread::sleep(Duration::from_millis(150));
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("reopen test file for append");
            writeln!(f, "ttnn.matmul dispatched").expect("append test line");
        }

        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(ev.kind, EventKind::Log);
        assert_eq!(ev.source, Source::Ttnn);

        h.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wrap_pid_nonexistent_is_graceful() {
        // Vanishingly unlikely to be a real pid; the real (linux-procfs) arm
        // should notice `Process::new` failing and return immediately, and
        // the no-op stub arm is already a no-op regardless — either way
        // nothing should be emitted and stop() must complete promptly.
        let (tx, rx) = sync_channel(16);
        let c = WrapCollector::new(WrapKind::Pid(i32::MAX - 1));
        let h = spawn(Box::new(c), tx);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        h.stop();
    }

    #[test]
    fn name_reflects_kind() {
        assert_eq!(
            WrapCollector::new(WrapKind::File(PathBuf::from("x"))).name(),
            "wrap:file"
        );
        assert_eq!(WrapCollector::new(WrapKind::Pid(1)).name(), "wrap:pid");
        assert_eq!(
            WrapCollector::new(WrapKind::Command(vec!["x".into()])).name(),
            "wrap:command"
        );
    }
}

/// Tests for `pid_attach`'s pure close-event/eviction helper. Gated
/// identically to the `pid_attach::real`-equivalent module itself (it's the
/// only target/feature combination where that code exists to test), mirroring
/// how `collectors::procfs` gates its own `mod real` unit tests.
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
#[cfg(test)]
mod pid_attach_tests {
    use super::pid_attach;
    use crate::workload::hivemind::event::Source;
    use std::collections::{HashMap, HashSet};

    /// Regression test mirroring `collectors::procfs`'s
    /// `same_poll_multi_device_close_keeps_cached_source_for_every_close`:
    /// a single `/watch pid` attach holds TWO device fds open, both closing
    /// in the SAME poll (`current_devices` is empty — every fd this pid held
    /// is now gone). Before this fix, the close path hardcoded
    /// `Source::Unknown` regardless of what the OPEN was classified as, so a
    /// `vLLM` open+close pair landed in two separate coalesced feed rows
    /// instead of folding into one (see `feed.rs`'s `coalesce_key`, keyed on
    /// `(source, device, pattern)`). Both closes here must instead see the
    /// cached `("python", Source::Vllm)` classification, and the pid's cache
    /// entry must be marked for eviction afterward (it holds no device fd in
    /// `current_devices` any more).
    #[test]
    fn pid_attach_close_reuses_cached_open_source_for_every_close_in_one_poll() {
        let pid = 4242;
        let seen_devices: HashSet<u8> = [0, 1].into_iter().collect();
        let current_devices: HashSet<u8> = HashSet::new(); // every fd closed this poll
        let mut open_cache: HashMap<i32, (String, Source)> = HashMap::new();
        open_cache.insert(pid, ("python".to_string(), Source::Vllm));

        let (events, should_evict) = pid_attach::close_events_and_eviction(
            &seen_devices,
            &current_devices,
            &open_cache,
            pid,
            "wrap:pid:4242",
        );

        assert_eq!(events.len(), 2);
        for ev in &events {
            assert_eq!(ev.source, Source::Vllm, "close event: {:?}", ev.text);
            assert!(
                ev.text
                    .starts_with("python (pid 4242) closed /dev/tenstorrent/"),
                "unexpected text: {}",
                ev.text
            );
        }
        let mut devices: Vec<Option<u8>> = events.iter().map(|e| e.device).collect();
        devices.sort();
        assert_eq!(devices, vec![Some(0), Some(1)]);

        assert!(
            should_evict,
            "pid holds no device fd in current_devices, so its cache entry should be evicted"
        );
    }

    /// A pid never observed opening (e.g. it already held the fd when the
    /// attach loop started) has nothing cached, so its close falls back to
    /// `Source::Unknown` — the same "genuinely unknown" fallback
    /// `close_event_source` documents, not the old "always Unknown" bug.
    #[test]
    fn pid_attach_close_falls_back_to_unknown_when_never_cached() {
        let pid = 777;
        let seen_devices: HashSet<u8> = [0].into_iter().collect();
        let current_devices: HashSet<u8> = HashSet::new();
        let open_cache: HashMap<i32, (String, Source)> = HashMap::new();

        let (events, should_evict) = pid_attach::close_events_and_eviction(
            &seen_devices,
            &current_devices,
            &open_cache,
            pid,
            "wrap:pid:777",
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, Source::Unknown);
        assert_eq!(events[0].text, "pid 777 closed /dev/tenstorrent/0");
        assert!(should_evict);
    }

    /// Staggered close across polls: two devices open, only one closes this
    /// poll — the pid still holds an fd for the other one in
    /// `current_devices`, so its cache entry must survive (not be evicted)
    /// for the still-pending close on the other device next poll.
    #[test]
    fn pid_attach_close_keeps_cache_when_pid_still_holds_a_device() {
        let pid = 55;
        let seen_devices: HashSet<u8> = [0, 1].into_iter().collect();
        let current_devices: HashSet<u8> = [1].into_iter().collect();
        let mut open_cache: HashMap<i32, (String, Source)> = HashMap::new();
        open_cache.insert(pid, ("python".to_string(), Source::Vllm));

        let (events, should_evict) = pid_attach::close_events_and_eviction(
            &seen_devices,
            &current_devices,
            &open_cache,
            pid,
            "wrap:pid:55",
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].device, Some(0));
        assert_eq!(events[0].source, Source::Vllm);
        assert!(
            !should_evict,
            "pid still holds device 1 open, cache must survive"
        );
    }
}
