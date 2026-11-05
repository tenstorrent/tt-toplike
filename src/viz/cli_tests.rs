// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for `viz new` and `viz list` (`src/viz/cli.rs`).
//!
//! Every scaffold runs in a temporary directory built to look like a
//! checkout: a `Cargo.toml` with the package name, a copy of the real
//! `src/viz/plugins/example_pulse/` and an empty `viz/`. Nothing here writes
//! into the real repository.

use super::cli::{format_list, new_rust, new_yaml, run};
use crate::viz::declarative::loader::Loader;
use crate::viz::registry::{EntryStatus, Registry};
use std::fs;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Copy a directory tree.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for item in fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (item.path(), to.join(item.file_name()));
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

/// A directory that looks like a checkout, with the real example folder in it
/// (golden files included, so the tests can prove the scaffold leaves them).
fn fake_checkout() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"tt-toplike\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    copy_tree(
        &repo().join("src/viz/plugins/example_pulse"),
        &tmp.path().join("src/viz/plugins/example_pulse"),
    );
    fs::create_dir_all(tmp.path().join("viz")).unwrap();
    tmp
}

/// Every file under `dir`, as (relative path, text).
fn files_under(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        for item in fs::read_dir(dir).unwrap().flatten() {
            let p = item.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                out.push((rel, fs::read_to_string(&p).unwrap()));
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Run the command, returning (exit code, stdout, stderr).
fn run_cmd(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&s(args), &mut out, &mut err, cwd);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn new_rust_creates_a_folder_that_registers_after_build() {
    let tmp = fake_checkout();
    let dir = new_rust("demo-probe", tmp.path(), &[]).expect("scaffold");
    assert_eq!(dir, tmp.path().join("src/viz/plugins/demo_probe"));

    // The new folder has the three template files and no golden folder.
    let files = files_under(&dir);
    let mut names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(names, ["README.md", "mod.rs", "tests.rs"], "{names:?}");
    assert!(
        !dir.join("golden").exists(),
        "golden files must not be copied"
    );

    // No trace of the example's names, in any case or spelling, in any file.
    for (name, text) in &files {
        let lower = text.to_lowercase();
        for stale in [
            "example-pulse",
            "example_pulse",
            "examplepulse",
            "example pulse",
        ] {
            assert!(!lower.contains(stale), "{name} still has {stale}");
        }
    }
    let get = |n: &str| &files.iter().find(|(f, _)| f == n).unwrap().1;
    let (m, t, r) = (get("mod.rs"), get("tests.rs"), get("README.md"));

    // The id, the title and the type name follow the new name.
    assert!(m.contains("\"demo-probe\""), "id");
    assert!(m.contains("\"Demo Probe\""), "title");
    assert!(m.contains("struct DemoProbe"), "type name");
    assert!(m.contains("impl Viz for DemoProbe"));
    assert!(m.contains("[viz.demo-probe]"), "config section");
    assert!(m.contains("pub const ENABLED_BY_DEFAULT: bool = true;"));
    assert!(
        !m.contains("An example stays hidden"),
        "comment must match the value"
    );
    assert!(m.contains("demo_probe") || m.contains("demo-probe"));
    // The tests find their golden folder in the new place and expect the
    // view to be on by default.
    assert!(t.contains("src/viz/plugins/demo_probe/golden"));
    assert!(t.contains("assert_eq!(make().id(), \"demo-probe\")"));
    assert!(t.contains("const { assert!(ENABLED_BY_DEFAULT) }"));
    // The README starts with the id and says how to enable it.
    assert!(r.starts_with("# demo-probe\n"), "{r}");
    assert!(r.contains("--viz demo-probe"));
    // The braces and parentheses still balance, a cheap check that the
    // rewrite did not cut a line in half. (The real proof is the hand build
    // recorded in the task report; `syn` is not a direct dependency.)
    for text in [m, t] {
        let count = |c: char| text.chars().filter(|x| *x == c).count();
        assert_eq!(count('{'), count('}'));
        assert_eq!(count('('), count(')'));
    }
}

#[test]
fn new_rust_rejects_non_kebab_names_and_existing_folders() {
    let tmp = fake_checkout();
    let long = "a".repeat(49);
    for bad in [
        "Demo",
        "demo_probe",
        "-demo",
        "demo-",
        "demo--probe",
        "1demo",
        "",
        "de mo",
        "démo",
        &long,
        "match",
        "loop",
        "type",
        "mod",
        "fn",
        "self",
        "yield",
        "register-all",
    ] {
        let err = new_rust(bad, tmp.path(), &[]).expect_err(bad);
        let msg = err.to_string();
        assert!(!msg.is_empty() && !msg.contains('\n'), "{bad:?}: {msg:?}");
    }
    // Nothing was created by any refusal.
    let folders: Vec<_> = fs::read_dir(tmp.path().join("src/viz/plugins"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(folders.len(), 1, "only example_pulse is there");

    // An existing folder is not overwritten, and the name is not taken twice.
    let err = new_rust("example-pulse", tmp.path(), &[]).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    let err = new_rust("demo-probe", tmp.path(), &s(&["demo-probe"])).unwrap_err();
    assert!(err.to_string().contains("already"), "{err}");
    // 48 characters are fine.
    let ok = format!("a{}", "b".repeat(47));
    assert!(new_rust(&ok, tmp.path(), &[]).is_ok());

    // Outside a checkout the message says Rust plugins need one and points
    // at --yaml.
    let elsewhere = tempfile::tempdir().unwrap();
    let msg = new_rust("demo-probe", elsewhere.path(), &[])
        .unwrap_err()
        .to_string();
    assert!(msg.contains("checkout") && msg.contains("--yaml"), "{msg}");
}

#[test]
fn a_type_name_that_would_clash_with_an_import_gets_a_suffix() {
    // `viz` would make `struct Viz`, which is the trait the file imports.
    let tmp = fake_checkout();
    let dir = new_rust("viz", tmp.path(), &[]).expect("scaffold");
    let m = fs::read_to_string(dir.join("mod.rs")).unwrap();
    assert!(m.contains("struct VizView"), "{m}");
    assert!(!m.contains("struct Viz "));
}

#[test]
fn new_yaml_copies_and_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("nested/viz");
    let path = new_yaml("demo-yaml", &dir, &[]).expect("scaffold");
    assert_eq!(path, dir.join("demo-yaml.yaml"));
    let text = fs::read_to_string(&path).unwrap();

    // The template is the example, with every comment kept: same number of
    // comment lines, none of the example's names left.
    let example = fs::read_to_string(repo().join("viz/examples/thermal-board.yaml")).unwrap();
    let comments = |t: &str| {
        t.lines()
            .filter(|l| l.trim_start().starts_with('#'))
            .count()
    };
    assert_eq!(comments(&text), comments(&example));
    assert!(text.contains("\nid: demo-yaml\n"));
    assert!(text.contains("\ntitle: Demo Yaml\n"));
    assert!(!text.to_lowercase().contains("thermal-board"));

    // It loads through the real loader, under the new id, and is on by
    // default because it is not under examples/.
    let mut reg = Registry::new();
    Loader::new(vec![dir.clone()]).scan_with(&mut reg);
    let entries = reg.entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].id, "demo-yaml");
    assert_eq!(entries[0].status, EntryStatus::Ok);
    assert!(entries[0].enabled_by_default);

    // Refusals: existing file, taken id, bad name. Nothing is overwritten.
    fs::write(&path, "mine").unwrap();
    let err = new_yaml("demo-yaml", &dir, &[]).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
    assert!(new_yaml("other", &dir, &s(&["other"])).is_err());
    assert!(new_yaml("Bad_Name", &dir, &[]).is_err());
    // A Rust keyword is a fine YAML name.
    assert!(new_yaml("match", &dir, &[]).is_ok());
}

#[test]
fn viz_list_shows_a_failed_entry_with_its_reason() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("broken.yaml"),
        "version: 1\nid: broken\nbogus_key: 1\n",
    )
    .unwrap();
    new_yaml("fine-one", tmp.path(), &[]).unwrap();
    let mut reg = Registry::new();
    Loader::new(vec![tmp.path().to_path_buf()]).scan_with(&mut reg);

    let text = format_list(&reg.entries());
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("ID"), "header first: {text}");
    let broken = lines.iter().find(|l| l.starts_with("broken")).expect(&text);
    assert!(
        broken.contains("failed: ") && broken.contains("bogus_key"),
        "{broken}"
    );
    let fine = lines
        .iter()
        .find(|l| l.starts_with("fine-one"))
        .expect(&text);
    assert!(
        fine.contains("Fine One") && fine.trim_end().ends_with("ok"),
        "{fine}"
    );
    assert!(
        fine.contains("fine-one.yaml"),
        "source is the file path: {fine}"
    );
    // Sorted by id and aligned: the title column starts at one place.
    let id_w = lines[0].find("TITLE").unwrap();
    assert!(
        lines[1..].iter().all(|l| l.as_bytes()[id_w - 1] == b' '),
        "{text}"
    );
    assert!(broken < fine);

    // Not on by default shows as hidden; nothing at all says so.
    assert_eq!(format_list(&[]), "no viz found\n");
}

#[test]
fn hidden_is_the_status_of_an_entry_that_is_off_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let ex = tmp.path().join("examples");
    new_yaml("quiet", &ex, &[]).unwrap();
    let mut reg = Registry::new();
    Loader::new(vec![tmp.path().to_path_buf()]).scan_with(&mut reg);
    let text = format_list(&reg.entries());
    assert!(
        text.lines()
            .any(|l| l.starts_with("quiet") && l.trim_end().ends_with("hidden")),
        "{text}"
    );
}

#[test]
fn command_line_shapes_and_exit_codes() {
    let tmp = fake_checkout();
    let cwd = tmp.path();
    // Usage: alone, --help and help print both commands and succeed.
    for args in [&["viz"][..], &["--help"], &["help"]] {
        let a = if args == ["viz"] { &[][..] } else { args };
        let (code, out, _) = run_cmd(a, cwd);
        assert_eq!(code, 0);
        assert!(
            out.contains("viz new <name>") && out.contains("viz list"),
            "{out}"
        );
    }
    // Usage errors are 2 and say so on stderr.
    for args in [
        &["new"][..],
        &["new", "a", "b"],
        &["new", "a", "--nope"],
        &["list", "extra"],
        &["frobnicate"],
    ] {
        let (code, out, err) = run_cmd(args, cwd);
        assert_eq!(code, 2, "{args:?}");
        assert!(out.is_empty() && !err.is_empty(), "{args:?}");
    }
    // A refusal is one line on stderr and code 1.
    let (code, out, err) = run_cmd(&["new", "Not_Kebab"], cwd);
    assert_eq!((code, out.as_str()), (1, ""));
    assert_eq!(err.lines().count(), 1, "{err}");
    // Outside a checkout the Rust tier points at --yaml.
    let elsewhere = tempfile::tempdir().unwrap();
    let (code, _, err) = run_cmd(&["new", "demo-probe"], elsewhere.path());
    assert_eq!(code, 1);
    assert!(err.contains("checkout") && err.contains("--yaml"), "{err}");
}

#[test]
fn run_scaffolds_in_a_checkout_and_prints_the_next_command() {
    let tmp = fake_checkout();
    let (code, out, err) = run_cmd(&["new", "demo-probe"], tmp.path());
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("UPDATE_GOLDEN=1 cargo test --lib --features tui viz::plugins::demo_probe"),
        "{out}"
    );
    assert!(tmp
        .path()
        .join("src/viz/plugins/demo_probe/mod.rs")
        .is_file());
    // A second run refuses and leaves the folder alone.
    let (code, _, err) = run_cmd(&["new", "demo-probe"], tmp.path());
    assert_eq!(code, 1);
    assert!(err.contains("already"), "{err}");

    // YAML in a checkout goes to <checkout>/viz/.
    let (code, out, err) = run_cmd(&["new", "demo-yaml", "--yaml"], tmp.path());
    assert_eq!(code, 0, "{err}");
    assert!(tmp.path().join("viz/demo-yaml.yaml").is_file());
    assert!(out.contains("tt-toplike"), "{out}");
}
