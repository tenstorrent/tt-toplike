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

use support::party::{self, CROSS4_TICKS, PARTY4_TICKS, SOLO_TICKS};
use tt_toplike_bus::{Payload, Record};
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::pack::Pack;
use tt_toplike_saga::rng::Pcg32;
use tt_toplike_saga::saga::plan_stream;
use tt_toplike_saga::{Label, Saga, SagaConfig, SagaEvent};

/// The note every fixture failure carries: how to write the files again.
const WRITERS: &str = "regenerate them with `cargo test -p tt-toplike-saga --test party -- --ignored write_solo_fixture write_party4_fixture write_cross4_fixture`";

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

/// The crossing log is committed too: `cross4.jsonl` is the output of
/// `records_with_links(4, CROSS4_TICKS)`, byte for byte.
#[test]
fn the_committed_cross_fixture_matches_the_generator() {
    let on_disk = std::fs::read_to_string(support::fixture_path("cross4.jsonl"))
        .unwrap_or_else(|e| panic!("cross4.jsonl is committed ({e}); {WRITERS}"));
    assert!(
        on_disk == party::file_text_with_links(4, CROSS4_TICKS),
        "cross4.jsonl differs from tests/support/party.rs; {WRITERS}"
    );
}

/// The crossing log keeps ledger ruling R6: every model is a `demo-*` name,
/// and every chip, including the chip inside an `eth:<chip>/<port>` key, is
/// a made-up PCI address. The test also checks that the log does carry
/// `LinkPeer` events, so the loop below has something to look at.
#[test]
fn the_cross_fixture_carries_no_real_names() {
    let mut peers = 0;
    for r in support::read_fixture("cross4.jsonl") {
        assert!(
            !matches!(&r.payload, Payload::Raw(_)),
            "cross4.jsonl: a raw frame at seq {}",
            r.seq
        );
        if matches!(
            &r.payload,
            Payload::Event(tt_toplike_bus::Event::LinkPeer { .. })
        ) {
            peers += 1;
        }
        let line = r.to_line();
        assert!(!line.contains("process:"), "a process key: {line}");
        for part in line.split('"') {
            if let Some(model) = part.strip_prefix("model:") {
                assert!(model.starts_with("demo-"), "model {part}");
            }
            if part.starts_with("chip:") {
                assert!(is_made_up_chip(part), "chip {part}");
            }
            if let Some(port) = part.strip_prefix("eth:") {
                let (chip, number) = port.split_once('/').expect("an eth key has a port");
                assert!(is_made_up_chip(chip), "eth key {part}");
                assert!(number.parse::<u8>().is_ok(), "eth key {part}");
            }
        }
    }
    // A ring of four chips has four links.
    assert_eq!(peers, 4, "cross4.jsonl holds one LinkPeer per ring link");
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

/// Writes `tests/fixtures/cross4.jsonl`. See [`write_solo_fixture`].
#[test]
#[ignore = "writes the cross4 fixture; run by hand after changing tests/support/party.rs"]
fn write_cross4_fixture() {
    std::fs::write(
        support::fixture_path("cross4.jsonl"),
        party::file_text_with_links(4, CROSS4_TICKS),
    )
    .expect("write cross4.jsonl");
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

// ---- The party in the core (Task A.3) -----------------------------------------

/// Feeds `records` until the first world is born, and returns the saga,
/// every event raised so far and the records not yet fed.
fn first_birth(records: &[Record]) -> (Saga, Vec<SagaEvent>, &[Record]) {
    let mut s = saga(NameMode::Real);
    let mut events = Vec::new();
    for (i, r) in records.iter().enumerate() {
        s.feed(r);
        events.extend(s.drain_events());
        if s.state().has_world() {
            return (s, events, &records[i + 1..]);
        }
    }
    panic!("no world was born");
}

#[test]
fn a_world_on_four_chips_has_four_members_each_on_its_floor() {
    let records = party::records(4, 60);
    let (mut s, events, rest) = first_birth(&records);
    let state = s.state();
    assert_eq!(state.world.chips.len(), 4);
    assert_eq!(state.party.len(), 4);
    // Member i is born on floor i, at that floor's gate, and plans its
    // first quest there.
    for (i, m) in state.party.iter().enumerate() {
        let floor = &state.world.floors[i];
        assert_eq!(m.hero.floor as usize, i);
        assert_eq!(m.hero.pos, floor.entrance, "member {i}");
        assert!(floor.cell(m.quest.goal.0, m.quest.goal.1).walkable());
        // Member 0's first quest line takes the birth tick's window, so the
        // others' first lines are left unwritten (Task A.4, the pace).
        assert_eq!(m.unwritten, u32::from(i > 0), "member {i}");
    }
    assert_eq!(state.lead().hero, state.party[0].hero);
    let on_2: Vec<usize> = state.on_floor(2).map(|(i, _)| i).collect();
    assert_eq!(on_2, [2]);

    // The birth's events: the world and its beat, the party's one line,
    // every HeroBorn in floor order with no beat (a party of more than one),
    // then each member's first quest. Only member 0's quest has its line:
    // it took the window, and the others' lines are left unwritten.
    let shape: Vec<String> = events
        .iter()
        .map(|e| match e {
            SagaEvent::WorldBorn { .. } => "W".to_string(),
            SagaEvent::HeroBorn { floor, .. } => format!("H{floor}"),
            SagaEvent::QuestStarted { floor, .. } => format!("Q{floor}"),
            SagaEvent::Beat { floor: None, .. } => "b".to_string(),
            SagaEvent::Beat { floor: Some(f), .. } => format!("b{f}"),
            other => panic!("unexpected at a birth: {other:?}"),
        })
        .collect();
    assert_eq!(
        shape,
        ["W", "b", "b", "H0", "H1", "H2", "H3", "Q0", "b0", "Q1", "Q2", "Q3"]
    );
    let born: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            SagaEvent::HeroBorn { name, .. } => Some(name),
            _ => None,
        })
        .collect();
    let names: Vec<&String> = s.state().party.iter().map(|m| &m.hero.name).collect();
    assert_eq!(born, names);

    // Each member walks its own floor and stays on it.
    for r in rest {
        s.feed(r);
        for (i, m) in s.state().party.iter().enumerate() {
            assert_eq!(m.hero.floor as usize, i);
            let floor = &s.state().world.floors[i];
            assert!(floor.cell(m.hero.pos.0, m.hero.pos.1).walkable());
        }
    }
    let moved = s
        .state()
        .party
        .iter()
        .enumerate()
        .filter(|(i, m)| m.hero.pos != s.state().world.floors[*i].entrance)
        .count();
    assert!(moved >= 3, "only {moved} members left their gate");
}

#[test]
fn a_party_of_32_has_32_names() {
    let records = party::records(32, 6);
    let (s, _, _) = first_birth(&records);
    let state = s.state();
    assert_eq!(state.party.len(), 32);
    let mut names: Vec<&str> = state.party.iter().map(|m| m.hero.name.as_str()).collect();
    // Each name comes from its own member's generator: member 0 on today's
    // plan stream, member i on stream 16 + i, unique against the members
    // before it.
    let mut taken: Vec<String> = Vec::new();
    for (i, m) in state.party.iter().enumerate() {
        let mut rng = Pcg32::new(state.world.seed, plan_stream(i));
        let want = Pack::builtin().hero_name_unique(&mut rng, &taken);
        assert_eq!(m.hero.name, want, "member {i}");
        taken.push(want);
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 32, "{names:?}");
}

#[test]
fn four_and_thirty_two_chips_give_the_same_digest_after_renumbering() {
    // A replay host renumbers and retimes every record (sequence numbers in
    // steps of 3). A party's saga reads neither, so the digest stays. 450
    // ticks reach every scripted event of the generator.
    for chips in [4u8, 32] {
        let records = party::records(chips, 450);
        let replayed = support::renumber(&records);
        assert!(records
            .iter()
            .zip(&replayed)
            .all(|(a, b)| a.seq != b.seq && a.t_mono_ns != b.t_mono_ns));
        assert!(replayed.windows(2).all(|w| w[1].seq == w[0].seq + 3));
        for names in [NameMode::Real, NameMode::Generated] {
            let digest = |records: &[Record]| {
                let mut s = saga(names);
                for r in records {
                    s.feed(r);
                }
                assert_eq!(s.state().party.len(), chips as usize);
                s.digest()
            };
            assert_eq!(
                digest(&records),
                digest(&replayed),
                "{chips} chips ({names:?}): a replay host's numbering changed the saga"
            );
        }
    }
}

// ---- The merged chronicle (Task A.4) ------------------------------------------

/// Ticks of the 32-chip log the pace tests read: long enough for a world to
/// live past a minute (600 ticks) of story.
const PACE_TICKS: u64 = 1300;

/// One record's worth of story: the tick after the record and the events it
/// raised.
type Step = (u64, Vec<SagaEvent>);

/// Feeds every record of `records` to a saga in `names` mode and returns
/// the saga and, for each record, the tick and the events it raised.
fn run_log(records: &[Record], names: NameMode) -> (Saga, Vec<Step>) {
    let mut s = saga(names);
    let mut steps = Vec::with_capacity(records.len());
    for r in records {
        s.feed(r);
        let events = s.drain_events();
        if !events.is_empty() {
            steps.push((s.state().tick, events));
        }
    }
    (s, steps)
}

/// The steps of the 32-chip log of [`PACE_TICKS`] ticks, fed in real-name
/// mode, built once per test process.
fn thirty_two() -> &'static [Step] {
    use std::sync::OnceLock;
    static RUN: OnceLock<Vec<Step>> = OnceLock::new();
    RUN.get_or_init(|| {
        let (s, steps) = run_log(&party::records(32, PACE_TICKS), NameMode::Real);
        assert_eq!(s.state().party.len(), 32);
        steps
    })
}

/// A member's line in a party is a routine line: its quest started, its
/// quest done (a party writes no hero's birth line). A world's line has no
/// floor.
fn routine_floor(e: &SagaEvent) -> Option<u8> {
    match e {
        SagaEvent::Beat { floor, .. } => *floor,
        _ => None,
    }
}

#[test]
fn routine_lines_come_from_one_floor_per_window() {
    use std::collections::{BTreeMap, BTreeSet};
    use tt_toplike_saga::saga::ROUTINE_WINDOW_TICKS;
    assert_eq!(ROUTINE_WINDOW_TICKS, 10);
    let steps = thirty_two();
    // Windows are keyed by the world and the window: a world's birth opens
    // the window again, so a window a birth falls in can hold the old
    // world's owner before the birth and a new member after it.
    let mut windows: BTreeMap<(u32, u64), BTreeSet<u8>> = BTreeMap::new();
    let (mut quests, mut lines, mut world) = (0, 0, 0u32);
    for (tick, events) in steps {
        quests += events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SagaEvent::QuestStarted { .. } | SagaEvent::QuestDone { .. }
                )
            })
            .count();
        for e in events {
            if let SagaEvent::WorldBorn { .. } = e {
                world += 1;
            }
            if let Some(f) = routine_floor(e) {
                lines += 1;
                windows
                    .entry((world, tick / ROUTINE_WINDOW_TICKS))
                    .or_default()
                    .insert(f);
            }
        }
    }
    for ((w, k), floors) in &windows {
        assert_eq!(
            floors.len(),
            1,
            "world {w}, window {k} (ticks {} to {}) has lines from floors {floors:?}",
            k * 10,
            k * 10 + 9
        );
    }
    // The check is not empty: many windows, many floors, and lines held
    // back from the chronicle.
    let heard: BTreeSet<u8> = windows.values().flatten().copied().collect();
    assert!(windows.len() >= 60, "only {} windows", windows.len());
    assert!(heard.len() >= 16, "only floors {heard:?} heard");
    assert!(
        quests > lines + 100,
        "{quests} quests started or done, {lines} lines"
    );
}

#[test]
fn every_floor_is_heard_within_a_minute() {
    // In each world, from its birth, every floor writes a routine line at
    // least once in every 600 ticks (one minute at 10 Hz).
    const MINUTE: u64 = 600;
    let steps = thirty_two();
    let mut last: Vec<u64> = Vec::new();
    let mut longest_world = 0;
    let mut born = 0;
    let check = |tick: u64, last: &[u64]| {
        for (f, &l) in last.iter().enumerate() {
            assert!(
                tick - l <= MINUTE,
                "floor {f} silent from tick {l} to tick {tick}"
            );
        }
    };
    for (tick, events) in steps {
        check(*tick, &last);
        for e in events {
            if let SagaEvent::WorldBorn { .. } = e {
                longest_world = longest_world.max(tick - born);
                born = *tick;
                last = vec![*tick; 32];
            }
            if let Some(f) = routine_floor(e) {
                last[f as usize] = *tick;
            }
        }
    }
    check(PACE_TICKS, &last);
    longest_world = longest_world.max(PACE_TICKS - born);
    assert!(
        longest_world > MINUTE,
        "no world lived past a minute ({longest_world} ticks)"
    );
}

/// The longest stretch of ticks with no chronicle line on the 32-chip log,
/// from the first world's birth to the log's end. The pace holds routine
/// lines back, and this pins how far. Measured at 26 ticks (party Task
/// A.8); the bound of 32 leaves about 25% for a later change to the pace or
/// the templates. A pace that gives open windows only to overdue members
/// turns this red.
const LONGEST_CHRONICLE_GAP: u64 = 32;

#[test]
fn the_chronicle_is_never_silent_for_long_with_32_chips() {
    let steps = thirty_two();
    let mut last: Option<u64> = None;
    let mut longest = 0;
    for (tick, events) in steps {
        for e in events {
            if let SagaEvent::Beat { .. } = e {
                if let Some(l) = last {
                    longest = longest.max(tick - l);
                }
                last = Some(*tick);
            }
        }
    }
    let last = last.expect("the 32-chip log writes lines");
    longest = longest.max(PACE_TICKS - last);
    println!("longest chronicle gap on the 32-chip log: {longest} ticks");
    assert!(
        longest <= LONGEST_CHRONICLE_GAP,
        "the chronicle was silent for {longest} ticks"
    );
}

#[test]
fn a_one_member_party_never_drops_a_line() {
    // One floor owns every window, so a one-chip story is today's story:
    // every quest started or done has its line, and nothing is unwritten.
    let records = support::read_fixture("solo.jsonl");
    for names in [NameMode::Real, NameMode::Generated] {
        let mut s = saga(names);
        let (mut quests, mut lines) = (0, 0);
        for r in &records {
            s.feed(r);
            assert_eq!(s.state().party.len(), 1);
            assert_eq!(s.state().party[0].unwritten, 0, "{names:?}");
            let events = s.drain_events();
            for w in events.windows(2) {
                if matches!(
                    w[0],
                    SagaEvent::QuestStarted { .. } | SagaEvent::QuestDone { .. }
                ) {
                    quests += 1;
                    lines += matches!(w[1], SagaEvent::Beat { .. }) as usize;
                }
            }
        }
        assert!(quests > 20, "{quests} quests");
        assert_eq!(lines, quests, "{names:?}");
    }
}

/// The model names in the party logs.
const FIXTURE_NAMES: [&str; 2] = ["demo-sage", "demo-scribe"];

/// Everything a party saga shows of names over `records` in `names` mode:
/// every drained event, every chronicle line and cause, and every member's
/// target label after every record.
fn everything_shown(records: &[Record], names: NameMode) -> String {
    let mut s = saga(names);
    let mut out = String::new();
    for r in records {
        s.feed(r);
        for e in s.drain_events() {
            out.push_str(&format!("{e:?}\n"));
        }
        for m in &s.state().party {
            if let Some(label) = &m.quest.target_label {
                out.push_str(label);
                out.push('\n');
            }
        }
    }
    for b in s.state().chronicle.lines(usize::MAX) {
        out.push_str(&format!("{} {:?}\n", b.text, b.cause));
    }
    out
}

#[test]
fn generated_mode_leaves_no_fixture_name_anywhere_in_a_party() {
    let logs = [
        ("party4.jsonl", support::read_fixture("party4.jsonl")),
        ("32 chips", party::records(32, 450)),
    ];
    for (what, records) in &logs {
        let shown = everything_shown(records, NameMode::Generated);
        for name in FIXTURE_NAMES {
            assert!(!shown.contains(name), "{what}: generated mode shows {name}");
        }
        // The control: real mode shows each fixture name in the same
        // places, labels included, so the search above can find either one.
        let real = everything_shown(records, NameMode::Real);
        for name in FIXTURE_NAMES {
            assert!(
                real.contains(&format!("\n{name}\n")),
                "{what}: no real label {name}"
            );
            assert!(
                real.contains(&format!("model:{name}")),
                "{what}: no real cause {name}"
            );
        }
    }
}

/// `SagaState::chips_seen` counts the chips the stream has shown, capped at
/// `MAX_CHIPS` (64): four for the four-chip log, forty for a forty-chip
/// stream whose world keeps 32 floors, 64 for seventy chips. Views read it
/// to say how many chips have no floor (party Task A.7, fix round 1). The
/// digest does not read it: the digest goldens in `determinism.rs` and the
/// solo story pins did not move when it was added.
#[test]
fn chips_seen_counts_the_streams_chips() {
    let mut s = saga(NameMode::Real);
    assert_eq!(s.state().chips_seen, 0, "no chip before any record");
    for r in party::records(4, 60) {
        s.feed(&r);
    }
    assert_eq!(s.state().chips_seen, 4);
    for (chips, want, floors) in [(40u8, 40usize, 32usize), (70, 64, 32)] {
        let mut s = saga(NameMode::Real);
        for r in party::records_with_extra_chips(chips, 60) {
            s.feed(&r);
        }
        assert_eq!(s.state().chips_seen, want, "{chips} chips");
        assert_eq!(s.state().world.chips.len(), floors, "{chips} chips");
    }
}

/// The ring of `cross4.jsonl` as (floor, port, far floor, far port): chip
/// `i`'s highest live port to chip `i + 1`'s port 0. Chip `i`'s mask has
/// `2 + i` low bits, so its highest port is `1 + i`.
const CROSS4_RING: [(u8, u8, u8, u8); 4] = [(0, 1, 1, 0), (1, 2, 2, 0), (2, 3, 3, 0), (3, 4, 0, 0)];

/// Whether ring link `k` of `cross4.jsonl` is up once tick `tick` is over.
/// Link 1 (chip 1's port 2) is down from tick 400 to tick 429. At tick 600
/// chip 0's mask becomes 0x30 (ports 4 and 5): both of chip 0's ring ports,
/// 0 and 1, go dark with no new peer record (review finding 6 of B.1/B.2),
/// so links 0 and 3 close for good.
fn cross4_link_up(k: usize, tick: u64) -> bool {
    match k {
        1 => !(400..430).contains(&tick),
        0 | 3 => tick < 600,
        _ => true,
    }
}

#[test]
fn the_saga_keeps_the_cross_logs_link_table() {
    // After each tick, every ring portal is bound to the right far floor
    // and port, and is open exactly while both of its ends are live.
    let records = party::records_with_links(4, CROSS4_TICKS);
    // A long season, so the quiet stretch after tick 600 births no dream.
    // A world born then would take its floors from chip 0's new mask
    // (ports 4 and 5), and its floor 0 would have no portal on ports 0 and
    // 1, which leaves links 0 and 3 unbound (checked at the end).
    let mut long = Saga::new(SagaConfig {
        season_ticks: 12_000,
        names: NameMode::Real,
        label: Label::Mock,
    });
    let mut last = 0;
    let mut checked = 0;
    for r in &records {
        long.feed(r);
        let tick = long.state().tick;
        if tick == last {
            continue;
        }
        // Tick `last` is over: every one of its records has been fed. The
        // record just fed is the next tick's pacemaker sweep, which says
        // nothing about links.
        let done = last;
        last = tick;
        if !long.state().has_world() {
            continue;
        }
        let world = &long.state().world;
        let links = long.links();
        for (k, &(floor, port, far, far_port)) in CROSS4_RING.iter().enumerate() {
            assert_eq!(
                links.bound(world, floor, port),
                Some((far, far_port)),
                "tick {done}"
            );
            assert_eq!(
                links.bound(world, far, far_port),
                Some((floor, port)),
                "tick {done}"
            );
            let want = cross4_link_up(k, done).then_some((far, far_port));
            assert_eq!(
                links.open(world, floor, port),
                want,
                "tick {done}, link {k}"
            );
            let back = cross4_link_up(k, done).then_some((floor, port));
            assert_eq!(
                links.open(world, far, far_port),
                back,
                "tick {done}, link {k} back"
            );
        }
        checked += 1;
    }
    assert!(checked > 700, "only {checked} ticks were checked");

    // With the 200-tick season of the determinism tests, a dream is born
    // in the quiet stretch, after chip 0's mask lost ports 0 and 1: its
    // floor 0 has portals on ports 4 and 5 only, so links 0 and 3 are no
    // longer bound, and links 1 and 2 still are.
    let mut short = saga(NameMode::Real);
    for r in &records {
        short.feed(r);
    }
    let world = &short.state().world;
    assert!(
        world.born_tick > 600,
        "a world was born after the mask change"
    );
    let ports: Vec<u8> = world.floors[0].portals.iter().map(|&(p, _)| p).collect();
    assert_eq!(ports, [4, 5]);
    let links = short.links();
    assert_eq!(links.bound(world, 0, 1), None);
    assert_eq!(links.bound(world, 3, 4), None);
    assert_eq!(links.open(world, 1, 2), Some((2, 0)));
    assert_eq!(links.open(world, 2, 3), Some((3, 0)));
}

#[test]
fn the_logs_without_peers_bind_no_portal() {
    // The party logs hold no `LinkPeer`, so no portal of any of their
    // floors is bound, live mask or not.
    for (chips, ticks) in [(1u8, 300u64), (4, 300)] {
        let records = party::records(chips, ticks);
        let mut saga = saga(NameMode::Real);
        for r in &records {
            saga.feed(r);
        }
        let world = &saga.state().world;
        for floor in 0..world.floors.len() as u8 {
            for &(port, _) in &world.floors[floor as usize].portals {
                assert_eq!(saga.links().bound(world, floor, port), None);
            }
        }
    }
}

// ---- Crossing (Task B.4) --------------------------------------------------------

/// The crossing events of a run: `(tick, event)` for every `HeroCrossed`
/// and `CrossingFailed`.
fn crossings(steps: &[Step]) -> Vec<(u64, SagaEvent)> {
    steps
        .iter()
        .flat_map(|(tick, events)| events.iter().map(move |e| (*tick, e)))
        .filter(|(_, e)| {
            matches!(
                e,
                SagaEvent::HeroCrossed { .. } | SagaEvent::CrossingFailed { .. }
            )
        })
        .map(|(t, e)| (t, e.clone()))
        .collect()
}

#[test]
fn the_cross_log_gives_the_same_digest_after_renumbering() {
    // A replay host renumbers and retimes every record. The crossing reads
    // neither, so the digest of the crossing log stays, in both modes. The
    // log must really cross, or the test would check nothing new.
    let records = support::read_fixture("cross4.jsonl");
    let replayed = support::renumber(&records);
    assert!(records
        .iter()
        .zip(&replayed)
        .all(|(a, b)| a.seq != b.seq && a.t_mono_ns != b.t_mono_ns));
    for names in [NameMode::Real, NameMode::Generated] {
        let (a, steps) = run_log(&records, names);
        let (b, _) = run_log(&replayed, names);
        assert_eq!(
            a.digest(),
            b.digest(),
            "{names:?}: a replay host's numbering changed the crossing saga"
        );
        let crossed = crossings(&steps);
        assert!(
            crossed
                .iter()
                .any(|(_, e)| matches!(e, SagaEvent::HeroCrossed { .. })),
            "{names:?}: no hero crossed in the cross log"
        );
    }
}

#[test]
fn the_cross_log_crosses_only_open_ring_links() {
    // Every crossing in the cross log goes through a ring link, from its
    // portal to the far end's portal, while the link is up.
    let records = party::records_with_links(4, CROSS4_TICKS);
    let (_, steps) = run_log(&records, NameMode::Real);
    let crossed = crossings(&steps);
    assert!(!crossed.is_empty());
    for (tick, e) in &crossed {
        if let SagaEvent::HeroCrossed {
            from_floor,
            from_port,
            to_floor,
            to_port,
            ..
        } = e
        {
            let k = CROSS4_RING
                .iter()
                .position(|&(f, p, g, q)| {
                    (f, p, g, q) == (*from_floor, *from_port, *to_floor, *to_port)
                        || (g, q, f, p) == (*from_floor, *from_port, *to_floor, *to_port)
                })
                .unwrap_or_else(|| panic!("tick {tick}: {e:?} is not a ring link"));
            assert!(cross4_link_up(k, *tick), "tick {tick}: link {k} is down");
        }
    }
}

#[test]
fn generated_mode_leaves_no_fixture_name_in_the_cross_log() {
    // Crossing hooks take a model's `ModelReady` as their reason: in
    // generated mode the crossing's event and line carry its alias.
    let records = support::read_fixture("cross4.jsonl");
    let shown = everything_shown(&records, NameMode::Generated);
    for name in ["demo-sage", "demo-scribe", "demo-pair"] {
        assert!(!shown.contains(name), "generated mode shows {name}");
    }
    // The control: in real mode a crossing's event names the pair's model.
    let real = everything_shown(&records, NameMode::Real);
    assert!(
        real.lines()
            .any(|l| l.starts_with("HeroCrossed") && l.contains("model:demo-pair")),
        "no crossing for demo-pair in real mode"
    );
}

// ---- Portals for the view (Task B.5) ----------------------------------------------

#[test]
fn the_states_portals_mirror_the_link_table() {
    // `SagaState::portals` lists every portal of the world, floor by floor
    // and west to east, with its far end when bound and whether it is open,
    // as the link table says after every record. A view reads it, so it
    // must never lag the table, and it holds one entry per portal, so it is
    // bounded by the floors' portals (at most 16 on each of 32 floors).
    let records = support::read_fixture("cross4.jsonl");
    let mut s = saga(NameMode::Real);
    let (mut closed_seen, mut open_seen) = (false, false);
    // Worlds that ended while some portal was bound: the next world's list
    // must be its own floors' portals, checked below like every other.
    let (mut world, mut ended_bound) = (0, 0);
    let mut bound_before = false;
    for r in &records {
        s.feed(r);
        let state = s.state();
        if state.world.id != world {
            world = state.world.id;
            ended_bound += usize::from(bound_before);
        }
        let world = &state.world;
        let mut want = Vec::new();
        if state.has_world() {
            for (f, floor) in world.floors.iter().enumerate() {
                let f = f as u8;
                for &(port, _) in &floor.portals {
                    let far = s.links().bound(world, f, port);
                    let open = s.links().open(world, f, port).is_some();
                    want.push(tt_toplike_saga::PortalState {
                        floor: f,
                        port,
                        far,
                        open,
                    });
                }
            }
        }
        assert_eq!(state.portals, want, "tick {}", state.tick);
        assert!(state.portals.len() <= 32 * 16);
        closed_seen |= state.portals.iter().any(|p| p.far.is_some() && !p.open);
        open_seen |= state.portals.iter().any(|p| p.open);
        bound_before = state.portals.iter().any(|p| p.far.is_some());
    }
    assert!(
        open_seen && closed_seen,
        "the log shows open and closed portals"
    );
    assert!(
        ended_bound >= 2,
        "{ended_bound} worlds ended with bound portals"
    );
    // The log's last world is born after chip 0's mask lost ports 0 and 1:
    // its floor 1 lists ports 4 and 5, and they lead nowhere.
    let state = s.state();
    let floor0: Vec<(u8, Option<(u8, u8)>)> = state
        .portals
        .iter()
        .filter(|p| p.floor == 0)
        .map(|p| (p.port, p.far))
        .collect();
    assert_eq!(floor0, [(4, None), (5, None)]);
    // The party logs have no peers: every portal is listed, none is bound.
    let records = support::read_fixture("party4.jsonl");
    let mut s = saga(NameMode::Real);
    for r in &records {
        s.feed(r);
    }
    let state = s.state();
    let count: usize = state.world.floors.iter().map(|f| f.portals.len()).sum();
    assert_eq!(state.portals.len(), count);
    assert!(state.portals.iter().all(|p| p.far.is_none() && !p.open));
}
