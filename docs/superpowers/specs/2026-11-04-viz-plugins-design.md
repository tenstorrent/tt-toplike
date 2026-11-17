# Viz plugins: design

Date: 2026-11-04. Status: draft for approval. Repo: tt-toplike, branch `what-if`.

## Intent

tt-toplike should be a platform that people remix. Someone should be able to tell their coding agent "add a new viz to tt-toplike that does X" and have the agent add it in one place. A contributor should be able to send that viz as a pull request that touches only its own folder, next to the other plugins. Someone who only wants a viz on their own machine should be able to get one without sending anything upstream, and ideally without a rebuild.

Today a new view costs about 29 match-arm edits in `src/ui/tui/mod.rs` (14,773 lines), a second enum in `src/animation/arcade_layout.rs`, edits in `cli.rs`, `config.rs`, `lib.rs`, the legend, explain and help overlays, and the docs. Saga also added a driver thread and its own crate. There is no registry. That cost is what this design removes.

## Goals

1. One registry holds every plugin view. The `v` cycle, `--viz`, the help, legend and explain overlays and the config section read plugin entries from it. Built-in views are not moved onto it (see Non-goals).
2. **Tier 1, a compiled Rust viz.** One folder, `src/viz/<name>/`, implementing a `Viz` trait. A PR adds that folder and nothing outside it except docs. Adding a viz does not edit a shared file, so two such PRs cannot conflict.
3. **Tier 2, a declarative viz.** A YAML or JSON file describes a view. It loads at startup (and reloads on save) with no rebuild. It is a peer of Tier 1 in the registry.
4. Each viz supplies its own legend, explain and help fragments. This delivers the "modular per-arcade `l` legend / `e` explain" item from the backlog.
5. One working example of each tier ships with the platform. Each is also the template the scaffold command copies.
6. `tt-toplike viz new <name>` (and `--yaml`) scaffolds a viz. A golden-frame test helper checks a render. A recipe in `AGENTS.md` tells an agent where to go and what to run.
7. ~~At least one existing small view moves onto the registry.~~ Dropped on 2026-11-04: the user decided not to disrupt existing views (see Decisions). The two examples prove the pattern. The example Rust plugin may use only the public `Viz` trait and context, never anything private to `mod.rs`.

## Non-goals

- Moving the built-in views onto the registry. The user decided not to disrupt the existing code. Built-in views keep their `DisplayMode` arms in `mod.rs`. Moving them is a possible later project and is not planned here.
- Plugins as arcade panels. In this project a plugin is a full-screen view only, and its arcade-panel flag is always false. `arcade_layout.rs` and the `[arcade]` config do not change.
- Direct key bindings for plugins.
- Runtime-loaded native code (dynamic libraries) or WASM.
- An external-process plugin that draws over the bus socket. Left for later.
- Custom drawing in the YAML tier (sprites, particles, animation). Version 1 composes existing widgets only.
- New bus events. A viz that needs a new signal is a separate piece of work. (The idea of a viz showing LLM request contents was raised and set aside; it would also need an opt-in and redaction through `src/bus/redact.rs`.)
- The Defrag taller-blocks idea and per-chip Saga styling. Both stay in the backlog as separate projects.

## Anti-goals

- A PR that adds a viz and also edits `mod.rs`, `cli.rs`, or `config.rs`.
- An example that has gone stale. If an example stops working, CI fails.
- The registry becoming a second place that lists plugins by hand. Plugin ids must come from the viz folders and files.

## Decisions made with the user

- Tier 1 is compiled in. The agent drops a folder in and runs `cargo build`.
- Tier 2 is a plugin that reads JSON or YAML definitions.
- Both tiers get an example plugin that shows how to write one.
- The examples are hidden by default. They appear in the `v` cycle only when enabled with a config line or a command-line argument (for example `--viz example-pulse`).
- PRs must stay modular and side by side with other plugins.
- The registry covers plugins only. Existing views are left alone ("we are deep deep in, let's not disrupt").
- The command-line flag is `--viz <id>`, a free string checked against the registry. `--mode` and its aliases are unchanged. An unknown id lists the available ones.

## Design

### Registry

A `viz` module owns a registry of `VizEntry` records. Each entry carries: id (kebab-case, unique), title, source (`BuiltIn`, `Folder`, or `File`), the `v`-cycle position, an optional direct key, min size, arcade-panel eligibility, whether it is enabled by default, and a constructor. The TUI asks the registry for the list of enabled views instead of matching on `DisplayMode`. The existing views stay as `DisplayMode` variants with their current arms. The registry adds one `DisplayMode::Plugin(id)` form, and `mod.rs` gets one new path that handles it (cycle, render, legend, explain, help). In the `v` cycle, built-ins keep today's order and enabled plugins follow, sorted alphabetically by id. *My call.*

*My call:* `build.rs` scans `src/viz/*/` and generates the module list and the constructor table, so a new folder is picked up with no shared-file edit. If the builder finds that `build.rs` scanning is unworkable (for example it breaks `cargo publish`), the fallback is the `inventory` crate, where each folder registers itself with one macro call inside its own folder.

### The `Viz` trait (Tier 1)

Exact signatures are left to the builder. The trait covers:

- `id`, `title`, `min_size`.
- `render(frame, area, ctx)`, where `ctx` gives a read-only view of the signal bus (current entity values and history), the selected chip, the tick, and the theme/colour mode.
- `legend()`, `explain()`, `help()` returning text fragments the overlays compose.
- ~~`on_key(key)` for keys the viz owns. Optional.~~ Removed 2026-11-04: the host never called it, and the non-goals rule out direct keys.
- `settings_schema()` naming a `[viz.<id>]` config section and its keys. Optional.
- `signals()` listing the bus entities it reads, so the bus driver can skip work for a viz that is not showing.

A viz never touches the terminal, the bus socket, or another viz. It only reads the context it is handed.

### Declarative viz (Tier 2)

A built-in host plugin scans `~/.config/tt-toplike/viz/*.yaml|yml|json` and a repo `viz/` folder at startup, validates each file against a schema, and registers one registry entry per valid file. An invalid file produces one clear error line in the log and a visible "failed to load" note in the `v` list. It never crashes the app. Files reload when saved.

A file declares: id, title, optional key, optional min size, the signals it uses, a layout (rows and columns with relative sizes), and widgets. The first widget set is: gauge, sparkline, table, log, grid, heat strip, text. Each widget names a signal or signal pattern, plus colours and thresholds. Legend and explain text come from fields in the file, with a generated fallback.

The schema is published as a JSON Schema file in the repo, so an agent can validate its output before the app runs.

### Examples

- **`src/viz/example_pulse/` (Rust).** One row per chip. Each row has a power sparkline coloured by temperature. About 150 lines, with a comment on each part of the trait. It uses a bus subscription, legend and explain fragments, one settings key, and a golden-frame test. `viz new` copies it.
- **`viz/examples/thermal-board.yaml` (YAML).** A gauge per chip for temperature and power, a table of chips sorted by temperature, and a threshold that turns the border red. It uses only signals the builder confirms are on the bus today (per-chip temperature and power); no per-core data. `viz new --yaml` copies it.

Both are marked as examples and are disabled by default. They are enabled by the config line `[viz] enable = ["example-pulse", "thermal-board"]` or by `--viz <id>`.

### Scaffold, tests, docs

- `tt-toplike viz new <name> [--yaml]` writes the folder or file from the example, with the id and title filled in, and prints the one command to run next.
- A test helper renders a viz into a fixed-size buffer from a fixed signal fixture and compares it with a stored golden frame. Updating the golden is one flag.
- A registry test loads every folder and every example file through the real registry and renders each at three sizes. A broken example fails CI. A test also checks that ids are unique and kebab-case.
- `AGENTS.md` gets a "Add a viz" recipe: which tier to pick, the scaffold command, the files to write, how to run it, how to test it, and what a PR may touch. `docs/viz-plugins.md` holds the full reference, including the YAML schema.

### Migration proof

Dropped. No existing view moves. The Rust example uses only the public trait, which is the proof that a plugin needs nothing private.

## Constraints

- Keep every existing CI form green (fmt, clippy with `-D warnings` for all four feature forms, workspace tests, TERM=dumb lib tests, the panic-hook tests, and the macOS and Windows cross-target checks).
- The YAML tier needs a YAML parser dependency. *My call:* `serde_yaml` or a maintained fork, behind the existing default features. It must build on the macOS and Windows targets.
- Rendering must stay inside the existing frame budget. A viz that is not visible costs nothing.
- No new bus events and no change to the bus wire format.
- Existing config files and command-line modes keep working.

## Calls I made (the user has not decided these)

- Ids are unique and kebab-case. A duplicate id fails CI and fails at startup with a clear message.
- Each viz keeps its own README in its folder. A PR does not edit a shared docs file. The shared `docs/viz-plugins.md` and `AGENTS.md` recipe are written once by this project.
- Hot reload polls every two seconds. New, edited and deleted YAML files are picked up while the app runs. A file that fails validation keeps its last good version and shows a note. If the view on screen disappears, the screen falls back to the first built-in view.
- The `viz` module is gated on the `tui` feature. It need not compile without it.
- `build.rs` scanning is the first choice. The builder runs `cargo package` first. If scanning breaks packaging, use `inventory`.
- The `ctx` data comes from whatever the bus signal store already holds. If a viz needs more history than the store keeps, the builder adds a small capped history in the viz host, and does not change the bus. Tier 2 signal patterns follow the bus's existing entity naming (`chip:<bus_id>`, `tile:...`). The builder finds the metric names in the bus crate.

## Left to the builder

- Exact trait signatures and the `ctx` type.
- `build.rs` scanning versus `inventory` (with the fallback above).
- The YAML parser choice within the constraint above.
- How hot reload watches files (polling or a file-watcher crate).
- The exact widget field names in the schema, as long as one JSON Schema covers them.

## Success

An agent given only the repo and the sentence "add a viz that shows X from the bus signals" produces a single folder or a single YAML file, runs the scaffold and test commands from `AGENTS.md`, and gets a working view in the `v` cycle with no edits to shared files.
