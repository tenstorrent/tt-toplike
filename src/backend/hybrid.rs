//! Hybrid backend: sysfs real-time metrics + persistent streaming tt-smi enrichment
//!
//! This backend combines the strengths of the Sysfs and JSON backends:
//!
//! - **Sysfs** provides fast, non-invasive real-time reads (temperature, power,
//!   voltage, current) via Linux hwmon. These run on every `update()` call and
//!   complete in microseconds, keeping the render loop smooth.
//!
//! - **Streaming tt-smi** provides rich SMBUS telemetry (DDR status, ARC health,
//!   board IDs, firmware versions). A persistent shell subprocess runs tt-smi
//!   every 1.5 seconds and writes RS-delimited JSON records to a pipe. The
//!   reader thread picks them up without spawning a new process each time.
//!
//! ## Why persistent streaming instead of polling?
//!
//! Spawning `tt-smi -s` from scratch every 5 seconds incurs:
//! - Process creation overhead (~50–200 ms)
//! - tt-smi SMBUS probe startup (~100–500 ms)
//! - A sudden "surge" as all device data updates on the same frame
//!
//! With a persistent shell loop the process stays warm, new data arrives every
//! ~1.5 s, and the reader thread is just a `read_until()` call on an open pipe.
//!
//! ## EMA smoothing
//!
//! When a fresh SMBUS record arrives, numeric fields (ARC health counters, DDR
//! speed, clock frequencies) are blended toward the new value via EMA (α = 0.25).
//! This distributes each visual change across ~4 render frames (~400 ms at
//! 100 ms/frame), eliminating the sudden pop that was visible with snapshot
//! replacement.
//!
//! Discrete fields (board IDs, firmware versions, DDR status bitmask) are always
//! copied verbatim — they carry no meaning as floats.
//!
//! ## Zero-allocation render path
//!
//! The SMBUS snapshot is stored as `Arc<HashMap<...>>`. When the reader thread
//! produces a fresh record it wraps the new map in a new Arc and swaps the
//! shared pointer. The render thread adopts the new snapshot with a single
//! `Arc::clone()` — one atomic increment, zero heap allocations.
//!
//! ## Degraded mode
//!
//! If `sh` is absent or tt-smi fails to start, the backend falls back to a
//! 5-second blocking poll loop (same behaviour as the previous implementation).
//! If tt-smi is absent entirely, the backend runs in sysfs-only mode.

use crate::backend::sysfs::SysfsBackend;
use crate::backend::{BackendConfig, TelemetryBackend};
use crate::backend::{json, smbus_smooth};
use crate::error::{BackendError, BackendResult};
use crate::models::{Device, SmbusTelemetry, Telemetry};
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;


/// Hybrid backend combining sysfs real-time + persistent streaming JSON enrichment.
pub struct HybridBackend {
    /// Primary real-time data source — never blocks more than a few µs.
    sysfs: SysfsBackend,

    /// Path to tt-smi executable (searched in PATH if bare name).
    tt_smi_path: String,

    /// Background-thread deposit slot: the reader thread atomically swaps
    /// the Arc pointer here without any lock — the render thread loads it
    /// with a single lock-free atomic operation.
    smbus_shared: Arc<ArcSwap<HashMap<usize, SmbusTelemetry>>>,

    /// The render thread's private view of the latest background snapshot.
    /// Updated via `Arc::clone()` — one atomic ref-count increment, zero
    /// heap allocations — when the generation counter indicates new data.
    smbus_latest: Arc<HashMap<usize, SmbusTelemetry>>,

    /// Incremented by the reader thread after each successful record parse.
    /// The render thread compares against `smbus_snapshot_gen` before paying
    /// even the cheap lock cost.
    smbus_generation: Arc<AtomicU64>,

    /// The generation reflected in `smbus_latest`.
    smbus_snapshot_gen: u64,

    /// What `smbus_telemetry()` actually returns.
    /// Converges toward `smbus_latest` via EMA blend on every render frame.
    smbus_blended: HashMap<usize, SmbusTelemetry>,

    /// Per-device EMA accumulators for numeric SMBUS fields.
    smbus_ema: smbus_smooth::SmbusEmaState,

    /// Tells the reader thread to stop cleanly.
    stop_flag: Arc<AtomicBool>,

    /// Handle to the reader thread.
    /// Wrapped in Mutex so HybridBackend implements Sync (JoinHandle is !Sync).
    refresh_handle: Mutex<Option<thread::JoinHandle<()>>>,

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
            smbus_shared: Arc::new(ArcSwap::from(Arc::clone(&empty))),
            smbus_latest: Arc::clone(&empty),
            smbus_generation: Arc::new(AtomicU64::new(0)),
            smbus_snapshot_gen: 0,
            smbus_blended: HashMap::new(),
            smbus_ema: HashMap::new(),
            stop_flag: Arc::new(AtomicBool::new(false)),
            refresh_handle: Mutex::new(None),
        }
    }
}

impl TelemetryBackend for HybridBackend {
    fn init(&mut self) -> BackendResult<()> {
        // ── 1. Primary device detection via sysfs ─────────────────────────────
        self.sysfs.init().map_err(|e| {
            BackendError::Initialization(format!("HybridBackend: sysfs init failed: {}", e))
        })?;
        log::info!(
            "HybridBackend: sysfs OK ({} devices)",
            self.sysfs.device_count()
        );

        // ── 2. Resolve tt-smi path ────────────────────────────────────────────
        //
        // Resolve a bare program name to its absolute path now, while we still
        // have a reliable PATH.  Subprocesses may inherit a stripped PATH (systemd,
        // SSH without login shell), so embedding the absolute path is more robust.
        let resolved_path = if std::path::Path::new(&self.tt_smi_path).is_absolute() {
            self.tt_smi_path.clone()
        } else {
            match which::which(&self.tt_smi_path) {
                Ok(p) => {
                    log::info!("HybridBackend: resolved {} → {}", self.tt_smi_path, p.display());
                    p.to_string_lossy().into_owned()
                }
                Err(e) => {
                    log::warn!(
                        "HybridBackend: '{}' not found in PATH ({}); \
                         will run sysfs-only mode",
                        self.tt_smi_path, e
                    );
                    String::new()
                }
            }
        };

        // ── 3. Start SMBUS reader thread — returns immediately ────────────────
        //
        // The thread polls tt-smi directly (no shell wrapper, no `timeout`
        // dependency, no RS delimiter) every 1.5 s.  Results are deposited via
        // lock-free ArcSwap; the render thread picks them up on the next frame.
        //
        // init() does NOT wait for the first record — the first render shows sysfs
        // data instantly, and SMBUS fields (board IDs, DDR status, etc.) populate
        // within ~1–2 s as the background thread completes its first poll.
        let smbus_shared     = Arc::clone(&self.smbus_shared);
        let smbus_generation = Arc::clone(&self.smbus_generation);
        let stop_flag        = Arc::clone(&self.stop_flag);

        let handle = thread::Builder::new()
            .name("hybrid-smbus-reader".to_string())
            .spawn(move || {
                log::debug!("HybridBackend: SMBUS reader thread started");

                if resolved_path.is_empty() {
                    log::debug!("HybridBackend: no tt-smi path — reader thread idle");
                    // Park the thread; Drop will set stop_flag to release it.
                    while !stop_flag.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_secs(1));
                    }
                    return;
                }

                while !stop_flag.load(Ordering::Relaxed) {
                    // Spawn tt-smi directly — no shell, no `timeout` dependency.
                    match std::process::Command::new(&resolved_path)
                        .arg("-s")
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::null())
                        .output()
                    {
                        Ok(out) if out.status.success() => {
                            let json = String::from_utf8_lossy(&out.stdout);
                            let data = json::parse_smbus_from_json(&json);
                            if !data.is_empty() {
                                smbus_shared.store(Arc::new(data));
                                smbus_generation.fetch_add(1, Ordering::Release);
                                log::debug!("HybridBackend: SMBUS snapshot updated");
                            } else {
                                log::debug!("HybridBackend: tt-smi returned empty/unparseable JSON");
                            }
                        }
                        Ok(out) => {
                            log::debug!(
                                "HybridBackend: tt-smi exited {:?}",
                                out.status.code()
                            );
                        }
                        Err(e) => {
                            log::warn!("HybridBackend: failed to run tt-smi: {}", e);
                            // Back off longer on spawn failure to avoid busy-looping.
                            thread::sleep(Duration::from_secs(5));
                            continue;
                        }
                    }

                    // Sleep 1.5 s between polls, checking stop flag every 50 ms
                    // so Drop doesn't block waiting for the full sleep to expire.
                    let poll_start = std::time::Instant::now();
                    while poll_start.elapsed() < Duration::from_millis(1500) {
                        if stop_flag.load(Ordering::Relaxed) { return; }
                        thread::sleep(Duration::from_millis(50));
                    }
                }

                log::debug!("HybridBackend: SMBUS reader thread exiting");
            })
            .expect("failed to spawn hybrid-smbus-reader thread");

        *self.refresh_handle.lock().unwrap() = Some(handle);

        log::info!(
            "HybridBackend: ready ({} sysfs devices; SMBUS data arriving in background)",
            self.sysfs.device_count()
        );
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        // Fast path: sysfs only — completes in microseconds.
        self.sysfs.update()?;

        // ── Adopt new SMBUS target if the reader thread has one ───────────────
        //
        // The generation counter is a cheap atomic load (no lock). Most frames
        // will skip the lock entirely — new records arrive only every ~1.5 s.
        let current_gen = self.smbus_generation.load(Ordering::Acquire);
        if current_gen != self.smbus_snapshot_gen {
            // load_full() is lock-free — one atomic read, no blocking possible.
            self.smbus_latest = self.smbus_shared.load_full();
            self.smbus_snapshot_gen = current_gen;
            // Remove devices that disappeared from the new snapshot.
            self.smbus_blended.retain(|k, _| self.smbus_latest.contains_key(k));
            self.smbus_ema.retain(|k, _| self.smbus_latest.contains_key(k));
        }

        // ── Apply EMA blend every frame ───────────────────────────────────────
        //
        // Runs whether or not a new record arrived this frame.  Each numeric
        // field absorbs 25 % of the remaining delta on every call, so a step
        // change distributes across ~4 frames (~400 ms at 100 ms/frame).
        //
        // Without this loop, `smbus_blended` would be frozen between record
        // arrivals and the display would jump on each new generation instead of
        // converging smoothly — producing the rhythmic stutter.
        for (idx, target) in self.smbus_latest.iter() {
            let existing = self.smbus_blended
                .entry(*idx)
                .or_insert_with(|| target.clone());
            smbus_smooth::apply_ema(&mut self.smbus_ema, *idx, target, existing);
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
        // Signal the reader thread to stop; it checks every 50 ms so this
        // unblocks quickly regardless of where it is in the poll sleep.
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
        assert!(backend.smbus_ema.is_empty());
    }

    #[test]
    fn test_hybrid_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = HybridBackend::with_config("tt-smi", config);
        assert_eq!(backend.tt_smi_path, "tt-smi");
    }

    #[test]
    fn test_ema_applied_on_new_generation() {
        let mut backend = HybridBackend::new("tt-smi");

        // Simulate reader thread depositing data for 2 devices.
        let mut fresh: HashMap<usize, SmbusTelemetry> = HashMap::new();
        for i in 0..2 {
            fresh.insert(i, SmbusTelemetry {
                arc0_health: Some("100".to_owned()),
                board_id:    Some(format!("board-{}", i)),
                ..SmbusTelemetry::default()
            });
        }
        backend.smbus_shared.store(Arc::new(fresh));
        backend.smbus_generation.store(1, Ordering::Release);

        // Manually drive the update logic (skip sysfs).
        let current_gen = backend.smbus_generation.load(Ordering::Acquire);
        assert_ne!(current_gen, backend.smbus_snapshot_gen);

        backend.smbus_latest = backend.smbus_shared.load_full();
        backend.smbus_snapshot_gen = current_gen;
        for idx in backend.smbus_latest.keys().copied().collect::<Vec<_>>() {
            let incoming = backend.smbus_latest[&idx].clone();
            let existing = backend.smbus_blended
                .entry(idx)
                .or_insert_with(|| incoming.clone());
            smbus_smooth::apply_ema(&mut backend.smbus_ema, idx, &incoming, existing);
        }

        // Both devices should appear in blended.
        assert_eq!(backend.smbus_blended.len(), 2);
        // board_id is discrete — must be copied verbatim.
        assert_eq!(backend.smbus_blended[&0].board_id.as_deref(), Some("board-0"));
        // arc0_health is numeric — first reading has no previous EMA → value is 100.
        assert_eq!(backend.smbus_blended[&0].arc0_health.as_deref(), Some("100"));
    }
}
