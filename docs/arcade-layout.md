# Arcade layout

The arcade (`a`) shows several views at once. Which views, in which rows, and how wide each one
is, come from the `[arcade]` section of the config file. With no `[arcade]` section the arcade
draws the layout it has always had.

## Where the config lives

* `~/.config/tt-toplike/config.toml` is the shared config file. The other settings in it
  (`sensitivity`, `max_particles_scale`, `tt_smi_reset_behavior`) work as before.
* `./.toplikerc` in the directory you start tt-toplike from may override it. It has the same
  schema. A key set in both files takes the value from `.toplikerc`, key by key: `cloudfield`
  set only in the shared file stays, and an `[[arcade.row]]` list in `.toplikerc` replaces the
  whole row list of the shared file.

Check the files without starting the TUI:

```sh
tt-toplike --check-config
```

It lists the files it read, every problem with its file and line, and the arcade layout that
will be used. It exits with status 1 when there is a problem and 0 otherwise. It opens no device.

## Schema

```toml
[arcade]
cloudfield = false         # the atmosphere backdrop behind the whole arcade (default false)

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

* `[arcade]` takes two keys: `cloudfield` and `row`.
* Each `[[arcade.row]]` takes `height` and `panels`. Rows are drawn top to bottom in file order.
* Each panel takes `view` and `width`. Panels are drawn left to right. They can also be written
  as `[[arcade.row.panels]]` tables.
* `height` and `width` are weights: whole numbers from 1 to 1000, 1 when left out. A row of
  height 2 gets twice the rows of a row of height 1. A panel of width 3 gets three times the
  columns of a panel of width 1 in the same row, less the one column the divider takes.
* Each view can be shown once.

## Views

| `view` | What it shows | Smallest panel (columns x rows) |
|---|---|---|
| `starfield` | Tensix cores as stars; the activity lanes go in its top rows | 1 x 1 |
| `castle` | Memory Castle: DDR, L2 and L1 as a dungeon of particles | 1 x 1 |
| `flow` | Memory Flow: NoC particles and the DDR perimeter | 1 x 1 |
| `defrag` | the GDDR block map | 1 x 1 |
| `die3d` | the Die 3D view of each Blackhole board, with its bus overlay (`waffler` is accepted as another name) | 16 x 6 |
| `training` | the Training view; `no training run` when there is none | 20 x 10 |
| `inference` | the `i` view's serving snake; `no inference server` when none is up or coming up | 24 x 8 |

Every panel draws the same view as its own mode, at the panel's size:

* The Die 3D panel is the Die 3D view (`Die3dView`), with the same background, theme and cell
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
panels share its columns. The row's rule names the panel and the size it needs, for example
`◌ DIE 3D needs 16x6`. The panel taken out first is the rightmost one that does not fit; the
columns are then shared again, so a removal that makes room keeps the other panels. A row with
no panel left keeps its rule and gives its content rows to the other rows.

The layout is worked out when the arcade is made: at start-up, on entering the arcade, and on a
resize. Drawing a frame does no layout work.

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

## The cloudfield

`cloudfield = true` lays the atmosphere backdrop (`src/animation/atmosphere.rs`, the one behind
the `i` view's snake) behind the whole arcade. It follows the featured inference service: a forge
while a model compiles or loads, a storm while requests wait for their first token, an aurora
while tokens flow, a dream while a model is ready and idle. It fills blank cells only, so no
view's drawing is covered. With no service up it draws nothing.

It is off by default, because the arcade has never drawn a backdrop. Switch it while the arcade
is on screen with `c`, or from the command bar with `/arcade cloudfield on`, `/arcade
cloudfield off`, or `/arcade cloudfield` to flip it. The runtime switch lasts until you quit and
holds across a resize.

The cloudfield does not change the solid black canvas behind every view. That is set by
`TT_TOPLIKE_TRANSPARENT` (any value except empty, `0` and `false` shows the terminal's own
background).

## Error messages

A problem never stops the TUI. It starts with the default for the setting that has the problem,
shows the first problem on the status line (`config: <file>:<line>: <message>`, with a count of
the others), and writes every problem to the log. Any problem in `[arcade]` gives the default
layout with the cloudfield off. A TOML syntax error makes the TUI ignore that whole file.

| Problem | Message |
|---|---|
| TOML syntax | `TOML syntax error: <the parser's message>` |
| an unknown top-level key | ``unknown key `colour` `` |
| an unknown key in `[arcade]` | ``unknown key `arcade.clouds` (expected cloudfield or row)`` |
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

Each message is prefixed with the file and, where the parser gives a position, the line:
``/home/me/.config/tt-toplike/config.toml:12: unknown view `castel` ...``.

## Cost

Measured with `die3d_panel_frame_time` (`src/animation/arcade_panel_tests.rs`), release build,
200 x 60, two Blackhole chips, on a shared box with a load average near 34:

| Layout | Die 3D panel | Average | 95th percentile |
|---|---|---|---|
| default | none | 2.6 to 2.9 ms | 3.7 to 3.9 ms |
| the example above | 66 x 23 | 1.9 to 2.0 ms | 2.8 ms |
| starfield over castle beside Die 3D at width 2 | 133 x 37 | 3.1 to 3.2 ms | 4.5 to 4.7 ms |

The Die 3D panel draws a new scene about 20 times a second and returns its last frame in
between, as the `3` view does. A frame at 60 fps has 16.7 ms.
