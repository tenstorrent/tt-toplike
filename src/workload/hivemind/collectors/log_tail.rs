// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Log-stream collector: tails host log files and `docker logs -f` for
//! detected TT inference-server containers, surfacing each non-health-poll
//! line as a `SniffEvent`. Read-only — `docker logs` is a passive read of the
//! container's existing log stream, never a write or an exec of anything that
//! changes workload behavior.
//!
//! Reuses established helpers rather than re-implementing them:
//! - [`crate::workload::inference_server::logs::last_non_health_line`]'s
//!   health-poll filter idiom (skip `/tt-liveness`, `/health`, `/v1/models`),
//!   applied per-line in [`event_from_log`].
//! - [`crate::workload::inference_server::detect`]'s container recognizer
//!   (`is_tt_inference_image`) and `docker inspect` JSON parser (`parse_inspect`),
//!   to discover which running containers are TT inference servers.
//! - [`crate::workload::inference_server::probe::docker`]'s hardened,
//!   timeout-guarded one-shot `docker <args>` shell-out, for the (non-streaming)
//!   `docker ps`/`docker inspect` discovery calls.
//! - [`super::procfs::device_from_fd_target`] to recognize a container "pinned"
//!   to one specific device (`/dev/tenstorrent/N`, as opposed to the whole
//!   `/dev/tenstorrent` device class) from its inspected `HostConfig.Devices`.

use crate::workload::hivemind::classify::classify;
use crate::workload::hivemind::collector::{Collector, Tx};
use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent};
use crate::workload::inference_server::detect::{is_tt_inference_image, parse_inspect};
use crate::workload::inference_server::probe::docker;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Turn one raw log line into a `Log` `SniffEvent`, or `None` for lines that
/// carry no signal: blank lines, and health/liveness poll spam (the same
/// `/tt-liveness` / `/health` / `/v1/models` substrings
/// `inference_server::logs::last_non_health_line` filters — those endpoints
/// are hit every few seconds by the readiness prober and would otherwise
/// drown out real activity).
///
/// `origin` tags the collector/source (`"docker:<container>"`, a file path,
/// …); `proc_name` is best-effort context for [`classify`] (the owning
/// container's image name, or a host process's `comm`) — `None` when unknown.
/// The returned event's `device` is always `None`: a log line is host-
/// attributed by default. Callers that know better (e.g. a docker container
/// pinned to a specific device) overwrite `.device` on the returned event.
pub fn event_from_log(origin: &str, proc_name: Option<&str>, line: &str) -> Option<SniffEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let low = trimmed.to_lowercase();
    if low.contains("/tt-liveness") || low.contains("/health") || low.contains("/v1/models") {
        return None;
    }
    // Log-level tokens as emitted by the servers we tail (uvicorn/loguru-style
    // uppercase level prefixes) — a literal, case-sensitive check rather than
    // a lowercase substring match, so an incidental lowercase "error"/"warn"
    // inside a message body (e.g. a stack-trace mention) doesn't spuriously
    // escalate severity.
    let severity = if trimmed.contains("ERROR") {
        Severity::Error
    } else if trimmed.contains("WARN") {
        Severity::Warn
    } else {
        Severity::Info
    };
    Some(SniffEvent {
        ts: Instant::now(),
        source: classify(proc_name, trimmed),
        device: None,
        severity,
        kind: EventKind::Log,
        text: trimmed.to_string(),
        origin: origin.to_string(),
    })
}

/// A discovered TT inference-server container to tail.
struct DockerTarget {
    container: String,
    /// Best-effort process-name hint for `classify` — the container's image
    /// (falls back to the `docker ps` listing's image column if `docker
    /// inspect` didn't parse), e.g. an image containing "vllm" tags vLLM lines.
    proc_name: String,
    /// `Some(n)` when this container is pinned to one specific device node
    /// (`/dev/tenstorrent/n`); `None` when it maps the whole device class
    /// (`/dev/tenstorrent`, common for single-tenant boxes) or isn't a TT
    /// device mount at all.
    device: Option<u8>,
}

/// Extract a device-pinned index from a `docker inspect <container>` JSON
/// blob's `HostConfig.Devices[].PathOnHost` entries, reusing
/// [`super::procfs::device_from_fd_target`]'s `/dev/tenstorrent/N` parse
/// (the same path shape `--device /dev/tenstorrent/3:/dev/tenstorrent/3`
/// produces on the host side). A container mounting the whole class
/// (`/dev/tenstorrent`, no trailing index) or with no TT device mount at all
/// yields `None` — "pinned" specifically means "to one numbered device".
fn pinned_device_from_inspect(json: &str) -> Option<u8> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_array()?.first()?;
    let devices = obj.get("HostConfig")?.get("Devices")?.as_array()?;
    devices.iter().find_map(|d| {
        let path = d.get("PathOnHost")?.as_str()?;
        super::procfs::device_from_fd_target(path)
    })
}

/// Enumerate running TT inference-server containers via `docker ps` +
/// `docker inspect` (mirrors
/// `inference_server::probe::DockerProbe::list_servers`'s enumeration
/// shape), independent of any foreground `docker run` host process — this is
/// how detached (`-d`)/compose/systemd containers get found for tailing.
/// All docker I/O goes through the shared, timeout-guarded `docker()` helper,
/// so a wedged daemon can't hang this collector's discovery pass.
fn discover_tt_containers() -> Vec<DockerTarget> {
    let listing = docker(&["ps", "--no-trunc", "--format", "{{.Names}}\t{{.Image}}"]);
    let mut out = Vec::new();
    for line in listing.lines() {
        let Some((name, image)) = line.split_once('\t') else {
            continue;
        };
        if !is_tt_inference_image(image) {
            continue;
        }
        let name = name.trim();
        let inspect_json = docker(&["inspect", name]);
        let device = pinned_device_from_inspect(&inspect_json);
        let proc_name = parse_inspect(&inspect_json)
            .map(|s| s.image)
            .unwrap_or_else(|| image.to_string());
        out.push(DockerTarget {
            container: name.to_string(),
            proc_name,
            device,
        });
    }
    out
}

/// Tail one host log file from its live end: open, seek to EOF (history
/// before the collector started isn't replayed), then loop reading newly
/// appended lines. Mirrors `kmsg`'s poll-and-sleep idiom, but a plain
/// (blocking) file read never returns `WouldBlock` — `Ok(0)` on a regular
/// file just means "caught up to EOF for now", so we sleep and retry the
/// read rather than treating it as a terminal condition.
fn tail_file(origin: &str, path: &Path, device: Option<u8>, tx: Tx, shutdown: Arc<AtomicBool>) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return, // missing/unreadable: nothing to tail (engine surfaces via its own probe)
    };
    let mut reader = BufReader::new(file);
    if reader.get_mut().seek(SeekFrom::End(0)).is_err() {
        return;
    }
    let mut line = String::new();
    while !shutdown.load(Ordering::SeqCst) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => thread::sleep(Duration::from_millis(300)), // caught up: wait for more
            Ok(_) => {
                if let Some(mut ev) = event_from_log(origin, None, &line) {
                    ev.device = device;
                    if tx.try_send(ev).is_err() {
                        // channel full: drop newest (uncounted; only ring eviction bumps dropped())
                    }
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(300)),
        }
    }
}

/// Spawn `docker logs -f --tail 0 <container>` (stream new lines only — no
/// history replay, matching `tail_file`'s "live tail" behavior) and a reader
/// thread that turns each line into a `SniffEvent`. Returns the child (so the
/// caller can kill it on shutdown) and the reader thread's handle. `None` if
/// the container disappeared before we could spawn (docker returns an error).
///
/// Killing `child` on shutdown closes its stdout pipe from the writer side,
/// which unblocks the reader thread's blocking `read_line` with an EOF —
/// the same "kill first, then join" idiom `inference_server::probe::docker`
/// uses for its own timeout handling.
fn spawn_docker_logs_tail(
    target: &DockerTarget,
    tx: Tx,
    shutdown: Arc<AtomicBool>,
) -> Option<(Child, JoinHandle<()>)> {
    let mut child = Command::new("docker")
        .arg("logs")
        .arg("-f")
        .arg("--tail")
        .arg("0")
        .arg(&target.container)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let origin = format!("docker:{}", target.container);
    let proc_name = target.proc_name.clone();
    let device = target.device;
    let handle = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF: container's log stream ended (container stopped)
                Ok(_) => {
                    if let Some(mut ev) = event_from_log(&origin, Some(&proc_name), &line) {
                        ev.device = device;
                        if tx.try_send(ev).is_err() {
                            // channel full: drop newest (uncounted; only ring eviction bumps dropped())
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    Some((child, handle))
}

/// Tails a fixed set of host log files plus `docker logs -f` for every
/// detected TT inference-server container, discovered on a background poll
/// (so a container launched after the collector starts still gets picked up).
pub struct LogTailCollector {
    /// Static (non-docker) log files to tail: `(origin label, path, device
    /// hint)`. Empty by default — v1 auto-discovers only docker containers;
    /// host log-file targets are meant to be supplied by the engine (e.g. a
    /// `/watch <path>` command in a later task).
    static_targets: Vec<(String, PathBuf, Option<u8>)>,
}

impl LogTailCollector {
    pub fn new() -> Self {
        Self {
            static_targets: Vec::new(),
        }
    }

    /// Construct with an explicit set of host log files to tail alongside the
    /// auto-discovered docker containers.
    pub fn with_static_targets(targets: Vec<(String, PathBuf, Option<u8>)>) -> Self {
        Self {
            static_targets: targets,
        }
    }
}

impl Default for LogTailCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// How often the docker container set is re-scanned for newly-launched
/// inference servers. Matches `cache_watch`'s 1s-class polling cadence order
/// of magnitude, but a little coarser since `docker ps`/`inspect` are far
/// more expensive per-call than a directory listing.
const DOCKER_RESCAN_INTERVAL: Duration = Duration::from_secs(5);

impl Collector for LogTailCollector {
    fn name(&self) -> &'static str {
        "log_tail"
    }

    fn run(&mut self, tx: Tx, shutdown: Arc<AtomicBool>) {
        // One dedicated thread per static log file, tailing independently.
        let mut file_handles = Vec::new();
        for (origin, path, device) in self.static_targets.clone() {
            let tx = tx.clone();
            let sd = shutdown.clone();
            file_handles.push(thread::spawn(move || {
                tail_file(&origin, &path, device, tx, sd)
            }));
        }

        // Docker containers are discovered on a poll (rather than once at
        // startup) so a container launched after this collector starts is
        // still picked up. Already-tailing containers are never re-spawned —
        // deduped by name — and a container that stops just lets its reader
        // thread hit EOF naturally (see `spawn_docker_logs_tail`).
        let mut docker_handles: Vec<(Child, JoinHandle<()>)> = Vec::new();
        let mut tailing: HashSet<String> = HashSet::new();
        // Scan immediately on the first loop iteration rather than waiting
        // out the first interval.
        let mut last_scan = Instant::now()
            .checked_sub(DOCKER_RESCAN_INTERVAL)
            .unwrap_or_else(Instant::now);

        while !shutdown.load(Ordering::SeqCst) {
            if last_scan.elapsed() >= DOCKER_RESCAN_INTERVAL {
                for target in discover_tt_containers() {
                    if tailing.insert(target.container.clone()) {
                        if let Some(pair) =
                            spawn_docker_logs_tail(&target, tx.clone(), shutdown.clone())
                        {
                            docker_handles.push(pair);
                        }
                    }
                }
                last_scan = Instant::now();
            }
            thread::sleep(Duration::from_millis(200));
        }

        // Shutdown: kill every docker-logs child (unblocks its reader thread
        // via EOF) then join everything so `stop()` doesn't return early with
        // orphaned threads/processes still running.
        for (mut child, handle) in docker_handles {
            let _ = child.kill();
            let _ = child.wait();
            let _ = handle.join();
        }
        for h in file_handles {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::hivemind::event::{EventKind, Severity, Source};

    #[test]
    fn skips_health_poll_lines() {
        assert!(event_from_log(
            "docker:z",
            None,
            r#"1.2.3.4 - "GET /tt-liveness HTTP/1.1" 405"#
        )
        .is_none());
    }

    #[test]
    fn surfaces_real_error_line() {
        let ev = event_from_log("docker:z", Some("vllm"), "ERROR weight load failed: OOM").unwrap();
        assert_eq!(ev.source, Source::Vllm);
        assert_eq!(ev.severity, Severity::Error);
        assert_eq!(ev.kind, EventKind::Log);
    }

    #[test]
    fn skips_empty_lines() {
        assert!(event_from_log("host", None, "   ").is_none());
        assert!(event_from_log("host", None, "").is_none());
    }

    #[test]
    fn warn_and_info_severity_inferred() {
        let warn = event_from_log("host", None, "WARN queue depth rising").unwrap();
        assert_eq!(warn.severity, Severity::Warn);
        let info = event_from_log("host", None, "compiling ncrisc.o").unwrap();
        assert_eq!(info.severity, Severity::Info);
    }

    #[test]
    fn device_defaults_to_none_host_attributed() {
        let ev = event_from_log("host", None, "starting up").unwrap();
        assert_eq!(ev.device, None);
    }

    const INSPECT_PINNED: &str =
        r#"[{"HostConfig":{"Devices":[{"PathOnHost":"/dev/tenstorrent/2"}]}}]"#;
    const INSPECT_WHOLE_CLASS: &str =
        r#"[{"HostConfig":{"Devices":[{"PathOnHost":"/dev/tenstorrent"}]}}]"#;

    #[test]
    fn pinned_device_parses_specific_index_not_whole_class() {
        assert_eq!(pinned_device_from_inspect(INSPECT_PINNED), Some(2));
        assert_eq!(pinned_device_from_inspect(INSPECT_WHOLE_CLASS), None);
        assert_eq!(pinned_device_from_inspect("not json"), None);
        assert_eq!(pinned_device_from_inspect("[]"), None);
    }
}
