// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The per-tick reducer (`fold_tick`) and the background monitor that drives
//! it from real container probes.
//!
//! `fold_tick` is pure: it turns one tick's [`TickSample`] plus the previous
//! [`ServiceState`] into the next `ServiceState`, with no I/O of its own —
//! that's what makes it exhaustively unit-testable against a synthetic sample
//! (see the tests below), independent of docker or the network.
//!
//! [`InferenceServerMonitor`] mirrors [`crate::workload::liveness_probe::LivenessProber`]'s
//! shape: a `std::sync::mpsc` channel hands the worker the currently-detected
//! servers, a detached `std::thread` paces itself with `recv_timeout` (backlog
//! drained to the latest submission) and does all the blocking docker/HTTP
//! probing, and the render loop reads a lock-free `arc-swap` snapshot. There
//! is no tokio runtime in this TUI, so a plain thread + blocking calls is the
//! simplest correct shape.

use arc_swap::ArcSwap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::workload::inference_server::detect::{service_key, InferenceServer, Source};
use crate::workload::inference_server::logs::last_non_health_line;
use crate::workload::inference_server::probe::{
    contains_python, count_lines, parse_docker_stats, parse_env_var, parse_liveness, top_process,
    ContainerProbe, DockerProbe, Readiness, TickSample,
};
use crate::workload::inference_server::services::{model_basename, service_for};
use crate::workload::inference_server::state::{
    estimate_progress, is_alarm, ModelProfile, Phase, ServiceState,
};

/// How often the background thread re-probes the detected services, at most.
/// (`submit` may be called far more often than this — e.g. once per render
/// tick — so the worker gates on `last_tick`'s age rather than probing on
/// every message, exactly like `LivenessProber`'s `PROBE_INTERVAL` gate.)
const TICK_INTERVAL: Duration = Duration::from_secs(5);
/// `TICK_INTERVAL` in whole seconds, passed to `fold_tick`'s alarm timer.
pub const CADENCE_SECS: u32 = TICK_INTERVAL.as_secs() as u32;

/// Sentinel absolute kernel count meaning "no prior tick observed yet". A
/// service just discovered may already have `.dephash` artifacts on disk from
/// a previous run, so treating its very first observed count as `0 -> N` (a
/// spurious "N kernels compiled this session") would misreport `Compiling`.
/// `i64::MAX` (rather than `usize::MAX`) is deliberate: casting back to `i64`
/// in `fold_tick`'s delta math must stay positive, and `usize::MAX as i64`
/// wraps around to `-1` (two's complement reinterpretation), which would
/// defeat the sentinel by making the "unobserved" delta come out positive.
const NEVER_OBSERVED_KERNEL_COUNT: usize = i64::MAX as usize;

/// The shell command run inside the container to count compiled-kernel
/// artifacts (one `.dephash` per JIT-compiled kernel program). Both roots are
/// searched because the location moved across tt-inference-server releases:
/// older images cached under `$TT_METAL_HOME/built`, while 0.14.0+ uses the
/// per-user JIT cache at `$HOME/.cache/tt-metal-cache` (the old `built/` is
/// empty there, which silently zeroed this count before). `$TT_METAL_HOME`/
/// `$HOME` are expanded by the container's own shell (via `sh -c`), not here —
/// the host-side `env` dump only decides *whether* to run this (see `build_sample`).
const KERNEL_FIND_CMD: &str =
    "find \"$TT_METAL_HOME/built\" \"$HOME/.cache/tt-metal-cache\" -name '*.dephash' 2>/dev/null";

/// The shell command that counts *loaded* weight shards — the device-format
/// tensor binaries (`.tensorbin`) the runtime writes under `$CACHE_ROOT` as it
/// converts and loads a model's weights. Distinct from compiled kernels: this
/// climbs during the weight-load phase, after (and alongside) the compile
/// phase. Gated on `$CACHE_ROOT` being set (see `build_sample`).
const LOADED_FIND_CMD: &str = "find \"$CACHE_ROOT\" -name '*.tensorbin' 2>/dev/null";
/// `ps` invocation whose first data row is the container's top CPU consumer.
const TOP_PROC_CMD: &str = "ps -eo pcpu,rss,comm --sort=-pcpu";

/// A never-yet-probed baseline for a newly discovered service, so the first
/// real tick's deltas are computed against a known-empty prior tick rather
/// than reusing another service's stale state.
fn fresh_state(key: &str, label: &str) -> ServiceState {
    ServiceState {
        key: key.to_string(),
        label: label.to_string(),
        phase: Phase::Down,
        cpu_pct: 0.0,
        rss_bytes: 0,
        rss_delta: 0,
        kernel_count: NEVER_OBSERVED_KERNEL_COUNT,
        kernel_delta: 0,
        // Same never-observed sentinel: a rediscovered service may already have
        // `.tensorbin` shards from a prior run, so its first count must not read
        // as "N loaded this session".
        loaded_count: NEVER_OBSERVED_KERNEL_COUNT,
        loaded_delta: 0,
        safetensors_fds: 0,
        readiness: Readiness::Down,
        top_proc: None,
        last_log: None,
        progress: None,
        flat_ticks: 0,
        serving: None,
        media: None,
    }
}

/// Fold one tick's sample into the previous state, producing the next.
///
/// Pure — no I/O — so the background monitor is the only real caller and
/// tests can drive it with a synthetic [`TickSample`] instead of a live
/// container. Rule: compute rss/kernel deltas from the previous tick, derive
/// a momentary phase from those deltas + readiness, track how many
/// consecutive ticks nothing moved (`flat_ticks`), escalate to `Alarm` once
/// that streak crosses the not-ready alarm threshold, then estimate progress
/// from the (possibly escalated) phase. Only `key`/`label` carry over from
/// `prev` — every other field reflects this tick's sample or its derivation.
pub(crate) fn fold_tick(
    prev: &ServiceState,
    sample: TickSample,
    profile: &ModelProfile,
    cadence_secs: u32,
) -> ServiceState {
    let rss_delta = sample.rss_bytes as i64 - prev.rss_bytes as i64;
    let kernel_delta = sample.kernel_count as i64 - prev.kernel_count as i64;
    let loaded_delta = sample.loaded_count as i64 - prev.loaded_count as i64;
    let mut phase = Phase::derive(
        kernel_delta,
        sample.python_alive,
        rss_delta,
        loaded_delta,
        &sample.readiness,
    );

    let moved = kernel_delta > 0 || rss_delta > 0 || loaded_delta > 0;
    let flat_ticks = if moved { 0 } else { prev.flat_ticks + 1 };
    if is_alarm(flat_ticks, cadence_secs, &sample.readiness) {
        phase = Phase::Alarm;
    }

    let progress = estimate_progress(phase, sample.kernel_count, sample.rss_bytes, profile);

    // `None` when this tick's `/metrics` scrape carried no `vllm:` lines (not
    // a vLLM server, or the service is down); prior counters (if any) seed
    // the rate computation so the very first tick with metrics reports 0
    // rates rather than a spurious spike from "0 -> current".
    let serving =
        crate::workload::inference_server::parse_vllm_metrics(&sample.metrics_text).map(|cur| {
            crate::workload::inference_server::ServingStats::fold(
                prev.serving.as_ref().map(|s| &s.counters),
                &cur,
                cadence_secs,
            )
        });

    // Same shape for diffusion/video servers (tt-media-inference-server), which
    // expose the `tt_media_server_*` namespace instead of `vllm:`. A given
    // scrape only ever carries one namespace, so at most one of these is `Some`.
    let media =
        crate::workload::inference_server::parse_media_metrics(&sample.metrics_text).map(|cur| {
            crate::workload::inference_server::MediaStats::fold(
                prev.media.as_ref().map(|m| &m.counters),
                &cur,
                cadence_secs,
            )
        });

    ServiceState {
        key: prev.key.clone(),
        label: prev.label.clone(),
        phase,
        cpu_pct: sample.cpu_pct,
        rss_bytes: sample.rss_bytes,
        rss_delta,
        kernel_count: sample.kernel_count,
        kernel_delta,
        loaded_count: sample.loaded_count,
        loaded_delta,
        safetensors_fds: sample.safetensors_fds,
        readiness: sample.readiness,
        top_proc: sample.top_proc,
        last_log: sample.last_log,
        progress,
        flat_ticks,
        serving,
        media,
    }
}

/// Gather one tick's raw [`TickSample`] for `container` via `probe`, given
/// the service's static port/health-path definition. All I/O; the only logic
/// here is parsing what the probe returns (via the pure parsers in `probe.rs`).
fn build_sample(
    probe: &dyn ContainerProbe,
    container: &str,
    port: u16,
    health_path: &str,
) -> TickSample {
    // Only run the (slower) exec'd `find` when the container actually has
    // TT_METAL_HOME set — e.g. `prompt-server` doesn't, and would otherwise
    // pay for a doomed `find` every tick.
    let env = probe.env(container);
    let kernel_count = if parse_env_var(&env, "TT_METAL_HOME").is_some() {
        count_lines(&probe.exec(container, KERNEL_FIND_CMD))
    } else {
        0
    };
    // Loaded weight shards live under `$CACHE_ROOT`; only probe when that's set
    // (e.g. `prompt-server` has no such cache and would pay for a doomed `find`).
    let loaded_count = if parse_env_var(&env, "CACHE_ROOT").is_some() {
        count_lines(&probe.exec(container, LOADED_FIND_CMD))
    } else {
        0
    };

    let (cpu_pct, rss_bytes) = parse_docker_stats(&probe.stats(container)).unwrap_or((0.0, 0));

    // One `ps` snapshot serves both the display (top-CPU process) and the
    // liveness check. `python_alive` scans ALL rows, not just the top: a
    // loading server is often IO-bound and not the CPU leader, so keying
    // liveness off the top row alone flapped a loading service to Down.
    let ps_output = probe.exec(container, TOP_PROC_CMD);
    let top = top_process(&ps_output);
    let top_proc = top.map(|(comm, _cpu, _rss)| comm);
    let python_alive = contains_python(&ps_output);

    let (status, body) = probe.http(port, health_path);
    let readiness = parse_liveness(status, &body);

    // Trail: `ContainerProbe` has no docker-logs method yet, so there's no
    // line source to hand `last_non_health_line` here. Keeping the call
    // documents the intended wiring for when a `logs()` probe method lands.
    let last_log = last_non_health_line(&[]);

    // Best-effort vLLM Prometheus scrape. A non-vLLM server (or any non-200)
    // just yields text that `parse_vllm_metrics` can't find `vllm:` lines in,
    // which folds to `serving: None` downstream — so the status is ignored here.
    let (_status, metrics_text) = probe.http(port, "/metrics");

    TickSample {
        cpu_pct,
        rss_bytes,
        kernel_count,
        loaded_count,
        safetensors_fds: 0, // trail: FD-count probe not wired yet
        readiness,
        top_proc,
        python_alive,
        last_log,
        metrics_text,
    }
}

/// Build one tick's full snapshot: map each detected server to its
/// [`TickSample`] (via `probe`), look up its prior [`ServiceState`] in `prev`
/// by key (falling back to [`fresh_state`] for a newly-discovered service),
/// and fold the two together with [`fold_tick`]. Pure aside from the `probe`
/// calls, so it's the one piece of the tick loop worth unit-testing directly
/// — including the edge case of `detected` going empty (all tracked
/// containers stopped): the `for` loop below simply doesn't iterate, so the
/// result is an empty `Vec` rather than a carryover of stale states. That
/// matters because the panel falls back to rendering known services as Down
/// from the SERVERS table when the snapshot is empty — so a stale non-empty
/// snapshot (e.g. stuck at `Loading`) would otherwise linger forever after
/// every tracked container stops.
pub(crate) fn rebuild_snapshot(
    detected: &[InferenceServer],
    prev: &[ServiceState],
    probe: &dyn ContainerProbe,
    cadence_secs: u32,
) -> Vec<ServiceState> {
    let mut next_states = Vec::with_capacity(detected.len());
    for server in detected {
        let container = service_key(&server.source);
        // Resolve to a curated SERVERS entry when the model matches (nicer label
        // + known port/health path); otherwise track the container generically
        // so ANY detected tt-inference-server appears — LLM/vLLM deployments run
        // arbitrary `--model` values (e.g. Qwen/Qwen3-32B) not in the table, and
        // dropping them left the [i] view cold while the server was actually up.
        let (key, label, port, health_path): (String, String, u16, &str) =
            match service_for(server.model.as_deref()) {
                Some(def) => (
                    def.key.to_string(),
                    def.label.to_string(),
                    def.port,
                    def.health_path,
                ),
                None => {
                    let label = server
                        .model
                        .as_deref()
                        .map(|m| model_basename(m).to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| container.clone());
                    // Key by the (unique, stable) container name so prev-state
                    // lookup and flat-tick accumulation persist across ticks.
                    // Port from the parsed `--publish`, defaulting to 8000.
                    (
                        container.clone(),
                        label,
                        server.port.unwrap_or(8000),
                        "/health",
                    )
                }
            };
        let sample = build_sample(probe, &container, port, health_path);
        let prev_state = prev
            .iter()
            .find(|s| s.key == key)
            .cloned()
            .unwrap_or_else(|| fresh_state(&key, &label));
        next_states.push(fold_tick(
            &prev_state,
            sample,
            &ModelProfile::default(),
            cadence_secs,
        ));
    }
    next_states
}

/// Union host-process detections (`submitted`) with containers the probe
/// enumerated directly (`enumerated`), deduped by container name. Host-process
/// detection only sees a foreground `docker run`; enumeration (docker ps) also
/// finds detached / compose / systemd containers — so the union covers real
/// deployments the process scan alone would miss. Pure → unit-tested.
fn merge_detections(
    submitted: &[InferenceServer],
    enumerated: Vec<InferenceServer>,
) -> Vec<InferenceServer> {
    let container_of = |s: &InferenceServer| -> String { service_key(&s.source) };
    let mut out = submitted.to_vec();
    let mut seen: std::collections::HashSet<String> = out.iter().map(&container_of).collect();
    // When a container is seen by BOTH paths, the host-process scan (`submitted`)
    // often lacks the published host port (`port: None`) while the inspect path
    // (`enumerated`, via docker inspect HostPort) resolves it. Prefer the
    // resolved port so the monitor probes the right endpoint instead of the
    // default 8000. First pass: backfill missing ports on matching records.
    for e in &enumerated {
        let c = container_of(e);
        if e.port.is_some() {
            for existing in out.iter_mut() {
                if container_of(existing) == c && existing.port.is_none() {
                    existing.port = e.port;
                }
            }
        }
    }
    // Second pass: append containers only the inspect path saw (order-preserving).
    for e in enumerated {
        if seen.insert(container_of(&e)) {
            out.push(e);
        }
    }
    out
}

/// Owns the background probe thread and a lock-free cache of per-service state.
///
/// Submit the currently-detected servers each refresh; read a fresh snapshot
/// when building rows. Dropping the monitor disconnects the channel and the
/// thread exits on its own.
pub struct InferenceServerMonitor {
    cache: Arc<ArcSwap<Vec<ServiceState>>>,
    tx: Sender<Vec<InferenceServer>>,
}

/// A cheap, order-independent signature of a detected server set: two sets with
/// the same members (by image + model + port) compare equal, so the monitor can
/// tell "the fleet changed" (probe now) from "same fleet, routine re-poll".
fn detected_sig(servers: &[InferenceServer]) -> Vec<String> {
    let mut v: Vec<String> = servers
        .iter()
        .map(|s| {
            format!(
                "{}|{}|{:?}",
                s.image,
                s.model.as_deref().unwrap_or(""),
                s.port
            )
        })
        .collect();
    v.sort();
    v
}

impl InferenceServerMonitor {
    /// Spawn the background thread. The thread coalesces queued detections to
    /// the latest set, then at most once per `TICK_INTERVAL` probes each
    /// recognized service's container and folds the result into its running
    /// state. All docker/HTTP I/O happens here, never on the render path.
    pub fn spawn() -> Self {
        Self::spawn_with_probe(Box::new(DockerProbe))
    }

    /// Same as [`Self::spawn`], but with the [`ContainerProbe`] injected —
    /// production always passes a real `DockerProbe`; tests can pass a fake
    /// to drive the worker without a docker daemon. `pub(crate)` since only
    /// `spawn()` and this module's tests need it.
    pub(crate) fn spawn_with_probe(probe: Box<dyn ContainerProbe + Send>) -> Self {
        let cache = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let (tx, rx) = mpsc::channel::<Vec<InferenceServer>>();
        let cache_bg = Arc::clone(&cache);

        // Detached worker: exits when `tx` (and thus all senders) drops.
        let _ = std::thread::Builder::new()
            .name("tt-inference-monitor".into())
            .spawn(move || {
                let mut detected: Vec<InferenceServer> = Vec::new();
                let mut prev_snapshot: Vec<ServiceState> = Vec::new();
                let mut last_tick: Option<Instant> = None;
                // Signature of the detected set we last acted on. When a
                // submission changes it (a server just launched or stopped), we
                // probe immediately rather than waiting out the rest of the 5s
                // steady-state interval — so the `[i]` view flips from the cold
                // catalog to the loading snake within a refresh tick of launch,
                // not up to `TICK_INTERVAL` later.
                let mut prev_sig: Vec<String> = Vec::new();
                loop {
                    match rx.recv_timeout(TICK_INTERVAL) {
                        Ok(mut d) => {
                            // Drain any backlog; only the most recent set matters.
                            while let Ok(newer) = rx.try_recv() {
                                d = newer;
                            }
                            detected = d;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }

                    // Probe now if the fleet changed (fast detection of a
                    // just-launched server), else hold to the steady cadence.
                    let sig = detected_sig(&detected);
                    let changed = sig != prev_sig;
                    let due = changed || last_tick.is_none_or(|l| l.elapsed() >= TICK_INTERVAL);
                    if !due {
                        continue;
                    }
                    prev_sig = sig;

                    // Union the submitted host-process detections with containers
                    // the probe enumerates directly (docker ps), so detached /
                    // compose / systemd containers — which have no foreground
                    // `docker run` process — are monitored too.
                    let all = merge_detections(&detected, probe.list_servers());

                    // Rebuild unconditionally (even when `all` is empty) so a
                    // fully-stopped fleet clears the snapshot instead of leaving
                    // the last-known states cached forever.
                    let snapshot =
                        rebuild_snapshot(&all, &prev_snapshot, probe.as_ref(), CADENCE_SECS);
                    cache_bg.store(Arc::new(snapshot.clone()));
                    prev_snapshot = snapshot;
                    last_tick = Some(Instant::now());
                }
            });

        InferenceServerMonitor { cache, tx }
    }

    /// Hand the worker the servers detected this cycle. Non-blocking; a dead
    /// worker just means the send is dropped.
    pub fn submit(&self, detected: Vec<InferenceServer>) {
        let _ = self.tx.send(detected);
    }

    /// Lock-free snapshot of the latest per-service states.
    pub fn snapshot(&self) -> Vec<ServiceState> {
        self.cache.load().as_ref().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::probe::Readiness;

    /// A never-yet-probed baseline, for driving `fold_tick`'s very first
    /// tick in tests. Just the production `fresh_state` under a name that
    /// matches the task brief's test.
    #[allow(non_snake_case)]
    fn ServiceState_zeroed(key: &str, label: &str) -> ServiceState {
        fresh_state(key, label)
    }

    /// Build a `TickSample` that repeats a prior `ServiceState`'s readings —
    /// used with struct-update syntax to vary just the fields a test cares
    /// about while keeping the rest consistent with the previous tick.
    fn sample_like(state: &ServiceState) -> TickSample {
        TickSample {
            cpu_pct: state.cpu_pct,
            rss_bytes: state.rss_bytes,
            kernel_count: state.kernel_count,
            loaded_count: state.loaded_count,
            safetensors_fds: state.safetensors_fds,
            readiness: state.readiness.clone(),
            top_proc: state.top_proc.clone(),
            python_alive: true,
            last_log: state.last_log.clone(),
            metrics_text: String::new(),
        }
    }

    /// Clone of [`sample_like`]'s shape but driven by raw `/metrics` text
    /// instead of a prior `ServiceState`, for tests exercising the vLLM
    /// serving-stats fold in `fold_tick`.
    fn sample_with_metrics(text: &str) -> TickSample {
        TickSample {
            cpu_pct: 0.0,
            rss_bytes: 0,
            kernel_count: 0,
            loaded_count: 0,
            safetensors_fds: 0,
            readiness: Readiness::NotReady,
            top_proc: None,
            python_alive: true,
            last_log: None,
            metrics_text: text.into(),
        }
    }

    #[test]
    fn fold_tick_marks_loading_and_tracks_flat_ticks() {
        let prev = ServiceState_zeroed("z-image-turbo", "Z-Image-Turbo");
        let sample = TickSample {
            cpu_pct: 33.0,
            rss_bytes: 9_000_000_000,
            kernel_count: 500,
            loaded_count: 0,
            safetensors_fds: 2,
            readiness: Readiness::NotReady,
            top_proc: Some("python3".into()),
            python_alive: true,
            last_log: None,
            metrics_text: String::new(),
        };
        let next = fold_tick(&prev, sample, &ModelProfile::default(), 5);
        assert_eq!(
            next.phase,
            crate::workload::inference_server::state::Phase::Loading
        );
        assert_eq!(next.rss_delta, 9_000_000_000); // grew from 0
                                                   // second identical tick → nothing moved → flat_ticks increments
        let sample2 = TickSample {
            rss_bytes: 9_000_000_000,
            kernel_count: 500,
            ..sample_like(&next)
        };
        let next2 = fold_tick(&next, sample2, &ModelProfile::default(), 5);
        assert_eq!(next2.flat_ticks, 1);
    }

    #[test]
    fn fold_tick_carries_key_and_label_from_prev() {
        let prev = ServiceState_zeroed("flux", "FLUX.1-schnell");
        let sample = sample_like(&prev);
        let next = fold_tick(&prev, sample, &ModelProfile::default(), 5);
        assert_eq!(next.key, "flux");
        assert_eq!(next.label, "FLUX.1-schnell");
    }

    #[test]
    fn fold_tick_populates_serving_from_metrics() {
        let prev = ServiceState_zeroed("k", "L");
        let s1 = fold_tick(
            &prev,
            sample_with_metrics(
                "vllm:generation_tokens_total{m=\"M\"} 1000.0\nvllm:num_requests_running{m=\"M\"} 2.0\n",
            ),
            &ModelProfile::default(),
            5,
        );
        assert!(
            s1.serving.is_some(),
            "serving populated when metrics present"
        );
        assert_eq!(s1.serving.unwrap().requests_running, 2);
        // Next tick: +4210 gen tokens over 5s ≈ 842 tok/s (rate needs prev counters).
        let s2 = fold_tick(
            &s1,
            sample_with_metrics("vllm:generation_tokens_total{m=\"M\"} 5210.0\n"),
            &ModelProfile::default(),
            5,
        );
        assert!((s2.serving.unwrap().generation_tps - 842.0).abs() < 1.0);
    }

    #[test]
    fn fold_tick_serving_none_without_metrics() {
        let s = fold_tick(
            &ServiceState_zeroed("k", "L"),
            sample_with_metrics(""),
            &ModelProfile::default(),
            5,
        );
        assert!(s.serving.is_none());
    }

    #[test]
    fn fold_tick_populates_media_from_metrics_not_serving() {
        // A media/diffusion scrape (`tt_media_server_*`) must fold into `media`,
        // leave `serving` empty (the two are mutually exclusive), and surface the
        // in-flight `jobs_in_progress` gauge — the "acking" signal. Two ticks so
        // the completion rate has a prior tick to delta against.
        let s1 = fold_tick(
            &ServiceState_zeroed("k", "L"),
            sample_with_metrics(
                "tt_media_server_requests_base_total{m=\"M\"} 1.0\n\
                 tt_media_server_jobs_in_progress{m=\"M\"} 2.0\n",
            ),
            &ModelProfile::default(),
            5,
        );
        assert!(
            s1.media.is_some(),
            "media populated when media metrics present"
        );
        assert!(
            s1.serving.is_none(),
            "media scrape must not populate serving"
        );
        assert_eq!(
            s1.media.unwrap().jobs_in_progress,
            2,
            "in-flight gauge folded"
        );
        // Next tick: +1 completed generation over 5s = 12/min (rate needs prev).
        let s2 = fold_tick(
            &s1,
            sample_with_metrics(
                "tt_media_server_requests_base_total{m=\"M\"} 2.0\n\
                 tt_media_server_jobs_in_progress{m=\"M\"} 1.0\n",
            ),
            &ModelProfile::default(),
            5,
        );
        let m = s2.media.unwrap();
        assert_eq!(m.jobs_in_progress, 1);
        assert!(
            (m.generations_per_min - 12.0).abs() < 0.5,
            "1 gen / 5s = 12/min, got {}",
            m.generations_per_min
        );
    }

    #[test]
    fn fold_tick_media_none_for_vllm_scrape() {
        // Conversely, a vLLM scrape leaves `media` empty (populates `serving`).
        let s = fold_tick(
            &ServiceState_zeroed("k", "L"),
            sample_with_metrics("vllm:num_requests_running{m=\"M\"} 1.0\n"),
            &ModelProfile::default(),
            5,
        );
        assert!(s.media.is_none(), "vLLM scrape must not populate media");
        assert!(s.serving.is_some());
    }

    /// A fake `ContainerProbe` that returns steady, plausible readings for
    /// every call, regardless of the container name — good enough to drive
    /// `rebuild_snapshot` without a real docker daemon.
    struct FakeProbe;
    impl ContainerProbe for FakeProbe {
        fn env(&self, _c: &str) -> String {
            "TT_METAL_HOME=/x/tt-metal\n".into()
        }
        fn stats(&self, _c: &str) -> String {
            "50.0%|9GiB / 249GiB".into()
        }
        fn exec(&self, _c: &str, _sh: &str) -> String {
            "5\n".into() // kernel count
        }
        fn http(&self, _port: u16, path: &str) -> (u16, String) {
            if path == "/metrics" {
                (
                    200,
                    "vllm:generation_tokens_total{model_name=\"M\"} 1000.0\n\
                     vllm:num_requests_running{model_name=\"M\"} 2.0\n"
                        .into(),
                )
            } else {
                (405, "{\"detail\":\"Model is not ready\"}".into())
            }
        }
    }

    /// Regression test for the bug where an empty `detected` set (all tracked
    /// containers stopped) left the snapshot stuck at its last-known states
    /// forever instead of clearing to empty — the panel is supposed to fall
    /// back to rendering known services as Down from the SERVERS table once
    /// nothing is detected, which only works if `snapshot()` actually goes
    /// empty.
    #[test]
    fn empty_detected_clears_the_snapshot() {
        let srv = InferenceServer {
            source: Source::Docker {
                container: "c".into(),
            },
            image: "ghcr.io/tenstorrent/tt-media-inference-server:0.17.0".into(),
            model: Some("Z-Image-Turbo".into()),
            mesh: None,
            arch: None,
            device: None,
            port: Some(8000),
            uses_tt_device: true,
        };
        let one = rebuild_snapshot(std::slice::from_ref(&srv), &[], &FakeProbe, 5);
        assert_eq!(one.len(), 1);
        // when nothing is detected, the rebuild must return an empty snapshot (no stale carryover)
        let cleared = rebuild_snapshot(&[], &one, &FakeProbe, 5);
        assert!(cleared.is_empty());
    }

    /// Regression: a detected TT inference-server whose `--model` isn't in the
    /// curated SERVERS table (e.g. a generic vLLM `Qwen/Qwen3-32B`) must still
    /// be tracked — previously it was dropped, so the [i] view sat cold while
    /// the container was actually up. It's tracked generically, keyed by the
    /// container name, labeled from the model basename.
    #[test]
    fn tracks_detected_server_not_in_servers_table() {
        let srv = InferenceServer {
            source: Source::Docker {
                container: "tt-inference-server-2269d4f6".into(),
            },
            image: "ghcr.io/tenstorrent/tt-inference-server/vllm-tt-metal-src-release:0.14.0"
                .into(),
            model: Some("Qwen/Qwen3-32B".into()), // not in SERVERS
            mesh: None,
            arch: None,
            device: None,
            port: Some(8002),
            uses_tt_device: true,
        };
        let snap = rebuild_snapshot(std::slice::from_ref(&srv), &[], &FakeProbe, 5);
        assert_eq!(snap.len(), 1, "unrecognized model must still be tracked");
        assert_eq!(snap[0].key, "tt-inference-server-2269d4f6");
        assert!(
            snap[0].label.contains("Qwen3-32B"),
            "label derived from model basename, got {:?}",
            snap[0].label
        );
    }

    fn srv(container: &str, model: &str) -> InferenceServer {
        InferenceServer {
            source: Source::Docker {
                container: container.into(),
            },
            image: "ghcr.io/tenstorrent/tt-inference-server/vllm:0.14.0".into(),
            model: Some(model.into()),
            mesh: None,
            arch: None,
            device: None,
            port: Some(8000),
            uses_tt_device: true,
        }
    }

    #[test]
    fn merge_detections_prefers_resolved_host_port() {
        // Host-process scan saw the container but couldn't resolve its port...
        let mut submitted = srv("vllm", "Qwen/Qwen3-32B");
        submitted.port = None;
        // ...while the inspect path (docker inspect HostPort) did.
        let mut enumerated = srv("vllm", "Qwen/Qwen3-32B");
        enumerated.port = Some(8001);
        let merged = merge_detections(&[submitted], vec![enumerated]);
        assert_eq!(merged.len(), 1, "same container collapses to one record");
        assert_eq!(
            merged[0].port,
            Some(8001),
            "the resolved host port is preferred over the unresolved None"
        );
    }

    #[test]
    fn detected_sig_is_order_independent_and_tracks_membership() {
        // Same members in a different order → equal signature (routine re-poll,
        // no forced probe).
        let a = vec![srv("c1", "Llama-3.1-8B-Instruct"), srv("c2", "Qwen3-32B")];
        let b = vec![srv("c2", "Qwen3-32B"), srv("c1", "Llama-3.1-8B-Instruct")];
        assert_eq!(detected_sig(&a), detected_sig(&b));
        // A new server appears → signature changes (the monitor probes now
        // instead of waiting out the 5s interval).
        let mut c = a.clone();
        c.push(srv("c3", "Mistral-7B-Instruct-v0.3"));
        assert_ne!(detected_sig(&a), detected_sig(&c));
        // Empty vs non-empty differ (fleet came up / went away).
        assert_ne!(detected_sig(&[]), detected_sig(&a));
    }

    #[test]
    fn merge_detections_unions_and_dedupes_by_container() {
        // A detached container the process scan missed, plus a foreground one.
        let submitted = vec![srv("fg-container", "FLUX.1-schnell")];
        let enumerated = vec![
            srv("fg-container", "FLUX.1-schnell"), // dup of submitted → dropped
            srv("detached-vllm", "Qwen/Qwen3-32B"), // new → kept
        ];
        let merged = merge_detections(&submitted, enumerated);
        assert_eq!(
            merged.len(),
            2,
            "dup container collapses, detached one added"
        );
        let containers: Vec<String> = merged
            .iter()
            .map(|s| service_key(&s.source))
            .collect();
        assert!(containers.iter().any(|c| c == "fg-container"));
        assert!(containers.iter().any(|c| c == "detached-vllm"));
    }

    #[test]
    fn fold_tick_escalates_to_alarm_after_flat_streak() {
        // NotReady and nothing moving for 60 ticks @ 5s cadence = 5 minutes.
        let mut state = ServiceState_zeroed("flux", "FLUX.1-schnell");
        for _ in 0..60 {
            let sample = TickSample {
                readiness: Readiness::NotReady,
                ..sample_like(&state)
            };
            state = fold_tick(&state, sample, &ModelProfile::default(), 5);
        }
        assert_eq!(
            state.phase,
            crate::workload::inference_server::state::Phase::Alarm
        );
        assert_eq!(state.flat_ticks, 60);
    }
}
