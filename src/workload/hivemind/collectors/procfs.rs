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
//!
//! One exception to "never re-walks `/proc` itself": as a fallback when a
//! device-holding process's cmdline carries no TT-specific token (e.g. a
//! pytest-based test harness that pulls in `ttnn`/`tt_metal` only via Python
//! imports), this collector does its own single, targeted
//! `/proc/<pid>/maps` read to look for loaded TT shared libraries — see
//! `classify_from_so_paths` and `real::ProcfsCollector::classify_open`.

use std::collections::HashSet;

use crate::workload::hivemind::classify::classify;
use crate::workload::hivemind::event::Source;

/// Substrings that mark a loaded shared-object path as TT-related for the
/// purposes of the maps-based classification fallback (see
/// `classify_from_so_paths` below). Deliberately broader than
/// `classify::RULES`'s own needle list — this is a *prefilter* that narrows
/// a process's (potentially huge) set of mapped libraries down to a small,
/// plausibly-TT candidate set; `classify()` itself makes the final call on
/// what `Source` (if any) those candidates imply. A path can pass this
/// prefilter (e.g. `libdevice.so` in isolation) yet still classify as
/// `Unknown` once handed to `classify()` — that's fine, it just means the
/// caller falls through to the `Source::Workload` catch-all.
const TT_SO_NEEDLES: &[&str] = &[
    "tt-metal",
    "tt_metal",
    "ttnn",
    "_ttnn",
    "libdevice",
    "tt-mlir",
    "tt_lib",
    "metalium",
];

/// Cap on how many matched `.so` paths get folded into the string handed to
/// `classify()`, so a process with an unusually large library set can't
/// balloon allocation/CPU on every fallback classification.
const MAX_TT_SO_PATHS: usize = 40;

/// Parse the contents of `/proc/<pid>/maps` (or any string in that format)
/// into the distinct mapped file paths that look like shared objects
/// (`....so` or a versioned `....so.N`). Pure string parsing — no `/proc`
/// access here — so it's unit-testable against a literal sample of `maps`
/// content, independent of any live process.
///
/// A `/proc/pid/maps` line looks like:
/// `7f1234500000-7f1234700000 r-xp 00000000 08:01 1234  /path/to/lib.so`
/// The mapped path, when present, is the final whitespace-separated field;
/// anonymous mappings (`[heap]`, `[stack]`, `[vdso]`, or no pathname at all)
/// have no real path there, so lines whose last field lacks a `/` are
/// skipped rather than misread as a path.
pub fn parse_maps_so_paths(maps_content: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in maps_content.lines() {
        let Some(path) = line.split_whitespace().last() else {
            continue;
        };
        if !path.contains('/') {
            continue; // no path field on this line (anonymous mapping, etc.)
        }
        let is_versioned_so = path
            .rsplit_once(".so.")
            .map(|(_, suffix)| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or(false);
        if !(path.ends_with(".so") || is_versioned_so) {
            continue;
        }
        if seen.insert(path.to_string()) {
            out.push(path.to_string());
        }
    }
    out
}

/// Given a process name and the set of its mapped shared-object paths,
/// decide a `Source` by re-purposing `classify()`'s existing needle rules
/// against the TT-related subset of those paths (see `TT_SO_NEEDLES`).
///
/// This is the fallback for processes whose `argv` carries no TT token at
/// all — e.g. a pytest-based test harness
/// (`python -m pytest tests/test_model_forward.py`) that pulls in
/// `ttnn`/`tt_metal` purely via Python imports, never naming them on the
/// command line, but which has `.../third-party/tt-metal/build/lib/_ttnn.so`
/// mapped into its address space.
///
/// Pure (no `/proc` access, just the paths handed in), so it's
/// unit-testable with literal path strings — no live process required.
pub fn classify_from_so_paths(process_name: Option<&str>, so_paths: &[String]) -> Source {
    let matched: Vec<&str> = so_paths
        .iter()
        .map(String::as_str)
        .filter(|p| {
            let lower = p.to_lowercase();
            TT_SO_NEEDLES.iter().any(|needle| lower.contains(needle))
        })
        .take(MAX_TT_SO_PATHS)
        .collect();
    if matched.is_empty() {
        return Source::Unknown;
    }
    let joined = matched.join(" ");
    classify(process_name, &joined)
}

/// Build a concise, human-readable label for an OPEN event's text,
/// preferring a hint pulled from the full cmdline over the bare process
/// name (`comm`) when the cmdline adds identifying information. This
/// matters most for interpreter-hosted workloads — `comm` is just "python"
/// whether the process is a vLLM server or a pytest harness, but the
/// module/script argument is the detail an operator actually wants to see
/// in the feed (e.g. `python pytest test_model_forward.py` rather than the
/// bare `python`).
///
/// Pure string manipulation, no `/proc` access, so it's unit-testable with
/// literal name/cmdline strings.
pub fn concise_process_label(name: &str, cmdline: &str) -> String {
    /// A positional argument (no leading `-`) that either contains a path
    /// separator or ends in `.py` "looks like" a script/test file worth
    /// naming. Deliberately narrow: a bare `.`-check would also match
    /// option *values* that merely contain a dot (an IP address, a version
    /// string), mistaking them for a script.
    fn looks_like_script(tok: &str) -> bool {
        !tok.starts_with('-') && (tok.contains('/') || tok.ends_with(".py"))
    }

    /// Append `tok`'s basename to `parts` unless it's already present
    /// (e.g. the module name and the trailing script happen to coincide).
    fn push_unique(parts: &mut Vec<String>, tok: &str) {
        let base = tok.rsplit('/').next().unwrap_or(tok).to_string();
        if !parts.contains(&base) {
            parts.push(base);
        }
    }

    let tokens: Vec<&str> = cmdline.split_whitespace().collect();
    let Some(&first) = tokens.first() else {
        return name.to_string();
    };
    let interpreter = first.rsplit('/').next().unwrap_or(first);
    let mut parts = vec![interpreter.to_string()];

    match tokens.iter().position(|&t| t == "-m") {
        // `-m <module>`: the module name is the identifying detail, kept
        // verbatim (dotted module paths like `vllm.entrypoints.openai` are
        // informative in full, unlike a filesystem path).
        Some(pos) => {
            if let Some(&module) = tokens.get(pos + 1) {
                push_unique(&mut parts, module);
            }
        }
        // No `-m`: the first positional argument is usually the script
        // itself, e.g. `python train.py --epochs 10`.
        None => {
            if let Some(&second) = tokens.get(1) {
                if looks_like_script(second) {
                    push_unique(&mut parts, second);
                }
            }
        }
    }
    // Either way, a trailing script/test-file argument (e.g. the test file
    // in `python -m pytest tests/test_model_forward.py`) is usually the
    // most identifying detail left, so consider it too.
    if let Some(&last) = tokens.last() {
        if looks_like_script(last) {
            push_unique(&mut parts, last);
        }
    }

    if parts.len() <= 1 {
        // Nothing more specific than the bare interpreter name was found in
        // the cmdline: prefer `name` (comm) verbatim, which may already
        // carry more identity than the interpreter basename alone (e.g. a
        // renamed binary).
        return if name.is_empty() {
            interpreter.to_string()
        } else {
            name.to_string()
        };
    }
    parts.join(" ")
}

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
    cached: Option<&(String, Source)>,
    pid: i32,
    dev: u8,
) -> (Source, String) {
    match cached {
        Some((name, source)) => (
            *source,
            format!("{name} (pid {pid}) closed /dev/tenstorrent/{dev}"),
        ),
        None => (
            Source::Unknown,
            format!("pid {pid} closed /dev/tenstorrent/{dev}"),
        ),
    }
}

#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
mod real {
    use super::{
        classify_from_so_paths, close_event_source, concise_process_label, parse_maps_so_paths,
    };
    use crate::workload::hivemind::classify::classify;
    use crate::workload::hivemind::collector::{Collector, Tx};
    use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
    use crate::workload::process_monitor::ProcessMonitor;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Throttle cadence for the persistent device-holder heartbeat (see
    /// `holding_events` below): a process that keeps a `/dev/tenstorrent/*`
    /// fd open across many polls would otherwise go silent after its
    /// one-time OPEN event, fading its board cell back to cold even though
    /// it's still actively holding the device. Emitting on every ~1s poll
    /// would be far too chatty (the whole point of the open/close diffing
    /// above is to stay quiet in steady state), so this is deliberately
    /// coarser than the poll interval.
    const HOLDING_INTERVAL: Duration = Duration::from_secs(3);

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
        /// Last time a `holding` heartbeat batch was emitted (see
        /// `holding_events`), throttling that heartbeat to roughly once
        /// every `HOLDING_INTERVAL` rather than every ~1s poll.
        last_holding_emit: Instant,
    }

    impl ProcfsCollector {
        pub fn new() -> Self {
            Self {
                monitor: ProcessMonitor::new(),
                seen: HashSet::new(),
                open_cache: HashMap::new(),
                // Start the clock at construction so the very first
                // heartbeat batch fires after one full HOLDING_INTERVAL, not
                // immediately alongside every device's initial OPEN event.
                last_holding_emit: Instant::now(),
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

            let mut events = Vec::new();
            // Newly opened: in `current` but not `seen`. Classify via the
            // cmdline -> loaded-libraries -> Workload fallback chain (see
            // `Self::classify_open`) and cache the FINAL resolved source so
            // the matching close (once the process may be long gone) can
            // reuse it verbatim.
            for (&(pid, dev), (name, cmdline)) in &current {
                if !self.seen.contains(&(pid, dev)) {
                    let source = Self::classify_open(pid, name, cmdline);
                    let label = concise_process_label(name, cmdline);
                    self.open_cache.insert(pid, (label.clone(), source));
                    events.push(SniffEvent {
                        ts: Instant::now(),
                        source,
                        device: Some(dev),
                        severity: Severity::Info,
                        kind: EventKind::Fd,
                        text: format!("{label} (pid {pid}) opened /dev/tenstorrent/{dev}"),
                        origin: "procfs".into(),
                    });
                }
            }
            // Newly closed: in `seen` but not `current` any more. Delegated to
            // the pure `close_events_and_evictions` helper below — see its
            // docs for why eviction must happen only *after* every close in
            // this poll has been generated.
            let (close_ev, evict) =
                close_events_and_evictions(&self.seen, &current_keys, &self.open_cache);
            events.extend(close_ev);
            for pid in evict {
                self.open_cache.remove(&pid);
            }

            // Persistent-holder heartbeat: every pid still holding a device
            // fd should already be in `open_cache` from the "newly opened"
            // loop above (the very first poll treats every current holder as
            // newly-opened, since `self.seen` starts empty). Classify any
            // stragglers defensively so a holder is never left unlabeled —
            // classifying only the uncached ones (not re-reading
            // `/proc/<pid>/maps` for pids already cached) keeps this cheap.
            for &(pid, dev) in &current_keys {
                if let std::collections::hash_map::Entry::Vacant(e) = self.open_cache.entry(pid) {
                    if let Some((name, cmdline)) = current.get(&(pid, dev)) {
                        let source = Self::classify_open(pid, name, cmdline);
                        let label = concise_process_label(name, cmdline);
                        e.insert((label, source));
                    }
                }
            }

            let throttle_fired = self.last_holding_emit.elapsed() >= HOLDING_INTERVAL;
            if throttle_fired {
                self.last_holding_emit = Instant::now();
            }
            events.extend(holding_events(
                &current_keys,
                &self.open_cache,
                throttle_fired,
            ));

            self.seen = current_keys;
            events
        }

        /// Classify a newly-opened device-holding pid via a three-stage
        /// fallback chain:
        ///
        /// 1. **cmdline** — `classify(Some(name), cmdline)`, as before. Most
        ///    processes (`tt-smi`, `vllm`, anything invoking `ttnn`/
        ///    `tt_metal` on the command line) resolve here.
        /// 2. **loaded libraries** — only reached when (1) is `Unknown`.
        ///    Reads `/proc/<pid>/maps` (a single, targeted read — not a
        ///    process-wide `/proc` walk) and re-classifies from the paths of
        ///    its mapped TT-related shared objects (`classify_from_so_paths`).
        ///    This is what identifies a process like
        ///    `python -m pytest tests/test_model_forward.py`: its `argv`
        ///    carries no `ttnn`/`tt_metal` token, but it has
        ///    `.../third-party/tt-metal/build/lib/_ttnn.so` mapped in,
        ///    because the test imports the framework rather than naming it
        ///    on the command line.
        /// 3. **`Source::Workload`** — reached only when both (1) and (2)
        ///    come back `Unknown`. The pid is, unconditionally, a confirmed
        ///    `/dev/tenstorrent/*` holder (that's the only reason this
        ///    method is being called at all), so it's tagged as a generic
        ///    workload rather than left as `Unknown`.
        ///
        /// The `/proc/<pid>/maps` read can fail (permission denied, or the
        /// process has already exited) — treated as "no match" rather than
        /// an error, falling straight through to `Source::Workload`.
        fn classify_open(pid: i32, name: &str, cmdline: &str) -> Source {
            let cmdline_source = classify(Some(name), cmdline);
            if cmdline_source != Source::Unknown {
                return cmdline_source;
            }
            let maps_source = std::fs::read_to_string(format!("/proc/{pid}/maps"))
                .ok()
                .map(|maps_content| {
                    let so_paths = parse_maps_so_paths(&maps_content);
                    classify_from_so_paths(Some(name), &so_paths)
                })
                .unwrap_or(Source::Unknown);
            if maps_source != Source::Unknown {
                return maps_source;
            }
            Source::Workload
        }
    }

    /// Pure close-event generation for one poll: given the `(pid, dev)` pairs
    /// held open last poll (`seen`), the ones still open now (`current_keys`),
    /// and the `open_cache` recorded at OPEN time, produce the CLOSE
    /// `SniffEvent`s for this poll, plus the set of pids whose `open_cache`
    /// entry should now be evicted.
    ///
    /// Eviction is returned rather than performed inline for a specific
    /// reason: a pid that closes *several* devices in the very same poll
    /// (e.g. a `tt-smi -s` snapshot on a multi-chip box exiting while it
    /// still held every `/dev/tenstorrent/*` node) shows up as multiple
    /// `(pid, dev)` entries in `seen`, all processed in this one loop. If the
    /// cache entry were removed as soon as the *first* of those closes was
    /// handled, every subsequent close for that same pid in the same poll
    /// would miss the cache and collapse to `Source::Unknown` — exactly the
    /// "unknown" churn this cache exists to eliminate. Looking the entry up
    /// without removing it lets every close in the poll see the same cached
    /// classification: a pid with several devices open must keep its cached
    /// classification for *all* of its closes this poll, not just the first
    /// one. Only after the whole loop has generated every close event is a
    /// pid's cache entry safe to drop — and only if `current_keys` shows it
    /// holds no device fd at all any more (a pid still present there is a
    /// staggered close across polls — one device closed now, another still
    /// open next poll — and must keep its cached classification for later).
    ///
    /// Pure (no `/proc` access, no `&mut self`), so this poll-boundary
    /// interaction is unit-testable with a synthetic `seen`/`current_keys`/
    /// `open_cache` — no live process or `ProcessMonitor` required.
    ///
    /// `pub(super)` (rather than private) purely so the `tests` module —
    /// which lives as a sibling of `mod real` in this file, not inside it —
    /// can call it directly with synthetic data.
    pub(super) fn close_events_and_evictions(
        seen: &HashSet<(i32, u8)>,
        current_keys: &HashSet<(i32, u8)>,
        open_cache: &HashMap<i32, (String, Source)>,
    ) -> (Vec<SniffEvent>, HashSet<i32>) {
        let active_pids: HashSet<i32> = current_keys.iter().map(|&(pid, _)| pid).collect();

        let mut events = Vec::new();
        let mut closed_pids: HashSet<i32> = HashSet::new();
        for &(pid, dev) in seen {
            if !current_keys.contains(&(pid, dev)) {
                let (source, text) = close_event_source(open_cache.get(&pid), pid, dev);
                events.push(SniffEvent {
                    ts: Instant::now(),
                    source,
                    device: Some(dev),
                    severity: Severity::Info,
                    kind: EventKind::Fd,
                    text,
                    origin: "procfs".into(),
                });
                closed_pids.insert(pid);
            }
        }

        let evict: HashSet<i32> = closed_pids
            .into_iter()
            .filter(|pid| !active_pids.contains(pid))
            .collect();
        (events, evict)
    }

    /// Pure holding-heartbeat generation for one poll: given the `(pid,
    /// dev)` pairs currently held (`current_keys`), the cached `(label,
    /// Source)` recorded at OPEN time for each pid (`open_cache`), and
    /// whether the throttle has fired this poll, produce one Trace-severity
    /// `Process` `SniffEvent` per currently-held pair — but only when the
    /// throttle fired; otherwise an empty `Vec` (see `HOLDING_INTERVAL`'s
    /// docs for why this must be throttled rather than emitted every poll).
    ///
    /// `Severity::Trace` is deliberate: it is the LOWEST severity, so a
    /// steady holder's heartbeat row shows in the feed at the default floor
    /// (`Trace`, i.e. `severity >= Trace` is true) — making the holder
    /// visible by default — yet is the first thing dropped when the operator
    /// raises the severity floor with `s`. Each heartbeat also bumps the
    /// board cell's heat (the engine's `poll()` bumps heat for every event
    /// regardless of severity), keeping a steady holder's cell warm/live
    /// instead of decaying to cold the moment its one-time OPEN event ages
    /// out.
    ///
    /// A pid with no `open_cache` entry is silently skipped rather than
    /// falling back to `Source::Unknown` — by the time this runs in `poll()`
    /// every currently-held pid has already been classified (see the
    /// straggler-classification loop there), so a missing entry here would
    /// only mean the caller didn't uphold that precondition; there's nothing
    /// useful to say about an unlabeled holder that the open/close events
    /// don't already say better.
    ///
    /// Pure (no `/proc` access, no `&mut self`), so it's unit-testable with
    /// synthetic `current_keys`/`open_cache` and an explicit throttle flag —
    /// no real `Instant` timing required.
    pub(super) fn holding_events(
        current_keys: &HashSet<(i32, u8)>,
        open_cache: &HashMap<i32, (String, Source)>,
        throttle_fired: bool,
    ) -> Vec<SniffEvent> {
        if !throttle_fired {
            return Vec::new();
        }
        let mut events = Vec::new();
        for &(pid, dev) in current_keys {
            if let Some((label, source)) = open_cache.get(&pid) {
                events.push(SniffEvent {
                    ts: Instant::now(),
                    source: *source,
                    device: Some(dev),
                    severity: Severity::Trace,
                    kind: EventKind::Process,
                    text: format!("{label} holding /dev/tenstorrent/{dev}"),
                    origin: "procfs".into(),
                });
            }
        }
        events
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
                        // channel full: drop newest (uncounted; only ring eviction bumps dropped())
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
    use crate::workload::hivemind::classify::classify;
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

    /// Regression test for the eviction-timing bug: a single pid (a
    /// `tt-smi -s` snapshot on a 4-chip box, here reduced to 2 devices)
    /// holds TWO device fds open, `(pid, 0)` and `(pid, 1)`, both closing in
    /// the SAME poll (`current` is empty — the process has exited). Before
    /// the fix, processing `(pid, 0)`'s close would evict the `open_cache`
    /// entry immediately (since the pid holds no fd in `current` either
    /// way), so the close for `(pid, 1)` — processed later in the same
    /// iteration over `seen` — would miss the cache and fall back to
    /// `Source::Unknown`. Both closes must instead see the cached
    /// `("tt-smi", Source::TtSmi)` classification, and the pid must be
    /// evicted from `open_cache` afterward (not left to leak).
    #[test]
    fn same_poll_multi_device_close_keeps_cached_source_for_every_close() {
        use std::collections::{HashMap, HashSet};

        let pid = 4242;
        let seen: HashSet<(i32, u8)> = [(pid, 0), (pid, 1)].into_iter().collect();
        let current_keys: HashSet<(i32, u8)> = HashSet::new(); // process has exited entirely
        let mut open_cache: HashMap<i32, (String, Source)> = HashMap::new();
        open_cache.insert(pid, ("tt-smi".to_string(), Source::TtSmi));

        let (events, evict) = real::close_events_and_evictions(&seen, &current_keys, &open_cache);

        // Both closes generated, both still classified via the cache.
        assert_eq!(events.len(), 2);
        for ev in &events {
            assert_eq!(ev.source, Source::TtSmi, "close event: {:?}", ev.text);
            assert!(
                ev.text
                    .starts_with("tt-smi (pid 4242) closed /dev/tenstorrent/"),
                "unexpected text: {}",
                ev.text
            );
        }
        let mut devices: Vec<Option<u8>> = events.iter().map(|e| e.device).collect();
        devices.sort();
        assert_eq!(devices, vec![Some(0), Some(1)]);

        // Pid holds no device fd in `current_keys`, so it must be marked for
        // eviction after all its closes were generated.
        assert_eq!(evict, [pid].into_iter().collect());
    }

    #[test]
    fn parse_maps_so_paths_extracts_deduped_shared_objects_only() {
        // Includes: a repeated libc mapping (dedup), an anonymous mapping
        // with no path field, a `[heap]`/`[stack]` pseudo-path, and a
        // TT-related extension module nested under a `third-party` build
        // tree -- the exact shape a real `/proc/<pid>/maps` takes for a
        // Python process that has loaded a compiled `ttnn` extension.
        let maps = "\
7f0000000000-7f0000010000 r-xp 00000000 08:01 1 /usr/lib/x86_64-linux-gnu/libc.so.6
7f0000010000-7f0000020000 r-xp 00000000 08:01 1 /usr/lib/x86_64-linux-gnu/libc.so.6
7f0000020000-7f0000030000 rw-p 00000000 00:00 0
7f0000030000-7f0000040000 rw-p 00000000 00:00 0 [heap]
7f0000040000-7f0000050000 r-xp 00000000 08:01 3 /opt/venv/third-party/tt-metal/build/lib/_ttnn.so
7f0000050000-7f0000060000 rw-p 00000000 00:00 0 [stack]
";
        assert_eq!(
            parse_maps_so_paths(maps),
            vec![
                "/usr/lib/x86_64-linux-gnu/libc.so.6".to_string(),
                "/opt/venv/third-party/tt-metal/build/lib/_ttnn.so".to_string(),
            ]
        );
    }

    #[test]
    fn classify_from_so_paths_recognizes_tt_metal_build_tree() {
        let so_paths = vec![
            "/usr/lib/x86_64-linux-gnu/libc.so.6".to_string(),
            "/opt/venv/lib/python3.10/site-packages/pyarrow/libarrow.so.1600".to_string(),
            "/opt/venv/third-party/tt-metal/build/lib/_ttnn.so".to_string(),
        ];
        assert_eq!(
            classify_from_so_paths(Some("python"), &so_paths),
            Source::Ttnn
        );
    }

    #[test]
    fn classify_from_so_paths_ignores_non_tt_libraries() {
        let so_paths = vec![
            "/opt/venv/lib/python3.10/site-packages/pyarrow/libarrow.so.1600".to_string(),
            "/usr/lib/x86_64-linux-gnu/libc.so.6".to_string(),
        ];
        assert_eq!(
            classify_from_so_paths(Some("python"), &so_paths),
            Source::Unknown
        );
    }

    #[test]
    fn concise_process_label_prefers_module_and_script_over_bare_comm() {
        assert_eq!(
            concise_process_label(
                "python",
                ".venv-qwythos/bin/python -m pytest tests/test_model_forward.py",
            ),
            "python pytest test_model_forward.py"
        );
    }

    #[test]
    fn concise_process_label_falls_back_to_name_when_cmdline_adds_nothing() {
        assert_eq!(concise_process_label("tt-smi", "tt-smi -s"), "tt-smi");
    }

    /// Two `(pid, dev)` pairs are currently held, both cached under
    /// `Source::TtMetal` labels, and the throttle has fired: exactly two
    /// Trace/Process holding events come back, one per held pair, tagged
    /// with the cached source and text naming the device.
    #[test]
    fn holding_events_emits_one_trace_event_per_held_pair_when_throttle_fires() {
        use std::collections::{HashMap, HashSet};

        let current_keys: HashSet<(i32, u8)> = [(100, 0), (200, 1)].into_iter().collect();
        let mut open_cache: HashMap<i32, (String, Source)> = HashMap::new();
        open_cache.insert(
            100,
            (
                "python pytest test_model_forward.py".to_string(),
                Source::TtMetal,
            ),
        );
        open_cache.insert(
            200,
            (
                "python pytest test_model_forward.py".to_string(),
                Source::TtMetal,
            ),
        );

        let events = real::holding_events(&current_keys, &open_cache, true);

        assert_eq!(events.len(), 2);
        for ev in &events {
            assert_eq!(ev.source, Source::TtMetal);
            assert_eq!(
                ev.severity,
                crate::workload::hivemind::event::Severity::Trace
            );
            assert_eq!(
                ev.kind,
                crate::workload::hivemind::event::EventKind::Process
            );
            assert!(
                ev.text.contains("holding /dev/tenstorrent/"),
                "unexpected text: {}",
                ev.text
            );
        }
        let mut devices: Vec<Option<u8>> = events.iter().map(|e| e.device).collect();
        devices.sort();
        assert_eq!(devices, vec![Some(0), Some(1)]);
    }

    /// Same held pairs and cache, but the throttle has NOT fired this poll:
    /// zero holding events, so a holder doesn't get re-announced on every
    /// ~1s poll.
    #[test]
    fn holding_events_emits_nothing_when_throttle_has_not_fired() {
        use std::collections::{HashMap, HashSet};

        let current_keys: HashSet<(i32, u8)> = [(100, 0), (200, 1)].into_iter().collect();
        let mut open_cache: HashMap<i32, (String, Source)> = HashMap::new();
        open_cache.insert(
            100,
            (
                "python pytest test_model_forward.py".to_string(),
                Source::TtMetal,
            ),
        );
        open_cache.insert(
            200,
            (
                "python pytest test_model_forward.py".to_string(),
                Source::TtMetal,
            ),
        );

        let events = real::holding_events(&current_keys, &open_cache, false);

        assert!(events.is_empty());
    }

    /// The maps-based classification fallback, exercised end to end (still
    /// purely, with a literal `maps`-format string rather than a live
    /// process): a `python -m pytest tests/test_model_forward.py` cmdline
    /// classifies as `Unknown` on its own (no `ttnn`/`tt_metal` token in
    /// `argv`), but the same process's loaded `_ttnn.so` -- as it would be
    /// read from `/proc/<pid>/maps` -- resolves it to `Source::Ttnn`. This
    /// is the exact case the maps fallback exists for: a model workload run
    /// through a test harness, where the framework is used via Python
    /// imports rather than named on the command line.
    #[test]
    fn maps_fallback_classifies_pytest_model_workload_as_ttnn() {
        let cmdline = ".venv-qwythos/bin/python -m pytest tests/test_model_forward.py";
        assert_eq!(classify(Some("python"), cmdline), Source::Unknown);

        let maps = "\
7f0000040000-7f0000050000 r-xp 00000000 08:01 3 /opt/venv/third-party/tt-metal/build/lib/_ttnn.so
";
        let so_paths = parse_maps_so_paths(maps);
        assert_eq!(
            classify_from_so_paths(Some("python"), &so_paths),
            Source::Ttnn
        );
    }
}
