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
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// A handle to a registered viz. Handed out once per registration and never
/// reused, so a stale handle finds nothing instead of someone else's viz.
/// The TUI's `DisplayMode::Plugin` carries one for a plugin view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VizHandle(pub u32);

/// Where a viz came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A Rust plugin compiled in from a plugin folder.
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
    /// The plugin panicked while it was being built or asked for its id and
    /// title. The string is the panic message. The plugin is not registered.
    Panicked(String),
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegisterError::BadId => write!(f, "viz id must be kebab-case: [a-z0-9]+(-[a-z0-9]+)*"),
            RegisterError::Duplicate => write!(f, "a viz with this id is already registered"),
            RegisterError::Panicked(m) => write!(f, "the plugin panicked during start-up: {m}"),
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

/// Lock `viz` (recovering a poisoned mutex) and run `f` on it.
fn run_on<R>(viz: &Arc<Mutex<Box<dyn Viz>>>, f: impl FnOnce(&mut dyn Viz) -> R) -> R {
    let mut guard = viz.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.as_mut())
}

/// One live registration. A failed entry keeps no viz.
///
/// The viz is an `Arc<Mutex<..>>`. `Viz` is `Send` but not `Sync`, and the
/// per-viz mutex makes the slot `Sync` (a `RwLock<Registry>` in a static needs
/// that). It also lets `with_viz` clone the `Arc`, drop the registry lock and
/// then run the closure, so a render never holds the registry lock.
struct Slot {
    entry: VizEntry,
    viz: Option<Arc<Mutex<Box<dyn Viz>>>>,
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
        self.register_built(id, title, source, enabled_by_default, make())
    }

    /// Like [`Registry::register`] with the viz already built. The global
    /// `register` builds the viz first, outside the lock, and calls this.
    pub fn register_built(
        &mut self,
        id: &str,
        title: &str,
        source: Source,
        enabled_by_default: bool,
        viz: Box<dyn Viz>,
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
                viz: Some(Arc::new(Mutex::new(viz))),
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

    /// The shared pointer to the viz behind `h`, if it is live and loaded.
    /// The global `with_viz` takes it under the lock, releases the lock and
    /// only then runs the closure.
    pub fn viz_arc(&self, h: VizHandle) -> Option<Arc<Mutex<Box<dyn Viz>>>> {
        self.slots.get(&h.0)?.viz.clone()
    }

    /// Run `f` on the viz behind `h`. `None` when the handle is unknown or
    /// removed, or the entry failed to load. A poisoned per-viz mutex (an
    /// earlier render panicked) is recovered, not propagated.
    ///
    /// Re-entering the same handle from inside `f` deadlocks on the per-viz
    /// mutex. Other registry calls are fine.
    pub fn with_viz<R>(&self, h: VizHandle, f: impl FnOnce(&mut dyn Viz) -> R) -> Option<R> {
        Some(run_on(&self.viz_arc(h)?, f))
    }

    /// Rebuild the viz behind `h` with `make` and mark it Ok (a YAML file
    /// that was edited and now loads). Returns false for an unknown handle.
    pub fn replace(&mut self, h: VizHandle, make: MakeViz) -> bool {
        if !self.slots.contains_key(&h.0) {
            return false;
        }
        self.replace_built(h, make())
    }

    /// Like [`Registry::replace`] with the viz already built. A render in
    /// flight on the old instance finishes on it.
    ///
    /// The old viz is dropped here, inside the registry lock. That is fine
    /// for a YAML `DeclarativeViz` (plain data, no custom `Drop`); a future
    /// Rust-plugin reload path must drop the old viz outside the lock.
    pub fn replace_built(&mut self, h: VizHandle, viz: Box<dyn Viz>) -> bool {
        match self.slots.get_mut(&h.0) {
            Some(slot) => {
                slot.viz = Some(Arc::new(Mutex::new(viz)));
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

    /// Mark `h` as failed with `msg` but keep its current viz, so the last
    /// good version stays drawable while the listing shows the error (a YAML
    /// file that was edited into a broken state). Returns false for an
    /// unknown handle. [`Registry::replace_built`] restores `Ok` later.
    pub fn mark_failed_keep_viz(&mut self, h: VizHandle, msg: &str) -> bool {
        match self.slots.get_mut(&h.0) {
            Some(slot) => {
                slot.entry.status = EntryStatus::Failed(msg.to_string());
                true
            }
            None => false,
        }
    }

    /// Remove `h`. It vanishes from `entries()` and the handle is never
    /// issued again. Returns false for an unknown handle.
    ///
    /// The viz is dropped here, inside the registry lock. That is fine for a
    /// YAML `DeclarativeViz` (plain data, no custom `Drop`); a future
    /// Rust-plugin reload path must drop the viz outside the lock.
    pub fn remove(&mut self, h: VizHandle) -> bool {
        self.slots.remove(&h.0).is_some()
    }
}

// ---- the process-wide registry -------------------------------------------

/// The one registry per process.
static GLOBAL: OnceLock<RwLock<Registry>> = OnceLock::new();

/// Run `f` on the global registry under its write lock. A poisoned lock
/// is taken over: the registry's data is
/// plain and stays consistent.
pub(crate) fn with_global<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let lock = GLOBAL.get_or_init(|| RwLock::new(Registry::new()));
    let mut guard = lock.write().unwrap_or_else(|e| e.into_inner());
    #[cfg(test)]
    let _held = HeldHere::set();
    f(&mut guard)
}

#[cfg(test)]
thread_local! {
    /// Test builds only: true while this thread is inside [`with_global`].
    static HELD_HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test builds only: sets [`HELD_HERE`] for its lifetime.
#[cfg(test)]
struct HeldHere;

#[cfg(test)]
impl HeldHere {
    fn set() -> HeldHere {
        HELD_HERE.with(|c| c.set(true));
        HeldHere
    }
}

#[cfg(test)]
impl Drop for HeldHere {
    fn drop(&mut self) {
        HELD_HERE.with(|c| c.set(false));
    }
}

/// Test builds only: true when the calling thread holds the global
/// registry lock. Tests use it to check that plugin code (a viz's `Drop`)
/// never runs under that lock. Per thread, so other tests running in
/// parallel cannot make it true.
#[cfg(test)]
pub(crate) fn this_thread_holds_global() -> bool {
    HELD_HERE.with(std::cell::Cell::get)
}

/// [`Registry::register`] on the global registry. Called by plugin
/// registration code at startup. The viz is built before the lock is taken.
pub fn register(
    id: &str,
    title: &str,
    source: Source,
    enabled_by_default: bool,
    make: MakeViz,
) -> Result<VizHandle, RegisterError> {
    if !is_valid_id(id) {
        return Err(RegisterError::BadId);
    }
    let viz = make();
    with_global(|r| r.register_built(id, title, source, enabled_by_default, viz))
}

/// Register a Rust plugin folder, with every call into plugin code guarded.
///
/// `make`, `id()`, `title()` and the viz's `Drop` are plugin code. They run
/// under `catch_unwind`, so a plugin that panics at start-up is skipped with
/// [`RegisterError::Panicked`] and the process carries on. The viz is built
/// once: that instance is the one registered. `folder` names the plugin in the
/// log line. The generated `register_all` calls this for each folder.
pub fn register_folder(
    folder: &str,
    enabled_by_default: bool,
    make: fn() -> Box<dyn Viz>,
) -> Result<VizHandle, RegisterError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let built = catch_unwind(AssertUnwindSafe(|| {
        let viz = make();
        let id = viz.id().to_string();
        let title = viz.title().to_string();
        (viz, id, title)
    }));
    let (viz, id, title) = match built {
        Ok(t) => t,
        Err(payload) => {
            let text = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            let e = RegisterError::Panicked(text);
            log::warn!("viz plugin (folder `{folder}`) not registered: {e}");
            return Err(e);
        }
    };
    let result = if is_valid_id(&id) {
        with_global(|r| r.register_built(&id, &title, Source::Folder, enabled_by_default, viz))
    } else {
        // Drop the viz under the guard too: its Drop is plugin code.
        let _ = catch_unwind(AssertUnwindSafe(move || drop(viz)));
        Err(RegisterError::BadId)
    };
    if let Err(e) = &result {
        log::warn!("viz plugin `{id}` (folder `{folder}`) not registered: {e}");
    }
    result
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
///
/// The registry lock is held only to clone the viz's pointer; `f` runs under
/// the viz's own mutex. So `f` may call other registry functions, and the
/// YAML watcher is never blocked by a render. Re-entering the same handle
/// from inside `f` deadlocks.
pub fn with_viz<R>(h: VizHandle, f: impl FnOnce(&mut dyn Viz) -> R) -> Option<R> {
    let arc = with_global(|r| r.viz_arc(h))?;
    Some(run_on(&arc, f))
}

/// [`Registry::mark_failed`] on the global registry. Called by the TUI host's
/// panic guard (`viz_host::fail`) when a plugin panics. (The YAML loader calls
/// the `Registry` method itself, inside `with_global`.)
pub fn mark_failed(h: VizHandle, msg: &str) -> bool {
    with_global(|r| r.mark_failed(h, msg))
}

/// [`Registry::remove`] on the global registry. Only tests call it. (The YAML
/// loader calls the `Registry` method itself, inside `with_global`, when a
/// file is deleted.)
pub fn remove(h: VizHandle) -> bool {
    with_global(|r| r.remove(h))
}
