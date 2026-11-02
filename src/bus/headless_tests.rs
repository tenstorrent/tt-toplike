//! Tests for the headless loop. `MockBackend` (wrapped where a raw `tt-smi`
//! frame is needed), temp directories and sockets in them. No hardware, no
//! gozer command, no network.

use super::*;
use crate::backend::mock::MockBackend;
use crate::error::BackendResult;
use crate::models::device::Device;
use crate::models::telemetry::{SmbusTelemetry, Telemetry};
use std::thread;
use tt_toplike_bus::{Bus, EntityKey, Replay};

/// The bring-up fixture recorded by the bus crate's tests.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/tt-toplike-bus/tests/fixtures/bringup.jsonl"
);

fn mock(n: usize) -> MockBackend {
    let mut b = MockBackend::new(n);
    b.init().unwrap();
    b.update().unwrap();
    b
}

fn cfg(dir: Option<PathBuf>, socket: Option<PathBuf>) -> HeadlessConfig {
    HeadlessConfig {
        bus: BusConfig {
            dir,
            ..BusConfig::default()
        },
        socket,
        interval: Duration::from_millis(20),
        relay: false,
        gozer: false,
        synthetic: false,
        backend_name: "test".into(),
    }
}

/// Wait up to `secs` for `f` to hold.
fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    f()
}

/// The full lifecycle: the loop writes a segment, serves a snapshot with
/// every chip on the socket, and on stop removes the socket file and frees
/// the log directory's lock.
#[cfg(unix)]
#[test]
fn headless_loop_records_serves_and_stops_cleanly() {
    use tt_toplike_bus::SocketClient;
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("bus");
    // The server creates its own 0700 directory, as with the default path.
    let sock = tmp.path().join("run").join("s.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (c, s) = (cfg(Some(log.clone()), Some(sock.clone())), stop.clone());
    let run = thread::spawn(move || {
        let mut b = mock(2);
        run_headless(&mut b, &mut NoWorkloads, c, None, &s)
    });

    let chips = [
        EntityKey::chip("0000:01:00.0"),
        EntityKey::chip("0000:02:00.0"),
    ];
    let served = wait_for(10, || {
        let Ok(mut client) = SocketClient::connect(&sock) else {
            return false;
        };
        let Ok(snap) = client.snapshot() else {
            return false;
        };
        chips
            .iter()
            .all(|c| snap.world.reading(c, crate::bus::signals::TDP_W).is_some())
    });
    assert!(served, "the socket never served both chips");
    // A second host on the same directory is refused while this one runs.
    assert!(Bus::open(BusConfig {
        dir: Some(log.clone()),
        ..BusConfig::default()
    })
    .is_err());

    stop.store(true, Ordering::SeqCst);
    let report = run.join().unwrap().unwrap();
    assert!(
        report.ticks >= 1 && report.head > 0 && report.flushed,
        "{report:?}"
    );
    assert!(!sock.exists(), "the socket file was left behind");
    // The lock is free again (allowing a moment for a child process of
    // another test that shares the lock file between fork and exec), and
    // the log reads back with chip sweeps.
    let reopened = wait_for(2, || {
        Bus::open(BusConfig {
            dir: Some(log.clone()),
            ..BusConfig::default()
        })
        .is_ok()
    });
    assert!(reopened, "the directory lock was released");
    let mut replay = Replay::open(&log).unwrap();
    let sweeps = replay
        .records()
        .filter(|r| matches!(&r.payload, tt_toplike_bus::Payload::Sweep(s) if s.entity == chips[0]))
        .count();
    assert!(sweeps >= 1, "no recorded sweep for chip 0");
}

/// A socket whose directory others can write to is refused before the loop
/// starts, with the reason in the error.
#[cfg(unix)]
#[test]
fn a_socket_in_a_shared_directory_is_refused_with_the_reason() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let mut b = mock(1);
    let err = run_headless(
        &mut b,
        &mut NoWorkloads,
        cfg(None, Some(shared.join("s.sock"))),
        None,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains("must be owned by this user"),
        "{err}"
    );
}

/// The replayed world is served on the socket: a snapshot shows the
/// fixture's model, and the bus says it is a replay.
#[cfg(unix)]
#[test]
fn headless_replay_serves_the_replayed_world() {
    use tt_toplike_bus::SocketClient;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("r.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (c, s) = (cfg(None, Some(sock.clone())), stop.clone());
    let done = Arc::new(std::sync::Mutex::new(None));
    let d = done.clone();
    let run = thread::spawn(move || {
        run_headless_replay(Path::new(FIXTURE), 1000.0, c, &s, |r| {
            *d.lock().unwrap() = Some(r.clone());
        })
    });
    let model = EntityKey::model("llama-8b");
    let served = wait_for(10, || {
        SocketClient::connect(&sock)
            .and_then(|mut c| c.snapshot())
            .is_ok_and(|snap| snap.world.models.iter().any(|m| m.key == model))
    });
    assert!(served, "the replayed model never reached the socket");
    assert!(wait_for(10, || done.lock().unwrap().is_some()));
    assert_eq!(*done.lock().unwrap(), Some(Ok(114)));
    stop.store(true, Ordering::SeqCst);
    let report = run.join().unwrap().unwrap();
    assert!(report.head >= 114, "{report:?}");
    assert!(!sock.exists());
}

/// `--record DIR` on a live host writes segments that `Replay` reads back
/// to the same world the host held.
#[test]
fn recorded_segments_replay_to_an_equal_world() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("rec");
    let b = mock(2);
    let mut host = BusHost::with_gozer(
        BusConfig {
            dir: Some(log.clone()),
            ..BusConfig::default()
        },
        &b,
        GozerSource::fed(),
    )
    .unwrap();
    let t0 = Instant::now();
    for i in 0..3 {
        host.tick(&b, t0 + Duration::from_millis(100 * i));
    }
    assert!(host.bus().flush(Duration::from_secs(5)));
    let live = host.world();
    drop(host);

    let mut replayed = tt_toplike_bus::World::new();
    for r in Replay::open(&log).unwrap().records() {
        replayed.apply(&r);
    }
    assert_eq!(replayed, live);
}

// ---- The /telemetry broadcast ------------------------------------------------

/// `MockBackend` plus a fixed raw `tt-smi` frame, as the JSON and hybrid
/// backends cache one.
#[cfg(feature = "remote")]
struct WithRaw(MockBackend, String);

#[cfg(feature = "remote")]
impl TelemetryBackend for WithRaw {
    fn init(&mut self) -> BackendResult<()> {
        self.0.init()
    }
    fn update(&mut self) -> BackendResult<()> {
        self.0.update()
    }
    fn devices(&self) -> &[Device] {
        self.0.devices()
    }
    fn telemetry(&self, i: usize) -> Option<&Telemetry> {
        self.0.telemetry(i)
    }
    fn smbus_telemetry(&self, i: usize) -> Option<&SmbusTelemetry> {
        self.0.smbus_telemetry(i)
    }
    fn backend_info(&self) -> String {
        "with-raw".into()
    }
    fn snapshot_json(&self) -> Option<String> {
        Some(self.1.clone())
    }
}

#[cfg(feature = "remote")]
const TT_SMI_FRAME: &str = include_str!("fixtures/tt_smi_frame.json");

/// Run the loop for a few ticks with a broadcast sink and return the frames.
#[cfg(feature = "remote")]
fn frames(relay: bool, workloads: &mut dyn Workloads) -> Vec<String> {
    let mut b = WithRaw(mock(2), TT_SMI_FRAME.to_string());
    let stop = AtomicBool::new(false);
    let mut got = Vec::new();
    let mut sink = |f: String| {
        got.push(f);
        if got.len() >= 3 {
            stop.store(true, Ordering::SeqCst);
        }
    };
    let c = HeadlessConfig {
        relay,
        ..cfg(None, None)
    };
    run_headless(&mut b, workloads, c, Some(&mut sink), &stop).unwrap();
    got
}

/// One fixed process row and no inference.
#[cfg(feature = "remote")]
struct OneProc;

#[cfg(feature = "remote")]
impl Workloads for OneProc {
    fn poll(&mut self) -> WorkloadSnapshot {
        WorkloadSnapshot {
            procs: vec![ProcRow {
                pid: 1,
                name: "init".into(),
                cpu_pct: 0.5,
                mem_bytes: 4096,
                inference: None,
                active: false,
                tt: None,
            }],
            inference: Vec::new(),
        }
    }
}

/// The frame equals what the old loop built: the raw `tt-smi` body with the
/// extension for this tick's processes injected.
#[cfg(feature = "remote")]
#[test]
fn serve_frames_are_the_raw_frame_with_the_extension() {
    let got = frames(false, &mut OneProc);
    let ext = crate::backend::build_extension(&OneProc.poll().procs, &[]);
    let want = crate::backend::inject_extension(TT_SMI_FRAME, &ext);
    assert_eq!(got.len(), 3);
    for f in &got {
        assert_eq!(f, &want);
    }
    assert!(want.contains("tt_toplike"), "the extension is present");
}

/// A `--remote` relay re-broadcasts the origin's frame byte for byte and
/// adds no extension of its own.
#[cfg(feature = "remote")]
#[test]
fn relay_frames_are_the_origin_frame_verbatim() {
    let got = frames(true, &mut OneProc);
    assert_eq!(got.len(), 3);
    for f in &got {
        assert_eq!(f, TT_SMI_FRAME);
    }
}

#[cfg(unix)]
#[test]
fn sigterm_raises_the_stop_flag() {
    let flag = install_stop_signals(None).unwrap();
    assert!(!flag.load(Ordering::SeqCst));
    let status = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(wait_for(5, || flag.load(Ordering::SeqCst)));
}

// ---- Fix round 1 -------------------------------------------------------------

/// Raises the stop flag on its first poll, so the loop runs one tick.
struct StopAfterPoll(Arc<AtomicBool>);

impl Workloads for StopAfterPoll {
    fn poll(&mut self) -> WorkloadSnapshot {
        self.0.store(true, Ordering::SeqCst);
        WorkloadSnapshot::default()
    }
}

/// Run one tick into `log` and return the recorded header and every backend
/// sample's provenance.
fn one_tick(
    log: &Path,
    synthetic: bool,
    backend_name: &str,
) -> (tt_toplike_bus::Header, Vec<tt_toplike_bus::Provenance>) {
    let stop = Arc::new(AtomicBool::new(false));
    let c = HeadlessConfig {
        synthetic,
        backend_name: backend_name.into(),
        ..cfg(Some(log.to_path_buf()), None)
    };
    let mut b = mock(2);
    run_headless(&mut b, &mut StopAfterPoll(stop.clone()), c, None, &stop).unwrap();
    let mut replay = Replay::open(log).unwrap();
    let header = replay.header().cloned().unwrap();
    let provs = replay
        .records()
        .filter(|r| r.source == "backend")
        .filter_map(|r| match r.payload {
            tt_toplike_bus::Payload::Sweep(s) => Some(s.prov),
            tt_toplike_bus::Payload::Sample(s) => Some(s.prov),
            _ => None,
        })
        .collect();
    (header, provs)
}

#[test]
fn a_mock_run_records_backend_mock_mode_mock_and_no_measured_values() {
    let tmp = tempfile::tempdir().unwrap();
    let (h, provs) = one_tick(&tmp.path().join("bus"), true, "mock");
    assert_eq!(h.tool_versions["backend"], "mock");
    assert_eq!(h.tool_versions["mode"], "mock");
    assert!(!provs.is_empty());
    assert!(
        provs
            .iter()
            .all(|p| *p == tt_toplike_bus::Provenance::Estimated),
        "{provs:?}"
    );
}

#[test]
fn a_live_run_records_its_backend_and_mode_live() {
    let tmp = tempfile::tempdir().unwrap();
    let (h, provs) = one_tick(&tmp.path().join("bus"), false, "sysfs");
    assert_eq!(h.tool_versions["backend"], "sysfs");
    assert_eq!(h.tool_versions["mode"], "live");
    assert!(provs
        .iter()
        .all(|p| *p == tt_toplike_bus::Provenance::Measured));
}

/// The first SIGTERM/SIGINT raises the flag; the second exits at once with
/// 128 + the signal number, after removing the socket file.
#[cfg(unix)]
#[test]
fn a_second_stop_signal_exits_at_once_and_removes_the_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("s.sock");
    // A socket file nobody listens on, as a crashed server leaves.
    let make_socket = || {
        let _ = std::fs::remove_file(&sock);
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        assert!(sock.exists());
    };
    for (second, code) in [(StopSignal::Int, 130), (StopSignal::Term, 143)] {
        make_socket();
        // Stands for a socket this process bound through `serve_socket`.
        let _mark = ServedMark::new(&sock);
        let flag = AtomicBool::new(false);
        let mut exits = Vec::new();
        watch_stop([StopSignal::Term, second], &flag, Some(&sock), &mut |c| {
            exits.push((c, sock.exists()))
        });
        assert!(flag.load(Ordering::SeqCst));
        assert_eq!(exits, vec![(code, false)], "socket removed before exit");
    }
    // One signal only: the flag, no exit, the socket left for the loop.
    make_socket();
    let flag = AtomicBool::new(false);
    let mut exits = Vec::new();
    watch_stop([StopSignal::Int], &flag, Some(&sock), &mut |c| {
        exits.push(c)
    });
    assert!(flag.load(Ordering::SeqCst) && exits.is_empty() && sock.exists());
    // A path that is not a socket is never removed.
    let file = tmp.path().join("plain");
    std::fs::write(&file, b"keep").unwrap();
    watch_stop(
        [StopSignal::Int, StopSignal::Int],
        &AtomicBool::new(false),
        Some(&file),
        &mut |_| {},
    );
    assert!(file.exists());
}

/// Fix round 1: the mark remembers which socket this process bound. When
/// the file at the path has been replaced by another socket (another host
/// bound the path while this one was still dropping its server), the
/// second signal leaves the new file alone.
#[cfg(unix)]
#[test]
fn a_second_signal_leaves_a_socket_that_replaced_ours() {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixListener;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("s.sock");
    let _ours = UnixListener::bind(&sock).unwrap();
    let mark = ServedMark::new(&sock);
    // Another host's socket, made beside ours (so it cannot reuse our
    // inode) and moved over the path.
    let other = tmp.path().join("other.sock");
    let _theirs = UnixListener::bind(&other).unwrap();
    let ino = std::fs::metadata(&other).unwrap().ino();
    std::fs::rename(&other, &sock).unwrap();
    let mut exits = Vec::new();
    watch_stop(
        [StopSignal::Int, StopSignal::Int],
        &AtomicBool::new(false),
        Some(&sock),
        &mut |c| exits.push(c),
    );
    assert_eq!(exits, vec![130]);
    assert!(sock.exists(), "the other host's socket was removed");
    assert_eq!(std::fs::metadata(&sock).unwrap().ino(), ino);
    drop(mark);
}

// ---- Hardening B1 ------------------------------------------------------------

/// A mock backend whose first `devices()` call says so on `entered` and
/// then blocks until `gate` is sent to: a backend that is slow to start.
struct SlowInit {
    inner: MockBackend,
    hold: std::sync::Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

impl TelemetryBackend for SlowInit {
    fn init(&mut self) -> BackendResult<()> {
        self.inner.init()
    }
    fn update(&mut self) -> BackendResult<()> {
        self.inner.update()
    }
    fn devices(&self) -> &[Device] {
        if let Some((entered, gate)) = self.hold.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = gate.recv();
        }
        self.inner.devices()
    }
    fn telemetry(&self, i: usize) -> Option<&Telemetry> {
        self.inner.telemetry(i)
    }
    fn smbus_telemetry(&self, i: usize) -> Option<&SmbusTelemetry> {
        self.inner.smbus_telemetry(i)
    }
    fn backend_info(&self) -> String {
        "slow-init".into()
    }
}

/// A second instance started on the socket path of a live host, stopped by
/// two signals while its backend is still starting, must leave the live
/// host's socket alone: this process never bound it.
#[cfg(unix)]
#[test]
fn two_signals_during_init_leave_another_hosts_socket_in_place() {
    use std::os::unix::net::UnixStream;
    let tmp = tempfile::tempdir().unwrap();
    // The server creates its own 0700 directory.
    let sock = tmp.path().join("run").join("bus.sock");
    // The live host (in this test process, but not started through
    // `serve_socket`, so it stands for another process).
    let live = tt_toplike_bus::SocketServer::spawn(Bus::open(BusConfig::default()).unwrap(), &sock)
        .unwrap();
    assert!(UnixStream::connect(&sock).is_ok());

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let path2 = sock.clone();
    let second = thread::spawn(move || {
        let mut b = SlowInit {
            inner: mock(1),
            hold: std::sync::Mutex::new(Some((entered_tx, gate_rx))),
        };
        run_headless(&mut b, &mut NoWorkloads, cfg(None, Some(path2)), None, &s2)
    });
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("init started");
    // Two signals while init is still running.
    let mut exits = Vec::new();
    watch_stop(
        [StopSignal::Int, StopSignal::Int],
        &stop,
        Some(&sock),
        &mut |c| exits.push(c),
    );
    assert_eq!(exits, vec![130]);
    assert!(sock.exists(), "the live host's socket was removed");
    assert!(UnixStream::connect(&sock).is_ok());
    // Let init finish: the bind fails, and the socket is still not ours.
    gate_tx.send(()).unwrap();
    let err = second.join().unwrap().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AddrInUse, "{err}");
    watch_stop(
        [StopSignal::Int, StopSignal::Term],
        &stop,
        Some(&sock),
        &mut |_| {},
    );
    assert!(UnixStream::connect(&sock).is_ok());
    drop(live);
}

/// Once this process has bound its socket, a second signal does remove it.
#[cfg(unix)]
#[test]
fn two_signals_after_the_bind_remove_this_hosts_socket() {
    let tmp = tempfile::tempdir().unwrap();
    // The server creates its own 0700 directory.
    let sock = tmp.path().join("run").join("bus.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let path2 = sock.clone();
    let host = thread::spawn(move || {
        let mut b = mock(1);
        run_headless(&mut b, &mut NoWorkloads, cfg(None, Some(path2)), None, &s2)
    });
    assert!(wait_for(10, || serving_socket(&sock)), "bound and marked");
    let mut exits = Vec::new();
    watch_stop(
        [StopSignal::Term, StopSignal::Term],
        &stop,
        Some(&sock),
        &mut |c| exits.push((c, sock.exists())),
    );
    assert_eq!(exits, vec![(143, false)]);
    host.join().unwrap().unwrap();
    // The mark goes with the server.
    assert!(!serving_socket(&sock));
}

#[test]
fn local_workloads_only_for_real_backends_or_on_request() {
    assert!(uses_local_workloads(false, false, false), "real backend");
    assert!(!uses_local_workloads(false, true, false), "mock");
    assert!(
        uses_local_workloads(false, true, true),
        "mock + --mock-workloads"
    );
    assert!(!uses_local_workloads(true, false, false), "relay");
}
