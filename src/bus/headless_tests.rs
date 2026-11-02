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
    // The lock is free again, and the log reads back with chip sweeps.
    drop(
        Bus::open(BusConfig {
            dir: Some(log.clone()),
            ..BusConfig::default()
        })
        .expect("the directory lock was released"),
    );
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
    let flag = install_stop_signals().unwrap();
    assert!(!flag.load(Ordering::SeqCst));
    let status = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(wait_for(5, || flag.load(Ordering::SeqCst)));
}
