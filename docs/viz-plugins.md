# Viz plugins

A viz is a full-screen view of tt-toplike that you add without touching the rest of the program.
There are two ways to write one:

* **A Rust folder.** Copy a folder, edit it, rebuild. You can draw anything a Rust function can
  draw, and you keep state between frames.
* **A YAML (or JSON) file.** Write one file, save it, and the running program loads it within a
  few seconds. No rebuild. You choose from seven ready-made widgets.

Both kinds appear in the same place: the `v` key cycle, `/viz <id>`, `--viz <id>`, and the
`tt-toplike viz list` table. This page is the reference. It is written for a person or a coding
agent who has not seen the repository before. Every statement here is checked against the code.

Contents

1. Pick a tier
2. Make one: `viz new` and `viz list`
3. What a plugin sees: signals and history
4. The Rust tier
5. The YAML tier
6. Using a viz: `--viz`, `[viz]`, and the `v` cycle
7. Testing and the golden frames
8. What a pull request may touch
9. Limits
10. Ask your agent

## 1. Pick a tier

| You want | Use |
| --- | --- |
| Gauges, bars, tables, a log or a heat strip of bus signals, with colours that change at limits | YAML |
| A look that changes as you edit, with no rebuild | YAML |
| Your own drawing, animation or state between frames | Rust |
| A setting the user can put in `config.toml` | Rust |

When both would do, start with YAML. It is shorter, and a mistake in it cannot crash the program.

The two examples in the repository are the best starting point. Read them before you write
anything:

* `src/viz/plugins/example_pulse/` is the Rust example (a power sparkline per chip, coloured by
  temperature).
* `viz/examples/thermal-board.yaml` is the YAML example (temperature and power bars per chip, and
  two tables). Every key in it has a comment.

Both are hidden by default: they are not in the `v` cycle until you ask for them (section 6).
`example-pulse` is compiled into the binary. `thermal-board` is found when tt-toplike runs inside a
checkout of the repository (the current folder, or a folder above the executable). An installed
binary can make its own copy with `tt-toplike viz new <name> --yaml`.

## 2. Make one: `viz new` and `viz list`

```
tt-toplike viz new <name>            make a Rust plugin (run it in a checkout of the repository)
tt-toplike viz new <name> --yaml     make a YAML plugin (no checkout needed)
tt-toplike viz list                  list every viz with its source and status
```

`<name>` is kebab-case: lowercase letters, digits and single hyphens, starting with a letter, at
most 48 characters. The command refuses a name that a built-in view already answers to (`/mode
<name>` would not reach the plugin) and a name another viz already has.

* `viz new <name>` copies `src/viz/plugins/example_pulse/` to `src/viz/plugins/<snake_name>/`
  (hyphens become underscores) and renames what belongs to the example. It copies `mod.rs`,
  `tests.rs` and `README.md`. It leaves the golden frames behind. The copy starts switched on
  (`ENABLED_BY_DEFAULT = true`). Rebuild to see it.
* `viz new <name> --yaml` writes a copy of `viz/examples/thermal-board.yaml` with the new id and
  title. Inside a checkout the file goes to `viz/<name>.yaml`. Anywhere else it goes to the user
  folder `<config dir>/tt-toplike/viz/` (on Linux, `~/.config/tt-toplike/viz/`). The comments in
  the template are kept.
* `viz list` prints one line per viz: id, title, source (`folder` or the file path) and status.
  The status is `ok`, `hidden` (loaded, but off until asked for, as the examples are) or
  `failed: <reason>`. A YAML file that does not load shows up as `failed` with the error and the
  line, so this is the place to look when a file seems to be ignored.

Exit codes: 0 on success, 1 when the command refuses (a bad or taken name, a file that already
exists) or fails, 2 for a usage error.

These commands run before the terminal, the backend or any hardware is touched.

**In a checkout, use `cargo run`.** The `tt-toplike` command above is the installed binary. In a
checkout that has no installed binary, run the same words as
`cargo run --features tui --bin tt-toplike-tui -- <words>`, for example
`cargo run --features tui --bin tt-toplike-tui -- viz new my-view --yaml`. A Rust plugin must be
rebuilt before it shows up; `cargo run` rebuilds. A YAML file needs no rebuild.

## 3. What a plugin sees: signals and history

A viz reads the signal bus. The bus holds readings: one latest value per *signal* per *entity*.
An entity is a chip, a board, a model, a process and so on; `EntityKind` in
`crates/tt-toplike-bus/src/entity.rs` lists them (`chip`, `board`, `tile`, `model`, `process`,
`lease`, `host`, `dram_channel`, `eth_port`, `training_run`). A signal is a name such as
`tt_asic_temperature_celsius`.

**Signal names are defined in `src/bus/signals.rs`** for everything the backend and the workload
sources write (chip telemetry, DRAM channels, models, training runs, processes). Open that file and
copy the name from it; do not guess one. A name that nothing writes gives a viz that shows `--`
forever. A viz cannot add a signal (section 9).

The two signals the examples use, both on `chip` entities, sent every poll:

| Signal | Meaning |
| --- | --- |
| `tt_asic_temperature_celsius` | ASIC temperature in degrees Celsius. |
| `tt_tdp_w` | The power the chip is drawing now, in watts. The name comes from the telemetry source. |

Other signals you might want: `tt_fan_speed_rpm` (fan speed, sent only when it changes),
`tt_ai_clock_mhz`, `toplike.voltage_v`, `toplike.current_a`. A reading is missing until the first
one arrives, and some are missing for good on some hardware. Always draw `--` for a missing
reading.

**History.** A plugin that wants a series (a sparkline) gets it from `ctx.history`. The host fills
it, and only under these rules:

* Only the signals the viz lists in `signals()` are recorded. Declare every signal you want a
  series for.
* Only while the viz is on screen. The first frame after you open it has no history.
* One sample on every fourth data tick, per entity of the declared kind, and only numeric
  readings that are finite (text and NaN are skipped).
* At most 240 samples per series (`DEFAULT_CAP` in `src/viz/history.rs`); the oldest drop first.
* `ctx.history.series(&key, "signal")` returns the samples oldest first, or an empty slice.

A history is a small convenience. For the current value, read the world instead
(`ctx.world.reading(&key, "signal")`).

## 4. The Rust tier

### The folder

A plugin is a folder `src/viz/plugins/<snake_name>/`. Its `mod.rs` must have two items:

```rust
/// Whether the view is in the view cycle before the user chooses.
pub const ENABLED_BY_DEFAULT: bool = true;

/// Build one instance of the plugin.
pub fn make() -> Box<dyn crate::viz::Viz> { Box::new(MyViz::new()) }
```

* The folder name is snake_case: lowercase letters, digits and `_`, starting with a letter. The
  build stops with a message when it is not.
* The id returned by `make().id()` is the folder name with each `_` replaced by `-`. A test checks
  this for every folder.
* `build.rs` finds the folder and registers it when `viz::init()` runs. No shared file lists
  plugins, so adding a folder edits nothing else. The longer rules are in
  `src/viz/plugins/README.md`.
* Put everything else (more files, helpers, golden frames) in the same folder.

### The trait

`Viz` is in `src/viz/ctx.rs`. The host owns one boxed instance per plugin and calls it only from
the draw thread. The trait needs `Send` but not `Sync`.

| Method | Contract |
| --- | --- |
| `id(&self) -> &str` | Required. The stable kebab-case id. `--viz <id>` and `[viz] enable` use it. |
| `title(&self) -> &str` | Required. The name in the status bar. |
| `min_size(&self) -> (u16, u16)` | Optional, default `(40, 10)`. Width and height. When the view area is smaller, the host draws "needs W x H (have w x h)" and does not call `render`. |
| `signals(&self) -> Vec<SignalSpec>` | Optional, default none. Each `SignalSpec` is an entity `kind` and a signal `name`. The host records history for these (section 3). |
| `render(&mut self, buf, area, ctx)` | Required. Draw into `area` of `buf`. Called every frame. Never write outside `area`: it can be smaller than the buffer or empty. |
| `legend(&self) -> Vec<String>` | Required. Lines for the legend panel (`l`). |
| `explain(&self) -> String` | Required. A plain-language account of the view for the explain panel (`!`). |
| `help(&self) -> String` | Required. Help text for the help panel (`?`). |
| `configure(&mut self, cfg: &toml_edit::Table)` | Optional. Receives the `[viz.<id>]` table (section 6). Called once after construction and again when the config reloads. A plugin with no such table is not called. Ignore values of the wrong type and keep the old setting. |

`VizCtx` is what `render` receives:

| Field | Meaning |
| --- | --- |
| `world: &dyn WorldRead` | Read access to the bus: `reading(&key, "signal")`, `entities(kind)`, `events_since(0)` and more (`crates/tt-toplike-bus/src/read.rs`). |
| `history: &History` | The series the host recorded (section 3). |
| `tick: u64` | The host's data-tick counter. |
| `now_mono_ns: u64` | The bus clock in nanoseconds. Every reading's `t_mono_ns` is on this scale. |

`ctx.chips()` is a shortcut for `ctx.world.entities(EntityKind::Chip)`.

### Rules every plugin follows

* **A panic is caught and turns the plugin off.** The host runs `min_size`, `render`, `signals`,
  `legend`, `explain`, `help` and `configure` (and the plugin's `Drop` after a failure) inside a
  panic guard. A panic marks the entry failed with a message such as "panicked in render: ...",
  drops the viz, logs one line and returns the user to the Insights view with the message. The
  rest of the program keeps running. The guard exists for bugs. Write the plugin so it does not
  panic: check for an empty world, a missing reading and a zero-size area.
* **Do not call registry functions on your own handle from inside a method.** Each viz runs under
  its own lock (`registry::with_viz`). Calling `with_viz` for the same handle from `render` (or any
  other method) waits for a lock that the call itself holds, and the program hangs. Calling the
  registry about other plugins is allowed, but a viz has no need to.
* **Never open real hardware in a viz or its test.** Read the bus through `ctx.world`. Tests use a
  fake world (section 7).
* **Draw only inside `area`.** Cut text to the width you have.
* **Draw `--` for a missing reading** and a short note (such as `no chips`) for an empty world.
* **Keep `render` cheap.** It runs every frame, on the draw thread.
* **No right-side border characters.** Use left and bottom bars only. A right-hand border breaks
  when the terminal is narrower than expected.

### Test helpers

`src/viz/testkit.rs` has what a plugin test needs:

* `WorldBuilder::new().sample("0000:01:00.0", "tt_tdp_w", 90.0).event(..).build()` makes a world.
* `render_to_text(&mut viz, width, height, &view, &history)` draws the viz and returns the screen
  as text.
* `assert_golden(path, text)` compares with a golden file (section 7).

`src/viz/plugins/example_pulse/tests.rs` shows all three in use.

## 5. The YAML tier

### Where the file goes

* `viz/<name>.yaml` in a checkout of the repository. A file here starts switched on.
* `viz/examples/<name>.yaml`. A file here starts switched off (these are the shipped samples).
* `<config dir>/tt-toplike/viz/<name>.yaml` (on Linux `~/.config/tt-toplike/viz/`). Starts
  switched on. This is where `viz new <name> --yaml` writes outside a checkout.

The loader reads each folder without descending into subfolders, except that it also reads a
folder named `examples` one level down. File types are `.yaml`, `.yml` and `.json`; JSON has the
same keys and rules. A file larger than 256 KiB is refused. YAML tags (`!name`) are refused.

An id belongs to the first file, in path order, that loads with it. A Rust plugin with the same id
always wins. A file that loses shows as `failed` in `viz list` and loads once the clash is gone.

A JSON Schema for editors is at `viz/schema/viz-v1.schema.json`. It checks the shape; the limits
below are checked when the file loads.

### The top level

| Key | Required | Meaning |
| --- | --- | --- |
| `version` | yes | Must be `1`. |
| `id` | yes | Kebab-case, starting with a letter, at most 48 characters. Must be unique across both tiers. |
| `title` | yes | The name in the status bar. Not empty. |
| `min_size` | no | `[columns, rows]`, each at least 1. Default `[40, 10]`. A smaller terminal shows "needs W x H". |
| `legend` | no | A list of lines for the legend panel. Without it, one line per widget is made from the widgets. |
| `explain` | no | A paragraph for the explain panel. Without it, one is made from the widgets. |
| `layout` | yes | A list of rows, top to bottom. At least 1, at most 16. |

An unknown key anywhere is an error that names the key. A typo does not turn into a default.

### Rows

| Key | Required | Meaning |
| --- | --- | --- |
| `cells` | yes | A list of cells, left to right. At least 1. |
| `height` | no | A number of terminal lines (1 or more) or the word `fill`. Default `fill`. |
| `each` | no | `chip` is the only value. The row is drawn once per chip, and its widgets read that chip. |

Heights: fixed rows take their lines first (cut to what is left). Rows marked `fill` share the
rest, and the first ones get the odd line. A row left with no line is not drawn.

A file holds at most 64 widgets (cells over all rows). The renderer also draws at most **64 widget
instances per screen**, counting each chip of an `each: chip` row (a six-cell row on 16 chips would
be 96). When rows do not fit, because of that cap or because the terminal is too short, the last
visible line says `+N more`, where N counts the rows not drawn. With no chips at all, a view that
reads chips draws `no chips - waiting for the first reading`.

### Cells

| Key | Required | Meaning |
| --- | --- | --- |
| `widget` | yes | `gauge`, `sparkline`, `table`, `log`, `grid`, `heat_strip` or `text`. |
| `weight` | no | The cell's share of the row width, 1 to 100. Default 1. |
| `signal` | yes | A signal name (not empty). For `log` it is an event filter (below). |
| `kind` | no | The entity kind the signal belongs to. Default `chip`. One of `chip`, `board`, `tile`, `model`, `process`, `lease`, `host`, `dram_channel`, `eth_port`, `training_run`. |
| `label` | no | A caption. In it, `{chip}` becomes the short name of the row's chip (`01:00.0` for `0000:01:00.0`), or `all` outside an `each` row. |
| `max` | no | The value that fills the widget. A number above 0. |
| `thresholds` | no | A list of `{at: <number>, color: <name>}`. |

**Cells have no gap between them.** The right end of one cell sits against the left end of the
next. A gauge's value at its right end therefore touches the next cell's bar. To get a gap, put
the widgets in separate rows (for example two rows per chip, one gauge each).

Every widget takes the same fields. Each widget uses the fields that mean something to it.

### Which entity a widget reads

* In a row with `each: chip`, the widget reads that row's chip. A widget in such a row whose `kind`
  is not `chip` has no defined entity, and draws the note `each: needs chip`.
* Outside `each`, `table` and `grid` show every entity of the widget's `kind`. `gauge`, `text`,
  `sparkline` and `heat_strip` read the first entity of that kind, in key order. Use `each: chip`
  to get one per chip.

### Colours and thresholds

`thresholds` recolour a widget once the value reaches `at`. **The highest threshold reached wins,**
whatever order the file lists them in. Below every threshold the widget has its default colour.

Colour names: `primary`, `secondary`, `success`, `error`, `warning`, `info`, `text`, `muted`,
`border` (these follow the theme and the terminal's colour mode) and the plain colours `red`,
`yellow`, `green`, `cyan`, `blue`, `magenta`, `white`, `gray`. An unknown name fails to load, and
the message names it.

Default colours: a gauge and a sparkline use `primary`. A `grid` and a `heat_strip` depend on `max`:

* With `max`, they use a heat ramp by the value's share of `max`: `success` below half, `warning`
  below four fifths, `error` above that.
* Without `max`, there is no fixed scale to measure against (a series' own peak would always look
  full), so they use one neutral colour, `primary`, and only thresholds recolour them.

### The seven widgets

* **`gauge`**: a bar that fills toward `max`, then the value: `label ████░░░░ 90`. In a row two or
  more lines high the label has its own line and the bar sits below it. In a one-line row the label
  and bar share the line, and the label gets at most a third of the width. The bar and the value
  take the threshold colour. Without `max`, the bar scales to the highest value in the history of
  that signal on that entity. If there is no history, or its highest value is 0 or below, it scales to 100. That scale moves as readings change, so
  set `max` for a stable bar.
* **`sparkline`**: the recent history of the signal as block heights on one line, then the latest
  value. Without `max` the scale is the highest value shown. It needs history (section 3), so the
  first moments after opening the view show only the latest value.
* **`heat_strip`**: the same line, drawn as full blocks, each coloured by its own value.
* **`text`**: `label: value`, the value in its threshold colour. With no `label`, the caption is
  the signal name.
* **`table`**: one line per entity, `name value`, **largest first**. Entities with no reading come
  last as `--`. **A table shows one signal.** For two signals, put two tables side by side. With a
  `label` and more than one line of room, the label is a header line. When the lines do not fit,
  the last one says `+N more`.
* **`grid`**: one two-column cell `██` per entity, coloured by value (or `--` for no reading),
  running left to right and wrapping to the next line. The label starts the first line.
* **`log`**: the newest events, oldest first, one per line. **Its `signal` is an event-name
  filter and names no signal:** any text matches an event's name as a substring, ignoring ASCII
  case, and `*` shows every event. The names are `ModelLoading`, `ModelReady`, `ModelGone`,
  `RequestsCompleted`, `KernelCompiled`, `Reset`, `LinkChanged`, `LeaseGranted`, `LeaseReleased`,
  `EccTick`, `ViewChanged`, `Spark` and `LinkPeer`. `kind`, `max` and `thresholds` have no effect
  on it. With no match it says `no events`.

A reading that does not exist draws `--`. A number that is not finite counts as missing. Whole
numbers print without decimals and others with one.

### Hot reload

While the program runs, it checks the viz folders about every 2 seconds and acts only on files
whose modified time or size changed. Save a file and the view changes within a few seconds.

* An edit that loads replaces the view under the same handle, and the `[viz.<id>]` config table is
  handed to it again.
* **An edit that does not load keeps the last good version drawing.** The entry is marked failed
  with the reason (file, line and message), and `tt-toplike viz list` and the log show it. Fix the
  file and the next check clears the failure.
* A file that has never loaded appears as a failed entry named after the file. It is not in the
  `v` cycle. `--viz <that id>` starts, then returns to Insights with the message
  "viz <id> is unavailable: <reason>".
* A deleted file removes its view. If it was on screen, the program returns to Insights and says
  so.
* A change that keeps the file size and lands within one timestamp tick of the previous save can
  be missed until the file changes again (a filesystem with coarse timestamps).

## 6. Using a viz: `--viz`, `[viz]`, and the `v` cycle

**`--viz <id>`** starts the TUI on that viz and switches it on for the run, even when it is hidden
by default and not in `[viz] enable`. An unknown id exits with a message that lists every
registered id and where it came from. With `--mode` as well, `--viz` wins. Use it with the mock
backend to try a viz with no hardware. (`thermal-board` needs a checkout; see section 1.) Modes
that do not start the TUI, such as `--bench`, `--print` and `--headless`, ignore `--viz`:

```
tt-toplike --mock 4 --viz example-pulse
cargo run --features tui --bin tt-toplike-tui -- --mock 4 --viz thermal-board   # in a checkout
```

**`[viz]` in `config.toml`** (`~/.config/tt-toplike/config.toml` on Linux):

```toml
[viz]
enable = ["example-pulse", "thermal-board"]   # views to switch on, besides those on by default (thermal-board: checkout only)

[viz.example-pulse]
hot_c = 85          # one table per viz; its keys are that plugin's own settings
```

* `enable` lists ids to add to the cycle. A later config file replaces the list. Entries must be
  kebab-case strings; an id no viz has shows a config problem in the status line.
* `[viz.<id>]` is passed whole to that viz's `configure` (a Rust plugin). Tables from several config
  files are merged key by key. A YAML viz has no settings.

**The `v` cycle.** The built-in views keep their order. After the Wafer Waffler (`3`), `v` goes to
each enabled plugin in id order, then to Insights. A viz is in the cycle when it is on by default
or listed in `enable` (or named by `--viz`), and it has a working instance. A failed entry that
kept no viz is not in the cycle.

**Commands.** `/mode <id>` and `/viz <id>` switch to an enabled plugin. Built-in names for `/mode`
come first and win. In a plugin view, `l` opens the legend, `!` the explanation and `?` the help
panel for that viz. The help panel of another view lists the enabled plugin views. `Esc` leaves. The status bar shows "Esc leave <title>".

## 7. Testing and the golden frames

Run the viz tests:

```
env -u COLORTERM -u TMUX TERM=dumb cargo test --lib --features tui viz::
```

A Rust plugin's own tests live in its folder. A good set has: the id and `ENABLED_BY_DEFAULT`, a
golden frame at two sizes, an empty world, a world with a missing reading, a zero-size area and
a small one, and the colour rules.

**Golden frames.** `assert_golden(path, text)` compares a rendered screen with a text file in the
plugin's `golden/` folder. When the output should change, regenerate the file and read it before
you commit:

```
UPDATE_GOLDEN=1 cargo test --lib --features tui viz::plugins::<snake_name>
```

A missing golden file fails and names `UPDATE_GOLDEN=1`. Read the new text before you accept it.

Two shared tests cover every viz found on disk, with no change from you:

* `all_examples_tests` loads every Rust folder and every YAML file under `viz/`, draws each at
  three sizes on an empty and a populated world, and fails on a panic or a file that does not
  load. A YAML viz is tested by this alone, so it needs no test file of its own.
* A test checks that every id is kebab-case and unique across both tiers.

Never open real hardware in a viz test. Use `WorldBuilder`.

## 8. What a pull request may touch

A viz pull request changes **only its own folder `src/viz/plugins/<name>/`** (Rust) **or its own
file `viz/<name>.yaml`** (YAML). It edits no shared file. That rules out `Cargo.toml`, `build.rs`,
`src/cli.rs`, `src/ui`, `src/bus`, another viz's files and the docs. The platform is
built so that no shared file needs a line for a new viz. If your viz seems to need a change to
shared code, stop and ask for that change as its own pull request.

Before you open the pull request, run what CI runs: `cargo fmt --all --check`,
`cargo clippy --features tui -- -D warnings`,
`cargo clippy --no-default-features --features tui,json-backend -- -D warnings`, and the viz tests
from section 7.

## 9. Limits

* **Full screen only.** A viz draws the whole view area (the terminal minus the status bar). It
  cannot draw a panel inside another view.
* **No direct keys.** The trait has no key method, so a viz cannot take a key press. The keys are the host's: `v`, `l`, `!`, `?`, `Esc`.
* **No arcade panels.** A viz cannot add a panel to the arcade layout.
* **No new signals.** A viz reads signals that sources already write (`src/bus/signals.rs`). It
  cannot write to the bus or add a source. A signal that is not there needs a source change in its
  own pull request.
* **No custom drawing in YAML.** A YAML viz uses the seven widgets and nothing else. Use the Rust
  tier for anything else.
* **A history is short and sampled** (section 3): at most 240 samples, one in four data ticks, only
  for the viz on screen.
* **YAML limits:** 256 KiB per file, 16 rows, 64 widgets per file, 64 widget instances per screen,
  an id of at most 48 characters.
* **Rust plugins need a rebuild** and a checkout of the repository.

## 10. Ask your agent

A coding agent can write a viz from one sentence, because the repository tells it how. For
example:

> Add a viz to tt-toplike that shows the fan speed of each chip.

The agent should read `AGENTS.md` (the section "Add a viz") and this page, pick a tier (this one
fits YAML: a gauge per chip on `tt_fan_speed_rpm`), run `tt-toplike viz new <name> --yaml`, edit
the file it made, and try it with `cargo run --features tui --bin tt-toplike-tui -- --mock 4 --viz <name>`. A fan reads thousands of
RPM, so it should set `max` on the gauge (for example 6000) to give the bar a fixed scale. Its change should be one
file, or one folder, and nothing shared (section 8).
