// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Authoritative liveness probes for inference *server* runtimes.
//!
//! The cheap snapshot tier in [`crate::workload::host_processes`] answers "is
//! this runtime working?" from CPU + a known subprocess shape. That's enough for
//! ollama (a loaded model spawns an `ollama runner` worker), but a server like
//! vllm can hold a model loaded and sit at ~0% CPU *between requests* — it looks
//! idle to the CPU heuristic but is really "loaded and ready". When a runtime
//! exposes an endpoint we know how to read, we ask it directly.
//!
//! Design (see `docs/superpowers/specs/2026-06-30-inference-liveness-probes-design.md`):
//! - **Confirm-only**: we only probe a runtime we already detected as a running
//!   process, on its own port (parsed from the cmdline, else the conventional
//!   default). We never scan ports blindly.
//! - **Off the hot path**: a background `std::thread` does the blocking socket
//!   work on a slow cadence with a short timeout; the render loop reads cached
//!   verdicts lock-free via `arc-swap`. (The TUI run loop is synchronous — there
//!   is no tokio runtime to spawn onto — so a plain thread + `std::net` keeps
//!   this truly dependency-free and simpler than standing up a runtime.)
//! - A timed-out or errored probe yields *no* verdict, so the caller falls back
//!   to the cheap tier rather than flapping a server to "idle".

use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the background thread re-probes, at most.
const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// Per-probe hard timeout (connect + read). Localhost should answer in single-digit ms.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
/// A cached verdict older than this is ignored, so a crashed server doesn't read
/// as "ready" forever. Two cadences of slack absorbs a single missed cycle.
const MAX_AGE: Duration = Duration::from_secs(12);

/// How to confirm a given runtime label is loaded/ready.
struct ProbeSpec {
    /// Conventional port when the cmdline doesn't specify one.
    default_port: u16,
    /// HTTP path to GET.
    path: &'static str,
    /// Verdict from (HTTP status, body) → is the runtime actively loaded/ready?
    judge: fn(u16, &[u8]) -> bool,
}

/// Registry of runtimes we know how to ask. Returns `None` for labels with no
/// authoritative endpoint (those stay on the cheap snapshot tier).
///
/// The `label` strings match [`crate::workload::inference_match`] output.
fn probe_spec(label: &str) -> Option<ProbeSpec> {
    match label {
        // `/api/ps` lists *currently loaded* models; empty ⇒ no model resident.
        "ollama" => Some(ProbeSpec {
            default_port: 11434,
            path: "/api/ps",
            judge: judge_models_nonempty, // {"models":[...]}
        }),
        // OpenAI-compatible: `/v1/models` `.data[]` is the served model list.
        "vllm" => Some(ProbeSpec {
            default_port: 8000,
            path: "/v1/models",
            judge: judge_data_nonempty, // {"object":"list","data":[...]}
        }),
        _ => None,
    }
}

/// A resolved thing to probe: a detected runtime and the port to reach it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    pub label: String,
    pub port: u16,
}

/// Cached outcome of one probe.
#[derive(Debug, Clone, Copy)]
struct ProbeVerdict {
    active: bool,
    observed_at: Instant,
}

/// Resolve the port for a detected runtime: prefer one parsed from its cmdline,
/// else the runtime's conventional default. Returns `None` if the label has no
/// probe spec (caller should skip it).
pub fn resolve_target(label: &str, cmdline: &str) -> Option<ProbeTarget> {
    let spec = probe_spec(label)?;
    let port = parse_port_from_cmdline(cmdline).unwrap_or(spec.default_port);
    Some(ProbeTarget {
        label: label.to_string(),
        port,
    })
}

/// Pull a port out of a server cmdline: `--port 8000`, `--port=8000`, or a
/// trailing `:8000` on a `--host`/`--url` value. First match wins. Pure.
pub fn parse_port_from_cmdline(cmdline: &str) -> Option<u16> {
    let toks: Vec<&str> = cmdline.split_whitespace().collect();
    for (i, tok) in toks.iter().enumerate() {
        // --port=8000 / --port 8000
        if let Some(rest) = tok.strip_prefix("--port") {
            if let Some(eq) = rest.strip_prefix('=') {
                if let Ok(p) = eq.parse::<u16>() {
                    return Some(p);
                }
            } else if rest.is_empty() {
                if let Some(next) = toks.get(i + 1) {
                    if let Ok(p) = next.parse::<u16>() {
                        return Some(p);
                    }
                }
            }
        }
        // host:port form, e.g. 127.0.0.1:8000
        if let Some((_, p)) = tok.rsplit_once(':') {
            if let Ok(p) = p.parse::<u16>() {
                return Some(p);
            }
        }
    }
    None
}

// ── HTTP (minimal, blocking, localhost) ────────────────────────────────────

/// Parse the status code from an HTTP response's first line (`HTTP/1.1 200 OK`).
fn http_status(resp: &[u8]) -> Option<u16> {
    let line_end = resp.windows(2).position(|w| w == b"\r\n")?;
    let line = std::str::from_utf8(&resp[..line_end]).ok()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Return the body slice after the `\r\n\r\n` header/body separator (empty if none).
fn http_body(resp: &[u8]) -> &[u8] {
    match resp.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(pos) => &resp[pos + 4..],
        None => &[],
    }
}

/// Is a top-level JSON array at `key` present and non-empty? Tolerant: any parse
/// failure ⇒ `false` (we only assert activity on a clear positive).
fn json_array_nonempty(body: &[u8], key: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get(key).and_then(|a| a.as_array()).map(|a| !a.is_empty()))
        .unwrap_or(false)
}

fn judge_models_nonempty(status: u16, body: &[u8]) -> bool {
    status == 200 && json_array_nonempty(body, "models")
}

fn judge_data_nonempty(status: u16, body: &[u8]) -> bool {
    status == 200 && json_array_nonempty(body, "data")
}

/// Blocking HTTP/1.0 GET to localhost with connect + read timeouts. `Connection:
/// close` lets us `read_to_end` without chunked/keep-alive handling.
fn http_get_localhost(port: u16, path: &str, timeout: Duration) -> std::io::Result<Vec<u8>> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\nAccept: */*\r\n\r\n"
    );
    stream.write_all(req.as_bytes())?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Probe one target. `Some(active)` on a clean response, `None` on any error /
/// unknown label so the caller falls back to the cheap tier.
fn probe_once(target: &ProbeTarget) -> Option<bool> {
    let spec = probe_spec(&target.label)?;
    match http_get_localhost(target.port, spec.path, PROBE_TIMEOUT) {
        Ok(resp) => Some((spec.judge)(
            http_status(&resp).unwrap_or(0),
            http_body(&resp),
        )),
        Err(_) => None,
    }
}

/// Is a verdict observed `age` ago still fresh enough to trust? Pure.
fn verdict_is_fresh(age: Duration) -> bool {
    age <= MAX_AGE
}

// ── Background prober ───────────────────────────────────────────────────────

/// Owns the background probe thread and a lock-free cache of verdicts.
///
/// Submit the currently-detected targets each refresh; read fresh verdicts when
/// building rows. Dropping the prober disconnects the channel and the thread
/// exits on its own.
pub struct LivenessProber {
    cache: Arc<ArcSwap<HashMap<String, ProbeVerdict>>>,
    tx: Sender<Vec<ProbeTarget>>,
}

impl LivenessProber {
    /// Spawn the background thread. The thread coalesces queued target updates to
    /// the latest, probes at most once per `PROBE_INTERVAL`, and stores results.
    pub fn spawn() -> Self {
        let cache = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        let (tx, rx) = mpsc::channel::<Vec<ProbeTarget>>();
        let cache_bg = Arc::clone(&cache);

        // Detached worker: exits when `tx` (and thus all senders) drops.
        let _ = std::thread::Builder::new()
            .name("tt-liveness-probe".into())
            .spawn(move || {
                let mut targets: Vec<ProbeTarget> = Vec::new();
                let mut last_probe: Option<Instant> = None;
                loop {
                    match rx.recv_timeout(PROBE_INTERVAL) {
                        Ok(mut t) => {
                            // Drain any backlog; only the most recent set matters.
                            while let Ok(newer) = rx.try_recv() {
                                t = newer;
                            }
                            targets = t;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }

                    let due = last_probe.is_none_or(|l| l.elapsed() >= PROBE_INTERVAL);
                    if targets.is_empty() || !due {
                        continue;
                    }

                    let mut map = HashMap::new();
                    for t in &targets {
                        if let Some(active) = probe_once(t) {
                            map.insert(
                                t.label.clone(),
                                ProbeVerdict {
                                    active,
                                    observed_at: Instant::now(),
                                },
                            );
                        }
                    }
                    cache_bg.store(Arc::new(map));
                    last_probe = Some(Instant::now());
                }
            });

        LivenessProber { cache, tx }
    }

    /// Hand the worker the runtimes detected this cycle. Non-blocking; a dead
    /// worker just means the send is dropped.
    pub fn submit(&self, targets: Vec<ProbeTarget>) {
        let _ = self.tx.send(targets);
    }

    /// Snapshot of `label → active` for verdicts still within `MAX_AGE`. Stale or
    /// missing labels are absent, so callers fall back to the cheap tier.
    pub fn fresh_verdicts(&self) -> HashMap<String, bool> {
        let now = Instant::now();
        self.cache
            .load()
            .iter()
            .filter(|(_, v)| verdict_is_fresh(now.duration_since(v.observed_at)))
            .map(|(k, v)| (k.clone(), v.active))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_from_various_cmdline_forms() {
        assert_eq!(
            parse_port_from_cmdline("python -m vllm.entrypoints.openai.api_server --port 8001"),
            Some(8001)
        );
        assert_eq!(
            parse_port_from_cmdline("vllm serve model --port=9000"),
            Some(9000)
        );
        assert_eq!(
            parse_port_from_cmdline("server --host 127.0.0.1:7070"),
            Some(7070)
        );
        assert_eq!(parse_port_from_cmdline("ollama serve"), None);
    }

    #[test]
    fn resolve_target_uses_cmdline_then_default() {
        assert_eq!(
            resolve_target("vllm", "vllm serve m --port 8123"),
            Some(ProbeTarget {
                label: "vllm".into(),
                port: 8123
            })
        );
        assert_eq!(
            resolve_target("ollama", "/usr/local/bin/ollama serve"),
            Some(ProbeTarget {
                label: "ollama".into(),
                port: 11434 // conventional default
            })
        );
        // No probe spec ⇒ not a probe target.
        assert_eq!(resolve_target("mlx", "mlx_lm.server"), None);
    }

    #[test]
    fn http_status_and_body_split() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"models\":[]}";
        assert_eq!(http_status(resp), Some(200));
        assert_eq!(http_body(resp), b"{\"models\":[]}");
        assert_eq!(http_status(b"garbage"), None);
        assert_eq!(http_body(b"no headers"), b"");
    }

    #[test]
    fn ollama_judge_reflects_loaded_models() {
        // /api/ps with a model loaded ⇒ active.
        assert!(judge_models_nonempty(
            200,
            br#"{"models":[{"name":"llama3"}]}"#
        ));
        // empty list ⇒ no model resident ⇒ idle.
        assert!(!judge_models_nonempty(200, br#"{"models":[]}"#));
        // non-200 ⇒ not active.
        assert!(!judge_models_nonempty(500, br#"{"models":[{"name":"x"}]}"#));
        // malformed ⇒ not active (no false positive).
        assert!(!judge_models_nonempty(200, b"not json"));
    }

    #[test]
    fn vllm_judge_reflects_served_models() {
        assert!(judge_data_nonempty(
            200,
            br#"{"object":"list","data":[{"id":"m"}]}"#
        ));
        assert!(!judge_data_nonempty(200, br#"{"object":"list","data":[]}"#));
        assert!(!judge_data_nonempty(200, b"{}"));
    }

    #[test]
    fn unknown_label_has_no_probe_spec() {
        assert!(probe_spec("llama.cpp").is_none());
        assert!(probe_spec("triton").is_none());
        assert!(probe_spec("ollama").is_some());
        assert!(probe_spec("vllm").is_some());
    }

    #[test]
    fn freshness_window() {
        assert!(verdict_is_fresh(Duration::from_secs(0)));
        assert!(verdict_is_fresh(MAX_AGE));
        assert!(!verdict_is_fresh(MAX_AGE + Duration::from_millis(1)));
    }

    #[test]
    fn prober_starts_empty_and_is_lock_free_to_read() {
        let p = LivenessProber::spawn();
        // No probes have run, so no verdicts yet — callers fall back to cheap tier.
        assert!(p.fresh_verdicts().is_empty());
        // Submitting targets must not block or panic.
        p.submit(vec![ProbeTarget {
            label: "vllm".into(),
            port: 65000, // nothing listening; probe will error → no verdict
        }]);
    }
}
