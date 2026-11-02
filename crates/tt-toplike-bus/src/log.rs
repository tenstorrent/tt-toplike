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
use crate::record::{Gap, Header, HeaderKind, Payload, Record, SCHEMA_VERSION};
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

/// Folds records into derived state (the world, in Task 3).
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
    /// Written into the header line of every new segment.
    kind: HeaderKind,
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
    /// The open `<dir>/.lock` file holding the directory's exclusive lock.
    /// Closing it (when this guard drops, after the writer has finished)
    /// releases the lock. `None` for a memory-only bus.
    _dir_lock: Option<File>,
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
    pub fn open_with(cfg: BusConfig, reducer: Box<dyn Reducer>) -> io::Result<Bus> {
        let window_records = cfg.window_records.max(1);
        let mut head = 0;
        let mut tx = None;
        let mut thread_handle = None;
        let mut done = None;
        let mut disk = None;
        let mut startup_failure = None;
        let mut dir_lock = None;

        if let Some(dir) = cfg.dir.clone() {
            // One writer per directory, across processes. A held lock is the
            // only error `open` returns for a directory problem: two writers
            // would corrupt each other's segments. A lock that cannot be
            // taken for another reason (the directory cannot be created or
            // is not writable) is a disk outage like any other, below.
            let lock_failure = match lock_dir(&dir) {
                Ok(f) => {
                    dir_lock = Some(f);
                    None
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(e),
                Err(e) => Some(format!("cannot lock log directory {dir:?}: {e}")),
            };
            let d = Arc::new(Disk {
                dir: dir.clone(),
                table: Mutex::new(SegTable::default()),
                failed: AtomicBool::new(false),
                gap_pending: AtomicBool::new(false),
                reason: Mutex::new(String::new()),
                dropped: AtomicU64::new(0),
                kind: cfg.header_kind,
                #[cfg(test)]
                hooks: TestHooks::default(),
            });
            let mut first = None;
            match prepare(&dir) {
                Ok((closed, last)) => {
                    head = last;
                    lock(&d.table).closed = closed;
                    match open_segment(&dir, head + 1, cfg.header_kind) {
                        Ok(seg) => first = Some(seg),
                        Err(e) => startup_failure = Some(format!("cannot open log segment: {e}")),
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
                tx,
            }),
            cond: Condvar::new(),
            window_records,
            clock: cfg.clock,
            disk,
            header_kind: cfg.header_kind,
            instance: new_instance(),
        });
        let bus = Bus {
            _writer: Arc::new(WriterGuard {
                inner: inner.clone(),
                thread: thread_handle,
                done: Mutex::new(done),
                _dir_lock: dir_lock,
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
        Header {
            schema: SCHEMA_VERSION,
            kind: self.inner.header_kind,
            tool_versions: segment::tool_versions(),
        }
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
/// dies. Another holder gives `WouldBlock` with the message
/// `log directory is in use by another process: <dir>`. The lock file is
/// never deleted: deleting it would let a third opener lock a new file
/// while the second still holds the old one.
fn lock_dir(dir: &Path) -> io::Result<File> {
    segment::create_dir_secure(dir)?;
    let mut o = std::fs::OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let f = o.open(dir.join(LOCK_FILE))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "log directory is in use by another process: {}",
                dir.display()
            ),
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Create the directory, list existing segments and find the newest
/// sequence number on disk. A header-only plain segment at the end (opened
/// by a run that published nothing) is removed so its name can be reused.
fn prepare(dir: &Path) -> io::Result<(Vec<SegInfo>, u64)> {
    segment::create_dir_secure(dir)?;
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
                std::fs::remove_file(&p)?;
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

/// The segment the writer appends to.
struct OpenSeg {
    first_seq: u64,
    start_ms: u64,
    file: BufWriter<File>,
    bytes: u64,
}

/// Create segment `first_seq` with its header line.
fn open_segment(dir: &Path, first_seq: u64, kind: HeaderKind) -> io::Result<OpenSeg> {
    segment::create_dir_secure(dir)?;
    let mut file = BufWriter::new(segment::create_file_secure(&segment::path(
        dir, first_seq, false,
    ))?);
    let bytes = segment::write_header(&mut file, kind)?;
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
}

impl Writer {
    fn run(mut self, rx: Receiver<Msg>) {
        // Segments left plain by an earlier run that stopped: compress them
        // and apply retention before taking new records.
        let now = self.clock.wall_ms();
        let _ = self.compress_closed();
        self.retain(now);
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
                let now = self.clock.wall_ms();
                self.retain(now);
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
        let now = rec.t_wall_ms;
        let due = self
            .open
            .as_ref()
            .is_some_and(|o| now.saturating_sub(o.start_ms) >= self.segment_ms);
        if due {
            self.rotate(now)?;
        }
        if self.open.is_none() {
            let mut seg = open_segment(&self.disk.dir, rec.seq, self.disk.kind)?;
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
    fn permissions_are_ignored(dir: &Path) -> bool {
        let probe = dir.join("probe");
        let ok = fs::write(&probe, b"x").is_ok();
        let _ = fs::remove_file(&probe);
        ok
    }

    #[cfg(unix)]
    #[test]
    fn read_only_dir_keeps_live_reads_and_publishes_one_gap() {
        let dir = TempDir::new("ro");
        fs::create_dir_all(dir.path()).unwrap();
        chmod(dir.path(), 0o500);
        if permissions_are_ignored(dir.path()) {
            eprintln!("skipped: running with permission checks off (root)");
            return;
        }
        let clock = ManualClock::new(T0);
        let bus = Bus::open(disk_cfg(dir.path(), &clock)).expect("open is Ok with disk off");
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

    #[cfg(unix)]
    #[test]
    fn disk_failure_mid_run_publishes_one_gap_per_outage() {
        let dir = TempDir::new("midrun");
        let clock = ManualClock::new(T0);
        let bus = Bus::open(BusConfig {
            segment_secs: 10,
            ..disk_cfg(dir.path(), &clock)
        })
        .unwrap();
        for i in 0..3 {
            bus.publish("t", sample(i));
        }
        flush(&bus);
        chmod(dir.path(), 0o500);
        if permissions_are_ignored(dir.path()) {
            eprintln!("skipped: running with permission checks off (root)");
            return;
        }
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
        chmod(dir.path(), 0o700);
        clock.advance_secs(11);
        bus.publish("t", sample(0));
        flush(&bus);
        assert!(bus.stats().disk_on);

        // A second outage gets a second gap.
        chmod(dir.path(), 0o500);
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
        assert!(
            err.to_string()
                .contains("log directory is in use by another process"),
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
