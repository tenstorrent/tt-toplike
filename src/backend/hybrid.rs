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
use std::collections::HashMap;
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

    /// The render thread's current view of SMBUS data.
    ///
    /// Updated via `Arc::clone()` — one atomic ref-count increment, zero heap
    /// allocations — so the render loop is never stalled by SMBUS refreshes.
    smbus_latest: Arc<HashMap<usize, SmbusTelemetry>>,

    /// Shared slot where the background thread deposits fresh SMBUS snapshots.
    ///
    /// The background thread builds a new `Arc<HashMap>` from scratch, then
    /// takes the lock, swaps the pointer, and releases the lock in ≤1µs.
    /// The render thread takes the lock only to `Arc::clone()` the pointer.
    smbus_shared: Arc<Mutex<Arc<HashMap<usize, SmbusTelemetry>>>>,

    /// Incremented by the background thread after each successful tt-smi refresh.
    /// The render thread compares this against `smbus_snapshot_generation` before
    /// paying even the cheap lock cost — ensures we skip it on most frames.
    smbus_generation: Arc<AtomicU64>,

    /// The generation reflected in `smbus_latest`. When this differs from
    /// `smbus_generation`, `update()` adopts the new Arc from `smbus_shared`.
    smbus_snapshot_generation: u64,

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
            smbus_latest: Arc::clone(&empty),
            smbus_shared: Arc::new(Mutex::new(empty)),
            smbus_generation: Arc::new(AtomicU64::new(0)),
            smbus_snapshot_generation: 0,
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
        // render frame. Run tt-smi once synchronously, but bound it to
        // INIT_TT_SMI_TIMEOUT_SECS so a slow binary doesn't delay the TUI.
        //
        // Thread + channel is used here ONLY at startup (once). The background
        // refresh thread calls fetch_smbus_snapshot() as a plain blocking call
        // since it is already off the render thread.
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
            // Still start the background thread — tt-smi may become available later.
        } else {
            log::info!(
                "HybridBackend: SMBUS data loaded for {} device(s)",
                initial.len()
            );
            // Wrap the initial data in an Arc and install it into both the shared
            // slot and the render thread's local handle in one step.
            let arc = Arc::new(initial);
            self.smbus_latest = Arc::clone(&arc);
            *self.smbus_shared.lock().unwrap() = arc;
            self.smbus_generation.store(1, Ordering::Release);
            self.smbus_snapshot_generation = 1;
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
                        log::debug!("HybridBackend: background refresh got no data (tt-smi unavailable?)");
                        continue;
                    }

                    // Wrap the fresh map in a new Arc (one allocation), swap the
                    // shared pointer, then release the lock immediately.
                    // The old Arc is dropped here, deallocating the previous map
                    // on the background thread rather than the render thread.
                    {
                        let new_arc = Arc::new(data);
                        let mut slot = smbus_shared.lock().unwrap();
                        let old = std::mem::replace(&mut *slot, new_arc);
                        drop(slot); // release lock before bumping generation
                        drop(old);  // drop old map on background thread
                        log::debug!("HybridBackend: SMBUS cache refreshed");
                    }

                    // Bump generation *after* releasing the lock so the render
                    // thread knows new data is ready and can re-acquire cheaply.
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

        // Check generation counter first (cheap atomic load, no lock).
        // If the background thread hasn't produced new data since our last
        // snapshot, skip entirely — this is the common case on every frame
        // between 5-second tt-smi refreshes.
        let current_gen = self.smbus_generation.load(Ordering::Acquire);
        if current_gen != self.smbus_snapshot_generation {
            // New data available — try to grab the lock.  try_lock() is
            // non-blocking: if the background thread is currently swapping
            // (should take <1µs), we defer until next frame.
            if let Ok(slot) = self.smbus_shared.try_lock() {
                // Arc::clone() is one atomic ref-count increment — no heap
                // allocations, no String copies.  The old Arc is dropped here
                // on the render thread, but it's just a ref-count decrement
                // (the actual HashMap data lives until the last Arc holder drops).
                self.smbus_latest = Arc::clone(&*slot);
                self.smbus_snapshot_generation = current_gen;
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
        self.smbus_latest.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        let n = self.sysfs.device_count();
        if self.smbus_latest.is_empty() {
            format!("Hybrid ({} via sysfs, no tt-smi)", n)
        } else {
            format!("Hybrid ({} via sysfs+json)", n)
        }
    }
}

impl Drop for HybridBackend {
    fn drop(&mut self) {
        // Signal the background thread to stop, then join it so we don't leak.
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.refresh_handle.lock() {
            if let Some(handle) = guard.take() {
                // Allow up to one extra sleep cycle for the thread to notice the flag.
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
        assert!(backend.smbus_latest.is_empty());
        assert_eq!(backend.json_refresh_interval, Duration::from_secs(DEFAULT_JSON_REFRESH_SECS));
    }

    #[test]
    fn test_hybrid_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = HybridBackend::with_config("tt-smi", config);
        assert_eq!(backend.tt_smi_path, "tt-smi");
    }
}
