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
    assert_eq!(state.hero.name, expected);
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
    let start = saga.state().hero.pos;
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
    assert_ne!(saga.state().hero.pos, start);
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
    assert_eq!(saga.state.quest.verb, Verb::VisitSage);
    // Wall the sage in: its exit becomes a wall cell in the hero's world.
    let floor = &mut saga.state.world.floors[0];
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
    assert_eq!(saga.state.quest.verb, Verb::Explore);
    assert!(saga.hooks.iter().all(|h| h.verb != Verb::VisitSage));
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
/// change, taken after each record.
#[derive(Debug, PartialEq)]
struct Walked {
    world: World,
    pos: (u8, u8),
    floor: u8,
    vitality: f32,
    name: String,
    quest: Quest,
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
    let walked = |s: &Saga| Walked {
        world: s.state.world.clone(),
        pos: s.state.hero.pos,
        floor: s.state.hero.floor,
        vitality: s.state.hero.vitality,
        name: s.state.hero.name.clone(),
        quest: s.state.quest.clone(),
        lines: s.state.chronicle.len(),
    };
    let mut real_events = Vec::new();
    let mut generated_events = Vec::new();
    for r in busy_script() {
        real.feed(&r);
        generated.feed(&r);
        assert_eq!(walked(&real), walked(&generated));
        // The plan stream itself never sees a chronicle draw.
        assert_eq!(real.plan_rng, generated.plan_rng);
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
    // The sage quests name a model, so their published causes differ.
    assert!(differ > 0);
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
    assert_eq!(saga.state.quest.verb, Verb::VisitSage);
    assert_eq!(saga.state.quest.target.as_ref(), Some(&sage));
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
    assert!(saga.hooks.iter().all(|h| h.target.as_ref() != Some(&sage)));
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
    let (x, y) = saga.state.hero.pos;
    let mut hand = vec![0x00, 0x02, 0x30];
    hand.extend_from_slice(&[0; 8]);
    hand.extend_from_slice(&[1, saga.state.hero.floor, x, y]);
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
    let (x, y) = saga.state.hero.pos;
    hand.push(0x30);
    hand.extend_from_slice(&[0; 8]);
    hand.extend_from_slice(&[1, saga.state.hero.floor, x, y]);
    assert_eq!(saga.digest(), crate::seed::fnv1a64(&hand));
}

/// The floor a beat should carry, from the event raised just before it: a
/// world's event gives `None`, a hero's or a quest's event gives the floor
/// it names.
fn floor_after(ev: &SagaEvent) -> Option<u8> {
    match ev {
        SagaEvent::WorldBorn { .. } | SagaEvent::WorldEnded { .. } => None,
        SagaEvent::HeroBorn { floor, .. }
        | SagaEvent::QuestStarted { floor, .. }
        | SagaEvent::QuestDone { floor, .. } => Some(*floor),
        SagaEvent::Beat { .. } => panic!("two beats in a row"),
    }
}

#[test]
fn quest_and_beat_events_carry_the_floor() {
    // A two-chip world. The hero is moved to floor 1's entrance, so a floor
    // copied from the hero differs from the 0 a hard-coded floor would give.
    let mut saga = Saga::new(config(NameMode::Real, Label::Live));
    saga.feed(&sample(&chip_b(), ASIC_TEMPERATURE, Value::F64(46.0)));
    run_ticks(&mut saga, 5);
    assert_eq!(saga.state.world.floors.len(), 2);
    saga.state.hero.floor = 1;
    saga.state.hero.pos = saga.state.world.floors[1].entrance;
    saga.drain_events();
    saga.dwell_until = None;
    saga.start_quest(false);
    run_ticks(&mut saga, 400);
    let events = saga.drain_events();

    let (mut started, mut done) = (0, 0);
    for e in &events {
        match e {
            SagaEvent::QuestStarted { floor, .. } => {
                assert_eq!(*floor, 1, "{e:?}");
                started += 1;
            }
            SagaEvent::QuestDone { floor, .. } => {
                assert_eq!(*floor, 1, "{e:?}");
                done += 1;
            }
            _ => {}
        }
    }
    assert!(started >= 2 && done >= 1, "{started} started, {done} done");
    // Each quest's beat comes right after its event and carries the floor.
    for w in events.windows(2) {
        if let SagaEvent::Beat { floor, .. } = &w[1] {
            assert_eq!(*floor, floor_after(&w[0]), "{:?} then {:?}", w[0], w[1]);
            assert_eq!(*floor, Some(1));
        }
    }
    // The stored lines carry it too.
    let n = events
        .iter()
        .filter(|e| matches!(e, SagaEvent::Beat { .. }))
        .count();
    assert!(saga.state.chronicle.lines(n).all(|b| b.floor == Some(1)));
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
                        assert_eq!(f, 0, "the hero never leaves floor 0 here");
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
