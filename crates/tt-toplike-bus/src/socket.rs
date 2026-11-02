//! The Unix socket transport: other local programs read the bus.
//!
//! [`SocketServer::spawn`] binds a Unix stream socket and serves the bus to
//! every client that connects. The client side is in [`crate::client`].
//!
//! # Protocol
//!
//! JSON Lines in both directions: one JSON object per line, ended by `\n`.
//! The client sends one request per line and the server replies. A
//! connection can make any number of `snapshot` and `raw` requests. A
//! `subscribe` or `replay` request ends the request phase: the server
//! streams to the end and sends nothing else on that connection.
//!
//! ```text
//! request                                   reply
//! {"op":"snapshot"}                         {"seq":N,"instance":I,"kind":K,"host":H,"backend":B,
//!                                            "mode":M,"world":{...}}   (one line)
//! {"op":"raw","source":"tt-smi"}            {"seq":N,"source":"tt-smi","body":"..."}
//!                                           (body is null when that source has no raw frame)
//! {"op":"subscribe","from":N}               {"subscribed":{"from":N,"instance":I,"host":H,
//!                                            "backend":B,"mode":M}}, then
//! {"op":"subscribe","from":"head"}          record lines N+1, N+2, ... until the client
//!                                           closes or an error line ends the stream
//! {"op":"replay","from_ts_ms":T,"speed":S,  {"header":{"schema":1,"kind":"Replay",...}},
//!  "entities":[...],"signals":[...]}        then record lines paced by speed, then
//!                                           {"done":COUNT}, then the server closes
//! ```
//!
//! * `snapshot`: the reply is
//!   `{"seq":N,"instance":I,"kind":K,"host":H,"backend":B,"mode":M,"world":{...}}`,
//!   where `K` is the bus's header kind (`Live` or `Replay`), `H` the
//!   machine's identity ([`crate::ident`]) and `B`, `M` the header's
//!   `backend` and `mode` tags. The `subscribed` line carries `host`,
//!   `backend` and `mode` too. Older servers send none of them.
//!   `world` is a [`WorldSummary`]; its fields are documented in
//!   [`crate::summary`]. `seq` and `world` are taken under one lock, so the
//!   world holds every record up to `seq` and none after it. `instance` is
//!   the bus's random id ([`Bus::instance`]). A bus reopened on the same
//!   log directory continues the sequence numbers with an empty world, so a
//!   client compares `instance` to tell that the bus changed.
//! * `subscribe`: `from` is the sequence number the client is current to
//!   (a snapshot's `seq`), and the stream starts at the record after it, so
//!   a snapshot plus a subscription from its `seq` misses nothing and sees
//!   nothing twice. `"head"` means the newest record now. The first line is
//!   the `subscribed` header with the start and the bus instance; it is
//!   sent only when the subscription starts, so `dropped` and `future`
//!   come in its place. Each record line is the log's own line format
//!   ([`Record::to_line`]).
//! * `replay`: replays RECORDED data from the bus's own segment files, to
//!   this client only. Nothing is published back into the bus, and records
//!   keep their recorded sequence numbers and timestamps. Records whose
//!   `t_wall_ms` is below `from_ts_ms` are skipped. The pause before a
//!   record is the recorded time since the previous record (`t_mono_ns`)
//!   divided by `speed`, and then capped at 1 s. A log with an overnight
//!   hole, or a slow speed (a 59 s gap at 0.01 would be 5900 s), therefore
//!   never stalls the stream for more than 1 s between two records; pauses
//!   longer than 1 s at the chosen speed are shortened to 1 s. `speed` must
//!   be between 0.01 and 1000; anything else
//!   (including 0, negative numbers and values too large or too small to
//!   pace) gets `bad_request "bad speed"`. A client that closes during a
//!   pause is noticed within 100 ms and its slot is freed.
//!   `entities` and `signals` filter as `bus inspect` does: an
//!   entity filter keeps samples and sweeps of those entities and events
//!   that name one; a signal filter keeps samples of those signals, trims
//!   sweeps to them, and drops events. With either filter set, raw frames
//!   and gaps are dropped. Every field is optional: `from_ts_ms` 0,
//!   `speed` 1, no filter.
//!
//! Errors are single lines, and after an error the server closes the
//! connection:
//!
//! ```text
//! {"error":"dropped","oldest":N}    the record after `from` is no longer kept;
//!                                   N is the oldest readable one
//! {"error":"future","head":N}       `from` is past the newest record N
//! {"error":"bad_request","detail":D} D is one of: "line too long", "not utf-8",
//!                                   "not json", "unknown op", "bad field",
//!                                   "bad from", "bad speed", "request timeout"
//! {"error":"no_recording"}          replay asked of a bus with no log directory
//! {"error":"no_world"}              snapshot or raw asked of a bus with no World
//! {"error":"too_slow"}              sent, when the socket has room, just before a
//!                                   slow subscriber is disconnected
//! {"error":"busy"}                  the server already has its maximum of clients
//! {"error":"io"}                    a segment file could not be read
//! ```
//!
//! # Limits
//!
//! * A request line may be up to [`ServerOptions::max_line`] bytes (64 KiB).
//!   A longer one gets `bad_request` and the connection is closed.
//! * While the server waits for a request, a client that sends nothing for
//!   [`ServerOptions::request_timeout`] is closed. A subscriber is idle by
//!   design, so the timeout does not apply once a stream has started.
//! * [`ServerOptions::max_clients`] (64) connections at once. The next one
//!   is sent `busy` and closed.
//! * A slow subscriber. Each subscriber's outgoing queue is the run of log
//!   records it has not been sent yet; the log holds them, so the server
//!   keeps no copy per client and a slow client costs the publisher
//!   nothing. When the client's socket has not accepted a write for one
//!   write-timeout tick (100 ms) and the queue is longer than
//!   [`ServerOptions::queue_lines`] (4096), the client is sent `too_slow`
//!   if the socket has room for it, and disconnected. A client that reads
//!   steadily is never cut, even while it works through a backlog.
//!
//! # Threads
//!
//! One accept thread. One thread per client; a subscriber or a replay has
//! a second, small thread that watches for the client closing its end. Every thread
//! wakes at least every 100 ms to check for shutdown. Dropping the
//! [`SocketServer`] closes the listener, shuts down every client socket and
//! joins the threads, waiting at most about one second in all. It also
//! removes the socket file if this server bound it (same device and inode).
//!
//! The server holds a [`Bus`] clone, so the bus outlives every
//! subscription it serves: the known limit of a [`crate::Subscription`]
//! that outlives every `Bus` (it returns `Ok(None)` forever) cannot occur
//! while the server lives, and dropping the server closes every client.
//!
//! # The socket file
//!
//! The parent directory is created with mode 0700 when it does not exist.
//! An existing parent must be a directory owned by this user and not
//! writable by group or others, or the server refuses with
//! `PermissionDenied`. In such a directory nobody else can create, swap or
//! rename the socket file, so the chmod to 0600 right after `bind` acts on
//! the socket this server bound, whatever the umask made it for that
//! moment. A symlink at the socket path is refused with `InvalidInput`. A
//! path where a server is listening is refused with `AddrInUse`. A socket
//! file nobody listens on (connecting is refused) is stale and replaced.
//! A path holding something that is not a socket is refused with
//! `AlreadyExists` and never deleted.

use crate::inspect::event_entities;
use crate::log::{Bus, ReadError};
use crate::record::{Header, HeaderKind, Payload, Record, SCHEMA_VERSION};
use crate::replay::{PlayClock, RealClock, Replay};
use crate::segment;
use crate::summary::{RemoteSnapshot, WorldSummary};
use crate::world::World;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often every server thread wakes to check for shutdown, and the
/// socket read and write timeout.
const TICK: Duration = Duration::from_millis(100);

/// How often the accept thread polls the non-blocking listener.
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// Slowest and fastest `replay` speed the socket accepts.
const MIN_SPEED: f64 = 0.01;
const MAX_SPEED: f64 = 1000.0;

/// Longest real wait (ns) between two replayed records, applied after the
/// recorded gap is divided by the speed. A long recorded hole (overnight)
/// and a slow speed (0.01) both end up as at most this wait, so a client
/// that stays connected is never held for minutes between two lines.
const MAX_WAIT_NS: u64 = 1_000_000_000;

/// How long `Drop` waits for client threads before detaching them.
const DROP_WAIT: Duration = Duration::from_millis(900);

/// Server settings. [`ServerOptions::default`] holds the documented limits.
#[derive(Clone)]
pub struct ServerOptions {
    /// Connections served at once. Further ones get `busy`.
    pub max_clients: usize,
    /// How far (in records) a subscriber whose socket is blocked may fall
    /// behind the head before it is cut with `too_slow`.
    pub queue_lines: usize,
    /// Longest request line, in bytes, newline excluded.
    pub max_line: usize,
    /// How long the server waits for a request line, and for a client to
    /// accept a snapshot or raw reply, before closing the connection.
    pub request_timeout: Duration,
    /// What the `replay` op waits on between records. Tests pass a
    /// [`crate::ManualPlayClock`].
    pub play_clock: Arc<dyn PlayClock + Send + Sync>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            max_clients: 64,
            queue_lines: 4096,
            max_line: 64 * 1024,
            request_timeout: Duration::from_secs(10),
            play_clock: Arc::new(RealClock),
        }
    }
}

impl std::fmt::Debug for ServerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerOptions")
            .field("max_clients", &self.max_clients)
            .field("queue_lines", &self.queue_lines)
            .field("max_line", &self.max_line)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

/// An error line. Shared by the server (which writes it) and the client
/// (which reads it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ErrorLine {
    pub(crate) error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) oldest: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) head: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

impl ErrorLine {
    fn new(error: &str) -> ErrorLine {
        ErrorLine {
            error: error.into(),
            oldest: None,
            head: None,
            detail: None,
        }
    }

    fn bad_request(detail: &str) -> ErrorLine {
        ErrorLine {
            detail: Some(detail.into()),
            ..ErrorLine::new("bad_request")
        }
    }

    fn line(&self) -> String {
        serde_json::to_string(self).expect("error line encodes")
    }
}

/// The reply to a `raw` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RawReply {
    pub(crate) seq: u64,
    pub(crate) source: String,
    pub(crate) body: Option<String>,
}

/// One parsed request.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    Snapshot,
    Raw {
        source: String,
    },
    Subscribe {
        from: From,
    },
    Replay {
        #[serde(default)]
        from_ts_ms: u64,
        #[serde(default = "one")]
        speed: f64,
        #[serde(default)]
        entities: Vec<String>,
        #[serde(default)]
        signals: Vec<String>,
    },
}

fn one() -> f64 {
    1.0
}

/// `"from"`: a sequence number or the word `"head"`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
enum From {
    Seq(u64),
    Word(String),
}

/// Parse a request line. The error is the `bad_request` detail.
fn parse_request(line: &str) -> Result<Request, &'static str> {
    let v: serde_json::Value = serde_json::from_str(line).map_err(|_| "not json")?;
    match v.get("op").and_then(|o| o.as_str()) {
        Some("snapshot" | "raw" | "subscribe" | "replay") => {}
        _ => return Err("unknown op"),
    }
    serde_json::from_value(v).map_err(|_| "bad field")
}

/// Shared by the server handle and every server thread.
struct Shared {
    bus: Bus,
    opts: ServerOptions,
    stop: AtomicBool,
    clients: Mutex<HashMap<u64, ClientEntry>>,
    next_id: AtomicU64,
}

/// A connected client: a handle on its socket (for shutdown) and its thread.
struct ClientEntry {
    stream: UnixStream,
    thread: Option<JoinHandle<()>>,
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn clients(&self) -> std::sync::MutexGuard<'_, HashMap<u64, ClientEntry>> {
        self.clients.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A running socket server. Dropping it stops the server.
pub struct SocketServer {
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
    path: PathBuf,
    /// Device and inode of the socket file this server bound, so `Drop`
    /// removes only its own file.
    ident: (u64, u64),
    /// The open `<path>.lock` file holding this path's exclusive lock.
    /// Dropped after the socket file is removed, so the next server can
    /// only start once this one is gone.
    _lock: fs::File,
}

impl std::fmt::Debug for SocketServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketServer")
            .field("path", &self.path)
            .field("clients", &self.clients())
            .finish()
    }
}

impl SocketServer {
    /// Serve `bus` on a socket at `path` with the default limits.
    pub fn spawn(bus: Bus, path: &Path) -> io::Result<SocketServer> {
        SocketServer::spawn_with(bus, path, ServerOptions::default())
    }

    /// Serve `bus` on a socket at `path` with `opts`.
    ///
    /// Fails with `AddrInUse` when a server is listening at `path` or
    /// another server holds its lock, with `AlreadyExists` when `path` is
    /// something other than a socket, and with `PermissionDenied` when the
    /// directory is not private to this user.
    ///
    /// Before it looks at the path, the server takes a non-blocking
    /// exclusive lock on `<path>.lock` (`flock`, through
    /// [`fs::File::try_lock`]) and holds it until it is dropped. Two servers
    /// starting at once on a path that holds a stale socket could otherwise
    /// both find it stale, and the slower one would delete the faster one's
    /// new socket. The lock file is left in place when the server stops.
    pub fn spawn_with(bus: Bus, path: &Path, opts: ServerOptions) -> io::Result<SocketServer> {
        SocketServer::check_path(path)?;
        prepare_dir(path)?;
        let lock = lock_path(path)?;
        clear_stale(path)?;
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let meta = fs::symlink_metadata(path)?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            bus,
            opts,
            stop: AtomicBool::new(false),
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        });
        let s = shared.clone();
        let accept = thread::Builder::new()
            .name("tt-toplike-bus-accept".into())
            .spawn(move || accept_loop(&s, listener))?;
        Ok(SocketServer {
            shared,
            accept: Some(accept),
            path: path.to_path_buf(),
            ident: (meta.dev(), meta.ino()),
            _lock: lock,
        })
    }

    /// Check that a server could be started at `path`, without creating
    /// anything: no directory, no lock file, no socket. A caller that opens
    /// other resources first (a log directory and its lock) calls this
    /// before them, so a refused path leaves nothing behind.
    ///
    /// Refuses, with the same kinds [`SocketServer::spawn_with`] uses:
    ///
    /// * a path too long for a Unix socket address (`InvalidInput`): it must
    ///   be shorter than [`MAX_SOCKET_PATH`] bytes;
    /// * a parent directory that exists and is not a directory, is not
    ///   owned by this user, or is writable by group or others
    ///   (`PermissionDenied`); a missing parent is accepted when its nearest
    ///   existing ancestor is a directory (`spawn_with` creates it 0700);
    /// * a path that is a symlink (`InvalidInput`) or exists and is not a
    ///   socket (`AlreadyExists`).
    ///
    /// A server already listening at the path, or holding its lock, is
    /// found only by `spawn_with`.
    pub fn check_path(path: &Path) -> io::Result<()> {
        let len = path.as_os_str().len();
        if len >= MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the socket path {} is {len} bytes; a Unix socket path must be shorter than {MAX_SOCKET_PATH}",
                    path.display()
                ),
            ));
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        match fs::metadata(parent) {
            Ok(_) => check_private(parent)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let mut up = parent.parent();
                while let Some(a) = up.filter(|a| !a.as_os_str().is_empty()) {
                    match fs::metadata(a) {
                        Ok(m) if m.is_dir() => break,
                        Ok(_) => return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!(
                                "the socket directory {} cannot be created: {} is not a directory",
                                parent.display(),
                                a.display()
                            ),
                        )),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => up = a.parent(),
                        Err(e) => return Err(e),
                    }
                }
            }
            Err(e) => return Err(e),
        }
        match fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symlink", path.display()),
            )),
            Ok(m) if !m.file_type().is_socket() => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            )),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many clients are connected now.
    pub fn clients(&self) -> usize {
        self.shared.clients().len()
    }
}

impl Drop for SocketServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // The accept thread polls every ACCEPT_POLL, so this join is short.
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        let entries: Vec<ClientEntry> = self.shared.clients().drain().map(|(_, e)| e).collect();
        let mut threads = Vec::new();
        for mut e in entries {
            let _ = e.stream.shutdown(std::net::Shutdown::Both);
            threads.extend(e.thread.take());
        }
        let deadline = Instant::now() + DROP_WAIT;
        while threads.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        for t in threads {
            if t.is_finished() {
                let _ = t.join();
            }
            // A thread still running is detached. It wakes within one TICK,
            // sees `stop`, and exits on its own.
        }
        if let Ok(m) = fs::symlink_metadata(&self.path) {
            if (m.dev(), m.ino()) == self.ident {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

/// Create the parent directory if needed and check it is private.
fn prepare_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    check_private(parent)
}

/// Longest socket path, in bytes, plus one: `sun_path` holds the path and a
/// NUL (108 bytes on Linux, 104 on macOS and the BSDs).
#[cfg(target_os = "linux")]
pub const MAX_SOCKET_PATH: usize = 108;
/// Longest socket path, in bytes, plus one: `sun_path` holds the path and a
/// NUL (108 bytes on Linux, 104 on macOS and the BSDs).
#[cfg(not(target_os = "linux"))]
pub const MAX_SOCKET_PATH: usize = 104;

/// Refuse a socket directory that others could write. Nobody else may
/// create, swap or rename entries there, so the socket cannot be replaced
/// between `bind` and the chmod after it.
fn check_private(parent: &Path) -> io::Result<()> {
    let dir = fs::metadata(parent)?;
    // SAFETY: `geteuid` takes no arguments, cannot fail and touches no memory.
    let me = unsafe { libc::geteuid() };
    if !dir.is_dir() || dir.uid() != me || dir.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "the socket directory {} must be owned by this user and not writable by group or others",
                parent.display()
            ),
        ));
    }
    Ok(())
}

/// `<path>.lock`: the socket path with `.lock` appended to its file name.
pub(crate) fn lock_file_for(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

/// Take the exclusive lock on `<path>.lock`, creating it with mode 0600.
/// Another holder gives `AddrInUse`.
fn lock_path(path: &Path) -> io::Result<fs::File> {
    let lock = lock_file_for(path);
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock)?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "the socket {} is in use by another process (it holds {})",
                path.display(),
                lock.display()
            ),
        )),
        Err(fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Remove a stale socket file at `path`. Fails when a server answers there
/// or the path is not a socket. Call only while holding the path's lock.
fn clear_stale(path: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a symlink", path.display()),
        ));
    }
    if !meta.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is not a socket", path.display()),
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("a server is already listening on {}", path.display()),
        )),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path),
        Err(e) => Err(e),
    }
}

fn accept_loop(shared: &Arc<Shared>, listener: UnixListener) {
    while !shared.stopping() {
        match listener.accept() {
            Ok((stream, _)) => admit(shared, stream),
            // WouldBlock is the normal idle case. Other errors (out of file
            // descriptors) are retried after the same pause.
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
    // The listener is dropped here, so new connections are refused.
}

/// Start a client thread, or send `busy` when the server is full.
fn admit(shared: &Arc<Shared>, stream: UnixStream) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(TICK)).is_err()
        || stream.set_write_timeout(Some(TICK)).is_err()
    {
        return;
    }
    let Ok(handle) = stream.try_clone() else {
        return;
    };
    let mut clients = shared.clients();
    if clients.len() >= shared.opts.max_clients {
        drop(clients);
        let _ = (&stream).write_all(format!("{}\n", ErrorLine::new("busy").line()).as_bytes());
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    }
    let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
    // Registered under the lock before the thread starts, so the thread's
    // own removal at exit always finds the entry.
    clients.insert(
        id,
        ClientEntry {
            stream: handle,
            thread: None,
        },
    );
    let s = shared.clone();
    let spawned = thread::Builder::new()
        .name("tt-toplike-bus-client".into())
        .spawn(move || {
            serve(&s, stream);
            s.clients().remove(&id);
        });
    match spawned {
        Ok(h) => {
            if let Some(e) = clients.get_mut(&id) {
                e.thread = Some(h);
            }
        }
        Err(_) => {
            clients.remove(&id);
        }
    }
}

/// How a write ended early.
#[derive(Debug, PartialEq, Eq)]
enum WriteEnd {
    /// The client closed, or the socket failed.
    Closed,
    /// The server is stopping.
    Stopped,
    /// The caller's stall rule gave up. `partial` when part of the line was
    /// already written.
    Stalled { partial: bool },
}

/// One client connection: the socket and the bytes read but not yet used.
struct Conn<'a> {
    shared: &'a Shared,
    stream: UnixStream,
    buf: Vec<u8>,
}

/// Why the request phase ended without a request.
enum ReqEnd {
    /// The client closed, the socket failed, or the server is stopping.
    Gone,
    TooLong,
    Timeout,
}

impl Conn<'_> {
    /// The next request line, newline removed.
    fn read_request(&mut self) -> Result<Vec<u8>, ReqEnd> {
        let max = self.shared.opts.max_line;
        let deadline = Instant::now() + self.shared.opts.request_timeout;
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                if pos > max {
                    return Err(ReqEnd::TooLong);
                }
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                line.pop();
                return Ok(line);
            }
            if self.buf.len() > max {
                return Err(ReqEnd::TooLong);
            }
            if self.shared.stopping() {
                return Err(ReqEnd::Gone);
            }
            if Instant::now() >= deadline {
                return Err(ReqEnd::Timeout);
            }
            match (&self.stream).read(&mut chunk) {
                Ok(0) => return Err(ReqEnd::Gone),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ReqEnd::Gone),
            }
        }
    }

    /// Write `line` and a newline. `stalled` is asked, each time the socket
    /// has refused bytes for another write-timeout tick, whether to give up;
    /// it gets how long the write has been blocked.
    fn write_line(
        &self,
        line: &str,
        stalled: &mut dyn FnMut(Duration) -> bool,
    ) -> Result<(), WriteEnd> {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        let mut off = 0;
        let mut blocked: Option<Instant> = None;
        while off < bytes.len() {
            if self.shared.stopping() {
                return Err(WriteEnd::Stopped);
            }
            match (&self.stream).write(&bytes[off..]) {
                Ok(0) => return Err(WriteEnd::Closed),
                Ok(n) => {
                    off += n;
                    blocked = None;
                }
                Err(e) if is_timeout(&e) => {
                    let since = *blocked.get_or_insert_with(Instant::now);
                    if stalled(since.elapsed().max(TICK)) {
                        return Err(WriteEnd::Stalled { partial: off > 0 });
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(WriteEnd::Closed),
            }
        }
        Ok(())
    }

    /// Write a reply in the request phase: give up when the client has not
    /// taken any of it for `request_timeout`.
    fn reply(&self, line: &str) -> Result<(), WriteEnd> {
        let limit = self.shared.opts.request_timeout;
        self.write_line(line, &mut |blocked| blocked >= limit)
    }

    /// Send an error line, best effort, and close.
    fn fail(&self, e: &ErrorLine) {
        let _ = self.reply(&e.line());
        self.close();
    }

    fn close(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Serve one client from its first request to the end.
fn serve(shared: &Shared, stream: UnixStream) {
    let mut c = Conn {
        shared,
        stream,
        buf: Vec::new(),
    };
    loop {
        let line = match c.read_request() {
            Ok(l) => l,
            Err(ReqEnd::Gone) => return c.close(),
            Err(ReqEnd::TooLong) => return c.fail(&ErrorLine::bad_request("line too long")),
            Err(ReqEnd::Timeout) => return c.fail(&ErrorLine::bad_request("request timeout")),
        };
        let Ok(text) = std::str::from_utf8(&line) else {
            return c.fail(&ErrorLine::bad_request("not utf-8"));
        };
        if text.trim().is_empty() {
            continue;
        }
        let req = match parse_request(text) {
            Ok(r) => r,
            Err(detail) => return c.fail(&ErrorLine::bad_request(detail)),
        };
        match req {
            Request::Snapshot => {
                let Some(line) = snapshot_line(&shared.bus) else {
                    return c.fail(&ErrorLine::new("no_world"));
                };
                if c.reply(&line).is_err() {
                    return c.close();
                }
            }
            Request::Raw { source } => {
                let got = shared.bus.with_reducer(|w: &World, seq| RawReply {
                    seq,
                    body: w.latest_raw(&source).map(|f| f.body.clone()),
                    source: source.clone(),
                });
                let Some(reply) = got else {
                    return c.fail(&ErrorLine::new("no_world"));
                };
                let line = serde_json::to_string(&reply).expect("raw reply encodes");
                if c.reply(&line).is_err() {
                    return c.close();
                }
            }
            Request::Subscribe { from } => {
                let from = match from {
                    From::Seq(n) => n,
                    From::Word(w) if w == "head" => shared.bus.head(),
                    From::Word(_) => return c.fail(&ErrorLine::bad_request("bad from")),
                };
                return stream_live(&c, from);
            }
            Request::Replay {
                from_ts_ms,
                speed,
                entities,
                signals,
            } => {
                if !(speed.is_finite() && (MIN_SPEED..=MAX_SPEED).contains(&speed)) {
                    return c.fail(&ErrorLine::bad_request("bad speed"));
                }
                let filter = ReplayFilter { entities, signals };
                return replay(&c, from_ts_ms, speed, &filter);
            }
        }
    }
}

/// The `snapshot` reply line, or `None` when the bus has no world.
///
/// `seq` and the world come from one [`Bus::snapshot`] call, which takes
/// both under the publish lock. Taking the head in a separate call would
/// let a record land between the two, and the client would then either
/// apply it twice or never see it.
fn snapshot_line(bus: &Bus) -> Option<String> {
    let snap = bus.snapshot()?;
    let header = bus.header();
    let tag = |k: &str| header.tool_versions.get(k).cloned().unwrap_or_default();
    let reply = RemoteSnapshot {
        seq: snap.seq,
        instance: bus.instance(),
        kind: header.kind,
        host: crate::ident::host_identity().to_string(),
        backend: tag("backend"),
        mode: tag("mode"),
        world: WorldSummary::from_world(&snap.world, bus.now_mono_ns()),
    };
    Some(serde_json::to_string(&reply).expect("snapshot encodes"))
}

/// Error line for a failed subscribe or read.
fn read_error_line(e: &ReadError) -> ErrorLine {
    match e {
        ReadError::Dropped { oldest } => ErrorLine {
            oldest: Some(*oldest),
            ..ErrorLine::new("dropped")
        },
        ReadError::Future { head } => ErrorLine {
            head: Some(*head),
            ..ErrorLine::new("future")
        },
        ReadError::Io(_) => ErrorLine::new("io"),
    }
}

/// Watches the read side of a streaming connection (subscribe or
/// replay) on a small thread, and says when the client has closed it or
/// the socket failed. Bytes the client sends while streaming are read and
/// ignored.
struct Watch {
    /// Set when the client has gone, or when the stream ends.
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    fn start(c: &Conn<'_>) -> Watch {
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let thread = c.stream.try_clone().ok().and_then(|stream| {
            thread::Builder::new()
                .name("tt-toplike-bus-watch".into())
                .spawn(move || {
                    let mut chunk = [0u8; 1024];
                    // The socket's read timeout (TICK) wakes this loop.
                    while !d.load(Ordering::SeqCst) {
                        match (&stream).read(&mut chunk) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => break,
                        }
                    }
                    d.store(true, Ordering::SeqCst);
                })
                .ok()
        });
        Watch { done, thread }
    }

    /// True once the client has closed its end.
    fn gone(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// End the stream: close the socket and join the watcher.
    fn finish(mut self, c: &Conn<'_>) {
        self.done.store(true, Ordering::SeqCst);
        c.close();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The line that starts a subscription: where it starts and which bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SubscribedLine {
    pub(crate) subscribed: Subscribed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Subscribed {
    pub(crate) from: u64,
    pub(crate) instance: u64,
    /// The server machine's identity, as in the snapshot. Empty from older
    /// servers.
    #[serde(default)]
    pub(crate) host: String,
    /// The bus header's `backend` tag.
    #[serde(default)]
    pub(crate) backend: String,
    /// The bus header's `mode` tag.
    #[serde(default)]
    pub(crate) mode: String,
}

/// Stream live records after `from` until the client closes, an error
/// ends the stream, the client falls too far behind, or the server stops.
fn stream_live(c: &Conn<'_>, from: u64) {
    let bus = &c.shared.bus;
    let mut sub = match bus.subscribe_from(from) {
        Ok(s) => s,
        Err(e) => return c.fail(&read_error_line(&e)),
    };
    let h = bus.header();
    let tag = |k: &str| h.tool_versions.get(k).cloned().unwrap_or_default();
    let header = SubscribedLine {
        subscribed: Subscribed {
            from,
            instance: bus.instance(),
            host: crate::ident::host_identity().to_string(),
            backend: tag("backend"),
            mode: tag("mode"),
        },
    };
    if c.reply(&serde_json::to_string(&header).expect("header encodes"))
        .is_err()
    {
        return c.close();
    }
    let watch = Watch::start(c);
    let limit = c.shared.opts.queue_lines as u64;
    loop {
        if watch.gone() || c.shared.stopping() {
            break;
        }
        match sub.next(TICK) {
            Ok(Some(rec)) => {
                let seq = rec.seq;
                // Behind by more than the queue limit while the socket is
                // blocked: the client is not keeping up.
                let mut stalled = |_: Duration| bus.head().saturating_sub(seq) > limit;
                match c.write_line(&rec.to_line(), &mut stalled) {
                    Ok(()) => {}
                    Err(WriteEnd::Stalled { partial }) => {
                        if !partial {
                            // One short attempt; a full socket just drops it.
                            let _ = c.stream.set_write_timeout(Some(Duration::from_millis(10)));
                            let line = format!("{}\n", ErrorLine::new("too_slow").line());
                            let _ = (&c.stream).write(line.as_bytes());
                        }
                        break;
                    }
                    Err(_) => break,
                }
            }
            Ok(None) => {}
            Err(e) => {
                let _ = c.reply(&read_error_line(&e).line());
                break;
            }
        }
    }
    watch.finish(c);
}

/// The `replay` op's entity and signal filter. Empty lists keep everything.
struct ReplayFilter {
    entities: Vec<String>,
    signals: Vec<String>,
}

impl ReplayFilter {
    fn entity_ok(&self, e: &str) -> bool {
        self.entities.is_empty() || self.entities.iter().any(|x| x == e)
    }

    fn signal_ok(&self, s: &str) -> bool {
        self.signals.is_empty() || self.signals.iter().any(|x| x == s)
    }

    /// The record as it should be sent, or `None` to skip it.
    fn apply(&self, mut rec: Record) -> Option<Record> {
        let keep = match &mut rec.payload {
            Payload::Sample(s) => self.entity_ok(s.entity.as_str()) && self.signal_ok(&s.signal),
            Payload::Sweep(s) => {
                if !self.entity_ok(s.entity.as_str()) {
                    false
                } else {
                    if !self.signals.is_empty() {
                        let (sigs, vals): (Vec<_>, Vec<_>) = s
                            .signals
                            .drain(..)
                            .zip(s.values.drain(..))
                            .filter(|(n, _)| self.signal_ok(n))
                            .unzip();
                        s.signals = sigs;
                        s.values = vals;
                    }
                    !s.signals.is_empty()
                }
            }
            Payload::Event(e) => {
                self.signals.is_empty()
                    && (self.entities.is_empty()
                        || event_entities(e).iter().any(|k| self.entity_ok(k.as_str())))
            }
            Payload::Raw(_) | Payload::Gap(_) => {
                self.entities.is_empty() && self.signals.is_empty()
            }
        };
        keep.then_some(rec)
    }
}

/// Line that starts a replay stream.
#[derive(Serialize, Deserialize)]
pub(crate) struct HeaderLine {
    pub(crate) header: Header,
}

/// Line that ends a replay stream.
#[derive(Serialize, Deserialize)]
pub(crate) struct DoneLine {
    pub(crate) done: u64,
}

/// Stream the bus's own recording to this client, paced by `speed`.
fn replay(c: &Conn<'_>, from_ts_ms: u64, speed: f64, filter: &ReplayFilter) {
    let bus = &c.shared.bus;
    let Some(dir) = bus.dir() else {
        return c.fail(&ErrorLine::new("no_recording"));
    };
    // Records still in the writer queue are not in the files yet.
    bus.flush(Duration::from_secs(1));
    let Ok(mut log) = Replay::open(dir) else {
        return c.fail(&ErrorLine::new("no_recording"));
    };
    let header = HeaderLine {
        header: Header {
            schema: SCHEMA_VERSION,
            kind: HeaderKind::Replay,
            tool_versions: segment::tool_versions(),
        },
    };
    if c.reply(&serde_json::to_string(&header).expect("header encodes"))
        .is_err()
    {
        return c.close();
    }
    let watch = Watch::start(c);
    let clock = &*c.shared.opts.play_clock;
    let tick = u64::try_from(TICK.as_nanos()).unwrap_or(u64::MAX);
    // Play-clock time of the first sent record, and how far after it (ns
    // of play time) the next record is due.
    let mut play0: Option<u64> = None;
    let mut due = 0u64;
    let mut prev_t: Option<u64> = None;
    let mut sent = 0u64;
    for rec in log.records() {
        if rec.t_wall_ms < from_ts_ms {
            continue;
        }
        let Some(rec) = filter.apply(rec) else {
            continue;
        };
        if let Some(p) = prev_t {
            let gap = rec.t_mono_ns.saturating_sub(p);
            // Divide by the speed first, then cap (see `MAX_WAIT_NS`).
            due = due.saturating_add(((gap as f64 / speed) as u64).min(MAX_WAIT_NS));
        }
        prev_t = Some(rec.t_mono_ns);
        let target = play0
            .get_or_insert_with(|| clock.now_ns())
            .saturating_add(due);
        // Sleep in slices of one tick, so a stopping server or a client
        // that has closed is noticed within a tick.
        loop {
            if c.shared.stopping() || watch.gone() {
                return watch.finish(c);
            }
            let wait = target.saturating_sub(clock.now_ns());
            if wait == 0 {
                break;
            }
            clock.sleep_ns(wait.min(tick));
        }
        if c.reply(&rec.to_line()).is_err() {
            return watch.finish(c);
        }
        sent += 1;
    }
    let _ = c.reply(&serde_json::to_string(&DoneLine { done: sent }).expect("done encodes"));
    watch.finish(c);
}

#[cfg(test)]
#[path = "socket_tests.rs"]
mod tests;
