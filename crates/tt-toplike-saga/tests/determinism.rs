//! Determinism tests for the Saga front door (spec 3.5): the same ordered
//! stream of record contents gives the same saga.
//!
//! The three fixtures in `tests/fixtures/`:
//!
//! * `quiet.jsonl`: a five-second recording of `tt-toplike-tui --headless
//!   --mock` made without `--mock-workloads` (ledger ruling R6), so it holds
//!   three mock chips and their DRAM channels and no process or host names.
//! * `bringup.jsonl`: a copy of the bus crate's fixture: a model loading, a
//!   reset and request completions, as plain samples. It has no sweeps, so it
//!   has no ticks and no world is ever born from it; its digest pins that a
//!   log without a clock tells no story.
//! * `busy.jsonl`: written by the generator in `tests/support/busy.rs` (a
//!   scripted model, requests, kernel compiles, a reset, a throttle, ECC
//!   errors, a training run). `the_busy_fixture_matches_its_generator`
//!   keeps the file and the generator in step.
//!
//! In generated-name mode the published beats carry aliased causes (ledger
//! ruling R17), so the digests differ between the two name modes and every
//! comparison here is made within one mode.

mod support;

use tt_toplike_bus::Record;
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::{Label, Saga, SagaConfig};

/// The three fixtures, by file name.
const FIXTURES: [&str; 3] = ["quiet.jsonl", "bringup.jsonl", "busy.jsonl"];

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

/// Golden digests for the three fixtures, one per name mode.
///
/// These pin the current behaviour: the seed rule, the maze, the hero's walk,
/// quest planning, the chronicle's grammar and lexicon, and the event order.
/// They were written once after the first green run. When a rule changes on
/// purpose, re-pin them deliberately (run the test, copy the printed values)
/// and say in the commit which rule changed. An unexplained change here means
/// the story changed for the same log, which breaks replay.
#[test]
fn a_snapshot_of_the_digests_matches() {
    let golden: [(&str, u64, u64); 3] = [
        ("quiet.jsonl", 0xf8b2d7d6ff73eaa8, 0xf8b2d7d6ff73eaa8),
        ("bringup.jsonl", 0xe06ca19a1295002d, 0xe06ca19a1295002d),
        ("busy.jsonl", 0xc9b045f373ff63b1, 0xb0c2b646d8116749),
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
                let floor = &state.world.floors[state.hero.floor as usize];
                assert!(floor.cell(state.hero.pos.0, state.hero.pos.1).walkable());
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
