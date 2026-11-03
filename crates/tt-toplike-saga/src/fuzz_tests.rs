//! Bounds tests for the Saga core (Task 1.12, ledger rulings R9 and R20).
//!
//! These are unit tests of the `saga` module because they read the saga's
//! private collections and the work counter in [`crate::cost`], which exists
//! only in the crate's own test build. The random fuzz is the integration
//! test `tests/fuzz.rs`; both use the generators in `tests/support/fuzz.rs`.
//!
//! The limits below are numbers copied from the ruling and the spec. The
//! tests compare the saga's collections with these copies, so raising a cap
//! in the code makes them fail.

use std::collections::BTreeSet;

use super::*;
use crate::cost;
use crate::hero::HEARTBEAT_ALIVE;
use crate::maze::{MIN_H, MIN_W};
use crate::stats::RESERVED_TRAILS;
use crate::topology::{MAX_COLS, MAX_ROWS};
use crate::world::MAX_FLOORS;

#[allow(dead_code)]
#[path = "../tests/support/fuzz.rs"]
mod gen;

/// Chips a saga keeps (ruling R20).
const RULED_CHIPS: usize = 64;
/// Signal names trailed per chip (ruling R20), not counting the reserved
/// vitality slots.
const RULED_TRAIL_NAMES: usize = 128;
/// Ready models kept, and so sage hooks (ruling R20).
const RULED_READY: usize = 64;
/// Undrained events kept (ruling R20).
const RULED_EVENTS: usize = 4096;
/// Bytes in the longest key, signal name or event string kept (ruling R21).
const RULED_ITEM_BYTES: usize = 256;
/// Floors one world generates (ruling R22).
const RULED_FLOORS: usize = 32;
/// Chronicle lines (spec 9).
const SPEC_CHRONICLE: usize = 500;
/// Entities (spec 9).
const SPEC_ENTITIES: usize = 500;
/// Generated names remembered (Task 1.10).
const ALIASES: usize = 256;
/// Work units one record may cost (the brief's "a step costs under a million
/// operations").
const BUDGET: u64 = 1_000_000;

fn config(names: NameMode) -> SagaConfig {
    SagaConfig {
        season_ticks: 12_000,
        names,
        label: Label::Live,
    }
}

#[test]
fn memory_stays_bounded() {
    let records = gen::flood_stream();
    for names in [NameMode::Real, NameMode::Generated] {
        // `saga` is never drained. `twin` sees the same records and is
        // drained after every one, which counts every event raised.
        let mut saga = Saga::new(config(names));
        let mut twin = Saga::new(config(names));
        let mut emitted = 0u64;
        let mut newest = None;
        for r in &records {
            saga.feed(r);
            twin.feed(r);
            let batch = twin.drain_events();
            emitted += batch.len() as u64;
            if let Some(last) = batch.last() {
                newest = Some(last.clone());
            }
        }
        let at = format!("{names:?}");

        // Chips: the flood reports 201, the saga keeps 64.
        assert_eq!(saga.chips.len(), RULED_CHIPS, "{at}: chips kept");
        assert_eq!(saga.topo.len(), saga.chips.len(), "{at}: one spec per chip");

        // Trails: only kept chips have trails, and each has at most 128
        // names plus the reserved vitality slots. The flood offers 300.
        let trailed = saga.trails.entities();
        assert!(trailed <= RULED_CHIPS, "{at}: {trailed} trailed entities");
        let chip = gen::flood_chip(0);
        let names_on_chip = saga.trails.names(&chip);
        assert!(
            (RULED_TRAIL_NAMES..=RULED_TRAIL_NAMES + RESERVED_TRAILS).contains(&names_on_chip),
            "{at}: {names_on_chip} trailed names on {chip}"
        );
        for c in saga.chips.keys() {
            assert!(
                saga.trails.names(c) <= RULED_TRAIL_NAMES + RESERVED_TRAILS,
                "{at}: {} trailed names on {c}",
                saga.trails.names(c)
            );
        }
        // A flooded chip still gets its heartbeat series: the vitality
        // inputs have their own slots.
        assert!(
            saga.trails.get(&chip, HEARTBEAT_ALIVE).is_some(),
            "{at}: the flood crowded out the heartbeat series"
        );

        // Ready models and hooks: 201 models are ready and none goes away.
        assert_eq!(saga.ready.len(), RULED_READY, "{at}: ready models kept");
        // Members: one per floor, at most 32 (the party addendum, 3.7).
        assert!(
            saga.members.len() <= RULED_FLOORS,
            "{at}: {} members",
            saga.members.len()
        );
        assert_eq!(saga.members.len(), saga.state.party.len(), "{at}");
        assert_eq!(saga.members.len(), saga.state.world.floors.len(), "{at}");
        for core in &saga.members {
            let sages = core
                .hooks
                .iter()
                .filter(|h| h.verb == Verb::VisitSage)
                .count();
            assert!(sages <= RULED_READY, "{at}: {sages} sage hooks");
            assert!(core.hooks.len() <= RULED_READY + 2, "{at}: hooks");
        }
        // Named chip keys that are not kept chips: one shared set of at
        // most 64 (ruling R31), and every model's list holds only kept
        // chips and keys of that set. The flood names 2,048 such keys.
        assert!(
            saga.named_unseen.len() <= RULED_CHIPS,
            "{at}: {} named keys held",
            saga.named_unseen.len()
        );
        // The flood reached the cap, so the bound above was tested.
        assert_eq!(
            saga.named_unseen.len(),
            RULED_CHIPS,
            "{at}: the flood's unseen keys did not fill the shared set"
        );
        for r in saga.ready.values() {
            for c in &r.chips {
                assert!(
                    saga.chips.contains_key(c) || saga.named_unseen.contains(c),
                    "{at}: {c} is held outside the shared set"
                );
            }
        }

        // Events: far more than 4096 were raised and none was drained.
        assert!(
            emitted > RULED_EVENTS as u64,
            "{at}: the flood raised only {emitted} events"
        );
        assert_eq!(saga.events.len(), RULED_EVENTS, "{at}: undrained events");
        assert_eq!(
            saga.dropped_events(),
            emitted - RULED_EVENTS as u64,
            "{at}: dropped events"
        );
        // The kept events are the newest (the oldest were dropped), and
        // dropping events does not change the digest.
        assert_eq!(saga.events.back(), newest.as_ref(), "{at}: newest event");
        assert_eq!(saga.digest(), twin.digest(), "{at}: digest");

        // The chronicle and its name table.
        assert!(
            saga.state.chronicle.len() <= SPEC_CHRONICLE,
            "{at}: chronicle"
        );
        assert!(
            saga.state.chronicle.alias_count() <= ALIASES,
            "{at}: aliases"
        );

        // The world: the huge ModelReady named 1000 chips; a world keeps 32,
        // and every floor fits the grid limit.
        let w = &saga.state.world;
        assert!(
            w.chips.len() <= RULED_FLOORS,
            "{at}: {} world chips",
            w.chips.len()
        );
        assert_eq!(w.chips.len(), w.floors.len(), "{at}: chips and floors");
        for f in &w.floors {
            assert!(
                usize::from(f.w) <= 120 && usize::from(f.h) <= 60,
                "{at}: a {}x{} floor",
                f.w,
                f.h
            );
        }

        // Item sizes (ruling R21): the flood's five 1 MB records were
        // dropped whole, and nothing the saga keeps is longer than 256
        // bytes.
        let mut longest = saga.trails.longest_item();
        let mut note = |what: &str, len: usize| {
            assert!(
                len <= RULED_ITEM_BYTES,
                "{at}: a {len}-byte {what} is stored"
            );
            longest = longest.max(len);
        };
        for c in saga.chips.keys() {
            note("chip key", c.as_str().len());
        }
        for (m, r) in &saga.ready {
            note("model key", m.as_str().len());
            note("cause entity", r.cause.entity.as_str().len());
            note("cause signal", r.cause.signal.len());
            for c in &r.chips {
                note("ready chip", c.as_str().len());
            }
        }
        for h in saga.members.iter().flat_map(|m| &m.hooks) {
            note(
                "hook target",
                h.target.as_ref().map_or(0, |t| t.as_str().len()),
            );
            note("hook reason", h.reason.entity.as_str().len());
            note("hook signal", h.reason.signal.len());
        }
        for beat in saga.state.chronicle.lines(CAPACITY) {
            note("beat cause entity", beat.cause.entity.as_str().len());
            note("beat cause signal", beat.cause.signal.len());
        }
        for c in &w.chips {
            note("world chip", c.as_str().len());
        }
        for m in &saga.state.party {
            note("quest reason", m.quest.reason.entity.as_str().len());
            note("quest signal", m.quest.reason.signal.len());
        }
        assert!(
            longest <= RULED_ITEM_BYTES,
            "{at}: a {longest}-byte trail key or name is stored"
        );
        assert_eq!(saga.oversized(), gen::FLOOD_OVERSIZED, "{at}: oversized");

        // Every entity the saga holds, counted once: under the spec's 500.
        let mut held: BTreeSet<&EntityKey> = BTreeSet::new();
        held.extend(saga.chips.keys());
        held.extend(saga.ready.keys());
        held.extend(saga.ready.values().flat_map(|r| &r.chips));
        held.extend(saga.named_unseen.iter());
        held.extend(w.chips.iter());
        held.extend(
            saga.members
                .iter()
                .flat_map(|m| &m.hooks)
                .filter_map(|h| h.target.as_ref()),
        );
        assert!(
            held.len() + trailed <= SPEC_ENTITIES,
            "{at}: {} entities held, {trailed} trailed",
            held.len()
        );
    }
}

/// One record's work stays under a million work units (spec 9's step
/// budget, ledger rulings R9 and R22).
///
/// A work unit is one of four things the test-only counter in
/// [`crate::cost`] counts: a maze cell generated, an A* expansion, a trail
/// push, or an event raised. A unit is not an elementary operation. A maze
/// cell costs roughly 30 elementary operations on today's floors (allocation,
/// room fill, corridors, Prim's comparisons, the ruin pass and the exit
/// search, estimated from the code in `maze.rs`), an A* expansion a few
/// dozen (four neighbours and the open set), and a trail push or an event a
/// few. So the budget of a million units is a budget of roughly tens of
/// millions of operations, and the number that matters for the 1 ms target
/// is the measured time quoted in the Task 1.12 report.
#[test]
fn a_step_stays_under_a_million_work_units() {
    // The structural worst case of one record, from the constants: at most
    // one world is born per record (see "Bounds" in the module comment of
    // saga.rs), with at most MAX_FLOORS floors of at most MAX_COLS x MAX_ROWS
    // cells; each of at most MAX_FLOORS members searches once, expanding
    // each cell of its floor at most once. Trail pushes and events are
    // counted below; they depend on the record's own size.
    let cells = u64::from(MAX_COLS) * u64::from(MAX_ROWS);
    let structural = MAX_FLOORS as u64 * cells * 2;
    assert!(
        structural < BUDGET,
        "the caps allow {structural} units in one record"
    );

    // The flood, then a fuzz stream, counted record by record.
    let mut worst = (cost::Units::default(), String::new());
    let mut measure = |records: &[Record], what: &str| {
        let mut saga = Saga::new(config(NameMode::Generated));
        cost::take();
        for (i, r) in records.iter().enumerate() {
            saga.feed(r);
            let u = cost::take();
            if u.total() > worst.0.total() {
                worst = (u, format!("{what} record {i}"));
            }
        }
    };
    measure(&gen::flood_stream(), "flood");
    measure(&gen::random_stream(1, 5_000), "fuzz seed 1");
    let (units, at) = worst;
    // Shown with `--nocapture`; the report for Task 1.12 quotes it.
    eprintln!("worst record: {at}: {units:?} = {} units", units.total());
    assert!(
        units.total() < BUDGET,
        "{at} cost {units:?} = {} units",
        units.total()
    );
    // The flood reached the widest birth: a floor for each of 32 chips, and
    // no more.
    let smallest_floor = u64::from(MIN_W) * u64::from(MIN_H);
    assert!(
        units.cells >= RULED_FLOORS as u64 * smallest_floor,
        "the worst record ({at}, {units:?}) was not a 32-floor birth"
    );
    assert!(
        units.cells <= RULED_FLOORS as u64 * cells,
        "the worst record ({at}, {units:?}) generated more than 32 floors"
    );
}
