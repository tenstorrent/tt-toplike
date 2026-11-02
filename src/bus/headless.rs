//! Headless mode: a bus host with no screen (`tt-toplike-tui --headless`),
//! and the no-TTY `--serve` publisher, which now runs on the same loop.
//!
//! [`run_headless`] is the loop. Each tick it:
//!
//! 1. calls `backend.update()` (a failure is logged and the loop goes on,
//!    as the old `--serve` loop did: clients keep the last frame);
//! 2. reads the workload monitors through a [`Workloads`] (the real one is
//!    [`LocalWorkloads`]; tests pass their own) and feeds the host. A
//!    `--remote` relay feeds the remote frame's extension instead: the
//!    local processes would name local `/dev/tenstorrent` indexes against
//!    the remote box's chips;
//! 3. runs [`BusHost::tick`];
//! 4. when a broadcast sink is given (`--serve`), builds the `/telemetry`
//!    frame from the bus: [`compat::telemetry_frame`] with the
//!    `tt_toplike` extension built from this tick's processes and
//!    inference snapshot, or [`compat::relay_frame`] (the origin's frame,
//!    byte for byte) for a `--remote` relay. Nothing is sent before the
//!    first `tt-smi` raw frame is on the bus;
//! 5. waits `interval`, checking the stop flag at least every 50 ms.
//!
//! When the stop flag is raised (SIGTERM or SIGINT through
//! [`install_stop_signals`], or a test) the loop ends, the bus is flushed,
//! the socket server is dropped (which removes the socket file and frees
//! its lock), and the host is dropped (which frees the log directory's
//! lock). The caller then exits 0.
//!
//! [`run_headless_replay`] serves a recorded log the same way: a replay
//! host, the socket, no backend and no broadcast. It keeps serving after
//! the log has played, until stopped, so a reader can attach late.

use crate::backend::TelemetryBackend;
use crate::bus::gozer::GozerSource;
use crate::bus::host::BusHost;
use crate::bus::proc_argv::snapshots_from_rows;
use crate::workload::host_processes::ProcRow;
use crate::workload::inference_server::ServiceState;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tt_toplike_bus::BusConfig;

#[cfg(feature = "remote")]
use crate::bus::compat;

/// How long shutdown waits for the log writer to flush.
const FLUSH_WAIT: Duration = Duration::from_secs(5);

/// Longest single sleep between stop-flag checks.
const STOP_POLL: Duration = Duration::from_millis(50);

/// Settings for [`run_headless`] and [`run_headless_replay`].
#[derive(Debug, Clone)]
pub struct HeadlessConfig {
    /// The bus: log directory (`None` keeps it in memory) and retention.
    pub bus: BusConfig,
    /// Where to serve the bus socket. `None` serves none. Ignored where
    /// Unix sockets do not exist.
    pub socket: Option<PathBuf>,
    /// Time between ticks.
    pub interval: Duration,
    /// The backend is a `--remote` box: feed its extension and re-broadcast
    /// its frames verbatim.
    pub relay: bool,
    /// Run the real `gozer status --json` source. Off for `--mock` and
    /// `--remote` (gozer describes this box's real chips) and in tests.
    pub gozer: bool,
    /// The backend makes its values up (`--mock`). Its readings are
    /// recorded as `Estimated` and the header's `mode` is `mock`.
    pub synthetic: bool,
    /// Short backend name for the header's `backend` entry (`hybrid`,
    /// `sysfs`, `mock`, ...; see `crate::cli::backend_tag`).
    pub backend_name: String,
}

/// What the workload monitors saw this tick.
#[derive(Debug, Clone, Default)]
pub struct WorkloadSnapshot {
    /// The process panel's rows.
    pub procs: Vec<ProcRow>,
    /// The inference monitor's snapshot.
    pub inference: Vec<ServiceState>,
}

/// Where the loop gets workload snapshots.
pub trait Workloads {
    /// Update the monitors and return what they see now.
    fn poll(&mut self) -> WorkloadSnapshot;
}

/// No monitors: every poll is empty. For a `--remote` relay and tests.
#[derive(Debug, Default)]
pub struct NoWorkloads;

impl Workloads for NoWorkloads {
    fn poll(&mut self) -> WorkloadSnapshot {
        WorkloadSnapshot::default()
    }
}

/// Most process rows read per tick. Headless has no panel height to size
/// rows for, so the cap is generous (the old `--serve` loop's value).
pub const HEADLESS_PROC_ROWS_MAX: usize = 64;

/// The real monitors: the host process monitor, the liveness prober and,
/// on Linux, the inference server monitor. Built the same way `run_tui`
/// builds them.
pub struct LocalWorkloads {
    procs: crate::workload::HostProcessMonitor,
    liveness: crate::workload::LivenessProber,
    #[cfg(target_os = "linux")]
    inference: crate::workload::InferenceServerMonitor,
}

impl LocalWorkloads {
    /// Start the monitors and take a first reading.
    pub fn new() -> Self {
        let mut procs = crate::workload::HostProcessMonitor::new();
        procs.update();
        let liveness = crate::workload::LivenessProber::spawn();
        liveness.submit(procs.detected_runtimes());
        #[cfg(target_os = "linux")]
        let inference = crate::workload::InferenceServerMonitor::spawn();
        #[cfg(target_os = "linux")]
        inference.submit(procs.detected_inference_servers());
        LocalWorkloads {
            procs,
            liveness,
            #[cfg(target_os = "linux")]
            inference,
        }
    }
}

impl Default for LocalWorkloads {
    fn default() -> Self {
        Self::new()
    }
}

impl Workloads for LocalWorkloads {
    fn poll(&mut self) -> WorkloadSnapshot {
        self.procs.update();
        self.liveness.submit(self.procs.detected_runtimes());
        #[cfg(target_os = "linux")]
        self.inference
            .submit(self.procs.detected_inference_servers());
        let procs = self
            .procs
            .rows(HEADLESS_PROC_ROWS_MAX, &self.liveness.fresh_verdicts());
        #[cfg(target_os = "linux")]
        let inference = self.inference.snapshot();
        #[cfg(not(target_os = "linux"))]
        let inference = Vec::new();
        WorkloadSnapshot { procs, inference }
    }
}

/// Whether a headless host scans this box's processes and model servers.
/// A `--remote` relay never does (its processes come from the remote
/// frame). A synthetic (mock) backend does only with `--mock-workloads`:
/// otherwise this box's real processes would be recorded against made-up
/// chips.
pub fn uses_local_workloads(relay: bool, synthetic: bool, mock_workloads: bool) -> bool {
    !relay && (!synthetic || mock_workloads)
}

/// What a headless run did, for the exit message and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadlessReport {
    /// Ticks run (0 for a replay).
    pub ticks: u64,
    /// The bus head at shutdown.
    pub head: u64,
    /// Frames handed to the broadcast sink.
    pub frames: u64,
    /// Whether the final flush finished in time.
    pub flushed: bool,
}

/// The socket server and its [`ServedMark`], or nothing where there is no
/// Unix socket. The server is dropped first (it removes its socket file),
/// then the mark.
#[cfg(unix)]
struct Server {
    _server: tt_toplike_bus::SocketServer,
    _mark: ServedMark,
}
#[cfg(not(unix))]
type Server = ();

/// Socket paths this process has bound through [`serve_socket`] and still
/// serves. The second-signal exit in [`watch_stop`] removes a socket file
/// only when its path is here: before the bind succeeds, the file at the
/// path may belong to another running host (a second instance started on
/// the same default path), and removing it would cut that host off.
static SERVED_SOCKETS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Is `p` a socket this process bound and still serves?
fn serving_socket(p: &Path) -> bool {
    SERVED_SOCKETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|s| s == p)
}

/// Puts a path on [`SERVED_SOCKETS`] and takes it off again when dropped.
struct ServedMark(PathBuf);

impl ServedMark {
    fn new(p: &Path) -> ServedMark {
        SERVED_SOCKETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(p.to_path_buf());
        ServedMark(p.to_path_buf())
    }
}

impl Drop for ServedMark {
    fn drop(&mut self) {
        let mut v = SERVED_SOCKETS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = v.iter().position(|s| *s == self.0) {
            v.remove(i);
        }
    }
}

/// Serve `bus` on `path`, with the path in the error. The path is marked as
/// this process's (see [`SERVED_SOCKETS`]) only after the bind succeeded.
#[cfg(unix)]
fn serve_socket(bus: &tt_toplike_bus::Bus, path: Option<&Path>) -> io::Result<Option<Server>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let server = tt_toplike_bus::SocketServer::spawn(bus.clone(), path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot serve the bus socket at {}: {e}", path.display()),
        )
    })?;
    Ok(Some(Server {
        _server: server,
        _mark: ServedMark::new(path),
    }))
}

#[cfg(not(unix))]
fn serve_socket(_bus: &tt_toplike_bus::Bus, _path: Option<&Path>) -> io::Result<Option<Server>> {
    Ok(None)
}

/// Sleep `d`, waking at least every [`STOP_POLL`] to check `stop`.
fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= end {
            break;
        }
        std::thread::sleep((end - now).min(STOP_POLL));
    }
}

/// Run a live headless host until `stop` is raised. See the module docs.
///
/// `broadcast` receives each `/telemetry` frame (the `--serve` publisher).
/// It is only used in a build with the `remote` feature. Fails before the
/// loop starts when the bus or the socket cannot be opened: another process
/// holds the log directory or the socket (`WouldBlock`, `AddrInUse`), or the
/// socket's directory is not private (`PermissionDenied`).
pub fn run_headless(
    backend: &mut dyn TelemetryBackend,
    workloads: &mut dyn Workloads,
    cfg: HeadlessConfig,
    mut broadcast: Option<&mut dyn FnMut(String)>,
    stop: &AtomicBool,
) -> io::Result<HeadlessReport> {
    let gozer = if cfg.gozer {
        GozerSource::with_command()
    } else {
        GozerSource::fed()
    };
    // Say in the log what produced it, so a mock run is never mistaken for
    // a recording of real chips.
    let mut bus_cfg = cfg.bus;
    let mode = if cfg.synthetic { "mock" } else { "live" };
    bus_cfg
        .header_tags
        .insert("backend".into(), cfg.backend_name.clone());
    bus_cfg.header_tags.insert("mode".into(), mode.into());
    let mut host = BusHost::with_gozer(bus_cfg, backend, gozer)?;
    host.set_synthetic(cfg.synthetic);
    let server = serve_socket(host.bus(), cfg.socket.as_deref())?;
    let mut report = HeadlessReport::default();

    while !stop.load(Ordering::SeqCst) {
        if let Err(e) = backend.update() {
            log::warn!("headless: backend update failed: {e}");
        }
        let snap = if cfg.relay {
            #[cfg(feature = "remote")]
            host.feed_remote(
                backend.remote_inference().as_deref(),
                backend.remote_processes().as_deref(),
            );
            WorkloadSnapshot::default()
        } else {
            let snap = workloads.poll();
            host.feed_processes(&snapshots_from_rows(&snap.procs));
            host.feed_inference(&snap.inference);
            snap
        };
        host.tick(backend, Instant::now());
        report.ticks += 1;

        if let Some(send) = broadcast.as_deref_mut() {
            if let Some(frame) = build_frame(&host, cfg.relay, &snap) {
                send(frame);
                report.frames += 1;
            }
        }
        sleep_unless_stopped(cfg.interval, stop);
    }

    report.head = host.bus().head();
    report.flushed = host.bus().flush(FLUSH_WAIT);
    drop(server);
    drop(host);
    Ok(report)
}

/// The `/telemetry` frame for this tick, built from the bus.
#[cfg(feature = "remote")]
fn build_frame(host: &BusHost, relay: bool, snap: &WorkloadSnapshot) -> Option<String> {
    if relay {
        return host.with_world(|w, _| compat::relay_frame(w));
    }
    let ext = crate::backend::build_extension(&snap.procs, &snap.inference);
    host.with_world(|w, _| compat::telemetry_frame(w, &ext))
}

#[cfg(not(feature = "remote"))]
fn build_frame(_host: &BusHost, _relay: bool, _snap: &WorkloadSnapshot) -> Option<String> {
    None
}

/// Play the recorded log at `path` into a replay host and serve it until
/// `stop` is raised. `cfg.bus.dir`, when set, records the replay (its
/// segments say `Replay`). `on_done` is called once, when the log has
/// played, with the record count or the error text.
pub fn run_headless_replay(
    path: &Path,
    speed: f64,
    cfg: HeadlessConfig,
    stop: &AtomicBool,
    mut on_done: impl FnMut(&Result<u64, String>),
) -> io::Result<HeadlessReport> {
    let host = BusHost::replay(path, speed, cfg.bus)?;
    let server = serve_socket(host.bus(), cfg.socket.as_deref())?;
    let mut told = false;
    while !stop.load(Ordering::SeqCst) {
        if !told {
            if let Some(r) = host.replay_result() {
                on_done(&r);
                told = true;
            }
        }
        sleep_unless_stopped(cfg.interval, stop);
    }
    let report = HeadlessReport {
        ticks: 0,
        head: host.bus().head(),
        frames: 0,
        flushed: host.bus().flush(FLUSH_WAIT),
    };
    drop(server);
    drop(host);
    Ok(report)
}

/// A stop request: SIGINT (Ctrl-C) or SIGTERM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Int,
    Term,
}

impl StopSignal {
    /// The shell's exit status for a process killed by this signal.
    pub fn exit_code(self) -> i32 {
        match self {
            StopSignal::Int => 130,
            StopSignal::Term => 143,
        }
    }
}

/// Act on stop signals as they arrive.
///
/// The first raises `flag`, and the loop is expected to notice it, flush
/// and exit. The second means the loop did not get out (a hung
/// `backend.update()`, a stalled flush): the socket file at `socket` is
/// removed if it is a socket AND this process bound it (best effort, so the
/// next start does not find it), and `exit` is called with 128 + the signal
/// number. `exit` is `std::process::exit` in normal use; tests record the
/// code.
///
/// Before [`serve_socket`] has bound the path for this process (a slow
/// backend start, or a bind that failed because another host serves
/// there), the file is left alone: it may be another running host's.
pub fn watch_stop(
    signals: impl IntoIterator<Item = StopSignal>,
    flag: &AtomicBool,
    socket: Option<&Path>,
    exit: &mut dyn FnMut(i32),
) {
    let mut seen = false;
    for sig in signals {
        if !seen {
            seen = true;
            flag.store(true, Ordering::SeqCst);
            continue;
        }
        if let Some(p) = socket.filter(|p| serving_socket(p)) {
            remove_if_socket(p);
        }
        exit(sig.exit_code());
        return;
    }
}

/// Remove `p` if it is a Unix socket. Anything else is left alone.
fn remove_if_socket(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_socket()) {
            let _ = std::fs::remove_file(p);
        }
    }
    #[cfg(not(unix))]
    let _ = p;
}

/// A flag raised by the first SIGTERM or SIGINT (Ctrl-C elsewhere). A
/// second one exits the process at once (see [`watch_stop`]), removing the
/// socket file at `socket` if this process has bound it.
///
/// The signals are caught on a small background thread running a
/// single-threaded tokio runtime (tokio is already a dependency), so no
/// `unsafe` signal handler is written here. The handlers are registered
/// before this returns and stay for the life of the process.
pub fn install_stop_signals(socket: Option<PathBuf>) -> io::Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<io::Result<()>>();
    let (sig_tx, sig_rx) = std::sync::mpsc::channel::<StopSignal>();
    std::thread::Builder::new()
        .name("tt-toplike-signals".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            rt.block_on(async move {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{signal, SignalKind};
                    let term = signal(SignalKind::terminate());
                    let int = signal(SignalKind::interrupt());
                    let (mut term, mut int) = match (term, int) {
                        (Ok(t), Ok(i)) => (t, i),
                        (Err(e), _) | (_, Err(e)) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(()));
                    loop {
                        let sig = tokio::select! {
                            _ = term.recv() => StopSignal::Term,
                            _ = int.recv() => StopSignal::Int,
                        };
                        if sig_tx.send(sig).is_err() {
                            return;
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = ready_tx.send(Ok(()));
                    while tokio::signal::ctrl_c().await.is_ok() {
                        if sig_tx.send(StopSignal::Int).is_err() {
                            return;
                        }
                    }
                }
            });
        })?;
    ready_rx
        .recv()
        .map_err(|_| io::Error::other("the signal thread stopped before it was ready"))??;
    std::thread::Builder::new()
        .name("tt-toplike-stop".into())
        .spawn(move || {
            watch_stop(sig_rx, &f, socket.as_deref(), &mut |code| {
                std::process::exit(code)
            })
        })?;
    Ok(flag)
}

#[cfg(test)]
#[path = "headless_tests.rs"]
mod tests;
