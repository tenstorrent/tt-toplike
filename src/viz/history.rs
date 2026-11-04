// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! A capped history of bus signals, kept by the host and read by plugins.

use std::collections::HashMap;
use tt_toplike_bus::EntityKey;

/// How many samples each series keeps unless the host asks otherwise.
pub const DEFAULT_CAP: usize = 240;

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
    /// sample when the series is over the cap. Called by the host each tick.
    pub fn record(&mut self, key: &EntityKey, signal: &str, v: f64) {
        let s = self
            .series
            .entry((key.clone(), signal.to_string()))
            .or_default();
        s.push(v);
        if s.len() > self.cap {
            let excess = s.len() - self.cap;
            s.drain(..excess);
        }
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
