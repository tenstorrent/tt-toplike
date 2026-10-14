// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Follows a training run for a view that does not own the training monitor
//! (Arcade). It never walks `/proc` itself: a trainer is noticed in a process
//! list the background scan already collected, then followed through a
//! `TrainMonitor` that only tails its log and reads its own `/proc/<pid>` stats.

use super::detect::find_trainer;
use super::monitor::{TrainMonitor, TrainState};
use std::collections::HashMap;

pub struct TrainWatch {
    monitor: TrainMonitor,
    /// `/proc/<pid>/maps` results by pid, so a Python process is probed once.
    ttml: HashMap<i32, bool>,
}

impl TrainWatch {
    pub fn new() -> Self {
        Self {
            monitor: TrainMonitor::new(),
            ttml: HashMap::new(),
        }
    }

    /// `processes` is `Some` on the tick a fresh scan arrives and `None`
    /// between scans. Between scans the attached run is only polled.
    pub fn update(&mut self, processes: Option<&[(i32, String, String)]>) {
        self.update_with(processes, &mut TrainMonitor::maps_ttml);
    }

    pub(crate) fn update_with(
        &mut self,
        processes: Option<&[(i32, String, String)]>,
        probe: &mut dyn FnMut(i32) -> bool,
    ) {
        if let Some(list) = processes {
            let attached = self.monitor.state().proc.as_ref().map(|p| p.pid);
            match attached {
                // The run is gone from a fresh scan: drop it at once.
                Some(pid) if !list.iter().any(|(p, _, _)| *p == pid) => self.monitor.detach(),
                Some(_) => {}
                None => {
                    let cache = &mut self.ttml;
                    let found = find_trainer(list, &mut |pid| {
                        *cache.entry(pid).or_insert_with(|| probe(pid))
                    });
                    if let Some(p) = found {
                        self.monitor.attach(p);
                    }
                    let live: Vec<i32> = list.iter().map(|(p, _, _)| *p).collect();
                    self.ttml.retain(|pid, _| live.contains(pid));
                }
            }
        }
        self.monitor.poll_attached();
    }

    /// The attached run's state, or `None` when no run is attached.
    pub fn state(&self) -> Option<&TrainState> {
        let s = self.monitor.state();
        s.proc.as_ref().map(|_| s)
    }
}

impl Default for TrainWatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, name: &str, cmd: &str) -> (i32, String, String) {
        (pid, name.to_string(), cmd.to_string())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_trainer_in_the_process_list_attaches_and_its_exit_detaches() {
        let mut w = TrainWatch::new();
        assert!(w.state().is_none());
        // Use this test process's own pid so `still_alive` is true.
        let me = std::process::id() as i32;
        let procs = vec![p(me, "nano_gpt", "/x/nano_gpt -c train.yaml")];
        w.update(Some(&procs));
        assert_eq!(w.state().unwrap().proc.as_ref().unwrap().binary, "nano_gpt");
        // A later scan without it: dropped at once.
        w.update(Some(&[p(1, "bash", "bash")]));
        assert!(w.state().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn no_new_scan_keeps_the_attached_run_and_never_searches() {
        let mut w = TrainWatch::new();
        w.update(None);
        assert!(w.state().is_none());
        let me = std::process::id() as i32;
        w.update(Some(&[p(me, "nano_gpt", "/x/nano_gpt -c t.yaml")]));
        w.update(None); // between scans
        assert!(w.state().is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn maps_results_are_cached_by_pid() {
        let mut w = TrainWatch::new();
        let me = std::process::id() as i32;
        let procs = vec![p(me, "python3", "python3 /w/app.py --steps 5")];
        let mut calls = 0;
        w.update_with(Some(&procs), &mut |_| {
            calls += 1;
            false
        });
        w.update_with(Some(&procs), &mut |_| {
            calls += 1;
            false
        });
        assert_eq!(calls, 1, "the maps probe for one pid runs once");
    }

    /// The per-tick cost of following a run between scans stays under the
    /// 0.1 ms budget (the spec's render-thread limit), and noticing a trainer
    /// in a 548-process list does not touch `/proc` for non-Python processes.
    #[cfg(target_os = "linux")]
    #[test]
    fn following_a_run_costs_under_a_tenth_of_a_millisecond_per_tick() {
        let me = std::process::id() as i32;
        let mut procs: Vec<(i32, String, String)> = (1..=547)
            .map(|i| (i, "worker".to_string(), format!("/usr/bin/worker --id {i}")))
            .collect();
        procs.push((me, "nano_gpt".into(), "/x/nano_gpt -c train.yaml".into()));
        let mut w = TrainWatch::new();
        let mut probes = 0;
        w.update_with(Some(&procs), &mut |_| {
            probes += 1;
            false
        });
        assert!(w.state().is_some(), "attached");
        assert_eq!(probes, 0, "no worker is Python, so nothing is probed");
        // Best of fifteen batches: the other tests run in parallel and a busy
        // machine can stretch any one batch, but it cannot make every batch slow.
        let per_tick = (0..15)
            .map(|_| {
                let start = std::time::Instant::now();
                for _ in 0..100 {
                    w.update_with(None, &mut |_| false);
                }
                start.elapsed() / 100
            })
            .min()
            .unwrap();
        assert!(
            per_tick < std::time::Duration::from_micros(100),
            "{per_tick:?}"
        );
    }
}
