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
