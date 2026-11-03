//! Events the saga core publishes.
//!
//! Each [`SagaEvent`] becomes one `saga.*` kind on the bus (spec section 6):
//! `WorldBorn` is `saga.world_born` and `WorldEnded` is `saga.world_ended`.
//! The core only makes these values. Publishing them is the host's job.
//!
//! This module starts with the world events (Task 1.8). Later tasks add
//! variants to the same enum: hero and quest events (`HeroBorn`,
//! `QuestStarted`, `QuestDone`, Task 1.9), the chronicle's `Beat` (Task
//! 1.10), and monster and treasure events (phase 2). To add one, add a
//! variant with named fields, starting with `world: u64` so every event says
//! which world it belongs to. The enum derives only `Debug`, `Clone` and
//! `PartialEq`, so a later variant may carry an `f32` intensity without
//! changing the derives.

/// The kind of place a world is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Biome {
    /// The usual world: a dungeon on the chips' floors. A model reaching
    /// `Ready` makes one on that model's chips.
    Dungeon,
    /// A training run's world. It lasts while the run's signals keep coming.
    Forge,
    /// The world that follows a quiet season.
    Dream,
}

/// Why a world ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndWhy {
    /// The model whose `Ready` made this dungeon went away (`ModelGone`).
    ModelDown,
    /// A chip of this world was reset.
    Reset,
    /// The machine was quiet for a whole season.
    Season,
    /// The hero died. Task 1.9 raises this one; the lifecycle never does.
    HeroDied,
    /// A new world began from a signal (a model became ready, or a training
    /// run started) and took this world's place. The brief's list has no
    /// cause for this case, so this variant was added for it.
    Superseded,
    /// This forge's training run stopped sending signals for 600 ticks.
    /// The brief's list has no cause for this case either.
    RunEnded,
}

/// One event the saga publishes. See the module comment for how to extend it.
#[derive(Debug, Clone, PartialEq)]
pub enum SagaEvent {
    /// World `world` began. World ids count up from 1.
    WorldBorn { world: u64, biome: Biome },
    /// World `world` ended. When a new world follows at once, its
    /// `WorldBorn` comes right after this event.
    WorldEnded { world: u64, why: EndWhy },
}
