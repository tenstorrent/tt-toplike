// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! A capped history of bus signals, kept by the host and read by plugins.

use std::collections::{HashMap, HashSet};
use tt_toplike_bus::{EntityKey, EntityKind};

/// How many samples each series keeps unless the host asks otherwise.
pub const DEFAULT_CAP: usize = 240;

/// The most series (entity and signal pairs) a history holds. A viz that reads
/// processes or models on a box with a lot of churn would otherwise add a
/// series for every entity it ever saw. When the history is full, a record for
/// a new series is refused and the viz draws no sparkline for it; series that
/// already exist keep recording. The host also drops the series of entities
/// that have left the world ([`History::retain_entities`]), which makes room.
pub const MAX_SERIES: usize = 4096;

/// Recent values per (entity, signal), oldest first.
///
/// The host calls [`History::record`] once per tick for the signals it
/// tracks; plugins read through [`History::series`] on `VizCtx::history`.
#[derive(Debug, Clone)]
pub struct History {
    cap: usize,
    series: HashMap<(EntityKey, String), Vec<f64>>,
}

impl Default for History {
    /// A history that keeps [`DEFAULT_CAP`] samples per series.
    fn default() -> Self {
        History::new(DEFAULT_CAP)
    }
}

impl History {
    /// A history that keeps at most `cap` samples per series (at least 1).
    pub fn new(cap: usize) -> History {
        History {
            cap: cap.max(1),
            series: HashMap::new(),
        }
    }

    /// Append `v` to the series for `signal` on `key`, dropping the oldest
    /// sample when the series is over the cap. A new series is refused when
    /// the history already holds [`MAX_SERIES`]. Called by the host each tick.
    pub fn record(&mut self, key: &EntityKey, signal: &str, v: f64) {
        let id = (key.clone(), signal.to_string());
        if self.series.len() >= MAX_SERIES && !self.series.contains_key(&id) {
            return;
        }
        let s = self.series.entry(id).or_default();
        s.push(v);
        if s.len() > self.cap {
            let excess = s.len() - self.cap;
            s.drain(..excess);
        }
    }

    /// Drop every series of an entity of `kind` that is not in `live`. The
    /// host calls it after recording, with the entities the world still holds,
    /// so a process or model that has gone does not keep its series forever.
    /// Series of other kinds are left alone.
    pub fn retain_entities(&mut self, kind: EntityKind, live: &HashSet<EntityKey>) {
        self.series
            .retain(|(key, _), _| key.kind() != kind || live.contains(key));
    }

    /// How many series are held.
    pub fn series_count(&self) -> usize {
        self.series.len()
    }

    /// The samples for `signal` on `key`, oldest first. Empty when nothing
    /// was recorded. Called by plugins while rendering.
    pub fn series(&self, key: &EntityKey, signal: &str) -> &[f64] {
        self.series
            .get(&(key.clone(), signal.to_string()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
