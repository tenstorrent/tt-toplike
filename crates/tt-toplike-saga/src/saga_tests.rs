//! Tests for the front door. Each one feeds hand-built records; none reads a
//! clock or a file. The determinism tests over the recorded fixtures live in
//! `tests/determinism.rs`.

use super::*;
use crate::chronicle::{Beat, Tone};
use crate::hero::{AI_CLOCK, TEMPERATURE_BASELINE};
use crate::maze::Cell;
use tt_toplike_bus::{Provenance, Sample, Sweep, Value};

/// The pacemaker chip of these tests.
fn chip_a() -> EntityKey {
    EntityKey::chip("0000:01:00.0")
}

/// A second chip, sorting after chip A.
fn chip_b() -> EntityKey {
    EntityKey::chip("0000:02:00.0")
}

/// A record with `payload` and made-up numbers (the saga never reads them).
fn rec(payload: Payload) -> Record {
    Record {
        seq: 0,
        t_mono_ns: 0,
        t_wall_ms: 0,
        source: "backend".to_string(),
        payload,
    }
}

/// A sweep of `chip` with a temperature and a clock.
fn sweep(chip: &EntityKey, temp: f64, clock: u64) -> Record {
    rec(Payload::Sweep(Sweep {
        entity: chip.clone(),
        signals: vec![ASIC_TEMPERATURE.to_string(), AI_CLOCK.to_string()],
        values: vec![Value::F64(temp), Value::U64(clock)],
        prov: Provenance::Estimated,
    }))
}

/// One sample.
fn sample(entity: &EntityKey, signal: &str, value: Value) -> Record {
    rec(Payload::Sample(Sample {
        entity: entity.clone(),
        signal: signal.to_string(),
        value,
        prov: Provenance::Estimated,
    }))
}

/// One event.
fn event(e: Event) -> Record {
    rec(Payload::Event(e))
}

fn config(names: NameMode, label: Label) -> SagaConfig {
    SagaConfig {
        season_ticks: 12_000,
        names,
        label,
    }
}

/// Feeds `ticks` sweeps of chip A with a wobbling temperature. The wobble
/// fills the seed pool, so a world is born by the fifth tick.
fn run_ticks(saga: &mut Saga, ticks: u64) {
    for _ in 0..ticks {
        let t = saga.state().tick as f64;
        saga.feed(&sweep(&chip_a(), 45.0 + t * 0.013, 1350));
    }
}

#[test]
fn the_mock_label_shows_in_the_state() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    assert_eq!(saga.state().label, Label::Mock);
    run_ticks(&mut saga, 6);
    let state = saga.state();
    assert_eq!(state.label, Label::Mock);
    let first = state
        .chronicle
        .lines(CAPACITY)
        .next()
        .expect("a first line")
        .clone();
    assert!(
        first.text.ends_with(" (mock)"),
        "first line: {}",
        first.text
    );
    // Only the first line carries the mark.
    assert!(state.chronicle.len() > 1);
    assert!(state
        .chronicle
        .lines(CAPACITY)
        .skip(1)
        .all(|b| !b.text.contains("(mock)")));
    // The published beat carries the mark too.
    let events = saga.drain_events();
    let first_beat = events.iter().find_map(|e| match e {
        SagaEvent::Beat { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(first_beat.as_deref(), Some(first.text.as_str()));
}

#[test]
fn a_replay_is_marked_and_a_live_saga_is_not() {
    for (label, mark) in [(Label::Replay, Some("(replay)")), (Label::Live, None)] {
        let mut saga = Saga::new(config(NameMode::Real, label));
        run_ticks(&mut saga, 6);
        let first = saga
            .state()
            .chronicle
            .lines(CAPACITY)
            .next()
            .unwrap()
            .text
            .clone();
        match mark {
            Some(m) => assert!(first.ends_with(m), "{label:?}: {first}"),
            None => assert!(!first.contains("(mock)") && !first.contains("(replay)")),
        }
    }
}

#[test]
fn the_heartbeat_series_is_pushed_each_tick_and_stalls_after_twenty() {
    // Ruling R15: 1.0 per tick while the heartbeat advanced within the last
    // 20 ticks, else 0.0.
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    run_ticks(&mut saga, 1);
    // The heartbeat changes once, at tick 1, and never again.
    saga.feed(&sample(&chip_a(), HEARTBEAT, Value::U64(7)));
    // Ticks 2 to 20: the change at tick 1 is within the last 20 ticks.
    run_ticks(&mut saga, 19);
    let trail = *saga.trails.get(&chip_a(), HEARTBEAT_ALIVE).expect("series");
    assert_eq!(trail.count(), 19);
    assert_eq!(trail.mean(), 1.0);
    // Tick 21 is 20 ticks after the change: stalled.
    run_ticks(&mut saga, 1);
    let trail = *saga.trails.get(&chip_a(), HEARTBEAT_ALIVE).unwrap();
    assert_eq!(trail.count(), 20);
    assert!(
        trail.mean() < 1.0,
        "a stalled heartbeat still counts as alive"
    );
    // The same value again is not an advance.
    saga.feed(&sample(&chip_a(), HEARTBEAT, Value::U64(7)));
    run_ticks(&mut saga, 1);
    let stalled = saga.trails.get(&chip_a(), HEARTBEAT_ALIVE).unwrap().mean();
    assert!(stalled < trail.mean());
    // A new value is, and the next tick pushes 1.0 again.
    saga.feed(&sample(&chip_a(), HEARTBEAT, Value::U64(8)));
    run_ticks(&mut saga, 1);
    let restarted = saga.trails.get(&chip_a(), HEARTBEAT_ALIVE).unwrap().mean();
    assert!(restarted > stalled);
    // A chip that never reported a heartbeat gets no series.
    assert!(saga.trails.get(&chip_b(), HEARTBEAT_ALIVE).is_none());
}

#[test]
fn every_temperature_goes_through_push_temperature() {
    // Ruling R15 (amended): the baseline series must exist, one push per
    // temperature sample.
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    run_ticks(&mut saga, 12);
    saga.feed(&sample(&chip_b(), ASIC_TEMPERATURE, Value::F64(50.25)));
    for chip in [chip_a(), chip_b()] {
        let fast = saga
            .trails
            .get(&chip, ASIC_TEMPERATURE)
            .expect("temperature trail");
        let base = saga
            .trails
            .get(&chip, TEMPERATURE_BASELINE)
            .expect("baseline trail");
        assert_eq!(fast.count(), base.count(), "{chip}");
    }
    assert_eq!(
        saga.trails
            .get(&chip_a(), TEMPERATURE_BASELINE)
            .unwrap()
            .count(),
        12
    );
    // Other chip samples go in as they are.
    assert_eq!(saga.trails.get(&chip_a(), AI_CLOCK).unwrap().count(), 12);
}

#[test]
fn a_birth_raises_world_hero_and_quest_events_in_order() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    run_ticks(&mut saga, 5);
    let events = saga.drain_events();
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            SagaEvent::WorldBorn { .. } => "WorldBorn",
            SagaEvent::WorldEnded { .. } => "WorldEnded",
            SagaEvent::HeroBorn { .. } => "HeroBorn",
            SagaEvent::QuestStarted { .. } => "QuestStarted",
            SagaEvent::QuestDone { .. } => "QuestDone",
            SagaEvent::Beat { .. } => "Beat",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "WorldBorn",
            "Beat",
            "HeroBorn",
            "Beat",
            "QuestStarted",
            "Beat"
        ]
    );
    // The hero's name comes from the pack, drawn first from the plan stream.
    let state = saga.state();
    let expected = Pack::builtin().hero_name(&mut Pcg32::new(state.world.seed, PLAN_STREAM));
    assert_eq!(state.party[0].hero.name, expected);
    assert!(
        matches!(&events[2], SagaEvent::HeroBorn { name, world: 1, floor: 0 } if *name == expected)
    );
    // Every beat event matches a chronicle line.
    let lines: Vec<String> = state
        .chronicle
        .lines(CAPACITY)
        .map(|b| b.text.clone())
        .collect();
    let beats: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            SagaEvent::Beat { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(beats, lines);
}

#[test]
fn the_hero_walks_and_finishes_quests() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    run_ticks(&mut saga, 5);
    saga.drain_events();
    let start = saga.state().party[0].hero.pos;
    run_ticks(&mut saga, 400);
    let events = saga.drain_events();
    let done = events
        .iter()
        .filter(|e| matches!(e, SagaEvent::QuestDone { .. }))
        .count();
    let started = events
        .iter()
        .filter(|e| matches!(e, SagaEvent::QuestStarted { .. }))
        .count();
    assert!(done >= 2, "only {done} quests done in 400 ticks");
    // After each arrival the hero dwells, then plans the next quest.
    assert!(started >= done.saturating_sub(1).max(1));
    assert_ne!(saga.state().party[0].hero.pos, start);
    // A done quest's beat is triumphant on a healthy chip.
    let tones: Vec<Tone> = events
        .windows(2)
        .filter_map(|w| match (&w[0], &w[1]) {
            (SagaEvent::QuestDone { .. }, SagaEvent::Beat { tone, .. }) => Some(*tone),
            _ => None,
        })
        .collect();
    assert!(!tones.is_empty() && tones.iter().all(|t| *t == Tone::Triumphant));
}

#[test]
fn a_no_path_quest_drops_its_hook_and_plans_again() {
    // Ruling R15: call plan again on no_path, and drop the hook whose goal
    // cannot be reached so the hero does not retry it forever.
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    saga.feed(&event(Event::ModelReady {
        model: EntityKey::model("demo-sage"),
        chips: vec![chip_a()],
    }));
    run_ticks(&mut saga, 5);
    assert_eq!(saga.state.party[0].quest.verb, Verb::VisitSage);
    // Wall the sage in: its exit becomes a wall cell in the hero's world.
    // The floors are shared behind an `Arc`; this saga holds the only
    // reference, so `make_mut` changes them in place.
    let floor = &mut std::sync::Arc::make_mut(&mut saga.state.world.floors)[0];
    let exit = floor.exit;
    let w = floor.w as usize;
    floor.cells[exit.1 as usize * w + exit.0 as usize] = Cell::Wall;
    saga.drain_events();

    run_ticks(&mut saga, 1);
    let events = saga.drain_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            SagaEvent::QuestStarted {
                verb: Verb::Explore,
                ..
            }
        )),
        "no new quest after no_path: {events:?}"
    );
    assert_eq!(saga.state.party[0].quest.verb, Verb::Explore);
    assert!(saga.members[0]
        .hooks
        .iter()
        .all(|h| h.verb != Verb::VisitSage));
    // Later plans never pick the dropped sage again.
    run_ticks(&mut saga, 300);
    let events = saga.drain_events();
    assert!(!events.iter().any(|e| matches!(
        e,
        SagaEvent::QuestStarted {
            verb: Verb::VisitSage,
            ..
        }
    )));
}

#[test]
fn floor_specs_follow_the_chips_seen_in_key_order() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    // Chip B is seen first, through a DRAM channel; chip A reports a Tensix
    // mask, which only Blackhole telemetry carries.
    saga.feed(&sample(
        &EntityKey::dram_channel(&chip_b(), 2),
        "toplike.dram.temp_top_c",
        Value::F64(40.0),
    ));
    saga.feed(&sample(&chip_a(), ENABLED_TENSIX_COL, Value::U64(0x3edf)));
    saga.feed(&sample(&chip_a(), ETH_LIVE_MASK, Value::U64(0x0f0)));
    run_ticks(&mut saga, 5);
    let chips: Vec<&EntityKey> = saga.topo.iter().map(|s| &s.chip).collect();
    assert_eq!(chips, [&chip_a(), &chip_b()]);
    assert_eq!(saga.topo[0].arch, Arch::Blackhole);
    assert_eq!(saga.topo[0].dead_cols, [6, 9]);
    assert_eq!(saga.topo[0].portals, 4);
    assert_eq!(saga.topo[1].arch, Arch::Generic);
    // The world was built from them.
    assert_eq!(saga.state().world.chips, [chip_a(), chip_b()]);
    // A chip whose key says Blackhole is Blackhole without a mask.
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    let named = EntityKey::chip("blackhole-0");
    saga.feed(&sample(&named, "tt_tdp_w", Value::F64(30.0)));
    saga.rebuild_topology();
    assert_eq!(saga.topo[0].arch, Arch::Blackhole);
}

#[test]
fn a_reset_ends_the_world_on_a_tense_beat() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    run_ticks(&mut saga, 10);
    saga.drain_events();
    saga.feed(&event(Event::Reset {
        chips: vec![chip_a()],
    }));
    let events = saga.drain_events();
    assert!(matches!(
        events[0],
        SagaEvent::WorldEnded {
            world: 1,
            why: EndWhy::Reset
        }
    ));
    // The end's beat and the new world's beats are tense, and they cite the
    // reset.
    let beats: Vec<(&Tone, &Cause)> = events
        .iter()
        .filter_map(|e| match e {
            SagaEvent::Beat { tone, cause, .. } => Some((tone, cause)),
            _ => None,
        })
        .collect();
    assert_eq!(beats.len(), 4);
    for (tone, cause) in &beats[..3] {
        assert_eq!(**tone, Tone::Tense);
        assert_eq!(cause.signal, "Reset");
        assert_eq!(cause.entity, chip_a());
    }
}

/// Everything about the walk and the world that the name mode must not
/// change, taken after each record: the world, every member (hero and
/// quest) and the chronicle's length.
#[derive(Debug, PartialEq)]
struct Walked {
    world: World,
    party: Vec<Member>,
    lines: usize,
}

/// A small busy script: a model ready before the first world, a second
/// model ready, then gone, a reset, a third model, and ticks between, with
/// the temperature climbing.
///
/// The first model matters for ruling R16. Generated mode draws a model's
/// alias the first time a beat names it. For a model that becomes ready
/// mid-run that is the `WorldEnded` beat just before the new world reseeds
/// both streams, so a shared stream would go unnoticed. A model that is
/// ready before the seed gate opens is first named inside world 1, where a
/// shared stream changes the next explore quest.
fn busy_script() -> Vec<Record> {
    let sage = EntityKey::model("demo-sage");
    let mut out = vec![event(Event::ModelReady {
        model: EntityKey::model("demo-early"),
        chips: vec![],
    })];
    for t in 0..900u64 {
        out.push(sweep(&chip_a(), 45.0 + t as f64 * 0.031, 1350));
        out.push(sweep(&chip_b(), 47.0 + t as f64 * 0.017, 1350));
        if t % 5 == 0 {
            out.push(sample(&chip_a(), HEARTBEAT, Value::U64(t)));
        }
        match t {
            400 => out.push(event(Event::ModelReady {
                model: sage.clone(),
                chips: vec![chip_a(), chip_b()],
            })),
            500 => out.push(event(Event::ModelGone {
                model: sage.clone(),
            })),
            600 => out.push(event(Event::Reset { chips: vec![] })),
            700 => out.push(event(Event::ModelReady {
                model: EntityKey::model("demo-scribe"),
                chips: vec![chip_b()],
            })),
            _ => {}
        }
    }
    out
}

#[test]
fn the_name_mode_changes_only_the_chronicle() {
    // Ruling R16: the chronicle draws from its own stream, so `names` cannot
    // change mazes, heroes or quests.
    let mut real = Saga::new(config(NameMode::Real, Label::Live));
    let mut generated = Saga::new(config(NameMode::Generated, Label::Live));
    // A quest's target label is the name the chronicle prints, so it is
    // the only part of a member that follows the names setting: it is left
    // out here and checked on its own below.
    let walked = |s: &Saga| Walked {
        world: s.state.world.clone(),
        party: s
            .state
            .party
            .iter()
            .cloned()
            .map(|mut m| {
                m.quest.target_label = None;
                m
            })
            .collect(),
        lines: s.state.chronicle.len(),
    };
    let mut labels_differ = 0;
    let mut real_events = Vec::new();
    let mut generated_events = Vec::new();
    for r in busy_script() {
        real.feed(&r);
        generated.feed(&r);
        assert_eq!(walked(&real), walked(&generated));
        // No member's plan stream ever sees a chronicle draw.
        let rngs =
            |s: &Saga| -> Vec<Pcg32> { s.members.iter().map(|m| m.plan_rng.clone()).collect() };
        assert_eq!(rngs(&real), rngs(&generated));
        for (a, b) in real.state.party.iter().zip(&generated.state.party) {
            assert_eq!(
                a.quest.target_label.is_some(),
                b.quest.target_label.is_some()
            );
            labels_differ += (a.quest.target_label != b.quest.target_label) as usize;
        }
        real_events.extend(real.drain_events());
        generated_events.extend(generated.drain_events());
    }
    assert!(
        real.state.world.id >= 4,
        "the script should make several worlds"
    );
    // The same events in the same order. Beats differ in text and cause,
    // and a quest's published reason and target are aliased.
    assert_eq!(real_events.len(), generated_events.len());
    let mut differ = 0;
    for (a, b) in real_events.iter().zip(&generated_events) {
        match (a, b) {
            (SagaEvent::Beat { .. }, SagaEvent::Beat { .. }) => {}
            (
                SagaEvent::QuestStarted { world, verb, .. },
                SagaEvent::QuestStarted {
                    world: w2,
                    verb: v2,
                    ..
                },
            ) => {
                assert_eq!((world, verb), (w2, v2));
                differ += (a != b) as usize;
            }
            _ => assert_eq!(a, b),
        }
    }
    // The sage quests name a model, so their published causes and their
    // labels differ.
    assert!(differ > 0);
    assert!(labels_differ > 0);
    // The generated story names no model.
    for e in &generated_events {
        let text = format!("{e:?}");
        assert!(
            !text.contains("demo-sage")
                && !text.contains("demo-scribe")
                && !text.contains("demo-early"),
            "{text}"
        );
    }
}

#[test]
fn a_model_that_goes_away_takes_its_sage_hook_with_it() {
    // Review finding for Task 1.11: under ruling R14 a `ModelGone` ends only
    // the dungeon that model made. A world born by something else (here a
    // reset) must still forget the gone model's sage: no quest for it is
    // started or finished after it goes away.
    let sage = EntityKey::model("demo-sage");
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    run_ticks(&mut saga, 6);
    saga.feed(&event(Event::ModelReady {
        model: sage.clone(),
        chips: vec![chip_a()],
    }));
    run_ticks(&mut saga, 3);
    // A reset births a dungeon that was not made by the model, so the model
    // going away does not end it.
    saga.feed(&event(Event::Reset { chips: vec![] }));
    assert_eq!(saga.state.party[0].quest.verb, Verb::VisitSage);
    assert_eq!(saga.state.party[0].quest.target.as_ref(), Some(&sage));
    let world = saga.state.world.id;
    run_ticks(&mut saga, 3);
    saga.drain_events();

    saga.feed(&event(Event::ModelGone {
        model: sage.clone(),
    }));
    run_ticks(&mut saga, 600);
    let events = saga.drain_events();
    assert_eq!(
        saga.state.world.id, world,
        "the model's going ended an unrelated world"
    );
    for e in &events {
        assert!(
            !matches!(
                e,
                SagaEvent::QuestDone {
                    verb: Verb::VisitSage,
                    ..
                } | SagaEvent::QuestStarted {
                    verb: Verb::VisitSage,
                    ..
                }
            ),
            "a quest for a model that went away: {e:?}"
        );
    }
    // The hero left the sage's quest at once and kept questing.
    assert!(matches!(
        events.first(),
        Some(SagaEvent::QuestStarted {
            verb: Verb::Explore,
            ..
        })
    ));
    assert!(events
        .iter()
        .any(|e| matches!(e, SagaEvent::QuestDone { .. })));
    assert!(saga.members[0]
        .hooks
        .iter()
        .all(|h| h.target.as_ref() != Some(&sage)));
}

/// One event of each variant, with a `Beat` for each floor option. The match
/// in `every_event_variant_has_its_own_tag` has no wildcard, so a new variant
/// does not compile until it is listed there and given a tag.
fn one_of_each() -> Vec<SagaEvent> {
    let cause = Cause {
        entity: chip_a(),
        signal: "s".to_string(),
        ordinal: 1,
    };
    vec![
        SagaEvent::WorldBorn {
            world: 1,
            biome: Biome::Dungeon,
        },
        SagaEvent::WorldEnded {
            world: 1,
            why: EndWhy::Season,
        },
        SagaEvent::HeroBorn {
            world: 1,
            name: "n".to_string(),
            floor: 0,
        },
        SagaEvent::QuestStarted {
            world: 1,
            floor: 2,
            verb: Verb::Rest,
            target: None,
            reason: cause.clone(),
        },
        SagaEvent::QuestDone {
            world: 1,
            floor: 3,
            verb: Verb::Rest,
        },
        SagaEvent::Beat {
            world: 1,
            floor: Some(4),
            tone: Tone::Calm,
            text: "t".to_string(),
            cause: cause.clone(),
        },
        SagaEvent::Beat {
            world: 1,
            floor: None,
            tone: Tone::Calm,
            text: "t".to_string(),
            cause,
        },
    ]
}

#[test]
fn every_event_variant_has_its_own_tag() {
    let mut seen = Vec::new();
    for ev in one_of_each() {
        // The module comment's table, written out again.
        let table = match &ev {
            SagaEvent::WorldBorn { .. } => 0x01,
            SagaEvent::WorldEnded { .. } => 0x02,
            SagaEvent::HeroBorn { .. } => 0x03,
            SagaEvent::QuestStarted { .. } => 0x04,
            SagaEvent::QuestDone { .. } => 0x05,
            SagaEvent::Beat { .. } => 0x06,
        };
        assert_eq!(event_tag(&ev), table, "{ev:?}");
        if !seen.contains(&table) {
            seen.push(table);
        }
    }
    assert_eq!(seen.len(), 6, "two variants share a tag");
    for other in [TAG_VERSION, TAG_SEED, TAG_LINE, TAG_END] {
        assert!(!seen.contains(&other), "an event tag equals a stream tag");
    }
}

/// The bytes of `one_of_each()[i]` at tick 9, written out by hand from the
/// module comment's table (digest version 2).
///
/// The encoding is written down in three places: the table in `saga.rs`'s
/// module comment (and the addendum's section 3.6), the encoders in
/// `saga.rs`, and this hand-written copy. The three change together, in one
/// commit, with `DIGEST_VERSION` moved and the digest goldens re-pinned.
fn one_of_each_by_hand() -> Vec<Vec<u8>> {
    // Tick 9 and world 1, 8 bytes little-endian each.
    let tick_world = [9, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    // The cause "chip:0000:01:00.0" / "s" / ordinal 1.
    let mut cause = vec![17, 0, 0, 0];
    cause.extend_from_slice(b"chip:0000:01:00.0");
    cause.extend_from_slice(&[1, 0, 0, 0, b's']);
    cause.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
    let item = |tag: u8, fields: &[&[u8]]| {
        let mut v = vec![tag];
        v.extend_from_slice(&tick_world);
        for f in fields {
            v.extend_from_slice(f);
        }
        v
    };
    vec![
        // WorldBorn: biome Dungeon 0.
        item(0x01, &[&[0]]),
        // WorldEnded: why Season 2.
        item(0x02, &[&[2]]),
        // HeroBorn: name "n", floor 0.
        item(0x03, &[&[1, 0, 0, 0, b'n'], &[0]]),
        // QuestStarted: floor 2, verb Rest 1, target absent, the cause.
        item(0x04, &[&[2], &[1], &[0], &cause]),
        // QuestDone: floor 3, verb Rest 1.
        item(0x05, &[&[3], &[1]]),
        // Beat: floor present 4, tone Calm 0, text "t", the cause.
        item(0x06, &[&[1, 4], &[0], &[1, 0, 0, 0, b't'], &cause]),
        // Beat: floor absent, tone Calm 0, text "t", the cause.
        item(0x06, &[&[0], &[0], &[1, 0, 0, 0, b't'], &cause]),
    ]
}

#[test]
fn the_encoding_table_matches_the_code() {
    // Every event, one at a time, against the table.
    for (ev, hand) in one_of_each().iter().zip(one_of_each_by_hand()) {
        let mut encoded = Vec::new();
        encode_event(&mut encoded, 9, ev);
        assert_eq!(encoded, hand, "{ev:?}");
    }
    // A chronicle line: tag, floor option, text.
    let beat = |floor| Beat {
        tone: Tone::Tense,
        text: "hi".to_string(),
        cause: Cause {
            entity: chip_a(),
            signal: String::new(),
            ordinal: 0,
        },
        tick: 4,
        floor,
    };
    for (floor, hand) in [
        (Some(7), vec![0x20, 1, 7, 2, 0, 0, 0, b'h', b'i']),
        (None, vec![0x20, 0, 2, 0, 0, 0, b'h', b'i']),
    ] {
        let mut encoded = Vec::new();
        encode_line(&mut encoded, &beat(floor));
        assert_eq!(encoded, hand, "line with floor {floor:?}");
    }
    // The end: tag, world seed 0x0102, one member, its floor, x and y.
    let mut encoded = Vec::new();
    encode_end(&mut encoded, 0x0102, &[(5, 6, 7)]);
    assert_eq!(encoded, [0x30, 2, 1, 0, 0, 0, 0, 0, 0, 1, 5, 6, 7]);
    // The version item.
    assert_eq!((TAG_VERSION, DIGEST_VERSION), (0x00, 2));
}

#[test]
fn an_empty_saga_digest_has_the_version_item() {
    // A saga that has seen nothing hashes the version item, then the end
    // item: the placeholder's seed 0, one member, and its floor and cell.
    let saga = Saga::new(config(NameMode::Real, Label::Live));
    let hero = &saga.state.party[0].hero;
    let (x, y) = hero.pos;
    let mut hand = vec![0x00, 0x02, 0x30];
    hand.extend_from_slice(&[0; 8]);
    hand.extend_from_slice(&[1, hero.floor, x, y]);
    assert_eq!(saga.digest(), crate::seed::fnv1a64(&hand));
}

#[test]
fn the_digest_encoding_matches_a_hand_written_byte_string() {
    // Ruling R19: the digest hashes an explicit byte encoding. The bytes
    // below are written out by hand from the module comment's table; the
    // expected digest is FNV-1a 64 of them, from the helper that
    // `fnv1a_matches_the_published_vectors` checks.
    let events = [
        (
            5u64,
            SagaEvent::WorldBorn {
                world: 1,
                biome: Biome::Forge,
            },
        ),
        (
            6,
            SagaEvent::QuestStarted {
                world: 1,
                floor: 0,
                verb: Verb::VisitSage,
                target: Some(EntityKey::model("ab")),
                reason: Cause {
                    entity: EntityKey::model("ab"),
                    signal: "ModelReady".to_string(),
                    ordinal: 3,
                },
            },
        ),
        (
            7,
            SagaEvent::QuestDone {
                world: 1,
                floor: 1,
                verb: Verb::Rest,
            },
        ),
        (
            300,
            SagaEvent::Beat {
                world: 1,
                floor: Some(1),
                tone: Tone::Tense,
                text: "hi".to_string(),
                cause: Cause {
                    entity: EntityKey::chip("x"),
                    signal: "t".to_string(),
                    ordinal: 258,
                },
            },
        ),
    ];
    // The version item comes first, once.
    let mut hand: Vec<u8> = vec![0x00, 0x02];
    // WorldBorn: tag, tick 5, world 1, Forge.
    hand.extend_from_slice(&[0x01, 5, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1]);
    // QuestStarted: tag, tick 6, world 1, floor 0, VisitSage, target present
    // "model:ab", reason "model:ab" / "ModelReady" / ordinal 3.
    hand.extend_from_slice(&[
        0x04, 6, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1,
    ]);
    hand.extend_from_slice(&[8, 0, 0, 0]);
    hand.extend_from_slice(b"model:ab");
    hand.extend_from_slice(&[8, 0, 0, 0]);
    hand.extend_from_slice(b"model:ab");
    hand.extend_from_slice(&[10, 0, 0, 0]);
    hand.extend_from_slice(b"ModelReady");
    hand.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 0]);
    // QuestDone: tag, tick 7, world 1, floor 1, Rest.
    hand.extend_from_slice(&[0x05, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 1]);
    // Beat: tag, tick 300 (0x012c), world 1, floor present 1, Tense, "hi",
    // cause "chip:x" / "t" / ordinal 258 (0x0102).
    hand.extend_from_slice(&[
        0x06, 0x2c, 0x01, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1,
    ]);
    hand.extend_from_slice(&[2, 0, 0, 0]);
    hand.extend_from_slice(b"hi");
    hand.extend_from_slice(&[6, 0, 0, 0]);
    hand.extend_from_slice(b"chip:x");
    hand.extend_from_slice(&[1, 0, 0, 0]);
    hand.extend_from_slice(b"t");
    hand.extend_from_slice(&[0x02, 0x01, 0, 0, 0, 0, 0, 0]);

    // The encoder writes exactly these bytes, after the version item.
    let mut encoded = vec![0x00, 0x02];
    for (tick, ev) in &events {
        encode_event(&mut encoded, *tick, ev);
    }
    assert_eq!(encoded, hand);

    // Through the saga: emit the events at their ticks, with nothing else
    // in the stream, and write one chronicle line on floor 1. The digest
    // then hashes that line (tag 0x20, floor present 1, its text) and the
    // end item: tag 0x30, the placeholder world's seed 0, one member, and
    // the hero's floor and cell.
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    for (tick, ev) in events {
        saga.state.tick = tick;
        saga.emit(ev);
    }
    let mut ctx = saga.ctx(
        Some(0),
        Tone::Calm,
        None,
        NameRole::Service,
        None,
        None,
        &Cause {
            entity: chip_a(),
            signal: String::new(),
            ordinal: 0,
        },
    );
    ctx.floor = Some(1);
    saga.write_beat(BeatKind::WorldBorn, &ctx);
    let text = saga.state.chronicle.lines(1).next().unwrap().text.clone();
    hand.extend_from_slice(&[0x20, 1, 1]);
    hand.extend_from_slice(&u32::try_from(text.len()).unwrap().to_le_bytes());
    hand.extend_from_slice(text.as_bytes());
    let hero = &saga.state.party[0].hero;
    let (x, y) = hero.pos;
    hand.push(0x30);
    hand.extend_from_slice(&[0; 8]);
    hand.extend_from_slice(&[1, hero.floor, x, y]);
    assert_eq!(saga.digest(), crate::seed::fnv1a64(&hand));
}

/// The floor a beat should carry, from the event raised just before it: a
/// world's event gives `None`, a hero's or a quest's event gives the floor
/// it names.
///
/// Two beats come in a row only at a party's birth: the party's line
/// follows the world's line. A beat after a world's beat is a world's beat
/// too.
fn floor_after(ev: &SagaEvent) -> Option<u8> {
    match ev {
        SagaEvent::WorldBorn { .. } | SagaEvent::WorldEnded { .. } => None,
        SagaEvent::HeroBorn { floor, .. }
        | SagaEvent::QuestStarted { floor, .. }
        | SagaEvent::QuestDone { floor, .. } => Some(*floor),
        SagaEvent::Beat { floor: None, .. } => None,
        SagaEvent::Beat { .. } => panic!("a member's beat followed by another beat"),
    }
}

#[test]
fn quest_and_beat_events_carry_the_floor() {
    // A two-chip world has two members, one on each floor. Every quest
    // event and quest beat carries the floor of the member it is about, and
    // its text names that member. A floor or a name taken from the lead
    // member gives 0 or the lead's name on floor 1's lines.
    let mut saga = saga_on(&[chip_b()]);
    assert_eq!(saga.state.world.floors.len(), 2);
    let names: Vec<String> = saga
        .state
        .party
        .iter()
        .map(|m| m.hero.name.clone())
        .collect();
    assert_ne!(names[0], names[1]);
    let mut events = saga.drain_events();
    run_ticks(&mut saga, 400);
    events.extend(saga.drain_events());

    let (mut started, mut done) = ([0; 2], [0; 2]);
    for e in &events {
        match e {
            SagaEvent::QuestStarted { floor, .. } => started[*floor as usize] += 1,
            SagaEvent::QuestDone { floor, .. } => done[*floor as usize] += 1,
            _ => {}
        }
    }
    for f in 0..2 {
        assert!(
            started[f] >= 2 && done[f] >= 1,
            "floor {f}: {} started, {} done",
            started[f],
            done[f]
        );
    }
    // Each quest's beat comes right after its event and carries its floor.
    // A beat that names a hero names the member of its floor.
    let mut named = [0; 2];
    for w in events.windows(2) {
        if let SagaEvent::Beat { floor, text, .. } = &w[1] {
            assert_eq!(*floor, floor_after(&w[0]), "{:?} then {:?}", w[0], w[1]);
            let Some(f) = *floor else { continue };
            let (own, other) = (&names[f as usize], &names[1 - f as usize]);
            assert!(!text.contains(other.as_str()), "floor {f}: {text}");
            named[f as usize] += text.contains(own.as_str()) as usize;
        }
    }
    assert!(named[0] >= 1 && named[1] >= 1, "named {named:?}");
    // The stored lines carry the floors too.
    let n = events
        .iter()
        .filter(|e| matches!(e, SagaEvent::Beat { .. }))
        .count();
    let floors: Vec<Option<u8>> = saga.state.chronicle.lines(n).map(|b| b.floor).collect();
    assert!(floors.contains(&Some(0)) && floors.contains(&Some(1)));
}

#[test]
fn world_beats_have_no_floor() {
    // The busy script births, ends and births again, in both name modes.
    for names in [NameMode::Real, NameMode::Generated] {
        let mut saga = Saga::new(config(names, Label::Live));
        let mut events = Vec::new();
        for r in busy_script() {
            saga.feed(&r);
            events.extend(saga.drain_events());
        }
        let (mut world, mut hero) = (0, 0);
        for w in events.windows(2) {
            if let SagaEvent::Beat { floor, .. } = &w[1] {
                let want = floor_after(&w[0]);
                assert_eq!(*floor, want, "{:?} then {:?}", w[0], w[1]);
                match want {
                    None => world += 1,
                    Some(f) => {
                        assert!(f < 2, "the script has two chips: floor {f}");
                        hero += 1;
                    }
                }
            }
        }
        assert!(
            world >= 4 && hero >= 4,
            "{world} world beats, {hero} hero beats"
        );
    }
}

#[test]
fn has_world_is_false_until_the_first_world_is_born() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    assert!(
        !saga.state().has_world(),
        "a fresh saga holds the placeholder"
    );
    // Four ticks of a one-chip machine: the seed pool has seen only eight
    // samples, so the gate is still closed (it opens at tick 5).
    run_ticks(&mut saga, 4);
    assert_eq!(saga.state().world.id, 0);
    assert!(!saga.state().has_world(), "no world before the gate opens");
    run_ticks(&mut saga, 1);
    assert_eq!(saga.state().world.id, 1);
    assert!(
        saga.state().has_world(),
        "the first world is born by tick 5"
    );
}

// ---- The party ------------------------------------------------------------

// Also used by `fuzz_tests.rs` (the file may be included only once).
#[allow(dead_code)]
#[path = "../tests/support/party.rs"]
pub(super) mod party_gen;

/// A third chip, sorting after chip B.
fn chip_c() -> EntityKey {
    EntityKey::chip("0000:03:00.0")
}

/// A saga whose kept chips are A and `others`, in a world born by tick 5.
/// Each other chip is seen through one temperature sample.
fn saga_on(others: &[EntityKey]) -> Saga {
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    for c in others {
        saga.feed(&sample(c, ASIC_TEMPERATURE, Value::F64(46.0)));
    }
    run_ticks(&mut saga, 5);
    assert_eq!(saga.state.party.len(), others.len() + 1);
    saga
}

/// What a member's walk is, after one record: everything another member's
/// draws could change. The member's `unwritten` count is left out (set to
/// 0): the chronicle's pace shares one window among all floors, so another
/// member's extra line can leave this member's line unwritten. That changes
/// the chronicle only, never the walk.
fn walk_of(saga: &Saga, i: usize) -> (Member, Pcg32, Option<u64>, Vec<Hook>) {
    let mut member = saga.state.party[i].clone();
    member.unwritten = 0;
    (
        member,
        saga.members[i].plan_rng.clone(),
        saga.members[i].dwell_until,
        saga.members[i].hooks.clone(),
    )
}

#[test]
fn a_member_that_replans_leaves_the_others_unchanged() {
    // The party addendum, section 4.2: each member has its own plan
    // generator, so one member's extra plan (here a no-path replan, at every
    // birth) never changes another member's walk.
    let records = party_gen::records(4, 600);
    let mut plain = Saga::new(config(NameMode::Real, Label::Mock));
    let mut walled = Saga::with_walled_sage(config(NameMode::Real, Label::Mock), 2);
    let (mut worlds, mut member_2_differs) = (0, false);
    for r in &records {
        plain.feed(r);
        walled.feed(r);
        let (a, b) = (plain.state(), walled.state());
        assert_eq!(a.world.id, b.world.id);
        if a.world.id != worlds {
            worlds = a.world.id;
        }
        if !a.has_world() {
            continue;
        }
        assert_eq!(a.party.len(), 4);
        for i in [0, 1, 3] {
            assert_eq!(
                walk_of(&plain, i),
                walk_of(&walled, i),
                "member {i} at tick {}",
                a.tick
            );
        }
        member_2_differs |= walk_of(&plain, 2) != walk_of(&walled, 2);
    }
    // The log births several worlds, and the injection did change member 2.
    assert!(worlds >= 3, "{worlds} worlds");
    assert!(member_2_differs, "the walled sage never changed member 2");
}

#[test]
fn a_sage_hook_is_dropped_only_for_the_member_who_visited() {
    let sage = EntityKey::model("demo-sage");
    let mut saga = saga_on(&[chip_b()]);
    // A model that names no chips: its sage stands on both floors, and its
    // world gives both members a sage quest.
    saga.feed(&event(Event::ModelReady {
        model: sage.clone(),
        chips: vec![],
    }));
    for i in 0..2 {
        assert_eq!(saga.state.party[i].quest.verb, Verb::VisitSage, "{i}");
    }
    // Member 0 stands at its sage; member 1 is still at its gate.
    let exit = saga.state.world.floors[0].exit;
    saga.state.party[0].hero.pos = exit;
    saga.drain_events();
    run_ticks(&mut saga, 1);
    let events = saga.drain_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            SagaEvent::QuestDone {
                floor: 0,
                verb: Verb::VisitSage,
                ..
            }
        )),
        "{events:?}"
    );
    let has_sage = |i: usize, saga: &Saga| {
        saga.members[i]
            .hooks
            .iter()
            .any(|h| h.target.as_ref() == Some(&sage))
    };
    assert!(!has_sage(0, &saga), "member 0 visited its sage");
    assert!(has_sage(1, &saga), "member 1 lost a sage it never visited");
    assert_eq!(saga.state.party[1].quest.verb, Verb::VisitSage);
}

#[test]
fn model_gone_replans_every_member_walking_to_it() {
    let sage = EntityKey::model("demo-sage");
    let mut saga = saga_on(&[chip_b(), chip_c()]);
    // The sage stands on floors 0 and 2. A reset then births a dungeon the
    // model did not make, so the model going away does not end it.
    saga.feed(&event(Event::ModelReady {
        model: sage.clone(),
        chips: vec![chip_a(), chip_c()],
    }));
    saga.feed(&event(Event::Reset { chips: vec![] }));
    let verbs: Vec<Verb> = saga.state.party.iter().map(|m| m.quest.verb).collect();
    assert_eq!(verbs, [Verb::VisitSage, Verb::Explore, Verb::VisitSage]);
    run_ticks(&mut saga, 3);
    let world = saga.state.world.id;
    let member_1 = walk_of(&saga, 1);
    saga.drain_events();

    saga.feed(&event(Event::ModelGone {
        model: sage.clone(),
    }));
    let events = saga.drain_events();
    assert_eq!(saga.state.world.id, world);
    // Members 0 and 2 plan again at once, in index order; member 1 walks on.
    let started: Vec<(u8, Verb)> = events
        .iter()
        .filter_map(|e| match e {
            SagaEvent::QuestStarted { floor, verb, .. } => Some((*floor, *verb)),
            _ => None,
        })
        .collect();
    assert_eq!(started, [(0, Verb::Explore), (2, Verb::Explore)]);
    assert_eq!(walk_of(&saga, 1), member_1);
    for (i, core) in saga.members.iter().enumerate() {
        assert!(
            core.hooks.iter().all(|h| h.target.as_ref() != Some(&sage)),
            "member {i} kept the gone model's hook"
        );
    }
}

#[test]
fn each_member_has_its_own_plan_stream() {
    // The party addendum, section 4.2, written out by hand: member 0 keeps
    // stream 1, member i draws from stream 16 + i, and no member shares the
    // chronicle's stream 2.
    assert_eq!(
        (PLAN_STREAM, PARTY_STREAM_BASE, CHRONICLE_STREAM),
        (1, 16, 2)
    );
    let streams: Vec<u64> = (0..32).map(plan_stream).collect();
    let mut want = vec![1u64];
    want.extend(17..=47);
    assert_eq!(streams, want);
    // Each member's generator is seeded on its stream at a birth.
    let saga = saga_on(&[chip_b(), chip_c()]);
    assert_eq!(saga.members.len(), 3);
    for (i, &stream) in want.iter().enumerate().take(3) {
        let mut fresh = Pcg32::new(saga.state.world.seed, stream);
        let name = Pack::builtin().hero_name_unique(
            &mut fresh,
            &saga.state.party[..i]
                .iter()
                .map(|m| m.hero.name.clone())
                .collect::<Vec<_>>(),
        );
        assert_eq!(saga.state.party[i].hero.name, name, "member {i}");
    }
}

#[test]
fn the_sage_floors_are_the_floors_with_a_sage_hook() {
    // Ruling R31: named keys that are not kept chips share one set of at
    // most MAX_CHIPS, first seen wins. A key the set has no room for is not
    // kept, so a world's sage_chips leaves its floor out, the same as that
    // floor's member, who gets no sage hook.
    let ghost = EntityKey::model("demo-ghost");
    let sage = EntityKey::model("demo-sage");
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    saga.feed(&sample(&chip_a(), ASIC_TEMPERATURE, Value::F64(45.0)));
    // Before the gate: one model names MAX_CHIPS chips no backend reports,
    // filling the shared set.
    saga.feed(&event(Event::ModelReady {
        model: ghost,
        chips: (0..MAX_CHIPS)
            .map(|k| EntityKey::chip(&format!("ghost-{k:02}")))
            .collect(),
    }));
    assert_eq!(saga.named_unseen.len(), MAX_CHIPS);
    // Then the sage names chip A (kept, so always held) and chip C (not yet
    // seen, and the set is full).
    saga.feed(&event(Event::ModelReady {
        model: sage.clone(),
        chips: vec![chip_a(), chip_c()],
    }));
    assert_eq!(saga.ready[&sage].chips, [chip_a()]);
    // Chip C is seen before the gate opens, so the sage's queued dungeon has
    // a floor on it. The lifecycle lists chip C as a sage chip; the saga
    // does not, and neither do the hooks.
    saga.feed(&sample(&chip_c(), ASIC_TEMPERATURE, Value::F64(46.0)));
    run_ticks(&mut saga, 5);
    assert_eq!(saga.state.world.chips, [chip_a(), chip_c()]);
    assert_eq!(
        saga.life.world().map(|w| w.sage_chips.clone()),
        Some(vec![chip_a(), chip_c()])
    );
    assert_eq!(saga.state.world.sage_chips, [chip_a()]);
    let has_sage = |i: usize| {
        saga.members[i]
            .hooks
            .iter()
            .any(|h| h.target.as_ref() == Some(&sage))
    };
    assert!(has_sage(0) && !has_sage(1));

    // On ordinary logs the two lists always agree: at every birth of the
    // busy script, a floor is a sage floor exactly when its member has the
    // world's model's hook.
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    let mut checked = 0;
    let mut seen = 0;
    for r in busy_script() {
        saga.feed(&r);
        let w = &saga.state.world;
        if w.id == seen {
            continue;
        }
        seen = w.id;
        let model = saga.life.model().cloned();
        for (i, chip) in w.chips.iter().enumerate() {
            let hook = model.as_ref().is_some_and(|m| {
                saga.members[i]
                    .hooks
                    .iter()
                    .any(|h| h.target.as_ref() == Some(m))
            });
            assert_eq!(w.sage_chips.contains(chip), hook, "world {seen}, {chip}");
            checked += hook as usize;
        }
        assert_eq!(
            saga.life.world().map(|l| &l.sage_chips),
            Some(&w.sage_chips)
        );
    }
    assert!(checked >= 2, "only {checked} sage floors checked");
}

// ---- The merged chronicle (Task A.4 of the party plan) -----------------------

/// A model name found in no lexicon, so a search for it in the text finds
/// only a leak.
const ODD_MODEL: &str = "Zyxwv-Model-9B";

/// A saga on chips A and B in `names` mode, where the model [`ODD_MODEL`]
/// became ready before the first world, naming no chips, so both members
/// get its sage hook and both start towards it at the birth. Returns the
/// saga and the birth's events.
fn two_sage_saga(names: NameMode) -> (Saga, Vec<SagaEvent>) {
    let mut saga = Saga::new(config(names, Label::Live));
    saga.feed(&sample(&chip_b(), ASIC_TEMPERATURE, Value::F64(46.0)));
    saga.feed(&event(Event::ModelReady {
        model: EntityKey::model(ODD_MODEL),
        chips: Vec::new(),
    }));
    run_ticks(&mut saga, 5);
    assert_eq!(saga.state.party.len(), 2);
    let events = saga.drain_events();
    (saga, events)
}

/// The short form of a birth's events: `W` world born, `H<f>` hero born,
/// `Q<f>` quest started, `b` a world's beat, `b<f>` a member's beat.
fn shape(events: &[SagaEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            SagaEvent::WorldBorn { .. } => "W".to_string(),
            SagaEvent::WorldEnded { .. } => "E".to_string(),
            SagaEvent::HeroBorn { floor, .. } => format!("H{floor}"),
            SagaEvent::QuestStarted { floor, .. } => format!("Q{floor}"),
            SagaEvent::QuestDone { floor, .. } => format!("D{floor}"),
            SagaEvent::Beat { floor: None, .. } => "b".to_string(),
            SagaEvent::Beat { floor: Some(f), .. } => format!("b{f}"),
        })
        .collect()
}

#[test]
fn a_party_birth_writes_one_party_line() {
    // Three chips: the world's line, then one line for the whole party, and
    // no hero's line. Each member still raises its HeroBorn.
    let mut saga = saga_on(&[chip_b(), chip_c()]);
    let events = saga.drain_events();
    assert_eq!(
        shape(&events),
        ["W", "b", "b", "H0", "H1", "H2", "Q0", "b0", "Q1", "Q2"]
    );
    let lines: Vec<&Beat> = saga.state.chronicle.lines(CAPACITY).collect();
    assert_eq!(lines.len(), 3, "world, party and the lead's first quest");
    let party = lines[1];
    assert_eq!(party.floor, None);
    assert!(party.text.contains("three"), "{}", party.text);
    // Its text comes from a PartyBorn template of the pack.
    let fits = Pack::builtin()
        .templates
        .iter()
        .filter(|t| t.kind == BeatKind::PartyBorn)
        .any(|t| {
            let head = t.text.split('{').next().unwrap_or("");
            party.text.starts_with(head)
        });
    assert!(fits, "{}", party.text);

    // One chip: the hero's line as before, and no party line.
    let mut solo = Saga::new(config(NameMode::Real, Label::Live));
    run_ticks(&mut solo, 5);
    assert_eq!(
        shape(&solo.drain_events()),
        ["W", "b", "H0", "b0", "Q0", "b0"]
    );
}

/// A world's birth opens the routine window again. A window an old world's
/// floor owned would otherwise silence the new party's first quest lines
/// for the rest of that window: here floor 2 of the old world owns the
/// window the birth falls in, and member 0 of the new world still writes
/// its first quest's line.
#[test]
fn a_new_world_does_not_inherit_the_old_windows_owner() {
    let mut saga = saga_on(&[chip_b(), chip_c()]);
    saga.drain_events();
    let born = saga.state.world.id;
    // Floor 2 of the old world owns the current window.
    saga.window_owner = Some((saga.state.tick / ROUTINE_WINDOW_TICKS, 2));
    // A ready model naming no chips ends this world and births a dungeon on
    // every chip, in the same record and so in the same window.
    let tick = saga.state.tick;
    saga.feed(&event(Event::ModelReady {
        model: EntityKey::model(ODD_MODEL),
        chips: Vec::new(),
    }));
    assert_ne!(saga.state.world.id, born, "a new world was born");
    assert_eq!(saga.state.tick, tick, "in the same tick");
    let events = saga.drain_events();
    let at = events
        .iter()
        .position(|e| matches!(e, SagaEvent::WorldBorn { .. }))
        .expect("the birth's events");
    let birth = shape(&events[at..]);
    let q0 = birth
        .iter()
        .position(|e| e == "Q0")
        .expect("member 0's quest");
    assert_eq!(
        birth.get(q0 + 1).map(String::as_str),
        Some("b0"),
        "member 0's first quest line was not written: {birth:?}"
    );
}

#[test]
fn a_dropped_quest_still_raises_its_event_with_a_clean_cause() {
    // At the birth both members set off for the sage. Member 0's line owns
    // the window, so member 1's line is not written. Its QuestStarted is
    // still raised, and in generated mode its reason and target carry the
    // alias the written line used. The model's name never appears.
    let (saga, events) = two_sage_saga(NameMode::Generated);
    assert_eq!(
        shape(&events),
        ["W", "b", "b", "H0", "H1", "Q0", "b0", "Q1"]
    );
    let started: Vec<(&Option<EntityKey>, &Cause)> = events
        .iter()
        .filter_map(|e| match e {
            SagaEvent::QuestStarted {
                verb: Verb::VisitSage,
                target,
                reason,
                ..
            } => Some((target, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 2, "both members visit the sage");
    let (target0, reason0) = started[0];
    let (target1, reason1) = started[1];
    assert!(!reason1.entity.as_str().contains(ODD_MODEL), "{reason1:?}");
    assert_eq!(reason1, reason0, "one alias for one model");
    assert_eq!(target1.as_ref(), Some(&reason1.entity));
    assert_eq!(target0, target1);
    assert_eq!(saga.state.party[0].unwritten, 0);
    assert_eq!(saga.state.party[1].unwritten, 1);
    // The member's own quest keeps the real reason (the modes plan alike).
    assert_eq!(
        saga.state.party[1].quest.reason.entity,
        EntityKey::model(ODD_MODEL)
    );

    // Real mode: the dropped quest's event carries the real reason.
    let (_, events) = two_sage_saga(NameMode::Real);
    let reasons: Vec<&Cause> = events
        .iter()
        .filter_map(|e| match e {
            SagaEvent::QuestStarted { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert!(reasons
        .iter()
        .all(|r| r.entity == EntityKey::model(ODD_MODEL)));
}

/// A real name longer than 32 characters is cut to its first 32 (R5), and
/// the target label is the cut name, the same text the beat writes. The
/// whole id stays in the quest's target and the event's cause.
#[test]
fn a_long_model_id_labels_its_quest_with_the_first_32_characters() {
    const LONG: &str = "Zyxwv-Model-9B-Instruct-Long-Context-v2";
    assert!(LONG.chars().count() > 32);
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    saga.feed(&sample(&chip_b(), ASIC_TEMPERATURE, Value::F64(46.0)));
    saga.feed(&event(Event::ModelReady {
        model: EntityKey::model(LONG),
        chips: Vec::new(),
    }));
    run_ticks(&mut saga, 5);
    let cut: String = LONG.chars().take(32).collect();
    let quest = &saga.state.party[0].quest;
    assert_eq!(quest.verb, Verb::VisitSage);
    assert_eq!(quest.target, Some(EntityKey::model(LONG)));
    assert_eq!(quest.target_label.as_deref(), Some(cut.as_str()));
    let line = saga
        .state
        .chronicle
        .lines(CAPACITY)
        .find(|b| b.floor == Some(0))
        .expect("member 0's first line");
    assert!(line.text.contains(&cut), "{}", line.text);
    assert!(!line.text.contains(LONG), "{}", line.text);
}

#[test]
fn target_label_is_the_beat_name_in_both_modes() {
    for names in [NameMode::Real, NameMode::Generated] {
        let (mut saga, events) = two_sage_saga(names);
        // Member 0's line was written: the label is the name in its text.
        let line = saga
            .state
            .chronicle
            .lines(CAPACITY)
            .find(|b| b.floor == Some(0))
            .expect("member 0's first line")
            .clone();
        let label0 = saga.state.party[0].quest.target_label.clone();
        let label1 = saga.state.party[1].quest.target_label.clone();
        let label = label0.clone().expect("a sage quest has a label");
        assert!(
            line.text.contains(&label),
            "{names:?}: {label} in {}",
            line.text
        );
        // It is the id of the event's reason: the alias in generated mode.
        let reason = events
            .iter()
            .find_map(|e| match e {
                SagaEvent::QuestStarted { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .expect("a quest");
        let id = reason.entity.as_str().split_once(':').map(|(_, id)| id);
        assert_eq!(id, Some(label.as_str()));
        match names {
            NameMode::Real => assert_eq!(label, ODD_MODEL),
            NameMode::Generated => assert_ne!(label, ODD_MODEL),
        }
        // Member 1's line was not written; its label is the same name.
        assert_eq!(label1, label0, "{names:?}");

        // Over a longer run, every quest has a label exactly when it visits
        // a sage, and a written sage line always holds its member's label.
        for _ in 0..400 {
            let t = saga.state().tick as f64;
            saga.feed(&sweep(&chip_a(), 45.0 + t * 0.013, 1350));
            saga.feed(&sweep(&chip_b(), 46.0 + t * 0.011, 1350));
            let events = saga.drain_events();
            for w in events.windows(2) {
                if let (
                    SagaEvent::QuestStarted {
                        verb: Verb::VisitSage,
                        floor,
                        ..
                    },
                    SagaEvent::Beat { text, .. },
                ) = (&w[0], &w[1])
                {
                    let label = saga.state.party[*floor as usize]
                        .quest
                        .target_label
                        .clone()
                        .expect("a label");
                    assert!(text.contains(&label), "{label} in {text}");
                }
            }
            for m in &saga.state.party {
                assert_eq!(
                    m.quest.target_label.is_some(),
                    m.quest.verb == Verb::VisitSage,
                    "{:?}",
                    m.quest
                );
            }
        }
    }
}

#[test]
fn a_model_ready_reads_only_the_first_64_chips_it_names() {
    // The front door reads the first MAX_NAMED_CHIPS (64) entries of a
    // ModelReady's chip list. Chip A as entry 64 (the 65th) is not read, so
    // the model's sage is on no floor; as entry 63 it is read.
    use crate::world::MAX_NAMED_CHIPS;
    assert_eq!(MAX_NAMED_CHIPS, 64);
    let sage = EntityKey::model("demo-sage");
    let hooked = |ghosts: usize| {
        let mut saga = Saga::new(config(NameMode::Real, Label::Live));
        let mut chips: Vec<EntityKey> = (0..ghosts)
            .map(|k| EntityKey::chip(&format!("ghost-{k:02}")))
            .collect();
        chips.push(chip_a());
        saga.feed(&sample(&chip_a(), ASIC_TEMPERATURE, Value::F64(45.0)));
        saga.feed(&event(Event::ModelReady {
            model: sage.clone(),
            chips,
        }));
        run_ticks(&mut saga, 5);
        assert_eq!(saga.state.world.chips, [chip_a()]);
        let hook = saga.members[0]
            .hooks
            .iter()
            .any(|h| h.target.as_ref() == Some(&sage));
        assert_eq!(saga.state.world.sage_chips.contains(&chip_a()), hook);
        hook
    };
    assert!(hooked(63), "the 64th entry is read");
    assert!(!hooked(64), "the 65th entry is not read");
}
