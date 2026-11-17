// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests that keep the written guidance true: the recipe in `AGENTS.md`, the
//! reference `docs/viz-plugins.md`, and the version numbers pinned in several
//! files. They read the files from the checkout, so a command that is renamed
//! or a file that is moved turns them red, so a stale recipe cannot stay.

use std::path::{Path, PathBuf};

/// The repository root (the directory that holds `Cargo.toml`).
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read a file of the checkout as text, failing with its name.
fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

/// The text of the `AGENTS.md` section "Add a viz": from its heading to the
/// next heading of the same or a higher level. Empty when there is none.
fn add_a_viz_section() -> String {
    let text = read("AGENTS.md");
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("## ") {
            if inside {
                break;
            }
            inside = line.trim_start_matches("## ").trim() == "Add a viz";
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The recipe names real things: every command and flag it tells an agent to
/// use is still defined in the code that implements it.
///
/// The guard sits at the code that defines each word (the clap flag, the
/// `viz` command arms, the golden variable) and not at the docs: the docs
/// are the thing being checked.
#[test]
fn agents_md_recipe_names_real_commands() {
    let recipe = add_a_viz_section();
    assert!(
        !recipe.is_empty(),
        "AGENTS.md has no `## Add a viz` section"
    );
    for needed in [
        "viz new",
        "--viz",
        "viz list",
        "UPDATE_GOLDEN=1",
        "docs/viz-plugins.md",
    ] {
        assert!(
            recipe.contains(needed),
            "the `Add a viz` recipe does not mention `{needed}`"
        );
    }

    // `--viz <id>`: a clap flag, parsed by the real parser.
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from(["tt-toplike", "--viz", "x"])
        .expect("the CLI no longer defines `--viz`");
    assert_eq!(cli.viz.as_deref(), Some("x"));

    // `viz new` and `viz list`: handled before the flag parser, in
    // `src/viz/cli.rs`, and shown in the `--help` examples of `src/cli.rs`.
    let viz_cli = read("src/viz/cli.rs");
    assert!(
        viz_cli.contains("[\"new\", rest @ ..]") && viz_cli.contains("[\"list\"]"),
        "src/viz/cli.rs no longer handles `viz new` and `viz list`"
    );
    assert!(viz_cli.contains("\"--yaml\""), "`viz new` lost `--yaml`");
    let cli_rs = read("src/cli.rs");
    assert!(
        cli_rs.contains("tt-toplike viz new") && cli_rs.contains("tt-toplike viz list"),
        "src/cli.rs --help no longer shows the viz commands"
    );

    // `UPDATE_GOLDEN=1`: the variable the golden helper reads.
    assert!(
        read("src/viz/testkit.rs").contains("\"UPDATE_GOLDEN\""),
        "the testkit no longer reads UPDATE_GOLDEN"
    );

    // The test command the recipe gives.
    assert!(
        recipe.contains("cargo test --lib --features tui viz::"),
        "the recipe lost its test command"
    );
}

/// Every path the guidance names under `prefix` (up to the end of the file or
/// folder name) is a real file or folder.
fn named_paths(text: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(prefix) {
        let tail = &rest[i + prefix.len()..];
        let name: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            .collect();
        // `<name>` placeholders and bare folder mentions are not paths.
        if !name.is_empty() {
            out.push(format!("{prefix}{}", name.trim_end_matches('.')));
        }
        rest = tail;
    }
    out.sort();
    out.dedup();
    out
}

/// The plugin folders, example files and docs that the guidance names exist.
#[test]
fn paths_named_in_the_docs_exist() {
    for file in [
        "AGENTS.md",
        "docs/viz-plugins.md",
        "README.md",
        "QUICK_START.md",
    ] {
        let text = if file == "AGENTS.md" {
            add_a_viz_section()
        } else {
            read(file)
        };
        let mut paths = named_paths(&text, "src/viz/plugins/");
        paths.extend(named_paths(&text, "viz/examples/"));
        paths.extend(named_paths(&text, "viz/schema/"));
        for p in paths {
            let on_disk = root().join(&p);
            assert!(on_disk.exists(), "{file} names `{p}`, which does not exist");
        }
    }
    // The recipe points at the reference, and the reference exists.
    assert!(
        add_a_viz_section().contains("docs/viz-plugins.md")
            && root().join("docs/viz-plugins.md").is_file(),
        "AGENTS.md must point at docs/viz-plugins.md, which must exist"
    );
    // The two shipped examples are named in the reference by their paths.
    let reference = read("docs/viz-plugins.md");
    for must in [
        "src/viz/plugins/example_pulse",
        "viz/examples/thermal-board.yaml",
        "src/bus/signals.rs",
    ] {
        assert!(
            reference.contains(must),
            "docs/viz-plugins.md lost `{must}`"
        );
        assert!(Path::new(&root().join(must)).exists(), "`{must}` is gone");
    }
}

/// The text between the first `start` and the following `end` in `text`.
fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let a = text.find(start).unwrap_or_else(|| panic!("no `{start}`")) + start.len();
    let b = text[a..].find(end).map_or(text.len(), |i| a + i);
    &text[a..b]
}

/// The `version` of the `[package]` table in a manifest. The key must start a
/// line, so `rust-version = "..."` (which a normalized manifest, as `cargo
/// package` writes it, lists before `version`) cannot match.
fn package_version(manifest: &str) -> String {
    let package = &manifest[manifest.find("[package]").expect("[package]")..];
    between(package, "\nversion = \"", "\"").to_string()
}

/// A normalized manifest lists `rust-version` first; the parser still finds
/// the package version, and it ignores the same key in later tables.
#[test]
fn package_version_ignores_rust_version() {
    let normalized = "[package]\nedition = \"2021\"\nrust-version = \"1.93\"\nname = \"tt-toplike\"\nversion = \"0.19.0\"\n\n[dependencies.x]\nversion = \"9\"\n";
    assert_eq!(package_version(normalized), "0.19.0");
    let plain = "[package]\nname = \"tt-toplike\"\nversion = \"1.2.3\"\nrust-version = \"1.93\"\n";
    assert_eq!(package_version(plain), "1.2.3");
}

/// The root crate's version in `Cargo.toml` matches the version at the top of
/// `debian/changelog` and `CHANGELOG.md`, and the version `QUICK_START.md` and
/// the site's eyebrow line show. A release bump that misses one fails here.
#[test]
fn the_root_version_is_the_same_everywhere_it_is_pinned() {
    let version = package_version(&read("Cargo.toml"));

    let deb = read("debian/changelog");
    let deb_top = between(&deb, "tt-toplike (", ")");
    assert_eq!(deb_top, version, "debian/changelog top entry");

    let log = read("CHANGELOG.md");
    let log_top = log
        .lines()
        .find_map(|l| l.strip_prefix("### "))
        .expect("a `### x.y.z` section");
    assert_eq!(log_top.trim(), version, "CHANGELOG.md top section");

    let quick = read("QUICK_START.md");
    assert_eq!(
        between(&quick, "**Version**: ", "\n").trim(),
        version,
        "QUICK_START.md version line"
    );

    let site = read("site/index.html");
    assert_eq!(
        between(&site, "Tenstorrent Hardware Monitor · v", " "),
        version,
        "site/index.html eyebrow"
    );

    // The lock file records the same version for this package.
    let lock = read("Cargo.lock");
    let at = lock
        .find("name = \"tt-toplike\"\nversion = \"")
        .expect("tt-toplike in Cargo.lock");
    assert_eq!(
        between(&lock[at..], "version = \"", "\""),
        version,
        "Cargo.lock"
    );
}
