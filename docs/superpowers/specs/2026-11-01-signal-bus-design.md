# The signal bus: one recorded stream of everything tt-toplike can see

Date: 2026-11-01. Branch: `what-if`. Status: design, nothing built. The product owner asked for it
in the Die 3D session after watching the view for a long time: "the viz is still too static ... we
need to integrate the same signals other viz use". This document grew from that request.

## Why

Each view in tt-toplike pulls its own data. Die 3D reads chip power, temperature and PCIe rates and
the list of processes that have a device open. It does not read the inference server's model name,
tokens per second, queue depth or load phase, the training state, the hivemind events, or the
per-channel DRAM data. The other views read those. A person switching between Die 3D, `i`, arcade
and `~` sees four unrelated pictures of one machine.

The product owner wants three things from the same stream of facts:

1. Views that agree. A model coming up on a chip looks like the same event in every view.
2. Views that show where a request or a model touches the system, as it happens.
3. Other programs that use the stream: tt-station (asks for state, then streams to a local
   tt-toplike), tensixinet (makes music from every signal), and a new view that is only watched.

The owner's rule for the data: record the full stream, as thick as the hardware allows, because
everything else is a slice of it and a past that was not recorded cannot be recovered, whether it is
two weeks or two seconds old.

## Goals

1. A bus: an append-only log of raw samples and events. Every consumer reads the same log.
2. Sources write to the bus. A source knows nothing about any view.
3. Views read the bus. A view knows nothing about any source.
4. Everything is recorded and replayable. A consumer can start at any past point and read at any
   speed, then continue live.
5. Die 3D and `i` read the bus first. Die 3D shows which model is on which chips and its phase,
   model bring-up as it happens, request completions as pulses, per-channel DRAM state, and
   hivemind events as sparks on the right chip.
6. A socket lets another process ask for a snapshot and then subscribe from an exact point.
7. The existing remote stream that tt-station sends today keeps working.

## Non-goals

- Changing the other views in the first release. They keep reading what they read today and move
  onto the bus one at a time later.
- Replacing tt-gozer, tt-station or tt-telemetry. The bus integrates with them.
- Reading anything from the hardware that the app does not read today, in the first release. The
  per-tile counter sidecar is a later phase (see below).
- A new GUI. The watch-it roguelike view and the tensixinet integration come after the bus works.

## Anti-goals (a failure even if it runs)

- A source that makes a chip read faster, or in a different way, because a consumer asked for it.
  Sources set their own rate.
- A source that touches a chip another process holds. Reads that open `/dev/tenstorrent` check the
  gozer lease first.
- A value drawn or sonified as a measurement when it is an estimate.
- A recording that grows without a limit.
- A frame-time rise in the TUI that a person can see.
- Right-hand border characters in anything drawn.

## What goes on the bus

**Entities.** Things with a stable identity: host, board, chip, DRAM channel, Ethernet port, tile,
model, process, lease, training run. Identity is a key, never a list position (the repo already had
bugs from indexing per-device data by loop position). A chip's key is its ASIC unique ID, with its
PCI address as the fallback, because the `/dev/tenstorrent/N` index can change. Board and chip
names follow the Kubernetes DRA attributes that tt-operator publishes: `boardName`, `chipArch`,
`trayID`, `asicLocation`, `pciAddress`. The key for a tile is its NoC0 `(x, y)` with the logical
name (`t3,4`, `d1,2`, `e`) as an attribute.

**Samples.** One reading of one signal on one entity: `{seq, t_mono_ns, t_wall_ms, source, entity,
signal, value, provenance}`. Signal names reuse tt-telemetry's `tt_*` names where one exists
(`tt_asic_temperature_celsius`, `tt_tdp_w`, `tt_ai_clock_mhz`). New names follow the same style.
A sweep of many tiles is one record holding an array of values, so one record does not cost one
line per tile.

**Provenance.** Each sample carries one of `Measured`, `Derived`, `Estimated` or `Decoration`. This
is the repo's rule ("every moving thing is real data, a labelled estimate, or named decoration")
written into the data. A missing reading is recorded as no sample at all.

**Events.** Discrete things that happened, each with an entity and a time: `ModelLoading`,
`ModelReady`, `ModelGone`, `RequestsCompleted(n)`, `KernelCompiled`, `Reset`, `LinkChanged`,
`LeaseGranted`, `LeaseReleased`, `EccTick`, `ViewChanged`. `ViewChanged` carries the name of the
view on screen, because tensixinet's roadmap asks for it.

**World.** A reducer that reads the log and holds the current state: for each entity, the latest
value of each signal, current phase, and recent events. Views read the world for "now" and the log
for history. The world is rebuilt from the log, so a replayed log gives the same world.

## Architecture

- **Crate.** A library crate `tt-toplike-bus` in a Cargo workspace inside this repo. It is moved to
  its own repository after tt-station and tensixinet have both used it and the schema has stopped
  changing. The name avoids `tt`, which belongs to the official CLI.
- **Traits.** `Source` (produces samples and events on its own schedule and says how often it may
  read), `Governor` (decides whether a source may touch a given chip right now), `View` (reads the
  world and the log), and `Sink` (receives every record: the recorder and each transport). Plain
  structs and an enum for events. No ECS crate in the first release; the entity count is in the
  dozens. A crate such as `hecs` can replace the store later without changing the traits.
- **Log.** One writer thread assigns the sequence number and appends. Readers follow by sequence
  number. A recent window stays in memory. Older records are in segment files on disk.
- **Disk format (my call).** JSON Lines, one record per line, in segment files that rotate every ten
  minutes and are compressed after rotation. It is readable from Python and a shell, which
  matters for tensixinet and for debugging. The format is versioned in a header line. The reader
  skips a corrupt line, as tt-gozer's `history.jsonl` reader does.
- **Retention (my call).** A setting with a default of 24 hours and 5 GB, whichever comes first. The
  per-tile sidecar's data is much larger and is off unless the sidecar is on.
- **Snapshot and subscribe.** A consumer asks for a snapshot and receives the world plus the
  sequence number it is current to. It then subscribes from that number and misses nothing and
  sees nothing twice.
- **Replay.** The same call with a start time, a speed and an optional filter on entities and
  signals. A replayed stream is marked as replay in its header.
- **Transports.** In-process channels for views. A Unix socket carrying JSON Lines for other local
  programs. For tt-station, the existing `/telemetry` WebSocket frames and the `tt_toplike`
  `schema: 1` extension keep their format and are built from bus records (see decision 2), so
  `--remote` keeps working. A
  WebSocket for remote consumers carries the same JSON Lines as the socket.
- **Rates.** Views are drawn at the frame rate they have today. The log is not tied to the frame
  rate. Slow sources (the inference monitor at 5 s, tt-smi at 1.5 s) emit at their own cadence and
  the world keeps the last value with its age, so a view can tell a fresh value from a stale one.

## Sources in the first release

| Source | Emits | Notes |
|---|---|---|
| hwmon and sysfs | power, voltage, current, temperature, clocks, fan, PCIe in and out | Every tick, as today. Needs no lease. |
| tt-smi snapshot | the slow-lane fields, per-channel DRAM data (`gddr_telemetry`), Ethernet link mask, harvesting | 1.5 s, as today. |
| Inference monitor | `ServiceState`: model, chips, phase, tokens/s, running and waiting requests, KV cache, completed delta | 5 s. Emits `ModelLoading`, `ModelReady`, `RequestsCompleted`. |
| Training monitor | `TrainState` | Chips come from joining the training process to the process monitor's device list. |
| Hivemind | `SniffEvent`s with source kind and chip | Already typed. |
| Process monitor | process to chip | 2 s. |
| gozer | leases keyed by PCI address, from `gozer status --json` and a tail of `history.jsonl` | `status` never opens a chip and is safe to poll. A release is also a reset, so it emits `Reset` and a gap marker. |
| tt-station agentd | processes and inference over `--remote` | The existing extension. |

A source that is absent or fails emits nothing. It does not emit zeros.

## The first views on the bus

**Die 3D.** Each chip gets the flag of the model on it (name, phase colour, tokens/s, waiting
requests), using the workload colours the repo already has (train amber, load purple, serve teal).
Bring-up shows as a fill from the DRAM edge toward the cores, driven by the load phase and by the
compiled-kernel and loaded-shard counts. Request completions show as pulses on the chip's board.
Each GDDR row takes its own channel's temperature and flashes on an ECC change. Hivemind events
appear as sparks on the chip they name. The tile glow stays an estimate and says so, until the
per-tile sidecar exists.

**`i` (inference monitor).** Reads the same world. It keeps its look. It stops reading
`ServiceState` directly.

Both views show the same phase, colour and numbers for the same model, which is the test that
decides whether the bus has done its job.

## The sidecar (a later phase, off by default)

Per-tile NoC NIU counters are the only signal that says where on the chip traffic goes. They come
from a Python sidecar built on tt-exalens, started with an explicit flag, local only. It writes raw
samples to the bus over a pipe. The Rust side derives rates.

Measured on this box (a QuietBox 2, two p300c boards, tt-exalens 0.4.2, tt_umd 0.9.14): a 10 Hz sweep
of 156 tiles per chip costs about 8 to 17 ms per chip sweep and about 15 to 19 percent of one core.
With the reader running beside a ttnn matmul loop and beside a vLLM serve of Llama-3.1-8B,
throughput and latency did not change by more than the noise (about 1 percent for matmul and about
3 percent for the serve). The counters show load starting within one 200 ms sample, and they
separate prefill from decode: decode puts DRAM traffic almost entirely on NoC0.

The rules the sidecar follows. Round 2 of the testing may tighten them, and this section is updated
from its report.

- Reads only. No writes, halts, resets, performance-counter control or ARC messages.
- Single-word reads, never bursts.
- Tensix, DRAM and Ethernet tiles only. Harvested tiles skipped. Never ARC, PCIe, L2CPU or security.
- At most 10 Hz, 5 Hz by default. A 2 s timeout per read. Stop on any error with no retry. A slow
  sweep skips a tick and the next sweep starts on schedule.
- Start only when gozer shows the chips held by the lease the sidecar runs in, or when the user
  overrides with a warning.
- Start after a server is ready until cold bring-up has been tested.
- Detach before any chip reset or server restart.
- Record the exalens, UMD and firmware versions in the log header. Record a gap marker after a
  reset.

The sidecar can later be replaced by a Rust binding to tt-umd without changing the schema.

## Versions

The bus library starts at 0.1.0. tt-toplike takes the next minor version for the first release and
a 1.0 when the views, tt-station and tensixinet all read the bus. This is my call; the owner said
this is a new version number for the project.

## Constraints

- Rust, edition 2021, the pinned 1.93 toolchain. ratatui 0.30 for the views.
- The repo's rules still apply: bump the version with each change, plain-English docs, left and
  bottom borders only, no change to the pinned frame hashes at 100 x 30 and above unless the
  change is a view change that says so.
- Tests never touch hardware. They use the `Fake` backend and recorded fixtures.
- Hardware tests run only under a gozer lease, on a board nobody else holds, with no `tt-smi -r`.

## Tests that must exist

- Sequence numbers are gapless and ordered across sources. A snapshot at sequence N followed by a
  subscription from N shows every later record once.
- A replayed log rebuilds the same world as the live run. A recorded bring-up fixture is checked in
  small enough to keep in the repo.
- The recorder survives a corrupt line and a truncated last segment. Retention removes old segments
  and stops at the limit.
- A source that fails emits nothing, and the world shows the value as stale.
- Provenance survives the round trip and is shown in the Die 3D caption for estimated glow.
- The tt-station frames produced from the bus match the current frames for the same input, byte for
  byte on a fixed fixture.
- Die 3D and `i` show the same phase and tokens/s for one `ServiceState` fixture.
- A governor test per rule above, each seen failing first.

## Decisions from the builder check

A reviewer read this design cold and asked eight questions. The first is the product owner's
answer. The rest are my calls.

1. **Process model (the owner's answer).** The TUI hosts the bus in-process by default. The same
   binary has a headless mode (`tt-toplike-tui --headless`) that records and serves the socket with
   no screen, for running under systemd. The log directory is the shared place. A second TUI on the
   same box reads the headless host's socket when one is running and otherwise hosts its own bus
   with its own log directory.
2. **tt-station frames (my call).** The bus carries each tt-smi snapshot as one opaque raw record,
   next to the typed samples taken from it. The `/telemetry` frame is rebuilt from the raw record
   and the `tt_toplike` extension, so it matches today's frame byte for byte on a fixed fixture.
   This replaces the earlier wording "produced from the bus unchanged": the transport code stays
   as it is and its input comes from the bus.
3. **Chip identity (my call).** In the first release the key is the PCI address (`bus_id`), which
   every backend already has. The ASIC unique ID is an attribute, filled in when a source supplies
   it (gozer's topology and the tt-smi snapshot do). The mapping from `/dev/tenstorrent/N` index to
   key is built from the backend's device list and refreshed on every discovery pass. Records from
   sources that only know an index (processes, training, hivemind, `ServiceState.chips`) are
   resolved through that map when they are written. An index that is not in the map yet is
   held for one discovery pass and then dropped with a counter.
4. **Which sources need the governor (my call).** The existing sources keep their present
   behaviour and need no lease: hwmon, sysfs, the `tt-smi -s` poll, `/proc` scans and the metrics
   scrapes. The governor applies to a source that opens a chip on its own account. In the first
   release there is none. The tt-umd or exalens sidecar is the first. When a governor denies a
   source, the source emits nothing for that tick and the world marks its values stale.
5. **Replay deliverables (my call).** The first release includes `--record <dir>`,
   `--replay <dir-or-file> [--speed N]` for the TUI, and `tt-toplike-tui bus inspect <log>`, which
   prints a summary and filters by entity and signal. The checked-in bring-up fixture is played
   into Die 3D by the replay mode in a test.
6. **Pinned frame hashes (my call).** Bus-fed decorations (model flags, bring-up fill, pulses,
   per-channel GDDR rows, sparks) draw only when the bus holds something to show. With no model,
   no events and no per-channel data, the frame is the one pinned today, so the existing hashes
   hold. New hashes are added for the decorated frames from a fixture.
7. **Volume (my call).** Sources are recorded at their own rate with no downsampling. Fast-lane
   readings are written as one record per chip per tick that holds all of that tick's values, not
   one line per value. A slow-lane field that a source repeats unchanged is recorded when the
   source updates it. My arithmetic puts the fast lane near 16 KB/s for four chips at 10 Hz before
   compression, about 1.4 GB a day, and compression makes the 5 GB limit hold for more than the 24
   hour default. The per-tile sidecar is far larger and is governed by its own settings.
8. **Platforms (my call).** The recorder, world, log format and replay build everywhere the crate
   builds today. The Unix socket is `cfg(unix)`. The gozer source is `cfg(target_os = "linux")`.
   The workspace conversion keeps `cargo build` at the root producing the same binaries, and the
   Debian packaging and CI are checked as part of the first task.
9. **Privacy (my call).** Process command lines are recorded with the arguments that match common
   secret patterns (tokens, keys, passwords, `HF_TOKEN=...`, `--api-key ...`) replaced by a
   placeholder. The log directory is created with owner-only permissions.

## Left to the builder

The exact field names beyond the ones above, the in-memory window size, the segment compression
choice, the socket path and permissions, the module layout in the workspace, and the wording of the
captions.

## Calls I made that need checking

- The bus is a library crate in a workspace in this repo and moves to its own repo later.
- JSON Lines on disk, with a 24 hour and 5 GB default retention.
- No ECS crate in the first release.
- The first release has no per-tile sidecar, no roguelike view and no tensixinet integration.
- Die 3D and `i` are the first two views on the bus.
- The tt-station frames are built from the raw tt-smi record on the bus, and the existing transport stays in place.
- The TUI hosts the bus by default and a headless mode runs it with no screen (the owner's answer).
- Version numbers: bus 0.1.0, tt-toplike next minor, 1.0 when all three consumers use the bus.
