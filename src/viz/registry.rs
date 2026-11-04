// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The plugin registry: every viz the host knows, by id and handle.
//!
//! The logic lives in the plain [`Registry`] struct so tests can build their
//! own. The free functions at the bottom wrap one process-wide instance
//! (a `RwLock` in a `OnceLock`); the host, the folder-plugin loader and the
//! YAML loader call those.

use super::ctx::Viz;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};

/// A handle to a registered viz. Handed out once per registration and never
/// reused, so a stale handle finds nothing instead of someone else's viz.
/// `DisplayMode::Plugin` carries one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VizHandle(pub u32);

/// Where a viz came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A Rust plugin compiled in from a folder under `src/viz/plugins`.
    Folder,
    /// A YAML plugin loaded from this file.
    File(PathBuf),
}

/// Whether a registered viz can be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStatus {
    /// Ready to render.
    Ok,
    /// Failed to load; the message says why. Shown in the plugin listing.
    Failed(String),
}

/// A listing row for one registered viz, returned by [`Registry::entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VizEntry {
    /// The handle to pass to `with_viz`.
    pub handle: VizHandle,
    /// The kebab-case id.
    pub id: String,
    /// The display title.
    pub title: String,
    /// Where the viz came from.
    pub source: Source,
    /// Whether it joins the view cycle without `[viz] enable` or `--viz`.
    pub enabled_by_default: bool,
    /// Whether it loaded.
    pub status: EntryStatus,
}

/// Why [`Registry::register`] refused a viz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// The id is not kebab-case (`[a-z0-9]+(-[a-z0-9]+)*`).
    BadId,
    /// A viz with this id is already registered.
    Duplicate,
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegisterError::BadId => write!(f, "viz id must be kebab-case: [a-z0-9]+(-[a-z0-9]+)*"),
            RegisterError::Duplicate => write!(f, "a viz with this id is already registered"),
        }
    }
}

impl std::error::Error for RegisterError {}

/// A factory the registry calls to build (or rebuild) a viz.
pub type MakeViz = Box<dyn Fn() -> Box<dyn Viz> + Send + Sync>;

/// True when `id` matches `[a-z0-9]+(-[a-z0-9]+)*`. Called by `register` and
/// by the loaders that derive an id from a folder or file name.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// One live registration. A failed entry keeps no viz.
///
/// The viz sits in a `Mutex` only to make `Slot` `Sync`: `Viz` is `Send` but
/// not `Sync`, and a `RwLock<Registry>` in a static needs `Registry: Sync`.
/// Every access goes through `&mut Registry`, so the mutex is never
/// contended (`get_mut` reaches it without locking).
struct Slot {
    entry: VizEntry,
    viz: Option<Mutex<Box<dyn Viz>>>,
}

/// The registry logic, without global state.
pub struct Registry {
    /// Live slots by handle. A removed handle is deleted here (a tombstone:
    /// the number stays spent because `next_handle` only grows).
    slots: HashMap<u32, Slot>,
    /// The next handle number to hand out.
    next_handle: u32,
}

impl Default for Registry {
    fn default() -> Self {
        Registry::new()
    }
}

impl Registry {
    /// An empty registry. The global one is built from this; tests build
    /// their own.
    pub fn new() -> Registry {
        Registry {
            slots: HashMap::new(),
            next_handle: 0,
        }
    }

    /// Register a viz under `id`, building it with `make` now.
    ///
    /// Errors: `BadId` when `id` is not kebab-case, `Duplicate` when a live
    /// entry already has it. A removed id may be registered again and gets a
    /// new handle.
    pub fn register(
        &mut self,
        id: &str,
        title: &str,
        source: Source,
        enabled_by_default: bool,
        make: MakeViz,
    ) -> Result<VizHandle, RegisterError> {
        if !is_valid_id(id) {
            return Err(RegisterError::BadId);
        }
        if self.handle_of(id).is_some() {
            return Err(RegisterError::Duplicate);
        }
        let handle = VizHandle(self.next_handle);
        self.next_handle += 1;
        self.slots.insert(
            handle.0,
            Slot {
                entry: VizEntry {
                    handle,
                    id: id.to_string(),
                    title: title.to_string(),
                    source,
                    enabled_by_default,
                    status: EntryStatus::Ok,
                },
                viz: Some(Mutex::new(make())),
            },
        );
        Ok(handle)
    }

    /// All live entries, sorted by id.
    pub fn entries(&self) -> Vec<VizEntry> {
        let mut v: Vec<VizEntry> = self.slots.values().map(|s| s.entry.clone()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// The handle of the live entry with `id`, if any.
    pub fn handle_of(&self, id: &str) -> Option<VizHandle> {
        self.slots
            .values()
            .find(|s| s.entry.id == id)
            .map(|s| s.entry.handle)
    }

    /// Run `f` on the viz behind `h`. `None` when the handle is unknown or
    /// removed, or the entry failed to load.
    pub fn with_viz<R>(&mut self, h: VizHandle, f: impl FnOnce(&mut dyn Viz) -> R) -> Option<R> {
        let viz = self.slots.get_mut(&h.0)?.viz.as_mut()?;
        let viz = viz.get_mut().unwrap_or_else(|e| e.into_inner());
        Some(f(viz.as_mut()))
    }

    /// Rebuild the viz behind `h` with `make` and mark it Ok (a YAML file
    /// that was edited and now loads). Returns false for an unknown handle.
    pub fn replace(&mut self, h: VizHandle, make: MakeViz) -> bool {
        match self.slots.get_mut(&h.0) {
            Some(slot) => {
                slot.viz = Some(Mutex::new(make()));
                slot.entry.status = EntryStatus::Ok;
                true
            }
            None => false,
        }
    }

    /// Mark `h` as failed with `msg` and drop its viz (a YAML file that was
    /// edited and no longer loads). Returns false for an unknown handle.
    pub fn mark_failed(&mut self, h: VizHandle, msg: &str) -> bool {
        match self.slots.get_mut(&h.0) {
            Some(slot) => {
                slot.viz = None;
                slot.entry.status = EntryStatus::Failed(msg.to_string());
                true
            }
            None => false,
        }
    }

    /// Remove `h`. It vanishes from `entries()` and the handle is never
    /// issued again. Returns false for an unknown handle.
    pub fn remove(&mut self, h: VizHandle) -> bool {
        self.slots.remove(&h.0).is_some()
    }
}

// ---- the process-wide registry -------------------------------------------

/// The one registry per process.
static GLOBAL: OnceLock<RwLock<Registry>> = OnceLock::new();

/// Run `f` on the global registry under its write lock. A poisoned lock
/// (a plugin panicked while held) is taken over: the registry's data is
/// plain and stays consistent.
fn with_global<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let lock = GLOBAL.get_or_init(|| RwLock::new(Registry::new()));
    let mut guard = lock.write().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// [`Registry::register`] on the global registry. Called by plugin
/// registration code at startup.
pub fn register(
    id: &str,
    title: &str,
    source: Source,
    enabled_by_default: bool,
    make: MakeViz,
) -> Result<VizHandle, RegisterError> {
    with_global(|r| r.register(id, title, source, enabled_by_default, make))
}

/// [`Registry::entries`] on the global registry. Called by the host's view
/// cycle and the plugin listing.
pub fn entries() -> Vec<VizEntry> {
    with_global(|r| r.entries())
}

/// [`Registry::handle_of`] on the global registry. Called when `--viz <id>`
/// or `[viz] enable` names a plugin.
pub fn handle_of(id: &str) -> Option<VizHandle> {
    with_global(|r| r.handle_of(id))
}

/// [`Registry::with_viz`] on the global registry. Called by the host to
/// render a plugin and to read its legend, explain and help text.
pub fn with_viz<R>(h: VizHandle, f: impl FnOnce(&mut dyn Viz) -> R) -> Option<R> {
    with_global(|r| r.with_viz(h, f))
}

/// [`Registry::replace`] on the global registry. Called by the YAML watcher
/// when an edited file loads.
pub fn replace(h: VizHandle, make: MakeViz) -> bool {
    with_global(|r| r.replace(h, make))
}

/// [`Registry::mark_failed`] on the global registry. Called by the YAML
/// watcher when an edited file no longer loads.
pub fn mark_failed(h: VizHandle, msg: &str) -> bool {
    with_global(|r| r.mark_failed(h, msg))
}

/// [`Registry::remove`] on the global registry. Called by the YAML watcher
/// when a file is deleted.
pub fn remove(h: VizHandle) -> bool {
    with_global(|r| r.remove(h))
}
