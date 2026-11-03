# The signal bus

The signal bus is one recorded stream of everything tt-toplike can see about a machine:
chip telemetry, models coming up and serving, training runs, processes, chip leases and
events such as resets. Sources write to it. Views and other programs read it. Every record
is also written to disk, so a past run can be inspected and replayed.

This page describes what is in the first release (tt-toplike 0.15.0, bus crate 0.1.0). The
design is in `docs/superpowers/specs/2026-11-01-signal-bus-design.md`. The code is in the
`crates/tt-toplike-bus` library crate (the record types, the log, the world, replay and the
socket) and in `src/bus/` (the adapters that turn tt-toplike's existing data feeds into
records).

Contents

1. Why there is a bus
2. Commands at a glance
3. The data model
4. The log on disk
5. Recording and replay
6. The socket protocol
7. Running headless under systemd
8. Attaching a TUI to a running host
9. Privacy
10. The governor and gozer
11. tt-station compatibility
12. The sidecar (not in this release)
13. Limits and known issues
14. For consumers (tt-station and tensixinet)

---

## 1. Why there is a bus

Before the bus, each view pulled its own data. The Wafer Waffler read power, temperature and PCIe rates.
The `i` view read the inference monitor. Arcade, hivemind and training read other things. A
person switching between views saw several unrelated pictures of one machine.

The bus gives every view and every other program the same stream of facts:

* A model coming up on a chip is one event, and every view that reads the bus sees the same
  event.
* Views can show where a request or a model touches the machine, as it happens.
* Other programs (tt-station, tensixinet, a future watch-only view) read the same stream.

The rule for the data is to record the full stream at the rate each source produces it. Views
and tools read parts of it, and data that was not recorded cannot be recovered later.

In this release every view reads the bus. The Wafer Waffler (the `3` view, `die3d` in the code)
draws model flags, the bring-up fill, request pulses, per-channel GDDR rows and hivemind sparks.
The arcade, Grid, the Insights chip panels, Starfield, Memory Flow, Memory Castle, Defrag,
Training and the `~` view each draw it in their own style (section 8). The `i` view reads it when
the TUI is attached to a `--headless` host (section 8).

```
  sources                         the bus                       readers
  --------------                  ------------------------      ------------------
  backend (hwmon, sysfs,          one writer assigns            every view (in process)
    tt-smi)                       sequence numbers              socket clients
  inference monitor               in-memory window              bus inspect, replay
  training monitor                segment files on disk         tt-station frames
  hivemind sniffer                world (latest state)
  process monitor
  gozer leases
  TUI (view changes)
```

---

## 2. Commands at a glance

| Command | What it does |
|---|---|
| `tt-toplike-tui --headless` | Host the bus with no screen. Records to `~/.local/state/tt-toplike/bus` and serves the socket until SIGTERM or SIGINT. |
| `tt-toplike-tui --record DIR` | Run the TUI and also record the bus to `DIR` (same as `--bus-dir DIR`). |
| `tt-toplike-tui --bus-inspect PATH` | Print a summary of a recorded log and exit. Narrow it with `--entity KEY` and `--signal NAME`. |
| `tt-toplike-tui --headless --replay PATH [--speed N] --bus-socket SOCKET` | Play a recorded log into a headless host. No hardware is read. |
| `tt-toplike-tui --bus-attach SOCKET` | Run a TUI that reads the bus of the host serving `SOCKET`. |
| `tt-toplike-tui --no-bus-attach` | Run a TUI that does not look for a headless host at start-up. |

All flags:

| Flag | Meaning |
|---|---|
| `--headless` | Run with no screen. Conflicts with `--print`, `--bench` and `--bus-inspect`. |
| `--bus-dir DIR` | Log directory. Default with `--headless`: `$XDG_STATE_HOME/tt-toplike/bus`, else `~/.local/state/tt-toplike/bus`. The TUI keeps its bus in memory unless this or `--record` is given. |
| `--record DIR` | The same as `--bus-dir DIR`. Giving both with different directories is an error. |
| `--bus-socket PATH` | Socket path. Default: `$XDG_RUNTIME_DIR/tt-toplike/bus.sock`, else `/tmp/tt-toplike-<uid>/bus.sock`. `--headless` always serves it. The TUI serves it only when this flag is given. |
| `--bus-retain HOURS[,GB]` | Retention. Default `24,5`. Fractions are allowed (`0.5` is 30 minutes). A GB is 10^9 bytes. |
| `--replay PATH` | A segment file (plain or `.gz`) or a directory of segments. Needs `--headless`. Cannot be combined with `--mock`, `--json`, `--host`, `--backend`, `--serve` or `--remote`. |
| `--speed N` | Replay speed, a number above zero. 1 is real time. Only valid with `--replay`. |
| `--bus-inspect PATH` | Summarise a log and exit. |
| `--entity KEY`, `--signal NAME` | With `--bus-inspect` only: show one entity or one signal. |
| `--bus-attach PATH` | TUI: attach to the host at `PATH` or fail at start-up. Cannot be combined with `--headless`, `--bus-socket`, `--record`, `--bus-dir` or `--bus-retain`. |
| `--no-bus-attach` | TUI: skip the start-up probe. Cannot be combined with `--headless` or `--bus-attach`. |
| `--mock-workloads` | With `--headless --mock`: also scan this box's real processes and model servers and record them against the mock chips. The log then holds only processes that hold a Tenstorrent device or are inference servers. Needs `--mock`. |

---

## 3. The data model

### Entities

An entity is a thing with a stable identity. Its key is a string of the form `kind:id`. A key
is built from stable identifiers and never from a list position, because `/dev/tenstorrent/N`
indexes can change after a reset or a hotplug.

| Kind | Key format | Written by |
|---|---|---|
| chip | `chip:<PCI address>`, for example `chip:0000:01:00.0` | backend source; gozer |
| DRAM channel | `dram:<chip key>/<channel>` | backend source (tt-smi `gddr_telemetry`) |
| model | `model:<service key>` | inference source, remote source |
| training run | `run:<binary>:<pid>@<start time>`, or `run:local` when only a log is known | training source |
| process | `process:<pid>@<start ticks>` (the start time makes a recycled pid a new entity) | process source, remote source |
| lease | `lease:<holder>`, where the holder is `pid-<pid>` or `who-<who>` | gozer source |
| board, tile, host, Ethernet port | `board:<serial>`, `tile:<chip key>/<x>,<y>`, `host:<name>`, `eth:<chip key>/<port>` | no source in this release; the key formats exist for the sidecar and later sources |

The chip key is the PCI address (`Device.bus_id`), which every backend has. A chip's ASIC
unique ID is not part of the key in this release. Records from sources that only know a device
index (processes, training, hivemind, a model's chip list) are converted to chip keys through
an index-to-key map that is rebuilt from the backend's device list on every tick. An index that
has no key yet is held for one rebuild and then dropped and counted. The map keeps at most 1024
held payloads.

### Samples

A sample is one reading of one signal on one entity. In the log it is one JSON line:

```json
{"q":1161,"m":2905690534,"w":1793625045140,"s":"backend","p":{"k":"sample","e":"chip:0000:01:00.0","n":"toplike.board_power_w","v":{"f":50.9},"r":"estimated"}}
```

| Field | Meaning |
|---|---|
| `q` | Sequence number. The first record is 1. Numbers are gapless and ordered across all sources. |
| `m` | Monotonic time in nanoseconds (`t_mono_ns`). Counts from the first time the process read its clock. |
| `w` | Wall-clock time in milliseconds since the Unix epoch (`t_wall_ms`). |
| `s` | Source name: `backend`, `inference`, `training`, `hivemind`, `processes`, `remote`, `gozer`, `view`, `identity`, `replay` or `tt-toplike-bus`. |
| `p` | Payload, tagged by `k`: `sample`, `sweep`, `event`, `raw` or `gap`. |

A sample payload has `e` (entity), `n` (signal name), `v` (value) and `r` (provenance). A value
is `{"f":1.5}` (float), `{"u":3}` (unsigned), `{"i":-3}` (signed) or `{"t":"text"}`. A reading
that cannot be taken is recorded as no sample at all. A float that is NaN or infinite is
dropped before it reaches the log and counted in the bus statistics.

### Sweeps

A sweep is many readings of one entity in one record, so a tick does not cost one line per
value. `n` holds the signal names and `v` the values, in the same order and the same length.

```json
{"q":1,"m":60,"w":1793625042235,"s":"backend","p":{"k":"sweep","e":"chip:0000:01:00.0","n":["tt_tdp_w","toplike.voltage_v","toplike.current_a","tt_asic_temperature_celsius","tt_ai_clock_mhz"],"v":[{"f":41.9},{"f":0.83},{"f":50.2},{"f":45.9},{"u":998}],"r":"estimated"}}
```

A sweep whose names and values differ in length is rejected. A sweep with some non-finite
values keeps only its finite pairs.

### Provenance

Every sample and sweep carries one of four provenance tags.

| Tag | Meaning |
|---|---|
| `measured` | Read from hardware or a real service. |
| `derived` | Computed from measured values (for example tokens per second from a counter delta). |
| `estimated` | A labelled guess. Every reading from a `--mock` backend is `estimated`. |
| `decoration` | Named decoration with no data behind it. |

A view that draws a value says which tag it has. A source that is absent or failed emits
nothing and never emits zeros. The world keeps the last value with its age, so a view can tell
a fresh value from a stale one.

### Events

An event is a discrete thing that happened. The `t` field names the type.

| Event | Fields | Emitted by |
|---|---|---|
| `ModelLoading` | `model`, `chips`, `progress` | inference, remote |
| `ModelReady` | `model`, `chips` | inference, remote |
| `ModelGone` | `model` | inference, remote |
| `RequestsCompleted` | `model`, `n` | inference, remote |
| `KernelCompiled` | `chip`, `source` | hivemind (compile events) |
| `Reset` | `chips` | hivemind (reset events), gozer (on a lease release) |
| `LinkChanged` | `chip`, `mask` (low 16 bits of the Ethernet live-link mask) | backend source |
| `LeaseGranted`, `LeaseReleased` | `lease`, `chips` | gozer |
| `Spark` | `chip`, `kind` (a fixed string such as `metal.log`) | hivemind |
| `ViewChanged` | `view` | the TUI, when the displayed view changes |
| `EccTick` | `chip`, `channel` | defined, but no source emits it in this release. The Wafer Waffler flashes a GDDR row when an ECC counter rises. |

The `ViewChanged` view names are `insights`, `grid`, `starfield`, `castle`, `flow`, `arcade`,
`defrag`, `inference`, `hivemind`, `training` and `die3d`.

Views must not animate on `ModelLoading` or `ModelReady` events. The host re-announces every
loading or ready model every five minutes (so retention cannot lose the announcement), which
repeats the event with an unchanged phase. A view reacts to a change of phase or progress.

### Raw frames

A raw record (`"k":"raw"`, fields `s` source and `b` body) carries a verbatim snapshot from a
source. The backend source writes each new `tt-smi -s` JSON snapshot as a raw record from the
source named `tt-smi`. The typed samples are taken from the same snapshot. The raw record
lets a replay or the tt-station frame builder use the original text. A mock backend and the
sysfs backend have no `tt-smi` snapshot and write no raw record.

### Gaps

A gap record (`"k":"gap"`, field `why`) says readings are missing for a stretch.

| Source | When it publishes a gap |
|---|---|
| `tt-toplike-bus` | The disk writer failed (directory not writable, disk full). One gap per outage, with the reason. |
| `replay` | A replay found a hole in the log it was reading (see section 5). |
| `gozer` | A lease was released. Gozer resets the chips on release, so readings taken just after may be wrong. |
| `identity` | An index could not be resolved to a chip key. |

### The world

The world is a reducer over the log. It holds the latest reading of every signal on every
entity (with provenance, sequence number and time), the announced models, the newest raw
frame per source, the last 512 events, and the sequence number of each chip's latest `Reset`.
A live world and a world rebuilt by replaying the log are equal.

A reading is never zeroed or removed because its source went quiet. Its age grows. A `Reset`
does not delete readings. It records the reset's sequence number, so a view can tell a reading
from before the reset from one after it.

Hardware-bounded entities (chips, boards, tiles, DRAM channels, Ethernet ports, hosts) stay in
the world. Churning ones (processes, leases, training runs, and models that were never
announced) are dropped after 15 minutes without a reading or event. A model appears in the
world only after an announcing event (`ModelLoading` or `ModelReady`); the sources emit the
event before the model's samples.

### Signal names

The strings are part of the log format. A name is never renamed. New names are added. Names
that tt-telemetry uses keep its spelling (`tt_*`). The rest start with `toplike.` or `model.`.

Fast lane, on a chip entity, in one sweep per poll. Written by the `backend` source.

| Signal | Unit |
|---|---|
| `tt_tdp_w` | watts (chip power) |
| `toplike.voltage_v` | volts |
| `toplike.current_a` | amps |
| `tt_asic_temperature_celsius` | degrees Celsius |
| `tt_ai_clock_mhz` | MHz |
| `toplike.pcie.in_bps` | bytes per second into the chip (absent when the backend has no counters) |
| `toplike.pcie.out_bps` | bytes per second out of the chip (absent when the backend has no counters) |

Slow lane, on a chip entity, recorded when the value changes. Written by the `backend` source.

| Signal | Unit |
|---|---|
| `toplike.board_power_w` | watts |
| `toplike.heartbeat` | ARC heartbeat counter |
| `tt_fan_speed_rpm` | RPM (firmware "no reading" values are not emitted) |
| `tt_axi_clock_mhz`, `tt_arc_clock_mhz` | MHz |
| `toplike.therm_trip_count` | lifetime count |
| `toplike.harvesting_state` | register value |
| `toplike.enabled_tensix_col` | column mask |
| `toplike.eth.live_mask` | 64-bit Ethernet live-link mask. A change also emits `LinkChanged`. |

Per-channel DRAM, on a `dram:` entity, recorded when the value changes. Written by the
`backend` source from tt-smi's `gddr_telemetry`.

| Signal | Unit |
|---|---|
| `toplike.dram.temp_top_c`, `toplike.dram.temp_bottom_c` | degrees Celsius |
| `toplike.dram.training_pass`, `toplike.dram.bist_pass`, `toplike.dram.harvested`, `toplike.dram.enabled` | 1 or 0 |
| `toplike.dram.ecc_corr_rd`, `toplike.dram.ecc_corr_wr`, `toplike.dram.ecc_uncorr_rd`, `toplike.dram.ecc_uncorr_wr` | error counts |

Model, on a `model:` entity. Written by the `inference` source (or `remote` for a `--remote`
box).

| Signal | Unit |
|---|---|
| `model.label` | text. The display name. |
| `model.phase` | text: `down`, `compiling`, `loading`, `ready` or `alarm` |
| `model.progress` | fraction 0.0 to 1.0 (estimated by the monitor) |
| `model.tokens_per_s` | tokens per second (derived) |
| `model.running`, `model.waiting` | requests running, requests waiting |
| `model.kv_cache` | fraction 0.0 to 1.0 |
| `model.ttft_s` | seconds, mean time to first token over the scrape window |
| `model.cpu_pct` | percent, 100 is one core |
| `model.rss_bytes` | bytes |
| `model.kernel_count`, `model.shard_count` | compiled kernels found, weight shards loaded |
| `model.media_jobs`, `model.generations_per_min` | media (image or video) servers |

Training run, on a `run:` entity. Written by the `training` source.

| Signal | Unit |
|---|---|
| `toplike.train.step`, `toplike.train.max_steps` | steps (max is sent only when known) |
| `toplike.train.loss` | loss (finite values only) |
| `toplike.train.step_ms` | milliseconds per step (measured when the trainer printed it, derived when timed from a progress bar) |
| `toplike.train.cpu_pct`, `toplike.train.rss_bytes` | trainer CPU percent and resident bytes |
| `toplike.train.chips` | text, comma-separated chip keys |

Process, on a `process:` entity. Written by the `processes` source (or `remote`).

| Signal | Unit |
|---|---|
| `toplike.proc.cpu_pct`, `toplike.proc.rss_bytes` | percent, bytes |
| `toplike.proc.name`, `toplike.proc.cmdline` | text, redacted and clipped (see section 9) |
| `toplike.proc.device` | text, comma-separated chip keys the process holds open |
| `toplike.proc.uses_tt` | 1 or 0 (remote source only) |

Gozer, on a chip entity, recorded when the value changes. Written by the `gozer` source.

| Signal | Unit |
|---|---|
| `toplike.gozer.state` | text: `FREE`, `HELD`, `HELD-FOREIGN`, `CLAIMED`, `STALE`, `BUSY-UNTRACKED`, `DEGRADED` or `UNREADABLE` |
| `toplike.gozer.who` | text, the lease's `--who`, redacted and clipped to 128 characters |

---

## 4. The log on disk

The log is a directory of segment files.

* **Names.** `bus-<first sequence number>.jsonl` while open, `bus-<n>.jsonl.gz` once closed.
* **Format.** JSON Lines. The first line of each segment is a header, for example
  `{"schema":1,"kind":"Live","tool_versions":{"backend":"mock","mode":"mock","tt-toplike-bus":"0.1.0"}}`.
  Every other line is one record. The schema version is 1. `kind` is `Live` or `Replay`.
* **Header tags.** `tool_versions` holds the bus crate's version plus two tags the host adds:
  `backend` (`hybrid`, `sysfs`, `json`, `mock`, `remote`, `replay`, ...) and `mode` (`live`,
  `mock` or `replay`). `bus inspect` prints both. The host's identity is not in the header. It
  travels in the socket's snapshot and subscribe lines.
* **Rotation and compression.** A segment closes after 10 minutes and is compressed with gzip.
  The compressed copy is written to a temporary name and renamed into place before the plain
  file is removed, so at least one of the two names exists at every moment. A reader tries the
  plain name first. `zcat` and Python's `gzip` module open the files.
* **Retention.** Defaults are 24 hours and 5 GB (10^9 bytes per GB), whichever limit is reached
  first. Change them with `--bus-retain HOURS[,GB]`. Closed segments that ended more than the
  age limit ago are deleted, and while the whole log is larger than the size limit the oldest
  closed segment is deleted. The open segment is never deleted. Retention runs at start-up, at
  each rotation, and when the host asks for maintenance (every 60 seconds), so an idle bus
  still ages out old segments.
* **Expected volume.** This estimate comes from reading `BackendSource`. Nobody has measured it on
  hardware. The source takes one sweep per chip per tick (10 per second per chip by default).
  Slow-lane samples are written only when a value changes, and a raw frame only when the tt-smi
  text changes. On a real 4-chip box that is roughly 55 to 95 records per second, about 20 to
  35 KB/s, or about 1.7 to 3 GB a day before compression. The mock backend changes every DRAM
  temperature on every tick and writes about 370 records per second for 2 chips, so a mock run
  is much larger than a real one. Retention (24 hours and 5 GB, gzip after rotation) bounds the
  disk in both cases.
* **Permissions.** A directory the bus creates has mode 0700. Segment files and the lock file
  have mode 0600. A directory that already exists keeps its mode. The log holds process
  command lines (after redaction), model names and raw tt-smi snapshots.
* **Locks.** One process at a time writes a directory. The bus takes a non-blocking exclusive
  lock on `<dir>/.lock`. A second process gets `this log directory is already open (another
  process, or a bus in this process that is still shutting down)` and the host exits with
  status 1. The lock is released when the process exits, including
  when it dies. A filesystem that cannot lock (for example an NFS home mounted `nolock`) makes
  the bus refuse to open, with a suggestion to use `--bus-dir` on a local filesystem. The
  socket has its own lock file at `<socket path>.lock`.
* **One kind of log per directory.** A directory whose newest segment header names a different
  `mode` (live, mock, replay) or a different backend class is refused when the bus opens, with
  `log directory holds a <class>/<mode> log; use another --bus-dir or remove it`. The classes
  are `mock`, `remote` and `local-hardware`. Every other backend (sysfs, hybrid, json, luwen,
  host) is local-hardware, so a change from one local backend to another is accepted. That
  matters for a systemd host whose `--backend auto` falls back between `hybrid` and `json`. A
  missing tag, an untagged older log and an unreadable header are accepted. When the lock could
  not be taken at open and is taken later, the check runs again at that point. A refusal then
  switches the disk off for the rest of the run: the bus keeps serving from memory, publishes one
  gap record with the reason and counts the records it did not write. The same late lock removes
  a header-only last segment that an earlier run left, so the new segment does not collide with it.
* **Memory.** The newest 65,536 records stay in memory. Older records are read back from the
  segment files.
* **Disk errors.** The disk never holds up a publisher. If the directory is not writable or
  the disk is full, the bus keeps serving live data from memory, publishes one gap record with
  the reason, counts every record it could not write, and retries after at least 10 seconds of
  record time. If the writer queue (16,384 records) is full, the record is still published and
  kept in memory, and its disk write is dropped and counted. A segment whose write failed is
  closed where it stopped. It stays readable and retention deletes it in time.
* **Damaged files.** A reader skips a line that does not decode (a corrupt line, a line cut off
  by a crash) and counts it. A missing or unreadable segment is a hole (see section 5).
* **Memory-only buses.** A TUI without `--record`, and a `--headless --mock` run without
  `--bus-dir`, keep the log in memory only.

---

## 5. Recording and replay

### Recording

`--headless` records to the default directory (or `--bus-dir`). The TUI records only when
`--record DIR` or `--bus-dir DIR` is given. A `--mock` run (including auto-detect falling back
to mock) writes to disk only when a directory is named on the command line. Its header says
`backend mock, mode mock` and its readings are tagged `estimated`.

### Inspecting a log

```
tt-toplike-tui --bus-inspect ~/.local/state/tt-toplike/bus
tt-toplike-tui --bus-inspect DIR --entity chip:0000:01:00.0 --signal tt_tdp_w
```

The path is a segment file (plain or `.gz`) or a directory. The summary lists the header, who
recorded it (backend and mode), the record count by kind, the time span, the entities, the
events, and for each signal the sample count with minimum, maximum and last value. The span is
taken from the records' wall-clock times: monotonic times restart with each process, so they
cannot measure a directory written by more than one run. Example from an 8-second
`--headless --mock 2` run:

```
header: schema 1, Live, written by tt-toplike-bus 0.1.0
recorded by: backend mock, mode mock
log: /tmp/smoke-bus
records: 2944 (skipped 0 bad lines)
holes: 0 holes
by kind: sample 2784, sweep 160, event 0, raw 0, gap 0
span (wall clock): 7.935s, 1793629669770 to 1793629677705 ms since the epoch, sequence 1 to 2944
```

Output cut short by a closed pipe (`| head`) is not an error: the command exits 0. It exits 1
when the log cannot be read or the output fails another way.

`--entity` and `--signal` narrow the entity, event and signal sections. The totals at the top
always cover the whole log. A signal filter leaves out the event section.

### Replaying

```
tt-toplike-tui --headless --replay DIR --speed 4 --bus-socket /run/user/1000/tt-toplike/replay.sock
tt-toplike-tui --bus-attach /run/user/1000/tt-toplike/replay.sock
```

A replay host reads no backend. It republishes the recorded records on a new bus with their
original spacing divided by `--speed`, and the bus is marked as a replay (`kind` `Replay`,
`mode` `replay`). The replay bus assigns its own sequence numbers and timestamps. After the
log has played, the host keeps serving until it is stopped, so a reader can attach late.
`--replay` needs `--headless` in this release: the TUI shows chips from a live backend, so a
replay is read in a TUI by attaching to the replay host. A replay cannot record into the log it
reads, so `--replay` is refused when its path is inside the `--bus-dir` or `--record`
directory. A replay with no `--bus-dir` records nothing to disk.

An attached TUI labels a replay in its status bar (`bus: replay`).

### Holes

A hole is a jump in sequence numbers between two records that were read: a deleted or
unreadable segment, or corrupt lines. `--bus-inspect` lists holes under `holes:`. A replay
publishes a gap record from the source `replay` at each hole, so a world built from a damaged
log shows the gap. An unreadable first segment counts as a hole. A deleted last segment cannot
be seen from sequence jumps and is not reported. Only an unreadable last segment is.

---

## 6. The socket protocol

The bus is served on a Unix stream socket. The protocol is JSON Lines in both directions: one
JSON object per line, ended by `\n`. The client sends one request per line. A connection can
make any number of `snapshot` and `raw` requests. A `subscribe` or `replay` request ends the
request phase: the server streams until it is done and sends nothing else on that connection.

### The socket file

The parent directory is created with mode 0700 when it does not exist. An existing parent must
be a directory owned by this user and not writable by group or others, or the server refuses
with `PermissionDenied`. A path directly in `/tmp` is refused for that reason. The socket file
is mode 0600. A symlink at the socket path is refused. A path where another server is
listening is refused. A socket file that nobody listens on is stale and replaced. A path that
holds something other than a socket is refused and never deleted.

### snapshot

```
-> {"op":"snapshot"}
<- {"seq":1124,"instance":962515043239013896,"kind":"Live","host":"box/d2813cb6e53f8ec0","backend":"hybrid","mode":"live","tags":{"backend":"hybrid","mode":"live","saga.season_ticks":"12000","tt-toplike-bus":"0.1.0"},"world":{...}}
```

The reply is one line. `seq` is the sequence number the world is current to. The world holds
every record up to `seq` and none after it. The `world` object is the summary described below.
`tags` carries every header tag of the server's bus. A host that runs the saga, or serves TUIs
that do, records its season there as `saga.season_ticks`. Older servers send no `tags`.

### raw

```
-> {"op":"raw","source":"tt-smi"}
<- {"seq":1159,"source":"tt-smi","body":"{...tt-smi JSON...}"}
```

`body` is `null` when that source has no raw frame (a mock or sysfs backend has none).

### subscribe

```
-> {"op":"subscribe","from":1124}
<- {"subscribed":{"from":1124,"instance":962515043239013896,"host":"box/d2813cb6e53f8ec0","backend":"hybrid","mode":"live"}}
<- {"q":1125,"m":2905677579,"w":1793625045140,"s":"backend","p":{...}}
<- {"q":1126,...}
```

`from` is the sequence number the client is current to (a snapshot's `seq`). The stream starts
at the next record, so a snapshot plus a subscription from its `seq` misses nothing and sees
nothing twice. `"from":"head"` means the newest record now. The first reply line is the
`subscribed` header. The server then sends record lines in the log's own format until the
client closes the connection or an error line ends the stream.

The `instance` is a random id chosen when the bus was opened. A bus reopened on the same log
directory continues the sequence numbers with an empty world, so a client compares `instance`
to the instance in its snapshot. When it differs, the client takes a fresh snapshot.

### replay

```
-> {"op":"replay","from_ts_ms":1793625042235,"speed":100,"entities":["chip:0000:01:00.0"],"signals":["tt_tdp_w"]}
<- {"header":{"schema":1,"kind":"Replay","tool_versions":{"tt-toplike-bus":"0.1.0"}}}
<- {"q":1,"m":60,"w":1793625042235,"s":"backend","p":{"k":"sweep",...}}
<- ...
<- {"done":140}
```

This replays recorded data from the server's own segment files to this client only. Nothing is
published back into the bus, and records keep their recorded sequence numbers and timestamps.
Every field is optional (`from_ts_ms` 0, `speed` 1, no filter). Records whose `t_wall_ms` is
below `from_ts_ms` are skipped. The pause before a record is the difference of recorded
monotonic times divided by `speed`, and each pause is capped at 1 second after that division.
At speed 1, a pause of more than 1 second in the recording is therefore shortened to 1 second.
The cap applies to the socket `replay` op only. `--replay` on a host keeps the original
spacing divided by `--speed`. `speed` must be between 0.01 and 1000. The filters work
as in `bus inspect`: an entity filter keeps samples and sweeps of those entities and the events
that name one. A signal filter keeps samples of those signals, trims sweeps to them, and drops
events. With either filter set, raw frames and gaps are dropped. A bus with no log directory
answers `no_recording`.

### Errors

An error is a single line, and the server closes the connection after it.

| Line | Meaning |
|---|---|
| `{"error":"dropped","oldest":N}` | The record after `from` is no longer kept. `N` is the oldest readable one. |
| `{"error":"future","head":N}` | `from` is past the newest record `N`. |
| `{"error":"bad_request","detail":D}` | `D` is one of `line too long`, `not utf-8`, `not json`, `unknown op`, `bad field`, `bad from`, `bad speed`, `request timeout`. |
| `{"error":"no_recording"}` | `replay` was asked of a bus with no log directory. |
| `{"error":"no_world"}` | `snapshot` or `raw` was asked of a bus with no world. |
| `{"error":"too_slow"}` | Sent, when the socket has room, just before a slow subscriber is disconnected. |
| `{"error":"busy"}` | The server already has its maximum number of clients. |
| `{"error":"io"}` | A segment file could not be read. |

### Limits

* 64 clients at once. The next connection is sent `busy` and closed.
* A request line may be up to 64 KiB. A longer one gets `bad_request` and the connection
  closes.
* A client that sends no request for 10 seconds while the server waits is closed. A
  subscriber is idle by design, so the timeout does not apply once a stream has started.
* A subscriber whose socket has not accepted a write for 100 ms while it is more than 4096
  records behind is cut (`too_slow`). A client that reads steadily is never cut, even while it
  works through a backlog.
* `replay` speed is limited to 0.01 through 1000, and each wait between records is at most 1
  second.
* A client reading the server accepts lines up to 16 MiB.

### The snapshot summary

The `world` object in a snapshot has these fields.

```json
{"now_mono_ns": 2781945405,
 "entities": ["chip:0000:01:00.0", "dram:chip:0000:01:00.0/0", "..."],
 "readings": [{"entity": "chip:0000:01:00.0", "signal": "tt_tdp_w",
               "value": {"f": 39.1}, "prov": "measured",
               "seq": 1091, "t_mono_ns": 2705332161, "age_ms": 76}],
 "models":   [{"key": "model:llama", "label": "llama", "phase": "ready",
               "chips": ["chip:0000:01:00.0"], "progress": null,
               "tokens_per_s": 12.5, "running": 1, "waiting": 0, "kv_cache": 0.4}],
 "events":   [{"seq": 40, "event": {"t": "Reset", "chips": ["chip:0000:01:00.0"]}}],
 "resets":   [{"chip": "chip:0000:01:00.0", "seq": 40}],
 "raw":      [{"source": "tt-smi", "bytes": 18342}]}
```

* `age_ms` is `now_mono_ns` minus the reading's `t_mono_ns`, in milliseconds. A reading is
  never zeroed when its source goes quiet. Its age grows.
* `events` holds the newest 64 events, oldest first.
* `raw` lists the newest raw frame of each source by size only. The `raw` op returns the body.
* `kind`, `host`, `backend` and `mode` on the snapshot line say what the server is. `host` is
  the host name plus a hash of the machine id (`name/<16 hex digits>`). The raw machine id is
  never sent. Older servers send none of these four fields.
* A client that keeps a summary current from a subscription prunes it by the server's rule:
  every 60 seconds of record time it drops processes, leases, training runs and models never
  announced that no record has mentioned for 15 minutes (`SummaryPruner`). Chips, boards, DRAM
  channels, Ethernet ports, tiles and hosts stay.

### The Rust client

The `tt-toplike-bus` crate has a `SocketClient` (snapshot, raw, subscribe) and a `RemoteWorld`
that keeps a snapshot current on a background thread. It takes a fresh snapshot on every
reconnect, retrying after 1, 2, 4 and 8 seconds and then every 10 seconds. Two snapshot requests
start at least 250 ms apart, so a server that ends every subscription at once is not asked in a
tight loop.

---

## 7. Running headless under systemd

`tt-toplike-tui --headless` hosts the bus with no screen. It runs until SIGTERM or SIGINT,
flushes the log, removes its socket file and exits 0. A second SIGTERM or SIGINT exits at once.
The second signal removes the socket file only when this process has bound it and only when the
file at that path still has the device and inode that this process bound. A second signal during
a slow start-up therefore leaves another host's socket in place.
Failures before the loop starts (the backend, a log directory or socket another process holds,
a socket directory that is not private, a socket path too long for a Unix socket) print one
line and exit with status 1. The socket path is checked before the log directory is opened, so
a refused path leaves no log directory, lock or socket directory behind. A failed telemetry
update inside the loop is logged and the loop continues.

It needs no root. It reads the same sources the TUI reads and takes no chip lease: the
telemetry sources (hwmon, sysfs, `tt-smi -s`, `/proc` scans, metrics scrapes) are the
sources the TUI already uses without a lease. The user that runs it must be able to run `tt-smi -s` when
the `hybrid` or `json` backend is used. A mock fallback (no real backend found) is recorded as
`mock`, so name the backend explicitly in a unit file.

An example user unit. The package does not install it. Save it as
`~/.config/systemd/user/tt-toplike-bus.service`.

```ini
# Example only. Adjust the binary path, backend and retention for your box.
[Unit]
Description=tt-toplike signal bus host (records telemetry and serves a local socket)

[Service]
Type=simple
# Paths default to $XDG_STATE_HOME/tt-toplike/bus and
# $XDG_RUNTIME_DIR/tt-toplike/bus.sock, which both exist for a user unit.
ExecStart=/usr/bin/tt-toplike-tui --headless --backend hybrid --bus-retain 24,5
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

```
systemctl --user daemon-reload
systemctl --user enable --now tt-toplike-bus.service
loginctl enable-linger "$USER"      # keep it running without a login session
```

With the unit running, a TUI started by the same user attaches to it by itself (section 8).
`Restart=on-failure` restarts the host after a crash or a start-up failure. A stop through
`systemctl stop` exits 0 and is not restarted. If a previous instance still holds the log
directory lock, the new one exits 1 and systemd tries again after `RestartSec`.

A `--headless` host also records processes and model servers on the box and runs the gozer
status source (for real backends only). It scans the box's processes and model servers every
2 seconds, the TUI's cadence, while the chips are read every `--interval`. It records only the
processes that hold a `/dev/tenstorrent` device or match an inference runtime, as the TUI
does. A model's samples are recorded when their value changes, and all of them again every
5 minutes. `--headless --serve [BIND:PORT]` additionally
broadcasts the `/telemetry` frames that tt-station and `--remote` consume (section 11).

---

## 8. Attaching a TUI to a running host

At start-up a TUI tries the default socket path once, with a 250 ms timeout. When a `--headless`
host answers and its world describes this machine's chips, the TUI reads that host's bus and
hosts none of its own. Chip telemetry still comes from the TUI's own backend. The Wafer Waffler reads the attached world,
limited to the chips the TUI has. The `i` view draws the host's models.

The probe attaches only when all of these hold:

1. The host's identity equals this machine's identity (same host name and machine-id hash).
2. The host's `mode` is `live`, and its header kind is `Live`.
3. At least one chip key in the host's world is a chip of the TUI's own backend.

Containers usually have a different host name from the machine that runs them, so a TUI inside
a container does not attach to a host outside it, even through a forwarded socket. It hosts its
own bus. The same refusal protects a TUI from showing another machine's world on its own chips
when a socket is forwarded over ssh.

The probe is skipped with `--no-bus-attach`, `--bus-socket`, `--record`, `--bus-dir`, and for a
`--mock` or `--remote` backend. In those cases the TUI hosts its own bus. The reason a probe
did not attach goes to the in-app debug log.

`--bus-attach PATH` attaches to `PATH` or fails at start-up (the host must answer within 2
seconds). It attaches even when the host does not match, and the status bar says why:
`bus: <path> (other host)`, `(mock)`, `(replay)` or `(no matching chips)`. A lost connection
adds `(reconnecting)`, then `(disconnected)` once a reconnect has failed. Treat a warning in
that label as a sign that the overlays on screen come from another machine or from synthetic
data. An attached TUI cannot publish on the host's bus, so `ViewChanged` events are not sent.

### The `i` view on an attached TUI

When the TUI is attached, the `i` view builds its rows from the host's world
(`attached_rows`, using `world_to_service_states`) and does not use the local inference
monitor. It lists every model the host knows, including models on chips this TUI does not have.
A chip key becomes a `/dev/tenstorrent/N` index only when the host's origin matches this
machine. The rotation's dwell on `i` (the `v` key) and the arcade activity lanes use the same
rows.

When the link to the host is lost, the view keeps the last world it received. It shows
`(reconnecting)` or `(host disconnected)` above the roster and draws the roster dim. The
creature under the roster is not dimmed.

A model gets `~` in front of its label when one of its label, phase, progress, tokens per
second or waiting readings is `estimated` or `decoration`. One helper makes this decision for both
the `i` view and the Wafer Waffler, so both mark the same models. Every model of a `--mock` host is marked.
A real host's model is marked while it compiles or loads with a progress figure.

The `i` view keeps the local monitor when the TUI hosts its own bus. Section 13 lists what an
attached `i` view does not show.

### The arcade on the bus

The arcade (`a`) reads the bus on every update, as the Wafer Waffler does, through `ArcadeFx::from_world`
(`src/animation/arcade_bus.rs`). Each device gets an equal share of the screen's width, in
device order, and the arcade draws only on the TUI's own chips. It draws:

| What | Signal | Measured, derived or decoration |
|---|---|---|
| A gauge on the `✧ STARFIELD` rule, purple from the left of the chip's share, while a model compiles or loads | `model_on_chip`, the model's phase and `model.progress` | the length is derived from the reported progress |
| A purple band sweeping across the chip's share when a loading model has no progress figure | the phase | decoration: it says the model is busy and does not say how far |
| The share lit teal on a change to `Ready`, grey on a change to `Down`, fading over 2 s | the phase, against the phase the arcade saw last | the phase change is read; the fade is decoration |
| The model's name, phase, progress and tokens/s at the right of the rule, `~` first for an estimate. When no form of the label fits in the chip's share, an estimate still shows a bare `~` there | `model.label`, `model.phase`, `model.progress`, `model.tokens_per_s`, `model.waiting` | as the source reports them; `~` through `model_text_is_estimated`, the helper the Wafer Waffler and the `i` view use |
| A ring of `✦` growing from the hero, one glyph per request up to eight | `RequestsCompleted` for a model on one of the TUI's chips | the event is read; the ring's shape, growth and fade are decoration |
| The snake in the hero duel lunges | the same `RequestsCompleted` events | the event is read |
| A `*` rising in the chip's share of the starfield, in the `spark_rgb` colours (purple for a compile) | `Spark` and `KernelCompiled` naming one of the TUI's chips | the event is read; where it starts and how it rises are decoration |
| The chip's share of the rule shown reversed for 0.6 s | a new `reset_seq` for the chip | the reset is read; the flash length is decoration |
| A faster hero heartbeat | the sum of `model.tokens_per_s` over the ready models on the TUI's chips, when at most 10 s old, smoothed over 2 s | derived from the reported rate |

Events already in the world when the arcade first reads it draw nothing, and so do events that
arrived while the arcade was off screen. The arcade uses the Wafer Waffler's rule and constants for this.
A model re-announced with an unchanged phase changes nothing. Events with no chip (a `Spark`
or `KernelCompiled` with `chip: None`) draw nothing, as in the Wafer Waffler. With an empty world the
arcade draws the frames it drew before the bus; a test pins them by hash.

When the TUI is attached to a headless host, the hero duel's serving stats come from the
host's world, from the rows the activity lanes use: the first ready model on one of the TUI's
chips, or the first ready model when none runs on them (`arcade_bus::duel_serving`). Those rows
carry no completion count, so the snake's lunge comes from the bus's `RequestsCompleted`
events.

#### The arcade with a configured layout

The arcade's rows and panels can be set in the config file (`docs/arcade-layout.md`). The bus
effects then attach to the panels that exist, by these rules:

* **Gauges, model labels and reset flashes** go on the rule above the *gauge panel*: the
  topmost, then leftmost, panel showing `starfield`, `castle`, `defrag` or `die3d`. With none of
  those shown, the first panel. Each chip gets an equal share of the gauge panel's columns, in
  device order. The model label never covers the rule's own labels at its left end. With no
  panel shown at all, none of these is drawn.
* **Spark and compile particles** rise in the starfield panel's rows below the lanes, in the
  chip's share of the starfield's columns. A layout with no starfield draws no particles; they
  are read and dropped.
* **Request hits** grow from the hero, wherever it is. They are not drawn above the first
  content row, or on the lanes when the lanes are in the top row.
* **The heartbeat** and **the duel's lunge** belong to the hero and the header, which every
  layout has.
* **The Wafer Waffler panel** reads the world itself, through `Die3dView::update`, and draws its own
  overlay (flags, fill, pulses, GDDR rows) exactly as the `3` view does. It draws it at the bus
  time of the last update (`Die3dView::render_at`).
* **The Training panel** reads the world through `train_bus`, and **the inference panel**
  draws the rows from `inference_bus::attached_rows` when the TUI is attached, as their own
  views do.

#### The shared backdrop

Every view can draw a backdrop behind what it shows (`docs/backdrop.md`,
`src/animation/backdrop.rs`). What it reads from the bus:

* **Clouds**: `toplike.pcie.in_bps` and `toplike.pcie.out_bps` (drift) and `tt_tdp_w` over the
  chip's TDP (brightness), on this TUI's own chips only, by PCI address. A chip with no reading
  in the last 10 s uses the backend's own telemetry.
* **Sky**: `toplike.train.step_ms` of the newest run (speed), while it is at most 30 s old.
* **Scene**: the same service rows the arcade and the `i` view draw (the attached host's models
  when attached).
* A reading marked `Estimated`, or a model whose text is an estimate, puts `~` before the
  signals on the legend's backdrop line.

The shapes of the sky and the clouds are decoration. A view the last update did not feed starts a
fresh backdrop, so events from while it was off screen draw nothing. With every
setting at its default, every view draws the frames it drew before; tests pin them by hash
(`src/animation/backdrop_pins.rs`, `src/animation/die3d/backdrop_pins.rs`).

The default layout puts the gauge on the `✧ STARFIELD` rule across the whole width, as before.
The brief that asked for this rule named castle, defrag and the Wafer Waffler; the starfield is first in
the list so that the default layout's gauge stays where it was. The default frames, with and
without bus effects, are pinned by hash in `src/animation/arcade_bus_tests.rs`.

A TUI that records (`--record`) or serves (`--bus-socket`) refuses a backend switch (the `b`
key and `/remote`), because the log header names one backend and mode for the whole run. An
attached TUI refuses it too (`backend switch is disabled while attached to a bus host`): a mock
backend would put mock chips at the PCI addresses real boards use, under the host's measured
readings. A memory-only bus follows the switch.

### Memory Castle on the bus

Memory Castle reads the bus on every update through the arcade's reading step,
`ArcadeFx::from_world`, so the two modes share the phase, history and away rules. The drawing
is in `src/animation/castle_bus.rs`. Each chip's tower is its column of the canvas (the whole
canvas with one chip). It draws:

| What | Signal | Measured, derived or decoration |
|---|---|---|
| A purple wing raised from the bottom of the tower, with its empty cells filled `░`, while a model compiles or loads | `model_on_chip`, the phase and `model.progress` | the height is derived from the reported progress |
| A purple band climbing the tower when there is no progress figure | the phase | decoration: it says the model is busy and does not say how far |
| The tower lit teal on a change to `Ready`, grey on a change to `Down`, fading over 2 s | the phase, against the phase seen last | the change is read; the fade is decoration |
| A banner on the tower's top row: name, phase, progress and tokens/s, `⚑` in front while it serves, `~` in front for an estimate (a bare `~` when no form fits) | `model.label`, `model.phase`, `model.progress`, `model.tokens_per_s`, `model.waiting` | as reported; `~` through `model_text_is_estimated` |
| One `♞` per request (up to eight) climbing the tower over 1 s | `RequestsCompleted` for a model on the chip | the count is read; the positions and the climb are decoration |
| A flickering `☼` for 0.8 s, in the `spark_rgb` colours (purple for a compile) | `Spark` and `KernelCompiled` naming the chip | the event is read; the position and flicker are decoration |
| A red `╳` at the bottom of the tower under the channel's eighth of the width, healing through `╱` and `·` over 2 s | `EccTick` on the chip | the event and channel are read; the healing is decoration |
| The tower shown reversed for 0.6 s | a new `reset_seq` for the chip | the reset is read |

In the fleet-grid tier (too many chips for towers) each chip's cell shows the gauge on its power
bar, the reset flash, and `~` for an estimate in the label's pad column (the bar's first column
when the label has no pad); the messengers, torches and cracks are not drawn there. The castle embedded in the arcade reads no bus of its
own. With an empty world the castle draws the frames it drew before the bus; a test pins them.

### Defrag on the bus

Defrag (`d`) is the model-loading view, and the bus drives its blocks directly. It reads the
world through `ArcadeFx::from_world` too, in `DefragVis::set_bus`, and `defrag_bus.rs` holds
the rules and the drawing.

| What | Signal | Measured, derived or decoration |
|---|---|---|
| `COMPILE`: the blocks hold and a purple scan sweeps each channel row | a model in `Compiling` on the chip | the phase is read; the scan is decoration |
| `DMA`: every trained channel filled to the model's progress | a model in `Loading` with `model.progress` | derived from the reported progress |
| `DMA` with the write head at Defrag's own pace | a model in `Loading` with no progress | decoration |
| `RUN` with every channel full, washed teal for 2 s on a change to `Ready` | a model in `Ready` | the phase is read; the wash is decoration |
| `EVICT`: the blocks drain right to left | the chip's model was up and is now `Down` or gone (`ModelGone`) | the change is read |
| A scatter burst across the blocks of a running chip | `RequestsCompleted` for a model on the chip | the brightness follows the count; the cells are decoration |
| One bright block for 0.8 s | `Spark` and `KernelCompiled` naming the chip | the event is read; the place is decoration |
| Name, phase and tokens/s right of the power bar, `~` for an estimate | the model's readings | as reported |
| The chip's label row reversed for 0.6 s | a new `reset_seq` | the reset is read |

A chip with no model keeps the phase its power and clock readings give, as before. The EVICT on
an unload used to come from the local inference monitor; it now comes from the bus, which the
monitor feeds, so an attached TUI sees the host's unloads on its own chips. On a local bus a
model that names no chip and has been ready evicts every chip when it goes `Down` or is gone,
as the monitor's signal did; a restart of its load evicts nothing. An attached TUI does not do
this, since there such a model runs on the host's chips. An eviction compares the phase at two
reads and reads no event, so it also happens after a gap (a return from another view or a
stalled update) and after a reconnect; only the first read evicts nothing. The model's
`model.kernel_count` is not used. With an empty world Defrag draws the frames it drew before
the bus; a test pins them.

### The Starfield on the bus

The Starfield (view `starfield`) reads the bus once per tick, before it reads the telemetry, through
`StarfieldFx::from_world` (`src/animation/starfield_bus.rs`). Each device has an equal share of
the width, as the stars are laid out, and the field draws only on the TUI's own chips. It
draws:

| What | Signal | Measured, derived or decoration |
|---|---|---|
| The chip's stars turn to purple `●` from the grid's centre out to a radius equal to the progress, while a model compiles or loads | `model_on_chip`, the model's phase and `model.progress` | the radius is derived from the reported progress |
| A purple ring sweeping out from the centre when the model has no progress figure | the phase | decoration: it says the model is busy and does not say how far |
| The chip's stars tinted teal on a change to `Ready`, grey on a change to `Down`, fading over 2 s | the phase, against the phase last seen | the change is read; the fade is decoration |
| The model's name, phase, progress and tokens/s on the top row of the chip's share, `~` first for an estimate | the model's readings | as reported; `~` through `model_text_is_estimated` |
| A teal comet from the chip's share across the field, one tail cell per request up to eight | `RequestsCompleted` for a model on one of the TUI's chips | the count is read; the row, speed and fade are decoration |
| Faster twinkle of the chip's stars and faster orbits of its planets | `model.tokens_per_s` of the ready model on the chip, at most 10 s old, smoothed over 2 s | the rate is measured; how much it speeds them is decoration |
| A red `✸` flare on the chip's DDR planet for the channel, for 0.6 s | a rise of the summed ECC counters of `dram:<chip>/<n>`, or `EccTick` | the rise is read; the flare's shape is decoration |
| One of the chip's stars twinkles `✦` in the `spark_rgb` colours (purple for a compile) | `Spark` and `KernelCompiled` naming the chip | the event is read; which star is decoration |
| The chip's stars turn white and fade back over 0.6 s | a new `reset_seq` for the chip | the reset is read |

The fleet galaxy view (32 or more devices) has no per-chip stars and draws nothing from the bus.
The arcade embeds the same starfield and never hands it a world, so the arcade's starfield band
is unchanged.

### MemoryFlow on the bus

MemoryFlow (view `flow`) reads the bus once per tick through `MemoryFlowFx::from_world`
(`src/animation/memory_flow_bus.rs`). Its DDR bars are one segment per channel of the first
device, as before. It draws:

| What | Signal | Measured, derived or decoration |
|---|---|---|
| A DDR segment's state (idle, trained, error) | `toplike.dram.harvested`, `enabled`, `training_pass`, `bist_pass` of `dram:<chip>/<n>` of the first device. Trained needs a training or BIST result; a channel with only a temperature, or none of these, keeps the SMBUS state | measured |
| A DDR segment tinted toward red as the channel gets hotter (40 to 90 C) | `toplike.dram.temp_top_c`, `temp_bottom_c` | the temperature is measured; how strongly it tints is decoration |
| `▸` filling each DDR IN segment from its left edge as far as the progress, while a model loads | the first loading model on one of the TUI's chips, its `model.progress` | derived from the reported progress |
| A short run of `▸` travelling along each DDR IN segment when the loading model has no progress figure | the phase | decoration |
| Both lanes brighten toward teal, fading over 1 s | `RequestsCompleted` for a model on one of the TUI's chips | the count sets the strength (the Wafer Waffler's pulse strength); the fade is decoration |
| A channel's segments shown reversed in red for 0.6 s | a rise of the summed ECC counters of `dram:<chip>/<n>` of the first device, or `EccTick` for it; another chip's rise draws nothing here, since the segments are the first device's channels | the rise is read |
| The model row (the blank line under DDR IN): each chip's model, phase, progress and tokens/s, right-aligned in its share, with a `▸` gauge while it compiles or loads and a teal or grey rule fading after `Ready` or `Down` | the model's readings and phase | as reported; `~` through `model_text_is_estimated`; the travelling band shown with no figure is decoration |
| The chip's share of the model row shown reversed for 0.6 s | a new `reset_seq` | the reset is read |
| A `✦` in the chip's share of the core grid, in the `spark_rgb` colours | `Spark` and `KernelCompiled` naming the chip | the event is read; where it lands is decoration |

For both modes, events already in the world when the mode first reads it, and events that
arrived while it was off screen, draw nothing. An ECC count or `reset_seq` seen for the first
time, or first seen again after being away, is recorded and draws nothing. The rules and
constants are copied from the arcade's, which copies the Wafer Waffler's, into
`src/animation/bus_intake.rs`; the arcade and the Wafer Waffler keep their own code, and a test feeds one
sequence of worlds to `bus_intake` and to the arcade and checks they decide alike. ECC is where
the copies differ: the Wafer Waffler flashes any rise, including one first seen after being away, and the
arcade draws no counter rises. Attached to a headless host, only the TUI's own chips are drawn.
With an empty world both modes draw the frames they drew before the bus; tests pin them by
hash.

### Grid and the Insights chip panels on the bus

Grid (`g`) and the chip panels of the Insights screen read the bus at the data rate, in
`src/ui/tui/grid_bus.rs`, through `TuiBus::read` (limited to local chips when attached). Grid
draws on each chip's cell:

| What | Signal | Measured, derived or decoration |
|---|---|---|
| The model's label on the cell's top row, after the device label, cut with `…` to fit | `model_on_chip`, `model.label` | as the source reports it |
| The phase on the bottom rule, with progress while loading and tokens/s when they fit, `~` first for an estimate | `model.phase`, `model.progress`, `model.tokens_per_s` | as the source reports them; `~` through `model_text_is_estimated` |
| The left bar filling purple from the bottom while a model compiles or loads | the phase and `model.progress` | the height is derived from the reported progress |
| A purple band climbing the left bar when a loading model has no progress figure | the phase | decoration: it says the model is busy and does not say how far |
| The left bar lit teal on a change to `Ready`, grey on a change to `Down`, fading over 2 s | the phase, against the phase Grid saw last | the phase change is read; the fade is decoration |
| A teal light climbing the PCIe spine (`╋`) of a Blackhole portrait, or the left bar of a Wormhole one, once per event | `RequestsCompleted` for the model on the chip | the event is read; the travel and fade are decoration |
| A `*` on one Tensix cell, in the `spark_rgb` colours (purple for a compile) | `Spark` and `KernelCompiled` naming the chip | the event is read; which cell is decoration |
| The portrait shown reversed for 0.6 s | a new `reset_seq` for the chip | the reset is read; the flash length is decoration |

The rule under each Insights chip panel names the model on that chip: label, phase, progress
while loading, tokens/s and waiting requests, `~` first for an estimate (the same text as Die
3D's flag). While the model compiles or loads, the dashes after the text are a progress bar,
purple as far as the reported progress. The Wafer Waffler inset on the Insights screen is the Wafer Waffler's
own drawing and shows the events.

In the Insights process table, a process on chips that serve a model has a serving row under
it. Attached to a host, that row comes from the world's model on the process's chips
(`[bus]`, the phase, `model=` with `~` for an estimate, progress while loading, tokens/s and
waiting requests). On a local bus, and for a process whose chips have no model in the world,
it is the local monitor's row (port, health, model and tokens/s from the server's metrics), as
before.

Grid uses the Wafer Waffler's rule for history: events already in the world when Grid first reads it, and
events that arrived while Grid was off screen, draw nothing. Leaving Grid clears its memory.
Both views draw at the data rate (about ten times a second), so a pulse takes about ten frames.
With an empty world both draw what they drew before the bus; a test pins the screens by hash.

### The `~` view (HivemindSweeper)

The `~` view reads the bus the way the Wafer Waffler does (`TuiBus::read`, limited to local chips when
attached), at the data rate, in `src/ui/tui/hivemind_bus.rs`. It draws three things from it.

* **A strip under the title.** One cell per local chip that runs a model (`model_on_chip`):
  `D<n>`, the Wafer Waffler's flag text (label, phase word, progress, tokens per second, waiting count),
  and an eight-cell bar. The bar follows the reported progress while the model compiles or
  loads and is full once it is ready. The cell takes the phase colour (`phase_rgb`). An
  estimated model starts with `~` (`model_text_is_estimated`). With no model on any chip the
  strip takes no row, and the view draws what it drew before the bus.
* **Bus rows in the feed.** `ModelLoading`, `ModelReady`, `ModelGone`, `RequestsCompleted`,
  `KernelCompiled`, `Spark` and `Reset` become feed rows that start with `bus: `. They coalesce
  like the sweeper's rows and follow the same `s` floor and `f` unified mode. Severity: a
  model coming up is `notice`, a model going away or a reset is `warn`, requests, kernel
  compiles and sparks are `info`. The module doc has the full table. A re-announcement with an
  unchanged phase and chips adds nothing.
* **The KITT scanner's input.** The larger of the board's rate (as before) and a bus rate: the
  smoothed `RequestsCompleted` rate (4 requests a second is full activity) or the summed
  `tokens_per_s` of the local models (100 a second is full), plus a one-second surge when
  requests complete. With no bus signal the scanner moves exactly as before.

Events already in the world when `~` first reads it, and events that arrived while it was off
screen, are history. They appear dimmed under the live rows and give no surge. The rule and
its constant are the Wafer Waffler's.

On a bus this TUI hosts, the view skips the `Spark`, `KernelCompiled` and `Reset` records that
its own sweeper published (record source `hivemind`), because the sweeper's feed already shows
them. The host keeps the sequence numbers of those records (`BusHost::from_hivemind_source`).
The same events from another source show: gozer publishes a `Reset` when it releases a lease
and resets the chips, and the sweeper's reset detector may not have seen it. Attached to a
headless host, nothing is the TUI's own, and every event shows. Events that name only chips this TUI does not have show
nothing. Attached to a host on another machine, no chip counts as local.

The scanner's motion and the surge's shape are decoration that responds to measured rates.
The strip's text and bar come from the reported readings. The collectors still run only
while `~` is on screen.

### The Training view on the bus

The Training view (`t`) reads the bus on every update through `TrainFx::from_world`
(`src/animation/train_bus.rs`), limited to local chips when attached. Views find the run
through `WorldRead::entities(EntityKind::TrainingRun)`: a run key holds the trainer's pid and
start time, so a view cannot build it. The view shows the run whose `toplike.train.step`
reading is newest, while that reading is at most 30 s old.

The world keeps only the latest reading of each signal, so the view keeps its own history of
successive reads. Each read in which the step went up adds one loss point and, when a step
time is known, one step sample. Several steps that land between two reads give one point.

| What | Signal | Measured, derived or decoration |
|---|---|---|
| With no trainer on this machine, the run seen on the bus, drawn with the view's usual look: header, step band, LIVE panel and loss mountains. The title says `TRAINING · bus` | `toplike.train.step`, `max_steps`, `loss`, `step_ms`, `cpu_pct`, `rss_bytes` | as the source reports them. A step time the source marks `Derived` (timed from a progress bar) puts "(from bar)" in the band's title |
| `~bus` in the title | any of the run's readings `Estimated` or `Decoration` | provenance |
| A pulse on the newest columns of the step band when a step lands: amber for a plain step, mint and brighter when the loss fell by more than 1% of its last value, dim coral when it rose by more than 1% | the step going up between two reads, and the loss change | the step and loss are read; the pulse's width (one column per percent of loss change, up to six) is derived from them; its colour, glyph `┊` and fade are decoration |
| The sky's aurora drift and star twinkle at half to twice their usual speed | `toplike.train.step_ms`, fresh | the mapping from step time to speed is decoration |
| `▸` before a chip's tag in the CHIPS row | `toplike.train.chips`, cut to the TUI's own chips | as the source reports it |
| `SAME CHIPS` under the CHIPS row: each model announced on one of the run's chips (not `Down`), with its phase and tokens/s, in its phase colour | `models()` and the run's chips | as the source reports them; `~` through `model_text_is_estimated` |

A local trainer always wins over the bus run: its state has the log path, the config and the
checkpoints, which the bus does not carry. The pulses, the sky's pace, the `▸` marks and the
`SAME CHIPS` strip describe the bus run, so they are drawn on a local trainer's view only when
the bus run is that trainer (the same pid and start time). The `v` rotation also asks the bus
for a fresh run, so an attached TUI visits Training while the host sees a trainer. A step already in the world when the view first
reads it makes no pulse, and neither does a step that landed while the view was off screen.
The rule and its constants are the Wafer Waffler's. With an empty world the view draws the frames it drew
before the bus; a test pins them by hash.

### Saga on the bus

Saga (`S`, [docs/saga.md](saga.md)) reads the record stream itself. It does not read the world.
A driver thread (`src/saga/driver.rs`) feeds every record, in order, to the saga core
(`crates/tt-toplike-saga`): from `Bus::read_from` on a TUI that hosts its bus, starting at that
bus instance's first record (`Bus::first_seq`, so a reused `--bus-dir` does not replay earlier
sessions into the story), and from the socket's `subscribe` on an attached TUI, which starts from
the host's snapshot. The core reads
only each record's source and payload, so the same records give the same saga whatever their
sequence numbers and times. `--bus-inspect DIR --saga-digest` prints that saga's digest for a
recorded log.

Saga publishes nothing to the bus in this release. Phase 2 adds `saga.*` events (a world born or
ended, a hero born or dead, a quest, a beat) as one new additive event variant, with source
`saga`, and the bus crate moves to 0.2.0. The core already skips records from source `saga`, so
a recording that carries them will not tell its story twice. `ViewChanged` names the view
`saga`.

---

## 9. Privacy

Process command lines are written to the log and served on the socket, so every command line,
process name, model label and gozer `who` string goes through `redact_cmdline` and is clipped
to 512 characters (128 for `who`) before it is recorded. Hivemind log text never goes on the
bus. A `Spark` carries only a fixed kind string. The log directory is created with mode 0700
and the files with 0600.

### What redaction hides

* The value after a secret flag, in the spaced and `=` forms. A flag, header, JSON key or query
  name is secret when one of its words (split at `-`, `_` and case changes) is `token`,
  `password`, `passwd`, `pwd`, `pass`, `passphrase`, `secret`, `apikey`, `credential(s)`, `key`,
  `auth` or `bearer`, or its words joined end with `token`, `key`, `secret`, `password`,
  `passwd`, `auth`, `pass` or `pwd`. `--max-num-batched-tokens`, `--tokenizer-mode`,
  `--ssl-keyfile` and `--keep-alive` are kept.
* `NAME=value` where `NAME` contains `TOKEN`, `KEY`, `SECRET`, `PASSWORD`, `PASSWD`,
  `CREDENTIAL`, `AUTH`, `PRIVATE` or `COOKIE`, or has `PAT`, `PASS`, `PWD` or `PASSPHRASE` as a
  whole word. `PATH` is kept.
* The value after `-p` and an attached `-pVALUE` (including an attached password that begins
  with a secret word, such as `-pwdXy7`), except port mappings (`8000:8000`) and the
  compiler flags `-pthread`, `-pedantic`, `-pipe`, `-pie` and `-pg`.
* The password in `-u user:pass` and `--user user:pass`.
* The word after `login`, and the word after `Bearer`.
* Header values for header names containing `key`, `token`, `auth`, `secret` or `cookie`.
* In a URL, the userinfo (`https://user:pass@host` becomes `https://<redacted>@host`) and query
  values whose name looks secret.
* Values for secret keys in JSON text or a Python dict. A value that is a nested object or array
  is masked as a balanced unit, with brackets inside quoted strings ignored and `{` matched only
  by `}` and `[` only by `]`. A value whose brackets do not match, or that nests more than 64
  levels, is masked to the end of the line.
* Anywhere in the text: `hf_` plus 16 or more characters, `sk-` plus 16 or more, GitHub token
  prefixes (`ghp_`, `gho_`, `ghs_`, `ghu_`, `ghr_`, `github_pat_`), `glpat-`, Slack token
  prefixes (`xoxb-`, `xoxp-`, `xoxa-`), `AKIA` access key ids, and JWTs.

A hidden value is replaced with `<redacted>`. Quotes around it are kept. A service key that
changed during redaction gets a hash suffix of 8 hex digits so two services whose keys differ only in
the hidden part stay two entities. The hash is salted with a random number drawn once per
process, so the suffix cannot be matched against hashes of guessed secrets, and it differs from
one host run to the next. A replay or `--bus-inspect` of a log that spans a host restart shows
the old and the new key as two entities.

### Known false positives

Redaction hides more than it has to. These harmless values are hidden:

* `--first-pass`, `--bypass`, `--compass` and `--monkey` (the flag name ends in `pass`, or
  contains `key`) and the environment variable `COMPASS=x`. `--compass-mode` is kept.
* Environment names such as `TOKENIZERS_PARALLELISM` or `AUTHOR`.
* `ssh -p 2222` and `docker -p 8000` (a bare port after `-p`), the word after `docker login`,
  and any `-p...` word not on the keep list above. `-pass` and `-passin` show as
  `-p<redacted>`.
* A secret value that is a nested object or array hides every word up to the closing bracket. A
  stray quote after the opening bracket, brackets that do not match, or more than 64 levels hide
  the rest of the line, so later arguments of that process can be missing from the record.
* A URL such as `https://h:8000/u?e=a@b` loses its host and path.

### Known residual leaks

Redaction is a pattern matcher. These shapes are known to get through:

* A secret that contains whitespace and sits among mis-paired quotes: the second word is not
  hidden.
* `HF_TOKEN = S` with spaces around the `=`.
* A `--remote` relay records the origin's raw frame as it arrives. That frame carries process
  names and model labels that the origin already redacted. This host does not redact them
  again.
* The raw `tt-smi` snapshot is stored verbatim. It holds board serial numbers and firmware
  versions.

A secret placed in a command line can be recorded if its shape is not one of the above. Pass secrets through a file or the
environment of a process the bus does not list, and keep the log directory private.

---

## 10. The governor and gozer

`gozer` is the box's chip lease manager. The bus runs two read-only commands:
`gozer status --json` (every 2 seconds, on a worker thread, with a 2 second timeout) and
`gozer env <lease> --json`. It never runs `acquire`, `run`, `release`, `reset` or anything that
opens a chip, and it never reads gozer's own files. Neither command opens
`/dev/tenstorrent/*`.

### GozerSource

The `gozer` source emits records from successive status snapshots:

* A `toplike.gozer.state` sample on a chip when its state changes, and a `toplike.gozer.who`
  sample when the holder text changes (empty text when it clears).
* `LeaseGranted` when a holder appears on a chip. The first snapshot after start emits no
  `LeaseGranted`, because the lease was taken before the bus started watching.
* `LeaseReleased`, then `Reset` and a gap record, when a holder goes away. Gozer resets the
  chips on release, so readings taken just after may be wrong.
* A chip that turns `UNREADABLE` or `DEGRADED` keeps its previous holder, so a flapping chip
  gives no release and no second grant.
* When gozer is absent or fails, nothing is emitted and the last snapshot is kept. A failed
  poll does not fake a release.

Gozer's status does not expose the lease id. A lease is named `pid-<owner pid>`, or
`who-<who>` for a detached lease (a bare `gozer acquire`, which has no owner pid).

The source runs in a `--headless` host and in a TUI on a real backend. It is off for `--mock`,
`--host` and `--remote` backends, because gozer describes this box's real chips.

### The governor

The `Governor` trait answers one question: may a source that opens a chip on its own account
read this chip now? No source in this release opens a chip. The hwmon, sysfs, `tt-smi -s`,
`/proc` and metrics sources need no lease. The first user of the governor is the sidecar
(section 12). The implementation, `GozerGovernor`, permits a chip only when all of these hold:

1. `TT_TOPLIKE_LEASE_ID` is set to a plain lease id: 1 to 64 characters from `A-Za-z0-9_-`,
   not starting with `-`. The launcher of the chip-opening process sets this variable. Gozer
   itself does not set it. Gozer builds a file path from the id, so anything else is refused
   before a command runs.
2. `gozer env <lease> --json` lists the chip's PCI address.
3. `TT_VISIBLE_DEVICES` in the process's own environment lists the chip. Gozer writes a comma
   list of PCI addresses. A plain number is read as a device index.
4. `gozer status --json` shows the chip in state exactly `HELD`.

Anything else denies. When a governor denies, the source emits nothing for that tick and the
world shows its values with a growing age.

What `HELD` proves depends on the lease. For a lease with an owner pid (`gozer run`, or
`acquire --owner-pid`), `HELD` means the owner or its descendants have a device open, and
rule 2 says the chip is in our lease, so the chip is in use by our lease's process tree. For
a detached lease, gozer reports `HELD` when any process has the chip open, including another
tenant's, because gozer cannot tell whose it is. There `HELD` means that something has the
chip open. It does not show that the workload of the lease is running. `CLAIMED` (lease taken,
nothing open yet) denies.

The governor re-runs its commands at most every 30 seconds and tolerates one failed refresh:
it keeps an older answer for up to 60 seconds and then denies. If `gozer env` says the lease
does not exist, it denies at once. A call to `permit` can block for about 4 seconds on a
refresh, so it belongs on the caller's own thread. The sidecar's rule to exit on the first
failed 30-second re-check is separate and the sidecar must implement it itself.

---

## 11. tt-station compatibility

The `/telemetry` WebSocket frames that tt-station (and `--remote`) consume keep their format.
The frame is the newest `tt-smi -s` JSON with one additive top-level key, `tt_toplike`
(`{schema, processes[], inference[]}`).

The bus carries each tt-smi snapshot verbatim as a `raw` record from the source `tt-smi`, next
to the typed samples taken from it. The frame is rebuilt from that raw record and the
`tt_toplike` extension, and matches the frame the code produced before the bus, byte for byte
on a fixed test fixture. `--serve` (with a TTY or without) builds its frames this way.

The relay rule: a `--remote` box re-broadcasts the origin's frame verbatim and never adds its
own extension. A `tt_toplike` key in a relayed frame is the origin's own. A relay's processes
and models come from the remote frame. Local monitors are not used.

`--serve` needs the `json` or `hybrid` backend, because only those keep a raw tt-smi snapshot.

---

## 12. The sidecar (not in this release)

The sidecar is not part of this release. No code for it is shipped, and nothing in tt-toplike
opens a chip on its own account.

The sidecar is a later phase, off by default. Per-tile NoC NIU counters are the only signal
that says where on the chip traffic goes. They would come from a Python sidecar built on
tt-exalens, started with an explicit flag, local only. It writes raw samples to the bus over a
pipe and the Rust side derives rates. The rules and measurements below come from hardware
tests done for the design. They are written down here so the sidecar is built to them.

### Measurements

Measured on a QuietBox 2 (two p300c boards, tt-exalens 0.4.2, tt_umd 0.9.14): a 10 Hz sweep of
156 tiles per chip costs about 8 to 17 ms per chip sweep and about 15 to 19 percent of one core.
With the reader running beside a ttnn matmul loop and beside a vLLM serve of Llama-3.1-8B,
throughput and latency did not change by more than the noise (about 1 percent for matmul and
about 3 percent for the serve). The counters show load starting within one 200 ms sample, and
they separate prefill from decode: decode puts DRAM traffic almost entirely on NoC0.

Round 2 (adversarial, same box, Llama-3.1-8B in docker, reader at 10 Hz on both chips of one
board) could not break the reader:

* Six SIGKILLs of the reader under load: the server kept serving and every fresh reader
  attached.
* 12 of 12 greedy-decoding output comparisons were byte-identical with the reader off and on.
* A reader stopped for 60 s mid-sweep changed nothing for the server. On resume its 2 s
  watchdog exited it, a harmless false hang.
* A 25-minute soak: 30,524 sweeps, no errors, flat memory, 15 to 19 percent of one core. Two of
  five throughput blocks were about 6 percent lower, and a reader-off run straight after
  matched the higher blocks, so the dip is not attributable to the reader.
* Cold bring-up with the reader attached from the start: ready in 293 s against 272 s without
  it (one trial).
* A restart without a chip reset failed on an Ethernet core timeout 2 of 2 times with no reader
  attached, so that failure is not the reader's.

One case is unsafe for the workflow: a live reader makes the supported `gozer reset` and
`gozer release` refuse ("chips still open by [pid]"). With `--force` the reset worked and took
the same 42 s as without a reader, and the reader exited on its own within about a second. The
sidecar must be gone before a reset or release can proceed.

### Rules

* Reads only. No writes, halts, resets, performance-counter control or ARC messages.
* Single-word reads, never bursts.
* Tensix, DRAM and Ethernet tiles only. Harvested tiles skipped. Never ARC, PCIe, L2CPU or
  security.
* At most 10 Hz, 5 Hz by default. A 2 s timeout per read. Stop on any error with no retry. A
  slow sweep skips a tick and the next sweep starts on schedule.
* Start only when `NOC_LEASE_ID` and `TT_VISIBLE_DEVICES` are set, the lease is active with its
  owner alive, and `gozer status --json` shows the chips held under the same lease. Re-check
  every 30 s and exit if the check fails. The user can override with a warning.
* Exit within seconds when the lease is released or a reset is requested (the bus publishes the
  `Reset` and `LeaseReleased` events the sidecar watches for), because a live reader blocks the
  supported reset and release.
* Start after a server is ready. A cold bring-up with the reader attached completed in the
  single trial run, which is not enough evidence to make that the default.
* Detach before any chip reset or server restart. A restart without a reset can fail on an
  Ethernet core timeout whether or not a reader is present.
* Record the exalens, UMD and firmware versions in the log header. Record a gap marker after a
  reset.

### Not yet proven

* A reset by another user or tool while a model is serving and the reader is attached.
* Models other than Llama-3.1-8B.
* Output identity on long or 32-way batches.
* Soaks longer than 25 minutes.

The sidecar can later be replaced by a Rust binding to tt-umd without changing the schema.
Until the sidecar exists, the Wafer Waffler tile glow stays an estimate and says so.

---

## 13. Limits and known issues

### What was and was not tested

* No hardware run of this release. Tests use the `Fake` and mock backends and checked-in
  fixtures. The headless host, recording, inspect, replay and the socket were exercised against
  `--mock`. The tt-smi, gozer and inference-server code paths were tested against recorded
  output and test doubles.
* The tile glow in the Wafer Waffler is an estimate and is labelled as one.
* The GDDR row layout in the Wafer Waffler follows channel order (channels 0 to 3 on the west side, 4 to 7
  on the east side). It is not a measured die position. The sides are documented (tt-metal and
  tt-umd Blackhole descriptors place channels 0 to 3 on the west NoC column and 4 to 7 on the
  east). The order of rows within a side is not documented. Each channel is a bank of three
  NoC tiles spread over its column. That the tt-smi channel index equals the bank index is
  inferred. It is not documented. A hardware check would settle it: read DRAM tile NIU counters
  at the bank endpoints during a workload pinned to one bank. That check has not been run.
* Training amber is not reachable in the Wafer Waffler: the model phases have no training phase, and a
  training run is a `run:` entity.

### Log and world

* A full writer queue drops the disk write without publishing a gap record. The drop is
  counted in the bus statistics.
* The in-memory window must be larger than the writer queue (the defaults are 65,536 and
  16,384; a TUI that serves no socket uses 4,096 and 2,048). A tiny window can report
  spurious `dropped` errors.
* Records lost from the end of a log (a deleted last segment) cannot be detected.
* After a late lock moves the head forward (the log directory already holds higher sequence
  numbers), the in-memory window keeps a gap in its numbering. A socket subscriber reading
  across the jump can stall or skip records until the older records leave the window. Local
  views are unaffected. The trigger is rare: a disk outage followed by a lock retake.
* Only a test (in `src/cli.rs`) checks `window_records > writer_queue`. The bus config does not enforce it.
* A compress error repeats at every rotation. Retention does not run on a segment whose
  metadata cannot be read.
* If the open cannot list the log directory, the bus numbers from 1 in memory. When the lock is
  taken later, records whose numbers the directory already holds are not written; the head
  jumps past the newest record on disk and a gap record says so.
* The `WorldRead` trait's `events_since` (what views read, local or attached) returns at most
  the newest 64 events, with no overflow indicator. The world itself keeps the newest 512
  (`World::events`).
* Process entities are never retired by an event. The world drops them after 15 minutes
  without a reading. Process names, command lines, model announcements and model samples are
  sent when they change and again every 5 minutes, so segment retention cannot remove the only
  copy.
* The first poll after start emits no `LinkChanged`, so a change that happened while the bus
  was down is not reported. `LinkChanged` carries 16 bits of a 64-bit mask. The sample
  `toplike.eth.live_mask` has all of it.
* A reducer panic is caught and the bus keeps running. A quiet panic hook keeps the message
  off a raw-mode terminal. It must be installed after the TUI's own hook.
  CI runs the `tui_panic_hook` integration test that guards this order.

### Socket

* A subscriber costs two threads. A snapshot copies the whole world, raw bodies included,
  under the publish lock.
* A socket replay at speed 1 plays a pause longer than 1 second as 1 second, and a replay at
  speed 0.01 of a 59-second recorded gap waits 1 second.
* A socket path whose parent is missing, where the nearest existing ancestor is not writable by
  the user, passes the pre-check and fails later.
* The client's start-up probe uses a blocking connect with no timeout of its own. A full
  listen backlog can stall it.
* The socket is marked as this process's after a successful bind and unmarked before the server
  finishes its shutdown. A second signal in either small window can leave a stale socket file.
  The next start replaces a stale socket.
* `--bus-inspect` prints only the first segment's header. Directories are refused when the
  mode or the backend class changes, so one directory holds one mode and class. The refusal
  counts an unrecognised backend tag as local-hardware, and a refusal after a late lock lasts
  for the rest of the run.
* A `--remote` relay of a mock box is recorded as backend `remote`, mode `live`, measured.
* A replay or inspect of a log that spans a host restart shows the old and the new salted key of
  a redacted service as two entities.
* Retention can drop the process text of an old segment. The host sends it again every 5
  minutes.

### Attach and views

* A TUI inside a container does not attach to a host outside it (different host name).
* The host identity is an unsalted FNV-1a hash of the machine id.
* When the host is lost, the rotation keeps dwelling on the host's last rows.
* An attached model shows dashes for the TTFT breakdowns (queue, prefill, decode, per-token) and
  for the prefix hit rate. The bus carries only the mean time to first token.
* An estimated running count shows without `~` in the `i` roster line. The mark follows the label,
  phase, progress, tokens per second and waiting readings, and the running count is not one of
  them.
* The `i` view keeps the local monitor when the TUI hosts its own bus.
* The TUI's bus label can be dropped at very narrow widths (the label is dropped when fewer than
  5 characters remain after the camera caption), so a mismatch warning such as `(other host)` can go silent.
* After the second update at a slow `--interval` (for example 2000 ms), the Wafer Waffler treats the first
  2 s gap as "away" once and drops that update's events. A data stall longer than 1 second at a
  100 ms tick does the same.
* The Wafer Waffler legend and explain text list three phase colours and omit Down (grey).
* The Wafer Waffler cannot tell a restarted host from the old one when the new sequence number is already
  past the old one, because `WorldRead` carries no instance id.
* After a memory-only backend switch, readings for signals the new backend no longer emits keep
  the old provenance until they age out.
* A model whose chips are all foreign (in an attached world) appears with an empty chip list.
  Views skip it.

### Gozer and governor

* The governor can be up to 60 seconds stale after a failed refresh.
* Two leases with one owner pid can swap on a chip within 2 seconds with no release event.
  Two detached holders with the same `who` back to back within 2 seconds can miss a release.
* A release after the source restarts can be reported for a lease that was never granted on
  this bus. Consumers tolerate it.
* A `Reset` event after `gozer release --no-reset` is not true. The accompanying gap is
  conservative.

### Hardware-testing rules learned

* A live reader on a chip blocks `gozer reset` and `gozer release`.
* A restart without a chip reset can fail on an Ethernet core timeout whether or not a reader
  is present.

---

## 14. For consumers (tt-station and tensixinet)

### tt-station

* Keep using the `/telemetry` frames. They are unchanged (section 11).
* To read the bus: connect to the Unix socket, send `snapshot`, and send `subscribe` with the
  snapshot's `seq`. Compare `instance` and `host` on the reply to know which bus and machine
  you are reading. Take a new snapshot when a subscription ends or the `instance` changes.
* A tt-station that wants history can use `replay` on the same socket.

### tensixinet

* Subscribe to the socket, or read the segment files. They are JSON Lines and gzip, readable
  from Python (`gzip.open(path, "rt")`, then `json.loads` per line). Skip a line that does not
  decode.
* The signals worth mapping to sound are the fast-lane chip signals (`tt_tdp_w`,
  `tt_asic_temperature_celsius`, `tt_ai_clock_mhz`, `toplike.pcie.*`), the per-channel DRAM
  signals, the model signals (`model.tokens_per_s`, `model.running`, `model.waiting`,
  `model.kv_cache`, `model.phase`) and the events `RequestsCompleted`, `ModelLoading`,
  `ModelReady`, `Reset` and `Spark`.
* Item 7d on tensixinet's roadmap asks for the name of the view on screen. `ViewChanged`
  carries it. The names are `insights`, `grid`, `starfield`, `castle`, `flow`, `arcade`,
  `defrag`, `inference`, `hivemind`, `training`, `die3d` and `saga`. A TUI that is attached to a
  headless host does not publish `ViewChanged`, so that name is available from a TUI that
  hosts its own bus (started with `--bus-socket`).
* Check `prov` on every value. Anything `estimated` is a guess, and a `--mock` run tags all of
  its readings that way. Check `mode` on the snapshot: `mock` and `replay` are not live
  readings.
