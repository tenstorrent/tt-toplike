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
| `clouds` | The Wafer Waffler's clouds, one band per cell | The drift follows the PCIe traffic of this machine's chips, in and out, on a log scale (0.8 pixels a second at or below 1 kB/s, 8 at 10 GB/s; a cell is two pixels wide). The brightness follows the busiest chip's load, its power over its TDP. The shapes are decoration. |

Every motion of the scene maps to a signal. The sky's and the clouds' shapes are decoration,
and the legend says so.

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
  Waffler views keep theirs unless `[backdrop.views]` names them.
* `[backdrop.views]` takes the view names `insights`, `grid`, `starfield`, `castle`, `flow`,
  `arcade`, `defrag`, `inference`, `hivemind`, `training` and `die3d`. `waffler` is accepted for
  `die3d` and `"~"` (quoted) for `hivemind`.
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
| `c` (Arcade only) | The arcade's scene on or off: the old cloudfield key. |
| `/arcade cloudfield on\|off` | The same, by its old name. Alone, it flips. |
| `/clouds on\|off` (Waffler only) | The Waffler's clouds on or off. It sets the Waffler's backdrop to `clouds` or `off`. |

`w` was free in every view. `c` was the arcade's cloudfield key; a global `c` would have taken
that key from the arcade, so `c` keeps its arcade meaning and `w` is the global key. The
Waffler's clouds were once on a key, and are now its `/clouds` command, which still works.

A setting made at run time lasts until you quit and holds across a resize.

The legend (`l`) of every view ends with the view's backdrop line, for example
`Backdrop: clouds follows PCIe (drift) and chip load (light); decoration`. When the signals
are estimates (the mock backend, or a host's estimated model), a `~` stands before them.

## Signals and the bus

The backdrop reads at the data rate (`BackdropSignals::gather`) and draws with arithmetic.

* PCIe traffic and power come from the bus world for this TUI's own chips, by PCI address. An
  attached host's other chips are never read. A chip with no fresh reading (none in the last
  10 s) falls back to the backend's own telemetry.
* The training step time is the newest run's `toplike.train.step_ms`, used while it is at most
  30 s old.
* The scene reads the same service rows as the arcade and the `i` view: the attached host's
  models when attached, the mock roster under `--mock`, else the local inference monitor.
* A view the last update did not feed starts a fresh backdrop, so nothing that
  happened while it was off screen draws.

## Cost

The sky and the clouds move on the Waffler clouds' step clock: four steps a second, and
between steps no cell changes. The clouds keep their three flat density bands and their
brightness hysteresis (it moves in steps of 0.05, and only when the load moves by more than
three quarters of a step). The sky's aurora is coloured in four flat brightness bands and
30-degree hue steps, so a step repaints only the cells whose band changed. (The Training view's
own sky keeps its smooth colours, so its frames are unchanged.) The scene keeps the
atmosphere's own clock, one frame per update, because its comets, rain and rings are the
request and token rates themselves.

Measured on the arcade at 200 x 60 in a release build, 300 frames at 60 a second, with the
default layout over a fixed two-chip backend (`src/animation/backdrop_cost.rs`; "busy" is a
model serving with requests and 4 GB/s of PCIe). Frame time is update, render and
drawing into a buffer, on a box with a load average near 50, so the times vary by more than the
backdrops cost.

| Backdrop | Signals | Cells changed per frame | Mean frame ms | 95th percentile ms |
|---|---|---|---|---|
| off | idle | 2461 | 3.51 | 8.22 |
| off | busy | 2461 | 2.96 | 6.32 |
| scene | idle | 2461 | 4.85 | 15.44 |
| scene | busy | 2908 | 3.15 | 5.96 |
| sky | idle | 2479 | 2.22 | 2.98 |
| sky | busy | 2479 | 2.67 | 4.29 |
| clouds | idle | 2458 | 2.22 | 3.03 |
| clouds | busy | 2485 | 3.15 | 10.78 |

The backdrop alone, best of seven batches on the busy frame:

| Backdrop | Drawing the canvas, ms | Laying it under the frame, ms |
|---|---|---|
| scene | 0.263 | 0.658 (the scene draws its canvas every frame) |
| sky | 0.601 | 0.402 (canvas cached between steps) |
| clouds | 0.739 | 0.409 (canvas cached between steps) |

The scene adds about 450 changed cells a frame while a model serves; with no service it draws
nothing and changes nothing. The sky adds about 18 and the clouds about 24. Before the sky's
aurora was banded, the sky added 137 a frame.
