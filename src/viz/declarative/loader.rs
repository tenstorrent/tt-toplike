// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Finding viz files on disk, loading them, and reloading them on save.
//!
//! A [`Loader`] watches a list of directories for `*.yaml`, `*.yml` and
//! `*.json` files and keeps a [`Registry`] in step with them:
//!
//! * [`Loader::scan`] does the first load. Every file becomes an entry. A
//!   file that does not load becomes a *failed* entry (under an id taken
//!   from the file name) so the user sees the error in the `v` list.
//! * [`Loader::poll`] compares each file's (modified time, length) with
//!   what it saw last and acts only on files that changed, appeared or
//!   vanished. It does one `read_dir` per directory and one `metadata` per
//!   file and reads no file contents unless the stamp changed. There is no
//!   thread and no watcher crate. **The caller rate-limits it** to once per
//!   two seconds; `poll` has no clock of its own.
//!
//! Limit: a change is detected by (modified time, length). An edit that keeps
//! the length and lands within one modified-time tick of the previous save
//! (a coarse-timestamp filesystem) is not seen until the file changes again.
//!
//! Id clashes: an id belongs to the first file in path order that loads
//! with it (Rust plugins always win). A file that loses stays a failed entry
//! and is re-read on every poll, so it loads once the other owner is gone or
//! changed. A failed placeholder (named from the file stem) never blocks a
//! real id; a real id evicts it and the placeholder moves to another id.
//!
//! Text is turned into a definition only by [`super::load`], which does the
//! size check, parse and validation and names the file in every error.
//!
//! Which files count: each directory is read non-recursively, plus one level
//! of subfolder named `examples`. Files under `examples` register with
//! `enabled_by_default = false` (they are the shipped samples); all others
//! register with `true`. Other folders (`schema/`, `examples/golden/`) and
//! other file types are ignored.

use super::validate::MAX_FILE_BYTES;
use crate::viz::registry::{is_valid_id, Registry, Source, VizHandle};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a poll or scan did to one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A file was registered (a new file, or an edit that changed its id).
    Added(VizHandle),
    /// An edited file loaded and replaced the viz under the same handle.
    Updated(VizHandle),
    /// A file was deleted (or its id changed); the handle is now a tombstone.
    Removed(VizHandle),
    /// A file did not load; the string is the message shown in the listing.
    Failed(VizHandle, String),
}

/// A file's change stamp: modified time and length.
type Stamp = (SystemTime, u64);

/// A file found by a directory walk.
struct Found {
    /// Registers enabled by default (false under `examples/`).
    enabled: bool,
    /// Its stamp; (epoch, 0) when its metadata could not be read.
    stamp: Stamp,
}

/// What the loader remembers about one file it has acted on.
#[derive(Clone)]
struct Known {
    /// The entry in the registry. `None` only when registering even a
    /// placeholder failed (logged); the stamp is still kept so the file is
    /// not retried and re-logged every poll.
    handle: Option<VizHandle>,
    /// The id the entry is registered under (the file's own id, or the
    /// file-name placeholder for a file that never loaded).
    id: String,
    /// The stamp of the version last acted on.
    stamp: Stamp,
    /// Whether the entry holds a good viz (false for a placeholder).
    has_viz: bool,
    /// The failure message shown for this file, empty when it is Ok.
    msg: String,
    /// The failure is an id clash. Such a file is retried on every poll even
    /// when unchanged, because the other owner of the id may have gone.
    clash: bool,
}

/// A file read and parsed in this poll.
struct Item {
    path: PathBuf,
    found: Found,
    result: Result<super::VizDef, String>,
}

/// Loads viz files from directories and keeps a registry in step with them.
pub struct Loader {
    dirs: Vec<PathBuf>,
    known: BTreeMap<PathBuf, Known>,
}

/// True when `manifest` (the text of a Cargo.toml) is this package's: a
/// line `name = "tt-toplike"`.
fn is_our_manifest(manifest: &str) -> bool {
    manifest
        .lines()
        .any(|l| l.trim() == "name = \"tt-toplike\"")
}

/// `<root>/viz` when `root` is a tt-toplike checkout: its `Cargo.toml` names
/// the package `tt-toplike` and `viz/` exists. Checking the package name
/// keeps an installed binary run inside some other Rust project from
/// scanning that project's `viz/` folder.
fn repo_viz(root: &Path) -> Option<PathBuf> {
    let viz = root.join("viz");
    let manifest = fs::read_to_string(root.join("Cargo.toml")).ok()?;
    (is_our_manifest(&manifest) && viz.is_dir()).then_some(viz)
}

/// Test hook for [`repo_viz`].
#[cfg(test)]
pub(crate) fn default_dirs_for_test(root: &Path) -> Option<PathBuf> {
    repo_viz(root)
}

/// The default directories to scan.
///
/// 1. The user directory, `<config dir>/tt-toplike/viz/`, with the config
///    dir from `dirs::config_dir()` exactly as `config::config_path` does.
/// 2. The repo `viz/` folder when running from a checkout of tt-toplike
///    (see [`repo_viz`]), looked for in the current directory and then in
///    the ancestors of the executable (a `cargo run` binary sits at
///    `<repo>/target/<profile>/`).
///
/// Directories that do not exist are fine: the loader skips them.
pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(c) = dirs::config_dir() {
        dirs.push(c.join("tt-toplike").join("viz"));
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).take(4).map(Path::to_path_buf));
    }
    if let Some(viz) = roots.iter().find_map(|r| repo_viz(r)) {
        if !dirs.contains(&viz) {
            dirs.push(viz);
        }
    }
    dirs
}

impl Loader {
    /// A loader over `dirs`. Nothing is read until [`Loader::scan`].
    pub fn new(dirs: Vec<PathBuf>) -> Loader {
        Loader {
            dirs,
            known: BTreeMap::new(),
        }
    }

    /// [`default_dirs`], for `Loader::new(Loader::default_dirs())`.
    pub fn default_dirs() -> Vec<PathBuf> {
        default_dirs()
    }

    /// First load into the global registry. Returns what happened.
    pub fn scan(&mut self) -> Vec<Change> {
        crate::viz::registry::with_global(|r| self.scan_with(r))
    }

    /// Check the directories for changes and apply them to the global
    /// registry. Cheap when nothing changed. Call at most every 2 seconds.
    pub fn poll(&mut self) -> Vec<Change> {
        crate::viz::registry::with_global(|r| self.poll_with(r))
    }

    /// [`Loader::scan`] on a given registry (tests use a private one).
    /// The same as a poll: every file is new on the first call.
    pub fn scan_with(&mut self, reg: &mut Registry) -> Vec<Change> {
        self.poll_with(reg)
    }

    /// [`Loader::poll`] on a given registry.
    ///
    /// Order of work: (1) deleted files are removed, so their ids are free;
    /// (2) the files to look at (new, changed, or failed on an id clash) are
    /// read; (3) files that load are applied, an id change being a remove
    /// plus an add and a placeholder never blocking a real id; (4) files
    /// that did not load get failed entries.
    pub fn poll_with(&mut self, reg: &mut Registry) -> Vec<Change> {
        let found = self.walk();
        let mut changes = Vec::new();

        // (1) Deleted files.
        let gone: Vec<PathBuf> = self
            .known
            .keys()
            .filter(|p| !found.contains_key(*p))
            .cloned()
            .collect();
        for path in gone {
            if let Some(k) = self.known.remove(&path) {
                if let Some(h) = k.handle {
                    if reg.remove(h) {
                        changes.push(Change::Removed(h));
                    }
                }
            }
        }

        // (2) Read what changed, plus clash-failed files (retried every poll).
        let mut items: Vec<Item> = Vec::new();
        for (path, f) in found {
            let due = match self.known.get(&path) {
                None => true,
                Some(k) => k.stamp != f.stamp || k.clash,
            };
            if !due {
                continue; // unchanged: no file read
            }
            let result = read_bounded(&path)
                .and_then(|text| super::load(&path, &text).map_err(|e| e.to_string()));
            items.push(Item {
                path,
                found: f,
                result,
            });
        }
        if items.is_empty() {
            return changes;
        }

        let clashed = self.resolve_clashes(reg, &items);

        // (3a) A file that loads under a new id gives up its old entry.
        for (i, it) in items.iter().enumerate() {
            let Ok(def) = &it.result else { continue };
            if clashed.contains_key(&i) {
                continue;
            }
            if self.known.get(&it.path).is_some_and(|k| k.id != def.id) {
                if let Some(k) = self.known.remove(&it.path) {
                    if let Some(h) = k.handle {
                        reg.remove(h);
                        changes.push(Change::Removed(h));
                    }
                }
            }
        }

        // (3b) A real id evicts a placeholder that holds it. The evicted
        // file gets a new placeholder id below.
        let mut evicted: Vec<(PathBuf, Found, String, bool)> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            let Ok(def) = &it.result else { continue };
            if clashed.contains_key(&i) {
                continue;
            }
            let Some(h) = reg.handle_of(&def.id) else {
                continue;
            };
            let own = self.known.get(&it.path).and_then(|k| k.handle) == Some(h);
            if own {
                continue;
            }
            if let Some(path) = self.placeholder_path(h) {
                if let Some(k) = self.known.remove(&path) {
                    reg.remove(h);
                    changes.push(Change::Removed(h));
                    let enabled = path.parent().is_some_and(is_examples_dir);
                    let stamp = k.stamp;
                    evicted.push((path, Found { enabled, stamp }, k.msg, k.clash));
                }
            }
        }

        // (3c) Register or replace the files that load.
        let mut late: Vec<(usize, String)> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            let Ok(def) = &it.result else { continue };
            if clashed.contains_key(&i) {
                continue;
            }
            let prior = self.known.get(&it.path).and_then(|k| k.handle);
            let viz = Box::new(super::DeclarativeViz::new(def.clone()));
            match prior {
                Some(h) => {
                    reg.replace_built(h, viz);
                    self.set_known(&it.path, Some(h), &def.id, &it.found, true, "", false);
                    changes.push(Change::Updated(h));
                }
                None => match reg.register_built(
                    &def.id,
                    &def.title,
                    Source::File(it.path.clone()),
                    it.found.enabled,
                    viz,
                ) {
                    Ok(h) => {
                        self.set_known(&it.path, Some(h), &def.id, &it.found, true, "", false);
                        changes.push(Change::Added(h));
                    }
                    // validate() guarantees a good id and the clash check
                    // ran, so this is unreachable; fail visibly, not by panic.
                    Err(e) => late.push((i, format!("{}: {e}", it.path.display()))),
                },
            }
        }

        // (4) Files that did not load get failed entries.
        for (i, it) in items.iter().enumerate() {
            let (msg, clash) = match (&it.result, clashed.get(&i)) {
                (_, Some(m)) => (m.clone(), true),
                (Err(m), None) => (m.clone(), false),
                (Ok(_), None) => match late.iter().find(|(j, _)| *j == i) {
                    Some((_, m)) => (m.clone(), false),
                    None => continue,
                },
            };
            self.fail(reg, &it.path, &it.found, msg, clash, &mut changes);
        }
        for (path, f, msg, clash) in evicted {
            self.fail(reg, &path, &f, msg, clash, &mut changes);
        }
        changes
    }

    /// Decide which loadable files cannot take their id: index into `items`
    /// to the message. An id is blocked by a live viz that stays (a Rust
    /// plugin, or a file's good viz that is not giving the id up this poll)
    /// and by an earlier file in path order that wants the same id.
    /// Placeholders never block. Files that swap or rotate ids are allowed
    /// because each one's old id counts as free when it is being given up.
    /// Re-run until no further file fails, since a failing file keeps its
    /// old id and so stops freeing it.
    fn resolve_clashes(&self, reg: &Registry, items: &[Item]) -> BTreeMap<usize, String> {
        let mut failed: BTreeMap<usize, String> = BTreeMap::new();
        loop {
            let leaving: HashSet<VizHandle> = items
                .iter()
                .enumerate()
                .filter(|(i, _)| !failed.contains_key(i))
                .filter_map(|(_, it)| match (&it.result, self.known.get(&it.path)) {
                    (Ok(def), Some(k)) if k.id != def.id => k.handle,
                    _ => None,
                })
                .collect();
            let mut claimed: HashMap<&str, usize> = HashMap::new();
            let mut grew = false;
            for (i, it) in items.iter().enumerate() {
                let Ok(def) = &it.result else { continue };
                if failed.contains_key(&i) {
                    continue;
                }
                let own = self.known.get(&it.path).and_then(|k| k.handle);
                let who = if let Some(&j) = claimed.get(def.id.as_str()) {
                    Some(items[j].path.display().to_string())
                } else {
                    match reg.handle_of(&def.id) {
                        Some(h)
                            if Some(h) != own
                                && !leaving.contains(&h)
                                && self.placeholder_path(h).is_none() =>
                        {
                            Some(describe_owner(reg, h))
                        }
                        _ => None,
                    }
                };
                match who {
                    Some(who) => {
                        failed.insert(
                            i,
                            format!(
                                "{}: id `{}` is already used by {who}",
                                it.path.display(),
                                def.id
                            ),
                        );
                        grew = true;
                    }
                    None => {
                        claimed.insert(def.id.as_str(), i);
                    }
                }
            }
            if !grew {
                return failed;
            }
        }
    }

    /// The file whose failed placeholder entry is `h`, if `h` is one.
    fn placeholder_path(&self, h: VizHandle) -> Option<PathBuf> {
        self.known
            .iter()
            .find(|(_, k)| k.handle == Some(h) && !k.has_viz)
            .map(|(p, _)| p.clone())
    }

    /// List the candidate files in every directory, in path order.
    fn walk(&self) -> BTreeMap<PathBuf, Found> {
        let mut out = BTreeMap::new();
        for dir in &self.dirs {
            list_dir(dir, true, &mut out);
            list_dir(&dir.join("examples"), false, &mut out);
        }
        out
    }

    /// Record a failure. A file that was good keeps its last good viz and
    /// shows the message; a file that never loaded gets a placeholder entry.
    /// Called again for an unchanged clash-failed file, it does nothing when
    /// the message is the same, so nothing is logged or reported twice.
    fn fail(
        &mut self,
        reg: &mut Registry,
        path: &Path,
        f: &Found,
        msg: String,
        clash: bool,
        out: &mut Vec<Change>,
    ) {
        let prior = self.known.get(path).cloned();
        if let Some(k) = &prior {
            if k.stamp == f.stamp && k.msg == msg && k.clash == clash {
                return;
            }
        }
        log::warn!("viz file failed to load: {msg}");
        match prior {
            Some(Known {
                handle: Some(h),
                id,
                has_viz,
                ..
            }) => {
                if has_viz {
                    reg.mark_failed_keep_viz(h, &msg);
                } else {
                    reg.mark_failed(h, &msg);
                }
                self.set_known(path, Some(h), &id, f, has_viz, &msg, clash);
                out.push(Change::Failed(h, msg));
            }
            _ => {
                let id = placeholder_id(reg, path);
                let title = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| id.clone());
                match reg.register_built(
                    &id,
                    &title,
                    Source::File(path.to_path_buf()),
                    f.enabled,
                    Box::new(Placeholder),
                ) {
                    Ok(h) => {
                        reg.mark_failed(h, &msg);
                        self.set_known(path, Some(h), &id, f, false, &msg, clash);
                        out.push(Change::Failed(h, msg));
                    }
                    Err(e) => {
                        log::error!(
                            "viz file {}: cannot register a failed entry: {e}",
                            path.display()
                        );
                        self.set_known(path, None, &id, f, false, &msg, clash);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn set_known(
        &mut self,
        path: &Path,
        handle: Option<VizHandle>,
        id: &str,
        f: &Found,
        has_viz: bool,
        msg: &str,
        clash: bool,
    ) {
        self.known.insert(
            path.to_path_buf(),
            Known {
                handle,
                id: id.to_string(),
                stamp: f.stamp,
                has_viz,
                msg: msg.to_string(),
                clash,
            },
        );
    }
}

/// True for a directory named `examples` (its files default to disabled).
fn is_examples_dir(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == "examples")
}

/// Who owns a live id, for a clash message: the file, or a built-in plugin.
fn describe_owner(reg: &Registry, h: VizHandle) -> String {
    match reg.entries().into_iter().find(|e| e.handle == h) {
        Some(e) => match e.source {
            Source::File(p) => p.display().to_string(),
            Source::Folder => "a built-in plugin".to_string(),
        },
        None => "another viz".to_string(),
    }
}

/// The stand-in viz a failed first load is registered with. The registry
/// drops it at once (`mark_failed`), so it is never drawn.
struct Placeholder;

impl crate::viz::Viz for Placeholder {
    fn id(&self) -> &str {
        ""
    }
    fn title(&self) -> &str {
        ""
    }
    fn render(
        &mut self,
        _buf: &mut ratatui::buffer::Buffer,
        _area: ratatui::layout::Rect,
        _ctx: &crate::viz::VizCtx,
    ) {
    }
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

/// Add the viz files directly inside `dir` to `out`. A missing or
/// unreadable directory adds nothing. Subfolders are not entered; a
/// directory, FIFO or device called `x.yaml` is skipped. A file whose metadata cannot be
/// read (a symlink loop, a dangling link) is kept so loading it reports why.
fn list_dir(dir: &Path, enabled: bool, out: &mut BTreeMap<PathBuf, Found>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let path = ent.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if !matches!(ext.as_deref(), Some("yaml" | "yml" | "json")) {
            continue;
        }
        let stamp = match fs::metadata(&path) {
            // Only regular files are ever opened: a FIFO or device named
            // x.yaml would block `File::open` and hang the poll.
            Ok(m) if !m.is_file() => continue,
            Ok(m) => (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()),
            Err(_) => (SystemTime::UNIX_EPOCH, 0),
        };
        out.entry(path).or_insert(Found { enabled, stamp });
    }
}

/// Read at most [`MAX_FILE_BYTES`] plus one byte of `path`. The extra byte
/// lets [`super::load`] see the file is too large without the loader ever
/// holding more. A file that is not UTF-8 is an error message.
fn read_bounded(path: &Path) -> Result<String, String> {
    let file =
        fs::File::open(path).map_err(|e| format!("{}: cannot read file: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: cannot read file: {e}", path.display()))?;
    match String::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) if e.as_bytes().len() > MAX_FILE_BYTES => {
            // Oversize wins over bad UTF-8: load() reports the size.
            Ok(String::from_utf8_lossy(e.as_bytes()).into_owned())
        }
        Err(_) => Err(format!("{}: file is not valid UTF-8 text", path.display())),
    }
}

/// A valid, unused id for a file that did not load: the file stem lowercased
/// with every run of characters outside `[a-z0-9]` turned into one `-`
/// (`invalid-file` when nothing is left), then `-file`, `-file-2`, ... added
/// until no live entry has it.
fn placeholder_id(reg: &Registry, path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut base = String::new();
    for c in stem.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            base.push(c);
        } else if !base.ends_with('-') {
            base.push('-');
        }
    }
    let base = base.trim_matches('-');
    let base = if base.is_empty() {
        "invalid-file"
    } else {
        base
    };
    debug_assert!(is_valid_id(base));
    let mut id = base.to_string();
    let mut n = 1;
    while reg.handle_of(&id).is_some() {
        id = if n == 1 {
            format!("{base}-file")
        } else {
            format!("{base}-file-{n}")
        };
        n += 1;
    }
    id
}
