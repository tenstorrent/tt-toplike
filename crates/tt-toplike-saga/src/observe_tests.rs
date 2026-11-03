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

/// A record that is neither a sweep nor about the host: it takes an
/// ordinal and nothing else.
fn filler() -> Payload {
    sample(EntityKey::model("m"), "filler", Value::U64(0))
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
    // Six records; the gap takes no ordinal.
    assert_eq!((n1, n2, n3), (5, 5, 5));
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

/// A `ViewChanged` event and a gap describe the process that hosts the bus
/// (a person switching views, a log hole a replay host found, a disk
/// outage). A replaying TUI adds them at moments set by key presses and
/// thread timing, so they take no ordinal and give no observation
/// (final-review finding C1).
#[test]
fn view_changes_and_gaps_take_no_ordinal() {
    let base = vec![
        ("tt-smi", sweep(chip(1), vec![("a", Value::U64(1))])),
        ("tt-smi", sample(chip(2), "s", Value::U64(9))),
        ("tt-smi", sweep(chip(1), vec![("a", Value::U64(2))])),
    ];
    let (want, want_ordinal) = run(&base, |i| (i, i));
    assert_eq!(want_ordinal, 3);
    let view = Payload::Event(Event::ViewChanged {
        view: "saga".into(),
    });
    // Each kind of host record, at the front, in the middle and at the end.
    for (source, extra) in [
        ("view", view.clone()),
        ("tt-smi", view),
        ("replay", gap()),
        ("tt-toplike-bus", gap()),
    ] {
        for at in [0, 1, 3] {
            let mut payloads = base.clone();
            payloads.insert(at, (source, extra.clone()));
            let (out, ordinal) = run(&payloads, |i| (i, i));
            assert_eq!(out, want, "{source} at {at} changed the observations");
            assert_eq!(ordinal, want_ordinal, "{source} at {at} took an ordinal");
        }
    }
    // The ordinal a later record gets is unchanged too: a beat's cause
    // carries it.
    let mut feed = Feed::new();
    let mut out = Vec::new();
    feed.push(&record(1, 0, "view", gap()), &mut out);
    feed.push(
        &record(2, 0, "tt-smi", sample(chip(1), "a", Value::U64(1))),
        &mut out,
    );
    assert_eq!(feed.ordinal(), 1);
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

/// One sweep of `entity` from the backend.
fn sweep_of(entity: EntityKey) -> (&'static str, Payload) {
    ("backend", sweep(entity, vec![("a", Value::U64(1))]))
}

/// `rounds` backend ticks, each one sweep of every chip in `chips`, in
/// order.
fn rounds(chips: &[u32], rounds: usize) -> Vec<(&'static str, Payload)> {
    let mut out = Vec::new();
    for _ in 0..rounds {
        for &c in chips {
            out.push(sweep_of(chip(c)));
        }
    }
    out
}

/// Ticks per round of `per_round` records, in order.
fn ticks_per_round(out_by_record: &[usize], per_round: usize) -> Vec<usize> {
    out_by_record
        .chunks(per_round)
        .map(|c| c.iter().sum())
        .collect()
}

/// Feeds each payload and returns the number of ticks each one gave.
fn ticks_by_record(payloads: &[(&str, Payload)]) -> Vec<usize> {
    let mut feed = Feed::new();
    payloads
        .iter()
        .enumerate()
        .map(|(i, (source, p))| {
            let mut out = Vec::new();
            feed.push(&record(i as u64, 0, source, p.clone()), &mut out);
            ticks(&out)
        })
        .collect()
}

/// Only a chip sweep can be the pacemaker. A process scan sweeps each TT
/// process every 2 s, so before the final fix wave a process sweep that
/// arrived first (an attached TUI's first record, or a backend whose first
/// poll had no telemetry) became the clock and ran it 20 times slower
/// (finding I2).
#[test]
fn a_sweep_of_anything_but_a_chip_never_ticks() {
    let process = EntityKey::process(4242, 17);
    let dram = EntityKey::dram_channel(&chip(1), 0);
    let mut payloads = vec![sweep_of(process.clone()), sweep_of(dram.clone())];
    payloads.extend(rounds(&[1], 1));
    payloads.push(sweep_of(process.clone()));
    payloads.extend(rounds(&[1], 1));
    payloads.push(sweep_of(dram));
    assert_eq!(ticks_by_record(&payloads), vec![0, 0, 1, 0, 1, 0]);
    // A stream with no chip sweep at all has no clock.
    let only_processes: Vec<_> = (0..10).map(|_| sweep_of(process.clone())).collect();
    assert!(ticks_by_record(&only_processes).iter().all(|&t| t == 0));
}

/// On a box with several chips the first chip to sweep stays the pacemaker
/// for as long as it sweeps: one tick per backend tick.
#[test]
fn a_live_pacemaker_keeps_the_clock() {
    let per = ticks_by_record(&rounds(&[1, 2, 3], 200));
    assert_eq!(ticks_per_round(&per, 3), vec![1; 200]);
    // The tick follows chip 1's sweep, the first in each round.
    assert!(per.chunks(3).all(|c| c == [1, 0, 0]));
}

/// When the pacemaker stops sweeping (its chip went away, or a backend
/// switch changed every key), the chip whose sweep completes the
/// [`PACEMAKER_TIMEOUT_ROUNDS`]th round without it takes over. From then on
/// there is still exactly one tick per backend tick. Before the final fix
/// wave every sweep ticked once 2,000 records had passed, for the rest of
/// the run (finding I2).
#[test]
fn a_quiet_pacemaker_hands_the_clock_to_the_next_chip() {
    let mut payloads = rounds(&[1, 2, 3], 5);
    payloads.extend(rounds(&[2, 3], 40));
    let per = ticks_by_record(&payloads);
    let (before, after) = per.split_at(15);
    assert_eq!(ticks_per_round(before, 3), vec![1; 5]);
    let after = ticks_per_round(after, 2);
    // A round is noticed as missed when a chip sweeps a second time since
    // the pacemaker last swept. Chip 1 would have swept first in each
    // round, so chip 2's sweep in quiet round 3 completes the third round
    // chip 1 missed, takes over and ticks. Rounds 1 and 2 have no tick.
    let n = PACEMAKER_TIMEOUT_ROUNDS as usize;
    assert_eq!(after[..n - 1], vec![0; n - 1]);
    assert_eq!(after[n - 1..], vec![1; 41 - n]);

    // The same after a backend switch: every key is new. The first round
    // of new keys repeats no chip, so the handover comes one round later.
    let mut payloads = rounds(&[1, 2, 3], 5);
    payloads.extend(rounds(&[5, 6, 7], 40));
    let per = ticks_by_record(&payloads);
    let after = ticks_per_round(&per[15..], 3);
    assert_eq!(after[..n], vec![0; n]);
    assert_eq!(after[n..], vec![1; 40 - n]);

    // A pacemaker that comes back is one chip among the others: the clock
    // stays with the chip that took over, at one tick a round.
    let mut payloads = rounds(&[1, 2], 5);
    payloads.extend(rounds(&[2], 10));
    payloads.extend(rounds(&[1, 2], 10));
    let per = ticks_by_record(&payloads);
    assert_eq!(ticks_per_round(&per[20..], 2), vec![1; 10]);
    assert!(per[20..].chunks(2).all(|c| c == [0, 1]));
}

/// Records that are not chip sweeps do not count toward the timeout: a
/// pacemaker that sweeps once per round keeps the clock however many other
/// records arrive between its sweeps.
#[test]
fn other_records_do_not_count_toward_the_timeout() {
    let mut payloads = Vec::new();
    for _ in 0..20 {
        payloads.push(sweep_of(chip(1)));
        for _ in 0..500 {
            payloads.push(("backend", filler()));
        }
        payloads.push(sweep_of(EntityKey::process(1, 1)));
    }
    let per = ticks_by_record(&payloads);
    assert_eq!(per.iter().sum::<usize>(), 20);
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
