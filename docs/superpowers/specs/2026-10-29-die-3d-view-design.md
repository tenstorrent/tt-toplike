# Die 3D view

Date: 2026-10-29. Branch: `what-if`. Status: built and shipped in v0.13.19 as tier 0
(sysfs, hwmon, smbus telemetry, PCIe counters). Tier 1 per-tile counters are not built; see
`docs/superpowers/plans/2026-10-29-die-3d-tier1-counters.md`. The first design was agreed in
conversation and then changed as Taylor judged recordings; the section "What changed from the
first design" at the end lists the differences. The mockups are in
`docs/superpowers/specs/assets/2026-10-29-die-3d/` (open in a browser; they animate). They
use the first design's teal palette.

## Why

Taylor wants a view that looks into the physical device: the board and the die, with
the insides lighting up and connecting while a model thinks or runs. The wish, in
their words: "the more we can derive from hardware the better", and "I want to blow
people's minds". The existing Chip Portrait (`src/ui/tui/chip_portrait.rs`) draws a
fixed 17x12 grid with particles derived from chip-wide activity. It does not know
where tiles sit on the die, and nothing in it is per-tile.

This came out of reading `dtsunami/tt-baremetal`'s `bhtop`, which reads per-tile NoC
counters and draws the die. It does not need to be copied. What we took from it is the
idea of two coordinate systems per tile (physical `die` and logical `noc0`) and the
NIU counter set.

## Goals

- A new terminal view, a "3D" mode on the `3` key, showing the board and the die in
  isometric perspective, drawn with braille characters (2x4 dots per cell, two colours per
  cell) or, by switch, half-block cells. The reference mockup is `die-3d-resolution.html`,
  panel 1: 150 x 42 cells.
- Everything that moves is either read from hardware, labelled on screen as an estimate, or
  named as decoration in the legend and the explain text. No decoration passes as telemetry.
- Per-tile NoC traffic as an opt-in data source (see Data tiers). It lights individual
  tiles and the NoC wires between them.
- Two scene types (Taylor's decision). A **board scene** shows one board, with all of its
  dies on the card (a p300c has two), and keys switch between boards. A **super scene**
  shows every board in one view. On a one-board machine the super scene is the board
  scene.
- The camera moves on signals. When something notable happens, such as a chip becoming
  more active, the view cuts to another angle. The cut does not have to be smooth. It
  can break into noise and re-form (see Camera).

## Non-goals

- A real 3D window. The egui mockup in `die-3d.html` was made for Taylor to judge. It is
  not part of this work. It may become a later project.
- Per-tile data in the default configuration. The default stays passive.
- Wormhole and Grayskull layouts in the first version. Blackhole first. Other chips are named
  in the footer and not drawn.
- Replacing the Chip Portrait. The 3D view is an additional mode.

## Anti-goals (a failure even if it runs)

- Drawing per-tile detail from chip-wide load without saying so on screen.
- Disturbing a running workload more than the user chose to risk.
- Anything that touches the ARC, PCIe, L2CPU or security tiles' NIU registers.

## Data tiers

Every tier is chosen by the user. Nothing in tier 1 starts by itself.

**Tier 0, passive (default, shipped).** Sysfs and hwmon only. Real signals: PCIe in and out
(the PCIe tile's NIU counters through `pcie_perf_counters`, fixed in v0.13.18), chip power,
temperature, GDDR temperature and Ethernet or harvest data when the tt-smi-fed backend
provides them. The die is drawn from the layout table. The per-tile glow is an estimate
from chip activity (see "Activity and the glow"), and the footer says so: `per-tile glow:
estimate from chip load`. A quiet idle wave keeps an idle die visible. The clouds and the
history panel are described below; the panel is real data and the clouds are decoration.

**Tier 1, per-tile counters, opt-in (not shipped).** Reads each Tensix, DRAM and Ethernet
tile's NIU counters through tt-exalens and lights the tiles and wires from real flit counts.
It is a separate, later and experimental plan:
`docs/superpowers/plans/2026-10-29-die-3d-tier1-counters.md`. Release 0.13.19 reads no
per-tile counters.

**Warning rule (Taylor's decision).** People own their hardware and can take the risk.
If nothing is running on the chip, start. If something is running, show a warning
that names what is running and the possible effect, then let the user continue. What
happens next is up to the other software. No hard block.

## Camera

Taylor's idea. A signal such as a chip becoming more active moves the view to a new
angle. The transition does not need to be smooth.

- **Presets (six).** The four corners at isometric pitch, a top-down view and a low view
  (`camera::PRESETS`). The close view of the busiest region (a seventh preset) is not built.
- **Triggers (two).** A chip's load rising, and a burst of PCIe traffic. A reset, a model
  starting to load and a GDDR ECC event are not detected, because each needs a signal source
  that lives elsewhere (the reset takeover, the inference monitor, the unparsed
  `gddr_telemetry` block).
- **Load rise.** Load is the chip's power measured against that chip's own idle (see
  "Activity and the glow"). A rise fires at 0.5 and the detector re-arms at 0.25 or below.
- **PCIe burst.** Fires when the chip's in plus out rate reaches 50 MB/s and re-arms at
  10 MB/s or below.
- **Cooldown.** 8 seconds between automatic cuts. `c` cuts by hand at any time, and `C`
  turns automatic cuts off. With them off the view also stops switching scenes on its own.
- **Scene changes.** In the all-boards scene a load rise also switches to that chip's
  board scene at the middle of the cut. The view returns to the all-boards scene after 20
  seconds with no load rise, only if the view made the switch. Choosing a scene by hand
  (`m`, `[`, `]`) cancels the return. In a board scene only that board's chips cut the
  camera, and only the angle changes.
- **Transitions.** Three kinds, chosen by a shuffle with weights 40 (chaos), 35 (dissolve)
  and 25 (spin), never the same kind twice in a row. Chaos (0.8 s) shifts rows and flickers
  colours until the camera swaps, then settles. Dissolve (0.9 s) replaces the old picture
  with the new one pixel by pixel in random order. Spin (1.6 s) turns the camera from the
  old angle to the new one. It is an attempt: the tiles keep their isometric shape while
  their positions rotate, so gaps and overlaps can show mid-spin. Spin is used only for
  camera changes where the board shown stays the same. `X` cycles shuffle, chaos, dissolve
  and spin.
- **Caption.** The footer names the corner of the board nearest the viewer: front-right,
  back-right, back-left or front-left, with "top-down, " in front for the top view. Front is
  the PCIe edge and right is the side of the right-hand GDDR blocks. During a cut it reads
  `view: a -> b`. The caption outranks the "N chips not drawn" note when the footer is
  short of room. A first convention named an edge, and two edges tie on a square board at
  every quarter turn; a corner has no tie.
- **Labels.** Each die carries a label with the board's chip name and its power
  (`BH0 44W`), so four dies in the all-boards scene are not one unnamed field. `L` turns
  them off.

## Rendering

- Software rasteriser into an RGB buffer, then converted to terminal cells. The mockup
  in `die-3d-resolution.html` is the reference: `Scene(300, 168, 2, true)` for braille.
  For each 2x4 block of pixels it picks a foreground and a background colour from the
  block's brightness and sets the dots for the foreground pixels.
- **Braille and half-block, both kept.** Braille is the default. The half-block render
  (`▀`, 1x2 pixels per cell) is crisper in some real fonts, because braille dots are small
  at a real font size. Taylor saw both in Konsole and GNOME-style fonts, liked both, and
  asked for a switch (`B`). A two-die board in half-block needs about 120 x 35 cells; below
  that the view shows a "press B" hint. The automatic switch to half-block at about 220
  columns is not built.
- **Palette.** A Grayskull-style palette from `src/animation/die3d/palette.rs`, the same
  family as the project's Grayskull theme. The first design's teal was replaced after
  Taylor asked for more Grayskull colour. Values (RGB): idle tile `58,52,78` (indigo grey),
  warm `168,120,210` (lilac), hot `255,92,170` (hot pink), GDDR `150,168,190`, Ethernet
  `196,170,230` (pale lavender), management `112,110,128`, slab `26,24,38`, NoC 0
  `190,120,235` (orchid), NoC 1 and packets in `120,205,225` (pale cyan), packets out
  `255,200,225` (pale pink), clouds `74,62,104`. The heat ramp stays in the purple and pink
  range: no orange, no green.
- **Background.** Transparent by default: cells with no scene content use the terminal's own
  background (`Color::Reset`), so a translucent or tinted terminal shows through. A terminal
  cannot blend, so there are no soft halos. `T` switches to opaque pure black. Opaque mode
  is flat black; the first design's tinted vignette was dropped when Taylor asked for black.
- Faces carry a shading gradient and tiles carry darker edges. These removed more of
  the blocky look than raising the resolution did.
- Truecolour. Left side and bottom bars only, no right-side borders (global CLAUDE.md
  terminal rule).

## Activity and the glow

The first version measured load as chip power over the hwmon power limit. On the test box
the limit is 125 W and idle chips draw 36 to 47 W, so an idle chip already read 0.30. That
is above the detector's re-arm level of 0.25, so the load trigger could never arm and the
camera never cut on activity. Under token serving (8 concurrent chat clients, 213 tokens
per second against a Qwen3-Coder-Next server, 96 requests, no errors) the two busy chips
drew a mean of 54 and 45 W with a peak of 68 W, which is 0.54 of the limit.

The view now measures each chip against its own history (`load.rs`): it keeps the last ten
minutes of that chip's power, one sample per second, and reports where the present draw
sits between the lowest and the highest draw in that window. The span has a floor of 25 W
so that a few watts of wobble at idle stay small; a real burst (37 W idle to a 68 W peak
under serving) still reaches full load. Until a chip has five samples the view uses power
over the limit.

With relative load an idle chip reads 0.0, which would leave an idle die dark and still.
The tile wave therefore has a floor: amplitude is 0.30 + 0.70 x load. An idle die keeps a
quiet lilac wave and activity ramps up from there.

The glow is still an estimate. It comes from chip-wide activity, and no tile counters feed
it. The footer carries the label.

PCIe thresholds come from the same serving test: the two busy chips moved a mean of 23 MB/s
into the chip (95th percentile 115 MB/s, maximum 131 MB/s) and 13 MB/s out (maximum 53 MB/s),
and idle chips were near zero. The first threshold of 2 GB/s was about 15 times above
anything token serving moves. 50 MB/s is above the serving mean and below its bursts, and
10 MB/s re-arms below the mean.

## Clouds

Clouds drift behind the dies (`clouds.rs`, `W` switches them). They are decoration, and the
legend and explain text say so. The drift speed follows the box's total PCIe traffic, from
a floor of 0.8 px/s (so the sky is never still) up to 8 px/s. The brightness follows the
busiest chip's load, from 0.25 to 0.75 of the cloud colour, so the brightest cloud is about
as bright as an idle tile. In transparent mode, sky with no cloud stays unpainted. To keep
the cost of the terminal traffic down, the cloud clock steps 4 times a second, the drift
moves in whole pixels and the colour comes in three flat density bands.

## History panel

A small panel at the top left (`history.rs`, `H` switches it) plots the last 60 seconds as
sparklines: power and temperature of each Blackhole chip (up to six) and the box's PCIe in
and out summed over the chips drawn, each with its latest value. Every value is a reading,
one sample per second. It needs a view of 120 x 36 cells (a terminal of 120 x 37) and is
hidden below that.

## Footer and switches

The footer is two rows plus a bar. The first row shows the scene, the estimate label and the
camera caption, in that order of priority. The second row shows each switch with its state,
built from one table (`Die3dView::switches_at`): `B` braille or half-block, `T` transparent or
opaque, `C` auto-cuts, `X` transition, `m` all boards or board, `L` labels, `W` clouds, `H`
history. A new switch is one table entry plus a key. The full form of the row, with the
action keys `[ ] board · c cut · 3 leave`, is 153 characters and needs a view 156 columns
wide. Narrower, the action keys drop first, then each switch shrinks to its key and state
(`C on`), then to its state word alone, and the last step cuts words with an ellipsis. The
legend (`l`) lists every key. With eight switches the action keys are already
gone at 150 columns. That is the trade-off for having the switches in the footer.

## Speed

The render was cheap in CPU terms before the speed work (0.69 ms for a braille frame of one
board at 150 x 42, 1.47 ms for four boards, release build). The cost that mattered for Konsole
and VTE was terminal traffic: about 350 changed cells a frame, drawn up to 60 times a second.
The work was: cache the sizing results, reuse pixel buffers, avoid allocating per cell,
redraw at most 20 times a second at rest and 30 in a transition, and step the tile wave's
clock 12 times a second. Measured on a Ryzen 7 9700X in a release build, 150 x 42 braille:
changed cells per frame fell from 347 to 157 for one board and from 358 to 174 for four
boards; at 60 frames a second the terminal received 119 kB/s (419 kB/s before) for one
board and 168 kB/s (526 kB/s before) for four. A frame cost 0.27 ms mean (0.53 ms at the
95th percentile) for one board and 0.67 ms (1.33 ms) for four. The box is shared, so single
runs are noisy. The 12 Hz wave step has been judged in recordings only. Nobody has looked at it by eye in
Konsole or VTE. If it looks choppy, `WAVE_STEPS_PER_SEC` is the constant to raise (20 was
the fallback). With the clouds and the panel on, changed cells per frame are held to at most
0.70 of the first version's count by tests.

## Size

The view's own area needs 100 x 30 cells. In the TUI the global status bar takes the bottom
row, so the smallest terminal that shows the scene is 100 x 31. The history panel needs a
terminal of 120 x 37. 150 x 42 or larger looks best, and the size hint names the numbers a
user can act on. The first design said "reference minimum 100 x 30 cells" for the terminal;
that was off by the status bar's row.

## Audience and recordings

The people who will see this use Konsole and GNOME Terminal. Fonts there make braille dots
smaller than in the mockups, so half-block is kept. Two lessons about recording
it: tmux makes the view look cheaper than it is (its status bar and its colour downgrade), so
recordings were made without tmux, and VHS gave stale frames. A pty harness that feeds
asciinema, converted with agg, worked.

## Hardware facts the builder needs

All measured on 2026-10-29 on a Blackhole p300c (2 boards, 4 chips), tt-kmd 2.9,
under gozer leases.

**PCIe counters (done).** One word is one 64-byte NoC flit. `slv_rd_data_word_sent` is
data into the chip and `slv_*_wr_data_word_received` is data out of it. The `mst_*`
directions are inferred and were not measured. Details in `docs/SYSFS_BACKEND.md` and the
v0.13.18 changelog.

**Per-tile NIU counters.**
- Base: `NIU_BASE` is `0xFFB2_0000` (NoC0) and `0xFFB3_0000` (NoC1). The counter
  array is at `NIU_BASE + 0x200`, 62 entries. Index names are in
  tt-isa-documentation `BlackholeA0/NoC/Counters.md`.
- **Read each counter as its own single word** (`word_count=1`). A 62-word burst
  returns one register's value repeated across chunks of consecutive indices (8-15,
  48-55, 56-61), so posted equals non-posted and index 51 repeats index 48. On
  exalens 0.4.2 the burst disagreed with single-word reads on 58 of 92 moving counters.
  bhtop's poller uses the burst, so by its code (not run) its per-tile numbers are
  affected.
- The six counters that make bandwidth: 3 (read data received), 8 and 9 (write data
  sent, non-posted and posted), 51 (read data sent), 56 and 57 (write data received).
- Sample every DRAM endpoint (`d0,0` to `d7,2`) on both NoCs. Under the test load
  `d?,1` served reads on noc1 and `d?,2` on noc0, and `d?,0` was idle. Three
  endpoints each served about 89 million flits per second (about 5.7 GB/s).
- Cross-check that passed: flits read by Tensix cores (1.456 billion over 1 s) against
  flits served by DRAM plus Tensix memory (1.431 billion), within 2%.
- Read only Tensix, DRAM and Ethernet tiles. Reading NIU registers on ARC, PCIe,
  L2CPU or security tiles can wedge NoC0, which only `tt-smi -r` clears (bhtop's
  note; not re-tested).
- Tile positions: exalens gives each tile a `die` and a `noc0` coordinate
  (`loc.to("die")`, `loc.to("noc0")`). The NoC is a folded torus: die index to noc0 index
  goes 0, 1, N-1, 2, N-2 and so on, so one logical hop spans about two cells of
  silicon. `bhtop/geometry.py` documents the fold.
- A second process can read tile counters while a workload runs. Polling 156 tiles x
  2 NoCs x 6 counters (1,872 single-word reads per sweep) took 6.7 ms per sweep at
  10 Hz and 6.2 ms back to back (161 sweeps per second, 3.0 million reads, 0 errors).
  The workload's throughput was 5440, 5440, 5409 and 5424 iterations per second across
  baseline, 10 Hz, back-to-back and baseline again. The test used one synthetic matmul
  loop on one chip for 60 s. It did not use a real model server.
- The monitor's own reads show up as traffic on the tile read (about 96 flits per
  second at idle).

**Versions.** The shared venv `~/.tenstorrent-venv` has tt-exalens 0.3.30 and tt-umd
0.9.12. The latest exalens is 0.4.2 (needs tt-umd 0.9.14). `tt-smi 6.7.0` pins
`tt-umd==0.9.12`, so upgrading in place would break that pin. 0.4.2 is installed in its
own venv, `~/.venvs/tt-exalens-latest`, and the tests above passed on it.

## Decisions and who made them

Taylor decided: braille for the terminal; board frame (C) and die (A) as the two
layers; both a one-board scene and an every-board super scene; derive from hardware wherever possible; per-tile counters are in scope as an
opt-in; warn then allow when a workload is running; camera cuts on signals, with a
chaotic transition allowed; fix the PCIe counters first (done). During the build Taylor,
as product owner, asked for: more Grayskull colour (purples, pinks, greys), transparency, a
black background, braille and half-block both switchable ("we might have a few switches for
this view"), shuffled transitions "with an attempt at an animated rotate", a caption that
says which side the camera looks at, a faster view, clouds moving behind, and a small panel
of the monitored data over time.

The author's calls, each reversible: the camera presets, triggers and cooldown above;
Blackhole first; the 3D view is an extra mode and not a replacement; the exalens bridge
(next section). Added after the builder check:

- **No gozer lease in the product.** tt-toplike ships to people who may not run gozer,
  so tier 1 does not take leases. When gozer is present, tier 1 reads its status
  (read-only) and names the holder in the warning. Agents that develop or test tier 1
  still lease, under the house rule. An "observer" lease kind in `tt-gozer` is out of
  scope.
- **Tier 1 is local-only in the first version.** Remote viewers (`--remote`, `--serve`)
  get the tier 0 view wherever the data they need already arrives in the frames. A
  per-tile frame extension is a later project.
- **Tier 1 is experimental and off by default**, labelled the way `--remote` and
  `--serve` are. A startup check compares the installed exalens with the tested 0.4.2
  and warns below it. Exalens 0.3.30 was not tested for this. The re-test against a
  real model server is a step in the plan, done before the feature is documented.
- **Polling cost.** Tier 1 polls only the chips drawn in the current scene. A board scene
  polls at 5 Hz by default and 10 Hz at most. The super scene polls every chip at 2 Hz by
  default and 5 Hz at most. The measured cost was 6.7 ms per sweep for one chip, so four
  chips are about 27 ms per sweep.
- **Super scene detail.** With four dies in one 150 x 42 cell view, each die gets about a
  quarter of the area, so individual tiles may be too small to draw. In the super scene
  the author's suggestion is to show each die's tiles at reduced detail (rows or blocks
  of tiles lit by their summed traffic) and to keep full tile detail for the board scene.
  The builder decides the exact cutoff by looking at the render.
- **Scene changes on signals.** A camera cut can also change scene: from the super scene
  to the board scene of the busiest chip, and back after the activity ends. The cooldown
  applies to scene changes as well as angle changes.
- **Keys.** `3` enters the view and `3` or Esc leaves it. It is an entry-key-only mode,
  left out of the `v` rotation. Inside: `m` toggles the board scene and the all-boards
  scene, `[` and `]` switch boards, `c` cuts the camera now, `C` turns automatic cuts on or
  off, `B` braille or half-block, `T` transparent or opaque, `X` the transition mode, `W`
  clouds, `H` history panel, `L` labels.

## Left to the builder

- **How Rust gets exalens data.** Exalens is a Python library. The author's suggestion
  is a small sidecar process that prints one JSON line per sweep, started only when
  tier 1 is enabled and run from a configured venv. Alternatives (UMD bindings, a
  native reader) are the builder's to weigh.
- The tile layout table. Needs the die and noc0 coordinates per tile for each
  Blackhole variant, and the harvested rows and columns, which differ per chip. Existing
  code already tracks harvesting (`tensix_col_harvested`).
- The exact key to enter the view, the key to cut the camera by hand, and the setting
  names. Decided: see "Keys" above.
- Minimum terminal size for the 3D view and what to show below it. Decided: see "Size".
- Where the code lives (a new module under `src/animation/` and a panel under
  `src/ui/tui/` are the natural places), and how it reuses `topology.rs`.
- How "something is running on the chip" is detected for the warning. The inference
  monitor, the process scan and gozer state already hold parts of this.

## Open items

- `mst_*` PCIe directions need one calibration through a BAR write and read.
- Posted and non-posted split is only meaningful with single-word reads. Confirm the
  two stay separate under a workload that uses posted and non-posted writes in known
  amounts.
- Sustained polling was tested against one synthetic load. The re-test against a real
  model server is a planned step (see Decisions) and comes before tier 1 is documented.
- The DRAM endpoint to GDDR channel mapping, for drawing GDDR activity from tile
  counters.

## What changed from the first design

- The palette is Grayskull (indigo grey, lilac, hot pink, cyan). The first design used teal.
- The background is the terminal's own (transparent) by default, with an opaque pure black
  option, `T`. The first design had a tinted slab and vignette.
- Half-block is a switchable peer of braille. The first design made it a fallback for fonts without braille.
- The minimum is 100 x 30 for the view and 100 x 31 for the terminal. The history panel
  needs 120 x 37.
- Six camera presets, as the first design guessed (4 to 6), without the close view.
- Activity is relative to each chip's own idle. The first form, power over the limit,
  could not fire. The tile wave has a floor so idle dies stay visible.
- PCIe burst threshold is 50 MB/s (re-arm 10 MB/s), measured, where the first guess was 2 GB/s.
- Transitions are a shuffle of chaos, dissolve and spin. The first design had one chaos transition.
- The caption names the nearest corner, and each die has a label.
- New: eight switches in a footer built from one table, clouds, the history panel and a speed
  pass.
- Tier 1 is deferred to its own plan. Camera triggers for a reset, a model load and a GDDR ECC
  event, the close-view preset and the automatic half-block switch at wide terminals are not
  built.

## Known limits

- It is an isometric illusion; there is no 3D scene behind it.
- Braille dot size depends on the terminal font.
- Blackhole only. Other chips are named in the footer.
- The glow is an estimate. The clouds are decoration.
- With eight switches the footer's action keys drop at 150 columns.
- The 12 Hz wave step has been judged in recordings only.
- The spin is an attempt and can show gaps or overlaps mid-spin.
- The die layout table was measured on two Blackhole chips of one p300c. The filled Tensix
  cells in a column with none measured, and the Ethernet cage count (13 measured, the die
  probably has 14), come from the other columns and not from a measurement.
