# Backdrops

Every full-screen view can draw a backdrop behind what it shows. A backdrop fills only the
cells the view leaves blank, so it never covers the view's own drawing, and it paints no
background colour: the canvas stays black, or the terminal's own with
`TT_TOPLIKE_TRANSPARENT`.

There are three kinds. Each is the painter one view already had, now shared
(`src/animation/backdrop.rs`).

| Kind | What it is | What drives it |
|---|---|---|
| `scene` | The Inference view's atmosphere | The featured inference service (a loading one, else the first ready one): a forge of beams while it compiles or loads, growing with kernels compiled and weight shards landed, with sparks while kernels compile and a floor that glows with host CPU; a storm while requests wait for their first token, heavier while they run; an aurora while tokens flow, its brightness from generation tokens per second and its hue from the KV cache; falling rain for prompt tokens per second and rising gold for generation tokens per second; a comet per running request and a dim star per waiting one; a tide as high as the KV cache is full; a ring for each finished request; red static after an error; embers when a load has stalled; a forming canvas while a diffusion job runs; a faint dream while a model is ready and idle. With no service up it draws nothing. |
| `sky` | The Training view's aurora and stars | The speed follows the training step time of the newest run on the bus (a 500 ms step is the usual speed, from half to twice as fast). The stars and the aurora are decoration. |
| `clouds` | The Wafer Waffler's clouds, one band per cell | The drift follows the PCIe traffic of this machine's chips, in and out, on a log scale (0.8 pixels a second at or below 1 kB/s, 8 at 10 GB/s; a cell is two pixels wide). The brightness follows the busiest chip's load, measured as the Waffler measures it: where the chip's draw sits between its own idle and its recent peak (`die3d::load::LoadScale`), with power over TDP for the first five samples. The shapes are decoration. |

The scene's motions map to signals, except its dream and its slow drift, which move with the
atmosphere's own frame count. The sky's and the clouds' shapes are decoration, and the legend
says so.

## Defaults and refusals

With no setting, every view looks as it did:

| View | Default backdrop |
|---|---|
| Inference (`i`) | `scene`, drawn by the snake's own painter |
| Training (`t`) | `sky`, drawn by the Training view's own painter |
| Wafer Waffler (`3`) | `clouds`, drawn at pixel resolution by the Waffler itself |
| every other view (Insights, Grid, Starfield, Castle, Flow, Arcade, Defrag, `~`) | off |

Choosing another kind for the Inference, Training or Waffler view turns that view's own
painter off, and the shared backdrop draws the new kind in its blank cells. Choosing
the view's own kind brings its own painter back.

The Starfield refuses `scene` and `sky`. Every star in the Starfield is a Tensix core, and both
of those backdrops draw stars that would look like cores. It takes `clouds`.

## Config

In `~/.config/tt-toplike/config.toml`, the only config file:

```toml
[backdrop]
default = "off"            # off | scene | sky | clouds

[backdrop.views]
arcade = "scene"
castle = "clouds"
training = "off"           # the Training view without its sky
```

* `default` sets every view that has no backdrop of its own. The Inference, Training and
  Waffler views keep theirs unless `[backdrop.views]` names them. A view that refuses the
  default stays off: `default = "scene"` or `"sky"` leaves the Starfield off, and the status
  line and `--check-config` print a note saying so.
* `[backdrop.views]` takes the view names `insights`, `grid`, `starfield`, `castle`, `flow`,
  `arcade`, `defrag`, `inference`, `hivemind`, `training` and `die3d`. `waffler` is accepted for
  `die3d` and `"~"` (quoted) for `hivemind`.
* An arcade layout can set the arcade's backdrop when it is selected (`backdrop = "clouds"` in
  an `[[arcade.layouts]]` table; `now-serving` does). See `docs/arcade-layout.md`, "A layout's
  backdrop".
* The old `[arcade] cloudfield = true` still works. It means `[backdrop.views] arcade =
  "scene"`; `false` means off. A `[backdrop.views] arcade` entry wins over it. The status line
  and `--check-config` print a note that it is deprecated; the note does not make
  `--check-config` fail.

`tt-toplike --check-config` prints the backdrop settings and every problem with its line. A bad
entry is left out and keeps its default; the other entries are used.

| Problem | Message |
|---|---|
| `backdrop` is not a table | `` `backdrop` must be a table: [backdrop] `` |
| an unknown key | ``unknown key `backdrop.fast` (expected default or views)`` |
| a bad `default` | `` `backdrop.default` must be off, scene, sky or clouds, got "fog" `` |
| `views` is not a table | `` `backdrop.views` must be a table: [backdrop.views] `` |
| an unknown view | ``unknown view `castel` in [backdrop.views] (expected one of insights, grid, ...)`` |
| a value that is not one of the four | `` `backdrop.views.grid` must be off, scene, sky or clouds `` |
| a refused kind | `starfield refuses sky: its stars are Tensix cores, and this backdrop draws stars too` |
| a view set twice (`die3d` and `waffler`) | ``view `die3d` is set twice in [backdrop.views]`` |

## Keys and commands

| Key or command | What it does |
|---|---|
| `w` | The next backdrop for the view on screen: off, scene, sky, clouds, then off again, skipping what the view refuses. Works in every view. |
| `/backdrop` | Shows the view's backdrop and what it follows. |
| `/backdrop off\|scene\|sky\|clouds` | Sets the view's backdrop. |
| `c` (Arcade only) | The arcade's backdrop off when one is drawn, else on as the layout's own kind (the scene when the layout sets none). |
| `/arcade cloudfield on\|off` | The arcade's scene on or off, by its old name. Alone, it flips. |
| `{` `}` (Arcade only) | The previous and next layout; a layout with its own backdrop sets it. |
| `/clouds on\|off` (Waffler only) | The Waffler's clouds on or off. It sets the Waffler's backdrop to `clouds` or `off`. |

`w` was free in every view. `c` was the arcade's cloudfield key; a global `c` would have taken
that key from the arcade, so `c` stays the arcade's backdrop switch and `w` is the global key. The
Waffler's clouds were once on a key, and are now its `/clouds` command, which still works.

A setting made at run time lasts until you quit and holds across a resize. The message a key
or `/backdrop` puts on the status line goes away after about 4 seconds, and when you leave the
view.

The legend (`l`) of every view ends with the view's backdrop line, for example
`Backdrop: clouds follows PCIe (drift), load (light); shape is decoration`. When the signals
are estimates (the mock backend, or a host's estimated model), a `~` stands before them.

## Signals and the bus

The backdrop reads at the data rate (`BackdropSignals::gather`) and draws with arithmetic.

* PCIe traffic and power come from the bus world for this TUI's own chips, by PCI address. An
  attached host's other chips are never read. A chip with no fresh reading (none in the last
  10 s) falls back to the backend's own telemetry.
* The training step time is the newest run's `toplike.train.step_ms`, used while it is at most
  30 s old.
* The scene reads the same service rows as the arcade: the attached host's models when
  attached, the mock roster under `--mock`, else the local inference monitor. Under `--remote`
  it gets none (the `i` view reads the remote box's own list there).
* A view the last update did not feed starts a fresh backdrop, so nothing that
  happened while it was off screen draws, and a frame drawn before the next update lays no
  backdrop. The chips' load scales are kept across views.
* The shared backdrop is laid after the view and before the status bar. A popup a view draws
  itself (the Insights kill dialog) has a background of its own, and the command bar clears its
  rows, so neither takes backdrop glyphs.

## Cost

Every kind moves on the Waffler clouds' step clock: four steps a second, and between steps no
cell changes and no canvas is drawn (each kind's canvas is drawn once per step and size). At
each step the scene takes the atmosphere frames a quarter second holds at 60 a second, so its
comets, rain and rings move at the same speed whatever the view's frame rate; a request that
finishes between steps rings at the next step. The shared scene's colours are banded per
channel. The clouds keep their three flat density bands and their brightness hysteresis (it
moves in steps of 0.05, and only when the load moves by more than three quarters of a step).
The sky's aurora is coloured in four flat brightness bands and 30-degree hue steps. The
Inference view's own scene and the Training view's own sky keep their own clocks and smooth
colours, so their frames are unchanged.

What the terminal receives for the backdrop alone on a blank 200 x 60 view, through ratatui's
diff and the crossterm backend (`backdrop_blank_view_traffic` in
`src/animation/backdrop_cost.rs`, a fixed clock, 10 s after 1 s to settle; "busy" is a model
serving and 4 GB/s of PCIe; the sky has no training run here, so it runs at its usual speed):

| Backdrop | Signals | 10 fps, bytes/s | 60 fps, bytes/s | 60 fps before the step clock |
|---|---|---|---|---|
| scene | idle | 250 | 1,500 | 1,500 |
| scene | busy | 72,848 | 74,098 | 1,173,881 |
| sky | either | 50,400 | 51,650 | 51,650 |
| clouds | idle | 6,558 | 7,808 | 7,808 |
| clouds | busy | 61,898 | 63,148 | 63,148 |

Before the step clock the busy scene sent 248,458 bytes a second at 10 fps. The idle rows are
the cursor and flush bytes of each frame.

Measured on the arcade at 200 x 60 in a release build, 300 frames at 60 a second, with the
default layout over a fixed two-chip backend (`backdrop_cost`). Frame time is update, render
and drawing into a buffer, on a box with a load average near 8, so the times vary by more than
the backdrops cost. This run is from 2026-11-02, after the step clock.

| Backdrop | Signals | Cells changed per frame | Mean frame ms | 95th percentile ms |
|---|---|---|---|---|
| off | idle | 2461 | 3.70 | 8.26 |
| off | busy | 2461 | 2.18 | 2.88 |
| scene | idle | 2461 | 2.75 | 3.41 |
| scene | busy | 2497 | 2.65 | 3.42 |
| sky | idle | 2479 | 3.43 | 6.48 |
| sky | busy | 2479 | 4.05 | 8.30 |
| clouds | idle | 2473 | 2.88 | 3.60 |
| clouds | busy | 2499 | 3.82 | 7.29 |

The backdrop alone on the busy frame, best of seven batches, from the same run
(`backdrop_layer_cost`):

| Backdrop | Drawing the canvas, ms | Laying it under the frame, ms |
|---|---|---|
| scene | 0.577 | 0.909 |
| sky | 1.502 | 0.562 |
| clouds | 1.222 | 0.472 |

The arcade changes about 2461 cells a frame of its own, so on the arcade the backdrops add
little; the blank-view table shows the cost on a quiet view.
