# Remote QuietBox — additive `WsBackend` report

## Status: complete

Additive `WsBackend` implemented — tt-toplike consumes a Tenstorrent box's
telemetry over a WebSocket, treating each frame exactly like local telemetry.

## What was built

1. **Pure parser refactor (`src/backend/json.rs`)** — extracted a shared,
   pure function `pub(crate) fn parse_tt_smi_snapshot(&str) -> BackendResult<ParsedSnapshot>`
   where `ParsedSnapshot { devices: Vec<Device>, telemetry, smbus }` is the exact
   triad every backend serves. `JSONBackend::init`/`update` now route through it
   (via a new `merge_parsed` that preserves the historical append-only device /
   per-index-replace semantics — behavior unchanged). The old `parse_snapshot`
   helper (used by `HybridBackend`) kept working; its result struct was renamed
   `SnapshotParts` to free the `ParsedSnapshot` name (Hybrid never names the type).

2. **`src/backend/ws.rs`** — `WsBackend` implements the unchanged
   `TelemetryBackend` trait. A dedicated background Tokio runtime holds the
   `ws://<host>:<port>/telemetry` connection (tokio-tungstenite), drops each
   received frame into a one-slot latest-frame buffer, and reconnects on drop.
   `update()` re-parses the newest frame via `parse_tt_smi_snapshot` (non-blocking,
   generation-guarded); `init()` blocks for the first frame or times out. Connect
   / disconnect / parse / bad-frame errors are surfaced as `BackendResult`, never
   panic. `backend_info()` → `"remote QuietBox @ <host>:<port> (connected, N device(s))"`.

3. **Wiring (additive)** — `--remote <HOST:PORT>` flag + `BackendType::Remote`
   (`src/cli.rs`); construction in `src/backend/factory.rs::create_backend` and in
   the inline backend selector `src/bin/tui.rs`. `--remote` is the sole entry point.

## How `--remote` selects WsBackend

`--remote HOST:PORT` sets `BackendType::Remote` in `parse_args`/`effective_backend`
(conflicts with `--mock`/`--json`/`--host`). Both `create_backend` and tui.rs match
`Remote` → `WsBackend::from_host_port(...)`. Auto-detect (`create_auto_backend`)
and Tab-cycling (`next_backend`) deliberately never target `Remote`; `next_backend`
maps `Remote` → local cycle start so Tab exits remote but can never re-enter it.

## Name resolution

**host:port only** for this slice (bare `HOST` defaults to port 8000, the agentd
control port; bracketed IPv6 `[::1]:8765` supported). `tt --json discover` name
resolution was NOT implemented (documented as the optional follow-up in the design).

## Additive-safety confirmation

No existing backend, the trait, the default, or the auto-detect/Tab-cycle order
changed. The `json.rs` change is a pure refactor: all pre-existing JSON tests pass
unchanged, and `JSONBackend`'s subprocess path preserves append-only device merge.
The full lib suite passes green (exact counts drift as tests are added — run
`cargo test --lib --features tui` for the current tally), including the backend
module's in-process fake-server end-to-end test and the inject-based WS unit
tests. `cargo build` OK; `cargo clippy` clean on touched files; `cargo fmt --check`
clean.

## Concerns / notes

- `tokio-tungstenite = "0.24"` + `futures-util` are optional, gated behind the
  `remote` cargo feature (in `default`, so normal builds/UX are unchanged).
  Packagers who don't want the WS stack build with `--no-default-features`; the
  `Remote` variant then returns a clean "built without the 'remote' feature" error.
- WS stream is unauthed (matches the design's v1 decision, consistent with the box's
  unauthed `/status`/`/models`). Fine on a trusted LAN; not for open networks.
- `WsBackend::init` blocks up to `--timeout` ms for the first frame; a slow/absent
  box surfaces a clean initialization error (verified against a refused port — no panic).
