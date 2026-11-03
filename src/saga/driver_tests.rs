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

/// A bus reopened on a directory that holds earlier sessions continues
/// their sequence numbers and reads their records back from the segment
/// files. A local saga starts at this bus instance's first record, so the
/// old sessions are not fed (final review finding I3).
#[test]
fn a_reused_bus_dir_feeds_only_this_sessions_records() {
    let tmp = tempfile::tempdir().unwrap();
    let records = busy_records();
    let (old, new) = records.split_at(records.len() / 2);
    let open = |clock: &Arc<ManualClock>| {
        Bus::open(BusConfig {
            dir: Some(tmp.path().to_path_buf()),
            clock: clock.clone(),
            ..BusConfig::default()
        })
        .unwrap()
    };
    // An earlier session, flushed to disk and closed.
    let earlier = open(&ManualClock::new(0));
    publish_all(&earlier, old);
    assert!(earlier.flush(WAIT));
    let old_head = earlier.head();
    assert_eq!(old_head, old.len() as u64);
    drop(earlier);

    let clock = ManualClock::new(0);
    let bus = open(&clock);
    assert_eq!(bus.first_seq(), old_head + 1);
    // The old records are still readable through the new bus, so a driver
    // that started at 1 would feed them.
    assert_eq!(bus.read_from(1, 1).unwrap()[0].seq, 1);
    publish_all(&bus, new);
    let head = bus.head();
    let d = SagaDriver::spawn(bus.clone(), cfg()).unwrap();
    assert!(
        wait_published(&d, &clock, |p| p.through == head),
        "the driver never caught up: {:?}",
        d.published()
    );
    let p = d.published();
    assert_eq!(p.fed, new.len() as u64, "only this session's records");
    assert_eq!(p.feeds, 1, "no restart");
    assert_eq!(p.digest, reference_digest_from(&bus, old_head + 1, cfg()));
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

/// A TUI that attaches to a host whose model is already serving gets that
/// model's world: the snapshot's ready models are fed as `ModelReady`
/// records after its readings. The test compares with a saga fed by hand
/// the records a TUI present from the start would have seen.
#[cfg(unix)]
#[test]
fn a_remote_driver_attached_after_a_model_is_ready_gets_its_world() {
    use tt_toplike_bus::{Event, SocketClient, SocketServer, Sweep};
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let bus = Bus::open_with_world(BusConfig::default()).unwrap();
    let chip = EntityKey::chip("0000:01:00.0");
    let model = EntityKey::model("meta-llama/Llama-3.1-8B-Instruct");
    let down = EntityKey::model("Qwen3-8B");
    // Two models announced before the TUI attaches: one ready, one gone.
    for m in [&model, &down] {
        bus.publish(
            "inference",
            Payload::Event(Event::ModelReady {
                model: m.clone(),
                chips: vec![chip.clone()],
            }),
        );
    }
    bus.publish(
        "inference",
        Payload::Event(Event::ModelGone {
            model: down.clone(),
        }),
    );
    let server = SocketServer::spawn(bus.clone(), &sock).unwrap();
    let d = SagaDriver::spawn_remote(SocketClient::connect(&sock).unwrap(), &sock, cfg()).unwrap();
    assert!(d.wait_for(WAIT, |p| p.feeds == 1));
    // Ticks after the attach open the seed gate, and the world is born.
    let sweep = |t: f64| {
        Payload::Sweep(Sweep {
            entity: chip.clone(),
            signals: vec!["tt_asic_temperature_celsius".into()],
            values: vec![Value::F64(t)],
            prov: Provenance::Measured,
        })
    };
    for i in 0..20 {
        bus.publish("backend", sweep(50.0 + f64::from(i) * 0.37));
    }
    let head = bus.head();
    assert!(d.wait_for(WAIT, |p| p.through == head));
    let p = d.published();
    assert!(p.state.has_world(), "a world was born");

    // The same story by hand: the ready model, then the records after the
    // snapshot. The snapshot world holds no readings here.
    let mut want = Saga::new(cfg());
    want.feed(&Record {
        seq: 0,
        t_mono_ns: 0,
        t_wall_ms: 0,
        source: SNAPSHOT_SOURCE.into(),
        payload: Payload::Event(Event::ModelReady {
            model: model.clone(),
            chips: vec![chip.clone()],
        }),
    });
    for r in bus.read_from(4, usize::MAX).unwrap() {
        want.feed(&r);
    }
    assert_eq!(p.digest, want.digest(), "the ready model's world was born");
    // Without the ready model the story differs, so the check above can
    // tell the two apart.
    let mut without = Saga::new(cfg());
    for r in bus.read_from(4, usize::MAX).unwrap() {
        without.feed(&r);
    }
    assert_ne!(p.digest, without.digest());
    d.stop();
    drop(server);
}

/// A driver that stops for any reason says so: after `stop`, and after a
/// panic in its thread (caught there), the published snapshot is marked
/// stopped and keeps the last state.
#[test]
fn the_driver_reports_when_it_stops() {
    let clock = ManualClock::new(0);
    let bus = manual_bus(&clock);
    let sample = |i: u64| {
        Payload::Sample(Sample {
            entity: EntityKey::chip("0000:01:00.0"),
            signal: "toplike.test".into(),
            value: Value::U64(i),
            prov: Provenance::Measured,
        })
    };
    bus.publish("backend", sample(1));
    let d = SagaDriver::spawn(bus.clone(), cfg()).unwrap();
    assert!(d.wait_for(WAIT, |p| p.fed == 1));
    assert!(!d.stopped());
    // A panic in the thread: the driver ends, and says so.
    d.panic_on_next_read();
    bus.publish("backend", sample(2));
    assert!(
        d.wait_for(WAIT, |p| p.stopped),
        "a driver that panicked does not say it stopped"
    );
    assert!(d.stopped());
    assert_eq!(d.published().fed, 1, "the last state stays");
    d.stop();

    // `stop`: the last snapshot is marked stopped too.
    let d = SagaDriver::spawn(bus, cfg()).unwrap();
    let shared = d.thread_handle().upgrade().unwrap();
    d.stop();
    assert!(shared.lock().stopped);
}

/// The driver thread starts no programs, so it runs at the lowered
/// priority of `crate::workload::background`.
#[cfg(target_os = "linux")]
#[test]
fn the_driver_thread_runs_at_lowered_priority() {
    use crate::workload::background::{thread_nice, NICE_STEP};
    let clock = ManualClock::new(0);
    let bus = manual_bus(&clock);
    let d = SagaDriver::spawn(bus.clone(), cfg()).unwrap();
    bus.publish(
        "backend",
        Payload::Sample(Sample {
            entity: EntityKey::chip("0000:01:00.0"),
            signal: "toplike.test".into(),
            value: Value::U64(1),
            prov: Provenance::Measured,
        }),
    );
    // Its first publish comes after its start, where it reads its nice value.
    assert!(d.wait_for(WAIT, |p| p.fed == 1));
    let want = (thread_nice() + NICE_STEP).min(19);
    assert_eq!(d.thread_nice(), Some(want));
    d.stop();
}

/// A season that differs from the default: 1 minute at a 300 ms interval,
/// 200 ticks. The busy fixture has quiet stretches longer than that, so
/// worlds end by season under it and not under the default 12,000.
fn short_season_tags() -> std::collections::BTreeMap<String, String> {
    let s = crate::config::SagaSettings {
        season_minutes: 1,
        ..Default::default()
    };
    let mut tags = std::collections::BTreeMap::new();
    tags.insert("backend".to_string(), "mock".to_string());
    tags.insert("mode".to_string(), "mock".to_string());
    super::super::config::write_season_tag(&mut tags, &s, 300);
    assert_eq!(tags[crate::config::SAGA_SEASON_TAG], "200");
    tags
}

/// The caller's config: the default 12,000-tick season, as a TUI whose
/// own `[saga]` section is empty passes.
fn default_cfg() -> SagaConfig {
    SagaConfig {
        season_ticks: super::super::config::default_season_ticks(),
        ..cfg()
    }
}

/// Waits for a replay host to finish playing. The wait is bounded by
/// [`WAIT`]; nothing is asserted about how long it took.
fn wait_replayed(host: &crate::bus::host::BusHost) -> u64 {
    let step = Duration::from_millis(20);
    for _ in 0..(WAIT.as_millis() / step.as_millis()) {
        if let Some(r) = host.replay_result() {
            return r.expect("the replay plays");
        }
        std::thread::sleep(step);
    }
    panic!("the replay never finished");
}

/// The season is in the log: a live saga, a replay of its recording and
/// `--saga-digest` of either recording all use the season the live host
/// wrote into its header, whatever the reader's own config says (final
/// review finding I4).
#[test]
fn live_replay_and_digest_agree_on_a_recorded_season() {
    use crate::bus::host::BusHost;
    let records = busy_records();
    let short = SagaConfig {
        season_ticks: 200,
        ..cfg()
    };
    // The short season must change the story, or this test proves nothing.
    let feed = |c: SagaConfig| {
        let mut saga = Saga::new(c);
        for r in &records {
            saga.feed(r);
        }
        saga.digest()
    };
    assert_ne!(feed(short), feed(default_cfg()));

    // Live: a host that wrote the short season, and a driver handed the
    // default config.
    let live_dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(0);
    let bus = Bus::open(BusConfig {
        dir: Some(live_dir.path().to_path_buf()),
        clock: clock.clone(),
        header_tags: short_season_tags(),
        ..BusConfig::default()
    })
    .unwrap();
    publish_all(&bus, &records);
    let head = bus.head();
    let d = SagaDriver::spawn(bus.clone(), default_cfg()).unwrap();
    assert!(wait_published(&d, &clock, |p| p.through == head));
    let mock_short = SagaConfig {
        label: Label::Mock,
        ..short
    };
    assert_eq!(d.published().digest, reference_digest(&bus, mock_short));
    d.stop();
    assert!(bus.flush(WAIT));
    drop(bus);

    // The digest of the live recording.
    let want = super::super::digest::digest_log(live_dir.path()).unwrap();
    let replay_short = SagaConfig {
        label: Label::Replay,
        ..short
    };
    assert_eq!(want.digest, feed(replay_short));

    // A replay of it, recorded again, with a driver handed the default.
    let replay_dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(0);
    let host = BusHost::replay(
        live_dir.path(),
        1.0e9,
        BusConfig {
            dir: Some(replay_dir.path().to_path_buf()),
            clock: clock.clone(),
            ..BusConfig::default()
        },
    )
    .unwrap();
    assert_eq!(wait_replayed(&host), records.len() as u64);
    let head = host.bus().head();
    let d = SagaDriver::spawn(host.bus().clone(), default_cfg()).unwrap();
    assert!(wait_published(&d, &clock, |p| p.through == head));
    assert_eq!(d.published().digest, want.digest, "replay and digest");
    d.stop();
    assert!(host.bus().flush(WAIT));
    drop(host);
    let again = super::super::digest::digest_log(replay_dir.path()).unwrap();
    assert_eq!(
        again.digest, want.digest,
        "the replay's recording keeps the season"
    );
}

/// A log recorded before the tag existed tells the default season to every
/// reader: `--saga-digest`, and a replay of it whatever the replaying TUI's
/// own config says.
#[test]
fn a_log_without_the_tag_uses_the_default_season() {
    use crate::bus::host::BusHost;
    let records = busy_records();
    let old_dir = tempfile::tempdir().unwrap();
    let bus = Bus::open(BusConfig {
        dir: Some(old_dir.path().to_path_buf()),
        ..BusConfig::default()
    })
    .unwrap();
    publish_all(&bus, &records);
    drop(bus);
    let want = super::super::digest::digest_log(old_dir.path()).unwrap();
    let mut saga = Saga::new(SagaConfig {
        label: Label::Replay,
        ..default_cfg()
    });
    for r in &records {
        saga.feed(r);
    }
    assert_eq!(want.digest, saga.digest());

    // The replaying TUI's own config asks for 200 ticks and its bus config
    // carries its own tag; the recorded log has none, so the replay uses
    // the default.
    let clock = ManualClock::new(0);
    let host = BusHost::replay(
        old_dir.path(),
        1.0e9,
        BusConfig {
            clock: clock.clone(),
            header_tags: short_season_tags(),
            ..BusConfig::default()
        },
    )
    .unwrap();
    wait_replayed(&host);
    let head = host.bus().head();
    let d = SagaDriver::spawn(host.bus().clone(), cfg()).unwrap();
    assert!(wait_published(&d, &clock, |p| p.through == head));
    assert_eq!(d.published().digest, want.digest);
    d.stop();
}

/// An attached driver takes the season from the host's snapshot tags.
#[cfg(unix)]
#[test]
fn a_remote_driver_takes_the_season_from_the_snapshot() {
    use tt_toplike_bus::{SocketClient, SocketServer};
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let bus = Bus::open_with_world(BusConfig {
        header_tags: short_season_tags(),
        ..BusConfig::default()
    })
    .unwrap();
    let server = SocketServer::spawn(bus.clone(), &sock).unwrap();
    let client = SocketClient::connect(&sock).unwrap();
    let d = SagaDriver::spawn_remote(client, &sock, default_cfg()).unwrap();
    assert!(d.wait_for(WAIT, |p| p.feeds == 1));
    let records = busy_records();
    publish_all(&bus, &records);
    let head = bus.head();
    assert!(d.wait_for(WAIT, |p| p.through == head));
    let want = reference_digest(
        &bus,
        SagaConfig {
            season_ticks: 200,
            label: Label::Mock,
            ..cfg()
        },
    );
    assert_eq!(d.published().digest, want);
    d.stop();
    drop(server);
}
