# Arcade layout

The arcade (`a`) shows several views at once. Which views, in which rows, and how wide each one
is, come from the `[arcade]` section of the config file. With no `[arcade]` section the arcade
draws the layout it has always had.

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

It says whether it found the file, lists every problem with its line, and shows the arcade
layout that will be used. It exits with status 1 when there is a problem and 0 otherwise. It opens no device.

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

* `[arcade]` takes two keys: `cloudfield` and `row`.
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
backdrop like any view's, and `/backdrop` sets it. `c` (in the arcade only) and `/arcade
cloudfield on|off` still switch the scene on or off; `/arcade cloudfield` alone flips it. The
runtime setting lasts until you quit and holds across a resize.

The backdrop does not change the solid black canvas behind every view. That is set by
`TT_TOPLIKE_TRANSPARENT` (any value except empty, `0` and `false` shows the terminal's own
background).

## Error messages

A problem never stops the TUI. It starts with the default for the setting that has the problem,
shows the first problem on the status line (`config: <file>:<line>: <message>`, with a count of
the others), and writes every problem to the log. Any problem in `[arcade]` gives the default
layout with the cloudfield off. A TOML syntax error, or a file that cannot be read, makes the TUI
ignore the whole file.

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

| Layout | Wafer Waffler panel | Average | 95th percentile |
|---|---|---|---|
| default | none | 2.6 to 2.9 ms | 3.7 to 3.9 ms |
| the example above | 66 x 23 | 1.9 to 2.0 ms | 2.8 ms |
| starfield over castle beside the Wafer Waffler at width 2 | 133 x 37 | 3.1 to 3.2 ms | 4.5 to 4.7 ms |

The Wafer Waffler panel draws a new scene about 20 times a second and returns its last frame in
between, as the `3` view does. A frame at 60 fps has 16.7 ms.
