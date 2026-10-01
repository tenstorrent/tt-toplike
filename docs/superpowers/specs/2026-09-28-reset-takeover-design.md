# Reset Takeover: full-screen `tt-smi -r` reaction animations

Date: 2026-09-28
Status: Draft, awaiting user review

## Problem / intent

The box is shared. When someone runs `tt-smi -r` (resetting one, several,
or all chips), tt-toplike currently shows nothing special — the telemetry
just glitches through the reset like any other transient. The user wants
the tool to *notice* and react: a brief, sometimes-spectacular, sometimes-
quiet full-screen "takeover" animation, with different flavor depending on
whether the reset targets all chips or a subset, and a distinct, non-
disruptive treatment when the user is already in the HivemindSweeper debug
view (whose whole purpose is showing real signals, not being interrupted).

This is strictly a reaction to *someone else's* `tt-smi -r` invocation
(another terminal, another engineer, a script) — tt-toplike never issues
resets itself.

Success looks like: running `tt-smi -r` on a box being watched by
`tt-toplike-tui --reset-takeover` produces a full-screen animation that
visibly tracks the real reset (not a fixed fake timer), is skippable with
any key, correctly distinguishes full vs. subset resets in its content,
and never fires when the feature is off (the default).

## Non-goals (explicit out of scope)

- Galaxy-tray resets (`-glx_reset` / `-glx_reset_auto`) — different CLI
  surface and semantics (whole-galaxy-host reset, not per-chip targets);
  a candidate follow-up, not built here.
- The egui GUI binary (`tt-toplike-egui`) — TUI only for this pass.
- Overlapping/queued resets — if a second reset is detected while a
  takeover is already active or pending, it is dropped. Rare in practice
  (resets are usually deliberate, spaced-out operator actions), and a
  queue adds real state-machine complexity for a case that's cheap to
  just ignore.
- Any GUI/config toggle to *disable mid-session* beyond the "any key
  skips the current animation" behavior — the feature is opt-in at
  launch (CLI flag / config), which is the only control surface asked for.

## Detection & lifecycle

### Where detection hooks in

The TUI's main loop (`src/ui/tui/mod.rs`) already re-scans host processes
every tick via `HostProcessMonitor` (`src/workload/host_processes.rs`,
backed by `sysinfo`) to feed the Insights process panel (`proc_rows`).
Reset detection reuses that same per-tick scan rather than adding a new
polling loop or thread — it needs the *full* process list, not the
top-N-by-resource-usage slice that `rows()` truncates to for display, so
it reads from the underlying `sysinfo::System` the monitor already
refreshed that tick.

### New module: `src/workload/reset_detect.rs`

```rust
pub struct ResetEvent {
    pub pid: i32,
    pub is_full: bool,
    pub chip_count: usize,
    pub targets: Vec<String>, // raw TARGETS tokens, for display/log-line text
}

pub struct ResetDetector {
    active: Option<ResetEvent>, // the one reset currently tracked, if any
}

impl ResetDetector {
    /// Called once per tick with the full process list and the backend's
    /// known device count. Returns `Some(event)` the tick a NEW reset is
    /// first observed (drives takeover start), and internally tracks
    /// liveness so callers can ask `is_finished()` on subsequent ticks.
    pub fn observe(&mut self, processes: &[(i32, String, String)], known_devices: usize) -> Option<&ResetEvent>;

    /// True the tick the tracked pid disappears (reset process exited).
    pub fn is_finished(&self, processes: &[(i32, String, String)]) -> bool;

    pub fn clear(&mut self);
}
```

Matching logic (pure, unit-testable):
- Candidate process: name or cmdline basename is `tt-smi`.
- Cmdline contains `-r` or `--reset` as a standalone token (not e.g. part
  of another flag's value).
- Everything after that flag, up to the next `-`-prefixed token or end of
  line, is the TARGETS list — per the real `tt-smi --help` grammar:
  `-r [TARGETS ...]`, targets are UMD logical IDs, PCI BDFs
  (`0000:0a:00.0`), or `/dev/tenstorrent/<id>`; omitted or literal `all`
  means every device.
- `is_full` = TARGETS is empty, or is exactly `["all"]`, **or** the parsed
  target count equals `known_devices` (an explicit full enumeration counts
  as full, per the original ask) — the device count comes from whatever
  backend is already active (`backend.device_count()`-equivalent), not a
  hardcoded number.
- Ignore any other `tt-smi` invocation (`-s`, `-ls`, `-f`, etc.) — only
  `-r`/`--reset` triggers anything.

### Lifecycle is honest, not a fixed timer

"In progress" means "the pid is still alive," checked every tick — never a
guessed fixed duration. "Done" fires the tick the pid disappears. This
directly drives the takeover animation's own pacing (Section: Animation
framework) rather than the animation free-running on its own clock.

### Optional kmsg enrichment

If `/dev/kmsg` is readable (same permission-dependent, silently-skip-if-
denied posture as `KmsgCollector`), a lightweight tail — *not* the full
Hivemind collector engine, just the existing `parse_kmsg_line` fed by a
minimal reader — supplies real tt-kmd reset-related log lines for the
duration of an active takeover, giving authentic per-chip log-line text.
When kmsg isn't available, animations fall back to generic, honestly-
generic per-chip beats (e.g. "resetting chip N…") paced by `chip_count`
and elapsed time — never asserting a specific chip completed when that
isn't actually known, per this project's "measured zero vs. no data"
discipline (AGENTS.md).

### Gating

All of the above — the extra process-list read, the kmsg tail, the
detector's per-tick `observe()` call — only runs when the feature is
enabled. With it off (the default), zero added work per tick.

## Animation framework

### New submodule: `src/animation/takeover/`

```
src/animation/takeover/
  mod.rs              // TakeoverAnimation trait, variant enum, weighted picker
  bbs.rs              // BBS sysop takeover
  blackhole_swarm.rs  // 1024-Blackholes swarm
  hatch_countdown.rs  // Lost-hatch countdown
  missile_command.rs  // Missile Command
  trek_reset.rs       // classic "Star Trek" BASIC-game reset screen
  quiet_notice.rs     // minimal full-screen status readout
```

Shared trait, mirroring the existing per-visualization convention
(`ArcadeVisualization`, `DefragVis`, etc.):

```rust
pub trait TakeoverAnimation {
    fn tick(&mut self, elapsed: Duration);
    fn render(&self, frame: &mut Frame, area: Rect);
    /// True once this animation's own choreography has played through at
    /// least one full cycle AND the real reset has finished. Until the
    /// real reset finishes, an animation may loop/hold its "in progress"
    /// beats rather than end early on a fake clock.
    fn is_done(&self) -> bool;
    /// Called the tick `ResetDetector::is_finished()` goes true — lets an
    /// in-progress animation transition to its short "done" resolution beat.
    fn note_reset_finished(&mut self);
    fn skip(&mut self);
}
```

Each variant is constructed from a `&ResetEvent` (so content reflects
`chip_count`/`is_full`/`targets` from the start) plus an optional live
kmsg-line feed handle.

### Variant pool

| Variant | Spectacle level | Content scoped by reset |
|---|---|---|
| BBS sysop takeover | high | per-chip log lines scroll BBS/ANSI-art style |
| 1024-Blackholes swarm | high | glyph-grid density scales with `chip_count` |
| Lost-hatch countdown | high | countdown paced by real elapsed/in-progress time |
| Missile Command | medium | crosshairs/explosions only at the actual affected chip(s) — a subset reset visibly fires at just those chips |
| Trek reset screen | medium | 1970s-BASIC-style "Star Trek" text screen (STARDATE header, sector/galaxy grid, sensor-scan readout) — one grid cell per targeted chip, `chip_count` sets sector-map size |
| Quiet notification | low | minimal dimmed full-screen status line, "the tool doing its thing" |

### Selection

Weighted random on every detected reset. `is_full` weights toward the four
spectacle variants; a subset reset weights toward Missile Command (scoped)
and the quiet notification. The real screen is tinted full-screen, and all
variants draw in one fixed 72x24 box centered on it, in the terminal's default
background. On a smaller terminal the box shrinks to keep at least 2 columns
and 1 row of margin. Scope changes *content*, never the box. (Updated
2026-10-01. This section originally said every variant renders full-screen.)

### Rendering integration

The main loop gains one new piece of state alongside existing per-mode
state (`defrag`, `snake`, etc.):

```rust
let mut takeover: Option<Box<dyn TakeoverAnimation>> = None;
let mut reset_detector = ResetDetector::default(); // only constructed if feature enabled
```

Each tick (when the feature is enabled): call `reset_detector.observe(...)`;
if it returns a new `ResetEvent` and `display_mode != HivemindSweeper` and
`takeover.is_none()`, pick a variant and populate `takeover`. If a
`takeover` is active: call `reset_detector.is_finished()` and forward to
`note_reset_finished()` on the transition; tick it; after rendering
whatever `display_mode` would have rendered anyway, render the takeover
over top as the unconditional last draw step.

Key handling while `takeover.is_some()` is an early-return branch at the
very top of the input match, before the normal key-dispatch logic runs:
any key calls `skip()` and consumes the event — it never also reaches
`q`/mode-switch/etc. handling in the same keypress, so skipping an
animation can never simultaneously quit the app or change `display_mode`
underneath it. Once `is_done()`, clear `takeover` to `None`; normal key
handling resumes on the next keypress.

Crucially, none of this touches `display_mode` or `prev_mode` — the
takeover is a pure overlay, so ending it always resumes exactly whatever
was already on screen, with no interaction with the existing
`HivemindSweeper`/`Training`/`InferenceMonitor` toggle-and-remember-
`prev_mode` machinery.

## HivemindSweeper special case

When `reset_detector.observe()` fires while `display_mode ==
HivemindSweeper`, no takeover is created. Instead the detected
`ResetEvent` is converted directly into a `SniffEvent` and injected into
the live `Hivemind` engine.

`Hivemind` already has a direct-injection path,
`push_for_test(&mut self, ev: SniffEvent)` (`src/workload/hivemind/mod.rs`).
This adds a real, non-test-only equivalent:

```rust
impl Hivemind {
    /// Inject an event from outside the collector-thread pipeline (e.g. the
    /// main loop's own reset detector). Goes through the same feed/grid
    /// update path as a collector-sourced event.
    pub fn inject(&mut self, ev: SniffEvent) { /* same body push_for_test uses */ }
}
```

Event shape: `Source::TtSmi` (already exists), a new `EventKind::Reset`
variant (`src/workload/hivemind/event.rs`) distinct from generic
`DriverMsg` so the feed pane and heat board can give it real visual
flourish — a bright flash on the affected chip's grid cell, a distinct
glyph/color in the feed row — rather than blending in as an ordinary log
line. `Severity::Warn`, consistent with how `parse_kmsg_line` already
classifies reset-related text.

This is the "special sizzle": a correctly-attributed real event in the
one view whose entire purpose is showing real signals, not an
interruption of it.

## Config / CLI

One new opt-in surface: `--reset-takeover` CLI flag (`src/cli.rs`) plus
the equivalent config key (`src/config.rs`), following existing
conventions for optional feature flags in this codebase. Off by default —
with it off, `ResetDetector` is never constructed and no per-tick scanning
or kmsg tailing happens.

## Testing approach

- `reset_detect.rs`: pure unit tests on the TARGETS-parsing/`is_full`
  logic against real `tt-smi -r` invocation shapes (no args, `all`,
  single UMD id, comma/space-separated ids, BDFs, device paths, and a
  full explicit enumeration matching `known_devices`) — no process
  spawning needed, feed fixture cmdline strings directly.
- `ResetDetector` lifecycle: fixture process lists across synthetic
  "ticks" (pid present → pid gone) to verify `is_finished()` transitions
  correctly and a second concurrent detection is dropped while one is
  active.
- Each `TakeoverAnimation` variant: `tick`/`is_done`/`skip` state-machine
  tests (no real rendering assertions needed beyond the project's existing
  "does not panic" render smoke tests, matching e.g.
  `render_snake_view_with_band_does_not_panic`).
- HivemindSweeper injection: a test constructing a `Hivemind`, calling
  `inject()` with a `Source::TtSmi`/`EventKind::Reset` event, and asserting
  it lands in `events()`/`grid()` exactly like a collector-sourced one.
- **Not hardware-verified at design time** — no live `tt-smi -r` run has
  been observed against this detector yet; per this project's established
  practice (see AGENTS.md Phase 27/28/33 entries), the implementation plan
  should include a real `tt-smi -r` run against a live box before this
  ships, and any discrepancy between the assumed CLI grammar and real
  behavior gets called out explicitly rather than assumed correct.
