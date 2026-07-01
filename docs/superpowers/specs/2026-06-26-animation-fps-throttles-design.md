# Animation FPS throttles (opt-in load + focus throttling)

**Status:** Design — awaiting review
**Branch:** `macos-cpu-mode`
**Date:** 2026-06-26

## Problem

The animated visualizations run at 60 FPS, which the `--bench` numbers show is
the dominant CPU cost (Memory Castle ~13% of a core, Memory Flow ~7%). On a
busy host — especially `--host` or a TT box whose host CPU feeds the
accelerator — a 60 FPS monitor can compete with the workload it's meant to
observe. We want ways to back off **without degrading the default experience**.

## Goal

Two **opt-in** throttles. The default animation experience is unchanged (full
60 FPS); throttling only happens when the user explicitly enables it.

- **Adaptive load throttle:** when enabled and the host is under heavy load,
  ease animated FPS down so the monitor yields CPU to the workload.
- **Focus-aware idle:** when enabled and the terminal loses focus, drop to a
  near-idle FPS.

Each is reachable via **both** a CLI flag and a slash command (1:1), per the
project's argument↔command parity principle. (Full parity across the *entire*
existing CLI surface is a separate spec; this feature only establishes 1:1 for
its own two toggles.)

## Decisions (from brainstorming)

- **Default animated FPS stays 60.** No default behavior change. Data screens
  stay at 10 FPS (unchanged).
- **Adaptive throttle:** off by default. When on, FPS = 30 while host CPU is
  high, 60 otherwise. **Hysteresis:** engage at ≥ 85% host CPU, disengage at
  < 80% (prevents flapping near the threshold). 30 was chosen as the
  still-enjoyable floor.
- **Focus-aware idle:** off by default. When on, FPS = 2 while the terminal is
  unfocused, restoring on focus.
- **Surfaces (1:1):** `--throttle` ↔ `/throttle`; `--idle-on-blur` ↔
  `/idle-on-blur`. CLI flag sets the initial state; the slash command toggles
  it live.
- Data-mode FPS (10) is untouched; these throttles only affect animated modes.

## Architecture

Reuses existing machinery: the run loop already holds a mutable
`ui_poll_rate_anim`, samples `host_cpu_pct` (system-wide CPU) every ~2 s, has an
`execute_command` slash-command dispatcher, and captures `FocusGained`/
`FocusLost` events. No new subsystems.

### 1. Pure FPS resolver (`src/ui/tui/throttle.rs`)
A small, fully-tested module holding the decision logic and the hysteresis state:
```text
struct ThrottleState { throttle_on: bool, idle_on_blur: bool, focused: bool, load_throttling: bool }
ThrottleState::update_load(&mut self, host_cpu_pct: f32)  // applies 85/80 hysteresis to load_throttling
fn effective_anim_fps(&self) -> u32                       // 2 if idle&unfocused; else 30 if load_throttling; else 60
fn anim_poll_interval(&self) -> Duration                  // 1000/fps ms
```
Priority order: focus-idle (lowest FPS) overrides load-throttle overrides base.
`BASE_FPS = 60`, `LOAD_FPS = 30`, `IDLE_FPS = 2`, `LOAD_HI = 85.0`, `LOAD_LO = 80.0` as named consts.

### 2. Run-loop wiring (`src/ui/tui/mod.rs`)
- Own a `ThrottleState`, initialized from the CLI flags.
- On `FocusGained`/`FocusLost`, set `focused`.
- Where `host_cpu_pct` is refreshed (~2 s), call `state.update_load(host_cpu_pct)`.
- Replace the fixed `ui_poll_rate_anim` for animated modes with
  `state.anim_poll_interval()` (recomputed each frame; cheap). Data modes keep 10 FPS.

### 3. Slash commands (`execute_command`)
- `/throttle` → toggles `state.throttle_on` (prints "load throttle: on/off").
- `/idle-on-blur` → toggles `state.idle_on_blur`.
Mirror exactly the CLI flags.

### 4. CLI flags (`src/cli.rs`)
- `--throttle: bool`, `--idle-on-blur: bool` (both default false). Add
  `bench: false`-style entries to any `Cli { .. }` test literals.

### 5. Status hint (`render_global_statusbar`)
When a throttle is actively reducing FPS, show a small dim indicator
(`⏷30` while load-throttling, `⏸2` while blur-idle) so the user knows why the
animation slowed. Nothing shown when running at full 60.

## Data flow
```
CLI flags → ThrottleState (throttle_on / idle_on_blur)
focus events → state.focused
~2s host CPU sample → state.update_load(pct)   (85/80 hysteresis → load_throttling)
each animated frame → render_interval = state.anim_poll_interval()
/throttle, /idle-on-blur → flip the flags live
```

## Error handling / cost
- When both throttles are off and focused (the default), `effective_anim_fps`
  returns 60 every time — zero behavior change, negligible cost (a couple of
  comparisons per frame).
- `host_cpu_pct` is already sampled; no new sampling. If unavailable it reads
  0.0 → never triggers load-throttle (fail-open to full FPS).

## Testing
- Unit-test `effective_anim_fps` across the matrix: focused/unfocused ×
  throttle_on/off × idle_on_blur on/off, asserting the priority order (blur-idle
  beats load-throttle beats base).
- Unit-test `update_load` hysteresis: rising through 85 engages, falling below
  80 disengages, and the 80–85 band holds the prior state (no flap).
- Existing suite stays green; new code cross-platform (uses the already
  cross-platform `host_cpu_pct`).

## Out of scope
- Full CLI↔slash-command parity across all existing args (separate spec, next).
- An arbitrary `--fps N` override (YAGNI; 60 default + 30 throttle covers the need).
- Data-screen FPS changes (already 10).
- GPU/accelerator-side throttling.

## Effort & risk
~1–2 days. Low risk: the only default-affecting change is none (60 stays 60);
the logic is a small pure module with the wiring reusing existing state. The
`--bench` tool lets us confirm the throttled FPS actually lands at the intended
cost.
