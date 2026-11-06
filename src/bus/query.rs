//! A query over the world a view reads: which entities carry which signals.

use tt_toplike_bus::{EntityKey, EntityKind, WorldRead};

/// The entities of `kind` that have a latest reading of every signal in
/// `signals`, in key order.
///
/// An entity with no reading of even one named signal is left out. An empty
/// `signals` slice names no requirement, so every entity of the kind is
/// returned. The function reads through the [`WorldRead`] trait only, so it
/// works the same on a local host's world and on an attached remote one.
///
/// ```ignore
/// // The chips that report both temperature and power:
/// let chips = entities_with(&view, EntityKind::Chip, &["temp_c", "power_w"]);
/// ```
pub fn entities_with(world: &dyn WorldRead, kind: EntityKind, signals: &[&str]) -> Vec<EntityKey> {
    world
        .entities(kind)
        .into_iter()
        .filter(|entity| {
            signals
                .iter()
                .all(|signal| world.reading(entity, signal).is_some())
        })
        .collect()
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
