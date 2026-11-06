# Die 3D at small sizes, and an inset on the Insights screen

Date: 2026-10-30. Status: built (v0.13.22 for the small sizes and the inset; v0.13.23 added
clipping, see "On shrinking"). Builds on `2026-10-29-die-3d-view-design.md` (the view shipped in
v0.13.19 to v0.13.21).

## Why

The Die 3D view refuses to draw below 100 x 30 cells. It prints one line asking for a bigger
terminal. People run tt-toplike in split panes and small windows, so for them the view does not
exist. The product owner also wants a small live scene on the main (Insights) screen, so the
board's activity is visible without leaving it. Both come from one capability: drawing the scene
well at whatever size it gets.

Measured facts that make this cheap (release build, one board, busy traffic):

| size (cells) | tile width | raster + braille ms per frame |
|---|---|---|
| 40 x 12 | 4 px | 0.11 |
| 60 x 16 | 4 px | 0.11 |
| 80 x 24 | 8 px | 0.21 |
| 100 x 28 | 10 px | 0.28 |
| 150 x 40 | 14 px | 0.47 |

The scene fits at 4 px tiles from 40 x 12 up and does not fit at 30 x 10. At 4 px the wave shows
as a moving band, and individual tiles are no longer distinguishable.

## Goals

1. **The full view shrinks.** Below 100 x 30 it draws the best picture that fits. Today it prints a
   refusal line. It drops things in a fixed order (the ladder below), and never draws text over the
   picture, never overflows its area and never panics at any size.
2. **An inset on the Insights screen** (the product owner chose this layout, "mock B"): a small
   live scene of one board in the process panel's area, to the right of the process table. The chip
   cards above it stay as they are.
3. **At 100 x 30 and above nothing changes.** Every frame there stays identical to v0.13.21, so
   the pinned frame hashes in `perf_tests.rs` stay as they are.

## Non-goals

- Replacing the chip portraits or changing the chip cards.
- A new data source. The inset reads what the view already reads. No extra device reads.
- Wormhole or Grayskull scenes. The inset does not appear without a Blackhole chip.
- Per-tile data (tier 1) and the height ideas for braille (face shading, edges, shadows). Those
  are separate pieces of work.

## Anti-goals (a failure even if it runs)

- The inset squeezing the process table until its columns truncate. The table wins any
  disagreement about space.
- A visible stutter on the Insights screen from the inset's drawing cost.
- Right-hand border characters in anything drawn.
- Text of the inset's own (labels, caption) over the drawn picture.

## The ladder (full view, in the order things go as the area shrinks)

Sizes are in terminal cells of the view's own area. They are starting points, set from the
numbers above, and the builder tunes them against the size sweep test.

1. 156 wide and up: all footer keys (as today).
2. 120 x 36 and up: history panel (as today).
3. 100 x 30 and up: the view as today.
4. Below 100 x 30: clouds off, history panel off, footer cut to one row, then to none below about
   60 x 16 (a one-line key hint may replace it). Flags stay for each die that has open space for
   one, as today.
5. Below about 60 x 16: flags off. The wave is the only per-chip signal.
6. The super scene falls back to the selected board when the super scene does not fit at 4 px
   tiles.
7. Below the size where one board fits at 4 px tiles (about 40 x 12 plus the footer rows the host
   keeps; 51 columns for a two-die board), the scene shrinks to a 2 px tile (0.13.23). If even
   that is too big, the whole scene is drawn at 2 px and the window shows its middle, with a note
   that says what size shows all of it. Below 16 x 6: a text summary, one line per chip (`BH0 44W  in 2.0G  out 1.0M`), in place of the
   current "does not fit" line. Rows that do not fit are cut, with a final `+N more`.

On shrinking, the scene is made smaller (down to the 4 px tile, and in 0.13.23 to 2 px for one
board). 0.13.23 also crops, as the last step before the text summary: it shows the middle of the
whole scene. Cropping is also used in step 6, which shows less of the system.

## The inset

- **Where.** The right part of the Insights process panel's area. It is shown only when the
  area is at least 122 columns wide (a 70-column table plus a 52-column inset) and 16 rows high.
  The width and height are the least that draw a two-die board (51 x 16 measured), plus one column.
  Otherwise it is hidden and the process panel has the whole area, as today.
- **What.** One board's scene, no footer, no clouds, no history, no flags. One caption row under
  the picture: the chip name and PCIe rates (`BH0 in 2.0G out 1.0M`) and the hint `3 = full view`.
  The wave follows traffic as in the full view.
- **Which boards.** As many as fit. With room for the super scene at its smallest tile size
  (see `super_scene::fit_super`), the inset shows every board. Otherwise it shows one board, the
  busiest chip's by current load, held for at least 10 s before it changes to another. The
  choice is made again on each resize. It is not tied to either Insights cursor, so scrolling the
  process table never changes the picture. No new keys. `3` opens the full view, as it does now.
  Showing a middle number of boards (the busiest two or three) is left to the builder if the
  super scene can draw a subset without new layout work.
- **Camera.** Automatic cuts as in the full view, on its own camera state. The inset is a
  second `Die3dView` with a compact layout (no footer rows, the switches above fixed off).
- **Rate.** Its own 20 frames a second cap, however often the host redraws. It reads the backend
  on the host's update tick, as the full view does.
- **Budget.** At 52 x 16 cells the inset costs 0.3 ms per frame or less in a release build, and the
  Insights screen's frame time rises by no more than that.
- **Plumbing.** The host loop owns a second `Option<Die3dView>`, beside the full view's. It is
  created the first time the inset has room, and updated on the host's tick in the Insights arm
  only while the inset has room. `render_insights` receives it as an optional `&Die3dView`.
  `Die3dView::render` already caches its last frame and redraws no more often than
  `STEADY_FRAME_SECS`, so the 20 frames a second cap needs no cache of its own.
- **Space.** The 122 x 16 threshold is the size of the process panel's area (the third chunk of
  the Insights layout). The inset takes the right 52 columns of that area.
  Its scene gets 15 rows and the caption 1. The process table keeps its own header and rows in
  the left part. The scene fits at 4 px tiles from 40 x 12.
- **Board changes.** The view is kept when the board changes: camera, loads and calibration stay.
  The inset redraws at once on a board change, with no cut.
- **Lifecycle.** A resize below the threshold hides the inset and stops its updates. The view is
  kept, so a drag-resize does not lose the camera or load calibration. The view is dropped on
  leaving Insights, and when no Blackhole chip is present.

## Constraints

- Rust, ratatui 0.30, the existing `Die3dView` and `scene.rs`. The change is mostly in
  `die3d/mod.rs` (layout, ladder), `scene.rs` (`fit_pixels` already has no minimum) and
  `src/ui/tui/mod.rs` (`render_insights`).
- Version bump with the change, and the usual docs (README, CHANGELOG, `debian/changelog`,
  AGENTS.md), written in plain English.
- Hardware is not touched by tests. Use the `Fake` backend.

## Tests that must exist

- A size sweep: every size from 1 x 1 to 200 x 60, both cell modes, one and four boards, all
  six camera cuts. No panic, no line longer than the width, no more lines than the height.
- The flags property test (flags never cover a drawn cell but their pin) extended over the sweep.
- The frames at 100 x 30 and above are byte-identical to v0.13.21 (the existing pins).
- Each ladder step turns on at its size and no sooner (one test per step, seen failing first).
- The inset: hidden when the area is too small, never narrows the process table below 70
  columns, shows the board of the chip under the cursor.
- The inset's frame cost stays under its budget (an ignored benchmark plus a changed-cells
  budget, as for the full view).

## Footer rows and the size gate

The thresholds in the ladder are on the view's own area, which excludes the rows the host keeps
(`reserved_rows`, the status bar). The view's own two footer rows are inside that area. The text
summary starts when the area cannot hold a one-board scene at 4 px tiles (about 40 x 12) plus
whatever footer rows the ladder step still keeps. `MIN_CELLS` stops being a single gate and becomes
the size at which the full layout starts. The pinned frames guard the full layout.

## Left to the builder

The exact thresholds of the ladder, the layout of the text summary, how the compact layout is
represented inside `Die3dView`, and the wording of the caption.

## Calls I made that need checking

- Shrink the scene to fit, and crop only in step 6. (Changed in 0.13.23: one board also shrinks to 2 px and then crops.)
- The text summary as the last step, where today there is a refusal line.
- The inset shows every board when they fit and the busiest chip's board (10 s hold) when not, and has no keys of its own.
- The 122 x 16 threshold for showing the inset.
- No flags in the inset, and none in the full view below about 60 x 16.
