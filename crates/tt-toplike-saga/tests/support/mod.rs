//! Helpers shared by the saga crate's integration tests.
//!
//! * [`busy`] is the deterministic generator behind `tests/fixtures/busy.jsonl`
//!   (ledger ruling R6: a committed fixture carries no real process or host
//!   names, so the busy log comes from a script).
//! * [`read_fixture`] reads one of the committed fixtures with the bus crate's
//!   segment reader, which skips the header line.
//! * [`renumber`] gives a log new sequence numbers and new times, the way a
//!   replay host does.

#![allow(dead_code)]

pub mod busy;

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
/// from 90,001, monotonic time from its own start in steps of 3.3 ms, and a
/// wall clock a year later that ticks at a different rate. Contents and order
/// stay as they were.
pub fn renumber(records: &[Record]) -> Vec<Record> {
    records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let i = i as u64;
            Record {
                seq: 90_001 + i,
                t_mono_ns: 1_000 + i * 3_333_333,
                t_wall_ms: 1_821_000_000_000 + i * 7,
                ..r.clone()
            }
        })
        .collect()
}
