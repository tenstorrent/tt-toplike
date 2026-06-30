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

    /// The inference *server* runtimes detected in the current snapshot, resolved
    /// to `(label, port)` probe targets (deduped by label). Feed these to a
    /// [`crate::workload::LivenessProber`] each refresh — confirm-only, so we only
    /// ever probe a port belonging to a process we actually saw.
    pub fn detected_runtime_targets(&self) -> Vec<crate::workload::ProbeTarget> {
        use crate::workload::liveness_probe::resolve_target;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for p in self.sys.processes().values() {
            let name = p.name().to_string_lossy().to_string();
            let cmdline = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(label) = inference_match(&name, &cmdline) {
                if seen.insert(label) {
                    if let Some(target) = resolve_target(label, &cmdline) {
                        out.push(target);
                    }
                }
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

        let all: Vec<ProcRow> = raw
            .into_iter()
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
            .collect();
        select_rows(all, max)
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
}
