# Arcade activity lanes

Date: 2026-10-14. Branch: `updates/reality`. Status: draft for review.

## Why

Arcade combines four visualizations and a hero, and it is the view people leave
on. Two things the tool can see have no presence in it:

- A **training run** (the Training view, `t`) saturates the chips for hours. Arcade
  looks the same as when the box is idle.
- An **inference server** (the Inference view, `i`) compiles, loads weights and
  serves. Arcade's only link is the `⚔` hero-versus-snake strip on Row 1, and it
  appears only while a vLLM service is *serving*. It does not appear while a
  model compiles or loads (the part with a goal and a clock), and it does not
  appear for diffusion or video servers.

When something with a goal or a clock is running, Arcade should show what it is,
how far along it is and how fast it is going, and the view should respond to it.

## Scope

In scope: Arcade (`DisplayMode::Arcade`) only.

Out of scope: other modes; a status-bar chip; new hotkeys (a lane shows the
existing `t` / `i` key as a hint); changes to the Training or Inference views;
a "finished" linger after a run ends.

## What the viewer sees

When no activity is running, Arcade renders exactly as it does today.

When one or more activities run, the top rows of the starfield region become
**lanes** and the starfield **responds**.

### Lanes

One lane per activity, at most two, one terminal row each, drawn over the top of
the starfield region. The starfield keeps the remaining rows. A lane has a left
edge and no right border (project rule: a right-hand glyph wraps or clips on a
narrow terminal).

Examples (widths are illustrative):

```
▎▶ TRAIN nano_gpt  step 4,120/10,000 ▕████░░░░▏ 41% · 12m left · 1.8 step/s · loss ↘   [t]
▎⏳ LOAD rag-gate-27b  :8100 · chips 0,1  loading 0:07                                  [i]
▎● SERVE Qwen3-32B  :8000 · chips 0,1  42 tok/s · 3 running                             [i]
```

Each lane carries its phase tag, a name, then fields in a fixed priority order so
a narrow terminal drops the least important first:

| Priority | Training | Inference |
|---|---|---|
| 1 | tag, name | tag, name |
| 2 | step / budget and `%` bar | placement (`:port · chips`) |
| 3 | ETA | elapsed (loading) or tok/s (serving) |
| 4 | step/s | requests running |
| 5 | loss direction (`↘ → ↗`) | |
| last | key hint `[t]` | key hint `[i]` |

A lane that cannot show its tag and name in the width it has is skipped.

Honesty rules, carried over from the Training and Inference views: no figure is
invented. A step counter that is chunk-local has no `%`, bar or ETA
(`TrainState::step_is_chunk_local`). A model load with no known progress shows
elapsed time and no percentage or ETA. A tok/s figure appears only from a real
`/metrics` scrape.

With three or more activities the two most urgent are shown and a `+N more` note
ends the second lane. Urgency order: stalled, loading or compiling, training,
serving.

A stalled service (`Phase::Alarm`) is drawn in the error colour.

### Starfield response

While any activity runs the stars take a tint by kind: warm for training, teal
for inference serving, slow violet for loading or compiling, red for stalled.
Each **pulse** (a finished training step, or a completed inference request) sends
a short flare through the starfield. This adds to the existing power-driven
behaviour of the stars; it does not replace it. The response is a pure function
of a frame counter and the activity data, so repeated renders of the same
instant match.

### Row 1 and the duel strip

Unchanged. The `⚔` strip stays on Row 1 and still follows
`Arcade::set_serving`.

## Components

### `src/animation/activity.rs` (new)

Pure data and pure functions. No I/O and no clock.

```rust
pub enum ActivityKind { Train, Load, Serve }

pub struct Activity {
    pub kind: ActivityKind,
    pub label: String,          // friendly name (see services::friendly_model_label)
    pub urgency: u8,            // for ordering and the +N more cut
    pub stalled: bool,
    pub progress: Option<f32>,  // 0.0..=1.0, only when it is real
    pub eta_secs: Option<f32>,  // only when it is real
    pub primary: Option<Rate>,  // step/s, tok/s
    pub detail: Vec<String>,    // pre-formatted fields in priority order
    pub elapsed_secs: Option<f32>,
    pub pulse_seq: u64,         // increments once per step or completed request
    pub key_hint: char,         // 't' or 'i'
}

pub fn from_train(state: &TrainState) -> Option<Activity>;
pub fn from_services(rows: &[ServiceState]) -> Vec<Activity>;
pub fn ordered(all: Vec<Activity>) -> Vec<Activity>; // urgency order, stable
```

`from_train` returns `None` unless a run is attached (`state.proc.is_some()`).
`pulse_seq` is `state.step_seq`. `from_services` maps `Phase::{Compiling,
Loading}` to `Load`, `Ready` to `Serve`, `Alarm` to a stalled `Load` or `Serve`,
and ignores `Down`. Its `pulse_seq` accumulates `serving.completed_delta` or the
media server's completed count.

### `src/animation/activity_lanes.rs` (new)

`pub fn render_lanes(activities: &[Activity], width: usize) -> Vec<Line<'static>>`
builds at most two rows from the ordered list, applying the field-dropping rules
above. Pure. Used by Arcade and covered by a width property test.

### `src/animation/arcade.rs`

- `ArcadeVisualization::set_activities(Vec<Activity>)`, called once per tick from
  the TUI like `set_serving`.
- `render()`: when the activity list is non-empty and the starfield region has at
  least six rows, the first `min(lanes, 2)` starfield rows are replaced by lane
  rows and the starfield renders into the rest. Otherwise unchanged.
- `update()`: passes the tint and any new pulse to the starfield.

### `src/animation/starfield.rs`

`HardwareStarfield::set_activity_tint(Option<ActivityTint>)` and
`HardwareStarfield::pulse()`. The tint is a hue offset blended into star colours
at render time. A pulse spawns a bounded number of flare rings that decay over a
fixed number of frames. Cost per frame is bounded (a cap on live flares).

### `src/ui/tui/mod.rs`

In the Arcade arm of the main loop, build the activity list and call
`arc.set_activities(...)`.

## Data sourcing

### Inference

`inference_monitor.snapshot()` already runs on its own thread and is already
read in the Arcade arm for `set_serving`. `from_services` consumes the same
snapshot. No new collection.

### Training

Today the `TrainMonitor` exists only while the Training view is open and rescans
`/proc` inline every two seconds. v0.13.8 moved the equivalent process scan off
the render thread because it cost about 135 ms per scan on a box with 548
processes. Arcade must not bring that back.

Constraint: Arcade adds **no `/proc` scan on the render thread**. A trainer is
noticed from the process snapshot the background scan already produces
(`host_proc_monitor.processes_snapshot()`), using the pure
`parse_train_process` and the Python-script check in `detect.rs`. The
`/proc/<pid>/maps` check that identifies a ttml-driven Python script is run only
for Python processes that already look like training scripts, and its result is
cached by pid. Once a trainer is found, a `TrainMonitor` is attached to it (log
tail only) and dropped when the process goes. How `TrainMonitor` takes a pid
instead of scanning for one is settled in the plan.

Success measure: the render thread spends under 0.1 ms per tick on this, measured
the way v0.13.8 measured the process scan.

### Other backends

- `--mock`: a synthetic run from `MockTrainRun`, as the Training view does, and
  the mock inference rows already used by the Inference view.
- `--remote`: lanes are suppressed, as the serving feed already is, because a
  local run beside remote silicon is incoherent.

## Edge cases

- Terminal too small for lanes (starfield under six rows): no lanes, no change.
- Narrow width: fields drop in the order above. Below the width of a tag and a
  name, the lane is skipped.
- Training step counter is chunk-local: no `%`, bar or ETA, the step number only.
- A trainer found but no readable log (stdout not redirected): the lane shows the
  name and `no per-step log`, no progress. The Training view says the same.
- An activity that disappears between ticks: its lane goes at once, no linger.
- Both a training run and a service active: two lanes. Three or more: two lanes
  and `+N more`.
- `pulse_seq` going backwards (a new run, or a restarted server) is treated as a
  reset, not as a burst of pulses.

## Testing

Each behaviour gets a test that was seen to fail first.

- **Builders.** `from_train` and `from_services` from fixed `TrainState` and
  `ServiceState` values: phase mapping, label, no ETA or `%` when not real,
  chunk-local steps, the pulse counter.
- **Lane width.** For widths 0 to 200 and every activity mix, no lane is wider
  than the width and none contains a right-border glyph (`│ ┐ ┘ ║` at the end of
  a row).
- **No-change guarantee.** With no activities no lane rows are reserved, no lane
  tag appears, and the render keeps its row count. This is a structural check:
  Arcade's render uses randomness (three runs of the same code produced three
  different frames), so a stored byte-for-byte frame cannot work.
- **Takeover.** With activities, the first starfield rows are lanes and the row
  count of the whole render is unchanged.
- **Pulse.** A `pulse_seq` increase spawns flares; the same value does not; a
  decrease is a reset.
- **Detection wiring.** A fake process snapshot containing a trainer attaches a
  monitor, and removing it drops it. A source check that the Arcade arm does not
  call the inline scan.
- **Render-thread cost.** The detection step on a 548-process fixture stays under
  the budget.
- **Docs.** Arcade's `l` and `!` text lists lanes, the tints and the key hints.
  A guard test ties the legend to the lane tags the renderer uses, as the Training
  legend test does for its glyphs.

## Release

User-facing, so the version goes to 0.13.11: `scripts/bump-version.sh 0.13.11`, a
`debian/changelog` entry, `CHANGELOG.md`, and the four-way version check.

## Risks and open points

- **`TrainMonitor` takes a pid.** It currently scans for its own process. Making
  it accept a pid from outside touches the Training view's path. The plan must
  keep the Training view's behaviour and tests unchanged.
- **Starfield rows.** The starfield region is 40% of the content height in the
  code (a doc comment still says 30%). Lanes take up to two rows from it. The
  six-row floor keeps the starfield readable on small terminals.
- **Tint strength.** Too strong a tint would hide the power-driven signal the
  stars carry today. The plan includes a screen check on real hardware and a
  cap on the blend.
- **Rotation.** `--rotate` gives Arcade a fixed dwell. Extending the dwell while
  an activity runs, as Training does, is out of scope here.
