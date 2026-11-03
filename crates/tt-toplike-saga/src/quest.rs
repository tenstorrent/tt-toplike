//! Quests: what the hero sets out to do, and why (spec section 4).
//!
//! A world offers [`Hook`]s: things a quest could be about, each with the
//! signal that raised it. [`plan`] picks one and turns it into a [`Quest`]
//! with a goal cell on the hero's floor. The hero then walks to the goal
//! (see [`crate::hero`]).
//!
//! # Phase 1 hooks
//!
//! * **Visit the sage** ([`Verb::VisitSage`]): a model is ready. Its sage
//!   stands at the floor's exit, the room farthest from the gate.
//! * **Explore** ([`Verb::Explore`]): always available. The goal is the
//!   centre of a room chosen from the world's random stream.
//! * **Rest** ([`Verb::Rest`]): always available. The goal is the centre of
//!   the room nearest the hero.
//!
//! Phase 2 adds slay, escort, cleanse and rebuild hooks, which carry a
//! target entity (a monster, a request stream, a vault, a link). The verbs
//! exist now so the enum does not change shape later. [`plan`] passes over
//! them until their placement rules exist.
//!
//! # How [`plan`] chooses
//!
//! 1. The first visit-the-sage hook, unless the hero already stands at the
//!    sage (then it moves on).
//! 2. If the hero's vitality is below [`REST_BELOW`], the first rest hook.
//! 3. The first explore hook.
//! 4. The first rest hook.
//! 5. With none of these, the hero rests where it stands, and the quest's
//!    cause names the floor's chip with an empty signal, because no signal
//!    asked for it.
//!
//! Hooks are read in slice order, and only an explore quest draws from the
//! random stream (one draw), so the same world, hero, hooks and generator
//! state give the same quest.

use tt_toplike_bus::EntityKey;

use crate::hero::{Cause, Hero};
use crate::maze::{Floor, Rect};
use crate::rng::Pcg32;
use crate::world::World;

/// Below this vitality a hero with a rest hook rests before exploring.
pub const REST_BELOW: f32 = 0.5;

/// What a quest is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Walk to a room.
    Explore,
    /// Walk to the nearest room and recover.
    Rest,
    /// Walk to a ready model's sage.
    VisitSage,
    /// Phase 2: fight a monster.
    Slay,
    /// Phase 2: walk with a request stream.
    Escort,
    /// Phase 2: clear a corrupted vault.
    Cleanse,
    /// Phase 2: repair a severed bridge.
    Rebuild,
}

/// Something a world offers as a quest. Phase 1 hooks are visit-the-sage,
/// explore and rest. Phase 2 adds slay, escort, cleanse and rebuild hooks;
/// new kinds of hook are new [`Verb`]s with a `target`, so this shape stays.
#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    pub verb: Verb,
    /// The entity the quest is about, for example the ready model a sage is
    /// named after. `None` for explore and rest.
    pub target: Option<EntityKey>,
    /// The signal that raised the hook.
    pub reason: Cause,
}

/// A quest: a verb, the entity it is about, the signal behind it, and the
/// cell the hero walks to.
#[derive(Debug, Clone, PartialEq)]
pub struct Quest {
    pub verb: Verb,
    pub target: Option<EntityKey>,
    pub reason: Cause,
    /// `(x, y)` on the hero's floor.
    pub goal: (u8, u8),
    /// The name a view prints for the target, or `None` to print none.
    /// [`plan`] leaves it `None`. The front door fills it when the quest
    /// starts, after the quest's chronicle line is written or left
    /// unwritten: a hardware key's id, or for a sage the model's real name
    /// in real mode and the chronicle's alias for it in generated mode (the
    /// party addendum, section 3.3). So no real name reaches it in
    /// generated mode.
    pub target_label: Option<String>,
}

/// Picks the hero's next quest from `hooks`. See the module comment for the
/// order. A hero whose floor index is past the world's floors plans on the
/// first floor; a world with no floors at all gives a rest where the hero
/// stands.
pub fn plan(world: &World, hero: &Hero, hooks: &[Hook], rng: &mut Pcg32) -> Quest {
    let floor = world
        .floors
        .get(hero.floor as usize)
        .or(world.floors.first());
    let first = |verb: Verb| hooks.iter().find(|h| h.verb == verb);
    let quest = |hook: &Hook, goal: (u8, u8)| Quest {
        verb: hook.verb,
        target: hook.target.clone(),
        reason: hook.reason.clone(),
        goal,
        target_label: None,
    };

    if let Some(floor) = floor {
        if let Some(sage) = first(Verb::VisitSage) {
            if hero.pos != floor.exit {
                return quest(sage, floor.exit);
            }
        }
        let rest = first(Verb::Rest);
        if hero.vitality < REST_BELOW {
            if let Some(hook) = rest {
                return quest(hook, nearest_room(&floor.rooms, hero.pos));
            }
        }
        if let Some(hook) = first(Verb::Explore) {
            return quest(hook, explore_goal(floor, hero.pos, rng));
        }
        if let Some(hook) = rest {
            return quest(hook, nearest_room(&floor.rooms, hero.pos));
        }
    }

    // Nothing to do: rest in place. The chip of the hero's floor is the
    // entity; no signal asked for this.
    let chip = world
        .chips
        .get(hero.floor as usize)
        .or(world.chips.first())
        .cloned()
        .unwrap_or_else(|| EntityKey::chip(crate::world::NO_CHIP));
    Quest {
        verb: Verb::Rest,
        target: None,
        reason: Cause {
            entity: chip,
            signal: String::new(),
            ordinal: 0,
        },
        goal: hero.pos,
        target_label: None,
    }
}

/// The centre of a room picked with one draw from `rng`. If that is where
/// the hero stands and there is another room, the next room is taken.
fn explore_goal(floor: &Floor, pos: (u8, u8), rng: &mut Pcg32) -> (u8, u8) {
    let n = floor.rooms.len();
    if n == 0 {
        return pos;
    }
    let i = rng.range(0, n as u32) as usize;
    let goal = floor.rooms[i].center();
    if goal == pos && n > 1 {
        floor.rooms[(i + 1) % n].center()
    } else {
        goal
    }
}

/// The centre of the room whose centre is nearest `pos` by Manhattan
/// distance, lowest index on ties, or `pos` itself with no rooms.
fn nearest_room(rooms: &[Rect], pos: (u8, u8)) -> (u8, u8) {
    let d = |r: &Rect| {
        let c = r.center();
        c.0.abs_diff(pos.0) as u32 + c.1.abs_diff(pos.1) as u32
    };
    rooms
        .iter()
        .min_by_key(|r| d(r))
        .map_or(pos, |r| r.center())
}
