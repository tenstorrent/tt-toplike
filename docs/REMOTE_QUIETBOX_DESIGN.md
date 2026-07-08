# Remote QuietBox: tt-toplike on macOS, watching a tt-station box over the LAN

## Question

What would it take for tt-toplike running on macOS to (a) discover Tenstorrent boxes exposed
via tt-station, and (b) display the "QuietBox experience" — tt-toplike's live hardware view —
remotely, over the LAN?

## Chosen architecture (decided after the original brief)

**A WebSocket telemetry stream, publisher on the box.** The design below is re-centered on this
decision:

- **Transport = a WebSocket stream, not HTTP polling.** `tt-station-agentd` gains a new route
  `GET /telemetry` that **upgrades to a WebSocket** and **pushes** telemetry snapshots on an
  interval. The agent is the *publisher*: it runs a `tt-smi -s` collector loop on the box and
  streams each snapshot frame as it's produced. (HTTP polling remains a trivial fallback — see
  §7 — but the WebSocket stream is the target.)
- **One schema, zero mapping.** Each pushed frame is *exactly* the JSON tt-toplike's existing
  `JSONBackend` already deserializes — the `Device` + `Telemetry` + `SmbusTelemetry` shape that
  `tt-smi -s` produces. There is a single schema shared end to end and no field-mapping layer
  on either side.
- **tt-toplike side = a new `WsBackend` datasource** that connects to the box's WebSocket and
  feeds the *same* telemetry structs its local backends already produce — "read telemetry from a
  websocket stream just like local telemetry." It sits behind the existing `TelemetryBackend`
  trait, so every visualization consumes it unchanged.
- **Strictly additive (hard constraint).** The WS route is added *alongside* every existing
  agent route; the `WsBackend` is added *alongside* every existing local backend; the default
  stays local. No existing behavior changes. See §3.

## TL;DR

- **Discovery is nearly free.** tt-station already advertises `_tenstorrent._tcp.local.` via
  mDNS and ships both a CLI (`tt --json discover`) and a Rust crate (`libttstation`) that browse
  it and decode the TXT record into a `BoxRecord`. tt-toplike reuses that to pick which box to
  connect the WebSocket to.
- **The telemetry gap is smaller than assumed — and that finding is the whole payload of the
  stream.** tt-toplike does not today display true per-core Tensix/NoC hardware-counter data on
  *any* backend, including direct-hardware `LuwenBackend`. Every visualization — including the
  "Tensix cores as stars" starfield — is driven by **chip-level aggregate telemetry**
  (`Telemetry` + `SmbusTelemetry`: power, temp, clocks, DDR/GDDR status, ARC health) fanned out
  across a grid sized by architecture. That aggregate telemetry is exactly what `tt-smi -s`
  already emits and what `JSONBackend` already parses. So a WebSocket that streams `tt-smi -s`
  snapshots reproduces almost the entire visual experience remotely, with no new box-side
  instrumentation and no new schema.
- **What's genuinely missing** — real per-core Tensix utilization, real NoC traffic, real
  per-channel DRAM bandwidth — doesn't exist anywhere in tt-toplike today, locally or remotely.
  Building it is a UMD/metal-level instrumentation project independent of this remote-viewing
  work, not a rehosting exercise.
- **Recommended first slice:** a new `GET /telemetry` WebSocket route on `tt-station-agentd`
  that runs a `tt-smi -s` collector loop and pushes each snapshot frame; a new `WsBackend` in
  tt-toplike that connects to that socket and deserializes each frame into the existing
  `Telemetry`/`SmbusTelemetry`/`Device` structs. Discovery picks the box via
  `_tenstorrent._tcp`. That alone demos "tt-toplike on my Mac shows my discovered QuietBox live."

---

## 1. Discovery

### What tt-station already exposes

- mDNS service type `_tenstorrent._tcp.local.`, defined once and shared by advertiser and
  browser: `crates/libttstation/src/discovery/mod.rs:14`.
- `MdnsProvider` (`crates/libttstation/src/discovery/mdns.rs:1-87`) browses that service type
  using the `mdns_sd` crate (pure-Rust mDNS/DNS-SD — it doesn't shell out to Apple's Bonjour
  daemon, but it speaks the same wire protocol, so it interoperates with a Bonjour-advertising
  box on macOS with no extra configuration). It dedups by service instance fullname and decodes
  each resolved instance's TXT record into a `BoxRecord` via `txt_decode`
  (`crates/libttstation/src/model.rs:79-118`).
- `BoxRecord` (`crates/libttstation/src/model.rs:9-16`): `{ name, host, ctrl_port, chips,
  status: ServingStatus, apiver }`. `host` + `ctrl_port` is exactly the address the WebSocket
  connects to (`ws://{host}:{ctrl_port}/telemetry`). `chips` is a short inventory string like
  `"4xBH"`; `status` round-trips through the same `idle` / `serving:<model>` text form used on
  the wire everywhere else (`crates/libttstation/src/model.rs:58-66`).
- `aggregate()` (`crates/libttstation/src/discovery/mod.rs:28-48`) runs multiple providers
  (mDNS plus a manual host-list fallback, `crates/libttstation/src/discovery/manual.rs`) and
  dedups by name — this backs the `tt` CLI's `discover` subcommand (`crates/tt/src/main.rs:7`,
  `:191-197`, `:268`), which can print machine-readable JSON with `tt --json discover`
  (`DiscoveredBox`, `crates/tt/src/main.rs:599-618`).

### How tt-toplike gets the box list

Discovery is unchanged by the WebSocket decision — it just answers "which box, at what
`host:port`, do I open the socket to." Two options:

- **Option A — shell out to `tt --json discover`.** Matches tt-toplike's existing subprocess
  idiom (`JSONBackend` already spawns `tt-smi -s` and parses its JSON stdout,
  `src/backend/json.rs:1-32`, `:363-412`). Zero new Cargo dependencies; needs the `tt` binary on
  `$PATH`. Good enough for the first slice.
- **Option B — depend on `libttstation` directly.** Its discovery module is dependency-light
  (`mdns_sd`, `serde`, `anyhow` — no axum/tokio needed on the browsing side), so tt-toplike can
  call `MdnsProvider::discover()` (or hold a live `ServiceDaemon::browse` for add/remove events)
  for a native, live-updating box picker, at the cost of a cross-repo Rust dependency. The
  productionized follow-up.

### Pairing / auth for the telemetry stream

`GET /status` and `GET /models` are already unauthed by design — routes.rs is explicit that
`/models` is "UNAUTHED, like GET /status ... read-only discovery info, not a control action"
(`crates/tt-station-agentd/src/routes.rs:709-712`). The bearer-token pairing flow (`/pair/init`,
`/pair/complete`, enforced by the `BearerAuth` extractor,
`crates/tt-station-agentd/src/routes.rs:795-824`) exists to gate the *control* routes that
mutate box state (`/run`, `/stop`, `/reset`, `/endpoint`; full table at `:962-971`).

A telemetry stream is exactly as read-only as `/status`/`/models`, so the WebSocket upgrade
should be **unauthed for v1**, consistent with those two. That means anyone on the LAN who can
reach the control port can watch the box's chip telemetry — but that's already true of `/status`
today, so it extends the existing exposure rather than creating a new class of it. Flag it as a
conscious tradeoff: if a box is ever reachable beyond a trusted LAN, all read-only surfaces need
the same network-level answer (firewall/VPN), not per-route auth. (If auth is later wanted on the
stream specifically, the `BearerAuth` extractor already composes onto any route and could gate
the upgrade handshake without touching the frame format.)

---

## 2. The telemetry stream payload (the crux)

### One schema, already serde-Serializable on both ends

The single most load-bearing fact for "one schema, zero mapping": **tt-toplike's telemetry
structs already derive `Serialize` as well as `Deserialize`**, so the box can construct and emit
the very same types the client reconstructs — there is no separate wire DTO to define or keep in
sync.

- `Telemetry` — `#[derive(Debug, Clone, Serialize, Deserialize)]` (`src/models/telemetry.rs:17`).
- `SmbusTelemetry` — `#[derive(... Serialize, Deserialize)]` (`src/models/telemetry.rs:97`),
  including its helper types `GddrTempPair` (`:286`), `FirmwaresInfo` (`:323`), `DeviceLimits`
  (`:332`).
- `Device` — `#[derive(... Serialize, Deserialize)]` (`src/models/device.rs:95`), and
  `Architecture` (`src/models/device.rs:17`).

So a frame can be, quite literally, `serde_json::to_string(&snapshot)` on the box and
`serde_json::from_str::<Snapshot>(&frame)` on the Mac, where `Snapshot` is a thin struct wrapping
`Vec<Device>` + per-device `Telemetry` + `SmbusTelemetry` — the exact shapes `JSONBackend` already
builds today.

### What tt-toplike's visuals actually consume

`src/backend/mod.rs:90-251` defines the `TelemetryBackend` trait every backend implements:
`init`, `update`, `devices() -> &[Device]`, `telemetry(idx) -> Option<&Telemetry>`,
`smbus_telemetry(idx) -> Option<&SmbusTelemetry>`, `backend_info()`. That's the entire surface
the UI is built against — a small fixed set of per-device state, refreshed on the backend's own
cadence.

- **`Device`** (`src/models/device.rs:96-139`): architecture (→ Tensix grid shape and DDR
  channel count via `tensix_grid()`/`memory_channels()`, `src/models/device.rs:50-68`),
  board_type, bus_id, coords, firmwares, limits, pcie info, optional grid/channel overrides.
- **`Telemetry`** (`src/models/telemetry.rs:17-40`): `voltage`, `current`, `power`,
  `asic_temperature`, `aiclk`, `heartbeat` — six scalar `Option` fields.
- **`SmbusTelemetry`** (`src/models/telemetry.rs:98-282`): larger, but still entirely
  chip/board-level — DDR speed/status, ARC0-3 health + firmware, ETH firmware/link status, GDDR
  per-pair temps and error counts, `harvesting_state`, `enabled_tensix_col` (a *bitmask of
  harvested Tensix columns*, not per-core activity), board/VREG temps, TDP/TDC, fan speed. No
  per-core, per-NoC-link, or per-DRAM-channel-bandwidth field exists here.

### The starfield is not reading per-core hardware counters

The original brief framed the rich view as needing "deep UMD access that only exists on the box."
That was the claim most worth verifying, and it doesn't hold.
`HardwareStarfield::update_from_telemetry` (`src/animation/starfield.rs:500-522`) pulls exactly
`telem.power_w()`, `telem.current_a()`, `telem.temp_c()`, `telem.aiclk_mhz()` — the same four
scalar `Telemetry` fields — for the *whole device*, then fans that single reading out across
every star in the device's grid with jitter/animation for variety
(`src/animation/starfield.rs:530-632`: hue from temperature, brightness from power *change* vs. a
rolling baseline, sparkle on spikes). The grid's *shape* comes from
`Architecture::tensix_grid()`, a static table — not live hardware.

This isn't unique to non-hardware backends: `LuwenBackend` (`src/backend/luwen.rs:26-40`), which
talks directly to silicon over PCIe/UMD, populates the exact same `Device`/`Telemetry`/`SmbusTelemetry`
triad and has no extra per-core fields to fill. Corroborated by `docs/SYSFS_BACKEND.md:62-70` and
`docs/HOST_BACKEND.md:51-57`. **The ceiling for all backends — local or streamed — is what
`Telemetry`/`SmbusTelemetry` already model.** That's precisely why streaming those two structs is
enough.

### The box-side collector loop

The agent already has the reference implementation for producing this payload sitting in
tt-toplike's own `JSONBackend`: run `tt-smi -s`, parse its snapshot JSON
(`TTSMIDeviceRaw` → `board_info`/`telemetry`/`smbus_telem`/`firmwares`/`limits`,
`src/backend/json.rs:76-233`) into the `Device`/`Telemetry`/`SmbusTelemetry` triad. On the agent
this becomes a loop:

```
every N ms:
    run `tt-smi -s`         (spawn_blocking — same off-the-async-runtime discipline every
                             agentd backend call already follows, e.g. routes.rs:726-732)
    parse → Snapshot        (the same reshape JSONBackend::parse_json already does)
    ws.send(serde_json::to_string(&snapshot))   (push frame to every connected client)
```

`tt-smi -s` can be flaky exactly when a model is serving (this is *why* the sysfs backend exists,
`docs/SYSFS_BACKEND.md:7-19`), so the loop keeps last-known-good and either re-sends it or sends a
frame with `Option` fields set to `None` (which the models already treat as "unavailable"
gracefully) rather than dropping the connection.

### Frame schema (v1)

```json
{
  "box": { "name": "qb2-lab", "chips": "4xBH", "status": "serving:llama3-70b" },
  "timestamp": "2026-07-03T20:14:00Z",
  "devices": [
    {
      "index": 0,
      "board_type": "p150a",
      "bus_id": "0000:01:00.0",
      "coords": "(0,0)",
      "telemetry": {
        "voltage": 0.82, "current": 145.3, "power": 118.7,
        "asic_temperature": 61.4, "aiclk": 1350, "heartbeat": 1
      },
      "smbus": {
        "ddr_speed": "0x3e80", "ddr_status": "0x55555555", "arc0_health": "0x10e7a",
        "tdp": "118.7", "tdc": "145.3", "board_temperature": "48.0",
        "gddr_temps": ["0x262a2c2c", null, null, null],
        "harvesting_state": 0, "enabled_tensix_col": 16383, "eth_live_status": 255
      },
      "firmwares": { "fw_bundle_version": "18.5.0", "eth_fw": "2.9.0" },
      "limits": { "tdp_limit": 150.0, "asic_fmax": 1400 }
    }
  ]
}
```

Field names mirror the existing `Device`/`Telemetry`/`SmbusTelemetry` serde derives plus the
box-level fields already in `StatusResponse` (`crates/tt-station-agentd/src/routes.rs:688-707`)
folded into a `box` object, so the `WsBackend` deserializes with the structs it already has.

### Genuinely hard / out of scope

Per-core Tensix utilization, per-NoC-link traffic, and true per-channel DRAM bandwidth (as
opposed to DDR *training status*, which `tt-smi` already reports) do not exist as consumable
telemetry anywhere in this codebase, on any backend. Building them means new UMD/driver counters,
a new/extended backend to read them, and new model fields — a hardware-telemetry R&D effort
independent of remote viewing, and it would benefit the *local* backends first. Not a "phase 2"
of this stream.

---

## 3. Strictly additive — hard constraint

This is a hard requirement, called out explicitly so no step of implementation quietly changes
existing behavior:

- **Agent routes (add one, change none).** `GET /telemetry` (WS upgrade) is added *alongside*
  the existing router table (`crates/tt-station-agentd/src/routes.rs:962-971`): `/status`,
  `/models`, `/pair/init`, `/pair/complete`, `/run`, `/stop`, `/reset`, `/endpoint` all keep
  their exact current behavior, auth, and response shapes. The new route shares `AppState` (it
  can read `state.name()`/`chips()`/`status()` for the `box` object) but mutates nothing.
- **tt-toplike backends (add one, change none).** `WsBackend` is added *alongside* the existing
  `host`/`sysfs`/`hybrid`/`json`/`luwen`/`mock` backends. The auto-detect order and default stay
  exactly as they are (`create_backend`/`create_auto_backend`, `src/backend/factory.rs:29-136`) —
  remote is opt-in only (an explicit `--remote <box>` / `BackendType::Remote`), never entered by
  auto-detect or backend-cycling unless the user asks. Local behavior on a Mac with no TT
  hardware (the `host`/`mock` path) is untouched.
- **If a shared telemetry crate is extracted, it must be a pure refactor.** The natural DRY move
  is to lift the `tt-smi -s` → `Device`/`Telemetry`/`SmbusTelemetry` parsing out of
  `src/backend/json.rs` into a small crate both repos depend on (so the agent's collector and
  tt-toplike's `JSONBackend`/`WsBackend` share one parser and one schema). That extraction must
  preserve tt-toplike's current local code paths byte-for-byte in behavior — `JSONBackend` keeps
  working identically, just calling the extracted parser. No behavior change may ride along with
  the refactor.

---

## 4. tt-toplike integration: `WsBackend`

### Seam

`src/backend/factory.rs:29-94` (`create_backend`, matching on `BackendType`) is the single
construction point; `src/cli.rs:196-220` declares `BackendType` and its CLI spelling. Adding the
stream consumer:

1. A new `BackendType::Remote` (or `::Ws`) variant in `src/cli.rs` — cross-platform, no
   `cfg(target_os = "linux")` gate — carrying/paired with a `--remote <box-name|host:port>` flag.
2. A new `src/backend/ws.rs` implementing `TelemetryBackend`. Structurally it is `JSONBackend`
   with the data source swapped: instead of `update()` spawning `tt-smi -s`, a background task
   holds the WebSocket connection and each received frame is deserialized (into the same
   `Vec<Device>` + `HashMap<usize, Telemetry>` + `HashMap<usize, SmbusTelemetry>` that
   `JSONBackend::parse_json`/`parse_smbus_from_json` build, `src/backend/json.rs:363-412`, `:738`)
   and stored behind a shared cell; `update()` just swaps in the latest frame (non-blocking).
   `init()` opens the socket and blocks for the first frame (or times out).
3. New match arm in `create_backend` (`src/backend/factory.rs:33-93`). Deliberately **not** added
   to `next_backend`/`switch_to_next_backend` (`src/backend/factory.rs:142-207`) — cycling into a
   live remote connection on `Tab` would be surprising; `--remote` is its own explicit, sticky
   mode, satisfying the additive constraint in §3.

Because the agent pushes, `WsBackend`'s "poll loop" (`update()`) does almost nothing — it reads
whatever the stream task last delivered. The trait's existing pull-shaped API (`update()` then
`telemetry(idx)`) is satisfied by a push source with a one-slot latest-frame buffer; no trait
change is needed.

### What maps cleanly vs. what's degraded

| tt-toplike concept | Streamed v1 source | Fidelity |
|---|---|---|
| `Device` (architecture, grid shape, DDR channels) | frame `board_type` → `Architecture::from_board_type` (`src/models/device.rs:76-88`) | Full — identical to local JSON/Luwen |
| `Telemetry` (power/temp/current/aiclk/heartbeat) | frame `telemetry` block | Full |
| `SmbusTelemetry` (DDR/GDDR/ARC/ETH/firmware) | frame `smbus` block | Full — same ceiling as local `tt-smi -s` (§2) |
| Starfield, Memory Castle/Flow, Arcade, Defrag | Derived entirely from the two rows above | Full — these never used per-core data anyway (§2) |
| Coarse serving status (which model is running) | frame `box.status` (`serving:<model>`) | Full for the coarse state |
| Per-request serving metrics (tok/s, TTFT, KV-cache %, queue depth — `ServingStats`, `src/workload/inference_server/metrics.rs:110-127`) | **Not in v1 frames** | Missing — produced by tt-toplike's own `workload::inference_server` monitor scraping `docker stats`/`docker exec`/localhost `/metrics` (`src/workload/inference_server/probe.rs:132-162`, `detect.rs:90-96`), all of which require running *on* the box |
| Per-core Tensix / NoC / true DRAM bandwidth | Not available anywhere, local or remote (§2) | Unavailable — separate R&D track |

The one real "degraded" item is the inference-server swimlane detail — not because it needs deep
hardware access, but because the code that produces it runs inside tt-toplike's own process and
talks to `docker`/localhost. `tt-station-agentd` only knows coarse `Idle`/`Serving(model)`
(`ServingStatus`, `crates/libttstation/src/model.rs:9-16`, held in `AppState.status`,
`crates/tt-station-agentd/src/routes.rs:88-90`, `:242-249`). Closing it means running tt-toplike
itself on the box as a richer publisher (see §5), not porting `workload::inference_server` into
agentd.

---

## 5. UX

- **Box picker**: a new screen (bound similarly to how `i`/`I` opens the existing Inference
  Servers monitor, `src/ui/tui/inference_panel.rs:1-18`) listing discovered boxes with name,
  chip inventory, and status — the exact fields already in `BoxRecord`/`DiscoveredBox`.
  Selecting a box opens the WebSocket (`ws://{host}:{ctrl_port}/telemetry`) and switches the
  active backend to `WsBackend`.
- **Remote-mode indicator**: `TelemetryBackend::backend_info()` (`src/backend/mod.rs:207-218`,
  already rendered as the on-screen backend label) is the seam — `WsBackend::backend_info()`
  returns e.g. `"Remote WS (qb2-lab @ 192.168.1.42:8765)"`. Because a push stream can silently
  stall (box rebooted, Wi-Fi drop) in a way a local read never does, surface last-frame age /
  connection state in that same status area, and reconnect automatically.
- **Reuse, don't rebuild**: the Inference Servers monitor (`src/ui/tui/inference_panel.rs`)
  already renders "service with no live data this tick → Down" gracefully
  (`inference_panel.rs:84-140`) — the same degradation shape a remote box with only coarse
  `box.status` should reuse rather than inventing a second remote-serving view.

---

## 6. Phasing & effort

### Phase 1 — Discover + streamed chip telemetry (temps / power / clocks / DDR-GDDR-ARC / coarse serving)

- **tt-station-agentd**: add the `GET /telemetry` WebSocket route (§2/§3) — a `tt-smi -s`
  collector loop (spawn_blocking) that serializes each `Snapshot` and pushes it to connected
  clients; route added next to the existing unauthed `/status`/`/models`
  (`crates/tt-station-agentd/src/routes.rs:962-971`). **Effort: S–M.** The parse/reshape logic
  already exists as a reference in tt-toplike's `src/backend/json.rs`; the new surface is the WS
  upgrade handler + broadcast-to-subscribers plumbing (axum has first-class WebSocket support, so
  this is idiomatic, not exotic).
- **tt-toplike**: add `BackendType::Remote` (`src/cli.rs`) + `WsBackend` (`src/backend/ws.rs`,
  sibling to `src/backend/json.rs`) + factory wiring (`src/backend/factory.rs:29-94`). Discovery
  via `tt --json discover` (§1, Option A). **Effort: M.** Mostly plumbing given the frame is the
  structs it already deserializes; the genuinely new bits are the async WS client task,
  reconnect/staleness handling, and the box picker.
- **Risks**: `tt-smi -s` flakiness under serving load (mitigated by last-known-good + `Option`
  fields, §2); WS lifecycle on the client (reconnect, timeout, back-pressure if the UI stalls —
  a one-slot latest-frame buffer sidesteps back-pressure by design).

### Phase 2 — Serving/swimlane detail (the real "v2")

The valuable next tier is the inference-server swimlane data (tok/s, TTFT, queue depth, KV-cache
%), **not** deeper chip telemetry (there is no hidden deeper chip tier, §2). It needs a producer
*on* the box, because only `workload::inference_server` (inside tt-toplike) knows how to derive it.

**Recommendation: run tt-toplike itself in a headless publisher mode on the box, rather than
reimplementing `workload::inference_server` inside `tt-station-agentd`.** Otherwise agentd grows a
second copy of "how do we know a model server is healthy" (docker-probing + vLLM-metrics parsing,
`src/workload/inference_server/{probe,detect,metrics}.rs`) that will drift from tt-toplike's. A
new tt-toplike mode (e.g. `tt-toplike --serve-telemetry ws://:9743`) reuses every line of the
existing collection loop and serves its already-computed
`Telemetry`/`SmbusTelemetry`/`ServiceState` snapshots over the same WS frame format, extended with
the serving block. agentd's only new job would be optional: advertise that port via mDNS so the
Mac client discovers it. **Effort: L** (a publisher mode is genuinely new — today tt-toplike only
renders, never serves, its snapshots; plus the opt-in/attack-surface decision, since it reads
`docker` state).

- **True per-core/NoC/DRAM-bandwidth**: out of scope entirely — separate hardware-instrumentation
  effort (§2).

---

## 7. Recommended first slice (WebSocket)

The smallest thing that demos "tt-toplike on my Mac shows my discovered QuietBox live":

1. **Agent (publisher).** One new route on `tt-station-agentd`: `GET /telemetry` that upgrades to
   a WebSocket and, on an interval, runs `tt-smi -s` (spawn_blocking), serializes the resulting
   `Snapshot` (the §2 frame), and pushes it to every connected client. Added *alongside* the
   existing routes, mutating no state. (`crates/tt-station-agentd/src/routes.rs`)
2. **tt-toplike (consumer).** A new `WsBackend` (`src/backend/ws.rs`) that (a) resolves
   `--remote <box-name>` to a `host:ctrl_port` via `tt --json discover` over `_tenstorrent._tcp`,
   (b) opens `ws://{host}:{ctrl_port}/telemetry`, and (c) deserializes each frame into the
   existing `Device`/`Telemetry`/`SmbusTelemetry` structs, exposing them through the unchanged
   `TelemetryBackend` trait. Wired into `create_backend` as `BackendType::Remote`
   (`src/backend/factory.rs:29-94`), opt-in only.
3. **Run any existing visualization** — `tt-toplike --remote qb2-lab --mode starfield` — and it
   works unmodified, because every visualization already only consumes the
   `Telemetry`/`SmbusTelemetry` fields the stream now delivers over the LAN.

No box picker UI, no serving swimlanes, and no new mDNS code inside tt-toplike are required for
this slice — just one additive agent route (WS publisher) and one additive backend (WS consumer),
sharing the one schema the structs already serialize.

**HTTP-poll fallback (note):** if a WebSocket upgrade is ever inconvenient (a proxy that won't
pass upgrades, a quick curl-based sanity check), the identical frame can be served from a plain
`GET /telemetry` that returns one snapshot per request, and `WsBackend` degrades to polling it.
This is a trivial fallback — same collector, same schema — but the WebSocket stream is the target
because it gives the Mac a live push at the box's own cadence instead of a client-driven poll.
