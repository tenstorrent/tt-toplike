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
//! * **The seed gate.** No world is born before the gate opens. It opens at
//!   the first tick at which the seed pool has seen at least 64 samples (its
//!   ring is full), or at tick 5, whichever comes first (ledger rulings R11
//!   and R13). A world seeded from a ring that is still mostly zeros would
//!   repeat on every fresh start.
//! * **First world.** Born at the tick the gate opens. It is a dungeon on
//!   every chip, unless a signal that births a world (a model ready or a
//!   training start, below) arrived while the gate was closed. That birth
//!   is queued and takes the plain dungeon's place at the gate tick. A later
//!   queued birth replaces an earlier one, and a queued model's dungeon is
//!   dropped if that model goes away before the gate opens. A queued birth
//!   covers the chips the topology lists at the gate tick.
//! * **A model reaches `Ready`.** The current world ends
//!   ([`EndWhy::Superseded`]) and a dungeon is born on every chip, the same
//!   set as every other birth, so no chip leaves the view when a model comes
//!   up on part of the box (the party addendum, section 3.1). The world
//!   remembers the model's chips in [`World::sage_chips`]: the floors its
//!   sage stands on. A model that names no chips could be on any of them,
//!   so its sage stands on every floor. A repeated `Ready` for the model
//!   whose dungeon this already is changes nothing.
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
//! Floors are listed in chip-key order, at most [`MAX_FLOORS`] of them: a
//! world asked for on more chips keeps the first [`MAX_FLOORS`] keys. A chip
//! that the topology does not list gets the generic spec. A world with no chips at all gets one generic
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

use std::sync::Arc;

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

/// Chips the front door keeps (ledger ruling R20): their floor specs,
/// heartbeats and trails. A chip beyond the cap is observed but gets none of
/// these.
pub const MAX_CHIPS: usize = 64;

/// Floors one world generates (ledger ruling R22, amending R20). A world
/// asked for on more chips (a `ModelReady` naming many, or a birth on every
/// one of 64 kept chips) keeps the first [`MAX_FLOORS`] chips in key order,
/// and `World::chips` and `World::floors` stay index-aligned. 32 is the
/// largest real box (a Galaxy). Birth cost is one maze per floor, so this
/// bounds it: see "Bounds" in [`crate::saga`].
pub const MAX_FLOORS: usize = 32;

/// Entries of a `ModelReady`'s chip list that the saga reads (see
/// [`named_chips`]). A real model names at most 32 chips (a Galaxy), so this
/// is a cost cap for a hostile writer's list, which can be any length.
pub const MAX_NAMED_CHIPS: usize = MAX_CHIPS;

/// The chips a `ModelReady` names, as the saga reads them: the first
/// [`MAX_NAMED_CHIPS`] entries of the event's list, in the event's order,
/// then sorted and without repeats. Later entries are not read. The
/// lifecycle and the front door both read the list through this function,
/// so they agree on which chips a model named.
pub fn named_chips(chips: &[EntityKey]) -> Vec<EntityKey> {
    let mut named: Vec<EntityKey> = chips.iter().take(MAX_NAMED_CHIPS).cloned().collect();
    named.sort();
    named.dedup();
    named
}

/// The signal whose first appearance starts a training run. A copy of the
/// root crate's bus signal name, checked there (see `crate::saga`'s note on
/// copied names).
pub const TRAIN_STEP: &str = "toplike.train.step";

/// Every training signal starts with this prefix (checked against each
/// bus training signal in the root crate).
pub const TRAIN_PREFIX: &str = "toplike.train.";

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
    /// One floor per chip, in the order of `chips`. A world's floors never
    /// change after its birth, so they sit behind an `Arc`: a clone of the
    /// world (the driver clones the saga's state for every snapshot, ten
    /// times a second at most) copies one pointer and no cell (the party
    /// addendum, section 3.7). Two worlds compare equal when their floors
    /// hold the same cells, wherever the cells live.
    pub floors: Arc<[Floor]>,
    pub biome: Biome,
    /// The chips of the model whose `Ready` made this world, in key order:
    /// the floors its sage stands on. Every chip of the world when the
    /// model named none. Empty for a world no model made. Only chips with
    /// a floor here are listed: a chip the model names that the topology
    /// does not list, or one past [`MAX_FLOORS`], is left out, so this holds
    /// at most [`MAX_FLOORS`] keys. The model's chips are read through
    /// [`named_chips`]. The saga's front door then sets this field again
    /// from its own list of the model's chips, which can hold fewer (see
    /// "Bounds" in [`crate::saga`]), so the floors listed here are exactly
    /// the floors whose member has the model's sage hook.
    pub sage_chips: Vec<EntityKey>,
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
    /// A birth asked for while the seed gate was still closed. It is born at
    /// the gate tick in place of the plain first dungeon.
    pending: Option<Pending>,
}

/// A world a signal asks for. Before the seed gate opens it is queued, and
/// born at the gate tick. Its chips are not part of it: every birth covers
/// the chips the topology lists at the moment of birth, so a model ready
/// before its chips' first samples still gets a floor on each of them.
#[derive(Debug, Clone)]
struct Pending {
    biome: Biome,
    /// The model whose `Ready` asked for it, if one did.
    model: Option<EntityKey>,
    /// The chips that model named, through [`named_chips`] (empty when it
    /// named none). Unused when no model asked.
    model_chips: Vec<EntityKey>,
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
            pending: None,
        }
    }

    /// The current world, or `None` before the first one is born.
    pub fn world(&self) -> Option<&World> {
        self.world.as_ref()
    }

    /// The model whose `Ready` made the current world, if one did.
    pub fn model(&self) -> Option<&EntityKey> {
        self.model.as_ref()
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
                // The dungeon covers every chip; the model's own chips are
                // where its sage stands (see `sage_chips`).
                let want = Pending {
                    biome: Biome::Dungeon,
                    model: Some(model.clone()),
                    model_chips: named_chips(chips),
                };
                self.supersede(want, at, out);
            }
            Event::ModelGone { model } => {
                if self.world.is_none() {
                    // Before the gate opens: drop a queued dungeon for a
                    // model that is no longer there.
                    if self.pending.as_ref().and_then(|p| p.model.as_ref()) == Some(model) {
                        self.pending = None;
                    }
                } else if self.model.as_ref() == Some(model) {
                    self.end(EndWhy::ModelDown, out);
                    self.birth(Biome::Dungeon, all_chips(at.topo), None, at, out);
                }
            }
            Event::Reset { chips } => {
                let hit = match &self.world {
                    Some(w) => chips.is_empty() || chips.iter().any(|c| w.chips.contains(c)),
                    None => false,
                };
                if hit {
                    self.end(EndWhy::Reset, out);
                    self.birth(Biome::Dungeon, all_chips(at.topo), None, at, out);
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
            let want = Pending {
                biome: Biome::Forge,
                model: None,
                model_chips: Vec::new(),
            };
            self.supersede(want, at, out);
        }
    }

    /// A signal asks for a new world. Before the first world is born the
    /// request is queued until the seed gate opens. After that the current
    /// world ends ([`EndWhy::Superseded`]) and the new one is born at once.
    fn supersede(&mut self, want: Pending, at: &Birth, out: &mut Vec<SagaEvent>) {
        if self.world.is_none() {
            self.pending = Some(want);
            return;
        }
        self.end(EndWhy::Superseded, out);
        let sage = want.model.as_ref().map(|_| want.model_chips.as_slice());
        self.birth(want.biome, all_chips(at.topo), sage, at, out);
        self.model = want.model;
    }

    /// Handles a tick: the first world, the forge timeout and the season.
    fn on_tick(&mut self, at: &Birth, out: &mut Vec<SagaEvent>) {
        let Some(world) = &self.world else {
            if at.seeds.pushed() >= FULL_POOL || at.tick >= FIRST_WORLD_BY_TICK {
                let want = self.pending.take().unwrap_or_else(|| Pending {
                    biome: Biome::Dungeon,
                    model: None,
                    model_chips: Vec::new(),
                });
                let sage = want.model.as_ref().map(|_| want.model_chips.as_slice());
                self.birth(want.biome, all_chips(at.topo), sage, at, out);
                self.model = want.model;
            }
            return;
        };

        if let Some(last) = self.training {
            if at.tick.saturating_sub(last) >= FORGE_TIMEOUT_TICKS {
                self.training = None;
                if world.biome == Biome::Forge {
                    self.end(EndWhy::RunEnded, out);
                    self.birth(Biome::Dungeon, all_chips(at.topo), None, at, out);
                    return;
                }
            }
        }

        // Re-read the world: the forge rule above may have replaced it.
        let Some(world) = &self.world else { return };
        let quiet_since = world.born_tick.max(self.last_activity);
        if at.tick.saturating_sub(quiet_since) >= self.season_ticks {
            self.end(EndWhy::Season, out);
            self.birth(Biome::Dream, all_chips(at.topo), None, at, out);
        }
    }

    /// Ends the current world, if there is one, and forgets its model.
    fn end(&mut self, why: EndWhy, out: &mut Vec<SagaEvent>) {
        if let Some(w) = self.world.take() {
            out.push(SagaEvent::WorldEnded { world: w.id, why });
        }
        self.model = None;
    }

    /// Births a world of `biome` on `chips` and makes it current. `sage` is
    /// the chips a ready model named, for a world that model made (empty
    /// when it named none), and `None` for any other world.
    fn birth(
        &mut self,
        biome: Biome,
        chips: Vec<EntityKey>,
        sage: Option<&[EntityKey]>,
        at: &Birth,
        out: &mut Vec<SagaEvent>,
    ) {
        let mut chips = chips;
        chips.sort();
        chips.dedup();
        chips.truncate(MAX_FLOORS);
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
            .collect::<Arc<[Floor]>>();
        // The sage stands on the floors of the chips its model named, in
        // the world's key order; on every floor when it named none.
        let sage_chips = match sage {
            None => Vec::new(),
            Some([]) => chips.clone(),
            Some(named) => chips
                .iter()
                .filter(|c| named.binary_search(c).is_ok())
                .cloned()
                .collect(),
        };
        let id = self.next_id;
        self.next_id += 1;
        self.world = Some(World {
            id,
            seed,
            born_tick: at.tick,
            chips,
            floors,
            biome,
            sage_chips,
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
