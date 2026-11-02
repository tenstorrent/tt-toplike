//! The bus log: one ordered, append-only stream of [`Record`]s.
//!
//! # Shape
//!
//! * [`Bus::publish`] takes one lock, assigns the next sequence number and
//!   both timestamps, runs the [`Reducer`], appends the record to the
//!   in-memory window, and hands a copy to the disk writer with a
//!   non-blocking `try_send`. All of that happens under a single lock, so the
//!   sequence order, the reducer's order, the window's order and the order
//!   the writer receives records are the same order.
//! * One writer thread owns the open segment file. It appends lines,
//!   rotates the segment every `segment_secs`, compresses the closed one,
//!   and applies retention (see [`crate::segment`]).
//! * Readers ask by sequence number. Recent records come from memory; older
//!   ones are read back from the segment files.
//!
//! # Sequence numbers
//!
//! The first record is 1. [`Bus::head`] is the newest assigned number (0
//! before anything is published). [`Bus::read_from`] is inclusive: it starts
//! at the number given. [`Bus::subscribe_from`] takes the number a consumer
//! is already current to (for example the number a snapshot returned) and
//! yields only records after it, so a snapshot plus a subscription misses
//! nothing and sees nothing twice.
//!
//! # Disk failures
//!
//! The disk never holds up a publisher or a reader of live data. If the
//! writer queue is full, the record is still published and kept in memory;
//! its disk write is dropped and counted in
//! [`BusStats::dropped_to_disk_errors`]. If a write fails (directory not
//! writable, disk full), the writer stops writing, counts every record it
//! drops, and the bus publishes one [`Gap`] record for the outage. The
//! writer retries once at least ten seconds (by record time) have passed
//! since the last failed attempt; when a retry succeeds, the outage is over
//! and the next failure publishes a new gap.
//!
//! A segment whose write failed is closed where it stopped. It stays in the
//! segment list with its size on disk, so its records can still be read, it
//! counts toward [`BusStats`], and retention deletes it in time. The next
//! successful write opens a new segment.
//!
//! # Shutdown
//!
//! Dropping the last [`Bus`] handle disconnects the writer queue. The writer
//! writes every record still queued, flushes and exits, and the drop waits
//! up to two seconds for it. A writer stuck on a hung disk is detached with a
//! message on stderr. A [`Subscription`] does not keep the writer running:
//! it holds the in-memory log only, so it keeps reading after the last `Bus`
//! is gone.
//!
//! When disk records are lost (a dropped write, a corrupt line, a deleted
//! segment), a read that reaches the hole stops there. A read that starts
//! inside a hole returns [`ReadError::Dropped`] with the next readable
//! sequence number. Records are never silently skipped.

use crate::config::{BusConfig, Clock};
use crate::record::{Gap, Header, HeaderKind, Payload, Record};
use crate::segment;
use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// Source name the bus uses for records it publishes itself.
pub const BUS_SOURCE: &str = "tt-toplike-bus";

/// After a failed disk write, wait this long (by record wall time) before
/// trying again.
const DISK_RETRY_MS: u64 = 10_000;

/// How many records a subscription fetches per read.
const SUBSCRIPTION_BATCH: usize = 512;

thread_local! {
    /// True on this thread while [`Inner::append`] runs the reducer.
    static IN_REDUCER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Sets [`IN_REDUCER`] for its lifetime. Clearing it in `Drop` means it is
/// cleared after an unwinding reducer too.
struct InReducer;

impl InReducer {
    fn enter() -> InReducer {
        IN_REDUCER.with(|f| f.set(true));
        InReducer
    }
}

impl Drop for InReducer {
    fn drop(&mut self) {
        IN_REDUCER.with(|f| f.set(false));
    }
}

/// Keep reducer panics off stderr.
///
/// `publish` catches a panicking reducer, so the bus keeps running, but the
/// process's panic hook runs first and by default prints the panic to
/// stderr. Under a TUI in raw mode that text lands in the middle of the
/// screen. This wraps the current hook: a panic raised while a reducer runs
/// on this thread is not passed on; every other panic is passed to the
/// previous hook unchanged.
///
/// Call it once, early, from the program that hosts the bus, after any hook
/// of its own is installed (a hook set later replaces this one). Later calls
/// do nothing.
pub fn install_quiet_reducer_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if IN_REDUCER.with(|f| f.get()) {
                return;
            }
            prev(info);
        }));
    });
}

/// Folds records into derived state (the [`crate::World`]).
///
/// The bus calls [`Reducer::apply`] under the same lock that assigns the
/// sequence number, so a reducer sees every published record exactly once
/// and in log order. Keep `apply` short: every publisher waits for it.
pub trait Reducer: Any + Send {
    fn apply(&mut self, rec: &Record);
}

/// The reducer [`Bus::open`] installs: it does nothing.
struct NoReducer;

impl Reducer for NoReducer {
    fn apply(&mut self, _: &Record) {}
}

/// Why a read could not start where it was asked to.
#[derive(Debug)]
pub enum ReadError {
    /// The requested record is no longer kept (or was never written to
    /// disk). `oldest` is the next sequence number that can be read.
    Dropped { oldest: u64 },
    /// The requested number is past the next one to be published. `head` is
    /// the newest assigned number.
    Future { head: u64 },
    /// A segment file could not be read.
    Io(io::Error),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Dropped { oldest } => {
                write!(f, "records before {oldest} are no longer kept")
            }
            ReadError::Future { head } => write!(f, "the newest record is {head}"),
            ReadError::Io(e) => write!(f, "cannot read the log: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// Counters for status lines and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusStats {
    /// Newest assigned sequence number.
    pub head: u64,
    /// Records published but not written to disk (full writer queue or a
    /// failed write).
    pub dropped_to_disk_errors: u64,
    /// Payloads [`Payload::sanitize`] rejected; they were never published.
    pub dropped_non_finite: u64,
    /// Segment files on disk, the open one included.
    pub segments: usize,
    /// Bytes in those files.
    pub bytes: u64,
    /// True when a log directory is set and the last disk write succeeded.
    pub disk_on: bool,
}

/// One segment the writer knows about.
#[derive(Debug, Clone, Copy)]
struct SegInfo {
    first_seq: u64,
    compressed: bool,
    bytes: u64,
    /// Wall time (ms) the segment was closed. For the open one, unused.
    end_ms: u64,
}

/// Segments on disk, oldest first. Shared by the writer and readers.
#[derive(Debug, Default)]
struct SegTable {
    closed: Vec<SegInfo>,
    open: Option<SegInfo>,
}

impl SegTable {
    fn all(&self) -> Vec<SegInfo> {
        self.closed.iter().copied().chain(self.open).collect()
    }
}

/// Disk state shared by the bus and its writer thread.
#[derive(Debug)]
struct Disk {
    dir: PathBuf,
    table: Mutex<SegTable>,
    failed: AtomicBool,
    gap_pending: AtomicBool,
    reason: Mutex<String>,
    dropped: AtomicU64,
    /// Set by the writer when a late lock finds records on disk numbered at
    /// or above records already published in memory: the newest sequence
    /// number on disk. The next `publish` moves the head past it (see
    /// `Writer::ensure_lock`). 0 when nothing is pending.
    skip_to: AtomicU64,
    /// Written into the header line of every new segment.
    kind: HeaderKind,
    /// Extra `tool_versions` entries for the header (see `BusConfig`).
    tags: std::collections::BTreeMap<String, String>,
    #[cfg(test)]
    hooks: TestHooks,
}

/// Fault injection for tests. Not compiled into normal builds.
#[cfg(test)]
#[derive(Debug, Default)]
struct TestHooks {
    /// Every record write fails as if the disk were full.
    fail_writes: AtomicBool,
    /// The writer sleeps this long after receiving each batch.
    batch_delay_ms: AtomicU64,
}

impl Disk {
    /// Mark an outage. Returns true when this starts a new one.
    fn fail(&self, why: String) -> bool {
        if self.failed.swap(true, Ordering::SeqCst) {
            return false;
        }
        *lock(&self.reason) = why;
        true
    }
}

/// State behind the publish lock.
struct State {
    head: u64,
    window: VecDeque<Record>,
    reducer: Box<dyn Reducer>,
    dropped_non_finite: u64,
    /// The lowest sequence number a read may return after a late lock moved
    /// the head forward. Numbers below it were assigned before the jump:
    /// the window no longer holds them and the disk holds other records
    /// under the same numbers, so a read there is refused.
    floor: u64,
    /// Queue to the disk writer. `None` with no log directory, and after the
    /// last `Bus` handle is dropped (that disconnect stops the writer).
    tx: Option<SyncSender<Msg>>,
}

enum Msg {
    Rec(Record),
    Flush(SyncSender<()>),
    /// Apply retention, flush, then acknowledge. See [`Bus::maintain`].
    Maintain(SyncSender<()>),
}

struct Inner {
    state: Mutex<State>,
    cond: Condvar,
    window_records: usize,
    clock: Arc<dyn Clock>,
    disk: Option<Arc<Disk>>,
    header_kind: HeaderKind,
    header_tags: std::collections::BTreeMap<String, String>,
    /// Random id chosen when the bus was opened. See [`Bus::instance`].
    instance: u64,
}

/// How long dropping the last `Bus` handle waits for the writer to finish.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// Owns the writer thread. Only `Bus` handles hold it, so the writer stops
/// when the last `Bus` is dropped even while subscriptions are alive.
/// Subscriptions hold the shared `Inner` (read-only use) and keep working
/// on what is in memory and on disk.
struct WriterGuard {
    inner: Arc<Inner>,
    thread: Option<thread::JoinHandle<()>>,
    /// Disconnects when the writer thread returns. In a `Mutex` only
    /// because `Receiver` is not `Sync` and `Bus` must be.
    done: Mutex<Option<Receiver<()>>>,
}

impl Drop for WriterGuard {
    /// Disconnect the queue so the writer drains it, flushes and exits, then
    /// wait for it. If it does not finish within [`SHUTDOWN_WAIT`] (a hung
    /// disk), detach it and say so on stderr.
    fn drop(&mut self) {
        drop(lock(&self.inner.state).tx.take());
        let done = lock(&self.done).take();
        if let Some(done) = done {
            if let Err(mpsc::RecvTimeoutError::Timeout) = done.recv_timeout(SHUTDOWN_WAIT) {
                eprintln!(
                    "tt-toplike-bus: log writer did not finish within {SHUTDOWN_WAIT:?}; detaching it"
                );
                return;
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Lock a mutex, recovering it if a thread panicked while holding it. A
/// poisoned lock must not turn into a panic in `publish`.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The signal bus. Cheap to clone; every clone shares one log.
#[derive(Clone)]
pub struct Bus {
    inner: Arc<Inner>,
    _writer: Arc<WriterGuard>,
}

impl fmt::Debug for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bus").field("stats", &self.stats()).finish()
    }
}

impl Bus {
    /// Open a bus with no reducer.
    pub fn open(cfg: BusConfig) -> io::Result<Bus> {
        Bus::open_with(cfg, Box::new(NoReducer))
    }

    /// Open a bus whose `reducer` sees every record in log order.
    ///
    /// With `cfg.dir` set, existing segments in the directory are kept and
    /// the sequence continues after the newest record found. If the
    /// directory cannot be created or written, the bus still opens: it runs
    /// from memory, publishes a [`Gap`] saying why, and keeps retrying the
    /// disk. The only error is failing to start the writer thread.
    ///
    /// # The directory lock
    ///
    /// One process at a time writes a directory. `open` takes a
    /// non-blocking exclusive lock on `<dir>/.lock` and fails when it cannot:
    ///
    /// * another holder: `WouldBlock`, `this log directory is already open
    ///   (another process, or a bus in this process that is still shutting
    ///   down): <dir>`;
    /// * the filesystem refuses to lock (`ENOLCK` on an NFS home mounted
    ///   `nolock`, for example): that error's kind, with the error and a
    ///   suggestion to use `--bus-dir` on a local filesystem. Running without
    ///   the lock would let two hosts write one directory.
    ///
    /// When the lock file itself cannot be created (the directory is missing
    /// and cannot be made, or is not writable) the bus opens with the disk
    /// off, as for any other disk outage. The writer takes the lock before
    /// its first write, on every retry, and does not write, compress or
    /// delete anything until it holds it. Once held, it is kept until the
    /// writer thread exits (after the last `Bus` handle is dropped).
    ///
    /// # One backend class and mode per directory
    ///
    /// When the newest segment's header names a `mode` (live, mock, replay)
    /// or a `backend` class (`mock`, `remote`, or `local-hardware` for every
    /// backend that reads this box's chips; see [`BusConfig::header_tags`])
    /// different from the one being opened, the open fails with
    /// `InvalidInput`: `log directory holds a <class>/<mode> log; use
    /// another --bus-dir or remove it`. A change between local backends
    /// (hybrid, json, sysfs, ...) is accepted. An empty directory, a log
    /// with no tags or an unreadable header, and a bus opened with no tags
    /// are accepted. Replaying a log reads its files and is never refused.
    /// The same check runs again when the lock is taken late (see
    /// `Writer::ensure_lock`).
    pub fn open_with(cfg: BusConfig, reducer: Box<dyn Reducer>) -> io::Result<Bus> {
        Bus::open_inner(cfg, reducer, Arc::new(lock_dir))
    }

    /// `open_with` with the lock function passed in (tests inject failures).
    fn open_inner(cfg: BusConfig, reducer: Box<dyn Reducer>, locker: Locker) -> io::Result<Bus> {
        let window_records = cfg.window_records.max(1);
        let mut head = 0;
        let mut tx = None;
        let mut thread_handle = None;
        let mut done = None;
        let mut disk = None;
        let mut startup_failure = None;

        if let Some(dir) = cfg.dir.clone() {
            // One writer per directory, across processes. See "The
            // directory lock" above.
            let mut dir_lock = None;
            let lock_failure = match locker(&dir) {
                Ok(f) => {
                    dir_lock = Some(f);
                    None
                }
                Err(e @ LockError::Held(_)) | Err(e @ LockError::Lock(..)) => {
                    return Err(e.into_io())
                }
                Err(LockError::Open(e)) => {
                    Some(format!("cannot create the lock file in {dir:?}: {e}"))
                }
            };
            let locked = dir_lock.is_some();
            let d = Arc::new(Disk {
                dir: dir.clone(),
                table: Mutex::new(SegTable::default()),
                failed: AtomicBool::new(false),
                gap_pending: AtomicBool::new(false),
                reason: Mutex::new(String::new()),
                dropped: AtomicU64::new(0),
                skip_to: AtomicU64::new(0),
                kind: cfg.header_kind,
                tags: cfg.header_tags.clone(),
                #[cfg(test)]
                hooks: TestHooks::default(),
            });
            let mut first = None;
            // Without the lock, only read: never delete or create a file.
            match prepare(&dir, locked) {
                Ok((closed, last)) => {
                    // One backend and mode per directory. See
                    // `check_same_log`. Returning here drops the lock.
                    check_same_log(&dir, &closed, &cfg.header_tags)?;
                    head = last;
                    lock(&d.table).closed = closed;
                    if locked {
                        match open_segment(&dir, head + 1, cfg.header_kind, &cfg.header_tags) {
                            Ok(seg) => first = Some(seg),
                            Err(e) => {
                                startup_failure = Some(format!("cannot open log segment: {e}"))
                            }
                        }
                    }
                }
                Err(e) => startup_failure = Some(format!("cannot use log directory {dir:?}: {e}")),
            }
            if startup_failure.is_none() {
                startup_failure = lock_failure;
            }
            if let Some(why) = &startup_failure {
                d.fail(why.clone());
            }
            if let Some(seg) = &first {
                lock(&d.table).open = Some(SegInfo {
                    first_seq: seg.first_seq,
                    compressed: false,
                    bytes: seg.bytes,
                    end_ms: 0,
                });
            }
            let (sender, receiver) = mpsc::sync_channel(cfg.writer_queue.max(1));
            let writer = Writer {
                disk: d.clone(),
                segment_ms: cfg.segment_secs.saturating_mul(1000),
                retain_ms: cfg.retain_secs.saturating_mul(1000),
                retain_bytes: cfg.retain_bytes,
                clock: cfg.clock.clone(),
                open: first.map(|s| OpenSeg {
                    start_ms: cfg.clock.wall_ms(),
                    ..s
                }),
                last_attempt_ms: None,
                dir_lock,
                locker,
                refused: false,
                floor: 0,
                floor_announced: false,
            };
            let (done_tx, done_rx) = mpsc::sync_channel::<()>(0);
            thread_handle = Some(
                thread::Builder::new()
                    .name("tt-toplike-bus-writer".into())
                    .spawn(move || {
                        // Dropped when this thread returns, which tells
                        // `WriterGuard::drop` the writer has finished.
                        let _done = done_tx;
                        writer.run(receiver);
                    })?,
            );
            done = Some(done_rx);
            tx = Some(sender);
            disk = Some(d);
        }

        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                head,
                window: VecDeque::new(),
                reducer,
                dropped_non_finite: 0,
                floor: 0,
                tx,
            }),
            cond: Condvar::new(),
            window_records,
            clock: cfg.clock,
            disk,
            header_kind: cfg.header_kind,
            header_tags: cfg.header_tags,
            instance: new_instance(),
        });
        let bus = Bus {
            _writer: Arc::new(WriterGuard {
                inner: inner.clone(),
                thread: thread_handle,
                done: Mutex::new(done),
            }),
            inner,
        };
        if let Some(why) = startup_failure {
            bus.publish(BUS_SOURCE, Payload::Gap(Gap { reason: why }));
        }
        Ok(bus)
    }

    /// Publish one record and return its sequence number.
    ///
    /// The payload goes through [`Payload::sanitize`] first. A payload it
    /// rejects is not published: it is counted in
    /// [`BusStats::dropped_non_finite`] and the current head is returned.
    ///
    /// Never blocks on disk and never panics.
    pub fn publish(&self, source: &str, payload: Payload) -> u64 {
        let inner = &*self.inner;
        let Some(payload) = payload.sanitize() else {
            let mut st = lock(&inner.state);
            st.dropped_non_finite += 1;
            return st.head;
        };
        let mut st = lock(&inner.state);
        if let Some(d) = &inner.disk {
            // A late lock found newer records on disk: continue numbering
            // after them, so sequence numbers stay unique in the directory.
            // The Gap that follows says why the numbers jump.
            let skip = d.skip_to.swap(0, Ordering::SeqCst);
            if skip > st.head {
                st.head = skip;
                // The window holds numbers on the other side of the jump;
                // `copy_window` indexes by offset, so it must not span it.
                st.window.clear();
                st.floor = skip + 1;
            }
            if d.gap_pending.swap(false, Ordering::SeqCst) {
                let reason = lock(&d.reason).clone();
                inner.append(&mut st, BUS_SOURCE, Payload::Gap(Gap { reason }));
            }
        }
        let seq = inner.append(&mut st, source, payload);
        drop(st);
        inner.cond.notify_all();
        seq
    }

    /// The header this bus writes at the top of its segments: `Live`, or
    /// `Replay` when [`BusConfig::header_kind`] says the bus republishes a
    /// recorded log. Consumers (the TUI status line, `bus inspect`) read it
    /// to label what they show.
    pub fn header(&self) -> Header {
        segment::header(self.inner.header_kind, &self.inner.header_tags)
    }

    /// A random id chosen when this bus was opened, shared by its clones.
    ///
    /// A bus reopened on the same directory continues the sequence numbers
    /// but starts with an empty world, so a sequence number alone does not
    /// say which bus it came from. Socket clients compare this id to notice
    /// that the server's bus changed.
    pub fn instance(&self) -> u64 {
        self.inner.instance
    }

    /// The log directory, or `None` for a memory-only bus.
    pub fn dir(&self) -> Option<&Path> {
        self.inner.disk.as_deref().map(|d| d.dir.as_path())
    }

    /// The bus clock's monotonic time now, in nanoseconds: the scale every
    /// record's `t_mono_ns` and every reading's age are on.
    pub fn now_mono_ns(&self) -> u64 {
        self.inner.clock.mono_ns()
    }

    /// The newest assigned sequence number, 0 before the first publish.
    pub fn head(&self) -> u64 {
        lock(&self.inner.state).head
    }

    /// Up to `max` records starting at `seq`, in order with no gaps.
    ///
    /// `seq == head() + 1` returns an empty list (nothing newer yet).
    /// Larger numbers return [`ReadError::Future`]. A number no longer kept
    /// returns [`ReadError::Dropped`]. 0 is read as 1. Records that have
    /// left memory are read from the segment files.
    pub fn read_from(&self, seq: u64, max: usize) -> Result<Vec<Record>, ReadError> {
        self.inner.read_from(seq, max)
    }

    /// Follow the log from just after `seq`.
    ///
    /// Pass the sequence number you are already current to (a snapshot's,
    /// or [`Bus::head`]); the subscription yields `seq + 1` onward. Returns
    /// [`ReadError::Future`] when `seq` is past the head and
    /// [`ReadError::Dropped`] when `seq + 1` is no longer kept.
    pub fn subscribe_from(&self, seq: u64) -> Result<Subscription, ReadError> {
        let head = self.head();
        if seq > head {
            return Err(ReadError::Future { head });
        }
        // Check that the first record is still readable now.
        let buf: VecDeque<Record> = self.read_from(seq + 1, SUBSCRIPTION_BATCH)?.into();
        let next = buf.back().map_or(seq + 1, |r| r.seq + 1);
        Ok(Subscription {
            inner: self.inner.clone(),
            next,
            buf,
            #[cfg(test)]
            reads: 0,
        })
    }

    /// Current counters.
    pub fn stats(&self) -> BusStats {
        let (head, dropped_non_finite) = {
            let st = lock(&self.inner.state);
            (st.head, st.dropped_non_finite)
        };
        let mut s = BusStats {
            head,
            dropped_non_finite,
            ..BusStats::default()
        };
        if let Some(d) = &self.inner.disk {
            s.dropped_to_disk_errors = d.dropped.load(Ordering::SeqCst);
            s.disk_on = !d.failed.load(Ordering::SeqCst);
            let t = lock(&d.table);
            s.segments = t.closed.len() + usize::from(t.open.is_some());
            s.bytes = t.all().iter().map(|g| g.bytes).sum();
        }
        s
    }

    /// Wait until the writer has written and flushed everything published
    /// so far. Returns false on timeout or when the writer has stopped. With
    /// no log directory it returns true at once. For tests and shutdown;
    /// `publish` never calls it.
    pub fn flush(&self, timeout: Duration) -> bool {
        self.ask_writer(Msg::Flush, timeout)
    }

    /// Apply retention now and flush, then wait up to `timeout` for the
    /// writer to finish. Returns false on timeout or when the writer has
    /// stopped. With no log directory it returns true at once.
    ///
    /// The writer applies retention when it rotates a segment, and a
    /// segment rotates only when a record arrives. A bus that publishes
    /// nothing for a long time would never age anything out, so a host calls
    /// this on a timer (`BusHost` does every 60 s). It never rotates: the
    /// open segment is never deleted, as with every other retention pass.
    pub fn maintain(&self, timeout: Duration) -> bool {
        self.ask_writer(Msg::Maintain, timeout)
    }

    /// Ask the writer to apply retention and flush, without waiting. For a
    /// host's tick, which must not block on the disk. Returns false when the
    /// request could not be queued (the writer queue is full or the writer
    /// has stopped); the next request tries again. With no log directory it
    /// returns true at once.
    pub fn request_maintenance(&self) -> bool {
        let st = lock(&self.inner.state);
        let Some(tx) = &st.tx else {
            return true;
        };
        // Nobody waits on the acknowledgement; the writer's send fails
        // quietly when it finds the receiver gone.
        let (ack_tx, _) = mpsc::sync_channel(1);
        tx.try_send(Msg::Maintain(ack_tx)).is_ok()
    }

    /// Send the writer a message that carries an acknowledgement channel and
    /// wait for the acknowledgement.
    fn ask_writer(&self, make: fn(SyncSender<()>) -> Msg, timeout: Duration) -> bool {
        // Clone the sender and release the lock before a send that may wait.
        let Some(tx) = lock(&self.inner.state).tx.clone() else {
            return true;
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if tx.send(make(ack_tx)).is_err() {
            return false;
        }
        ack_rx.recv_timeout(timeout).is_ok()
    }

    /// Run `f` on the reducer under the publish lock, with the head it is
    /// current to. Returns `None` when the installed reducer is not an `R`.
    /// No record is published while `f` runs, so keep it short, and do not
    /// call [`Bus::publish`] from inside `f`: it would wait on the same lock.
    pub fn with_reducer<R: Reducer, T>(&self, f: impl FnOnce(&R, u64) -> T) -> Option<T> {
        let st = lock(&self.inner.state);
        let any: &dyn Any = &*st.reducer;
        any.downcast_ref::<R>().map(|r| f(r, st.head))
    }
}

impl Inner {
    fn read_from(&self, seq: u64, max: usize) -> Result<Vec<Record>, ReadError> {
        let seq = seq.max(1);
        let mem_oldest = {
            let st = lock(&self.state);
            if seq > st.head + 1 {
                return Err(ReadError::Future { head: st.head });
            }
            if seq < st.floor {
                return Err(ReadError::Dropped { oldest: st.floor });
            }
            let mem_oldest = st.window.front().map_or(st.head + 1, |r| r.seq);
            if seq >= mem_oldest {
                return Ok(copy_window(&st.window, seq, max));
            }
            mem_oldest
        };
        // Older than memory: read from disk, then top up from memory.
        let mut out = self.read_disk(seq, max, mem_oldest)?;
        if out.len() < max {
            let next = seq + out.len() as u64;
            let st = lock(&self.state);
            if st.window.front().is_some_and(|r| r.seq <= next) {
                out.extend(copy_window(&st.window, next, max - out.len()));
            }
        }
        Ok(out)
    }

    /// Read `[seq, limit)` from the segment files, at most `max` records.
    fn read_disk(&self, seq: u64, max: usize, limit: u64) -> Result<Vec<Record>, ReadError> {
        let Some(d) = &self.disk else {
            return Err(ReadError::Dropped { oldest: limit });
        };
        let segs = lock(&d.table).all();
        // Start in the newest segment that begins at or before `seq`.
        let start = match segs.iter().rposition(|g| g.first_seq <= seq) {
            Some(i) => i,
            None => {
                let oldest = segs.first().map_or(limit, |g| g.first_seq.min(limit));
                return Err(ReadError::Dropped { oldest });
            }
        };
        let mut out = Vec::new();
        let mut want = seq;
        for g in &segs[start..] {
            if out.len() >= max || want >= limit {
                break;
            }
            let Some(read) = read_either(&d.dir, g.first_seq, want, max - out.len())? else {
                // Deleted by retention while we looked. Continue at the next.
                continue;
            };
            for r in read.records {
                if r.seq >= limit || out.len() >= max {
                    break;
                }
                if r.seq < want {
                    continue;
                }
                if r.seq > want {
                    // A hole: stop here, never skip silently.
                    return if out.is_empty() {
                        Err(ReadError::Dropped { oldest: r.seq })
                    } else {
                        Ok(out)
                    };
                }
                out.push(r);
                want += 1;
            }
        }
        if out.is_empty() {
            // Nothing on disk at `seq`: the next readable record is the first
            // segment after it, or memory.
            let oldest = segs
                .iter()
                .map(|g| g.first_seq)
                .find(|&f| f > seq)
                .map_or(limit, |f| f.min(limit));
            return Err(ReadError::Dropped { oldest });
        }
        Ok(out)
    }

    /// Assign the next sequence number and timestamps, run the reducer,
    /// keep the record in memory and queue it for disk. Caller holds the
    /// state lock.
    fn append(&self, st: &mut State, source: &str, payload: Payload) -> u64 {
        st.head += 1;
        let rec = Record {
            seq: st.head,
            t_mono_ns: self.clock.mono_ns(),
            t_wall_ms: self.clock.wall_ms(),
            source: source.to_string(),
            payload,
        };
        // A panicking reducer must not take `publish` down with it.
        let reducer = &mut st.reducer;
        {
            // Lets `install_quiet_reducer_hook` tell a reducer panic apart.
            let _flag = InReducer::enter();
            let _ = catch_unwind(AssertUnwindSafe(|| reducer.apply(&rec)));
        }
        if let (Some(tx), Some(d)) = (&st.tx, &self.disk) {
            match tx.try_send(Msg::Rec(rec.clone())) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    d.dropped.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        st.window.push_back(rec);
        while st.window.len() > self.window_records {
            st.window.pop_front();
        }
        st.head
    }
}

/// A random 64-bit id: std's per-process random hash keys, mixed with the
/// time, the process id and a counter so two buses in one process differ.
fn new_instance() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    h.write_u64(now);
    h.write_u32(std::process::id());
    h.write_u64(COUNT.fetch_add(1, Ordering::SeqCst));
    h.finish()
}

/// Copy up to `max` records from the window starting at `seq`.
fn copy_window(window: &VecDeque<Record>, seq: u64, max: usize) -> Vec<Record> {
    let Some(front) = window.front() else {
        return Vec::new();
    };
    let skip = usize::try_from(seq.saturating_sub(front.seq)).unwrap_or(usize::MAX);
    window.iter().skip(skip).take(max).cloned().collect()
}

/// Read a segment by first sequence number, plain name first, then the
/// compressed one (the writer may have compressed it in between). `None`
/// when neither exists.
fn read_either(
    dir: &Path,
    first_seq: u64,
    from: u64,
    max: usize,
) -> Result<Option<segment::SegmentRead>, ReadError> {
    for compressed in [false, true] {
        match segment::read_segment_from(&segment::path(dir, first_seq, compressed), from, max) {
            Ok(r) => return Ok(Some(r)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

/// Name of the lock file inside a log directory.
pub const LOCK_FILE: &str = ".lock";

/// Create `dir` if needed and take a non-blocking exclusive lock on
/// `<dir>/.lock` (mode 0600). The lock is `flock` on Unix and `LockFileEx`
/// on Windows, through [`File::try_lock`]; it belongs to the returned file
/// and is released when the file is closed, including when the process
/// dies. Another holder gives `WouldBlock` with the message `this log
/// directory is already open (another process, or a bus in this process that
/// is still shutting down): <dir>`. The lock file is
/// never deleted: deleting it would let a third opener lock a new file
/// while the second still holds the old one.
fn lock_dir(dir: &Path) -> Result<File, LockError> {
    segment::create_dir_secure(dir).map_err(LockError::Open)?;
    let mut o = std::fs::OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let f = o.open(dir.join(LOCK_FILE)).map_err(LockError::Open)?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::Held(dir.to_path_buf())),
        Err(std::fs::TryLockError::Error(e)) => Err(LockError::Lock(dir.to_path_buf(), e)),
    }
}

/// Why the directory lock was not taken.
#[derive(Debug)]
enum LockError {
    /// Another holder has it.
    Held(PathBuf),
    /// The directory or the lock file could not be created or opened: a
    /// disk problem, handled as a disk outage.
    Open(io::Error),
    /// The lock call itself failed (`ENOLCK`, `EOPNOTSUPP`): the filesystem
    /// cannot lock.
    Lock(PathBuf, io::Error),
}

impl LockError {
    fn into_io(self) -> io::Error {
        match self {
            LockError::Held(dir) => io::Error::new(
                io::ErrorKind::WouldBlock,
                // The holder can be this process's own detached writer (a
                // bus dropped a moment ago whose writer thread is still
                // finishing), so do not name another process alone.
                format!(
                    "this log directory is already open (another process, or a bus in this \
                     process that is still shutting down): {}",
                    dir.display()
                ),
            ),
            LockError::Lock(dir, e) => io::Error::new(
                e.kind(),
                format!(
                    "cannot lock log directory {}: {e}. The filesystem does not support \
                     file locks (an NFS home mounted nolock, for example); use --bus-dir \
                     on a local filesystem",
                    dir.display()
                ),
            ),
            LockError::Open(e) => e,
        }
    }
}

/// Takes the directory lock. `lock_dir` in normal use; tests inject one.
type Locker = Arc<dyn Fn(&Path) -> Result<File, LockError> + Send + Sync>;

/// Create the directory, list existing segments and find the newest
/// sequence number on disk. A header-only plain segment at the end (opened
/// by a run that published nothing) is removed so its name can be reused.
///
/// `may_write` is false when the directory lock is not held: the directory
/// is then only read: a header-only segment is skipped and left on disk.
fn prepare(dir: &Path, may_write: bool) -> io::Result<(Vec<SegInfo>, u64)> {
    if may_write {
        segment::create_dir_secure(dir)?;
    }
    let mut found = segment::list(dir)?;
    let mut last = 0;
    while let Some(newest) = found.last().cloned() {
        let p = segment::path(dir, newest.first_seq, newest.compressed);
        let read = segment::read_segment(&p)?;
        match read.records.last() {
            Some(r) => {
                last = r.seq.max(newest.first_seq);
                break;
            }
            None if !newest.compressed => {
                if may_write {
                    std::fs::remove_file(&p)?;
                }
                found.pop();
            }
            None => {
                last = newest.first_seq;
                break;
            }
        }
    }
    let closed = found
        .into_iter()
        .map(|f| SegInfo {
            first_seq: f.first_seq,
            compressed: f.compressed,
            bytes: f.bytes,
            end_ms: f.mtime_ms,
        })
        .collect();
    Ok((closed, last))
}

/// The class of a `backend` header tag: `mock` (made-up values), `remote`
/// (a relay of another machine's data), or `local-hardware` (sysfs,
/// hybrid, json, luwen, host and any other backend that reads this box's
/// chips). `--backend auto` can choose a different local backend from one
/// start to the next, so logs are kept apart by class, not by name.
fn backend_class(backend: &str) -> &'static str {
    match backend {
        "mock" => "mock",
        "remote" => "remote",
        _ => "local-hardware",
    }
}

/// Refuse to add to a directory whose newest segment (`closed`, oldest
/// first, as [`prepare`] lists them) names a different `mode` (live, mock,
/// replay), or a `backend` of a different class (see [`backend_class`]),
/// in its header than `tags`, the bus being opened. Mixing them would make
/// one log of made-up and real data, or of two machines' data, and
/// `bus inspect` prints only the first segment's header. A change of local
/// backend (hybrid one start, json the next) is accepted. A tag missing on
/// either side is not compared, and a header that cannot be read is
/// accepted: an empty directory, a log written before tags existed, and a
/// bus opened with no tags all open. A replay reads its source files
/// directly and never opens the source directory as a bus, so this does
/// not stop a replay of any log.
fn check_same_log(
    dir: &Path,
    closed: &[SegInfo],
    tags: &std::collections::BTreeMap<String, String>,
) -> io::Result<()> {
    let Some(newest) = closed.last() else {
        return Ok(());
    };
    let p = segment::path(dir, newest.first_seq, newest.compressed);
    let Ok(Some(header)) = segment::read_header(&p) else {
        return Ok(());
    };
    let theirs = |k: &str| header.tool_versions.get(k).map(String::as_str);
    let ours = |k: &str| tags.get(k).map(String::as_str);
    let mode_differs = matches!((ours("mode"), theirs("mode")), (Some(a), Some(b)) if a != b);
    let class_differs = matches!(
        (ours("backend"), theirs("backend")),
        (Some(a), Some(b)) if backend_class(a) != backend_class(b)
    );
    if !mode_differs && !class_differs {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "log directory holds a {}/{} log; use another --bus-dir or remove it ({})",
            theirs("backend").map_or("unknown", backend_class),
            theirs("mode").unwrap_or("unknown"),
            dir.display()
        ),
    ))
}

/// The segment the writer appends to.
struct OpenSeg {
    first_seq: u64,
    start_ms: u64,
    file: BufWriter<File>,
    bytes: u64,
}

/// Create segment `first_seq` with its header line.
fn open_segment(
    dir: &Path,
    first_seq: u64,
    kind: HeaderKind,
    tags: &std::collections::BTreeMap<String, String>,
) -> io::Result<OpenSeg> {
    segment::create_dir_secure(dir)?;
    let mut file = BufWriter::new(segment::create_file_secure(&segment::path(
        dir, first_seq, false,
    ))?);
    let bytes = segment::write_header(&mut file, kind, tags)?;
    file.flush()?;
    Ok(OpenSeg {
        first_seq,
        start_ms: 0,
        file,
        bytes,
    })
}

/// The disk writer. Runs on its own thread; owns the open segment.
struct Writer {
    disk: Arc<Disk>,
    segment_ms: u64,
    retain_ms: u64,
    retain_bytes: u64,
    clock: Arc<dyn Clock>,
    open: Option<OpenSeg>,
    last_attempt_ms: Option<u64>,
    /// The open `<dir>/.lock` file. `None` until the lock is taken. Kept
    /// until the writer exits; closing it releases the lock.
    dir_lock: Option<File>,
    locker: Locker,
    /// The directory holds a log of another class or mode (found when the
    /// lock was taken late). Nothing more is written in this run.
    refused: bool,
    /// The newest sequence number on disk when the lock was taken late.
    /// Records numbered at or below it were published before the lock and
    /// would collide with records already in the directory, so they are not
    /// written (they are counted as dropped). 0 when the lock was taken at
    /// open, when the head already continues after the disk.
    floor: u64,
    /// The jump past `floor` has been asked for (once per run).
    floor_announced: bool,
}

impl Writer {
    /// Take the directory lock if it is not held yet. Every write, rotation,
    /// compression and deletion happens only after this succeeds.
    ///
    /// When the lock is taken late (it could not be taken at open), the
    /// open only read the directory: it skipped a header-only last segment
    /// without deleting it. That file would block the next segment with the
    /// same first sequence number and retention never sees it. So the
    /// cleanup in [`prepare`] runs again here, with the lock, and the
    /// segment table is refreshed from the directory. Nothing was written
    /// in this run before the lock was held, so the refresh loses nothing.
    /// If the cleanup fails the lock is released and the next retry tries
    /// both again.
    ///
    /// The directory may also have changed during the outage, or could not
    /// be listed at open, so the class and mode check of [`check_same_log`]
    /// runs again here. On a refusal the lock is released and the disk is
    /// switched off for the rest of this run (see [`Writer::refuse`]), so
    /// two logs are never mixed.
    ///
    /// The newest sequence number on disk ([`prepare`]'s `last`) becomes
    /// [`Writer::floor`]. When the open could not list the directory, the
    /// bus started numbering at 1, so records already published in memory
    /// may carry numbers the directory already holds. Those records keep
    /// their numbers (subscribers and the reducer have seen them) and are
    /// not written; see [`Writer::try_write`] for how the head moves on.
    fn ensure_lock(&mut self) -> io::Result<()> {
        if self.dir_lock.is_none() {
            let f = (self.locker)(&self.disk.dir).map_err(LockError::into_io)?;
            let (closed, last) = prepare(&self.disk.dir, true)?;
            if let Err(e) = check_same_log(&self.disk.dir, &closed, &self.disk.tags) {
                drop(f);
                self.refuse(e.to_string());
                return Err(e);
            }
            lock(&self.disk.table).closed = closed;
            self.dir_lock = Some(f);
            self.floor = last;
        }
        Ok(())
    }

    /// Switch the disk off for good: record `why` as the outage reason and
    /// publish a new Gap carrying it (even when an outage was already
    /// running, so the reason is not the open's older one). Every later
    /// record is dropped and counted without touching the directory.
    fn refuse(&mut self, why: String) {
        self.refused = true;
        *lock(&self.disk.reason) = why;
        self.disk.failed.store(true, Ordering::SeqCst);
        self.disk.gap_pending.store(true, Ordering::SeqCst);
    }

    fn run(mut self, rx: Receiver<Msg>) {
        // Segments left plain by an earlier run that stopped: compress them
        // and apply retention before taking new records. Only with the lock.
        let now = self.clock.wall_ms();
        if self.dir_lock.is_some() {
            let _ = self.compress_closed();
            self.retain(now);
        }
        while let Ok(msg) = rx.recv() {
            #[cfg(test)]
            thread::sleep(Duration::from_millis(
                self.disk.hooks.batch_delay_ms.load(Ordering::SeqCst),
            ));
            self.handle(msg);
            while let Ok(msg) = rx.try_recv() {
                self.handle(msg);
            }
            self.flush_open();
        }
        self.flush_open();
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Rec(r) => self.write_record(&r),
            Msg::Flush(ack) => {
                self.flush_open();
                let _ = ack.send(());
            }
            Msg::Maintain(ack) => {
                self.flush_open();
                if self.dir_lock.is_some() {
                    let now = self.clock.wall_ms();
                    self.retain(now);
                }
                let _ = ack.send(());
            }
        }
    }

    fn flush_open(&mut self) {
        if let Some(o) = &mut self.open {
            if let Err(e) = o.file.flush() {
                let now = self.clock.wall_ms();
                let _ = self.retire_open(now);
                self.failed(format!("log write failed: {e}"));
            }
        }
    }

    fn write_record(&mut self, rec: &Record) {
        let d = &self.disk;
        if self.refused {
            d.dropped.fetch_add(1, Ordering::SeqCst);
            return;
        }
        if d.failed.load(Ordering::SeqCst) {
            if let Some(t) = self.last_attempt_ms {
                if rec.t_wall_ms.saturating_sub(t) < DISK_RETRY_MS {
                    d.dropped.fetch_add(1, Ordering::SeqCst);
                    return;
                }
            }
        }
        match self.try_write(rec) {
            Ok(()) => {
                self.disk.failed.store(false, Ordering::SeqCst);
                self.last_attempt_ms = None;
            }
            Err(e) => {
                // Keep what reached the disk as a closed segment; the next
                // successful write opens a new one.
                let _ = self.retire_open(rec.t_wall_ms);
                self.disk.dropped.fetch_add(1, Ordering::SeqCst);
                self.last_attempt_ms = Some(rec.t_wall_ms);
                self.failed(format!("log write failed: {e}"));
            }
        }
    }

    /// Start an outage if one is not already running.
    fn failed(&self, why: String) {
        if self.disk.fail(why) {
            self.disk.gap_pending.store(true, Ordering::SeqCst);
        }
    }

    fn try_write(&mut self, rec: &Record) -> io::Result<()> {
        // A disk that recovers is not written without the lock.
        self.ensure_lock()?;
        if rec.seq <= self.floor {
            // Published before a late lock, and the directory already holds
            // this number: writing it would give the log two records with
            // one sequence number. Drop it, and the first time ask the
            // publisher to move the head past the disk's newest record and
            // publish a Gap that says so. Numbers stay strictly increasing
            // in the directory.
            if !self.floor_announced {
                self.floor_announced = true;
                let n = self.floor;
                *lock(&self.disk.reason) = format!(
                    "the log directory already held records up to {n}; records numbered up to \
                     {n} that were published before the lock was taken are not on disk, and \
                     numbering continues after {n}"
                );
                self.disk.skip_to.fetch_max(self.floor, Ordering::SeqCst);
                self.disk.gap_pending.store(true, Ordering::SeqCst);
            }
            self.disk.dropped.fetch_add(1, Ordering::SeqCst);
            return Ok(());
        }
        let now = rec.t_wall_ms;
        let due = self
            .open
            .as_ref()
            .is_some_and(|o| now.saturating_sub(o.start_ms) >= self.segment_ms);
        if due {
            self.rotate(now)?;
        }
        if self.open.is_none() {
            let mut seg = open_segment(&self.disk.dir, rec.seq, self.disk.kind, &self.disk.tags)?;
            seg.start_ms = now;
            lock(&self.disk.table).open = Some(SegInfo {
                first_seq: seg.first_seq,
                compressed: false,
                bytes: seg.bytes,
                end_ms: 0,
            });
            self.open = Some(seg);
        }
        let Some(o) = &mut self.open else {
            return Ok(());
        };
        #[cfg(test)]
        if self.disk.hooks.fail_writes.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected write failure"));
        }
        let line = rec.to_line();
        o.file.write_all(line.as_bytes())?;
        o.file.write_all(b"\n")?;
        o.bytes += line.len() as u64 + 1;
        if let Some(g) = &mut lock(&self.disk.table).open {
            g.bytes = o.bytes;
        }
        Ok(())
    }

    /// Close the open segment, compress every closed plain segment, then
    /// apply retention.
    fn rotate(&mut self, now: u64) -> io::Result<()> {
        self.retire_open(now)?;
        let compressed = self.compress_closed();
        self.retain(now);
        compressed
    }

    /// Close the open segment (if any) and move it to the closed list with
    /// its size on disk, so it is counted, compressed, read and deleted by
    /// retention like any other. Used on rotation and after a write error.
    /// Returns the result of the final flush.
    fn retire_open(&mut self, now: u64) -> io::Result<()> {
        let Some(mut o) = self.open.take() else {
            lock(&self.disk.table).open = None;
            return Ok(());
        };
        let flushed = o.file.flush();
        drop(o.file);
        let size =
            std::fs::metadata(segment::path(&self.disk.dir, o.first_seq, false)).map(|m| m.len());
        let mut t = lock(&self.disk.table);
        t.open = None;
        // A file that is gone (deleted under us) is not kept in the table.
        if let Ok(bytes) = size {
            t.closed.push(SegInfo {
                first_seq: o.first_seq,
                compressed: false,
                bytes,
                end_ms: now,
            });
        }
        flushed
    }

    /// Compress every closed segment still in plain form.
    fn compress_closed(&mut self) -> io::Result<()> {
        let plain: Vec<u64> = lock(&self.disk.table)
            .closed
            .iter()
            .filter(|g| !g.compressed)
            .map(|g| g.first_seq)
            .collect();
        for first_seq in plain {
            let size = segment::compress(&self.disk.dir, first_seq)?;
            let mut t = lock(&self.disk.table);
            if let Some(g) = t.closed.iter_mut().find(|g| g.first_seq == first_seq) {
                g.compressed = true;
                g.bytes = size;
            }
        }
        Ok(())
    }

    /// Delete the oldest closed segments while the oldest ended more than
    /// `retain_ms` ago or all segments together exceed `retain_bytes`. The
    /// open segment is never a candidate.
    fn retain(&mut self, now: u64) {
        let mut t = lock(&self.disk.table);
        loop {
            let total: u64 = t.all().iter().map(|g| g.bytes).sum();
            let Some(oldest) = t.closed.first().copied() else {
                break;
            };
            let too_old = now.saturating_sub(oldest.end_ms) > self.retain_ms;
            let too_big = total > self.retain_bytes;
            if !too_old && !too_big {
                break;
            }
            let p = segment::path(&self.disk.dir, oldest.first_seq, oldest.compressed);
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // Cannot delete it. Stop so the loop ends; the next rotation tries again.
                Err(_) => break,
            }
            t.closed.remove(0);
        }
    }
}

/// A live feed of records after a given sequence number.
///
/// A subscription keeps the in-memory log alive but not the disk writer:
/// dropping the last [`Bus`] still flushes and stops the writer.
pub struct Subscription {
    inner: Arc<Inner>,
    next: u64,
    buf: VecDeque<Record>,
    /// Number of log reads, so tests can tell waiting from spinning.
    #[cfg(test)]
    reads: u64,
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription")
            .field("next", &self.next)
            .field("buffered", &self.buf.len())
            .finish()
    }
}

impl Subscription {
    /// The next record, waiting up to `timeout` for one.
    ///
    /// * `Ok(Some(record))`: the next record in sequence.
    /// * `Ok(None)`: `timeout` passed with nothing new. The call blocks on a
    ///   condition variable while it waits; it does not poll.
    /// * `Err(_)`: the subscription cannot continue. [`ReadError::Dropped`]
    ///   means the subscriber fell so far behind that its next record is no
    ///   longer kept; [`ReadError::Io`] means a segment could not be read.
    ///   The error comes back at once. The caller must resubscribe from a
    ///   fresh snapshot; calling `next` again returns the same error.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Record>, ReadError> {
        if let Some(r) = self.buf.pop_front() {
            return Ok(Some(r));
        }
        let deadline = Instant::now() + timeout;
        loop {
            #[cfg(test)]
            {
                self.reads += 1;
            }
            let got = self.inner.read_from(self.next, SUBSCRIPTION_BATCH)?;
            if let Some(last) = got.last() {
                self.next = last.seq + 1;
                self.buf.extend(got);
                return Ok(self.buf.pop_front());
            }
            let inner = &self.inner;
            let mut st = lock(&inner.state);
            while st.head < self.next {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                st = inner
                    .cond
                    .wait_timeout(st, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ManualClock;
    use crate::record::{Gap, Provenance, Sample, Value};
    use crate::segment::{self, file_name};
    use crate::testutil::TempDir;
    use crate::EntityKey;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    const T0: u64 = 1_700_000_000_000;

    fn sample(i: u64) -> Payload {
        Payload::Sample(Sample {
            entity: EntityKey::chip("0000:03:00.0"),
            signal: "n".into(),
            value: Value::U64(i),
            prov: Provenance::Measured,
        })
    }

    fn mem_bus(window: usize) -> Bus {
        Bus::open(BusConfig {
            window_records: window,
            ..BusConfig::default()
        })
        .unwrap()
    }

    fn disk_cfg(dir: &Path, clock: &Arc<ManualClock>) -> BusConfig {
        BusConfig {
            dir: Some(dir.to_path_buf()),
            clock: clock.clone(),
            ..BusConfig::default()
        }
    }

    fn flush(b: &Bus) {
        assert!(b.flush(Duration::from_secs(10)), "writer did not flush");
    }

    fn seqs(recs: &[Record]) -> Vec<u64> {
        recs.iter().map(|r| r.seq).collect()
    }

    fn gaps(recs: &[Record]) -> usize {
        recs.iter()
            .filter(|r| matches!(r.payload, Payload::Gap(_)))
            .count()
    }

    fn exists(dir: &Path, first_seq: u64, compressed: bool) -> bool {
        dir.join(file_name(first_seq, compressed)).exists()
    }

    #[cfg(unix)]
    fn chmod(p: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn seq_is_gapless_and_increasing_under_four_threads() {
        let bus = mem_bus(10_000);
        let handles: Vec<_> = (0..4u64)
            .map(|t| {
                let b = bus.clone();
                thread::spawn(move || {
                    (0..1000u64)
                        .map(|i| b.publish("t", sample(t * 1000 + i)))
                        .collect::<Vec<u64>>()
                })
            })
            .collect();
        let mut all = Vec::new();
        for h in handles {
            let mine = h.join().unwrap();
            assert!(mine.windows(2).all(|w| w[0] < w[1]), "per-thread order");
            all.extend(mine);
        }
        all.sort_unstable();
        assert_eq!(
            all,
            (1..=4000).collect::<Vec<_>>(),
            "gapless, no duplicates"
        );
        assert_eq!(bus.head(), 4000);
        let recs = bus.read_from(1, 5000).unwrap();
        assert_eq!(seqs(&recs), (1..=4000).collect::<Vec<_>>(), "log order");
        assert!(recs.windows(2).all(|w| w[0].t_mono_ns <= w[1].t_mono_ns));
    }

    #[test]
    fn disk_order_matches_seq_order() {
        let dir = TempDir::new("order");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(disk_cfg(dir.path(), &clock)).unwrap();
        let handles: Vec<_> = (0..4u64)
            .map(|t| {
                let b = bus.clone();
                thread::spawn(move || {
                    for i in 0..500 {
                        b.publish("t", sample(t * 1000 + i));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        flush(&bus);
        assert_eq!(bus.stats().dropped_to_disk_errors, 0);
        let r = segment::read_segment(&dir.path().join(file_name(1, false))).unwrap();
        assert_eq!(seqs(&r.records), (1..=2000).collect::<Vec<_>>());
        assert_eq!(r.bad_lines, 0);
    }

    #[test]
    fn read_from_returns_exactly_n_to_head_in_order() {
        let bus = mem_bus(100);
        for i in 0..20 {
            bus.publish("t", sample(i));
        }
        let m = bus.head();
        assert_eq!(m, 20);
        for n in [1, 7, 20] {
            let r = bus.read_from(n, usize::MAX).unwrap();
            assert_eq!(seqs(&r), (n..=m).collect::<Vec<_>>(), "from {n}");
        }
        assert_eq!(seqs(&bus.read_from(5, 3).unwrap()), vec![5, 6, 7]);
        // The next sequence number is not in the future: it is empty.
        assert!(bus.read_from(m + 1, 10).unwrap().is_empty());
    }

    #[test]
    fn subscribe_from_head_sees_only_later_records_once() {
        let bus = mem_bus(1000);
        for i in 0..5 {
            bus.publish("t", sample(i));
        }
        let h = bus.head();
        let mut sub = bus.subscribe_from(h).unwrap();
        assert!(sub.next(Duration::from_millis(20)).unwrap().is_none());
        let b2 = bus.clone();
        let t = thread::spawn(move || {
            for i in 0..50 {
                b2.publish("t", sample(i));
            }
        });
        let mut got = Vec::new();
        while got.len() < 50 {
            match sub.next(Duration::from_secs(5)).unwrap() {
                Some(r) => got.push(r.seq),
                None => break,
            }
        }
        t.join().unwrap();
        assert_eq!(got, (h + 1..=h + 50).collect::<Vec<_>>());
        assert!(
            sub.next(Duration::from_millis(20)).unwrap().is_none(),
            "nothing twice"
        );
    }

    #[test]
    fn read_below_oldest_is_dropped() {
        let bus = mem_bus(10);
        for i in 0..50 {
            bus.publish("t", sample(i));
        }
        match bus.read_from(1, 5) {
            Err(ReadError::Dropped { oldest }) => assert_eq!(oldest, 41),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            seqs(&bus.read_from(41, 100).unwrap()),
            (41..=50).collect::<Vec<_>>()
        );
        assert!(matches!(
            bus.subscribe_from(5),
            Err(ReadError::Dropped { oldest: 41 })
        ));
        let mut sub = bus.subscribe_from(40).unwrap();
        assert_eq!(
            sub.next(Duration::from_secs(1)).unwrap().map(|r| r.seq),
            Some(41)
        );
    }

    #[test]
    fn read_above_head_is_future() {
        let bus = mem_bus(10);
        for i in 0..5 {
            bus.publish("t", sample(i));
        }
        assert!(matches!(
            bus.read_from(7, 1),
            Err(ReadError::Future { head: 5 })
        ));
        assert!(matches!(
            bus.subscribe_from(6),
            Err(ReadError::Future { head: 5 })
        ));
        assert!(bus.subscribe_from(5).is_ok());
    }

    #[test]
    fn non_finite_payload_is_dropped_and_counted() {
        let bus = mem_bus(10);
        bus.publish("t", sample(1));
        bus.publish("t", sample(2));
        let nan = Payload::Sample(Sample {
            entity: EntityKey::chip("0000:03:00.0"),
            signal: "x".into(),
            value: Value::F64(f64::NAN),
            prov: Provenance::Measured,
        });
        assert_eq!(bus.publish("t", nan), 2, "returns the current head");
        assert_eq!(bus.head(), 2);
        assert_eq!(bus.stats().dropped_non_finite, 1);
    }

    struct Seen(Vec<u64>);
    impl Reducer for Seen {
        fn apply(&mut self, rec: &Record) {
            self.0.push(rec.seq);
        }
    }
    struct Other;
    impl Reducer for Other {
        fn apply(&mut self, _: &Record) {}
    }

    #[test]
    fn reducer_sees_records_in_log_order() {
        let bus = Bus::open_with(BusConfig::default(), Box::new(Seen(Vec::new()))).unwrap();
        let handles: Vec<_> = (0..4u64)
            .map(|t| {
                let b = bus.clone();
                thread::spawn(move || {
                    for i in 0..250 {
                        b.publish("t", sample(t * 1000 + i));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let ok = bus.with_reducer(|s: &Seen, head| {
            assert_eq!(head, 1000);
            s.0 == (1..=head).collect::<Vec<_>>()
        });
        assert_eq!(ok, Some(true));
        assert!(bus.with_reducer(|_: &Other, _| ()).is_none());
        assert!(mem_bus(1).with_reducer(|_: &Seen, _| ()).is_none());
    }

    #[test]
    fn segments_rotate_on_the_clock_and_closed_one_is_compressed() {
        let dir = TempDir::new("rotate");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            window_records: 2,
            segment_secs: 600,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        for i in 0..3 {
            bus.publish("t", sample(i));
        }
        clock.advance_secs(599);
        bus.publish("t", sample(3));
        flush(&bus);
        assert!(exists(dir.path(), 1, false));
        assert!(!exists(dir.path(), 1, true), "rotated too early");

        clock.advance_secs(1);
        bus.publish("t", sample(4));
        flush(&bus);
        assert!(exists(dir.path(), 1, true), "closed segment compressed");
        assert!(!exists(dir.path(), 1, false), "plain copy removed");
        assert!(exists(dir.path(), 5, false), "new open segment");
        let gz = dir.path().join(file_name(1, true));
        assert_eq!(&fs::read(&gz).unwrap()[..2], &[0x1f, 0x8b], "gzip magic");
        let r = segment::read_segment(&gz).unwrap();
        assert!(r.header.is_some());
        assert_eq!(seqs(&r.records), vec![1, 2, 3, 4]);
        assert_eq!(bus.stats().segments, 2);
        // Records 1..=3 left memory; they come back from the compressed file.
        assert_eq!(seqs(&bus.read_from(1, 100).unwrap()), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn retention_deletes_oldest_by_age_and_keeps_newer() {
        let dir = TempDir::new("age");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            window_records: 1,
            segment_secs: 10,
            retain_secs: 60,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        // One record per segment: segments 1..=6, closed at T0+10s .. T0+50s.
        bus.publish("t", sample(0));
        for k in 1..=5 {
            clock.advance_secs(10);
            bus.publish("t", sample(k));
        }
        flush(&bus);
        for s in 1..=5 {
            assert!(exists(dir.path(), s, true), "segment {s}");
        }
        // At T0+75s segment 1 (closed at T0+10s) is 65 s old; segment 2 is 55 s.
        clock.advance_secs(25);
        bus.publish("t", sample(6));
        flush(&bus);
        assert!(!exists(dir.path(), 1, true), "oldest deleted");
        for s in 2..=6 {
            assert!(exists(dir.path(), s, true), "segment {s} kept");
        }
        assert!(exists(dir.path(), 7, false), "open segment kept");
        assert!(matches!(
            bus.read_from(1, 10),
            Err(ReadError::Dropped { oldest: 2 })
        ));
        assert_eq!(
            seqs(&bus.read_from(2, 10).unwrap()),
            (2..=7).collect::<Vec<_>>()
        );
    }

    #[test]
    fn retention_by_bytes_never_deletes_the_open_segment() {
        let dir = TempDir::new("bytes");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            window_records: 1,
            segment_secs: 10,
            retain_bytes: 0,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        for k in 0..4 {
            bus.publish("t", sample(k));
            bus.publish("t", sample(k));
            clock.advance_secs(10);
        }
        bus.publish("t", sample(9));
        flush(&bus);
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != LOCK_FILE)
            .collect();
        assert_eq!(names, vec![file_name(9, false)], "only the open segment");
        let s = bus.stats();
        assert_eq!(s.segments, 1);
        assert!(s.bytes > 0);
        assert!(matches!(
            bus.read_from(1, 10),
            Err(ReadError::Dropped { oldest: 9 })
        ));
    }

    #[test]
    fn reopen_continues_sequence_and_reads_old_records_from_disk() {
        let dir = TempDir::new("reopen");
        let clock = ManualClock::new(T0);
        let cfg = BusConfig {
            window_records: 4,
            ..disk_cfg(dir.path(), &clock)
        };
        let bus = Bus::open(cfg.clone()).unwrap();
        for i in 0..10 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        drop(bus);
        let bus = Bus::open(cfg).unwrap();
        assert_eq!(bus.head(), 10);
        assert_eq!(bus.publish("t", sample(10)), 11);
        assert_eq!(
            seqs(&bus.read_from(1, 100).unwrap()),
            (1..=11).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn log_dir_is_0700_and_segments_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("perms");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            segment_secs: 10,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        bus.publish("t", sample(0));
        clock.advance_secs(10);
        bus.publish("t", sample(1));
        flush(&bus);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.path()), 0o700, "log directory");
        let mut n = 0;
        for e in fs::read_dir(dir.path()).unwrap() {
            let p = e.unwrap().path();
            assert_eq!(mode(&p), 0o600, "{p:?}");
            n += 1;
        }
        assert_eq!(n, 3, "the lock file, one compressed and one open segment");
    }

    /// True when the current user can write into a 0500 directory (root).
    #[cfg(unix)]
    /// Put a plain file where the directory `dir` was, keeping the
    /// directory at `<dir>.away`. Creating anything under `dir` then fails
    /// with "not a directory" for every user, root included, so the tests
    /// below need no permission bits (which root ignores) to make the disk
    /// fail.
    fn break_dir(dir: &Path) {
        fs::rename(dir, away(dir)).unwrap();
        fs::write(dir, b"not a directory").unwrap();
    }

    /// Undo [`break_dir`].
    fn mend_dir(dir: &Path) {
        fs::remove_file(dir).unwrap();
        fs::rename(away(dir), dir).unwrap();
    }

    fn away(dir: &Path) -> PathBuf {
        let mut s = dir.as_os_str().to_os_string();
        s.push(".away");
        PathBuf::from(s)
    }

    /// A log path that cannot be used as a directory (a plain file is in
    /// the way): the bus opens with the disk off, keeps serving reads from
    /// memory and publishes one gap for the outage. (This used a 0500
    /// directory before, which root can write; a file in the way fails for
    /// every user.)
    #[test]
    fn an_unusable_dir_keeps_live_reads_and_publishes_one_gap() {
        let dir = TempDir::new("ro");
        fs::create_dir_all(dir.path()).unwrap();
        let log = dir.path().join("log");
        fs::write(&log, b"not a directory").unwrap();
        let clock = ManualClock::new(T0);
        let bus = Bus::open(disk_cfg(&log, &clock)).expect("open is Ok with disk off");
        assert!(!bus.stats().disk_on);
        for i in 0..10 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        let d1 = bus.stats().dropped_to_disk_errors;
        assert!(d1 > 0);
        for i in 0..10 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        assert!(bus.stats().dropped_to_disk_errors > d1, "rising");
        let recs = bus.read_from(1, 100).unwrap();
        assert_eq!(gaps(&recs), 1, "one gap per outage");
        assert_eq!(recs.len(), 21);
        let mut sub = bus.subscribe_from(bus.head()).unwrap();
        let s = bus.publish("t", sample(99));
        assert_eq!(
            sub.next(Duration::from_secs(1)).unwrap().map(|r| r.seq),
            Some(s)
        );
    }

    /// The directory goes away mid-run (a file takes its place), comes
    /// back, and goes away again: one gap per outage, and writing resumes
    /// in between. Runs the same as root.
    #[cfg(unix)]
    #[test]
    fn disk_failure_mid_run_publishes_one_gap_per_outage() {
        let root = TempDir::new("midrun");
        let dir = root.path().join("log");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            segment_secs: 10,
            ..disk_cfg(&dir, &clock)
        })
        .unwrap();
        for i in 0..3 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        break_dir(&dir);
        // Rotation needs a new file, which now fails.
        clock.advance_secs(10);
        bus.publish("t", sample(3));
        flush(&bus);
        for i in 0..5 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        assert!(!bus.stats().disk_on);
        assert_eq!(gaps(&bus.read_from(1, 100).unwrap()), 1);

        // The disk comes back; the next retry succeeds.
        mend_dir(&dir);
        clock.advance_secs(11);
        bus.publish("t", sample(0));
        flush(&bus);
        assert!(bus.stats().disk_on);

        // A second outage gets a second gap.
        break_dir(&dir);
        clock.advance_secs(10);
        bus.publish("t", sample(1));
        flush(&bus);
        bus.publish("t", sample(2));
        bus.publish("t", sample(3));
        flush(&bus);
        assert_eq!(gaps(&bus.read_from(1, 100).unwrap()), 2);
        let g = bus
            .read_from(1, 100)
            .unwrap()
            .into_iter()
            .find_map(|r| match r.payload {
                Payload::Gap(Gap { reason }) => Some(reason),
                _ => None,
            })
            .unwrap();
        assert!(!g.is_empty());
        mend_dir(&dir);
    }

    // ---- fix round 1 ----

    /// A subscription that fell behind the window gets a visible error at
    /// once. The old `next` turned it into `None`, and a view loop calling it
    /// spun at full CPU (over four million calls in 300 ms).
    #[test]
    fn lagged_subscription_returns_dropped_promptly() {
        let bus = mem_bus(10);
        let mut sub = bus.subscribe_from(bus.head()).unwrap();
        for i in 0..50 {
            bus.publish("t", sample(i));
        }
        let start = Instant::now();
        let r = sub.next(Duration::from_secs(5));
        assert!(matches!(r, Err(ReadError::Dropped { oldest: 41 })), "{r:?}");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
        // It stays an error: the caller must resubscribe.
        assert!(sub.next(Duration::from_secs(5)).is_err());
    }

    /// An idle subscription sleeps until the timeout. It does not poll.
    #[test]
    fn idle_subscription_waits_without_spinning() {
        let bus = mem_bus(10);
        bus.publish("t", sample(0));
        let mut sub = bus.subscribe_from(bus.head()).unwrap();
        let timeout = Duration::from_millis(200);
        let start = Instant::now();
        assert!(sub.next(timeout).unwrap().is_none());
        assert!(start.elapsed() >= timeout, "{:?}", start.elapsed());
        assert!(
            sub.reads <= 2,
            "read the log {} times while idle",
            sub.reads
        );
    }

    #[test]
    fn failed_segment_is_kept_counted_readable_and_retained() {
        let dir = TempDir::new("failseg");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            window_records: 1,
            segment_secs: 30,
            retain_secs: 60,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        for i in 0..5 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        let d = bus.inner.disk.clone().unwrap();
        d.hooks.fail_writes.store(true, Ordering::SeqCst);
        bus.publish("t", sample(5));
        flush(&bus);
        let on_disk = fs::metadata(dir.path().join(file_name(1, false)))
            .unwrap()
            .len();
        let s = bus.stats();
        assert!(!s.disk_on);
        assert_eq!(s.segments, 1, "failed segment still counted");
        assert_eq!(s.bytes, on_disk, "with its real size");
        // Records 1..=5 reached the disk and left memory; they are readable.
        assert_eq!(seqs(&bus.read_from(1, 5).unwrap()), vec![1, 2, 3, 4, 5]);

        // The disk recovers: a new segment opens on the next retry.
        d.hooks.fail_writes.store(false, Ordering::SeqCst);
        clock.advance_secs(11);
        bus.publish("t", sample(6));
        flush(&bus);
        assert!(bus.stats().disk_on);
        assert_eq!(bus.stats().segments, 2);
        // Once old enough, retention removes the failed segment.
        clock.advance_secs(61);
        bus.publish("t", sample(7));
        flush(&bus);
        assert!(!exists(dir.path(), 1, false) && !exists(dir.path(), 1, true));
    }

    #[test]
    fn dropping_the_bus_flushes_every_queued_record() {
        let dir = TempDir::new("dropflush");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(disk_cfg(dir.path(), &clock)).unwrap();
        // Keep the writer behind the publisher.
        let d = bus.inner.disk.clone().unwrap();
        d.hooks.batch_delay_ms.store(50, Ordering::SeqCst);
        for i in 0..1000 {
            bus.publish("t", sample(i));
        }
        drop(bus);
        let r = segment::read_segment(&dir.path().join(file_name(1, false))).unwrap();
        assert_eq!(seqs(&r.records), (1..=1000).collect::<Vec<_>>());
    }

    #[test]
    fn dropping_the_bus_flushes_even_with_a_live_subscription() {
        let dir = TempDir::new("dropsub");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(disk_cfg(dir.path(), &clock)).unwrap();
        let d = bus.inner.disk.clone().unwrap();
        d.hooks.batch_delay_ms.store(50, Ordering::SeqCst);
        let sub = bus.subscribe_from(0).unwrap();
        for i in 0..1000 {
            bus.publish("t", sample(i));
        }
        drop(bus);
        let r = segment::read_segment(&dir.path().join(file_name(1, false))).unwrap();
        assert_eq!(seqs(&r.records), (1..=1000).collect::<Vec<_>>());
        drop(sub);
    }

    #[test]
    fn a_second_bus_on_one_directory_is_refused_until_the_first_is_dropped() {
        let dir = TempDir::new("lock");
        let clock = ManualClock::new(T0);
        let first = Bus::open(disk_cfg(dir.path(), &clock)).unwrap();
        let err = Bus::open(disk_cfg(dir.path(), &clock)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
        // Hardening B6: the holder here is this process's own bus, so the
        // message must not say only "another process".
        assert!(
            err.to_string().contains(
                "this log directory is already open (another process, or a bus in \
                 this process that is still shutting down)"
            ),
            "{err}"
        );
        // A clone shares the lock; only dropping the last handle frees it.
        let clone = first.clone();
        drop(first);
        assert!(Bus::open(disk_cfg(dir.path(), &clock)).is_err());
        drop(clone);
        let again = Bus::open(disk_cfg(dir.path(), &clock)).unwrap();
        assert!(again.stats().disk_on);
    }

    fn segment_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .map(|rd| {
                rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("bus-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A filesystem that cannot lock (ENOLCK, as on an NFS home mounted
    /// `nolock`) refuses the open. Running unlocked would let two hosts write.
    #[test]
    fn a_filesystem_that_cannot_lock_refuses_to_open() {
        let dir = TempDir::new("enolck");
        let clock = ManualClock::new(T0);
        #[cfg(unix)]
        let enolck = || io::Error::from_raw_os_error(libc::ENOLCK);
        #[cfg(not(unix))]
        let enolck = || io::Error::other("No locks available");
        let locker: Locker =
            Arc::new(move |d: &Path| Err(LockError::Lock(d.to_path_buf(), enolck())));
        let err =
            Bus::open_inner(disk_cfg(dir.path(), &clock), Box::new(NoReducer), locker).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cannot lock log directory"), "{msg}");
        #[cfg(unix)]
        assert!(msg.contains(&format!("os error {}", libc::ENOLCK)), "{msg}");
        assert!(msg.contains("--bus-dir on a local filesystem"), "{msg}");
        assert!(segment_files(dir.path()).is_empty());
    }

    /// The lock file cannot be created at open (a disk outage). When the
    /// writer retries later it must take the lock first: while the lock still
    /// cannot be taken nothing is written, and once it can, writing resumes.
    #[test]
    fn a_recovered_disk_is_not_written_without_the_lock() {
        let dir = TempDir::new("relock");
        fs::create_dir_all(dir.path()).unwrap();
        let clock = ManualClock::new(T0);
        let allow = Arc::new(AtomicBool::new(false));
        let a = allow.clone();
        let locker: Locker = Arc::new(move |d: &Path| {
            if a.load(Ordering::SeqCst) {
                lock_dir(d)
            } else {
                Err(LockError::Open(io::Error::other("injected: no lock file")))
            }
        });
        let bus =
            Bus::open_inner(disk_cfg(dir.path(), &clock), Box::new(NoReducer), locker).unwrap();
        assert!(!bus.stats().disk_on);
        bus.publish("t", sample(0));
        // Past the 10 s retry: the directory is writable, the lock is not.
        clock.advance_secs(11);
        bus.publish("t", sample(1));
        clock.advance_secs(11);
        bus.publish("t", sample(2));
        flush(&bus);
        assert!(
            segment_files(dir.path()).is_empty(),
            "written without the lock: {:?}",
            segment_files(dir.path())
        );
        assert!(bus.stats().dropped_to_disk_errors > 0);
        // The lock can be taken now: the next retry writes.
        allow.store(true, Ordering::SeqCst);
        clock.advance_secs(11);
        bus.publish("t", sample(3));
        flush(&bus);
        assert!(!segment_files(dir.path()).is_empty());
        assert!(bus.stats().disk_on);
    }

    /// A locker that fails like a missing lock file on its first call (the
    /// open) and takes the real lock on every later call (the writer).
    fn fail_first_lock() -> Locker {
        let calls = Arc::new(AtomicU64::new(0));
        Arc::new(move |d: &Path| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(LockError::Open(io::Error::other("injected: no lock file")))
            } else {
                lock_dir(d)
            }
        })
    }

    /// Hardening B2: a header-only last segment (left by a run that
    /// published nothing) is skipped, not deleted, when the lock cannot be
    /// taken at open. Once a later retry takes the lock, the cleanup runs:
    /// the file goes, and the next record can use its sequence number.
    #[test]
    fn a_header_only_segment_is_cleaned_up_once_a_late_lock_is_taken() {
        let dir = TempDir::new("latelock");
        let clock = ManualClock::new(T0);
        {
            let bus = Bus::open(disk_cfg(dir.path(), &clock)).unwrap();
            bus.publish("t", sample(0));
            bus.publish("t", sample(1));
            flush(&bus);
        }
        // A run that published nothing leaves segment 3 with a header only.
        drop(Bus::open(disk_cfg(dir.path(), &clock)).unwrap());
        assert!(exists(dir.path(), 3, false));

        let locker = fail_first_lock();
        let bus =
            Bus::open_inner(disk_cfg(dir.path(), &clock), Box::new(NoReducer), locker).unwrap();
        assert!(!bus.stats().disk_on);
        assert!(exists(dir.path(), 3, false), "left alone without the lock");
        // The open published its outage Gap as seq 3. Writing it takes the
        // lock (the second call succeeds) and opens segment 3.
        assert_eq!(bus.publish("t", sample(2)), 4);
        flush(&bus);
        assert!(bus.stats().disk_on, "{:?}", bus.stats());
        assert_eq!(bus.stats().dropped_to_disk_errors, 0);
        let r = segment::read_segment(&dir.path().join(file_name(3, false))).unwrap();
        assert_eq!(seqs(&r.records), vec![3, 4], "records 3 and 4 in segment 3");
        assert_eq!(gaps(&r.records), 1);
        // The table holds the old segment and the new one, nothing else.
        let recs = bus.inner.read_disk(1, 10, u64::MAX).unwrap();
        assert_eq!(seqs(&recs), vec![1, 2, 3, 4]);
    }

    /// The late lock also refreshes the segment table: a directory that
    /// could not be listed at open (so the table started empty) is read
    /// again, and its old segments become readable.
    #[cfg(unix)]
    #[test]
    fn a_late_lock_refreshes_the_segment_table() {
        let dir = TempDir::new("latetable");
        let clock = ManualClock::new(T0);
        {
            let bus = Bus::open(disk_cfg(dir.path(), &clock)).unwrap();
            bus.publish("t", sample(0));
            bus.publish("t", sample(1));
            flush(&bus);
        }
        // Unlistable at open, and the lock file cannot be made.
        chmod(dir.path(), 0o000);
        if fs::read_dir(dir.path()).is_ok() {
            // Root ignores the mode; nothing to test here.
            chmod(dir.path(), 0o700);
            return;
        }
        let locker = fail_first_lock();
        let opened = Bus::open_inner(
            BusConfig {
                window_records: 1,
                ..disk_cfg(dir.path(), &clock)
            },
            Box::new(NoReducer),
            locker,
        );
        chmod(dir.path(), 0o700);
        let bus = opened.unwrap();
        let table = |b: &Bus| -> Vec<u64> {
            let t = lock(&b.inner.disk.as_ref().unwrap().table);
            t.closed.iter().map(|g| g.first_seq).collect()
        };
        assert!(table(&bus).is_empty());
        // The open published its outage Gap, and the writer may have tried
        // it while the directory was still unreadable: go past the retry
        // window.
        clock.advance_secs(11);
        bus.publish("t", sample(2));
        flush(&bus);
        // (The sequence restarted at 1 because the open could not list the
        // directory; `a_late_lock_continues_numbering_after_the_disk` covers
        // what happens to those numbers. The table is what this test is
        // about.)
        assert_eq!(
            table(&bus),
            vec![1],
            "old segment back in the table; {:?} {}",
            bus.stats(),
            lock(&bus.inner.disk.as_ref().unwrap().reason)
        );
    }

    /// Final fix wave S1: the open could not list the directory (here the
    /// path was a plain file at open, so no root-only trick is needed), so
    /// the bus numbered from 1. When the lock is taken late on a directory
    /// that holds records 1 to 5, the records published before the lock
    /// keep their numbers in memory but are not written, the head jumps
    /// past 5 with a Gap saying why, and the directory's numbers stay
    /// unique and increasing.
    #[test]
    fn a_late_lock_continues_numbering_after_the_disk() {
        let root = TempDir::new("lateseq");
        let clock = ManualClock::new(T0);
        // A log of records 1..=5 in another directory, moved in later.
        let other = root.path().join("other");
        {
            let bus = Bus::open(disk_cfg(&other, &clock)).unwrap();
            for i in 0..5 {
                bus.publish("t", sample(i));
            }
            flush(&bus);
        }
        let log = root.path().join("log");
        fs::write(&log, b"not a directory").unwrap();
        let bus = Bus::open_inner(
            disk_cfg(&log, &clock),
            Box::new(NoReducer),
            fail_first_lock(),
        )
        .unwrap();
        assert!(!bus.stats().disk_on);
        assert_eq!(bus.head(), 1, "the open's Gap is record 1");
        flush(&bus);
        fs::remove_file(&log).unwrap();
        fs::rename(&other, &log).unwrap();
        // Past the retry window. This record may take the lock (then it is
        // dropped as an overlap) or may come after the lock was taken.
        clock.advance_secs(11);
        let a = bus.publish("t", sample(10));
        flush(&bus);
        let b = bus.publish("t", sample(11));
        flush(&bus);
        assert!(b > 5, "numbering continues after the disk: a={a} b={b}");
        assert!(bus.stats().disk_on, "{:?}", bus.stats());
        let recs = bus.inner.read_disk(1, 100, u64::MAX).unwrap();
        let got = seqs(&recs);
        assert!(
            got.windows(2).all(|w| w[0] < w[1]),
            "strictly increasing: {got:?}"
        );
        assert_eq!(&got[..5], &[1, 2, 3, 4, 5], "the old records: {got:?}");
        assert!(got.contains(&b), "{got:?}");
        let gap = recs
            .iter()
            .find_map(|r| match &r.payload {
                Payload::Gap(g) if r.seq > 5 => Some(g.reason.clone()),
                _ => None,
            })
            .expect("a Gap after the jump");
        assert!(gap.contains("already held records up to 5"), "{gap}");
        // The replay of the directory finds no repeated numbers.
        let mut seen = std::collections::BTreeSet::new();
        for r in crate::Replay::open(&log).unwrap().records() {
            assert!(seen.insert(r.seq), "sequence {} twice on disk", r.seq);
        }
    }

    /// After a late lock moves the head forward, the in-memory window must
    /// not answer a read with records whose numbers differ from the ones
    /// asked for. Before the fix `copy_window` indexed by `seq - front.seq`
    /// across the jump and returned the wrong records or none.
    #[test]
    fn reads_across_a_late_lock_jump_never_return_the_wrong_records() {
        let root = TempDir::new("lateread");
        let clock = ManualClock::new(T0);
        let other = root.path().join("other");
        {
            let bus = Bus::open(disk_cfg(&other, &clock)).unwrap();
            for i in 0..50 {
                bus.publish("t", sample(i));
            }
            flush(&bus);
        }
        let log = root.path().join("log");
        fs::write(&log, b"not a directory").unwrap();
        let bus = Bus::open_inner(
            disk_cfg(&log, &clock),
            Box::new(NoReducer),
            fail_first_lock(),
        )
        .unwrap();
        for i in 0..3 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        fs::remove_file(&log).unwrap();
        fs::rename(&other, &log).unwrap();
        clock.advance_secs(11);
        for i in 0..6 {
            bus.publish("t", sample(100 + i));
            flush(&bus);
        }
        let head = bus.head();
        assert!(head > 50, "the head jumped past the disk: {head}");
        for s in 1..=head {
            match bus.read_from(s, 20) {
                Ok(recs) => {
                    for (i, r) in recs.iter().enumerate() {
                        assert_eq!(
                            r.seq,
                            s + i as u64,
                            "read_from({s}) returned {:?}",
                            seqs(&recs)
                        );
                    }
                }
                Err(_) => {}
            }
        }
        // A reader behind the jump is told, not served the other run's records.
        match bus.read_from(4, 10) {
            Err(ReadError::Dropped { oldest }) => assert!(oldest > 50, "oldest {oldest}"),
            other => panic!("a read behind the jump must be Dropped, got {other:?}"),
        }
        // A reader at the first number after the jump still gets records.
        let tail = bus.read_from(head, 5).unwrap();
        assert_eq!(seqs(&tail), vec![head]);
    }

    /// A disk config with `backend` and `mode` header tags.
    fn tagged(dir: &Path, clock: &Arc<ManualClock>, backend: &str, mode: &str) -> BusConfig {
        let mut cfg = disk_cfg(dir, clock);
        cfg.header_tags.insert("backend".into(), backend.into());
        cfg.header_tags.insert("mode".into(), mode.into());
        cfg
    }

    /// Open `cfg`, publish one record, flush and drop.
    fn record_one(cfg: BusConfig) {
        let bus = Bus::open(cfg).unwrap();
        bus.publish("t", sample(0));
        flush(&bus);
    }

    /// Hardening B3: a directory whose newest segment was written for
    /// another mode, or another backend class, is not reused.
    #[test]
    fn a_directory_of_another_class_or_mode_is_refused() {
        let dir = TempDir::new("mixmode");
        let clock = ManualClock::new(T0);
        record_one(tagged(dir.path(), &clock, "sysfs", "live"));
        // Live then mock, local hardware then a remote relay, and a mode
        // change within one class.
        for (backend, mode) in [("mock", "mock"), ("remote", "live"), ("sysfs", "replay")] {
            let err = Bus::open(tagged(dir.path(), &clock, backend, mode)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
            let msg = err.to_string();
            assert!(
                msg.contains(
                    "log directory holds a local-hardware/live log; use another --bus-dir or \
                     remove it"
                ),
                "{msg}"
            );
        }
        // Nothing was written by the refused opens, and the same backend
        // and mode still open.
        assert_eq!(segment_files(dir.path()).len(), 1);
        record_one(tagged(dir.path(), &clock, "sysfs", "live"));
    }

    /// Fix round 1: `--backend auto` can pick `hybrid` on one boot and fall
    /// back to `json` on the next, and someone may run `--backend sysfs`
    /// once to debug. A change within the local-hardware class is accepted.
    #[test]
    fn local_hardware_backends_share_a_directory() {
        let dir = TempDir::new("mixlocal");
        let clock = ManualClock::new(T0);
        for backend in ["sysfs", "hybrid", "json", "luwen", "host", "sysfs"] {
            record_one(tagged(dir.path(), &clock, backend, "live"));
        }
        // Mock after mock opens, and a remote relay after a remote relay.
        let mock = TempDir::new("mixmock");
        record_one(tagged(mock.path(), &clock, "mock", "mock"));
        record_one(tagged(mock.path(), &clock, "mock", "mock"));
        let remote = TempDir::new("mixremote");
        record_one(tagged(remote.path(), &clock, "remote", "live"));
        record_one(tagged(remote.path(), &clock, "remote", "live"));
        let err = Bus::open(tagged(remote.path(), &clock, "hybrid", "live")).unwrap_err();
        assert!(err.to_string().contains("holds a remote/live log"), "{err}");
    }

    /// An empty directory, a log with no tags (written before tags
    /// existed) and an opener with no tags are all accepted.
    #[test]
    fn untagged_logs_and_empty_directories_are_accepted() {
        let clock = ManualClock::new(T0);
        let empty = TempDir::new("mixempty");
        record_one(tagged(empty.path(), &clock, "mock", "mock"));
        let old = TempDir::new("mixold");
        record_one(disk_cfg(old.path(), &clock));
        record_one(tagged(old.path(), &clock, "sysfs", "live"));
        // An opener with no tags does not check.
        record_one(disk_cfg(old.path(), &clock));
    }

    /// Only the newest segment counts: after an untagged run, a different
    /// backend may continue, and then the older one is refused.
    #[test]
    fn the_newest_segment_decides() {
        let dir = TempDir::new("mixnewest");
        let clock = ManualClock::new(T0);
        record_one(tagged(dir.path(), &clock, "sysfs", "live"));
        record_one(disk_cfg(dir.path(), &clock));
        record_one(tagged(dir.path(), &clock, "mock", "mock"));
        let err = Bus::open(tagged(dir.path(), &clock, "sysfs", "live")).unwrap_err();
        assert!(err.to_string().contains("holds a mock/mock log"), "{err}");
    }

    /// Fix round 1: the class and mode check runs again when the lock is
    /// taken late. Here the directory was empty at open (lock unavailable)
    /// and a mock log was written into it during the outage. The live bus
    /// must not add to it: the disk goes off with a Gap naming the reason,
    /// and later records are not written either.
    #[test]
    fn a_late_lock_refuses_a_directory_of_another_class() {
        let dir = TempDir::new("latemix");
        fs::create_dir_all(dir.path()).unwrap();
        let clock = ManualClock::new(T0);
        let allow = Arc::new(AtomicBool::new(false));
        let a = allow.clone();
        let locker: Locker = Arc::new(move |d: &Path| {
            if a.load(Ordering::SeqCst) {
                lock_dir(d)
            } else {
                Err(LockError::Open(io::Error::other("injected: no lock file")))
            }
        });
        let bus = Bus::open_inner(
            tagged(dir.path(), &clock, "sysfs", "live"),
            Box::new(NoReducer),
            locker,
        )
        .unwrap();
        // The open's outage Gap has been tried (and refused) by now.
        flush(&bus);
        // Another run records a mock log while this bus has no lock.
        record_one(tagged(dir.path(), &clock, "mock", "mock"));
        let before = segment_files(dir.path());
        allow.store(true, Ordering::SeqCst);
        clock.advance_secs(11);
        bus.publish("t", sample(1));
        flush(&bus);
        assert_eq!(segment_files(dir.path()), before, "added to a mock log");
        assert!(!bus.stats().disk_on);
        // Still refused at the next retry. The refusal's Gap goes out with
        // the next publish.
        clock.advance_secs(11);
        bus.publish("t", sample(2));
        flush(&bus);
        assert_eq!(segment_files(dir.path()), before);
        // After a refusal the writer does not retry the directory, so no
        // second refusal Gap follows.
        clock.advance_secs(11);
        bus.publish("t", sample(3));
        flush(&bus);
        clock.advance_secs(11);
        bus.publish("t", sample(4));
        flush(&bus);
        assert_eq!(segment_files(dir.path()), before);
        let recs = bus.read_from(1, 100).unwrap();
        let reasons: Vec<String> = recs
            .iter()
            .filter_map(|r| match &r.payload {
                Payload::Gap(g) => Some(g.reason.clone()),
                _ => None,
            })
            .collect();
        let refusals = reasons
            .iter()
            .filter(|r| r.contains("log directory holds a mock/mock log"))
            .count();
        assert_eq!(refusals, 1, "{reasons:?}");
    }

    #[test]
    fn header_tags_are_in_the_header_and_every_segment() {
        let dir = TempDir::new("tags");
        let clock = ManualClock::new(T0);
        let mut cfg = disk_cfg(dir.path(), &clock);
        cfg.header_tags.insert("backend".into(), "mock".into());
        cfg.header_tags.insert("mode".into(), "mock".into());
        // A tag never replaces the crate's own entry.
        cfg.header_tags
            .insert("tt-toplike-bus".into(), "9.9.9".into());
        let bus = Bus::open(cfg).unwrap();
        let h = bus.header();
        assert_eq!(h.tool_versions["backend"], "mock");
        assert_eq!(h.tool_versions["mode"], "mock");
        assert_eq!(h.tool_versions["tt-toplike-bus"], env!("CARGO_PKG_VERSION"));
        bus.publish("t", sample(0));
        flush(&bus);
        let r = segment::read_segment(&dir.path().join(file_name(1, false))).unwrap();
        assert_eq!(r.header.unwrap(), h);
    }

    #[test]
    fn two_threads_opening_one_directory_get_exactly_one_bus() {
        for round in 0..20 {
            let dir = TempDir::new("lockrace");
            let clock = ManualClock::new(T0);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let cfg = disk_cfg(dir.path(), &clock);
                    let b = barrier.clone();
                    thread::spawn(move || {
                        b.wait();
                        Bus::open(cfg)
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let ok = results.iter().filter(|r| r.is_ok()).count();
            assert_eq!(ok, 1, "round {round}: {ok} buses opened");
        }
    }

    #[test]
    fn maintain_applies_age_retention_on_an_idle_bus() {
        let dir = TempDir::new("maintain");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            window_records: 1,
            segment_secs: 10,
            retain_secs: 60,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        // Segment 1 closes at T0+10s when record 2 rotates the log.
        bus.publish("t", sample(0));
        clock.advance_secs(10);
        bus.publish("t", sample(1));
        flush(&bus);
        assert!(exists(dir.path(), 1, true));
        // Nothing is published for two minutes. Retention used to run only
        // at a rotation, so segment 1 stayed forever on an idle bus.
        clock.advance_secs(120);
        flush(&bus);
        assert!(exists(dir.path(), 1, true), "flush alone does not age");
        assert!(bus.maintain(Duration::from_secs(10)));
        assert!(!exists(dir.path(), 1, true), "maintain deleted it");
        assert!(exists(dir.path(), 2, false), "the open segment is kept");
    }

    #[test]
    fn request_maintenance_applies_retention_without_waiting() {
        let dir = TempDir::new("reqmaint");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            window_records: 1,
            segment_secs: 10,
            retain_secs: 60,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        bus.publish("t", sample(0));
        clock.advance_secs(10);
        bus.publish("t", sample(1));
        clock.advance_secs(120);
        assert!(bus.request_maintenance());
        // Messages are handled in order, so after this flush it has run.
        flush(&bus);
        assert!(!exists(dir.path(), 1, true));
    }

    #[test]
    fn maintain_on_a_memory_bus_returns_at_once() {
        assert!(mem_bus(4).maintain(Duration::from_millis(1)));
    }

    #[test]
    fn bus_and_subscription_are_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<Bus>();
        check::<Subscription>();
    }
}
