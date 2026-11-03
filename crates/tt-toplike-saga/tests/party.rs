//! Party tests: the generator behind the party fixtures, the fixtures
//! themselves, and the pins of today's single-chip story.
//!
//! `solo.jsonl` (one chip, 1,200 ticks) and `party4.jsonl` (four chips, 600
//! ticks) are written by `tests/support/party.rs`, with made-up model names
//! and made-up PCI addresses (ledger ruling R6). Later tasks of the party
//! plan add their tests to this file.
//!
//! The story pins were taken on the single-chip code before the party
//! (0.16.4). One chip must stay today's saga through the whole of Phase A,
//! so `solo_story_print_is_unchanged` fails if the party work changes what
//! a lone chip tells.

mod support;

use support::party::{self, PARTY4_TICKS, SOLO_TICKS};
use tt_toplike_bus::{Payload, Record};
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::{Label, Saga, SagaConfig};

/// The note every fixture failure carries: how to write the files again.
const WRITERS: &str = "regenerate them with `cargo test -p tt-toplike-saga --test party -- --ignored write_solo_fixture write_party4_fixture`";

/// A saga as the determinism tests build it: a 200-tick season.
fn saga(names: NameMode) -> Saga {
    Saga::new(SagaConfig {
        season_ticks: 200,
        names,
        label: Label::Mock,
    })
}

#[test]
fn the_party_generator_is_deterministic() {
    for chips in [1u8, 2, 4, 32] {
        // 450 ticks reach every scripted event but keep 32 chips quick.
        assert!(
            party::records(chips, 450) == party::records(chips, 450),
            "{chips} chips gave two different logs"
        );
    }
}

#[test]
fn the_committed_party_fixtures_match_the_generator() {
    for (name, chips, ticks) in [
        ("solo.jsonl", 1u8, SOLO_TICKS),
        ("party4.jsonl", 4, PARTY4_TICKS),
    ] {
        let on_disk = std::fs::read_to_string(support::fixture_path(name))
            .unwrap_or_else(|e| panic!("{name} is committed ({e}); {WRITERS}"));
        assert!(
            on_disk == party::file_text(chips, ticks),
            "{name} differs from tests/support/party.rs; {WRITERS}"
        );
    }
}

/// True when `key` is `chip:0000:XX:00.0` with XX two lowercase hex digits.
fn is_made_up_chip(key: &str) -> bool {
    let b = key.as_bytes();
    key.len() == 17
        && key.starts_with("chip:0000:")
        && key.ends_with(":00.0")
        && b[10..12]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

#[test]
fn the_party_fixtures_carry_no_real_names() {
    for name in ["solo.jsonl", "party4.jsonl"] {
        for r in support::read_fixture(name) {
            // A process sample or event would carry a real command name.
            assert!(
                !matches!(&r.payload, Payload::Raw(_)),
                "{name}: a raw frame at seq {}",
                r.seq
            );
            let line = r.to_line();
            assert!(!line.contains("process:"), "{name}: a process key: {line}");
            // Every key that names a model or a chip, wherever it sits.
            for part in line.split('"') {
                if let Some(model) = part.strip_prefix("model:") {
                    assert!(model.starts_with("demo-"), "{name}: model {part}");
                }
                if part.starts_with("chip:") {
                    assert!(is_made_up_chip(part), "{name}: chip {part}");
                }
            }
        }
    }
}

#[test]
fn the_committed_fixtures_are_under_a_megabyte() {
    for name in ["solo.jsonl", "party4.jsonl"] {
        let bytes = std::fs::metadata(support::fixture_path(name))
            .unwrap_or_else(|e| panic!("{name} is committed ({e}); {WRITERS}"))
            .len();
        assert!(bytes < 1_000_000, "{name} is {bytes} bytes");
    }
}

/// The chip keys follow `chip:0000:{i+1:02x}:00.0`.
#[test]
fn chip_keys_follow_the_made_up_pattern() {
    assert_eq!(party::chip(0).as_str(), "chip:0000:01:00.0");
    assert_eq!(party::chip(9).as_str(), "chip:0000:0a:00.0");
    assert_eq!(party::chip(31).as_str(), "chip:0000:20:00.0");
    assert!((0..32).all(|i| is_made_up_chip(party::chip(i).as_str())));
}

/// Writes `tests/fixtures/solo.jsonl`. Run by hand after changing the
/// generator; `the_committed_party_fixtures_match_the_generator` then checks
/// the result.
#[test]
#[ignore = "writes the solo fixture; run by hand after changing tests/support/party.rs"]
fn write_solo_fixture() {
    std::fs::write(
        support::fixture_path("solo.jsonl"),
        party::file_text(1, SOLO_TICKS),
    )
    .expect("write solo.jsonl");
}

/// Writes `tests/fixtures/party4.jsonl`. See [`write_solo_fixture`].
#[test]
#[ignore = "writes the party4 fixture; run by hand after changing tests/support/party.rs"]
fn write_party4_fixture() {
    std::fs::write(
        support::fixture_path("party4.jsonl"),
        party::file_text(4, PARTY4_TICKS),
    )
    .expect("write party4.jsonl");
}

// ---- Pins taken on the single-chip code ---------------------------------------

/// The story print and the position trace of the whole solo log, in both
/// name modes. The trace follows the world's real names and the print the
/// chronicle's text, so each mode has its own print.
fn solo_story() -> [u64; 3] {
    let records: Vec<Record> = support::read_fixture("solo.jsonl");
    let print = |names| {
        let mut s = saga(names);
        for r in &records {
            s.feed(r);
        }
        // A pin over an empty chronicle would pin nothing.
        assert!(s.state().chronicle.len() > 10, "the solo log tells a story");
        support::story_print(&s)
    };
    [
        print(NameMode::Real),
        print(NameMode::Generated),
        support::story_trace(&records),
    ]
}

/// Today's single-chip story, taken on the code before the party: the
/// chronicle's print in real-name mode and in generated-name mode, then the
/// hero's per-tick position trace.
const SOLO_STORY: [u64; 3] = [0x7ab6701b6a3d2265, 0x89a75bc5cc77eb57, 0x3cab350b81b8c5a6];

/// Prints the values for [`SOLO_STORY`].
#[test]
#[ignore = "prints the values to pin"]
fn print_solo_story() {
    println!("{:#018x?}", solo_story());
}

#[test]
fn solo_story_print_is_unchanged() {
    assert_eq!(
        solo_story(),
        SOLO_STORY,
        "the single-chip story changed; if that is intended, say which rule moved"
    );
}
