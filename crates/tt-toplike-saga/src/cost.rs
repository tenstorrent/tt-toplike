//! A work counter for the bounds tests. It exists only in the crate's own
//! test build (ledger ruling R9): a release or a normal debug build of the
//! crate has no counter and no call to it.
//!
//! The core counts four kinds of work unit, at the places that do the work:
//!
//! * `cells`: maze cells generated, `w * h` for each floor
//!   [`crate::maze::generate`] builds;
//! * `expansions`: cells [`crate::hero::astar`] takes from its open set;
//! * `pushes`: values pushed through [`crate::stats::TrailBook`];
//! * `events`: saga events raised by the front door.
//!
//! The counter is per thread, so tests running in parallel do not see each
//! other's work. No clock is read.

use std::cell::Cell;

/// Work counted since the last [`take`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Units {
    pub cells: u64,
    pub expansions: u64,
    pub pushes: u64,
    pub events: u64,
}

impl Units {
    /// Every unit, of all four kinds.
    pub fn total(&self) -> u64 {
        self.cells + self.expansions + self.pushes + self.events
    }
}

thread_local! {
    static UNITS: Cell<Units> = const { Cell::new(Units {
        cells: 0,
        expansions: 0,
        pushes: 0,
        events: 0,
    }) };
}

/// Adds `n` maze cells.
pub fn cells(n: u64) {
    UNITS.with(|u| {
        let mut v = u.get();
        v.cells += n;
        u.set(v);
    });
}

/// Adds one A* expansion.
pub fn expansion() {
    UNITS.with(|u| {
        let mut v = u.get();
        v.expansions += 1;
        u.set(v);
    });
}

/// Adds one trail push.
pub fn push() {
    UNITS.with(|u| {
        let mut v = u.get();
        v.pushes += 1;
        u.set(v);
    });
}

/// Adds one event.
pub fn event() {
    UNITS.with(|u| {
        let mut v = u.get();
        v.events += 1;
        u.set(v);
    });
}

/// The work counted since the last call, and a fresh start.
pub fn take() -> Units {
    UNITS.with(|u| u.replace(Units::default()))
}
