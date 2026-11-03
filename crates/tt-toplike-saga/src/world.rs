//! The world lifecycle: birth, death and rebirth from signals (spec 3.2).
//!
//! There is one world at a time. A [`Lifecycle`] watches observations and
//! decides when the current world ends and the next begins. Once the first
//! world is born there is always a world: every rule that ends a world births
//! the next one in the same step, and [`SagaEvent::WorldEnded`] is always
//! followed by [`SagaEvent::WorldBorn`].
//!
//! # The rules
//!
//! * **First world.** Born at the first tick at which the seed pool has seen
//!   at least 64 samples (its ring is full), or at tick 5, whichever comes
//!   first (ledger ruling R11). It is a dungeon on every chip. A signal that
//!   births a world (below) may come earlier; that world is then the first.
//! * **A model reaches `Ready`.** The current world ends
//!   ([`EndWhy::Superseded`]) and a dungeon is born on the model's chips. A
//!   repeated `Ready` for the model whose dungeon this already is changes
//!   nothing.
//! * **That model goes away (`ModelGone`).** Its dungeon ends
//!   ([`EndWhy::ModelDown`]) and a dungeon on every chip follows. A model
//!   that did not make the current world can go away without ending it.
//! * **A chip resets.** If a reset chip has a floor in the current world, or
//!   the reset names no chips (so it could be any of them), the world ends
//!   ([`EndWhy::Reset`]) and a dungeon on every chip follows.
//! * **A training run starts.** The first `toplike.train.step` sample while
//!   no run is being tracked starts a run. The current world ends
//!   ([`EndWhy::Superseded`]) and a forge is born on every chip. Any
//!   `toplike.train.*` sample keeps the run alive. After
//!   [`FORGE_TIMEOUT_TICKS`] ticks with no training sample the run is over:
//!   if the forge is still the current world it ends ([`EndWhy::RunEnded`])
//!   and a dungeon on every chip follows. A later `toplike.train.step`
//!   starts a new run and a new forge.
//! * **A quiet season.** When `season_ticks` ticks pass with no activity
//!   since the later of the world's birth and the last activity, the world
//!   ends ([`EndWhy::Season`]) and a dream is born on every chip. A dream
//!   that stays quiet ends the same way and another dream follows, so the
//!   cycle never stalls.
//!
//! # What counts as activity
//!
//! The quiet rule needs a definition that a quiet machine does not break. A
//! quiet machine's temperatures and power still wobble in their low bits
//! every sweep (spec 3.1), so a changed sample value cannot count. Activity
//! is therefore:
//!
//! * any bus event except `ViewChanged` (a view change is a person at the
//!   terminal, which says nothing about the machine), and
//! * any `toplike.train.*` sample (a training run is work even when it
//!   raises no events).
//!
//! Other samples never count. The rule uses only the order of observations
//! and the tick count, so a replay gives the same answer.
//!
//! # Seeds and floors
//!
//! A world's seed is `seeds.draw(ordinal)`, taken at the observation that
//! births it. Ordinals are distinct per record, so two births never share a
//! seed from one pool. Each chip's floor is
//! `maze::generate(spec, world_seed ^ fnv1a64(chip key bytes), 0.0)`, so two
//! chips of one shape still get different floors (ledger ruling R12).
//! Floors are listed in chip-key order. A chip that the topology does not
//! list gets the generic spec. A world with no chips at all gets one generic
//! floor for the placeholder chip [`NO_CHIP`], so later tasks always have a
//! floor to put the hero on.
//!
//! Traffic is 0.0 at birth, so every corridor is narrow.
//!
//! # The driver's side
//!
//! The caller pushes each observation into the [`SeedPool`] before it calls
//! [`Lifecycle::step`] with that observation. `tick` is the number of
//! [`Observation::Tick`]s seen so far, counting `obs` itself when it is a
//! tick, so the first tick is tick 1. `ordinal` is the observation's record
//! ordinal from [`crate::observe::Feed::ordinal`].

use tt_toplike_bus::{EntityKey, Event};

use crate::event::{Biome, EndWhy, SagaEvent};
use crate::maze::{self, Floor};
use crate::observe::Observation;
use crate::seed::{fnv1a64, SeedPool};
use crate::topology::FloorSpec;

/// Ticks without a training sample after which a run is over and its forge
/// closes. At the default 10 Hz this is one minute.
pub const FORGE_TIMEOUT_TICKS: u64 = 600;

/// The latest tick at which the first world is born even if the seed pool
/// is not yet full (ledger ruling R11).
pub const FIRST_WORLD_BY_TICK: u64 = 5;

/// Samples the seed pool must have seen for the first world to be born
/// before [`FIRST_WORLD_BY_TICK`]. This is the pool's ring length, so every
/// ring byte is a real reading.
pub const FULL_POOL: u64 = 64;

/// The chip id of the placeholder floor in a world with no chips.
pub const NO_CHIP: &str = "none";

/// The signal whose first appearance starts a training run.
const TRAIN_STEP: &str = "toplike.train.step";

/// Every training signal starts with this prefix.
const TRAIN_PREFIX: &str = "toplike.train.";

/// One world.
#[derive(Debug, Clone, PartialEq)]
pub struct World {
    /// Counts up from 1, one per world, in birth order.
    pub id: u64,
    /// The seed drawn from the pool at birth.
    pub seed: u64,
    /// The tick at which the world was born.
    pub born_tick: u64,
    /// The chip each floor belongs to: `floors[i]` is the floor of
    /// `chips[i]`. Ascending by key, never empty.
    pub chips: Vec<EntityKey>,
    /// One floor per chip, in the order of `chips`.
    pub floors: Vec<Floor>,
    pub biome: Biome,
}

/// Decides when worlds are born and end.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    /// Quiet ticks that end a world. At least 1.
    season_ticks: u64,
    /// The current world. `None` only before the first world is born.
    world: Option<World>,
    /// The id the next world gets.
    next_id: u64,
    /// The model whose `Ready` made the current world, if one did.
    model: Option<EntityKey>,
    /// The tick of the latest activity (see the module comment).
    last_activity: u64,
    /// The tick of the latest training sample while a run is tracked.
    /// `None` when no run is tracked.
    training: Option<u64>,
}

impl Lifecycle {
    /// A lifecycle with no world yet. A world ends after `season_ticks`
    /// quiet ticks; 0 is treated as 1 so a world lives at least one tick.
    pub fn new(season_ticks: u64) -> Lifecycle {
        Lifecycle {
            season_ticks: season_ticks.max(1),
            world: None,
            next_id: 1,
            model: None,
            last_activity: 0,
            training: None,
        }
    }

    /// The current world, or `None` before the first one is born.
    pub fn world(&self) -> Option<&World> {
        self.world.as_ref()
    }

    /// Takes in one observation and appends any world events to `out`. See
    /// the module comment for the rules and for what the arguments mean.
    pub fn step(
        &mut self,
        obs: &Observation,
        tick: u64,
        seeds: &SeedPool,
        ordinal: u64,
        topo: &[FloorSpec],
        out: &mut Vec<SagaEvent>,
    ) {
        // Everything a birth needs besides its biome and chips. The seed is
        // drawn only when a world is born.
        let at = Birth {
            tick,
            seeds,
            ordinal,
            topo,
        };
        match obs {
            Observation::Event(e) => self.on_event(e, &at, out),
            Observation::Sample { signal, .. } => self.on_sample(signal, &at, out),
            Observation::Tick => self.on_tick(&at, out),
        }
    }

    /// Handles a bus event.
    fn on_event(&mut self, e: &Event, at: &Birth, out: &mut Vec<SagaEvent>) {
        if !matches!(e, Event::ViewChanged { .. }) {
            self.last_activity = at.tick;
        }
        match e {
            Event::ModelReady { model, chips } => {
                if self.world.is_some() && self.model.as_ref() == Some(model) {
                    return;
                }
                self.end(EndWhy::Superseded, out);
                // An empty chip list says nothing about where the model
                // runs, so its dungeon covers every chip.
                let chips = if chips.is_empty() {
                    all_chips(at.topo)
                } else {
                    chips.clone()
                };
                self.birth(Biome::Dungeon, chips, at, out);
                self.model = Some(model.clone());
            }
            Event::ModelGone { model } => {
                if self.world.is_some() && self.model.as_ref() == Some(model) {
                    self.end(EndWhy::ModelDown, out);
                    self.birth(Biome::Dungeon, all_chips(at.topo), at, out);
                }
            }
            Event::Reset { chips } => {
                let hit = match &self.world {
                    Some(w) => chips.is_empty() || chips.iter().any(|c| w.chips.contains(c)),
                    None => false,
                };
                if hit {
                    self.end(EndWhy::Reset, out);
                    self.birth(Biome::Dungeon, all_chips(at.topo), at, out);
                }
            }
            _ => {}
        }
    }

    /// Handles a sample. Only training signals matter here.
    fn on_sample(&mut self, signal: &str, at: &Birth, out: &mut Vec<SagaEvent>) {
        if !signal.starts_with(TRAIN_PREFIX) {
            return;
        }
        self.last_activity = at.tick;
        let starts = self.training.is_none() && signal == TRAIN_STEP;
        if self.training.is_some() || starts {
            self.training = Some(at.tick);
        }
        if starts {
            self.end(EndWhy::Superseded, out);
            self.birth(Biome::Forge, all_chips(at.topo), at, out);
        }
    }

    /// Handles a tick: the first world, the forge timeout and the season.
    fn on_tick(&mut self, at: &Birth, out: &mut Vec<SagaEvent>) {
        let Some(world) = &self.world else {
            if at.seeds.pushed() >= FULL_POOL || at.tick >= FIRST_WORLD_BY_TICK {
                self.birth(Biome::Dungeon, all_chips(at.topo), at, out);
            }
            return;
        };

        if let Some(last) = self.training {
            if at.tick.saturating_sub(last) >= FORGE_TIMEOUT_TICKS {
                self.training = None;
                if world.biome == Biome::Forge {
                    self.end(EndWhy::RunEnded, out);
                    self.birth(Biome::Dungeon, all_chips(at.topo), at, out);
                    return;
                }
            }
        }

        // Re-read the world: the forge rule above may have replaced it.
        let Some(world) = &self.world else { return };
        let quiet_since = world.born_tick.max(self.last_activity);
        if at.tick.saturating_sub(quiet_since) >= self.season_ticks {
            self.end(EndWhy::Season, out);
            self.birth(Biome::Dream, all_chips(at.topo), at, out);
        }
    }

    /// Ends the current world, if there is one, and forgets its model.
    fn end(&mut self, why: EndWhy, out: &mut Vec<SagaEvent>) {
        if let Some(w) = self.world.take() {
            out.push(SagaEvent::WorldEnded { world: w.id, why });
        }
        self.model = None;
    }

    /// Births a world of `biome` on `chips` and makes it current.
    fn birth(&mut self, biome: Biome, chips: Vec<EntityKey>, at: &Birth, out: &mut Vec<SagaEvent>) {
        let mut chips = chips;
        chips.sort();
        chips.dedup();
        if chips.is_empty() {
            chips.push(EntityKey::chip(NO_CHIP));
        }
        let seed = at.seeds.draw(at.ordinal);
        let floors = chips
            .iter()
            .map(|chip| {
                let spec = at
                    .topo
                    .iter()
                    .find(|s| &s.chip == chip)
                    .cloned()
                    .unwrap_or_else(|| FloorSpec::generic(chip));
                maze::generate(&spec, floor_seed(seed, chip), 0.0)
            })
            .collect::<Vec<_>>();
        let id = self.next_id;
        self.next_id += 1;
        self.world = Some(World {
            id,
            seed,
            born_tick: at.tick,
            chips,
            floors,
            biome,
        });
        self.model = None;
        out.push(SagaEvent::WorldBorn { world: id, biome });
    }
}

/// What every birth in one step shares.
struct Birth<'a> {
    tick: u64,
    seeds: &'a SeedPool,
    ordinal: u64,
    topo: &'a [FloorSpec],
}

/// Every chip the topology lists (sorted and deduplicated by `birth`).
fn all_chips(topo: &[FloorSpec]) -> Vec<EntityKey> {
    topo.iter().map(|s| s.chip.clone()).collect()
}

/// The seed of `chip`'s floor in a world seeded `world_seed`: the world seed
/// XOR FNV-1a 64 of the chip key's bytes (ledger ruling R12).
fn floor_seed(world_seed: u64, chip: &EntityKey) -> u64 {
    world_seed ^ fnv1a64(chip.as_str().as_bytes())
}

#[cfg(test)]
#[path = "world_tests.rs"]
mod world_tests;
