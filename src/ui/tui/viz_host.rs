// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The TUI's side of viz plugins (`crate::viz`).
//!
//! `mod.rs` knows plugins only as `DisplayMode::Plugin(handle)`. Every
//! plugin-specific decision lives here, and each place `mod.rs` touches a
//! plugin is a one-line call into this file:
//!
//! * start-up: [`VizHost::new`] registers the Rust plugins, scans the YAML
//!   folders and checks `[viz] enable`; [`VizHost::start_mode`] turns
//!   `--mode` / `--viz` into the first view, and refuses an unknown `--viz`;
//! * each data tick: [`VizHost::tick`] re-checks the YAML folders at most
//!   once every [`POLL_EVERY`] and records history for the plugin on screen;
//! * each frame: [`VizHost::render`] draws the plugin on screen, behind a
//!   size guard and a panic guard, and says whether the view must close;
//! * keys and commands: [`VizHost::next_mode`] is the `v` cycle,
//!   [`VizHost::command_fallthrough`] is `/mode <id>` and `/viz <id>`;
//! * the status bar and the `l`, `!` and `?` panels: [`statusbar_group`],
//!   [`legend_overlay`], [`explain_overlay`], [`help_overlay`] and
//!   [`plugin_views_section`].
//!
//! Every call into plugin code (`min_size`, `render`, `signals`, `legend`,
//! `explain`, `help`, `configure`, and the viz's `Drop` after a failure) goes
//! through one guard, [`call`]: a panic there turns the plugin off and the
//! TUI goes on. The host never calls `id` or `title`; it reads both from the
//! registry listing.
//!
//! Most of the logic is in plain functions that take the enabled list as an
//! argument ([`cycle_list`], [`next_mode_in`], [`command_in`],
//! [`resolve_start`]), so the tests in `viz_host_tests.rs` can check it
//! without the process-wide registry deciding what is enabled.

use super::{next_display_mode, DisplayMode, OverlayPanel, STATUS_BAR_ROWS};
use crate::config::VizSettings;
use crate::viz::declarative::loader::{Change, Loader};
use crate::viz::{registry, EntryStatus, History, Source, Viz, VizCtx, VizEntry, VizHandle};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    Frame,
};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tt_toplike_bus::{HeaderKind, LocalView, Value, World, WorldRead};

/// How often the YAML folders are re-checked. `Loader::poll` holds the
/// registry's write lock while it reads changed files, so it must not run
/// every frame.
pub(super) const POLL_EVERY: Duration = Duration::from_secs(2);

/// History is recorded on one data tick in this many, and only for the
/// plugin on screen.
pub(super) const HISTORY_EVERY: u64 = 4;

/// What [`VizHost::render`] did with one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RenderOutcome {
    /// The plugin drew its view.
    Drawn,
    /// The view area is smaller than the plugin's `min_size`. A centred
    /// "needs W x H (have w x h)" line was drawn and `render` was not called.
    TooSmall,
    /// The plugin panicked in `min_size` or `render`. The entry is now
    /// marked failed and its viz dropped; the string is the failure message.
    Crashed(String),
    /// There is no viz behind the handle: the YAML file was deleted, or the
    /// entry failed (earlier panic, or a file that never loaded).
    Gone,
}

thread_local! {
    /// True while this thread is running plugin code under [`call`]. The TUI
    /// panic hook checks it ([`hook_skips_restore`]): a plugin panic is
    /// caught here, so the hook must not restore the terminal for it.
    static IN_PLUGIN: Cell<bool> = const { Cell::new(false) };
}

/// True while the calling thread is running plugin code under [`call`].
fn in_plugin_call() -> bool {
    IN_PLUGIN.with(Cell::get)
}

/// The panic hook's decision for a panic raised while `in_plugin` is the
/// state of [`IN_PLUGIN`]: `true` means the panic will be caught by
/// [`call`], so the hook logs it and leaves the terminal alone.
fn skips_restore(in_plugin: bool) -> bool {
    in_plugin
}

/// [`skips_restore`] for the calling thread. Called by `install_panic_hook`
/// in `mod.rs`.
pub(super) fn hook_skips_restore() -> bool {
    skips_restore(in_plugin_call())
}

/// Sets [`IN_PLUGIN`] for its lifetime. Clearing it in `drop` means a
/// panicking plugin call clears it too, as the panic unwinds through it.
struct PluginFlag;

impl PluginFlag {
    fn set() -> PluginFlag {
        IN_PLUGIN.with(|c| c.set(true));
        PluginFlag
    }
}

impl Drop for PluginFlag {
    fn drop(&mut self) {
        IN_PLUGIN.with(|c| c.set(false));
    }
}

/// What one guarded call into a plugin returned.
#[derive(Debug, PartialEq)]
enum Call<R> {
    /// The plugin method returned this.
    Done(R),
    /// There is no viz behind the handle; no plugin code ran.
    Gone,
    /// The plugin method panicked. The entry is marked failed, its viz
    /// dropped and one line logged; the string is the failure message.
    Panicked(String),
}

/// Run `f` on the viz behind `h`: the one way the host calls plugin code.
///
/// `f` runs under `catch_unwind`, with [`IN_PLUGIN`] set so the TUI panic
/// hook does not restore the terminal. On a panic the entry is marked
/// failed with "panicked in `what`: ..." and its viz dropped (see
/// [`fail`]), so the same viz cannot panic again. `what` names the method,
/// for the message.
fn call<R>(h: VizHandle, what: &str, f: impl FnOnce(&mut dyn Viz) -> R) -> Call<R> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _flag = PluginFlag::set();
        registry::with_viz(h, f)
    }));
    match result {
        Ok(Some(r)) => Call::Done(r),
        Ok(None) => Call::Gone,
        Err(payload) => {
            let msg = format!("panicked in {what}: {}", panic_text(payload.as_ref()));
            fail(h, &msg);
            Call::Panicked(msg)
        }
    }
}

/// Mark `h` failed with `msg`, drop its viz, and log one line.
///
/// The registry rule: no plugin code runs inside the global registry lock.
/// `registry::mark_failed` drops the slot's pointer under that lock, so a
/// second pointer is taken first; the slot's copy is then not the last one,
/// and the viz (whose `Drop` is plugin code) is dropped here, after the lock
/// is released, under the same guard as any other plugin call.
fn fail(h: VizHandle, msg: &str) {
    let keep = registry::with_global(|r| r.viz_arc(h));
    registry::mark_failed(h, msg);
    log::error!("viz `{}` {msg}; it is turned off", id_of(h));
    let dropped = catch_unwind(AssertUnwindSafe(move || {
        let _flag = PluginFlag::set();
        drop(keep);
    }));
    if dropped.is_err() {
        log::error!("viz `{}` also panicked while being dropped", id_of(h));
    }
}

/// The (id, title) of each handle seen, so the status bar does not list and
/// sort the registry every frame, and a removed handle's id is still known
/// for the message that closes its view. Handles are never reused, so a
/// cached row cannot belong to another viz. [`VizHost::tick`] forgets a row
/// when its YAML file is reloaded (the title may have changed).
static NAMES: OnceLock<Mutex<HashMap<VizHandle, (String, String)>>> = OnceLock::new();

/// The cached (id, title) of `h`, filled from the registry on first use.
/// `None` for a handle never registered.
fn names(h: VizHandle) -> Option<(String, String)> {
    let cache = NAMES.get_or_init(Default::default);
    if let Some(n) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&h) {
        return Some(n.clone());
    }
    // Read the registry without holding the cache lock.
    let e = entry(h)?;
    let n = (e.id, e.title);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(h)
        .or_insert_with(|| n.clone());
    Some(n)
}

/// Drop the cached names of `h` (its file was reloaded).
fn forget_names(h: VizHandle) {
    if let Some(cache) = NAMES.get() {
        cache.lock().unwrap_or_else(|e| e.into_inner()).remove(&h);
    }
}

/// The id of `h` for messages, or `#n` for a handle never seen.
fn id_of(h: VizHandle) -> String {
    names(h).map_or_else(|| format!("#{}", h.0), |n| n.0)
}

/// The TUI's plugin state: the YAML loader, the history plugins read, and
/// which plugins the user enabled for this run.
pub(super) struct VizHost {
    /// Watches the YAML folders; [`VizHost::tick`] polls it.
    loader: Loader,
    /// Recent values of the on-screen plugin's signals.
    history: History,
    /// When the loader last polled; `None` until the first poll.
    last_poll: Option<Instant>,
    /// How many polls ran. Read by the rate-limit test.
    polls: u64,
    /// Data ticks seen; `VizCtx::tick`, and the history cadence.
    ticks: u64,
    /// Ids enabled on top of `enabled_by_default`: `[viz] enable` (deduped)
    /// plus the `--viz` one for this run.
    enabled: BTreeSet<String>,
    /// `[viz.<id>]` tables, handed to each viz's `configure`.
    per_viz: BTreeMap<String, toml_edit::Table>,
    /// One config problem per unknown id in `[viz] enable`.
    problems: Vec<String>,
}

impl VizHost {
    /// Register the Rust plugins (`crate::viz::init`, whose errors come back
    /// once per process and are logged here), scan the YAML folders, then
    /// check `[viz] enable` against what loaded.
    ///
    /// Reads the plugin folders on disk. Called once by `run_tui`, before
    /// the backend, the bus and raw mode start.
    pub(super) fn new(config: &VizSettings) -> VizHost {
        for e in crate::viz::init() {
            log::warn!("viz plugin not registered: {e}");
        }
        let mut loader = Loader::new(Loader::default_dirs());
        // The loader logs each file that fails to load itself.
        let _ = loader.scan();
        let mut host = VizHost::with_loader(loader, config);
        host.last_poll = Some(Instant::now());
        for e in registry::entries() {
            host.configure(e.handle);
        }
        host
    }

    /// A host over `loader` without touching disk or calling `init`. The
    /// unknown-id check runs against the registry as it is now. Used by
    /// [`VizHost::new`] and by tests.
    pub(super) fn with_loader(loader: Loader, config: &VizSettings) -> VizHost {
        // `[viz] enable` may name an id twice; one problem line per id.
        let mut seen = BTreeSet::new();
        let deduped = VizSettings {
            enable: config
                .enable
                .iter()
                .filter(|id| seen.insert(id.as_str()))
                .cloned()
                .collect(),
            per_viz: BTreeMap::new(),
        };
        let problems: Vec<String> = deduped
            .unknown_enabled(&|id| registry::handle_of(id).is_some())
            .into_iter()
            .map(|id| format!("[viz] enable: no viz has the id `{id}`"))
            .collect();
        for p in &problems {
            log::warn!("config: {p}");
        }
        VizHost {
            loader,
            history: History::default(),
            last_poll: None,
            polls: 0,
            ticks: 0,
            enabled: deduped.enable.into_iter().collect(),
            per_viz: config.per_viz.clone(),
            problems,
        }
    }

    /// The config problems found by [`VizHost::new`], one per unknown id.
    /// Tests read them; the TUI logs them and shows the first.
    #[cfg(test)]
    pub(super) fn problems(&self) -> &[String] {
        &self.problems
    }

    /// The status-line message for the first problem, as an error, the way
    /// `config::status_message` reports a config problem.
    pub(super) fn status_problem(&self) -> Option<(String, bool)> {
        let first = self.problems.first()?;
        let more = self.problems.len() - 1;
        Some(if more == 0 {
            (format!("config: {first}"), true)
        } else {
            (format!("config: {first} (and {more} more)"), true)
        })
    }

    /// The first view, from `--mode` and `--viz` (`--viz` wins; see
    /// `cli::effective_start`). A known `--viz` id is enabled for this run
    /// even when it is off by default. An unknown one (the empty string
    /// too) is an error whose text lists every registered id; `run_tui`
    /// returns it before touching the terminal, so the TUI exits with it.
    pub(super) fn start_mode(&mut self, cli: &crate::cli::Cli) -> Result<DisplayMode, String> {
        let start = crate::cli::effective_start(cli.mode, cli.viz.as_deref());
        let entries = registry::entries();
        let mode = resolve_start(start, &entries)?;
        if let DisplayMode::Plugin(h) = mode {
            if let Some(e) = entries.iter().find(|e| e.handle == h) {
                self.enabled.insert(e.id.clone());
            }
        }
        Ok(mode)
    }

    /// Hand the viz behind `h` its `[viz.<id>]` table, if the config has one.
    /// The id comes from the registry, so no plugin code runs unless there
    /// is a table to hand over.
    fn configure(&self, h: VizHandle) {
        let Some((id, _)) = names(h) else {
            return;
        };
        if let Some(t) = self.per_viz.get(&id) {
            let _ = call(h, "configure", |v| v.configure(t));
        }
    }

    /// Once per data tick, in every mode.
    ///
    /// Polls the YAML folders when [`POLL_EVERY`] has passed since the last
    /// poll (`now` is passed in so tests can move the clock). Every
    /// [`HISTORY_EVERY`] ticks, and only when `mode` is a plugin, reads
    /// the world through `world` and records that plugin's `signals()`. A
    /// plugin that is not on screen costs nothing: `world` is not called.
    pub(super) fn tick<W: WorldRead>(
        &mut self,
        now: Instant,
        mode: DisplayMode,
        world: impl FnOnce() -> W,
    ) {
        self.ticks = self.ticks.wrapping_add(1);
        let due = self
            .last_poll
            .is_none_or(|t| now.saturating_duration_since(t) >= POLL_EVERY);
        if due {
            self.last_poll = Some(now);
            self.polls += 1;
            for c in self.loader.poll() {
                // An edited or new file gets its `[viz.<id>]` table again,
                // and an edited one may have a new title.
                if let Change::Updated(h) = c {
                    forget_names(h);
                }
                if let Change::Added(h) | Change::Updated(h) = c {
                    self.configure(h);
                }
            }
        }
        if let DisplayMode::Plugin(h) = mode {
            if self.ticks.is_multiple_of(HISTORY_EVERY) {
                self.record(h, &world());
            }
        }
    }

    /// Record the current value of each signal `h` declares, for every
    /// entity of the signal's kind that has a numeric reading.
    fn record(&mut self, h: VizHandle, world: &dyn WorldRead) {
        let Call::Done(specs) = call(h, "signals", |v| v.signals()) else {
            return;
        };
        let mut done: Vec<(tt_toplike_bus::EntityKind, &str)> = Vec::new();
        let mut live: Vec<(tt_toplike_bus::EntityKind, std::collections::HashSet<_>)> = Vec::new();
        for s in &specs {
            if done.contains(&(s.kind, s.name.as_str())) {
                continue;
            }
            done.push((s.kind, s.name.as_str()));
            let keys = world.entities(s.kind);
            for key in &keys {
                if let Some(x) = world.reading(key, &s.name).and_then(|r| number(&r.value)) {
                    self.history.record(key, &s.name, x);
                }
            }
            match live.iter_mut().find(|(k, _)| *k == s.kind) {
                Some((_, set)) => set.extend(keys),
                None => live.push((s.kind, keys.into_iter().collect())),
            }
        }
        // Forget the series of entities that have left the world, for the
        // kinds just recorded (a process that exited, a lease that ended).
        for (kind, keys) in &live {
            self.history.retain_entities(*kind, keys);
        }
    }

    /// The history plugins read. Tests check what was recorded.
    #[cfg(test)]
    pub(super) fn history(&self) -> &History {
        &self.history
    }

    /// How many times the loader polled. Tests check the rate limit.
    #[cfg(test)]
    pub(super) fn polls(&self) -> u64 {
        self.polls
    }

    /// The plugins in the `v` cycle now, in id order (see [`cycle_list`]).
    pub(super) fn plugins(&self) -> Vec<VizHandle> {
        cycle_list(&registry::entries(), &self.enabled, drawable)
    }

    /// Where `v` goes from `m` (see [`next_mode_in`]). Called by the `v`
    /// key handler.
    pub(super) fn next_mode(&self, m: DisplayMode) -> DisplayMode {
        next_mode_in(m, &self.plugins())
    }

    /// `/mode <id>` and `/viz <id>` after `execute_command` has had its go
    /// (see [`command_in`]). Called by the command bar's Enter handler.
    pub(super) fn command_fallthrough(
        &self,
        cmd: &str,
        out: (String, bool),
        mode: &mut DisplayMode,
    ) -> (String, bool) {
        command_in(cmd, out, mode, &self.rows())
    }

    /// (handle, id, title) of each plugin in the cycle.
    fn rows(&self) -> Vec<(VizHandle, String, String)> {
        let entries = registry::entries();
        self.plugins()
            .into_iter()
            .filter_map(|h| {
                let e = entries.iter().find(|e| e.handle == h)?;
                Some((h, e.id.clone(), e.title.clone()))
            })
            .collect()
    }

    /// (id, title) of each enabled plugin, for the help panel's "Plugin
    /// views" section; empty unless `kind` is the help panel, so the draw
    /// loop asks the registry only while that panel is open.
    pub(super) fn help_rows(&self, kind: OverlayPanel) -> Vec<(String, String)> {
        if kind != OverlayPanel::Help {
            return Vec::new();
        }
        self.rows().into_iter().map(|(_, id, t)| (id, t)).collect()
    }

    /// Draw the plugin `h` over the frame minus the status bar. See
    /// [`VizHost::render_into`].
    pub(super) fn render(
        &mut self,
        h: VizHandle,
        f: &mut Frame,
        world: Option<&dyn WorldRead>,
    ) -> RenderOutcome {
        let area = view_area(f.area());
        self.render_into(h, f.buffer_mut(), area, world)
    }

    /// Draw the plugin `h` into `area` of `buf`.
    ///
    /// * No viz behind `h`: `Gone`, nothing drawn.
    /// * `area` smaller than its `min_size`: a centred "needs W x H (have
    ///   w x h)" line and `TooSmall`; the plugin's `render` is not called.
    /// * `min_size` and `render` run through [`call`]. A panic in either
    ///   marks the entry failed and drops its viz (so it cannot panic again
    ///   next frame), logs one line, and returns `Crashed`.
    ///
    /// `world` is `None` only if the caller took no bus snapshot; the
    /// plugin then sees an empty world.
    pub(super) fn render_into(
        &mut self,
        h: VizHandle,
        buf: &mut Buffer,
        area: Rect,
        world: Option<&dyn WorldRead>,
    ) -> RenderOutcome {
        // Cache who `h` is while it is still registered, so the message after
        // a removal can name it. A cache hit on every later frame.
        let _ = names(h);
        let (min_w, min_h) = match call(h, "min_size", |v| v.min_size()) {
            Call::Done(size) => size,
            Call::Gone => return RenderOutcome::Gone,
            Call::Panicked(msg) => return RenderOutcome::Crashed(msg),
        };
        if area.width < min_w || area.height < min_h {
            draw_needs(buf, area, (min_w, min_h));
            return RenderOutcome::TooSmall;
        }
        let empty;
        let world: &dyn WorldRead = match world {
            Some(w) => w,
            None => {
                empty = empty_world();
                &empty
            }
        };
        let ctx = VizCtx {
            world,
            history: &self.history,
            tick: self.ticks,
            now_mono_ns: world.now_mono_ns(),
        };
        match call(h, "render", |v| v.render(buf, area, &ctx)) {
            Call::Done(()) => RenderOutcome::Drawn,
            // Removed between the size check and the render.
            Call::Gone => RenderOutcome::Gone,
            Call::Panicked(msg) => RenderOutcome::Crashed(msg),
        }
    }

    /// The status-line message for a render that closes the view (`Gone`
    /// or `Crashed`), or `None` when the view stays. The frame loop shows
    /// it for a few seconds over Insights.
    pub(super) fn leave_message(&self, h: VizHandle, outcome: &RenderOutcome) -> Option<String> {
        let id = || id_of(h);
        match outcome {
            RenderOutcome::Drawn | RenderOutcome::TooSmall => None,
            RenderOutcome::Crashed(msg) => Some(format!("viz crashed: {} ({msg})", id())),
            RenderOutcome::Gone => Some(match entry(h) {
                Some(VizEntry {
                    id,
                    status: EntryStatus::Failed(msg),
                    ..
                }) => format!("viz {id} is unavailable: {msg}"),
                Some(e) => format!("viz {} is unavailable", e.id),
                None => format!("viz {} was removed", id()),
            }),
        }
    }
}

/// True when the registry holds a drawable viz behind `h`. A placeholder
/// (a YAML file that never loaded) and a crashed plugin have none; a file
/// that broke after loading keeps its last good viz and stays drawable.
/// The closure runs no plugin code, so it needs no [`call`] guard.
fn drawable(h: VizHandle) -> bool {
    registry::with_viz(h, |_| ()).is_some()
}

/// The registry entry for `h`, if it is still registered.
fn entry(h: VizHandle) -> Option<VizEntry> {
    registry::entries().into_iter().find(|e| e.handle == h)
}

/// The plugin's title for the status bar, or `#n` for a handle never seen.
/// Cached (see [`NAMES`]), so a frame does not list the registry.
pub(super) fn name_of(h: VizHandle) -> String {
    names(h).map_or_else(|| format!("#{}", h.0), |n| n.1)
}

/// The plugins in the `v` cycle: entries enabled by default or listed in
/// `enabled`, that `drawable` says have a viz, in id order (`entries`
/// comes sorted by id from the registry and the order is kept).
pub(super) fn cycle_list(
    entries: &[VizEntry],
    enabled: &BTreeSet<String>,
    drawable: impl Fn(VizHandle) -> bool,
) -> Vec<VizHandle> {
    entries
        .iter()
        .filter(|e| e.enabled_by_default || enabled.contains(&e.id))
        .filter(|e| drawable(e.handle))
        .map(|e| e.handle)
        .collect()
}

/// The plugin `v` goes to from `current`, given the cycle `list`: the
/// first one from Die 3D, the next one from a plugin, `None` after the
/// last plugin, from a plugin no longer in the list, and from any other
/// view.
pub(super) fn cycle_after_in(current: DisplayMode, list: &[VizHandle]) -> Option<VizHandle> {
    match current {
        DisplayMode::Die3d => list.first().copied(),
        DisplayMode::Plugin(h) => {
            let i = list.iter().position(|x| *x == h)?;
            list.get(i + 1).copied()
        }
        _ => None,
    }
}

/// Where `v` goes from `m`. The built-in views keep `next_display_mode`'s
/// order. Die 3D and each plugin go to the next plugin in `list`, and to
/// Insights after the last, so with an empty `list` this is exactly
/// `next_display_mode`.
pub(super) fn next_mode_in(m: DisplayMode, list: &[VizHandle]) -> DisplayMode {
    match m {
        DisplayMode::Die3d | DisplayMode::Plugin(_) => cycle_after_in(m, list)
            .map(DisplayMode::Plugin)
            .unwrap_or(DisplayMode::Insights),
        _ => next_display_mode(m),
    }
}

/// `/mode <id>` and `/viz <id>`, run after `execute_command` returned
/// `out` (message, is error) for the same `cmd`.
///
/// * `/mode` that `execute_command` accepted (a built-in name) is left
///   alone: built-in names win.
/// * `/mode <id>` it refused, and any `/viz <id>`, switch to the plugin
///   with that id in `rows` (handle, id, title), the enabled plugins.
/// * No such plugin: an error line naming the enabled plugin ids (for
///   `/mode`, after the built-in names `execute_command` listed).
/// * Any other command: `out` unchanged.
pub(super) fn command_in(
    cmd: &str,
    out: (String, bool),
    mode: &mut DisplayMode,
    rows: &[(VizHandle, String, String)],
) -> (String, bool) {
    let mut parts = cmd.trim().splitn(2, ' ');
    let verb = parts
        .next()
        .unwrap_or("")
        .trim_start_matches('/')
        .to_lowercase();
    let arg = parts.next().unwrap_or("").trim();
    let is_mode = verb == "mode";
    if !(is_mode || verb == "viz") || (is_mode && !out.1) {
        return out;
    }
    if let Some((h, _, title)) = rows.iter().find(|(_, id, _)| id == arg) {
        *mode = DisplayMode::Plugin(*h);
        return (format!("→ {title}"), false);
    }
    let ids = if rows.is_empty() {
        "none enabled".to_string()
    } else {
        rows.iter()
            .map(|(_, id, _)| id.as_str())
            .collect::<Vec<_>>()
            .join("|")
    };
    if is_mode {
        (format!("{}; plugins: {ids}", out.0), true)
    } else {
        (
            format!("viz: no enabled plugin `{arg}`; plugins: {ids}"),
            true,
        )
    }
}

/// The first view for `start` (from `cli::effective_start`), checked
/// against the registry listing `entries`. An unknown `--viz` id is an
/// error carrying `cli::unknown_viz_message`, listing every registered id
/// and where it came from.
pub(super) fn resolve_start(
    start: crate::cli::StartView,
    entries: &[VizEntry],
) -> Result<DisplayMode, String> {
    use crate::cli::StartView;
    match start {
        StartView::Default => Ok(DisplayMode::Insights),
        StartView::Mode(m) => Ok(super::display_mode_for(m)),
        StartView::Viz(id) => match entries.iter().find(|e| e.id == id) {
            Some(e) => Ok(DisplayMode::Plugin(e.handle)),
            None => {
                let listing: Vec<(String, String)> = entries
                    .iter()
                    .map(|e| (e.id.clone(), source_name(&e.source)))
                    .collect();
                Err(crate::cli::unknown_viz_message(&id, &listing))
            }
        },
    }
}

/// Where an entry came from, for the unknown-id message: `rust` for a
/// compiled-in folder plugin, the file's path for a YAML one.
fn source_name(s: &Source) -> String {
    match s {
        Source::Folder => "rust".to_string(),
        // A file name is chosen by whoever wrote the file: strip control
        // characters as `viz list` does, so an ESC cannot reach the terminal.
        Source::File(p) => p
            .display()
            .to_string()
            .chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect(),
    }
}

/// The handle when `mode` is a plugin view. Used by `mod.rs` wherever it
/// needs to know whether a plugin is on screen.
pub(super) fn active(mode: DisplayMode) -> Option<VizHandle> {
    match mode {
        DisplayMode::Plugin(h) => Some(h),
        _ => None,
    }
}

/// The area a plugin draws in: the frame minus the status bar rows, as for
/// the other full-screen views.
pub(super) fn view_area(frame: Rect) -> Rect {
    Rect {
        height: frame.height.saturating_sub(STATUS_BAR_ROWS as u16),
        ..frame
    }
}

/// The size guard's message, centred in `area` and cut to its width.
fn draw_needs(buf: &mut Buffer, area: Rect, (w, h): (u16, u16)) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let text = format!("needs {w} x {h} (have {} x {})", area.width, area.height);
    let text: String = text.chars().take(area.width as usize).collect();
    let len = text.chars().count() as u16;
    let x = area.x + (area.width - len) / 2;
    let y = area.y + area.height / 2;
    buf.set_string(
        x,
        y,
        text,
        Style::default().fg(crate::ui::colors::rgb(160, 160, 160)),
    );
}

/// A world with nothing in it, for a render with no bus snapshot.
fn empty_world() -> LocalView {
    LocalView {
        seq: 0,
        world: World::new(),
        now_mono_ns: 0,
        header_kind: HeaderKind::Live,
    }
}

/// A reading as a number for the history, or `None` for text and for a
/// value that is not finite.
fn number(v: &Value) -> Option<f64> {
    let x = match v {
        Value::F64(x) => *x,
        Value::U64(x) => *x as f64,
        Value::I64(x) => *x as f64,
        Value::Text(_) => return None,
    };
    x.is_finite().then_some(x)
}

/// The text of a panic payload (`panic!` with a literal or a format).
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// The status-bar group for a plugin view: `Esc leave <title>`, as Die 3D
/// and Saga show `3 leave` and `S leave`. `None` in any other mode.
pub(super) fn statusbar_group(
    mode: DisplayMode,
    key: Style,
    label: Style,
) -> Option<Vec<Span<'static>>> {
    let h = active(mode)?;
    Some(vec![
        Span::styled(" Esc ", key),
        Span::styled(format!(" leave {}", name_of(h)), label),
    ])
}

/// One `║ `-prefixed overlay line holding `spans`, as `overlay_lines_with`
/// builds them.
fn bar_line(bar: Color, bg: Color, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut v = vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
    v.extend(spans);
    Line::from(v)
}

/// The legend panel (`l`) for plugin `h`: its `legend()` lines, or a line
/// saying it has none.
pub(super) fn legend_overlay(
    h: VizHandle,
    bar: Color,
    bg: Color,
    dim: Color,
) -> Vec<Line<'static>> {
    let lines = match call(h, "legend", |v| v.legend()) {
        Call::Done(lines) => lines,
        Call::Gone | Call::Panicked(_) => Vec::new(),
    };
    let lines = if lines.is_empty() {
        vec!["  (this view has no legend)".to_string()]
    } else {
        lines
    };
    lines
        .into_iter()
        .map(|l| bar_line(bar, bg, vec![Span::styled(l, Style::default().fg(dim))]))
        .collect()
}

/// The text lines of the explain panel (`!`) for plugin `h`: its title, a
/// blank line, then its `explain()` text, one entry per line of it.
fn explain_text(h: VizHandle) -> Vec<String> {
    let body = match call(h, "explain", |v| v.explain()) {
        Call::Done(text) => text,
        Call::Gone | Call::Panicked(_) => "This view is no longer available.".to_string(),
    };
    let mut text = vec![name_of(h), String::new()];
    text.extend(body.lines().map(str::to_string));
    text
}

/// The explain panel (`!`) for plugin `h`, wrapped by the same
/// `explain_lines` the built-in views use.
pub(super) fn explain_overlay(h: VizHandle, colors: [Color; 4], cols: usize) -> Vec<Line<'static>> {
    let [bar, bg, dim, lbl] = colors;
    let text = explain_text(h);
    let text: Vec<&str> = text.iter().map(String::as_str).collect();
    super::explain_lines(bar, bg, dim, lbl, &text, cols)
}

/// The help panel (`?`) while plugin `h` is on screen: a heading, its
/// `help()` text wrapped like the explain text, then the first two rows of
/// the global keys (`global_keys`, as the help panel builds them), as the
/// Saga and Die 3D panels do.
pub(super) fn help_overlay(
    h: VizHandle,
    colors: [Color; 5],
    cols: usize,
    global_keys: Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    let [bar, bg, dim, lbl, head] = colors;
    let heading = |t: String| {
        bar_line(
            bar,
            bg,
            vec![Span::styled(
                t,
                Style::default().fg(head).add_modifier(Modifier::BOLD),
            )],
        )
    };
    let body = match call(h, "help", |v| v.help()) {
        Call::Done(text) => text,
        Call::Gone | Call::Panicked(_) => String::new(),
    };
    let mut text: Vec<&str> = body.lines().collect();
    text.push("Esc leaves this view; v goes to the next one.");
    let mut out = vec![heading(format!("{} keys", name_of(h)))];
    out.extend(super::explain_lines(bar, bg, dim, lbl, &text, cols));
    out.push(heading(
        "Everywhere  (every key: ? in another view)".to_string(),
    ));
    out.extend(global_keys.into_iter().take(2));
    out
}

/// The help panel's "Plugin views" section: a heading and one line per
/// enabled plugin, `id  title`. Empty when no plugin is enabled, so the
/// panel is then unchanged.
pub(super) fn plugin_views_section(
    rows: &[(String, String)],
    bar: Color,
    bg: Color,
    head: Color,
    key: Color,
    dim: Color,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let w = rows
        .iter()
        .map(|(id, _)| id.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = vec![bar_line(
        bar,
        bg,
        vec![Span::styled(
            "Plugin views  (v after the Waffler, or /viz <id>)",
            Style::default().fg(head).add_modifier(Modifier::BOLD),
        )],
    )];
    out.extend(rows.iter().map(|(id, title)| {
        bar_line(
            bar,
            bg,
            vec![
                Span::styled(
                    format!(" {id:<w$}"),
                    Style::default().fg(key).add_modifier(Modifier::BOLD),
                ),
                Span::styled(title.clone(), Style::default().fg(dim)),
            ],
        )
    }));
    out
}

#[cfg(test)]
#[path = "viz_host_tests.rs"]
mod tests;
