# `--serve` / `/serve` — tt-toplike as a telemetry publisher

**Status:** approved (2026-07-05)
**Feature:** a WebSocket telemetry publisher so any tt-toplike instance can broadcast the same
`/telemetry` stream `--remote` already consumes — decentralizing the remote data source (no
`tt-station-agentd` required).

## Goal

tt-toplike can already *consume* a remote box's telemetry (`--remote <host:port>`, `WsBackend`),
where each frame is the verbatim stdout of `tt-smi -s` served on a `/telemetry` WebSocket. This
feature adds the *publisher* half: `--serve [BIND:PORT]` at launch and `/serve [BIND:PORT]` in-app.
Any tt-toplike then connects to another with `--remote` — a mesh of instances, agentd optional.

## Non-goals (v1)

- Serializing non-tt-smi backends (host/sysfs/luwen/mock) into the tt-smi schema — deferred
  follow-up (the tt-smi JSON structs already derive `Serialize`, so it is a clean later slice).
- Authentication / TLS. Same trusted-LAN, plaintext, unauthed posture as the consumer.
- Discovery/pairing/lifecycle — those remain `tt-station-agentd`'s control-plane value.

## Architecture

Three pieces:

### 1. `TelemetryBackend::snapshot_json(&self) -> Option<String>`

New trait method (default `None`). Returns the current telemetry as `tt-smi -s` JSON — the exact
schema `parse_tt_smi_snapshot` consumes — or `None` when this backend can't produce it.

- **`JSONBackend`, `HybridBackend`** — retain the last raw `tt-smi -s` stdout (they already run it
  in `run_tt_smi`) and return it verbatim. Byte-identical to agentd's frames.
- **`WsBackend`** — return the last received raw frame (from its `latest` one-slot buffer). This
  makes a `--remote` session a **relay**: it re-broadcasts the box it's watching.
- **`HostBackend`, `SysfsBackend`, `LuwenBackend`, `MockBackend`** — `None` in v1 (default).

"Broadcast what the app shows": the publisher emits exactly what the running app displays.

### 2. `src/backend/serve.rs` → `TelemetryPublisher`

A WebSocket server on the `/telemetry` path that fans a frame out to all connected clients.

- Owns a single-worker tokio runtime, **off the render thread** (same containment as `WsBackend`);
  `Drop` sets a stop flag + bounded `shutdown_timeout`.
- Built on the `tokio::net::TcpListener` + `tokio_tungstenite::accept_async` pattern already proven
  in `ws.rs`'s end-to-end test.
- API: `TelemetryPublisher::spawn(bind: SocketAddr) -> BackendResult<Self>` (binds synchronously so
  a port-in-use error surfaces immediately, not on a background thread), `broadcast(&self, frame:
  String)`, `client_count(&self) -> usize`, `bind_addr(&self) -> SocketAddr`.
- Fan-out via a `tokio::sync::watch::Sender<String>` holding the latest frame; each accepted client
  spawns a task that awaits `changed()` and writes the new frame. A client whose write fails (slow
  or gone) drops its own task; others are unaffected. No back-pressure onto the collector.
- Only the `/telemetry` path is served; any other path gets a 404 close. (Matches `WsBackend`'s
  connect URL so `--remote <host:port>` — which targets `/telemetry` — connects cleanly.)

### 3. Wiring (data flow)

Each data-refresh tick (the existing cadence, off the 60fps render path), the app calls
`backend.snapshot_json()`; if `Some(frame)` and a publisher is active, `publisher.broadcast(frame)`.
Connected `--remote` clients receive it on `/telemetry` and parse it with the unchanged
`parse_tt_smi_snapshot`.

## Commands & modes

### `--serve [BIND:PORT]` (launch)

- Optional value; default **`0.0.0.0:8770`**. Port `8770` is distinct from vLLM's `:8000` and
  agentd's `:8765` to avoid collisions; consumers connect with `--remote <box>:8770`. A value may be
  `PORT` (→ `0.0.0.0:PORT`), `HOST:PORT`, or `HOST` (→ `HOST:8770`). `127.0.0.1:8770` restricts to
  local only.
- **Auto-detect display:** if stdout is a TTY, launch the normal TUI and serve alongside it (the
  interactive "serve while I watch" case); if there's no TTY (SSH without `-t`, a pipe, a systemd
  unit), run **headless** — construct the backend, run a collector loop (`update()` +
  `snapshot_json()` + `broadcast()` at `--interval`), and the publisher, with no rendering; log
  status to stderr; run until Ctrl-C / SIGTERM.
- If the resolved backend's `snapshot_json()` is `None`, `--serve` prints a clear error and exits
  non-zero ("broadcast needs the json/hybrid backend — relaunch with `--backend hybrid`").

### `/serve [BIND:PORT]` (in-app)

Mirrors `/remote`'s shape (off-thread worker + `mpsc` outcome, so a blocking bind never stalls the
render loop):

- `/serve` — start broadcasting on the default `0.0.0.0:8770`.
- `/serve 9000` / `/serve 1.2.3.4:9000` — explicit port / bind.
- `/serve off` (alias `/serve stop`) — stop the publisher.
- Re-issuing `/serve` while active → a status `cmd_message` (already serving on …).
- A bind failure (port in use, permission) → a clear error `cmd_message`, never a panic.
- Refuses (with the same "needs json/hybrid" message) if `snapshot_json()` is `None`.
- While active, a status indicator shows in the bar: `◉ serving :8770 · N clients`.

Serving while `--remote`-connected is allowed and is the **relay** case (WsBackend `snapshot_json`
returns the received frame).

## Security

Plaintext `ws://`, unauthenticated, frames broadcast verbatim — **trusted-LAN only**, identical to
the consumer posture. Documented in `--serve` help, the `serve.rs` module docs, and a one-time
`cmd_message` warning on the first `/serve`. Default bind is `0.0.0.0` (so a remote laptop can
reach it); `--serve 127.0.0.1:…` restricts to local.

## Error handling

- **Bind failure** — `/serve`: clear `cmd_message`, no server started, no panic; `--serve` headless:
  stderr message + non-zero exit.
- **Client disconnect / slow client** — that client's task drops; the publisher and other clients
  continue.
- **No clients** — the server runs idle (broadcasts are dropped into the watch channel).
- **`snapshot_json()` == None** — refuse to start with the actionable "needs json/hybrid" message.
- **Malformed nothing to fear** — the publisher only *sends*; it never parses client input.

## Testing

- **Pure `snapshot_json`**: `JSONBackend`/`HybridBackend` return the retained raw output;
  `WsBackend` returns the last frame; the default returns `None`.
- **Loopback end-to-end** (mirrors `ws.rs`'s test): spawn a `TelemetryPublisher`, connect a
  `WsBackend` to it, `broadcast()` a known tt-smi frame, assert the `WsBackend` applies + parses it
  (our publisher feeds our own consumer — closes the loop).
- **Command parsing**: `/serve`, `/serve 9000`, `/serve 1.2.3.4:9000`, `/serve off`, and malformed
  input resolve correctly and never panic (pure parse helper, like `resolve_remote_pick`).
- **Bind-in-use**: `spawn` on an already-bound port returns `Err`, not a panic.

## Feature gating

Behind the existing `remote` cargo feature (it already provides `tokio-tungstenite` + `futures-util`
and the runtime). With the feature off, `--serve`/`/serve` report "not compiled (build with
--features remote)" and the `serve` module isn't compiled — verified by the
`--no-default-features --features tui,json-backend` build.

## Files

- **Create** `src/backend/serve.rs` — `TelemetryPublisher` (WS server, runtime, watch fan-out).
- **Modify** `src/backend/mod.rs` — `snapshot_json()` trait method (default `None`); `#[cfg(feature
  = "remote")] mod serve;` + re-export.
- **Modify** `src/backend/json.rs`, `src/backend/hybrid.rs` — retain last raw `tt-smi -s` output,
  implement `snapshot_json`.
- **Modify** `src/backend/ws.rs` — `snapshot_json` returns the last received frame (relay).
- **Modify** `src/cli.rs` — `--serve` arg (optional value, parsed to a `SocketAddr`).
- **Modify** `src/bin/tui.rs` — `--serve` dispatch: TTY → TUI+serve, no TTY → headless publisher.
- **Modify** `src/ui/tui/mod.rs` — `/serve` command (`ServeOutcome` off-thread bind), the status
  indicator, and the per-tick `broadcast` call; hold the optional `TelemetryPublisher`.

## REVISION (2026-07-05): richer tt-toplike frame (full remote picture)

Field feedback: under `--remote`, only the *chips* reflect the remote box — the process panel shows
**local** processes and the `[i]` tab probes **local** docker, because the stream carries only
`tt-smi -s` telemetry. That's confusing ("looks like I'm watching the box, but the processes are my
laptop's"). This revision makes `tt-toplike --serve` broadcast the **full** picture, keeps agentd's
plain tt-smi stream working as a telemetry-only fallback, and labels the fallback honestly.

### Frame protocol — tt-smi + optional `tt_toplike` extension

The `/telemetry` frame stays valid `tt-smi -s` JSON, with **one optional added top-level key**,
`tt_toplike`, that plain tt-smi consumers ignore (serde ignores unknown fields):

```json
{
  "time": "...", "device_info": [ /* tt-smi, unchanged */ ], /* …tt-smi fields… */,
  "tt_toplike": {                    // OPTIONAL; absent from agentd's current plain frames
    "schema": 1,
    "processes": [ { "pid": 123, "name": "python3", "cmd": "…",
                     "uses_tt": true, "cpu_pct": 30.0, "mem_bytes": 8000000000 } ],
    "inference":  [ /* ServiceState-shaped: key,label,phase,progress,serving{…} */ ]
  }
}
```

- **Backward compatible / one schema:** a richer frame is still parseable `tt-smi` JSON (telemetry
  path unchanged); the extension is additive. agentd's current frames simply lack the key.
- **`snapshot_json()` on the serve side** emits the richer frame: the tt-smi telemetry (raw
  passthrough / relay as before) **plus** the `tt_toplike` object built from the running app's own
  `host_proc_monitor` TT-process rows and `inference_monitor` snapshot. Producing it must not block
  the render thread (built on the data tick, off the 60fps path).

### Consuming remote processes + inference (with local fallback + labeling)

`WsBackend` gains, alongside its telemetry parse, an optional decode of the `tt_toplike` extension,
exposed as `remote_processes()` / `remote_inference()` (empty/None when absent).

Under `--remote`:
- **Process panel:** if `remote_processes()` is present → render the **remote** box's TT process
  rows; else render local processes **labeled "LOCAL — remote processes not streamed"** so nothing
  masquerades as the box.
- **`[i]` tab:** if `remote_inference()` is present → drive the snake/roster from the **remote**
  `ServiceState`s (no local docker probing needed); else the `[i]` tab shows a clear "remote box's
  inference isn't in this stream (telemetry-only source)" state rather than silently probing local
  docker.

### Honest-labeling first (independent, shippable now)

Even before the richer frame lands, the misleading local-masquerade is fixed as the **first task**:
under `--remote` with a telemetry-only stream, the process panel is labeled LOCAL and the `[i]` tab
is gated/labeled so it doesn't imply it's the remote box's inference. This ships immediately and is
subsumed by the richer-frame path once present.

### tt-station coordination

`tt-station`'s `tt-station-agentd` should emit the **same `tt_toplike` extension** on its
`/telemetry` frames so its `--remote` sessions also show the box's processes + inference — see
`~/code/tt-station/TT_TOPLIKE_STREAM.md` (this feature's brief for that repo). Until it does, agentd
stays telemetry-only and tt-toplike falls back+labels as above. The schema is owned here (tt-toplike
is the reference producer/consumer); agentd mirrors it.

## Deferred follow-ups

- Serialize `Host`/`Sysfs`/`Luwen` *telemetry* into the tt-smi schema so a non-tt-smi box can also
  broadcast (the `tt_toplike` extension is backend-independent; only the tt-smi telemetry base needs
  this).
- Optional bearer-token auth to match agentd's paired mode, for untrusted networks.
