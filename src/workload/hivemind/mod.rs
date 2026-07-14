// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Hivemindsweeper: opt-in passive activity sniffer. See
//! docs/superpowers/specs/2026-07-14-hivemindsweeper-design.md.

pub mod classify;
pub mod collector;
pub mod collectors;
pub mod event;
pub mod grid;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
pub use event::{EventKind, Severity, SniffEvent, Source};

use collector::{CollectorStatus, Handle};
use collectors::{
    cache_watch::CacheWatchCollector, inspector::InspectorCollector, kmsg::KmsgCollector,
    log_tail::LogTailCollector, procfs::ProcfsCollector,
    wrap::{WrapCollector, WrapKind},
};
pub use grid::HeatGrid;

/// Capacity of the bounded channel collectors push `SniffEvent`s into. Sized
/// generously above the ring capacity so a burst doesn't need to spill into
/// the channel's own backpressure before `poll()` gets a chance to drain it;
/// actual overflow accounting happens at the ring (`ingest`), not here.
const CHANNEL_CAP: usize = 1024;

/// Bounded event history + drop accounting, plus the live collector engine:
/// heat grid, collector handles, and the channel collectors publish into.
pub struct Hivemind {
    events: VecDeque<SniffEvent>,
    dropped: u64,

    /// Decaying (source × device/host) activity heat, fed by `poll()`.
    grid: HeatGrid,
    /// Sending half handed to every spawned collector (cloned per-collector).
    /// `None` when not running; dropping it (on `stop`) lets any collector
    /// still mid-send observe a closed channel rather than blocking forever.
    tx: Option<SyncSender<SniffEvent>>,
    /// Receiving half drained by `poll()`. `None` when not running.
    rx: Option<Receiver<SniffEvent>>,
    /// Join/shutdown handles for every spawned collector, stopped in `stop()`.
    handles: Vec<Handle>,
    running: bool,
}

impl Hivemind {
    pub const RING_CAP: usize = 4096;

    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(256),
            dropped: 0,
            grid: HeatGrid::new(),
            tx: None,
            rx: None,
            handles: Vec::new(),
            running: false,
        }
    }

    /// Append an event, evicting the oldest (and counting it) at capacity.
    pub fn push_for_test(&mut self, ev: SniffEvent) {
        self.ingest(ev);
    }

    fn ingest(&mut self, ev: SniffEvent) {
        if self.events.len() >= Self::RING_CAP {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back(ev);
    }

    pub fn events(&self) -> &VecDeque<SniffEvent> {
        &self.events
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Build the channel and spawn every auto-detected collector (kmsg,
    /// cache_watch, log_tail, procfs, inspector). Idempotent: a no-op if
    /// already running, so callers can call `start()` unconditionally (e.g.
    /// on every toggle keypress) without accidentally spawning duplicate
    /// collector threads.
    pub fn start(&mut self) {
        if self.running {
            return;
        }
        let (tx, rx) = sync_channel::<SniffEvent>(CHANNEL_CAP);
        self.tx = Some(tx.clone());
        self.rx = Some(rx);

        self.handles.push(collector::spawn(Box::new(KmsgCollector), tx.clone()));
        self.handles
            .push(collector::spawn(Box::new(CacheWatchCollector::new()), tx.clone()));
        self.handles
            .push(collector::spawn(Box::new(LogTailCollector::new()), tx.clone()));
        self.handles
            .push(collector::spawn(Box::new(ProcfsCollector::new()), tx.clone()));
        self.handles
            .push(collector::spawn(Box::new(InspectorCollector::new()), tx));

        self.running = true;
    }

    /// Stop all collectors and tear down the channel. Event history and drop
    /// counters are kept — only the live engine state is reset — so a
    /// subsequent `start()` resumes with the same ring/heat context.
    pub fn stop(&mut self) {
        for h in self.handles.drain(..) {
            h.stop();
        }
        self.tx = None;
        self.rx = None;
        self.running = false;
    }

    /// Drain every `SniffEvent` currently queued on the channel into the ring
    /// (bumping the heat grid for each), then decay the grid once. Infallible
    /// and non-blocking: a `None` receiver (not running) or an empty channel
    /// both just fall through to the decay step.
    pub fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            let mut drained = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                drained.push(ev);
            }
            for ev in drained {
                self.grid.bump(ev.source, ev.device);
                self.ingest(ev);
            }
        }
        self.grid.decay();
    }

    /// Register a host log file to tail (`/watch <path>`). Ensures the
    /// engine is running — `start()` is documented as idempotent/safe to
    /// call unconditionally — then spawns a `WrapCollector { kind:
    /// WrapKind::File(path) }` on the engine's live channel, pushing its
    /// handle alongside the auto-detected collectors' so a later `stop()`
    /// tears it down too.
    pub fn add_watch_path(&mut self, path: PathBuf) {
        self.start();
        let tx = self.tx.clone().expect("start() guarantees tx is Some");
        self.handles
            .push(collector::spawn(Box::new(WrapCollector::new(WrapKind::File(path))), tx));
    }

    /// Register a PID whose device-fd activity (and best-effort open log
    /// files) should be watched (`/watch pid <n>`). Same running/spawn
    /// contract as `add_watch_path`.
    pub fn add_watch_pid(&mut self, pid: i32) {
        self.start();
        let tx = self.tx.clone().expect("start() guarantees tx is Some");
        self.handles
            .push(collector::spawn(Box::new(WrapCollector::new(WrapKind::Pid(pid))), tx));
    }

    /// Spawn a wrapped-command collector (`/wrap <command…>`, i.e.
    /// `tt-toplike sniff -- <argv>`). Same running/spawn contract as
    /// `add_watch_path`. The only validation failure this returns is an
    /// empty `argv` (nothing to spawn) — `collector::spawn` itself can't
    /// fail (a panicking collector is caught and marked, not propagated as
    /// an `Err` here).
    ///
    /// **Side effect**: unlike `add_watch_path`/`add_watch_pid`, this runs
    /// the caller-supplied command directly (no shell, no env injection —
    /// see `collectors::wrap::run_command`). The UI confirmation gate for
    /// this lives in a later task; this method assumes the caller has
    /// already obtained consent.
    pub fn add_wrap(&mut self, argv: Vec<String>) -> Result<(), String> {
        if argv.is_empty() {
            return Err("add_wrap: empty command".to_string());
        }
        self.start();
        let tx = self.tx.clone().expect("start() guarantees tx is Some");
        self.handles.push(collector::spawn(
            Box::new(WrapCollector::new(WrapKind::Command(argv))),
            tx,
        ));
        Ok(())
    }

    pub fn grid(&self) -> &HeatGrid {
        &self.grid
    }

    /// Current status of every spawned collector, in spawn order.
    pub fn statuses(&self) -> Vec<(&'static str, CollectorStatus)> {
        self.handles
            .iter()
            .map(|h| (h.name, h.status.lock().unwrap().clone()))
            .collect()
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Test-only hook: bump the heat grid directly, mirroring what `poll()`
    /// does for a real drained event, without needing a live collector.
    #[cfg(test)]
    pub fn bump_grid_for_test(&mut self, source: Source, device: Option<u8>) {
        self.grid.bump(source, device);
    }

    /// Test-only hook: hand back a cloned sender into the engine's live
    /// channel so a test can inject events exactly as a real collector would
    /// (i.e. exercising `poll()`'s actual drain path), rather than only the
    /// `push_for_test`/`bump_grid_for_test` shortcuts. Panics if called
    /// before `start()` — that mirrors a real collector never existing
    /// without a running engine.
    #[cfg(test)]
    pub fn test_sender(&self) -> SyncSender<SniffEvent> {
        self.tx.clone().expect("test_sender called before start()")
    }
}

impl Default for Hivemind {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ev() -> SniffEvent {
        SniffEvent {
            ts: Instant::now(),
            source: Source::Host,
            device: None,
            severity: Severity::Info,
            kind: EventKind::Log,
            text: "x".into(),
            origin: "test".into(),
        }
    }

    #[test]
    fn ring_evicts_oldest_and_counts_drops() {
        let mut h = Hivemind::new();
        for _ in 0..(Hivemind::RING_CAP + 5) {
            h.push_for_test(ev());
        }
        assert_eq!(h.events().len(), Hivemind::RING_CAP);
        assert_eq!(h.dropped(), 5);
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use crate::workload::hivemind::grid::Column;
    use std::time::{Duration, Instant};

    fn ev(source: Source, device: Option<u8>) -> SniffEvent {
        SniffEvent {
            ts: Instant::now(),
            source,
            device,
            severity: Severity::Info,
            kind: EventKind::Compile,
            text: "brisc.o".into(),
            origin: "cache".into(),
        }
    }

    /// Brief's sample scenario: start/stop lifecycle plus the manual
    /// injection shortcuts (`push_for_test` + `bump_grid_for_test`). These
    /// shortcuts bypass the real channel entirely, so this test alone would
    /// NOT catch a broken `poll()` drain loop — see
    /// `poll_drains_real_channel_into_ring_and_heat` below for that.
    #[test]
    fn lifecycle_start_stop_and_manual_injection() {
        let mut h = Hivemind::new();
        h.start();
        assert!(h.is_running());

        h.push_for_test(ev(Source::TtMetal, Some(0)));
        h.bump_grid_for_test(Source::TtMetal, Some(0));
        assert!(h.grid().heat(Source::TtMetal, Column::Device(0)) > 0.0);

        h.stop();
        assert!(!h.is_running());
    }

    /// Strengthened per Task 9 review: genuinely exercises `poll()`'s real
    /// drain path. Obtains a live `SyncSender` clone into the engine's own
    /// channel (`test_sender()` — exactly what `collector::spawn` hands to a
    /// real collector), injects events through it, then calls `poll()` and
    /// checks (a) the events landed in `events()` and (b) `grid().heat(..)`
    /// rose for their (source, device) pair. This is the only test in the
    /// suite that would fail if `poll()`'s `try_recv` loop, its `ingest`
    /// call, or its `grid.bump` call were removed or short-circuited.
    ///
    /// `start()` also spawns the five real auto-collectors (kmsg,
    /// cache_watch, log_tail, procfs, inspector) on this same channel; they
    /// are all designed to emit nothing in a clean dev/CI sandbox (no
    /// `/dev/kmsg` access, no compile-cache artifacts churning, no docker
    /// containers, no `/proc` device fds, no Inspector logs), so in practice
    /// they contribute zero extra events here. Rather than assert an exact
    /// ring length (which *would* be broken by a stray real event on a box
    /// where one of those preconditions doesn't hold), the assertions below
    /// only check for an increase of at least our two injected events and
    /// heat on the exact (source, column) pairs we injected — both robust to
    /// incidental real-collector activity.
    #[test]
    fn poll_drains_real_channel_into_ring_and_heat() {
        let mut h = Hivemind::new();
        h.start();
        assert!(h.is_running());

        let tx = h.test_sender();
        let before = h.events().len();
        tx.try_send(ev(Source::TtMetal, Some(2)))
            .expect("bounded channel has room for a single test event");
        tx.try_send(ev(Source::Ttnn, None))
            .expect("bounded channel has room for a single test event");

        // A single poll() should already observe both sends (try_send
        // returns only after the value is queued for the receiver), but
        // retry briefly to stay robust under scheduler contention rather
        // than assuming perfect timing.
        let mut ring_len = h.events().len();
        for _ in 0..50 {
            h.poll();
            ring_len = h.events().len();
            if ring_len >= before + 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ring_len >= before + 2,
            "poll() should drain both injected events into the ring (before={before}, after={ring_len})"
        );
        assert!(h.grid().heat(Source::TtMetal, Column::Device(2)) > 0.0);
        assert!(h.grid().heat(Source::Ttnn, Column::Host) > 0.0);

        h.stop();
        assert!(!h.is_running());
    }

    /// Task 10 integration: `add_wrap` on a fresh (not-yet-started) engine
    /// auto-starts it (per `add_watch_path`'s doc comment — `start()` is
    /// idempotent/safe to call unconditionally) and actually spawns a live
    /// `WrapCollector` on the engine's channel, whose output reaches
    /// `poll()`'s ring exactly like every other collector's. Also checks the
    /// one validation `add_wrap` performs itself: an empty argv is rejected
    /// before anything starts or spawns.
    #[test]
    fn add_wrap_auto_starts_and_spawns_collector() {
        let mut h = Hivemind::new();
        assert!(!h.is_running());

        assert!(h.add_wrap(vec![]).is_err());
        assert!(!h.is_running(), "an empty argv must not start the engine");

        h.add_wrap(vec!["echo".into(), "wrapped-hello".into()])
            .expect("non-empty argv spawns fine");
        assert!(h.is_running());
        // 5 auto-detected collectors (kmsg, cache_watch, log_tail, procfs,
        // inspector) plus the one wrap collector just spawned.
        assert_eq!(h.statuses().len(), 6);

        let mut found = false;
        for _ in 0..100 {
            h.poll();
            if h.events().iter().any(|e| {
                e.kind == EventKind::Emission && e.text.contains("wrapped-hello")
            }) {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            found,
            "expected the wrapped echo's stdout to land in the ring as an Emission"
        );

        h.stop();
        assert!(!h.is_running());
    }

    /// Task 10 integration: `add_watch_path`/`add_watch_pid` follow the same
    /// auto-start + spawn-and-register contract as `add_wrap` (checked above
    /// end-to-end); this just confirms both also auto-start a fresh engine
    /// and register their collector handle, without duplicating the full
    /// poll-for-output dance (the File/Pid tailing behavior itself is
    /// covered by `collectors::wrap`'s own unit tests).
    #[test]
    fn add_watch_path_and_pid_auto_start_and_register() {
        let mut h = Hivemind::new();
        assert!(!h.is_running());
        h.add_watch_path(PathBuf::from("/nonexistent-hivemind-watch-path"));
        assert!(h.is_running());
        assert_eq!(h.statuses().len(), 6);

        h.add_watch_pid(i32::MAX - 1);
        assert_eq!(h.statuses().len(), 7);

        h.stop();
        assert!(!h.is_running());
    }
}
