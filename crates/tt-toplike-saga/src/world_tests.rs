//! Tests for the world lifecycle.
//!
//! Every test drives a [`Lifecycle`] through a small harness that does what
//! the core's driver does: it counts ordinals and ticks, pushes each
//! observation into the seed pool, then steps the lifecycle. All inputs are
//! fixed; nothing reads a clock.

use super::*;
use crate::event::EndWhy;
use crate::maze;
use crate::seed::fnv1a64;
use crate::topology::Arch;
use tt_toplike_bus::Event;

const C1: &str = "0000:01:00.0";
const C2: &str = "0000:02:00.0";
const C3: &str = "0000:03:00.0";

fn chip(id: &str) -> EntityKey {
    EntityKey::chip(id)
}

/// Three Blackhole chips of one shape, listed out of key order so the tests
/// can check that floors come out in chip-key order.
fn topo() -> Vec<FloorSpec> {
    [C3, C1, C2]
        .iter()
        .map(|id| FloorSpec::for_chip(&chip(id), None, None, None, Arch::Blackhole))
        .collect()
}

/// The driver's side of the loop, kept minimal.
struct Run {
    lc: Lifecycle,
    seeds: SeedPool,
    tick: u64,
    ordinal: u64,
    topo: Vec<FloorSpec>,
}

impl Run {
    fn new(season_ticks: u64) -> Run {
        Run {
            lc: Lifecycle::new(season_ticks),
            seeds: SeedPool::new(),
            tick: 0,
            ordinal: 0,
            topo: topo(),
        }
    }

    /// Feeds one observation and returns the events that step produced.
    fn feed(&mut self, obs: Observation) -> Vec<SagaEvent> {
        self.ordinal += 1;
        if obs == Observation::Tick {
            self.tick += 1;
        }
        self.seeds.push(&obs);
        let mut out = Vec::new();
        self.lc.step(
            &obs,
            self.tick,
            &self.seeds,
            self.ordinal,
            &self.topo,
            &mut out,
        );
        out
    }

    fn tick(&mut self) -> Vec<SagaEvent> {
        self.feed(Observation::Tick)
    }

    fn event(&mut self, e: Event) -> Vec<SagaEvent> {
        self.feed(Observation::Event(e))
    }

    /// One tick with one temperature sample before it, whose low bits
    /// wobble the way a quiet machine's do.
    fn quiet_tick(&mut self) -> Vec<SagaEvent> {
        let i = self.ordinal;
        let mut out = self.feed(sample(
            C1,
            "toplike.asic_temp",
            40.0 + (i % 7) as f64 * 0.01,
        ));
        out.extend(self.tick());
        out
    }

    /// Runs quiet ticks until the first world is born (tick 5 at the
    /// latest) and returns its id.
    fn start(&mut self) -> u64 {
        while self.lc.world().is_none() {
            self.quiet_tick();
            assert!(self.tick <= 5, "no world by tick 5");
        }
        self.lc.world().unwrap().id
    }

    fn world(&self) -> &World {
        self.lc.world().expect("a world exists")
    }
}

fn sample(entity: &str, signal: &str, value: f64) -> Observation {
    Observation::Sample {
        entity: chip(entity),
        signal: signal.into(),
        value,
        bits: None,
    }
}

fn train_step(step: u64) -> Observation {
    Observation::Sample {
        entity: EntityKey::training_run("r1"),
        signal: "toplike.train.step".into(),
        value: step as f64,
        bits: None,
    }
}

fn born(world: u64, biome: Biome) -> SagaEvent {
    SagaEvent::WorldBorn { world, biome }
}

fn ended(world: u64, why: EndWhy) -> SagaEvent {
    SagaEvent::WorldEnded { world, why }
}

/// The floor the lifecycle should build for `id` in a world seeded `seed`.
fn expected_floor(id: &str, seed: u64) -> maze::Floor {
    let spec = FloorSpec::for_chip(&chip(id), None, None, None, Arch::Blackhole);
    let key = chip(id);
    maze::generate(&spec, seed ^ fnv1a64(key.as_str().as_bytes()), 0.0)
}

#[test]
fn the_first_world_is_born_by_tick_five() {
    // No samples at all: the pool never fills, so tick 5 births the world.
    let mut run = Run::new(12_000);
    for _ in 0..4 {
        assert_eq!(run.tick(), vec![]);
        assert!(run.lc.world().is_none());
    }
    assert_eq!(run.tick(), vec![born(1, Biome::Dungeon)]);
    let w = run.world();
    assert_eq!((w.id, w.born_tick, w.biome), (1, 5, Biome::Dungeon));
    // A world with no model covers every chip, in chip-key order.
    assert_eq!(w.chips, vec![chip(C1), chip(C2), chip(C3)]);
    assert_eq!(w.floors.len(), 3);
    // Events before the first world do not count as ticks.
    let mut run = Run::new(12_000);
    for _ in 0..10 {
        run.event(Event::RequestsCompleted {
            model: EntityKey::model("m"),
            n: 1,
        });
    }
    assert!(run.lc.world().is_none());
}

#[test]
fn a_full_pool_births_the_first_world_at_the_first_tick_after_it_fills() {
    let mut run = Run::new(12_000);
    for i in 0..63 {
        assert_eq!(
            run.feed(sample(C1, "toplike.power", 10.0 + i as f64)),
            vec![]
        );
    }
    // 63 samples are one short of a full ring, so tick 1 births nothing.
    assert_eq!(run.tick(), vec![]);
    assert!(run.lc.world().is_none());
    // Sample 64 fills the ring. Samples never birth a world on their own;
    // the next tick (tick 2, well before tick 5) does.
    assert_eq!(run.feed(sample(C1, "toplike.power", 99.0)), vec![]);
    assert!(run.lc.world().is_none());
    assert_eq!(run.tick(), vec![born(1, Biome::Dungeon)]);
    assert_eq!(run.world().born_tick, 2);
    // The seed is the pool's draw at the birth's ordinal.
    assert_eq!(run.world().seed, run.seeds.draw(run.ordinal));
}

#[test]
fn an_early_model_ready_waits_for_the_seed_gate() {
    let mut run = Run::new(12_000);
    run.feed(sample(C1, "toplike.power", 1.0));
    // Before tick 1, with one sample in the pool: the gate is closed, so the
    // ready is queued and nothing is born.
    let ready = Event::ModelReady {
        model: EntityKey::model("llama"),
        chips: vec![chip(C2)],
    };
    assert_eq!(run.event(ready), vec![]);
    assert!(run.lc.world().is_none());
    for _ in 0..4 {
        assert_eq!(run.tick(), vec![]);
    }
    // Tick 5 opens the gate and births the queued dungeon on every chip,
    // with the sage on the model's chip, seeded from the gate tick's
    // ordinal.
    assert_eq!(run.tick(), vec![born(1, Biome::Dungeon)]);
    let w = run.world();
    assert_eq!(w.born_tick, 5);
    assert_eq!(w.chips, vec![chip(C1), chip(C2), chip(C3)]);
    assert_eq!(w.sage_chips, vec![chip(C2)]);
    assert_eq!(w.seed, run.seeds.draw(run.ordinal));
    assert_eq!(w.floors[1], expected_floor(C2, w.seed));
    // The dungeon belongs to the model: its going away ends it.
    let out = run.event(Event::ModelGone {
        model: EntityKey::model("llama"),
    });
    assert_eq!(out[0], ended(1, EndWhy::ModelDown));
}

#[test]
fn an_early_training_start_waits_for_the_seed_gate_and_a_later_signal_replaces_an_earlier_one() {
    let mut run = Run::new(12_000);
    assert_eq!(
        run.event(Event::ModelReady {
            model: EntityKey::model("llama"),
            chips: vec![chip(C2)],
        }),
        vec![]
    );
    // The training start comes later, so its forge replaces the queued
    // dungeon.
    assert_eq!(run.feed(train_step(1)), vec![]);
    assert!(run.lc.world().is_none());
    let mut out = Vec::new();
    for _ in 0..5 {
        out.extend(run.tick());
    }
    assert_eq!(out, vec![born(1, Biome::Forge)]);
    assert_eq!(run.world().chips.len(), 3);
    assert_eq!(run.world().seed, run.seeds.draw(run.ordinal));
}

#[test]
fn a_queued_model_that_goes_away_is_not_born() {
    let mut run = Run::new(12_000);
    run.event(Event::ModelReady {
        model: EntityKey::model("llama"),
        chips: vec![chip(C2)],
    });
    // The model leaves before the gate opens, so the plain first world is
    // born on every chip.
    assert_eq!(
        run.event(Event::ModelGone {
            model: EntityKey::model("llama"),
        }),
        vec![]
    );
    let mut out = Vec::new();
    for _ in 0..5 {
        out.extend(run.tick());
    }
    assert_eq!(out, vec![born(1, Biome::Dungeon)]);
    assert_eq!(run.world().chips.len(), 3);
}

#[test]
fn a_model_dungeon_spans_every_chip_with_the_sage_on_its_own() {
    // The party addendum, section 3.1: a model's dungeon has a floor for
    // every kept chip, so no chip leaves the view when a model comes up on
    // part of the box. The sage stands only on the model's own chips.
    let mut run = Run::new(12_000);
    let first = run.start();
    // A world no model made has no sage chips.
    assert!(run.world().sage_chips.is_empty());
    // Chips listed out of order, one of them outside the topology.
    let unknown = "0000:09:00.0";
    let out = run.event(Event::ModelReady {
        model: EntityKey::model("llama"),
        chips: vec![chip(C3), chip(unknown), chip(C1)],
    });
    assert_eq!(
        out,
        vec![
            ended(first, EndWhy::Superseded),
            born(first + 1, Biome::Dungeon)
        ]
    );
    let w = run.world().clone();
    assert_eq!(w.id, first + 1);
    assert_eq!(w.seed, run.seeds.draw(run.ordinal));
    // Every kept chip has a floor, in key order; the chip the topology does
    // not list has none.
    assert_eq!(w.chips, vec![chip(C1), chip(C2), chip(C3)]);
    for (i, id) in [C1, C2, C3].iter().enumerate() {
        assert_eq!(w.floors[i], expected_floor(id, w.seed), "{id}");
    }
    // The sage stands on the model's chips that have a floor, in key order.
    assert_eq!(w.sage_chips, vec![chip(C1), chip(C3)]);
    // The same model reporting ready again does not rebuild its world.
    let again = run.event(Event::ModelReady {
        model: EntityKey::model("llama"),
        chips: vec![chip(C1)],
    });
    assert_eq!(again, vec![]);
    assert_eq!(run.world().id, first + 1);
    assert_eq!(run.world().sage_chips, vec![chip(C1), chip(C3)]);

    // A model that names no chips could be on any of them: its sage stands
    // on every floor.
    run.event(Event::ModelReady {
        model: EntityKey::model("anywhere"),
        chips: vec![],
    });
    assert_eq!(run.world().chips, vec![chip(C1), chip(C2), chip(C3)]);
    assert_eq!(run.world().sage_chips, run.world().chips);
    // A model's going away ends its dungeon, and the world that follows has
    // no sage chips.
    run.event(Event::ModelGone {
        model: EntityKey::model("anywhere"),
    });
    assert_eq!(run.world().chips.len(), 3);
    assert!(run.world().sage_chips.is_empty());
}

#[test]
fn a_model_going_away_ends_its_dungeon() {
    let mut run = Run::new(12_000);
    run.start();
    run.event(Event::ModelReady {
        model: EntityKey::model("llama"),
        chips: vec![chip(C2)],
    });
    let id = run.world().id;
    // Another model going away leaves this world alone.
    assert_eq!(
        run.event(Event::ModelGone {
            model: EntityKey::model("other"),
        }),
        vec![]
    );
    let out = run.event(Event::ModelGone {
        model: EntityKey::model("llama"),
    });
    assert_eq!(
        out,
        vec![ended(id, EndWhy::ModelDown), born(id + 1, Biome::Dungeon)]
    );
    // The world that follows covers every chip and belongs to no model, so
    // the same model going away again does nothing.
    assert_eq!(run.world().chips.len(), 3);
    assert_eq!(
        run.event(Event::ModelGone {
            model: EntityKey::model("llama"),
        }),
        vec![]
    );
}

#[test]
fn a_reset_ends_the_world_and_a_new_one_follows() {
    let mut run = Run::new(12_000);
    let first = run.start();
    let old_seed = run.world().seed;
    let out = run.event(Event::Reset {
        chips: vec![chip(C2)],
    });
    assert_eq!(
        out,
        vec![ended(first, EndWhy::Reset), born(first + 1, Biome::Dungeon)]
    );
    let w = run.world();
    assert_eq!(w.id, first + 1);
    assert_eq!(w.born_tick, run.tick);
    assert_ne!(w.seed, old_seed);
    assert_eq!(w.floors.len(), 3);
    // A reset of a chip outside the world leaves it alone. A model's
    // dungeon spans every chip the topology lists, so only a chip the
    // topology does not list is outside it.
    run.event(Event::ModelReady {
        model: EntityKey::model("llama"),
        chips: vec![chip(C1)],
    });
    let id = run.world().id;
    assert_eq!(
        run.event(Event::Reset {
            chips: vec![chip("0000:09:00.0")],
        }),
        vec![]
    );
    // A reset that names no chips could be any chip, so it ends the world.
    assert_eq!(
        run.event(Event::Reset { chips: vec![] }),
        vec![ended(id, EndWhy::Reset), born(id + 1, Biome::Dungeon)]
    );
}

#[test]
fn a_quiet_season_ends_the_world_in_a_dream() {
    let season = 100;
    let mut run = Run::new(season);
    let first = run.start();
    let born_at = run.world().born_tick;
    // Wobbling samples do not count as activity, so a season after the
    // birth the world ends.
    while run.tick < born_at + season - 1 {
        assert_eq!(run.quiet_tick(), vec![]);
    }
    assert_eq!(
        run.quiet_tick(),
        vec![ended(first, EndWhy::Season), born(first + 1, Biome::Dream)]
    );
    let dream_at = run.tick;
    assert_eq!(run.world().born_tick, dream_at);
    // A bus event restarts the quiet count: the dream lasts a season from
    // the event.
    for _ in 0..50 {
        run.quiet_tick();
    }
    run.event(Event::RequestsCompleted {
        model: EntityKey::model("m"),
        n: 3,
    });
    let active_at = run.tick;
    while run.tick < active_at + season - 1 {
        assert_eq!(run.quiet_tick(), vec![]);
    }
    assert_eq!(
        run.quiet_tick(),
        vec![
            ended(first + 1, EndWhy::Season),
            born(first + 2, Biome::Dream)
        ]
    );
}

#[test]
fn a_world_always_exists_across_ten_thousand_quiet_ticks() {
    let season = 100;
    let mut run = Run::new(season);
    let mut events = Vec::new();
    for _ in 0..10_000 {
        events.extend(run.quiet_tick());
        if run.tick >= 5 {
            assert!(run.lc.world().is_some(), "no world at tick {}", run.tick);
        }
    }
    // Born at tick 5, then a dream every 100 ticks: ends at 105, 205, ...,
    // 9905, which is 99 seasons.
    assert_eq!(events.len(), 1 + 2 * 99);
    assert_eq!(events[0], born(1, Biome::Dungeon));
    for (k, pair) in events[1..].chunks(2).enumerate() {
        let id = k as u64 + 1;
        assert_eq!(
            pair,
            [ended(id, EndWhy::Season), born(id + 1, Biome::Dream)]
        );
    }
    assert_eq!(run.world().id, 100);
}

#[test]
fn a_training_run_births_and_closes_a_forge() {
    let mut run = Run::new(12_000);
    let first = run.start();
    let out = run.feed(train_step(1));
    assert_eq!(
        out,
        vec![
            ended(first, EndWhy::Superseded),
            born(first + 1, Biome::Forge)
        ]
    );
    let forge = first + 1;
    // More training signals keep the same forge.
    for step in 2..100 {
        assert_eq!(run.feed(train_step(step)), vec![]);
        let loss = Observation::Sample {
            entity: EntityKey::training_run("r1"),
            signal: "toplike.train.loss".into(),
            value: 2.0,
            bits: None,
        };
        assert_eq!(run.feed(loss), vec![]);
        run.quiet_tick();
    }
    assert_eq!(run.world().id, forge);
    // The last training signal arrived one tick before the loop's last
    // quiet tick. 600 ticks without one close the forge.
    let last = run.tick - 1;
    while run.tick < last + FORGE_TIMEOUT_TICKS - 1 {
        assert_eq!(run.quiet_tick(), vec![]);
    }
    assert_eq!(
        run.quiet_tick(),
        vec![
            ended(forge, EndWhy::RunEnded),
            born(forge + 1, Biome::Dungeon)
        ]
    );
    // A new run later births a new forge.
    assert_eq!(
        run.feed(train_step(1)),
        vec![
            ended(forge + 1, EndWhy::Superseded),
            born(forge + 2, Biome::Forge)
        ]
    );
}

#[test]
fn two_same_shaped_chips_get_different_floors_from_one_world_seed() {
    let mut run = Run::new(12_000);
    run.start();
    let w = run.world();
    // All three chips have one spec shape; only the key differs.
    assert_ne!(w.floors[0], w.floors[1]);
    assert_ne!(w.floors[1], w.floors[2]);
    assert_ne!(w.floors[0], w.floors[2]);
    assert_eq!(w.floors[1], expected_floor(C2, w.seed));
}

#[test]
fn a_machine_with_no_chips_still_gets_a_world_with_a_floor() {
    let mut run = Run::new(12_000);
    run.topo.clear();
    run.start();
    assert_eq!(run.world().floors.len(), 1);
    assert_eq!(run.world().chips.len(), 1);
}

#[test]
fn the_same_observations_give_the_same_worlds() {
    fn script() -> (Vec<SagaEvent>, World) {
        let mut run = Run::new(50);
        let mut out = Vec::new();
        for i in 0..400u64 {
            out.extend(run.quiet_tick());
            if i == 30 {
                out.extend(run.event(Event::Reset {
                    chips: vec![chip(C1)],
                }));
            }
            if i == 120 {
                out.extend(run.feed(train_step(1)));
            }
        }
        (out, run.world().clone())
    }
    let (a, wa) = script();
    let (b, wb) = script();
    assert_eq!(a, b);
    assert_eq!(wa, wb);
}
