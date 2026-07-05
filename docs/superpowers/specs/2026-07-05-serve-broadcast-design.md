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

## Deferred follow-ups

- Serialize `Host`/`Sysfs`/`Luwen` snapshots into the tt-smi schema so any backend can broadcast.
- Optional bearer-token auth to match agentd's paired mode, for untrusted networks.
