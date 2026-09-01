# Training View ("Robot Brain Food") — Design

**Status:** approved design, ready for implementation plan
**Branch:** `feat/train-visualization`
**Date:** 2026-08-29

## Goal

A new full-screen TUI view (`t`) that visualizes a **live tt-train run** — the
model rendered as a character-grid network being fed tokens, a loss-mountain
range against a twinkling aurora nightscape, and the chips doing the work —
in the spirit of Memory Castle / Starfield / Defrag: psychedelic, dense, and
every pixel driven by a real signal.

Crucially: **it attaches by itself.** No commands, no flags. Open the view and
it finds training already in progress.

## What tt-train actually emits (research, primary-source)

Read from the local clone at `/home/ttuser/code/tt-metal/tt-train`. tt-train is
**C++**, not Python — a compiled binary (`nano_gpt`, `mnist_mlp`, …), not a
script.

**Per-step, on stdout** (`sources/examples/nano_gpt/main.cpp`):

```
Step: {global_step}, Loss: {average_loss}
Full step time {duration} ms, cache entries: {num_program_cache_entries}
```

**At startup, on stdout:**

```
Max steps {N}
Batch size {N}
Gradient accumulation steps {N}
Scheduler type {name}
Number of parameters: {N}
```

**Config** is a YAML file passed with `-c`, referencing a second model-config
YAML: `batch_size`, `max_steps`, `learning_rate`, `model_save_interval`,
`num_heads`, `embedding_dim`, `num_blocks`, `vocab_size`,
`max_sequence_length`.

**Checkpoints:** `save_training_state(...)` writes to a *single rolling path*
from config (e.g. `transformer.msgpack`) every `model_save_interval` steps and
once at the end — overwritten in place, not versioned. An mtime bump is a
clean "just saved" event.

**Not available live** (important — the design must not fabricate these):
no gradient norms, no throughput/MFU counter, no Prometheus endpoint, no
TensorBoard. `scripts/tt_train_metrics.py` defines a rich final-summary JSON
(`mfu`, `tps`, per-category DRAM breakdown) but it is written **once at run
end**, not streamed. `TT_LOGGER_LEVEL` controls the framework's internal
logger, separate from the `fmt::print` lines above.

## Auto-attach (the key UX decision)

Three-stage discovery, all reusing machinery tt-toplike already has:

1. **Find the process.** Scan for a process holding `/dev/tenstorrent/*` whose
   binary *name* matches a tt-train example (`nano_gpt`, `mnist_mlp`, …).
   Match on binary name, **never on flags** — the flag sets demonstrably drift
   across tt-metal versions (`-c/-n/--multihost` in one checkout, `-i/-p/-s/-m`
   in another). Same procfs attribution HivemindSweeper's collector already
   does.

2. **Find its log.** `readlink /proc/<pid>/fd/1`. If it resolves to a **regular
   file**, that's the log — tail it from the current offset. Verified working:
   a process launched as `./nano_gpt … > train.log &` exposes the real path
   here, readable from outside knowing only the pid. This is what makes
   zero-command attachment possible, and it covers the common case since long
   training runs are almost always launched with stdout redirected.

3. **Degrade honestly.** If fd 1 is a pipe or tty, the loss stream is
   genuinely unavailable — retroactively tailing an un-redirected process is
   an OS limitation, not a design gap. The view then shows what it *can* still
   see (process alive, RSS, CPU, device fds, chip telemetry, checkpoint mtime)
   and says plainly that per-step metrics need stdout redirected to a file.
   It must not render an empty or fake loss curve.

The checkpoint path comes from the run's YAML config (discoverable from the
`-c` argument in the process cmdline); its mtime is polled the same way
HivemindSweeper's `cache_watch` polls compile-cache churn.

## The color language

The previous iteration's mistake was mapping *everything* to one hue. Nine
channels, each carrying a different real signal:

| Channel | Encodes | Source |
|---|---|---|
| Node/mountain hue: magenta 325° → teal 158° | **loss magnitude** | stdout |
| Amber (42°) sweep, left→right | **forward pass** | step cadence |
| Violet (268°) sweep, right→left | **backward pass / gradients** | step cadence |
| Per-column hue across the river | **run history** — each column keeps its own loss's hue, so the range is a magenta→teal timeline of the whole run | loss history |
| Mint ▼ / coral ▲ | **loss delta direction** (distinct from magnitude) | consecutive steps |
| Cyan→green→amber→red | **chip temperature** — the app's existing ramp | tt-toplike telemetry |
| Bar density `█▓▒░·` | **chip power draw** | tt-toplike telemetry |
| Violet shimmer → dim | **kernel cache compiling → steady** | stdout |
| Mint burst + comet | **checkpoint saved** | file mtime |

## Layout (134×40 reference; must reflow)

```
╔═[ TRAINING ]  nano_gpt · pid 48213 · 4 chips ═══        LOSS 1.8342  ▼0.0121
║ auto-attached  /proc/48213/fd/1 → run/shakespeare.log   step 2,431 / 50,000  4.9%
║
║  MODEL          tokens →   [ 6 blocks × 6 heads ]   ∇ gradients      LIVE
║  params 11.2M      ▪  ·    ◉──◇╲◉──●╱◇──○                          tok/s   58,204
║  blocks 6                  …the network…                            step/s  0.89
║  …                                                                  cache   21 steady
║  BATCH                                                              ELAPSED / ETA
║  size 64 …                                                          CHECKPOINT
║
║ LOSS  · mountains colored by their own value
║   ✦     ░░  ·      ✦        ← aurora + starfield in the negative space
║  ███▓▒░  ✦    ∙  ░░░
║  ██████▓▒░▂▁            ← mountains descend as the model converges
║  0.42 low                                          high 4.58
║
║ CHIPS  dev0 61.2°C 78W ███████▓░····   dev1 …
║
║ ● loss state  ─ forward  ∙ gradients  █ chip temp  ✦ checkpoint  ░ aurora
╚═[ t training  l legend  v cycle  ? help  q quit ]═
```

Left-and-bottom borders only (`╔ ║ ╚`) per the project's TUI convention — never
right-side borders, which wrap when the terminal is narrower than expected.

## The nightscape

The loss mountains descend as the model converges, so the sky **opens up on the
right** — the negative space is itself a progress signal. It's filled with:

- **Two counter-drifting aurora bands** (`░▒`), hue-cycling around green 152°
  and magenta 292°, low luminance (10–25%) so the mountains stay legible.
- **A deterministic twinkling starfield** — star positions from a stable
  spatial hash so they never jitter between frames; each twinkles at its own
  phase and rate through `· ∙ ✦`; a minority are vividly colored.
- **A comet** released across the sky on each checkpoint save, trailing
  `✦ ∙ ·`, clipped against the mountain skyline.

## Components

| Unit | Responsibility |
|---|---|
| `workload/train/detect.rs` | Pure: recognize a tt-train process from (binary name, cmdline); extract the config path |
| `workload/train/parse.rs` | Pure: parse the four stdout line shapes → `TrainEvent` |
| `workload/train/config.rs` | Pure: parse the training + model YAML → `TrainConfig` |
| `workload/train/monitor.rs` | I/O: pid discovery, `/proc/<pid>/fd/1` resolution, log tailing, checkpoint mtime polling; owns `TrainState` |
| `animation/train_view.rs` | Pure-ish render: the network, mountains, nightscape → `Vec<Line>` |
| `ui/tui/mod.rs` | `DisplayMode::Training`, `t` key, legend + explain overlays |

Parsers stay pure and unit-testable without a running trainer; all I/O is
confined to `monitor.rs`, matching how `inference_server/` is already split.

## Error handling

Every stage degrades rather than failing: no training process → an honest
"scanning" state; fd 1 not a regular file → process-level view plus a plain
explanation; unparseable log line → skipped, not fatal; missing/unreadable YAML
→ omit the model card rather than guess; log rotated or truncated → detect
shrinking size, re-seek. A dead process returns the view to scanning.

## Testing

Pure parsers get table-driven tests against **verbatim** tt-train output
strings. The monitor gets tests against a temp-dir fake log + a real spawned
process for `/proc/<pid>/fd/1` resolution (the technique is the load-bearing
part of auto-attach, so it gets a real-process test, not a mock). The renderer
gets tests for the no-data, no-log, and mid-run states, plus width-safety at
narrow terminals. Nightscape determinism is testable: the same (x, y, frame)
must always produce the same star.

## Deliverables beyond the view

- **Legend** (`l`): the nine color channels, in the established
  `*_legend_lines` style.
- **Explain** (`?`→explain): what tt-train emits, what's derived, what
  auto-attach does, and why per-step metrics need redirected stdout.
- **Website copy**: a feature entry in `site/index.html`.
- **Docs**: README + QUICK_START mention; `AGENTS.md` phase entry.

## Out of scope

Post-hoc run summaries (`tt_train_metrics.py` JSON), multi-run comparison,
multi-host/MPI rank aggregation (`--multihost` prints `[Rank {}]`), and
launching training from inside tt-toplike. Single live local run only.
