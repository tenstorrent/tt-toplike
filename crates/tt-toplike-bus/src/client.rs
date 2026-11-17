//! The client side of the socket transport (see [`crate::socket`] for the
//! protocol).
//!
//! * [`SocketClient`] makes requests on one connection: `snapshot`, `raw`,
//!   and `subscribe_from`, which turns the connection into a
//!   [`RemoteSubscription`].
//! * [`RemoteWorld`] keeps a [`RemoteView`] of a server's world current on a
//!   background thread: it takes a snapshot, subscribes from the snapshot's
//!   sequence number, and folds every record in with
//!   [`WorldSummary::apply`].
//!
//! # Reconnecting
//!
//! Every reconnect takes a fresh snapshot, which replaces the view, and
//! subscribes from it. A server restarted on the same log directory
//! continues the sequence numbers with an empty world, and one restarted
//! after a crash can reuse numbers, so the old connection's sequence
//! number says nothing about the new bus.
//!
//! * When the connection is lost, the first retry waits 1 s, then 2 s, 4 s,
//!   8 s, then every 10 s.
//! * When the server still answers but the position is lost, a new
//!   snapshot is taken at once. That covers `dropped`, `future`, `too_slow`,
//!   a record that does not follow the view's sequence number by exactly
//!   one (a gap or a repeat), and a `subscribed` header whose bus instance
//!   differs from the snapshot's. Consecutive snapshots are still at least
//!   250 ms apart, so a server that keeps losing the position (or has a
//!   sequence bug) gets at most four snapshot requests a second.
//! * A line from the server longer than 16 MiB is a decode error and ends
//!   the connection.
//!
//! # Pruning
//!
//! Records are folded in through a [`SummaryPruner`], which drops quiet
//! churning entities (processes, leases, training runs, models never
//! announced) by the rule `World` uses, so a view kept current for days
//! stays about as large as the server's world. Each snapshot starts a new
//! pruner.

use crate::record::{HeaderKind, Record};
use crate::replay::{PlayClock, RealClock};
use crate::socket::{ErrorLine, RawReply, SubscribedLine};
use crate::summary::{EventSummary, ReadingSummary, RemoteSnapshot, SummaryPruner, WorldSummary};
use crate::world::ModelView;
use crate::EntityKey;
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long a request waits for its reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest line the client accepts from a server (16 MiB). A longer one is
/// a [`ClientError::Decode`] and ends the connection.
const MAX_LINE: usize = 16 << 20;

/// How often the [`RemoteWorld`] thread checks whether it should stop.
const TICK: Duration = Duration::from_millis(100);

/// First reconnect delay; it doubles up to [`BACKOFF_MAX`].
const BACKOFF_FIRST: Duration = Duration::from_secs(1);

/// Longest reconnect delay.
const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// Shortest time between the start of one snapshot request and the next.
/// A server that keeps answering `too_slow`, `dropped` or `future`, or has
/// a sequence bug, makes the follower take a new snapshot "at once" every
/// time; this floor keeps that to at most four snapshots a second.
const SNAPSHOT_FLOOR: Duration = Duration::from_millis(250);

/// Why a remote read failed.
#[derive(Debug)]
pub enum ClientError {
    /// The record after `from` is no longer kept; `oldest` is the oldest
    /// readable one. Take a fresh snapshot.
    Dropped { oldest: u64 },
    /// `from` is past the server's newest record `head`.
    Future { head: u64 },
    /// The server cut this client for falling too far behind.
    TooSlow,
    /// Any other error line from the server.
    Server {
        error: String,
        detail: Option<String>,
    },
    /// The server closed the connection.
    Closed,
    /// A line from the server did not decode.
    Decode(String),
    /// The socket failed.
    Io(io::Error),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Dropped { oldest } => {
                write!(f, "records before {oldest} are no longer kept")
            }
            ClientError::Future { head } => write!(f, "the newest record is {head}"),
            ClientError::TooSlow => write!(f, "the server cut this client for reading too slowly"),
            ClientError::Server { error, detail } => match detail {
                Some(d) => write!(f, "server error {error}: {d}"),
                None => write!(f, "server error {error}"),
            },
            ClientError::Closed => write!(f, "the server closed the connection"),
            ClientError::Decode(d) => write!(f, "cannot decode a line from the server: {d}"),
            ClientError::Io(e) => write!(f, "socket error: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ErrorLine> for ClientError {
    fn from(e: ErrorLine) -> Self {
        match e.error.as_str() {
            "dropped" => ClientError::Dropped {
                oldest: e.oldest.unwrap_or(0),
            },
            "future" => ClientError::Future {
                head: e.head.unwrap_or(0),
            },
            "too_slow" => ClientError::TooSlow,
            _ => ClientError::Server {
                error: e.error,
                detail: e.detail,
            },
        }
    }
}

impl From<ClientError> for io::Error {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Io(e) => e,
            ClientError::Closed => io::Error::new(io::ErrorKind::UnexpectedEof, e),
            other => io::Error::other(other),
        }
    }
}

/// Lines read from a socket, with the bytes after the last full line kept.
struct Lines {
    stream: UnixStream,
    buf: Vec<u8>,
    /// Bytes of `buf` already searched for a newline.
    scanned: usize,
}

impl Lines {
    /// The next line without its newline; `Ok(None)` when `timeout` passes
    /// first. A partial line stays buffered for the next call.
    fn next_line(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, ClientError> {
        let deadline = Instant::now() + timeout;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if let Some(p) = self.buf[self.scanned..].iter().position(|&b| b == b'\n') {
                let end = self.scanned + p;
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                line.pop();
                self.scanned = 0;
                return Ok(Some(line));
            }
            self.scanned = self.buf.len();
            if self.buf.len() > MAX_LINE {
                return Err(ClientError::Decode(format!(
                    "a line from the server is longer than {MAX_LINE} bytes"
                )));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.stream
                .set_read_timeout(Some(left.max(Duration::from_millis(1))))
                .map_err(ClientError::Io)?;
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(ClientError::Closed),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {
                    return Err(ClientError::Closed)
                }
                Err(e) => return Err(ClientError::Io(e)),
            }
        }
    }

    /// The next line, waiting at most [`REPLY_TIMEOUT`].
    fn reply(&mut self) -> Result<Vec<u8>, ClientError> {
        self.reply_within(REPLY_TIMEOUT)
    }

    /// The next line, waiting at most `timeout`.
    fn reply_within(&mut self, timeout: Duration) -> Result<Vec<u8>, ClientError> {
        self.next_line(timeout)?.ok_or_else(|| {
            ClientError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "no reply from the bus socket",
            ))
        })
    }
}

/// Decode a reply line as `T`, or as an error line.
fn decode<T: serde::de::DeserializeOwned>(line: &[u8]) -> Result<T, ClientError> {
    match serde_json::from_slice::<T>(line) {
        Ok(v) => Ok(v),
        Err(e) => match serde_json::from_slice::<ErrorLine>(line) {
            Ok(err) => Err(err.into()),
            Err(_) => Err(ClientError::Decode(e.to_string())),
        },
    }
}

/// One connection to a bus socket.
pub struct SocketClient {
    /// `None` once `subscribe_from` has taken the connection.
    lines: Option<Lines>,
}

impl fmt::Debug for SocketClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketClient")
            .field("open", &self.lines.is_some())
            .finish()
    }
}

impl SocketClient {
    /// Connect to the server at `path`.
    pub fn connect(path: &Path) -> io::Result<SocketClient> {
        let stream = UnixStream::connect(path)?;
        Ok(SocketClient {
            lines: Some(Lines {
                stream,
                buf: Vec::new(),
                scanned: 0,
            }),
        })
    }

    fn lines(&mut self) -> io::Result<&mut Lines> {
        self.lines.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "this connection was turned into a subscription",
            )
        })
    }

    fn send(&mut self, req: &serde_json::Value) -> io::Result<()> {
        let mut line = req.to_string();
        line.push('\n');
        self.lines()?.stream.write_all(line.as_bytes())
    }

    /// The server's world summary and the sequence number it is current to.
    pub fn snapshot(&mut self) -> io::Result<RemoteSnapshot> {
        self.snapshot_within(REPLY_TIMEOUT)
    }

    /// [`SocketClient::snapshot`], waiting at most `timeout` for the reply.
    /// A server that accepts the connection but does not answer in time
    /// gives `TimedOut`.
    pub fn snapshot_within(&mut self, timeout: Duration) -> io::Result<RemoteSnapshot> {
        self.send(&serde_json::json!({"op": "snapshot"}))?;
        let line = self.lines()?.reply_within(timeout)?;
        Ok(decode::<RemoteSnapshot>(&line)?)
    }

    /// The newest raw frame body published by `source`, if any.
    pub fn raw(&mut self, source: &str) -> io::Result<Option<String>> {
        self.send(&serde_json::json!({"op": "raw", "source": source}))?;
        let line = self.lines()?.reply()?;
        Ok(decode::<RawReply>(&line)?.body)
    }

    /// Follow the server's log from just after `from`. This connection then
    /// belongs to the subscription; further requests on this client fail.
    /// A `dropped` or `future` answer arrives from the first
    /// [`RemoteSubscription::next`].
    pub fn subscribe_from(&mut self, from: u64) -> io::Result<RemoteSubscription> {
        self.send(&serde_json::json!({"op": "subscribe", "from": from}))?;
        let lines = self.lines.take().expect("checked by send");
        Ok(RemoteSubscription {
            lines,
            instance: None,
        })
    }
}

/// Records from a server's log, in order.
pub struct RemoteSubscription {
    lines: Lines,
    /// The bus instance from the `subscribed` header, once it has arrived.
    instance: Option<u64>,
}

impl fmt::Debug for RemoteSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteSubscription").finish_non_exhaustive()
    }
}

impl RemoteSubscription {
    /// The next record, waiting up to `timeout`. `Ok(None)` when none
    /// arrived in time. An error ends the subscription.
    ///
    /// The `subscribed` header line is read here and not returned; its bus
    /// instance is then available from [`RemoteSubscription::instance`].
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Record>, ClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(line) = self.lines.next_line(left)? else {
                return Ok(None);
            };
            if let Some(r) = std::str::from_utf8(&line)
                .ok()
                .and_then(|s| Record::from_line(s).ok())
            {
                return Ok(Some(r));
            }
            if let Ok(h) = serde_json::from_slice::<SubscribedLine>(&line) {
                self.instance = Some(h.subscribed.instance);
                continue;
            }
            return match serde_json::from_slice::<ErrorLine>(&line) {
                Ok(e) => Err(e.into()),
                Err(e) => Err(ClientError::Decode(e.to_string())),
            };
        }
    }

    /// The server bus's instance id, once the `subscribed` header arrived.
    pub fn instance(&self) -> Option<u64> {
        self.instance
    }
}

/// A copy of a remote world: the summary and the sequence number it is
/// current to.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteView {
    pub seq: u64,
    /// The server bus's instance id (see [`crate::Bus::instance`]).
    pub instance: u64,
    /// The server bus's header kind: `Replay` when it republishes a
    /// recorded log.
    pub header_kind: HeaderKind,
    /// The server machine's identity ([`crate::ident`]); empty from older
    /// servers.
    pub host: String,
    /// The server bus's `backend` header tag; empty when absent.
    pub backend: String,
    /// The server bus's `mode` header tag (`live`, `mock`, `replay`); empty
    /// when absent.
    pub mode: String,
    pub world: WorldSummary,
}

impl Default for RemoteView {
    fn default() -> Self {
        RemoteView {
            seq: 0,
            instance: 0,
            header_kind: HeaderKind::Live,
            host: String::new(),
            backend: String::new(),
            mode: String::new(),
            world: WorldSummary::default(),
        }
    }
}

impl From<RemoteSnapshot> for RemoteView {
    fn from(s: RemoteSnapshot) -> Self {
        RemoteView {
            seq: s.seq,
            instance: s.instance,
            header_kind: s.kind,
            host: s.host,
            backend: s.backend,
            mode: s.mode,
            world: s.world,
        }
    }
}

impl RemoteView {
    /// The announced models, in key order.
    pub fn models(&self) -> &[ModelView] {
        &self.world.models
    }

    /// The newest events, oldest first.
    pub fn events(&self) -> &[EventSummary] {
        &self.world.events
    }

    /// The latest reading of `signal` on `entity`.
    pub fn reading(&self, entity: &EntityKey, signal: &str) -> Option<&ReadingSummary> {
        self.world.reading(entity, signal)
    }
}

/// The view and when its server time was last learned.
struct ViewState {
    view: RemoteView,
    /// Local time at which `view.world.now_mono_ns` was last set, so ages
    /// keep growing while the server is quiet.
    learned: Instant,
    /// Prunes `view.world` as records are applied.
    pruner: SummaryPruner,
}

struct RemoteShared {
    path: PathBuf,
    state: Mutex<ViewState>,
    connected: AtomicBool,
    /// At least one reconnect attempt has failed since the connection was
    /// lost (see [`Link`]).
    retry_failed: AtomicBool,
    stop: AtomicBool,
    /// What [`RemoteShared::pause`] waits on: [`RealClock`] in normal use,
    /// a [`crate::ManualPlayClock`] in tests.
    clock: Arc<dyn PlayClock + Send + Sync>,
}

/// The state of a [`RemoteWorld`]'s connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// A subscription is open; the view is current.
    Connected,
    /// The connection was lost and the first reconnect is pending.
    Reconnecting,
    /// A reconnect attempt has failed; the follower keeps trying with
    /// backoff. The view is the last one received and goes stale.
    Disconnected,
}

impl RemoteShared {
    fn state(&self) -> std::sync::MutexGuard<'_, ViewState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Wait `d` on the clock, waking every tick to check for stop. False
    /// when stopping.
    fn pause(&self, d: Duration) -> bool {
        let tick = u64::try_from(TICK.as_nanos()).unwrap_or(u64::MAX);
        let end = self
            .clock
            .now_ns()
            .saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        loop {
            if self.stopping() {
                return false;
            }
            let left = end.saturating_sub(self.clock.now_ns());
            if left == 0 {
                return true;
            }
            self.clock.sleep_ns(left.min(tick));
        }
    }
}

/// A remote world kept current on a background thread. See the module doc.
pub struct RemoteWorld {
    shared: Arc<RemoteShared>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for RemoteWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteWorld")
            .field("path", &self.shared.path)
            .field("connected", &self.connected())
            .finish()
    }
}

/// Connect, take a snapshot (waiting at most `timeout` for it), and
/// subscribe from it on the same connection.
fn open(path: &Path, timeout: Duration) -> io::Result<(RemoteSnapshot, RemoteSubscription)> {
    let mut c = SocketClient::connect(path)?;
    let snap = c.snapshot_within(timeout)?;
    let sub = c.subscribe_from(snap.seq)?;
    Ok((snap, sub))
}

impl RemoteWorld {
    /// Connect to the server at `path` and take the first snapshot. Fails
    /// when no server answers, so the caller can host its own bus instead.
    pub fn connect(path: &Path) -> io::Result<RemoteWorld> {
        RemoteWorld::connect_within(path, REPLY_TIMEOUT)
    }

    /// [`RemoteWorld::connect`], waiting at most `timeout` for the first
    /// snapshot. For a probe at start-up: a server that accepts but does
    /// not answer in time fails with `TimedOut`. Reconnects later use the
    /// normal reply timeout.
    pub fn connect_within(path: &Path, timeout: Duration) -> io::Result<RemoteWorld> {
        RemoteWorld::connect_with_clock(path, timeout, Arc::new(RealClock))
    }

    /// [`RemoteWorld::connect_within`] with the clock the follower waits on
    /// (reconnect backoff, the gap between snapshots). Tests pass a
    /// [`crate::ManualPlayClock`].
    pub(crate) fn connect_with_clock(
        path: &Path,
        timeout: Duration,
        clock: Arc<dyn PlayClock + Send + Sync>,
    ) -> io::Result<RemoteWorld> {
        let (snap, sub) = open(path, timeout)?;
        let shared = Arc::new(RemoteShared {
            path: path.to_path_buf(),
            state: Mutex::new(ViewState {
                pruner: SummaryPruner::for_snapshot(&snap.world),
                view: RemoteView::from(snap),
                learned: Instant::now(),
            }),
            connected: AtomicBool::new(true),
            retry_failed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            clock,
        });
        let s = shared.clone();
        let thread = thread::Builder::new()
            .name("tt-toplike-bus-remote".into())
            .spawn(move || follow(&s, sub))?;
        Ok(RemoteWorld {
            shared,
            thread: Some(thread),
        })
    }

    /// A copy of the view, with reading ages taken now.
    pub fn latest(&self) -> RemoteView {
        let st = self.shared.state();
        let mut v = st.view.clone();
        let elapsed = u64::try_from(st.learned.elapsed().as_nanos()).unwrap_or(u64::MAX);
        drop(st);
        let now = v.world.now_mono_ns.saturating_add(elapsed);
        v.world.set_now(now);
        v
    }

    /// True while a subscription to the server is open.
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// The connection's state: connected, waiting for the first reconnect,
    /// or down after a failed reconnect.
    pub fn link(&self) -> Link {
        if self.connected() {
            Link::Connected
        } else if self.shared.retry_failed.load(Ordering::SeqCst) {
            Link::Disconnected
        } else {
            Link::Reconnecting
        }
    }

    /// The server machine's identity, `backend` tag and `mode` tag, from the
    /// latest snapshot.
    pub fn origin(&self) -> (String, String, String) {
        let st = self.shared.state();
        (
            st.view.host.clone(),
            st.view.backend.clone(),
            st.view.mode.clone(),
        )
    }

    /// The chip keys the remote world knows, in key order.
    pub fn chips(&self) -> Vec<EntityKey> {
        let st = self.shared.state();
        st.view
            .world
            .entities
            .iter()
            .filter(|e| e.kind() == crate::EntityKind::Chip)
            .cloned()
            .collect()
    }

    /// The sequence number the view is current to. One lock and no copy, so
    /// a caller can check it every frame and call [`RemoteWorld::latest`]
    /// only when it moved.
    pub fn seq(&self) -> u64 {
        self.shared.state().view.seq
    }

    /// The server's bus instance id the view belongs to. A reconnect to a
    /// restarted server changes it, possibly with the same `seq`.
    pub fn instance(&self) -> u64 {
        self.shared.state().view.instance
    }

    /// The server's monotonic time now, estimated from the last time it was
    /// learned plus the local time since. The scale reading ages are on.
    pub fn now_mono_ns(&self) -> u64 {
        let st = self.shared.state();
        let elapsed = u64::try_from(st.learned.elapsed().as_nanos()).unwrap_or(u64::MAX);
        st.view.world.now_mono_ns.saturating_add(elapsed)
    }

    /// The server bus's header kind: `Replay` when it republishes a
    /// recorded log.
    pub fn header_kind(&self) -> HeaderKind {
        self.shared.state().view.header_kind
    }

    /// The socket path this world reads.
    pub fn path(&self) -> &Path {
        &self.shared.path
    }
}

/// How long dropping a [`RemoteWorld`] waits for its thread.
const DROP_WAIT: Duration = Duration::from_millis(500);

impl Drop for RemoteWorld {
    /// Ask the thread to stop and wait at most [`DROP_WAIT`] for it. The
    /// thread checks the flag every [`TICK`], but it can be inside a
    /// reconnect waiting up to [`REPLY_TIMEOUT`] on a hung server. Past the
    /// wait it is left to finish on its own (it holds only its own `Arc`
    /// and exits at its next check), so a program quitting is never held
    /// up by a dead host.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let end = Instant::now() + DROP_WAIT;
            while !t.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(5));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

/// The background loop: apply records; when the subscription ends, take
/// a fresh snapshot.
///
/// Every reconnect starts from a snapshot. A server restarted on the same
/// log directory continues the sequence numbers with an empty world, and
/// one restarted after a crash can reuse numbers, so a sequence number
/// from the old connection says nothing about the new bus.
fn follow(shared: &RemoteShared, first: RemoteSubscription) {
    let mut sub = Some(first);
    let mut backoff = BACKOFF_FIRST;
    let floor = u64::try_from(SNAPSHOT_FLOOR.as_nanos()).unwrap_or(u64::MAX);
    // Clock time of the last snapshot request: the connect's, at first.
    let mut last_snapshot = shared.clock.now_ns();
    loop {
        if let Some(mut s) = sub.take() {
            shared.connected.store(true, Ordering::SeqCst);
            shared.retry_failed.store(false, Ordering::SeqCst);
            let at_once = match drain(shared, &mut s) {
                None => return,
                Some(at_once) => at_once,
            };
            shared.connected.store(false, Ordering::SeqCst);
            // The first retry after a lost connection waits BACKOFF_FIRST;
            // each failed attempt below doubles the wait up to BACKOFF_MAX.
            backoff = BACKOFF_FIRST;
            if !at_once {
                if !shared.pause(backoff) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
        if shared.stopping() {
            return;
        }
        // Keep snapshots at least SNAPSHOT_FLOOR apart.
        let since = shared.clock.now_ns().saturating_sub(last_snapshot);
        if since < floor && !shared.pause(Duration::from_nanos(floor - since)) {
            return;
        }
        last_snapshot = shared.clock.now_ns();
        match resnapshot(shared) {
            Ok(s) => {
                sub = Some(s);
                backoff = BACKOFF_FIRST;
            }
            Err(_) => {
                shared.retry_failed.store(true, Ordering::SeqCst);
                if !shared.pause(backoff) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Apply records from `s` until it can no longer be followed. `None` when
/// the `RemoteWorld` is stopping. `Some(true)` when the server still
/// answers but this position is lost (`dropped`, `future`, `too_slow`, a
/// gap or repeat in the sequence, a different bus instance), so a new
/// snapshot should be taken at once; `Some(false)` when the connection was
/// lost and the retry waits.
fn drain(shared: &RemoteShared, s: &mut RemoteSubscription) -> Option<bool> {
    loop {
        if shared.stopping() {
            return None;
        }
        let got = s.next(TICK);
        let mut st = shared.state();
        if s.instance().is_some_and(|i| i != st.view.instance) {
            return Some(true);
        }
        match got {
            Ok(Some(rec)) => {
                // Records follow the view's seq one by one. Anything else
                // would skip or double-apply a record.
                if rec.seq != st.view.seq + 1 {
                    return Some(true);
                }
                let before = st.view.world.now_mono_ns;
                let ViewState { view, pruner, .. } = &mut *st;
                pruner.apply(&mut view.world, &rec);
                st.view.seq = rec.seq;
                if st.view.world.now_mono_ns != before {
                    st.learned = Instant::now();
                }
            }
            Ok(None) => {}
            Err(e) => {
                return Some(matches!(
                    e,
                    ClientError::Dropped { .. } | ClientError::Future { .. } | ClientError::TooSlow
                ));
            }
        }
    }
}

/// Connect, take a fresh snapshot that replaces the view, and subscribe
/// from it.
fn resnapshot(shared: &RemoteShared) -> io::Result<RemoteSubscription> {
    let (snap, sub) = open(&shared.path, REPLY_TIMEOUT)?;
    let mut st = shared.state();
    st.pruner = SummaryPruner::for_snapshot(&snap.world);
    st.view = RemoteView::from(snap);
    st.learned = Instant::now();
    Ok(sub)
}
