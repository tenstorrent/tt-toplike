//! Tests for the socket transport and its client. Every socket lives in a
//! fresh directory under the system temp dir (see `TempDir`), with a short
//! name so the path stays well under the 108-byte `sun_path` limit.

use super::*;
use crate::client::{ClientError, RemoteWorld, SocketClient};
use crate::config::{BusConfig, ManualClock};
use crate::record::{Event, Payload, Provenance, RawFrame, Sample, Sweep, Value};
use crate::replay::ManualPlayClock;
use crate::testutil::TempDir;
use crate::EntityKey;
use std::io::{BufRead, BufReader};

fn sock(tag: &str) -> (TempDir, PathBuf) {
    let d = TempDir::new(tag);
    let p = d.path().join("s");
    assert!(p.as_os_str().len() < 100, "socket path too long: {p:?}");
    (d, p)
}

fn chip(n: u32) -> EntityKey {
    EntityKey::chip(&format!("0000:{n:02x}:00.0"))
}

fn sample(e: &EntityKey, sig: &str, v: Value) -> Payload {
    Payload::Sample(Sample {
        entity: e.clone(),
        signal: sig.into(),
        value: v,
        prov: Provenance::Measured,
    })
}

fn world_bus() -> Bus {
    Bus::open_with_world(BusConfig::default()).unwrap()
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

/// A raw line-level client, for checking exact wire lines.
struct Raw {
    r: BufReader<UnixStream>,
    w: UnixStream,
}

impl Raw {
    fn connect(path: &Path) -> Raw {
        let s = UnixStream::connect(path).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Raw {
            r: BufReader::new(s.try_clone().unwrap()),
            w: s,
        }
    }

    fn send(&mut self, s: &str) {
        self.w.write_all(s.as_bytes()).unwrap();
    }

    fn line(&mut self) -> serde_json::Value {
        let mut l = String::new();
        let n = self.r.read_line(&mut l).unwrap();
        assert!(n > 0, "connection closed before a line arrived");
        serde_json::from_str(&l).unwrap_or_else(|e| panic!("bad line {l:?}: {e}"))
    }

    /// True when the server has closed this connection.
    fn closed(&mut self) -> bool {
        let mut l = String::new();
        match self.r.read_line(&mut l) {
            Ok(0) => true,
            Err(e) => e.kind() == io::ErrorKind::ConnectionReset,
            Ok(_) => panic!("expected the connection to close, got {l:?}"),
        }
    }
}

/// Snapshot then subscribe from its `seq`, while 1000 records are being
/// published: the snapshot holds exactly the records up to `seq`, and the
/// subscription yields every later record exactly once. Sixteen clients
/// take their snapshot at staggered moments during the publishing.
#[test]
fn snapshot_then_subscribe_sees_every_record_exactly_once() {
    const N: u64 = 1000;
    let bus = world_bus();
    let (_d, path) = sock("once");
    let _server = SocketServer::spawn(bus.clone(), &path).unwrap();
    // Publisher and clients start together; the publishing takes about
    // 50 ms and the clients connect about 3 ms apart across it.
    let start = Arc::new(std::sync::Barrier::new(17));
    let publisher = {
        let bus = bus.clone();
        let start = start.clone();
        thread::spawn(move || {
            start.wait();
            for i in 1..=N {
                bus.publish("t", sample(&chip((i % 50) as u32), "n", Value::U64(i)));
                if i % 5 == 0 {
                    thread::sleep(Duration::from_micros(250));
                }
            }
        })
    };
    let clients: Vec<_> = (0..16u64)
        .map(|k| {
            let path = path.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                thread::sleep(Duration::from_millis(3 * k));
                let mut c = SocketClient::connect(&path).unwrap();
                // Twenty snapshots per client, so a race between taking the
                // seq and taking the world has many chances to show.
                let mut snap = c.snapshot().unwrap();
                for _ in 0..20 {
                    snap = c.snapshot().unwrap();
                    // Every record is a sample, so the newest reading in the
                    // snapshot is the record at `seq` exactly.
                    let newest = snap.world.readings.iter().map(|r| r.seq).max();
                    assert_eq!(
                        newest.unwrap_or(0),
                        snap.seq,
                        "the snapshot world must hold every record up to its seq and none after"
                    );
                }
                let mut sub = c.subscribe_from(snap.seq).unwrap();
                let mut want = snap.seq + 1;
                while want <= N {
                    let r = sub
                        .next(Duration::from_secs(5))
                        .unwrap()
                        .expect("a record within 5 s");
                    assert_eq!(r.seq, want, "records must arrive once each, in order");
                    want += 1;
                }
                snap.seq
            })
        })
        .collect();
    publisher.join().unwrap();
    let seqs: Vec<u64> = clients.into_iter().map(|c| c.join().unwrap()).collect();
    // At least one handshake landed while records were being published.
    assert!(
        seqs.iter().any(|&s| s > 0 && s < N),
        "no handshake overlapped the publishing: {seqs:?}"
    );
}

/// Review Focus 4: a subscription from a dropped sequence gets `dropped`,
/// from the future gets `future`, and neither hangs.
#[test]
fn subscribe_from_dropped_or_future_is_a_clear_error() {
    let bus = Bus::open_with_world(BusConfig {
        window_records: 10,
        ..BusConfig::default()
    })
    .unwrap();
    for i in 0..50 {
        bus.publish("t", sample(&chip(1), "n", Value::U64(i)));
    }
    let (_d, path) = sock("err");
    let _server = SocketServer::spawn(bus.clone(), &path).unwrap();

    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"subscribe\",\"from\":5}\n");
    assert_eq!(c.line(), serde_json::json!({"error":"dropped","oldest":41}));
    assert!(c.closed());

    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"subscribe\",\"from\":100}\n");
    assert_eq!(c.line(), serde_json::json!({"error":"future","head":50}));
    assert!(c.closed());

    // The client API maps them to typed errors.
    let mut sub = SocketClient::connect(&path)
        .unwrap()
        .subscribe_from(39)
        .unwrap();
    match sub.next(Duration::from_secs(5)) {
        Err(ClientError::Dropped { oldest: 41 }) => {}
        other => panic!("expected Dropped {{ oldest: 41 }}, got {other:?}"),
    }
    let mut sub = SocketClient::connect(&path)
        .unwrap()
        .subscribe_from(51)
        .unwrap();
    match sub.next(Duration::from_secs(5)) {
        Err(ClientError::Future { head: 50 }) => {}
        other => panic!("expected Future {{ head: 50 }}, got {other:?}"),
    }
    // The oldest kept record is still a valid start.
    let mut sub = SocketClient::connect(&path)
        .unwrap()
        .subscribe_from(40)
        .unwrap();
    assert_eq!(sub.next(Duration::from_secs(5)).unwrap().unwrap().seq, 41);
}

/// A client that never reads is cut once it is `queue_lines` behind with
/// its socket blocked. Another client keeps receiving every record, and
/// `publish` stays fast throughout.
#[test]
fn a_client_that_never_reads_is_cut_and_others_keep_receiving() {
    const N: u64 = 10_000;
    let bus = Bus::open_with_world(BusConfig {
        window_records: 1_000_000,
        ..BusConfig::default()
    })
    .unwrap();
    let (_d, path) = sock("slow");
    let server = SocketServer::spawn_with(
        bus.clone(),
        &path,
        ServerOptions {
            queue_lines: 2000,
            ..ServerOptions::default()
        },
    )
    .unwrap();

    let mut stuck = Raw::connect(&path);
    stuck.send("{\"op\":\"subscribe\",\"from\":0}\n");
    let reader = {
        let path = path.clone();
        thread::spawn(move || {
            let mut sub = SocketClient::connect(&path)
                .unwrap()
                .subscribe_from(0)
                .unwrap();
            let mut want = 1;
            while want <= N {
                let r = sub
                    .next(Duration::from_secs(10))
                    .expect("the reading client must not be cut")
                    .expect("a record within 10 s");
                assert_eq!(r.seq, want);
                want += 1;
            }
        })
    };
    assert!(wait_for(5, || server.clients() == 2));

    let body = "x".repeat(1000);
    let mut worst = Duration::ZERO;
    for i in 1..=N {
        let t = Instant::now();
        bus.publish(
            "t",
            sample(&chip(1), "n", Value::Text(format!("{i}{body}"))),
        );
        worst = worst.max(t.elapsed());
        if i % 50 == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    reader.join().unwrap();
    assert!(
        worst < Duration::from_millis(200),
        "publish must not wait on clients; worst was {worst:?}"
    );
    assert!(
        wait_for(5, || server.clients() == 0),
        "the stuck client must be gone"
    );

    // Drain what the stuck client was sent: whole record lines, then at most
    // one `too_slow` line, then the end of the stream.
    let mut last = String::new();
    loop {
        let mut l = String::new();
        match stuck.r.read_line(&mut l) {
            Ok(0) => break,
            Ok(_) => last = l,
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => break,
            Err(e) => panic!("the stuck client was not disconnected: {e}"),
        }
    }
    if last.ends_with('\n') && last.starts_with("{\"error\"") {
        assert_eq!(last.trim(), "{\"error\":\"too_slow\"}");
    }
}

/// Two servers start at once on a path holding a stale socket. Without the
/// lock both could see the stale file, the slower one could delete the
/// faster one's fresh socket, and both would run. Exactly one may win.
#[test]
fn two_servers_racing_for_one_path_leave_exactly_one() {
    for round in 0..50 {
        let (_d, path) = sock("race");
        let bus = world_bus();
        // Create the private directory and leave a stale socket in it.
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        drop(UnixListener::bind(&path).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (b, bus, path) = (barrier.clone(), bus.clone(), path.clone());
                thread::spawn(move || {
                    b.wait();
                    SocketServer::spawn(bus, &path)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let ok = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 1, "round {round}: {ok} servers started");
        for r in &results {
            if let Err(e) = r {
                assert_eq!(e.kind(), io::ErrorKind::AddrInUse, "round {round}: {e}");
            }
        }
        // The socket on disk belongs to the server that started.
        SocketClient::connect(&path).unwrap().snapshot().unwrap();
    }
}

#[test]
fn the_socket_lock_is_released_when_the_server_drops() {
    let (_d, path) = sock("relock");
    let bus = world_bus();
    let first = SocketServer::spawn(bus.clone(), &path).unwrap();
    let err = SocketServer::spawn(bus.clone(), &path).unwrap_err();
    assert!(err.to_string().contains("in use"), "{err}");
    drop(first);
    assert!(!path.exists(), "the socket file is removed");
    let _second = SocketServer::spawn(bus, &path).unwrap();
}

#[test]
fn a_live_socket_is_refused_and_a_stale_one_replaced() {
    let (_d, path) = sock("live");
    let bus = world_bus();
    let first = SocketServer::spawn(bus.clone(), &path).unwrap();
    let err = SocketServer::spawn(bus.clone(), &path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
    // The first server is untouched.
    SocketClient::connect(&path).unwrap().snapshot().unwrap();
    drop(first);

    // A socket file nobody listens on is stale.
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let _again = SocketServer::spawn(bus.clone(), &path).unwrap();
    SocketClient::connect(&path).unwrap().snapshot().unwrap();

    // Something that is not a socket is never deleted.
    let (_d2, other) = sock("file");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::set_permissions(other.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&other, b"keep me").unwrap();
    let err = SocketServer::spawn(bus, &other).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&other).unwrap(), b"keep me");
}

#[test]
fn socket_is_0600_and_a_new_parent_is_0700() {
    let d = TempDir::new("mode");
    let path = d.path().join("sub").join("s");
    let server = SocketServer::spawn(world_bus(), &path).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(d.path()), 0o700);
    drop(server);
    assert!(!path.exists(), "drop removes the socket file");
}

/// Drop returns in well under 2 s with three clients connected: one idle
/// in the request phase, one idle subscriber, and one subscriber whose
/// socket is full and blocked.
#[test]
fn drop_is_prompt_with_clients_connected() {
    let bus = Bus::open_with_world(BusConfig {
        window_records: 100_000,
        ..BusConfig::default()
    })
    .unwrap();
    let (_d, path) = sock("drop");
    let server = SocketServer::spawn_with(
        bus.clone(),
        &path,
        ServerOptions {
            queue_lines: usize::MAX,
            ..ServerOptions::default()
        },
    )
    .unwrap();
    let mut idle = Raw::connect(&path);
    let mut sub = Raw::connect(&path);
    sub.send("{\"op\":\"subscribe\",\"from\":\"head\"}\n");
    let mut blocked = Raw::connect(&path);
    blocked.send("{\"op\":\"subscribe\",\"from\":0}\n");
    assert!(wait_for(5, || server.clients() == 3));
    let body = "y".repeat(4000);
    for _ in 0..2000 {
        bus.publish("t", sample(&chip(1), "n", Value::Text(body.clone())));
    }
    // Give the blocked client's thread time to fill its socket.
    thread::sleep(Duration::from_millis(300));
    // Drop on another thread. A drop that hangs then fails the test after
    // 2 s and does not hang the test run.
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Instant::now();
    thread::spawn(move || {
        drop(server);
        let _ = tx.send(());
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(2)).is_ok(),
        "drop did not return within 2 s"
    );
    let took = t.elapsed();
    assert!(took < Duration::from_secs(2), "drop took {took:?}");
    assert!(idle.closed());
    // The idle subscriber got the records, then the end of the stream.
    loop {
        let mut l = String::new();
        match sub.r.read_line(&mut l) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => break,
            Err(e) => panic!("subscriber not closed: {e}"),
        }
    }
    assert!(!path.exists());
}

#[test]
fn oversize_and_garbage_requests_get_bad_request_and_a_close() {
    let (_d, path) = sock("bad");
    let _server = SocketServer::spawn(world_bus(), &path).unwrap();
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            {
                let mut v = vec![b'a'; 70 * 1024];
                v.push(b'\n');
                v
            },
            "line too long",
        ),
        // Over the limit with no newline at all.
        (vec![b'{'; 65 * 1024], "line too long"),
        (b"\xff\xfe\x00garbage\n".to_vec(), "not utf-8"),
        (b"{nope\n".to_vec(), "not json"),
        (b"{\"op\":\"dance\"}\n".to_vec(), "unknown op"),
        (b"[1,2]\n".to_vec(), "unknown op"),
        (b"{\"op\":\"raw\"}\n".to_vec(), "bad field"),
        (
            b"{\"op\":\"subscribe\",\"from\":\"tail\"}\n".to_vec(),
            "bad from",
        ),
        (b"{\"op\":\"replay\",\"speed\":0}\n".to_vec(), "bad speed"),
    ];
    for (bytes, detail) in cases {
        let mut c = Raw::connect(&path);
        c.w.write_all(&bytes).unwrap();
        assert_eq!(
            c.line(),
            serde_json::json!({"error":"bad_request","detail":detail}),
            "for {:?}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(40)])
        );
        assert!(c.closed());
    }
}

#[test]
fn the_65th_client_is_busy() {
    let (_d, path) = sock("busy");
    let server = SocketServer::spawn(world_bus(), &path).unwrap();
    let held: Vec<Raw> = (0..64).map(|_| Raw::connect(&path)).collect();
    assert!(wait_for(5, || server.clients() == 64));
    let mut extra = Raw::connect(&path);
    assert_eq!(extra.line(), serde_json::json!({"error":"busy"}));
    assert!(extra.closed());
    drop(held);
    // Slots free up when clients leave.
    assert!(wait_for(5, || server.clients() == 0));
    SocketClient::connect(&path).unwrap().snapshot().unwrap();
}

#[test]
fn a_silent_client_times_out_but_an_idle_subscriber_does_not() {
    let bus = world_bus();
    let (_d, path) = sock("idle");
    let _server = SocketServer::spawn_with(
        bus.clone(),
        &path,
        ServerOptions {
            request_timeout: Duration::from_millis(300),
            ..ServerOptions::default()
        },
    )
    .unwrap();
    let mut silent = Raw::connect(&path);
    let mut sub = SocketClient::connect(&path)
        .unwrap()
        .subscribe_from(0)
        .unwrap();
    assert_eq!(
        silent.line(),
        serde_json::json!({"error":"bad_request","detail":"request timeout"})
    );
    assert!(silent.closed());
    thread::sleep(Duration::from_millis(700));
    bus.publish("t", sample(&chip(1), "n", Value::U64(1)));
    let r = sub.next(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(r.seq, 1);
}

#[test]
fn split_requests_and_two_requests_in_one_write_both_work() {
    let bus = world_bus();
    bus.publish(
        "tt-smi",
        Payload::Raw(RawFrame {
            source: "tt-smi".into(),
            body: "{\"device_info\":[]}".into(),
        }),
    );
    let (_d, path) = sock("split");
    let _server = SocketServer::spawn(bus, &path).unwrap();
    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"snap");
    thread::sleep(Duration::from_millis(50));
    c.send("shot\"}");
    thread::sleep(Duration::from_millis(50));
    c.send("\n");
    let snap = c.line();
    assert_eq!(snap["seq"], 1);
    assert_eq!(
        snap["world"]["raw"],
        serde_json::json!([{"source":"tt-smi","bytes":18}])
    );

    c.send("{\"op\":\"snapshot\"}\n{\"op\":\"raw\",\"source\":\"tt-smi\"}\n");
    assert_eq!(c.line()["seq"], 1);
    assert_eq!(
        c.line(),
        serde_json::json!({"seq":1,"source":"tt-smi","body":"{\"device_info\":[]}"})
    );
    c.send("{\"op\":\"raw\",\"source\":\"none\"}\n");
    assert_eq!(
        c.line(),
        serde_json::json!({"seq":1,"source":"none","body":null})
    );
}

/// A bus recording to a temp directory, with a manual clock: ten records
/// 100 ms apart on two chips, a reset event, and a sweep.
fn recorded_bus(d: &TempDir) -> (Bus, Arc<ManualClock>) {
    let clock = ManualClock::new(1_000_000);
    let bus = Bus::open_with_world(BusConfig {
        dir: Some(d.path().join("log")),
        clock: clock.clone(),
        ..BusConfig::default()
    })
    .unwrap();
    for i in 0..10u64 {
        clock.advance(Duration::from_millis(100));
        let c = chip(1 + (i % 2) as u32);
        let sig = if i % 2 == 0 { "a" } else { "b" };
        bus.publish("t", sample(&c, sig, Value::U64(i)));
    }
    clock.advance(Duration::from_millis(100));
    bus.publish(
        "t",
        Payload::Event(Event::Reset {
            chips: vec![chip(1)],
        }),
    );
    clock.advance(Duration::from_millis(100));
    bus.publish(
        "t",
        Payload::Sweep(Sweep {
            entity: chip(1),
            signals: vec!["a".into(), "c".into()],
            values: vec![Value::U64(7), Value::U64(8)],
            prov: Provenance::Derived,
        }),
    );
    assert!(bus.flush(Duration::from_secs(5)));
    (bus, clock)
}

/// Read a replay stream: the header, the records, and the done count.
fn read_replay(c: &mut Raw) -> Vec<Record> {
    let h = c.line();
    assert_eq!(h["header"]["kind"], "Replay", "{h}");
    let mut out = Vec::new();
    loop {
        let v = c.line();
        if let Some(n) = v.get("done") {
            assert_eq!(n.as_u64().unwrap(), out.len() as u64);
            break;
        }
        out.push(Record::from_line(&v.to_string()).unwrap());
    }
    assert!(c.closed());
    out
}

#[test]
fn replay_streams_the_recording_paced_and_filtered() {
    let d = TempDir::new("replay");
    let (bus, _clock) = recorded_bus(&d);
    let head = bus.head();
    let play = Arc::new(ManualPlayClock::new());
    let path = d.path().join("s");
    let server = SocketServer::spawn_with(
        bus.clone(),
        &path,
        ServerOptions {
            play_clock: play.clone(),
            ..ServerOptions::default()
        },
    )
    .unwrap();

    // No filter: every recorded record, original numbers, paced by speed.
    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"replay\",\"speed\":2}\n");
    let all = read_replay(&mut c);
    assert_eq!(
        all.iter().map(|r| r.seq).collect::<Vec<_>>(),
        (1..=head).collect::<Vec<_>>()
    );
    let span = all.last().unwrap().t_mono_ns - all[0].t_mono_ns;
    assert_eq!(play.now_ns(), span / 2, "paced at speed 2");
    assert_eq!(bus.head(), head, "replay publishes nothing into the bus");

    // Entity and signal filters; the sweep is trimmed to the signal.
    let before = play.now_ns();
    let mut c = Raw::connect(&path);
    c.send(&format!(
        "{{\"op\":\"replay\",\"speed\":4,\"entities\":[\"{}\"],\"signals\":[\"a\"]}}\n",
        chip(1)
    ));
    let got = read_replay(&mut c);
    assert_eq!(got.len(), 6, "five samples of a on chip 1 and the sweep");
    for r in &got {
        match &r.payload {
            Payload::Sample(s) => assert_eq!((&s.entity, s.signal.as_str()), (&chip(1), "a")),
            Payload::Sweep(s) => {
                assert_eq!(s.signals, vec!["a".to_string()]);
                assert_eq!(s.values, vec![Value::U64(7)]);
            }
            other => panic!("filtered out: {other:?}"),
        }
    }
    let span = got.last().unwrap().t_mono_ns - got[0].t_mono_ns;
    assert_eq!(play.now_ns() - before, span / 4, "paced at speed 4");

    // An entity filter alone keeps the event that names the entity.
    let mut c = Raw::connect(&path);
    c.send(&format!(
        "{{\"op\":\"replay\",\"entities\":[\"{}\"]}}\n",
        chip(1)
    ));
    let got = read_replay(&mut c);
    assert!(got
        .iter()
        .any(|r| matches!(r.payload, Payload::Event(Event::Reset { .. }))));
    assert!(got.iter().all(|r| match &r.payload {
        Payload::Sample(s) => s.entity == chip(1),
        _ => true,
    }));

    // A start time skips earlier records.
    let from = all[4].t_wall_ms;
    let mut c = Raw::connect(&path);
    c.send(&format!("{{\"op\":\"replay\",\"from_ts_ms\":{from}}}\n"));
    let got = read_replay(&mut c);
    assert_eq!(got[0].seq, all[4].seq);
    assert_eq!(got.len(), all.len() - 4);
    drop(server);

    // A memory-only bus has nothing recorded.
    let (_d2, path2) = sock("norec");
    let _server = SocketServer::spawn(world_bus(), &path2).unwrap();
    let mut c = Raw::connect(&path2);
    c.send("{\"op\":\"replay\"}\n");
    assert_eq!(c.line(), serde_json::json!({"error":"no_recording"}));
    assert!(c.closed());
}

/// `RemoteWorld` converges to the server's world, and after the server
/// restarts on the same path it resumes from the last record it applied:
/// it ends equal to the server's summary, with no event applied twice.
#[test]
fn remote_world_converges_and_resumes_after_a_restart() {
    let bus = world_bus();
    let llama = EntityKey::model("llama");
    bus.publish(
        "t",
        Payload::Event(Event::ModelReady {
            model: llama.clone(),
            chips: vec![chip(1)],
        }),
    );
    bus.publish(
        "t",
        sample(&llama, crate::signals::MODEL_TOKENS_PER_S, Value::F64(9.5)),
    );
    let (_d, path) = sock("remote");
    let server = SocketServer::spawn(bus.clone(), &path).unwrap();
    let rw = RemoteWorld::connect(&path).unwrap();
    assert!(rw.connected());
    bus.publish("t", sample(&chip(1), "chip.temp_c", Value::F64(51.0)));
    assert!(wait_for(5, || rw.latest().seq == bus.head()));
    let v = rw.latest();
    assert_eq!(v.models().len(), 1);
    assert_eq!(v.models()[0].tokens_per_s, Some(9.5));
    assert_eq!(
        v.reading(&chip(1), "chip.temp_c").unwrap().value,
        Value::F64(51.0)
    );

    drop(server);
    assert!(wait_for(5, || !rw.connected()));
    for _ in 0..5 {
        bus.publish(
            "t",
            Payload::Event(Event::Reset {
                chips: vec![chip(1)],
            }),
        );
    }
    let _server = SocketServer::spawn(bus.clone(), &path).unwrap();
    bus.publish("t", sample(&chip(1), "chip.temp_c", Value::F64(52.0)));
    assert!(
        wait_for(15, || rw.latest().seq == bus.head()),
        "the view must catch up after the restart"
    );
    assert!(rw.connected());

    let mut got = rw.latest().world;
    let snap = bus.snapshot().unwrap();
    let mut want = WorldSummary::from_world(&snap.world, bus.now_mono_ns());
    got.set_now(0);
    want.set_now(0);
    assert_eq!(got, want);
    let seqs: Vec<u64> = got.events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
}

// ---------------------------------------------------------------------
// Fix round 1
// ---------------------------------------------------------------------

/// The models in a view, by key.
fn model_keys(v: &crate::client::RemoteView) -> Vec<String> {
    v.models().iter().map(|m| m.key.to_string()).collect()
}

/// Restart with a NEW `Bus` on the same log directory: the sequence goes
/// on past the client's last record, but the world starts empty. The view
/// must follow the new world (the old model gone, the new one there).
#[test]
fn remote_world_follows_a_new_bus_on_the_same_directory() {
    let d = TempDir::new("newbus");
    let cfg = || BusConfig {
        dir: Some(d.path().join("log")),
        ..BusConfig::default()
    };
    let path = d.path().join("s");
    let old = Bus::open_with_world(cfg()).unwrap();
    old.publish(
        "t",
        Payload::Event(Event::ModelReady {
            model: EntityKey::model("old"),
            chips: vec![chip(1)],
        }),
    );
    let server = SocketServer::spawn(old.clone(), &path).unwrap();
    let rw = RemoteWorld::connect(&path).unwrap();
    assert!(wait_for(5, || rw.latest().seq == old.head()));
    assert_eq!(model_keys(&rw.latest()), vec!["model:old"]);
    let old_head = old.head();
    drop(server);
    assert!(old.flush(Duration::from_secs(5)));
    drop(old);

    let new = Bus::open_with_world(cfg()).unwrap();
    assert!(
        new.head() >= old_head,
        "the new bus continues the numbering"
    );
    new.publish(
        "t",
        Payload::Event(Event::ModelReady {
            model: EntityKey::model("new"),
            chips: vec![chip(2)],
        }),
    );
    let _server = SocketServer::spawn(new.clone(), &path).unwrap();
    assert!(
        wait_for(8, || model_keys(&rw.latest()) == vec!["model:new"]),
        "the view kept {:?}",
        model_keys(&rw.latest())
    );
    let mut got = rw.latest().world;
    let mut want = WorldSummary::from_world(&new.snapshot().unwrap().world, 0);
    got.set_now(0);
    want.set_now(0);
    assert_eq!(got, want);
}

/// A crash-like restart: a new bus reuses sequence numbers the client has
/// already applied, and has passed the client's last one.
#[test]
fn remote_world_recovers_when_a_new_bus_reuses_sequence_numbers() {
    let (_d, path) = sock("reuse");
    let a = world_bus();
    for i in 0..5 {
        a.publish("t", sample(&chip(1), "a", Value::U64(i)));
    }
    let server = SocketServer::spawn(a.clone(), &path).unwrap();
    let rw = RemoteWorld::connect(&path).unwrap();
    assert!(wait_for(5, || rw.latest().seq == 5));
    drop(server);
    drop(a);

    let b = world_bus();
    for i in 0..7 {
        b.publish("t", sample(&chip(2), "b", Value::U64(i)));
    }
    let _server = SocketServer::spawn(b.clone(), &path).unwrap();
    assert!(
        wait_for(8, || rw.latest().reading(&chip(1), "a").is_none()
            && rw.latest().seq == 7),
        "the view kept the old bus's readings"
    );
    let mut got = rw.latest().world;
    let mut want = WorldSummary::from_world(&b.snapshot().unwrap().world, 0);
    got.set_now(0);
    want.set_now(0);
    assert_eq!(got, want);
}

#[test]
fn a_subscribe_reply_starts_with_the_instance_header() {
    let bus = world_bus();
    bus.publish("t", sample(&chip(1), "a", Value::U64(1)));
    let (_d, path) = sock("hdr");
    let _server = SocketServer::spawn(bus.clone(), &path).unwrap();
    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"snapshot\"}\n{\"op\":\"subscribe\",\"from\":0}\n");
    let snap = c.line();
    assert_eq!(snap["instance"], bus.instance());
    assert_eq!(
        c.line(),
        serde_json::json!({"subscribed":{
            "from":0,
            "instance":bus.instance(),
            "host":crate::host_identity(),
            "backend":"",
            "mode":""
        }})
    );
    assert_eq!(snap["host"], crate::host_identity());
    assert_eq!(c.line()["q"], 1);
    // A second bus has a different id.
    assert_ne!(world_bus().instance(), bus.instance());
}

/// A scripted server for client tests. Connection `i` uses script entry
/// `min(i, last)`: the snapshot line it answers with, and the lines it
/// sends after a subscribe request (then it holds the connection open).
struct FakeServer {
    stop: Arc<AtomicBool>,
    snapshots: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl FakeServer {
    fn start(path: &Path, script: Vec<(String, Vec<String>)>) -> FakeServer {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let snapshots = Arc::new(AtomicU64::new(0));
        let (st, sn) = (stop.clone(), snapshots.clone());
        let thread = thread::spawn(move || {
            let mut n = 0;
            while !st.load(Ordering::SeqCst) {
                let Ok((s, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                };
                s.set_nonblocking(false).unwrap();
                let (snap, lines) = script[n.min(script.len() - 1)].clone();
                n += 1;
                let (st, sn) = (st.clone(), sn.clone());
                thread::spawn(move || {
                    s.set_read_timeout(Some(Duration::from_millis(100)))
                        .unwrap();
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut w = s;
                    loop {
                        if st.load(Ordering::SeqCst) {
                            return;
                        }
                        let mut l = String::new();
                        match r.read_line(&mut l) {
                            Ok(0) => return,
                            Ok(_) if l.contains("snapshot") => {
                                sn.fetch_add(1, Ordering::SeqCst);
                                let _ = w.write_all(format!("{snap}\n").as_bytes());
                            }
                            Ok(_) if l.contains("subscribe") => {
                                for x in &lines {
                                    let _ = w.write_all(format!("{x}\n").as_bytes());
                                }
                            }
                            Ok(_) => {}
                            Err(_) => {}
                        }
                    }
                });
            }
        });
        FakeServer {
            stop,
            snapshots,
            thread: Some(thread),
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn rec_line(seq: u64, sig: &str) -> String {
    Record {
        seq,
        t_mono_ns: seq,
        t_wall_ms: 0,
        source: "t".into(),
        payload: sample(&chip(1), sig, Value::U64(seq)),
    }
    .to_line()
}

/// A snapshot line at `seq` holding one reading named `sig`.
fn snap_line(seq: u64, instance: u64, sig: &str) -> String {
    let mut w = WorldSummary::default();
    w.apply(&Record::from_line(&rec_line(seq, sig)).unwrap());
    serde_json::to_string(&RemoteSnapshot {
        seq,
        instance,
        kind: crate::HeaderKind::Live,
        host: String::new(),
        backend: String::new(),
        mode: String::new(),
        world: w,
    })
    .unwrap()
}

fn header_line(from: u64, instance: u64) -> String {
    format!("{{\"subscribed\":{{\"from\":{from},\"instance\":{instance}}}}}")
}

/// Run a `RemoteWorld` against a server whose first subscription sends
/// `first`, and whose second connection answers a snapshot at seq 10
/// holding reading `fresh`. The view must end on that second snapshot.
fn resnapshots_after(tag: &str, first: Vec<String>) {
    let (_d, path) = sock(tag);
    let fake = FakeServer::start(
        &path,
        vec![
            (snap_line(1, 7, "old"), first),
            (snap_line(10, 7, "fresh"), vec![header_line(10, 7)]),
        ],
    );
    let rw = RemoteWorld::connect(&path).unwrap();
    assert!(
        wait_for(5, || rw.latest().seq == 10),
        "no fresh snapshot; view at seq {}",
        rw.latest().seq
    );
    let v = rw.latest();
    assert!(v.reading(&chip(1), "fresh").is_some());
    assert!(v.reading(&chip(1), "old").is_none());
    assert_eq!(fake.snapshots.load(Ordering::SeqCst), 2);
}

#[test]
fn remote_world_resnapshots_on_a_gap() {
    resnapshots_after(
        "gap",
        vec![header_line(1, 7), rec_line(2, "old"), rec_line(4, "old")],
    );
}

#[test]
fn remote_world_resnapshots_on_a_duplicate() {
    resnapshots_after(
        "dup",
        vec![header_line(1, 7), rec_line(2, "old"), rec_line(2, "old")],
    );
}

#[test]
fn remote_world_resnapshots_on_an_instance_change() {
    resnapshots_after("inst", vec![header_line(1, 8), rec_line(2, "old")]);
}

/// Hardening B5: a server that answers every subscription with `too_slow`
/// (or has a sequence bug) used to get snapshot requests back to back with
/// no delay. Consecutive snapshots are now at least 250 ms apart.
#[test]
fn remote_world_waits_250_ms_between_at_once_snapshots() {
    let (_d, path) = sock("floor");
    let fake = FakeServer::start(
        &path,
        vec![(
            snap_line(1, 7, "x"),
            vec![r#"{"error":"too_slow"}"#.to_string()],
        )],
    );
    let clock = Arc::new(ManualPlayClock::new());
    let rw =
        RemoteWorld::connect_with_clock(&path, Duration::from_secs(10), clock.clone()).unwrap();
    assert!(
        wait_for(5, || fake.snapshots.load(Ordering::SeqCst) >= 6),
        "{} snapshots",
        fake.snapshots.load(Ordering::SeqCst)
    );
    drop(rw);
    let n = fake.snapshots.load(Ordering::SeqCst);
    // The first snapshot is the connect; each later one waited first.
    assert!(
        clock.now_ns() >= (n - 1) * 250_000_000,
        "{n} snapshots in {} ns of clock time",
        clock.now_ns()
    );
}

#[test]
fn replay_speed_out_of_range_is_bad_request() {
    let d = TempDir::new("speed");
    let (bus, _clock) = recorded_bus(&d);
    let path = d.path().join("s");
    let _server = SocketServer::spawn(bus, &path).unwrap();
    for speed in ["1e-300", "0", "-1", "0.009", "1001", "\"NaN\"", "1e309"] {
        let mut c = Raw::connect(&path);
        c.send(&format!("{{\"op\":\"replay\",\"speed\":{speed}}}\n"));
        let l = c.line();
        assert_eq!(l["error"], "bad_request", "speed {speed}: {l}");
        assert!(c.closed());
    }
    for speed in ["0.01", "1000"] {
        let mut c = Raw::connect(&path);
        c.send(&format!("{{\"op\":\"replay\",\"speed\":{speed}}}\n"));
        assert_eq!(c.line()["header"]["kind"], "Replay", "speed {speed}");
    }
}

/// A recorded bus with two records `gap` apart (recorded time).
fn gap_bus(d: &TempDir, gap: Duration) -> Bus {
    let clock = ManualClock::new(1_000_000);
    let bus = Bus::open_with_world(BusConfig {
        dir: Some(d.path().join("log")),
        clock: clock.clone(),
        ..BusConfig::default()
    })
    .unwrap();
    bus.publish("t", sample(&chip(1), "a", Value::U64(1)));
    clock.advance(gap);
    bus.publish("t", sample(&chip(1), "a", Value::U64(2)));
    clock.advance(Duration::from_millis(500));
    bus.publish("t", sample(&chip(1), "a", Value::U64(3)));
    assert!(bus.flush(Duration::from_secs(5)));
    bus
}

/// 64 clients wait in a slow replay and then close: their slots free up
/// within about a second, so the next client is served.
#[test]
fn closed_replay_clients_free_their_slots() {
    let d = TempDir::new("rslot");
    let bus = gap_bus(&d, Duration::from_secs(30));
    let path = d.path().join("s");
    let server = SocketServer::spawn(bus, &path).unwrap();
    let mut held: Vec<Raw> = (0..64)
        .map(|_| {
            let mut c = Raw::connect(&path);
            // 30 s of recorded time at speed 0.01 would be 3000 s; each
            // wait is capped at 1 s, so a client stays about 2 s, longer
            // than the 1.5 s this test allows for the slots to free.
            c.send("{\"op\":\"replay\",\"speed\":0.01}\n");
            c
        })
        .collect();
    for c in &mut held {
        assert_eq!(c.line()["header"]["kind"], "Replay");
        assert_eq!(c.line()["q"], 1);
    }
    assert_eq!(server.clients(), 64);
    let t = Instant::now();
    drop(held);
    assert!(
        wait_for(2, || server.clients() == 0),
        "{} replay clients still held",
        server.clients()
    );
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t.elapsed()
    );
    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"snapshot\"}\n");
    assert!(c.line().get("seq").is_some(), "the next client is served");
}

/// A gap of more than 60 s of recorded time is replayed as a 1 s wait.
#[test]
fn a_long_recorded_gap_is_compressed() {
    let d = TempDir::new("rgap");
    let bus = gap_bus(&d, Duration::from_secs(3600));
    let play = Arc::new(ManualPlayClock::new());
    let path = d.path().join("s");
    let _server = SocketServer::spawn_with(
        bus,
        &path,
        ServerOptions {
            play_clock: play.clone(),
            ..ServerOptions::default()
        },
    )
    .unwrap();
    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"replay\",\"speed\":2}\n");
    assert_eq!(read_replay(&mut c).len(), 3);
    // 1 s for the hour-long gap, then 500 ms / 2.
    assert_eq!(play.now_ns(), 1_000_000_000 + 250_000_000);
}

/// Hardening B4: every wait is capped at 1 s after the speed division. A
/// 59 s gap (under the 60 s long-gap rule) at speed 0.01 was a 5900 s wait
/// while the client stayed connected.
#[test]
fn each_replay_wait_is_capped_at_one_second_after_the_speed() {
    let d = TempDir::new("rcap");
    let bus = gap_bus(&d, Duration::from_secs(59));
    let play = Arc::new(ManualPlayClock::new());
    let path = d.path().join("s");
    let _server = SocketServer::spawn_with(
        bus,
        &path,
        ServerOptions {
            play_clock: play.clone(),
            ..ServerOptions::default()
        },
    )
    .unwrap();
    let mut c = Raw::connect(&path);
    c.send("{\"op\":\"replay\",\"speed\":0.01}\n");
    assert_eq!(read_replay(&mut c).len(), 3);
    // 59 s / 0.01 capped at 1 s, then 500 ms / 0.01 = 50 s capped at 1 s.
    assert_eq!(play.now_ns(), 2_000_000_000);
}

/// A server that sends a line longer than 16 MiB gets a decode error.
#[test]
fn the_client_refuses_a_line_over_16_mib() {
    let (_d, path) = sock("huge");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut req = [0u8; 64];
        let _ = s.read(&mut req);
        let chunk = vec![b'a'; 1 << 20];
        for _ in 0..20 {
            if s.write_all(&chunk).is_err() {
                return;
            }
        }
    });
    let mut c = SocketClient::connect(&path).unwrap();
    let err = c.snapshot().unwrap_err();
    match err.get_ref().and_then(|e| e.downcast_ref::<ClientError>()) {
        Some(ClientError::Decode(_)) => {}
        other => panic!("expected a decode error, got {other:?} ({err})"),
    }
    drop(c);
    server.join().unwrap();
}

#[test]
fn a_parent_directory_others_can_write_is_refused() {
    let d = TempDir::new("open");
    fs::create_dir_all(d.path()).unwrap();
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o777)).unwrap();
    let path = d.path().join("s");
    let err = SocketServer::spawn(world_bus(), &path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    assert!(!path.exists());
    // Group-writable is refused too; 0755 is fine.
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o770)).unwrap();
    let err = SocketServer::spawn(world_bus(), &path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let _server = SocketServer::spawn(world_bus(), &path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn a_symlink_at_the_socket_path_is_refused() {
    let d = TempDir::new("link");
    fs::create_dir_all(d.path()).unwrap();
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = d.path().join("target");
    let path = d.path().join("s");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let err = SocketServer::spawn(world_bus(), &path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    assert!(!target.exists());
    assert!(fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
}

// ---- Task 10b fix round 1 ----------------------------------------------------

/// The link goes from connected to reconnecting when the server goes away,
/// and to disconnected once a reconnect has failed.
#[test]
fn link_reports_reconnecting_then_disconnected() {
    use crate::client::Link;
    let bus = world_bus();
    bus.publish("t", sample(&chip(1), "a", Value::U64(1)));
    let (_d, path) = sock("link");
    let server = SocketServer::spawn(bus.clone(), &path).unwrap();
    let rw = RemoteWorld::connect(&path).unwrap();
    assert_eq!(rw.link(), Link::Connected);
    drop(server);
    assert!(
        wait_for(3, || rw.link() == Link::Reconnecting),
        "{:?}",
        rw.link()
    );
    // Nothing listens any more: the first retry (after 1 s) fails.
    assert!(
        wait_for(5, || rw.link() == Link::Disconnected),
        "{:?}",
        rw.link()
    );
}

/// Dropping a remote world whose follower is stuck waiting on a hung host
/// returns within about half a second.
#[test]
fn dropping_a_remote_world_on_a_hung_host_is_bounded() {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let bus = world_bus();
    let (_d, path) = sock("hung");
    let server = SocketServer::spawn(bus.clone(), &path).unwrap();
    let rw = RemoteWorld::connect(&path).unwrap();
    drop(server);
    // A host that accepts and never answers.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let a = accepted.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for c in listener.incoming() {
            a.fetch_add(1, Ordering::SeqCst);
            held.push(c);
        }
    });
    assert!(
        wait_for(10, || accepted.load(Ordering::SeqCst) > 0),
        "the follower never reconnected"
    );
    let t = std::time::Instant::now();
    drop(rw);
    let took = t.elapsed();
    assert!(took < Duration::from_millis(900), "{took:?}");
}
