//! The bus host: owns the [`Bus`], the chip identity map and every source,
//! and drives them on the host's tick.
//!
//! A host is what the TUI (a later task) and the headless mode
//! ([`crate::bus::headless`]) hold. It does not run any monitor itself. The
//! caller already runs the backend and the workload monitors; it hands their
//! latest results to the host with the `feed_*` methods, then calls
//! [`BusHost::tick`]. Each tick:
//!
//! 1. refreshes the [`IdentityMap`] from `backend.devices()` and publishes
//!    the payloads it held that now resolve. This runs FIRST, so a source
//!    that only knows a `/dev/tenstorrent/N` index (processes, training,
//!    hivemind, model chips) resolves it against this tick's device list.
//!    Two chips that swapped indexes since the last tick land on the right
//!    keys;
//! 2. every [`RESEND_EVERY`], asks every source to resend what it otherwise
//!    sends only on change ([`Source::resend_all`]), so log retention, which
//!    deletes whole segments, cannot lose a process's command line or a
//!    model's announcement;
//! 3. polls the backend source and publishes what it emits;
//! 4. polls the other sources (inference, training, hivemind, processes,
//!    `--remote`, gozer) and publishes;
//! 5. every [`MAINTAIN_EVERY`], asks the bus to apply retention and flush
//!    ([`Bus::request_maintenance`]; it does not wait for the disk).
//!
//! `tick` never blocks on the disk or on a command: publishing is
//! non-blocking, and the gozer source runs `gozer status` on its own worker
//! thread.
//!
//! # The world
//!
//! The bus is opened with the [`World`] reducer, which the bus keeps current
//! under its publish lock. [`BusHost::with_world`] reads it in place (cheap,
//! no copy; keep the closure short, publishers wait for it).
//! [`BusHost::world`] returns a copy made by [`Bus::snapshot`].
//!
//! # Replay
//!
//! [`BusHost::replay`] opens a bus whose header says `Replay` and plays a
//! recorded log into it on a background thread. It has no live sources:
//! `tick` only runs maintenance. Dropping the host stops the thread.

use crate::backend::TelemetryBackend;
use crate::bus::gozer::{GozerSource, GozerStatus};
use crate::bus::identity::IdentityMap;
use crate::bus::source::{BackendSource, PollCtx, Source};
use crate::bus::workload_sources::{
    HivemindSource, InferenceSource, ProcSnapshot, ProcessSource, TrainingSource,
};
use crate::workload::hivemind::event::SniffEvent;
use crate::workload::inference_server::ServiceState;
use crate::workload::train::monitor::TrainState;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tt_toplike_bus::{Bus, BusConfig, Event, Payload, RealClock, Replay, World};

/// How often `tick` asks the bus to apply retention.
pub const MAINTAIN_EVERY: Duration = Duration::from_secs(60);

/// How often `tick` asks the sources to resend values sent only on change.
pub const RESEND_EVERY: Duration = Duration::from_secs(5 * 60);

/// Source label on records the host publishes for the identity map (held
/// payloads that resolved late).
pub const IDENTITY_SOURCE: &str = "identity";

/// Source label on `ViewChanged` events.
pub const VIEW_SOURCE: &str = "view";

/// A replay playing on a background thread.
struct ReplayThread {
    stop: Arc<AtomicBool>,
    /// The play result once the thread has finished: the record count, or
    /// the error text.
    done: Arc<Mutex<Option<Result<u64, String>>>>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for ReplayThread {
    /// Stop the replay and wait for its thread. `play_until` checks the
    /// flag at least every 100 ms, so this is short.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Owns the bus, the identity map and the sources. See the module docs.
pub struct BusHost {
    bus: Bus,
    identity: Arc<IdentityMap>,
    backend: BackendSource,
    inference: InferenceSource,
    training: TrainingSource,
    hivemind: HivemindSource,
    processes: ProcessSource,
    #[cfg(feature = "remote")]
    remote: crate::bus::workload_sources::RemoteSource,
    gozer: GozerSource,
    /// False for a replay host: no live source is polled.
    live: bool,
    last_maintain: Option<Instant>,
    last_resend: Option<Instant>,
    maintenance_runs: u64,
    resends: u64,
    replay: Option<ReplayThread>,
}

impl BusHost {
    /// A live host reading `backend`, with the real gozer source on Linux
    /// (`gozer status --json` every 2 s on a worker thread; it never opens
    /// a chip) and a fed-only one elsewhere.
    ///
    /// Opens the bus with the [`World`] reducer, refreshes the identity map
    /// from `backend.devices()`, and installs the quiet reducer panic hook
    /// ([`tt_toplike_bus::install_quiet_reducer_hook`]). Fails when the bus
    /// cannot be opened, for example when another process holds the log
    /// directory (`WouldBlock`).
    pub fn new(cfg: BusConfig, backend: &dyn TelemetryBackend) -> io::Result<BusHost> {
        #[cfg(target_os = "linux")]
        let gozer = GozerSource::with_command();
        #[cfg(not(target_os = "linux"))]
        let gozer = GozerSource::fed();
        BusHost::with_gozer(cfg, backend, gozer)
    }

    /// [`BusHost::new`] with the given gozer source. Tests and the
    /// `--mock`/`--remote` hosts pass [`GozerSource::fed`], which never runs
    /// a command.
    pub fn with_gozer(
        cfg: BusConfig,
        backend: &dyn TelemetryBackend,
        gozer: GozerSource,
    ) -> io::Result<BusHost> {
        let host = BusHost::empty(Bus::open_with_world(cfg)?, gozer);
        host.identity.refresh(backend.devices());
        Ok(host)
    }

    /// Play the recorded log at `path` (a segment file or a directory) into
    /// a new bus, `speed` times faster than recorded, on a background
    /// thread. The bus's header kind is `Replay`; `cfg.header_kind` is
    /// overridden. Fails at once when the speed is not above zero, the log
    /// cannot be opened, or the bus cannot be opened.
    pub fn replay(path: &Path, speed: f64, cfg: BusConfig) -> io::Result<BusHost> {
        tt_toplike_bus::replay::check_speed(speed)?;
        let mut log = Replay::open(path)?;
        let bus = Bus::open_with_world(Replay::configure(cfg))?;
        let mut host = BusHost::empty(bus, GozerSource::fed());
        host.live = false;
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(Mutex::new(None));
        let (b, s, d) = (host.bus.clone(), stop.clone(), done.clone());
        let handle = std::thread::Builder::new()
            .name("tt-toplike-replay".into())
            .spawn(move || {
                let r = log
                    .play_until(&b, speed, &RealClock, &s)
                    .map_err(|e| e.to_string());
                *d.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            })?;
        host.replay = Some(ReplayThread {
            stop,
            done,
            handle: Some(handle),
        });
        Ok(host)
    }

    fn empty(bus: Bus, gozer: GozerSource) -> BusHost {
        tt_toplike_bus::install_quiet_reducer_hook();
        let identity = Arc::new(IdentityMap::new());
        BusHost {
            bus,
            backend: BackendSource::new(),
            inference: InferenceSource::new(identity.clone()),
            training: TrainingSource::new(identity.clone()),
            hivemind: HivemindSource::new(identity.clone()),
            processes: ProcessSource::new(identity.clone()),
            #[cfg(feature = "remote")]
            remote: crate::bus::workload_sources::RemoteSource::new(identity.clone()),
            gozer,
            identity,
            live: true,
            last_maintain: None,
            last_resend: None,
            maintenance_runs: 0,
            resends: 0,
            replay: None,
        }
    }

    /// One host tick. See the module docs for the order. `now` drives the
    /// maintenance and resend timers and the sources' own rates; tests pass
    /// a made-up `Instant` to move time forward.
    pub fn tick(&mut self, backend: &dyn TelemetryBackend, now: Instant) {
        if self.live {
            // 1. Identity first: index-only sources below resolve against
            //    this tick's device list.
            self.identity.refresh(backend.devices());
            for r in self.identity.resolve_pending() {
                self.bus.publish(IDENTITY_SOURCE, r.payload);
            }

            // 2. Resend what is otherwise sent only on change.
            if due(&mut self.last_resend, now, RESEND_EVERY) {
                self.resends += 1;
                for s in self.sources_mut() {
                    s.resend_all();
                }
            }

            // 3 and 4. Backend first, then the rest, each publishing as it
            // emits.
            let ctx = PollCtx { backend };
            let bus = self.bus.clone();
            for s in self.sources_mut() {
                let name = s.name();
                s.poll(&ctx, now, &mut |p| {
                    // A raw frame is labelled with its own source (`tt-smi`):
                    // the world keeps raw frames by record source, and the
                    // `/telemetry` helpers look them up by that name.
                    let label = match &p {
                        Payload::Raw(f) => f.source.clone(),
                        _ => name.to_string(),
                    };
                    bus.publish(&label, p);
                });
            }
        }

        // 5. Retention on a timer, so an idle bus still ages out old
        //    segments.
        if due(&mut self.last_maintain, now, MAINTAIN_EVERY) {
            self.maintenance_runs += 1;
            self.bus.request_maintenance();
        }
    }

    /// Every source, the backend source first.
    fn sources_mut(&mut self) -> Vec<&mut dyn Source> {
        vec![
            &mut self.backend,
            &mut self.inference,
            &mut self.training,
            &mut self.hivemind,
            &mut self.processes,
            #[cfg(feature = "remote")]
            &mut self.remote,
            &mut self.gozer,
        ]
    }

    /// The inference monitor's latest snapshot.
    pub fn feed_inference(&mut self, snapshot: &[ServiceState]) {
        self.inference.feed(snapshot);
    }

    /// The current process list. Build it with
    /// [`crate::bus::proc_argv::snapshots_from_rows`] to carry command lines
    /// and start times.
    pub fn feed_processes(&mut self, procs: &[ProcSnapshot]) {
        self.processes.feed(procs);
    }

    /// The training run and, when known, each process's device indexes.
    pub fn feed_training(&mut self, state: &TrainState, procs: Option<&[(u32, Vec<usize>)]>) {
        self.training.feed(state, procs);
    }

    /// Hivemind events not fed before.
    pub fn feed_hivemind(&mut self, events: &[SniffEvent]) {
        self.hivemind.feed(events);
    }

    /// One `--remote` frame's extension lists (see `RemoteSource::feed`).
    #[cfg(feature = "remote")]
    pub fn feed_remote(
        &mut self,
        inference: Option<&[crate::backend::RemoteInference]>,
        procs: Option<&[crate::backend::RemoteProc]>,
    ) {
        self.remote.feed(inference, procs);
    }

    /// A parsed `gozer status --json`. The real gozer source also fetches
    /// its own; a fed one only reads these.
    pub fn feed_gozer_status(&mut self, status: GozerStatus) {
        self.gozer.feed(status);
    }

    /// The bus. Clone it to hand to a socket server or a view.
    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    /// The identity map the host refreshes each tick.
    pub fn identity(&self) -> &Arc<IdentityMap> {
        &self.identity
    }

    /// A copy of the world ([`Bus::snapshot`]). Clones every reading; for a
    /// per-frame read use [`BusHost::with_world`].
    pub fn world(&self) -> World {
        self.bus
            .snapshot()
            .map(|s| s.world)
            // The host always opens the bus with a World reducer.
            .unwrap_or_default()
    }

    /// Run `f` on the live world in place, under the bus's publish lock,
    /// with the sequence number it is current to. No copy is made. Keep `f`
    /// short and do not publish from inside it.
    pub fn with_world<T>(&self, f: impl FnOnce(&World, u64) -> T) -> T {
        let mut f = Some(f);
        let got = self
            .bus
            .with_reducer(|w: &World, seq| (f.take().expect("called once"))(w, seq));
        match got {
            Some(v) => v,
            // Unreachable: the host always opens the bus with a World.
            None => (f.take().expect("not called"))(&World::new(), 0),
        }
    }

    /// Publish `Event::ViewChanged { view }`.
    pub fn publish_view_changed(&self, view: &str) {
        self.bus.publish(
            VIEW_SOURCE,
            Payload::Event(Event::ViewChanged {
                view: view.to_string(),
            }),
        );
    }

    /// For a replay host: `None` while playing (and for a live host), then
    /// the number of records played or the error text.
    pub fn replay_result(&self) -> Option<Result<u64, String>> {
        let r = self.replay.as_ref()?;
        r.done.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// How many times `tick` has asked the bus to apply retention.
    pub fn maintenance_runs(&self) -> u64 {
        self.maintenance_runs
    }

    /// How many times `tick` has asked the sources to resend.
    pub fn resends(&self) -> u64 {
        self.resends
    }
}

/// True when `every` has passed since `*last`, and then moves `*last` to
/// `now`. The first call only starts the timer.
fn due(last: &mut Option<Instant>, now: Instant, every: Duration) -> bool {
    match *last {
        None => {
            *last = Some(now);
            false
        }
        Some(t) if now.saturating_duration_since(t) >= every => {
            *last = Some(now);
            true
        }
        Some(_) => false,
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
