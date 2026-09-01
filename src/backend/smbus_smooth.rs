// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! EMA smoothing for numeric SMBUS telemetry fields.
//!
//! When a fresh SMBUS snapshot arrives (every ~1.5 s in streaming mode), numeric
//! fields such as ARC health counters, DDR speed, and clock frequencies can jump
//! suddenly even when the underlying value hasn't meaningfully changed.  Rather
//! than hard-replacing every field on every snapshot, we blend each numeric value
//! toward the new reading using an exponential moving average.
//!
//! ## Algorithm
//!
//! ```text
//! smoothed_t = α * raw_t + (1 - α) * smoothed_{t-1}
//! ```
//!
//! With α = 0.25, a step change distributes across ~4 frames:
//! - Frame 0: +25% of the delta applied
//! - Frame 1: +44% cumulative
//! - Frame 2: +58% cumulative
//! - Frame 3: +68% cumulative
//!
//! At 100 ms/frame, a full transition takes ~400 ms — invisible to the human eye
//! but enough to avoid the sudden "surge" that characterised 5-second polling.
//!
//! ## Field policy
//!
//! - **Numeric strings** (parseable as f64): EMA-blended, re-formatted on output.
//!   Integer-looking originals (no `.`) are formatted as integers on output.
//! - **Non-numeric strings** (hex hashes, version strings, status bitmasks):
//!   copied verbatim — they carry no meaning as floats.
//! - **When `incoming` is `None`**: the existing value and EMA state are left
//!   unchanged (missing field in this snapshot doesn't mean the device lost it).
//! - **`fan_speed`**: numeric, but a *reported* firmware sentinel (`0`,
//!   `0xFFFF`, `0xFFFFFFFF`) means "no reading" and must replace rather than
//!   blend — see [`blend_fan`].

use crate::models::SmbusTelemetry;
use std::collections::HashMap;

/// Parse a string as f64, accepting decimal ("43.5"), plain integer ("1000"),
/// and hex ("0x3e80", "0x10e7a") formats.  Returns None for non-numeric strings
/// like firmware version strings, status bitmasks, and date stamps.
fn try_parse_numeric(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        // Hex integer — parse as u64 to handle large counters, then widen to f64.
        u64::from_str_radix(hex, 16).ok().map(|v| v as f64)
    } else {
        s.parse::<f64>().ok()
    }
}

/// Fraction of new value applied per frame (0.25 → ~4 frames to full transition).
const EMA_ALPHA: f64 = 0.25;

type FieldEma = Option<f64>;

/// Per-device EMA accumulators for each smoothable numeric field.
#[derive(Default)]
pub struct DeviceEmaState {
    ddr_speed: FieldEma,
    arc0_health: FieldEma,
    arc1_health: FieldEma,
    arc2_health: FieldEma,
    arc3_health: FieldEma,
    aiclk: FieldEma,
    axiclk: FieldEma,
    arcclk: FieldEma,
    asic_temperature: FieldEma,
    vreg_temperature: FieldEma,
    board_temperature: FieldEma,
    vcore: FieldEma,
    tdp: FieldEma,
    tdc: FieldEma,
    fan_speed: FieldEma,
    input_power: FieldEma,
    board_power_limit: FieldEma,
    mvddq_power: FieldEma,
    therm_trip_count: FieldEma,
    rt_seconds: FieldEma,
}

/// Map from device index to that device's per-field EMA state.
pub type SmbusEmaState = HashMap<usize, DeviceEmaState>;

/// Blend `incoming` into `existing` using EMA for numeric fields.
///
/// Discrete fields (board_id, firmware versions, DDR status bitmask, etc.) are
/// copied directly — they must not be blended as floats.
///
/// `ema` accumulates the float state between calls; pass the same `SmbusEmaState`
/// on every call to maintain continuity.
pub fn apply_ema(
    ema: &mut SmbusEmaState,
    device_idx: usize,
    incoming: &SmbusTelemetry,
    existing: &mut SmbusTelemetry,
) {
    let state = ema.entry(device_idx).or_default();

    // ── Discrete / identifier fields — copy verbatim ─────────────────────────
    copy_field(&incoming.board_id, &mut existing.board_id);
    copy_field(&incoming.device_id, &mut existing.device_id);
    copy_field(&incoming.enum_version, &mut existing.enum_version);
    copy_field(&incoming.ddr_status, &mut existing.ddr_status);
    copy_field(&incoming.arc0_fw_version, &mut existing.arc0_fw_version);
    copy_field(&incoming.arc1_fw_version, &mut existing.arc1_fw_version);
    copy_field(&incoming.arc2_fw_version, &mut existing.arc2_fw_version);
    copy_field(&incoming.arc3_fw_version, &mut existing.arc3_fw_version);
    copy_field(&incoming.eth_fw_version, &mut existing.eth_fw_version);
    copy_field(&incoming.m3_bl_fw_version, &mut existing.m3_bl_fw_version);
    copy_field(&incoming.m3_app_fw_version, &mut existing.m3_app_fw_version);
    copy_field(
        &incoming.spibootrom_fw_version,
        &mut existing.spibootrom_fw_version,
    );
    copy_field(&incoming.tt_flash_version, &mut existing.tt_flash_version);
    copy_field(&incoming.pcie_status, &mut existing.pcie_status);
    copy_field(&incoming.eth_status0, &mut existing.eth_status0);
    copy_field(&incoming.eth_status1, &mut existing.eth_status1);
    copy_field(&incoming.eth_debug_status0, &mut existing.eth_debug_status0);
    copy_field(&incoming.eth_debug_status1, &mut existing.eth_debug_status1);
    copy_field(&incoming.aux_status, &mut existing.aux_status);
    copy_field(&incoming.faults, &mut existing.faults);
    copy_field(&incoming.throttler, &mut existing.throttler);
    copy_field(&incoming.vdd_limits, &mut existing.vdd_limits);
    copy_field(&incoming.thm_limits, &mut existing.thm_limits);
    copy_field(&incoming.boot_date, &mut existing.boot_date);
    copy_field(&incoming.wh_fw_date, &mut existing.wh_fw_date);
    copy_field(&incoming.gddr_train_temp0, &mut existing.gddr_train_temp0);
    copy_field(&incoming.gddr_train_temp1, &mut existing.gddr_train_temp1);
    copy_field(&incoming.asic_tmon0, &mut existing.asic_tmon0);
    copy_field(&incoming.asic_tmon1, &mut existing.asic_tmon1);

    // ── Non-string fields — copy verbatim, latest value wins ──────────────────
    //
    // These arrive already typed (u32/f32/bitmask/temperature arrays) rather
    // than as numeric strings, so the string-based EMA machinery above can't
    // carry them. They were previously not carried *at all*, which meant
    // `HybridBackend` — whose only full-struct copy happens on the FIRST insert
    // for a device — froze them at the first snapshot: GDDR ECC counters
    // accrued during a run never appeared (they read 0 on a healthy boot), GDDR
    // temperatures never moved, and an ETH link going down was never reflected.
    //
    // None of them wants EMA even in principle: the ECC fields are monotonic
    // error counters, `max_gddr_temp` is a maximum (smoothing a max understates
    // the peak, which is the whole point of the reading), and the rest are
    // bitmasks where an interpolated value is meaningless.
    copy_opt(&incoming.max_gddr_temp, &mut existing.max_gddr_temp);
    copy_opt(&incoming.gddr_uncorr_errs, &mut existing.gddr_uncorr_errs);
    copy_opt(&incoming.harvesting_state, &mut existing.harvesting_state);
    copy_opt(&incoming.eth_live_status, &mut existing.eth_live_status);
    copy_opt(&incoming.enabled_eth, &mut existing.enabled_eth);
    copy_opt(&incoming.enabled_gddr, &mut existing.enabled_gddr);
    copy_opt(&incoming.enabled_l2cpu, &mut existing.enabled_l2cpu);
    copy_opt(
        &incoming.enabled_tensix_col,
        &mut existing.enabled_tensix_col,
    );
    // Per-channel GDDR training/BIST/harvest/temp/ECC rollup (tt-smi ≥ 6.3.0).
    // Same "copy, don't smooth" reasoning as the rest of this block: channel
    // temps are point-in-time and the ECC counters inside it are monotonic,
    // so blending it toward a prior snapshot would be meaningless.
    copy_opt(&incoming.gddr_telemetry, &mut existing.gddr_telemetry);
    for (src, dst) in incoming
        .gddr_corr_errs
        .iter()
        .zip(existing.gddr_corr_errs.iter_mut())
    {
        copy_opt(src, dst);
    }
    for (src, dst) in incoming
        .gddr_temps
        .iter()
        .zip(existing.gddr_temps.iter_mut())
    {
        copy_opt(src, dst);
    }

    // ── Numeric fields — EMA blend ────────────────────────────────────────────
    blend(
        &mut state.ddr_speed,
        &incoming.ddr_speed,
        &mut existing.ddr_speed,
    );
    blend(
        &mut state.arc0_health,
        &incoming.arc0_health,
        &mut existing.arc0_health,
    );
    blend(
        &mut state.arc1_health,
        &incoming.arc1_health,
        &mut existing.arc1_health,
    );
    blend(
        &mut state.arc2_health,
        &incoming.arc2_health,
        &mut existing.arc2_health,
    );
    blend(
        &mut state.arc3_health,
        &incoming.arc3_health,
        &mut existing.arc3_health,
    );
    blend(&mut state.aiclk, &incoming.aiclk, &mut existing.aiclk);
    blend(&mut state.axiclk, &incoming.axiclk, &mut existing.axiclk);
    blend(&mut state.arcclk, &incoming.arcclk, &mut existing.arcclk);
    blend(
        &mut state.asic_temperature,
        &incoming.asic_temperature,
        &mut existing.asic_temperature,
    );
    blend(
        &mut state.vreg_temperature,
        &incoming.vreg_temperature,
        &mut existing.vreg_temperature,
    );
    blend(
        &mut state.board_temperature,
        &incoming.board_temperature,
        &mut existing.board_temperature,
    );
    blend(&mut state.vcore, &incoming.vcore, &mut existing.vcore);
    blend(&mut state.tdp, &incoming.tdp, &mut existing.tdp);
    blend(&mut state.tdc, &incoming.tdc, &mut existing.tdc);
    blend_fan(
        &mut state.fan_speed,
        &incoming.fan_speed,
        &mut existing.fan_speed,
    );
    blend(
        &mut state.input_power,
        &incoming.input_power,
        &mut existing.input_power,
    );
    blend(
        &mut state.board_power_limit,
        &incoming.board_power_limit,
        &mut existing.board_power_limit,
    );
    blend(
        &mut state.mvddq_power,
        &incoming.mvddq_power,
        &mut existing.mvddq_power,
    );
    blend(
        &mut state.therm_trip_count,
        &incoming.therm_trip_count,
        &mut existing.therm_trip_count,
    );
    blend(
        &mut state.rt_seconds,
        &incoming.rt_seconds,
        &mut existing.rt_seconds,
    );
}

/// Copy `src` into `dst` only when `src` is `Some` — a missing field in the
/// incoming snapshot doesn't erase what we already know.
#[inline]
fn copy_field(src: &Option<String>, dst: &mut Option<String>) {
    copy_opt(src, dst);
}

/// [`copy_field`] for any cloneable payload — used for the typed (non-string)
/// SMBUS fields that the string EMA path can't carry.
#[inline]
fn copy_opt<T: Clone>(src: &Option<T>, dst: &mut Option<T>) {
    if src.is_some() {
        *dst = src.clone();
    }
}

/// Apply EMA to a single string field.
///
/// - If `incoming` is `None`: leave `existing` unchanged (field absent this snapshot).
/// - If `incoming` parses as f64: apply EMA, format back to string.
///   Integer-looking originals (no `.`) are formatted as integers; others as `{:.2}`.
/// - If `incoming` is non-numeric: copy verbatim and clear EMA state.
#[inline]
fn blend(state: &mut FieldEma, incoming: &Option<String>, existing: &mut Option<String>) {
    let raw_str = match incoming {
        Some(s) => s.as_str(),
        None => return, // absent — keep existing unchanged
    };

    match try_parse_numeric(raw_str) {
        Some(v) => {
            let smoothed = match *state {
                Some(prev) => EMA_ALPHA * v + (1.0 - EMA_ALPHA) * prev,
                None => v, // first reading — no smoothing yet
            };
            *state = Some(smoothed);
            // Decimal output: all smoothable SMBUS fields are integers or simple
            // floats.  We always output decimal so downstream parse_hex_or_dec()
            // helpers continue to work without the "0x" prefix.
            *existing = Some(if raw_str.trim().contains('.') {
                format!("{:.2}", smoothed)
            } else {
                format!("{}", smoothed.round() as i64)
            });
        }
        None => {
            // Non-numeric (version string, date stamp, etc.): pass through unchanged.
            *state = None;
            *existing = Some(raw_str.to_owned());
        }
    }
}

/// [`blend`] for the fan field, which has a value class the generic path can't
/// handle: a *reported non-reading*.
///
/// The firmware signals "no fan reading" with a sentinel register value (`0`,
/// `0xFFFF`, `0xFFFFFFFF` — see `models::telemetry::is_fan_sentinel`), and those
/// are numeric, so the generic EMA path would happily blend one in: a card whose
/// fan stops goes `1882 → 1412 → 1059 → …`, a decaying sequence of entirely
/// plausible-looking RPMs that `fan_rpm()` cannot recognise as bogus. The
/// operator watching for a fan failure sees a fan slowing down, then a fan
/// running at some low speed, indefinitely.
///
/// So a sentinel is copied through verbatim and the EMA accumulator is reset:
/// `existing` becomes the honest register value, `fan_rpm()` filters it to
/// `None`, and the Insights Fan row disappears — which is the correct report for
/// a fan that isn't turning. Resetting the accumulator also means the *next*
/// real reading lands immediately instead of being dragged up from the stale
/// value.
///
/// `None` incoming still means "field absent from this snapshot" and preserves
/// `existing`, exactly as for every other field.
#[inline]
fn blend_fan(state: &mut FieldEma, incoming: &Option<String>, existing: &mut Option<String>) {
    if let Some(raw) = incoming {
        if crate::models::telemetry::real_fan_rpm(raw).is_none() {
            *state = None;
            *existing = Some(raw.clone());
            return;
        }
    }
    blend(state, incoming, existing);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_smbus(arc0: &str, board_id: &str) -> SmbusTelemetry {
        SmbusTelemetry {
            arc0_health: Some(arc0.to_owned()),
            board_id: Some(board_id.to_owned()),
            ..SmbusTelemetry::default()
        }
    }

    #[test]
    fn test_ema_numeric_converges() {
        let mut ema: SmbusEmaState = HashMap::new();
        let incoming = make_smbus("100", "board-xyz");
        let mut existing = make_smbus("0", "board-xyz");

        // After several applications the smoothed value should approach 100
        for _ in 0..20 {
            apply_ema(&mut ema, 0, &incoming, &mut existing);
        }
        let v: f64 = existing.arc0_health.unwrap().parse().unwrap();
        assert!(v > 95.0, "EMA should converge toward 100, got {}", v);
    }

    #[test]
    fn test_discrete_field_copied_verbatim() {
        let mut ema: SmbusEmaState = HashMap::new();
        let incoming = make_smbus("50", "new-board-id-abc");
        let mut existing = make_smbus("50", "old-board-id");

        apply_ema(&mut ema, 0, &incoming, &mut existing);
        // board_id must be replaced verbatim, not blended
        assert_eq!(existing.board_id.as_deref(), Some("new-board-id-abc"));
    }

    #[test]
    fn test_absent_incoming_leaves_existing() {
        let mut ema: SmbusEmaState = HashMap::new();
        let incoming = SmbusTelemetry::default(); // all None
        let mut existing = make_smbus("77", "keep-me");

        apply_ema(&mut ema, 0, &incoming, &mut existing);
        // Both fields absent in incoming — existing must be preserved
        assert_eq!(existing.arc0_health.as_deref(), Some("77"));
        assert_eq!(existing.board_id.as_deref(), Some("keep-me"));
    }

    #[test]
    fn test_ema_hex_string_converges() {
        let mut ema: SmbusEmaState = HashMap::new();
        // arc0_health arrives from tt-smi as a hex string like "0x10e7a" (= 68218)
        let incoming = SmbusTelemetry {
            arc0_health: Some("0x10e7a".to_owned()),
            ..SmbusTelemetry::default()
        };
        let mut existing = SmbusTelemetry {
            arc0_health: Some("0".to_owned()),
            ..SmbusTelemetry::default()
        };

        for _ in 0..20 {
            apply_ema(&mut ema, 0, &incoming, &mut existing);
        }

        // Output is decimal string after EMA; parse_hex_or_dec accepts decimal.
        let v: i64 = existing.arc0_health.as_deref().unwrap().parse().unwrap();
        assert!(v > 60000, "EMA should converge toward 68218, got {}", v);
        assert!(v > 0, "is_arc0_healthy equivalent check");
    }

    #[test]
    fn test_ema_ddr_speed_hex_stable() {
        let mut ema: SmbusEmaState = HashMap::new();
        // DDR_SPEED arrives as "0x3e80" = 16000 MT/s; stable across snapshots
        let incoming = SmbusTelemetry {
            ddr_speed: Some("0x3e80".to_owned()),
            ..SmbusTelemetry::default()
        };
        let mut existing = SmbusTelemetry::default();

        for _ in 0..10 {
            apply_ema(&mut ema, 0, &incoming, &mut existing);
        }

        let v: i64 = existing.ddr_speed.as_deref().unwrap().parse().unwrap();
        assert_eq!(
            v, 16000,
            "stable DDR speed should converge to 16000, got {}",
            v
        );
    }

    /// Regression: the typed (non-string) SMBUS fields must reach the blended
    /// output on every apply, not just via `HybridBackend`'s first-insert clone.
    ///
    /// `existing` starts as a healthy-boot snapshot (zero ECC counters, cool
    /// GDDR); the second snapshot brings errors and a hotter maximum. Before the
    /// fix, `apply_ema` never touched these fields, so the ECC row stayed
    /// invisible for the whole session no matter how many errors accrued.
    #[test]
    fn typed_fields_are_carried_through_the_blend() {
        use crate::models::telemetry::GddrTempPair;

        let mut ema: SmbusEmaState = HashMap::new();
        let mut existing = SmbusTelemetry {
            max_gddr_temp: Some(48.0),
            gddr_corr_errs: [Some(0), Some(0), None, None],
            gddr_uncorr_errs: Some(0),
            gddr_temps: [
                Some(GddrTempPair([44.0, 45.0, 46.0, 47.0])),
                None,
                None,
                None,
            ],
            eth_live_status: Some(0xFF),
            enabled_eth: Some(0xFFF),
            enabled_tensix_col: Some(0x3FFF),
            harvesting_state: Some(0),
            enabled_gddr: Some(0xFF),
            enabled_l2cpu: Some(0xF),
            ..SmbusTelemetry::default()
        };
        let incoming = SmbusTelemetry {
            max_gddr_temp: Some(71.0),
            gddr_corr_errs: [Some(3), Some(4), None, None],
            gddr_uncorr_errs: Some(7),
            gddr_temps: [
                Some(GddrTempPair([70.0, 71.0, 69.0, 68.0])),
                None,
                None,
                None,
            ],
            eth_live_status: Some(0x0F), // half the links dropped
            enabled_eth: Some(0xFFF),
            enabled_tensix_col: Some(0x3FFE), // a column got harvested
            harvesting_state: Some(1),
            enabled_gddr: Some(0xFE),
            enabled_l2cpu: Some(0x7),
            ..SmbusTelemetry::default()
        };

        // Second apply — the case HybridBackend actually hits after the first
        // snapshot has already been cloned into `smbus_blended`.
        apply_ema(&mut ema, 0, &incoming, &mut existing);
        apply_ema(&mut ema, 0, &incoming, &mut existing);

        assert_eq!(existing.gddr_uncorr_errs, Some(7), "uncorr ECC must update");
        assert_eq!(existing.gddr_corr_errs[0], Some(3));
        assert_eq!(existing.gddr_corr_errs[1], Some(4));
        // Counters are copied straight through, never EMA-smoothed toward the
        // new value — a 7 must read as exactly 7, not 1.75 rounded.
        assert_eq!(existing.max_gddr_temp, Some(71.0), "max temp must not lag");
        assert_eq!(
            existing.gddr_temps[0],
            Some(GddrTempPair([70.0, 71.0, 69.0, 68.0]))
        );
        assert_eq!(existing.eth_live_status, Some(0x0F), "link drop must show");
        assert_eq!(existing.enabled_tensix_col, Some(0x3FFE));
        assert_eq!(existing.harvesting_state, Some(1));
        assert_eq!(existing.enabled_gddr, Some(0xFE));
        assert_eq!(existing.enabled_l2cpu, Some(0x7));
        assert_eq!(existing.enabled_eth, Some(0xFFF));
    }

    /// A field absent from the incoming snapshot must not erase what we know —
    /// same contract as the string fields.
    #[test]
    fn typed_fields_absent_in_incoming_are_preserved() {
        let mut ema: SmbusEmaState = HashMap::new();
        let mut existing = SmbusTelemetry {
            gddr_uncorr_errs: Some(7),
            max_gddr_temp: Some(71.0),
            eth_live_status: Some(0xFF),
            gddr_corr_errs: [Some(3), None, None, None],
            ..SmbusTelemetry::default()
        };
        apply_ema(&mut ema, 0, &SmbusTelemetry::default(), &mut existing);
        assert_eq!(existing.gddr_uncorr_errs, Some(7));
        assert_eq!(existing.max_gddr_temp, Some(71.0));
        assert_eq!(existing.eth_live_status, Some(0xFF));
        assert_eq!(existing.gddr_corr_errs[0], Some(3));
    }

    /// Regression (v0.8.1): a fan that stops must drop its row, not freeze at the
    /// last healthy RPM.
    ///
    /// The sequence is the live p300c one: tt-smi reports a real
    /// `FAN_RPM = 0x75a` (1882), the fan later stops and the register returns a
    /// sentinel. With the sentinel collapsed to `None` at the parse boundary,
    /// `blend`'s "absent → keep existing" rule pinned `Fan  1882 RPM` on screen
    /// forever; with it EMA-blended, the value decayed through plausible RPMs
    /// instead. Either way the operator watching for a fan failure saw a fan.
    #[test]
    fn fan_sentinel_clears_a_stale_healthy_rpm() {
        let mut ema: SmbusEmaState = HashMap::new();
        let mut existing = SmbusTelemetry::default();

        // Several ticks of a real reading — enough for the EMA to settle on it.
        let healthy = SmbusTelemetry {
            fan_speed: Some("0x75a".to_owned()),
            ..SmbusTelemetry::default()
        };
        for _ in 0..10 {
            apply_ema(&mut ema, 0, &healthy, &mut existing);
        }
        assert_eq!(existing.fan_rpm(), Some(1882), "baseline: fan row renders");

        // The fan stops: the register now reads the no-reading sentinel.
        let stopped = SmbusTelemetry {
            fan_speed: Some("0xffffffff".to_owned()),
            ..SmbusTelemetry::default()
        };
        apply_ema(&mut ema, 0, &stopped, &mut existing);
        assert_eq!(
            existing.fan_rpm(),
            None,
            "a reported sentinel must clear the stale RPM, not preserve or decay it"
        );
        // …and stay cleared on subsequent ticks (no EMA tail dragging it back).
        for _ in 0..5 {
            apply_ema(&mut ema, 0, &stopped, &mut existing);
        }
        assert_eq!(existing.fan_rpm(), None);

        // A real reading afterwards lands immediately — the accumulator was reset,
        // so the recovering fan isn't dragged up from the stale 1882.
        apply_ema(&mut ema, 0, &healthy, &mut existing);
        assert_eq!(existing.fan_rpm(), Some(1882));
    }

    /// The fan field keeps the ordinary "absent this snapshot → keep what we
    /// know" contract; only a *reported* sentinel clears.
    #[test]
    fn fan_absent_in_incoming_preserves_existing() {
        let mut ema: SmbusEmaState = HashMap::new();
        let mut existing = SmbusTelemetry {
            fan_speed: Some("1882".to_owned()),
            ..SmbusTelemetry::default()
        };
        apply_ema(&mut ema, 0, &SmbusTelemetry::default(), &mut existing);
        assert_eq!(existing.fan_rpm(), Some(1882));
    }

    /// Real fan readings are still EMA-smoothed like any other numeric field.
    #[test]
    fn fan_real_readings_still_smooth() {
        let mut ema: SmbusEmaState = HashMap::new();
        let mut existing = SmbusTelemetry {
            fan_speed: Some("1000".to_owned()),
            ..SmbusTelemetry::default()
        };
        // Seed the accumulator at 1000, then step to 2000.
        let seed = SmbusTelemetry {
            fan_speed: Some("1000".to_owned()),
            ..SmbusTelemetry::default()
        };
        apply_ema(&mut ema, 0, &seed, &mut existing);
        let step = SmbusTelemetry {
            fan_speed: Some("2000".to_owned()),
            ..SmbusTelemetry::default()
        };
        apply_ema(&mut ema, 0, &step, &mut existing);
        let v = existing.fan_rpm().unwrap();
        assert!(
            (1100..1500).contains(&v),
            "one EMA step should land ~25% of the way (got {v})"
        );
    }

    #[test]
    fn test_non_numeric_string_passes_through() {
        let mut ema: SmbusEmaState = HashMap::new();
        let incoming = SmbusTelemetry {
            ddr_status: Some("0x55555555".to_owned()),
            ..SmbusTelemetry::default()
        };
        let mut existing = SmbusTelemetry::default();

        apply_ema(&mut ema, 0, &incoming, &mut existing);
        // Hex string — must be copied verbatim, not mangled
        assert_eq!(existing.ddr_status.as_deref(), Some("0x55555555"));
    }

    /// Regression: `gddr_telemetry` must be copied through exactly as it
    /// arrives on every `apply_ema` call — never averaged/smoothed. Channel
    /// temps are point-in-time and ECC counters are monotonic; neither is
    /// meaningful blended toward a prior snapshot, matching how every other
    /// GDDR field in this module is already handled ("copy, don't smooth").
    #[test]
    fn blend_copies_gddr_telemetry_through_unsmoothed() {
        use crate::models::telemetry::{GddrChannel, GddrTelemetry};

        let ch_prev = GddrChannel {
            channel: 0,
            harvested: false,
            enabled: true,
            training_pass: true,
            bist_pass: true,
            temp_top: Some(40.0),
            temp_bottom: Some(42.0),
            corr_rd: Some(0),
            corr_wr: Some(0),
            uncorr_rd: Some(0),
            uncorr_wr: Some(0),
        };
        let mut existing = SmbusTelemetry {
            gddr_telemetry: Some(GddrTelemetry {
                speed: Some("16G".into()),
                max_temp: Some(42.0),
                enabled_mask: Some(0xff),
                channels: vec![ch_prev],
            }),
            ..SmbusTelemetry::default()
        };

        let ch_now = GddrChannel {
            temp_top: Some(60.0),
            temp_bottom: Some(62.0),
            corr_rd: Some(5),
            ..ch_prev
        };
        let incoming = SmbusTelemetry {
            gddr_telemetry: Some(GddrTelemetry {
                speed: Some("16G".into()),
                max_temp: Some(62.0),
                enabled_mask: Some(0xff),
                channels: vec![ch_now],
            }),
            ..SmbusTelemetry::default()
        };

        let mut ema: SmbusEmaState = HashMap::new();
        apply_ema(&mut ema, 0, &incoming, &mut existing);

        // The whole tick's incoming value wins verbatim — not an average with
        // `existing`'s prior 40/42 readings. Temps are point-in-time, ECC
        // counters are monotonic; neither is meaningful smoothed.
        assert_eq!(existing.gddr_telemetry, incoming.gddr_telemetry);
        assert_eq!(
            existing.gddr_telemetry.unwrap().channels[0].temp_top,
            Some(60.0),
            "must be this tick's reading, not an EMA toward the previous 40.0"
        );
    }

    /// Regression guard for the exact bug class Phase 25/26 already hit
    /// twice: a field added to `SmbusTelemetry` that `apply_ema()` forgets to
    /// touch. Constructing this literal with NO `..SmbusTelemetry::default()`
    /// forces every field to be named — if a future field is added to
    /// `SmbusTelemetry` but not handled here, THIS TEST STOPS COMPILING until
    /// it's added, turning a silent runtime bug into a compile error.
    ///
    /// The whole-struct `assert_eq!` below is equally load-bearing: it forces
    /// every named field to actually be *checked* too, not just named in the
    /// literal — a developer who adds a field, hits the compile error above,
    /// and adds it to the literal has done nothing to prove `apply_ema()`
    /// copies it. Only a struct-wide comparison closes that gap, which is why
    /// `SmbusTelemetry` now derives `PartialEq` (all field types already did).
    #[test]
    fn blend_touches_every_smbus_telemetry_field() {
        use crate::models::telemetry::GddrTelemetry;

        let incoming = SmbusTelemetry {
            board_id: Some("b".into()),
            enum_version: Some("1".into()),
            device_id: Some("d".into()),
            ddr_speed: Some("16G".into()),
            ddr_status: Some("0x2".into()),
            arc0_health: Some("healthy".into()),
            arc1_health: Some("healthy".into()),
            arc2_health: Some("healthy".into()),
            arc3_health: Some("healthy".into()),
            arc0_fw_version: Some("1.0".into()),
            arc1_fw_version: Some("1.0".into()),
            arc2_fw_version: Some("1.0".into()),
            arc3_fw_version: Some("1.0".into()),
            eth_fw_version: Some("1.0".into()),
            m3_bl_fw_version: Some("1.0".into()),
            m3_app_fw_version: Some("1.0".into()),
            spibootrom_fw_version: Some("1.0".into()),
            tt_flash_version: Some("1.0".into()),
            aiclk: Some("1000MHz".into()),
            axiclk: Some("1000MHz".into()),
            arcclk: Some("1000MHz".into()),
            asic_temperature: Some("50C".into()),
            vreg_temperature: Some("50C".into()),
            board_temperature: Some("40C".into()),
            vcore: Some("0.8V".into()),
            tdp: Some("100W".into()),
            tdc: Some("100A".into()),
            throttler: Some("0".into()),
            vdd_limits: Some("0.7-0.9".into()),
            thm_limits: Some("90".into()),
            fan_speed: Some("1000rpm".into()),
            faults: Some("0".into()),
            pcie_status: Some("Gen4 x16".into()),
            eth_status0: Some("0".into()),
            eth_status1: Some("0".into()),
            input_power: Some("100W".into()),
            board_power_limit: Some("300W".into()),
            therm_trip_count: Some("zero".into()),
            boot_date: Some("2026-01-01".into()),
            rt_seconds: Some("100s".into()),
            wh_fw_date: Some("2026-01-01".into()),
            asic_tmon0: Some("50".into()),
            asic_tmon1: Some("50".into()),
            mvddq_power: Some("5W".into()),
            gddr_train_temp0: Some("40".into()),
            gddr_train_temp1: Some("40".into()),
            aux_status: Some("0".into()),
            eth_debug_status0: Some("0".into()),
            eth_debug_status1: Some("0".into()),
            gddr_temps: [None, None, None, None],
            max_gddr_temp: Some(50.0),
            gddr_corr_errs: [None, None, None, None],
            gddr_uncorr_errs: Some(0),
            harvesting_state: Some(0),
            eth_live_status: Some(0),
            enabled_eth: Some(0),
            enabled_gddr: Some(0xff),
            enabled_l2cpu: Some(0),
            enabled_tensix_col: Some(0x3fff),
            gddr_telemetry: Some(GddrTelemetry::default()),
        };
        let mut existing = SmbusTelemetry::default();
        let mut ema: SmbusEmaState = HashMap::new();
        apply_ema(&mut ema, 0, &incoming, &mut existing);

        // Every field named above must have made it through apply_ema
        // unchanged (all values here are non-numeric strings or fresh
        // typed values, so no EMA-numeric reformatting applies — see the
        // other tests in this module for that behaviour). A whole-struct
        // comparison is what actually enforces this: it fails the moment
        // apply_ema() drops any field, named or not.
        assert_eq!(
            existing, incoming,
            "apply_ema() must copy or correctly merge every field — if this assertion fails after adding a new SmbusTelemetry field, apply_ema() is silently dropping it"
        );
    }
}
