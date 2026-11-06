# Signal Bus Implementation Plan (first release)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A recorded, replayable stream of every signal tt-toplike sees, read by Die 3D and `i`, served on a socket, hosted by the TUI or a headless process.

**Architecture:** A library crate `tt-toplike-bus` (workspace member) holds the record types, an append-only log with a single writer, a world reducer, replay, and transports. The main crate gets source adapters that poll the existing backend and monitors and publish to the bus, a governor trait, and the two views. No existing backend, monitor or view changes behaviour except Die 3D and `i`, which read the bus.

**Tech Stack:** Rust 1.93, edition 2021, serde and serde_json (already present), ratatui 0.30, `std::sync` and `std::thread` (no async runtime in the bus crate), one compression crate chosen in Task 2.

**Spec:** `docs/superpowers/specs/2026-11-01-signal-bus-design.md` (read it first, including "Decisions from the builder check").

## Global Constraints

- Bump the version with every task that changes behaviour. The workspace root starts at 0.14.5. The first release is 0.15.0, set in Task 13. The bus crate is 0.1.0.
- Default branch is `main`. Work is on `what-if`. Do not push, merge or publish.
- Left and bottom borders only in anything drawn. No right-hand border characters.
- No hardware in any test. Tests use `Fake`/`MockBackend` and checked-in fixtures. Never open `/dev/tenstorrent`, run `tt-smi`, or import `ttnn`.
- Plain-English docs. Before committing prose, grep it for `, not `, `rather than`, `instead of`, `reads as`, `honest`, `the one`, `which is why`, `This is what`.
- Every moving thing in a view is `Measured`, `Derived`, `Estimated` or `Decoration`, and the data says which.
- Chip identity is the PCI address (`Device.bus_id`). Never index per-device data by loop position. Records from index-only sources go through the index-to-key map.
- Existing pinned frame hashes (`GOLDEN_*`, `OUTSIDE_PANEL_*` in `src/animation/die3d/perf_tests.rs`) must not change, except the new hashes added in Task 11 for bus-decorated frames.
- CI commands in `.github/workflows/ci.yml` stay green: `cargo fmt --check`, `cargo clippy --locked --lib --bin tt-toplike-tui --features tui -- -D warnings`, `cargo test --locked --lib --features tui`, and the no-default-features build. Task 1 adds the bus crate to them.
- A test is not done until it has been seen failing against the bug it guards (reintroduce, watch it fail, restore).

## Review Focus

The inputs and conditions the spec implies and that a person using this will meet. Each has a test in the task named.

1. A source stops responding or fails for a minute: the world shows its values with a growing age and never as zero (Task 3, Task 6).
2. The log directory is full, unwritable or the disk runs out mid-segment: the bus keeps serving live data, records a gap marker, and the TUI does not freeze or crash (Task 2).
3. The process list contains a secret in a command line (`HF_TOKEN=...`, `--api-key`): it never reaches the log or the socket (Task 7).
4. A consumer subscribes from a sequence number that has already been dropped from memory and disk, or from the future: it gets a clear error, not a hang or a silent skip (Task 2, Task 9).
5. Two chips swap `/dev/tenstorrent/N` indices after a reset or hotplug: records from index-only sources land on the right chip (Task 5).

---

### Task 1: Workspace and record types

**Files:**
- Modify: `Cargo.toml` (add `[workspace] members = ["crates/tt-toplike-bus"]`, add the path dependency), `Cargo.lock`, `.github/workflows/ci.yml` (test and clippy the bus crate), `build-deb.sh` and `debian/rules` only if the workspace breaks them.
- Create: `crates/tt-toplike-bus/Cargo.toml`, `crates/tt-toplike-bus/src/lib.rs`, `src/record.rs`, `src/entity.rs`.
- Test: `crates/tt-toplike-bus/src/record.rs` (module tests).

**Interfaces:**
- Produces:
  - `pub struct EntityKey(String)` with constructors `chip(bus_id: &str)`, `board(serial: &str)`, `tile(chip: &EntityKey, noc0_x: u8, noc0_y: u8)`, `model(key: &str)`, `process(pid: u32, start_ticks: u64)`, `lease(id: &str)`, `host(name: &str)`, `dram_channel(chip: &EntityKey, ch: u8)`, `eth_port(chip: &EntityKey, port: u8)`, `training_run(key: &str)`.
  - `pub enum Provenance { Measured, Derived, Estimated, Decoration }`.
  - `pub enum Value { F64(f64), U64(u64), I64(i64), Text(String) }`.
  - `pub struct Sample { entity: EntityKey, signal: String, value: Value, prov: Provenance }` and `pub struct Sweep { entity: EntityKey, signals: Vec<String>, values: Vec<Value>, prov: Provenance }` (one chip per tick, or one tile sweep).
  - `pub enum Event { ModelLoading{model: EntityKey, chips: Vec<EntityKey>, progress: Option<f32>}, ModelReady{..}, ModelGone{..}, RequestsCompleted{model: EntityKey, n: u32}, KernelCompiled{chip: Option<EntityKey>, source: String}, Reset{chips: Vec<EntityKey>}, LinkChanged{chip: EntityKey, mask: u16}, LeaseGranted{..}, LeaseReleased{..}, EccTick{chip: EntityKey, channel: u8}, ViewChanged{view: String}, Spark{chip: Option<EntityKey>, kind: String} }`.
  - `pub struct RawFrame { source: String, body: String }` (the verbatim tt-smi snapshot) and `pub struct Gap { reason: String }`.
  - `pub enum Payload { Sample(Sample), Sweep(Sweep), Event(Event), Raw(RawFrame), Gap(Gap) }`.
  - `pub struct Record { seq: u64, t_mono_ns: u64, t_wall_ms: u64, source: String, payload: Payload }` with `to_line(&self) -> String` (one JSON line, no newline inside) and `from_line(&str) -> Result<Record, DecodeError>`.
  - `pub const SCHEMA_VERSION: u32 = 1;` and `pub struct Header { schema: u32, kind: HeaderKind /* Live | Replay */, tool_versions: BTreeMap<String,String> }`.

- [ ] **Step 1:** Write failing tests: every `Payload` variant round-trips through `to_line`/`from_line`; a line never contains a raw newline even when `Text` or `RawFrame.body` contains one; `from_line` of a truncated line returns `Err`; `EntityKey::tile` includes the chip key and both coordinates; a missing reading has no representation (there is no `Value::None`).
- [ ] **Step 2:** Run `cargo test -p tt-toplike-bus`; expect compile failure (types missing).
- [ ] **Step 3:** Create the crate and types with serde derives; JSON field names are short and stable and documented in the module doc.
- [ ] **Step 4:** Convert the root `Cargo.toml` to a workspace with the root package unchanged; confirm `cargo build --locked --bin tt-toplike-tui --features tui` still builds the same binaries and `cargo test --locked --lib --features tui` still passes. Add `cargo test --locked -p tt-toplike-bus` and `cargo clippy --locked -p tt-toplike-bus -- -D warnings` to CI. Check `build-deb.sh` and `debian/rules` still work (read them; run the deb build only if it is quick and offline).
- [ ] **Step 5:** Commit: `feat(bus): workspace and record types`.

### Task 2: The log, segments, retention

**Files:**
- Create: `crates/tt-toplike-bus/src/log.rs`, `src/segment.rs`, `src/config.rs`.
- Modify: `crates/tt-toplike-bus/Cargo.toml` (one compression crate; prefer a pure-Rust one; record the choice and why in the module doc; run `cargo audit` locally if available).
- Test: module tests with `tempfile`-style temp directories under `std::env::temp_dir()`.

**Interfaces:**
- Consumes: Task 1 types.
- Produces:
  - `pub struct BusConfig { dir: Option<PathBuf>, window_records: usize, segment_secs: u64, retain_secs: u64, retain_bytes: u64 }` with `Default` (dir `None` means memory only; defaults 10 minutes per segment, 24 h, 5 GB).
  - `pub struct Bus` (cheap `Clone`, all clones share one log): `open(BusConfig) -> io::Result<Bus>`, `publish(&self, source: &str, payload: Payload) -> u64` (returns the assigned `seq`; never blocks on disk, never panics), `head(&self) -> u64`, `read_from(&self, seq: u64, max: usize) -> Result<Vec<Record>, ReadError>`, `subscribe_from(&self, seq: u64) -> Result<Subscription, ReadError>` with `Subscription::next(&mut self, timeout: Duration) -> Option<Record>`.
  - `pub enum ReadError { Dropped { oldest: u64 }, Future { head: u64 }, Io(io::Error) }`.
  - `Bus::stats(&self) -> BusStats { head, dropped_to_disk_errors: u64, segments: usize, bytes: u64 }`.

- [ ] **Step 1:** Write failing tests: sequence numbers are gapless and strictly increasing with four threads publishing 1000 records each; `read_from(n)` after `head()=m` returns exactly records n..m in order; `subscribe_from(head)` sees only later records, exactly once; `read_from` below the oldest kept record returns `Dropped{oldest}`; above head returns `Future{head}`; segments rotate on `segment_secs` (use an injectable clock) and the closed segment is compressed; retention deletes the oldest segments past `retain_secs` or `retain_bytes` and never the open one; a segment with a corrupt middle line and a truncated last line is read up to and past the corruption, skipping the bad lines and counting them; the log directory is created with mode 0700 and segment files 0600 (unix only); a read-only directory makes `open` return `Ok` with disk off, `stats().dropped_to_disk_errors` rising, a `Gap` record published once, and live reads still working (Review Focus 2).
- [ ] **Step 2:** Run them; expect failures.
- [ ] **Step 3:** Implement `Bus` with one writer thread fed by a bounded channel (publishing assigns the `seq` and timestamps under one lock so order matches the log, then hands the record to the writer; a full channel drops the disk write and counts it, it never blocks the caller). Memory holds the last `window_records` records. Segment files are `bus-<first_seq>.jsonl` with a header line, compressed to `.jsonl.<ext>` on rotation.
- [ ] **Step 4:** Run the tests; confirm each failing-first test fails when its fix is removed (gapless: remove the lock; retention: skip the delete; permissions: skip the chmod).
- [ ] **Step 5:** Commit: `feat(bus): append-only log, segments, retention`.

### Task 3: The world reducer

**Files:**
- Create: `crates/tt-toplike-bus/src/world.rs`.
- Test: module tests.

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces:
  - `pub struct World` with `apply(&mut self, rec: &Record)`, `seq(&self) -> u64`.
  - `pub struct Reading { value: Value, prov: Provenance, seq: u64, t_mono_ns: u64 }` and `world.reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading>`, `world.age_ms(&self, r: &Reading, now_mono_ns: u64) -> u64`.
  - `world.entities(&self, kind: EntityKind) -> impl Iterator<Item=&EntityKey>`.
  - `world.models(&self) -> Vec<ModelView>` where `ModelView { key: EntityKey, label: String, phase: ModelPhase /* Down|Compiling|Loading|Ready|Alarm */, chips: Vec<EntityKey>, progress: Option<f32>, tokens_per_s: Option<f64>, running: Option<u32>, waiting: Option<u32>, kv_cache: Option<f64> }`.
  - `world.model_on_chip(&self, chip: &EntityKey) -> Option<&ModelView>`.
  - `world.events(&self) -> &VecDeque<(u64 /*seq*/, Event)>` bounded to the last 512, and `world.events_since(seq)`.
  - `Bus::snapshot(&self) -> Snapshot { seq: u64, world: World }` (built from the in-memory window plus the latest values; documented: a snapshot never needs the segments).

- [ ] **Step 1:** Failing tests: a `Sample` sets the latest reading and keeps its provenance; a `Sweep` applies every value; reading a signal never published is `None`; a source that goes quiet leaves its readings with growing `age_ms` and no zero value appears (Review Focus 1); `ModelLoading` then `ModelReady` then `ModelGone` moves the phase and removes the model's chip association on `ModelGone`; `Reset` marks affected chips' readings stale-before-reset (their `seq` is below the reset's and `world.reset_seq(chip)` is set); applying the same records in the same order from a replayed log gives an equal `World` (`PartialEq` on a normalised view).
- [ ] **Step 2-4:** Implement; run; confirm the stale test fails if `age_ms` is clamped to zero.
- [ ] **Step 5:** Commit: `feat(bus): world reducer and snapshot`.

### Task 4: Replay, record and inspect

**Files:**
- Create: `crates/tt-toplike-bus/src/replay.rs`, `tests/fixtures/bringup.jsonl` (a small hand-made bring-up: one chip pair, one model going Compiling to Loading to Ready, 20 seconds of samples, a few `RequestsCompleted`, one `Reset`; under 100 KB).
- Modify: `src/cli.rs` (flags `--record <DIR>`, `--replay <PATH>`, `--speed <N>`, and subcommand-style `bus inspect <PATH> [--entity K] [--signal S]` implemented as a flag-driven mode of `tt-toplike-tui`, in the style of the existing `--print` and `--bench` modes), `src/bin/tui.rs`.
- Test: `crates/tt-toplike-bus/src/replay.rs` module tests, `src/cli.rs` parse tests.

**Interfaces:**
- Produces: `pub struct Replay` with `open(path: &Path) -> io::Result<Replay>` (a segment, a directory of segments, or a compressed segment), `Replay::records(&mut self) -> impl Iterator<Item=Record>` in order, `Replay::play(&mut self, bus: &Bus, speed: f64, clock: &dyn Clock)` that republishes with the original spacing divided by `speed` and marks the bus header `Replay`. `pub trait Clock { fn now_ns(&self) -> u64; fn sleep_ns(&self, ns: u64); }` with a real clock and a manual test clock.
- `inspect` prints: header, record count by payload kind, time span, entities, and the signal list with min/max/last, filtered by `--entity`/`--signal`.

- [ ] **Step 1:** Failing tests: playing the fixture at speed 1 with the manual clock republishes records with their original spacing; speed 4 quarters the spacing; the world after replay equals the world from applying the fixture directly; a replayed bus reports `HeaderKind::Replay` to subscribers; `inspect` output for the fixture lists the model entity and the `Reset` event; CLI parses `--record`, `--replay`, `--speed` and rejects `--replay` combined with a live-only flag (`--serve`, `--remote`) with a clear message.
- [ ] **Step 2-4:** Implement and run. Confirm the spacing test fails when speed is ignored.
- [ ] **Step 5:** Commit: `feat(bus): replay, record and inspect`.

### Task 5: Chip identity and the index map

**Files:**
- Create: `src/bus/mod.rs`, `src/bus/identity.rs` (main crate; `pub mod bus;` in `src/lib.rs`).
- Test: module tests.

**Interfaces:**
- Consumes: `Device { index, bus_id, board_type, .. }` from `src/models/device.rs`, `TelemetryBackend::devices()`.
- Produces: `pub struct IdentityMap`, `IdentityMap::refresh(&mut self, devices: &[Device])`, `key_for_index(&self, index: usize) -> Option<EntityKey>`, `resolve_pending(&mut self) -> Vec<PendingResolved>`, and `fn hold_unresolved(&mut self, index: usize, payload: Payload)` which keeps the record for one `refresh` and then drops it with `identity.dropped` incremented.

- [ ] **Step 1:** Failing tests: after `refresh` with devices `[A@idx0, B@idx1]` then `[B@idx0, A@idx1]`, `key_for_index(0)` is B's key (Review Focus 5); a record for an unknown index is held through exactly one `refresh` and delivered if the index appears, dropped and counted otherwise; two devices with the same `bus_id` collapse to one key; the key is `EntityKey::chip(bus_id)`.
- [ ] **Step 2-4:** Implement; run; confirm the swap test fails if the map is never rebuilt.
- [ ] **Step 5:** Commit: `feat(bus): chip identity map`.

### Task 6: Backend and monitor sources

**Files:**
- Create: `src/bus/source.rs` (the `Source` trait and `BackendSource`), `src/bus/signals.rs` (signal-name constants).
- Test: module tests using `MockBackend` and a recorded `Fake`.

**Interfaces:**
- Consumes: Tasks 1, 2, 5; `TelemetryBackend` (`devices`, `telemetry`, `smbus_telemetry`, `pcie_bandwidth`, `snapshot_json`), `Telemetry` in `src/models/telemetry.rs`.
- Produces:
  - `pub trait Source { fn name(&self) -> &'static str; fn poll(&mut self, now: Instant, out: &mut dyn FnMut(Payload)); }` (a source emits only what it has; it decides its own rate by returning early).
  - `pub struct BackendSource` built from `Arc<IdentityMap>` that, each `poll`, emits per chip one `Sweep` of the fast-lane fields (power, voltage, current, temperature, `aiclk`, PCIe in and out, marked `Measured`; fields that are derived such as load relative to baseline are `Derived`), the slow-lane fields only when the backend's slow update changed them (GDDR per-channel data from `gddr_telemetry` as `Sample`s on `dram_channel` entities, Ethernet link mask as a `LinkChanged` event when it changes, harvesting), and one `Raw` record per new `snapshot_json()` value.
  - Signal names reuse tt-telemetry names where they exist (`tt_asic_temperature_celsius`, `tt_tdp_w`, `tt_ai_clock_mhz`, `tt_fan_speed_rpm`, `tt_pcie_link_alive`) and `toplike.*` for the rest; the full list lives in `signals.rs` with a doc line each.
  - A reading the backend reports as `None` is not emitted.

- [ ] **Step 1:** Failing tests with `MockBackend`: one poll emits one sweep per chip carrying the expected signals and `Measured` provenance; a device whose telemetry is `None` emits nothing for it (Review Focus 1); an unchanged slow-lane value is not re-emitted; a new `snapshot_json` emits one `Raw` record and the same JSON twice emits one; a changed `eth_live` mask emits exactly one `LinkChanged`; per-channel GDDR data lands on the `dram_channel` entity of the right chip, not by loop position; PCIe fields are absent (not zero) on a backend that returns `None` for `pcie_bandwidth`.
- [ ] **Step 2-4:** Implement; run; confirm the "not zero" test fails if `None` is mapped to 0.
- [ ] **Step 5:** Commit: `feat(bus): backend source`.

### Task 7: Workload sources, process redaction, gozer

**Files:**
- Create: `src/bus/workload_sources.rs` (inference, training, hivemind, processes, remote), `src/bus/redact.rs`, `src/bus/gozer.rs`.
- Test: module tests.

**Interfaces:**
- Consumes: `ServiceState` (`src/workload/inference_server/state.rs`), `InferenceServerMonitor::snapshot()`, `TrainState`/`TrainMonitor`, hivemind `SniffEvent`/`HeatGrid` (`src/workload/hivemind`), process rows (`ProcRow`), `DeviceProcess`, `remote_inference()`, `IdentityMap`.
- Produces:
  - `InferenceSource::poll` turns each `ServiceState` into `Sample`s on a `model` entity (phase, progress, tokens/s, running, waiting, KV cache, TTFT, CPU, RSS, kernel and shard counts) and events: `ModelLoading` when the phase enters Compiling or Loading, `ModelReady`, `ModelGone` when a service disappears, `RequestsCompleted{n}` from the per-tick completed delta, using `ServiceState.chips` through the identity map.
  - `TrainingSource` (chips from joining the training pid to the process monitor's device list), `HivemindSource` (each `SniffEvent` becomes a `Spark` or a `KernelCompiled`/`Reset` event with the chip resolved by `device`), `ProcessSource`, `RemoteSource` (the `--remote` extension).
  - `pub fn redact_cmdline(cmd: &str) -> String` replacing values after `--api-key`, `--token`, `--password`, `--secret`, `-p` followed by a token-like value, and `NAME=value` where the name contains `TOKEN`, `KEY`, `SECRET`, `PASSWORD` or `PASSWD` with `<redacted>`.
  - `GozerSource` polling `gozer status --json` (about once every 2 s, off the render thread, with a 2 s timeout, nothing emitted when gozer is absent) and emitting `LeaseGranted`/`LeaseReleased` and `Reset` events keyed by PCI address, plus a `Gap` after a `Reset`. It never runs anything but `gozer status`.
  - `pub trait Governor { fn permit(&self, chip: &EntityKey) -> Permit; }`, `pub enum Permit { Go, Deny(String) }`, and `GozerGovernor` that permits a chip only when `gozer status --json` shows it held under the lease id in the environment, and denies on any other state, on a missing lease environment, or when gozer is absent. No first-release source uses it; the sidecar will.

- [ ] **Step 1:** Failing tests: a `ServiceState` fixture with phase moving Compiling to Ready emits `ModelLoading` then `ModelReady` once each and `RequestsCompleted` from a completed delta of 3; a vanished service emits `ModelGone`; `ServiceState.chips = [0,1]` resolves to the two chip keys through the map and an unresolved index is held (Task 5 rule); `redact_cmdline("python serve.py --api-key sk-abc HF_TOKEN=hf_xyz --port 8000")` hides both secrets and keeps `--port 8000`, and a fixture log built from a process list with those arguments contains neither `sk-abc` nor `hf_xyz` anywhere in its bytes (Review Focus 3); `GozerSource` parses a recorded `gozer status --json` and emits the right events, emits nothing when the command is missing, and gives up after the 2 s timeout; `GozerGovernor` denies with no `GOZER_LEASE`/lease environment variable set, denies a `HELD-FOREIGN` chip, denies `BUSY-UNTRACKED`, and permits only a chip held under our own lease id (read the gozer README for the variable `gozer env` exports and use that).
- [ ] **Step 2-4:** Implement; run; confirm the redaction test fails when the HF_TOKEN pattern is removed.
- [ ] **Step 5:** Commit: `feat(bus): workload, gozer and governor sources`.

### Task 8: tt-station frame compatibility

**Files:**
- Create: `src/bus/compat.rs`.
- Modify: `src/ui/tui/mod.rs` (the `/serve` broadcast block near `publisher.broadcast`) and `src/bin/tui.rs` (the headless broadcast loop) only to take the frame from the helper below.
- Test: module tests with a checked-in fixture of a real `tt-smi -s` JSON frame and its extension.

**Interfaces:**
- Consumes: `build_extension`, `inject_extension` (`src/backend/remote_ext.rs`), `Raw` records from Task 6.
- Produces: `pub fn telemetry_frame(world: &World, ext: &RemoteExt) -> Option<String>`, which takes the newest `Raw` tt-smi record kept in the world (add `World::latest_raw(source: &str) -> Option<&RawFrame>` in Task 3's file if needed) and returns exactly what `inject_extension(&raw.body, ext)` returns today.

- [ ] **Step 1:** Failing tests: for the fixture, `telemetry_frame` equals `inject_extension(raw, ext)` byte for byte; with no raw record it returns `None`; a relayed `--remote` frame is passed through verbatim (the existing relay rule at the `backend_type == BackendType::Remote` branch stays).
- [ ] **Step 2-4:** Implement; switch the two broadcast sites to call it; run the existing serve and remote tests unchanged.
- [ ] **Step 5:** Commit: `feat(bus): tt-station frame built from the bus`.

### Task 9: The socket transport

**Files:**
- Create: `crates/tt-toplike-bus/src/socket.rs` (`#[cfg(unix)]`).
- Test: module tests using a temp socket path.

**Interfaces:**
- Consumes: `Bus`, `World`.
- Produces: `pub struct SocketServer` with `spawn(bus: Bus, path: &Path) -> io::Result<SocketServer>` (parent directory created 0700, socket 0600, an existing live socket is refused, a stale one is replaced) and `Drop` that shuts down within a second. Protocol, JSON Lines in both directions: a client sends `{"op":"snapshot"}` and gets `{"seq":N,"world":{...}}` where `world` is a documented summary (entities, latest readings with provenance and age, models, last events); `{"op":"subscribe","from":N}` streams records from N (or `"from":"head"`); `{"op":"replay","from_ts_ms":T,"speed":S,"entities":[...],"signals":[...]}` replays recorded data. Errors are lines `{"error":"dropped","oldest":N}`, `{"error":"future","head":N}`. A slow client is disconnected when its queue passes a limit and never stalls the writer.

- [ ] **Step 1:** Failing tests: snapshot then subscribe from the returned `seq` yields every later record exactly once with 1000 records published during the handshake; subscribing from a dropped sequence returns the `dropped` error and from the future returns `future` (Review Focus 4); a client that never reads is disconnected and a second client keeps receiving; the server refuses a path with a live listener and replaces a stale socket file; socket permissions are 0600; `Drop` returns promptly with clients connected.
- [ ] **Step 2-4:** Implement with `std::os::unix::net` and a thread per client; run; confirm the exactly-once test fails if the snapshot and subscribe are not taken at one `seq`.
- [ ] **Step 5:** Commit: `feat(bus): unix socket transport`.

### Task 10: Hosting in the TUI and headless mode

**Files:**
- Modify: `src/ui/tui/mod.rs` (`run_app`: create the `Bus`, `IdentityMap` and sources; poll them on the existing tick; publish `ViewChanged` when the display mode changes), `src/cli.rs` (`--headless`, `--bus-dir <DIR>`, `--bus-socket <PATH>`, `--bus-retain <HOURS>[,<GB>]`), `src/bin/tui.rs` (headless branch: extend the existing no-TTY `--serve` loop so `--headless` runs the bus, the sources, the recorder and the socket with no screen, and still broadcasts `/telemetry` when `--serve` is also given).
- Test: `src/ui/tui/mod.rs` tests with `MockBackend`, `src/cli.rs` parse tests, a headless test that runs three ticks against `MockBackend` with a temp directory.

**Interfaces:**
- Consumes: Tasks 2 to 9.
- Produces: `crate::bus::host::BusHost { bus: Bus, world: World (updated each tick), sources: Vec<Box<dyn Source>>, identity: IdentityMap }` with `BusHost::new(cfg: BusConfig, backend: &dyn TelemetryBackend) -> BusHost`, `BusHost::tick(&mut self, backend: &dyn TelemetryBackend, now: Instant)`, `BusHost::world(&self) -> &World`. Default `BusConfig` for the TUI has `dir: None` (memory only) unless `--record`/`--bus-dir` is given; headless defaults to `~/.local/state/tt-toplike/bus` with the spec's retention.
- A TUI started while a headless host's socket exists at the default path connects to it as a client and shows the world from it (read-only snapshot plus subscribe); this is the second-TUI rule in the spec. If connecting fails it hosts its own.

- [ ] **Step 1:** Failing tests: `--headless` parses and implies no TTY; `BusHost::tick` against `MockBackend` publishes sweeps for every chip and the world shows them; the TUI render path does not slow measurably (frame cost with the host ticking is within 0.3 ms of without, using the existing bench test style); `--record DIR` writes a segment that `Replay` reads back to an equal world; the headless loop writes a segment, serves a snapshot on the socket, and exits cleanly on SIGTERM; a second TUI with a live socket at the default path reads the world from it (use a test socket path); `ViewChanged` is published once per mode change.
- [ ] **Step 2-4:** Implement; run; confirm the no-slowdown test would fail if `tick` ran on every render frame instead of the data tick.
- [ ] **Step 5:** Commit: `feat(bus): TUI host and headless mode`.

### Task 11: Die 3D reads the bus

**Files:**
- Modify: `src/animation/die3d/mod.rs`, `src/animation/die3d/scene.rs`, `src/animation/die3d/perf_tests.rs`, `src/ui/tui/mod.rs` (pass `&World` into `Die3dView::update` and `render`), add `src/animation/die3d/overlay.rs`.
- Test: `src/animation/die3d/overlay.rs`, `perf_tests.rs`.

**Interfaces:**
- Consumes: `World` (Task 3), the workload colours in `src/animation/activity_lanes.rs` (train amber, load purple, serve teal, stalled red).
- Produces: `pub struct Overlay { chips: Vec<ChipOverlay> }` built by `Overlay::from_world(world: &World, chips: &[EntityKey], now: u64) -> Overlay`, `ChipOverlay { flag: Option<ModelFlag>, fill: Option<f32>, pulse: Option<f32>, gddr: Vec<GddrRow>, sparks: Vec<Spark> }`. `ModelFlag { label, phase_colour, tokens_per_s, waiting }`. `Die3dView::update` takes `Option<&World>`; with `None`, or a world holding no model, no event and no per-channel data, every frame is unchanged.
- Behaviour: a model flag per chip using the existing pin-and-flag mechanism; bring-up fill from the DRAM edge toward the cores driven by the `ModelLoading` progress (or the kernel and shard counts when progress is absent); a pulse on the model's chip for each `RequestsCompleted` event decaying over about a second; each GDDR row coloured from its own channel temperature, flashing on an ECC event; a spark per `Spark` event on its chip. The caption keeps `per-tile glow: estimate from chip load` and adds `bus: replay` when the bus header says so.

- [ ] **Step 1:** Failing tests: with no world, the existing pinned hashes hold at 100 x 30, 120 x 36, the inset size and the small sizes (run the existing tests unchanged); with a world holding a `ModelReady` model on chip 0, chip 0's flag shows the model label and tokens/s and chip 1's does not; `ModelLoading` at progress 0.5 fills half the DRAM-to-core span and 1.0 fills it; a `RequestsCompleted` event makes a pulse that is gone after 1.5 s; two GDDR channels with different temperatures draw different row colours; flags still never cover a drawn cell (extend the existing flags property test over a world fixture); the redraw-cost budget tests still hold with an active overlay (`changed_from` budget, 0.3 ms inset budget); new pinned hashes are added for a decorated frame at 100 x 30 and 150 x 42 from a fixture world.
- [ ] **Step 2-4:** Implement; run the full Die 3D test set; confirm the "no world means old hash" test fails if the overlay draws something with an empty world.
- [ ] **Step 5:** Commit: `feat(die3d): model flags, bring-up fill, pulses and per-channel DRAM from the bus`.

### Task 12: `i` reads the bus

**Files:**
- Modify: the `InferenceMonitor` view code in `src/ui/tui/mod.rs` and `src/animation/` (serving creature, `inference_load`, atmosphere) so their `ServiceState` input is derived from the world's `ModelView`s, with a helper `world_to_service_states(world: &World) -> Vec<ServiceState>` in `src/bus/compat.rs`.
- Test: a shared-fixture test in `src/bus/compat.rs`.

- [ ] **Step 1:** Failing tests: for one fixture world, `world_to_service_states` gives the same phase, tokens/s, running, waiting and KV-cache values that Die 3D's flag shows (the coherence test: both read `ModelView`, so assert equality of the numbers in the Die 3D caption/flag text and in the `i` view's rendered text for the fixture); the `i` view frames for the existing `ServiceState` fixtures are unchanged when the world is built from those states (existing `i` tests pass unchanged).
- [ ] **Step 2-4:** Implement; run all `i`, arcade and rotation tests.
- [ ] **Step 5:** Commit: `feat(tui): the inference monitor reads the bus`.

### Task 13: Docs, version, final checks

**Files:**
- Modify: `Cargo.toml` (0.15.0), `Cargo.lock`, `QUICK_START.md`, `site/index.html`, `debian/changelog`, `CHANGELOG.md`, `README.md`, `AGENTS.md`, the bus crate README and module docs; add `docs/bus.md` (record format, socket protocol, signal list, retention, privacy, the sidecar's governor rules from `scripts`-free text, how to replay).
- Test: the existing version-sync checks, if any; the full CI command set.

- [ ] **Step 1:** Update the docs in plain English; grep the prose for the banned phrases and fix each hit. State the limits: no hardware run of this release, the tile glow is still an estimate, the sidecar is not included, and which parts were checked only against the mock backend.
- [ ] **Step 2:** Run `cargo fmt --check`, the CI clippy and test commands, the no-default-features build, `cargo test -p tt-toplike-bus`, the `--bench --mock` smoke, and a headless run for 10 s against `--mock 3` with a temp bus directory followed by `--replay` of it and `bus inspect`. Record the outputs.
- [ ] **Step 3:** Commit: `docs: signal bus (0.15.0)`.
