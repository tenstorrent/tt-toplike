//! Hybrid backend: sysfs real-time metrics + background tt-smi JSON enrichment
//!
//! This backend combines the strengths of the Sysfs and JSON backends:
//!
//! - **Sysfs** provides fast, non-invasive real-time reads (temperature, power,
//!   voltage, current) via Linux hwmon. These run on every `update()` call and
//!   complete in microseconds, keeping the render loop smooth.
//!
//! - **tt-smi JSON** provides rich SMBUS telemetry (DDR status, ARC health,
//!   board IDs, firmware versions). A background thread refreshes this data
//!   every 5 seconds, so `tt-smi -s` startup overhead never blocks rendering.
//!
//! ## Why this matters
//!
//! Running `tt-smi -s` on every 100ms frame adds 50–500ms of latency per
//! frame with tt-smi 4.0+, making `--backend json` unusable at interactive
//! refresh rates. The hybrid backend solves this by decoupling the two data
//! sources: fast sysfs for the hot path, slow JSON for enrichment.
//!
//! ## Zero-allocation render path
//!
//! The SMBUS snapshot is stored as `Arc<HashMap<...>>`. When the background
//! thread produces fresh data it wraps the new map in a new Arc and swaps the
//! shared pointer. The render thread adopts the new snapshot with a single
//! `Arc::clone()` — one atomic increment, zero heap allocations — instead of
//! deep-cloning hundreds of `Option<String>` fields on the render thread.
//!
//! ## Gradual adoption (continuous blending)
//!
//! When a fresh SMBUS snapshot arrives, rather than applying all device data
//! on the same frame (causing a sudden visual "surge"), the render thread
//! stagger-adopts one device per `update()` call into `smbus_blended`.
//! For N devices at 100 ms/frame the visual transition spreads over N×100 ms,
//! giving a smooth rolling-update feel instead of a simultaneous pop.
//!
//! The `smbus_blended` HashMap is what callers see via `smbus_telemetry()`.
//! `smbus_latest` is the background-thread deposit slot; `smbus_blended` is
//! the render-side working copy that trails it by at most N frames.
//!
//! ## Degraded mode
//!
//! If tt-smi is absent or fails, the backend runs in sysfs-only mode. All
//! core telemetry still works; SMBUS data (DDR status, board IDs) is simply
//! absent (returns `None`). This is identical to using `--backend sysfs`.

use crate::backend::sysfs::SysfsBackend;
use crate::backend::{BackendConfig, TelemetryBackend};
use crate::backend::json;
use crate::error::{BackendError, BackendResult};
use crate::models::{Device, SmbusTelemetry, Telemetry};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How often the background thread runs tt-smi to refresh SMBUS data.
/// 5 seconds is fast enough to catch board reboots and DDR retraining events.
const DEFAULT_JSON_REFRESH_SECS: u64 = 5;

/// Hybrid backend combining sysfs real-time + background JSON enrichment
pub struct HybridBackend {
    /// Primary real-time data source — never blocks more than a few µs
    sysfs: SysfsBackend,

    /// Path to tt-smi executable (searched in PATH if bare name)
    tt_smi_path: String,

    /// Background-thread deposit slot: fresh SMBUS snapshot lives here.
    ///
    /// The background thread builds a new `Arc<HashMap>` from scratch, then
    /// takes the lock, swaps the pointer, and releases the lock in ≤1µs.
    /// The render thread takes the lock only to `Arc::clone()` the pointer.
    smbus_shared: Arc<Mutex<Arc<HashMap<usize, SmbusTelemetry>>>>,

    /// The render thread's private view of the latest background snapshot.
    /// Updated via `Arc::clone()` — one atomic ref-count increment, zero
    /// heap allocations — when the generation counter indicates new data.
    smbus_latest: Arc<HashMap<usize, SmbusTelemetry>>,

    /// Incremented by the background thread after each successful tt-smi
    /// refresh.  The render thread compares against `smbus_snapshot_gen`
    /// before paying even the cheap lock cost.
    smbus_generation: Arc<AtomicU64>,

    /// The generation reflected in `smbus_latest`.
    smbus_snapshot_gen: u64,

    /// What `smbus_telemetry()` actually returns.
    ///
    /// Populated gradually from `smbus_latest`: one device per `update()`
    /// call, in device-index order.  This spreads visual changes across N
    /// frames (N = device count) instead of popping all at once.
    smbus_blended: HashMap<usize, SmbusTelemetry>,

    /// Device indices waiting to be blended from `smbus_latest` into
    /// `smbus_blended`.  Filled when a new snapshot is adopted, drained at
    /// one entry per `update()` call.
    smbus_adopt_queue: VecDeque<usize>,

    /// Tells the background thread to stop cleanly.
    stop_flag: Arc<AtomicBool>,

    /// Handle to the background thread.
    /// Wrapped in Mutex so HybridBackend implements Sync (JoinHandle is !Sync).
    refresh_handle: Mutex<Option<thread::JoinHandle<()>>>,

    /// SMBUS refresh interval. Exposed here so tests can override it.
    json_refresh_interval: Duration,
}

impl HybridBackend {
    /// Create a new Hybrid backend using the given tt-smi path.
    pub fn new(tt_smi_path: impl Into<String>) -> Self {
        Self::with_config(tt_smi_path, BackendConfig::default())
    }

    /// Create a new Hybrid backend with explicit configuration.
    pub fn with_config(tt_smi_path: impl Into<String>, _config: BackendConfig) -> Self {
        let empty: Arc<HashMap<usize, SmbusTelemetry>> = Arc::new(HashMap::new());
        Self {
            sysfs: SysfsBackend::new(),
            tt_smi_path: tt_smi_path.into(),
            smbus_shared: Arc::new(Mutex::new(Arc::clone(&empty))),
            smbus_latest: Arc::clone(&empty),
            smbus_generation: Arc::new(AtomicU64::new(0)),
            smbus_snapshot_gen: 0,
            smbus_blended: HashMap::new(),
            smbus_adopt_queue: VecDeque::new(),
            stop_flag: Arc::new(AtomicBool::new(false)),
            refresh_handle: Mutex::new(None),
            json_refresh_interval: Duration::from_secs(DEFAULT_JSON_REFRESH_SECS),
        }
    }

    /// Probe whether the tt-smi binary is reachable without spawning a full run.
    #[allow(dead_code)]
    fn probe_tt_smi(tt_smi_path: &str) -> bool {
        std::process::Command::new(tt_smi_path)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }
}

impl TelemetryBackend for HybridBackend {
    fn init(&mut self) -> BackendResult<()> {
        // ── 1. Primary device detection via sysfs ──────────────────────────────
        self.sysfs.init().map_err(|e| {
            BackendError::Initialization(format!("HybridBackend: sysfs init failed: {}", e))
        })?;

        log::info!(
            "HybridBackend: sysfs OK ({} devices)",
            self.sysfs.device_count()
        );

        // ── 2. Best-effort initial SMBUS load (with startup timeout) ─────────
        //
        // We want SMBUS data (board IDs, DDR status) available on the very first
        // render frame.  Run tt-smi once synchronously, but bound it to
        // INIT_TT_SMI_TIMEOUT_SECS so a slow binary doesn't delay the TUI.
        let initial = {
            use std::sync::mpsc;
            const INIT_TT_SMI_TIMEOUT_SECS: u64 = 3;
            let (tx, rx) = mpsc::channel();
            let path = self.tt_smi_path.clone();
            thread::spawn(move || { let _ = tx.send(json::fetch_smbus_snapshot(&path)); });
            rx.recv_timeout(Duration::from_secs(INIT_TT_SMI_TIMEOUT_SECS))
                .unwrap_or_default()
        };

        if initial.is_empty() {
            log::warn!(
                "HybridBackend: tt-smi produced no SMBUS data on startup — \
                 running in sysfs-only mode (no DDR status, no board IDs). \
                 Background thread will retry every {}s.",
                DEFAULT_JSON_REFRESH_SECS
            );
        } else {
            log::info!(
                "HybridBackend: SMBUS data loaded for {} device(s)",
                initial.len()
            );
            // Populate smbus_blended directly at init — no staggering needed
            // for startup because nothing is animating yet.
            self.smbus_blended.clone_from(&initial);
            let arc = Arc::new(initial);
            self.smbus_latest = Arc::clone(&arc);
            *self.smbus_shared.lock().unwrap() = arc;
            self.smbus_generation.store(1, Ordering::Release);
            self.smbus_snapshot_gen = 1;
        }

        // ── 3. Start background refresh thread ────────────────────────────────
        let smbus_shared = Arc::clone(&self.smbus_shared);
        let smbus_generation = Arc::clone(&self.smbus_generation);
        let stop_flag = Arc::clone(&self.stop_flag);
        let tt_smi_path = self.tt_smi_path.clone();
        let interval = self.json_refresh_interval;

        let handle = thread::Builder::new()
            .name("hybrid-json-refresh".to_string())
            .spawn(move || {
                log::debug!("HybridBackend: background refresh thread started");
                while !stop_flag.load(Ordering::Relaxed) {
                    // Sleep first, then fetch — keeps startup fast.
                    thread::sleep(interval);
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }

                    // Blocking call — safe because we're on a background thread,
                    // never on the render loop.
                    let data = json::fetch_smbus_snapshot(&tt_smi_path);
                    if data.is_empty() {
                        log::debug!("HybridBackend: background refresh got no data");
                        continue;
                    }

                    // Wrap the fresh map in a new Arc (one allocation), swap the
                    // shared pointer, then release the lock immediately.
                    // The old Arc is dropped here on the background thread rather
                    // than the render thread.
                    {
                        let new_arc = Arc::new(data);
                        let mut slot = smbus_shared.lock().unwrap();
                        let old = std::mem::replace(&mut *slot, new_arc);
                        drop(slot);
                        drop(old);
                        log::debug!("HybridBackend: SMBUS cache refreshed");
                    }

                    // Bump generation *after* releasing the lock so the render
                    // thread can re-acquire cheaply.
                    smbus_generation.fetch_add(1, Ordering::Release);
                }
                log::debug!("HybridBackend: background refresh thread exiting");
            })
            .expect("failed to spawn hybrid-json-refresh thread");

        *self.refresh_handle.lock().unwrap() = Some(handle);
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        // Fast path: sysfs only — completes in microseconds.
        self.sysfs.update()?;

        // ── Step 1: check if background thread has new data ───────────────────
        //
        // The generation counter is a cheap atomic load (no lock).  Skip
        // entirely on most frames — new SMBUS data arrives only every 5 s.
        let current_gen = self.smbus_generation.load(Ordering::Acquire);
        if current_gen != self.smbus_snapshot_gen {
            if let Ok(slot) = self.smbus_shared.try_lock() {
                // Arc::clone = one atomic ref-count increment, zero heap allocs.
                self.smbus_latest = Arc::clone(&*slot);
                self.smbus_snapshot_gen = current_gen;

                // Queue all devices for gradual adoption into smbus_blended.
                // We sort by device index so the update rolls through devices
                // in a consistent, deterministic order.
                self.smbus_adopt_queue.clear();
                let mut keys: Vec<usize> = self.smbus_latest.keys().copied().collect();
                keys.sort_unstable();
                self.smbus_adopt_queue.extend(keys);
                log::debug!(
                    "HybridBackend: queued {} device(s) for gradual SMBUS adoption",
                    self.smbus_adopt_queue.len()
                );
            }
            // If try_lock() fails the background thread is mid-swap (<1 µs);
            // defer until next frame.
        }

        // ── Step 2: adopt one device per frame from smbus_latest ─────────────
        //
        // Processing one entry per update() spreads the visual change across
        // N frames (N = device count).  At 100 ms/frame, 4 devices = 400 ms
        // of rolling transition instead of a single-frame surge.
        if let Some(device_idx) = self.smbus_adopt_queue.pop_front() {
            if let Some(smbus) = self.smbus_latest.get(&device_idx) {
                self.smbus_blended.insert(device_idx, smbus.clone());
            }
        }

        Ok(())
    }

    fn devices(&self) -> &[Device] {
        self.sysfs.devices()
    }

    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry> {
        self.sysfs.telemetry(device_idx)
    }

    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        self.smbus_blended.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        let n = self.sysfs.device_count();
        if self.smbus_blended.is_empty() {
            format!("Hybrid ({} via sysfs, no tt-smi)", n)
        } else {
            format!("Hybrid ({} via sysfs+json)", n)
        }
    }
}

impl Drop for HybridBackend {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.refresh_handle.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hybrid_backend_creation() {
        let backend = HybridBackend::new("tt-smi");
        assert_eq!(backend.tt_smi_path, "tt-smi");
        assert!(backend.smbus_blended.is_empty());
        assert_eq!(backend.json_refresh_interval, Duration::from_secs(DEFAULT_JSON_REFRESH_SECS));
    }

    #[test]
    fn test_hybrid_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = HybridBackend::with_config("tt-smi", config);
        assert_eq!(backend.tt_smi_path, "tt-smi");
    }

    #[test]
    fn test_gradual_adoption_one_per_frame() {
        let mut backend = HybridBackend::new("tt-smi");

        // Simulate background thread depositing data for 4 devices.
        let mut fresh: HashMap<usize, SmbusTelemetry> = HashMap::new();
        for i in 0..4 {
            fresh.insert(i, SmbusTelemetry::default());
        }
        {
            let arc = Arc::new(fresh);
            *backend.smbus_shared.lock().unwrap() = Arc::clone(&arc);
            backend.smbus_generation.store(1, Ordering::Release);
        }

        // First update(): should detect new gen, queue 4 devices, adopt device 0.
        // (sysfs will fail since no hardware — we test the SMBUS path directly.)
        let current_gen = backend.smbus_generation.load(Ordering::Acquire);
        assert_ne!(current_gen, backend.smbus_snapshot_gen);

        // Manually drive the adopt logic (skip sysfs).
        if let Ok(slot) = backend.smbus_shared.try_lock() {
            backend.smbus_latest = Arc::clone(&*slot);
            backend.smbus_snapshot_gen = current_gen;
            let mut keys: Vec<usize> = backend.smbus_latest.keys().copied().collect();
            keys.sort_unstable();
            backend.smbus_adopt_queue.extend(keys);
        }

        // Should have 4 devices queued, none blended yet.
        assert_eq!(backend.smbus_adopt_queue.len(), 4);
        assert!(backend.smbus_blended.is_empty());

        // Adopt one device at a time.
        for expected_blended in 1..=4 {
            if let Some(idx) = backend.smbus_adopt_queue.pop_front() {
                if let Some(s) = backend.smbus_latest.get(&idx) {
                    backend.smbus_blended.insert(idx, s.clone());
                }
            }
            assert_eq!(backend.smbus_blended.len(), expected_blended);
        }

        assert!(backend.smbus_adopt_queue.is_empty());
    }
}
