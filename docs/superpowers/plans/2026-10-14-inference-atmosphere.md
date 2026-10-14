# Inference Atmosphere Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (the
> session executes natively). Every task is test-first: write the test, watch it
> fail for the right reason, write the minimum, watch it pass, mutation-check the
> guards, commit.
>
> This plan lists exact interfaces, the tests each task adds and the formulas for
> each layer. It does not paste full code: the code was written test-first
> straight into the source, which is the record of it.

**Goal:** A backdrop behind the Inference view that gives loading, thinking and
generating a visible feel, every effect tied to a real signal.

**Architecture:** `Atmosphere` observes the roster each update, keeps a few
smoothed gauges, and paints a backdrop canvas for the featured service's scene.
`composite` lays the backdrop under the existing renderer's lines, filling blank
cells only. `Snake` owns the atmosphere.

**Tech Stack:** Rust, ratatui, `train_sky` (aurora, star hash), `ServiceState`.

**Spec:** `docs/superpowers/specs/2026-10-14-inference-atmosphere-design.md`

## Global Constraints

- Deterministic (no randomness), bounded (45% lit cells, 6 comets, 12 horizon
  stars, 8 flares), foreground wins, nothing drawn under 4 by 3.
- No effect without its signal. No percentage or ETA for a load. Never show
  generated text.
- No change when no service is featured.
- `cargo fmt`, clippy `-D warnings`, `cargo test --locked --lib --features tui`;
  the Windows/macOS feature set too.
- Version goes to 0.13.11 with the Arcade work (Task 6).

## Review Focus

- A sentinel or negative kernel/shard delta (first tick after a service appears)
  must not grow the forge. Pinned in Task 5.
- A wide character in the foreground must not shift later columns. Pinned in Task 1.
- Zero-size and tiny areas: no panic, no output. Pinned in Task 1.
- Signals at their maximum: the backdrop stays under the density cap. Pinned in Task 3.
- A service that goes from Ready back to cold: the atmosphere switches off and
  the view is exactly as before. Pinned in Task 6.

## File Structure

- Create `src/animation/atmosphere.rs`.
- Modify `src/animation/mod.rs`, `src/animation/train_sky.rs` (visibility only),
  `src/animation/snake.rs`, `src/ui/tui/mod.rs` (Inference legend and explain),
  `AGENTS.md`, version files.

---

### Task 1: Canvas, composite, scene selection

**Interfaces:**
- `pub type Cell = Option<(char, Color)>; pub type Canvas = Vec<Vec<Cell>>;`
- `pub enum Scene { None, Forge, Storm, Aurora, Dream, Forming, Stalled }`
- `pub fn featured(rows: &[ServiceState]) -> Option<&ServiceState>`
- `pub fn scene_of(row: Option<&ServiceState>) -> Scene`
- `pub fn composite(canvas: &Canvas, lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>>`

**Tests:**
- `scene_of` for: no row; Down; Compiling and Loading (Forge); Alarm (Stalled);
  Ready with no stats (Dream); Ready with serving tps 40 (Aurora); Ready with
  running 2 and tps 0 (Storm); Ready with waiting 3 and tps 0 (Storm); Ready with
  media jobs_in_progress 1 (Forming); Ready with media jobs 0 (Dream).
- `featured` picks a loading service over a ready one, and the first ready one
  otherwise.
- `composite`: a foreground cell that is not a space is never replaced; a space is
  replaced by the backdrop cell with the backdrop colour; a wide character keeps
  its width so later columns line up; a short line is padded with backdrop cells up
  to `width`; a line with no backdrop on its row is returned unchanged;
  zero-width and empty inputs return the input.

### Task 2: Sky, aurora and tide

**Interfaces:** `Atmosphere::new()`, `Atmosphere::observe(&mut self, rows: &[ServiceState])`,
`Atmosphere::active(&self) -> bool`, `Atmosphere::render_backdrop(&self, w: usize, h: usize) -> Canvas`.
Layers: `sky` (stars plus aurora scaled by energy, hue shifted by KV), `tide`.

**Formulas:**
- Energy = smoothed `tps / peak`, where `peak = max(peak * 0.999, tps, 1.0)`; smoothing
  `e += (target - e) * 0.15` per observe.
- Aurora lit value from `train_sky::aurora` times `0.25 + 0.75 * energy`; glyph `▒` above
  0.62 else `░` above 0.14; hue `base + kv * 120`.
- Dream uses energy 0.15. Tide rows `round(kv * 3)`, glyph alternating `≈`/`~` by
  frame, hue 200 below kv 0.5, 40 below 0.8, else 5.

**Tests:** same inputs give identical backdrops (determinism); an idle Dream scene
lights some cells but fewer than an Aurora scene at tps peak; tide height tracks
`kv_cache_usage` (0.0 none, 0.95 three rows) and its hue changes across 0.5 and 0.8;
sizes from 0 by 0 to 160 by 50 do not panic and return `h` rows of `w` cells.

### Task 3: Token rain, comets, horizon stars, flares, static

**Formulas:**
- Falling glyphs: column `x` is a drop when `star_hash(x, 7) < 0.10 * prompt_n`;
  head at `((frame * (0.25 + h_x * 0.5) + h_x * h * 7) mod (h + 4)) - 2`, a 3-cell
  fading tail above it; dim cyan.
- Rising glyphs: drops when `star_hash(x, 13) < 0.12 * tps_n`, moving up, braille
  glyphs `⠁⠃⠇⡇`, gold.
- Comets: `n = min(running, 6)`; ellipse centre (w/2, h/2), radii (0.40 w, 0.35 h);
  angle `frame * 0.02 * (1 + 0.15 i) + i * TAU / n`; 5-cell trail `✦•∙··`; teal.
- Horizon stars: `min(waiting, 12)` dim `·` on row `0.8 h`, evenly spaced, slow twinkle.
- Flares: one per `completed_delta` (at most 3 per observe, at most 8 live), a
  ring `○ ◌ ·` expanding for 24 frames.
- Static: after `errored_delta > 0`, for 40 frames a row (chosen by
  `star_hash(frame / 2, 3)`) is filled `░` at density 0.3, dim red.

**Tests:** no falling or rising glyphs at zero prompt and generation rates; falling
density grows with `prompt_tps`; comet count equals `min(running, 6)`; horizon stars
equal `min(waiting, 12)`; flares spawn per completion, cap at 8, and expire after 24
frames; static appears after an error and clears; at maximum signals the lit
fraction stays at or under 0.45.

### Task 4: Storm

**Formulas:** top band rows `0 .. 0.35 h`. Cloud value
`0.5 + 0.5 * sin(x*0.07 + f*0.01) * 0.5 + sin(x*0.19 - f*0.017 + y) * 0.3`; lit when
above `1.0 - 0.6 * intensity`; glyph `░▒▓` by value; grey-blue. Intensity 1.0 with
requests running, 0.5 with only waiting. Heartbeat: three dots at the top centre,
one brighter per 20-frame step. Lightning every 90 frames for 3 frames when
intensity is 1.0, a zigzag of `╲╱` five rows long at `star_hash(f/90, 5) * w`.

**Tests:** Storm lights top rows and leaves the bottom third empty; intensity 0.5
lights fewer cells than 1.0; lightning appears at frame 90 and not at frame 40;
none of it appears in the Aurora scene.

### Task 5: Forge and Forming

**Formulas:**
- `built += max(kernel_delta, 0) + max(loaded_delta, 0)` per observe while the scene
  is Forge or Stalled; reset to 0 otherwise. Level `1 - exp(-built / 150)`.
- Beams every 7 columns from x = 3; height `level * (0.6 + 0.4 * star_hash(i, 2)) * 0.7 h`;
  `│`, with `┼` every 4th row; ember orange, brightness from `min(cpu_pct / 400, 1)`.
- Sparks: when `kernel_delta > 0`, a `✦` on top of beam `frame % beams`, every third frame.
- Floor glow: bottom row `▁▂▃▅` by `cpu_n` with a flicker. Stalled: dim red, no
  sparks, no growth.
- Forming: rectangle x in `w/3 .. 2w/3`, y in `h/4 .. 3h/4`; density
  `0.8 - 0.65 * tri(frame / 360)`; cells with `star_hash(x, y) < density` drawn `▒`,
  and as density falls only cells on a diamond pattern
  `(|dx| + |dy|) mod 4 == 0` remain; hue rotating slowly.

**Tests:** a negative or sentinel delta does not grow the forge; positive deltas do,
monotonically, and the level reaches but stays under 1.0; `built` resets when the
scene leaves Forge; sparks only when `kernel_delta > 0`; Stalled has no sparks and
no growth; Forming appears only with `jobs_in_progress > 0` and its rectangle stays
inside the canvas; every effect absent in the wrong scene.

### Task 6: Snake integration, docs, version

**Interfaces:** `Snake` gains `atmosphere: Atmosphere`; `update` calls `observe`;
`render` returns `composite(...)` when `active()`, else the foreground unchanged.
`pub(crate) fn render_foreground(&self, w, h) -> Vec<Line>` holds the old match.

**Tests:** with no rows, `render` equals `render_foreground` (atmosphere off); with a
Ready row with serving stats, the non-space foreground cells are all preserved and
at least one backdrop cell shows; going Ready then cold turns the atmosphere off
again; zero dims do not panic. Inference `l` and `!` text name the scenes (guard
test, with the tags tied to `Scene`). Real app check at 80 and 150 columns under
`--mock` and on this box.

**Docs and version:** `AGENTS.md` entry; `scripts/bump-version.sh 0.13.11`,
`debian/changelog`, `CHANGELOG.md`, `QUICK_START.md` date; full CI steps; push.
