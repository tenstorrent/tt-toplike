# Die 3D at small sizes, and an Insights inset: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Die 3D view draws a useful picture below 100 x 30, and a small live scene sits beside the process table on the Insights screen.

**Architecture:** A pure function maps the view's area to a `Tier` (footer rows, clouds, flags). `Die3dView::draw` uses it in place of the single 100 x 30 gate, and falls back to a text summary when no scene fits. The inset is a second `Die3dView` built with an inset layout (one caption row instead of the footer, clouds, history and flags off, scene fixed to "all boards that fit, else the busiest board"). The host loop owns it and `render_insights` splits the process panel's area to draw it.

**Tech Stack:** Rust, ratatui 0.30, the `die3d` module (`mod.rs`, `scene.rs`, `super_scene.rs`, `switching.rs`) and `src/ui/tui/mod.rs`.

**Spec:** `docs/superpowers/specs/2026-10-30-die-3d-small-sizes-design.md`

## Global Constraints

- Frames at 100 x 30 and above stay byte-identical to v0.13.21. The pinned hashes in `perf_tests.rs` (`GOLDEN_*`, `OUTSIDE_PANEL_*`) must not be repinned by this work.
- No new device reads. The inset uses `Die3dView::update(backend, now)` as the full view does.
- Never draw right-hand border characters.
- The process table keeps at least 70 columns when the inset is shown. The inset is 42 columns wide.
- Version 0.13.22, with CHANGELOG, `debian/changelog`, README and AGENTS.md entries in plain English.
- Tests use the `Fake` backend (`die3d/test_backend.rs`). Hardware is not touched.
- CI-form checks stay green: `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings`, `cargo fmt --all -- --check`, `cargo test --locked --lib --features tui`.
- A new test is run red against a deliberate break before it is trusted (reintroduce, watch it fail, restore).

## Review Focus

Inputs the spec implies and no task's happy path exercises, most likely first:

1. An area of 0 columns or 0 rows, or fewer rows than the footer. Nothing may subtract below zero or panic.
2. A drag-resize back and forth across a threshold: the inset keeps its camera and load calibration, and no stale cached fit or frame is drawn at the new size.
3. No Blackhole chip (only other architectures, or no devices): the inset is hidden and the process panel keeps the whole area.
4. The board count shrinks while the inset shows board `n`: the index is clamped, as the full view does.
5. 32 or more devices (fleet heat map) and a zoomed slice: the card area height differs, so the inset rect must come from the same helper the renderer uses.

## File Structure

- Create `src/animation/die3d/tiers.rs`: `Tier`, `tier_for`, thresholds.
- Create `src/animation/die3d/summary.rs`: the text summary lines.
- Create `src/animation/die3d/busiest.rs`: the busiest-board tracker with hold.
- Modify `src/animation/die3d/mod.rs`: use tiers, the inset layout, constructor and caption.
- Modify `src/animation/die3d/perf_tests.rs`: sweep, inset budget (new tests only; no repins).
- Modify `src/ui/tui/mod.rs`: layout helpers, `render_insights`, host loop.

---

### Task 1: Tiers

**Files:** Create `src/animation/die3d/tiers.rs`; modify `src/animation/die3d/mod.rs` (add `mod tiers;`).

**Interfaces:**
- Produces:
  - `pub(super) struct Tier { pub footer_rows: usize, pub clouds: bool, pub flags: bool }`
  - `pub(super) fn tier_for(cols: usize, rows: usize) -> Tier`
  - `pub(super) const FOOTER_TWO: (usize, usize) = (100, 30)`, `FOOTER_ONE: (usize, usize) = (60, 16)`, `FLAGS_MIN: (usize, usize) = (60, 16)`, `CLOUDS_MIN: (usize, usize) = (100, 30)`.

- [ ] **Step 1: Write failing tests** in `tiers.rs`:
  - `at_100_by_30_and_up_everything_is_on`: `tier_for(100, 30)` and `tier_for(200, 60)` give `Tier { footer_rows: 2, clouds: true, flags: true }`.
  - `below_100_by_30_clouds_go_and_the_footer_is_one_row`: `tier_for(99, 40)` and `tier_for(120, 29)` give `footer_rows 1`, `clouds false`, `flags true`.
  - `below_60_by_16_flags_and_footer_go`: `tier_for(59, 40)` and `tier_for(80, 15)` give `footer_rows 0`, `flags false`.
  - `tier_is_monotonic`: for every (cols, rows) in 0..=200 x 0..=60, growing either dimension never turns a feature off or lowers `footer_rows`.
- [ ] **Step 2:** Run `cargo test --locked --lib --features tui tiers` and confirm the tests fail to compile or fail.
- [ ] **Step 3:** Implement `tier_for` from the constants (both dimensions must reach a threshold).
- [ ] **Step 4:** Run the tests; they pass. Change one constant by one, watch `at_100_by_30...` or the matching test fail, restore.
- [ ] **Step 5:** Commit `feat(die3d): size tiers for the shrinking view`.

### Task 2: The full view uses the tiers

**Files:** Modify `src/animation/die3d/mod.rs`.

**Interfaces:**
- Consumes: `tier_for` (Task 1), `fit_pixels` (no minimum), `FOOTER_ROWS` (the two-row footer constant, used in `draw`, `scene_with` and tests).
- Produces: `Die3dView::tier(&self) -> Tier` (private), and `footer` returning `Vec<Line<'static>>` of `tier.footer_rows` lines.

- [ ] **Step 1: Write failing tests** in `mod.rs` tests:
  - `below_100_by_30_a_scene_is_drawn`: a view at 80 x 24 (one `p150`, `Fake` backend, `update` once) renders lines whose scene rows contain a braille glyph, not the "needs a terminal" hint.
  - `clouds_are_off_below_100_by_30`: with `show_clouds` on, the frame at 99 x 40 equals the frame with `ToggleClouds` pressed.
  - `the_footer_is_one_row_then_none`: line counts equal `rows` at 99 x 40 (last line is the status line), 59 x 40 (last line is scene).
  - `flags_are_off_below_60_by_16`: at 59 x 40 no cell holds `●`.
- [ ] **Step 2:** Run them; they fail.
- [ ] **Step 3:** In `draw`, replace the `MIN_CELLS` gate with the tier and the scene-fit result. `scene_rows` becomes `rows - tier.footer_rows` (checked). `scene_with` takes its row count the same way. Clouds draw only when `show_clouds && tier.clouds`. Flags draw only when `show_labels && tier.flags`. The footer builds `tier.footer_rows` lines: two as today, one is the status line only (scene name and estimate label), none is empty. `MIN_CELLS` stays exported as the size where the full layout starts.
- [ ] **Step 4:** Run `cargo test --locked --lib --features tui die3d`. All existing tests pass unchanged, including every golden hash. Break check: make the tier threshold 100 x 31 and watch a golden hash fail; restore.
- [ ] **Step 5:** Commit `feat(die3d): draw below 100 x 30 with the tiers`.

### Task 3: Text summary

**Files:** Create `src/animation/die3d/summary.rs`; modify `mod.rs`.

**Interfaces:**
- Produces: `pub(super) fn summary_lines(chips: &[Vec<ChipInputs>], undrawn: &[String], cols: usize, rows: usize) -> Vec<Line<'static>>`. One line per chip, `BH0 44W  in 2.0G  out 1.0M` (a missing value shows `-`), cut to `cols`. If chips do not fit in `rows`, the last row is `+N more`. With `rows == 0` it returns an empty vector.
- Consumes: `chip_label`, `ChipInputs`.

- [ ] **Step 1: Write failing tests** in `summary.rs`: one line per chip; a missing rate shows `-`; 5 chips in 3 rows gives 2 chips and `+3 more`; each line's width is at most `cols` (sweep cols 0..=60); `rows 0` and `cols 0` return empty and do not panic.
- [ ] **Step 2:** Run them; they fail. **Step 3:** Implement. **Step 4:** Run; pass.
- [ ] **Step 5: Wire** into `draw`: when no scene fits (the `None` branch that showed `no_fit_hint`), show the summary over the area. The old hint remains only when there are no chips. Test `a_tiny_view_shows_the_summary`: 30 x 10 view, two chips: lines contain `BH0` and `BH1`, no panic.
- [ ] **Step 6:** Commit `feat(die3d): a text summary where no scene fits`.

### Task 4: Size sweep

**Files:** Modify `src/animation/die3d/perf_tests.rs`.

- [ ] **Step 1: Write the tests** (these should pass once Tasks 1 to 3 are in; each is then run red against a deliberate break):
  - `every_size_draws_without_overflow`: for cols 1..=200 step 1 near the thresholds (every value from 1 to 130, then every 10) and rows likewise to 60, both cell modes, boards `["p150"]` and `["p300"; 8]`, camera cuts 0..6: `render` returns at most `rows` lines and no line wider than `cols` cells, and does not panic.
  - `flags_never_cover_a_drawn_cell_at_any_size`: the existing property test's comparison (flags on against off), over sizes 40 x 12 up to 150 x 42.
  - `sizes_at_and_above_100_by_30_use_the_full_layout`: a spot check of the footer line count (2) at 100 x 30, 120 x 36 and 150 x 42.
- [ ] **Step 2:** Break check: make `summary_lines` return one line too many, watch the first test fail; restore.
- [ ] **Step 3:** Commit `test(die3d): size sweep`.

### Task 5: Busiest board and the inset layout

**Files:** Create `src/animation/die3d/busiest.rs`; modify `mod.rs`.

**Interfaces:**
- Produces:
  - `pub(super) struct BusiestBoard` with `fn new() -> Self`, `fn observe(&mut self, now: f64, board_loads: &[f32])` and `fn board(&self) -> usize`. It switches to the board with the highest load only after that board has stayed highest for `BUSIEST_HOLD_SECS = 10.0`. Ties keep the current board. An empty slice keeps the current board. A current index past the end is clamped by the caller.
  - `pub fn Die3dView::inset(cols: usize, rows: usize) -> Die3dView`. `rows` includes one caption row. The inset has clouds, history and flags off, auto cuts on, and `inset: true`.
- Consumes: `Switcher`, `Director`, `TriggerDetector`, `scene_with`.

- [ ] **Step 1: Write failing tests**:
  - `busiest.rs`: a second board that stays higher for 9 s does not take over; at 10 s it does; a board that is higher for 6 s, lower, then higher again for 6 s does not; equal loads keep the current board; empty input keeps it.
  - `mod.rs`: `the_inset_draws_all_boards_when_they_fit`: an inset sized so `fit_super` fits (find it by search in the test) shows a frame equal to the super scene's pixels for the same size; `the_inset_shows_the_busiest_board_when_all_do_not_fit`: 8 boards at 42 x 14, board 3 busiest for over 10 s: the frame equals the board scene for board 3; `the_inset_never_changes_scene_on_a_trigger`: a `LoadRise` on another board changes the camera only; `the_inset_has_a_caption_row_and_no_footer`: the last line holds the chip name and `3 = full view`, and no line holds `board 1/`.
- [ ] **Step 2:** Run; they fail. **Step 3:** Implement. `scene_at` (a private helper) returns `Scene::All` for an inset and `switcher.scene_at(now)` otherwise; use it at every call site that read the switcher's scene (`frame`, `transition_scenes`, `render`'s cache key, `scene_now`). In an inset, the fallback board in `scene_with` is `BusiestBoard::board()` clamped. `update` feeds the tracker with each board's mean load and, for an inset, tells only the director about triggers, never the switcher. The caption shows the drawn board's (or boards') name and PCIe rates and `3 = full view`, cut to `cols`.
- [ ] **Step 4:** Run `cargo test --locked --lib --features tui die3d`; all pass with no golden hash changed. Break check: set `BUSIEST_HOLD_SECS` to 0 and watch the hold test fail; restore.
- [ ] **Step 5:** Commit `feat(die3d): an inset layout with a busiest-board fallback`.

### Task 6: Insights layout helpers and drawing

**Files:** Modify `src/ui/tui/mod.rs`.

**Interfaces:**
- Produces:
  - `fn insights_chunks(area: Rect, panel_devices: &[Device]) -> [Rect; 3]` (the existing header, cards and process-panel split, extracted unchanged).
  - `fn insights_inset_rect(area: Rect, panel_devices: &[Device]) -> Option<Rect>`: from the third chunk, minus `STATUS_BAR_ROWS` rows at the bottom. `Some` only when that area is at least 112 wide and 14 tall. The rect is the right 42 columns, full height.
  - `render_insights(..., inset: Option<&Die3dView>)`: the new parameter goes before the `#[cfg]` serving-metrics one. When `inset` is `Some` and `insights_inset_rect` gives a rect, the process panel renders in the area less that rect's width and the inset's lines render in the rect.
- Consumes: `device_panels_height`, the zoom slice logic at the top of `render_insights`, `Die3dView::render`.

- [ ] **Step 1: Write failing tests**:
  - `the_inset_rect_needs_112_by_14`: two chips at 150 x 42 gives `Some` with width 42 and the right edge at 150; four chips at 150 x 42 gives `None`; two chips at 111 wide gives `None`.
  - `the_process_table_keeps_70_columns`: at every width from 112 to 200 the process panel's rect is at least 70 wide when the inset is shown.
  - `the_rect_matches_the_renderer`: draw `render_insights` into a `TestBackend` with an inset and check the inset's glyphs lie inside the rect and the process panel's text does not.
  - `no_inset_view_means_the_old_layout`: with `None`, the buffer equals the buffer from the old code path (compare to a render of the same state before the change, saved as a pinned hash).
  - Fleet and zoom: 40 devices (fleet heat map) and a zoomed slice each give the same rect the renderer uses.
- [ ] **Step 2:** Run; they fail. **Step 3:** Implement. Update the bench call site (the `insights` timing near the end of the file) to pass `None`.
- [ ] **Step 4:** Run the lib tests; pass. Break check: change 112 to 100 and watch the width test fail; restore.
- [ ] **Step 5:** Commit `feat(tui): Insights can carry an inset beside the process table`.

### Task 7: Host loop

**Files:** Modify `src/ui/tui/mod.rs`.

**Interfaces:**
- Produces: a loop variable `insights_inset: Option<crate::animation::Die3dView>` and `insights_inset_started: Instant`; helper `fn resize_inset(inset: &mut Option<Die3dView>, rect: Option<Rect>)`.
- Consumes: `insights_inset_rect` (Task 6), `Die3dView::inset`, `Die3dView::resize`, `leaves_die3d`-style exit hook.

- [ ] **Step 1: Write failing tests** (pure helpers, in the style of the existing `resize_die3d` tests at the end of the file):
  - `the_inset_is_created_when_there_is_room_and_a_blackhole_chip`.
  - `a_resize_below_the_threshold_hides_it_and_keeps_the_view`: the `Option` stays `Some`, and the camera and load calibration are unchanged after a resize back above.
  - `leaving_insights_drops_the_inset`.
  - `no_blackhole_chip_means_no_inset`.
- [ ] **Step 2:** Run; they fail.
- [ ] **Step 3:** Implement. In the Insights update arm: compute the rect from `size` and the panel slice; if `Some` and the view is missing, create it (`Die3dView::inset(rect.width, rect.height)`); if `Some`, `resize` when the rect's size changed, then `update(backend, started.elapsed())`; if `None`, do not update. After the mode change check, drop the inset when leaving Insights. In the Insights draw arm, pass `insights_inset.as_ref()` only when the view has a scene to draw.
- [ ] **Step 4:** Run the lib tests and `cargo test --locked --test backend_isolation`; pass.
- [ ] **Step 5:** Run the mock backend by hand under a pty at 150 x 42, capture the Insights screen with the existing capture script (`scratchpad` helpers named in AGENTS.md), and read the frame for the inset's position and caption. Record what was seen in the commit body.
- [ ] **Step 6:** Commit `feat(tui): show the Die 3D inset on the Insights screen`.

### Task 8: Budget, docs and version

**Files:** Modify `src/animation/die3d/perf_tests.rs`, `Cargo.toml`, `Cargo.lock`, `QUICK_START.md`, `site/index.html`, `CHANGELOG.md`, `debian/changelog`, `README.md`, `AGENTS.md`.

- [ ] **Step 1: Write the tests.** `inset_changed_cells_stay_within_budget`: a one-board inset at 42 x 14, two seconds of frames; assert a ceiling set at 1.25 times the measured value (record the value in the comment). An ignored benchmark `inset_render_cost_report` prints mean ms per frame at 42 x 14 in release; the spec's budget is 0.3 ms.
- [ ] **Step 2:** Run the benchmark in release and write the measured numbers into the comment and the CHANGELOG entry. If the mean is over 0.3 ms, stop and report; do not raise the budget silently.
- [ ] **Step 3:** Bump to 0.13.22 in the five places (`Cargo.toml`, `Cargo.lock`, `QUICK_START.md`, `site/index.html`, `debian/changelog`) and write the CHANGELOG, README (sizes, the ladder, the inset) and AGENTS.md entries. Sweep the prose for `, not `, `rather than`, `instead of`, `reads as`, `honest`, `the one`, `which is why`, `This is what`.
- [ ] **Step 4:** Run `cargo fmt --all`, the CI-form clippy, and `cargo test --locked --lib --features tui`; all green, goldens unchanged (`git diff` shows no `GOLDEN_` or `OUTSIDE_PANEL_` line).
- [ ] **Step 5:** Commit `feat(die3d): small sizes and an Insights inset (0.13.22)`.
