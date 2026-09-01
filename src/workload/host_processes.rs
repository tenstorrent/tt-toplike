// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Cross-platform process collector for the Insights screen.
//!
//! Enumerates host processes via `sysinfo` and tags likely ML/inference
//! runtimes (see `inference_match`). Produces gate-free `ProcRow`s that the
//! Linux/TT path may enrich by PID with device-fd attribution.

use crate::workload::inference_match;
use std::collections::HashMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// TT-specific per-process info, merged in by PID on TT hardware. Primitives
/// only, so `ProcRow` stays free of any `cfg`-gated type.
#[derive(Debug, Clone, Default)]
pub struct TtProcInfo {
    pub device_indices: Vec<usize>,
    pub hugepages_1g: usize,
    pub hugepages_2m: usize,
}

/// One row in the Insights process panel.
#[derive(Debug, Clone)]
pub struct ProcRow {
    pub pid: i32,
    pub name: String,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
    /// Matched inference-runtime label (name/cmdline heuristic), or `None`.
    /// This says *what kind* of process it is, not whether it's doing work —
    /// see `active` for that.
    pub inference: Option<&'static str>,
    /// Whether a matched runtime is *actively working* right now (model loaded /
    /// burning CPU), as opposed to an idle resident daemon. Always `false` for
    /// non-inference rows. Idle runtimes are not force-surfaced (see
    /// `select_rows`), so a parked `ollama serve` with no model no longer
    /// crowds the panel.
    pub active: bool,
    pub tt: Option<TtProcInfo>,
}

/// A matched runtime whose own CPU is at/below this percent is treated as idle
/// *unless* a runtime-specific "busy" signal corroborates activity (e.g. an
/// ollama model-runner worker). Small but non-zero so a daemon's housekeeping
/// blips don't read as real inference work.
const IDLE_CPU_PCT: f32 = 1.0;

/// Does this process look like an ollama *model runner* — the worker ollama
/// spawns once a model is actually loaded (`ollama runner ...`, or the older
/// `ollama_llama_server` helper)? Its mere existence means a model is resident,
/// which is the authoritative "ollama is doing something" signal even when the
/// parent `ollama serve` daemon sits at 0% CPU. Pure for testability.
fn is_ollama_runner(name: &str, cmdline: &str) -> bool {
    let hay = format!("{} {}", name, cmdline).to_lowercase();
    hay.contains("ollama runner") || hay.contains("ollama_llama_server")
}

/// Decide whether a matched inference runtime is actively working.
///
/// Precedence: a fresh authoritative probe verdict wins; otherwise fall back to
/// the cheap, pure, snapshot-driven signal (no network calls on the hot path):
/// - Any matched process above `IDLE_CPU_PCT` is active (covers every runtime).
/// - For runtimes whose idle/active shape we *know*, corroborate at ~0% CPU:
///   ollama's `serve` daemon idles permanently with no model loaded, so it's
///   only active when a model-runner worker is present in the same snapshot.
/// - Runtimes with no known idle shape fall back to the CPU signal alone.
fn runtime_active(
    label: &str,
    cpu_pct: f32,
    ollama_runner_present: bool,
    probe: Option<bool>,
) -> bool {
    // An authoritative probe (e.g. ollama /api/ps, vllm /v1/models) is the truth
    // when available — it catches a server holding a model at ~0% CPU.
    if let Some(ready) = probe {
        return ready;
    }
    if cpu_pct > IDLE_CPU_PCT {
        return true;
    }
    match label {
        "ollama" => ollama_runner_present,
        _ => false,
    }
}

/// Testable core of the per-process decision `detected_inference_servers`
/// makes: given one process's (name, cmdline, environ, pid), which
/// `InferenceServer` (if any) it contributes. Docker detection is tried
/// first — a TT inference-server container's foreground `docker run`
/// process would also, in principle, cmdline-match nothing
/// vLLM-direct-shaped, so the ordering is defensive rather than
/// load-bearing today.
#[cfg(target_os = "linux")]
fn classify_one(
    name: &str,
    cmdline: &str,
    environ: &str,
    pid: i32,
) -> Option<crate::workload::InferenceServer> {
    use crate::workload::inference_server::{parse_direct_vllm, parse_inference_server};
    parse_inference_server(name, cmdline).or_else(|| parse_direct_vllm(name, cmdline, environ, pid))
}

/// True when a `/proc/<pid>/cgroup` body places the process inside a container.
///
/// [`parse_direct_vllm`] is by contract the *non-Docker* path, and containers
/// are enumerated separately (`docker ps`) then merged by container name — a
/// key that can never collide with `host-vllm-<pid>`. So a containerized
/// process matching the host path becomes a second roster row for one server:
/// exactly the duplicate Phase 30 removed, reintroduced from the other side.
///
/// This matters because tt-inference-server's image ENTRYPOINT is
/// `python run_vllm_api_server.py`, which the host matcher now recognizes.
/// Cross-uid `/proc/<pid>/environ` is unreadable, so an unprivileged run never
/// gets far enough to classify one — but that is a permission accident, not a
/// rule, and it stops protecting us the moment tt-toplike runs as root.
///
/// Matched per path *segment* rather than by substring: the docker daemon
/// itself lives at `/system.slice/docker.service` on the host, and a substring
/// test would exclude host processes on the strength of the daemon's unit name.
/// Read from the host, where the full path is visible — a process reading its
/// own cgroup from inside a cgroup namespace would just see `0::/`.
#[cfg(target_os = "linux")]
fn cgroup_is_containerized(text: &str) -> bool {
    text.lines().any(|line| {
        // v1 lines are `hier:controllers:/path`; v2 is `0::/path`. The path is
        // whatever follows the last colon in either case.
        let path = line.rsplit(':').next().unwrap_or(line);
        path.split('/').any(|seg| {
            let seg = seg.trim();
            seg == "docker"
                || seg.starts_with("docker-")
                || seg.starts_with("containerd-")
                || seg.starts_with("cri-containerd-")
                || seg.starts_with("crio-")
                || seg.starts_with("libpod-")
                || seg.starts_with("kubepods")
        })
    })
}

/// True when `pid` is the *root* of its inference-server match family: no
/// ancestor (walking `parent_of`, which maps a pid to its parent pid) is
/// itself in `matched`. Kept independent of `sysinfo` types (generic over any
/// `Eq + Hash + Copy` id) so it's directly unit-testable with plain `u32`s —
/// the real caller builds `parent_of`/`matched` from `sysinfo::Process`.
///
/// `hops` is capped at 32 purely as a defensive bound against a corrupt or
/// cyclic parent map (a real process tree is never anywhere near this deep) —
/// it does not reflect any expected multiprocessing fan-out depth.
fn is_match_root<T: Eq + std::hash::Hash + Copy>(
    pid: T,
    parent_of: &HashMap<T, T>,
    matched: &std::collections::HashSet<T>,
) -> bool {
    let mut ancestor = parent_of.get(&pid).copied();
    let mut hops = 0;
    while let Some(anc) = ancestor {
        if hops >= 32 {
            break;
        }
        if matched.contains(&anc) {
            return false;
        }
        ancestor = parent_of.get(&anc).copied();
        hops += 1;
    }
    true
}

/// Owns a `sysinfo::System` refreshed for process listing.
pub struct HostProcessMonitor {
    sys: System,
}

impl HostProcessMonitor {
    pub fn new() -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
        );
        HostProcessMonitor { sys }
    }

    /// Refresh the process list (call on the ~2s cadence).
    ///
    /// Note: sysinfo 0.38 changed `refresh_processes_specifics` to take
    /// `(ProcessesToUpdate, remove_dead: bool, ProcessRefreshKind)` — the
    /// brief's single-argument form was written against an older API sketch.
    pub fn update(&mut self) {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything(),
        );
    }

    /// The inference runtimes detected in the current snapshot (deduped by label),
    /// as cheap `(label, pid, cmdline)` records — no socket or network I/O here.
    /// Feed these to a [`crate::workload::LivenessProber`] each refresh; the
    /// prober's background thread resolves each one's real listening port and
    /// probes it. Confirm-only: every record corresponds to a process we saw.
    pub fn detected_runtimes(&self) -> Vec<crate::workload::DetectedRuntime> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (pid, p) in self.sys.processes() {
            let name = p.name().to_string_lossy().to_string();
            let cmdline = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(label) = inference_match(&name, &cmdline) {
                if seen.insert(label) {
                    out.push(crate::workload::DetectedRuntime {
                        label: label.to_string(),
                        pid: i32::try_from(pid.as_u32()).unwrap_or(i32::MAX),
                        cmdline,
                    });
                }
            }
        }
        out
    }

    /// The TT inference-server containers *and* direct (non-Docker) vLLM-on-TT
    /// processes detected in the current snapshot (deduped by identity key),
    /// as structured [`crate::workload::InferenceServer`] records — no
    /// docker/proc I/O beyond what `sysinfo` already collected this refresh.
    /// Feed these to a [`crate::workload::InferenceServerMonitor`] each
    /// refresh.
    ///
    /// A real direct-vLLM launch commonly forks several worker/engine-core
    /// processes (one per tensor-parallel rank, etc.) — `fork` preserves the
    /// parent's cmdline and environment in `/proc/<pid>/cmdline`, so each
    /// worker independently matches [`classify_one`]'s heuristic too. Without
    /// filtering these out, one real launch shows up as N duplicate roster
    /// entries (each with its own `host-vllm-<pid>` key, so the existing
    /// dedup-by-key never collapses them). [`is_match_root`] keeps only the
    /// root of each match family — a matching process whose ancestors, if
    /// any, don't also match.
    #[cfg(target_os = "linux")]
    pub fn detected_inference_servers(&self) -> Vec<crate::workload::InferenceServer> {
        use crate::workload::inference_server::service_key;

        let mut classified: HashMap<sysinfo::Pid, crate::workload::InferenceServer> =
            HashMap::new();
        let mut parent_of: HashMap<sysinfo::Pid, sysinfo::Pid> = HashMap::new();
        for (pid, p) in self.sys.processes() {
            if let Some(parent) = p.parent() {
                parent_of.insert(*pid, parent);
            }
            let name = p.name().to_string_lossy().to_string();
            let cmdline = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            let environ = p
                .environ()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n");
            let pid_i32 = i32::try_from(pid.as_u32()).unwrap_or(i32::MAX);
            if let Some(server) = classify_one(&name, &cmdline, &environ, pid_i32) {
                // Only the host path needs the container check: a `Source::Docker`
                // match came from a `docker run` cmdline, which is the client on
                // the host by construction. Reading cgroup here rather than in
                // `classify_one` keeps that function pure and costs one small
                // read per *matched* process, not per process on the box.
                if matches!(server.source, crate::workload::Source::Host { .. })
                    && std::fs::read_to_string(format!("/proc/{pid_i32}/cgroup"))
                        .is_ok_and(|c| cgroup_is_containerized(&c))
                {
                    continue;
                }
                classified.insert(*pid, server);
            }
        }

        let matched: std::collections::HashSet<sysinfo::Pid> = classified.keys().copied().collect();

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (&pid, server) in &classified {
            if is_match_root(pid, &parent_of, &matched) && seen.insert(service_key(&server.source))
            {
                out.push(server.clone());
            }
        }
        out
    }

    /// Build selected rows: all *actively-working* inference runtimes (force-kept),
    /// plus the busiest remaining processes filling the slots up to `max` total.
    /// Idle resident daemons (e.g. `ollama serve` with no model) are not
    /// force-kept — see `select_rows`.
    ///
    /// `probe_verdicts` maps a runtime label to an authoritative `active` verdict
    /// from a [`crate::workload::LivenessProber`]; pass an empty map to rely solely
    /// on the cheap snapshot tier.
    pub fn rows(&self, max: usize, probe_verdicts: &HashMap<String, bool>) -> Vec<ProcRow> {
        select_rows(self.build_all_rows(probe_verdicts), max)
    }

    /// Rows for TT-attributed processes only (real-TT backends). `tt_pids` is the
    /// set of PIDs holding a /dev/tenstorrent fd (from `flat_process_list`). Rows are
    /// not TT-enriched here — the caller runs `enrich_proc_rows_tt` afterward as usual.
    pub fn tt_rows(&self, max: usize, tt_pids: &std::collections::HashSet<i32>) -> Vec<ProcRow> {
        let all = self.build_all_rows(&HashMap::new());
        select_tt_rows(all, tt_pids, max)
    }

    /// Build the full, untrimmed set of `ProcRow`s for the current snapshot,
    /// tagging each with its matched inference runtime (if any) and whether
    /// that runtime looks actively busy right now. Shared by `rows()` (host
    /// path, uses `probe_verdicts`) and `tt_rows()` (TT path, no probe
    /// verdicts — the panel is filtered by device-fd attribution instead).
    fn build_all_rows(&self, probe_verdicts: &HashMap<String, bool>) -> Vec<ProcRow> {
        // First pass: pull the raw fields once (name + cmdline + cpu/mem). We
        // need the whole snapshot before we can judge liveness, because some
        // "busy" signals are cross-process (e.g. an ollama model-runner worker
        // proves the sibling `ollama serve` daemon is active).
        let raw: Vec<(i32, String, String, f32, u64)> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let name = p.name().to_string_lossy().to_string();
                let cmdline = p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                (
                    i32::try_from(pid.as_u32()).unwrap_or(i32::MAX), // PIDs never exceed i32::MAX on supported OSes; clamp defensively.
                    name,
                    cmdline,
                    p.cpu_usage(),
                    p.memory(),
                )
            })
            .collect();

        // Snapshot-level "busy" signal: is any ollama model-runner resident?
        let ollama_runner_present = raw
            .iter()
            .any(|(_, name, cmdline, _, _)| is_ollama_runner(name, cmdline));

        raw.into_iter()
            .map(|(pid, name, cmdline, cpu_pct, mem_bytes)| {
                let inference = inference_match(&name, &cmdline);
                let active = inference.is_some_and(|l| {
                    runtime_active(
                        l,
                        cpu_pct,
                        ollama_runner_present,
                        probe_verdicts.get(l).copied(),
                    )
                });
                ProcRow {
                    pid,
                    inference,
                    active,
                    name,
                    cpu_pct,
                    mem_bytes,
                    tt: None,
                }
            })
            .collect()
    }
}

impl Default for HostProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure selection: all *actively-working* inference rows first (CPU desc), then
/// the highest-CPU remaining rows fill the slots up to `max`.
///
/// Only `active` runtimes are force-kept. An idle resident daemon (matched but
/// `active == false`, e.g. `ollama serve` with no model loaded) is demoted into
/// the fill pool, so it only appears if it's genuinely among the busiest — it
/// no longer claims a guaranteed slot just for existing.
pub(crate) fn select_rows(mut all: Vec<ProcRow>, max: usize) -> Vec<ProcRow> {
    // Sort entire list by cpu desc so both partitions are already ordered.
    all.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let (mut kept, rest): (Vec<ProcRow>, Vec<ProcRow>) = all.into_iter().partition(|r| r.active);
    // Fill remaining slots (after the active runtimes) with the busiest rest.
    let remaining = max.saturating_sub(kept.len());
    kept.extend(rest.into_iter().take(remaining));
    kept
}

/// Pure: keep only rows whose PID is in `tt_pids` (attributed to a TT device),
/// sorted by CPU desc, capped at `max`. Used on real-TT backends so the panel
/// shows only processes contributing to TT load.
pub(crate) fn select_tt_rows(
    mut all: Vec<ProcRow>,
    tt_pids: &std::collections::HashSet<i32>,
    max: usize,
) -> Vec<ProcRow> {
    all.retain(|r| tt_pids.contains(&r.pid));
    all.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all.truncate(max);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i32, cpu: f32, inf: Option<&'static str>, active: bool) -> ProcRow {
        ProcRow {
            pid,
            name: format!("p{pid}"),
            cpu_pct: cpu,
            mem_bytes: 0,
            inference: inf,
            active,
            tt: None,
        }
    }

    #[test]
    fn active_runtimes_sort_first_then_by_cpu() {
        let all = vec![
            row(1, 90.0, None, false),
            row(2, 10.0, Some("ollama"), true),
            row(3, 50.0, None, false),
            row(4, 5.0, Some("vllm"), true),
        ];
        let out = select_rows(all, 3);
        // active runtimes (2,4) first by cpu desc; then top non-active by cpu (1)
        assert_eq!(out.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![2, 4, 1]);
    }

    #[test]
    fn active_runtime_kept_even_at_low_cpu() {
        // An *active* runtime at tiny cpu must survive even when many busy procs exist.
        let mut all: Vec<ProcRow> = (10..20).map(|p| row(p, 80.0, None, false)).collect();
        all.push(row(99, 0.1, Some("vllm"), true));
        let out = select_rows(all, 3);
        assert!(
            out.iter().any(|r| r.pid == 99),
            "active runtime must be force-kept"
        );
    }

    #[test]
    fn idle_runtime_not_force_kept() {
        // An idle, matched-but-inactive daemon (e.g. `ollama serve`, no model) at
        // ~0% cpu must NOT claim a guaranteed slot when busier processes exist.
        let mut all: Vec<ProcRow> = (10..20).map(|p| row(p, 80.0, None, false)).collect();
        all.push(row(99, 0.1, Some("ollama"), false));
        let out = select_rows(all, 3);
        assert!(
            !out.iter().any(|r| r.pid == 99),
            "idle runtime should be demoted to the fill pool, not force-kept"
        );
    }

    #[test]
    fn runtime_active_uses_cpu_threshold_for_any_runtime() {
        assert!(
            runtime_active("vllm", 5.0, false, None),
            "busy runtime is active"
        );
        assert!(
            !runtime_active("vllm", 0.0, false, None),
            "idle unknown-shape runtime is inactive"
        );
    }

    #[test]
    fn ollama_idle_without_runner_is_inactive() {
        // The lone `ollama serve` daemon at 0% cpu, no model-runner present.
        assert!(!runtime_active("ollama", 0.0, false, None));
        // ...but a loaded model (runner present) makes it active even at 0% cpu.
        assert!(runtime_active("ollama", 0.0, true, None));
    }

    #[test]
    fn fresh_probe_verdict_overrides_cheap_signal() {
        // A server holding a model at 0% cpu with no known subprocess shape would
        // read idle on the cheap tier, but a probe says "ready" → active.
        assert!(runtime_active("vllm", 0.0, false, Some(true)));
        // And a probe can authoritatively say "idle" even if cpu briefly spiked.
        assert!(!runtime_active("vllm", 50.0, false, Some(false)));
    }

    #[test]
    fn detects_ollama_runner_but_not_the_serve_daemon() {
        assert!(is_ollama_runner(
            "ollama",
            "/usr/local/bin/ollama runner --model /root/.ollama/models/blob --ctx-size 8192"
        ));
        assert!(is_ollama_runner("ollama_llama_server", ""));
        assert!(!is_ollama_runner("ollama", "/usr/local/bin/ollama serve"));
    }

    #[test]
    fn select_tt_rows_keeps_only_attributed_pids_sorted_by_cpu() {
        use std::collections::HashSet;
        let all = vec![
            row(1, 10.0, None, false),
            row(2, 90.0, None, false), // high cpu but NOT a TT pid
            row(3, 50.0, Some("vllm"), true),
            row(4, 20.0, None, false),
        ];
        let tt: HashSet<i32> = [1, 3, 4].into_iter().collect();
        let out = select_tt_rows(all, &tt, 10);
        // only TT pids, sorted by cpu desc: 3 (50), 4 (20), 1 (10)
        assert_eq!(out.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![3, 4, 1]);
        // empty pid set → empty
        assert!(select_tt_rows(vec![row(1, 5.0, None, false)], &HashSet::new(), 10).is_empty());
    }

    /// Smoke test: `detected_inference_servers` must not panic against a real
    /// process snapshot on whatever box CI happens to run on, and — since it's
    /// almost certainly not running a `docker run ... tt-inference-server ...`
    /// process — should return an empty (or at least non-panicking) `Vec`.
    #[test]
    #[cfg(target_os = "linux")]
    fn detected_inference_servers_smoke() {
        let mut mon = HostProcessMonitor::new();
        mon.update();
        let servers = mon.detected_inference_servers();
        // No assertion on contents — just confirm the call is safe and typed.
        let _: Vec<crate::workload::InferenceServer> = servers;
    }

    /// `parse_direct_vllm` is by contract the *non-Docker* path, and widening
    /// it to `run_vllm_api_server.py` made that contract load-bearing: that
    /// script is the ENTRYPOINT of tt-inference-server's own image, so the
    /// process inside a running container matches it exactly.
    ///
    /// Containers are enumerated separately (`docker ps`) and merged by
    /// container name, which can never collide with a `host-vllm-<pid>` key —
    /// so a containerized match would show up as a second roster row for one
    /// server. An unprivileged reader is saved by `/proc/<pid>/environ` being
    /// unreadable across uids, but that is a permission accident, not a rule;
    /// run tt-toplike as root and it would double-count.
    #[test]
    #[cfg(target_os = "linux")]
    fn containerized_processes_are_excluded_from_the_host_path() {
        // Verbatim from this box — a plain user process must not be excluded.
        assert!(!cgroup_is_containerized(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/\
             app-org.kde.konsole-cff422f472f04f79b3ba30f6e259e313.scope"
        ));

        // The docker *daemon* runs on the host under a unit whose name
        // contains "docker" — the case a naive `contains("docker")` gets
        // wrong, excluding host processes it should keep.
        assert!(!cgroup_is_containerized("0::/system.slice/docker.service"));
        assert!(!cgroup_is_containerized("0::/"));

        for inside in [
            // cgroup v2, systemd driver (what modern Docker writes).
            "0::/system.slice/docker-3f1c9a2b4d5e6f708192a3b4c5d6e7f8.scope",
            // cgroup v2, cgroupfs driver.
            "0::/docker/3f1c9a2b4d5e6f708192a3b4c5d6e7f8",
            // cgroup v1, multiple controller lines.
            "12:cpuset:/docker/3f1c9a2b4d5e6f708192a3b4c5d6e7f8\n\
             11:memory:/docker/3f1c9a2b4d5e6f708192a3b4c5d6e7f8",
            // Kubernetes, podman, cri-o.
            "0::/kubepods.slice/kubepods-besteffort.slice/cri-containerd-abc.scope",
            "0::/machine.slice/libpod-3f1c9a2b4d5e.scope",
            "0::/machine.slice/crio-3f1c9a2b4d5e.scope",
        ] {
            assert!(
                cgroup_is_containerized(inside),
                "{inside:?} is a containerized cgroup path"
            );
        }
    }

    #[test]
    fn is_match_root_keeps_the_root_and_rejects_matching_descendants() {
        // A launcher (100) forks 3 worker ranks (101, 102, 103) that all
        // inherit its cmdline/environment and independently match the same
        // classification -- exactly the vLLM tensor-parallel-worker shape
        // that flooded the roster with duplicates. Only 100 has no matching
        // ancestor.
        let parent_of: HashMap<u32, u32> =
            [(101, 100), (102, 100), (103, 100)].into_iter().collect();
        let matched: std::collections::HashSet<u32> = [100u32, 101, 102, 103].into_iter().collect();

        assert!(
            is_match_root(100, &parent_of, &matched),
            "the launcher itself has no ancestor at all"
        );
        assert!(
            !is_match_root(101, &parent_of, &matched),
            "101's parent (100) also matches -- not a root"
        );
        assert!(
            !is_match_root(102, &parent_of, &matched),
            "102's parent (100) also matches -- not a root"
        );
        assert!(
            !is_match_root(103, &parent_of, &matched),
            "103's parent (100) also matches -- not a root"
        );
    }

    #[test]
    fn is_match_root_keeps_a_match_whose_parent_does_not_also_match() {
        // 200's parent (199) is a plain shell that never matched
        // classify_one -- 200 is a genuine standalone launch, not a
        // multiprocessing worker, and must be counted.
        let parent_of: HashMap<u32, u32> = [(200, 199)].into_iter().collect();
        let matched: std::collections::HashSet<u32> = [200u32].into_iter().collect();

        assert!(is_match_root(200, &parent_of, &matched));
    }

    #[test]
    fn is_match_root_walks_past_a_non_matching_intermediate_ancestor() {
        // grandchild (302) -> child (301, does NOT match) -> launcher (300,
        // matches). A real vLLM launch can insert an intermediate shell/
        // supervisor hop between the launcher and a worker, so the check
        // must walk the whole chain, not just the immediate parent.
        let parent_of: HashMap<u32, u32> = [(301, 300), (302, 301)].into_iter().collect();
        let matched: std::collections::HashSet<u32> = [300u32, 302].into_iter().collect();

        assert!(
            !is_match_root(302, &parent_of, &matched),
            "302's grandparent (300) matches, even though its immediate parent (301) doesn't"
        );
    }

    #[test]
    fn is_match_root_defends_against_a_parent_cycle() {
        // A corrupt/racing parent map (pid reuse mid-scan) could in
        // principle form a cycle; the hop cap must stop the walk rather than
        // looping forever.
        let parent_of: HashMap<u32, u32> = [(1, 2), (2, 1)].into_iter().collect();
        let matched: std::collections::HashSet<u32> = std::collections::HashSet::new();
        assert!(is_match_root(1, &parent_of, &matched));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_one_prefers_docker_then_falls_back_to_direct_vllm() {
        let docker_cmd = "docker run --rm --name tt-inference-server-x \
            --device /dev/tenstorrent:/dev/tenstorrent --publish 0.0.0.0:8002:8002 \
            ghcr.io/tenstorrent/tt-inference-server/vllm-tt-metal-src-release:0.14.0 --model M";
        assert!(matches!(
            classify_one("docker", docker_cmd, "", 1).unwrap().source,
            crate::workload::Source::Docker { .. }
        ));

        let direct = classify_one(
            "vllm",
            "vllm serve episod/tt-tnt-1024 --port 8000",
            "MESH_DEVICE=P300x2\n",
            4242,
        )
        .unwrap();
        assert!(matches!(
            direct.source,
            crate::workload::Source::Host { pid: 4242 }
        ));

        assert!(classify_one("bash", "bash -c ls", "", 1).is_none());
    }
}
