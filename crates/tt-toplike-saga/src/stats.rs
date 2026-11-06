//! Trailing statistics: a running mean and a slowly decaying maximum.
//!
//! In the story these are the yardsticks the world judges a number against.
//! The hero's vitality (spec section 3.4) is a reading compared with its own
//! recent best, and the frost wyrm's clock ratio (spec section 3.10) is the
//! current clock divided by the highest clock seen lately. Both need a
//! "recent best" that forgets old peaks slowly and a smoothed level.
//!
//! The update rules are exact and use only `+ - * /` and comparisons, so the
//! results are bit-for-bit the same on every platform:
//!
//! * the mean moves 1/16 of the way toward each pushed value (the first push
//!   sets it outright);
//! * the max first loses 1/4096 of itself, then is raised to the pushed
//!   value if that is higher, so it never falls below the value just pushed;
//! * a push that is NaN or infinite is ignored and changes nothing.
//!
//! 1/16 and 4095/4096 are exact in binary floating point, so no rounding
//! error enters through the constants.

use std::collections::BTreeMap;
use tt_toplike_bus::EntityKey;

/// Weight of each new value in the running mean.
const MEAN_WEIGHT: f64 = 1.0 / 16.0;
/// Fraction of the max kept after each push: 1 - 1/4096.
const MAX_KEEP: f64 = 4095.0 / 4096.0;

/// A running mean and a decaying maximum for one series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trail {
    max: f64,
    mean: f64,
    n: u32,
}

impl Default for Trail {
    fn default() -> Self {
        Self::new()
    }
}

impl Trail {
    /// An empty trail: max and mean 0, no pushes.
    pub fn new() -> Self {
        Trail {
            max: 0.0,
            mean: 0.0,
            n: 0,
        }
    }

    /// Feeds one value. NaN and infinite values are ignored.
    pub fn push(&mut self, v: f64) {
        if !v.is_finite() {
            return;
        }
        // The mean starts at the first value, then moves 1/16 of the way.
        self.mean = if self.n == 0 {
            v
        } else {
            self.mean + (v - self.mean) * MEAN_WEIGHT
        };
        // Decay first, then lift to the pushed value: explicit comparison
        // instead of `f64::max`, so the result never depends on a library
        // tie-break. The max starts at 0, so a series that is only ever
        // negative keeps a max of 0.
        let decayed = self.max * MAX_KEEP;
        self.max = if v > decayed { v } else { decayed };
        self.n = self.n.saturating_add(1);
    }

    /// `v / max`, or 1.0 when the max is 0 (an idle series is at its best).
    pub fn ratio(&self, v: f64) -> f64 {
        if self.max <= 0.0 || !v.is_finite() {
            1.0
        } else {
            v / self.max
        }
    }

    /// The decaying maximum.
    pub fn max(&self) -> f64 {
        self.max
    }

    /// The running mean.
    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// How many finite values have been pushed (saturating).
    pub fn count(&self) -> u32 {
        self.n
    }
}

/// Distinct series names a [`TrailBook`] keeps for one entity through
/// [`TrailBook::push`] (ledger ruling R20). A value for a further name is not
/// trailed. A noisy or hostile local writer can send any number of signal
/// names; this keeps one chip's trails to a fixed size.
pub const MAX_TRAILS_PER_ENTITY: usize = 128;

/// Extra names per entity that only [`TrailBook::push_reserved`] may open.
/// The hero's vitality reads four series (the AI clock, the temperature, its
/// baseline and the heartbeat series, see [`crate::hero::vitality`]); they
/// are pushed with `push_reserved`, so a flood of other names cannot crowd
/// them out. One entity therefore holds at most
/// `MAX_TRAILS_PER_ENTITY + RESERVED_TRAILS` trails.
pub const RESERVED_TRAILS: usize = 4;

/// Trails for many series, keyed by entity and then by series name.
///
/// The book does not limit the number of entities: its callers decide which
/// entities get trails (the front door keeps trails only for the chips it
/// keeps, see `MAX_CHIPS` in [`crate::world`]). It does limit the names per
/// entity: see [`MAX_TRAILS_PER_ENTITY`] and [`RESERVED_TRAILS`].
#[derive(Debug, Clone, Default)]
pub struct TrailBook {
    trails: BTreeMap<EntityKey, BTreeMap<String, Trail>>,
}

impl TrailBook {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes `v` onto the trail for `(e, s)`, creating it on first use while
    /// `e` has fewer than [`MAX_TRAILS_PER_ENTITY`] trails. A value for a new
    /// name beyond that is dropped. An existing trail always takes the push.
    pub fn push(&mut self, e: &EntityKey, s: &str, v: f64) {
        self.push_within(e, s, v, MAX_TRAILS_PER_ENTITY);
    }

    /// Pushes `v` as [`TrailBook::push`] does, but may also open one of the
    /// [`RESERVED_TRAILS`] extra slots. Only the vitality inputs use it.
    pub fn push_reserved(&mut self, e: &EntityKey, s: &str, v: f64) {
        self.push_within(e, s, v, MAX_TRAILS_PER_ENTITY + RESERVED_TRAILS);
    }

    /// The push: an existing trail takes the value; a new one is made only
    /// while the entity has fewer than `limit` trails. The entity key and
    /// the name are cloned only when a trail is made.
    fn push_within(&mut self, e: &EntityKey, s: &str, v: f64, limit: usize) {
        #[cfg(test)]
        crate::cost::push();
        let names = match self.trails.get_mut(e) {
            Some(names) => names,
            None => self.trails.entry(e.clone()).or_default(),
        };
        if let Some(trail) = names.get_mut(s) {
            trail.push(v);
            return;
        }
        if names.len() >= limit {
            return;
        }
        let mut trail = Trail::new();
        trail.push(v);
        names.insert(s.to_string(), trail);
    }

    /// The trail for `(e, s)`, if anything was ever pushed to it.
    pub fn get(&self, e: &EntityKey, s: &str) -> Option<&Trail> {
        self.trails.get(e)?.get(s)
    }

    /// Entities with at least one trail (for the bounds tests).
    #[cfg(test)]
    pub(crate) fn entities(&self) -> usize {
        self.trails.len()
    }

    /// The longest entity key or series name held, in bytes (for the
    /// bounds tests).
    #[cfg(test)]
    pub(crate) fn longest_item(&self) -> usize {
        self.trails
            .iter()
            .flat_map(|(e, names)| {
                std::iter::once(e.as_str().len()).chain(names.keys().map(String::len))
            })
            .max()
            .unwrap_or(0)
    }

    /// The number of trails `e` has (for the bounds tests).
    #[cfg(test)]
    pub(crate) fn names(&self, e: &EntityKey) -> usize {
        self.trails.get(e).map_or(0, BTreeMap::len)
    }
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod stats_tests;
