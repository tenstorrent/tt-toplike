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
use std::collections::BTreeMap;
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

/// What the loader remembers about one file it has registered.
struct Known {
    /// The entry in the registry.
    handle: VizHandle,
    /// The id the entry is registered under (the file's own id, or the
    /// file-name placeholder for a file that never loaded).
    id: String,
    /// The stamp of the version last acted on.
    stamp: Stamp,
    /// Whether the entry holds a good viz (false for a placeholder).
    has_viz: bool,
}

/// Loads viz files from directories and keeps a registry in step with them.
pub struct Loader {
    dirs: Vec<PathBuf>,
    known: BTreeMap<PathBuf, Known>,
}

/// The default directories to scan.
///
/// 1. The user directory, `<config dir>/tt-toplike/viz/`, with the config
///    dir from `dirs::config_dir()` exactly as `config::config_path` does.
/// 2. The repo `viz/` folder when running from a checkout. A checkout is
///    recognised by a `Cargo.toml` next to a `viz/` folder, looked for in the
///    current directory and then in the ancestors of the executable (a
///    `cargo run` binary sits at `<repo>/target/<profile>/`). Requiring the
///    `Cargo.toml` keeps an unrelated `./viz` from being picked up.
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
    for root in roots {
        let viz = root.join("viz");
        if root.join("Cargo.toml").is_file() && viz.is_dir() && !dirs.contains(&viz) {
            dirs.push(viz);
            break;
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
    pub fn poll_with(&mut self, reg: &mut Registry) -> Vec<Change> {
        let found = self.walk();
        let mut changes = Vec::new();

        // Deleted files first, so an id freed by a delete can be taken by a
        // new file in the same poll.
        let gone: Vec<PathBuf> = self
            .known
            .keys()
            .filter(|p| !found.contains_key(*p))
            .cloned()
            .collect();
        for path in gone {
            if let Some(k) = self.known.remove(&path) {
                if reg.remove(k.handle) {
                    changes.push(Change::Removed(k.handle));
                }
            }
        }

        for (path, f) in found {
            if self.known.get(&path).is_some_and(|k| k.stamp == f.stamp) {
                continue; // unchanged: no file read
            }
            self.apply(reg, &path, &f, &mut changes);
        }
        changes
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

    /// Load one new or changed file and apply the result to `reg`.
    fn apply(&mut self, reg: &mut Registry, path: &Path, f: &Found, out: &mut Vec<Change>) {
        let loaded =
            read_bounded(path).and_then(|text| super::load(path, &text).map_err(|e| e.to_string()));
        let prior = self
            .known
            .get(path)
            .map(|k| (k.handle, k.id.clone(), k.has_viz));

        let def = match loaded {
            Ok(def) => def,
            Err(msg) => return self.fail(reg, path, f, prior, msg, out),
        };

        // An id that another live entry owns is a failure, unless it is this
        // file's own entry.
        let taken_by_other = reg
            .handle_of(&def.id)
            .is_some_and(|h| prior.as_ref().is_none_or(|(ph, _, _)| *ph != h));
        if taken_by_other {
            let msg = format!(
                "{}: id `{}` is already used by another viz",
                path.display(),
                def.id
            );
            return self.fail(reg, path, f, prior, msg, out);
        }

        let title = def.title.clone();
        let id = def.id.clone();
        let viz = Box::new(super::DeclarativeViz::new(def));
        match prior {
            // Same id (or a placeholder that now has the right one): replace
            // in place and keep the handle.
            Some((h, old_id, _)) if old_id == id => {
                reg.replace_built(h, viz);
                self.set_known(path, h, id, f, true);
                out.push(Change::Updated(h));
            }
            // The id changed: the old entry goes, a new one appears.
            Some((h, _, _)) => {
                reg.remove(h);
                out.push(Change::Removed(h));
                self.add(reg, path, f, &id, &title, viz, out);
            }
            None => self.add(reg, path, f, &id, &title, viz, out),
        }
    }

    /// Register a freshly loaded viz.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        reg: &mut Registry,
        path: &Path,
        f: &Found,
        id: &str,
        title: &str,
        viz: Box<dyn crate::viz::Viz>,
        out: &mut Vec<Change>,
    ) {
        match reg.register_built(id, title, Source::File(path.to_path_buf()), f.enabled, viz) {
            Ok(h) => {
                self.set_known(path, h, id.to_string(), f, true);
                out.push(Change::Added(h));
            }
            // validate() guarantees a good id and apply() checked for a
            // clash, so this is unreachable; fail visibly rather than panic.
            Err(e) => self.fail(reg, path, f, None, format!("{}: {e}", path.display()), out),
        }
    }

    /// Record a failure. A file that was good keeps its last good viz and
    /// shows the message; a file that never loaded gets a placeholder entry.
    fn fail(
        &mut self,
        reg: &mut Registry,
        path: &Path,
        f: &Found,
        prior: Option<(VizHandle, String, bool)>,
        msg: String,
        out: &mut Vec<Change>,
    ) {
        // poll_with only calls this when the stamp changed, so an unchanged
        // broken file is never logged twice.
        log::warn!("viz file failed to load: {msg}");
        match prior {
            Some((h, id, true)) => {
                reg.mark_failed_keep_viz(h, &msg);
                self.set_known(path, h, id, f, true);
                out.push(Change::Failed(h, msg));
            }
            Some((h, id, false)) => {
                reg.mark_failed(h, &msg);
                self.set_known(path, h, id, f, false);
                out.push(Change::Failed(h, msg));
            }
            None => {
                let id = placeholder_id(reg, path);
                let title = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| id.clone());
                let placeholder = Box::new(Placeholder);
                if let Ok(h) = reg.register_built(
                    &id,
                    &title,
                    Source::File(path.to_path_buf()),
                    f.enabled,
                    placeholder,
                ) {
                    reg.mark_failed(h, &msg);
                    self.set_known(path, h, id, f, false);
                    out.push(Change::Failed(h, msg));
                }
            }
        }
    }

    fn set_known(&mut self, path: &Path, handle: VizHandle, id: String, f: &Found, has_viz: bool) {
        self.known.insert(
            path.to_path_buf(),
            Known {
                handle,
                id,
                stamp: f.stamp,
                has_viz,
            },
        );
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
/// directory called `x.yaml` is skipped. A file whose metadata cannot be
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
            Ok(m) if m.is_dir() => continue,
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
