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

// ---- fix round 1 ----------------------------------------------------------

/// Run `f` on a thread and fail the test if it has not finished in 10
/// seconds, so a hang fails instead of blocking CI.
fn within_10s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(10)).expect("poll hung")
}

fn ids(r: &Registry) -> Vec<String> {
    r.entries().into_iter().map(|e| e.id).collect()
}

#[test]
fn file_versus_file_clash_first_by_path_wins_and_names_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.yaml");
    let b = dir.path().join("b.yaml");
    write(&a, &def("same", "A"), 0);
    write(&b, &def("same", "B"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let h = r.handle_of("same").unwrap();
    assert_eq!(r.with_viz(h, |v| v.title().to_string()).unwrap(), "A");
    let msg = failed_msg(&r, "b");
    assert!(msg.contains("same") && msg.contains("a.yaml"), "{msg}");
}

#[test]
fn clash_clears_when_the_other_file_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.yaml");
    let b = dir.path().join("b.yaml");
    write(&a, &def("same", "A"), 0);
    write(&b, &def("same", "B"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    // b.yaml is unchanged, yet a retry on the next poll must not log or
    // report anything while the clash stands.
    assert!(l.poll_with(&mut r).is_empty());
    fs::remove_file(&a).unwrap();
    let ch = l.poll_with(&mut r);
    // a's entry goes; b's placeholder is replaced by the real viz.
    assert!(ch.iter().any(|c| matches!(c, Change::Removed(_))));
    assert!(ch.iter().any(|c| matches!(c, Change::Added(_))));
    assert_eq!(ids(&r), ["same"]);
    assert_eq!(r.entries()[0].status, EntryStatus::Ok);
    assert_eq!(r.entries()[0].title, "B");
}

#[test]
fn clash_clears_when_the_other_file_changes_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.yaml");
    let b = dir.path().join("b.yaml");
    write(&a, &def("same", "A"), 0);
    write(&b, &def("same", "B"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    write(&a, &def("other", "A2"), 5);
    l.poll_with(&mut r);
    assert_eq!(ids(&r), ["other", "same"]);
    assert!(r.entries().iter().all(|e| e.status == EntryStatus::Ok));
}

#[test]
fn clash_on_a_good_file_keeps_its_last_good_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.yaml");
    let b = dir.path().join("b.yaml");
    write(&a, &def("a-id", "A"), 0);
    write(&b, &def("b-id", "B"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let hb = r.handle_of("b-id").unwrap();
    write(&b, &def("a-id", "B2"), 5); // now clashes with a.yaml
    let ch = l.poll_with(&mut r);
    assert!(matches!(&ch[..], [Change::Failed(h, _)] if *h == hb));
    assert_eq!(r.with_viz(hb, |v| v.title().to_string()).unwrap(), "B");
    fs::remove_file(&a).unwrap();
    l.poll_with(&mut r);
    // b.yaml now owns a-id (an id change: new handle).
    assert_eq!(ids(&r), ["a-id"]);
    assert_eq!(r.entries()[0].status, EntryStatus::Ok);
    assert_eq!(r.entries()[0].title, "B2");
}

#[test]
fn two_files_swapping_ids_both_load() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.yaml");
    let b = dir.path().join("b.yaml");
    write(&a, &def("one", "From A"), 0);
    write(&b, &def("two", "From B"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    write(&a, &def("two", "From A"), 5);
    write(&b, &def("one", "From B"), 5);
    l.poll_with(&mut r);
    assert!(r.entries().iter().all(|e| e.status == EntryStatus::Ok));
    let title = |id: &str| r.entries().into_iter().find(|e| e.id == id).unwrap().title;
    assert_eq!(title("two"), "From A");
    assert_eq!(title("one"), "From B");
}

#[test]
fn placeholder_never_blocks_a_real_id_in_the_same_scan() {
    let dir = tempfile::tempdir().unwrap();
    // "alpha.yaml" is broken: its placeholder id would be "alpha". The
    // good file "zeta.yaml" has `id: alpha` and sorts later.
    fs::write(dir.path().join("alpha.yaml"), "x: [").unwrap();
    write(&dir.path().join("zeta.yaml"), &def("alpha", "Real"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let real = r.entries().into_iter().find(|e| e.id == "alpha").unwrap();
    assert_eq!(real.status, EntryStatus::Ok);
    assert_eq!(real.title, "Real");
    // The broken file got another placeholder id.
    assert_eq!(ids(&r), ["alpha", "alpha-file"]);
    assert!(failed_msg(&r, "alpha-file").contains("alpha.yaml"));
}

#[test]
fn real_id_evicts_a_placeholder_in_a_later_poll() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("alpha.yaml"), "x: [").unwrap();
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    assert_eq!(ids(&r), ["alpha"]);
    write(&dir.path().join("zeta.yaml"), &def("alpha", "Real"), 0);
    l.poll_with(&mut r);
    assert_eq!(ids(&r), ["alpha", "alpha-file"]);
    assert_eq!(r.entries()[0].status, EntryStatus::Ok);
    assert_eq!(r.entries()[0].title, "Real");
    assert!(matches!(r.entries()[1].status, EntryStatus::Failed(_)));
    assert!(l.poll_with(&mut r).is_empty());
}

#[cfg(unix)]
#[test]
fn a_fifo_named_like_a_viz_file_does_not_hang_poll() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("pipe.yaml");
    let ok = std::process::Command::new("mkfifo").arg(&p).status();
    if !ok.map(|s| s.success()).unwrap_or(false) {
        eprintln!("mkfifo unavailable; skipping");
        return;
    }
    let d = dir.path().to_path_buf();
    let n = within_10s(move || {
        let mut r = Registry::new();
        let mut l = Loader::new(vec![d]);
        l.scan_with(&mut r);
        l.poll_with(&mut r);
        r.entries().len()
    });
    assert_eq!(n, 0);
}

#[test]
fn mtime_going_backwards_is_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.yaml");
    write(&p, &def("alpha", "One"), 100);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let h = r.handle_of("alpha").unwrap();
    // Same length, older mtime (a restored backup).
    write(&p, &def("alpha", "Two"), 0);
    assert_eq!(l.poll_with(&mut r), vec![Change::Updated(h)]);
    assert_eq!(r.with_viz(h, |v| v.title().to_string()).unwrap(), "Two");
}

#[test]
fn same_mtime_but_different_length_is_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.yaml");
    let t = SystemTime::now() - Duration::from_secs(50);
    let put = |text: &str| {
        fs::write(&p, text).unwrap();
        let f = fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(t).unwrap();
    };
    put(&def("alpha", "One"));
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let h = r.handle_of("alpha").unwrap();
    put(&def("alpha", "Longer Title"));
    assert_eq!(l.poll_with(&mut r), vec![Change::Updated(h)]);
}

#[test]
fn a_file_replaced_by_a_directory_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.yaml");
    write(&p, &def("alpha", "One"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![dir.path().to_path_buf()]);
    l.scan_with(&mut r);
    let h = r.handle_of("alpha").unwrap();
    fs::remove_file(&p).unwrap();
    fs::create_dir(&p).unwrap();
    assert_eq!(l.poll_with(&mut r), vec![Change::Removed(h)]);
    assert!(r.entries().is_empty());
}

#[test]
fn examples_under_any_dir_and_multiple_dirs_work() {
    let user = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir(user.path().join("examples")).unwrap();
    write(
        &user.path().join("examples/u.yaml"),
        &def("user-example", "U"),
        0,
    );
    write(&user.path().join("mine.yaml"), &def("user-mine", "M"), 0);
    write(&repo.path().join("repo.yaml"), &def("repo-one", "R"), 0);
    let mut r = Registry::new();
    let mut l = Loader::new(vec![user.path().to_path_buf(), repo.path().to_path_buf()]);
    l.scan_with(&mut r);
    let got: Vec<_> = r
        .entries()
        .into_iter()
        .map(|e| (e.id, e.enabled_by_default))
        .collect();
    assert_eq!(
        got,
        vec![
            ("repo-one".to_string(), true),
            ("user-example".to_string(), false),
            ("user-mine".to_string(), true)
        ]
    );
}

#[test]
fn repo_viz_dir_is_accepted_only_for_a_tt_toplike_checkout() {
    use super::loader::default_dirs_for_test;
    let ours = tempfile::tempdir().unwrap();
    fs::create_dir(ours.path().join("viz")).unwrap();
    fs::write(
        ours.path().join("Cargo.toml"),
        "[package]\nname = \"tt-toplike\"\n",
    )
    .unwrap();
    let other = tempfile::tempdir().unwrap();
    fs::create_dir(other.path().join("viz")).unwrap();
    fs::write(
        other.path().join("Cargo.toml"),
        "[package]\nname = \"someone-else\"\n",
    )
    .unwrap();
    let bare = tempfile::tempdir().unwrap();
    fs::create_dir(bare.path().join("viz")).unwrap();
    assert_eq!(
        default_dirs_for_test(ours.path()),
        Some(ours.path().join("viz"))
    );
    assert_eq!(default_dirs_for_test(other.path()), None);
    assert_eq!(default_dirs_for_test(bare.path()), None);
}
