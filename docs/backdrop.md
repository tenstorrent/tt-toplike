# Backdrops

Three views carry a background of their own: the Inference view, the Training view and the
Wafer Waffler. The backdrop applies to these three only. Each draws its own kind by default
and can be set to one of the others or off. Every other view carries no backdrop.

A backdrop fills only the cells the view leaves blank, so it never covers the view's own
drawing, and it paints no background colour: the canvas stays black, or the terminal's own with
`TT_TOPLIKE_TRANSPARENT`.

There are three kinds. Each is the painter one of those views already had, now shared
(`src/animation/backdrop.rs`).

| Kind | What it is | What drives it |
|---|---|---|
| `scene` | The Inference view's atmosphere | The featured inference service (a loading one, else the first ready one): a forge of beams while it compiles or loads, growing with kernels compiled and weight shards landed, with sparks while kernels compile and a floor that glows with host CPU; a storm while requests wait for their first token, heavier while they run; an aurora while tokens flow, its brightness from generation tokens per second and its hue from the KV cache; falling rain for prompt tokens per second and rising gold for generation tokens per second; a comet per running request and a dim star per waiting one; a tide as high as the KV cache is full; a ring for each finished request; red static after an error; embers when a load has stalled; a forming canvas while a diffusion job runs; a faint dream while a model is ready and idle. With no service up it draws nothing. |
| `sky` | The Training view's aurora and stars | The speed follows the training step time of the newest run on the bus (a 500 ms step is the usual speed, from half to twice as fast). The stars and the aurora are decoration. |
| `clouds` | The Wafer Waffler's clouds, one band per cell | The drift follows the PCIe traffic of this machine's chips, in and out, on a log scale (0.8 pixels a second at or below 1 kB/s, 8 at 10 GB/s; a cell is two pixels wide). The brightness follows the busiest chip's load, measured as the Waffler measures it: where the chip's draw sits between its own idle and its recent peak (`die3d::load::LoadScale`), with power over TDP for the first five samples. The shapes are decoration. |

The scene's motions map to signals, except its dream and its slow drift, which move with the
atmosphere's own frame count. The sky's and the clouds' shapes are decoration, and the legend
says so.

## Which views take one

| View | Takes | Default backdrop |
|---|---|---|
| Inference (`i`) | every kind | `scene`, drawn by the snake's own painter |
| Training (`t`) | every kind | `sky`, drawn by the Training view's own painter |
| Wafer Waffler (`3`) | every kind | `clouds`, drawn at pixel resolution by the Waffler itself |
| Insights, Grid, Starfield, Castle, Flow, Arcade, Defrag, `~` | none | none |

Choosing another kind for the Inference, Training or Waffler view turns that view's own
painter off, and the shared backdrop draws the new kind in its blank cells. Choosing
the view's own kind brings its own painter back.

One table says which view takes which kind: `accepts` in `src/animation/backdrop.rs`. Letting
another view take a backdrop is one line there. A Wafer Waffler panel inside an arcade layout
draws its own clouds, as the view does; the arcade draws nothing behind its panels.

## Config

In `~/.config/tt-toplike/config.toml`, the only config file:

```toml
[backdrop]
default = "off"            # off | scene | sky | clouds

[backdrop.views]
training = "off"           # the Training view without its sky
waffler = "scene"          # the Wafer Waffler with the Inference view's scene
```

* `[backdrop.views]` takes the view names `insights`, `grid`, `starfield`, `castle`, `flow`,
  `arcade`, `defrag`, `inference`, `hivemind`, `training` and `die3d`. `waffler` is accepted for
  `die3d` and `"~"` (quoted) for `hivemind`. An entry for a view that takes no backdrop is
  ignored, with the note `[backdrop.views] castle is ignored: no backdrop on this view`.
* `default` is for views that take a backdrop and have none of their own. Today there is no
  such view, so a `default` other than `off` is ignored, with a note.
* The old arcade settings, `[arcade] cloudfield` and a layout's `backdrop` or `cloudfield`, are
  read and ignored, with the note "the arcade carries no backdrop".
* A note shows on the status line and in `--check-config`, and does not make `--check-config`
  fail.

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
| a view set twice (`die3d` and `waffler`) | ``view `die3d` is set twice in [backdrop.views]`` |

## Keys and commands

| Key or command | What it does |
|---|---|
| `w` | In the Inference, Training and Waffler views, the next backdrop: off, scene, sky, clouds, then off again. In every other view it says "no backdrop on this view" and changes nothing. |
| `/backdrop` | Shows the view's backdrop and what it follows, or "no backdrop on this view". |
| `/backdrop off\|scene\|sky\|clouds` | Sets the view's backdrop, in a view that takes one. |
| `/arcade cloudfield` | Says "the arcade carries no backdrop" and changes nothing. |
| `/clouds on\|off` (Waffler only) | The Waffler's clouds on or off. It sets the Waffler's backdrop to `clouds` or `off`. |

The arcade's `c` key is gone with its backdrop. The Waffler's clouds were once on a key, and
are now its `/clouds` command, which still works.

A setting made at run time lasts until you quit and holds across a resize. The message a key
or `/backdrop` puts on the status line goes away after about 4 seconds, and when you leave the
view.

The legend (`l`) of the Inference, Training and Waffler views ends with the view's backdrop
line, for example
`Backdrop: clouds follows PCIe (drift), load (light); shape is decoration`. When the signals
are estimates (the mock backend, or a host's estimated model), a `~` stands before them.

## Signals and the bus

The backdrop reads at the data rate (`BackdropSignals::gather`) and draws with arithmetic.

* PCIe traffic and power come from the bus world for this TUI's own chips, by PCI address. An
  attached host's other chips are never read. A chip with no fresh reading (none in the last
  10 s) falls back to the backend's own telemetry.
* The training step time is the newest run's `toplike.train.step_ms`, used while it is at most
  30 s old.
* The scene reads the same service rows as the `i` view: the attached host's models when
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
