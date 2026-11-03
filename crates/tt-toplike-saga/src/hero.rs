//! The hero: where it stands, how healthy it is, and how it walks.
//!
//! A world has one hero (spec section 4). On autopilot the hero takes a
//! [`Quest`] from [`crate::quest::plan`] and walks towards the quest's goal,
//! one cell per [`Hero::step`], along a path from [`astar`].
//!
//! # Walking
//!
//! * **Moves.** One cell per step, north, south, east or west, onto a cell
//!   for which [`Cell::walkable`](crate::maze::Cell::walkable) is true. The
//!   hero never leaves the grid: [`Floor::cell`] reports `Wall` off the grid,
//!   so such a move is never taken.
//! * **Planning.** The hero searches for a path when its quest's goal
//!   changes, when it finds itself off the start of its path (something
//!   moved it, for example a person steering in a later phase), and when the
//!   stuck detector fires.
//! * **The stuck detector.** A step that does not bring the hero closer to
//!   the goal than it has been is an idle step. After [`STUCK_STEPS`] idle
//!   steps in a row the next step searches again. Two cases make idle steps:
//!   the next cell of the path has become a wall (the hero waits, in case the
//!   block is brief, then routes around it), and there is no path at all (the
//!   hero stays put and retries every [`STUCK_STEPS`] steps, so an
//!   unreachable goal costs one search per twenty steps). [`StepResult::no_path`]
//!   tells the caller that the goal cannot be reached now, so it can plan a
//!   different quest.
//!
//! # Vitality
//!
//! [`vitality`] turns a chip's trailing statistics into a number from 0 to 1
//! (spec sections 3.4 and 3.10). See its comment for the formula.
//!
//! # Determinism
//!
//! [`astar`] uses integer costs and a `BTreeSet` open set ordered by
//! `(f, y, x)`, so equal-cost cells are expanded lowest `(y, x)` first and
//! the same floor always gives the same path. [`vitality`] uses only `+ - *
//! /` and comparisons.

use std::collections::BTreeSet;

use tt_toplike_bus::EntityKey;

use crate::maze::Floor;
use crate::quest::Quest;
use crate::stats::{Trail, TrailBook};

/// Idle steps in a row after which the hero searches for a path again.
pub const STUCK_STEPS: u32 = 20;

/// The trail series that says whether a chip's ARC heartbeat is alive.
///
/// This is a series the saga keeps, derived from the bus signal
/// `toplike.heartbeat` (a counter the bus records only when it changes). The
/// driver pushes 1.0 for the chip on every tick at which the counter has
/// advanced within the last [`HEARTBEAT_GRACE_TICKS`] ticks, and 0.0 on every
/// other tick once the chip has reported a heartbeat. The trail's mean is
/// then the recent fraction of live ticks. The grace window keeps a counter
/// that advances less often than once a tick from reading as a stall.
pub const HEARTBEAT_ALIVE: &str = "saga.heartbeat_alive";

/// Ticks a heartbeat change keeps the chip alive (2 s at 10 Hz). See
/// [`HEARTBEAT_ALIVE`].
pub const HEARTBEAT_GRACE_TICKS: u64 = 20;

/// The AI clock in MHz, pushed as each bus sample arrives.
pub const AI_CLOCK: &str = "tt_ai_clock_mhz";

/// The ASIC temperature in degrees Celsius. Push each bus sample with
/// [`push_temperature`], which also keeps [`TEMPERATURE_BASELINE`].
pub const ASIC_TEMPERATURE: &str = "tt_asic_temperature_celsius";

/// The temperature's slow baseline: a series the saga keeps. Each
/// [`push_temperature`] pushes the [`ASIC_TEMPERATURE`] trail's mean (after
/// the new sample) onto this trail, so its mean is the fast mean smoothed a
/// second time. It lags the fast mean while the temperature moves and equals
/// it once the temperature has settled.
pub const TEMPERATURE_BASELINE: &str = "saga.temperature_baseline";

/// Weight of the clock term in [`vitality`]. The temperature term gets the
/// rest.
const CLOCK_WEIGHT: f64 = 0.6;

/// A clock ratio at or above this counts as full health in [`vitality`].
/// 0.9 is the spec's throttle line (spec 3.8: a clock below 90% of its own
/// trailing maximum).
const BAND_TOP: f64 = 0.9;

/// A clock ratio at or below this counts as no health in [`vitality`].
const BAND_BOTTOM: f64 = 0.5;

/// The temperature counts as rising when its fast mean is more than 2%
/// above its baseline. The margin keeps low-bit wobble on a quiet chip from
/// counting as a rise.
const RISE_LINE: f64 = 1.02;

/// A fast mean 20% or more above the baseline gives no heat health.
const RISE_TOP: f64 = 1.2;

/// Why something happened in the story: the entity, the bus signal, and the
/// record's ordinal in the stream the core was fed (spec 3.5). Every quest
/// and every chronicle beat carries one.
#[derive(Debug, Clone, PartialEq)]
pub struct Cause {
    pub entity: EntityKey,
    /// The bus signal or event kind. Empty when nothing asked for the
    /// quest (see [`crate::quest::plan`]).
    pub signal: String,
    pub ordinal: u64,
}

/// What one [`Hero::step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepResult {
    /// The hero moved one cell.
    pub moved: bool,
    /// The hero stands on the quest's goal after this step.
    pub arrived: bool,
    /// The step ran a path search.
    pub replanned: bool,
    /// No path to the goal exists right now. The hero did not move.
    pub no_path: bool,
}

/// The hero of a world.
#[derive(Debug, Clone, PartialEq)]
pub struct Hero {
    /// `(x, y)` on the current floor.
    pub pos: (u8, u8),
    /// Index into the world's floors.
    pub floor: u8,
    /// 0 to 1. See [`vitality`].
    pub vitality: f32,
    /// The tick at which the hero was born.
    pub born_tick: u64,
    pub name: String,
    /// The walk in progress.
    walk: Walk,
}

/// The hero's walk towards one goal.
#[derive(Debug, Clone, PartialEq, Default)]
struct Walk {
    /// The goal this walk is for. `None` before the first step.
    goal: Option<(u8, u8)>,
    /// The cells still to walk, last one next (reversed so a step pops).
    path: Vec<(u8, u8)>,
    /// The last search found no path.
    no_path: bool,
    /// The fewest cells left to walk so far on this walk.
    best: usize,
    /// Idle steps in a row.
    idle: u32,
}

impl Hero {
    /// A hero at full vitality standing at `pos` on floor `floor`.
    pub fn new(name: String, floor: u8, pos: (u8, u8), born_tick: u64) -> Hero {
        Hero {
            pos,
            floor,
            vitality: 1.0,
            born_tick,
            name,
            walk: Walk::default(),
        }
    }

    /// Takes one step towards `quest.goal` on `floor`. See the module comment
    /// for when the hero searches and how the stuck detector works.
    pub fn step(&mut self, floor: &Floor, quest: &Quest) -> StepResult {
        let mut r = StepResult::default();
        let goal = quest.goal;

        // A new goal starts a new walk, which searches at once.
        let fresh = self.walk.goal != Some(goal);
        if fresh {
            self.walk = Walk {
                goal: Some(goal),
                ..Walk::default()
            };
        }
        if self.pos == goal {
            self.walk.path.clear();
            r.arrived = true;
            return r;
        }

        // The next cell must be one step from where the hero stands. If it is
        // not (or the path ran out short of the goal) the hero was moved off
        // its path, and searches again at once.
        let off_path = !self.walk.no_path
            && self
                .walk
                .path
                .last()
                .is_none_or(|&next| manhattan(self.pos, next) != 1);
        if fresh || off_path || self.walk.idle >= STUCK_STEPS {
            r.replanned = true;
            self.walk.idle = 0;
            match astar(floor, self.pos, goal) {
                Some(mut path) => {
                    // Reverse, so `pop` gives the next cell, then drop
                    // the hero's own cell (now last).
                    path.reverse();
                    path.pop();
                    self.walk.best = path.len();
                    self.walk.path = path;
                    self.walk.no_path = false;
                }
                None => {
                    self.walk.path.clear();
                    self.walk.no_path = true;
                }
            }
        }

        if self.walk.no_path {
            r.no_path = true;
            self.walk.idle += 1;
            return r;
        }

        // Follow the path if its next cell can be stood on. A cell that has
        // become a wall is waited out (an idle step).
        let next = *self
            .walk
            .path
            .last()
            .expect("a found path reaches the goal");
        if floor.cell(next.0, next.1).walkable() {
            self.pos = next;
            self.walk.path.pop();
            r.moved = true;
        }
        let left = self.walk.path.len();
        if r.moved && left < self.walk.best {
            self.walk.best = left;
            self.walk.idle = 0;
        } else {
            self.walk.idle += 1;
        }
        r.arrived = self.pos == goal;
        r
    }
}

/// Manhattan distance between two cells.
fn manhattan(a: (u8, u8), b: (u8, u8)) -> u32 {
    a.0.abs_diff(b.0) as u32 + a.1.abs_diff(b.1) as u32
}

/// Maps a clock ratio onto 0 to 1: [`BAND_TOP`] or more gives 1,
/// [`BAND_BOTTOM`] or less gives 0, linear between.
fn band(r: f64) -> f64 {
    if r >= BAND_TOP {
        1.0
    } else if r <= BAND_BOTTOM {
        0.0
    } else {
        (r - BAND_BOTTOM) / (BAND_TOP - BAND_BOTTOM)
    }
}

/// Pushes one temperature sample for `chip`: the raw value onto the
/// [`ASIC_TEMPERATURE`] trail, then that trail's new mean onto the
/// [`TEMPERATURE_BASELINE`] trail. A NaN or infinite sample changes neither.
/// The driver calls this for every `tt_asic_temperature_celsius` sample.
pub fn push_temperature(trails: &mut TrailBook, chip: &EntityKey, celsius: f64) {
    if !celsius.is_finite() {
        return;
    }
    trails.push(chip, ASIC_TEMPERATURE, celsius);
    if let Some(mean) = trails.get(chip, ASIC_TEMPERATURE).map(Trail::mean) {
        trails.push(chip, TEMPERATURE_BASELINE, mean);
    }
}

/// The temperature's fast mean divided by its baseline's mean: above 1 while
/// the temperature rises, below 1 while it falls, 1 once it has settled.
/// 1.0 when either trail is missing or the baseline is not positive.
fn temperature_rise(trails: &TrailBook, chip: &EntityKey) -> f64 {
    match (
        trails.get(chip, ASIC_TEMPERATURE),
        trails.get(chip, TEMPERATURE_BASELINE),
    ) {
        (Some(fast), Some(base)) if base.mean() > 0.0 => fast.mean() / base.mean(),
        _ => 1.0,
    }
}

/// The hero's vitality on `chip`, from 0.0 to 1.0, read from `trails`.
///
/// ```text
/// vitality = alive * (0.6 * clock + 0.4 * heat)
/// rise     = temperature fast mean / temperature baseline mean
/// ratio    = clock mean / clock trailing max
/// clock    = band(ratio)   if ratio < 0.9 and rise > 1.02, else 1
/// heat     = 1 if rise <= 1.02, 0 if rise >= 1.2, linear between
/// ```
///
/// * `alive` is the mean of the [`HEARTBEAT_ALIVE`] trail, clamped to 0..1:
///   the recent fraction of ticks with a live heartbeat. Its mean moves 1/16
///   of the way on each tick, so a stall drains vitality within a few dozen
///   ticks and a restart restores it as fast.
/// * `rise` compares the [`ASIC_TEMPERATURE`] trail's mean (it moves 1/16 of
///   the way per sample) with the [`TEMPERATURE_BASELINE`] trail's mean (the
///   same smoothing applied a second time, see [`push_temperature`]).
/// * `clock` is the derived throttle of spec 3.8: the [`AI_CLOCK`] trail's
///   mean below 90% of its trailing maximum while the temperature rises. The
///   penalty is the band: 90% or more is full health, 50% or less is none,
///   linear between. A clock that falls while the temperature falls or stays
///   level is a chip going idle, and costs nothing.
/// * `heat` is the temperature margin against the chip's own trailing
///   behaviour (spec 3.10). Only a temperature climbing above its own
///   baseline costs heat health. Cooling costs nothing. A chip that has
///   settled at any temperature, hot or cool, has full heat health: the margin
///   is measured against the chip's own recent past, and a chip held steady
///   at a high temperature is a working chip. An absolute thermal danger is a
///   thermal trip, which phase 2 turns into a dragon.
///
/// How long a penalty lasts, from the constants. The rise needs both means to
/// meet again: after a step from 60 to 90 degrees, the rise falls back under
/// 1.02 about 50 samples after the step. The clock's trailing maximum loses
/// 1/4096 per sample, so a clock that settles at a fraction `f` of its old
/// maximum stays under the 90% line for about `4096 * ln(0.9 / f)` samples:
/// about 1,700 samples for an idle Blackhole (800 of 1350 MHz). That only
/// matters if the temperature rises during that time.
///
/// A series with no trail counts as full health, so a chip with no readings
/// has vitality 1.0. All the series are positive quantities. Faults, ECC
/// errors and link count (spec 3.10) join the formula when the cast that
/// uses them arrives in phase 2. Only `+ - * /` and comparisons are used.
pub fn vitality(trails: &TrailBook, chip: &EntityKey) -> f32 {
    let alive = trails
        .get(chip, HEARTBEAT_ALIVE)
        .map_or(1.0, |t| t.mean().clamp(0.0, 1.0));
    let rise = temperature_rise(trails, chip);
    let rising = rise > RISE_LINE;
    let clock = match trails.get(chip, AI_CLOCK) {
        Some(t) => {
            let ratio = t.ratio(t.mean());
            if rising && ratio < BAND_TOP {
                band(ratio)
            } else {
                1.0
            }
        }
        None => 1.0,
    };
    let heat = if rise <= RISE_LINE {
        1.0
    } else if rise >= RISE_TOP {
        0.0
    } else {
        (RISE_TOP - rise) / (RISE_TOP - RISE_LINE)
    };
    let v = alive * (CLOCK_WEIGHT * clock + (1.0 - CLOCK_WEIGHT) * heat);
    v.clamp(0.0, 1.0) as f32
}

/// The shortest 4-way path from `from` to `to` over walkable cells, both
/// ends included, or `None` when `to` is a wall, either end is off the grid,
/// or no path exists. `from == to` gives `[from]`. `from` itself need not be
/// walkable, so a hero on a cell that has become a wall can still walk off.
///
/// Costs are integers: 1 per step, with the Manhattan distance as the
/// estimate, which never overestimates, so the path is a shortest one. The
/// open set is a `BTreeSet` of `(f, y, x)`: among cells of equal `f`, the
/// cell with the lowest `(y, x)` is expanded first. A cell's parent changes only
/// on a strictly shorter route. Neighbours are visited west, east, north,
/// south. At most `w * h` cells are expanded.
pub fn astar(floor: &Floor, from: (u8, u8), to: (u8, u8)) -> Option<Vec<(u8, u8)>> {
    if from.0 >= floor.w || from.1 >= floor.h || !floor.cell(to.0, to.1).walkable() {
        return None;
    }
    if from == to {
        return Some(vec![from]);
    }
    let w = floor.w as usize;
    let n = w * floor.h as usize;
    let idx = |p: (u8, u8)| p.1 as usize * w + p.0 as usize;
    let mut g = vec![u32::MAX; n];
    let mut parent = vec![usize::MAX; n];
    let mut closed = vec![false; n];
    let mut open: BTreeSet<(u32, u8, u8)> = BTreeSet::new();

    g[idx(from)] = 0;
    open.insert((manhattan(from, to), from.1, from.0));
    while let Some((_, y, x)) = open.pop_first() {
        let here = (x, y);
        let k = idx(here);
        if here == to {
            // Walk the parents back to the start.
            let mut path = vec![here];
            let mut at = k;
            while parent[at] != usize::MAX {
                at = parent[at];
                path.push(((at % w) as u8, (at / w) as u8));
            }
            path.reverse();
            return Some(path);
        }
        closed[k] = true;
        // West, east, north, south. Wrapping keeps 0 - 1 off the grid.
        let next = [
            (x.wrapping_sub(1), y),
            (x.wrapping_add(1), y),
            (x, y.wrapping_sub(1)),
            (x, y.wrapping_add(1)),
        ];
        for p in next {
            if p.0 >= floor.w || p.1 >= floor.h || !floor.cell(p.0, p.1).walkable() {
                continue;
            }
            let pk = idx(p);
            let ng = g[k] + 1;
            if closed[pk] || ng >= g[pk] {
                continue;
            }
            if g[pk] != u32::MAX {
                open.remove(&(g[pk] + manhattan(p, to), p.1, p.0));
            }
            g[pk] = ng;
            parent[pk] = k;
            open.insert((ng + manhattan(p, to), p.1, p.0));
        }
    }
    None
}

#[cfg(test)]
#[path = "hero_tests.rs"]
mod hero_tests;
