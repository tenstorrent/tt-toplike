# Die 3D Tier 1 (Per-Tile Counters) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An experimental, opt-in data source that reads each Tensix, DRAM and Ethernet tile's NoC counters through tt-exalens and lights the Die 3D view from real per-tile traffic, with a warning when something is already running on the chip.

**Architecture:** A small Python sidecar owns the device read and prints one JSON line per chip per sweep, holding raw 32-bit counters. Rust spawns it, parses the lines, turns counter steps into per-tile rates (wrap-aware), and hands the view a per-tile heat map through `ChipInputs.tile_flow`. If the sidecar is missing, slow or dead, the view returns to the labelled estimate.

**Tech Stack:** Rust (std `process`, `mpsc`, serde_json already a dependency), Python 3 with tt-exalens 0.4.2 (`~/.venvs/tt-exalens-latest`).

**Spec:** `docs/superpowers/specs/2026-10-29-die-3d-view-design.md`. **Depends on:** `docs/superpowers/plans/2026-10-29-die-3d-view.md` (Die3dView, `ChipInputs`, `DieLayout` exist).

## Global Constraints

- Opt-in and experimental: nothing here starts without `--noc-tiles`. Local only; remote viewers do not get per-tile data.
- Read each counter as its own single word (`word_count=1`). Never a multi-word burst: on exalens 0.4.2 a 62-word burst disagreed with single-word reads on 58 of 92 moving counters.
- Counters: indices 3, 8, 9, 51, 56, 57 at `NIU_BASE + 0x200 + 4*i`; `NIU_BASE` is `0xFFB2_0000` (NoC0) and `0xFFB3_0000` (NoC1). One counter word is 64 bytes.
- Read only Tensix, DRAM and Ethernet tiles. The sidecar enforces this with an allowlist and refuses any other tile kind.
- Sample every DRAM endpoint (`d0,0` to `d7,2`) on both NoCs.
- Poll rate: a board scene polls at 5 Hz by default and 10 Hz at most; the super scene at 2 Hz by default and 5 Hz at most; only chips in the current scene are polled.
- Tested on tt-exalens 0.4.2 only. 0.3.30 is untested. The shared `~/.tenstorrent-venv` pins `tt-umd==0.9.12` for `tt-smi`, so do not upgrade it; use `~/.venvs/tt-exalens-latest`.
- The product takes no gozer lease. When `gozer` is on `PATH`, read `gozer status --json` (read-only) to name the holder in the warning. Developers and agents testing this plan still lease: `gozer acquire --chips 1 --who claude:tt-toplike --reason "..."`; release by lease id (`gozer history` shows it); retry if refused with "device still open".
- Warning rule: if something is running on the chip, show a warning that names it, then let the user continue. No hard block.
- Bump the version: this work is 0.13.20.
- Test commands: `cargo test --locked --lib --features tui`; `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings`.

## Review Focus

- Python or exalens missing, or exalens older than 0.4.2: a clear message, the view stays on the estimate, no crash.
- The sidecar dies or stalls mid-run: after 2 s without a frame the view returns to the labelled estimate; a restart is not attempted silently.
- A counter wrap or a device reset between sweeps: no spike in the drawn heat.
- The user answers no to the warning: no sidecar is spawned and nothing touches the device.
- A chip whose tile list is shorter than the layout (harvested): missing tiles draw flat and nothing indexes out of range.

## File Structure

- Create `scripts/noc_sidecar.py`: the device reader.
- Create `src/backend/noc_tiles.rs`: frame parsing, `TileRateTracker`, sidecar handle, busy detection and warning text.
- Modify `src/backend/pcie_counters.rs`: make `forward_delta` `pub(crate)`.
- Modify `src/backend/mod.rs` (module), `src/animation/die3d/scene.rs`, `src/animation/die3d/mod.rs`, `src/cli.rs`, `src/ui/tui/mod.rs`, docs and changelogs.

---

### Task 1: The sidecar

**Files:** Create `scripts/noc_sidecar.py`.

**Interfaces:** Produces a line protocol on stdout (one JSON object per line):
- `{"type":"hello","exalens":"0.4.2","chips":[{"chip":0,"bdf":"0000:03:00.0","tiles":156}]}` once at start.
- `{"type":"frame","chip":0,"t":<epoch seconds>,"tiles":[{"x":1,"y":2,"kind":"tensix","noc":[[c3,c8,c9,c51,c56,c57],[...]]}, ...]}` where `x,y` are the tile's noc0 coordinates and `noc[0]`, `noc[1]` are the raw u32 counters for NoC0 and NoC1.
- `{"type":"error","message":"..."}` on a failed read of a whole chip (the sidecar keeps running).
Arguments: `--chips 0,1` (exalens device indices), `--hz 5`. It exits cleanly on SIGTERM and on a closed stdout.

- [ ] **Step 1:** Write the script. Enumerate tiles with `dev.get_block_locations` for `functional_workers`, `dram`, `eth` only (a constant `ALLOWED_KINDS`; any other kind raises). Read with `read_words_from_device(loc, NIU[noc] + 0x200 + 4*i, word_count=1, noc_id=noc, context=ctx)`. Print `hello` with the exalens version from `importlib.metadata`. Report the PCI address for each chip so the Rust side can map exalens indices to toplike device indices (print what exalens exposes for it; if it exposes none, report that and stop).
- [ ] **Step 2: Verify on hardware.** Take a lease, start a ttnn matmul loop on the chip (`scratchpad` style workload, 30 s), run the sidecar at 5 Hz for 10 s. Expected: `hello` lists the chip and 156 tiles; frames arrive at about 5 per second; no tile of kind `arc`, `pcie`, `l2cpu` or `security` appears; busy tiles show counters moving; after SIGTERM `gozer status --json` shows `pids_holding` empty. Record the numbers in the commit message.
- [ ] **Step 3:** Commit `feat(noc): exalens sidecar for per-tile NIU counters`.

### Task 2: Frame parsing and rate tracker

**Files:** Create `src/backend/noc_tiles.rs`; modify `src/backend/mod.rs` (`pub mod noc_tiles;`) and `src/backend/pcie_counters.rs` (`pub(crate) fn forward_delta`).

**Interfaces:**
- Produces:
  - `pub const COUNTER_IDX: [usize; 6] = [3, 8, 9, 51, 56, 57];`
  - `pub enum SidecarMsg { Hello { exalens: String, chips: Vec<HelloChip> }, Frame(TileFrame), Error(String) }` and `pub fn parse_line(line: &str) -> Result<SidecarMsg, String>`.
  - `pub struct TileFrame { pub chip: usize, pub t: f64, pub tiles: Vec<TileSample> }`; `pub struct TileSample { pub noc0: (u8, u8), pub kind: String, pub words: [[u32; 6]; 2] }`.
  - `pub struct TileRateTracker` with `new()` and `sample(&mut self, frame: &TileFrame) -> HashMap<(u8, u8), [f64; 2]>`: bytes per second per tile per NoC, equal to `sum(forward_delta over the six counters) * 64 / dt`. The first frame for a chip returns an empty map. A step over half the counter range counts as a reset and contributes 0 (reuse `forward_delta`).
  - `pub struct AutoScale` with `new()`, `update(&mut self, max_bps: f64) -> f64`: a full-scale value that rises at once to a new maximum and decays 2% per call toward the current maximum, never below `1.0e9`.
  - `pub fn flow_heat(bps: [f64; 2], scale: f64) -> [f32; 2]`: each value `(bps / scale)` clamped to 0..1; NaN or infinity gives 0.

- [ ] **Step 1: Write failing tests:** `parse_line_reads_hello_frame_and_error`; `parse_line_rejects_garbage_with_a_message`; `first_frame_primes_and_returns_nothing`; `rate_is_flit_steps_times_64_over_dt` (two frames 1 s apart, +1000 on index 51 of NoC0 gives 64000.0); `a_wrap_is_a_forward_step`; `a_reset_reads_as_zero`; `chips_are_tracked_separately`; `autoscale_never_drops_below_one_gb_per_second`; `autoscale_rises_immediately_and_decays_slowly`; `flow_heat_clamps_and_zeroes_non_finite_values`.
- [ ] **Step 2:** Run `cargo test --lib noc_tiles`. Expected: FAIL. **Step 3:** Implement. **Step 4:** Run. Expected: PASS.
- [ ] **Step 5:** Commit `feat(noc): per-tile frame parsing and rate tracking`.

### Task 3: Sidecar handle

**Files:** Modify `src/backend/noc_tiles.rs`.

**Interfaces:**
- Produces: `pub struct NocTilesConfig { pub python: PathBuf, pub script: PathBuf, pub chips: Vec<usize>, pub hz: f32 }`; `pub struct NocTiles` with `spawn(cfg: &NocTilesConfig) -> Result<NocTiles, String>`; `poll(&mut self) -> Vec<SidecarMsg>` (drains the channel without blocking); `last_frame_age(&self) -> Option<Duration>`; `status(&self) -> NocStatus` where `pub enum NocStatus { Starting, Live, Stale, Dead(String), OldExalens(String) }`. `Drop` kills the child and waits for it. `Stale` means no frame for 2 s. `OldExalens` carries the version string when the `hello` version is below `TESTED_EXALENS = (0, 4, 2)`; frames are still used. The Python path defaults to `~/.venvs/tt-exalens-latest/bin/python`, then `python3`, and can be set with `TT_TOPLIKE_NOC_PYTHON`.

- [ ] **Step 1: Write failing tests** using a fake sidecar (a shell script in a temp dir that prints a hello and frames): `spawn_reports_starting_then_live_after_a_frame`; `a_silent_sidecar_becomes_stale_after_two_seconds` (use an injectable clock: `status_at(&self, now: Instant)`); `a_sidecar_that_exits_is_dead_with_its_stderr_tail`; `a_missing_python_is_an_error_naming_the_path`; `old_exalens_version_is_flagged_but_frames_still_flow`; `dropping_the_handle_kills_the_child`.
- [ ] **Step 2:** Run. Expected: FAIL. **Step 3:** Implement with a reader thread per child (stdout lines into `mpsc`, stderr tail kept). **Step 4:** Run. Expected: PASS.
- [ ] **Step 5:** Commit `feat(noc): sidecar process handle with staleness and exit detection`.

### Task 4: Busy detection and warning text

**Files:** Modify `src/backend/noc_tiles.rs`.

**Interfaces:**
- Produces: `pub struct BusyChip { pub chip: usize, pub who: String }`; `pub fn busy_chips(backend: &dyn TelemetryBackend, gozer_json: Option<&str>) -> Vec<BusyChip>` (a chip is busy when `backend.device_processes()` lists a process on it, or gozer shows a holder for its PCI address); `pub fn parse_gozer_status(json: &str) -> Vec<(String /*bdf*/, String /*who*/)>` (reads the `chips[]` entries whose `state` is `HELD` or `CLAIMED`, using `bdf` and `who`); `pub fn warning_text(busy: &[BusyChip]) -> Option<String>`.
- The warning text names each chip and its holder, says that reading tile counters while it runs may disturb it, and that the other software decides what happens next. `None` when nothing is busy.

- [ ] **Step 1: Write failing tests:** `parse_gozer_status_reads_held_and_claimed_chips` (use the real shape: `{"chips":[{"bdf":"0000:03:00.0","state":"CLAIMED","who":"claude:x","pids_holding":[]}, ...]}`); `free_chips_are_not_busy`; `a_process_on_the_chip_makes_it_busy`; `warning_text_names_the_holder`; `warning_text_is_none_when_nothing_is_busy`; `malformed_gozer_json_is_ignored`.
- [ ] **Step 2:** Run. Expected: FAIL. **Step 3:** Implement. **Step 4:** Run. Expected: PASS.
- [ ] **Step 5:** Commit `feat(noc): busy-chip detection and warning text`.

### Task 5: Wire tier 1 into the view

**Files:** Modify `src/cli.rs` (flag `--noc-tiles`, marked experimental in its help text, like `--remote`; update every struct literal of the args type in tests if the compiler requires it), `src/animation/die3d/scene.rs`, `src/animation/die3d/mod.rs`, `src/ui/tui/mod.rs`.

**Interfaces:**
- Consumes: Plan 1's `ChipInputs` (its `tile_flow: Option<HashMap<(u8, u8), [f32; 2]>>` slot, keyed by noc0 coordinates), `ESTIMATE_LABEL`, `DieLayout`; Tasks 2 to 4.
- Produces: `Die3dView::set_noc_tiles(&mut self, h: Option<NocTiles>)`; the footer shows `per-tile counters (experimental)` when `NocStatus::Live`, `ESTIMATE_LABEL` otherwise, and the reason text for `Dead`, `Stale` or `OldExalens`. GDDR block heat for channel `c` is the mean of the `tile_flow` values (larger NoC value per tile) of the DRAM endpoints `d{c},0..2` when per-tile data is live. The view fills each chip's `tile_flow` from `flow_heat` after each poll. The NoC wires between tiles brighten with the lower of the two neighbouring tiles' heat on that NoC. In the TUI: with `--noc-tiles`, entering the view computes `busy_chips`, and if `warning_text` is `Some` shows it as a modal (`y` continues, any other key cancels and leaves tier 0). Nothing is spawned before the answer. The polled chip set follows the current scene and the rates in Global Constraints.

- [ ] **Step 1: Write failing tests:** `live_tile_data_replaces_the_estimate_label`; `stale_data_returns_to_the_estimate_label`; `gddr_block_heat_is_the_mean_of_its_dram_endpoints`; `a_missing_tile_in_the_frame_draws_flat`; `answering_no_to_the_warning_spawns_nothing` (assert on a pure function `decide(answer: char, busy: &[BusyChip]) -> Decision`); `polled_chips_follow_the_scene_and_rate` (board scene: that board's chips at 5 Hz; super scene: all chips at 2 Hz); `dead_sidecar_shows_its_reason_and_keeps_rendering`.
- [ ] **Step 2:** Run. Expected: FAIL. **Step 3:** Implement. **Step 4:** Run `cargo test --locked --lib --features tui` and clippy. Expected: pass, clean.
- [ ] **Step 5: Run it for real.** Take a lease, start the matmul loop, run `cargo run --features tui --bin tt-toplike-tui -- --noc-tiles`, press `3`. Check by eye: the warning appears and names the loop, `y` continues, tiles and GDDR blocks light unevenly, the footer says per-tile counters. Report what you saw.
- [ ] **Step 6:** Commit `feat(die3d): per-tile counters as an experimental data source`.

### Task 6: Validate against a real server, document, release

**Files:** Modify `README.md`, `CHANGELOG.md`, `debian/changelog`, `Cargo.toml`, `QUICK_START.md`, `site/index.html`, `AGENTS.md`, `docs/superpowers/specs/2026-10-29-die-3d-view-design.md` (update Open items).

- [ ] **Step 1: Real-server re-test.** With a lease on a chip running a real model server (vLLM or tt-inference-server from this box), poll at 5 Hz for 5 minutes. Check: the server's own request latency and tokens per second against a 5-minute run without polling, the conservation cross-check (flits read by Tensix cores against flits served by DRAM plus Tensix memory, within 5%), and no errors in the sidecar. Record numbers in `AGENTS.md`. If latency rises more than 3%, stop and report before documenting the feature.
- [ ] **Step 2:** Write the README section (experimental, off by default, needs the exalens 0.4.2 venv, what the warning means, the poll rates). Run the plain-English sweep over new text.
- [ ] **Step 3:** Run the full CI commands. Expected: pass. Copy release binaries to `~/.local/bin`.
- [ ] **Step 4:** Commit `docs: tier 1 per-tile counters (experimental), v0.13.20`.
