// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for [`Registry`]. Each builds its own registry, so none shares the
//! global one.

use super::ctx::{Viz, VizCtx};
use super::registry::{is_valid_id, RegisterError, Registry, Source};
use ratatui::{buffer::Buffer, layout::Rect};

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
