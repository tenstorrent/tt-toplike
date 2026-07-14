# Hivemindsweeper — Activity Sniffer Mode

- **Date:** 2026-07-14
- **Status:** Design approved, pending spec review
- **Owner:** Taylor Singletary
- **Component:** `tt-toplike` TUI — new opt-in display mode

## Summary

Hivemindsweeper is a new, **opt-in** TUI mode whose only job is to unearth activity
on Tenstorrent hardware and the software stack driving it, and make it visible. It is
a **passive sniffer**: it observes signals that already exist (process/fd state, compile
caches, log streams, kernel-driver messages, and downstream debug artifacts) and merges
them into a minesweeper-style `source × device` board with a drill-in feed. It never
instruments the stack and never touches device debug buffers.

The name and the board are a nod to Minesweeper: activity is "hidden" until swept, and
the board reveals sources as they light up. A unified tcpdump-style feed is available
alongside the board.

## Motivation

During model bring-up (tt-lang, TTNN, tt-metal, tt-forge, tt-xla, vLLM/media servers)
there are long silent windows — first-run kernel compilation and weight loading — where
the operator has no signal about what is happening. Separately, there is no single view
that answers "what on this machine is touching the TT hardware right now, and what is it
doing?" tt-toplike already has a narrow version of this instinct in
`workload/inference_server/` (it surfaces the last non-health log line and derives a
lifecycle from resource/process probes). Hivemindsweeper is the general case: a
low-effort, safe, on-demand firehose of stack activity.

## Goals

- Surface activity from multiple sources in one place, on demand, with zero idle cost
  when the mode is not active.
- Attribute activity to a device when possible; otherwise to a `host` lane.
- Be strictly read-only and non-invasive — never change the behavior of the workloads
  being observed. Consistent with tt-toplike's safe-by-default ethos.
- Auto-discover ambient sources with no configuration, and let the user point the sniffer
  at a specific target (file, pid, or a wrapped command) for the deliberate
  "watch this bring-up" case.
- Reuse existing helpers (`inference_server` log-tail/docker, `process_monitor`,
  `animation/common` color ramps) rather than duplicating them.

## Non-goals

- No instrumentation of the stack. We never set `TT_METAL_DPRINT_CORES`,
  `TT_METAL_WATCHER`, the profiler env vars, or `TT_METAL_INSPECTOR` on behalf of a
  workload — those are compile-time/launch-time decisions that change the binary and
  cannot be flipped on for a running process.
- No reading of device-side debug/print buffers (see Safety invariant).
- No gameplay. The Minesweeper metaphor is cosmetic (cover/reveal), not a game.
- No remote/agentd transport in v1 (noted as a future phase).

## Core UX

### The board (primary view)

A `source × device` matrix:

```
        D0  D1  D2  D3  host
ttnn    ▓▓  ░░  ··  ██   ░░
metal   ██  ▓▓  ░░  ▓▓   ··
tt-lang ··  ··  ··  ··   ██
vLLM    ░░  ··  ··  ▓▓   ▓▓
driver  ··  ··  ··  ··   ░░
 → sweep [metal×D0]: compiling ncrisc.o
```

- **Rows** = sources that have *ever* fired (the board grows as activity is unearthed).
- **Columns** = `D0…Dn` plus a `host` column for events that cannot be pinned to a device.
- **Cell heat** = a time-decayed activity counter rendered on the `· ░ ▒ ▓ █` ramp with
  color, reused from `animation/common.rs`.
- A cursor (arrow keys / `hjkl`) selects a cell; the caption line under the board shows
  that cell's latest event.

### Sweeping semantics (the Minesweeper nod)

- A cell with activity that has not yet been visited renders "covered" (dim, subtle
  marker). Moving the cursor onto it "uncovers" it and its feed becomes readable.
- A cell that goes hot while still covered gets a blink/marker — an unrevealed hotspot,
  like an un-flagged square.
- This is purely cosmetic state layered on the heat data. No scoring, no mines.

### Drill-in / unified feed (bottom pane)

- Default: filtered, scrolling event feed for the **selected cell**.
- `f` toggles to the **unified feed** — all sources merged, timestamped, color-coded
  (the tcpdump view).
- `s` cycles a severity floor (hide Trace/Info). Substring filtering is deferred to a
  later phase to avoid colliding with the `/` command bar (see below).

### Activation (opt-in, never default)

- Hotkey `~` enters the mode from Insights/Grid; excluded from the `v`-cycle, exactly like
  `i` for `InferenceMonitor`. `Esc`/`~` exits to the previous mode.
- Slash command: `/hivemind`, via the TUI's existing `/` command bar (the same bar that
  hosts `/remote`, `/serve`). This matches the "hotkey or slash command" activation.

### Point-at-target commands (via the `/` command bar, live)

- `/watch <path>` — tail a file into a dedicated lane; source classified from content.
- `/watch pid <n>` — attach to a PID: read `/proc/<n>/fd` and cmdline, best-effort follow
  its open log files.
- `/wrap <command…>` — spawn the command, capture stdout+stderr live into an `Emission`
  lane. This is the `/wrap ttl export mykernel.py` case that streams emitted C++/MLIR.
  Gated behind a one-line confirmation because it has a side effect (running a process).

These three dispatch inline in the command-bar Enter handler (like `/serve`), because they
mutate the loop's live `Hivemind` engine. `/mode hivemind` is also accepted as an alias.
- Each target becomes its own row, or merges into an existing source row if it classifies
  as one.

## Architecture (Approach A: in-process collectors + event bus)

New module `src/workload/hivemind/`:

| File | Responsibility |
|------|----------------|
| `mod.rs` | `Hivemind` engine: owns ring buffer, running collectors, heat grid; `start()`/`stop()`/`poll()`/`add_target()` |
| `event.rs` | `SniffEvent`, `Source`, `Severity`, `EventKind` |
| `collector.rs` | `Collector` trait + `spawn` helper (thread + shutdown flag + panic guard) |
| `classify.rs` | source classifier (process name + log signature → `Source`) |
| `grid.rs` | `(source × device)` heat aggregator with time decay |
| `collectors/procfs.rs` | process + fd scan |
| `collectors/cache_watch.rs` | compile-cache / `generated/` fs-watch |
| `collectors/log_tail.rs` | log streams + docker (reuses `inference_server` helpers) |
| `collectors/kmsg.rs` | `/dev/kmsg` / `dmesg` driver messages |
| `collectors/wrap.rs` | point-at-target: file tail, pid attach, wrapped subprocess |
| `collectors/inspector.rs` | tt-metal Inspector log-file tail (RPC subscribe = future phase) |

TUI integration: a new `DisplayMode::HivemindSweeper` in `src/ui/tui/mod.rs`, rendered by
a new `src/ui/tui/hivemind_view.rs` (board + drill-in), plus Legend/Help/Explain overlay
entries.

### Data model

```rust
struct SniffEvent {
    ts: Instant,             // monotonic; wall-clock formatted at render
    source: Source,          // Ttnn, TtMetal, TtLang, TtForge, TtXla, Vllm, Driver,
                             //   Inspector, Host, Unknown
    device: Option<u8>,      // None → "host" column
    severity: Severity,      // Trace, Info, Notice, Warn, Error
    kind: EventKind,         // Log, Compile, Process, Fd, DriverMsg, Emission, Inspector
    text: String,            // one line, already trimmed
    origin: String,          // collector tag, e.g. "kmsg", "docker:z-image", "wrap:ttl"
}
```

### Event bus and heat grid

- Collectors are producers on a bounded `std::sync::mpsc::sync_channel` (no new crate
  dependency). The engine drains it each `poll()` into a bounded `VecDeque<SniffEvent>`
  (cap ~4096; oldest dropped).
- Back-pressure: `try_send` on a full channel drops the **newest** event for that collector
  and bumps a per-collector `dropped` counter shown in the header — no silent truncation
  (house rule).
- Heat: each drained event increments a decaying float `grid[source][device]`; decay runs
  per frame (`heat *= 0.9`) so idle cells fade to cold.

### Threading & lifecycle

- Nothing spawns until `DisplayMode::HivemindSweeper` is entered.
- Leaving the mode calls `stop()`, which signals every collector's shutdown flag and joins.
- Idle cost when not in the mode is exactly zero.

## Collectors

Each implements `Collector { name(), run(tx, shutdown), device_hint() }`.

- **procfs** — walk `/proc/*/fd` for symlinks into `/dev/tenstorrent/*` (the `N` gives the
  device column) and for open `.safetensors`/`.o` files; read `cmdline`/`comm` to classify.
  Polls ~1 s; emits `Process`/`Fd` events on **change** (open/close), so steady state is
  quiet. Builds on `workload/process_monitor.rs`.
- **cache_watch** — polls (via directory scan + seen-set, no `notify` dependency)
  `TT_METAL_CACHE`, `tt_metal_cache/`, `tt_dit_cache`/`TT_DIT_CACHE_DIR`, and `generated/`
  under `TT_METAL_HOME`/cwd. Emits `Compile` events on newly-seen `.o`/artifact files.
  Device column parsed from `cache_{model}/{device_type}/` when present, else `host`.
- **log_tail** — reuses `inference_server/logs.rs` health-poll filtering and the docker-logs
  helper; tails discovered server logs plus reachable ttnn/tt-metal loguru stderr. Docker
  per-container → device column when the container is device-pinned. **Also the pickup
  point for DPRINT and Inspector *file* output** (see Observability boundary).
- **kmsg** — reads `/dev/kmsg` (falls back to `dmesg -w` if kmsg is unreadable); filters to
  `tenstorrent`/`tt_kmd`/PCIe-AER lines; classifies as `Driver`. Device index parsed from
  the message when present.
- **wrap** — point-at-target: file tail, pid attach, or wrapped subprocess (stdout+stderr
  via piped `Command`, killed on `stop()`).
- **inspector** — tails tt-metal Inspector log files (`inspector::Logger` output under its
  `logging_path`) when present, surfacing program/kernel-compile metadata as
  `Inspector`/`Compile` events. RPC subscription (Cap'n-Proto) is a future phase.

### Source classifier

`classify.rs` maps `(process name, log-line signature)` → `Source`, so tt-forge-onnx and
tt-xla — which lower through tt-mlir → tt-metal and emit their own loguru logs — get their
own labeled lanes for free, without dedicated taps. Unrecognized → `Unknown` (still shown,
still swept).

## Observability boundary (DPRINT / Inspector) — the key safety insight

tt-metal's DPRINT transport is a per-RISC ring buffer in device L1 with a single
write pointer (device) and read pointer (host). The host-side `DPrintServer` thread runs
**inside the process that owns the device** and is the sole legitimate reader; the device
calls `wait_for_space()` and **stalls** when the buffer is full. A second reader (e.g.
tt-toplike attaching to the device) would race `rpos`, desync the reset handshake, and
could corrupt or hang the very kernel being observed.

**Therefore, the hard invariant:** hivemindsweeper **never reads device-side debug/print
buffers**. It consumes only downstream artifacts:

- **DPRINT** → tail its file sink (`TT_METAL_DPRINT_FILE`,
  `TT_METAL_DPRINT_ONE_FILE_PER_RISC` → `generated/dprint/device-N_worker-core-*.txt`) or
  the owning process's stdout/docker logs. Reading these is inert. If DPRINT output has no
  reachable destination, we cannot see it — and that is an accepted limit, not a bug.
- **Inspector** (`tt_metal/impl/debug/inspector/`, env `TT_METAL_INSPECTOR`) is designed
  for cross-process consumption: it writes log files *and* exposes an RPC server (the
  mechanism `tt-triage` uses). This is the preferred "magnetic" pickup — safe by design,
  no interference. v1 tails its files; RPC subscribe is a future phase.

Consequence: "magnetic" pickup means we tail artifacts and subscribe to services that
already exist; we never conjure output from on-chip buffers. Because DPRINT/Inspector are
opt-in at build/launch time, we can *detect and surface* that they are active (their files
exist) but never enable them for a workload.

## Safety & permissions

- **Strictly read-only / non-invasive.** No instrumentation env vars are ever set. Only
  pre-existing signals are observed.
- **Graceful degradation.** A collector lacking permission (e.g. `/dev/kmsg` unreadable,
  other users' `/proc/<pid>/fd`) marks itself `✗(perm)` in the header with a one-line hint;
  other collectors keep running. Never a hard failure.
- **Panic isolation.** Each collector's `run` is wrapped; a panicking collector is marked
  `✗(err)` and does not take down the engine or peers (learned from the Luwen panic-catch
  history).
- **`:wrap` confirmation.** `:wrap` is the only action with a side effect (spawning a
  process); it requires an explicit confirm. Args are passed directly to `Command` — no
  shell interpolation.

## Error handling

- `Hivemind::poll()` is infallible — it drains whatever is present and updates the grid.
- Collector spawn failures are surfaced in the header, not propagated as fatal.
- Malformed log/kmsg lines are passed through as `Unknown`/`Info` rather than dropped
  silently.

## Testing

- **Unit:** `classify.rs` (process-name + log-signature fixtures → expected `Source`);
  `grid.rs` decay + heat ramp; ring-buffer drop/counter behavior; device-column parsing
  from cache paths and kmsg lines.
- **Collector-level:** each collector tested against a temp dir / fake `/proc`-style
  fixture / canned log file via a `LineSource` abstraction — no real hardware needed
  (mirrors the backend mock tests).
- **Render:** golden-style test that a fixed event set produces the expected board heat and
  filtered feed (matches `overlay_panel_does_not_truncate_wide_content`).
- **TUI conventions:** left/bottom borders only; panels sized to widest real display column
  via `unicode-width`; never clip.

## Future phases

- **Inspector RPC subscribe** — speak the Cap'n-Proto RPC directly instead of tailing files,
  for richer, lower-latency program/kernel-compile events.
- **agentd transport (Approach C)** — move heavy/privileged collectors into `agentd` and
  have tt-toplike subscribe over its `tt_toplike` stream, so remote boxes get sniffing for
  free.
- **Persistence** — optional session capture of the event stream to a file for post-hoc
  review.

## References

- Prior art in-repo: `src/workload/inference_server/` (logs, detect, metrics, monitor,
  probe), `src/workload/process_monitor.rs`, `src/animation/common.rs`.
- tt-metal: `impl/debug/dprint_server.{cpp,hpp}`, `hw/inc/api/debug/dprint.h`,
  `hostdevcommon/api/hostdevcommon/dprint_common.h`, `impl/debug/inspector/`.
- Conversation research (2026-07-14): DPRINT transport / back-pressure / Inspector.
