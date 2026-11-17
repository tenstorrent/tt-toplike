//! Tests for `BusHost`. `MockBackend` and a small swappable backend only: no
//! hardware, no gozer command (every host here uses `GozerSource::fed`).

use super::*;
use crate::backend::mock::MockBackend;
use crate::bus::signals as sig;
use crate::error::BackendResult;
use crate::models::device::Device;
use crate::models::telemetry::{SmbusTelemetry, Telemetry};
use crate::workload::hivemind::event::{EventKind, Severity, Source as SniffSource};
use tt_toplike_bus::{EntityKey, HeaderKind, Record, Value};

const A: &str = "0000:01:00.0";
const B: &str = "0000:02:00.0";

/// The bring-up fixture recorded by the bus crate's tests.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/tt-toplike-bus/tests/fixtures/bringup.jsonl"
);

/// A backend whose device list a test sets directly. It reports no
/// telemetry: only the device list matters to the identity map.
#[derive(Default)]
struct Devices(Vec<Device>);

impl Devices {
    fn with(list: &[(usize, &str)]) -> Devices {
        Devices(
            list.iter()
                .map(|(i, b)| Device::new(*i, "n150".into(), (*b).into(), String::new()))
                .collect(),
        )
    }
}

impl TelemetryBackend for Devices {
    fn init(&mut self) -> BackendResult<()> {
        Ok(())
    }
    fn update(&mut self) -> BackendResult<()> {
        Ok(())
    }
    fn devices(&self) -> &[Device] {
        &self.0
    }
    fn telemetry(&self, _: usize) -> Option<&Telemetry> {
        None
    }
    fn smbus_telemetry(&self, _: usize) -> Option<&SmbusTelemetry> {
        None
    }
    fn backend_info(&self) -> String {
        "devices".into()
    }
}

fn mock(n: usize) -> MockBackend {
    let mut b = MockBackend::new(n);
    b.init().unwrap();
    b.update().unwrap();
    b
}

fn host(backend: &dyn TelemetryBackend) -> BusHost {
    BusHost::with_gozer(BusConfig::default(), backend, GozerSource::fed()).unwrap()
}

fn records(h: &BusHost) -> Vec<Record> {
    h.bus().read_from(1, usize::MAX).unwrap()
}

fn proc_snap(pid: u32, devs: &[usize]) -> ProcSnapshot {
    ProcSnapshot {
        pid,
        start_ticks: 7,
        name: "python".into(),
        cmdline: "python serve.py --port 8000".into(),
        cpu_pct: 1.0,
        rss_bytes: 10,
        device_indices: devs.to_vec(),
        uses_tt: None,
    }
}

fn text(w: &World, e: &EntityKey, signal: &str) -> Option<String> {
    match w.reading(e, signal)?.value {
        Value::Text(t) => Some(t),
        _ => None,
    }
}

#[test]
fn tick_publishes_one_sweep_per_chip_and_the_world_shows_them() {
    let b = mock(3);
    let mut h = host(&b);
    h.tick(&b, Instant::now());
    let recs = records(&h);
    let w = h.world();
    assert_eq!(b.devices().len(), 3);
    for d in b.devices() {
        let key = EntityKey::chip(&d.bus_id);
        let sweeps = recs
            .iter()
            .filter(|r| matches!(&r.payload, Payload::Sweep(s) if s.entity == key))
            .count();
        assert_eq!(sweeps, 1, "{key}");
        assert!(w.reading(&key, sig::TDP_W).is_some(), "{key} has no power");
    }
    // `with_world` reads the same world without a copy.
    let n = h.with_world(|w, seq| {
        assert_eq!(seq, w.seq());
        w.entities(tt_toplike_bus::EntityKind::Chip).count()
    });
    assert_eq!(n, 3);
}

/// Review Focus 5: two chips swap indexes between ticks. The process and
/// hivemind records fed after the swap name index 0, which is now chip B.
/// They must land on B. This holds only if `tick` refreshes the map before
/// it polls the sources.
#[test]
fn tick_refreshes_the_identity_map_before_index_only_sources() {
    let mut b = Devices::with(&[(0, A), (1, B)]);
    let mut h = host(&b);
    let p = EntityKey::process(42, 7);
    h.feed_processes(&[proc_snap(42, &[0])]);
    h.tick(&b, Instant::now());
    assert_eq!(
        text(&h.world(), &p, sig::PROC_DEVICE).as_deref(),
        Some(format!("chip:{A}").as_str())
    );

    // The swap: A is now index 1 and B index 0.
    b = Devices::with(&[(0, B), (1, A)]);
    h.feed_processes(&[proc_snap(42, &[0])]);
    h.feed_hivemind(&[SniffEvent {
        ts: Instant::now(),
        source: SniffSource::TtMetal,
        device: Some(0),
        severity: Severity::Info,
        kind: EventKind::Compile,
        text: String::new(),
        origin: String::new(),
    }]);
    h.tick(&b, Instant::now());
    let w = h.world();
    assert_eq!(
        text(&w, &p, sig::PROC_DEVICE).as_deref(),
        Some(format!("chip:{B}").as_str()),
        "the process landed on the chip index 0 named before the swap"
    );
    let compiled = w.events().iter().find_map(|(_, e)| match e {
        Event::KernelCompiled { chip, .. } => Some(chip.clone()),
        _ => None,
    });
    assert_eq!(compiled, Some(Some(EntityKey::chip(B))));
}

#[test]
fn maintenance_runs_every_60_s_and_resend_every_5_min() {
    let b = Devices::with(&[(0, A)]);
    let mut h = host(&b);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let cmdlines = |h: &BusHost| {
        records(h)
            .iter()
            .filter(|r| matches!(&r.payload, Payload::Sample(s) if s.signal == sig::PROC_CMDLINE))
            .count()
    };

    for (secs, maint, resends, cmd) in [
        (0, 0, 0, 1), // first tick: text sent once, timers start
        (30, 0, 0, 1),
        (60, 1, 0, 1),
        (119, 1, 0, 1),
        (120, 2, 0, 1),
        (299, 3, 0, 1),
        (300, 3, 1, 2), // 5 min: the command line goes out again
        (360, 4, 1, 2),
        (600, 5, 2, 3),
    ] {
        h.feed_processes(&[proc_snap(42, &[0])]);
        h.tick(&b, at(secs));
        assert_eq!(
            (h.maintenance_runs(), h.resends(), cmdlines(&h)),
            (maint, resends, cmd),
            "at {secs} s"
        );
    }
}

#[test]
fn resend_reaches_every_text_signal_of_a_process() {
    let b = Devices::with(&[(0, A)]);
    let mut h = host(&b);
    let t0 = Instant::now();
    h.feed_processes(&[proc_snap(42, &[0])]);
    h.tick(&b, t0);
    let before = h.bus().head();
    h.feed_processes(&[proc_snap(42, &[0])]);
    h.tick(&b, t0 + RESEND_EVERY);
    let after = h.bus().read_from(before + 1, usize::MAX).unwrap();
    let names: Vec<&str> = after
        .iter()
        .filter_map(|r| match &r.payload {
            Payload::Sample(s) => Some(s.signal.as_str()),
            _ => None,
        })
        .collect();
    for s in [sig::PROC_NAME, sig::PROC_CMDLINE, sig::PROC_DEVICE] {
        assert!(names.contains(&s), "{s} not resent: {names:?}");
    }
}

#[test]
fn held_index_that_resolves_at_the_next_tick_is_published() {
    // Index 1 is unknown at the first tick and appears at the second.
    let mut b = Devices::with(&[(0, A)]);
    let mut h = host(&b);
    h.feed_processes(&[proc_snap(42, &[1])]);
    h.tick(&b, Instant::now());
    b = Devices::with(&[(0, A), (1, B)]);
    h.tick(&b, Instant::now());
    let late = records(&h)
        .iter()
        .filter(|r| r.source == IDENTITY_SOURCE)
        .count();
    assert_eq!(late, 1);
    assert_eq!(h.identity().dropped(), 0);
}

#[test]
fn view_changed_is_published_as_an_event() {
    let b = Devices::default();
    let h = host(&b);
    h.publish_view_changed("die3d");
    let recs = records(&h);
    assert!(matches!(
        &recs[0].payload,
        Payload::Event(Event::ViewChanged { view }) if view == "die3d"
    ));
    assert_eq!(recs[0].source, VIEW_SOURCE);
}

#[test]
fn a_replay_host_is_marked_replay_and_plays_the_fixture() {
    let h = BusHost::replay(Path::new(FIXTURE), 1000.0, BusConfig::default()).unwrap();
    assert_eq!(h.bus().header().kind, HeaderKind::Replay);
    let end = Instant::now() + Duration::from_secs(10);
    while h.replay_result().is_none() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.replay_result(), Some(Ok(114)));
    let w = h.world();
    assert!(
        w.models()
            .iter()
            .any(|m| m.key == EntityKey::model("llama-8b")),
        "{:?}",
        w.models()
    );
    // A replay host has no live sources: a tick publishes nothing.
    let head = h.bus().head();
    let mut h = h;
    h.tick(&Devices::with(&[(0, A)]), Instant::now());
    assert_eq!(h.bus().head(), head);
}

#[test]
fn dropping_a_replay_host_stops_a_slow_replay_promptly() {
    // At speed 0.01 the 20 s fixture would take over half an hour.
    let h = BusHost::replay(Path::new(FIXTURE), 0.01, BusConfig::default()).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let t = Instant::now();
    drop(h);
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}

#[test]
fn replay_rejects_a_bad_speed_and_a_missing_log() {
    assert!(BusHost::replay(Path::new(FIXTURE), 0.0, BusConfig::default()).is_err());
    assert!(BusHost::replay(Path::new("/nonexistent/log"), 1.0, BusConfig::default()).is_err());
}

#[test]
fn a_second_host_on_one_log_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = || BusConfig {
        dir: Some(dir.path().join("bus")),
        ..BusConfig::default()
    };
    let b = Devices::default();
    let first = BusHost::with_gozer(cfg(), &b, GozerSource::fed()).unwrap();
    let err = match BusHost::with_gozer(cfg(), &b, GozerSource::fed()) {
        Ok(_) => panic!("two hosts on one directory"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    drop(first);
    // Other tests in this binary spawn processes. A child between fork and
    // exec briefly shares every open file, the lock file included, so the
    // lock can outlive `drop` by that instant. Allow it a moment.
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        match BusHost::with_gozer(cfg(), &b, GozerSource::fed()) {
            Ok(_) => break,
            Err(e) if Instant::now() < end => {
                assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("the lock was never released: {e}"),
        }
    }
}

/// The maintenance timer reaches the disk: on an idle bus a closed segment
/// older than the retention limit is deleted after the 60 s tick, and not
/// before it.
#[test]
fn the_maintenance_tick_ages_out_an_old_segment_on_an_idle_bus() {
    use tt_toplike_bus::ManualClock;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("bus");
    let clock = ManualClock::new(1_700_000_000_000);
    let b = Devices::default();
    let mut h = BusHost::with_gozer(
        BusConfig {
            dir: Some(log.clone()),
            segment_secs: 10,
            retain_secs: 60,
            clock: clock.clone(),
            ..BusConfig::default()
        },
        &b,
        GozerSource::fed(),
    )
    .unwrap();
    let t0 = Instant::now();
    h.tick(&b, t0);
    // Two records 10 s apart: the second rotates segment 1 closed.
    h.publish_view_changed("a");
    clock.advance_secs(10);
    h.publish_view_changed("b");
    assert!(h.bus().flush(Duration::from_secs(5)));
    let seg1 = log.join("bus-1.jsonl.gz");
    assert!(seg1.exists());
    // Two idle minutes by the bus clock.
    clock.advance_secs(120);
    h.tick(&b, t0 + Duration::from_secs(59));
    assert!(h.bus().flush(Duration::from_secs(5)));
    assert!(seg1.exists(), "no maintenance before 60 s");
    h.tick(&b, t0 + MAINTAIN_EVERY);
    assert!(h.bus().flush(Duration::from_secs(5)));
    assert!(!seg1.exists(), "the 60 s tick deleted the old segment");
}

// ---- Fix round 1 -------------------------------------------------------------

/// A simulated backend's readings are tagged `Estimated`, never `Measured`.
#[test]
fn a_synthetic_backend_is_recorded_as_estimated() {
    let b = mock(2);
    let mut h = host(&b);
    h.set_backend_provenance(tt_toplike_bus::Provenance::Estimated);
    h.tick(&b, Instant::now());
    let mut n = 0;
    for r in records(&h).iter().filter(|r| r.source == "backend") {
        let prov = match &r.payload {
            Payload::Sweep(s) => s.prov,
            Payload::Sample(s) => s.prov,
            _ => continue,
        };
        assert_eq!(prov, tt_toplike_bus::Provenance::Estimated, "{r:?}");
        n += 1;
    }
    assert!(n > 2, "only {n} backend records");
}

/// Hardening B3: a log directory keeps one backend and mode, but that never
/// stops a replay. A replay reads the source files and does not open the
/// source directory as a bus, so a live log replays into a directory of its
/// own; a live bus is then refused on that replay directory.
#[test]
fn a_live_log_replays_into_its_own_directory_and_live_is_refused_there() {
    let tmp = tempfile::tempdir().unwrap();
    let live_dir = tmp.path().join("live");
    let replay_dir = tmp.path().join("replay");
    let tagged = |dir: &Path, mode: &str| {
        let mut cfg = BusConfig {
            dir: Some(dir.to_path_buf()),
            ..BusConfig::default()
        };
        cfg.header_tags.insert("backend".into(), "sysfs".into());
        cfg.header_tags.insert("mode".into(), mode.into());
        cfg
    };
    {
        let bus = tt_toplike_bus::Bus::open(tagged(&live_dir, "live")).unwrap();
        bus.publish("t", Payload::Event(Event::ViewChanged { view: "x".into() }));
        assert!(bus.flush(Duration::from_secs(5)));
    }
    let play = |into: Option<&Path>| {
        let h = BusHost::replay(
            &live_dir,
            1000.0,
            BusConfig {
                dir: into.map(Path::to_path_buf),
                ..BusConfig::default()
            },
        )
        .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while h.replay_result().is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(h.replay_result(), Some(Ok(1)));
        assert!(h.bus().flush(Duration::from_secs(5)));
    };
    play(None);
    play(Some(&replay_dir));
    // A second replay of the same log adds to the replay directory.
    play(Some(&replay_dir));
    let err = tt_toplike_bus::Bus::open(tagged(&replay_dir, "live")).unwrap_err();
    assert!(
        err.to_string()
            .contains("holds a local-hardware/replay log"),
        "{err}"
    );
}

/// A replay host says so in its header, and carries over the backend the
/// log was recorded from (`unknown` for a log that does not name one).
#[test]
fn a_replay_host_header_says_replay_and_names_the_recorded_backend() {
    let h = BusHost::replay(Path::new(FIXTURE), 1000.0, BusConfig::default()).unwrap();
    let t = h.bus().header().tool_versions;
    assert_eq!(t.get("mode").map(String::as_str), Some("replay"));
    assert_eq!(t.get("backend").map(String::as_str), Some("unknown"));
}
