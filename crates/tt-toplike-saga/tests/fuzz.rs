//! Fuzz test for the Saga front door (spec 11, "Robustness"): random record
//! streams never panic, and the state stays well formed after every record.
//!
//! The streams come from `tests/support/fuzz.rs`, driven by the crate's own
//! `Pcg32`, so every run is the same and a failure names the seed and the
//! record that caused it. To replay a seed alone, put it first in [`SEEDS`].
//! (The crate's `clippy.toml` bans `std::env::var` in tests as well, so the
//! seed list is not read from the environment.)
//!
//! The bounds tests (`memory_stays_bounded`,
//! `a_step_costs_under_a_million_operations`) are unit tests in
//! `src/fuzz_tests.rs`: they read the saga's private collections and a work
//! counter that exists only in the crate's own test build (ledger ruling R9).

mod support;

use std::panic::{catch_unwind, AssertUnwindSafe};

use support::fuzz::{random_stream, FUZZ_RECORDS};
use tt_toplike_bus::Record;
use tt_toplike_saga::chronicle::{NameMode, CAPACITY};
use tt_toplike_saga::maze::Cell;
use tt_toplike_saga::rng::Pcg32;
use tt_toplike_saga::world::MAX_CHIPS;
use tt_toplike_saga::{Label, Saga, SagaConfig};

/// The seeds a normal run tries. Each gives a stream of [`FUZZ_RECORDS`]
/// records.
const SEEDS: [u64; 3] = [1, 0x005e_ed0f_5a9a, 0xdead_beef_caff];

/// A short season, so quiet stretches end worlds and dreams are born within
/// one stream. The seed picks the name mode and the label, so the three
/// seeds cover both name modes and every label.
fn config(seed: u64) -> SagaConfig {
    SagaConfig {
        season_ticks: 40,
        names: if seed.is_multiple_of(2) {
            NameMode::Real
        } else {
            NameMode::Generated
        },
        label: [Label::Live, Label::Mock, Label::Replay][(seed % 3) as usize],
    }
}

/// Checks the state after record `i` of the stream from `seed`. Every
/// message names the seed and the record, so a failure can be replayed.
fn check_state(saga: &Saga, seed: u64, i: usize, last_tick: u64) {
    let s = saga.state();
    let at = format!("seed {seed:#x}, record {i}");
    assert!(
        s.chronicle.len() <= CAPACITY,
        "{at}: chronicle {}",
        s.chronicle.len()
    );
    assert!(s.tick >= last_tick, "{at}: the tick went back");
    let w = &s.world;
    assert_eq!(
        w.chips.len(),
        w.floors.len(),
        "{at}: chips and floors differ"
    );
    assert!(!w.floors.is_empty(), "{at}: a world with no floor");
    assert!(w.chips.len() <= MAX_CHIPS, "{at}: {} chips", w.chips.len());
    assert_eq!(s.has_world(), w.id > 0, "{at}: has_world disagrees with id");
    let floor = w
        .floors
        .get(s.hero.floor as usize)
        .unwrap_or_else(|| panic!("{at}: hero on floor {} of {}", s.hero.floor, w.floors.len()));
    let (x, y) = s.hero.pos;
    assert!(
        x < floor.w && y < floor.h,
        "{at}: hero at {x},{y} on a {}x{} floor",
        floor.w,
        floor.h
    );
    // The hero stands on a walkable cell, or on the entrance gate it was born
    // on. A ruin or a door is walkable; a wall never is.
    assert!(
        floor.cell(x, y) != Cell::Wall,
        "{at}: hero inside a wall at {x},{y}"
    );
}

/// Feeds `records` to a fresh saga, checking the state after each record and
/// draining events at random points. Returns the saga.
fn run(seed: u64, records: &[Record]) -> Saga {
    let mut saga = Saga::new(config(seed));
    // When to drain: a separate stream, so the record stream is unchanged.
    let mut drains = Pcg32::new(seed, 0xd1a1);
    let mut last_tick = 0;
    for (i, rec) in records.iter().enumerate() {
        // A record from source `saga` must change nothing at all.
        let before = (rec.source == "saga").then(|| (saga.digest(), saga.state().tick));
        let fed = catch_unwind(AssertUnwindSafe(|| saga.feed(rec)));
        if let Err(cause) = fed {
            eprintln!("saga panicked: seed {seed:#x}, record {i}: {rec:?}");
            std::panic::resume_unwind(cause);
        }
        if let Some(before) = before {
            assert_eq!(
                before,
                (saga.digest(), saga.state().tick),
                "seed {seed:#x}, record {i}: a saga-sourced record changed the saga"
            );
        }
        check_state(&saga, seed, i, last_tick);
        last_tick = saga.state().tick;
        if drains.chance(1, 50) {
            saga.drain_events();
        }
    }
    saga
}

#[test]
fn random_record_streams_never_panic() {
    for (n, seed) in SEEDS.into_iter().enumerate() {
        let records = random_stream(seed, FUZZ_RECORDS);
        let saga = run(seed, &records);

        // The stream reached the saga's main paths: the clock ran, many
        // worlds were born, and some chips were turned away at the cap.
        let s = saga.state();
        assert!(s.has_world(), "seed {seed:#x}: no world was ever born");
        assert!(s.tick > 1000, "seed {seed:#x}: only {} ticks", s.tick);
        assert!(
            s.world.id > 10,
            "seed {seed:#x}: only {} worlds were born",
            s.world.id
        );

        // The first seed is checked for reproducibility: the generator gives
        // the same stream again, and the stream gives the same digest. (Each
        // run costs several seconds in a debug build, so only one seed is run
        // twice.) Records holding NaN never compare equal, so the streams are
        // compared by their Debug text, which prints NaN the same way each
        // time.
        if n == 0 {
            let text = |rs: &[Record]| format!("{rs:?}");
            assert!(
                text(&random_stream(seed, FUZZ_RECORDS)) == text(&records),
                "seed {seed:#x}: the generator gave two streams"
            );
            let mut again = Saga::new(config(seed));
            for r in &records {
                again.feed(r);
            }
            assert_eq!(
                again.digest(),
                saga.digest(),
                "seed {seed:#x}: one stream gave two digests"
            );
        }
    }
}
