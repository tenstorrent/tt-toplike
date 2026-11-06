//! Helpers shared by the saga crate's integration tests.
//!
//! * [`busy`] is the deterministic generator behind `tests/fixtures/busy.jsonl`
//!   (ledger ruling R6: a committed fixture carries no real process or host
//!   names, so the busy log comes from a script).
//! * [`read_fixture`] reads one of the committed fixtures with the bus crate's
//!   segment reader, which skips the header line.
//! * [`party`] is the generator behind `solo.jsonl` and `party4.jsonl` and
//!   every in-memory party of the party tests.
//! * [`renumber`] gives a log new sequence numbers and new times, the way a
//!   replay host does.
//! * [`fuzz`] holds the seeded random streams and the adversarial flood of
//!   the fuzz and bounds tests.

#![allow(dead_code)]

pub mod busy;
pub mod fuzz;
pub mod party;

use std::path::PathBuf;

use tt_toplike_bus::segment::read_segment;
use tt_toplike_bus::Record;

/// The path of `tests/fixtures/<name>`.
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Every record of `tests/fixtures/<name>`, in file order. Panics when the
/// file is missing or has a line that does not decode, so a damaged fixture
/// fails loudly.
pub fn read_fixture(name: &str) -> Vec<Record> {
    let path = fixture_path(name);
    let read =
        read_segment(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    assert_eq!(
        read.bad_lines,
        0,
        "{} has lines that do not decode",
        path.display()
    );
    assert!(
        !read.records.is_empty(),
        "{} holds no records",
        path.display()
    );
    read.records
}

/// The same records with every `seq`, `t_mono_ns` and `t_wall_ms` replaced,
/// as a replay host does when it republishes a log: its own sequence counter
/// from 90,001 in steps of 3 (a host that shares its counter with other
/// writers leaves holes), monotonic time from its own start in steps of 3.3 ms, and a
/// wall clock a year later that ticks at a different rate. Contents and order
/// stay as they were.
pub fn renumber(records: &[Record]) -> Vec<Record> {
    records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let i = i as u64;
            Record {
                seq: 90_001 + i * 3,
                t_mono_ns: 1_000 + i * 3_333_333,
                t_wall_ms: 1_821_000_000_000 + i * 7,
                ..r.clone()
            }
        })
        .collect()
}

/// FNV-1a, 64 bit, one byte at a time.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

/// A hash of the saga's chronicle: for each line, oldest first, its tone
/// code (calm 0, tense 1, triumphant 2) and its text. It reads only public
/// state. Only the lines still in the ring count, so a log long enough to
/// push lines out hashes the newest 500.
pub fn story_print(saga: &tt_toplike_saga::Saga) -> u64 {
    use tt_toplike_saga::chronicle::Tone;
    let chronicle = &saga.state().chronicle;
    let mut h = FNV_START;
    for beat in chronicle.lines(chronicle.len()) {
        let tone = match beat.tone {
            Tone::Calm => 0u8,
            Tone::Tense => 1,
            Tone::Triumphant => 2,
        };
        fnv(&mut h, &[tone]);
        fnv(&mut h, beat.text.as_bytes());
        // A separator, so two lines cannot trade characters unseen.
        fnv(&mut h, &[0xff]);
    }
    h
}

/// A hash of where the lead hero stood after every tick. It feeds every
/// record to a fresh saga (real names, mock label, 200-tick season) and,
/// each time the tick counter moves, folds the hero's floor, x and y into
/// the hash. It reads only public state. Task A.3 changed a single accessor
/// line below (`state.hero` to `state.party[0].hero`) and nothing else.
pub fn story_trace(records: &[Record]) -> u64 {
    use tt_toplike_saga::chronicle::NameMode;
    use tt_toplike_saga::{Label, Saga, SagaConfig};
    let mut saga = Saga::new(SagaConfig {
        season_ticks: 200,
        names: NameMode::Real,
        label: Label::Mock,
    });
    let mut h = FNV_START;
    let mut seen_tick = saga.state().tick;
    for r in records {
        saga.feed(r);
        let state = saga.state();
        if state.tick != seen_tick {
            seen_tick = state.tick;
            let hero = &state.party[0].hero;
            fnv(&mut h, &[hero.floor, hero.pos.0, hero.pos.1]);
        }
    }
    h
}
