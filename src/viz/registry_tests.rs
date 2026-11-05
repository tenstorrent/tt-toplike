// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for [`Registry`]. Each builds its own registry, so none shares the
//! global one.

use super::ctx::{Viz, VizCtx};
use super::registry::{is_valid_id, EntryStatus, RegisterError, Registry, Source};
use ratatui::{buffer::Buffer, layout::Rect};
use std::sync::mpsc;
use std::time::Duration;

/// A viz that draws nothing; the registry tests only need one to exist.
struct Stub(String);

impl Viz for Stub {
    fn id(&self) -> &str {
        &self.0
    }
    fn title(&self) -> &str {
        "Stub"
    }
    fn render(&mut self, _buf: &mut Buffer, _area: Rect, _ctx: &VizCtx) {}
    fn legend(&self) -> Vec<String> {
        Vec::new()
    }
    fn explain(&self) -> String {
        String::new()
    }
    fn help(&self) -> String {
        String::new()
    }
}

fn add(r: &mut Registry, id: &str) -> Result<super::registry::VizHandle, RegisterError> {
    let owned = id.to_string();
    r.register(
        id,
        "Stub",
        Source::Folder,
        false,
        Box::new(move || Box::new(Stub(owned.clone()))),
    )
}

#[test]
fn duplicate_id_is_rejected() {
    let mut r = Registry::new();
    assert!(add(&mut r, "pulse").is_ok());
    assert_eq!(add(&mut r, "pulse"), Err(RegisterError::Duplicate));
}

#[test]
fn bad_id_is_rejected() {
    let mut r = Registry::new();
    for bad in ["Foo", "a_b", "", "-a"] {
        assert_eq!(add(&mut r, bad), Err(RegisterError::BadId), "{bad:?}");
        assert!(!is_valid_id(bad));
    }
    assert!(add(&mut r, "a-b2").is_ok());
}

#[test]
fn id_validator_rejects_empty_parts() {
    assert!(!is_valid_id("a--b"));
    assert!(!is_valid_id("a-"));
    assert!(is_valid_id("a-b-c9"));
}

fn mk(id: &str) -> super::registry::MakeViz {
    let owned = id.to_string();
    Box::new(move || Box::new(Stub(owned.clone())))
}

#[test]
fn with_viz_runs_the_closure_and_returns_some() {
    let mut r = Registry::new();
    let h = add(&mut r, "pulse").unwrap();
    assert_eq!(r.with_viz(h, |v| v.id().to_string()), Some("pulse".into()));
}

#[test]
fn mark_failed_drops_the_viz_and_sets_failed_status() {
    let mut r = Registry::new();
    let h = add(&mut r, "pulse").unwrap();
    assert!(r.mark_failed(h, "bad yaml"));
    assert!(r.with_viz(h, |_| ()).is_none());
    assert_eq!(
        r.entries()[0].status,
        EntryStatus::Failed("bad yaml".into())
    );
}

#[test]
fn replace_restores_ok_status() {
    let mut r = Registry::new();
    let h = add(&mut r, "pulse").unwrap();
    r.mark_failed(h, "bad");
    assert!(r.replace(h, mk("pulse")));
    assert_eq!(r.entries()[0].status, EntryStatus::Ok);
    assert!(r.with_viz(h, |_| ()).is_some());
}

#[test]
fn a_failed_id_still_counts_as_duplicate() {
    let mut r = Registry::new();
    let h = add(&mut r, "pulse").unwrap();
    r.mark_failed(h, "bad");
    assert_eq!(add(&mut r, "pulse"), Err(RegisterError::Duplicate));
}

#[test]
fn a_removed_id_can_be_registered_again_with_a_new_handle() {
    let mut r = Registry::new();
    let a = add(&mut r, "pulse").unwrap();
    r.remove(a);
    let b = add(&mut r, "pulse").unwrap();
    assert_ne!(a, b);
    assert_eq!(r.entries().len(), 1);
}

#[test]
fn handle_of_finds_live_ids_and_not_removed_ones() {
    let mut r = Registry::new();
    let h = add(&mut r, "pulse").unwrap();
    assert_eq!(r.handle_of("pulse"), Some(h));
    assert_eq!(r.handle_of("other"), None);
    r.remove(h);
    assert_eq!(r.handle_of("pulse"), None);
}

/// The global `with_viz` must not hold the registry lock while the closure
/// runs. A hang fails the test through the channel timeout.
#[test]
fn registry_calls_from_inside_with_viz_do_not_deadlock() {
    let a = super::registry::register(
        "deadlock-probe-a",
        "A",
        Source::Folder,
        false,
        mk("deadlock-probe-a"),
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = super::registry::with_viz(a, |_| {
            let n = super::registry::entries().len();
            let _ = super::registry::handle_of("deadlock-probe-a");
            n
        });
        let _ = tx.send(r);
    });
    let got = rx.recv_timeout(Duration::from_secs(5)).expect("deadlocked");
    assert!(got.unwrap() >= 1);
}

#[test]
fn removed_handle_is_not_reused() {
    let mut r = Registry::new();
    let a = add(&mut r, "alpha").unwrap();
    assert!(r.remove(a));
    let b = add(&mut r, "beta").unwrap();
    assert_ne!(a, b);
    assert!(r.entries().iter().all(|e| e.id != "alpha"));
    assert!(r.with_viz(a, |_| ()).is_none());
}

#[test]
fn entries_are_sorted_by_id() {
    let mut r = Registry::new();
    for id in ["zeta", "alpha", "mid"] {
        add(&mut r, id).unwrap();
    }
    let ids: Vec<String> = r.entries().into_iter().map(|e| e.id).collect();
    assert_eq!(ids, ["alpha", "mid", "zeta"]);
}

#[test]
fn mark_failed_keep_viz_sets_failed_status_and_keeps_the_viz() {
    let mut r = Registry::new();
    let h = add(&mut r, "keeper").unwrap();
    assert!(r.mark_failed_keep_viz(h, "bad edit"));
    assert_eq!(
        r.entries()[0].status,
        EntryStatus::Failed("bad edit".into())
    );
    // The last good version still renders.
    assert!(r.with_viz(h, |v| v.id().to_string()).is_some());
    // An unknown handle is refused.
    assert!(!r.mark_failed_keep_viz(super::registry::VizHandle(999), "x"));
    // A later good load restores Ok.
    assert!(r.replace(h, mk("keeper")));
    assert_eq!(r.entries()[0].status, EntryStatus::Ok);
}
