// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for `viz new` and `viz list` (`src/viz/cli.rs`).
//!
//! Every scaffold runs in a temporary directory built to look like a
//! checkout: a `Cargo.toml` with the package name, a copy of the real
//! `src/viz/plugins/example_pulse/` and an empty `viz/`. Nothing here writes
//! into the real repository.

use super::cli::{format_list, new_rust, new_yaml, run_in, viz_intercept, Env};
use crate::viz::declarative::loader::Loader;
use crate::viz::registry::{EntryStatus, Registry, Source, VizEntry, VizHandle};
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

/// Run the command, returning (exit code, stdout, stderr). It never reads
/// the real user config directory: the scan sees only `<cwd>/viz` and a
/// YAML scaffold outside a checkout goes to `<cwd>/user-viz`.
fn run_cmd(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let env = Env {
        loader_dirs: vec![cwd.join("viz")],
        user_dir: Some(cwd.join("user-viz")),
    };
    run_cmd_in(args, cwd, &env)
}

fn run_cmd_in(args: &[&str], cwd: &Path, env: &Env) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_in(&s(args), &mut out, &mut err, cwd, env);
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
    assert!(
        lines.iter().position(|l| l == broken) < lines.iter().position(|l| l == fine),
        "{text}"
    );

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

/// A listing row built by hand, so the test controls the order.
fn row(id: &str, title: &str, source: Source, status: EntryStatus, on: bool) -> VizEntry {
    VizEntry {
        handle: VizHandle(1),
        id: id.into(),
        title: title.into(),
        source,
        enabled_by_default: on,
        status,
    }
}

#[test]
fn list_is_sorted_by_id_whatever_the_registration_order() {
    // Registered zeta first, then alpha: the output must still lead with alpha.
    let entries = [
        row("zeta", "Zeta", Source::Folder, EntryStatus::Ok, true),
        row("alpha", "Alpha", Source::Folder, EntryStatus::Ok, true),
    ];
    let text = format_list(&entries);
    let at = |id: &str| text.lines().position(|l| l.starts_with(id)).expect(&text);
    assert!(at("alpha") < at("zeta"), "{text}");
    assert_eq!(text.lines().count(), 3);
}

#[test]
fn list_replaces_control_characters_and_aligns_by_display_width() {
    let entries = [
        row(
            "evil",
            "x\u{1b}[2Jy\nz",
            Source::File(PathBuf::from("/tmp/a\u{7}b.yaml")),
            EntryStatus::Failed("bad\u{1b}]0;pwned\u{7} key\nnext line".into()),
            true,
        ),
        row(
            "wide",
            "\u{4e2d}\u{6587}",
            Source::Folder,
            EntryStatus::Ok,
            true,
        ),
    ];
    let text = format_list(&entries);
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}'),
        "{text:?}"
    );
    assert_eq!(
        text.lines().count(),
        3,
        "a newline in a cell must not add a line: {text:?}"
    );
    assert!(text.contains("x?[2Jy?z"), "{text:?}");
    // The source column starts at the same display column on every line:
    // the two-character CJK title is four cells wide.
    use unicode_width::UnicodeWidthStr;
    let col = |l: &str, key: &str| UnicodeWidthStr::width(&l[..l.find(key).unwrap()]);
    let lines: Vec<&str> = text.lines().collect();
    let header = col(lines[0], "SOURCE");
    assert_eq!(col(lines[1], "/tmp/"), header, "{text}");
    assert_eq!(col(lines[2], "folder"), header, "{text}");
}

#[test]
fn run_list_prints_the_header_and_a_failed_entry_and_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let vizdir = tmp.path().join("viz");
    fs::create_dir_all(&vizdir).unwrap();
    fs::write(
        vizdir.join("broken.yaml"),
        "version: 1\nid: broken\nbogus_key: 1\n",
    )
    .unwrap();
    let env = Env {
        loader_dirs: vec![vizdir],
        user_dir: None,
    };
    let (code, out, err) = run_cmd_in(&["list"], tmp.path(), &env);
    assert_eq!(code, 0, "{err}");
    assert!(out.lines().next().unwrap().starts_with("ID"), "{out}");
    let line = out.lines().find(|l| l.starts_with("broken")).expect(&out);
    assert!(
        line.contains("failed: ") && line.contains("bogus_key"),
        "{line}"
    );
    // The Rust plugin folders are listed too.
    assert!(out.lines().any(|l| l.starts_with("example-pulse")), "{out}");
    // No `no viz found` here: the Rust folders always exist. That text is
    // covered by `format_list(&[])` in the test above.
}

#[test]
fn yaml_outside_a_checkout_with_no_config_dir_is_a_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let env = Env {
        loader_dirs: vec![],
        user_dir: None,
    };
    let (code, out, err) = run_cmd_in(&["new", "demo-yaml", "--yaml"], tmp.path(), &env);
    assert_eq!(code, 1);
    assert!(out.is_empty() && err.contains("config directory"), "{err}");
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn yaml_outside_a_checkout_goes_to_the_given_user_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, out, err) = run_cmd(&["new", "demo-yaml", "--yaml"], tmp.path());
    assert_eq!(code, 0, "{err}");
    assert!(tmp.path().join("user-viz/demo-yaml.yaml").is_file());
    assert!(out.contains("loads on its own"), "{out}");
}

#[test]
fn the_viz_decision_never_panics_on_odd_arguments() {
    use std::ffi::OsString;
    let os = |t: &str| OsString::from(t);
    assert_eq!(viz_intercept(&[]), None);
    assert_eq!(viz_intercept(&[os("--mock")]), None);
    assert_eq!(viz_intercept(&[os("vizz")]), None);
    assert_eq!(viz_intercept(&[os("list"), os("viz")]), None);
    assert_eq!(viz_intercept(&[os("viz")]), Some(vec![]));
    assert_eq!(
        viz_intercept(&[os("viz"), os("new"), os("a")]),
        Some(vec!["new".to_string(), "a".to_string()])
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0x66, 0xff, 0x6f]);
        // A first argument that is not UTF-8 is not `viz`: no panic, no match.
        assert_eq!(viz_intercept(&[bad.clone(), os("viz")]), None);
        // A later one is converted lossily on the viz path only.
        let got = viz_intercept(&[os("viz"), os("new"), bad]).unwrap();
        assert_eq!(got[1], "f\u{fffd}o");
    }
}

#[test]
fn hostile_names_are_refused_before_any_path_is_built() {
    let tmp = fake_checkout();
    let before = files_under(tmp.path()).len();
    for bad in [
        "a/b", "..", ".x", "a\\b", "/abs", "a\0b", "../x", "x/..", ".",
    ] {
        assert!(new_rust(bad, tmp.path(), &[]).is_err(), "{bad:?}");
        assert!(
            new_yaml(bad, &tmp.path().join("viz"), &[]).is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(files_under(tmp.path()).len(), before, "nothing was written");
    assert!(!tmp.path().join("viz").read_dir().unwrap().any(|_| true));
}

#[test]
fn only_the_three_template_files_are_copied() {
    let tmp = fake_checkout();
    let tpl = tmp.path().join("src/viz/plugins/example_pulse");
    fs::write(tpl.join(".hidden"), "x").unwrap();
    fs::write(tpl.join("notes.txt"), "x").unwrap();
    fs::write(tpl.join("mod.rs.orig"), "x").unwrap();
    let dir = new_rust("demo-probe", tmp.path(), &[]).unwrap();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|d| d.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["README.md", "mod.rs", "tests.rs"]);
}

#[test]
fn names_containing_the_templates_words_come_out_right() {
    for (name, title, ty) in [
        ("heart-pulse", "Heart Pulse", "HeartPulse"),
        ("pulse-x", "Pulse X", "PulseX"),
        ("example-x", "Example X", "ExampleX"),
        ("pulse", "Pulse", "Pulse"),
    ] {
        let tmp = fake_checkout();
        let dir = new_rust(name, tmp.path(), &[]).unwrap();
        let m = fs::read_to_string(dir.join("mod.rs")).unwrap();
        let t = fs::read_to_string(dir.join("tests.rs")).unwrap();
        let r = fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(
            m.contains(&format!("        \"{title}\"\n")),
            "{name}: title\n{m}"
        );
        assert_eq!(m.matches(&format!("\"{title}\"")).count(), 1, "{name}");
        assert!(m.contains(&format!("struct {ty} {{")), "{name}");
        assert!(m.contains(&format!("impl Viz for {ty} {{")), "{name}");
        assert!(m.contains(&format!("Box::new({ty} {{")), "{name}");
        assert!(m.contains(&format!("        \"{name}\"\n")), "{name}: id");
        assert!(t.contains(&format!("make().id(), \"{name}\"")), "{name}");
        assert!(r.starts_with(&format!("# {name}\n")), "{name}");
        // Nothing doubled or left over.
        for text in [&m, &t, &r] {
            for stale in [
                "ExamplePulse",
                "example_pulse",
                "example-pulse",
                "Example Pulse",
            ] {
                assert!(!text.contains(stale), "{name}: {stale}");
            }
            assert!(!text.contains('\u{1}'), "{name}: a placeholder leaked");
        }
        assert!(
            !m.contains(&format!("{ty}{ty}")) && !m.contains(&format!("{title} {ty}")),
            "{name}"
        );
    }
}

#[test]
fn built_in_view_names_are_refused() {
    let tmp = fake_checkout();
    for name in [
        "flow",
        "castle",
        "memory",
        "topology",
        "starfield",
        "arcade",
        "normal",
        "insights",
        "grid",
        "defrag",
        "hivemind",
        "hive",
        "sniff",
        "training",
        "train",
        "tt-train",
        "die3d",
        "saga",
    ] {
        let err = new_rust(name, tmp.path(), &[]).expect_err(name).to_string();
        assert!(err.contains("built-in view"), "{name}: {err}");
        assert!(
            new_yaml(name, &tmp.path().join("viz"), &[]).is_err(),
            "{name}"
        );
    }
    assert!(new_rust("flow-two", tmp.path(), &[]).is_ok());
}

/// Every name the TUI's `/mode` command answers to is refused by the scaffold.
/// The list is read from the `"mode" => match arg {` block of
/// `src/ui/tui/mod.rs`, so a view added there without updating the scaffold
/// fails here.
#[test]
fn every_mode_the_tui_command_accepts_is_reserved() {
    let src = include_str!("../ui/tui/mod.rs");
    let start = src
        .find("\"mode\" => match arg {")
        .expect("the /mode match moved");
    let block = &src[start..];
    let end = block
        .find("\"theme\" => match arg {")
        .expect("the /theme match moved");
    let mut seen = Vec::new();
    // `skip(1)`: the first line is the `"mode" => match arg {` line itself.
    for line in block[..end].lines().skip(1) {
        let line = line.trim_start();
        let Some((arms, _)) = line.split_once("=>") else {
            continue;
        };
        if !arms.trim_start().starts_with('"') {
            continue;
        }
        for arm in arms.split('|') {
            let arm = arm.trim();
            if let Some(name) = arm.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                seen.push(name.to_string());
            }
        }
    }
    assert!(seen.len() >= 10, "the scan found too little: {seen:?}");
    let tmp = fake_checkout();
    for name in seen {
        let err = new_rust(&name, tmp.path(), &[]).expect_err(&name);
        assert!(err.to_string().contains("built-in view"), "{name}: {err}");
    }
    // The aliases that are constants rather than literals.
    for name in [
        crate::animation::DIE3D_MODE_ALIAS,
        crate::ui::tui::saga_view::MODE_NAME,
    ] {
        assert!(new_rust(name, tmp.path(), &[]).is_err(), "{name}");
    }
}

/// The scaffold avoids type names the template imports. If the template gains
/// an import, this fails until `IMPORTED_NAMES` is updated.
#[test]
fn every_type_the_template_imports_is_in_the_clash_list() {
    let template = fs::read_to_string(repo().join("src/viz/plugins/example_pulse/mod.rs")).unwrap();
    let mut checked = 0;
    for line in template.lines().filter(|l| l.starts_with("use ")) {
        for word in line.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                checked += 1;
                // A name equal to it, lowercased, must be given the suffix.
                let mut kebab = String::new();
                for (i, c) in word.chars().enumerate() {
                    if c.is_ascii_uppercase() && i > 0 {
                        kebab.push('-');
                    }
                    kebab.push(c.to_ascii_lowercase());
                }
                let tmp = fake_checkout();
                let dir = new_rust(&kebab, tmp.path(), &[]).expect(&kebab);
                let m = fs::read_to_string(dir.join("mod.rs")).unwrap();
                assert!(
                    m.contains(&format!("struct {word}View")),
                    "`{word}` is imported by the template but not in IMPORTED_NAMES"
                );
            }
        }
    }
    assert!(checked >= 8, "the scan found too few imports: {checked}");
}
