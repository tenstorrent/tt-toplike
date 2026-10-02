// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The 2-second process scan, run on a background thread.
//!
//! # Why this exists
//!
//! The TUI refreshes its process panel, the `/proc` TT attribution and the
//! serving probes every 2 seconds. Until 0.13.8 that work ran inline on the
//! render and input thread. Measured on a box with 548 processes and 2,909
//! threads (release build), one refresh cost about 135 ms:
//!
//! - `HostProcessMonitor::update` (sysinfo, every process field): ~30 ms
//! - `ProcessMonitor::update` (the `/proc` fd and hugepage scan): ~82 ms
//! - building the rows, runtimes and inference-server lists: ~22 ms
//! - `InferenceServerProbe::update`: network probes with a 150 ms timeout
//!   each, so a slow server adds to the stall
//!
//! That is about eight dropped frames at 60 fps every 2 seconds. The user saw
//! it as a visible hitch in the animation.
//!
//! # Design
//!
//! - [`Scanner`] owns the three monitors and does exactly what the inline
//!   block did, in the same order, with the same `cfg` variants.
//! - [`ScannerHandle`] owns one worker thread that runs the scanner.
//!   [`ScannerHandle::request`] and [`ScannerHandle::try_result`] never block.
//!   At most one request is outstanding at a time, so scans never queue up
//!   behind a slow one.
//! - The loop calls [`apply_scan_result`] when a result arrives. It swaps in
//!   the new rows and hands back the lists the loop still has to feed to the
//!   reset scan, the liveness prober and the inference monitor. Those steps
//!   stay on the loop thread because they touch loop state.
//!
//! The cost of the move is one scan of latency: a result appears about one
//! scan duration (~0.15 s) after the loop asks for it.
//!
//! The worker never touches the terminal or any loop state. A panic inside a
//! scan is caught: that request yields no result, a warning is logged, and the
//! worker keeps serving. The TUI's panic hook checks [`is_scan_thread`] so a
//! caught panic here does not tear down the terminal.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use crate::workload::InferenceServer;
use crate::workload::{DetectedRuntime, HostProcessMonitor, ProcRow};
#[cfg(all(target_os = "linux", feature = "linux-procfs"))]
use crate::workload::{InferenceServerProbe, ProcessMonitor, ServingMetrics};

use super::PROC_PANEL_MAX_ROWS;

/// Name given to the worker thread. The TUI panic hook uses it (through
/// [`is_scan_thread`]) to tell a caught scan panic from a fatal one.
pub(crate) const SCAN_THREAD_NAME: &str = "tt-toplike-proc-scan";

/// How long dropping a [`ScannerHandle`] waits for the worker to finish its
/// current scan. After that the thread is detached and the UI exits anyway.
const DROP_JOIN_WAIT: Duration = Duration::from_millis(250);

/// True when the calling thread is the scan worker.
pub(crate) fn is_scan_thread() -> bool {
    std::thread::current().name() == Some(SCAN_THREAD_NAME)
}

/// What the loop asks the scanner for. Built on the loop thread at the 2 s
/// cadence from values that are cheap to copy.
#[derive(Debug, Clone, Default)]
pub(crate) struct ScanRequest {
    /// Show only TT-attributed processes (`backend_shows_only_tt`). Only the
    /// Linux `/proc` build can filter; other builds ignore it.
    #[cfg_attr(
        not(all(target_os = "linux", feature = "linux-procfs")),
        allow(dead_code)
    )]
    pub only_tt: bool,
    /// The liveness prober's fresh verdicts (`LivenessProber::fresh_verdicts`),
    /// used to rank server runtimes in the full host row set.
    pub verdicts: HashMap<String, bool>,
    /// Take the `(pid, name, cmdline)` snapshot for the `tt-smi -r` reset scan.
    /// The loop sets this from `ResetBehavior::detects()`, so `ignore` never
    /// reads the process list, as before the move.
    pub want_processes: bool,
}

/// Everything one scan produced. Owned values only, so it can cross threads.
#[derive(Debug, Default)]
pub(crate) struct ScanResult {
    /// The process panel rows, TT attribution already merged in on Linux.
    pub proc_rows: Vec<ProcRow>,
    /// Live serving metrics keyed by PID (Linux `/proc` builds only).
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    pub serving_metrics: HashMap<i32, ServingMetrics>,
    /// `(pid, name, cmdline)` for every real process, for the reset scan.
    /// Empty unless the request set `want_processes`.
    pub processes: Vec<(i32, String, String)>,
    /// Inference runtimes for the liveness prober.
    pub runtimes: Vec<DetectedRuntime>,
    /// TT inference servers for the inference monitor (Linux only, as before).
    #[cfg(target_os = "linux")]
    pub inference_servers: Vec<InferenceServer>,
    /// Wall time the scan took on the worker.
    pub took: Duration,
}

/// Something that can run one scan. [`Scanner`] is the real one; tests inject
/// a slow or panicking fake.
pub(crate) trait Scan: Send + 'static {
    fn scan(&mut self, req: &ScanRequest) -> ScanResult;
}

/// The real scanner: owns the host process monitor and, on Linux with
/// `linux-procfs`, the `/proc` TT attribution monitor and the serving probe.
pub(crate) struct Scanner {
    host: HostProcessMonitor,
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    procs: ProcessMonitor,
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    probe: InferenceServerProbe,
}

impl Scanner {
    pub fn new() -> Self {
        Scanner {
            host: HostProcessMonitor::new(),
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            procs: ProcessMonitor::new(),
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            probe: InferenceServerProbe::new(),
        }
    }
}

impl Scan for Scanner {
    /// One full refresh, in the order the inline loop block used:
    /// refresh sysinfo, take the reset snapshot, list runtimes and inference
    /// servers, then build the rows. On Linux with `linux-procfs` the rows
    /// come after the `/proc` scan and the serving probes, are TT-filtered
    /// when `only_tt`, and get TT attribution merged in. Other builds always
    /// use the full host row set and have no serving metrics.
    fn scan(&mut self, req: &ScanRequest) -> ScanResult {
        let start = Instant::now();
        self.host.update();

        let processes = if req.want_processes {
            self.host.processes_snapshot()
        } else {
            Vec::new()
        };
        let runtimes = self.host.detected_runtimes();
        #[cfg(target_os = "linux")]
        let inference_servers = self.host.detected_inference_servers();

        // Non-Linux / no-procfs: TT filtering needs the /proc device-fd
        // attribution, which these builds do not have.
        #[cfg(not(all(target_os = "linux", feature = "linux-procfs")))]
        let proc_rows = self.host.rows(PROC_PANEL_MAX_ROWS, &req.verdicts);

        #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
        let (proc_rows, serving_metrics) = {
            self.procs.update();
            let flat = super::flat_process_list(&self.procs);
            let serving_metrics = self.probe.update(&flat);
            let mut rows = if req.only_tt {
                let tt_pids: std::collections::HashSet<i32> = flat.iter().map(|p| p.pid).collect();
                self.host.tt_rows(PROC_PANEL_MAX_ROWS, &tt_pids)
            } else {
                self.host.rows(PROC_PANEL_MAX_ROWS, &req.verdicts)
            };
            super::enrich_proc_rows_tt(&mut rows, &self.procs);
            (rows, serving_metrics)
        };

        ScanResult {
            proc_rows,
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            serving_metrics,
            processes,
            runtimes,
            #[cfg(target_os = "linux")]
            inference_servers,
            took: start.elapsed(),
        }
    }
}

/// Owns the scan worker thread.
///
/// `request` and `try_result` never block. At most one request is in flight:
/// a `request` made while one is outstanding is refused and nothing is
/// queued. The outstanding flag clears when `try_result` takes the reply,
/// which is a result or, after a caught panic, nothing.
pub(crate) struct ScannerHandle {
    /// Requests to the worker. `None` once dropped, which ends the worker.
    tx: Option<Sender<ScanRequest>>,
    /// Replies from the worker: `Some(result)`, or `None` when the scan
    /// panicked. Exactly one reply per request.
    rx: Receiver<Option<ScanResult>>,
    /// True from an accepted `request` until its reply is taken.
    outstanding: bool,
    thread: Option<JoinHandle<()>>,
}

impl ScannerHandle {
    /// Move `scanner` onto a new worker thread.
    pub fn spawn<S: Scan>(mut scanner: S) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ScanRequest>();
        let (res_tx, res_rx) = mpsc::channel::<Option<ScanResult>>();
        let thread = std::thread::Builder::new()
            .name(SCAN_THREAD_NAME.to_string())
            .spawn(move || {
                // Ends when the handle drops its sender.
                while let Ok(req) = req_rx.recv() {
                    let reply =
                        std::panic::catch_unwind(AssertUnwindSafe(|| scanner.scan(&req))).ok();
                    if reply.is_none() {
                        log::warn!("process scan panicked; skipping this refresh");
                    }
                    if res_tx.send(reply).is_err() {
                        break; // the handle is gone
                    }
                }
            })
            .ok();
        if thread.is_none() {
            log::warn!(
                "could not spawn the process scan thread; the process panel will not refresh"
            );
        }
        ScannerHandle {
            tx: Some(req_tx),
            rx: res_rx,
            outstanding: false,
            thread,
        }
    }

    /// Ask for a scan. Returns `false`, and queues nothing, when a request is
    /// already outstanding or the worker is gone. Never blocks.
    pub fn request(&mut self, req: ScanRequest) -> bool {
        if self.outstanding {
            return false;
        }
        match self.tx.as_ref() {
            Some(tx) if tx.send(req).is_ok() => {
                self.outstanding = true;
                true
            }
            _ => false,
        }
    }

    /// Take the reply to the outstanding request if it has arrived. Returns
    /// `None` while the scan runs, when nothing was requested, and when the
    /// scan panicked. Never blocks.
    pub fn try_result(&mut self) -> Option<ScanResult> {
        match self.rx.try_recv() {
            Ok(reply) => {
                self.outstanding = false;
                reply
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // The worker is gone; `request` will now refuse.
                self.outstanding = false;
                None
            }
        }
    }
}

impl Drop for ScannerHandle {
    /// Close the request channel so the worker exits after its current scan,
    /// then wait for it up to [`DROP_JOIN_WAIT`]. A scan stuck past that
    /// (a slow network probe) is detached so quitting never hangs.
    fn drop(&mut self) {
        self.tx = None;
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + DROP_JOIN_WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if thread.is_finished() {
            let _ = thread.join();
        } else {
            log::warn!("process scan still running at exit; detaching its thread");
        }
    }
}

/// What the loop still has to do with a result after [`apply_scan_result`]:
/// run the reset scan over `processes`, and submit `runtimes` and
/// `inference_servers` to their monitors.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    pub processes: Vec<(i32, String, String)>,
    pub runtimes: Vec<DetectedRuntime>,
    #[cfg(target_os = "linux")]
    pub inference_servers: Vec<InferenceServer>,
}

/// Apply one scan result to the loop's display state. Replaces `proc_rows`
/// and (Linux `/proc` builds) `serving_metrics` outright, as the inline block
/// did on every refresh, and returns the parts the loop feeds onward.
/// Moves only; no copying of the process list.
pub(crate) fn apply_scan_result(
    res: ScanResult,
    proc_rows: &mut Vec<ProcRow>,
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))] serving_metrics: &mut HashMap<
        i32,
        ServingMetrics,
    >,
) -> Applied {
    *proc_rows = res.proc_rows;
    #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
    {
        *serving_metrics = res.serving_metrics;
    }
    Applied {
        processes: res.processes,
        runtimes: res.runtimes,
        #[cfg(target_os = "linux")]
        inference_servers: res.inference_servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A scan that sleeps, counts its calls, and tags its result with the
    /// call number in `took` (as milliseconds) so tests can check ordering.
    struct SlowFake {
        delay: Duration,
        calls: Arc<AtomicUsize>,
        /// Panic on these call numbers (1-based).
        panic_on: Vec<usize>,
    }

    impl Scan for SlowFake {
        fn scan(&mut self, _req: &ScanRequest) -> ScanResult {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::sleep(self.delay);
            if self.panic_on.contains(&n) {
                panic!("fake scan {n} panicked");
            }
            ScanResult {
                took: Duration::from_millis(n as u64),
                ..Default::default()
            }
        }
    }

    fn fake(delay_ms: u64) -> (SlowFake, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            SlowFake {
                delay: Duration::from_millis(delay_ms),
                calls: calls.clone(),
                panic_on: Vec::new(),
            },
            calls,
        )
    }

    /// Poll `try_result` until a reply arrives or `limit` passes. Every
    /// individual call must return quickly.
    fn wait_result(h: &mut ScannerHandle, limit: Duration) -> Option<ScanResult> {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let t = Instant::now();
            let r = h.try_result();
            assert!(t.elapsed() < Duration::from_millis(5), "try_result blocked");
            if r.is_some() {
                return r;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn request_returns_immediately_while_scan_is_slow() {
        let (f, _) = fake(300);
        let mut h = ScannerHandle::spawn(f);
        let t = Instant::now();
        assert!(h.request(ScanRequest::default()));
        assert!(t.elapsed() < Duration::from_millis(5), "request blocked");
    }

    #[test]
    fn try_result_never_blocks_during_a_scan() {
        // The calls run on a helper thread so a blocking `try_result` fails
        // this test on a timeout and cannot hang the suite.
        let (done_tx, done_rx) = mpsc::channel::<Vec<Duration>>();
        std::thread::spawn(move || {
            let (f, _) = fake(300);
            let mut h = ScannerHandle::spawn(f);
            let mut took = Vec::new();
            let t = Instant::now();
            assert!(h.try_result().is_none(), "nothing requested yet");
            took.push(t.elapsed());
            h.request(ScanRequest::default());
            for _ in 0..10 {
                let t = Instant::now();
                assert!(h.try_result().is_none(), "the scan is still running");
                took.push(t.elapsed());
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = done_tx.send(took);
            std::mem::forget(h); // keep a blocked worker out of the timing
        });
        let took = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("try_result blocked");
        for t in took {
            assert!(t < Duration::from_millis(5), "try_result took {t:?}");
        }
    }

    #[test]
    fn second_request_while_outstanding_is_refused_and_not_queued() {
        let (f, calls) = fake(150);
        let mut h = ScannerHandle::spawn(f);
        assert!(h.request(ScanRequest::default()));
        assert!(!h.request(ScanRequest::default()));
        assert!(!h.request(ScanRequest::default()));
        let r = wait_result(&mut h, Duration::from_secs(2)).expect("first result");
        assert_eq!(r.took, Duration::from_millis(1));
        // Give a wrongly queued second scan time to run, then check none did.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(h.try_result().is_none(), "no second result exists");
    }

    #[test]
    fn results_arrive_in_order_exactly_once_and_a_new_request_is_accepted() {
        let (f, calls) = fake(20);
        let mut h = ScannerHandle::spawn(f);
        for n in 1..=3u64 {
            assert!(h.request(ScanRequest::default()), "request {n} accepted");
            let r = wait_result(&mut h, Duration::from_secs(2)).expect("result");
            assert_eq!(r.took, Duration::from_millis(n));
            assert!(h.try_result().is_none(), "result {n} delivered once");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn drop_mid_scan_returns_promptly() {
        let (f, _) = fake(3_000);
        let mut h = ScannerHandle::spawn(f);
        h.request(ScanRequest::default());
        std::thread::sleep(Duration::from_millis(20)); // the scan is running
        let t = Instant::now();
        drop(h);
        assert!(t.elapsed() < Duration::from_secs(1), "drop hung");
    }

    #[test]
    fn panicking_scan_yields_nothing_and_the_worker_keeps_serving() {
        let calls = Arc::new(AtomicUsize::new(0));
        let f = SlowFake {
            delay: Duration::from_millis(10),
            calls: calls.clone(),
            panic_on: vec![1],
        };
        let mut h = ScannerHandle::spawn(f);
        assert!(h.request(ScanRequest::default()));
        // Wait for the panic reply to be taken: try_result returns None but
        // clears the outstanding flag, so a new request is accepted.
        let deadline = Instant::now() + Duration::from_secs(2);
        while calls.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(h.try_result().is_none(), "a panicked scan yields no result");
        assert!(
            h.request(ScanRequest::default()),
            "accepted after the panic"
        );
        let r = wait_result(&mut h, Duration::from_secs(2)).expect("worker still serves");
        assert_eq!(r.took, Duration::from_millis(2));
        assert!(h.request(ScanRequest::default()));
        assert!(wait_result(&mut h, Duration::from_secs(2)).is_some());
    }

    #[test]
    fn worker_thread_is_named_for_the_panic_hook() {
        struct NameProbe(Arc<std::sync::Mutex<Option<bool>>>);
        impl Scan for NameProbe {
            fn scan(&mut self, _req: &ScanRequest) -> ScanResult {
                *self.0.lock().unwrap() = Some(is_scan_thread());
                ScanResult::default()
            }
        }
        let seen = Arc::new(std::sync::Mutex::new(None));
        let mut h = ScannerHandle::spawn(NameProbe(seen.clone()));
        h.request(ScanRequest::default());
        wait_result(&mut h, Duration::from_secs(2)).expect("result");
        assert_eq!(*seen.lock().unwrap(), Some(true));
        assert!(!is_scan_thread(), "the test thread is not the worker");
    }

    fn row(pid: i32) -> ProcRow {
        ProcRow {
            pid,
            name: format!("p{pid}"),
            cpu_pct: 0.0,
            mem_bytes: 0,
            inference: None,
            active: false,
            tt: None,
        }
    }

    #[test]
    fn apply_replaces_rows_and_returns_what_to_feed_onward() {
        let mut proc_rows = vec![row(1), row(2)];
        #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
        let mut serving: HashMap<i32, ServingMetrics> = HashMap::new();
        let res = ScanResult {
            proc_rows: vec![row(7)],
            processes: vec![(7, "tt-smi".into(), "tt-smi -r 0".into())],
            runtimes: vec![DetectedRuntime {
                label: "vllm".into(),
                pid: 7,
                cmdline: "vllm serve".into(),
            }],
            ..Default::default()
        };
        let applied = apply_scan_result(
            res,
            &mut proc_rows,
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            &mut serving,
        );
        assert_eq!(proc_rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![7]);
        assert_eq!(applied.processes.len(), 1);
        assert_eq!(applied.processes[0].0, 7);
        assert_eq!(applied.runtimes.len(), 1);
        assert_eq!(applied.runtimes[0].label, "vllm");
    }

    #[test]
    fn apply_of_an_empty_result_empties_rows_and_hands_back_nothing() {
        // The inline block replaced the rows on every refresh, so an empty
        // scan (no processes visible) empties the panel. It must not invent
        // processes for the reset scan or targets for the monitors.
        let mut proc_rows = vec![row(1)];
        #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
        let mut serving: HashMap<i32, ServingMetrics> = HashMap::new();
        let applied = apply_scan_result(
            ScanResult::default(),
            &mut proc_rows,
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            &mut serving,
        );
        assert!(proc_rows.is_empty());
        assert!(applied.processes.is_empty());
        assert!(applied.runtimes.is_empty());
        #[cfg(target_os = "linux")]
        assert!(applied.inference_servers.is_empty());
        #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
        assert!(serving.is_empty());
    }

    #[test]
    fn real_scanner_reads_this_machine_twice_without_panicking() {
        let mut s = Scanner::new();
        let req = ScanRequest {
            only_tt: false,
            verdicts: HashMap::new(),
            want_processes: true,
        };
        for _ in 0..2 {
            let r = s.scan(&req);
            assert!(!r.processes.is_empty(), "this test process at least");
            assert!(
                !r.proc_rows.is_empty(),
                "the full host row set is non-empty"
            );
            assert!(r.took < Duration::from_secs(10), "took {:?}", r.took);
        }
    }

    /// The body of `run_app` in `mod.rs`, the render/input loop.
    fn run_app_source() -> &'static str {
        let src = include_str!("mod.rs");
        let start = src.find("\nfn run_app(").expect("run_app exists");
        let len = src[start + 1..].find("\n}\n").expect("run_app ends");
        &src[start..start + 1 + len]
    }

    /// Guard at the wiring layer: the loop must reach the monitors only
    /// through the scanner. A monitor constructed or refreshed inside
    /// `run_app` would put the ~135 ms scan back on the render thread.
    #[test]
    fn render_loop_reaches_the_monitors_only_through_the_handle() {
        let body = run_app_source();
        for banned in [
            "HostProcessMonitor::new",
            "ProcessMonitor::new",
            "InferenceServerProbe::new",
            "host_proc_monitor",
            "process_monitor.update",
            "inference_probe",
        ] {
            assert!(!body.contains(banned), "run_app mentions `{banned}`");
        }
        // One start-up scan by a single scanner, which then moves to the
        // worker. Any other `.scan(` call in the loop runs on this thread.
        assert_eq!(body.matches(".scan(").count(), 1, "one start-up scan");
        assert_eq!(
            body.matches("proc_scan::Scanner::new()").count(),
            1,
            "one scanner"
        );
        assert!(body.contains("ScannerHandle::spawn(scanner)"));
        assert!(body.contains("scan_handle.try_result()"));
        assert!(body.contains("scan_handle.request("));
        assert!(body.contains("want_processes: reset_behavior.detects()"));
    }

    #[test]
    fn real_scanner_skips_the_reset_snapshot_when_not_wanted() {
        let mut s = Scanner::new();
        let r = s.scan(&ScanRequest::default());
        assert!(r.processes.is_empty());
    }
}
