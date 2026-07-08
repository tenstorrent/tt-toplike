// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! WebSocket backend — the consumer half of "remote QuietBox".
//!
//! `WsBackend` reads a Tenstorrent box's telemetry over a WebSocket, exactly
//! like local telemetry. It connects to `ws://<host>:<port>/telemetry`, where a
//! `tt-station-agentd` publisher pushes frames on an interval. **Each frame is
//! the verbatim stdout of `tt-smi -s`** — the same JSON the local [`JSONBackend`]
//! already parses — so this backend reuses the exact same parser
//! ([`parse_tt_smi_snapshot`]). One schema, zero mapping.
//!
//! ## Security
//!
//! The transport is plaintext `ws://` with no authentication, and each frame is
//! trusted and parsed verbatim. This is a **trusted-LAN-only** feature — do not
//! point it across an untrusted network. As defense-in-depth against a
//! malformed/hostile frame, incoming messages are size-capped (see the reader)
//! and the device list is bounded ([`MAX_REMOTE_DEVICES`]).
//!
//! ## Architecture
//!
//! ```text
//! box: tt-station-agentd --(ws frames = tt-smi -s stdout)--> WsBackend (this) --> UI
//! ```
//!
//! A background Tokio task owns the connection and drops each received frame into
//! a one-slot latest-frame buffer (a push source with back-pressure sidestepped
//! by design — only the newest frame is kept). The synchronous [`TelemetryBackend`]
//! trait is satisfied by `update()` re-parsing whatever the task last delivered.
//! The task reconnects automatically after a drop.
//!
//! ## Strictly additive
//!
//! This is a new sibling backend. It changes no existing backend, the trait, the
//! default, or the auto-detect path. It is entered only via an explicit
//! `--remote <HOST:PORT>` (`BackendType::Remote`), never by auto-detect or Tab.

use crate::backend::json::{parse_tt_smi_snapshot, ParsedSnapshot};
use crate::backend::remote_ext::{parse_extension, RemoteInference, RemoteProc, TtToplikeExt};
use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Device, SmbusTelemetry, Telemetry};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Upper bound on devices tracked from a remote peer. The WS stream is
/// unauthenticated plaintext (trusted-LAN only); this caps persistent state so a
/// malformed/hostile frame can't OOM us. Far above any real topology (a
/// Blackhole Galaxy is 32 chips).
const MAX_REMOTE_DEVICES: usize = 256;

/// State shared between the synchronous backend and the async WS reader task.
///
/// The reader writes the newest frame + bumps `generation`; `update()` reads it.
/// Only the latest frame is retained (one-slot buffer), so a slow UI never
/// applies back-pressure to the stream — it just skips intermediate frames.
#[derive(Default)]
struct Shared {
    /// Newest received frame (raw `tt-smi -s` JSON). `None` until the first arrives.
    latest: Mutex<Option<String>>,
    /// Bumped on every new frame; `update()` compares it to detect fresh data.
    generation: AtomicU64,
    /// Whether the WebSocket is currently connected (for status display).
    connected: AtomicBool,
    /// Instant the most recent frame landed. `None` until the first arrives.
    /// Lets `backend_info()` detect a *silent stall* — socket still open but
    /// frames stopped — which `connected` alone can't see. Same Mutex pattern
    /// as `latest`; touched once per frame, off the hot render path.
    last_frame_at: Mutex<Option<Instant>>,
    /// Most recent connection/parse error message, for `backend_info()` context.
    last_error: Mutex<Option<String>>,
}

impl Shared {
    fn set_frame(&self, frame: String) {
        *self.latest.lock().unwrap() = Some(frame);
        *self.last_frame_at.lock().unwrap() = Some(Instant::now());
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn set_error(&self, msg: String) {
        *self.last_error.lock().unwrap() = Some(msg);
    }
}

/// WebSocket telemetry backend for a remote Tenstorrent box (remote QuietBox).
pub struct WsBackend {
    /// Remote host (hostname or IP).
    host: String,
    /// Remote control port.
    port: u16,
    /// Fully-formed `ws://host:port/telemetry` URL.
    url: String,

    /// Backend configuration (timeouts, verbosity).
    config: BackendConfig,

    /// Cached device list (append-only, stable across updates — like JSONBackend).
    devices: Vec<Device>,
    /// Current per-device core telemetry.
    telemetry: HashMap<usize, Telemetry>,
    /// Current per-device SMBUS telemetry.
    smbus: HashMap<usize, SmbusTelemetry>,
    /// Decoded `tt_toplike` extension (processes + inference) from the latest
    /// *successfully applied* frame. `None` when that frame carried no
    /// extension (a plain `tt-smi -s` frame, or a peer that isn't a
    /// `tt-toplike --serve` publisher) or no frame has arrived yet. Re-derived
    /// from scratch on every applied frame — see `apply_latest` — so this
    /// always tracks the newest frame, never a stale union of past ones. Plain
    /// field (not behind `Shared`'s mutex): only `apply_latest` (`&mut self`)
    /// writes it, and the accessors below just clone out of it.
    remote_ext: Option<TtToplikeExt>,

    /// Shared latest-frame buffer written by the reader task.
    shared: Arc<Shared>,
    /// Generation of the frame last applied by `update()`.
    last_gen: u64,

    /// Dedicated runtime driving the reader task. `None` in unit tests that
    /// inject frames directly rather than opening a real socket.
    runtime: Option<tokio::runtime::Runtime>,
    /// Signals the reader task (and its reconnect loop) to stop, set on Drop.
    stop: Arc<AtomicBool>,
}

impl WsBackend {
    /// Create a new WebSocket backend for `host:port`.
    ///
    /// Connects to `ws://<host>:<port>/telemetry`. No network activity happens
    /// until [`init`](TelemetryBackend::init).
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self::with_config(host, port, BackendConfig::default())
    }

    /// Create a new WebSocket backend with an explicit configuration.
    pub fn with_config(host: impl Into<String>, port: u16, config: BackendConfig) -> Self {
        let host = host.into();
        let url = format!("ws://{}:{}/telemetry", host, port);
        Self {
            host,
            port,
            url,
            config,
            devices: Vec::new(),
            telemetry: HashMap::new(),
            smbus: HashMap::new(),
            remote_ext: None,
            shared: Arc::new(Shared::default()),
            last_gen: 0,
            runtime: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Parse a `HOST:PORT` string into a backend. Bare host defaults to port 8000
    /// (the tt-station-agentd control port). IPv6 in brackets is supported, e.g.
    /// `[::1]:8765`.
    pub fn from_host_port(spec: &str, config: BackendConfig) -> BackendResult<Self> {
        let (host, port) = split_host_port(spec)?;
        Ok(Self::with_config(host, port, config))
    }

    /// Merge a parsed snapshot into cached state.
    ///
    /// Append-only device list keyed by index (devices never disappear across
    /// updates); telemetry and SMBUS maps replaced per-index. Identical semantics
    /// to `JSONBackend`, so every visualization consumes this backend unchanged.
    fn merge_parsed(&mut self, parsed: ParsedSnapshot) {
        // The WS stream is unauthenticated (trusted-LAN only), so a malformed or
        // hostile frame must not grow our state without bound. Real hardware tops
        // out far below MAX_REMOTE_DEVICES (a Blackhole Galaxy is 32 chips), so
        // this cap never bites a legitimate box while defusing an OOM vector.
        for device in parsed.devices {
            if self.devices.len() >= MAX_REMOTE_DEVICES {
                break;
            }
            if !self.devices.iter().any(|d| d.index == device.index) {
                self.devices.push(device);
            }
        }
        for (idx, telem) in parsed.telemetry {
            if self.telemetry.len() >= MAX_REMOTE_DEVICES && !self.telemetry.contains_key(&idx) {
                continue;
            }
            self.telemetry.insert(idx, telem);
        }
        for (idx, smbus) in parsed.smbus {
            if self.smbus.len() >= MAX_REMOTE_DEVICES && !self.smbus.contains_key(&idx) {
                continue;
            }
            self.smbus.insert(idx, smbus);
        }
    }

    /// Apply the newest received frame if it is fresher than the last applied one.
    ///
    /// Returns `Ok(true)` if a new frame was applied, `Ok(false)` if there was
    /// nothing new, or an error if the frame failed to parse (surfaced, not
    /// panicked). A parse error still advances `last_gen` so a single bad frame
    /// isn't re-parsed on every tick.
    fn apply_latest(&mut self) -> BackendResult<bool> {
        let gen = self.shared.generation.load(Ordering::Acquire);
        if gen == self.last_gen {
            return Ok(false);
        }
        let frame = self.shared.latest.lock().unwrap().clone();
        self.last_gen = gen;
        let Some(frame) = frame else {
            return Ok(false);
        };
        match parse_tt_smi_snapshot(&frame) {
            Ok(parsed) => {
                self.merge_parsed(parsed);
                // Re-derive from this frame every time (not merged with the
                // previous value): `None` here means *this* frame carried no
                // extension, and that must be what the accessors report, even
                // if an earlier frame had one.
                self.remote_ext = parse_extension(&frame);
                Ok(true)
            }
            Err(e) => {
                self.shared.set_error(format!("frame parse error: {}", e));
                Err(e)
            }
        }
    }

    /// Spawn the background reader runtime + task. Idempotent-ish: only called
    /// once from `init()`.
    fn spawn_reader(&mut self) -> BackendResult<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("ws-telemetry-reader")
            .build()
            .map_err(|e| {
                BackendError::Initialization(format!("failed to build WS runtime: {}", e))
            })?;

        let shared = Arc::clone(&self.shared);
        let stop = Arc::clone(&self.stop);
        let url = self.url.clone();
        let verbose = self.config.verbose;
        // Reconnect backoff derives from the read timeout but is capped so we
        // retry a downed box on a sane cadence.
        let reconnect = Duration::from_millis(self.config.read_timeout_ms.clamp(500, 5000));

        runtime.spawn(reader_loop(url, shared, stop, reconnect, verbose));
        self.runtime = Some(runtime);
        Ok(())
    }

    // ── Test hooks ────────────────────────────────────────────────────────────

    /// Test-only: construct a backend with no live socket, for frame injection.
    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self::with_config("test", 0, BackendConfig::default())
    }

    /// Test-only: push a frame into the shared buffer as if the reader received it.
    #[cfg(test)]
    pub(crate) fn test_inject(&self, frame: &str) {
        self.shared.set_frame(frame.to_string());
    }
}

impl TelemetryBackend for WsBackend {
    fn init(&mut self) -> BackendResult<()> {
        log::info!("WsBackend: connecting to {}", self.url);
        self.spawn_reader()?;

        // Block for the first frame (or time out) so the UI starts with devices.
        let deadline =
            Instant::now() + Duration::from_millis(self.config.read_timeout_ms.max(1000));
        while Instant::now() < deadline {
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            match self.apply_latest() {
                Ok(true) => {
                    if !self.devices.is_empty() {
                        log::info!(
                            "WsBackend: connected to {} — {} device(s) from first frame",
                            self.url,
                            self.devices.len()
                        );
                        return Ok(());
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    // A bad first frame is surfaced but we keep waiting for a good one.
                    log::debug!("WsBackend: {}", e);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let last_err = self
            .shared
            .last_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "no telemetry frame received".to_string());
        Err(BackendError::Initialization(format!(
            "WsBackend: timed out connecting to {} ({})",
            self.url, last_err
        )))
    }

    fn update(&mut self) -> BackendResult<()> {
        // Non-blocking: apply whatever the reader task last delivered.
        self.apply_latest()?;
        Ok(())
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry> {
        self.telemetry.get(&device_idx)
    }

    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        self.smbus.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        let connected = self.shared.connected.load(Ordering::Relaxed);
        let last_frame_at = *self.shared.last_frame_at.lock().unwrap();
        let devices = self.devices.len();

        if !connected {
            return format!(
                "remote QuietBox @ {}:{} (disconnected, {} device(s))",
                self.host, self.port, devices
            );
        }

        // Connected: distinguish a live stream from a silent stall using the
        // age of the last frame. `staleness_word` decides "connected" vs "stale".
        match last_frame_at {
            Some(t) => {
                let age = t.elapsed().as_secs();
                let interval_secs = self.config.update_interval_ms / 1000;
                format!(
                    "remote QuietBox @ {}:{} ({}, last frame {}s ago, {} device(s))",
                    self.host,
                    self.port,
                    staleness_word(age, interval_secs),
                    age,
                    devices
                )
            }
            // Socket up but no frame has arrived yet.
            None => format!(
                "remote QuietBox @ {}:{} (connected, awaiting first frame, {} device(s))",
                self.host, self.port, devices
            ),
        }
    }

    fn snapshot_json(&self) -> Option<String> {
        // Relay the last raw frame received from the remote publisher — itself
        // verbatim `tt-smi -s` stdout by contract (see module docs). Deliberately
        // independent of `apply_latest`/`last_gen`: this always reflects the
        // newest bytes the reader task has seen, even if that frame failed to
        // parse (a --serve re-broadcaster downstream may have a newer/different
        // parser than we do; we don't want a local parse hiccup to blank the
        // relay). `.lock().ok()` — never panics on a poisoned lock.
        self.shared.latest.lock().ok().and_then(|g| g.clone())
    }

    /// Decoded remote processes from the `tt_toplike` extension on the latest
    /// applied frame. `None` if that frame carried no extension (a plain
    /// `tt-smi -s` frame from a non-`tt-toplike` publisher) or none has
    /// arrived yet. Cheap: a clone of already-decoded state, safe to call
    /// from the render loop.
    fn remote_processes(&self) -> Option<Vec<RemoteProc>> {
        // `None` when there's no extension OR the producer didn't stream
        // processes (missing sub-key) — both mean "fall back to local".
        self.remote_ext
            .as_ref()
            .and_then(|ext| ext.processes.clone())
    }

    /// Decoded remote inference workloads from the `tt_toplike` extension on
    /// the latest applied frame. See [`Self::remote_processes`] for the
    /// `None` conditions and cost.
    fn remote_inference(&self) -> Option<Vec<RemoteInference>> {
        self.remote_ext
            .as_ref()
            .and_then(|ext| ext.inference.clone())
    }
}

/// Freshness word for `backend_info`, given the age of the last frame and the
/// expected inter-frame interval (both in seconds).
///
/// A frame is "stale" once it is older than 3× the expected interval, with a
/// 5-second floor so a fast configured interval doesn't flap the label on
/// ordinary network jitter. Pure and side-effect-free so it's unit-testable
/// without a live socket.
fn staleness_word(age_secs: u64, interval_secs: u64) -> &'static str {
    let threshold = interval_secs.saturating_mul(3).max(5);
    if age_secs > threshold {
        "stale"
    } else {
        "connected"
    }
}

impl Drop for WsBackend {
    fn drop(&mut self) {
        // Signal the reader/reconnect loop to stop, then let the runtime shut down.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.runtime.take() {
            // Don't block indefinitely on a task parked in await; background
            // shutdown drops the runtime without joining forever.
            rt.shutdown_timeout(Duration::from_millis(200));
        }
    }
}

/// Split a `HOST:PORT` (or bare `HOST`) spec into components.
///
/// Supports bracketed IPv6 (`[::1]:8765`). A bare host defaults to port 8000,
/// the tt-station-agentd control port.
fn split_host_port(spec: &str) -> BackendResult<(String, u16)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(BackendError::Initialization(
            "--remote requires a HOST:PORT (or HOST)".to_string(),
        ));
    }

    // Bracketed IPv6: [addr] or [addr]:port
    if let Some(rest) = spec.strip_prefix('[') {
        let (addr, tail) = rest.split_once(']').ok_or_else(|| {
            BackendError::Initialization(format!("malformed IPv6 remote address: {}", spec))
        })?;
        let port = match tail.strip_prefix(':') {
            Some(p) => parse_port(p)?,
            None if tail.is_empty() => 8000,
            None => {
                return Err(BackendError::Initialization(format!(
                    "malformed remote address: {}",
                    spec
                )))
            }
        };
        return Ok((addr.to_string(), port));
    }

    match spec.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Ok((host.to_string(), parse_port(port)?)),
        _ => Ok((spec.to_string(), 8000)),
    }
}

fn parse_port(p: &str) -> BackendResult<u16> {
    p.trim()
        .parse::<u16>()
        .map_err(|_| BackendError::Initialization(format!("invalid remote port: {}", p)))
}

/// Background reader: connect, stream frames into `shared`, reconnect on drop.
async fn reader_loop(
    url: String,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    reconnect: Duration,
    verbose: bool,
) {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
    use tokio_tungstenite::tungstenite::Message;

    // Cap the incoming message/frame size well below tungstenite's 64 MiB
    // default: a `tt-smi -s` frame is a few KB even for a Galaxy, and the peer
    // is unauthenticated (trusted-LAN only), so a smaller ceiling bounds a
    // hostile/oversized frame's transient allocation before it's ever parsed.
    let ws_config = WebSocketConfig {
        max_message_size: Some(4 << 20), // 4 MiB
        max_frame_size: Some(4 << 20),
        ..Default::default()
    };

    while !stop.load(Ordering::Relaxed) {
        match tokio_tungstenite::connect_async_with_config(&url, Some(ws_config), false).await {
            Ok((ws, _resp)) => {
                shared.connected.store(true, Ordering::Relaxed);
                if verbose {
                    log::debug!("WsBackend: connected to {}", url);
                }
                let (_write, mut read) = ws.split();
                while let Some(msg) = read.next().await {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match msg {
                        Ok(Message::Text(t)) => shared.set_frame(t.to_string()),
                        Ok(Message::Binary(b)) => {
                            if let Ok(s) = String::from_utf8(b.to_vec()) {
                                shared.set_frame(s);
                            }
                        }
                        Ok(Message::Close(_)) => break,
                        // Ping/Pong/Frame: nothing to do — tungstenite auto-replies to pings.
                        Ok(_) => {}
                        Err(e) => {
                            shared.set_error(format!("stream error: {}", e));
                            break;
                        }
                    }
                }
                shared.connected.store(false, Ordering::Relaxed);
            }
            Err(e) => {
                shared.connected.store(false, Ordering::Relaxed);
                shared.set_error(format!("connect error: {}", e));
                if verbose {
                    log::debug!("WsBackend: connect to {} failed: {}", url, e);
                }
            }
        }

        // Reconnect on drop (unless we're shutting down), sleeping in small
        // slices so Drop's stop flag is honoured promptly.
        let wake = Instant::now() + reconnect;
        while Instant::now() < wake {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic tt-smi -s frame (single Blackhole device) — the exact JSON a
    // tt-station-agentd publisher would push verbatim.
    const FRAME: &str = r#"{
        "device_info": [{
            "board_info": { "board_type": "p150a", "bus_id": "0000:01:00.0", "coords": "(0,0)" },
            "telemetry": {
                "voltage": "0.85", "current": "30.0", "power": "145.0",
                "aiclk": "800", "asic_temperature": "52.0", "heartbeat": "1234"
            },
            "smbus_telem": {
                "DDR_STATUS": "0x55555555", "DDR_SPEED": "0x3e80",
                "TIMER_HEARTBEAT": "0x10e7a", "AICLK": "0x320",
                "ENABLED_TENSIX_COL": "0x3FFF", "ETH_LIVE_STATUS": "0x00000000000000FF"
            }
        }]
    }"#;

    /// The pure age→wording helper: "stale" only past 3× interval, 5s floor.
    #[test]
    fn test_staleness_word() {
        // With a tiny (or zero) interval, the 5s floor governs.
        assert_eq!(staleness_word(0, 0), "connected");
        assert_eq!(staleness_word(5, 0), "connected"); // exactly at floor, not past it
        assert_eq!(staleness_word(6, 0), "stale"); // past the 5s floor

        // With a larger interval, threshold is 3× interval (10s → 30s).
        assert_eq!(staleness_word(29, 10), "connected");
        assert_eq!(staleness_word(30, 10), "connected"); // exactly 3× is not yet stale
        assert_eq!(staleness_word(31, 10), "stale");

        // Saturating multiply: an absurd interval must not overflow/panic.
        assert_eq!(staleness_word(u64::MAX, u64::MAX), "connected");
    }

    #[test]
    fn test_host_port_parsing() {
        let cfg = BackendConfig::default();
        let b = WsBackend::from_host_port("192.168.1.42:8765", cfg.clone()).unwrap();
        assert_eq!(b.host, "192.168.1.42");
        assert_eq!(b.port, 8765);
        assert_eq!(b.url, "ws://192.168.1.42:8765/telemetry");

        // Bare host defaults to the agentd control port.
        let b = WsBackend::from_host_port("qb2-lab", cfg.clone()).unwrap();
        assert_eq!(b.host, "qb2-lab");
        assert_eq!(b.port, 8000);

        // Bracketed IPv6.
        let b = WsBackend::from_host_port("[::1]:9743", cfg.clone()).unwrap();
        assert_eq!(b.host, "::1");
        assert_eq!(b.port, 9743);

        assert!(WsBackend::from_host_port("host:notaport", cfg.clone()).is_err());
        assert!(WsBackend::from_host_port("", cfg).is_err());
    }

    /// After a frame is received, `update()` must reflect the parsed snapshot in
    /// `devices()` / `telemetry()` / `smbus_telemetry()` — same triad as local.
    #[test]
    fn test_frame_applied_on_update() {
        let mut b = WsBackend::new_for_test();

        // Not-yet-received state: no data, no panic.
        assert!(b.update().is_ok());
        assert_eq!(b.devices().len(), 0);
        assert!(b.telemetry(0).is_none());

        // A frame arrives from the (simulated) reader task.
        b.test_inject(FRAME);
        assert!(b.update().is_ok());

        assert_eq!(b.devices().len(), 1);
        assert_eq!(b.devices()[0].board_type, "p150a");
        assert_eq!(
            b.devices()[0].architecture,
            crate::models::Architecture::Blackhole
        );

        let telem = b.telemetry(0).expect("telemetry missing after frame");
        assert_eq!(telem.power, Some(145.0));
        assert_eq!(telem.asic_temperature, Some(52.0));
        assert_eq!(telem.aiclk, Some(800));

        let smbus = b.smbus_telemetry(0).expect("smbus missing after frame");
        assert_eq!(smbus.enabled_tensix_col, Some(0x3FFF));
    }

    /// A second `update()` with no new frame is a cheap no-op (generation guard).
    #[test]
    fn test_update_no_new_frame_is_noop() {
        let mut b = WsBackend::new_for_test();
        b.test_inject(FRAME);
        assert!(b.update().is_ok());
        let gen_after_first = b.last_gen;
        // No new frame injected.
        assert!(b.update().is_ok());
        assert_eq!(b.last_gen, gen_after_first, "generation must not advance");
        assert_eq!(b.devices().len(), 1);
    }

    /// `snapshot_json()` relays the last received frame verbatim, independent of
    /// whether `update()`/`apply_latest()` has run.
    #[test]
    fn test_snapshot_json_relays_last_frame() {
        let b = WsBackend::new_for_test();
        assert_eq!(
            b.snapshot_json(),
            None,
            "no frame received yet — snapshot_json must be None"
        );

        b.test_inject(FRAME);
        assert_eq!(
            b.snapshot_json(),
            Some(FRAME.to_string()),
            "snapshot_json must relay the exact injected frame"
        );
    }

    /// A malformed frame is surfaced as an error, never a panic.
    #[test]
    fn test_bad_frame_surfaces_error() {
        let mut b = WsBackend::new_for_test();
        b.test_inject("this is not json");
        let res = b.update();
        assert!(res.is_err(), "bad frame should surface an error");
        // State stays empty; subsequent updates don't loop on the same bad frame.
        assert_eq!(b.devices().len(), 0);
        assert!(b.update().is_ok());
    }

    /// A frame carrying a `tt_toplike` extension decodes into
    /// `remote_processes()` / `remote_inference()`; a plain tt-smi frame (no
    /// extension) leaves both `None`.
    #[test]
    fn test_remote_extension_decoded_from_frame() {
        use crate::backend::remote_ext::{
            inject_extension, RemoteInference, RemoteProc, TtToplikeExt, TT_TOPLIKE_SCHEMA,
        };

        let ext = TtToplikeExt {
            schema: TT_TOPLIKE_SCHEMA,
            processes: Some(vec![RemoteProc {
                pid: 4242,
                name: "tt-inference-server".to_string(),
                cmd: "/usr/bin/tt-inference-server --port 8080".to_string(),
                uses_tt: true,
                cpu_pct: 87.5,
                mem_bytes: 12_884_901_888,
            }]),
            inference: Some(vec![RemoteInference {
                key: "vllm-llama3-70b".to_string(),
                label: "Llama-3 70B (vLLM)".to_string(),
                phase: "ready".to_string(),
                progress: None,
                serving: None,
            }]),
        };
        let frame_with_ext = inject_extension(FRAME, &ext);

        let mut b = WsBackend::new_for_test();

        // No frame applied yet: both accessors are None.
        assert_eq!(b.remote_processes(), None);
        assert_eq!(b.remote_inference(), None);

        // (a) A frame WITH the extension decodes into both accessors.
        b.test_inject(&frame_with_ext);
        assert!(b.update().is_ok());

        let processes = b.remote_processes().expect("processes should decode");
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].name, "tt-inference-server");

        let inference = b.remote_inference().expect("inference should decode");
        assert_eq!(inference.len(), 1);
        assert_eq!(inference[0].label, "Llama-3 70B (vLLM)");

        // (b) A subsequent plain tt-smi frame (no extension) clears both back
        // to None — the accessors reflect the *latest* applied frame only.
        b.test_inject(FRAME);
        assert!(b.update().is_ok());
        assert_eq!(b.remote_processes(), None);
        assert_eq!(b.remote_inference(), None);
    }

    /// End-to-end: an in-process tokio-tungstenite server pushes a frame; a real
    /// `WsBackend` connects, receives it, and reflects it after `update()`.
    #[test]
    fn test_end_to_end_against_fake_server() {
        use std::net::TcpListener as StdListener;

        // Reserve an ephemeral port synchronously so we know the URL up front.
        let listener = StdListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();

        // Server thread: accept one client and stream the frame a few times.
        let server = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                use futures_util::SinkExt;
                use tokio_tungstenite::tungstenite::Message;
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                // Accept the WsBackend connection (retry until it shows up).
                let (stream, _) = loop {
                    match listener.accept().await {
                        Ok(pair) => break pair,
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            continue;
                        }
                    }
                };
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                for _ in 0..5 {
                    if ws.send(Message::Text(FRAME.into())).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });
        });

        let cfg = BackendConfig {
            read_timeout_ms: 4000,
            ..BackendConfig::default()
        };
        let mut b = WsBackend::with_config(addr.ip().to_string(), addr.port(), cfg);
        b.init().expect("init should connect and get first frame");
        assert_eq!(b.devices().len(), 1);
        assert_eq!(b.devices()[0].board_type, "p150a");
        assert!(b.telemetry(0).is_some());

        drop(b); // triggers reader shutdown
        let _ = server.join();
    }
}
