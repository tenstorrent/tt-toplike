// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Cross-platform process collector for the Insights screen.
//!
//! Enumerates host processes via `sysinfo` and tags likely ML/inference
//! runtimes (see `inference_match`). Produces gate-free `ProcRow`s that the
//! Linux/TT path may enrich by PID with device-fd attribution.

use crate::workload::inference_match;
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
    pub inference: Option<&'static str>,
    pub tt: Option<TtProcInfo>,
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

    /// Build selected rows: all inference-matched processes (never dropped),
    /// plus the busiest non-matched processes filling the remaining slots up to `max` total.
    pub fn rows(&self, max: usize) -> Vec<ProcRow> {
        let all: Vec<ProcRow> = self
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
                ProcRow {
                    pid: i32::try_from(pid.as_u32()).unwrap_or(i32::MAX), // PIDs never exceed i32::MAX on supported OSes; clamp defensively.
                    inference: inference_match(&name, &cmdline),
                    name,
                    cpu_pct: p.cpu_usage(),
                    mem_bytes: p.memory(),
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

/// Pure selection: all inference-matched rows first (CPU desc), then the
/// highest-CPU non-matched rows fill remaining slots up to `max`.
/// Matched rows are never dropped regardless of `max`.
pub(crate) fn select_rows(mut all: Vec<ProcRow>, max: usize) -> Vec<ProcRow> {
    // Sort entire list by cpu desc so both partitions are already ordered.
    all.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let (mut matched, non_matched): (Vec<ProcRow>, Vec<ProcRow>) =
        all.into_iter().partition(|r| r.inference.is_some());
    // Fill remaining slots (after matched rows) with the busiest non-matched.
    let remaining = max.saturating_sub(matched.len());
    matched.extend(non_matched.into_iter().take(remaining));
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i32, cpu: f32, inf: Option<&'static str>) -> ProcRow {
        ProcRow {
            pid,
            name: format!("p{pid}"),
            cpu_pct: cpu,
            mem_bytes: 0,
            inference: inf,
            tt: None,
        }
    }

    #[test]
    fn matched_rows_sort_first_then_by_cpu() {
        let all = vec![
            row(1, 90.0, None),
            row(2, 10.0, Some("ollama")),
            row(3, 50.0, None),
            row(4, 5.0, Some("vllm")),
        ];
        let out = select_rows(all, 3);
        // matched (2,4) first, each group by cpu desc; then top non-matched by cpu
        assert_eq!(out.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![2, 4, 1]);
    }

    #[test]
    fn matched_idle_process_still_included_over_cap() {
        // a matched process with tiny cpu must survive even when many busy procs exist
        let mut all: Vec<ProcRow> = (10..20).map(|p| row(p, 80.0, None)).collect();
        all.push(row(99, 0.1, Some("mlx")));
        let out = select_rows(all, 3);
        assert!(
            out.iter().any(|r| r.pid == 99),
            "matched process must be kept"
        );
    }
}
