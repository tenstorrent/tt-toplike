//! Tests for the front door. Each one feeds hand-built records; none reads a
//! clock or a file. The determinism tests over the recorded fixtures live in
//! `tests/determinism.rs`.

use super::*;
use crate::chronicle::Tone;
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
