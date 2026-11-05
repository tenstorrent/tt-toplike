// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! `tt-toplike viz new <name> [--yaml]` and `tt-toplike viz list`.
//!
//! These commands run before the TUI, backend or terminal exist. The binary
//! (`src/bin/tui.rs`) asks [`viz_intercept`] whether the first argument is the
//! word `viz`; if so it hands the rest to [`run`], which writes to the two
//! streams it is given and returns the exit code. The first argument is
//! compared as an `OsString`, so an argument that is not valid UTF-8 never
//! panics here and reaches the flag parser exactly as before. Only on the
//! `viz` path are the remaining arguments converted to text (lossily). No
//! subcommand is added to the flag parser, so no existing invocation parses
//! differently.
//!
//! # The two tiers
//!
//! * `viz new <name>` makes a Rust plugin: it copies the example folder
//!   `src/viz/plugins/example_pulse/` of a checkout to
//!   `src/viz/plugins/<snake_name>/` and renames what belongs to the example.
//!   The template is read from the checkout at run time and is not embedded in
//!   the binary. The example's own tests keep that folder working, so the
//!   scaffold cannot drift from it. A Rust plugin needs a rebuild, so the
//!   command only works inside a checkout.
//! * `viz new <name> --yaml` makes a YAML plugin: a copy of
//!   `viz/examples/thermal-board.yaml` with a new id and title, in `viz/` of a
//!   checkout or, anywhere else, in the user directory the loader watches
//!   (`<config dir>/tt-toplike/viz/`). The YAML template is embedded with
//!   `include_str!`, so it works with no checkout; it is compiled from the
//!   same file the example tests check, so it cannot drift either. The same
//!   embedded copy is used inside a checkout, so both places give one result.
//!
//! # What `viz list` and the checks read
//!
//! Neither touches the global registry's YAML side, hardware or the terminal.
//! Rust plugins come from `viz::init()`; YAML files are scanned into a
//! private registry, so running these commands (or their tests) never adds
//! entries a later test would see.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use unicode_width::UnicodeWidthStr;

use crate::viz::declarative::loader::{is_our_manifest, Loader};
use crate::viz::registry::{self, is_valid_id, EntryStatus, Registry, Source, VizEntry};

/// The YAML template, compiled in from the example file itself.
const YAML_TEMPLATE: &str = include_str!("../../viz/examples/thermal-board.yaml");
/// The id and title in the YAML template, replaced in the copy.
const YAML_TEMPLATE_ID: &str = "thermal-board";
const YAML_TEMPLATE_TITLE: &str = "Thermal Board";

/// The Rust template's folder, relative to a checkout.
const RUST_TEMPLATE_DIR: &str = "src/viz/plugins/example_pulse";
/// The template's names, replaced in the copy. `Pulse` is the struct and
/// (quoted) the title.
const RUST_TEMPLATE_ID: &str = "example-pulse";
const RUST_TEMPLATE_MODULE: &str = "example_pulse";
const RUST_TEMPLATE_TYPE: &str = "Pulse";

/// View names the TUI already answers to, beyond what `VisualizationMode`
/// lists: `/mode <name>` in the command line (`execute_command` in
/// `src/ui/tui/mod.rs`) and the in-app cycle. A plugin with one of these
/// names could not be reached with `/mode`, so a scaffold refuses them. The
/// names `--mode` accepts (and its aliases) are read from `VisualizationMode`
/// itself, see [`reserved_names`]; a test compares this list with the match
/// in `execute_command`, so a new built-in view cannot be missed silently.
const EXTRA_VIEW_NAMES: &[&str] = &[
    "insights",
    "normal",
    "grid",
    "starfield",
    "castle",
    "flow",
    "arcade",
    "defrag",
    "hivemind",
    "training",
    "train",
    "die3d",
    "saga",
];

/// Every name a scaffold may not take: `--mode` values and aliases, the
/// extra names above, and the Die3d alias.
fn reserved_names() -> Vec<String> {
    let mut names: Vec<String> = crate::cli::VisualizationMode::value_variants()
        .iter()
        .filter_map(|m| m.to_possible_value())
        .flat_map(|v| {
            v.get_name_and_aliases()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect();
    names.extend(EXTRA_VIEW_NAMES.iter().map(|n| n.to_string()));
    names.push(crate::animation::DIE3D_MODE_ALIAS.to_string());
    names
}

/// The template files a Rust scaffold copies.
const RUST_TEMPLATE_FILES: [&str; 3] = ["mod.rs", "tests.rs", "README.md"];

/// Longest name accepted. The same limit the YAML schema has on `id`.
const MAX_NAME_LEN: usize = 48;

/// Names the template's `mod.rs` already imports. A type with one of these
/// names would not compile, so such a type gets a `View` suffix.
const IMPORTED_NAMES: &[&str] = &[
    "Buffer",
    "EntityKey",
    "EntityKind",
    "Rect",
    "SignalSpec",
    "Style",
    "Value",
    "Viz",
    "VizCtx",
    "Box",
    "Vec",
    "String",
    "Option",
    "Result",
    "Self",
];

/// Rust keywords (strict, 2018+ and reserved). A module cannot have one of
/// these as its name. `gen` is reserved in edition 2024.
const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// Why a scaffold was refused. The text is one plain line that says what is
/// wrong and what to do; `run` prints it as is.
#[derive(Debug, PartialEq, Eq)]
pub enum ScaffoldError {
    /// The refusal, ready to print.
    Refused(String),
}

impl std::fmt::Display for ScaffoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScaffoldError::Refused(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ScaffoldError {}

/// Shorthand for building a refusal.
fn refuse<T>(msg: impl Into<String>) -> Result<T, ScaffoldError> {
    Err(ScaffoldError::Refused(msg.into()))
}

/// Check the rules both tiers share: kebab-case, at most 48 characters,
/// starts with a letter, no trailing hyphen, and an id nobody has.
fn check_name(name: &str, taken: &[String]) -> Result<(), ScaffoldError> {
    if !is_valid_id(name)
        || name.len() > MAX_NAME_LEN
        || !name.starts_with(|c: char| c.is_ascii_lowercase())
    {
        return refuse(format!(
            "`{name}` is not a valid viz name: use lowercase letters, digits and single hyphens, \
             starting with a letter, at most {MAX_NAME_LEN} characters (for example `heat-map`)"
        ));
    }
    if reserved_names().iter().any(|r| r == name) {
        return refuse(format!(
            "`{name}` is the name of a built-in view, so `/mode {name}` could not reach a plugin; \
             choose another name"
        ));
    }
    if taken.iter().any(|t| t == name) {
        return refuse(format!(
            "a viz with the id `{name}` already exists (see `tt-toplike viz list`); choose another name"
        ));
    }
    Ok(())
}

/// `heat-map` to `Heat Map`.
fn title_case(name: &str) -> String {
    name.split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `heat-map` to `HeatMap`, with a `View` suffix when the result would clash
/// with a name the template imports (`viz` would give the trait `Viz`).
fn type_name(name: &str) -> String {
    let t = title_case(name).replace(' ', "");
    if IMPORTED_NAMES.contains(&t.as_str()) {
        format!("{t}View")
    } else {
        t
    }
}

/// Replace `word` where it stands alone as an identifier (not inside a longer
/// word), so `Pulse` in `struct Pulse` changes and `Impulse` would not.
fn replace_ident(text: &str, word: &str, with: &str) -> String {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(word) {
        let before_ok = !rest[..i].chars().next_back().is_some_and(is_ident);
        let after = &rest[i + word.len()..];
        let after_ok = !after.chars().next().is_some_and(is_ident);
        out.push_str(&rest[..i]);
        out.push_str(if before_ok && after_ok { with } else { word });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// True when `root` is a tt-toplike checkout: its `Cargo.toml` names the
/// package `tt-toplike`. (The same test the loader uses before it scans a
/// `viz/` folder, so an installed binary run inside another Rust project
/// does not write into that project.)
fn is_checkout(root: &Path) -> bool {
    fs::read_to_string(root.join("Cargo.toml")).is_ok_and(|m| is_our_manifest(&m))
}

/// Placeholders used while rewriting, so that no replacement can match text
/// an earlier replacement inserted (a name such as `heart-pulse` contains the
/// template's type name). Control characters never appear in the template.
const P_ID: &str = "\u{1}ID\u{1}";
const P_MODULE: &str = "\u{1}MODULE\u{1}";
const P_TITLE: &str = "\u{1}TITLE\u{1}";
const P_TYPE: &str = "\u{1}TYPE\u{1}";

/// Rewrite one template file for the new plugin. The replacements are spelled
/// out so a reader can see exactly what a scaffold changes. Every one writes a
/// placeholder; the real id, module, title and type are put in last, in one
/// pass, so a replacement never rewrites another's output.
///
/// 1. The module doc and README intro that describe the file as the example.
/// 2. `ENABLED_BY_DEFAULT`: false in the example, true in the copy, with its
///    comment and the test that checks it.
/// 3. Every `example-pulse` and `example_pulse`, the quoted title `"Pulse"`
///    and the type name `Pulse`.
fn rewrite_rust(text: &str, id: &str, module: &str, title: &str, ty: &str) -> String {
    let mut t = text.to_string();
    // Module doc of mod.rs.
    t = t.replace(
        "//! `example-pulse`: the example Rust viz plugin. Copy this folder to start\n//! your own.\n",
        &format!("//! `{P_ID}`: a Rust viz plugin made with `tt-toplike viz new {P_ID}`.\n"),
    );
    // README intro.
    t = t.replace(
        "The example Rust viz plugin. Copy this folder to start your own; the rule is in\n`../README.md`.",
        &format!("A Rust viz plugin made with `tt-toplike viz new {P_ID}`. The rule it follows is in\n`../README.md`."),
    );
    // The scaffold is on by default; say so where the example says it is off.
    t = t.replace(
        "/// Whether the view is in the view cycle before the user chooses. An example\n/// stays hidden: enable it with `--viz example-pulse` or `[viz] enable`.\npub const ENABLED_BY_DEFAULT: bool = false;",
        "/// Whether the view is in the view cycle before the user chooses. Set it to\n/// false to hide the view until `--viz <id>` or `[viz] enable` asks for it.\npub const ENABLED_BY_DEFAULT: bool = true;",
    );
    t = t.replace(
        "const { assert!(!ENABLED_BY_DEFAULT) };",
        "const { assert!(ENABLED_BY_DEFAULT) };",
    );
    t = t.replace(
        "It is hidden by default. Either run",
        "It is in the view cycle by default. You can also run",
    );
    // Names, to placeholders. The quoted title goes first so the bare type
    // pass below cannot see it.
    t = t.replace(RUST_TEMPLATE_ID, P_ID);
    t = t.replace(RUST_TEMPLATE_MODULE, P_MODULE);
    t = t.replace(
        &format!("\"{RUST_TEMPLATE_TYPE}\""),
        &format!("\"{P_TITLE}\""),
    );
    t = replace_ident(&t, RUST_TEMPLATE_TYPE, P_TYPE);
    t.replace(P_ID, id)
        .replace(P_MODULE, module)
        .replace(P_TITLE, title)
        .replace(P_TYPE, ty)
}

/// Make the Rust plugin `name` in the checkout at `root`.
///
/// `taken` lists the ids that are already registered (Rust and YAML), so the
/// new id is unique across both tiers. Returns the new folder. Nothing is
/// overwritten: an existing folder is a refusal, and a copy that fails half
/// way is removed again.
pub fn new_rust(name: &str, root: &Path, taken: &[String]) -> Result<PathBuf, ScaffoldError> {
    check_name(name, taken)?;
    let module = name.replace('-', "_");
    if KEYWORDS.contains(&module.as_str()) {
        return refuse(format!(
            "`{name}` is a Rust keyword and cannot be a module name; choose another name"
        ));
    }
    if module == "register_all" {
        return refuse(
            "`register-all` is kept free so it is not confused with the generated `register_all` function; choose another name",
        );
    }
    let plugins = root.join("src/viz/plugins");
    if !is_checkout(root) || !plugins.is_dir() {
        return refuse(
            "Rust plugins are added in a checkout of the tt-toplike repo: run this from its top \
             folder, or use `tt-toplike viz new <name> --yaml` for a view that needs no rebuild",
        );
    }
    let template = root.join(RUST_TEMPLATE_DIR);
    if !template.is_dir() {
        return refuse(format!(
            "the template folder {RUST_TEMPLATE_DIR} is missing from this checkout; restore it with git"
        ));
    }
    let dest = plugins.join(&module);
    if dest.exists() {
        return refuse(format!(
            "{} already exists; choose another name or remove it first",
            dest.display()
        ));
    }

    let (title, ty) = (title_case(name), type_name(name));
    // Read everything before creating anything, so a bad template leaves the
    // checkout untouched. Only these three files are copied: not the golden
    // folder (its frames are of the example's drawing, and the new plugin's
    // tests create their own with UPDATE_GOLDEN=1), and nothing else that may
    // sit in the folder (editor files, dotfiles).
    let mut files: Vec<(&str, String)> = Vec::new();
    for file in RUST_TEMPLATE_FILES {
        let path = template.join(file);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) if file != "mod.rs" => continue,
            Err(e) => {
                return refuse(format!(
                    "cannot read {}: {e}; restore it with git",
                    path.display()
                ))
            }
        };
        files.push((file, rewrite_rust(&text, name, &module, &title, &ty)));
    }

    fs::create_dir(&dest)
        .map_err(|e| ScaffoldError::Refused(format!("cannot create {}: {e}", dest.display())))?;
    for (file, text) in &files {
        if let Err(e) = fs::write(dest.join(file), text) {
            let _ = fs::remove_dir_all(&dest);
            return refuse(format!("cannot write {}: {e}", dest.join(file).display()));
        }
    }
    Ok(dest)
}

/// Make the YAML plugin `name` as `<dir>/<name>.yaml`, creating `dir`.
///
/// The file is the embedded example with its id, title and every mention of
/// the example's name changed. Comments are kept. It is not under an
/// `examples/` folder, so the loader turns it on by default.
pub fn new_yaml(name: &str, dir: &Path, taken: &[String]) -> Result<PathBuf, ScaffoldError> {
    check_name(name, taken)?;
    let path = dir.join(format!("{name}.yaml"));
    if path.exists() {
        return refuse(format!(
            "{} already exists; choose another name or remove it first",
            path.display()
        ));
    }
    let text = YAML_TEMPLATE
        .replace(YAML_TEMPLATE_ID, name)
        .replace(YAML_TEMPLATE_TITLE, &title_case(name));
    fs::create_dir_all(dir)
        .map_err(|e| ScaffoldError::Refused(format!("cannot create {}: {e}", dir.display())))?;
    // create_new refuses to replace a file that appeared since the check.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| ScaffoldError::Refused(format!("cannot create {}: {e}", path.display())))?;
    if let Err(e) = file.write_all(text.as_bytes()) {
        // Do not leave half a file for the loader to report as broken.
        drop(file);
        let _ = fs::remove_file(&path);
        return refuse(format!("cannot write {}: {e}", path.display()));
    }
    Ok(path)
}

/// The table `viz list` prints: a header and one aligned line per entry,
/// sorted by id. Status is `ok`, `hidden` (loaded but off by default, as the
/// examples are) or `failed: <reason>`.
pub fn format_list(entries: &[VizEntry]) -> String {
    if entries.is_empty() {
        return "no viz found\n".into();
    }
    // A title, path or reason comes from a file anybody can write. Control
    // characters (an ESC sequence, a newline) would be sent to the terminal
    // as they are, so each becomes `?` before anything is measured or printed.
    let clean = |t: &str| -> String {
        t.chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect()
    };
    let mut rows: Vec<[String; 4]> = entries
        .iter()
        .map(|e| {
            let source = match &e.source {
                Source::Folder => "folder".to_string(),
                Source::File(p) => p.display().to_string(),
            };
            let status = match &e.status {
                // A reason can span lines; the table keeps to one.
                EntryStatus::Failed(why) => {
                    format!(
                        "failed: {}",
                        why.split_whitespace().collect::<Vec<_>>().join(" ")
                    )
                }
                EntryStatus::Ok if e.enabled_by_default => "ok".into(),
                EntryStatus::Ok => "hidden".into(),
            };
            [
                clean(&e.id),
                clean(&e.title),
                clean(&source),
                clean(&status),
            ]
        })
        .collect();
    rows.sort_by(|a, b| a[0].cmp(&b[0]));
    rows.insert(0, ["ID", "TITLE", "SOURCE", "STATUS"].map(String::from));
    // Align by display width (a wide character takes two cells). Pad every
    // column but the last, which can be long and ends the line.
    let width = |c: usize| rows.iter().map(|r| r[c].width()).max().unwrap_or(0);
    let widths = [width(0), width(1), width(2)];
    let mut out = String::new();
    for r in &rows {
        for (c, w) in widths.iter().enumerate() {
            out.push_str(&r[c]);
            out.push_str(&" ".repeat(w - r[c].width() + 2));
        }
        out.push_str(&r[3]);
        out.push('\n');
    }
    out
}

/// Every entry the program would know at start-up, without touching the
/// global registry's YAML side: the Rust plugin folders from `viz::init()`,
/// and each YAML file in `dirs` loaded into a private registry. Errors from
/// registering the folders go to `err`.
fn gather(dirs: Vec<PathBuf>, err: &mut dyn Write) -> Vec<VizEntry> {
    for e in crate::viz::init() {
        let _ = writeln!(err, "viz: {e}");
    }
    let mut all: Vec<VizEntry> = registry::entries()
        .into_iter()
        .filter(|e| e.source == Source::Folder)
        .collect();
    let mut private = Registry::new();
    Loader::new(dirs).scan_with(&mut private);
    all.extend(private.entries());
    all
}

const USAGE: &str = "\
Usage:
  tt-toplike viz new <name>         make a Rust viz plugin (run in a checkout of the repo)
  tt-toplike viz new <name> --yaml  make a YAML viz plugin (no rebuild needed)
  tt-toplike viz list               list every viz, with its source and status

<name> is kebab-case: lowercase letters, digits and single hyphens, starting
with a letter, at most 48 characters, and not already in use.
";

/// Where the commands look, so tests can point them at temporary folders
/// instead of the real user config directory.
pub struct Env {
    /// Directories scanned for YAML files (see [`Loader::default_dirs`]).
    pub loader_dirs: Vec<PathBuf>,
    /// Where `viz new --yaml` writes outside a checkout. `None` when the
    /// system has no user config directory.
    pub user_dir: Option<PathBuf>,
}

impl Env {
    /// The real locations: the loader's default directories and
    /// `<config dir>/tt-toplike/viz/`.
    pub fn real() -> Env {
        Env {
            loader_dirs: Loader::default_dirs(),
            user_dir: dirs::config_dir().map(|c| c.join("tt-toplike").join("viz")),
        }
    }
}

/// Decide whether the command line is a `viz` command. `args` are the
/// arguments after the program name, as the OS gave them. Returns the
/// arguments after `viz` as text (invalid UTF-8 replaced) when the first one
/// is exactly `viz`, and `None` otherwise. Nothing is converted, and nothing
/// can panic, unless the first argument is `viz`.
pub fn viz_intercept(args: &[OsString]) -> Option<Vec<String>> {
    match args.split_first() {
        Some((first, rest)) if first == "viz" => Some(
            rest.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        ),
        _ => None,
    }
}

/// Run `tt-toplike viz <args...>`. `args` is everything after the word
/// `viz`. Returns the exit code: 0 for success, 1 for a refusal or failure,
/// 2 for a usage error.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write, cwd: &Path) -> i32 {
    run_in(args, out, err, cwd, &Env::real())
}

/// [`run`] with the locations given.
pub fn run_in(
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
    cwd: &Path,
    env: &Env,
) -> i32 {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["-h"] | ["--help"] | ["help"] => {
            let _ = out.write_all(USAGE.as_bytes());
            0
        }
        ["list"] => {
            let entries = gather(env.loader_dirs.clone(), err);
            let _ = out.write_all(format_list(&entries).as_bytes());
            0
        }
        ["new", rest @ ..] => run_new(rest, out, err, cwd, env),
        _ => usage_error(err, &format!("unknown viz command: {}", words.join(" "))),
    }
}

/// Print a usage error to `err` and return 2.
fn usage_error(err: &mut dyn Write, why: &str) -> i32 {
    let _ = writeln!(err, "viz: {why}\n\n{USAGE}");
    2
}

/// The `new` command: parse `<name> [--yaml]` (the flag in either place).
fn run_new(args: &[&str], out: &mut dyn Write, err: &mut dyn Write, cwd: &Path, env: &Env) -> i32 {
    let mut yaml = false;
    let mut name = None;
    for a in args {
        match *a {
            "--yaml" => yaml = true,
            f if f.starts_with("--") => return usage_error(err, &format!("unknown option {f}")),
            n if name.is_none() => name = Some(n),
            _ => return usage_error(err, "viz new takes one name"),
        }
    }
    let Some(name) = name else {
        return usage_error(err, "viz new needs a name");
    };

    // The ids in use, so a new one is unique across both tiers.
    let taken: Vec<String> = gather(env.loader_dirs.clone(), err)
        .into_iter()
        .map(|e| e.id)
        .collect();

    let checkout = is_checkout(cwd);
    let made = if yaml {
        // In a checkout the file goes next to the examples; elsewhere into
        // the user directory the loader watches (the same path
        // `Loader::default_dirs` lists first).
        let dir = if checkout {
            Some(cwd.join("viz"))
        } else {
            env.user_dir.clone()
        };
        match dir {
            Some(d) => new_yaml(name, &d, &taken),
            None => refuse(
                "no user config directory on this system; run this in a checkout of the repo",
            ),
        }
    } else {
        new_rust(name, cwd, &taken)
    };
    let path = match made {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "viz: {e}");
            return 1;
        }
    };

    let _ = writeln!(out, "created {}", path.display());
    if !yaml {
        let module = name.replace('-', "_");
        let _ = writeln!(
            out,
            "next, create its golden test frames (read them before you commit):\n  \
             UPDATE_GOLDEN=1 cargo test --lib --features tui viz::plugins::{module}\n\
             then build and look at it:\n  \
             cargo run --features tui --bin tt-toplike-tui -- --mock --viz {name}"
        );
    } else if checkout {
        let _ = writeln!(
            out,
            "next: start tt-toplike; the file loads on its own and joins the `v` cycle. To test it:\n  \
             cargo test --lib --features tui viz::declarative"
        );
    } else {
        let _ = writeln!(
            out,
            "next: start tt-toplike; the file loads on its own (an edit shows within a few \
             seconds) and joins the `v` cycle"
        );
    }
    0
}
