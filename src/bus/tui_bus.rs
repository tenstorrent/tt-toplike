//! The TUI's side of the signal bus: start-up (host or attach), the data
//! tick, `ViewChanged`, and the `/serve` frame.
//!
//! # Start-up
//!
//! [`TuiBus::start`] decides, from [`TuiBusOptions`]:
//!
//! 1. `--bus-attach PATH`: read the bus of the host serving PATH. If it does
//!    not answer within [`ATTACH_TIMEOUT`], start-up fails with a message
//!    naming the path.
//! 2. Otherwise, unless the probe is off, try the default socket path once
//!    ([`probe`], [`PROBE_TIMEOUT`]). When a `--headless` host answers AND
//!    its world describes this machine's chips ([`origin_mismatch`] finds
//!    nothing: the same host identity, mode `live`, a live header, and at
//!    least one chip key among the local backend's devices), attach to it
//!    and host nothing. Otherwise host a bus here; the reason goes to the
//!    debug log (the in-app log buffer; stderr is off under the TUI). The
//!    probe is off with
//!    `--no-bus-attach`, `--bus-socket` (the TUI then serves its own bus),
//!    `--record`/`--bus-dir` (the TUI then records its own bus), and for a
//!    `--mock` or `--remote` backend, whose chips are not the chips a local
//!    headless host reads.
//! 3. Otherwise host a bus in this process ([`BusHost`]): memory only unless
//!    `--record`/`--bus-dir` names a directory, with `--bus-retain`, and
//!    serve it on `--bus-socket` when given.
//!
//! An attached TUI still reads chip telemetry from its own backend. Only
//! the views that read the bus (so far Die 3D) read the attached world, and [`TuiBus::read`] limits it to the local chips
//! ([`ReadView::only_chips`]), so overlays never land on another machine's
//! chips. An explicit `--bus-attach` attaches even when the origin does not
//! match, and the label says why: `bus: <path> (other host)`, `(mock)`,
//! `(replay)` or `(no matching chips)`. A lost connection adds
//! `(reconnecting)`, then `(disconnected)` once a reconnect has failed.
//!
//! # Backend switches
//!
//! A bus that records (`--record`/`--bus-dir`) or serves (`--bus-socket`)
//! refuses a backend switch ([`TuiBus::switch_refusal`]): its log header
//! names one backend and mode for the whole run. A memory-only bus follows
//! the switch ([`TuiBus::backend_switched`]): it re-derives whether the run
//! is synthetic and whether it relays a `--remote` box, resets the backend
//! source, and keeps `/serve` off the world's raw frame until the new
//! backend has published one ([`BusHost::raw_is_current`]).
//!
//! The caller must build the TUI's panic hook BEFORE calling `start`:
//! [`BusHost`] installs the quiet reducer hook, which wraps whatever hook
//! is current when it is first installed, and a hook set later would
//! replace it (see `ui::tui::start_bus_after_panic_hook`).
//!
//! # The data tick
//!
//! [`run_data_tick`] is the TUI loop's data tick: when the update interval
//! has passed it calls `backend.update()` and then [`TuiBus::data_tick`],
//! which ticks the host. Nothing bus-related runs per render frame.
//!
//! Workload feeds arrive at their own rates: the process rows, the inference
//! snapshot and the training state with each applied process scan (every
//! 2 s, [`TuiBus::feed_workloads`]); hivemind events on every loop pass but
//! only when the collector has produced new ones ([`TuiBus::feed_hivemind`]);
//! a `--remote` frame's extension on each data tick.

use crate::backend::TelemetryBackend;
use crate::bus::gozer::GozerSource;
use crate::bus::host::BusHost;
use crate::bus::proc_argv::snapshots_from_rows;
use crate::bus::view::{BusView, ReadCache, ReadView};
use crate::workload::host_processes::ProcRow;
use crate::workload::inference_server::ServiceState;
use crate::workload::train::monitor::TrainState;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tt_toplike_bus::{BusConfig, EntityKey, HeaderKind};

/// How long the start-up probe of the default socket waits for a snapshot.
/// A live local host answers in well under a millisecond.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// How long `--bus-attach` waits for the first snapshot.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether and where the TUI looks for a host to attach to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// Host a bus here; look for no other.
    Off,
    /// Try this path once; host here if nothing answers.
    Probe(PathBuf),
    /// Attach to this path or fail (`--bus-attach`).
    Required(PathBuf),
}

/// Everything [`TuiBus::start`] needs from the command line.
#[derive(Debug, Clone)]
pub struct TuiBusOptions {
    /// The local bus: log directory (`None` keeps it in memory), retention
    /// and header tags.
    pub bus: BusConfig,
    /// `--bus-socket`: serve the local bus here.
    pub serve_socket: Option<PathBuf>,
    pub attach: Attach,
    /// The backend makes its values up (`--mock`): its readings are
    /// recorded as `Estimated`.
    pub synthetic: bool,
    /// The backend is a `--remote` box: feed its frame's extension, and
    /// re-broadcast its frames verbatim.
    pub relay: bool,
    /// Run the real `gozer status --json` source.
    pub gozer: bool,
    /// Feed this box's process rows and inference monitor (see
    /// [`crate::bus::headless::uses_local_workloads`]).
    pub local_workloads: bool,
    /// This machine's identity, compared with an attached host's
    /// ([`tt_toplike_bus::host_identity`]; tests pass another).
    pub local_host: String,
}

impl TuiBusOptions {
    /// The options for `cli` and the backend the TUI built. `backend_info`
    /// is the backend's `backend_info()`, so an auto-detect that fell back
    /// to mock counts as synthetic.
    pub fn from_cli(
        cli: &crate::cli::Cli,
        backend_type: crate::cli::BackendType,
        backend_info: &str,
    ) -> TuiBusOptions {
        use crate::cli::BackendType;
        let name = crate::cli::backend_tag(backend_type, backend_info);
        let synthetic = name == "mock";
        let relay = backend_type == BackendType::Remote;
        let mut bus = cli.tui_bus_config();
        bus.header_tags.insert("backend".into(), name);
        bus.header_tags.insert(
            "mode".into(),
            if synthetic { "mock" } else { "live" }.into(),
        );
        let hosts_own = cli.no_bus_attach
            || cli.bus_socket.is_some()
            || cli.record.is_some()
            || cli.bus_dir.is_some()
            || synthetic
            || relay;
        let attach = match &cli.bus_attach {
            Some(p) => Attach::Required(p.clone()),
            None if hosts_own => Attach::Off,
            #[cfg(unix)]
            None => Attach::Probe(crate::cli::default_bus_socket()),
            #[cfg(not(unix))]
            None => Attach::Off,
        };
        TuiBusOptions {
            bus,
            serve_socket: cli.bus_socket.clone(),
            attach,
            synthetic,
            relay,
            gozer: !matches!(
                backend_type,
                BackendType::Mock | BackendType::Host | BackendType::Remote
            ) && !synthetic,
            local_workloads: crate::bus::headless::uses_local_workloads(
                relay,
                synthetic,
                cli.mock_workloads,
            ),
            local_host: tt_toplike_bus::host_identity().to_string(),
        }
    }
}

/// Why a remote world does not describe this machine's chips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// The host replays a recorded log.
    Replay,
    /// The host's backend is a mock (`mode mock`).
    Mock,
    /// The host runs on another machine (or did not say which).
    OtherHost,
    /// The host's mode is neither `live`, `mock` nor `replay` (an older
    /// host that sends no mode).
    UnknownMode,
    /// None of the remote world's chips is one of the local backend's.
    NoMatchingChips,
}

impl Mismatch {
    /// The label suffix.
    pub fn label(&self) -> &'static str {
        match self {
            Mismatch::Replay => "(replay)",
            Mismatch::Mock => "(mock)",
            Mismatch::OtherHost => "(other host)",
            Mismatch::UnknownMode => "(unknown mode)",
            Mismatch::NoMatchingChips => "(no matching chips)",
        }
    }
}

/// What the attached host says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub kind: HeaderKind,
    pub host: String,
    pub mode: String,
    pub chips: Vec<EntityKey>,
}

#[cfg(unix)]
impl Origin {
    fn of(r: &tt_toplike_bus::RemoteWorld) -> Origin {
        let (host, _backend, mode) = r.origin();
        Origin {
            kind: r.header_kind(),
            host,
            mode,
            chips: r.chips(),
        }
    }
}

/// The first reason the remote world in `o` is not this machine's live
/// chips, or `None` when it is. Checked in this order: a replay, a mock,
/// another host, an unknown mode, no chip in common with `local_chips`.
pub fn origin_mismatch(
    o: &Origin,
    local_host: &str,
    local_chips: &BTreeSet<EntityKey>,
) -> Option<Mismatch> {
    if o.kind == HeaderKind::Replay || o.mode == "replay" {
        return Some(Mismatch::Replay);
    }
    if o.mode == "mock" {
        return Some(Mismatch::Mock);
    }
    if o.host != local_host {
        return Some(Mismatch::OtherHost);
    }
    if o.mode != "live" {
        return Some(Mismatch::UnknownMode);
    }
    if !o.chips.iter().any(|c| local_chips.contains(c)) {
        return Some(Mismatch::NoMatchingChips);
    }
    None
}

/// The chip keys of `backend`'s devices.
pub fn chip_keys(backend: &dyn TelemetryBackend) -> BTreeSet<EntityKey> {
    backend
        .devices()
        .iter()
        .map(|d| EntityKey::chip(&d.bus_id))
        .collect()
}

/// Try the socket at `path` once: `Some` when a bus host answers a
/// snapshot within `timeout`. Anything else (no file, not a socket, nobody
/// listening, no answer in time) is `None`, quietly.
#[cfg(unix)]
pub fn probe(path: &Path, timeout: Duration) -> Option<tt_toplike_bus::RemoteWorld> {
    use std::os::unix::fs::FileTypeExt;
    let is_socket = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket());
    if !is_socket {
        return None;
    }
    tt_toplike_bus::RemoteWorld::connect_within(path, timeout).ok()
}

/// No Unix sockets here: nothing to attach to.
#[cfg(not(unix))]
pub fn probe(_path: &Path, _timeout: Duration) -> Option<()> {
    None
}

/// What the label shows, to rebuild it only on a change.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LabelKey {
    Local(HeaderKind),
    #[cfg(unix)]
    Attached(tt_toplike_bus::Link, Option<Mismatch>),
}

/// The TUI's bus. See the module doc.
pub struct TuiBus {
    view: BusView,
    cache: ReadCache,
    #[cfg(unix)]
    _server: Option<tt_toplike_bus::SocketServer>,
    relay: bool,
    local_workloads: bool,
    synthetic: bool,
    /// The log directory, when the local bus records.
    records: Option<PathBuf>,
    /// The local bus is served on a socket.
    serves: bool,
    local_host: String,
    /// The local backend's chips; an attached world is read only for these.
    local_chips: Arc<BTreeSet<EntityKey>>,
    /// The label, rebuilt only when `label_key` changes.
    label: String,
    label_key: Option<LabelKey>,
    /// The last view name published, so a redraw publishes nothing.
    last_view: Option<String>,
    ticks: u64,
    view_changes: u64,
    /// Hivemind events seen so far (ring length plus evicted), so each
    /// event is fed once.
    hivemind_seen: u64,
}

impl TuiBus {
    /// Host or attach, as the module doc describes. The error is one line
    /// for the user.
    pub fn start(opts: &TuiBusOptions, backend: &dyn TelemetryBackend) -> Result<TuiBus, String> {
        #[cfg(unix)]
        match &opts.attach {
            Attach::Required(p) => {
                return tt_toplike_bus::RemoteWorld::connect_within(p, ATTACH_TIMEOUT)
                    .map(|r| TuiBus::attached(r, opts, backend))
                    .map_err(|e| {
                        format!(
                            "--bus-attach {}: no bus host answered there ({e}). Start one \
                             with `tt-toplike-tui --headless --bus-socket {}`, or drop \
                             --bus-attach to host a bus in this TUI",
                            p.display(),
                            p.display()
                        )
                    });
            }
            Attach::Probe(p) => {
                if let Some(r) = probe(p, PROBE_TIMEOUT) {
                    let local = chip_keys(backend);
                    match origin_mismatch(&Origin::of(&r), &opts.local_host, &local) {
                        None => {
                            log::info!("bus: attached to the host at {}", p.display());
                            return Ok(TuiBus::attached(r, opts, backend));
                        }
                        Some(m) => {
                            // Debug log only: the TUI owns the terminal.
                            log::debug!(
                                "bus: not attaching to the host at {}: {:?}; hosting a bus here",
                                p.display(),
                                m
                            );
                        }
                    }
                }
            }
            Attach::Off => {}
        }
        #[cfg(not(unix))]
        if let Attach::Required(p) = &opts.attach {
            return Err(format!(
                "--bus-attach {}: bus sockets need a Unix system",
                p.display()
            ));
        }

        let gozer = if opts.gozer {
            GozerSource::with_command()
        } else {
            GozerSource::fed()
        };
        let mut host = BusHost::with_gozer(opts.bus.clone(), backend, gozer).map_err(|e| {
            match &opts.bus.dir {
                Some(d) => format!("cannot open the bus log at {}: {e}", d.display()),
                None => format!("cannot open the bus: {e}"),
            }
        })?;
        host.set_synthetic(opts.synthetic);
        #[cfg(unix)]
        let server = match &opts.serve_socket {
            None => None,
            Some(p) => Some(
                tt_toplike_bus::SocketServer::spawn(host.bus().clone(), p)
                    .map_err(|e| format!("cannot serve the bus socket at {}: {e}", p.display()))?,
            ),
        };
        let mut b = TuiBus::local(host, opts);
        #[cfg(unix)]
        {
            b.serves = server.is_some();
            b._server = server;
        }
        Ok(b)
    }

    /// A TUI bus around a host this process owns.
    pub fn local(host: BusHost, opts: &TuiBusOptions) -> TuiBus {
        let records = host.bus().dir().map(Path::to_path_buf);
        let mut b = TuiBus::with_view(BusView::Local(Box::new(host)), opts);
        b.records = records;
        b.refresh_label();
        b
    }

    /// A TUI bus reading another process's host, for `backend`'s chips.
    #[cfg(unix)]
    pub fn attached(
        remote: tt_toplike_bus::RemoteWorld,
        opts: &TuiBusOptions,
        backend: &dyn TelemetryBackend,
    ) -> TuiBus {
        let mut b = TuiBus::with_view(BusView::Attached(remote), opts);
        b.local_chips = Arc::new(chip_keys(backend));
        b.refresh_label();
        b
    }

    fn with_view(view: BusView, opts: &TuiBusOptions) -> TuiBus {
        TuiBus {
            view,
            cache: ReadCache::new(),
            #[cfg(unix)]
            _server: None,
            relay: opts.relay,
            local_workloads: opts.local_workloads,
            synthetic: opts.synthetic,
            records: None,
            serves: false,
            local_host: opts.local_host.clone(),
            local_chips: Arc::new(BTreeSet::new()),
            label: String::new(),
            label_key: None,
            last_view: None,
            ticks: 0,
            view_changes: 0,
            hivemind_seen: 0,
        }
    }

    /// Recompute the label when what it shows changed: for an attached bus
    /// the link state and the origin mismatch; a local bus's label is fixed.
    fn refresh_label(&mut self) {
        let key = self.label_state();
        if self.label_key.as_ref() == Some(&key) && !self.label.is_empty() {
            return;
        }
        self.label = match (&self.view, &key) {
            (BusView::Local(_), LabelKey::Local(HeaderKind::Replay)) => "bus: replay".to_string(),
            (BusView::Local(_), _) => "bus: local".to_string(),
            #[cfg(unix)]
            (BusView::Attached(r), LabelKey::Attached(link, mismatch)) => {
                let mut l = format!("bus: {}", r.path().display());
                if let Some(m) = mismatch {
                    l.push(' ');
                    l.push_str(m.label());
                }
                match link {
                    tt_toplike_bus::Link::Connected => {}
                    tt_toplike_bus::Link::Reconnecting => l.push_str(" (reconnecting)"),
                    tt_toplike_bus::Link::Disconnected => l.push_str(" (disconnected)"),
                }
                l
            }
            #[cfg(unix)]
            (BusView::Attached(r), _) => format!("bus: {}", r.path().display()),
        };
        self.label_key = Some(key);
    }

    /// What the label depends on, read now.
    fn label_state(&self) -> LabelKey {
        match &self.view {
            BusView::Local(h) => LabelKey::Local(h.bus().header().kind),
            #[cfg(unix)]
            BusView::Attached(r) => LabelKey::Attached(
                r.link(),
                origin_mismatch(&Origin::of(r), &self.local_host, &self.local_chips),
            ),
        }
    }

    /// The host, when this process hosts the bus.
    pub fn host(&self) -> Option<&BusHost> {
        match &self.view {
            BusView::Local(h) => Some(h),
            #[cfg(unix)]
            BusView::Attached(_) => None,
        }
    }

    fn host_mut(&mut self) -> Option<&mut BusHost> {
        match &mut self.view {
            BusView::Local(h) => Some(h),
            #[cfg(unix)]
            BusView::Attached(_) => None,
        }
    }

    /// True when the world comes from another process's socket.
    pub fn is_attached(&self) -> bool {
        self.host().is_none()
    }

    /// One data tick: feed a `--remote` frame's extension, then tick the
    /// host. An attached TUI hosts nothing, so only the count moves.
    pub fn data_tick(&mut self, backend: &dyn TelemetryBackend, now: Instant) {
        self.ticks += 1;
        let relay = self.relay;
        let Some(host) = self.host_mut() else {
            // Attached: keep the local chip set and the label current.
            let chips = chip_keys(backend);
            if *self.local_chips != chips {
                self.local_chips = Arc::new(chips);
            }
            self.refresh_label();
            return;
        };
        if relay {
            #[cfg(feature = "remote")]
            host.feed_remote(
                backend.remote_inference().as_deref(),
                backend.remote_processes().as_deref(),
            );
        }
        host.tick(backend, now);
    }

    /// How many data ticks have run.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// The workload monitors' latest results, once per applied process
    /// scan. Process rows are fed only when this box's processes belong on
    /// this bus (not for a `--remote` relay or a plain `--mock` run) and
    /// only the TT-relevant ones (a row holding a TT device or matched as
    /// an inference runtime), with command lines read through
    /// [`snapshots_from_rows`]. `inference` and `training` are what the
    /// views show: under `--mock` the synthetic roster and run.
    pub fn feed_workloads(
        &mut self,
        procs: &[ProcRow],
        inference: Option<&[ServiceState]>,
        training: Option<&TrainState>,
    ) {
        let local = self.local_workloads;
        let Some(host) = self.host_mut() else {
            return;
        };
        if local {
            let rows: Vec<ProcRow> = procs
                .iter()
                .filter(|r| r.tt.is_some() || r.inference.is_some())
                .cloned()
                .collect();
            host.feed_processes(&snapshots_from_rows(&rows));
        }
        if let Some(inf) = inference {
            host.feed_inference(inf);
        }
        if let Some(st) = training {
            // Each process's devices, for the run's chips.
            let devs: Vec<(u32, Vec<usize>)> = procs
                .iter()
                .filter_map(|r| {
                    r.tt.as_ref()
                        .map(|t| (r.pid.max(0) as u32, t.device_indices.clone()))
                })
                .collect();
            host.feed_training(st, Some(&devs));
        }
    }

    /// Feed the hivemind collector's events that were not fed before. Call
    /// only while its collector runs; this never starts one. Cheap when
    /// nothing is new (two integer reads).
    pub fn feed_hivemind(&mut self, h: &crate::workload::hivemind::Hivemind) {
        let ring = h.events();
        let total = ring.len() as u64 + h.dropped();
        if total < self.hivemind_seen {
            // A fresh engine: start over.
            self.hivemind_seen = 0;
        }
        let new = (total - self.hivemind_seen).min(ring.len() as u64) as usize;
        self.hivemind_seen = total;
        if new == 0 {
            return;
        }
        let Some(host) = self.host_mut() else {
            return;
        };
        let fresh: Vec<_> = ring.iter().skip(ring.len() - new).cloned().collect();
        host.feed_hivemind(&fresh);
    }

    /// Publish `ViewChanged { view }` when `view` differs from the last one
    /// published. A redraw of the same view publishes nothing. An attached
    /// TUI cannot publish on another process's bus and does nothing.
    pub fn note_view(&mut self, view: &str) {
        if self.last_view.as_deref() == Some(view) {
            return;
        }
        self.last_view = Some(view.to_string());
        if let Some(h) = self.host() {
            h.publish_view_changed(view);
            self.view_changes += 1;
        }
    }

    /// How many `ViewChanged` events this TUI published.
    pub fn view_changes(&self) -> u64 {
        self.view_changes
    }

    /// The world for a view, once per frame. See [`crate::bus::view`]. An
    /// attached world answers only for the local backend's chips
    /// ([`ReadView::only_chips`]).
    pub fn read(&self) -> ReadView {
        let v = self.view.read(&self.cache);
        if self.is_attached() {
            v.only_chips(self.local_chips.clone())
        } else {
            v
        }
    }

    /// The world with no chip limit, once per frame. For a view that shows
    /// the host's models whatever chips they run on (the `i` view when
    /// attached). Die 3D and every other chip-keyed overlay use
    /// [`TuiBus::read`]. For a local bus the two are the same.
    pub fn read_unfiltered(&self) -> ReadView {
        self.view.read(&self.cache)
    }

    /// True when the bus is local, or attached to a host whose origin
    /// matches this machine ([`origin_mismatch`] finds nothing, as of the
    /// last data tick). Only then do the world's chip keys name this
    /// machine's chips, so only then may they be turned into local
    /// `/dev/tenstorrent/N` indexes.
    pub fn origin_is_local(&self) -> bool {
        match &self.label_key {
            #[cfg(unix)]
            Some(LabelKey::Attached(_, mismatch)) => mismatch.is_none(),
            _ => !self.is_attached(),
        }
    }

    /// The local chip keys an attached world is read for (empty for a local
    /// bus, whose world is built from the local backend).
    pub fn local_chips(&self) -> &BTreeSet<EntityKey> {
        &self.local_chips
    }

    /// Why a backend switch must be refused, as one status line, or `None`
    /// when it may go ahead. A bus that records or serves names one backend
    /// and mode in its log header for the whole run.
    pub fn switch_refusal(&self) -> Option<String> {
        if let Some(d) = &self.records {
            return Some(format!(
                "backend switch refused: the bus is recording to {} for this backend; \
                 restart to switch",
                d.display()
            ));
        }
        if self.serves {
            return Some(
                "backend switch refused: the bus is served on --bus-socket for this \
                 backend; restart to switch"
                    .to_string(),
            );
        }
        None
    }

    /// The TUI swapped its backend (`b` or `/remote`). A local bus re-derives
    /// whether the run is synthetic and whether it relays a `--remote` box,
    /// resets the backend source, and holds `/serve` off the old raw frame
    /// ([`BusHost::mark_backend_switch`]). Call only when
    /// [`TuiBus::switch_refusal`] said nothing.
    pub fn backend_switched(
        &mut self,
        backend_type: crate::cli::BackendType,
        backend_info: &str,
        mock_workloads: bool,
    ) {
        let synthetic = crate::cli::backend_tag(backend_type, backend_info) == "mock";
        let relay = backend_type == crate::cli::BackendType::Remote;
        self.synthetic = synthetic;
        self.relay = relay;
        self.local_workloads =
            crate::bus::headless::uses_local_workloads(relay, synthetic, mock_workloads);
        if let Some(h) = self.host_mut() {
            h.set_synthetic(synthetic);
            h.mark_backend_switch();
        }
    }

    /// Whether this bus feeds a `--remote` frame's extension each tick.
    pub fn relays(&self) -> bool {
        self.relay
    }

    /// Whether readings are marked `Estimated` (a mock backend).
    pub fn synthetic(&self) -> bool {
        self.synthetic
    }

    /// How many times [`TuiBus::read`] had to copy the world.
    pub fn read_rebuilds(&self) -> u64 {
        self.cache.rebuilds()
    }

    /// `bus: local`, `bus: replay`, or `bus: <socket path>` (attached) with
    /// a mismatch and link suffix (see the module doc). Built only when it
    /// changes (on a data tick); this clones it.
    pub fn bus_label(&self) -> String {
        self.label.clone()
    }

    /// [`TuiBus::bus_label`] without the copy, for a per-frame caller.
    pub fn bus_label_str(&self) -> &str {
        &self.label
    }
}

/// The TUI loop's data tick. When `interval` has passed since `*last`:
/// `backend.update()` (a failure is logged), then `*last` moves to the end
/// of the update, then `bus.data_tick`. Returns whether it ran.
///
/// `now` is the loop's clock for this pass. `*last` is set to the later of
/// `now` and the time after the update, so a slow `update()` (a `tt-smi`
/// call) does not count against the next interval, as before.
pub fn run_data_tick(
    last: &mut Instant,
    interval: Duration,
    now: Instant,
    backend: &mut dyn TelemetryBackend,
    bus: Option<&mut TuiBus>,
) -> bool {
    if now.saturating_duration_since(*last) < interval {
        return false;
    }
    if let Err(e) = backend.update() {
        log::warn!("Update failed: {}", e);
    }
    *last = Instant::now().max(now);
    if let Some(b) = bus {
        b.data_tick(&*backend, now);
    }
    true
}

/// The `/serve` frame for this refresh. `backend_frame` is the backend's
/// `snapshot_json()`; with none there is nothing to serve, as before.
///
/// With a local bus the frame is built from the world:
/// [`crate::bus::compat::relay_frame`] for a `--remote` relay (the origin's
/// frame, byte for byte, never the local extension) and
/// [`crate::bus::compat::telemetry_frame`] otherwise. Before the bus holds
/// a raw frame, and for an attached TUI (whose world is another box's
/// host), the backend frame is used the old way.
#[cfg(feature = "remote")]
pub fn serve_frame(
    bus: Option<&TuiBus>,
    backend_frame: Option<String>,
    relay: bool,
    ext: impl FnOnce() -> crate::backend::TtToplikeExt,
) -> Option<String> {
    use crate::bus::compat;
    let frame = backend_frame?;
    // After a backend switch the world's newest raw frame can be the old
    // backend's until the new one publishes; use the backend frame until
    // then.
    let host = bus.and_then(TuiBus::host).filter(|h| h.raw_is_current());
    if relay {
        let from_bus = host.and_then(|h| h.with_world(|w, _| compat::relay_frame(w)));
        return Some(from_bus.unwrap_or(frame));
    }
    let ext = ext();
    let from_bus = host.and_then(|h| h.with_world(|w, _| compat::telemetry_frame(w, &ext)));
    Some(from_bus.unwrap_or_else(|| crate::backend::inject_extension(&frame, &ext)))
}

#[cfg(test)]
#[path = "tui_bus_tests.rs"]
mod tests;
