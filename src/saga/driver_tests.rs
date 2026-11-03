//! Tests for the saga driver. Every wait is on the driver's condition
//! variable with a generous timeout; nothing here asserts on wall-clock
//! time. A local driver's publish cadence follows its bus's clock, which is
//! a [`ManualClock`] here, so a test decides when a publish is due.

use super::*;
use std::sync::Arc;
use std::time::Duration;
use tt_toplike_bus::segment::read_segment;
use tt_toplike_bus::{BusConfig, EntityKey, ManualClock, Provenance, Value};
use tt_toplike_saga::chronicle::NameMode;

/// Longest wait for any one condition. A healthy run takes milliseconds;
/// this only bounds a broken one.
const WAIT: Duration = Duration::from_secs(60);

fn cfg() -> SagaConfig {
    SagaConfig {
        season_ticks: 200,
        names: NameMode::Real,
        label: Label::Live,
    }
}

/// A memory-only bus on a manual clock.
fn manual_bus(clock: &Arc<ManualClock>) -> Bus {
    Bus::open(BusConfig {
        clock: clock.clone(),
        ..BusConfig::default()
    })
    .unwrap()
}

/// The saga crate's busy fixture: a model, requests, a reset, a training
/// run and several thousand records over many ticks.
fn busy_records() -> Vec<Record> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/tt-toplike-saga/tests/fixtures/busy.jsonl");
    let read = read_segment(&path).unwrap();
    assert_eq!(read.bad_lines, 0);
    assert!(read.records.len() > 3 * BATCH, "spans several batches");
    read.records
}

/// Publishes each record's source and payload; the bus numbers them.
fn publish_all(bus: &Bus, records: &[Record]) {
    for r in records {
        bus.publish(&r.source, r.payload.clone());
    }
}

/// The digest of a fresh saga fed every record `bus` holds, in order.
fn reference_digest(bus: &Bus, cfg: SagaConfig) -> u64 {
    reference_digest_from(bus, 1, cfg)
}

/// The digest of a fresh saga fed every record of `bus` from `start` to
/// the head, in order.
fn reference_digest_from(bus: &Bus, start: u64, cfg: SagaConfig) -> u64 {
    let records = bus.read_from(start, usize::MAX).unwrap();
    assert_eq!(records.len() as u64, bus.head() + 1 - start);
    let mut saga = Saga::new(cfg);
    for r in &records {
        saga.feed(r);
    }
    saga.digest()
}

/// Waits until `pred` holds for `d`'s published snapshot, moving `clock` on
/// a second before each wait so the next publish is due. False after
/// [`WAIT`] of tries.
fn wait_published(d: &SagaDriver, clock: &ManualClock, pred: impl Fn(&Published) -> bool) -> bool {
    let step = Duration::from_millis(100);
    for _ in 0..(WAIT.as_millis() / step.as_millis()) {
        clock.advance(Duration::from_secs(1));
        if d.wait_for(step, &pred) {
            return true;
        }
    }
    false
}

#[test]
fn the_driver_feeds_every_record_in_order() {
    let clock = ManualClock::new(0);
    let bus = manual_bus(&clock);
    let records = busy_records();
    let d = SagaDriver::spawn(bus.clone(), cfg()).unwrap();
    // Half before the driver has looked, half while it runs, so the reads
    // start at several different boundaries.
    let (a, b) = records.split_at(records.len() / 2 + 7);
    publish_all(&bus, a);
    publish_all(&bus, b);
    let head = bus.head();
    assert_eq!(head, records.len() as u64);
    assert!(
        wait_published(&d, &clock, |p| p.through == head),
        "the driver never caught up: {:?}",
        d.published()
    );
    let p = d.published();
    assert_eq!(p.fed, head, "every record is fed once");
    assert_eq!(p.feeds, 1, "no restart");
    assert_eq!(
        p.digest,
        reference_digest(&bus, cfg()),
        "the driver's saga matches a saga fed the same records in order"
    );
    assert_eq!(p.state.tick, d.state().tick);
    assert!(p.state.has_world());
    d.stop();
}

#[test]
fn the_driver_stops_on_stop() {
    let clock = ManualClock::new(0);
    let bus = manual_bus(&clock);
    let d = SagaDriver::spawn(bus.clone(), cfg()).unwrap();
    let alive = d.thread_handle();
    assert!(alive.upgrade().is_some());
    d.stop();
    // `stop` returned after the thread ended: only the thread and the
    // handle held the shared state, and both are gone.
    assert!(alive.upgrade().is_none(), "the thread still runs");

    // Dropping the handle stops it the same way.
    let d = SagaDriver::spawn(bus, cfg()).unwrap();
    let alive = d.thread_handle();
    drop(d);
    assert!(
        alive.upgrade().is_none(),
        "the thread still runs after drop"
    );
}

/// A driver that cannot keep up never slows the publisher down. The test
/// holds the driver's publish lock while 100,000 records go onto the bus,
/// so the driver stops at its first publish that falls due. Every publish
/// returns, and the bus keeps its newest 65,536 records. The driver then
/// finds its next record gone and starts a new saga. How often it restarts
/// depends on how far it got before it stopped (on a loaded box it may
/// chase the moving window a few times), so the test checks only that it
/// restarted and caught up; the next test pins where a restart begins.
#[test]
fn a_slow_reader_never_blocks_publishing() {
    const N: u64 = 100_000;
    let clock = ManualClock::new(0);
    let bus = manual_bus(&clock);
    let chip = EntityKey::chip("0000:01:00.0");
    let sample = |i: u64| {
        Payload::Sample(Sample {
            entity: chip.clone(),
            signal: "toplike.test".into(),
            value: Value::U64(i),
            prov: Provenance::Estimated,
        })
    };
    bus.publish("backend", sample(0));
    let d = SagaDriver::spawn(bus.clone(), cfg()).unwrap();
    // The first publish is due at once; wait for it, then make the next one
    // due and hold the lock it needs.
    assert!(d.wait_for(WAIT, |p| p.fed == 1));
    clock.advance(Duration::from_secs(1));
    let mut returned = 1u64;
    {
        let _held = d.hold_publishing();
        for i in 1..N {
            bus.publish("backend", sample(i));
            returned += 1;
        }
        assert_eq!(returned, N, "every publish returned");
        assert_eq!(bus.head(), N);
    }
    assert!(
        wait_published(&d, &clock, |p| p.through == N),
        "the driver never caught up: {:?}",
        d.published()
    );
    let p = d.published();
    let window = BusConfig::default().window_records as u64;
    assert!(p.feeds >= 2, "records 2 to 34,464 are gone: a restart");
    assert!(p.fed <= window, "a restarted saga starts inside the window");
    d.stop();
}

/// A driver whose next record is gone starts a new saga at the oldest
/// record the bus still keeps, and feeds every record from there.
#[test]
fn a_restart_begins_at_the_oldest_record_kept() {
    let clock = ManualClock::new(0);
    let bus = Bus::open(BusConfig {
        clock: clock.clone(),
        window_records: 1000,
        ..BusConfig::default()
    })
    .unwrap();
    let records = busy_records();
    publish_all(&bus, &records);
    let head = bus.head();
    let oldest = head - 1000 + 1;
    // The driver starts at record 1, which is gone.
    let d = SagaDriver::spawn(bus.clone(), cfg()).unwrap();
    assert!(
        wait_published(&d, &clock, |p| p.through == head),
        "the driver never caught up: {:?}",
        d.published()
    );
    let p = d.published();
    assert_eq!(p.feeds, 2, "one restart");
    assert_eq!(p.fed, 1000, "fed from the oldest kept record, {oldest}");
    assert_eq!(p.digest, reference_digest_from(&bus, oldest, cfg()));
    d.stop();
}

#[test]
fn the_label_follows_the_header() {
    let tagged = |kind: HeaderKind, tags: &[(&str, &str)]| {
        let mut c = BusConfig {
            header_kind: kind,
            ..BusConfig::default()
        };
        for (k, v) in tags {
            c.header_tags.insert((*k).into(), (*v).into());
        }
        Bus::open(c).unwrap()
    };
    let cases = [
        (tagged(HeaderKind::Live, &[]), Label::Live),
        (
            tagged(HeaderKind::Live, &[("backend", "hybrid"), ("mode", "live")]),
            Label::Live,
        ),
        (
            tagged(HeaderKind::Live, &[("backend", "mock"), ("mode", "mock")]),
            Label::Mock,
        ),
        (
            tagged(HeaderKind::Live, &[("backend", "mock")]),
            Label::Mock,
        ),
        (
            tagged(
                HeaderKind::Replay,
                &[("backend", "mock"), ("mode", "replay")],
            ),
            Label::Replay,
        ),
    ];
    for (bus, want) in cases {
        // The caller's label is replaced by the header's.
        let d = SagaDriver::spawn(
            bus.clone(),
            SagaConfig {
                label: Label::Mock,
                ..cfg()
            },
        )
        .unwrap();
        assert_eq!(d.state().label, want, "{:?}", bus.header());
        bus.publish(
            "backend",
            Payload::Sample(Sample {
                entity: EntityKey::chip("0000:01:00.0"),
                signal: "toplike.test".into(),
                value: Value::U64(1),
                prov: Provenance::Measured,
            }),
        );
        assert!(d.wait_for(WAIT, |p| p.fed == 1));
        assert_eq!(d.state().label, want, "{:?}", bus.header());
        d.stop();
    }
}

#[test]
fn label_for_prefers_replay_then_mock() {
    use HeaderKind::{Live, Replay};
    assert_eq!(label_for(Live, "", ""), Label::Live);
    assert_eq!(label_for(Live, "sysfs", "live"), Label::Live);
    assert_eq!(label_for(Live, "mock", "live"), Label::Mock);
    assert_eq!(label_for(Live, "hybrid", "mock"), Label::Mock);
    assert_eq!(label_for(Live, "mock", "replay"), Label::Replay);
    assert_eq!(label_for(Replay, "mock", "mock"), Label::Replay);
    assert_eq!(label_for(Replay, "", ""), Label::Replay);
}

#[test]
fn a_snapshot_becomes_one_sample_per_reading_in_order() {
    let chip = EntityKey::chip("0000:01:00.0");
    let mut world = WorldSummary::default();
    for (signal, value) in [
        ("toplike.enabled_tensix_col", Value::U64(0x3fff)),
        ("toplike.heartbeat", Value::U64(9)),
    ] {
        world.readings.push(tt_toplike_bus::ReadingSummary {
            entity: chip.clone(),
            signal: signal.into(),
            value,
            prov: Provenance::Estimated,
            seq: 40,
            t_mono_ns: 5,
            age_ms: 1,
        });
    }
    let recs = snapshot_records(&world);
    assert_eq!(recs.len(), 2);
    for (r, w) in recs.iter().zip(&world.readings) {
        assert_eq!(r.source, SNAPSHOT_SOURCE);
        assert_eq!(
            r.payload,
            Payload::Sample(Sample {
                entity: w.entity.clone(),
                signal: w.signal.clone(),
                value: w.value.clone(),
                prov: w.prov,
            })
        );
    }
}

/// A remote driver ends with the digest of a saga fed the same records,
/// and a server that goes away and comes back with a new bus starts a new
/// saga.
#[cfg(unix)]
#[test]
fn a_remote_driver_tells_the_same_saga_and_restarts_with_a_new_server() {
    use tt_toplike_bus::{SocketClient, SocketServer};
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let bus = Bus::open_with_world(BusConfig::default()).unwrap();
    let server = SocketServer::spawn(bus.clone(), &sock).unwrap();
    let client = SocketClient::connect(&sock).unwrap();
    let d = SagaDriver::spawn_remote(client, &sock, cfg()).unwrap();
    // Wait for the first snapshot, so every record below comes through the
    // subscription.
    assert!(d.wait_for(WAIT, |p| p.feeds == 1));
    assert_eq!(d.published().fed, 0, "an empty world seeds nothing");
    let records = busy_records();
    publish_all(&bus, &records);
    let head = bus.head();
    assert!(
        d.wait_for(WAIT, |p| p.through == head),
        "the remote driver never caught up: {:?}",
        d.published()
    );
    let p = d.published();
    assert_eq!(p.fed, head);
    assert_eq!(p.digest, reference_digest(&bus, cfg()));
    assert_eq!(p.state.label, Label::Live);

    // The server goes away; a new one with a mock bus takes its path.
    drop(server);
    drop(bus);
    let mut c = BusConfig::default();
    c.header_tags.insert("backend".into(), "mock".into());
    c.header_tags.insert("mode".into(), "mock".into());
    let bus = Bus::open_with_world(c).unwrap();
    let server = SocketServer::spawn(bus.clone(), &sock).unwrap();
    assert!(
        d.wait_for(WAIT, |p| p.feeds == 2),
        "the remote driver did not start a new feed: {:?}",
        d.published()
    );
    let p = d.published();
    assert_eq!(p.fed, 0, "a new saga, fed nothing yet");
    assert_eq!(
        p.state.label,
        Label::Mock,
        "the label follows the new header"
    );
    publish_all(&bus, &records[..BATCH]);
    let head = bus.head();
    assert!(d.wait_for(WAIT, |p| p.feeds == 2 && p.through == head));
    let want = reference_digest(
        &bus,
        SagaConfig {
            label: Label::Mock,
            ..cfg()
        },
    );
    assert_eq!(d.published().digest, want);
    d.stop();
    drop(server);
}

/// A remote driver that attaches mid-run starts from the snapshot's
/// readings, then the records after it.
#[cfg(unix)]
#[test]
fn a_remote_driver_starts_from_the_snapshot() {
    use tt_toplike_bus::{SocketClient, SocketServer};
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let bus = Bus::open_with_world(BusConfig::default()).unwrap();
    let records = busy_records();
    publish_all(&bus, &records[..100]);
    let server = SocketServer::spawn(bus.clone(), &sock).unwrap();
    let mut probe = SocketClient::connect(&sock).unwrap();
    let snap = probe.snapshot().unwrap();
    assert!(!snap.world.readings.is_empty());
    let d = SagaDriver::spawn_remote(SocketClient::connect(&sock).unwrap(), &sock, cfg()).unwrap();
    assert!(d.wait_for(WAIT, |p| p.feeds == 1));
    assert_eq!(d.published().fed, snap.world.readings.len() as u64);
    assert_eq!(d.published().through, 100);
    d.stop();
    drop(server);
}

#[test]
fn only_a_driver_thread_is_a_saga_thread() {
    assert!(!is_saga_thread(), "the test thread is not a driver");
    for name in [LOCAL_THREAD, REMOTE_THREAD] {
        let on = std::thread::Builder::new()
            .name(name.into())
            .spawn(is_saga_thread)
            .unwrap()
            .join()
            .unwrap();
        assert!(on, "{name}");
    }
    let off = std::thread::Builder::new()
        .name("tt-toplike-sagas".into())
        .spawn(is_saga_thread)
        .unwrap()
        .join()
        .unwrap();
    assert!(!off);
}
