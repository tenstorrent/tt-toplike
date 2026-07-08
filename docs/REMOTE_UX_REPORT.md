# Remote-box DISCOVERY + CONNECT UX — implementation report

Additive `--remote` UX on top of the existing `--remote HOST:PORT` `WsBackend`.

## What shipped

### Part 1 — `--remote <name>` name resolution
New module `src/backend/discovery.rs` (gated behind the existing `remote`
feature). It parses `tt --json discover` (a JSON array of
`{name, host, ctrl_port, chips, status, apiver}`) and resolves a requested
target to a `HOST:PORT`:

- **Pure** `resolve_remote_target(json, requested)` — the tested Part-1 contract:
  `HOST:PORT` passthrough (JSON never consulted), bare-name lookup against the
  list → `host:ctrl_port`, or a clear error listing the discovered names.
- **Impure** `resolve_remote_spec(spec)` — CLI wiring: `HOST:PORT`/`[IPv6]:PORT`
  passes straight through (no `tt` call); a bare token is resolved by name via
  `run_tt_discover()`. The `tt` binary is overridable via `TT_BIN` (default `tt`).

Wired at the two remote entry points: `backend/factory.rs` (covers `run_tt`
TUI, `--bench`, backend-switch) and `bin/tui.rs` `main`.

**Bare-host compatibility decision:** `resolve_remote_target` is strict (errors
on no-match, as required and tested). The CLI wiring is deliberately *lenient*:
if discovery is unavailable (`tt` missing/errored) or the name matches no box,
it **falls back to treating the token as a bare host** (WsBackend defaults the
port), so `--remote <bare-host>` / `--remote <IP>` keep working exactly as
before this feature. The reason is logged (`-v`). This resolves the inherent
tension between "return a clear error on no match" and "keep bare-host working
exactly as now" — the strict error lives in the pure fn (and is what the TUI
`/remote <name>` path surfaces); the CLI stays backward-compatible.

### Part 2 — `/remote` TUI slash command (chosen mechanism: **hot-swap (a)**)
`/remote` is dispatched in the event loop's Enter handler (not in
`execute_command`, which lacks backend access):

- `/remote` (no arg) → `tt --json discover`, shows a numbered picker
  (`name · host:port · chips · status`) in a multi-line command bar; boxes are
  cached for selection.
- `/remote <n|name|host:port>` → resolves (1-based index into the last list,
  cached-name, or `HOST:PORT` passthrough; else a fresh discovery-by-name) and
  **hot-swaps the live backend** to a new `WsBackend` via `WsBackend::init()`,
  setting `backend_type = Remote` and resetting per-mode visualizations.

**Off the render thread:** both operations run on a worker `std::thread`; the
render loop shows a `discovering…`/`connecting…` status line and polls a
`std::sync::mpsc` receiver once per tick with `try_recv()`. The blocking work
(mDNS discovery, WebSocket connect + `init()`) never freezes the UI; the
backend hot-swap and viz reset happen on the main thread when the worker
reports back. Esc while pending cancels the wait (drops the receiver; the
worker finishes and its send is discarded).

**Why hot-swap over relaunch:** the runtime backend-swap machinery already
exists — the `b` key does `*backend = new_backend` at runtime. `/remote` reuses
that exact mechanism (`*backend = Box::new(ws)`), so no relaunch/exec is needed
and it's a natural, low-risk fit for the current architecture. Failures
(discover error, no boxes, connect failure) report a status message and leave
the current backend untouched.

## Additive-safety confirmation
- No existing backend, the default, auto-detect order, or Tab/`b`-cycle behavior
  changed. `Remote` is still never a target of `create_auto_backend` or
  `next_backend`.
- New code paths are entered *only* via the explicit `--remote` flag or the
  explicit `/remote` command, and are gated behind the `remote` feature
  (`--no-default-features` build verified clean, both `cargo build` and clippy).
- `render_command_bar`'s multi-line branch only triggers for messages that
  contain newlines (i.e. the `/remote` picker) — the single-line prompt/result
  path is byte-for-byte the prior behavior.

## Tests
- `backend::discovery` — 10 unit tests: match / no-match (lists names) /
  no-boxes / malformed-JSON / `HOST:PORT` passthrough / lenient parse /
  `host:port` detection / `TT_BIN` override. JSON is injected, no `tt` shelled.
- `ui::tui::cmd_tests::remote` — 5 unit tests on `resolve_remote_pick` (pure):
  index / out-of-range / name / `HOST:PORT` passthrough / unknown-without-list.
- Full runtime hot-swap/connect needs a live WS box → **manual verification**:
  launch TUI, `/remote` to list, `/remote <n>` to connect, watch the header
  switch to `Remote (WebSocket QuietBox)`.

## Manual end-to-end verification (Part 1, with a fake `TT_BIN`)
- `--remote qb2-lab` → connected target `ws://10.9.8.7:8899/telemetry` ✓
- `--remote 127.0.0.1:9999` → passthrough, no `tt` call ✓
- `--remote nope` (tt present, no match) → warns + lists `qb2-lab`, falls back
  to `nope:8000` ✓
- `TT_BIN` missing → warns "discovery unavailable", falls back to bare host ✓

## Concerns / notes
- A bare-name `--remote` shells `tt --json discover` (mDNS browse ≈ a timeout)
  before connecting; the `HOST:PORT` fast path is unaffected. Bare-host usage
  pays this discovery cost too (unavoidable — a bare token is syntactically
  indistinguishable from a name). Discovery is now bounded by a 10s timeout
  (`DISCOVER_TIMEOUT`) so a wedged mDNS lookup can't hang the caller.
- Startup discovery runs **once**: `bin/tui.rs` resolves + constructs the
  `WsBackend` and hands it to the run path, which passes it through to the TUI
  instead of letting the backend factory resolve `--remote` a second time.
