//! Tests for observations and the sweep clock.

use super::*;
use tt_toplike_bus::{Gap, Provenance, Sample, Sweep};

fn chip(n: u32) -> EntityKey {
    EntityKey::chip(&format!("0000:0{n}:00.0"))
}

fn record(seq: u64, t: u64, source: &str, payload: Payload) -> Record {
    Record {
        seq,
        t_mono_ns: t,
        t_wall_ms: t / 1_000,
        source: source.into(),
        payload,
    }
}

fn sweep(entity: EntityKey, pairs: Vec<(&str, Value)>) -> Payload {
    Payload::Sweep(Sweep {
        entity,
        signals: pairs.iter().map(|(n, _)| n.to_string()).collect(),
        values: pairs.into_iter().map(|(_, v)| v).collect(),
        prov: Provenance::Measured,
    })
}

fn sample(entity: EntityKey, name: &str, value: Value) -> Payload {
    Payload::Sample(Sample {
        entity,
        signal: name.into(),
        value,
        prov: Provenance::Measured,
    })
}

fn gap() -> Payload {
    Payload::Gap(Gap {
        reason: "filler".into(),
    })
}

/// Runs every payload through a fresh feed. `number` supplies the (seq, time)
/// of the i-th record, so a test can renumber and retime the same contents.
fn run(
    payloads: &[(&str, Payload)],
    number: impl Fn(u64) -> (u64, u64),
) -> (Vec<Observation>, u64) {
    let mut feed = Feed::new();
    let mut out = Vec::new();
    for (i, (source, p)) in payloads.iter().enumerate() {
        let (seq, t) = number(i as u64);
        feed.push(&record(seq, t, source, p.clone()), &mut out);
    }
    (out, feed.ordinal())
}

fn ticks(out: &[Observation]) -> usize {
    out.iter().filter(|o| **o == Observation::Tick).count()
}

#[test]
fn renumbered_and_retimed_records_give_the_same_observations() {
    let payloads = vec![
        (
            "tt-smi",
            sweep(
                chip(1),
                vec![
                    ("a", Value::F64(1.5)),
                    ("b", Value::U64(7)),
                    ("c", Value::I64(-3)),
                    ("d", Value::Text("hot".into())),
                    ("e", Value::F64(f64::NAN)),
                ],
            ),
        ),
        ("tt-smi", sample(chip(2), "s", Value::U64(9))),
        ("tt-smi", sample(chip(2), "t", Value::Text("x".into()))),
        (
            "tt-smi",
            Payload::Event(Event::ModelGone {
                model: EntityKey::model("m"),
            }),
        ),
        ("tt-smi", gap()),
        ("tt-smi", sweep(chip(1), vec![("a", Value::F64(2.5))])),
    ];
    let (plain, n1) = run(&payloads, |i| (i, i * 1_000));
    let (shifted, n2) = run(&payloads, |i| (900 + i * 7, 5_000_000 - i));
    let (zeroed, n3) = run(&payloads, |_| (0, 0));
    assert_eq!(plain, shifted);
    assert_eq!(plain, zeroed);
    assert_eq!((n1, n2, n3), (6, 6, 6));
    // Flattening: three numeric pairs and one sample, text and NaN skipped,
    // the event, and a tick after each pacemaker sweep.
    let want = vec![
        Observation::Sample {
            entity: chip(1),
            signal: "a".into(),
            value: 1.5,
        },
        Observation::Sample {
            entity: chip(1),
            signal: "b".into(),
            value: 7.0,
        },
        Observation::Sample {
            entity: chip(1),
            signal: "c".into(),
            value: -3.0,
        },
        Observation::Tick,
        Observation::Sample {
            entity: chip(2),
            signal: "s".into(),
            value: 9.0,
        },
        Observation::Event(Event::ModelGone {
            model: EntityKey::model("m"),
        }),
        Observation::Sample {
            entity: chip(1),
            signal: "a".into(),
            value: 2.5,
        },
        Observation::Tick,
    ];
    assert_eq!(plain, want);
}

#[test]
fn a_saga_record_is_skipped() {
    let payloads = vec![
        ("tt-smi", sweep(chip(1), vec![("a", Value::U64(1))])),
        (
            "saga",
            Payload::Event(Event::Spark {
                chip: None,
                kind: "boom".into(),
            }),
        ),
        // A saga sweep must neither tick nor become the pacemaker.
        ("saga", sweep(chip(3), vec![("z", Value::U64(1))])),
        ("tt-smi", sweep(chip(1), vec![("a", Value::U64(2))])),
    ];
    let (out, ordinal) = run(&payloads, |i| (i, i));
    // The ordinal counts the two real records and not the two saga ones.
    assert_eq!(ordinal, 2);
    assert_eq!(ticks(&out), 2);
    assert!(out
        .iter()
        .all(|o| !matches!(o, Observation::Event(_)) && !is_signal(o, "z")));
    assert_eq!(out.len(), 4);

    // A saga sweep arriving first is not the pacemaker either.
    let payloads = vec![
        ("saga", sweep(chip(3), vec![("z", Value::U64(1))])),
        ("tt-smi", sweep(chip(1), vec![("a", Value::U64(1))])),
        ("tt-smi", sweep(chip(3), vec![("z", Value::U64(1))])),
    ];
    let (out, ordinal) = run(&payloads, |i| (i, i));
    assert_eq!(ordinal, 2);
    assert_eq!(ticks(&out), 1);
}

fn is_signal(o: &Observation, name: &str) -> bool {
    matches!(o, Observation::Sample { signal, .. } if signal == name)
}

#[test]
fn the_first_sweep_entity_is_the_pacemaker() {
    let payloads = vec![
        ("tt-smi", sweep(chip(2), vec![("a", Value::U64(1))])),
        ("tt-smi", sweep(chip(1), vec![("a", Value::U64(1))])),
        ("tt-smi", sweep(chip(1), vec![("a", Value::U64(2))])),
        ("tt-smi", sweep(chip(2), vec![("a", Value::U64(3))])),
        // A sweep whose values are all skipped still ticks for the pacemaker.
        (
            "tt-smi",
            sweep(chip(2), vec![("a", Value::Text("x".into()))]),
        ),
    ];
    let (out, _) = run(&payloads, |i| (i, i));
    // Ticks follow records 1, 4 and 5 only. Chip 1 is not the pacemaker.
    let shape: String = out
        .iter()
        .map(|o| if *o == Observation::Tick { 'T' } else { 's' })
        .collect();
    assert_eq!(shape, "sTsssTT");
    // A sample from the pacemaker's entity does not tick.
    let (out, _) = run(&[("tt-smi", sample(chip(1), "a", Value::U64(1)))], |i| {
        (i, i)
    });
    assert_eq!(ticks(&out), 0);
}

#[test]
fn the_fallback_tick_fires_when_the_pacemaker_goes_quiet() {
    let quiet_then_other = |filler: u64| {
        let mut payloads = vec![("tt-smi", sweep(chip(1), vec![("a", Value::U64(1))]))];
        for _ in 0..filler {
            payloads.push(("tt-smi", gap()));
        }
        payloads.push(("tt-smi", sweep(chip(2), vec![("a", Value::U64(1))])));
        run(&payloads, |i| (i, i)).0
    };
    // One pacemaker tick, plus the other chip's sweep only after 2000 quiet records.
    assert_eq!(ticks(&quiet_then_other(1999)), 1);
    assert_eq!(ticks(&quiet_then_other(2000)), 2);

    // While the pacemaker stays quiet, every sweep ticks; once it sweeps
    // again the others stop ticking.
    let mut payloads = vec![("tt-smi", sweep(chip(1), vec![("a", Value::U64(1))]))];
    for _ in 0..2000 {
        payloads.push(("tt-smi", gap()));
    }
    payloads.push(("tt-smi", sweep(chip(2), vec![("a", Value::U64(1))])));
    payloads.push(("tt-smi", sweep(chip(3), vec![("a", Value::U64(1))])));
    payloads.push(("tt-smi", sweep(chip(1), vec![("a", Value::U64(2))])));
    payloads.push(("tt-smi", sweep(chip(2), vec![("a", Value::U64(1))])));
    let (out, _) = run(&payloads, |i| (i, i));
    assert_eq!(ticks(&out), 4);
}

/// Feeds one record from `source` to a fresh feed and returns the feed and
/// the observations.
fn feed_one(source: &str, payload: Payload) -> (Feed, Vec<Observation>) {
    let mut feed = Feed::new();
    let mut out = Vec::new();
    feed.push(&record(1, 1, source, payload), &mut out);
    (feed, out)
}

/// Text of exactly `n` bytes.
fn text(n: usize) -> String {
    "x".repeat(n)
}

#[test]
fn an_item_of_256_bytes_is_accepted_and_257_is_dropped() {
    // A signal name.
    let (feed, out) = feed_one("backend", sample(chip(1), &text(256), Value::F64(1.0)));
    assert_eq!((out.len(), feed.ordinal(), feed.oversized()), (1, 1, 0));
    let (feed, out) = feed_one("backend", sample(chip(1), &text(257), Value::F64(1.0)));
    assert_eq!((out.len(), feed.ordinal(), feed.oversized()), (0, 0, 1));
    // An entity key, measured whole: "chip:" is 5 bytes.
    let key = |n: usize| EntityKey::chip(&text(n - 5));
    assert_eq!(key(256).as_str().len(), 256);
    let (feed, out) = feed_one("backend", sample(key(256), "temp", Value::F64(1.0)));
    assert_eq!((out.len(), feed.ordinal(), feed.oversized()), (1, 1, 0));
    let (feed, out) = feed_one("backend", sample(key(257), "temp", Value::F64(1.0)));
    assert_eq!((out.len(), feed.ordinal(), feed.oversized()), (0, 0, 1));
}

#[test]
fn a_record_with_any_oversized_string_is_dropped_whole() {
    let long = text(MAX_ITEM_BYTES + 1);
    let big = EntityKey::model(&long);
    let big_chip = EntityKey::chip(&long);
    let payloads = vec![
        sweep(big_chip.clone(), vec![("temp", Value::F64(1.0))]),
        // One long signal among short ones drops the whole sweep, tick and
        // all.
        sweep(
            chip(1),
            vec![("temp", Value::F64(1.0)), (long.as_str(), Value::F64(2.0))],
        ),
        Payload::Event(Event::ModelLoading {
            model: chip(1),
            chips: vec![chip(2), big_chip.clone()],
            progress: None,
        }),
        Payload::Event(Event::ModelReady {
            model: big.clone(),
            chips: vec![],
        }),
        Payload::Event(Event::ModelGone { model: big.clone() }),
        Payload::Event(Event::RequestsCompleted {
            model: big.clone(),
            n: 1,
        }),
        Payload::Event(Event::KernelCompiled {
            chip: None,
            source: long.clone(),
        }),
        Payload::Event(Event::KernelCompiled {
            chip: Some(big_chip.clone()),
            source: "k.cpp".into(),
        }),
        Payload::Event(Event::Reset {
            chips: vec![big_chip.clone()],
        }),
        Payload::Event(Event::LinkChanged {
            chip: big_chip.clone(),
            mask: 1,
        }),
        Payload::Event(Event::LeaseGranted {
            lease: EntityKey::lease(&long),
            chips: vec![],
        }),
        Payload::Event(Event::LeaseReleased {
            lease: EntityKey::lease("l"),
            chips: vec![big_chip.clone()],
        }),
        Payload::Event(Event::EccTick {
            chip: big_chip.clone(),
            channel: 0,
        }),
        Payload::Event(Event::ViewChanged { view: long.clone() }),
        Payload::Event(Event::Spark {
            chip: None,
            kind: long.clone(),
        }),
        Payload::Event(Event::Spark {
            chip: Some(big_chip),
            kind: "k".into(),
        }),
        Payload::Gap(Gap {
            reason: long.clone(),
        }),
    ];
    let n = payloads.len() as u64;
    let mut feed = Feed::new();
    let mut out = Vec::new();
    for (i, p) in payloads.into_iter().enumerate() {
        feed.push(&record(i as u64, 0, "backend", p.clone()), &mut out);
        assert!(out.is_empty(), "payload {i} gave observations: {p:?}");
    }
    assert_eq!(feed.oversized(), n);
    assert_eq!(feed.ordinal(), 0, "a dropped record has no ordinal");
    // The pacemaker is still unset: the dropped sweeps were not seen.
    let ord = feed.ordinal();
    feed.push(
        &record(
            99,
            0,
            "backend",
            sweep(chip(3), vec![("t", Value::F64(1.0))]),
        ),
        &mut out,
    );
    assert_eq!(feed.ordinal(), ord + 1);
    assert_eq!(ticks(&out), 1, "chip 3 became the pacemaker");
}

#[test]
fn saga_signals_from_another_source_are_rejected() {
    for p in [
        sample(chip(1), "saga.heartbeat_alive", Value::F64(1.0)),
        sample(chip(1), "saga.temperature_baseline", Value::F64(1.0)),
        sweep(
            chip(1),
            vec![
                ("temp", Value::F64(1.0)),
                ("saga.anything", Value::F64(1.0)),
            ],
        ),
    ] {
        let (feed, out) = feed_one("backend", p.clone());
        assert!(out.is_empty(), "{p:?} was accepted");
        assert_eq!(
            (feed.ordinal(), feed.rejected(), feed.oversized()),
            (0, 1, 0)
        );
        // From the saga itself the record is skipped, and not counted as
        // rejected.
        let (feed, out) = feed_one("saga", p);
        assert!(out.is_empty());
        assert_eq!((feed.ordinal(), feed.rejected()), (0, 0));
    }
    // A name that merely contains "saga" is fine.
    let (feed, out) = feed_one(
        "backend",
        sample(chip(1), "toplike.saga.x", Value::F64(1.0)),
    );
    assert_eq!((out.len(), feed.rejected()), (1, 0));
}
