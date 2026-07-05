// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! `TelemetryPublisher` — the server half of "remote QuietBox".
//!
//! Serves a WebSocket endpoint at `/telemetry` that fans the newest telemetry
//! frame out to every connected client. This is the counterpart to
//! [`crate::backend::ws::WsBackend`]: a `--serve`-mode process broadcasts here,
//! and any number of `WsBackend` consumers on the LAN connect and stream it.
//!
//! ## Architecture
//!
//! ```text
//! collector (any backend) --broadcast(frame)--> TelemetryPublisher --(ws fan-out)--> N clients
//! ```
//!
//! The publisher itself never parses or inspects the frame — it is treated as
//! an opaque `String` (in practice, verbatim `tt-smi -s` JSON, matching
//! [`crate::backend::json::parse_tt_smi_snapshot`]'s expectations on the
//! consumer side, but `serve.rs` doesn't need to know that).
//!
//! A `tokio::sync::watch` channel is the fan-out primitive: `broadcast()`
//! writes the latest frame with `send_replace` (unconditional — this must
//! succeed even before any client has connected, so a client that connects
//! later still finds a frame waiting), and each per-client task holds its own
//! `Receiver`. A slow or dead client only stalls its own task; it never
//! back-pressures the collector or other clients.
//!
//! ## Security
//!
//! Same trust model as `WsBackend`: plaintext `ws://`, no auth. Trusted-LAN
//! only. The only validation performed is that the HTTP upgrade request path
//! is exactly `/telemetry`; anything else is rejected at the handshake.
//!
//! ## Panics
//!
//! No unwrap/expect on network operations or locks outside of `#[cfg(test)]`
//! code. [`TelemetryPublisher::spawn`] binds synchronously so a port-in-use
//! error surfaces to the caller instead of panicking on a background thread;
//! everything after that (accept loop, per-client tasks) is best-effort and
//! logs rather than panics.

use crate::error::{BackendError, BackendResult};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// WebSocket telemetry publisher: serves `/telemetry`, fanning the latest
/// broadcast frame out to every connected client.
///
/// Owns a dedicated single-worker Tokio runtime (mirrors [`crate::backend::ws::WsBackend`]'s
/// containment shape) so it never touches the render thread or any caller's
/// own runtime.
pub struct TelemetryPublisher {
    /// Dedicated runtime driving the accept loop and per-client tasks.
    /// `Some` from construction until `Drop` takes it for a bounded shutdown.
    runtime: Option<tokio::runtime::Runtime>,
    /// Fan-out channel: `broadcast()` writes here, each client task subscribes.
    tx: watch::Sender<String>,
    /// Count of currently-connected clients, maintained by the per-client tasks.
    client_count: Arc<AtomicUsize>,
    /// The resolved local address actually bound (useful when `spawn` was
    /// given port `0` and the OS picked one).
    bind_addr: SocketAddr,
    /// Signals the accept loop (and transitively every per-client task's next
    /// poll) to stop. Set by `Drop`.
    stop: Arc<AtomicBool>,
}

impl TelemetryPublisher {
    /// Bind `bind` synchronously and start serving `/telemetry` on a
    /// dedicated background runtime.
    ///
    /// Binding happens on the calling thread *before* any runtime or
    /// background task exists, so a port-in-use (or other bind) failure is
    /// returned here as an `Err` rather than surfacing asynchronously (or
    /// panicking) on a background thread.
    pub fn spawn(bind: SocketAddr) -> BackendResult<Self> {
        let std_listener = std::net::TcpListener::bind(bind)
            .map_err(|e| BackendError::Initialization(format!("failed to bind {}: {}", bind, e)))?;
        let bind_addr = std_listener.local_addr().map_err(|e| {
            BackendError::Initialization(format!("failed to read local address after bind: {}", e))
        })?;
        std_listener.set_nonblocking(true).map_err(|e| {
            BackendError::Initialization(format!("failed to set listener non-blocking: {}", e))
        })?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("telemetry-publisher")
            .build()
            .map_err(|e| {
                BackendError::Initialization(format!(
                    "failed to build telemetry publisher runtime: {}",
                    e
                ))
            })?;

        let (tx, _initial_rx) = watch::channel(String::new());
        let client_count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let tx_task = tx.clone();
        let client_count_task = Arc::clone(&client_count);
        let stop_task = Arc::clone(&stop);

        // Hand the std listener to the runtime and start accepting. Adopting
        // it must happen inside the runtime context (`from_std` requires an
        // active Tokio reactor).
        runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(std_listener) {
                Ok(listener) => {
                    accept_loop(listener, tx_task, client_count_task, stop_task).await;
                }
                Err(e) => {
                    log::error!("TelemetryPublisher: failed to adopt listener: {}", e);
                }
            }
        });

        Ok(Self {
            runtime: Some(runtime),
            tx,
            client_count,
            bind_addr,
            stop,
        })
    }

    /// Publish a new frame to every connected (and future) client.
    ///
    /// Uses `send_replace` rather than `send`: `watch::Sender::send` fails
    /// (and leaves the stored value untouched) when there are currently zero
    /// receivers, which would silently drop a frame broadcast before the
    /// first client connects. `send_replace` always stores the value, so a
    /// client connecting later still gets it immediately (see
    /// `handle_client`'s "current value" send-on-connect).
    pub fn broadcast(&self, frame: String) {
        let _ = self.tx.send_replace(frame);
    }

    /// Number of currently-connected `/telemetry` clients.
    pub fn client_count(&self) -> usize {
        self.client_count.load(Ordering::Relaxed)
    }

    /// The address actually bound. Differs from the address passed to
    /// `spawn` when that address's port was `0` (OS-assigned).
    pub fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }
}

impl Drop for TelemetryPublisher {
    fn drop(&mut self) {
        // Signal the accept loop + every per-client task to stop, then let
        // the runtime shut down. Bounded, mirroring `WsBackend`'s Drop: don't
        // block indefinitely on a task parked in await.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_millis(200));
        }
    }
}

/// Accept loop: bind is already done by the caller; this just accepts
/// connections and hands each one to a per-client task.
///
/// Polls `accept()` under a short timeout rather than awaiting it directly so
/// the `stop` flag is checked promptly even with no incoming connections —
/// mirrors the slice-sleep pattern `ws.rs`'s `reader_loop` uses for the same
/// reason (a task parked in a bare `.await` can't otherwise be interrupted).
async fn accept_loop(
    listener: tokio::net::TcpListener,
    tx: watch::Sender<String>,
    client_count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        let (stream, _peer_addr) = match accepted {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                log::debug!("TelemetryPublisher: accept error: {}", e);
                continue;
            }
            Err(_timeout) => continue,
        };

        let rx = tx.subscribe();
        let client_count = Arc::clone(&client_count);
        tokio::spawn(async move {
            match tokio_tungstenite::accept_hdr_async(stream, check_telemetry_path).await {
                Ok(ws) => handle_client(ws, rx, client_count).await,
                Err(e) => {
                    // Handshake failed or path rejected. Never counted as a
                    // client, so nothing to decrement.
                    log::debug!("TelemetryPublisher: handshake rejected: {}", e);
                }
            }
        });
    }
}

/// Handshake callback: accept only `/telemetry`, reject everything else with
/// a 404 before the WebSocket upgrade completes.
///
/// The `Err` type (`ErrorResponse`) is dictated by tungstenite's `Callback`
/// trait signature, which this function must match exactly to satisfy the
/// blanket `impl<F> Callback for F` bound used by `accept_hdr_async` — it
/// can't be boxed away, hence the explicit lint allow.
#[allow(clippy::result_large_err)]
fn check_telemetry_path(
    request: &tokio_tungstenite::tungstenite::handshake::server::Request,
    response: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    if request.uri().path() == "/telemetry" {
        Ok(response)
    } else {
        // `Response::new` is infallible (unlike a `Builder`), so this never
        // needs an unwrap/expect to construct the rejection.
        let mut rejection =
            tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(Some(format!(
                "tt-toplike telemetry publisher only serves /telemetry (got {})",
                request.uri().path()
            )));
        *rejection.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
        Err(rejection)
    }
}

/// Per-client task: stream the latest frame to one connected WebSocket client
/// until it disconnects or a write fails, then decrement the client count.
///
/// A slow or dead client only blocks this task (and the connection is
/// eventually severed by a write error) — it never touches the watch channel
/// in a way that could stall the publisher, other clients, or the collector.
async fn handle_client(
    mut ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    mut rx: watch::Receiver<String>,
    client_count: Arc<AtomicUsize>,
) {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    client_count.fetch_add(1, Ordering::Relaxed);

    // `watch::Sender::subscribe()` marks whatever value is current at
    // subscribe time as already "seen" by the new receiver — its first
    // `changed()` only resolves on the *next* send after that. Without this
    // explicit catch-up send, a client connecting after a frame has already
    // been broadcast (the common case: the collector broadcasts continuously,
    // clients connect whenever) would see nothing until the next tick. An
    // empty string means "no frame yet" (the channel's initial value) and is
    // deliberately not sent.
    let current = rx.borrow_and_update().clone();
    if !current.is_empty() && ws.send(Message::Text(current)).await.is_err() {
        client_count.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    loop {
        if rx.changed().await.is_err() {
            break; // Publisher dropped its Sender (shutting down).
        }
        let frame = rx.borrow_and_update().clone();
        if frame.is_empty() {
            continue;
        }
        if ws.send(Message::Text(frame)).await.is_err() {
            break; // Client gone; sever this task only.
        }
    }

    client_count.fetch_sub(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ws::WsBackend;
    use crate::backend::{BackendConfig, TelemetryBackend};
    use std::time::Instant;

    // Same minimal single-Blackhole-device tt-smi -s frame used by
    // `ws.rs`'s tests — a realistic verbatim frame a real publisher would
    // broadcast.
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

    /// End-to-end loopback: a real `TelemetryPublisher` serves `/telemetry`;
    /// a real `WsBackend` connects to it, and a broadcast frame round-trips
    /// through the actual WebSocket stack into `device_count()`.
    #[test]
    fn test_loopback_round_trip() {
        let publisher = TelemetryPublisher::spawn("127.0.0.1:0".parse().unwrap())
            .expect("spawn on port 0 (OS-assigned) must succeed");

        // Seed the channel before the client connects — this exercises the
        // send_replace/send-on-connect path (a frame broadcast before any
        // client exists must still reach a client that connects afterward).
        publisher.broadcast(FRAME.to_string());

        let cfg = BackendConfig {
            read_timeout_ms: 4000,
            ..BackendConfig::default()
        };
        let mut backend = WsBackend::from_host_port(&publisher.bind_addr().to_string(), cfg)
            .expect("HOST:PORT parse of the publisher's own bind_addr must succeed");
        backend
            .init()
            .expect("init should connect and receive the already-seeded frame");

        // Keep broadcasting + updating for a bit (bounded ~5s) to also
        // exercise the steady-state fan-out path, not just the initial catch-up.
        let deadline = Instant::now() + Duration::from_secs(5);
        while backend.device_count() == 0 && Instant::now() < deadline {
            publisher.broadcast(FRAME.to_string());
            let _ = backend.update();
            std::thread::sleep(Duration::from_millis(50));
        }

        assert_eq!(backend.device_count(), 1, "frame should have applied");
        assert_eq!(backend.devices()[0].board_type, "p150a");
        assert!(backend.telemetry(0).is_some());
        assert_eq!(
            publisher.client_count(),
            1,
            "exactly one WsBackend client should be connected"
        );

        drop(backend);
        drop(publisher);
    }

    /// Binding an address that's already in use must return `Err`, never panic.
    #[test]
    fn test_bind_in_use_returns_err() {
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = blocker.local_addr().unwrap();

        let result = TelemetryPublisher::spawn(addr);
        assert!(
            result.is_err(),
            "binding an already-bound address must surface Err, not panic"
        );

        drop(blocker);
    }
}
