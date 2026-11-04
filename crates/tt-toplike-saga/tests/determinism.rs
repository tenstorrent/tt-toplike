//! Determinism tests for the Saga front door (spec 3.5): the same ordered
//! stream of record contents gives the same saga.
//!
//! The fixtures in `tests/fixtures/`:
//!
//! * `quiet.jsonl`: a six-second recording (60 ticks) of `tt-toplike-tui
//!   --headless --mock` made without `--mock-workloads` (ledger ruling R6), so
//!   it holds three mock chips and their DRAM channels and no process or host
//!   names. It was recorded after the mock's ARC heartbeat began counting up
//!   on every update (ruling R18), so its chips look alive to the saga.
//! * `bringup.jsonl`: a copy of the bus crate's fixture: a model loading, a
//!   reset and request completions, as plain samples. It has no sweeps, so it
//!   has no ticks and no world is ever born from it; its digest pins that a
//!   log without a clock tells no story.
//! * `busy.jsonl`: written by the generator in `tests/support/busy.rs` (a
//!   scripted model, requests, kernel compiles, a reset, a throttle, ECC
//!   errors, a training run). `the_busy_fixture_matches_its_generator`
//!   keeps the file and the generator in step.
//!
//! * `solo.jsonl` and `party4.jsonl`: written by `tests/support/party.rs` (one
//!   chip and four chips). `tests/party.rs` keeps them in step with the
//!   generator. Their digests were first pinned on the single-chip code
//!   before the party (0.16.4), then re-pinned with the others for digest
//!   versions 2 and 3.
//! * `cross4.jsonl`: the four-chip party log with a ring of Ethernet links
//!   (`records_with_links` in the same generator), the log in which heroes
//!   cross. Pinned by Task B.4 of the party plan.
//!
//! In generated-name mode the published beats carry aliased causes (ledger
//! ruling R17), so the digests differ between the two name modes and every
//! comparison here is made within one mode.

mod support;

use tt_toplike_bus::Record;
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::{Label, Saga, SagaConfig};

/// The six fixtures, by file name.
const FIXTURES: [&str; 6] = [
    "quiet.jsonl",
    "bringup.jsonl",
    "busy.jsonl",
    "solo.jsonl",
    "party4.jsonl",
    "cross4.jsonl",
];

/// A season of 200 ticks (20 s at 10 Hz), short enough that the busy log's
/// quiet stretches end worlds and dreams are born.
fn config(names: NameMode) -> SagaConfig {
    SagaConfig {
        season_ticks: 200,
        names,
        label: Label::Mock,
    }
}

/// Feeds every record to a fresh saga and returns its digest.
fn digest_of(records: &[Record], names: NameMode) -> u64 {
    let mut saga = Saga::new(config(names));
    for r in records {
        saga.feed(r);
    }
    saga.digest()
}

#[test]
fn the_busy_fixture_matches_its_generator() {
    let on_disk = std::fs::read_to_string(support::fixture_path("busy.jsonl"))
        .expect("busy.jsonl is committed; regenerate it with `cargo test -p tt-toplike-saga --test determinism -- --ignored write_busy_fixture`");
    assert!(
        on_disk == support::busy::file_text(),
        "busy.jsonl differs from tests/support/busy.rs; regenerate it with the ignored test write_busy_fixture"
    );
}

/// Writes `tests/fixtures/busy.jsonl` from the generator. Run it by hand
/// after changing the generator; the test above then checks the result.
#[test]
#[ignore = "writes the busy fixture; run by hand after changing tests/support/busy.rs"]
fn write_busy_fixture() {
    std::fs::write(
        support::fixture_path("busy.jsonl"),
        support::busy::file_text(),
    )
    .expect("write busy.jsonl");
}

#[test]
fn the_same_records_give_the_same_digest_twice() {
    for name in FIXTURES {
        let records = support::read_fixture(name);
        for names in [NameMode::Real, NameMode::Generated] {
            assert_eq!(
                digest_of(&records, names),
                digest_of(&records, names),
                "{name} ({names:?}) gave two digests"
            );
        }
    }
}

#[test]
fn renumbered_and_retimed_records_give_the_same_digest() {
    for name in FIXTURES {
        let records = support::read_fixture(name);
        let replayed = support::renumber(&records);
        // The renumbering must really change every record's numbers, or this
        // test could pass without testing anything.
        assert!(records.iter().zip(&replayed).all(|(a, b)| a.seq != b.seq
            && a.t_mono_ns != b.t_mono_ns
            && a.t_wall_ms != b.t_wall_ms));
        for names in [NameMode::Real, NameMode::Generated] {
            assert_eq!(
                digest_of(&records, names),
                digest_of(&replayed, names),
                "{name} ({names:?}): a replay host's numbering changed the saga"
            );
        }
    }
}

/// A TUI that hosts its bus publishes records about itself into it: a
/// `ViewChanged` on its first frame and on every view key, and a replay host
/// a gap at each hole it finds in the log (the bus adds one for a disk
/// outage too). Their positions depend on thread timing and key presses, so
/// two replays of one log would tell two stories if they counted (final
/// review finding C1). Inserting them anywhere leaves every digest as it was.
#[test]
fn records_a_tui_adds_about_itself_do_not_change_the_digest() {
    use tt_toplike_bus::{Event, Gap, Payload};
    use tt_toplike_saga::rng::Pcg32;
    let host_record = |k: u32| {
        let (source, payload) = match k % 3 {
            0 => (
                "view",
                Payload::Event(Event::ViewChanged {
                    view: "saga".into(),
                }),
            ),
            1 => (
                "replay",
                Payload::Gap(Gap {
                    reason: "a hole in the log".into(),
                }),
            ),
            _ => (
                "tt-toplike-bus",
                Payload::Gap(Gap {
                    reason: "disk outage".into(),
                }),
            ),
        };
        Record {
            seq: 0,
            t_mono_ns: 0,
            t_wall_ms: 0,
            source: source.into(),
            payload,
        }
    };
    for (i, name) in FIXTURES.iter().enumerate() {
        let records = support::read_fixture(name);
        let mut rng = Pcg32::new(0x5a6a + i as u64, 7);
        for trial in 0..4u32 {
            let mut mixed = records.clone();
            for k in 0..12u32 {
                let at = rng.next_u32() as usize % (mixed.len() + 1);
                mixed.insert(at, host_record(k + trial));
            }
            for names in [NameMode::Real, NameMode::Generated] {
                assert_eq!(
                    digest_of(&records, names),
                    digest_of(&mixed, names),
                    "{name} ({names:?}) trial {trial}: a host record changed the saga"
                );
            }
        }
    }
}

/// The first world's seed, and the index of the record at which it was
/// born, for `records`.
fn first_world(records: &[Record]) -> (u64, usize) {
    let mut saga = Saga::new(config(NameMode::Real));
    for (i, r) in records.iter().enumerate() {
        saga.feed(r);
        if saga.state().world.id == 1 {
            return (saga.state().world.seed, i);
        }
    }
    panic!("no world was born");
}

/// The smallest change a real temperature reading can make, the lowest
/// mantissa bit of its `f32`, changes the first world. Every float the
/// backend sends is an `f32` widened to `f64`, so the low byte of its `f64`
/// bits is always 0; before the final fix wave the seed ring kept only that
/// byte, and this failed (finding I1).
#[test]
fn one_temperature_bit_changes_the_first_world() {
    use tt_toplike_bus::{Payload, Value};
    let records = support::read_fixture("quiet.jsonl");
    let (seed, born_at) = first_world(&records);
    // The newest temperature reading at or before the birth. The birth
    // comes from a pacemaker sweep's tick, which follows that sweep's own
    // samples, so its temperature is still in the seed ring when the seed
    // is drawn. (An older one may not be: the mock sends about 90 DRAM
    // samples between two sweeps.)
    let (idx, pos) = (0..=born_at)
        .rev()
        .find_map(|i| match &records[i].payload {
            Payload::Sweep(s) => s
                .signals
                .iter()
                .position(|n| n == "tt_asic_temperature_celsius")
                .map(|p| (i, p)),
            _ => None,
        })
        .expect("a temperature before the first world");
    let mut changed = records.clone();
    let Payload::Sweep(s) = &mut changed[idx].payload else {
        unreachable!()
    };
    let Value::F64(x) = s.values[pos] else {
        panic!("a temperature is a float")
    };
    let narrow = x as f32;
    assert_eq!(f64::from(narrow), x, "the recorded temperature is an f32");
    s.values[pos] = Value::F64(f64::from(f32::from_bits(narrow.to_bits() ^ 1)));
    let (changed_seed, changed_born_at) = first_world(&changed);
    assert_eq!(born_at, changed_born_at);
    assert_ne!(seed, changed_seed, "a changed reading gave the same world");
}

/// Golden digests for the fixtures, one per name mode.
///
/// These pin the current behaviour: the seed rule, the maze, the hero's walk,
/// quest planning, the chronicle's grammar and lexicon, and the event order.
/// They were written once after the first green run. When a rule changes on
/// purpose, re-pin them deliberately (run the test, copy the printed values)
/// and say in the commit which rule changed. An unexplained change here means
/// the story changed for the same log, which breaks replay.
///
/// Every row was re-pinned for digest version 2 (the party addendum, section
/// 3.6): the version item moves all of them, and the floor fields on quest
/// events, beats and lines move every fixture that births a world. The
/// stories themselves did not change (`tests/party.rs` pins the solo story
/// print and trace, and they held).
///
/// `quiet`, `busy` and `party4` were re-pinned again for the party (Task A.3
/// of the party plan): a world on more than one chip now has one member per
/// floor, each with its own quests, events and position in the end item,
/// and a model's dungeon now spans every chip (in `busy`, `demo-scribe`'s
/// dungeon, ready on chip A, gained chip B's floor). `bringup` births no world and `solo` has
/// one chip, so neither moved.
///
/// `quiet`, `busy` and `party4` were re-pinned again for the merged
/// chronicle (Task A.4): a party's birth writes one `PartyBorn` line in
/// place of no hero line, and the pace leaves some members' routine lines
/// unwritten, so the beat events, the chronicle lines and the chronicle
/// generator's later draws all change. In generated mode an unwritten
/// quest's reason is aliased through `clean_cause`. `bringup` and `solo`
/// did not move.
///
/// Every row was re-pinned for digest version 3 (Task B.3 of the party
/// plan): only the version byte moved. Task B.3 also binds each portal to
/// an Ethernet port and lets a generic floor's portal count follow its live
/// mask. With `DIGEST_VERSION` set back to 2 on the B.3 code, every row
/// matched the version 2 pins, so no fixture's floors, walks or lines
/// changed: `quiet` (mock chips) and the party logs are Blackhole-shaped,
/// and no fixture has a generic chip with a known mask.
///
/// `quiet` was re-pinned once more in B.3's fix round: the saga now reads a
/// register from its exact `U64` bits. Its three mock chips report the live
/// mask 0xAAAA_AAAA_AAAA_AAAA, which as an f64 lost its low bits and gave
/// each Blackhole floor one portal (port 11); exact, it gives six (ports 1,
/// 3, 5, 7, 9 and 11), so the floors' cells and the walks change. The other
/// fixtures' registers are below 2^53 and did not move.
///
/// `cross4` was added by Task B.4 (the crossing quest), pinned on its first
/// green run. It is the only fixture with `LinkPeer` events, so the only one
/// in which heroes cross: its digest holds the `HeroCrossed` events (tag
/// 0x07), the crossing lines and the members' changed floors. No hero is
/// in transit on any of its links when one goes down, so it holds no
/// `CrossingFailed`. The crossing added no item to a stream without a
/// crossing (a member's transit bit in the end item is 0 off a crossing),
/// so the other five rows did not move.
#[test]
fn a_snapshot_of_the_digests_matches() {
    let golden: [(&str, u64, u64); 6] = [
        ("quiet.jsonl", 0xd6e0200609dcd3eb, 0xd6e0200609dcd3eb),
        ("bringup.jsonl", 0xc9142f2256b8175a, 0xc9142f2256b8175a),
        ("busy.jsonl", 0x23a8d7bcf22b2d8b, 0xfd4e69d1d98aa692),
        ("solo.jsonl", 0xa2eadd2b95cffd79, 0x3abf9f424e37b575),
        ("party4.jsonl", 0x01b9f4175d5de836, 0x0976f42c301be352),
        ("cross4.jsonl", 0x2a88827bd487ef7a, 0x34a41fcbacbba99c),
    ];
    let mut actual = Vec::new();
    for (name, _, _) in golden {
        let records = support::read_fixture(name);
        actual.push((
            name,
            digest_of(&records, NameMode::Real),
            digest_of(&records, NameMode::Generated),
        ));
    }
    let shown: Vec<String> = actual
        .iter()
        .map(|(n, r, g)| format!("(\"{n}\", {r:#018x}, {g:#018x}),"))
        .collect();
    assert_eq!(
        actual.as_slice(),
        golden.as_slice(),
        "digests changed; if a rule changed on purpose, re-pin with:\n{}",
        shown.join("\n")
    );
}

#[test]
fn a_busy_log_never_panics_and_always_has_a_world() {
    let records = support::read_fixture("busy.jsonl");
    for names in [NameMode::Real, NameMode::Generated] {
        let mut saga = Saga::new(config(names));
        let mut worlds = 0u64;
        for r in &records {
            saga.feed(r);
            let state = saga.state();
            // The first world must be born by tick 5 (ledger rulings R11 and
            // R13); from then on there is always one.
            if state.tick >= 5 {
                assert!(state.world.id >= 1, "no world at tick {}", state.tick);
                assert!(!state.world.floors.is_empty());
                // Every member stands on a walkable cell of its own floor.
                assert_eq!(state.party.len(), state.world.floors.len());
                for m in &state.party {
                    let floor = &state.world.floors[m.hero.floor as usize];
                    assert!(floor.cell(m.hero.pos.0, m.hero.pos.1).walkable());
                }
            }
            worlds = worlds.max(state.world.id);
        }
        // The script has a ready model, a reset, a model going away, a second
        // model, a training run and quiet stretches longer than a season.
        assert!(
            worlds >= 5,
            "only {worlds} worlds in the busy log ({names:?})"
        );
        assert!(!saga.state().chronicle.is_empty());
    }
}

#[test]
fn a_quiet_log_births_a_world_by_tick_five() {
    let records = support::read_fixture("quiet.jsonl");
    let mut saga = Saga::new(config(NameMode::Real));
    for r in &records {
        saga.feed(r);
        if saga.state().tick == 5 {
            break;
        }
    }
    let state = saga.state();
    assert_eq!(state.tick, 5);
    assert!(state.world.id >= 1, "no world by tick 5");
    assert!(state.world.born_tick <= 5);
    // The recording has three mock chips, each sending its Tensix column mask
    // in the first poll, so the world has three Blackhole floors.
    assert_eq!(state.world.chips.len(), 3);
}
