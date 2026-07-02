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
use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::workload::inference_server::detect::{InferenceServer, Source};
use crate::workload::inference_server::logs::last_non_health_line;
use crate::workload::inference_server::probe::{
    count_lines, parse_docker_stats, parse_env_var, parse_liveness, top_process, ContainerProbe,
    DockerProbe, Readiness, TickSample,
};
use crate::workload::inference_server::services::{service_for, ServiceDef};
use crate::workload::inference_server::state::{
    estimate_progress, is_alarm, ModelProfile, Phase, ServiceState,
};

/// How often the background thread re-probes the detected services, at most.
/// (`submit` may be called far more often than this — e.g. once per render
/// tick — so the worker gates on `last_tick`'s age rather than probing on
/// every message, exactly like `LivenessProber`'s `PROBE_INTERVAL` gate.)
const TICK_INTERVAL: Duration = Duration::from_secs(5);
/// `TICK_INTERVAL` in whole seconds, passed to `fold_tick`'s alarm timer.
const CADENCE_SECS: u32 = TICK_INTERVAL.as_secs() as u32;

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
/// artifacts. `$TT_METAL_HOME` is expanded by the container's own shell (via
/// `sh -c`), not substituted here — we only use the host-side `env` dump to
/// decide *whether* to run this at all (see `build_sample`).
const KERNEL_FIND_CMD: &str = "find \"$TT_METAL_HOME/built\" -name '*.dephash' 2>/dev/null";
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
        safetensors_fds: 0,
        readiness: Readiness::Down,
        top_proc: None,
        last_log: None,
        progress: None,
        flat_ticks: 0,
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
    let mut phase = Phase::derive(kernel_delta, sample.python_alive, rss_delta, &sample.readiness);

    let moved = kernel_delta > 0 || rss_delta > 0;
    let flat_ticks = if moved { 0 } else { prev.flat_ticks + 1 };
    if is_alarm(flat_ticks, cadence_secs, &sample.readiness) {
        phase = Phase::Alarm;
    }

    let progress = estimate_progress(phase, sample.kernel_count, sample.rss_bytes, profile);

    ServiceState {
        key: prev.key.clone(),
        label: prev.label.clone(),
        phase,
        cpu_pct: sample.cpu_pct,
        rss_bytes: sample.rss_bytes,
        rss_delta,
        kernel_count: sample.kernel_count,
        kernel_delta,
        safetensors_fds: sample.safetensors_fds,
        readiness: sample.readiness,
        top_proc: sample.top_proc,
        last_log: sample.last_log,
        progress,
        flat_ticks,
    }
}

/// Gather one tick's raw [`TickSample`] for `container` via `probe`, given
/// the service's static port/health-path definition. All I/O; the only logic
/// here is parsing what the probe returns (via the pure parsers in `probe.rs`).
fn build_sample(probe: &dyn ContainerProbe, container: &str, def: &ServiceDef) -> TickSample {
    // Only run the (slower) exec'd `find` when the container actually has
    // TT_METAL_HOME set — e.g. `prompt-server` doesn't, and would otherwise
    // pay for a doomed `find` every tick.
    let env = probe.env(container);
    let kernel_count = if parse_env_var(&env, "TT_METAL_HOME").is_some() {
        count_lines(&probe.exec(container, KERNEL_FIND_CMD))
    } else {
        0
    };

    let (cpu_pct, rss_bytes) = parse_docker_stats(&probe.stats(container)).unwrap_or((0.0, 0));

    let top = top_process(&probe.exec(container, TOP_PROC_CMD));
    let top_proc = top.map(|(comm, _cpu, _rss)| comm);
    let python_alive = top_proc.as_deref().is_some_and(|c| c.contains("python"));

    let (status, body) = probe.http(def.port, def.health_path);
    let readiness = parse_liveness(status, &body);

    // Trail: `ContainerProbe` has no docker-logs method yet, so there's no
    // line source to hand `last_non_health_line` here. Keeping the call
    // documents the intended wiring for when a `logs()` probe method lands.
    let last_log = last_non_health_line(&[]);

    TickSample {
        cpu_pct,
        rss_bytes,
        kernel_count,
        safetensors_fds: 0, // trail: FD-count probe not wired yet
        readiness,
        top_proc,
        python_alive,
        last_log,
    }
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

impl InferenceServerMonitor {
    /// Spawn the background thread. The thread coalesces queued detections to
    /// the latest set, then at most once per `TICK_INTERVAL` probes each
    /// recognized service's container and folds the result into its running
    /// state. All docker/HTTP I/O happens here, never on the render path.
    pub fn spawn() -> Self {
        let cache = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let (tx, rx) = mpsc::channel::<Vec<InferenceServer>>();
        let cache_bg = Arc::clone(&cache);
        let probe: Box<dyn ContainerProbe> = Box::new(DockerProbe);

        // Detached worker: exits when `tx` (and thus all senders) drops.
        let _ = std::thread::Builder::new()
            .name("tt-inference-monitor".into())
            .spawn(move || {
                let mut detected: Vec<InferenceServer> = Vec::new();
                let mut history: HashMap<String, ServiceState> = HashMap::new();
                let mut last_tick: Option<Instant> = None;
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

                    let due = last_tick.is_none_or(|l| l.elapsed() >= TICK_INTERVAL);
                    if detected.is_empty() || !due {
                        continue;
                    }

                    let mut next_states = Vec::with_capacity(detected.len());
                    for server in &detected {
                        let Source::Docker { container } = &server.source;
                        let Some(def) = service_for(server.model.as_deref()) else {
                            continue; // unrecognized model → not tracked
                        };
                        let sample = build_sample(probe.as_ref(), container, def);
                        let prev = history
                            .remove(def.key)
                            .unwrap_or_else(|| fresh_state(def.key, def.label));
                        next_states.push(fold_tick(&prev, sample, &ModelProfile::default(), CADENCE_SECS));
                    }
                    // Rebuild history from this cycle's outputs for the next tick.
                    history = next_states.iter().map(|s| (s.key.clone(), s.clone())).collect();
                    cache_bg.store(Arc::new(next_states));
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
            safetensors_fds: state.safetensors_fds,
            readiness: state.readiness.clone(),
            top_proc: state.top_proc.clone(),
            python_alive: true,
            last_log: state.last_log.clone(),
        }
    }

    #[test]
    fn fold_tick_marks_loading_and_tracks_flat_ticks() {
        let prev = ServiceState_zeroed("z-image-turbo", "Z-Image-Turbo");
        let sample = TickSample {
            cpu_pct: 33.0,
            rss_bytes: 9_000_000_000,
            kernel_count: 500,
            safetensors_fds: 2,
            readiness: Readiness::NotReady,
            top_proc: Some("python3".into()),
            python_alive: true,
            last_log: None,
        };
        let next = fold_tick(&prev, sample, &ModelProfile::default(), 5);
        assert_eq!(next.phase, crate::workload::inference_server::state::Phase::Loading);
        assert_eq!(next.rss_delta, 9_000_000_000); // grew from 0
        // second identical tick → nothing moved → flat_ticks increments
        let sample2 = TickSample { rss_bytes: 9_000_000_000, kernel_count: 500, ..sample_like(&next) };
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
    fn fold_tick_escalates_to_alarm_after_flat_streak() {
        // NotReady and nothing moving for 60 ticks @ 5s cadence = 5 minutes.
        let mut state = ServiceState_zeroed("flux", "FLUX.1-schnell");
        for _ in 0..60 {
            let sample = TickSample { readiness: Readiness::NotReady, ..sample_like(&state) };
            state = fold_tick(&state, sample, &ModelProfile::default(), 5);
        }
        assert_eq!(state.phase, crate::workload::inference_server::state::Phase::Alarm);
        assert_eq!(state.flat_ticks, 60);
    }
}
