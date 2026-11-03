//! Events the saga core publishes.
//!
//! Each [`SagaEvent`] becomes one `saga.*` kind on the bus (spec section 6):
//! `WorldBorn` is `saga.world_born` and `WorldEnded` is `saga.world_ended`.
//! The core only makes these values. Publishing them is the host's job.
//!
//! The world events come from Task 1.8 and the hero and quest events
//! (`HeroBorn`, `QuestStarted`, `QuestDone`) from Task 1.9. Later tasks add
//! variants to the same enum: the chronicle's `Beat` (Task 1.10, now
//! here), and monster and treasure events (phase 2). To add one, add a
//! variant with named fields, starting with `world: u64` so every event says
//! which world it belongs to. The enum derives only `Debug`, `Clone` and
//! `PartialEq`, so a later variant may carry an `f32` intensity without
//! changing the derives.

use tt_toplike_bus::EntityKey;

use crate::chronicle::Tone;
use crate::hero::Cause;
use crate::quest::Verb;

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
    /// The hero died. Hero death arrives with combat in phase 2 (plan task
    /// 2.2); the lifecycle never raises this one.
    HeroDied,
    /// A signal asked for a new world while this one was current: a model
    /// became ready, or a training run started. The new world took this
    /// world's place.
    Superseded,
    /// This forge's training run sent no training signal for 600 ticks, so
    /// the run is over and its forge closed.
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
    /// A hero named `name` was born in world `world`, on floor `floor` (an
    /// index into the world's floors).
    HeroBorn { world: u64, name: String, floor: u8 },
    /// The hero set out on a quest: `verb`, about `target`, because of
    /// `reason`.
    QuestStarted {
        world: u64,
        verb: Verb,
        target: Option<EntityKey>,
        reason: Cause,
    },
    /// The hero reached the goal of its `verb` quest.
    QuestDone { world: u64, verb: Verb },
    /// The chronicle wrote a line (`saga.beat`): its tone, its text, and
    /// the signal behind it. See [`crate::chronicle`].
    Beat {
        world: u64,
        tone: Tone,
        text: String,
        cause: Cause,
    },
}
