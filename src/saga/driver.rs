//! The saga driver: a thread that feeds the saga core from the bus and
//! publishes snapshots of its state for the view (spec 3.6).
//!
//! The core ([`tt_toplike_saga::Saga`]) is pure: it knows no thread, no
//! clock and no bus. A [`SagaDriver`] owns one on a background thread,
//! feeds it every record in order, and publishes a snapshot of its state
//! that a view reads with [`SagaDriver::state`]. The TUI starts one at
//! start-up unless `[saga] enabled = false` (ledger ruling R24), whether or
//! not the Saga view is on screen, so the story has no gaps.
//!
//! # Where the records come from
//!
//! * [`SagaDriver::spawn`] reads a [`Bus`] this process hosts with
//!   [`Bus::read_from`], [`BATCH`] records at a time, from the bus
//!   instance's first sequence number ([`Bus::first_seq`]), and parks for
//!   [`IDLE_PARK`] when it has caught up. A `--bus-dir` or `--record`
//!   directory that holds earlier sessions is read from this session on:
//!   the old records are on disk but belong to other stories.
//! * [`SagaDriver::spawn_remote`] (Unix only) reads another process's bus
//!   over its socket: a snapshot, then `subscribe_from` the snapshot's
//!   sequence number. The saga starts from the snapshot's readings (each
//!   becomes one sample record) and its ready models (each becomes one
//!   `ModelReady`, see [`snapshot_records`]), plus the records after it, so
//!   floors get their chips' topology and a serving model gets its world at
//!   once (spec 3.5: "A TUI that starts mid-run begins its saga from the
//!   world snapshot plus the records after it").
//!
//! # Restarts
//!
//! A restarted feed means a new [`Saga`]: its story starts again from world
//! 1. The core's contract is that the same ordered stream gives the same
//! saga, and a feed with a hole in it is a different stream, so the driver
//! never continues an old saga across one. A feed restarts when:
//!
//! * the local bus no longer keeps the next record ([`ReadError::Dropped`]:
//!   the driver fell more than the bus's memory window behind). The new saga
//!   starts at the oldest record still kept.
//! * the local bus says the next record is in the future
//!   ([`ReadError::Future`]). The new saga starts after the head.
//! * the remote server says `dropped`, `future` or `too_slow`, a record does
//!   not follow the last one by exactly one sequence number, or the server's
//!   bus instance changed. The driver takes a fresh snapshot at once, at
//!   most four times a second, as `RemoteWorld` does.
//! * the remote connection is lost. The driver reconnects after 1 s, then
//!   2 s, 4 s, 8 s and every 10 s, and starts a new saga from the new
//!   snapshot.
//!
//! [`Published::feeds`] counts the feeds started, so a view can tell that
//! the story restarted.
//!
//! # Publishing
//!
//! After feeding, the driver publishes a [`Published`] snapshot (an
//! `Arc<SagaState>` behind a mutex) at most once per [`PUBLISH_EVERY`] (ten
//! times a second, ledger ruling R7), and only when it fed something since
//! the last one. A local driver reads the bus's own clock
//! ([`Bus::now_mono_ns`]), so a test bus with a manual clock decides the
//! cadence; a remote driver reads `Instant`. The cadence decides only what
//! the view can see and when. The saga is fed every record either way, so
//! the story never depends on it.
//!
//! Cloning the state is cheap: the chronicle shares its beats through `Arc`s
//! (ruling R7). A full state (32 floors, 500 beats) took about 6 us to clone
//! and its digest about 27 us, in release mode on the development box.
//!
//! # Labels and seasons
//!
//! The saga's label (spec 3.9) comes from the bus header and replaces the
//! one in the caller's config: see [`label_for`]. So does the season, when
//! the header records one (`saga.season_ticks`, see
//! [`super::config::season_for_header`]), so a live saga, its replay and
//! `--saga-digest` of its log end worlds at the same tick. A local driver
//! reads both once from [`Bus::header`]; a remote driver reads them from
//! each snapshot.
//!
//! # Events
//!
//! The driver drains the saga's events after every record and drops them,
//! which keeps the core's buffer empty. Publishing them as `saga.*` events
//! on the bus (spec 6) is not done here.
//!
//! # Stopping
//!
//! The thread ends when [`SagaDriver::stop`] is called or the handle is
//! dropped, and also after a panic in the core, which it catches. Either
//! way it sets [`Published::stopped`] as its last act, so a view can say
//! that the story has stopped. Without it the view would show a frozen
//! state with no reason given.
//!
//! The thread starts no programs, so it lowers its own scheduling priority
//! ([`crate::workload::background::lower_priority`]) and yields the CPU to
//! the thread that draws the frames.
//!
//! # Batch cost
//!
//! Measured in release mode on the shared development box while it was
//! loaded (ruling R22): a batch of [`BATCH`] records that holds a 32-floor
//! birth, the largest world the core builds, took 0.94 ms on average and
//! 1.30 ms at worst over 30 runs; a batch of 512 sweeps with no birth took
//! 0.16 ms on average. A batch is fed whole. The controller accepted the
//! 1.30 ms worst case for phase 1; lazy floors or a birth rate limit are
//! deferred to phase 2.6.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tt_toplike_bus::{
    Bus, Event, HeaderKind, ModelPhase, ModelView, Payload, ReadError, Record, Sample, WorldSummary,
};
use tt_toplike_saga::{Label, Saga, SagaConfig, SagaState};

use super::config::season_for_header;

/// Records read from a local bus per [`Bus::read_from`] call.
pub const BATCH: usize = 512;

/// How long a caught-up local driver parks before it reads again. Stopping
/// unparks it at once.
pub const IDLE_PARK: Duration = Duration::from_millis(20);

/// The shortest time between two published snapshots: ten a second
/// (ledger ruling R7).
pub const PUBLISH_EVERY: Duration = Duration::from_millis(100);

/// The source name of the records [`snapshot_records`] makes from a remote
/// snapshot's readings. The core reads a record's source only to skip its
/// own (`saga`).
pub const SNAPSHOT_SOURCE: &str = "snapshot";

/// The label a saga takes from its bus header (spec 3.9).
///
/// * [`Label::Replay`] when the header kind is `Replay` or its `mode` tag is
///   `replay`. A replay of a mock recording is still labelled a replay: the
///   title says where the records come from now.
/// * [`Label::Mock`] when the `backend` tag or the `mode` tag is `mock`.
/// * [`Label::Live`] otherwise.
pub fn label_for(kind: HeaderKind, backend: &str, mode: &str) -> Label {
    if kind == HeaderKind::Replay || mode == "replay" {
        Label::Replay
    } else if backend == "mock" || mode == "mock" {
        Label::Mock
    } else {
        Label::Live
    }
}

/// The label for a local bus, from [`Bus::header`].
fn bus_label(bus: &Bus) -> Label {
    let h = bus.header();
    let tag = |k: &str| h.tool_versions.get(k).map(String::as_str).unwrap_or("");
    label_for(h.kind, tag("backend"), tag("mode"))
}

/// What a driver publishes for a view.
#[derive(Debug, Clone)]
pub struct Published {
    /// The saga's state when it was published.
    pub state: Arc<SagaState>,
    /// [`Saga::digest`] of that state: two sagas fed the same records have
    /// the same digest.
    pub digest: u64,
    /// Records fed to the current saga, its seeded snapshot records
    /// included.
    pub fed: u64,
    /// The bus sequence number of the newest record fed (0 before any, and
    /// the snapshot's sequence number right after a remote feed starts).
    pub through: u64,
    /// Feeds started: 1 for the first saga, one more for each restart (see
    /// "Restarts" in the module comment). 0 until a remote driver has its
    /// first snapshot.
    pub feeds: u64,
    /// The driver thread has ended: after [`SagaDriver::stop`], or after a
    /// panic it caught. The state is the last one published and will not
    /// change again.
    pub stopped: bool,
}

/// What the driver thread and the [`SagaDriver`] handle share.
struct Shared {
    published: Mutex<Published>,
    /// Notified on every publish ([`SagaDriver::wait_for`]).
    cond: Condvar,
    stop: AtomicBool,
    /// Test hook: the local driver panics after its next non-empty read.
    #[cfg(test)]
    panic_on_read: AtomicBool,
    /// Test hook: the driver thread's nice value, read at its start.
    #[cfg(test)]
    nice: Mutex<Option<i32>>,
}

impl Shared {
    fn new(p: Published) -> Shared {
        Shared {
            published: Mutex::new(p),
            cond: Condvar::new(),
            stop: AtomicBool::new(false),
            #[cfg(test)]
            panic_on_read: AtomicBool::new(false),
            #[cfg(test)]
            nice: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Published> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// One saga and what the driver knows about it.
struct Feeder {
    saga: Saga,
    fed: u64,
    through: u64,
    feeds: u64,
    /// Something was fed since the last publish.
    dirty: bool,
    /// When the last snapshot was published, on the driver's clock (ns).
    last_publish: Option<u64>,
}

impl Feeder {
    fn new(cfg: SagaConfig, feeds: u64) -> Feeder {
        Feeder {
            saga: Saga::new(cfg),
            fed: 0,
            through: 0,
            feeds,
            dirty: false,
            last_publish: None,
        }
    }

    /// The published form of a saga that has seen nothing.
    fn initial(cfg: SagaConfig, feeds: u64) -> Published {
        Feeder::new(cfg, feeds).published()
    }

    /// Feeds one record and drops the events it raised (see "Events" in the
    /// module comment).
    fn feed(&mut self, rec: &Record) {
        self.saga.feed(rec);
        let _ = self.saga.drain_events();
        self.fed += 1;
        self.through = rec.seq;
        self.dirty = true;
    }

    fn published(&self) -> Published {
        Published {
            state: Arc::new(self.saga.state().clone()),
            digest: self.saga.digest(),
            fed: self.fed,
            through: self.through,
            feeds: self.feeds,
            stopped: false,
        }
    }

    /// Publishes when something was fed and [`PUBLISH_EVERY`] has passed
    /// since the last publish (or there was none). `now_ns` is the driver's
    /// clock.
    fn publish_if_due(&mut self, now_ns: u64, shared: &Shared) {
        if !self.dirty {
            return;
        }
        let every = u64::try_from(PUBLISH_EVERY.as_nanos()).unwrap_or(u64::MAX);
        if self
            .last_publish
            .is_some_and(|last| now_ns.saturating_sub(last) < every)
        {
            return;
        }
        let p = self.published();
        *shared.lock() = p;
        shared.cond.notify_all();
        self.dirty = false;
        self.last_publish = Some(now_ns);
    }
}

/// The name of a local driver's thread.
const LOCAL_THREAD: &str = "tt-toplike-saga";
/// The name of a remote driver's thread.
const REMOTE_THREAD: &str = "tt-toplike-saga-remote";

/// How long stopping a remote driver waits for its thread. The thread can be
/// inside a connect or a snapshot request that waits up to 10 s on a hung
/// server; past this wait it is left to end on its own (it holds only its
/// own `Arc` and checks the stop flag at its next step), so quitting the
/// TUI is never held up by a dead host. A local driver is always joined.
const REMOTE_STOP_WAIT: Duration = Duration::from_millis(500);

/// True when the calling thread is a saga driver's. The TUI's panic hook
/// checks it: a panic in the driver is caught there (the driver stops and
/// the view keeps its last state) and must not take the terminal out of raw
/// mode.
pub fn is_saga_thread() -> bool {
    matches!(thread::current().name(), Some(LOCAL_THREAD | REMOTE_THREAD))
}

/// The body of a driver thread: lowers its priority, runs the loop `f`,
/// catches a panic in it, and marks the published snapshot stopped when
/// the loop ends for any reason (see "Stopping" in the module comment).
fn guarded(shared: &Shared, f: impl FnOnce()) {
    crate::workload::background::lower_priority();
    #[cfg(all(test, target_os = "linux"))]
    {
        *shared.nice.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(crate::workload::background::thread_nice());
    }
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        log::warn!("saga: the driver stopped after a panic; the view keeps its last state");
    }
    shared.lock().stopped = true;
    shared.cond.notify_all();
}

/// A saga fed on a background thread. See the module comment.
///
/// Dropping it stops the thread and waits for it, as [`SagaDriver::stop`]
/// does.
pub struct SagaDriver {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// A remote driver: stopping waits at most [`REMOTE_STOP_WAIT`].
    remote: bool,
}

impl std::fmt::Debug for SagaDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.shared.lock();
        f.debug_struct("SagaDriver")
            .field("fed", &p.fed)
            .field("through", &p.through)
            .field("feeds", &p.feeds)
            .finish_non_exhaustive()
    }
}

impl SagaDriver {
    /// Feed a saga from `bus`, starting at this bus instance's first record
    /// ([`Bus::first_seq`]), on a new thread. The label comes from the bus
    /// header ([`label_for`]) and replaces `cfg.label`; so does the season
    /// when the header records one (see [`season_for_header`]). Fails only
    /// when the thread cannot be started.
    pub fn spawn(bus: Bus, cfg: SagaConfig) -> std::io::Result<SagaDriver> {
        let h = bus.header();
        let cfg = SagaConfig {
            label: bus_label(&bus),
            season_ticks: season_for_header(h.kind, &h.tool_versions, cfg.season_ticks),
            ..cfg
        };
        let shared = Arc::new(Shared::new(Feeder::initial(cfg, 1)));
        let s = shared.clone();
        let thread = thread::Builder::new()
            .name(LOCAL_THREAD.into())
            .spawn(move || guarded(&s, || run_local(&bus, cfg, &s)))?;
        Ok(SagaDriver {
            shared,
            thread: Some(thread),
            remote: false,
        })
    }

    /// Feed a saga from the bus served at `path`, on a new thread, starting
    /// with the connection `client` (one already connected to `path`).
    /// Reconnects open new connections to `path`. Each snapshot starts a
    /// new saga (see "Restarts" in the module comment) labelled from that
    /// snapshot's header kind and tags. Fails only when the thread cannot
    /// be started.
    #[cfg(unix)]
    pub fn spawn_remote(
        client: tt_toplike_bus::SocketClient,
        path: &std::path::Path,
        cfg: SagaConfig,
    ) -> std::io::Result<SagaDriver> {
        let shared = Arc::new(Shared::new(Feeder::initial(cfg, 0)));
        let s = shared.clone();
        let path = path.to_path_buf();
        let thread = thread::Builder::new()
            .name(REMOTE_THREAD.into())
            .spawn(move || guarded(&s, || remote::run(client, &path, cfg, &s)))?;
        Ok(SagaDriver {
            shared,
            thread: Some(thread),
            remote: true,
        })
    }

    /// The newest published state. Before the first publish it is the state
    /// of a saga that has seen nothing (the placeholder world).
    pub fn state(&self) -> Arc<SagaState> {
        self.shared.lock().state.clone()
    }

    /// Whether the driver thread has ended (see [`Published::stopped`]).
    pub fn stopped(&self) -> bool {
        self.shared.lock().stopped
    }

    /// The newest published snapshot with its counters.
    pub fn published(&self) -> Published {
        self.shared.lock().clone()
    }

    /// Waits until `pred` holds for the published snapshot, or `timeout`
    /// passes. Returns whether it held. For tests and status checks; a view
    /// calls [`SagaDriver::state`] and never waits.
    pub fn wait_for(&self, timeout: Duration, pred: impl Fn(&Published) -> bool) -> bool {
        let guard = self.shared.lock();
        let (guard, _) = self
            .shared
            .cond
            .wait_timeout_while(guard, timeout, |p| !pred(p))
            .unwrap_or_else(PoisonError::into_inner);
        pred(&guard)
    }

    /// Stops the thread and waits for it to end (a remote driver's for at
    /// most [`REMOTE_STOP_WAIT`]).
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let Some(t) = self.thread.take() else {
            return;
        };
        // The thread may be parked; wake it so it sees the flag.
        t.thread().unpark();
        if self.remote {
            let end = std::time::Instant::now() + REMOTE_STOP_WAIT;
            while !t.is_finished() && std::time::Instant::now() < end {
                thread::sleep(Duration::from_millis(5));
            }
            if !t.is_finished() {
                // Left to end on its own; see REMOTE_STOP_WAIT.
                return;
            }
        }
        let _ = t.join();
    }

    /// A handle that outlives the driver only while its thread still runs,
    /// so a test can tell that the thread ended.
    #[cfg(test)]
    fn thread_handle(&self) -> std::sync::Weak<Shared> {
        Arc::downgrade(&self.shared)
    }

    /// The published snapshot's lock, so a test can hold it and stand for a
    /// driver that is slow to publish.
    #[cfg(test)]
    fn hold_publishing(&self) -> MutexGuard<'_, Published> {
        self.shared.lock()
    }

    /// Makes a local driver panic after its next non-empty read.
    #[cfg(test)]
    fn panic_on_next_read(&self) {
        self.shared.panic_on_read.store(true, Ordering::SeqCst);
    }

    /// The driver thread's nice value as it read it at its start, once it
    /// has started (Linux).
    #[cfg(all(test, target_os = "linux"))]
    fn thread_nice(&self) -> Option<i32> {
        *self
            .shared
            .nice
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for SagaDriver {
    fn drop(&mut self) {
        self.halt();
    }
}

/// The local driver's loop. See the module comment.
fn run_local(bus: &Bus, cfg: SagaConfig, shared: &Shared) {
    let mut f = Feeder::new(cfg, 1);
    // This bus instance's first record. A bus reopened on a directory that
    // holds earlier sessions continues their numbers and can still read
    // them from disk; starting at 1 would tell those old sessions first and
    // then run them into this one as one story (final review finding I3).
    let mut next = bus.first_seq();
    let mut warned = false;
    while !shared.stopping() {
        match bus.read_from(next, BATCH) {
            Ok(batch) => {
                let Some(last) = batch.last() else {
                    f.publish_if_due(bus.now_mono_ns(), shared);
                    thread::park_timeout(IDLE_PARK);
                    continue;
                };
                next = last.seq + 1;
                #[cfg(test)]
                if shared.panic_on_read.load(Ordering::SeqCst) {
                    panic!("test hook: the saga driver panics");
                }
                for r in &batch {
                    f.feed(r);
                }
                f.publish_if_due(bus.now_mono_ns(), shared);
            }
            Err(ReadError::Dropped { oldest }) => {
                log::debug!("saga: records before {oldest} are gone; the story starts again");
                f = Feeder::new(cfg, f.feeds + 1);
                next = oldest;
            }
            Err(ReadError::Future { head }) => {
                log::debug!("saga: the bus head {head} is behind the saga; the story starts again");
                f = Feeder::new(cfg, f.feeds + 1);
                next = head + 1;
            }
            Err(ReadError::Io(e)) => {
                // The same position is tried again after a park.
                if !warned {
                    log::warn!("saga: cannot read the bus log: {e}");
                    warned = true;
                }
                thread::park_timeout(IDLE_PARK);
            }
        }
    }
}

/// The records a remote saga starts from, all from source
/// [`SNAPSHOT_SOURCE`], with sequence numbers and times of 0 (the core reads
/// neither):
///
/// 1. one sample per reading in the snapshot's world, in the summary's
///    order;
/// 2. one `ModelReady` per model whose phase is `Ready`, in key order. The
///    core births dungeons and sage hooks only from `ModelReady`, so without
///    these a TUI attached to a host that is already serving would show no
///    world for that model until it reloads.
///
/// The snapshot's events are left out: they are only the newest 64, and
/// already past.
pub fn snapshot_records(world: &WorldSummary) -> Vec<Record> {
    let record = |payload: Payload| Record {
        seq: 0,
        t_mono_ns: 0,
        t_wall_ms: 0,
        source: SNAPSHOT_SOURCE.to_string(),
        payload,
    };
    let mut out: Vec<Record> = world
        .readings
        .iter()
        .map(|r| {
            record(Payload::Sample(Sample {
                entity: r.entity.clone(),
                signal: r.signal.clone(),
                value: r.value.clone(),
                prov: r.prov,
            }))
        })
        .collect();
    let mut ready: Vec<&ModelView> = world
        .models
        .iter()
        .filter(|m| m.phase == ModelPhase::Ready)
        .collect();
    ready.sort_by(|a, b| a.key.cmp(&b.key));
    for m in ready {
        out.push(record(Payload::Event(Event::ModelReady {
            model: m.key.clone(),
            chips: m.chips.clone(),
        })));
    }
    out
}

/// The remote driver (Unix sockets only).
#[cfg(unix)]
mod remote {
    use super::{label_for, season_for_header, snapshot_records, Feeder, Shared};
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};
    use tt_toplike_bus::{ClientError, RemoteSnapshot, RemoteSubscription, SocketClient};
    use tt_toplike_saga::SagaConfig;

    /// How long one wait for a record lasts, so the thread checks for stop
    /// at least this often.
    const TICK: Duration = Duration::from_millis(100);
    /// First reconnect delay; it doubles up to [`BACKOFF_MAX`].
    const BACKOFF_FIRST: Duration = Duration::from_secs(1);
    /// Longest reconnect delay.
    const BACKOFF_MAX: Duration = Duration::from_secs(10);
    /// Shortest time between two snapshot requests (four a second at most).
    const SNAPSHOT_FLOOR: Duration = Duration::from_millis(250);

    /// Waits `d`, waking at least every [`TICK`] to check for stop. False
    /// when stopping.
    fn pause(shared: &Shared, d: Duration) -> bool {
        let end = Instant::now() + d;
        loop {
            if shared.stopping() {
                return false;
            }
            let now = Instant::now();
            if now >= end {
                return true;
            }
            thread::park_timeout((end - now).min(TICK));
        }
    }

    /// A snapshot and a subscription from it, on `client`.
    fn open(mut client: SocketClient) -> std::io::Result<(RemoteSnapshot, RemoteSubscription)> {
        let snap = client.snapshot()?;
        let sub = client.subscribe_from(snap.seq)?;
        Ok((snap, sub))
    }

    /// The remote driver's loop. See the module comment.
    pub(super) fn run(first: SocketClient, path: &Path, cfg: SagaConfig, shared: &Shared) {
        let base = Instant::now();
        let now_ns = || u64::try_from(base.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut client = Some(first);
        let mut backoff = BACKOFF_FIRST;
        let mut feeds = 0u64;
        let mut last_snapshot: Option<Instant> = None;
        while !shared.stopping() {
            // Keep snapshot requests at least SNAPSHOT_FLOOR apart.
            if let Some(at) = last_snapshot {
                let since = at.elapsed();
                if since < SNAPSHOT_FLOOR && !pause(shared, SNAPSHOT_FLOOR - since) {
                    return;
                }
            }
            last_snapshot = Some(Instant::now());
            let opened = match client.take() {
                Some(c) => open(c),
                None => SocketClient::connect(path).and_then(open),
            };
            let (snap, mut sub) = match opened {
                Ok(o) => o,
                Err(e) => {
                    log::debug!("saga: no bus at {}: {e}", path.display());
                    if !pause(shared, backoff) {
                        return;
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                    continue;
                }
            };
            backoff = BACKOFF_FIRST;
            // A new snapshot is a new feed: a new saga with the label the
            // server's header calls for.
            feeds += 1;
            let cfg = SagaConfig {
                label: label_for(snap.kind, &snap.backend, &snap.mode),
                season_ticks: season_for_header(snap.kind, &snap.tags, cfg.season_ticks),
                ..cfg
            };
            let mut f = Feeder::new(cfg, feeds);
            for r in snapshot_records(&snap.world) {
                f.feed(&r);
            }
            f.through = snap.seq;
            // Publish the new feed at once, seeded or not, so a view (and a
            // test) can tell that it started.
            f.dirty = true;
            f.last_publish = None;
            f.publish_if_due(now_ns(), shared);
            let mut expect = snap.seq + 1;
            let at_once = loop {
                if shared.stopping() {
                    return;
                }
                let got = sub.next(TICK);
                if sub.instance().is_some_and(|i| i != snap.instance) {
                    break true;
                }
                match got {
                    Ok(Some(rec)) => {
                        // A gap or a repeat would skip or double a record.
                        if rec.seq != expect {
                            break true;
                        }
                        f.feed(&rec);
                        expect = rec.seq + 1;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::debug!("saga: the bus subscription ended: {e}");
                        break matches!(
                            e,
                            ClientError::Dropped { .. }
                                | ClientError::Future { .. }
                                | ClientError::TooSlow
                        );
                    }
                }
                f.publish_if_due(now_ns(), shared);
            };
            // Publish what the ended feed saw last, then start again.
            f.last_publish = None;
            f.publish_if_due(now_ns(), shared);
            if !at_once {
                if !pause(shared, backoff) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod driver_tests;
