// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Luwen backend for direct hardware access
//!
//! This backend uses the official Tenstorrent `luwen-api` / `luwen-pci` /
//! `luwen-def` crates (crates.io, published from tenstorrent/luwen) to
//! communicate directly with hardware via PCIe, providing the fastest and most
//! efficient telemetry access.
//!
//! Previously this backend was built on the third-party `all-smi-luwen-*`
//! forks, which have been stale since 2025-07 (missing Blackhole telemetry
//! tags and a negative 16.16-fixed-point temperature decode fix). Migrated
//! to the official 0.8.5 crates to pick up full Blackhole telemetry (GDDR
//! temps/ECC, harvesting, enabled masks, therm trips, input/board power).
//!
//! # Architecture
//!
//! - `luwen-pci` owns the PCIe transport *and* local device enumeration
//!   (`PciDevice::scan()` → `ExtendedPciDevice::open()` → `Chip::open()`),
//!   wrapped up as `luwen_pci::detect_chips_silent(options)`.
//! - `luwen-api` owns the protocol/arch layer: `Chip`, `ChipImpl::get_telemetry`,
//!   and the per-arch telemetry decode. It has **no** hardware transport of its
//!   own — `luwen_api::detect_chips_silent(root_chips, …)` walks outward from
//!   chips it is *handed*, so it can't be used to find local devices.
//! - Bypasses subprocess overhead (unlike JSON backend)
//! - Direct memory-mapped I/O for telemetry reads
//! - Supports all Tenstorrent architectures (Grayskull, Wormhole, Blackhole)
//!
//! # Known differences from the sysfs / hybrid backends
//!
//! * **No PCIe link row.** The Insights sidebar's `PCIe Gen4 ×16` line is
//!   absent on this backend by design. luwen exposes no `pcie_bandwidth()`
//!   equivalent and carries no link *speed* or *width* source at all — the one
//!   PCIe register it does surface is `PCIE_USAGE` (a lane count, published via
//!   [`crate::models::SmbusTelemetry::pcie_status`]). sysfs reads link geometry
//!   from `/sys/bus/pci/devices/*/current_link_{speed,width}` and tt-smi reports
//!   it in `board_info`; neither path exists here, and inventing a plausible
//!   "Gen4 ×16" would be a fabricated reading. If this is ever needed, the honest
//!   fix is to read the PCI config space / sysfs link attributes for the chip's
//!   own bus id — i.e. borrow the sysfs backend's reader — not to guess from
//!   luwen telemetry.
//! * **Limits are Blackhole-only.** `Device::limits` comes from BH telemetry
//!   tags that Wormhole never assigns; see [`map_limits`].
//!
//! # Verification status
//!
//! **Exercised on hardware: 4× Blackhole p300c (tt-kmd 2.9.0), idle.** The
//! backend detects all four chips and its readings agree with the sysfs/hybrid
//! path and `tt-smi -s` for power, current, ASIC temperature, GDDR
//! temperatures and ETH link state. Two mappings that this migration
//! specifically had to get right were confirmed live: ARC health comes from
//! `telemetry_heartbeat()` (Blackhole never assigns the per-ARC registers, so
//! the raw fields would read 0 and paint every healthy card as firmware-
//! stalled), and `ENABLED_ETH`/`EthLiveStatus` together render `12/12 live`
//! rather than a dark `0/12`.
//!
//! Still unverified, deliberately:
//! * **Wormhole and Grayskull** — no such silicon on the development box, so
//!   the WH register layout is covered only by unit tests over hand-built
//!   `Telemetry` values, and the arch gating in [`map_limits`] is what keeps WH
//!   from publishing Blackhole-only tags as a confident `0`.
//! * **Behaviour under load** — every hardware check so far ran against idle
//!   cards. Luwen/UMD arbitration is unresolved upstream (DEVINFRA-4445), which
//!   is why this backend stays explicit-only and is never auto-detected.
//!
//! # Differences from the safe backends
//!
//! Expect these, they are not faults:
//! * **Slower startup.** Detection scans PCI and issues a scratch-register read
//!   per chip, where sysfs just opens files — a visible fraction of a second
//!   rather than instant.
//! * **No PCIe row.** Link bandwidth comes from tt-kmd's `pcie_perf_counters/`
//!   and the geometry from `tt-smi`'s `board_info`; neither has a luwen source,
//!   so [`TelemetryBackend::pcie_bandwidth`] keeps the `None` default here and
//!   the Insights PCIe row is absent rather than fabricated.

#[cfg(feature = "luwen-backend")]
use luwen_api::chip::{Chip, ChipImpl};

#[cfg(feature = "luwen-backend")]
use luwen_api::ChipDetectOptions;

#[cfg(feature = "luwen-backend")]
use luwen_def::Arch;

#[cfg(feature = "luwen-backend")]
use crate::models::telemetry::{thm_limit_shutdown_c, unpack_gddr_temps_u32};

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::error::{BackendError, BackendResult};
use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use std::collections::HashMap;

/// Map a luwen architecture tag onto our own enum.
///
/// Grayskull is `#[deprecated]` in `luwen_def::Arch` ("legacy architecture that
/// is no longer supported") but the variant still exists — keep mapping it for
/// completeness rather than silently dropping support. The allow is scoped to
/// just that arm (upstream only deprecated the one variant; Wormhole/Blackhole
/// shouldn't be silently exempted from future deprecation warnings too).
#[cfg(feature = "luwen-backend")]
fn map_arch(arch: Arch) -> Architecture {
    match arch {
        #[allow(deprecated)]
        Arch::Grayskull => Architecture::Grayskull,
        Arch::Wormhole => Architecture::Wormhole,
        Arch::Blackhole => Architecture::Blackhole,
    }
}

/// Outcome of triaging one detection sweep by ARC liveness.
///
/// See [`triage_chips`] for what "ARC liveness" means to an operator and why the
/// positions in here are load-bearing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChipTriage {
    /// Detection-order positions of the chips whose ARC answered — i.e. the
    /// [`Device::index`] each survivor must be published under.
    ///
    /// Deliberately the *original* positions, not `0..live.len()`: this is the
    /// only handle the operator has on which physical card a panel refers to.
    live: Vec<usize>,

    /// How many chips the PCI scan turned up in total, live or not.
    detected: usize,
}

impl ChipTriage {
    /// Chips that were detected on the bus but had to be dropped.
    fn skipped(&self) -> usize {
        self.detected.saturating_sub(self.live.len())
    }
}

/// Triage a detection sweep: which chips can be monitored, under which indices.
///
/// Takes the per-chip `arc_alive()` answers in **detection order** (so
/// `arc_alive[2]` is the third chip the PCI scan turned up) and returns the
/// positions that survived, plus the total, so the caller can say how many it
/// dropped.
///
/// # What "ARC not responding" means for the operator
///
/// The ARC is the small management core on each Tenstorrent ASIC that owns the
/// telemetry mailbox — every power, current, temperature, clock and ETH reading
/// this backend serves is read *through* it. A chip whose ARC doesn't answer a
/// scratch-register read is not necessarily broken silicon; the usual causes are
/// a wedged or mid-reset ARC firmware, a card that needs `tt-smi -r`, or a stale
/// tt-kmd after a driver reload. But it does mean the card has **no telemetry
/// surface at all** for as long as it stays that way, so there is nothing this
/// backend can render for it beyond "it exists and it is silent".
///
/// # Why the positions must be preserved
///
/// Dropping a dead chip and renumbering the survivors (`enumerate()` over the
/// survivors) is the mis-attribution bug this release exists to eliminate. On a
/// 4-card box whose card 0 has a wedged ARC, renumbering publishes three devices
/// labelled 0/1/2 that are physically cards 1/2/3 — so every per-index
/// reference (panel order, the Insights device selection, a saved layout, a
/// `--devices 0` filter) silently points one card off, and the operator who
/// power-cycles "card 0" on a thermal alarm pulls the wrong card. Keeping the
/// true positions makes the index set *sparse* instead of wrong, which is the
/// honest shape: consumers look telemetry up by [`Device::index`] (see the
/// `memory_castle`/`starfield` fixes on this branch), and the gap in the numbers
/// is itself the signal that a card is missing.
///
/// # Errors
///
/// * no chips detected at all — nothing is attached, or the PCI scan found
///   nothing this build can talk to;
/// * chips detected but **every** ARC silent — the caller must fail rather than
///   hand back an empty device list, or the TUI comes up looking like a healthy
///   box with no hardware in it.
fn triage_chips(arc_alive: &[bool]) -> BackendResult<ChipTriage> {
    if arc_alive.is_empty() {
        return Err(BackendError::Initialization("No devices found".to_string()));
    }

    let live: Vec<usize> = arc_alive
        .iter()
        .enumerate()
        .filter_map(|(pos, alive)| if *alive { Some(pos) } else { None })
        .collect();

    if live.is_empty() {
        return Err(BackendError::Initialization(format!(
            "Found {} chip(s) but none had a live ARC — no telemetry available. \
             The ARC firmware may be wedged or mid-reset; try `tt-smi -r` (or a \
             host reboot), and check `dmesg` for tt-kmd errors.",
            arc_alive.len()
        )));
    }

    Ok(ChipTriage {
        live,
        detected: arc_alive.len(),
    })
}

/// Render the footer's backend label, naming any chip this backend had to skip.
///
/// A skipped chip is otherwise invisible outside the log — the panel grid just
/// has one fewer card in it, which on a fleet you don't have memorised looks
/// exactly like a correctly-monitored smaller fleet. The label is the cheapest
/// place that is *already rendered* (the TUI footer shows `backend_info()`), so
/// the count travels with the screen the operator is looking at.
///
/// The fuller fix the review asks for is a **degraded device entry**: keep the
/// dead chip in `devices()` with its true index and let the panel grid draw it
/// greyed-out and labelled "ARC unresponsive", so the gap is visible in the
/// place the operator actually reads and the index set stays dense. That is a
/// new rendering concept (every consumer would need a "has no telemetry" state)
/// and so is deliberately out of scope for a bugfix release — but it is the
/// right end state, and this label is the honest interim.
fn backend_label(detected: usize, live: usize) -> String {
    let skipped = detected.saturating_sub(live);
    if skipped == 0 {
        // Includes the pre-`init()` state (0 detected, 0 live).
        "Luwen (Direct Hardware)".to_string()
    } else {
        format!(
            "Luwen (Direct Hardware; {} of {} chips — {} ARC-unresponsive)",
            live, detected, skipped
        )
    }
}

/// Map a luwen `Telemetry` reading onto our core [`Telemetry`] model.
///
/// Split out of `update()` so the mapping decisions are unit-testable from a
/// hand-built luwen `Telemetry` — this backend can't be exercised against
/// hardware from a box that is serving live inference.
#[cfg(feature = "luwen-backend")]
fn map_telemetry(luwen_telem: &luwen_api::chip::Telemetry) -> Telemetry {
    // Note: luwen returns f64, but our model uses f32.
    Telemetry {
        timestamp: chrono::Utc::now(),
        voltage: Some(luwen_telem.voltage() as f32),
        current: Some(luwen_telem.current() as f32),
        power: Some(luwen_telem.power() as f32),
        asic_temperature: Some(luwen_telem.asic_temperature() as f32),
        aiclk: Some(luwen_telem.ai_clk()),
        // telemetry_heartbeat() falls back correctly on BH
        // where the per-ARC health registers don't exist.
        heartbeat: Some(luwen_telem.telemetry_heartbeat()),
        // The official luwen Telemetry has no distinct whole-board
        // power rail (it does have asic_power, but that's not the
        // same measurement as tt-smi's board_power) — leave None.
        board_power: None,
    }
}

/// Map a luwen `Telemetry` reading onto our [`SmbusTelemetry`] model.
/// Field names carry over unchanged from the all-smi fork.
///
/// Split out of `update()` for the same reason as [`map_telemetry`]; see the
/// per-field comments for the arch-dependent decisions.
///
/// # Why architecture matters
///
/// Blackhole builds its `Telemetry` from a *tag table* (`blackhole.rs`) and
/// finishes with the struct's `Default`, so every field whose tag BH doesn't
/// emit reads 0 — indistinguishable from a real zero. Wormhole reads a
/// *fixed-offset* struct and likewise fills the BH-only fields from `Default`.
/// The fields below are therefore split by which arch actually populates them;
/// writing `Some(0)` for the other arch is what produced rows like
/// "0/12 ETH live" on a healthy card.
#[cfg(feature = "luwen-backend")]
fn map_smbus(luwen_telem: &luwen_api::chip::Telemetry) -> SmbusTelemetry {
    let arch = map_arch(luwen_telem.arch);
    let is_bh = matches!(arch, Architecture::Blackhole);
    SmbusTelemetry {
        board_id: Some(luwen_telem.board_id.to_string()),
        ddr_status: Some(luwen_telem.ddr_status.to_string()),
        ddr_speed: luwen_telem.ddr_speed.map(|v| v.to_string()),
        // On BH `eth_status0` holds the EthLiveStatus tag (see
        // eth_live_status below), NOT a WH-style ETH_STATUS0
        // register — don't publish it under that name, or the
        // Insights ETH-error check reads a healthy link mask
        // as a fault. Both are genuine status registers on WH.
        eth_status0: (!is_bh).then(|| luwen_telem.eth_status0.to_string()),
        eth_status1: (!is_bh).then(|| luwen_telem.eth_status1.to_string()),
        // `pcie_status` means PCIE_USAGE downstream (a lane
        // count: 4/8/16 — see json.rs, which maps tt-smi's
        // PCIE_USAGE here, and defrag.rs which parses it as
        // one). BH publishes that as the PcieUsage tag, which
        // luwen lands in `enabled_pcie`; `pcie_status` is
        // never assigned on BH so it read 0. WH's
        // `pcie_status` register is a *status* word, not a
        // lane count, so it stays None there rather than
        // feeding a wrong number to a lane-count consumer.
        pcie_status: is_bh.then(|| luwen_telem.enabled_pcie.to_string()),
        faults: Some(luwen_telem.faults.to_string()),
        arc0_fw_version: Some(luwen_telem.arc0_fw_version.to_string()),
        arc1_fw_version: Some(luwen_telem.arc1_fw_version.to_string()),
        arc2_fw_version: Some(luwen_telem.arc2_fw_version.to_string()),
        arc3_fw_version: Some(luwen_telem.arc3_fw_version.to_string()),
        eth_fw_version: Some(luwen_telem.eth_fw_version.to_string()),
        m3_bl_fw_version: Some(luwen_telem.m3_bl_fw_version.to_string()),
        m3_app_fw_version: Some(luwen_telem.m3_app_fw_version.to_string()),
        // Blackhole has no per-ARC health registers: its tag
        // table never assigns arc0..3_health, so they read 0
        // from `Default`. A permanent `Some(0)` here made
        // `InferenceEngine` classify every device Stalled
        // with Confidence::High after ~30 frames (an
        // unchanging observed counter is exactly its stall
        // signal). `telemetry_heartbeat()` is the accessor
        // that papers over the gap — timer_heartbeat on BH,
        // arc0_health on WH — and it puts the counter in the
        // same slot both safe backends use (json:
        // TIMER_HEARTBEAT, sysfs: tt_heartbeat).
        arc0_health: Some(luwen_telem.telemetry_heartbeat().to_string()),
        // arc1..3 stay None on BH so the stall detector
        // treats them as "not observed" rather than "frozen";
        // they're real registers on WH.
        arc1_health: (!is_bh).then(|| luwen_telem.arc1_health.to_string()),
        arc2_health: (!is_bh).then(|| luwen_telem.arc2_health.to_string()),
        arc3_health: (!is_bh).then(|| luwen_telem.arc3_health.to_string()),
        aiclk: Some(luwen_telem.aiclk.to_string()),
        axiclk: Some(luwen_telem.axiclk.to_string()),
        arcclk: Some(luwen_telem.arcclk.to_string()),
        vcore: Some(luwen_telem.vcore.to_string()),
        asic_temperature: Some(luwen_telem.asic_temperature.to_string()),
        vreg_temperature: Some(luwen_telem.vreg_temperature.to_string()),
        board_temperature: Some(luwen_telem.board_temperature.to_string()),
        tdp: Some(luwen_telem.tdp.to_string()),
        tdc: Some(luwen_telem.tdc.to_string()),
        throttler: Some(luwen_telem.throttler.to_string()),
        // BH exposes fan RPM through the FanRpm tag and leaves
        // FanSpeed at the 0 sentinel (the same split tt-smi
        // shows: FAN_SPEED=0x0, FAN_RPM=0x75a), so take
        // whichever register holds a real reading — FanSpeed
        // first for WH, where it's the only one populated.
        // Both sentinels → None, so `fan_rpm()` honestly
        // reports "no fan" on a fanless card.
        fan_speed: [luwen_telem.fan_speed, luwen_telem.fan_rpm]
            .into_iter()
            .find(|&v| !crate::models::telemetry::is_fan_sentinel(v))
            .map(|v| v.to_string()),
        vdd_limits: Some(luwen_telem.vdd_limits.to_string()),
        // `thm_limits` is a RAW register here, whereas json.rs
        // stores the already-decoded THM_LIMIT_SHUTDOWN in
        // plain °C — and starfield.rs reads this field
        // directly as a trip point. Publishing the WH packed
        // word made that comparison ~6.8M °C, so the
        // thermal-headroom cue could never fire. Decode to
        // the shared representation.
        thm_limits: Some(thm_limit_shutdown_c(luwen_telem.thm_limits, arch).to_string()),
        asic_tmon0: Some(luwen_telem.asic_tmon0.to_string()),
        asic_tmon1: Some(luwen_telem.asic_tmon1.to_string()),
        mvddq_power: Some(luwen_telem.mvddq_power.to_string()),
        gddr_train_temp0: Some(luwen_telem.gddr_train_temp0.to_string()),
        gddr_train_temp1: Some(luwen_telem.gddr_train_temp1.to_string()),
        boot_date: Some(luwen_telem.boot_date.to_string()),
        rt_seconds: Some(luwen_telem.rt_seconds.to_string()),
        eth_debug_status0: Some(luwen_telem.eth_debug_status0.to_string()),
        eth_debug_status1: Some(luwen_telem.eth_debug_status1.to_string()),
        tt_flash_version: Some(luwen_telem.tt_flash_version.to_string()),
        enum_version: Some(luwen_telem.enum_version.to_string()),
        device_id: Some(luwen_telem.device_id.to_string()),
        spibootrom_fw_version: Some(luwen_telem.spibootrom_fw_version.to_string()),
        wh_fw_date: Some(luwen_telem.wh_fw_date.to_string()),
        aux_status: luwen_telem.aux_status.map(|v| v.to_string()),
        // ── Blackhole-only telemetry tags ──────────────────
        // Everything from here down is emitted only by BH's
        // tag table; on WH these come from `Default` (0), and
        // publishing `Some(0)` renders as a real reading —
        // "0 W input power", "0/0 ETH live", every Tensix
        // column harvested. Gate them so WH shows "unknown"
        // instead of a confident zero.
        input_power: is_bh.then(|| luwen_telem.input_power.to_string()),
        board_power_limit: is_bh.then(|| luwen_telem.board_power_limit.to_string()),
        therm_trip_count: is_bh.then(|| luwen_telem.therm_trip_count.to_string()),
        // ── GDDR temps / ECC / harvesting — new in official 0.8.x ──
        gddr_temps: [
            is_bh.then(|| unpack_gddr_temps_u32(luwen_telem.gddr01_temp)),
            is_bh.then(|| unpack_gddr_temps_u32(luwen_telem.gddr23_temp)),
            is_bh.then(|| unpack_gddr_temps_u32(luwen_telem.gddr45_temp)),
            is_bh.then(|| unpack_gddr_temps_u32(luwen_telem.gddr67_temp)),
        ],
        max_gddr_temp: is_bh.then_some(luwen_telem.max_gddr_temp as f32),
        gddr_corr_errs: [
            is_bh.then_some(luwen_telem.gddr01_corr_errs),
            is_bh.then_some(luwen_telem.gddr23_corr_errs),
            is_bh.then_some(luwen_telem.gddr45_corr_errs),
            is_bh.then_some(luwen_telem.gddr67_corr_errs),
        ],
        gddr_uncorr_errs: is_bh.then_some(luwen_telem.gddr_uncorr_errs),
        harvesting_state: is_bh.then_some(luwen_telem.harvesting_state),
        // ETH: `enabled_eth` is the denominator and BH's
        // EthLiveStatus tag — which luwen stores in
        // `eth_status0` (blackhole.rs:
        // `TelemetryTags::EthLiveStatus => eth_status0`) — is
        // the numerator. Populating only the denominator, as
        // this branch first did, rendered "0/12 live" on a
        // healthy card; publishing both restores a truthful
        // count. `eth_live_status` is a u64 mask downstream
        // (tt-smi's ETH_LIVE_STATUS is 64-bit), so widen.
        eth_live_status: is_bh.then_some(luwen_telem.eth_status0 as u64),
        enabled_eth: is_bh.then_some(luwen_telem.enabled_eth),
        enabled_gddr: is_bh.then_some(luwen_telem.enabled_gddr),
        enabled_l2cpu: is_bh.then_some(luwen_telem.enabled_l2cpu),
        enabled_tensix_col: is_bh.then_some(luwen_telem.tensix_enabled_col),
        // luwen's Telemetry has no gddr_telemetry-equivalent tag today —
        // absent on both BH and WH until upstream adds one.
        gddr_telemetry: None,
        // No `..Default::default()`: every SmbusTelemetry
        // field is now mapped explicitly, so adding one
        // upstream fails the build here instead of silently
        // defaulting to None on this backend.
    }
}

/// Map a luwen `Telemetry` reading onto [`DeviceLimits`] — the per-board
/// firmware ceilings the Insights sidebar annotates its Power/Temp/Current rows
/// with.
///
/// Without this the sidebar falls back to the UI's generic constants (300 W /
/// 105 °C) and drops the Current row's `(lim …A)` text altogether, so a luwen
/// build disagreed with the safe backends side by side on the same card.
///
/// # Why this is Blackhole-only
///
/// The four registers below are Blackhole telemetry *tags*
/// (`TdpLimitMax`/`TdcLimitMax`/`AiclkLimitMax`/`ThmLimitThrottle`). Wormhole
/// reads a fixed-offset struct that never assigns them, so on WH they come from
/// `Default` and read 0 — and `Some(0.0)` renders as "(lim 0W)", which is worse
/// than no annotation at all. WH's *throttle* point is derivable from the high
/// half of its packed `thm_limits` register (that's what tt-smi's
/// `get_chip_limits()` does), but there is no WH source for the TDP/TDC/fmax
/// ceilings, so the block would still be mostly fabricated — it stays absent
/// until it can be verified against Wormhole hardware.
///
/// # Why `thm_limit_throttle`, not `thm_limits`
///
/// `DeviceLimits::thm_limit` is what the UI prints next to Temp, and it must
/// mean the same thing on every backend: the throttle trip point (90 °C on a
/// p300c), which is also hwmon's `temp1_max`. `thm_limits` carries the harder
/// *shutdown* trip (110 °C) and is published separately in
/// [`SmbusTelemetry::thm_limits`] for the starfield thermal-headroom cue.
#[cfg(feature = "luwen-backend")]
fn map_limits(luwen_telem: &luwen_api::chip::Telemetry) -> Option<crate::models::DeviceLimits> {
    if !matches!(map_arch(luwen_telem.arch), Architecture::Blackhole) {
        return None;
    }
    Some(crate::models::DeviceLimits {
        tdp_limit: Some(luwen_telem.tdp_limit_max as f32),
        tdc_limit: Some(luwen_telem.tdc_limit_max as f32),
        asic_fmax: Some(luwen_telem.aiclk_limit_max),
        thm_limit: Some(luwen_telem.thm_limit_throttle as f32),
    })
}

/// Map a luwen `Telemetry` reading onto [`FirmwaresInfo`] so the Insights FW row
/// renders a version instead of "—".
///
/// Unlike the limits above this is **not** arch-gated: Wormhole populates
/// `fw_bundle_version` too (from telemetry offset 49, whenever ARC firmware is
/// at least 2.25.0.0 — otherwise luwen leaves it 0). A zero register therefore
/// means "not reported" on either arch, and reporting it as "0.0.0.0" would be a
/// fake version, so the whole block is omitted in that case.
///
/// Only `fw_bundle_version` is filled: the component versions tt-smi shows
/// (`eth_fw`, `cm_fw`, `gddr_fw`) are rendered from the raw ARC/ETH registers
/// this backend already publishes through [`SmbusTelemetry`], and duplicating
/// them here with a different decode would risk two backends disagreeing.
#[cfg(feature = "luwen-backend")]
fn map_firmwares(luwen_telem: &luwen_api::chip::Telemetry) -> Option<crate::models::FirmwaresInfo> {
    if luwen_telem.fw_bundle_version == 0 {
        return None;
    }
    Some(crate::models::FirmwaresInfo {
        fw_bundle_version: Some(crate::models::fw_bundle_version_string(
            luwen_telem.fw_bundle_version,
        )),
        ..Default::default()
    })
}

/// Luwen backend implementation for direct hardware access
pub struct LuwenBackend {
    /// Backend configuration
    config: BackendConfig,

    /// Detected devices
    devices: Vec<Device>,

    /// Cached telemetry data (per device index)
    telemetry_cache: HashMap<usize, Telemetry>,

    /// Cached SMBUS telemetry (per device index)
    smbus_cache: HashMap<usize, SmbusTelemetry>,

    /// Luwen chip handles, each paired with the **physical** detection-order
    /// position it was found at — which is also the [`Device::index`] it is
    /// published under and the key its telemetry is cached by.
    ///
    /// The pairing is the whole point: this vector is dense (a chip with a dead
    /// ARC has no handle to store) while the index space is sparse, so the
    /// handle's position in here must never be used as a device index. See
    /// [`triage_chips`].
    #[cfg(feature = "luwen-backend")]
    chips: Vec<(usize, Chip)>,

    /// How many chips the last detection sweep found on the bus, including any
    /// whose ARC was silent and were therefore not registered as devices.
    /// Compared against `devices.len()` by [`backend_label`] so the skip is
    /// visible in the footer rather than only in the log.
    detected_chips: usize,

    /// Consecutive `update()` calls in which *no* chip's telemetry could be
    /// read. Compared against `config.max_consecutive_errors` so a card that has
    /// gone away surfaces as an error instead of silently freezing on stale
    /// telemetry forever. Reset by any successful read.
    consecutive_errors: usize,
}

impl LuwenBackend {
    /// Create a new Luwen backend with default configuration
    pub fn new() -> Self {
        Self::with_config(BackendConfig::default())
    }

    /// Create a new Luwen backend with custom configuration
    pub fn with_config(config: BackendConfig) -> Self {
        Self {
            config,
            devices: Vec::new(),
            telemetry_cache: HashMap::new(),
            smbus_cache: HashMap::new(),
            #[cfg(feature = "luwen-backend")]
            chips: Vec::new(),
            detected_chips: 0,
            consecutive_errors: 0,
        }
    }

    /// Detect and initialize hardware devices
    #[cfg(feature = "luwen-backend")]
    fn detect_devices(&mut self) -> BackendResult<()> {
        log::info!("LuwenBackend: Detecting devices...");

        // Try with noc_safe first (safer for active hardware)
        let options = ChipDetectOptions {
            local_only: true,          // Only detect locally attached devices
            noc_safe: true,            // Use safer NoC access (won't interfere with workloads)
            continue_on_failure: true, // Try all devices even if some fail
            ..Default::default()
        };

        log::info!("LuwenBackend: Trying noc_safe mode (non-invasive for active workloads)");
        // Enumerate through luwen-pci, NOT luwen-api.
        //
        // `luwen_pci::detect_chips_silent(options)` does the PCI scan itself
        // (`PciDevice::scan()` → `ExtendedPciDevice::open()` → `Chip::open()`,
        // then hands the resulting root chips to `luwen_api::detect_chips`) —
        // this is exactly the path upstream's own `detect_chips`/
        // `detect_local_chips` helpers take, differing only in that we supply
        // our own noc_safe options and stay silent. `luwen_api`'s same-named
        // function takes the root chips as its *first argument* and enumerates
        // outward from them, so calling it with `Vec::new()` (as this backend
        // did) can only ever return an empty list.
        let detected = luwen_pci::detect_chips_silent(options)
            .map_err(|e| BackendError::Initialization(format!("Device detection failed: {}", e)))?;

        log::info!("LuwenBackend: Found {} candidate chips", detected.len());

        // Detection yields `UninitChip`s. We deliberately do NOT call `init()`
        // (which re-runs `wait_for_init` and can block on DRAM/ETH bring-up we
        // have no business driving on a card that is busy serving) — telemetry
        // lives behind the ARC, so `arc_alive()` is the whole precondition, and
        // `upgrade()` just unwraps the already-open chip handle.
        //
        // `arc_alive()` is a scratch-register read, so ask each chip exactly
        // once and carry the answers: the triage decision, the log lines and the
        // footer's skip count all work off this one sweep.
        let arc_alive: Vec<bool> = detected.iter().map(|uninit| uninit.arc_alive()).collect();

        // Errors when nothing was detected, or when every ARC was silent — in
        // the latter case a card *is* present and we must say so rather than
        // hand back an empty device list that reads as "no hardware installed".
        let triage = triage_chips(&arc_alive)?;
        self.detected_chips = triage.detected;

        if triage.skipped() > 0 {
            log::warn!(
                "LuwenBackend: {} of {} chips have no live ARC and cannot be monitored; \
                 the surviving chips keep their true indices, so the device list is sparse",
                triage.skipped(),
                triage.detected
            );
        }

        // Pair every surviving handle with its **physical** detection-order
        // position. Zipping the liveness answers back over the chips keeps that
        // position attached; collapsing to `enumerate()` over the survivors is
        // exactly the renumbering bug this pairing exists to prevent.
        let mut chips: Vec<(usize, Chip)> = Vec::with_capacity(triage.live.len());
        for (pos, (uninit, alive)) in detected.into_iter().zip(&arc_alive).enumerate() {
            if !*alive {
                log::warn!(
                    "LuwenBackend: skipping chip {pos} — ARC not responding, no telemetry \
                     available. Index {pos} stays reserved for it; the remaining chips keep \
                     their own indices and are NOT renumbered."
                );
                continue;
            }
            chips.push((pos, uninit.upgrade()));
        }
        debug_assert_eq!(
            chips.iter().map(|(pos, _)| *pos).collect::<Vec<_>>(),
            triage.live,
            "triage and the kept handles must agree on the surviving positions"
        );

        // Register each chip whose ARC answered, under its physical index.
        for (idx, chip) in chips {
            // Get architecture
            let arch = chip.get_arch();
            let architecture = map_arch(arch);

            // Get device info for better identification
            let device_info = chip.get_device_info().ok().flatten();
            let bus_id = if let Some(info) = &device_info {
                format!("{:?}", info) // Will include PCI address
            } else {
                format!("pci:{}", idx)
            };

            // One telemetry read serves three purposes: the board type, the
            // firmware-limit ceilings, and the firmware bundle version. All
            // three are static device metadata, so they are read once here at
            // detect time rather than on every `update()` tick — and reading
            // once keeps the touch on a possibly-busy card minimal.
            let detect_telem = chip.get_telemetry().ok();
            let board_type = match &detect_telem {
                Some(telem) => telem.board_type().to_string(),
                None => format!("{:?}", arch),
            };

            // Create device.
            //
            // `index` is the chip's physical detection-order position, so a
            // skipped ARC-dead chip leaves a *gap* in the index set rather than
            // shifting its healthy neighbours down by one. Consumers address
            // telemetry by this value (never by list position) — see the
            // memory_castle/starfield/arcade fixes on this branch.
            let device = Device {
                index: idx,
                board_type,
                bus_id,
                coords: String::new(), // Coordinates not provided by luwen-if
                architecture,
                firmwares: detect_telem.as_ref().and_then(map_firmwares),
                limits: detect_telem.as_ref().and_then(map_limits),
                // No PCIe link geometry on this backend: luwen exposes no
                // `pcie_bandwidth()` and has no source for link speed/width
                // (only PCIE_USAGE, a lane count, which is published through
                // SmbusTelemetry::pcie_status). See the module header — the
                // PCIe row is legitimately absent here rather than faked.
                pcie_speed: None,
                pcie_width: None,
                grid_override: None,
                channels_override: None,
            };

            self.devices.push(device);

            // Store chip handle for telemetry reads, still paired with its
            // physical index — `update()` keys the caches by that, not by the
            // handle's position in this vector.
            self.chips.push((idx, chip));
        }

        log::info!(
            "LuwenBackend: Successfully initialized {} devices",
            self.devices.len()
        );
        Ok(())
    }

    #[cfg(not(feature = "luwen-backend"))]
    fn detect_devices(&mut self) -> BackendResult<()> {
        Err(BackendError::Initialization(
            "Luwen backend not enabled. Rebuild with --features luwen-backend".to_string(),
        ))
    }
}

impl Default for LuwenBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryBackend for LuwenBackend {
    fn init(&mut self) -> BackendResult<()> {
        self.detect_devices()?;
        Ok(())
    }

    fn update(&mut self) -> BackendResult<()> {
        #[cfg(feature = "luwen-backend")]
        {
            use luwen_api::chip::ChipImpl;

            let mut any_ok = false;

            // Read telemetry from each chip.
            //
            // `idx` comes off the stored pair — the chip's physical index —
            // never from the handle's position in `self.chips`. That vector is
            // dense while the index space can be sparse (an ARC-dead chip has no
            // handle), so enumerating it would cache card 1's watts under key 0
            // and every panel would read one card off.
            for (idx, chip) in self.chips.iter() {
                let idx = *idx;
                match chip.get_telemetry() {
                    Ok(luwen_telem) => {
                        any_ok = true;
                        let telemetry = map_telemetry(&luwen_telem);
                        let smbus = map_smbus(&luwen_telem);

                        // Cache the telemetry data
                        self.telemetry_cache.insert(idx, telemetry);
                        self.smbus_cache.insert(idx, smbus);
                    }
                    Err(e) => {
                        log::warn!(
                            "LuwenBackend: Failed to read telemetry for device {}: {:?}",
                            idx,
                            e
                        );
                        // Keep existing cached data on error (don't remove from cache)
                    }
                }
            }

            // Stale telemetry is preferable to a blank screen for a transient
            // read failure, but not forever: once no chip has answered for
            // `max_consecutive_errors` consecutive updates the device is gone
            // (surprise-removed, reset, or the ARC has wedged) and the caller
            // needs to know rather than keep rendering a frozen snapshot.
            if any_ok || self.chips.is_empty() {
                self.consecutive_errors = 0;
            } else {
                self.consecutive_errors += 1;
                if self.consecutive_errors >= self.config.max_consecutive_errors {
                    return Err(BackendError::Update(format!(
                        "no chip answered a telemetry read in {} consecutive updates",
                        self.consecutive_errors
                    )));
                }
            }

            Ok(())
        }

        #[cfg(not(feature = "luwen-backend"))]
        {
            Err(BackendError::Update(
                "Luwen backend not enabled".to_string(),
            ))
        }
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn device_count(&self) -> usize {
        self.devices.len()
    }

    fn telemetry(&self, device_idx: usize) -> Option<&Telemetry> {
        self.telemetry_cache.get(&device_idx)
    }

    fn smbus_telemetry(&self, device_idx: usize) -> Option<&SmbusTelemetry> {
        self.smbus_cache.get(&device_idx)
    }

    /// Backend label for the footer — carries the ARC-unresponsive skip count so
    /// a missing card is visible on screen and not only in the log. See
    /// [`backend_label`].
    fn backend_info(&self) -> String {
        backend_label(self.detected_chips, self.devices.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_luwen_backend_creation() {
        let backend = LuwenBackend::new();
        assert_eq!(backend.backend_info(), "Luwen (Direct Hardware)");
        assert_eq!(backend.device_count(), 0);
    }

    #[test]
    fn test_luwen_backend_with_config() {
        let config = BackendConfig::default().with_interval(50);
        let backend = LuwenBackend::with_config(config);
        assert_eq!(backend.config.update_interval_ms, 50);
    }

    /// Index-assignment triage over a detection sweep.
    ///
    /// `detect_devices()` itself can't run here (it scans PCI and pokes each
    /// chip's ARC; the development box serves live inference, which is why this
    /// backend is explicit-only), so the index arithmetic and the skip
    /// accounting are factored into [`triage_chips`] / [`backend_label`] and
    /// pinned here — the same pattern as the `map_telemetry` / `map_smbus`
    /// mapping tests below.
    mod triage {
        use super::*;

        /// The regression: a wedged ARC on card 0 must NOT renumber cards 1–3
        /// down to 0–2.
        ///
        /// Before the fix the survivors were re-enumerated, so a 4-card box with
        /// a dead card 0 came up showing three devices labelled 0/1/2 that were
        /// physically cards 1/2/3 — every per-index reference (panel order,
        /// device selection, saved layouts, `--devices N`) off by one, and an
        /// operator acting on a thermal alarm for "card 0" pulls the wrong card.
        #[test]
        fn dead_first_chip_keeps_the_survivors_true_indices() {
            let triage = triage_chips(&[false, true, true, true])
                .expect("three live ARCs is a monitorable box");

            assert_eq!(
                triage.live,
                vec![1, 2, 3],
                "survivors keep their physical detection-order positions"
            );
            assert_eq!(triage.detected, 4);
            assert_eq!(triage.skipped(), 1);
        }

        /// A gap in the middle behaves the same way — the index set is sparse,
        /// never compacted.
        #[test]
        fn dead_middle_chip_leaves_a_gap_rather_than_shifting() {
            let triage = triage_chips(&[true, false, true, true]).expect("three live ARCs");
            assert_eq!(triage.live, vec![0, 2, 3]);
            assert_eq!(triage.skipped(), 1);
        }

        /// The healthy box is untouched: dense 0..n, nothing skipped.
        #[test]
        fn all_live_chips_are_numbered_densely() {
            let triage = triage_chips(&[true, true, true, true]).expect("a healthy box");
            assert_eq!(triage.live, vec![0, 1, 2, 3]);
            assert_eq!(triage.detected, 4);
            assert_eq!(triage.skipped(), 0);
        }

        /// Cards present but every ARC silent must be an error, not an empty
        /// device list — otherwise the TUI comes up looking like a working box
        /// with no hardware installed, which reads as "tt-toplike is fine, the
        /// machine has no cards" instead of "your cards need a reset".
        #[test]
        fn all_arcs_dead_is_an_error_not_an_empty_fleet() {
            let err = triage_chips(&[false, false, false, false])
                .expect_err("a fleet with no telemetry surface must fail loudly");
            let msg = err.to_string();
            assert!(
                msg.contains("4 chip(s)") && msg.contains("live ARC"),
                "the error must say how many chips were found and why they're unusable: {msg}"
            );
            assert!(
                msg.contains("tt-smi -r"),
                "and point at the recovery an operator can actually perform: {msg}"
            );
        }

        /// Nothing detected at all keeps the pre-existing "No devices found"
        /// wording — a distinct condition from "found, but all wedged".
        #[test]
        fn no_chips_detected_is_a_distinct_error() {
            let err = triage_chips(&[]).expect_err("an empty sweep is an error");
            assert!(err.to_string().contains("No devices found"), "{err}");
        }

        /// The skip has to be visible somewhere the operator is already looking.
        /// `backend_info()` renders in the TUI footer, so the count rides along
        /// with it; a `log::warn!` alone is invisible inside a full-screen TUI.
        #[test]
        fn backend_label_names_the_skipped_chips() {
            assert_eq!(
                backend_label(4, 3),
                "Luwen (Direct Hardware; 3 of 4 chips — 1 ARC-unresponsive)"
            );
            assert_eq!(
                backend_label(4, 1),
                "Luwen (Direct Hardware; 1 of 4 chips — 3 ARC-unresponsive)"
            );
        }

        /// A healthy fleet (and the pre-`init()` state) keeps the plain label —
        /// no scary counts on a box where nothing is wrong.
        #[test]
        fn backend_label_stays_plain_when_nothing_was_skipped() {
            assert_eq!(backend_label(4, 4), "Luwen (Direct Hardware)");
            assert_eq!(backend_label(0, 0), "Luwen (Direct Hardware)");
        }
    }

    /// Mapping tests over a hand-built luwen `Telemetry`.
    ///
    /// This backend cannot be exercised against hardware here (the box it was
    /// developed on serves live inference, and luwen is explicit-only for exactly
    /// that reason), so the arch-dependent mapping decisions are pinned here
    /// instead. Each case mirrors a register layout confirmed in the luwen 0.8.5
    /// crate source.
    #[cfg(feature = "luwen-backend")]
    mod mapping {
        use super::*;
        use luwen_api::chip::Telemetry as LuwenTelemetry;

        /// A Blackhole reading as luwen builds it: only the tags BH's firmware
        /// emits are set; everything else stays at `Default` (0).
        fn bh_telemetry() -> LuwenTelemetry {
            LuwenTelemetry {
                arch: Arch::Blackhole,
                // BH publishes its heartbeat as TimerHeartbeat; arc0..3_health
                // are never assigned by the tag table.
                timer_heartbeat: 43874,
                // EthLiveStatus lands in eth_status0 on BH (blackhole.rs:782).
                eth_status0: 0x0FFF,
                enabled_eth: 0x0FFF,
                // PcieUsage lands in enabled_pcie (blackhole.rs:803).
                enabled_pcie: 16,
                // ThmLimits on BH is already a plain °C shutdown limit.
                thm_limits: 110,
                // FanRpm carries the reading; FanSpeed stays at the 0 sentinel.
                fan_rpm: 1882,
                gddr_uncorr_errs: 7,
                max_gddr_temp: 71,
                tensix_enabled_col: 0x3FFF,
                // Limit tags + packed bundle version, ground truth from a live
                // p300c (tt-smi 5.3.0 / tt-kmd 2.9.0).
                tdp_limit_max: 0x7d,            // 125 W
                tdc_limit_max: 0x1f4,           // 500 A
                aiclk_limit_max: 0x546,         // 1350 MHz
                thm_limit_throttle: 0x5a,       // 90 °C
                fw_bundle_version: 0x130b_0000, // "19.11.0.0"
                ..Default::default()
            }
        }

        /// A Wormhole reading: the fixed-offset struct fields are set, every
        /// BH-only tag field stays at `Default`.
        fn wh_telemetry() -> LuwenTelemetry {
            LuwenTelemetry {
                arch: Arch::Wormhole,
                arc0_health: 1234,
                arc1_health: 5678,
                // WH's real ETH status registers.
                eth_status0: 0x1,
                eth_status1: 0x2,
                // WH packs throttle (high half) and shutdown (low half).
                thm_limits: (100u32 << 16) | 110,
                fan_speed: 2500,
                ..Default::default()
            }
        }

        /// ARC health must come from the heartbeat accessor, and arc1..3 must be
        /// absent on BH — a permanent `Some(0)` there made `InferenceEngine`
        /// declare every device Stalled/High after ~30 frames.
        #[test]
        fn bh_arc_health_uses_timer_heartbeat_and_omits_arc1_3() {
            let s = map_smbus(&bh_telemetry());
            assert_eq!(s.arc0_health.as_deref(), Some("43874"));
            assert!(s.is_arc0_healthy(), "must not read as a stalled ARC");
            assert_eq!(s.arc1_health, None);
            assert_eq!(s.arc2_health, None);
            assert_eq!(s.arc3_health, None);
            // Core Telemetry carries the same counter.
            assert_eq!(map_telemetry(&bh_telemetry()).heartbeat, Some(43874));
        }

        /// Wormhole's per-ARC registers are real and must still come through.
        #[test]
        fn wh_arc_health_keeps_real_per_arc_registers() {
            let s = map_smbus(&wh_telemetry());
            assert_eq!(s.arc0_health.as_deref(), Some("1234"));
            assert_eq!(s.arc1_health.as_deref(), Some("5678"));
        }

        /// `thm_limits` must be plain °C on both arches so starfield's
        /// thermal-headroom cue compares against a real trip point rather than
        /// a ~6.8M packed word.
        #[test]
        fn thm_limits_is_decoded_to_celsius() {
            assert_eq!(
                map_smbus(&bh_telemetry()).thm_limits.as_deref(),
                Some("110")
            );
            assert_eq!(
                map_smbus(&wh_telemetry()).thm_limits.as_deref(),
                Some("110")
            );
        }

        /// ETH must report a live numerator alongside the enabled denominator.
        #[test]
        fn bh_eth_live_status_populated_with_enabled_eth() {
            let s = map_smbus(&bh_telemetry());
            assert_eq!(s.eth_live_status, Some(0x0FFF));
            assert_eq!(s.enabled_eth, Some(0x0FFF));
            assert_eq!(
                s.eth_live_status.unwrap().count_ones(),
                s.enabled_eth.unwrap().count_ones(),
                "healthy card must not render as 0/12 live"
            );
            // eth_status0 is the live mask on BH, not an error register — it must
            // not be republished under the WH name.
            assert_eq!(s.eth_status0, None);
        }

        /// On Wormhole neither ETH field is available, so both stay None and the
        /// row reads honestly as "0/?" rather than "0/0".
        #[test]
        fn wh_eth_masks_stay_absent() {
            let s = map_smbus(&wh_telemetry());
            assert_eq!(s.eth_live_status, None);
            assert_eq!(s.enabled_eth, None);
            // The genuine WH status registers do come through.
            assert_eq!(s.eth_status0.as_deref(), Some("1"));
            assert_eq!(s.eth_status1.as_deref(), Some("2"));
        }

        /// `pcie_status` is a lane count downstream; BH publishes it as
        /// PcieUsage → `enabled_pcie`.
        #[test]
        fn bh_pcie_status_comes_from_enabled_pcie() {
            assert_eq!(
                map_smbus(&bh_telemetry()).pcie_status.as_deref(),
                Some("16")
            );
            // WH's pcie_status register is a status word, not a lane count.
            assert_eq!(map_smbus(&wh_telemetry()).pcie_status, None);
        }

        /// Fan: BH leaves FanSpeed at 0 and puts RPM in FanRpm; WH populates
        /// FanSpeed only.
        #[test]
        fn fan_picks_whichever_register_has_a_real_rpm() {
            assert_eq!(map_smbus(&bh_telemetry()).fan_rpm(), Some(1882));
            assert_eq!(map_smbus(&wh_telemetry()).fan_rpm(), Some(2500));
            // Fanless card: both registers sentinel → no reading.
            let fanless = LuwenTelemetry {
                arch: Arch::Blackhole,
                fan_speed: 0,
                fan_rpm: 0xFFFF_FFFF,
                ..Default::default()
            };
            assert_eq!(map_smbus(&fanless).fan_speed, None);
            assert_eq!(map_smbus(&fanless).fan_rpm(), None);
        }

        /// BH-only telemetry tags must not be published as confident zeros on WH.
        #[test]
        fn bh_only_fields_are_absent_on_wormhole() {
            let bh = map_smbus(&bh_telemetry());
            assert_eq!(bh.gddr_uncorr_errs, Some(7));
            assert_eq!(bh.max_gddr_temp, Some(71.0));
            assert_eq!(bh.enabled_tensix_col, Some(0x3FFF));
            assert!(bh.gddr_temps.iter().all(|t| t.is_some()));

            let wh = map_smbus(&wh_telemetry());
            assert_eq!(wh.gddr_uncorr_errs, None);
            assert_eq!(wh.max_gddr_temp, None);
            assert_eq!(wh.enabled_tensix_col, None);
            assert_eq!(wh.harvesting_state, None);
            assert_eq!(wh.input_power, None);
            assert_eq!(wh.therm_trip_count, None);
            assert!(wh.gddr_temps.iter().all(|t| t.is_none()));
            assert!(wh.gddr_corr_errs.iter().all(|e| e.is_none()));
        }

        /// Blackhole's limit tags must reach `Device::limits`, or the Insights
        /// sidebar silently falls back to the UI's generic ceilings (300 W /
        /// 105 °C) and drops the Current row's `(lim …A)` annotation entirely.
        /// Values are ground truth from a live p300c (tt-smi 5.3.0):
        /// TDP_LIMIT_MAX=0x7d, TDC_LIMIT_MAX=0x1f4, AICLK_LIMIT_MAX=0x546,
        /// THM_LIMIT_THROTTLE=0x5a.
        #[test]
        fn bh_limits_come_from_the_limit_max_tags() {
            let lim = map_limits(&bh_telemetry()).expect("BH must report limits");
            assert_eq!(lim.tdp_limit, Some(125.0));
            assert_eq!(lim.tdc_limit, Some(500.0));
            assert_eq!(lim.asic_fmax, Some(1350));
            // The throttle trip point (90 °C), which is what hwmon reports as
            // temp1_max and what the safe backends display — NOT the 110 °C
            // shutdown trip in `thm_limits`.
            assert_eq!(lim.thm_limit, Some(90.0));
        }

        /// Wormhole's fixed-offset telemetry struct never assigns the
        /// `*_limit_max` / `thm_limit_throttle` fields, so they read 0 from
        /// `Default`. `Some(0.0)` would render "(lim 0W)" — strictly worse than
        /// no annotation — so the whole block stays absent on WH.
        #[test]
        fn wh_limits_are_absent_rather_than_zero() {
            assert!(
                map_limits(&wh_telemetry()).is_none(),
                "WH must not publish fabricated zero limits"
            );
        }

        /// The firmware bundle version is a packed register on this backend
        /// (tt-smi's JSON carries it pre-rendered), so it must be decoded to the
        /// same dotted string tt-smi shows — else the FW row renders "—".
        #[test]
        fn fw_bundle_version_is_decoded_for_the_fw_row() {
            let fw = map_firmwares(&bh_telemetry()).expect("BH must report a bundle version");
            assert_eq!(fw.fw_bundle_version.as_deref(), Some("19.11.0.0"));
            // WH populates the same register (only for ARC fw >= 2.25.0.0), so
            // it is not arch-gated — a real reading comes through.
            let wh = map_firmwares(&LuwenTelemetry {
                fw_bundle_version: 0x130b_0000,
                ..wh_telemetry()
            })
            .expect("WH with a populated bundle register must report it");
            assert_eq!(wh.fw_bundle_version.as_deref(), Some("19.11.0.0"));
            // A zero register means "not reported" (WH below the ARC firmware
            // threshold, or a BH tag table that omitted it) — not "0.0.0.0".
            assert!(map_firmwares(&wh_telemetry()).is_none());
        }

        #[test]
        fn arch_mapping_covers_all_luwen_variants() {
            assert_eq!(map_arch(Arch::Wormhole), Architecture::Wormhole);
            assert_eq!(map_arch(Arch::Blackhole), Architecture::Blackhole);
            #[allow(deprecated)]
            {
                assert_eq!(map_arch(Arch::Grayskull), Architecture::Grayskull);
            }
        }
    }
}
