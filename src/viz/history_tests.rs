// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for [`History`].

use super::history::History;
use tt_toplike_bus::EntityKey;

#[test]
fn history_caps_each_series() {
    let mut h = History::new(240);
    let k = EntityKey::chip("0000:01:00.0");
    for i in 1..=300 {
        h.record(&k, "power", i as f64);
    }
    let s = h.series(&k, "power");
    assert_eq!(s.len(), 240);
    assert_eq!(s[0], 61.0);
    assert_eq!(s[239], 300.0);
}

#[test]
fn unknown_series_is_empty() {
    let h = History::new(240);
    let k = EntityKey::chip("0000:01:00.0");
    assert!(h.series(&k, "nothing").is_empty());
}
