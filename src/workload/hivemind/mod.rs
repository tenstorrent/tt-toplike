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

    /// Register a host log file to tail. Stubbed until Task 10 wires a
    /// dedicated wrap/watch collector capable of accepting targets after
    /// `start()`; for now this only records intent and is a documented no-op.
    // wired in Task 10
    pub fn add_watch_path(&mut self, _path: PathBuf) {
        // TODO(Task 10): hand off to a live-updatable collector (or restart
        // LogTailCollector::with_static_targets including this path).
    }

    /// Register a PID whose device fd activity should be watched. Stubbed
    /// until Task 10; see `add_watch_path`.
    // wired in Task 10
    pub fn add_watch_pid(&mut self, _pid: i32) {
        // TODO(Task 10): wire to the future PID-focused collector.
    }

    /// Spawn a wrapped-command collector (`tt-toplike sniff -- <argv>`).
    /// Stubbed until Task 10 provides `WrapCollector`; returns `Ok(())` so
    /// callers can integrate the CLI/keybinding now and get real behavior
    /// once Task 10 lands.
    // wired in Task 10
    pub fn add_wrap(&mut self, _argv: Vec<String>) -> Result<(), String> {
        // TODO(Task 10): collector::spawn(Box::new(WrapCollector::new(argv)), tx)
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
}
