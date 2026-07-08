# `--serve` / `/serve` Telemetry Publisher — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** tt-toplike can broadcast a `/telemetry` WebSocket that any tt-toplike `--remote` consumes —
carrying chips (tt-smi) **plus** the box's processes + inference status — with an honest local
fallback when the stream is telemetry-only (today's agentd).

**Architecture:** A `TelemetryBackend::snapshot_json()` produces the current frame (raw tt-smi
passthrough / relay). A `TelemetryPublisher` (own tokio runtime, off the render thread, watch-channel
fan-out) serves it on `/telemetry`. The frame is valid tt-smi JSON + an optional additive
`tt_toplike` extension (processes + inference). `--remote` decodes the extension when present and
renders the remote box's process panel + `[i]` tab; otherwise it labels those views LOCAL.

**Tech Stack:** Rust, tokio + tokio-tungstenite (behind the `remote` feature), serde_json, ratatui.

## Global Constraints

- Everything new is behind the `remote` cargo feature (`#[cfg(feature = "remote")]`); with it off,
  `--serve`/`/serve` print "not compiled (build with --features remote)" and nothing references the
  serve module. Verified by `cargo check --no-default-features --features tui,json-backend`.
- No blocking I/O on the render/event thread. The WS server + socket bind run off-thread (own tokio
  runtime like `WsBackend`; `/serve` binds via an off-thread `ServeOutcome` worker like `/remote`).
- Panic-free: no `unwrap`/`expect`/indexing on network or parsed data in non-test code.
- Frame contract: valid `tt-smi -s` JSON + one optional top-level key `tt_toplike` (`schema: 1`);
  additive, serde ignores it on the telemetry parse.
- Defaults: bind `0.0.0.0:8770`; path `/telemetry`; plaintext/unauthed/trusted-LAN (documented).
- Verify each task: `cargo test --locked --lib --features tui,remote`; clippy
  `--features tui,remote -- -D warnings`; `cargo fmt`. Never pipe cargo through `tail`/`head`
  (append `; echo "exit: $?"`).

---

### Task 1: Honest LOCAL labeling under `--remote` (shippable, independent)

Removes the immediate "local processes masquerade as the remote box" confusion, before richer frames
land. Subsumed later by real remote data (Task 9) — the helper stays as the fallback path.

**Files:**
- Modify: `src/ui/tui/mod.rs` (process-panel header ~line 706/1380 region; `[i]` view gating in
  `render_snake_view` / the InferenceMonitor branch)

**Interfaces — Produces:**
- `fn remote_local_fallback_note(backend_type: BackendType, has_remote_data: bool) -> Option<&'static str>`
  — `Some("LOCAL — remote not streamed")` when `backend_type == Remote && !has_remote_data`, else
  `None`. (In Task 9, `has_remote_data` is wired to the WsBackend extension presence; for now pass
  `false`.)

- [ ] **Step 1: failing test** in `mod.rs` tests:
```rust
#[test]
fn remote_fallback_note_only_for_remote_without_data() {
    use crate::cli::BackendType;
    assert_eq!(remote_local_fallback_note(BackendType::Remote, false), Some("LOCAL — remote not streamed"));
    assert_eq!(remote_local_fallback_note(BackendType::Remote, true), None); // remote data present
    assert_eq!(remote_local_fallback_note(BackendType::Json, false), None);  // local backend, no note
}
```
- [ ] **Step 2:** run `cargo test --lib --features tui remote_fallback_note` → FAIL (undefined).
- [ ] **Step 3:** implement the helper (pure `match`).
- [ ] **Step 4:** in the process-panel title and the `[i]` view, when the note is `Some`, append it to
      the panel/tab header (process panel: append to its title Span; `[i]`: a dim header line via the
      existing roster/header slot, or a one-line banner atop the snake). Use `backend_type` (already
      in scope) and `false` for `has_remote_data` (Task 9 rewires it).
- [ ] **Step 5:** run the test → PASS; `cargo fmt`.
- [ ] **Step 6:** commit `feat(remote): label process panel + [i] as LOCAL when only telemetry is streamed`.

---

### Task 2: `TelemetryBackend::snapshot_json()` + per-backend impls

**Files:**
- Modify: `src/backend/mod.rs` (trait method, default `None`)
- Modify: `src/backend/json.rs`, `src/backend/hybrid.rs` (retain last raw tt-smi output)
- Modify: `src/backend/ws.rs` (return last received frame)

**Interfaces — Produces:**
- `trait TelemetryBackend { … fn snapshot_json(&self) -> Option<String> { None } }`

- [ ] **Step 1: failing test** in `json.rs` tests: construct a `JSONBackend` whose `run_tt_smi`
      returns a canned string (inject via the existing test seam / a stored `last_raw` field), call
      `snapshot_json()`, assert it returns that string. In `ws.rs` tests: after `set_frame("X")`,
      `snapshot_json()` returns `Some("X")`.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** add the trait method (default `None`). In `JSONBackend`/`HybridBackend`, store the
      raw tt-smi stdout on each `update()` in a `last_raw: Option<String>` field and return its clone
      from `snapshot_json`. In `WsBackend`, return `self.shared.latest.lock().unwrap().clone()`.
- [ ] **Step 4:** run → PASS.
- [ ] **Step 5:** commit `feat(backend): snapshot_json() — current telemetry as tt-smi JSON (passthrough/relay)`.

---

### Task 3: `tt_toplike` extension — structs, build, inject, parse

**Files:**
- Create: `src/backend/remote_ext.rs` (serde structs + helpers; `#[cfg(feature = "remote")]`)
- Modify: `src/backend/mod.rs` (`#[cfg(feature = "remote")] mod remote_ext; pub use …`)

**Interfaces — Produces:**
```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TtToplikeExt { pub schema: u32, pub processes: Vec<RemoteProc>, pub inference: Vec<RemoteInference> }
pub struct RemoteProc { pub pid: u32, pub name: String, pub cmd: String, pub uses_tt: bool, pub cpu_pct: f32, pub mem_bytes: u64 }
pub struct RemoteInference { pub key: String, pub label: String, pub phase: String, pub progress: Option<f32>, pub serving: Option<RemoteServing> }
pub struct RemoteServing { /* mirror ServingStats display fields: generation_tps, prompt_tps, requests_running, requests_waiting, kv_cache_usage, ttft_avg_s, queue_avg_s, prefill_avg_s, decode_avg_s, tpot_avg_s, completed_delta, errored_delta, prefix_hit_rate, preemptions_delta */ }
pub const TT_TOPLIKE_SCHEMA: u32 = 1;
pub fn inject_extension(tt_smi_json: &str, ext: &TtToplikeExt) -> String; // parse Value, insert "tt_toplike", re-serialize; on parse failure return tt_smi_json unchanged
pub fn parse_extension(frame: &str) -> Option<TtToplikeExt>; // None if key absent, unparsable, or schema != TT_TOPLIKE_SCHEMA
```

- [ ] **Step 1: failing tests**: (a) `inject_extension` into a minimal tt-smi frame then
      `parse_extension` round-trips the ext; (b) a plain tt-smi frame (no key) → `parse_extension`
      returns `None`; (c) a frame with `tt_toplike.schema = 999` → `None`; (d) `inject_extension`
      output still parses as tt-smi (`parse_tt_smi_snapshot` succeeds on it).
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** implement the structs + helpers with serde_json `Value` manipulation.
- [ ] **Step 4:** run → PASS.
- [ ] **Step 5:** commit `feat(remote): tt_toplike frame extension (processes + inference) — inject/parse`.

---

### Task 4: `TelemetryPublisher` (WS server on `/telemetry`)

**Files:**
- Create: `src/backend/serve.rs` (`#[cfg(feature = "remote")]`)
- Modify: `src/backend/mod.rs` (`#[cfg(feature = "remote")] mod serve; pub use serve::TelemetryPublisher;`)

**Interfaces — Produces:**
```rust
pub struct TelemetryPublisher { /* runtime handle, watch::Sender<String>, client count Arc<AtomicUsize>, bind_addr, stop */ }
impl TelemetryPublisher {
    pub fn spawn(bind: std::net::SocketAddr) -> crate::error::BackendResult<Self>; // binds synchronously; Err on bind failure
    pub fn broadcast(&self, frame: String);      // updates the watch channel
    pub fn client_count(&self) -> usize;
    pub fn bind_addr(&self) -> std::net::SocketAddr;
}
// Drop: signal stop + bounded shutdown_timeout(200ms), mirroring WsBackend.
```
Server: `std::net::TcpListener::bind(bind)?` synchronously (surfaces the error), set non-blocking,
hand to `tokio::net::TcpListener::from_std` inside the runtime; accept loop upgrades each stream with
`tokio_tungstenite::accept_async`, checks the request path is `/telemetry` (else close), then the
per-client task `subscribe()`s the `watch` and writes each changed frame; on write error it drops
(decrementing the client count). Mirror the accept-side pattern already in `ws.rs`'s end-to-end test.

- [ ] **Step 1: failing loopback test** in `serve.rs`: `let pub_ = TelemetryPublisher::spawn("127.0.0.1:0")?;`
      (port 0 = OS-assigned; read back via `bind_addr()`), then construct a `WsBackend` pointed at
      `pub_.bind_addr()`, `init()` it, `pub_.broadcast(FRAME)`, poll `backend.update()` until it
      applies, assert `backend.device_count()` matches `FRAME`. Also: `TelemetryPublisher::spawn` on
      an already-bound addr returns `Err`, not a panic.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** implement the publisher.
- [ ] **Step 4:** run → PASS (`cargo test --features tui,remote serve::`).
- [ ] **Step 5:** commit `feat(serve): TelemetryPublisher — /telemetry WebSocket server with watch fan-out`.

---

### Task 5: `WsBackend` exposes decoded remote processes + inference

**Files:**
- Modify: `src/backend/ws.rs`

**Interfaces — Consumes:** `parse_extension` (Task 3). **Produces:**
- `impl WsBackend { pub fn remote_processes(&self) -> Option<Vec<RemoteProc>>; pub fn remote_inference(&self) -> Option<Vec<RemoteInference>>; }`
  (decoded from the latest applied frame; `None` when the frame had no `tt_toplike`).

- [ ] **Step 1: failing test:** feed a frame WITH a `tt_toplike` ext (via the loopback publisher or
      `set_frame`), assert `remote_inference()` is `Some` with the expected model; feed a plain
      tt-smi frame, assert both accessors are `None`.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** on `apply()`/frame update, `parse_extension` the raw frame and store
      `Option<TtToplikeExt>` behind the existing lock; expose the accessors.
- [ ] **Step 4:** run → PASS.
- [ ] **Step 5:** commit `feat(remote): WsBackend decodes tt_toplike extension (remote processes + inference)`.

---

### Task 6: `--serve` CLI arg + bind parsing

**Files:**
- Modify: `src/cli.rs`

**Interfaces — Produces:**
- `pub serve: Option<String>` on `Cli` (clap `long`, value optional via `num_args(0..=1)` +
  `default_missing_value("0.0.0.0:8770")`).
- `pub fn parse_serve_bind(spec: &str) -> Result<std::net::SocketAddr, String>` — `PORT` →
  `0.0.0.0:PORT`; `HOST:PORT` → as-is; `HOST` → `HOST:8770`; error (not panic) on garbage.

- [ ] **Step 1: failing tests:** `parse_serve_bind("8770") == 0.0.0.0:8770`; `("127.0.0.1:9000")`
      as-is; `("192.168.1.5") -> …:8770`; `("nonsense:x")` is `Err`; `("[::1]:8770")` IPv6 ok.
- [ ] **Step 2–4:** implement + pass.
- [ ] **Step 5:** commit `feat(cli): --serve [BIND:PORT] flag + bind parsing (default 0.0.0.0:8770)`.

---

### Task 7: `--serve` dispatch in `bin/tui.rs` (TTY → TUI+serve, no TTY → headless)

**Files:**
- Modify: `src/bin/tui.rs`

**Interfaces — Consumes:** `parse_serve_bind`, `TelemetryPublisher`, `snapshot_json`, the extension
builder (Task 8's `build_extension`). Note: gate the whole arm on `#[cfg(feature = "remote")]`; the
`#[cfg(not)]` arm prints the not-compiled error + exits.

- [ ] **Step 1:** when `cli.serve.is_some()`: resolve bind (exit(1) on parse err). If
      `std::io::stdout().is_terminal()` → set a flag so `run_tui` starts a publisher at startup
      (pass the resolved bind through to `run_tui`, e.g. via `cli.serve` already holding the
      resolved string). Else (no TTY) → **headless**: build the backend, `spawn(bind)` the publisher
      (exit on Err), then loop: `backend.update()`, build the richer frame
      (`inject_extension(snapshot_json()?, build_extension(local proc rows, inference snapshot))`),
      `publisher.broadcast(frame)`, sleep `cli.interval`; log client count periodically; run until
      Ctrl-C. If `snapshot_json()` is `None`, print the "needs json/hybrid" error and exit.
- [ ] **Step 2:** manual smoke (documented in the commit body): `cargo run --features tui,remote --
      --serve 127.0.0.1:8771 --backend json` in one shell (piped, no TTY → headless), `--remote
      127.0.0.1:8771` in another; confirm connect. (No unit test — this is process wiring; the
      loopback test in Task 4 covers the publisher.)
- [ ] **Step 3:** commit `feat(serve): --serve dispatch — TUI+serve with a TTY, headless publisher without`.

---

### Task 8: `/serve` in-app command + status + per-tick broadcast

**Files:**
- Modify: `src/ui/tui/mod.rs`
- Modify (add if not present): a `build_extension(proc_rows, inference_snapshot) -> TtToplikeExt`
  helper (in `remote_ext.rs`, Task 3, or here) mapping the app's `ProcRow`s + `ServiceState`s to the
  wire structs.

**Interfaces — Consumes:** `TelemetryPublisher`, `parse_serve_bind`, `snapshot_json`,
`inject_extension`, `build_extension`. **Produces:** `enum ServeOutcome { Started(SocketAddr) |
Failed(String) }` handled like `RemoteOutcome`; a `Option<TelemetryPublisher>` held in the loop.

- [ ] **Step 1: failing test** for the pure command parse: `parse_serve_command("/serve")` →
      `Serve(Some(default))`, `"/serve 9000"` → bind :9000, `"/serve 1.2.3.4:9000"`, `"/serve off"` →
      `Stop`, `"/serve wat"` → `Err`. (Model on the `/remote` parse helper.)
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** implement `/serve` in `execute_command`: parse; on start, spawn the off-thread bind
      worker → `ServeOutcome` via mpsc (never block render); on `Started`, store the publisher +
      `cmd_message("serving on … (trusted-LAN, unauthed)")`; on `Failed`, error `cmd_message`; refuse
      with the "needs json/hybrid" message when `snapshot_json()` is `None`; `off`/`stop` drops the
      publisher. Each data tick, if a publisher is held and `snapshot_json()` is `Some`, broadcast
      `inject_extension(frame, build_extension(proc_rows, inference_snapshot))`. Show a status
      indicator (`◉ serving :PORT · N clients`) in the bar. Also wire Task 1's
      `remote_local_fallback_note` `has_remote_data` to `WsBackend::remote_*().is_some()`.
- [ ] **Step 4:** run tests → PASS.
- [ ] **Step 5:** commit `feat(serve): /serve [BIND:PORT] in-app command + status + per-tick broadcast`.

---

### Task 9: `--remote` renders remote processes + inference (richer frame)

**Files:**
- Modify: `src/ui/tui/mod.rs` (process panel + `[i]` view source selection)

**Interfaces — Consumes:** `WsBackend::remote_processes()` / `remote_inference()`.

- [ ] **Step 1:** downcast/observe the active backend as `WsBackend` under `backend_type == Remote`
      (add a `remote_snapshot()` accessor to the backend trait returning
      `Option<(Vec<RemoteProc>, Vec<RemoteInference>)>`, default `None`, so no `dyn` downcast is
      needed — cleaner). Test: the trait method returns the WsBackend extension when present.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** in the process panel: under Remote with `remote_processes()` present, render those
      rows (map `RemoteProc` → the panel's row type) instead of the local scan; else keep local +
      Task 1's LOCAL label. In the `[i]` branch: under Remote with `remote_inference()` present, build
      the `SnakeWorld.rows` from the remote `ServiceState`s (map `RemoteInference` → `ServiceState`)
      and skip local docker probing; else the labeled fallback. Map helpers get small unit tests.
- [ ] **Step 4:** run → PASS; full gate + a manual loopback (`--serve` on one instance shows a model,
      `--remote` on another shows that model in `[i]` + the box's processes).
- [ ] **Step 5:** commit `feat(remote): render remote processes + [i] inference from the tt_toplike extension`.

---

## Self-Review

- **Spec coverage:** snapshot_json (T2), publisher (T4), richer frame inject/parse (T3), consume (T5),
  cli --serve (T6), headless/TUI dispatch (T7), /serve command (T8), remote rendering + fallback
  labeling (T1, T9), feature gating + trusted-LAN posture (Global Constraints). Deferred non-tt-smi
  telemetry serialization is out of scope per spec. ✓
- **Type consistency:** `TtToplikeExt`/`RemoteProc`/`RemoteInference`/`RemoteServing`,
  `TT_TOPLIKE_SCHEMA=1`, `snapshot_json() -> Option<String>`, `parse_extension`, `inject_extension`,
  `TelemetryPublisher::{spawn,broadcast,client_count,bind_addr}`, `parse_serve_bind`,
  `remote_processes/remote_inference/remote_snapshot` used consistently across tasks. ✓
- **Ordering:** T1 ships alone; T2–T6 are backend/lib (no UI); T7–T9 wire UI + entrypoints on top.
