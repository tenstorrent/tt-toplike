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

#[test]
fn zero_cap_keeps_one_sample() {
    let mut h = History::new(0);
    let k = EntityKey::chip("0000:01:00.0");
    h.record(&k, "power", 1.0);
    h.record(&k, "power", 2.0);
    assert_eq!(h.series(&k, "power"), [2.0]);
}

#[test]
fn retain_entities_drops_dead_entities_of_that_kind_only() {
    use super::history::History;
    let mut h = History::new(10);
    let (a, b) = (
        EntityKey::chip("0000:01:00.0"),
        EntityKey::chip("0000:02:00.0"),
    );
    let host = EntityKey::host("box");
    for k in [&a, &b, &host] {
        h.record(k, "x", 1.0);
    }
    let live: std::collections::HashSet<EntityKey> = [a.clone()].into_iter().collect();
    h.retain_entities(tt_toplike_bus::EntityKind::Chip, &live);
    assert_eq!(h.series(&a, "x"), [1.0]);
    assert!(h.series(&b, "x").is_empty(), "the dead chip is gone");
    assert_eq!(h.series(&host, "x"), [1.0], "other kinds are left alone");
    assert_eq!(h.series_count(), 2);
}

#[test]
fn the_series_count_is_capped_and_existing_series_keep_recording() {
    use super::history::{History, MAX_SERIES};
    let mut h = History::new(10);
    let first = EntityKey::chip("0000:00:00.0");
    h.record(&first, "x", 1.0);
    for i in 0..MAX_SERIES + 50 {
        h.record(&EntityKey::chip(&format!("0000:{i:02x}:00.0")), "y", 1.0);
    }
    assert_eq!(h.series_count(), MAX_SERIES);
    h.record(&first, "x", 2.0);
    assert_eq!(
        h.series(&first, "x"),
        [1.0, 2.0],
        "an old series still records"
    );
    assert_eq!(h.series_count(), MAX_SERIES);
}
