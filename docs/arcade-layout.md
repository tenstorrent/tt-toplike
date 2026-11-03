# Arcade layout

The arcade (`a`) shows several views at once. Which views, in which rows, and how wide each one
is, is its layout. There are eight built-in layouts, and the `[arcade]` section of the config
file can add your own. `}` steps to the next layout and `{` to the previous one. With no config
file the arcade starts with `classic`, the layout it has always had.

## Where the config lives

The layout is set in `~/.config/tt-toplike/config.toml`, the only config file tt-toplike
reads. The other settings in it (`sensitivity`, `max_particles_scale`, `tt_smi_reset_behavior`)
work as before. No file in the working directory is read, so a repository you check out cannot
change tt-toplike.

The file must be a regular file of at most 256 KiB. A symlink to one works. A directory, FIFO,
socket or device is never opened, and a larger file is not read; each is reported as a problem,
and so is a file that cannot be read. tt-toplike reads the file once, before the terminal
switches to raw mode.

Check the file without starting the TUI:

```sh
tt-toplike --check-config
```

It says whether it found the file, lists every problem with its line, and lists every layout
in the order `}` steps through them, with each one's rows and panels; a `*` marks the layout the
arcade starts with. It exits with status 1 when there is a problem and 0 otherwise. It opens no
device. `tt-toplike --check-config --arcade-layout NAME` checks a start-up choice too.

## Switching layouts

| Key or command | What it does |
|---|---|
| `}` | the next layout; after the last comes the first |
| `{` | the previous layout; before the first comes the last |
| `/arcade layout next` | the same as `}` |
| `/arcade layout prev` | the same as `{` |
| `/arcade layout <name>` | the layout called `<name>` (letter case is ignored) |
| `/arcade layout list` | names every layout on the status line (`/arcade layout` alone does too) |

The keys work only while the arcade is on screen. After a switch the status line shows
`layout: <name> (<i>/<n>), <backdrop> — <description>`, for example
`layout: now-serving (2/8), clouds — inference over Wafer Waffler and defrag`. The backdrop part
is there only for a layout that sets one (see "A layout's backdrop"). The line is at most 78
characters, so it fits an 80-column terminal; a longer user description is cut with `…`.
`/arcade layout list` names every layout on as many lines as it needs. The message goes away
after about 4 seconds, and when you leave the arcade.

The order is: `config` when the file has `[[arcade.row]]` tables, then the file's
`[[arcade.layouts]]` tables in file order, then the built-in layouts in the order of the next
section. The arcade starts with the first one, unless the config says `[arcade] layout =
"<name>"` or the command line says `--arcade-layout <name>`; the command line wins. An unknown
name is a problem (reported like any other, see below) and the arcade starts with the first
layout.

The choice lasts until you quit. It holds across a resize, and when you leave the arcade and
come back. A switch lays the arcade out again at the same size, as a resize does. The hero keeps
its place and its trail, and the backdrop, the power baseline, the activity lanes,
the duel and what the arcade has read from the bus are kept, so events seen before the switch
make no new pulse after it. The panels start fresh: a new starfield, castle and so on, at their
new sizes. The new arcade is updated once before it is drawn, so its first frame shows the
devices and services the old one had.

## The built-in layouts

Each sketch shows the rows from the top, with the panels of a row side by side; `│` is the
divider between two panels. The title row and Row 1 are above every layout, and the status bar
is below. Every layout is tested at 0x0, at several small sizes, at 80x24, 60x20, 120x40 and
200x60, and at every width from 1 to 240 at six heights: no row runs past the terminal and no
row ends in a right-side border glyph. At 120x40 and 200x60 every layout shows every panel it
names.

Changed from 0.15.3: when one of these glyphs is in the terminal's last column, the arcade draws
a space there in the same colours:

* the verticals `│ ┃ ║`, their dashed forms `╎ ╏ ┆ ┇ ┊ ┋` and the half verticals `╵ ╷ ╹ ╻ ╽ ╿`;
* the top-right corners `┐ ┑ ┒ ┓ ╕ ╖ ╗ ╮` and the bottom-right corners `┘ ┙ ┚ ┛ ╛ ╜ ╝ ╯`;
* the right-edge joints `┤ ┥ ┦ ┧ ┨ ┩ ┪ ┫ ╡ ╢ ╣`;
* the blocks `▐ ▕` and the ASCII bar `|`.

No other cell changes. The list is `RIGHT_EDGE_GLYPHS` in `src/animation/arcade.rs`. In `classic`
this clears a panel divider at widths 1 and 2, the title row's separator at width 18, the defrag
view's field separators at widths such as 23, 24, 52 and 53, and the memory flow's field
separator `|` at widths 7 and 23. When the inference panel is the rightmost panel (`now-serving`,
`workload`, `deck`), it clears the right corners of the last stage card, which is then drawn
open on its right side, and the loading scene's scaffold beam where it stands in the last
column. The beam's `+` cells stay.

A panel that does not fit is taken out of its row and named on the row's rule (see "How the
space is shared"). The activity lanes need a starfield of 6 rows or more.

### `classic`

The arcade's own layout: the starfield over 40% of the rows, the castle (55% of the columns)
beside defrag, the flow below. It draws the same frame as the arcade before it had layouts, at
every size.

```text
║ ✧ starfield
║ · · · · · · · · · · · · · · · · · ·
║ · · · · · · · · · · · · · · · · · ·
╠ 🏰 castle          │ ▓ defrag
║ · · · · · · · · ·  │ · · · · · · ·
╠ 🌊 flow
║ · · · · · · · · · · · · · · · · · ·
╚═════════════════════════════════════
```

At 80x24: everything; the starfield is 80x7, so the lanes show. At 60x20: everything.

### `now-serving`

The user's words for it: "now serving: top: 3 wide inference server, big bottom: left: 3d cube,
right: defrag", with no starfield ("this is not the default") and the clouds behind the whole
arcade. The top row is the inference server alone, across the whole width. The bottom row is the
bigger one (heights 3 and 4), with the Wafer Waffler on the left and defrag on the right at equal
widths. Selecting this layout sets the arcade's backdrop to `clouds` (see "A layout's
backdrop").

```text
║ ≈ inference
║ · · · · · · · · · · · · · · · · · ·
║ · · · · · · · · · · · · · · · · · ·
╠ ◆ Wafer Waffler    │ ▓ defrag
║ · · · · · · · · ·  │ · · · · · · · ·
║ · · · · · · · · ·  │ · · · · · · · ·
║ · · · · · · · · ·  │ · · · · · · · ·
╚═════════════════════════════════════
```

At 80x24: everything (inference 80x8, Wafer Waffler 40x11, defrag 39x11). The inference panel
needs 24x8, so it stays at any terminal of at least 24 columns and about 23 rows. At 60x20 the
top row is 6 rows high: the inference panel is taken out and named on its rule (`◌ INFERENCE
needs 24x8`), and its rows go to the Wafer Waffler and defrag. Heights 1 and 2 would make the
bottom row twice as big, but the top row would then be 6 rows at 80x24 and the inference panel
would be taken out there too.

With nothing running, the inference panel shows its one-line note, ` no inference server`. The
clouds draw anyway: they follow the PCIe traffic and chip load of this TUI's chips, so they move
with or without an inference service (the scene, the old cloudfield, draws nothing until a
service loads). On an idle box this layout is the note over the Wafer Waffler beside defrag,
with dim clouds in the blank cells.

It has no starfield, so it draws no activity lanes and no sparks.

### `defrag-wide`

The castle beside a defrag three times as wide, the starfield above and the flow below.

```text
║ ✧ starfield
║ · · · · · · · · · · · · · · · · · ·
╠ 🏰 castle │ ▓ defrag
║ · · · ·   │ · · · · · · · · · · · ·
║ · · · ·   │ · · · · · · · · · · · ·
╠ 🌊 flow
║ · · · · · · · · · · · · · · · · · ·
╚═════════════════════════════════════
```

At 80x24: everything (castle 20x7, defrag 59x7). At 60x20: everything (castle 15x5, defrag
44x5); the starfield is 4 rows, so no lanes.

### `waffler`

A large Wafer Waffler panel beside the castle, a short starfield above and the flow below.

```text
║ ✧ starfield
╠ 🏰 castle     │ ◆ Wafer Waffler
║ · · · · · ·   │ · · · · · · · · · ·
║ · · · · · ·   │ · · · · · · · · · ·
║ · · · · · ·   │ · · · · · · · · · ·
╠ 🌊 flow
╚═════════════════════════════════════
```

At 80x24: everything; the Wafer Waffler panel is 47x10. At 60x20: every panel shows, and the
Wafer Waffler panel (35x7) is clipped and says so (`clipped, needs 25x8`). Below 16x6 the Wafer Waffler panel is taken out and the castle takes the row. The
starfield is 4 rows at 80x24, so no lanes.

### `workload`

What the box is doing: the Training view and the inference server side by side, over the
defrag map. It has no starfield, so no lanes and no sparks.

```text
║ ∿ training         │ ≈ inference
║ · · · · · · · · ·  │ · · · · · · · ·
║ · · · · · · · · ·  │ · · · · · · · ·
║ · · · · · · · · ·  │ · · · · · · · ·
╠ ▓ defrag
║ · · · · · · · · · · · · · · · · · ·
╚═════════════════════════════════════
```

At 80x24: everything (training 40x12, inference 39x12). At 60x20: everything (training 30x10,
inference 29x10). Below that the inference panel goes first, then training, and defrag takes
their rows. With nothing running both panels are a one-line note over blank rows.

### `deck`

All seven views: the starfield; castle, defrag and the Wafer Waffler; flow, training and
inference. It needs a large terminal to show them all; at 80x24 it shows five.

```text
║ ✧ starfield
╠ 🏰 castle  │ ▓ defrag  │ ◆ Wafer Waffler
║ · · · · ·  │ · · · · · │ · · · · · · ·
║ · · · · ·  │ · · · · · │ · · · · · · ·
╠ 🌊 flow    │ ∿ training │ ≈ inference
║ · · · · ·  │ · · · · ·  │ · · · · · · ·
╚═════════════════════════════════════════
```

At 80x24: starfield, castle, defrag, the Wafer Waffler (26x8) and flow; the last row is 6 rows
high, so training (needs 20x10) and inference (needs 24x8) are taken out and named on the rule,
and the flow takes the whole row. At 60x20: the same, with the Wafer Waffler panel at 19x6.
Everything shows at 120x40 and 200x60.

### `skyline`

The quiet one: a tall starfield over the flow.

```text
║ ✧ starfield
║ · · · · · · · · · · · · · · · · · ·
║ · · · · · · · · · · · · · · · · · ·
║ · · · · · · · · · · · · · · · · · ·
╠ 🌊 flow
╚═════════════════════════════════════
```

At 80x24: everything (starfield 80x14). At 60x20: everything (starfield 60x11).

### `memory-lane`

Memory only: a big flow over a small castle and a wide defrag. No starfield, so no lanes and no
sparks.

```text
║ 🌊 flow
║ · · · · · · · · · · · · · · · · · ·
║ · · · · · · · · · · · · · · · · · ·
╠ 🏰 castle │ ▓ defrag
║ · · · ·   │ · · · · · · · · · · · ·
╚═════════════════════════════════════
```

At 80x24: everything (flow 80x11, castle 20x8, defrag 59x8). At 60x20: everything.

`each_preset_degrades_as_the_docs_say` (`src/animation/arcade_presets_tests.rs`) checks what
each layout shows at 80x24 and 60x20.

## Your own layouts

Add a layout with an `[[arcade.layouts]]` table: a `name`, an optional `description`, and its
rows as `[[arcade.layouts.row]]` tables, which take the same keys as `[[arcade.row]]` (see
"Schema" below). The rows after an `[[arcade.layouts]]` line belong to that layout.

```toml
[arcade]
layout = "big-castle"      # start with this layout (optional)

[[arcade.layouts]]
name = "big-castle"
description = "the castle, large, beside the Wafer Waffler; flow below"
backdrop = "sky"           # the arcade's backdrop when this layout is selected (optional)

[[arcade.layouts.row]]
height = 3
panels = [{ view = "castle", width = 2 }, { view = "waffler" }]

[[arcade.layouts.row]]
panels = [{ view = "flow" }]

[[arcade.layouts]]
name = "watch"
description = "training over the inference server"

[[arcade.layouts.row]]
panels = [{ view = "training" }]

[[arcade.layouts.row]]
panels = [{ view = "inference" }]
```

With this file the order is `big-castle`, `watch`, then the eight built-in layouts, and the
arcade starts with `big-castle`.

* A name is 1 to 32 characters: ASCII letters, digits, `-` and `_`. Names are unique, ignoring
  letter case. A name cannot be a built-in layout's name, `config` (kept for the
  `[[arcade.row]]` tables), or `next`, `prev` or `list` (words of `/arcade layout`).
* A description is at most 80 characters with no control character and no invisible format
  character (such as a right-to-left override). Without one, the status
  line shows only the name and the place.
* `backdrop = "off"`, `"scene"`, `"sky"` or `"clouds"` sets the arcade's backdrop when the
  layout is selected; see "A layout's backdrop".
* Each view can be shown once in a layout; two layouts can show the same view.
* `[arcade] layout` names the layout to start with. Your own layouts go in
  `[[arcade.layouts]]`, with an `s`; TOML does not let one key be both.

A problem in one `[[arcade.layouts]]` table drops only that layout. The other layouts, the
`[[arcade.row]]` tables and `[arcade] cloudfield` still work. For `[[arcade.row]]`, `cloudfield` and
the other keys of `[arcade]` the old rule holds: any problem there gives the default arcade with
the cloudfield off, and no `config` layout. A start name that is not known does not count as a
problem of `[arcade]`: the arcade starts with the first layout and everything else is used.

## Schema

```toml
[arcade]
cloudfield = false         # deprecated: [backdrop.views] arcade = "scene" (docs/backdrop.md)

[[arcade.row]]
height = 2                 # a weight (default 1)
panels = [{ view = "starfield" }]

[[arcade.row]]
height = 3
panels = [
  { view = "defrag", width = 3 },     # triple wide
  { view = "castle", width = 1 },
  { view = "die3d",  width = 2 },     # double wide
]

[[arcade.row]]
height = 2
panels = [{ view = "flow" }, { view = "training" }, { view = "inference" }]
```

This example is `arcade_layout::example_rows()`; the bench draws it as `arcade-panels`.

* `[arcade]` takes four keys: `cloudfield`, `row`, `layout` (the layout to start with) and
  `layouts` (your own layouts, see above).
* The `[[arcade.row]]` tables are the layout named `config`, first in the order.
* Each `[[arcade.row]]` takes `height` and `panels`. Rows are drawn top to bottom in file order.
* Each panel takes `view` and `width`. Panels are drawn left to right. They can also be written
  as `[[arcade.row.panels]]` tables.
* `height` and `width` are weights: whole numbers from 1 to 1000, 1 when left out. A row of
  height 2 gets twice the rows of a row of height 1. A panel of width 3 gets three times the
  columns of a panel of width 1 in the same row, less a column for the divider.
* Each view can be shown once.

## Views

| `view` | What it shows | Smallest panel (columns x rows) |
|---|---|---|
| `starfield` | Tensix cores as stars; the activity lanes go in its top rows | 1 x 1 |
| `castle` | Memory Castle: DDR, L2 and L1 as a dungeon of particles | 1 x 1 |
| `flow` | Memory Flow: NoC particles and the DDR perimeter | 1 x 1 |
| `defrag` | the GDDR block map | 1 x 1 |
| `die3d` | the Wafer Waffler view of each Blackhole board, with its bus overlay (`waffler` is accepted as another name) | 16 x 6 |
| `training` | the Training view; `no training run` when there is none | 20 x 10 |
| `inference` | the `i` view's serving snake; `no inference server` when none is up or coming up | 24 x 8 |

Every panel draws the same view as its own mode, at the panel's size:

* The Wafer Waffler panel is the Wafer Waffler view (`Die3dView`), with the same background, theme and cell
  style the `3` view uses. It reads the bus as the `3` view does, so model flags, the bring-up
  fill, pulses and the per-channel GDDR rows show in it. At 16 x 6 and above it always draws a
  scene; the history panel appears from 120 x 36 and the chip flags at the sizes the `3` view
  shows them. It takes no keys; press `3` for the full view.
* The Training panel is the Training view. It reads the bus (`train_bus`), so a run seen on an
  attached host shows in it.
* The inference panel draws the rows the `i` view draws, including an attached host's rows
  (`inference_bus::attached_rows`). Its backdrop is the `i` view's own.

## How the space is shared

From the top, the arcade has the title row, Row 1 (the telemetry strip, or the hero duel while a
model serves), then each row's rule (`╔══[ LABEL ]═══▓▒░`) followed by its content rows. The
bottom row is the footer that the status bar covers. The rule of a row with several panels
lists their labels, separated by `│`.

The content rows, and each row's columns, are cut at `floor(total * running weight / weight
sum)`. Every row and every column is used once. Between two panels of a row there is one `│`
divider column, taken from the right panel's share. The default layout is rows of weight 4, 3
and 3, with castle at width 11 and defrag at width 9. These weights give the old 40% / 30% / 30%
rows and the castle's 55% of the columns exactly.

A panel smaller than its view's smallest size is taken out of its row, and the row's other
panels share its columns. The four views with a 1 x 1 minimum are never taken out: a panel of
theirs that gets no column or no content row draws nothing and keeps its label, as the arcade
did before it had a layout, so the default layout draws the same frame as before at every size. The row's rule names the panel and the size it needs, for example
`◌ WAFER WAFFLER needs 16x6`. The panel taken out first is the rightmost one that does not fit; the
columns are then shared again, so a removal that makes room keeps the other panels. A row with
no panel left keeps its rule and gives its content rows to the other rows.

The layout is worked out when the arcade is made: at start-up, on entering the arcade, on a
resize, and on a switch to another layout. Drawing a frame does no layout work.

## The hero

The hero `@` moves over the rows the layout draws. Power sets its row: 80 W and above puts it in
the top row, 30 to 80 W in the middle row, below 30 W in the bottom row. With two rows the middle
row is the top one; with one row all three ranges share it. Current sets its column across the
whole width. The hero never sits on the activity lanes.

## The bus effects

The rule is written down in `docs/bus.md` ("The arcade with a configured layout"). In short:
gauges, model labels and reset flashes go on the rule of the gauge panel, which is the
topmost, then leftmost, of `starfield`, `castle`, `defrag` and `die3d`, or the first panel when
none of them is shown. Each chip gets an equal share of that panel's columns. Sparks and
compiles rise in the starfield, so a layout with no starfield does not draw them. Request hits
are drawn around the hero.

## The cloudfield (deprecated)

The cloudfield is now the arcade's backdrop, one of the shared backdrops every view can draw
(`docs/backdrop.md`). `cloudfield = true` still works: it is `[backdrop.views] arcade =
"scene"`, and the status line and `--check-config` print a note saying so. A `[backdrop.views]
arcade` entry wins over it. The scene follows the featured inference service: a forge while a
model compiles or loads, a storm while requests wait for their first token, an aurora while
tokens flow, a dream while a model is ready and idle. With no service up it draws nothing. The
arcade can also draw the sky or the clouds, which follow the training step time and the PCIe
traffic and chip load.

It is off by default, because the arcade has never drawn a backdrop. `w` cycles the arcade's
backdrop like any view's, and `/backdrop` sets it. `/arcade cloudfield on|off` still switches the
scene on or off; `/arcade cloudfield` alone flips it. `c` (in the arcade only) turns the
arcade's backdrop off, or back on as the layout's kind (see "A layout's backdrop"). The runtime
setting lasts until you quit and holds across a resize and a layout switch, until a layout with
its own backdrop is selected.

The backdrop does not change the solid black canvas behind every view. That is set by
`TT_TOPLIKE_TRANSPARENT` (any value except empty, `0` and `false` shows the terminal's own
background).

## A layout's backdrop

A layout can set the arcade's backdrop: `backdrop = "off"`, `"scene"`, `"sky"` or `"clouds"` in
its `[[arcade.layouts]]` table. `now-serving` sets `clouds`; the other built-in layouts set none.
When a layout with a backdrop is selected (at start-up, with `{` or `}`, or with `/arcade
layout`), the arcade's backdrop becomes that kind. A layout without one leaves the current
backdrop alone. `w`, `c`, `/backdrop` and `/arcade cloudfield` still change it for the session,
and the next switch to a layout with a backdrop applies that backdrop again. `--check-config`
shows it under the layout's name as `backdrop clouds when selected`, and says which layout the
arcade starts with.

`c` turns the arcade's backdrop off when one is drawn, and back on as the layout's own kind, or
the scene when the layout sets none. `w` steps through every kind.

The older `cloudfield = true` (or `false`) in a layout still works. It is `backdrop = "scene"`
(or `"off"`), and `--check-config` and the status line print a note saying so. When a layout has
both, `backdrop` wins.

## Error messages

A problem never stops the TUI. It starts with the default for the setting that has the problem,
shows the first problem on the status line (`config: <file>:<line>: <message>`, with a count of
the others), and writes every problem to the log. Any problem in `[arcade]` outside the
`[[arcade.layouts]]` tables gives the default layout with the cloudfield off. A problem in one
`[[arcade.layouts]]` table drops only that layout. A TOML syntax error, or a file that cannot be
read, makes the TUI ignore the whole file.

| Problem | Message |
|---|---|
| the file is not a regular file | `is a FIFO; the config must be a regular file` (or a directory, a character device, a block device, a socket) |
| the file is too large | `is larger than 256 KiB (300000 bytes); it was not read` |
| the file cannot be read | `cannot be read: Permission denied (os error 13)` |
| the file is not UTF-8 | `is not UTF-8 text` |
| a symlink to nothing | `is a symlink to a file that does not exist` |
| TOML syntax | `TOML syntax error: <the parser's message>`, its lines joined with `; ` |
| an unknown `tt_smi_reset_behavior` | `unknown tt_smi_reset_behavior "bogus" (expected ignore, inform, dazzle or demo); the TUI uses inform` |
| an unknown top-level key | ``unknown key `colour` `` |
| an unknown key in `[arcade]` | ``unknown key `arcade.clouds` (expected cloudfield, row, layout or layouts)`` |
| an unknown key in a row | ``unknown key `heigth` in row 1 (expected height or panels)`` |
| an unknown key in a panel | ``unknown key `wide` in row 1 panel 1 (expected view or width)`` |
| an unknown view | ``unknown view `castel` (expected one of starfield, castle, flow, defrag, die3d, training, inference)`` |
| a zero, negative or too large weight | `row 1 height must be a whole number from 1 to 1000, got 0` |
| a weight that is not a whole number | `row 1 panel 2 width must be a whole number from 1 to 1000` |
| an empty row | `row 2 is empty: give it at least one panel` |
| a panel with no view | ``row 1 panel 1 has no `view` `` |
| a view twice | ``view `flow` appears twice; each view can be shown once`` |
| a bad cloudfield | `` `arcade.cloudfield` must be true or false `` |
| a wrong type for an existing key | the TOML parser's message, for example `invalid type: string "high", expected f32` |
| an unknown start layout | ``unknown arcade layout `nope` (expected one of classic, now-serving, defrag-wide, waffler, workload, deck, skyline, memory-lane); the arcade starts with `classic` `` |
| a start layout that is not a string | `` `arcade.layout` must be a layout name, such as layout = "deck" `` |
| `[[arcade.layout]]` written for `[[arcade.layouts]]` | `` `arcade.layout` names the layout to start with, such as layout = "deck"; write your own layouts as [[arcade.layouts]] `` |
| `arcade.layouts` is not a list of tables | `` `arcade.layouts` must be a list of layouts: [[arcade.layouts]] `` |
| a layout with no name | ``layout 1 has no `name` `` |
| a name that is not a string | ``layout 1: `name` must be a string`` |
| an empty name | ``layout 1: the name `` is empty`` |
| a name with other characters | ``layout 1: the name `my layout` may hold only letters, digits, `-` and `_` `` |
| a name too long | ``layout 1: the name `...` is longer than 32 characters`` |
| a built-in name | ``layout 1: the name `deck` is a built-in layout; choose another`` |
| the name `config` | ``layout 1: the name `config` is kept for the [[arcade.row]] tables`` |
| the name `next`, `prev` or `list` | ``layout 1: the name `next` is a word of /arcade layout; choose another`` |
| a name used twice | ``layout 2: the name `A` is used twice; each layout needs its own name`` (only a kept layout takes its name) |
| an unknown key in a layout | ``unknown key `rows` in layout 1 (expected name, description, backdrop or row)`` |
| a layout's backdrop that is not a kind | ``layout `a`: `backdrop` must be off, scene, sky or clouds, got "fog"`` |
| a layout's cloudfield that is not true or false | ``layout `a`: `cloudfield` must be true or false`` |
| a description that is not a string | ``layout `a`: `description` must be a string`` |
| a description too long | ``layout `a`: the description is longer than 80 characters`` |
| a control or format character in a description | ``layout `a`: the description has a control character`` |
| a layout with no rows | ``layout `a` has no rows: give it at least one [[arcade.layouts.row]]`` |
| any row or panel problem in a layout | the message above, after the layout: ``layout `a`: row 1 is empty: give it at least one panel`` |
| an unknown `--arcade-layout` | ``--arcade-layout: unknown arcade layout `nope` (expected one of ...); the arcade starts with `classic` `` |

A layout's problems name it by its name when the name is good, and by its place (`layout 2`)
when it is not. A name is shown with control characters escaped and cut at 40 characters.

Each message is prefixed with the file and, where the parser gives a position, the line:
``/home/me/.config/tt-toplike/config.toml:12: unknown view `castel` ...``.

## Cost

Measured with `die3d_panel_frame_time` (`src/animation/arcade_panel_tests.rs`), release build,
200 x 60, two Blackhole chips, on a shared box with a load average near 34:

| Layout | Wafer Waffler panel | Average | 95th percentile |
|---|---|---|---|
| default | none | 2.6 to 2.9 ms | 3.7 to 3.9 ms |
| the example above | 66 x 23 | 1.9 to 2.0 ms | 2.8 ms |
| starfield over castle beside the Wafer Waffler at width 2 | 133 x 37 | 3.1 to 3.2 ms | 4.5 to 4.7 ms |

The Wafer Waffler panel draws a new scene about 20 times a second and returns its last frame in
between, as the `3` view does. A frame at 60 fps has 16.7 ms.
