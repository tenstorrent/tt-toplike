// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Follows a training run for a view that does not own the training monitor
//! (Arcade).
//!
//! Finding the run and following it are separate jobs on separate threads:
//!
//! - [`TrainerFinder`] notices a trainer in a process list. It reads
//!   `/proc/<pid>/maps` for Python processes, so it runs on the background scan
//!   worker (see `ui::tui::proc_scan`), never on the render thread.
//! - [`TrainWatch`] lives on the loop thread. It is handed what the finder
//!   found and then follows the attached run through a `TrainMonitor`, which
//!   only tails its log and reads its own `/proc/<pid>` stats.

use super::detect::{find_trainer, TrainProcess};
use super::monitor::{TrainMonitor, TrainState};
use std::collections::HashMap;

/// A Python process that did not have `ttml` mapped is probed again after this
/// many scans (the scan runs every 2 s, so about 10 s). A harness such as
/// `python3 app.py` is often scanned before it imports `ttml`, so a negative
/// is not permanent.
pub const NEGATIVE_RETRY_SCANS: u32 = 5;

/// Notices a trainer among already-collected processes. Owns the per-pid cache
/// of `/proc/<pid>/maps` results: a positive stays while the same process
/// lives, a negative is retried every [`NEGATIVE_RETRY_SCANS`] scans. An entry
/// is for one process, not one pid number: if the pid is reused by a new
/// process (a different start time) the entry is discarded and probed afresh.
#[derive(Default)]
pub struct TrainerFinder {
    /// pid -> (ttml was mapped, scans since it was probed, start time of the
    /// process that was probed).
    probes: HashMap<i32, (bool, u32, Option<u64>)>,
}

impl TrainerFinder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The trainer in `processes`, or `None`. One call is one scan.
    pub fn find(&mut self, processes: &[(i32, String, String)]) -> Option<TrainProcess> {
        self.find_with_start(
            processes,
            &mut TrainMonitor::maps_ttml,
            &mut crate::workload::proc_access::start_time,
        )
    }

    /// [`Self::find_with_start`] for callers with no process identity: every
    /// start time is unknown, so no cache entry is ever invalidated by one.
    #[cfg(test)]
    pub(crate) fn find_with(
        &mut self,
        processes: &[(i32, String, String)],
        probe: &mut dyn FnMut(i32) -> bool,
    ) -> Option<TrainProcess> {
        self.find_with_start(processes, probe, &mut |_| None)
    }

    /// `probe` says whether `ttml` is mapped into a pid; `start` gives a pid's
    /// start time (its identity across pid reuse).
    pub(crate) fn find_with_start(
        &mut self,
        processes: &[(i32, String, String)],
        probe: &mut dyn FnMut(i32) -> bool,
        start: &mut dyn FnMut(i32) -> Option<u64>,
    ) -> Option<TrainProcess> {
        for (_, age, _) in self.probes.values_mut() {
            *age += 1;
        }
        let cache = &mut self.probes;
        let found = find_trainer(processes, &mut |pid| {
            let now = start(pid);
            // An entry made for another process that held this pid is stale.
            // Unknown on either side counts as the same process.
            let fresh = |then: &Option<u64>| then.is_none() || now.is_none() || *then == now;
            match cache.get(&pid) {
                Some((true, _, then)) if fresh(then) => true,
                Some((false, age, then)) if fresh(then) && *age < NEGATIVE_RETRY_SCANS => false,
                _ => {
                    let loaded = probe(pid);
                    cache.insert(pid, (loaded, 0, now));
                    loaded
                }
            }
        });
        let live: Vec<i32> = processes.iter().map(|(p, _, _)| *p).collect();
        self.probes.retain(|pid, _| live.contains(pid));
        found.map(|mut t| {
            t.start_time = start(t.pid);
            t
        })
    }
}

pub struct TrainWatch {
    monitor: TrainMonitor,
}

impl TrainWatch {
    pub fn new() -> Self {
        Self {
            monitor: TrainMonitor::new(),
        }
    }

    /// `found` is `Some` on the tick a fresh scan result arrives (`Some(None)`
    /// means the scan saw no trainer) and `None` between scans. Between scans
    /// the attached run is only polled. This does no `/proc` scan and reads no
    /// `maps` file.
    pub fn update(&mut self, found: Option<Option<TrainProcess>>) {
        if let Some(scan) = found {
            let attached = self
                .monitor
                .state()
                .proc
                .as_ref()
                .map(|p| (p.pid, p.start_time));
            match (attached, scan) {
                // Same run only if the pid AND the process match. A pid reused by
                // a new process has a new start time and must be re-attached, or
                // the new process's progress is shown as the old run's. Unknown
                // on either side counts as the same process.
                (Some((pid, then)), Some(p))
                    if pid == p.pid
                        && (then.is_none() || p.start_time.is_none() || then == p.start_time) => {}
                (_, Some(p)) => self.monitor.attach(p),
                (Some(_), None) => self.monitor.detach(),
                (None, None) => {}
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

    fn proc_of(pid: i32, binary: &str) -> TrainProcess {
        TrainProcess {
            pid,
            binary: binary.into(),
            config_path: None,
            start_time: None,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_found_trainer_attaches_and_a_scan_that_sees_none_detaches() {
        let mut w = TrainWatch::new();
        assert!(w.state().is_none());
        // Use this test process's own pid so `still_alive` is true.
        let me = std::process::id() as i32;
        w.update(Some(Some(proc_of(me, "nano_gpt"))));
        assert_eq!(w.state().unwrap().proc.as_ref().unwrap().binary, "nano_gpt");
        // A later scan that finds no trainer: dropped at once.
        w.update(Some(None));
        assert!(w.state().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn between_scans_the_attached_run_is_kept_and_nothing_is_searched() {
        let mut w = TrainWatch::new();
        w.update(None);
        assert!(w.state().is_none());
        let me = std::process::id() as i32;
        w.update(Some(Some(proc_of(me, "nano_gpt"))));
        w.update(None); // between scans
        assert!(w.state().is_some());
        // The same run found again does not re-attach (its state is kept).
        let first_seen = w.state().unwrap().first_seen;
        w.update(Some(Some(proc_of(me, "nano_gpt"))));
        assert_eq!(w.state().unwrap().first_seen, first_seen);
    }

    /// The maps probe runs once per pid while it is positive, and a negative is
    /// retried after `NEGATIVE_RETRY_SCANS` scans. A harness scanned before it
    /// imports `ttml` is found once it has.
    #[test]
    fn a_negative_probe_is_retried_and_a_later_positive_is_found() {
        let me = 4242;
        let procs = vec![p(me, "python3", "python3 /w/app.py --steps 5")];
        let mut f = TrainerFinder::new();
        let mut calls = 0;
        let mut loaded = false;
        // Scan 1: ttml not yet imported.
        let mut probe = |_: i32| {
            calls += 1;
            loaded
        };
        assert!(f.find_with(&procs, &mut probe).is_none());
        // Scans 2..=5: the negative is cached, no probe.
        for _ in 0..(NEGATIVE_RETRY_SCANS - 1) {
            assert!(f.find_with(&procs, &mut probe).is_none());
        }
        assert_eq!(calls, 1, "a recent negative is not probed again");
        // The script imports ttml; the retry scan finds it.
        loaded = true;
        let mut probe2 = |_: i32| {
            calls += 1;
            loaded
        };
        let found = f.find_with(&procs, &mut probe2);
        assert_eq!(found.map(|t| t.pid), Some(me), "found after the import");
        // A positive is cached for the pid's life.
        let before = calls;
        let mut never = |_: i32| -> bool { panic!("a positive must not be probed again") };
        for _ in 0..10 {
            assert!(f.find_with(&procs, &mut never).is_some());
        }
        assert_eq!(calls, before);
    }

    #[test]
    fn the_cache_forgets_pids_that_left_the_list() {
        let mut f = TrainerFinder::new();
        let procs = vec![p(77, "python3", "python3 /w/app.py")];
        f.find_with(&procs, &mut |_| false);
        assert_eq!(f.probes.len(), 1);
        f.find_with(&[p(1, "bash", "bash")], &mut |_| false);
        assert!(f.probes.is_empty());
    }

    /// Noticing a trainer in a 548-process list does not touch `/proc` for
    /// non-Python processes, and a compiled trainer needs no probe at all.
    #[test]
    fn a_big_process_list_probes_only_python_processes() {
        let mut procs: Vec<(i32, String, String)> = (1..=547)
            .map(|i| (i, "worker".to_string(), format!("/usr/bin/worker --id {i}")))
            .collect();
        procs.push(p(9000, "nano_gpt", "/x/nano_gpt -c train.yaml"));
        let mut f = TrainerFinder::new();
        let mut probes = 0;
        let found = f.find_with(&procs, &mut |_| {
            probes += 1;
            false
        });
        assert_eq!(found.map(|t| t.pid), Some(9000));
        assert_eq!(probes, 0, "no worker is Python, so nothing is probed");
    }

    /// The per-tick cost of following a run between scans stays under the
    /// 0.1 ms budget (the spec's render-thread limit).
    #[cfg(target_os = "linux")]
    #[test]
    fn following_a_run_costs_under_a_tenth_of_a_millisecond_per_tick() {
        let me = std::process::id() as i32;
        let mut w = TrainWatch::new();
        w.update(Some(Some(proc_of(me, "nano_gpt"))));
        assert!(w.state().is_some(), "attached");
        // Best of fifteen batches: the other tests run in parallel and a busy
        // machine can stretch any one batch, but it cannot make every batch slow.
        let per_tick = (0..15)
            .map(|_| {
                let start = std::time::Instant::now();
                for _ in 0..100 {
                    w.update(None);
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

    /// A pid is reused by a new process. A positive cached for the old process
    /// must not make the new one look like a trainer, and a negative cached for
    /// the old one must not delay detecting the new one.
    #[test]
    fn a_reused_pid_is_probed_afresh_not_taken_from_the_cache() {
        let pid = 5150;
        let procs = vec![p(pid, "python3", "python3 /w/app.py --steps 5")];
        let mut f = TrainerFinder::new();
        let mut probes = 0;

        // Old process (start 100) has ttml mapped: found, and the result cached.
        let old = f.find_with_start(
            &procs,
            &mut |_| {
                probes += 1;
                true
            },
            &mut |_| Some(100),
        );
        assert_eq!(old.as_ref().and_then(|t| t.start_time), Some(100));
        assert_eq!(probes, 1);
        // Same process again: served from the cache.
        f.find_with_start(&procs, &mut |_| panic!("cached"), &mut |_| Some(100));

        // The pid is reused by an unrelated Python process (start 200) without
        // ttml. The cached `true` must not be reused: it is probed and refused.
        let new = f.find_with_start(
            &procs,
            &mut |_| {
                probes += 1;
                false
            },
            &mut |_| Some(200),
        );
        assert!(new.is_none(), "the replacement is not the trainer");
        assert_eq!(probes, 2, "the replacement was probed, not read from cache");

        // And a cached negative for that process does not hide a later
        // replacement that does have ttml: a new start time is probed at once.
        let again = f.find_with_start(
            &procs,
            &mut |_| {
                probes += 1;
                true
            },
            &mut |_| Some(300),
        );
        assert_eq!(again.and_then(|t| t.start_time), Some(300));
        assert_eq!(probes, 3);
    }

    /// The attached run is the same run only if the process matches, not just
    /// the pid. A reused pid re-attaches; an unchanged one keeps its state.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_reused_pid_reattaches_instead_of_keeping_the_old_run() {
        let me = std::process::id() as i32;
        let with_start = |t: Option<u64>| TrainProcess {
            start_time: t,
            ..proc_of(me, "nano_gpt")
        };
        let mut w = TrainWatch::new();
        w.update(Some(Some(with_start(Some(100)))));
        let first = w.state().unwrap().first_seen;
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Same pid, same start time: the run is kept.
        w.update(Some(Some(with_start(Some(100)))));
        assert_eq!(w.state().unwrap().first_seen, first, "unchanged run kept");

        // Same pid, different start time: a new process, so a fresh attach.
        w.update(Some(Some(with_start(Some(200)))));
        assert_ne!(
            w.state().unwrap().first_seen,
            first,
            "a reused pid must not keep the old run's state"
        );
        assert_eq!(
            w.state().unwrap().proc.as_ref().unwrap().start_time,
            Some(200)
        );

        // An unknown start time on either side is treated as the same process,
        // so a failed read does not drop the run.
        let kept = w.state().unwrap().first_seen;
        w.update(Some(Some(with_start(None))));
        assert_eq!(w.state().unwrap().first_seen, kept);
    }
}
