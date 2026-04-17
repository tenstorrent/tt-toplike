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

    /// Latest SMBUS telemetry copied from background cache on each update().
    /// Returned by smbus_telemetry() via reference, so it must be owned.
    smbus_snapshot: HashMap<usize, SmbusTelemetry>,

    /// Background thread writes here; main thread reads via smbus_snapshot.
    smbus_cache: Arc<Mutex<HashMap<usize, SmbusTelemetry>>>,

    /// Incremented by the background thread after each successful tt-smi refresh.
    /// The main thread compares this against `smbus_snapshot_generation` before
    /// doing the expensive clone — ensures we only copy new data, not every frame.
    smbus_generation: Arc<AtomicU64>,

    /// The generation number reflected in `smbus_snapshot`. When this differs
    /// from `smbus_generation`, `update()` re-clones the cache.
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
        // BackendConfig fields like verbose/interval are used by sysfs internally.
        Self {
            sysfs: SysfsBackend::new(),
            tt_smi_path: tt_smi_path.into(),
            smbus_snapshot: HashMap::new(),
            smbus_cache: Arc::new(Mutex::new(HashMap::new())),
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
        // We spawn a thread here — but only ONCE at startup, not on every refresh
        // cycle. The background refresh thread calls fetch_smbus_snapshot() as a
        // plain blocking call because it's already off the render thread.
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
            // Copy into both the snapshot (for immediate use) and the shared cache.
            // Set generation to 1 so the first update() doesn't redundantly re-clone.
            self.smbus_snapshot = initial.clone();
            *self.smbus_cache.lock().unwrap() = initial;
            self.smbus_generation.store(1, Ordering::Release);
            self.smbus_snapshot_generation = 1;
        }

        // ── 3. Start background refresh thread ────────────────────────────────
        let smbus_cache = Arc::clone(&self.smbus_cache);
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

                    let data = json::fetch_smbus_snapshot(&tt_smi_path);
                    if data.is_empty() {
                        log::debug!("HybridBackend: background refresh got no data (tt-smi unavailable?)");
                        continue;
                    }

                    {
                        let mut cache = smbus_cache.lock().unwrap();
                        for (idx, smbus) in data {
                            cache.insert(idx, smbus);
                        }
                        log::debug!("HybridBackend: SMBUS cache refreshed ({} entries)", cache.len());
                    } // lock released before bumping generation

                    // Bump generation *after* releasing the lock so the main
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
        // snapshot, skip the clone entirely — this is the common case on every
        // frame between 5-second tt-smi refreshes.
        let current_gen = self.smbus_generation.load(Ordering::Acquire);
        if current_gen != self.smbus_snapshot_generation {
            // New data available — try to grab the lock.  try_lock() is
            // non-blocking: if the background thread is currently inserting
            // (should take <1ms), we defer until next frame.
            if let Ok(cache) = self.smbus_cache.try_lock() {
                if !cache.is_empty() {
                    self.smbus_snapshot.clone_from(&*cache);
                    self.smbus_snapshot_generation = current_gen;
                }
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
        self.smbus_snapshot.get(&device_idx)
    }

    fn backend_info(&self) -> String {
        let n = self.sysfs.device_count();
        if self.smbus_snapshot.is_empty() {
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
        assert!(backend.smbus_snapshot.is_empty());
        assert_eq!(backend.json_refresh_interval, Duration::from_secs(DEFAULT_JSON_REFRESH_SECS));
    }

    #[test]
    fn test_hybrid_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = HybridBackend::with_config("tt-smi", config);
        assert_eq!(backend.tt_smi_path, "tt-smi");
    }
}
