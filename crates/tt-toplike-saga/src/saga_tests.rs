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
            SagaEvent::HeroCrossed { .. } => "HeroCrossed",
            SagaEvent::CrossingFailed { .. } => "CrossingFailed",
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
    assert_eq!(saga.topo[0].portal_ports, [4, 5, 6, 7]);
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
            cause: cause.clone(),
        },
        SagaEvent::HeroCrossed {
            world: 1,
            member: 5,
            from_floor: 2,
            from_port: 11,
            to_floor: 3,
            to_port: 1,
            cause: cause.clone(),
        },
        SagaEvent::CrossingFailed {
            world: 1,
            member: 5,
            floor: 2,
            port: 11,
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
            SagaEvent::HeroCrossed { .. } => 0x07,
            SagaEvent::CrossingFailed { .. } => 0x08,
        };
        assert_eq!(event_tag(&ev), table, "{ev:?}");
        if !seen.contains(&table) {
            seen.push(table);
        }
    }
    assert_eq!(seen.len(), 8, "two variants share a tag");
    for other in [TAG_VERSION, TAG_SEED, TAG_LINE, TAG_END] {
        assert!(!seen.contains(&other), "an event tag equals a stream tag");
    }
}

/// The bytes of `one_of_each()[i]` at tick 9, written out by hand from the
/// module comment's table (digest version 3, whose items are those of
/// version 2: only the version byte moved).
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
        // HeroCrossed: member 5, from floor 2 port 11, to floor 3 port 1,
        // the cause.
        item(0x07, &[&[5, 2, 11, 3, 1], &cause]),
        // CrossingFailed: member 5, floor 2, port 11, the cause.
        item(0x08, &[&[5, 2, 11], &cause]),
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
    // A member in transit: its floor byte carries the transit bit 0x80.
    assert_eq!(IN_TRANSIT, 0x80);
    // The crossing verb's code.
    assert_eq!(verb_code(Verb::Cross), 7);
    // The version item.
    assert_eq!((TAG_VERSION, DIGEST_VERSION), (0x00, 3));
}

#[test]
fn an_empty_saga_digest_has_the_version_item() {
    // A saga that has seen nothing hashes the version item, then the end
    // item: the placeholder's seed 0, one member, and its floor and cell.
    let saga = Saga::new(config(NameMode::Real, Label::Live));
    let hero = &saga.state.party[0].hero;
    let (x, y) = hero.pos;
    let mut hand = vec![0x00, 0x03, 0x30];
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
    let mut hand: Vec<u8> = vec![0x00, 0x03];
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
    let mut encoded = vec![0x00, 0x03];
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
        // A crossing's line is written on the floor the hero left.
        SagaEvent::HeroCrossed { from_floor: f, .. }
        | SagaEvent::CrossingFailed { floor: f, .. } => Some(*f),
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
            SagaEvent::HeroCrossed { from_floor, .. } => format!("X{from_floor}"),
            SagaEvent::CrossingFailed { floor, .. } => format!("F{floor}"),
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

#[test]
fn the_saga_records_masks_and_peers_in_its_link_table() {
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    // A peer record may come before the far chip's first sample (the bus
    // source writes each chip's peers right after that chip's own readings),
    // so it is kept even though chip B is not yet seen.
    saga.feed(&sample(&chip_a(), ETH_LIVE_MASK, Value::U64(0x1_0003)));
    saga.feed(&event(Event::LinkPeer {
        port: EntityKey::eth_port(&chip_a(), 1),
        peer: EntityKey::eth_port(&chip_b(), 0),
    }));
    assert_eq!(
        saga.links().peer(&chip_a(), 1),
        Some((chip_b(), 0)),
        "the peer is recorded"
    );
    assert_eq!(saga.links().mask(&chip_a()), Some(0x1_0003));
    assert!(saga.links().live(&chip_a(), 1));
    assert!(!saga.links().live(&chip_b(), 0), "chip B's mask is unseen");
    // A `LinkChanged` carries the low 16 bits: it replaces those and keeps
    // the rest of the mask.
    saga.feed(&event(Event::LinkChanged {
        chip: chip_a(),
        mask: 0x0001,
    }));
    assert_eq!(saga.links().mask(&chip_a()), Some(0x1_0001));
    assert!(!saga.links().live(&chip_a(), 1));
    // The cause of the change is the event, with its ordinal.
    let cause = saga.links().mask_cause(&chip_a()).cloned().unwrap();
    assert_eq!((cause.signal.as_str(), cause.ordinal), ("LinkChanged", 3));
    // A sample sets the whole mask again.
    saga.feed(&sample(&chip_a(), ETH_LIVE_MASK, Value::U64(0x3)));
    assert_eq!(saga.links().mask(&chip_a()), Some(0x3));
    let cause = saga.links().mask_cause(&chip_a()).cloned().unwrap();
    assert_eq!((cause.signal.as_str(), cause.ordinal), (ETH_LIVE_MASK, 4));
}

#[test]
fn a_register_value_reaches_the_floor_and_the_link_table_exactly() {
    // `--mock` reports the live mask 0xAAAA_AAAA_AAAA_AAAA as a `U64`. As
    // an f64 it would round to 0xaaaa_aaaa_aaaa_a800 and lose ports 1, 3,
    // 5, 7 and 9 (review of B.3, Important 1). The saga must keep the
    // register's every bit.
    const MASK: u64 = 0xAAAA_AAAA_AAAA_AAAA;
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    saga.feed(&sample(&chip_a(), ENABLED_TENSIX_COL, Value::U64(0x3fff)));
    saga.feed(&sample(&chip_a(), ETH_LIVE_MASK, Value::U64(MASK)));
    run_ticks(&mut saga, 5);
    assert_eq!(saga.links().mask(&chip_a()), Some(MASK));
    assert!(saga.links().live(&chip_a(), 1));
    assert!(saga.links().live(&chip_a(), 11));
    assert!(!saga.links().live(&chip_a(), 0));
    let want = FloorSpec::for_chip(&chip_a(), None, Some(0x3fff), Some(MASK), Arch::Blackhole);
    assert_eq!(saga.topo[0].portal_ports, want.portal_ports);
    assert_eq!(saga.topo[0].portal_ports, [1, 3, 5, 7, 9, 11]);
    let floor = &saga.state().world.floors[0];
    let ports: Vec<u8> = floor.portals.iter().map(|&(p, _)| p).collect();
    assert_eq!(ports, [1, 3, 5, 7, 9, 11], "the world's floor has them too");
    // A Tensix mask with a high bit set keeps its low bits as well.
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    saga.feed(&sample(
        &chip_a(),
        ENABLED_TENSIX_COL,
        Value::U64((1 << 63) | 0x3edf),
    ));
    run_ticks(&mut saga, 1);
    assert_eq!(saga.topo[0].dead_cols, [6, 9]);
}

// ---- Crossing (Task B.4 of the party plan) ------------------------------------

/// Ethernet port `port` of `chip`.
fn eth(chip: &EntityKey, port: u8) -> EntityKey {
    EntityKey::eth_port(chip, port)
}

/// A live-mask sample of `chip` with provenance `prov`.
fn mask_sample(chip: &EntityKey, mask: u64, prov: Provenance) -> Record {
    rec(Payload::Sample(Sample {
        entity: chip.clone(),
        signal: ETH_LIVE_MASK.to_string(),
        value: Value::U64(mask),
        prov,
    }))
}

/// A mask change as the bus source writes it: the sample, then the
/// `LinkChanged` with the low 16 bits.
fn set_mask(saga: &mut Saga, chip: &EntityKey, mask: u64) {
    saga.feed(&mask_sample(chip, mask, Provenance::Estimated));
    saga.feed(&event(Event::LinkChanged {
        chip: chip.clone(),
        mask: mask as u16,
    }));
}

/// Ports 0 and 1 live: the mask of both chips of [`pair_saga`].
const PAIR_MASK: u64 = 0b11;

/// Chips A and B, two Blackhole chips whose ports 0 and 1 are live, with
/// A's port 1 cabled to B's port 0, in a dungeon born by tick 5 (world 1).
/// Floor 0 (chip A) and floor 1 (chip B) each have portals on ports 0 and
/// 1, and the link joins floor 0's port 1 to floor 1's port 0. With
/// `model`, `demo-pair` then becomes ready on both chips, which births
/// world 2, the world the test reads. The live masks carry `prov`.
fn pair_saga(model: bool, prov: Provenance) -> Saga {
    pair_saga_with(model, prov, &[(1, 0)])
}

/// [`pair_saga`] with the links `links`: each `(a, b)` cables chip A's
/// port `a` to chip B's port `b`. The first link must be `(1, 0)`.
fn pair_saga_with(model: bool, prov: Provenance, links: &[(u8, u8)]) -> Saga {
    let mut saga = Saga::new(config(NameMode::Real, Label::Mock));
    for c in [chip_a(), chip_b()] {
        saga.feed(&sample(&c, ENABLED_TENSIX_COL, Value::U64(0x3fff)));
        saga.feed(&mask_sample(&c, PAIR_MASK, prov));
    }
    for &(a, b) in links {
        saga.feed(&event(Event::LinkPeer {
            port: eth(&chip_a(), a),
            peer: eth(&chip_b(), b),
        }));
    }
    run_ticks(&mut saga, 5);
    assert_eq!(saga.state.party.len(), 2);
    assert_eq!(saga.links().open(&saga.state.world, 0, 1), Some((1, 0)));
    if model {
        saga.feed(&event(Event::ModelReady {
            model: EntityKey::model("demo-pair"),
            chips: vec![chip_a(), chip_b()],
        }));
        assert_eq!(saga.state.world.id, 2);
    }
    saga.drain_events();
    saga
}

/// Runs ticks until one raises an event that `pred` accepts, at most `max`
/// ticks. Returns that tick's events.
fn tick_until(saga: &mut Saga, max: u64, pred: impl Fn(&SagaEvent) -> bool) -> Vec<SagaEvent> {
    saga.drain_events();
    for _ in 0..max {
        run_ticks(saga, 1);
        let events = saga.drain_events();
        if events.iter().any(&pred) {
            return events;
        }
    }
    panic!("no such event in {max} ticks");
}

/// Whether `e` is member `m`'s `HeroCrossed`.
fn crossed(m: u8) -> impl Fn(&SagaEvent) -> bool {
    move |e| matches!(e, SagaEvent::HeroCrossed { member, .. } if *member == m)
}

/// The cell of the portal with port `port` on floor `floor`.
fn portal_cell(saga: &Saga, floor: usize, port: u8) -> (u8, u8) {
    saga.state.world.floors[floor]
        .portals
        .iter()
        .find(|&&(p, _)| p == port)
        .map(|&(_, c)| c)
        .expect("the floor has that portal")
}

#[test]
fn a_model_on_both_ends_sends_a_hero_across() {
    // A model that names only one end offers no crossing.
    let mut one_end = pair_saga(false, Provenance::Estimated);
    one_end.feed(&event(Event::ModelReady {
        model: EntityKey::model("demo-pair"),
        chips: vec![chip_a()],
    }));
    assert!(one_end.cross_hooks.iter().all(Vec::is_empty));

    // A model on both ends gives each floor one crossing hook through the
    // link, with the model's `ModelReady` as its reason. A hook keeps its
    // own port and no key: the far port's key is the quest's target, taken
    // when the quest starts (see "Bounds" in the module comment).
    let mut saga = pair_saga(true, Provenance::Estimated);
    let hooks = saga.cross_hooks.clone();
    assert_eq!(hooks.len(), 2);
    for (floor, port) in [(0, 1), (1, 0)] {
        assert_eq!(hooks[floor].len(), 1, "floor {floor}");
        let h = &hooks[floor][0];
        assert_eq!(
            (h.verb, h.port, h.target.clone()),
            (Verb::Cross, Some(port), None)
        );
        assert_eq!(h.reason.entity, EntityKey::model("demo-pair"));
        assert_eq!(h.reason.signal, "ModelReady");
    }
    let reason = hooks[0][0].reason.clone();

    // Member 0 walks to the portal and steps in.
    let events = tick_until(&mut saga, 3000, crossed(0));
    let ev = events.iter().find(|e| crossed(0)(e)).unwrap();
    assert_eq!(
        *ev,
        SagaEvent::HeroCrossed {
            world: 2,
            member: 0,
            from_floor: 0,
            from_port: 1,
            to_floor: 1,
            to_port: 0,
            cause: reason,
        }
    );
    let m = &saga.state.party[0];
    assert_eq!(m.quest.verb, Verb::Cross);
    assert_eq!(m.quest.target, Some(eth(&chip_b(), 0)));
    assert_eq!(m.hero.pos, portal_cell(&saga, 0, 1));
    assert!(
        saga.cross_hooks[0].is_empty(),
        "the crossing dropped its hook"
    );
    // Three ticks later it stands on floor 1's portal of port 0.
    run_ticks(&mut saga, TRANSIT_TICKS);
    let hero = &saga.state.party[0].hero;
    assert_eq!(hero.floor, 1);
    assert_eq!(hero.pos, portal_cell(&saga, 1, 0));
    assert_eq!(hero.transit, None);
}

#[test]
fn a_link_coming_up_offers_one_crossing() {
    // The plain dungeon offers no crossing: no model runs on the link.
    let mut saga = pair_saga(false, Provenance::Estimated);
    assert!(saga.cross_hooks.iter().all(Vec::is_empty));
    // Chip A's port 1 goes down, then comes up. The sample that brings it
    // back offers nothing; the `LinkChanged` after it offers floor 0 one
    // crossing through it, with that event as its reason.
    set_mask(&mut saga, &chip_a(), 0b01);
    assert!(saga.cross_hooks.iter().all(Vec::is_empty));
    saga.feed(&mask_sample(&chip_a(), PAIR_MASK, Provenance::Estimated));
    assert!(saga.cross_hooks.iter().all(Vec::is_empty), "a sample alone");
    saga.feed(&event(Event::LinkChanged {
        chip: chip_a(),
        mask: PAIR_MASK as u16,
    }));
    let ordinal = saga.feed.ordinal();
    assert_eq!(saga.cross_hooks[0].len(), 1);
    assert!(saga.cross_hooks[1].is_empty(), "chip B's bit never fell");
    let h = &saga.cross_hooks[0][0];
    assert_eq!((h.verb, h.port), (Verb::Cross, Some(1)));
    assert_eq!(
        (&h.reason.entity, h.reason.signal.as_str(), h.reason.ordinal),
        (&chip_a(), "LinkChanged", ordinal)
    );
    // The hero takes it and crosses for that reason.
    let events = tick_until(&mut saga, 3000, crossed(0));
    let cause = events.iter().find_map(|e| match e {
        SagaEvent::HeroCrossed { cause, .. } => Some(cause.clone()),
        _ => None,
    });
    assert_eq!(
        cause.map(|c| (c.signal, c.ordinal)),
        Some(("LinkChanged".to_string(), ordinal))
    );
}

#[test]
fn a_crossing_writes_a_line_on_each_floor() {
    let mut saga = pair_saga(true, Provenance::Estimated);
    let name = saga.state.party[0].hero.name.clone();
    // On the departure floor, the step into the portal: a beat on floor 0,
    // right after the event, with the quest's reason as its cause.
    let events = tick_until(&mut saga, 3000, crossed(0));
    let entered = saga.state.tick;
    let at = events.iter().position(|e| crossed(0)(e)).unwrap();
    let SagaEvent::Beat {
        floor, text, cause, ..
    } = &events[at + 1]
    else {
        panic!("no beat after the crossing: {:?}", events.get(at + 1));
    };
    assert_eq!(*floor, Some(0));
    assert!(text.contains(&name), "{text}");
    assert!(
        text.contains("floor 2"),
        "names the far floor as its tag F2: {text}"
    );
    assert_eq!(cause.signal, "ModelReady");
    // On the arrival floor, three ticks later, the step out: a beat on
    // floor 1.
    run_ticks(&mut saga, TRANSIT_TICKS - 1);
    assert!(saga.state.party[0].hero.transit.is_some());
    saga.drain_events();
    run_ticks(&mut saga, 1);
    let lines: Vec<(Option<u8>, String)> = saga
        .drain_events()
        .into_iter()
        .filter_map(|e| match e {
            SagaEvent::Beat { floor, text, .. } if text.contains(&name) => Some((floor, text)),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].0, Some(1));
    assert!(
        lines[0].1.contains("floor 1"),
        "names the floor it came from: {}",
        lines[0].1
    );
    // Both lines are in the chronicle, and both carry the `~` mark: the
    // live masks behind them are estimated (spec 3.9). (The crossing
    // quest's own start line, before them, names the portal too.)
    let both: Vec<&Beat> = saga
        .state
        .chronicle
        .lines(CAPACITY)
        .filter(|b| b.tick >= entered && b.text.contains(&name) && b.text.contains("portal"))
        .collect();
    assert_eq!(both.len(), 2);
    assert!(both.iter().all(|b| b.text.ends_with(" ~")), "{both:?}");
}

#[test]
fn a_crossing_on_measured_links_has_no_tilde_mark() {
    let mut saga = pair_saga(true, Provenance::Measured);
    let events = tick_until(&mut saga, 3000, crossed(0));
    let at = events.iter().position(|e| crossed(0)(e)).unwrap();
    let SagaEvent::Beat { text, .. } = &events[at + 1] else {
        panic!("no beat after the crossing");
    };
    assert!(!text.contains('~'), "{text}");
    // One end estimated is enough for the mark.
    let mut saga = pair_saga(true, Provenance::Measured);
    saga.feed(&mask_sample(&chip_b(), PAIR_MASK, Provenance::Estimated));
    let events = tick_until(&mut saga, 3000, crossed(0));
    let at = events.iter().position(|e| crossed(0)(e)).unwrap();
    let SagaEvent::Beat { text, .. } = &events[at + 1] else {
        panic!("no beat after the crossing");
    };
    assert!(text.ends_with(" ~"), "{text}");
}

#[test]
fn a_hero_in_transit_is_on_no_map_for_three_ticks() {
    assert_eq!(TRANSIT_TICKS, 3);
    let mut saga = pair_saga(true, Provenance::Estimated);
    tick_until(&mut saga, 3000, crossed(0));
    let entered = saga.state.tick;
    let on = |saga: &Saga, floor: u8| saga.state.on_floor(floor).any(|(i, _)| i == 0);
    let mut away = 0;
    loop {
        let t = saga.state.party[0].hero.transit.clone();
        if t.is_none() {
            break;
        }
        let t = t.unwrap();
        assert_eq!(
            t,
            Transit {
                to_floor: 1,
                to_port: 0,
                from_port: 1,
                until: entered + TRANSIT_TICKS,
            }
        );
        assert!(!on(&saga, 0) && !on(&saga, 1), "tick {}", saga.state.tick);
        away += 1;
        run_ticks(&mut saga, 1);
    }
    assert_eq!(away, 3, "in transit for three ticks");
    assert_eq!(saga.state.tick, entered + TRANSIT_TICKS);
    assert!(on(&saga, 1) && !on(&saga, 0));
    // In transit the end item marks the member; on the floor it does not.
    let mut saga = pair_saga(true, Provenance::Estimated);
    tick_until(&mut saga, 3000, crossed(0));
    let d_transit = saga.digest();
    saga.state.party[0].hero.transit = None;
    assert_ne!(saga.digest(), d_transit, "transit is part of the digest");
}

#[test]
fn a_link_down_in_transit_throws_the_hero_back() {
    let mut saga = pair_saga(true, Provenance::Estimated);
    tick_until(&mut saga, 3000, crossed(0));
    let entered = saga.state.tick;
    let cell = saga.state.party[0].hero.pos;
    // The far end's port goes down and comes back up before the next tick.
    // It went down while the hero was in transit, so the crossing fails.
    saga.feed(&mask_sample(&chip_b(), 0b10, Provenance::Estimated));
    let down = saga.feed.ordinal();
    saga.feed(&event(Event::LinkChanged {
        chip: chip_b(),
        mask: 0b10,
    }));
    set_mask(&mut saga, &chip_b(), PAIR_MASK);
    saga.drain_events();
    run_ticks(&mut saga, 1);
    let events = saga.drain_events();
    let failed = events
        .iter()
        .position(|e| matches!(e, SagaEvent::CrossingFailed { .. }));
    let Some(at) = failed else {
        panic!("no CrossingFailed: {events:?}");
    };
    assert_eq!(
        events[at],
        SagaEvent::CrossingFailed {
            world: 2,
            member: 0,
            floor: 0,
            port: 1,
            cause: Cause {
                entity: chip_b(),
                signal: ETH_LIVE_MASK.to_string(),
                ordinal: down,
            },
        }
    );
    // A tense line on the departure floor.
    let SagaEvent::Beat { floor, tone, .. } = &events[at + 1] else {
        panic!("no beat after the failure");
    };
    assert_eq!((*floor, *tone), (Some(0), Tone::Tense));
    // The hero is back on its portal on floor 0, and stays on floor 0.
    let hero = &saga.state.party[0].hero;
    assert_eq!(
        (hero.floor, hero.pos, hero.transit.clone()),
        (0, cell, None)
    );
    assert_eq!(saga.state.tick, entered + 1);
    run_ticks(&mut saga, TRANSIT_TICKS + 2);
    assert_eq!(saga.state.party[0].hero.floor, 0);
    assert!(saga.drain_events().iter().all(|e| !crossed(0)(e)));
}

#[test]
fn a_dark_portal_drops_the_hook_and_replans() {
    // Two links join A and B: A's port 1 to B's port 0, and A's port 0 to
    // B's port 1. The model on both offers floor 0 two crossings, one per
    // portal.
    let mut saga = pair_saga_with(true, Provenance::Estimated, &[(1, 0), (0, 1)]);
    assert_eq!(saga.cross_hooks[0].len(), 2);
    // Run until member 0 sets out for a portal.
    tick_until(&mut saga, 3000, |e| {
        matches!(
            e,
            SagaEvent::QuestStarted {
                floor: 0,
                verb: Verb::Cross,
                ..
            }
        )
    });
    let goal = saga.state.party[0].quest.goal;
    let port = portal_at(&saga.state.world, 0, goal).expect("the goal is a portal");
    // Both of chip A's ports go down while it walks.
    set_mask(&mut saga, &chip_a(), 0);
    let name = saga.state.party[0].hero.name.clone();
    saga.drain_events();
    let mut events = Vec::new();
    while saga.state.party[0].hero.pos != goal {
        run_ticks(&mut saga, 1);
        events = saga.drain_events();
        assert!(saga.state.tick < 3000, "the hero never reached the portal");
    }
    // A tense line on floor 0 says the portal is dark; no crossing.
    assert!(events.iter().all(|e| !crossed(0)(e)), "{events:?}");
    let dark = events.iter().position(|e| {
        matches!(e, SagaEvent::Beat { floor: Some(0), tone: Tone::Tense, text, .. }
            if text.contains(&name) && text.contains("dark"))
    });
    let Some(at) = dark else {
        panic!("no dark line: {events:?}");
    };
    // The portal's hook is gone; the other portal's hook stays.
    let left: Vec<Option<u8>> = saga.cross_hooks[0].iter().map(|h| h.port).collect();
    assert_eq!(left, [Some(1 - port)]);
    // The hero planned again at once. The plan after a dark portal takes
    // no crossing, though one is on offer: a hero cannot walk from one
    // dark portal straight to the next and fill the chronicle with dark
    // lines.
    let next = events[at..].iter().find_map(|e| match e {
        SagaEvent::QuestStarted { floor: 0, verb, .. } => Some(*verb),
        _ => None,
    });
    assert!(
        matches!(next, Some(v) if v != Verb::Cross),
        "next quest: {next:?}"
    );
    assert_eq!(saga.state.party[0].hero.floor, 0);
}

#[test]
fn a_flapping_link_adds_one_hook_per_rise() {
    let mut saga = pair_saga(false, Provenance::Estimated);
    let count = |saga: &Saga| saga.cross_hooks[0].len();
    // One rise, one hook. A second report of the same mask is no rise.
    set_mask(&mut saga, &chip_a(), 0b01);
    set_mask(&mut saga, &chip_a(), PAIR_MASK);
    assert_eq!(count(&saga), 1);
    saga.feed(&event(Event::LinkChanged {
        chip: chip_a(),
        mask: PAIR_MASK as u16,
    }));
    assert_eq!(count(&saga), 1);
    // With the hook taken away, the repeated report still adds none: only
    // a rise does.
    saga.cross_hooks[0].clear();
    saga.feed(&event(Event::LinkChanged {
        chip: chip_a(),
        mask: PAIR_MASK as u16,
    }));
    assert_eq!(count(&saga), 0, "a report with no rise added a hook");
    // Each rise adds one, and a floor never holds two for one port.
    set_mask(&mut saga, &chip_a(), 0b01);
    set_mask(&mut saga, &chip_a(), PAIR_MASK);
    assert_eq!(count(&saga), 1);
    set_mask(&mut saga, &chip_a(), 0b01);
    set_mask(&mut saga, &chip_a(), PAIR_MASK);
    assert_eq!(count(&saga), 1, "two rises, one port: one hook");

    // Now the link flaps every other tick for 3,000 ticks. Hooks stay at
    // one per port, every crossing has a rise behind it, and the
    // chronicle is not flooded: each member writes at most two crossing
    // lines (in and out, or out and back) per crossing, and a crossing
    // takes a walk, three ticks of transit and a 30-tick dwell.
    let (mut rises, mut crossings, mut lines) = (1u64, 0u64, 0u64);
    let mut per_window: BTreeMap<u64, u64> = BTreeMap::new();
    let start = saga.state.tick;
    saga.drain_events();
    for k in 0..3000u64 {
        if k % 2 == 0 {
            let mask = if k % 4 == 0 { 0b01 } else { PAIR_MASK };
            set_mask(&mut saga, &chip_a(), mask);
            rises += u64::from(mask == PAIR_MASK);
        }
        run_ticks(&mut saga, 1);
        assert!(saga.cross_hooks.iter().all(|h| h.len() <= 1));
        for e in saga.drain_events() {
            match e {
                SagaEvent::HeroCrossed { .. } => crossings += 1,
                SagaEvent::Beat { .. } => {
                    lines += 1;
                    *per_window.entry(saga.state.tick / 30).or_default() += 1;
                }
                _ => {}
            }
        }
    }
    assert!(crossings > 0, "the flapping link was never crossed");
    assert!(crossings <= rises);
    let ticks = saga.state.tick - start;
    // Two members; the busiest 30-tick window and the whole run.
    let worst = per_window.values().copied().max().unwrap_or(0);
    assert!(worst <= 8, "{worst} lines in one 30-tick window");
    assert!(lines <= ticks / 5, "{lines} lines in {ticks} ticks");
}

#[test]
fn a_crossed_hero_takes_its_vitality_from_the_far_chip() {
    let mut saga = pair_saga(true, Provenance::Estimated);
    // Chip A's heartbeat ticks on; chip B's stalls after one beat, so B's
    // vitality drains.
    saga.feed(&sample(&chip_b(), HEARTBEAT, Value::U64(1)));
    let mut beat = 0u64;
    let mut step = |saga: &mut Saga| {
        beat += 1;
        saga.feed(&sample(&chip_a(), HEARTBEAT, Value::U64(beat)));
        run_ticks(saga, 1);
        saga.drain_events()
    };
    loop {
        if step(&mut saga).iter().any(crossed(0)) {
            break;
        }
        assert!(saga.state.tick < 3000, "no crossing");
    }
    for _ in 0..TRANSIT_TICKS + 1 {
        step(&mut saga);
    }
    let hero = &saga.state.party[0].hero;
    assert_eq!(hero.floor, 1);
    let (a, b) = (
        vitality(&saga.trails, &chip_a()),
        vitality(&saga.trails, &chip_b()),
    );
    assert!(
        b < 0.5 && a > 0.9,
        "the test needs two vitalities: A {a}, B {b}"
    );
    assert_eq!(hero.vitality, b);
}

#[test]
fn crossing_beats_skip_the_routine_budget() {
    let mut saga = pair_saga(true, Provenance::Estimated);
    // Every window belongs to a floor that does not exist, so no routine
    // line of either member is written.
    let mut events = Vec::new();
    for _ in 0..3000 {
        let window = (saga.state.tick + 1) / ROUTINE_WINDOW_TICKS;
        saga.window_owner = Some((window, 99));
        run_ticks(&mut saga, 1);
        events.extend(saga.drain_events());
        if events.iter().any(crossed(0)) {
            break;
        }
    }
    let at = events.iter().position(crossed(0)).expect("a crossing");
    // No routine line was written, yet the crossing's line was.
    assert!(saga.state.party[0].unwritten > 0);
    assert!(matches!(
        &events[at + 1],
        SagaEvent::Beat { floor: Some(0), .. }
    ));
    let name = &saga.state.party[0].hero.name;
    let last = saga.state.chronicle.lines(1).next().unwrap();
    assert!(last.text.contains(name.as_str()), "{}", last.text);
}

#[test]
fn a_model_gone_takes_its_crossing_hooks_with_it() {
    let mut saga = pair_saga(true, Provenance::Estimated);
    // A second model, ready on both chips, births world 3 with the same
    // hooks (one per port: the first model in key order gives the reason).
    saga.feed(&event(Event::ModelReady {
        model: EntityKey::model("demo-zpair"),
        chips: vec![chip_a(), chip_b()],
    }));
    assert_eq!(saga.state.world.id, 3);
    assert_eq!(saga.cross_hooks[0].len(), 1);
    assert_eq!(
        saga.cross_hooks[0][0].reason.entity,
        EntityKey::model("demo-pair")
    );
    // demo-pair did not make world 3, so it goes away without ending it,
    // and its crossing hooks leave with it.
    saga.feed(&event(Event::ModelGone {
        model: EntityKey::model("demo-pair"),
    }));
    assert_eq!(saga.state.world.id, 3);
    // demo-zpair still runs on both chips, so it offers the crossing
    // again: one hook on each floor, with its own `ModelReady` as the
    // reason.
    for floor in 0..2 {
        let hooks = &saga.cross_hooks[floor];
        assert_eq!(hooks.len(), 1, "floor {floor}: {hooks:?}");
        assert_eq!(hooks[0].reason.entity, EntityKey::model("demo-zpair"));
    }
    // When demo-zpair goes too, no ready model is left on the link.
    saga.feed(&event(Event::ModelGone {
        model: EntityKey::model("demo-zpair"),
    }));
    assert!(saga.cross_hooks.iter().all(Vec::is_empty));
}

#[test]
fn a_model_ready_that_births_no_world_offers_its_crossing() {
    let mut saga = pair_saga(true, Provenance::Estimated);
    assert_eq!(saga.cross_hooks[0].len(), 1);
    // The heroes used the hooks up.
    for hooks in &mut saga.cross_hooks {
        hooks.clear();
    }
    // The world's own model is ready again: no new world, and the
    // crossing is offered again on both floors.
    let ordinal = saga.feed.ordinal() + 1;
    saga.feed(&event(Event::ModelReady {
        model: EntityKey::model("demo-pair"),
        chips: vec![chip_a(), chip_b()],
    }));
    assert_eq!(saga.state.world.id, 2, "no birth");
    for floor in 0..2 {
        let hooks = &saga.cross_hooks[floor];
        assert_eq!(hooks.len(), 1, "floor {floor}: {hooks:?}");
        assert_eq!(hooks[0].verb, Verb::Cross);
        assert_eq!(hooks[0].reason.entity, EntityKey::model("demo-pair"));
        assert_eq!(hooks[0].reason.ordinal, ordinal, "the new record");
    }
}
