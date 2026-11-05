// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the file loader. Each test uses a temp dir and a private
//! [`Registry`], so none touches the global registry.

use super::loader::{Change, Loader};
use crate::viz::ctx::{Viz, VizCtx};
use crate::viz::registry::{EntryStatus, Registry, Source};
use ratatui::{buffer::Buffer, layout::Rect};
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// A minimal valid definition with the given id and title.
fn def(id: &str, title: &str) -> String {
    format!(
        "version: 1\nid: {id}\ntitle: {title}\nlayout:\n  - height: 3\n    cells:\n      - widget: gauge\n        signal: power_w\n        max: 100.0\n"
    )
}

/// Write `text` to `path` and push its mtime `bump` seconds into the
/// future, so an edit within one filesystem timestamp tick is still seen.
fn write(path: &Path, text: &str, bump: u64) {
    fs::write(path, text).unwrap();
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(bump))
        .unwrap();
}

/// A viz for the clash test, standing in for a Rust plugin.
struct Stub;
impl Viz for Stub {
    fn id(&self) -> &str {
        "gpu-power"
    }
    fn title(&self) -> &str {
        "Stub"
    }
    fn render(&mut self, _b: &mut Buffer, _a: Rect, _c: &VizCtx) {}
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

fn failed_msg(r: &Registry, id: &str) -> String {
    let e = r.entries().into_iter().find(|e| e.id == id).unwrap();
    match e.status {
        EntryStatus::Failed(m) => m,
        EntryStatus::Ok => panic!("{id} is Ok"),
    }
}

#[test]
fn new_file_is_registered_on_poll() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    assert!(r.entries().is_empty());
    assert!(l.poll_with(&mut r).is_empty());
    write(&dir.path().join("a.yaml"), &def("alpha", "Alpha"), 0);
    let ch = l.poll_with(&mut r);
    let h = r.handle_of("alpha").unwrap();
    assert_eq!(ch, vec![Change::Added(h)]);
    let e = &r.entries()[0];
    assert!(e.enabled_by_default);
    assert_eq!(e.source, Source::File(dir.path().join("a.yaml")));
    // Nothing changed: the next poll reports nothing.
    assert!(l.poll_with(&mut r).is_empty());
}

#[test]
fn edited_file_replaces_the_viz_and_keeps_the_handle() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.yaml");
    write(&p, &def("alpha", "Alpha"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let h = r.handle_of("alpha").unwrap();
    write(&p, &def("alpha", "Alpha Two"), 5);
    assert_eq!(l.poll_with(&mut r), vec![Change::Updated(h)]);
    assert_eq!(r.handle_of("alpha"), Some(h));
    assert_eq!(
        r.with_viz(h, |v| v.title().to_string()).unwrap(),
        "Alpha Two"
    );
}

#[test]
fn edit_that_changes_the_id_is_remove_plus_add() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.yaml");
    write(&p, &def("alpha", "Alpha"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let old = r.handle_of("alpha").unwrap();
    write(&p, &def("beta", "Beta"), 5);
    let ch = l.poll_with(&mut r);
    let new = r.handle_of("beta").unwrap();
    assert_ne!(old, new);
    assert_eq!(ch, vec![Change::Removed(old), Change::Added(new)]);
    assert!(r.handle_of("alpha").is_none());
}

#[test]
fn broken_edit_keeps_last_good_version_and_marks_failed() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.yaml");
    write(&p, &def("alpha", "Alpha"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let h = r.handle_of("alpha").unwrap();
    write(&p, "version: 1\nid: alpha\nlayout: [", 5);
    let ch = l.poll_with(&mut r);
    assert!(matches!(&ch[..], [Change::Failed(x, _)] if *x == h));
    assert!(failed_msg(&r, "alpha").contains("a.yaml"));
    // The last good version still draws.
    assert_eq!(r.with_viz(h, |v| v.title().to_string()).unwrap(), "Alpha");
    // Unchanged broken file: no repeat report.
    assert!(l.poll_with(&mut r).is_empty());
    // Fixing it restores Ok on the same handle.
    write(&p, &def("alpha", "Alpha Fixed"), 10);
    assert_eq!(l.poll_with(&mut r), vec![Change::Updated(h)]);
    assert_eq!(r.entries()[0].status, EntryStatus::Ok);
}

#[test]
fn first_load_of_a_broken_file_is_a_failed_entry_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fs::write(d.join("syntax.yaml"), "layout: [").unwrap();
    // The unknown-widget fixture from the format tests.
    let fx = include_str!("fixtures/unknown_widget.yaml");
    fs::write(d.join("My Widget.yaml"), fx).unwrap();
    // Oversize: one byte more than the limit.
    let big = "#".repeat(super::validate::MAX_FILE_BYTES + 1);
    fs::write(d.join("big.yaml"), big).unwrap();
    // Not UTF-8.
    fs::write(d.join("latin.json"), [0xff, 0xfe, 0x7b]).unwrap();
    // Empty stem after cleaning.
    fs::write(d.join("---.yaml"), "x: [").unwrap();
    // A directory called x.yaml is not a viz file.
    fs::create_dir(d.join("dir.yaml")).unwrap();
    let mut r = Registry::new();
    let mut l = Loader::new(vec![d.to_path_buf()]);
    let ch = l.scan_with(&mut r);
    assert_eq!(ch.len(), 5, "{ch:?}");
    assert!(failed_msg(&r, "syntax").contains("syntax.yaml"));
    assert!(failed_msg(&r, "my-widget").contains("pie_chart"));
    assert!(failed_msg(&r, "big").contains("too large"));
    assert!(failed_msg(&r, "latin").contains("UTF-8"));
    assert!(failed_msg(&r, "invalid-file").contains("---.yaml"));
    // Failed placeholders cannot render.
    let h = r.handle_of("syntax").unwrap();
    assert!(r.with_viz(h, |_| ()).is_none());
    // No repeat reports while the files stay as they are.
    assert!(l.poll_with(&mut r).is_empty());
}

#[test]
fn deleted_file_is_removed_and_its_handle_is_a_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.yaml");
    write(&p, &def("alpha", "Alpha"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let h = r.handle_of("alpha").unwrap();
    fs::remove_file(&p).unwrap();
    assert_eq!(l.poll_with(&mut r), vec![Change::Removed(h)]);
    assert!(r.with_viz(h, |_| ()).is_none());
    assert!(r.entries().is_empty());
    // Restoring the file registers a new handle.
    write(&p, &def("alpha", "Alpha"), 0);
    l.poll_with(&mut r);
    assert_ne!(r.handle_of("alpha").unwrap(), h);
}

#[test]
fn id_clash_with_a_rust_plugin_is_a_failed_entry() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("mine.yaml"), &def("gpu-power", "Mine"), 0);
    let mut r = Registry::new();
    let rust = r
        .register_built("gpu-power", "Stub", Source::Folder, true, Box::new(Stub))
        .unwrap();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    assert!(failed_msg(&r, "mine").contains("gpu-power"));
    // The Rust plugin is untouched.
    assert_eq!(r.handle_of("gpu-power"), Some(rust));
    assert_eq!(
        r.entries()
            .iter()
            .find(|e| e.id == "gpu-power")
            .unwrap()
            .status,
        EntryStatus::Ok
    );
}

#[test]
fn placeholder_id_that_clashes_gets_a_suffix() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("gpu-power.yaml"), "x: [").unwrap();
    let mut r = Registry::new();
    r.register_built("gpu-power", "Stub", Source::Folder, true, Box::new(Stub))
        .unwrap();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    assert!(r.handle_of("gpu-power-file").is_some());
}

#[test]
fn examples_folder_is_disabled_by_default_and_other_folders_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fs::create_dir_all(d.join("examples/golden")).unwrap();
    fs::create_dir_all(d.join("schema")).unwrap();
    fs::create_dir_all(d.join("other")).unwrap();
    write(&d.join("examples/ex.yml"), &def("example-one", "Ex"), 0);
    write(
        &d.join("examples/golden/g.yaml"),
        &def("golden-one", "G"),
        0,
    );
    write(&d.join("schema/s.json"), "{}", 0);
    write(&d.join("other/o.yaml"), &def("other-one", "O"), 0);
    let json = r#"{"version":1,"id":"mine-one","title":"M","layout":[{"height":3,"cells":[{"widget":"gauge","signal":"power_w","max":100.0}]}]}"#;
    write(&d.join("mine.json"), json, 0);
    write(&d.join("notes.txt"), "hi", 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![d.to_path_buf()]);
    l.scan_with(&mut r);
    let e: Vec<_> = r
        .entries()
        .into_iter()
        .map(|e| (e.id, e.enabled_by_default))
        .collect();
    assert_eq!(
        e,
        vec![
            ("example-one".to_string(), false),
            ("mine-one".to_string(), true)
        ]
    );
}

#[cfg(unix)]
#[test]
fn symlink_loop_and_unreadable_file_become_failed_entries() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::os::unix::fs::symlink(d.join("loop.yaml"), d.join("loop.yaml")).unwrap();
    std::os::unix::fs::symlink(d.join("gone.yaml"), d.join("dangling.yaml")).unwrap();
    let mut r = Registry::new();
    let mut l = Loader::new(vec![d.to_path_buf()]);
    l.scan_with(&mut r);
    assert!(failed_msg(&r, "loop").contains("loop.yaml"));
    assert!(failed_msg(&r, "dangling").contains("dangling.yaml"));
}

#[test]
fn missing_directory_is_not_an_error() {
    let mut r = Registry::new();
    let mut l = Loader::new(vec!["/nonexistent/tt-toplike-viz".into()]);
    assert!(l.scan_with(&mut r).is_empty());
    assert!(l.poll_with(&mut r).is_empty());
}
