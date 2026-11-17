# Viz Plugins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A viz registry with two kinds of plugin: a Rust folder under `src/viz/plugins/<name>/` and a YAML/JSON file. Each has one working example, a scaffold command, golden-frame tests, and an agent recipe.

**Architecture:** A new `src/viz/` module (gated on the `tui` feature) holds the `Viz` trait, a process-wide registry, a capped signal history, and a declarative (YAML/JSON) host that is itself a `Viz`. `build.rs` scans `src/viz/plugins/*/` and generates the module list and constructor table, so adding a folder edits no shared file. `src/ui/tui/mod.rs` gains one `DisplayMode::Plugin(VizHandle)` path that delegates to a new `src/ui/tui/viz_host.rs`. Built-in views are not touched.

**Tech Stack:** Rust, ratatui (`Buffer`/`Rect`), `tt-toplike-bus` (`WorldRead`, `EntityKey`, `Value`), serde, a maintained YAML crate (first candidate `serde_yaml_ng`; confirm it is maintained and builds for `aarch64-apple-darwin` and `x86_64-pc-windows-gnu`), `serde_json`, `toml_edit` (config, already used).

**Spec:** `docs/superpowers/specs/2026-11-04-viz-plugins-design.md`. One ruling made while planning: the spec's goal 7 (migrate one existing view) conflicts with the user's decision not to disrupt existing views, so it is dropped and the spec is edited to say so.

## Global Constraints

- No change to the bus wire format or `Event` enum. No new bus signals.
- Built-in `DisplayMode` arms, `arcade_layout.rs`, `[arcade]` config and `--mode` (with its aliases) stay as they are.
- `DisplayMode` is `Copy`; the plugin variant is `DisplayMode::Plugin(VizHandle)` where `VizHandle(u32)` is `Copy`, handed out once per id per process and never reused.
- Plugin ids are kebab-case and unique. A Rust plugin's id equals its folder name with `_` turned into `-`.
- Examples are disabled by default. They show only when enabled by `[viz] enable = ["example-pulse", "thermal-board"]` or `--viz <id>`.
- Plugins are full-screen views only. No direct keys, no arcade panels.
- Every source file starts with the repo's SPDX header and carries heavy explanatory comments (the user's standing rule). Keep comments true to the code.
- TUI text: no right-side border characters.
- Tests run as CI does: `env -u COLORTERM -u TMUX TERM=dumb cargo test --lib --features tui` and the same with `--no-default-features --features tui,json-backend`. Also `cargo fmt --all --check`, `cargo clippy -D warnings` for `--features tui`, `--no-default-features --features tui,json-backend`, `-p tt-toplike-bus`, `-p tt-toplike-saga`, and `cargo check --locked --lib --features tui` for `aarch64-apple-darwin` and `x86_64-pc-windows-gnu`.
- Version bump: root crate 0.18.0 to 0.19.0 (Cargo.toml, Cargo.lock, debian/changelog, CHANGELOG.md, install.sh and docs that pin it, as 0.18.0 did).
- Commit messages end with the attribution lines from the session's system reminder.

## Review Focus

Failure modes the spec implies that no task would test unless pinned. Each has a test in the task named in brackets.

1. A malformed, oversize or unknown-widget YAML file must produce a "failed to load" entry and one log line, never a crash. [Task 3]
2. A plugin whose `render` panics must not take the TUI down; the host catches it, shows "viz crashed: <id>", and leaves the view. [Task 7]
3. A terminal smaller than the viz's `min_size` shows a "needs W x H" message, not a broken render. [Task 7]
4. An empty world (no chips, no readings) renders a placeholder in both examples. [Tasks 8, 9]
5. A YAML file deleted while its view is on screen drops the screen to the first built-in view. [Task 5]

---

### Task 1: Core module, trait, registry, history, golden helper

**Files:**
- Create: `src/viz/mod.rs`, `src/viz/ctx.rs`, `src/viz/history.rs`, `src/viz/registry.rs`, `src/viz/testkit.rs`
- Modify: `src/lib.rs` (add `#[cfg(feature = "tui")] pub mod viz;`)
- Test: `src/viz/registry_tests.rs`, `src/viz/history_tests.rs`

**Interfaces:**
- Produces (exact names later tasks rely on):
  - `pub trait Viz: Send` with `fn id(&self) -> &str`, `fn title(&self) -> &str`, `fn min_size(&self) -> (u16, u16)` (default `(40, 10)`), `fn signals(&self) -> Vec<SignalSpec>` (default empty), `fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &VizCtx)`, `fn legend(&self) -> Vec<String>`, `fn explain(&self) -> String`, `fn help(&self) -> String`, `fn configure(&mut self, cfg: &toml_edit::Table)` (default no-op). (`on_key` was planned here and removed 2026-11-04: the host never called it.)
  - `pub struct SignalSpec { pub kind: EntityKind, pub name: String }`
  - `pub struct VizCtx<'a> { pub world: &'a dyn WorldRead, pub history: &'a History, pub tick: u64, pub now_mono_ns: u64 }` plus `VizCtx::chips(&self) -> Vec<EntityKey>` (via `world.entities(EntityKind::Chip)`).
  - `pub struct History` with `History::new(cap: usize)`, `record(&mut self, key: &EntityKey, signal: &str, v: f64)`, `series(&self, key: &EntityKey, signal: &str) -> &[f64]` (oldest first; empty slice when unknown), default cap 240 per series.
  - `pub struct VizHandle(pub u32)` (`Copy, Eq, Hash`), `pub enum Source { Folder, File(PathBuf) }`.
  - `pub struct VizEntry { pub handle: VizHandle, pub id: String, pub title: String, pub source: Source, pub enabled_by_default: bool, pub status: EntryStatus }` with `EntryStatus::{Ok, Failed(String)}`.
  - Registry functions (process-wide, `RwLock` inside a `OnceLock`): `registry::register(id, title, source, enabled_by_default, make: Box<dyn Fn() -> Box<dyn Viz> + Send + Sync>) -> Result<VizHandle, RegisterError>`, `registry::entries() -> Vec<VizEntry>` (sorted by id), `registry::handle_of(id: &str) -> Option<VizHandle>`, `registry::with_viz<R>(h: VizHandle, f: impl FnOnce(&mut dyn Viz) -> R) -> Option<R>`, `registry::replace(h, make)` and `registry::mark_failed(h, msg)` and `registry::remove(h)` (tombstone: entry vanishes from `entries()`, handle never reused), `registry::is_valid_id(&str) -> bool` (`[a-z0-9]+(-[a-z0-9]+)*`).
  - `testkit::render_to_text(viz: &mut dyn Viz, w: u16, h: u16, world: &dyn WorldRead, history: &History) -> String` (one line per row, trailing spaces trimmed) and `testkit::assert_golden(path: &Path, actual: &str)` (reads the file, compares; when env `UPDATE_GOLDEN=1` rewrites it; a missing file fails with a message naming the env var).
- Consumes: `tt_toplike_bus::{WorldRead, EntityKey, EntityKind, Value}`.

- [ ] **Step 1: Write failing tests** in `registry_tests.rs`: `duplicate_id_is_rejected` (second `register` of the same id returns `Err(RegisterError::Duplicate)`), `bad_id_is_rejected` (`"Foo"`, `"a_b"`, `""`, `"-a"` all rejected; `"a-b2"` accepted), `removed_handle_is_not_reused` (register, remove, register another id: handles differ and the removed id is absent from `entries()`), `entries_are_sorted_by_id`. In `history_tests.rs`: `history_caps_each_series` (record 300 values with cap 240: length 240, first element is value 61) and `unknown_series_is_empty`. Tests use a private `Registry` constructor (`Registry::new()` behind the global) so they do not share global state.
- [ ] **Step 2: Run and confirm they fail** (`env -u COLORTERM -u TMUX TERM=dumb cargo test --lib --features tui viz::`): FAIL, module missing.
- [ ] **Step 3: Implement** the files above. `Registry` is a plain struct with the logic; the global wraps one in `OnceLock<RwLock<Registry>>`. Tombstones keep a `next_handle` counter. Comment every public item and say who calls it.
- [ ] **Step 4: Add `testkit` test** `golden_helper_round_trips`: write a temp golden with `UPDATE_GOLDEN=1` semantics via an explicit `update: bool` internal function, then check equality, then check a changed string fails.
- [ ] **Step 5: Run and confirm everything passes**, then `cargo clippy --features tui -- -D warnings`.
- [ ] **Step 6: Commit** `feat(viz): the plugin trait, registry, signal history and golden-frame helper (0.19.0)`.

---

### Task 2: build.rs discovery of Rust plugin folders

**Files:**
- Create: `build.rs`, `src/viz/plugins/mod.rs`, `src/viz/plugins/README.md`
- Modify: `Cargo.toml` (`build = "build.rs"`, make sure `cargo package` includes `src/viz/plugins/**`), `src/viz/mod.rs` (call `plugins::register_all()` from `viz::init()`)
- Test: `src/viz/plugins_tests.rs`, plus a throw-away fixture folder created and removed by the test run's setup script described in Step 4

**Interfaces:**
- Consumes: `registry::register` from Task 1.
- Produces: a rule every plugin folder follows, written into `src/viz/plugins/README.md`: the folder `src/viz/plugins/<snake_name>/mod.rs` exposes `pub const ENABLED_BY_DEFAULT: bool` and `pub fn make() -> Box<dyn Viz>`; the id returned by `make().id()` must equal `<snake_name>` with `_` replaced by `-`. `viz::init()` registers every folder once; it returns `Vec<RegisterError>` for the caller to log.

- [ ] **Step 1: Write failing tests**: `every_plugin_folder_id_matches_its_folder_name` (iterates `std::fs::read_dir("src/viz/plugins")`, for each folder checks the registered entry whose handle maps to it), `init_is_idempotent` (calling `viz::init()` twice registers nothing new the second time).
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement `build.rs`.** It lists folders under `src/viz/plugins/` that contain `mod.rs`, sorted, and writes `$OUT_DIR/viz_plugins.rs` containing, for each folder, `#[path = "<absolute path>/mod.rs"] pub mod <name>;` and a `pub fn register_all() -> Vec<RegisterError>` that registers each. It prints `cargo:rerun-if-changed=src/viz/plugins`. `src/viz/plugins/mod.rs` does `include!(concat!(env!("OUT_DIR"), "/viz_plugins.rs"));`.
- [ ] **Step 4: Prove the discovery** by creating a temporary folder `src/viz/plugins/zz_probe/` with a stub plugin, running the id test (it must pass with the probe registered), then deleting the folder and re-running (it must still pass). Record both runs in the commit message body.
- [ ] **Step 5: Run `cargo package --list --allow-dirty | grep viz/plugins`** and confirm the files ship; run `cargo check --locked --lib --features tui` for both cross targets. If `build.rs` breaks packaging or a cross target, switch the discovery to the `inventory` crate (each folder calls `inventory::submit!` once inside its own `mod.rs`) and say so in the commit.
- [ ] **Step 6: Commit** `feat(viz): plugin folders are found by build.rs, so adding one edits no shared file`.

---

### Task 3: Declarative schema, parser and validation

**Files:**
- Create: `src/viz/declarative/mod.rs`, `src/viz/declarative/schema.rs`, `src/viz/declarative/validate.rs`, `viz/schema/viz-v1.schema.json`, `src/viz/declarative/fixtures/` (small YAML and JSON fixtures)
- Modify: `Cargo.toml` (YAML crate dependency under the `tui` feature)
- Test: `src/viz/declarative/schema_tests.rs`

**Interfaces:**
- Produces: `pub struct VizDef` (serde-derived: `version: u32` must be 1, `id`, `title`, `min_size: Option<(u16, u16)>`, `legend: Vec<String>`, `explain: Option<String>`, `layout: Vec<RowDef>`), `RowDef { height: Height, each: Option<EachChip>, cells: Vec<CellDef> }`, `Height::{Rows(u16), Fill}`, `CellDef { weight: u16, widget: WidgetDef }`, `WidgetDef` tagged by `widget:` with the variants `gauge`, `sparkline`, `table`, `log`, `grid`, `heat_strip`, `text`. Each names a `signal` (and `kind` where it is not the chip), plus `label`, `max`, `thresholds: Vec<Threshold { at: f64, color: ColorName }>`. `pub fn parse(path: &Path, text: &str) -> Result<VizDef, LoadError>` (`.json` goes through `serde_json`, `.yaml`/`.yml` through the YAML crate, same structs) and `pub fn validate(def: &VizDef) -> Result<(), LoadError>`. `LoadError` has a one-line `Display` that names the file, and where known the line.
- Limits (constants in `validate.rs`): file size at most 256 KiB, at most 64 widgets, at most 16 rows, `weight` 1 to 100.
- The builder may rename fields. The fixtures, the Rust types and `viz-v1.schema.json` must stay in sync, and a test enforces that.

- [ ] **Step 1: Write failing tests**: `parses_the_reference_yaml` (a fixture `reference.yaml` with one gauge row, one sparkline row with `each: chip`, one table and a `legend:` list parses and `validate` passes), `same_def_from_json_matches_yaml` (`reference.json` and `reference.yaml` parse to equal `VizDef`s), `unknown_widget_is_an_error_naming_the_widget`, `bad_id_is_an_error`, `version_two_is_rejected_with_a_message_naming_version`, `oversize_file_is_rejected_before_parsing` (257 KiB of text), `too_many_widgets_is_rejected`, `schema_file_accepts_the_reference_and_rejects_the_unknown_widget_fixture` (validate against `viz-v1.schema.json` with a JSON Schema crate as a dev-dependency).
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement** types, `parse`, `validate` and the JSON Schema file. Use `#[serde(deny_unknown_fields)]` so a typo is an error, not silence.
- [ ] **Step 4: Run, then clippy and the both-feature-forms test run.** Confirm the YAML crate builds on both cross targets (`cargo check --locked --lib --features tui --target ...`).
- [ ] **Step 5: Commit** `feat(viz): the YAML/JSON viz definition, its schema and its limits`.

---

### Task 4: Declarative widgets and the `DeclarativeViz` renderer

**Files:**
- Create: `src/viz/declarative/widgets.rs`, `src/viz/declarative/viz.rs`, `src/viz/declarative/golden/`
- Test: `src/viz/declarative/widgets_tests.rs`

**Interfaces:**
- Consumes: `VizDef` (Task 3), `Viz`/`VizCtx`/`History` (Task 1), `testkit` (Task 1).
- Produces: `pub struct DeclarativeViz { def: VizDef }` implementing `Viz` (`id`, `title`, `min_size` come from the def, default `(40, 10)`; `signals()` is the union of the widgets' signals; `legend`, `explain` come from the def with a generated fallback that lists each widget's signal and thresholds). `pub fn new(def: VizDef) -> DeclarativeViz`. Rendering draws the layout: rows by `height`, cells by `weight`, `each: chip` repeats the row once per chip with `{chip}` in labels replaced by the chip's short name. A widget whose reading is missing draws `--`, never panics.

- [ ] **Step 1: Write failing tests** using a hand-built `WorldRead` from `tt_toplike_bus::testutil`: `gauge_fills_in_proportion_and_takes_threshold_colour` (reading 90 of max 110 with thresholds 85 yellow, 100 red gives yellow cells and a filled fraction within one cell of 90/110), `sparkline_uses_history_series`, `table_sorts_rows_by_the_named_signal_descending`, `missing_reading_draws_dashes`, `each_chip_repeats_the_row_per_chip` (3 chips give 3 rows), `empty_world_renders_a_placeholder_line` (no chips: the text "no chips" appears), `reference_fixture_golden_80x24` (golden in `golden/`).
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement** the seven widgets as small functions over `(&mut Buffer, Rect, &WidgetDef, &VizCtx)`. Colours come from `ColorName` mapped to the existing palette in `crate::ui::colors`. Respect the colour mode the rest of the TUI uses (do not hard-code RGB).
- [ ] **Step 4: Generate the golden with `UPDATE_GOLDEN=1`, read it, confirm it looks right, then run without the variable.**
- [ ] **Step 5: Commit** `feat(viz): the declarative widgets and the renderer that draws a viz definition`.

---

### Task 5: Loader, hot reload and failed-file entries

**Files:**
- Create: `src/viz/declarative/loader.rs`
- Modify: `src/viz/mod.rs` (`viz::poll_files()`, `viz::init()` also scans)
- Test: `src/viz/declarative/loader_tests.rs`

**Interfaces:**
- Consumes: `parse`/`validate` (Task 3), `DeclarativeViz` (Task 4), registry (Task 1).
- Produces: `pub struct Loader` with `Loader::new(dirs: Vec<PathBuf>)` (user dir `~/.config/tt-toplike/viz/` resolved with the existing config-dir helper, plus the repo `viz/` folder when running from a checkout), `Loader::scan(&mut self)` (first load), `Loader::poll(&mut self) -> Vec<Change>` (compares mtimes; call it at most every 2 seconds), `Change::{Added(VizHandle), Updated(VizHandle), Removed(VizHandle), Failed(VizHandle, String)}`. A file that fails keeps its last good version registered and shows `EntryStatus::Failed`. A file that fails on first load registers a failed entry so the user sees it in the `v` list. One log line per failure. The id in a file must be unique across both tiers; a clash is a failed entry.

- [ ] **Step 1: Write failing tests** in a temp dir: `new_file_is_registered_on_poll`, `edited_file_replaces_the_viz_and_keeps_the_handle`, `broken_edit_keeps_last_good_version_and_marks_failed`, `first_load_of_a_broken_file_is_a_failed_entry_not_a_crash` (also the oversize and unknown-widget fixtures from Task 3), `deleted_file_is_removed_and_its_handle_is_a_tombstone`, `id_clash_with_a_rust_plugin_is_a_failed_entry`.
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement** with `std::fs::metadata` mtimes, no thread and no watcher crate.
- [ ] **Step 4: Run and confirm pass; clippy.**
- [ ] **Step 5: Commit** `feat(viz): YAML viz files load at startup and reload on save`.

---

### Task 6: `[viz]` config and `--viz` flag

**Files:**
- Modify: `src/config.rs` (a `VizSettings { enable: Vec<String>, per_viz: BTreeMap<String, toml_edit::Table> }` field on the config struct, a `parse_viz` function next to `parse_saga`, and a line in the config summary), `src/cli.rs` (`#[arg(long, value_name = "ID")] pub viz: Option<String>` with help text), `src/main.rs` or wherever `cli.mode` is first resolved (resolve `--viz` against the registry)
- Test: tests in `src/config.rs`'s existing test module and `src/cli.rs`'s

**Interfaces:**
- Consumes: `registry::{handle_of, entries}` (Task 1).
- Produces: `config.viz.enable` is the list of ids shown in the `v` cycle on top of those with `enabled_by_default`; `[viz.<id>]` tables are handed to `Viz::configure` at first use. `--viz <id>` starts the TUI on that plugin and enables it for the run; an unknown id exits with a message that lists every registered id and its source. `--viz` and `--mode` together: `--viz` wins. An `enable` entry that names no known id gives a `ConfigProblem` (follow `parse_saga`'s reporting), not an error exit.

- [ ] **Step 1: Write failing tests**: `viz_enable_list_parses`, `viz_enable_unknown_id_is_a_config_problem_not_a_failure`, `viz_per_id_table_is_kept`, `cli_viz_flag_takes_a_string`, `unknown_viz_flag_lists_known_ids` (message contains `example-pulse` once Task 8 lands; until then assert it contains the word "available").
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement.** Existing config files with no `[viz]` section behave exactly as before; add a test that proves it with an existing sample config.
- [ ] **Step 4: Run both feature forms of the tests; clippy.**
- [ ] **Step 5: Commit** `feat(viz): the [viz] config section and the --viz flag`.

---

### Task 7: TUI integration

**Files:**
- Create: `src/ui/tui/viz_host.rs`, `src/ui/tui/viz_host_tests.rs`
- Modify: `src/ui/tui/mod.rs` (add `DisplayMode::Plugin(VizHandle)`; add to `ALL_DISPLAY_MODES` test list how the compiler demands; add the arms the compiler requires, each a one-line call into `viz_host`), `src/cli.rs` only through Task 6

**Interfaces:**
- Consumes: registry, `Loader`, `History` (Tasks 1, 5), config (Task 6).
- Produces in `viz_host.rs`: `pub struct VizHost` (owns the `Loader`, a `History`, the `last_poll` time) with `VizHost::new(config: &VizSettings)`, `tick(&mut self, now: Instant, world: &dyn WorldRead, active: Option<VizHandle>)` (polls files every 2 s; records history only for the active viz's `signals()` once per 4 ticks, so a viz not on screen costs nothing), `render(&mut self, h: VizHandle, f: &mut Frame, area: Rect, world, tick) -> RenderOutcome` where `RenderOutcome::{Drawn, TooSmall, Crashed(String), Gone}`, `legend_lines(h) -> Vec<String>`, `explain_text(h) -> String`, `help_text(h) -> String`, `cycle_after(current: DisplayMode) -> Option<VizHandle>` (the next enabled plugin in id order), `first_plugin() -> Option<VizHandle>`, `name_of(h) -> String`.
- `render` wraps the viz call in `std::panic::catch_unwind(AssertUnwindSafe(..))`, and on a panic marks the entry `Failed("panicked: ...")`, logs once, and returns `Crashed`. `TooSmall` draws a centred "needs W x H (have w x h)" line. `Gone` means the handle was removed.
- Rotation: `next_display_mode` keeps today's order for built-ins; from `Die3d` it goes to the first enabled plugin, then each next plugin, then `Insights` after the last. Plugins are not entry-key modes: `Esc` goes to `prev_mode` as for other full-screen views.
- When the frame loop sees `Gone` or `Crashed` for the active plugin it sets `display_mode` to `Insights` and shows the reason in the status bar for a few seconds, using the existing transient-message path.
- Slash commands: `/mode <id>` and `/viz <id>` switch to a plugin by id (a fallthrough in `execute_command` after the built-in names). Help overlay: a "Plugin views" section listing enabled plugin ids and their one-line titles, generated from the registry.

- [ ] **Step 1: Write failing tests** in `viz_host_tests.rs` with a stub `Viz`: `too_small_terminal_shows_needs_message_and_does_not_call_render` (stub counts render calls: 0), `panicking_render_is_caught_marks_entry_failed_and_reports_crashed`, `removed_handle_reports_gone`, `history_is_recorded_only_for_the_active_viz`, `cycle_goes_from_die3d_into_plugins_in_id_order_then_to_insights`, `cycle_is_unchanged_when_no_plugin_is_enabled` (compare against the current `next_display_mode` table), `slash_mode_by_id_switches_and_unknown_id_is_an_error_line`. In `mod.rs`'s tests: extend the existing `ROTATION`/`ALL_DISPLAY_MODES` assertions so that with no plugins enabled they still hold unchanged.
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement.** Keep every plugin-specific line in `viz_host.rs`; `mod.rs` additions are one-line delegations. Plugin legend/explain/help text feeds the existing overlay builders (`explain_lines` and the legend path) like a built-in's constant text.
- [ ] **Step 4: Run both feature forms of the lib tests, the `tui_panic_hook` and `splash_panic` tests, clippy for both forms, and run the TUI under `--mock` with a throw-away stub plugin to see it cycle (record what you saw).**
- [ ] **Step 5: Commit** `feat(viz): plugin views in the TUI, with a panic guard, a size guard and a v-cycle slot`.

---

### Task 8: Rust example plugin `example-pulse`

**Files:**
- Create: `src/viz/plugins/example_pulse/mod.rs`, `src/viz/plugins/example_pulse/README.md`, `src/viz/plugins/example_pulse/golden/80x24.txt`, `src/viz/plugins/example_pulse/golden/40x10.txt`, `src/viz/plugins/example_pulse/golden/empty-80x24.txt`
- Test: `src/viz/plugins/example_pulse/tests.rs` (declared from `mod.rs`)

**Interfaces:**
- Consumes: only public items from `crate::viz` (the proof that a plugin needs nothing private). Produces: `ENABLED_BY_DEFAULT = false` and `make()`.
- Behaviour: one row per chip: short chip name, a power sparkline from `History` coloured by the chip's temperature (green below 70, yellow below 85, red above), the latest power and temperature as numbers. Signals: `tt_asic_temperature_celsius` and `toplike.board_power_w` on `EntityKind::Chip` (check the exact power signal name on chips in `src/bus/signals.rs` and `src/bus/source.rs` and use what the sources emit). Settings key `[viz.example-pulse] hot_c` (default 85). Legend and explain fragments. About 150 lines; a comment on each part of the trait so the file reads as the recipe.

- [ ] **Step 1: Write failing tests**: `registers_under_its_folder_id` (id `example-pulse`, disabled by default), `golden_80x24_with_three_chips`, `golden_40x10_small`, `empty_world_shows_a_placeholder` (golden `empty-80x24.txt` contains "no chips"), `hot_c_setting_moves_the_colour_threshold` (configure to 60, a 65 C chip draws red).
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement** the plugin.
- [ ] **Step 4: Generate goldens with `UPDATE_GOLDEN=1`, read them, then run normally.** Run the app with `--mock --viz example-pulse` and describe the screen in the report.
- [ ] **Step 5: Commit** `feat(viz): example-pulse, the Rust example plugin`.

---

### Task 9: YAML example plugin `thermal-board`

**Files:**
- Create: `viz/examples/thermal-board.yaml`, `viz/examples/golden/thermal-board-80x24.txt`, `viz/examples/golden/thermal-board-empty-80x24.txt`
- Test: `src/viz/declarative/example_tests.rs`

**Interfaces:**
- Consumes: the schema (Task 3), renderer (Task 4), loader (Task 5). Behaviour: a temperature gauge and a power gauge per chip (`each: chip`), a table of chips sorted by temperature, and a border threshold that turns red above the hot temperature. Uses only signals confirmed on the bus (chip temperature and power). Disabled by default: lives in `viz/examples/`, which the loader registers as `enabled_by_default = false`.

- [ ] **Step 1: Write failing tests**: `thermal_board_loads_and_validates`, `thermal_board_golden_80x24`, `thermal_board_empty_world_placeholder`, `thermal_board_is_hidden_until_enabled` (not in the enabled list with an empty `[viz]`; in it with `enable = ["thermal-board"]`).
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Write the YAML** with comments on every key, so it is the template `viz new --yaml` copies.
- [ ] **Step 4: Generate goldens, read them, run again. Run `--mock --viz thermal-board` and edit the file's `max:` while it runs to confirm hot reload; describe what you saw.**
- [ ] **Step 5: Commit** `feat(viz): thermal-board, the YAML example plugin`.

---

### Task 10: Scaffold command and the examples-stay-alive test

**Files:**
- Create: `src/viz/scaffold.rs`, `src/viz/all_examples_tests.rs`
- Modify: `src/cli.rs` (a `viz` subcommand group: `viz new <name> [--yaml]` and `viz list`; check how the CLI handles subcommands today and match it), `src/main.rs` dispatch

**Interfaces:**
- Produces: `scaffold::new_rust(name: &str, root: &Path) -> Result<PathBuf, ScaffoldError>` copies `src/viz/plugins/example_pulse/` to `src/viz/plugins/<snake_name>/`, replacing the id, title and golden-folder names (goldens are left out; the scaffold prints the `UPDATE_GOLDEN=1` command to create them). `scaffold::new_yaml(name, dir)` copies `thermal-board.yaml` to `viz/<name>.yaml` (or to the user dir when run outside a checkout). Refuses a name that is not kebab-case, refuses to overwrite. Both print the single next command to run. `viz list` prints id, title, source and status for every entry (failed ones show the reason).
- `all_examples_tests.rs`: loads every Tier 1 plugin and every YAML file under `viz/` through the real registry and renders each at 40x10, 80x24 and 120x40, against an empty world and the fixture world; no panics, unique kebab-case ids.

- [ ] **Step 1: Write failing tests**: `new_rust_creates_a_folder_that_registers_after_build` (scaffold into a temp copy of the tree is impractical, so assert on file contents: id replaced, no `example-pulse` left, README present), `new_rust_rejects_non_kebab_names_and_existing_folders`, `new_yaml_copies_and_renames`, `viz_list_shows_a_failed_entry_with_its_reason`, `every_example_renders_at_three_sizes_without_panicking`, `ids_are_unique_and_kebab_case_across_both_tiers`.
- [ ] **Step 2: Run and confirm failure.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Prove the scaffold end to end once by hand** in a scratch worktree: `tt-toplike viz new demo-probe`, build, `--viz demo-probe` under `--mock`; record the output; delete the scratch worktree.
- [ ] **Step 5: Reintroduce a bug to see the all-examples test fail** (make one example's render panic), watch it go red, restore.
- [ ] **Step 6: Commit** `feat(viz): viz new / viz list, and a test that fails when an example breaks`.

---

### Task 11: Docs, agent recipe, version bump, full CI pass

**Files:**
- Create: `docs/viz-plugins.md`
- Modify: `AGENTS.md` (a section "Add a viz": pick a tier, the scaffold command, the files to write, how to run and test, what a PR may touch, and the rule that a viz PR changes only its own folder or file plus nothing shared), `README.md` and `QUICK_START.md` (one short paragraph each), `CHANGELOG.md`, `debian/changelog` (dated after the 0.18.0 entry, kept in descending order), `Cargo.toml`, `Cargo.lock`, `install.sh` and any doc that pins the version, as 0.18.0 did
- Test: a test `agents_md_recipe_names_real_commands` that greps `AGENTS.md` for `viz new`, `--viz`, and `UPDATE_GOLDEN=1`, and fails if `src/cli.rs` no longer defines them.

**Interfaces:** `docs/viz-plugins.md` holds the reference: the trait, the context, the history rules, signal names and where they are defined, the YAML schema with every widget and field, config keys, `--viz`, golden-frame workflow, what a PR may touch, and the limits (full-screen only, no direct keys, no arcade panels, no new signals).

- [ ] **Step 1: Write the failing recipe test.** Run it; confirm it fails.
- [ ] **Step 2: Write the docs** in plain English (no "X, not Y" framing, no aphoristic closers; run the sweep from the user's CLAUDE.md over the new text).
- [ ] **Step 3: Bump the version everywhere 0.18.0 was pinned** (grep for `0.18.0` first and list the hits in the commit body). Make sure there is no "Die 3D" in user-facing text (an existing test guards it).
- [ ] **Step 4: Run the full CI set from Global Constraints**, plus the saga and bus crate tests and `tools/party_traffic.py --mock 32` to confirm traffic is unchanged. Install the release binary to `~/.local/bin/tt-toplike-busted` after backing up the old one as `.0.18.0.bak`.
- [ ] **Step 5: Commit** `docs(viz): the plugin reference, the agent recipe, and 0.19.0`.
- [ ] **Step 6: Run the agent test from the spec's Success section.** In a scratch worktree, give a fresh agent only the repo and the sentence "add a viz that shows the fan speed of each chip". Check that its diff is one folder or one file and touches nothing shared. Record the result in the PR body, and delete the scratch worktree.
