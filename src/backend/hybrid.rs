// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Hybrid backend: sysfs real-time metrics + persistent streaming tt-smi enrichment
//!
//! This backend combines the strengths of the Sysfs and JSON backends:
//!
//! - **Sysfs** provides fast, non-invasive real-time reads (temperature, power,
//!   voltage, current) via Linux hwmon. These four reads per device run on every
//!   `update()` call and complete in well under a millisecond, keeping the render
//!   loop smooth. The heavier sysfs reads behind them (tt-kmd class attrs, the
//!   synthesized SMBUS block, the twelve PCIe counters) are decimated to ~1 Hz
//!   inside `SysfsBackend::update` — see its `SLOW_LANE_INTERVAL`.
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
use crate::backend::{json, smbus_smooth};
use crate::backend::{BackendConfig, TelemetryBackend};
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
    /// Primary real-time data source — a handful of sysfs file reads per tick,
    /// never a subprocess or a device open (see the module docs for the fast /
    /// slow lane split).
    sysfs: SysfsBackend,

    /// Path to tt-smi executable (searched in PATH if bare name).
    tt_smi_path: String,

    /// Background-thread deposit slot: the reader thread atomically swaps
    /// the Arc pointer here without any lock — the render thread loads it
    /// with a single lock-free atomic operation.
    smbus_shared: Arc<ArcSwap<HashMap<usize, SmbusTelemetry>>>,

    /// Per-device metadata (firmwares, limits, PCIe link, board power) from
    /// the same JSON snapshot, stamped with the moment it was published.
    /// Populated by the reader thread; `update()` copies the static fields into
    /// sysfs devices when a new payload arrives, and overlays the *measurement*
    /// field (`board_power`) onto telemetry every tick — but only while the
    /// stamp is fresh, see [`BOARD_POWER_MAX_AGE`].
    device_meta_shared: Arc<ArcSwap<MetaSnapshot>>,

    /// The `MetaSnapshot::at` stamp whose static fields are already copied into
    /// the sysfs device list, so that copy runs once per payload rather than
    /// every frame. `None` until the first payload arrives.
    meta_applied_at: Option<std::time::Instant>,

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

    /// Verbatim stdout of the last successful `tt-smi -s` run made by the
    /// background reader thread. Retained for the `--serve` telemetry publisher
    /// (see [`TelemetryBackend::snapshot_json`]) so Hybrid can relay the same raw
    /// JSON that `JSONBackend` would, even though Hybrid's primary telemetry path
    /// is sysfs. `None` until the first successful background poll completes, or
    /// permanently if `tt-smi` was never found on PATH (sysfs-only mode) — never
    /// fabricated from sysfs data.
    last_raw: Arc<Mutex<Option<String>>>,

    /// `{ normalized PCI bus id → sysfs device index }`, built from the sysfs
    /// device list at `init()` (sysfs discovery is a one-shot, so this is stable
    /// for the backend's lifetime). The reader thread gets a clone and uses it to
    /// re-key each tt-smi snapshot onto these indices — see
    /// [`rekey_snapshot_by_bus_id`] for why the join can't be positional.
    bus_to_index: HashMap<String, usize>,
}

/// A published `DeviceMeta` payload and the instant it was published.
///
/// The stamp exists for `board_power`, the one *measurement* in the payload.
/// The rest (firmware versions, limits, PCIe link geometry) is static metadata
/// that doesn't go stale — a value read five minutes ago is still true. Board
/// power is a live wattage, and re-applying the last one on every tick made a
/// dead `tt-smi` reader (subprocess killed, ARC busy under load, tt-smi removed
/// or upgraded mid-run) render a frozen "Board  NW total" row directly beneath
/// a Power row that *was* still updating, with nothing to tell them apart.
#[derive(Default)]
struct MetaSnapshot {
    /// `None` for the empty payload every backend starts with — nothing to age.
    at: Option<std::time::Instant>,
    meta: HashMap<usize, json::DeviceMeta>,
}

/// How long a published `board_power` reading stays eligible for display.
///
/// The reader polls tt-smi every ~1.5 s and backs off to 5 s after a failure,
/// so this tolerates a failed poll or two without flickering the row, while a
/// genuinely stopped reader drops it within a few seconds instead of never.
const BOARD_POWER_MAX_AGE: Duration = Duration::from_secs(10);

/// Re-key a tt-smi snapshot from tt-smi's **array positions** onto this
/// backend's **sysfs device indices**, joining on normalized PCI bus id.
///
/// ## Why a bus-id join and not an index join
///
/// `parse_snapshot` keys SMBUS/meta by position in tt-smi's `device_info[]`, and
/// sysfs discovery assigns its indices in ascending bus-id order specifically so
/// those two sequences line up (see `SysfsBackend::collect_hwmon_candidates`).
/// That agreement holds only while both sides enumerate the *same set* of cards.
/// They can legitimately differ: a card that's busy or failed to enumerate is
/// missing from tt-smi's list but still has a live hwmon node, an upstream
/// `--devices` filter can trim the snapshot, and a hotplug event can change the
/// count mid-run. With a positional join, one missing entry shifts every later
/// card's SMBUS / PCIe link / board power onto its neighbour — the exact
/// mis-attribution this release set out to eliminate, arrived at by a different
/// route. Bus id is carried by both sides, so it is the join key.
///
/// ## Fallbacks
///
/// * `bus_to_index` empty (no sysfs bus ids resolvable at all — every device
///   fell back to its hwmon directory name): positional join, logged at `warn`.
/// * a snapshot entry with no bus id (a tt-smi old enough to omit
///   `board_info.bus_id`): that entry alone falls back to its position, logged
///   at `warn`.
/// * a snapshot entry whose bus id matches no sysfs device: **dropped**. sysfs
///   can't see that card, so there is no device to attribute it to; keying it by
///   position would put it on someone else's row.
fn rekey_snapshot_by_bus_id(
    parts: json::SnapshotParts,
    bus_to_index: &HashMap<String, usize>,
) -> json::SnapshotParts {
    if bus_to_index.is_empty() {
        log::warn!(
            "HybridBackend: no PCI bus ids resolved for the sysfs devices — \
             falling back to a positional tt-smi join (per-device attribution \
             may be wrong if tt-smi enumerates a different set of cards)"
        );
        return parts;
    }

    let json::SnapshotParts {
        smbus,
        meta,
        bus_ids,
    } = parts;

    // Every position mentioned anywhere in the snapshot, resolved once.
    // BTreeSet keeps the (rare) warn output in a stable order.
    let positions: std::collections::BTreeSet<usize> = smbus
        .keys()
        .chain(meta.keys())
        .chain(bus_ids.keys())
        .copied()
        .collect();

    // Two passes, because the bus-id join is authoritative and the positional
    // fallback must never step on it: with a snapshot that mixes entries (one
    // without a `board_info.bus_id`, one whose bus id resolves to that same
    // index) an unconditional `mapping.insert(pos, pos)` produced two positions
    // pointing at one device index, and `rekey_map`'s `HashMap` collect then
    // kept whichever pair the iterator emitted last — nondeterministic
    // mis-attribution of exactly the kind the bus-id join exists to prevent.
    let mut mapping: HashMap<usize, usize> = HashMap::new();
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut no_bus_id: Vec<usize> = Vec::new();
    for pos in positions {
        match bus_ids.get(&pos) {
            Some(bus) => match bus_to_index.get(bus) {
                Some(&device_idx) => {
                    mapping.insert(pos, device_idx);
                    claimed.insert(device_idx);
                }
                None => {
                    log::debug!(
                        "HybridBackend: tt-smi device_info[{pos}] (bus {bus}) has no \
                         matching sysfs device — dropping its SMBUS/meta rather than \
                         attributing it to another card"
                    );
                }
            },
            None => no_bus_id.push(pos),
        }
    }
    // Second pass: positional fallback, only onto indices the bus-id pass left
    // free. A contested index means we'd be guessing between two cards, and the
    // guess that loses gets someone else's telemetry — dropping is the honest
    // outcome (the same choice the unmatched-bus-id branch above makes).
    for pos in no_bus_id {
        if claimed.insert(pos) {
            log::warn!(
                "HybridBackend: tt-smi device_info[{pos}] carries no bus_id — \
                 falling back to a positional join for this entry"
            );
            mapping.insert(pos, pos);
        } else {
            log::warn!(
                "HybridBackend: tt-smi device_info[{pos}] carries no bus_id and device \
                 index {pos} is already claimed by a bus-id match — dropping its \
                 SMBUS/meta rather than overwriting that card's data"
            );
        }
    }

    json::SnapshotParts {
        smbus: rekey_map(smbus, &mapping),
        meta: rekey_map(meta, &mapping),
        bus_ids: rekey_map(bus_ids, &mapping),
    }
}

/// Move every entry of a position-keyed map onto its joined device index,
/// dropping entries the join couldn't resolve. Generic (rather than a closure)
/// because it's applied to three maps with three different value types.
fn rekey_map<V>(map: HashMap<usize, V>, mapping: &HashMap<usize, usize>) -> HashMap<usize, V> {
    map.into_iter()
        .filter_map(|(pos, v)| mapping.get(&pos).map(|&idx| (idx, v)))
        .collect()
}

/// Parse one verbatim `tt-smi -s` document, re-key it onto sysfs device indices,
/// and deposit it for the render thread.
///
/// Factored out of the reader-thread closure so the join is exercised by tests
/// through the same code the thread runs, rather than a re-implementation of it.
/// Each payload is gated on its *own* emptiness. A snapshot whose SMBUS map
/// comes out empty (unparseable output, or every entry dropped for want of a
/// matching sysfs device) doesn't replace the previously deposited SMBUS map,
/// so a single bad poll doesn't blank those rows — but it also doesn't suppress
/// the `DeviceMeta` payload, which is parsed from `board_info`/`telemetry` and
/// is independent of `smbus_telem`. Gating meta on SMBUS meant a snapshot with
/// no SMBUS block at all (permission-restricted run, or a card whose ARC didn't
/// answer the SMBUS read) dropped the PCIe link row and the Board power row
/// too, even though both parsed fine.
fn publish_snapshot(
    json_str: &str,
    bus_to_index: &HashMap<String, usize>,
    smbus_shared: &ArcSwap<HashMap<usize, SmbusTelemetry>>,
    device_meta_shared: &ArcSwap<MetaSnapshot>,
    smbus_generation: &AtomicU64,
) {
    let snapshot = rekey_snapshot_by_bus_id(json::parse_snapshot(json_str), bus_to_index);
    if snapshot.smbus.is_empty() && snapshot.meta.is_empty() {
        log::debug!("HybridBackend: tt-smi returned empty/unparseable JSON");
        return;
    }
    if !snapshot.meta.is_empty() {
        device_meta_shared.store(Arc::new(MetaSnapshot {
            at: Some(std::time::Instant::now()),
            meta: snapshot.meta,
        }));
    }
    if !snapshot.smbus.is_empty() {
        smbus_shared.store(Arc::new(snapshot.smbus));
        smbus_generation.fetch_add(1, Ordering::Release);
        log::debug!("HybridBackend: SMBUS snapshot updated");
    }
}

impl HybridBackend {
    /// Create a new Hybrid backend using the given tt-smi path.
    pub fn new(tt_smi_path: impl Into<String>) -> Self {
        Self::with_config(tt_smi_path, BackendConfig::default())
    }

    /// `{ normalized sysfs bus id → sysfs device index }` — the join key map the
    /// tt-smi side is matched against. Devices whose bus id couldn't be resolved
    /// from the hwmon `device` symlink fall back to their hwmon directory name
    /// (e.g. `"hwmon3"`), which no tt-smi bus id will ever match, so they simply
    /// don't participate in the join.
    fn sysfs_bus_index(&self) -> HashMap<String, usize> {
        self.sysfs
            .devices()
            .iter()
            .filter_map(|d| {
                let bus = json::normalize_bus_id(&d.bus_id);
                // Only PCI-shaped ids are usable keys; the hwmon-dirname
                // fallback ("hwmon3") is deliberately excluded.
                bus.contains(':').then_some((bus, d.index))
            })
            .collect()
    }

    /// Create a new Hybrid backend with explicit configuration.
    pub fn with_config(tt_smi_path: impl Into<String>, _config: BackendConfig) -> Self {
        let empty: Arc<HashMap<usize, SmbusTelemetry>> = Arc::new(HashMap::new());
        let empty_meta: Arc<MetaSnapshot> = Arc::new(MetaSnapshot::default());
        Self {
            sysfs: SysfsBackend::new(),
            tt_smi_path: tt_smi_path.into(),
            smbus_shared: Arc::new(ArcSwap::from(Arc::clone(&empty))),
            device_meta_shared: Arc::new(ArcSwap::from(Arc::clone(&empty_meta))),
            meta_applied_at: None,
            smbus_latest: Arc::clone(&empty),
            smbus_generation: Arc::new(AtomicU64::new(0)),
            smbus_snapshot_gen: 0,
            smbus_blended: HashMap::new(),
            smbus_ema: HashMap::new(),
            stop_flag: Arc::new(AtomicBool::new(false)),
            refresh_handle: Mutex::new(None),
            last_raw: Arc::new(Mutex::new(None)),
            bus_to_index: HashMap::new(),
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
                    log::info!(
                        "HybridBackend: resolved {} → {}",
                        self.tt_smi_path,
                        p.display()
                    );
                    p.to_string_lossy().into_owned()
                }
                Err(e) => {
                    log::warn!(
                        "HybridBackend: '{}' not found in PATH ({}); \
                         will run sysfs-only mode",
                        self.tt_smi_path,
                        e
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
        let smbus_shared = Arc::clone(&self.smbus_shared);
        let device_meta_shared = Arc::clone(&self.device_meta_shared);
        let smbus_generation = Arc::clone(&self.smbus_generation);
        let stop_flag = Arc::clone(&self.stop_flag);
        let last_raw = Arc::clone(&self.last_raw);
        // Join key map, snapshotted for the reader thread. Built now — after
        // sysfs.init() above — because that is where the device list (and thus
        // every bus id) comes from.
        self.bus_to_index = self.sysfs_bus_index();
        let bus_to_index = self.bus_to_index.clone();

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
                            let json_str = String::from_utf8_lossy(&out.stdout);

                            // Retain the verbatim tt-smi output for snapshot_json()
                            // (the --serve relay), regardless of what the SMBUS
                            // parse below finds — this is exactly what tt-smi
                            // printed, so there's nothing to fabricate. Off the
                            // render hot path: this lock is only ever touched by
                            // this background thread (writer) and an occasional
                            // snapshot_json() caller (reader), never per-frame.
                            if let Ok(mut guard) = last_raw.lock() {
                                *guard = Some(json_str.to_string());
                            }

                            // Single parse pass (SMBUS + firmware/limits/PCIe/
                            // board power), re-keyed onto sysfs device indices by
                            // bus id, then deposited for the render thread.
                            publish_snapshot(
                                &json_str,
                                &bus_to_index,
                                &smbus_shared,
                                &device_meta_shared,
                                &smbus_generation,
                            );
                        }
                        Ok(out) => {
                            log::debug!("HybridBackend: tt-smi exited {:?}", out.status.code());
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
                        if stop_flag.load(Ordering::Relaxed) {
                            return;
                        }
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
        // Fast path: sysfs only — four hwmon reads per device, plus the ~1 Hz
        // slow lane (class attrs / SMBUS synthesis / PCIe counters) when due.
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
            self.smbus_blended
                .retain(|k, _| self.smbus_latest.contains_key(k));
            self.smbus_ema
                .retain(|k, _| self.smbus_latest.contains_key(k));
        }

        // ── Adopt new device metadata (firmwares / limits / PCIe link) ────────
        //
        // Keyed off the meta payload's own stamp, NOT the SMBUS generation: the
        // two are published independently, and a snapshot with no `smbus_telem`
        // block (permission-restricted run, ARC that didn't answer the SMBUS
        // read) still carries perfectly good `board_info` — gating this on SMBUS
        // dropped the PCIe link row and the FW/limits enrichment along with it.
        //
        // Always overwrite (not just when None) so a firmware upgrade or a
        // hot-plugged link renegotiation applied while the tool is running is
        // reflected without restarting. A degraded snapshot that omits a field
        // retains the previous value rather than reverting to None —
        // intentional: firmware versions and PCIe link geometry don't regress,
        // and a momentary ARC issue shouldn't blank these rows.
        //
        // `board_power` is deliberately NOT copied here — it's a live
        // measurement, not static metadata, and is overlaid onto telemetry
        // every tick below (with a freshness check).
        let meta_snapshot = self.device_meta_shared.load_full();
        if meta_snapshot.at.is_some() && meta_snapshot.at != self.meta_applied_at {
            self.meta_applied_at = meta_snapshot.at;
            for device in self.sysfs.devices_mut() {
                if let Some(dm) = meta_snapshot.meta.get(&device.index) {
                    if dm.firmwares.is_some() {
                        device.firmwares = dm.firmwares.clone();
                    }
                    if dm.limits.is_some() {
                        device.limits = dm.limits.clone();
                    }
                    if dm.pcie_speed.is_some() {
                        device.pcie_speed = dm.pcie_speed.clone();
                    }
                    if dm.pcie_width.is_some() {
                        device.pcie_width = dm.pcie_width;
                    }
                }
            }
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
            let existing = self
                .smbus_blended
                .entry(*idx)
                .or_insert_with(|| target.clone());
            smbus_smooth::apply_ema(&mut self.smbus_ema, *idx, target, existing);
        }

        // ── Overlay SMBUS TDP/TDC + whole-card board power onto sysfs telemetry ──
        //
        // The hwmon driver exposes only the Tensix VDD rail via power1_input.
        // On Blackhole (and newer firmwares) the firmware-reported TDP register
        // is a real-time measurement covering all power domains (Tensix + GDDR).
        // Prefer it when available so the displayed wattage matches tt-smi.
        //
        // VCORE from SMBUS is in millivolts; TDP/TDC are in watts/amperes directly.
        for (idx, smbus) in &self.smbus_blended {
            if let Some(telem) = self.sysfs.telemetry_cache_mut(*idx) {
                if let Some(w) = smbus.tdp_watts() {
                    telem.power = Some(w);
                }
                if let Some(a) = smbus.tdc_amperes() {
                    telem.current = Some(a);
                }
            }
        }
        // board_power comes from tt-smi's `telemetry` block (not SMBUS), and
        // — unlike firmwares/limits/pcie link above — is a per-tick
        // measurement rather than static metadata, so it's applied here, every
        // tick, in lockstep with the TDP/TDC overlay.
        //
        // Gated on the payload's age. sysfs rebuilds each `Telemetry` from
        // scratch every tick with `board_power: None`, so skipping the overlay
        // is what drops the row: once the reader stops producing snapshots the
        // Board row disappears within `BOARD_POWER_MAX_AGE` instead of pinning
        // the last poll's wattage under a Power row that keeps moving.
        let board_power_fresh = meta_snapshot
            .at
            .is_some_and(|at| at.elapsed() <= BOARD_POWER_MAX_AGE);
        if board_power_fresh {
            for (idx, dm) in meta_snapshot.meta.iter() {
                if let Some(bp) = dm.board_power {
                    if let Some(telem) = self.sysfs.telemetry_cache_mut(*idx) {
                        telem.board_power = Some(bp);
                    }
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

    /// SMBUS telemetry for a device, preferring the tt-smi-derived blend.
    ///
    /// Falls back to the inner sysfs backend's synthesized block for any device
    /// the blend doesn't cover. That is the whole story on a modern-kmd box
    /// *without* tt-smi installed — auto-detect still resolves to Hybrid on
    /// Linux, so returning only `smbus_blended` there meant this method reported
    /// `None` for every device and the tt-kmd-sourced fan/clock/therm-trip/
    /// serial/heartbeat rows were reachable only via an explicit
    /// `--backend sysfs`.
    ///
    /// The blend wins wherever it exists because tt-smi's data is strictly
    /// richer (DDR status, GDDR temps/ECC, firmware versions, enabled masks)
    /// than what the driver exposes through sysfs.
    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        self.smbus_blended
            .get(&device_idx)
            .or_else(|| self.sysfs.smbus_telemetry(device_idx))
    }

    fn backend_info(&self) -> String {
        let n = self.sysfs.device_count();
        if self.smbus_blended.is_empty() {
            format!("Hybrid ({} via sysfs, no tt-smi)", n)
        } else {
            format!("Hybrid ({} via sysfs+json)", n)
        }
    }

    fn snapshot_json(&self) -> Option<String> {
        // `.lock().ok()` — never panics on a poisoned lock (panic-free contract).
        self.last_raw.lock().ok().and_then(|g| g.clone())
    }

    fn pcie_bandwidth(
        &self,
        device_idx: usize,
    ) -> Option<crate::backend::pcie_counters::PcieBandwidth> {
        self.sysfs.pcie_bandwidth(device_idx)
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
            fresh.insert(
                i,
                SmbusTelemetry {
                    arc0_health: Some("100".to_owned()),
                    board_id: Some(format!("board-{}", i)),
                    ..SmbusTelemetry::default()
                },
            );
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
            let existing = backend
                .smbus_blended
                .entry(idx)
                .or_insert_with(|| incoming.clone());
            smbus_smooth::apply_ema(&mut backend.smbus_ema, idx, &incoming, existing);
        }

        // Both devices should appear in blended.
        assert_eq!(backend.smbus_blended.len(), 2);
        // board_id is discrete — must be copied verbatim.
        assert_eq!(
            backend.smbus_blended[&0].board_id.as_deref(),
            Some("board-0")
        );
        // arc0_health is numeric — first reading has no previous EMA → value is 100.
        assert_eq!(
            backend.smbus_blended[&0].arc0_health.as_deref(),
            Some("100")
        );
    }

    /// `snapshot_json()` must relay whatever the background reader thread last
    /// deposited into `last_raw`, and be `None` until the first successful
    /// tt-smi poll. Simulates the reader thread's deposit (as
    /// `test_ema_applied_on_new_generation` above simulates its SMBUS deposit)
    /// rather than spinning up a real subprocess + thread in a unit test.
    #[test]
    fn test_snapshot_json_relays_reader_thread_deposit() {
        let backend = HybridBackend::new("tt-smi");
        assert_eq!(
            backend.snapshot_json(),
            None,
            "no background poll has completed yet"
        );

        let raw = r#"{"device_info": [{"board_info": {"board_type": "p150a"}}]}"#;
        *backend.last_raw.lock().unwrap() = Some(raw.to_string());

        assert_eq!(
            backend.snapshot_json(),
            Some(raw.to_string()),
            "snapshot_json must relay the exact raw tt-smi output"
        );
    }

    /// On a modern-kmd box with no tt-smi installed, `smbus_telemetry()` must
    /// still surface the sysfs-synthesized block — auto-detect lands on Hybrid,
    /// so returning only the (empty) tt-smi blend hid the tt-kmd fan/clock/
    /// therm-trip/serial rows behind an explicit `--backend sysfs`.
    #[test]
    fn test_smbus_falls_back_to_sysfs_without_tt_smi() {
        let mut backend = HybridBackend::new("tt-smi");

        backend.sysfs.insert_smbus_for_test(
            0,
            SmbusTelemetry {
                aiclk: Some("800".to_string()),
                fan_speed: Some("2100".to_string()),
                arc0_health: Some("43874".to_string()), // tt_heartbeat
                ..SmbusTelemetry::default()
            },
        );

        // No tt-smi record ever arrived, so the blend is empty.
        assert!(backend.smbus_blended.is_empty());

        let smbus = backend
            .smbus_telemetry(0)
            .expect("sysfs-synthesized SMBUS must be visible through Hybrid");
        assert_eq!(smbus.aiclk.as_deref(), Some("800"));
        assert_eq!(smbus.fan_rpm(), Some(2100));
        assert_eq!(smbus.arc0_health.as_deref(), Some("43874"));

        // Unknown device index is still None on both paths.
        assert!(backend.smbus_telemetry(9).is_none());
    }

    /// When tt-smi data *is* present it must win — it is strictly richer than
    /// the sysfs synthesis.
    #[test]
    fn test_json_blend_wins_over_sysfs_smbus() {
        let mut backend = HybridBackend::new("tt-smi");
        backend.sysfs.insert_smbus_for_test(
            0,
            SmbusTelemetry {
                aiclk: Some("111".to_string()),
                ..SmbusTelemetry::default()
            },
        );
        backend.smbus_blended.insert(
            0,
            SmbusTelemetry {
                aiclk: Some("800".to_string()),
                ddr_status: Some("0x55555555".to_string()),
                ..SmbusTelemetry::default()
            },
        );
        let smbus = backend.smbus_telemetry(0).unwrap();
        assert_eq!(smbus.aiclk.as_deref(), Some("800"));
        assert_eq!(smbus.ddr_status.as_deref(), Some("0x55555555"));
    }

    /// The tt-smi ↔ sysfs join must be by PCI bus id, not by list position.
    ///
    /// Regression: `parse_snapshot` keys SMBUS/meta by tt-smi *array position*
    /// and this backend looked them up by *sysfs device index*. That only agrees
    /// while both sides enumerate the same cards. Here sysfs sees four cards and
    /// tt-smi reports only the 2nd and 4th (a card busy or failed to enumerate,
    /// `--devices` filtering, hotplug) — with the positional join, tt-smi's two
    /// records landed on devices 0 and 1, i.e. **every card past the gap showed a
    /// neighbour's SMBUS, PCIe link and board power**: the same mis-attribution
    /// v0.8.0 set out to fix, reached by a different route.
    ///
    /// Drives the production path end to end: `publish_snapshot` (what the reader
    /// thread calls) then `update()` (what the render loop calls), so the
    /// board_power overlay is covered too.
    #[test]
    fn tt_smi_records_join_sysfs_devices_by_bus_id_not_position() {
        use crate::models::{Device, Telemetry};

        let mut backend = HybridBackend::new("tt-smi");

        // Four cards visible via sysfs, indices assigned in bus-id order.
        for (idx, bus) in [
            "0000:01:00.0",
            "0000:02:00.0",
            "0000:03:00.0",
            "0000:04:00.0",
        ]
        .iter()
        .enumerate()
        {
            backend.sysfs.push_device_for_test(Device::new(
                idx,
                "p150a".to_string(),
                bus.to_string(),
                String::new(),
            ));
            backend.sysfs.insert_telemetry_for_test(
                idx,
                Telemetry {
                    power: Some(20.0 + idx as f32),
                    current: None,
                    voltage: Some(0.72),
                    asic_temperature: Some(40.0),
                    aiclk: None,
                    heartbeat: None,
                    timestamp: chrono::Utc::now(),
                    board_power: None,
                },
            );
        }
        backend.bus_to_index = backend.sysfs_bus_index();
        assert_eq!(backend.bus_to_index.len(), 4);

        // tt-smi reports only the 2nd and 4th card — note the *uppercase* hex in
        // one bus id, which the normalized join key handles.
        let json = r#"{
            "device_info": [
                {
                    "board_info": {"bus_id": "0000:02:00.0", "board_type": "p150a",
                                   "pcie_speed": 4, "pcie_width": "16"},
                    "telemetry": {"power": " 21.0", "board_power": " 55.0"},
                    "smbus_telem": {"BOARD_ID_LOW": "0xCARD02", "AICLK": "800"}
                },
                {
                    "board_info": {"bus_id": "0000:04:00.0", "board_type": "p150a",
                                   "pcie_speed": 5, "pcie_width": "8"},
                    "telemetry": {"power": " 23.0", "board_power": " 77.0"},
                    "smbus_telem": {"BOARD_ID_LOW": "0xCARD04", "AICLK": "1000"}
                }
            ]
        }"#;
        publish_snapshot(
            json,
            &backend.bus_to_index,
            &backend.smbus_shared,
            &backend.device_meta_shared,
            &backend.smbus_generation,
        );
        backend.update().expect("update should not fail");

        // ── SMBUS lands on the right cards ────────────────────────────────────
        assert_eq!(
            backend
                .smbus_telemetry(1)
                .and_then(|s| s.board_id.as_deref()),
            Some("0xCARD02"),
            "the 0000:02:00.0 record must land on sysfs device 1"
        );
        assert_eq!(
            backend
                .smbus_telemetry(3)
                .and_then(|s| s.board_id.as_deref()),
            Some("0xCARD04"),
            "the 0000:04:00.0 record must land on sysfs device 3"
        );
        assert!(
            backend.smbus_telemetry(0).is_none(),
            "device 0 was absent from the snapshot — it must have no SMBUS, \
             not device 1's"
        );
        assert!(backend.smbus_telemetry(2).is_none(), "same for device 2");

        // ── PCIe link geometry follows the same join ──────────────────────────
        let devices = backend.devices();
        assert_eq!(devices[1].pcie_speed.as_deref(), Some("Gen4"));
        assert_eq!(devices[1].pcie_width, Some(16));
        assert_eq!(devices[3].pcie_speed.as_deref(), Some("Gen5"));
        assert_eq!(devices[3].pcie_width, Some(8));
        assert!(devices[0].pcie_speed.is_none() && devices[2].pcie_speed.is_none());

        // ── board_power overlay (applied every tick) follows it too ───────────
        assert_eq!(backend.telemetry(1).and_then(|t| t.board_power), Some(55.0));
        assert_eq!(backend.telemetry(3).and_then(|t| t.board_power), Some(77.0));
        assert!(
            backend.telemetry(0).and_then(|t| t.board_power).is_none(),
            "board power must not shift onto a card tt-smi never reported"
        );
        assert!(backend.telemetry(2).and_then(|t| t.board_power).is_none());
    }

    /// A snapshot entry with no `bus_id` at all (a tt-smi old enough to omit it)
    /// falls back to a positional join for that entry rather than being dropped.
    #[test]
    fn snapshot_entries_without_bus_id_fall_back_to_positional_join() {
        let mut bus_to_index = HashMap::new();
        bus_to_index.insert("0000:01:00.0".to_string(), 0);
        bus_to_index.insert("0000:02:00.0".to_string(), 1);

        let json = r#"{
            "device_info": [
                {"smbus_telem": {"BOARD_ID_LOW": "0xNOBUS"}},
                {"board_info": {"bus_id": "0000:02:00.0"},
                 "smbus_telem": {"BOARD_ID_LOW": "0xCARD02"}}
            ]
        }"#;
        let parts = rekey_snapshot_by_bus_id(json::parse_snapshot(json), &bus_to_index);
        assert_eq!(
            parts.smbus.get(&0).and_then(|s| s.board_id.as_deref()),
            Some("0xNOBUS"),
            "no bus id → keep the array position"
        );
        assert_eq!(
            parts.smbus.get(&1).and_then(|s| s.board_id.as_deref()),
            Some("0xCARD02")
        );
    }

    /// With no resolvable sysfs bus ids (every device fell back to its hwmon
    /// directory name) the join degrades to positional rather than dropping
    /// everything.
    #[test]
    fn empty_bus_index_degrades_to_positional_join() {
        let json = r#"{
            "device_info": [
                {"board_info": {"bus_id": "0000:01:00.0"},
                 "smbus_telem": {"BOARD_ID_LOW": "0xA"}},
                {"board_info": {"bus_id": "0000:02:00.0"},
                 "smbus_telem": {"BOARD_ID_LOW": "0xB"}}
            ]
        }"#;
        let parts = rekey_snapshot_by_bus_id(json::parse_snapshot(json), &HashMap::new());
        assert_eq!(
            parts.smbus.get(&0).and_then(|s| s.board_id.as_deref()),
            Some("0xA")
        );
        assert_eq!(
            parts.smbus.get(&1).and_then(|s| s.board_id.as_deref()),
            Some("0xB")
        );
    }

    /// A card tt-smi reports but sysfs can't see has nowhere to go — it is
    /// dropped, never attributed to some other index.
    #[test]
    fn unmatched_bus_id_is_dropped_not_reassigned() {
        let mut bus_to_index = HashMap::new();
        bus_to_index.insert("0000:01:00.0".to_string(), 0);

        let json = r#"{
            "device_info": [
                {"board_info": {"bus_id": "0000:09:00.0"},
                 "smbus_telem": {"BOARD_ID_LOW": "0xGHOST"}}
            ]
        }"#;
        let parts = rekey_snapshot_by_bus_id(json::parse_snapshot(json), &bus_to_index);
        assert!(
            parts.smbus.is_empty(),
            "an unmatched card must not be attributed to device 0"
        );
    }

    /// A snapshot carrying `board_info`/`telemetry` but no `smbus_telem` block
    /// must still publish its PCIe link geometry and board power.
    ///
    /// Gating the whole payload on a non-empty SMBUS map meant a
    /// permission-restricted `tt-smi` run — or a card whose ARC didn't answer
    /// the SMBUS read — silently cost the PCIe row and the Board power row,
    /// neither of which comes from SMBUS.
    #[test]
    fn meta_is_published_even_when_the_snapshot_has_no_smbus_block() {
        use crate::models::{Device, Telemetry};

        let mut backend = HybridBackend::new("tt-smi");
        backend.sysfs.push_device_for_test(Device::new(
            0,
            "p300c".to_string(),
            "0000:01:00.0".to_string(),
            String::new(),
        ));
        backend.sysfs.insert_telemetry_for_test(
            0,
            Telemetry {
                power: Some(17.0),
                current: None,
                voltage: Some(0.72),
                asic_temperature: Some(39.0),
                aiclk: None,
                heartbeat: None,
                timestamp: chrono::Utc::now(),
                board_power: None,
            },
        );
        backend.bus_to_index = backend.sysfs_bus_index();

        let json = r#"{
            "device_info": [
                {
                    "board_info": {"bus_id": "0000:01:00.0", "board_type": "p300c",
                                   "pcie_speed": 4, "pcie_width": "4"},
                    "telemetry": {"power": " 17.0", "board_power": " 42.0"}
                }
            ]
        }"#;
        publish_snapshot(
            json,
            &backend.bus_to_index,
            &backend.smbus_shared,
            &backend.device_meta_shared,
            &backend.smbus_generation,
        );
        backend.update().expect("update should not fail");

        assert_eq!(
            backend.telemetry(0).and_then(|t| t.board_power),
            Some(42.0),
            "board power is parsed from `telemetry`, not SMBUS — it must survive"
        );
        let dev = &backend.devices()[0];
        assert_eq!(dev.pcie_speed.as_deref(), Some("Gen4"));
        assert_eq!(dev.pcie_width, Some(4));
    }

    /// `board_power` must expire when the tt-smi reader stops producing.
    ///
    /// The overlay re-applied the last stored payload on every tick, and nothing
    /// ever reset it — so a reader whose subprocess died mid-run kept stamping a
    /// frozen "Board NW total" row onto freshly-read sysfs telemetry, directly
    /// beneath a Power row that *was* still live, with nothing to distinguish
    /// the two.
    #[test]
    fn board_power_expires_when_the_meta_payload_goes_stale() {
        use crate::models::{Device, Telemetry};

        let mut backend = HybridBackend::new("tt-smi");
        backend.sysfs.push_device_for_test(Device::new(
            0,
            "p300c".to_string(),
            "0000:01:00.0".to_string(),
            String::new(),
        ));
        let seed_telemetry = |b: &mut HybridBackend| {
            b.sysfs.insert_telemetry_for_test(
                0,
                Telemetry {
                    power: Some(17.0),
                    current: None,
                    voltage: Some(0.72),
                    asic_temperature: Some(39.0),
                    aiclk: None,
                    heartbeat: None,
                    timestamp: chrono::Utc::now(),
                    // sysfs rebuilds this as None every tick; only the overlay
                    // ever sets it.
                    board_power: None,
                },
            );
        };
        seed_telemetry(&mut backend);
        backend.bus_to_index = backend.sysfs_bus_index();

        let json = r#"{
            "device_info": [
                {
                    "board_info": {"bus_id": "0000:01:00.0", "board_type": "p300c"},
                    "telemetry": {"power": " 17.0", "board_power": " 42.0"}
                }
            ]
        }"#;
        publish_snapshot(
            json,
            &backend.bus_to_index,
            &backend.smbus_shared,
            &backend.device_meta_shared,
            &backend.smbus_generation,
        );
        backend.update().expect("update should not fail");
        assert_eq!(
            backend.telemetry(0).and_then(|t| t.board_power),
            Some(42.0),
            "a fresh payload must be applied"
        );

        // Age the published payload past the freshness window — what a reader
        // thread that stopped polling looks like from the render thread.
        let stale = backend.device_meta_shared.load_full();
        let aged = std::time::Instant::now()
            .checked_sub(BOARD_POWER_MAX_AGE + Duration::from_secs(1))
            .expect("monotonic clock far enough from its origin");
        backend.device_meta_shared.store(Arc::new(MetaSnapshot {
            at: Some(aged),
            meta: stale.meta.clone(),
        }));

        seed_telemetry(&mut backend); // next sysfs tick: board_power back to None
        backend.update().expect("update should not fail");
        assert_eq!(
            backend.telemetry(0).and_then(|t| t.board_power),
            None,
            "a stale payload must drop the Board row, not keep stamping it"
        );
        assert_eq!(
            backend.telemetry(0).and_then(|t| t.power),
            Some(17.0),
            "the live sysfs Power reading is unaffected"
        );
    }

    /// A bus-id-less entry must never land on an index a bus-id match already
    /// claimed — the unconditional `mapping.insert(pos, pos)` fallback let two
    /// snapshot entries resolve to the same device index, and the `HashMap`
    /// collect in `rekey_map` then kept whichever pair the iterator happened to
    /// emit last. One card's SMBUS/meta on the wrong index, the other card with
    /// none at all, and the outcome varying run to run.
    #[test]
    fn positional_fallback_never_overwrites_a_bus_id_match() {
        let mut bus_to_index = HashMap::new();
        bus_to_index.insert("0000:01:00.0".to_string(), 0);
        bus_to_index.insert("0000:02:00.0".to_string(), 1);

        // Entry 0 carries no bus id (→ position 0 by fallback); entry 1 resolves
        // by bus id to device 0 as well.
        let json = r#"{
            "device_info": [
                {"board_info": {"board_type": "p300c"},
                 "smbus_telem": {"BOARD_ID_LOW": "0xNOBUS"}},
                {"board_info": {"bus_id": "0000:01:00.0"},
                 "smbus_telem": {"BOARD_ID_LOW": "0xREAL"}}
            ]
        }"#;

        // Run it repeatedly: the old collision was decided by HashMap iteration
        // order, so a single pass could pass by luck.
        for _ in 0..32 {
            let parts = rekey_snapshot_by_bus_id(json::parse_snapshot(json), &bus_to_index);
            assert_eq!(
                parts.smbus.len(),
                1,
                "the bus-id-less entry must be dropped, not collide onto index 0"
            );
            assert_eq!(
                parts.smbus.get(&0).and_then(|s| s.board_id.as_deref()),
                Some("0xREAL"),
                "the bus-id match owns index 0"
            );
        }
    }

    /// HybridBackend must overlay SMBUS TDP/TDC onto sysfs telemetry.
    ///
    /// Verifies the fix for firmware versions (e.g. 19.10.0) where the SMBUS
    /// TDP register reports total board power (Tensix + GDDR) while the hwmon
    /// driver only exposes the vcore rail — causing sysfs to read ~19W and
    /// tt-smi to read ~40W for the same device.
    #[test]
    fn test_smbus_tdp_tdc_overlay() {
        use crate::models::Telemetry;

        let mut backend = HybridBackend::new("tt-smi");

        // Seed sysfs cache with vcore-only readings (mimics hwmon power1_input).
        backend.sysfs.insert_telemetry_for_test(
            0,
            Telemetry {
                power: Some(19.0),   // hwmon vcore-only — will be overwritten
                current: Some(26.0), // hwmon vcore current — will be overwritten
                voltage: Some(0.72),
                asic_temperature: Some(42.0),
                aiclk: None,
                heartbeat: None,
                timestamp: chrono::Utc::now(),
                board_power: None,
            },
        );

        // Simulate SMBUS blended data reporting firmware total-board power.
        backend.smbus_blended.insert(
            0,
            SmbusTelemetry {
                tdp: Some("40".to_string()), // firmware total-board power (Tensix + GDDR)
                tdc: Some("55".to_string()), // firmware total current
                ..SmbusTelemetry::default()
            },
        );

        // Exercise the real update() path.  sysfs.update() is a no-op when
        // `devices` is empty, so the cache entry seeded above is preserved and
        // the overlay runs through the production code path.
        backend.update().expect("update should not fail");

        let telem = backend.sysfs.telemetry(0).unwrap();
        assert_eq!(
            telem.power,
            Some(40.0),
            "power must be SMBUS TDP, not hwmon vcore"
        );
        assert_eq!(
            telem.current,
            Some(55.0),
            "current must be SMBUS TDC, not hwmon vcore current"
        );
        assert_eq!(
            telem.voltage,
            Some(0.72),
            "voltage must be preserved from sysfs"
        );
    }
}
