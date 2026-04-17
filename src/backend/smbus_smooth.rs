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
    ddr_speed:         FieldEma,
    arc0_health:       FieldEma,
    arc1_health:       FieldEma,
    arc2_health:       FieldEma,
    arc3_health:       FieldEma,
    aiclk:             FieldEma,
    axiclk:            FieldEma,
    arcclk:            FieldEma,
    asic_temperature:  FieldEma,
    vreg_temperature:  FieldEma,
    board_temperature: FieldEma,
    vcore:             FieldEma,
    tdp:               FieldEma,
    tdc:               FieldEma,
    fan_speed:         FieldEma,
    input_power:       FieldEma,
    board_power_limit: FieldEma,
    mvddq_power:       FieldEma,
    therm_trip_count:  FieldEma,
    rt_seconds:        FieldEma,
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
    ema:        &mut SmbusEmaState,
    device_idx: usize,
    incoming:   &SmbusTelemetry,
    existing:   &mut SmbusTelemetry,
) {
    let state = ema.entry(device_idx).or_default();

    // ── Discrete / identifier fields — copy verbatim ─────────────────────────
    copy_field(&incoming.board_id,              &mut existing.board_id);
    copy_field(&incoming.device_id,             &mut existing.device_id);
    copy_field(&incoming.enum_version,          &mut existing.enum_version);
    copy_field(&incoming.ddr_status,            &mut existing.ddr_status);
    copy_field(&incoming.arc0_fw_version,       &mut existing.arc0_fw_version);
    copy_field(&incoming.arc1_fw_version,       &mut existing.arc1_fw_version);
    copy_field(&incoming.arc2_fw_version,       &mut existing.arc2_fw_version);
    copy_field(&incoming.arc3_fw_version,       &mut existing.arc3_fw_version);
    copy_field(&incoming.eth_fw_version,        &mut existing.eth_fw_version);
    copy_field(&incoming.m3_bl_fw_version,      &mut existing.m3_bl_fw_version);
    copy_field(&incoming.m3_app_fw_version,     &mut existing.m3_app_fw_version);
    copy_field(&incoming.spibootrom_fw_version, &mut existing.spibootrom_fw_version);
    copy_field(&incoming.tt_flash_version,      &mut existing.tt_flash_version);
    copy_field(&incoming.pcie_status,           &mut existing.pcie_status);
    copy_field(&incoming.eth_status0,           &mut existing.eth_status0);
    copy_field(&incoming.eth_status1,           &mut existing.eth_status1);
    copy_field(&incoming.eth_debug_status0,     &mut existing.eth_debug_status0);
    copy_field(&incoming.eth_debug_status1,     &mut existing.eth_debug_status1);
    copy_field(&incoming.aux_status,            &mut existing.aux_status);
    copy_field(&incoming.faults,                &mut existing.faults);
    copy_field(&incoming.throttler,             &mut existing.throttler);
    copy_field(&incoming.vdd_limits,            &mut existing.vdd_limits);
    copy_field(&incoming.thm_limits,            &mut existing.thm_limits);
    copy_field(&incoming.boot_date,             &mut existing.boot_date);
    copy_field(&incoming.wh_fw_date,            &mut existing.wh_fw_date);
    copy_field(&incoming.gddr_train_temp0,      &mut existing.gddr_train_temp0);
    copy_field(&incoming.gddr_train_temp1,      &mut existing.gddr_train_temp1);
    copy_field(&incoming.asic_tmon0,            &mut existing.asic_tmon0);
    copy_field(&incoming.asic_tmon1,            &mut existing.asic_tmon1);

    // ── Numeric fields — EMA blend ────────────────────────────────────────────
    blend(&mut state.ddr_speed,         &incoming.ddr_speed,         &mut existing.ddr_speed);
    blend(&mut state.arc0_health,       &incoming.arc0_health,       &mut existing.arc0_health);
    blend(&mut state.arc1_health,       &incoming.arc1_health,       &mut existing.arc1_health);
    blend(&mut state.arc2_health,       &incoming.arc2_health,       &mut existing.arc2_health);
    blend(&mut state.arc3_health,       &incoming.arc3_health,       &mut existing.arc3_health);
    blend(&mut state.aiclk,             &incoming.aiclk,             &mut existing.aiclk);
    blend(&mut state.axiclk,            &incoming.axiclk,            &mut existing.axiclk);
    blend(&mut state.arcclk,            &incoming.arcclk,            &mut existing.arcclk);
    blend(&mut state.asic_temperature,  &incoming.asic_temperature,  &mut existing.asic_temperature);
    blend(&mut state.vreg_temperature,  &incoming.vreg_temperature,  &mut existing.vreg_temperature);
    blend(&mut state.board_temperature, &incoming.board_temperature, &mut existing.board_temperature);
    blend(&mut state.vcore,             &incoming.vcore,             &mut existing.vcore);
    blend(&mut state.tdp,               &incoming.tdp,               &mut existing.tdp);
    blend(&mut state.tdc,               &incoming.tdc,               &mut existing.tdc);
    blend(&mut state.fan_speed,         &incoming.fan_speed,         &mut existing.fan_speed);
    blend(&mut state.input_power,       &incoming.input_power,       &mut existing.input_power);
    blend(&mut state.board_power_limit, &incoming.board_power_limit, &mut existing.board_power_limit);
    blend(&mut state.mvddq_power,       &incoming.mvddq_power,       &mut existing.mvddq_power);
    blend(&mut state.therm_trip_count,  &incoming.therm_trip_count,  &mut existing.therm_trip_count);
    blend(&mut state.rt_seconds,        &incoming.rt_seconds,        &mut existing.rt_seconds);
}

/// Copy `src` into `dst` only when `src` is `Some` — a missing field in the
/// incoming snapshot doesn't erase what we already know.
#[inline]
fn copy_field(src: &Option<String>, dst: &mut Option<String>) {
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
        assert_eq!(v, 16000, "stable DDR speed should converge to 16000, got {}", v);
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
}
