//! The client side of the socket transport (see [`crate::socket`] for the
//! protocol).
//!
//! * [`SocketClient`] makes requests on one connection: `snapshot`, `raw`,
//!   and `subscribe_from`, which turns the connection into a
//!   [`RemoteSubscription`].
//! * [`RemoteWorld`] keeps a [`RemoteView`] of a server's world current on a
//!   background thread: it takes a snapshot, subscribes from the snapshot's
//!   sequence number, and folds every record in with
//!   [`WorldSummary::apply`]. When the connection drops it reconnects with
//!   a backoff of 1 s, 2 s, 4 s, 8 s, then every 10 s, and subscribes from
//!   the last sequence number it applied, so no record is applied twice.
//!   When the server answers `dropped`, `future` or `too_slow`, it takes a
//!   fresh snapshot at once.
//!
//! # Limits
//!
//! * A server restarted with a fresh bus numbers its records from 1 again.
//!   When its head is below the last applied sequence number the server
//!   answers `future` and the view starts over from a snapshot. When the
//!   new bus has already passed that number, the view cannot tell and
//!   follows the new log from there, so it can keep values the new bus
//!   never published until they are overwritten.
//! * The view does not prune quiet churning entities the way `World` does
//!   (see [`crate::summary`]); a reconnect with a snapshot does.

use crate::record::Record;
use crate::socket::{ErrorLine, RawReply};
use crate::summary::{EventSummary, ReadingSummary, RemoteSnapshot, WorldSummary};
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

/// How often the [`RemoteWorld`] thread checks whether it should stop.
const TICK: Duration = Duration::from_millis(100);

/// First reconnect delay; it doubles up to [`BACKOFF_MAX`].
const BACKOFF_FIRST: Duration = Duration::from_secs(1);

/// Longest reconnect delay.
const BACKOFF_MAX: Duration = Duration::from_secs(10);

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
        self.next_line(REPLY_TIMEOUT)?.ok_or_else(|| {
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
        self.send(&serde_json::json!({"op": "snapshot"}))?;
        let line = self.lines()?.reply()?;
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
        Ok(RemoteSubscription { lines })
    }
}

/// Records from a server's log, in order.
pub struct RemoteSubscription {
    lines: Lines,
}

impl fmt::Debug for RemoteSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteSubscription").finish_non_exhaustive()
    }
}

impl RemoteSubscription {
    /// The next record, waiting up to `timeout`. `Ok(None)` when none
    /// arrived in time. An error ends the subscription.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Record>, ClientError> {
        match self.lines.next_line(timeout)? {
            None => Ok(None),
            Some(line) => match std::str::from_utf8(&line)
                .ok()
                .and_then(|s| Record::from_line(s).ok())
            {
                Some(r) => Ok(Some(r)),
                None => match serde_json::from_slice::<ErrorLine>(&line) {
                    Ok(e) => Err(e.into()),
                    Err(e) => Err(ClientError::Decode(e.to_string())),
                },
            },
        }
    }
}

/// A copy of a remote world: the summary and the sequence number it is
/// current to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteView {
    pub seq: u64,
    pub world: WorldSummary,
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
}

struct RemoteShared {
    path: PathBuf,
    state: Mutex<ViewState>,
    connected: AtomicBool,
    stop: AtomicBool,
}

impl RemoteShared {
    fn state(&self) -> std::sync::MutexGuard<'_, ViewState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Wait `d`, waking every tick to check for stop. False when stopping.
    fn pause(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while Instant::now() < end {
            if self.stopping() {
                return false;
            }
            thread::sleep(TICK.min(end.saturating_duration_since(Instant::now())));
        }
        !self.stopping()
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

/// Connect, take a snapshot, and subscribe from it on the same connection.
fn open(path: &Path) -> io::Result<(RemoteSnapshot, RemoteSubscription)> {
    let mut c = SocketClient::connect(path)?;
    let snap = c.snapshot()?;
    let sub = c.subscribe_from(snap.seq)?;
    Ok((snap, sub))
}

impl RemoteWorld {
    /// Connect to the server at `path` and take the first snapshot. Fails
    /// when no server answers, so the caller can host its own bus instead.
    pub fn connect(path: &Path) -> io::Result<RemoteWorld> {
        let (snap, sub) = open(path)?;
        let shared = Arc::new(RemoteShared {
            path: path.to_path_buf(),
            state: Mutex::new(ViewState {
                view: RemoteView {
                    seq: snap.seq,
                    world: snap.world,
                },
                learned: Instant::now(),
            }),
            connected: AtomicBool::new(true),
            stop: AtomicBool::new(false),
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
}

impl Drop for RemoteWorld {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            // The thread wakes every TICK (and a reply wait is bounded by
            // REPLY_TIMEOUT), so this join is bounded.
            let _ = t.join();
        }
    }
}

/// The background loop: apply records; on a drop, reconnect.
fn follow(shared: &RemoteShared, first: RemoteSubscription) {
    let mut sub = Some(first);
    let mut need_snapshot = false;
    let mut backoff = BACKOFF_FIRST;
    loop {
        if let Some(mut s) = sub.take() {
            shared.connected.store(true, Ordering::SeqCst);
            loop {
                if shared.stopping() {
                    return;
                }
                match s.next(TICK) {
                    Ok(Some(rec)) => {
                        let mut st = shared.state();
                        let before = st.view.world.now_mono_ns;
                        st.view.world.apply(&rec);
                        st.view.seq = rec.seq;
                        if st.view.world.now_mono_ns != before {
                            st.learned = Instant::now();
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        shared.connected.store(false, Ordering::SeqCst);
                        // The server still answers but this position is
                        // lost: start over from a snapshot without waiting.
                        need_snapshot = matches!(
                            e,
                            ClientError::Dropped { .. }
                                | ClientError::Future { .. }
                                | ClientError::TooSlow
                        );
                        break;
                    }
                }
            }
            // The first retry after a drop waits BACKOFF_FIRST; each
            // failed attempt below doubles the wait up to BACKOFF_MAX.
            backoff = BACKOFF_FIRST;
            if !need_snapshot {
                if !shared.pause(backoff) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
        if shared.stopping() {
            return;
        }
        match reconnect(shared, need_snapshot) {
            Ok(s) => {
                sub = Some(s);
                need_snapshot = false;
                backoff = BACKOFF_FIRST;
            }
            Err(_) => {
                if !shared.pause(backoff) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Open a new subscription: from the last applied record, or from a fresh
/// snapshot that replaces the view.
fn reconnect(shared: &RemoteShared, need_snapshot: bool) -> io::Result<RemoteSubscription> {
    if need_snapshot {
        let (snap, sub) = open(&shared.path)?;
        let mut st = shared.state();
        st.view = RemoteView {
            seq: snap.seq,
            world: snap.world,
        };
        st.learned = Instant::now();
        return Ok(sub);
    }
    let seq = shared.state().view.seq;
    SocketClient::connect(&shared.path)?.subscribe_from(seq)
}
