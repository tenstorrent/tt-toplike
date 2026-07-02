// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure helpers for the `[i]` Inference Servers panel.
//!
//! Kept separate from `mod.rs`'s ratatui rendering so the text layout is
//! unit-testable without a terminal: `format_service_row` turns one
//! `ServiceState` into a single status line, and `service_rows` merges the
//! live snapshot with the static `SERVERS` table so every known service is
//! always shown (a service with no running container renders as `Down`
//! rather than vanishing from the list — see the design doc's "services
//! registry" section). Both are pure — no I/O, no ratatui types — so they're
//! exercised directly in tests below.

use crate::workload::inference_server::{Phase, Readiness, ServiceState, SERVERS};

/// Map a `Phase` to the fixed-width word shown in the panel.
fn phase_word(phase: Phase) -> &'static str {
    match phase {
        Phase::Down => "DOWN",
        Phase::Compiling => "COMPILING",
        Phase::Loading => "LOADING",
        Phase::Ready => "READY",
        Phase::Alarm => "ALARM",
    }
}

/// Map `Readiness` to the raw status-shaped code ops already read off
/// `/tt-liveness`: `000` (down / connection refused, not a real HTTP status
/// but visually consistent with the other two), `405` ("Model is not ready"),
/// `200` (ready).
fn liveness_code(readiness: &Readiness) -> &'static str {
    match readiness {
        Readiness::Down => "000",
        Readiness::NotReady => "405",
        Readiness::Ready { .. } => "200",
    }
}

/// Format one `[i]` panel row from a `ServiceState`:
/// `{label}  {PHASE}[ NN%]  RSS {gib}G ({+MB}/tick)  k={kernel_count}  live={code}  {evidence}`
///
/// Pure and panic-free: every field already came out of the reducer, but the
/// formatting itself stays defensive (no unwraps, saturating semantics via
/// plain floats) since this is the last stop before a human reads it.
pub fn format_service_row(st: &ServiceState) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;

    let phase = phase_word(st.phase);
    let progress = st
        .progress
        .map(|p| format!(" {:.0}%", (p * 100.0).clamp(0.0, 100.0)))
        .unwrap_or_default();

    let rss_gib = st.rss_bytes as f64 / GIB;
    let rate_mib = st.rss_delta as f64 / MIB;
    let live = liveness_code(&st.readiness);
    let kernel_count = st.kernel_count;
    let label = &st.label;

    // Coarse progress/stall glyph: something moved this tick → progressing;
    // Alarm (all probes flat ≥5 min, per `is_alarm`) → stalled. Anything else
    // (e.g. freshly Down) gets no glyph rather than a misleading one.
    let evidence = if st.phase == Phase::Alarm {
        " ⚠ stalled"
    } else if st.rss_delta != 0 || st.kernel_delta != 0 {
        " ✓ progressing"
    } else {
        ""
    };

    format!(
        "{label}  {phase}{progress}  RSS {rss_gib:.1}G ({rate_mib:+.0}MB/tick)  k={kernel_count}  live={live}{evidence}"
    )
}

/// Merge the static `SERVERS` table with the live snapshot: a service found
/// in `snapshot` (matched by `key`) is shown with its real state; a service
/// with no matching entry (no running container this tick) is synthesized as
/// `Down` so the panel always lists every known service, per the design
/// doc's "the panel shows every known service with its state; a service with
/// no running container shows Down gracefully."
///
/// Returns rows in `SERVERS` order (not snapshot order) so the panel doesn't
/// reshuffle as services come and go.
pub fn service_rows(snapshot: &[ServiceState]) -> Vec<ServiceState> {
    SERVERS
        .iter()
        .map(|def| {
            snapshot
                .iter()
                .find(|s| s.key == def.key)
                .cloned()
                .unwrap_or_else(|| down_placeholder(def.key, def.label))
        })
        .collect()
}

/// A `Down`, never-probed placeholder for a known service with no running
/// container this tick. Mirrors the monitor's own `fresh_state` (private to
/// `monitor.rs`) — duplicated here rather than exposed publicly since the two
/// call sites want it for different reasons (a genuinely fresh reducer
/// baseline vs. a display-only stand-in) and the shapes could diverge later.
fn down_placeholder(key: &'static str, label: &'static str) -> ServiceState {
    ServiceState {
        key: key.to_string(),
        label: label.to_string(),
        phase: Phase::Down,
        cpu_pct: 0.0,
        rss_bytes: 0,
        rss_delta: 0,
        kernel_count: 0,
        kernel_delta: 0,
        safetensors_fds: 0,
        readiness: Readiness::Down,
        top_proc: None,
        last_log: None,
        progress: None,
        flat_ticks: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full-field `ServiceState` builder for tests — every field is explicit
    /// so each test only overrides what it cares about via `..base()`.
    fn base() -> ServiceState {
        ServiceState {
            key: "z-image-turbo".to_string(),
            label: "Z-Image-Turbo (P150X4)".to_string(),
            phase: Phase::Loading,
            cpu_pct: 98.0,
            rss_bytes: 42 * 1024 * 1024 * 1024,
            rss_delta: 700 * 1024 * 1024,
            kernel_count: 500,
            kernel_delta: 0,
            safetensors_fds: 3,
            readiness: Readiness::NotReady,
            top_proc: Some("python3".to_string()),
            last_log: None,
            progress: Some(0.68),
            flat_ticks: 0,
        }
    }

    #[test]
    fn formats_loading_row_with_model_and_rss() {
        let st = base();
        let row = format_service_row(&st);
        assert!(row.contains("Z-Image-Turbo"), "row: {row}");
        assert!(row.contains("LOADING"), "row: {row}");
        assert!(row.contains("68%"), "row: {row}");
        assert!(row.contains("RSS 42.0G"), "row: {row}");
        assert!(row.contains("live=405"), "row: {row}");
        assert!(row.contains("progressing"), "row: {row}");
    }

    #[test]
    fn formats_down_row_with_no_progress_and_000_code() {
        let st = ServiceState {
            phase: Phase::Down,
            rss_bytes: 0,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            readiness: Readiness::Down,
            progress: None,
            ..base()
        };
        let row = format_service_row(&st);
        assert!(row.contains("DOWN"), "row: {row}");
        assert!(row.contains("live=000"), "row: {row}");
        assert!(!row.contains('%'), "no progress % expected: {row}"); // no progress estimate
        assert!(!row.contains("progressing"), "row: {row}");
    }

    #[test]
    fn formats_ready_row_with_200_code() {
        let st = ServiceState {
            phase: Phase::Ready,
            readiness: Readiness::Ready {
                runner: Some("tt-z-image-turbo".to_string()),
            },
            progress: None,
            ..base()
        };
        let row = format_service_row(&st);
        assert!(row.contains("READY"), "row: {row}");
        assert!(row.contains("live=200"), "row: {row}");
    }

    #[test]
    fn formats_alarm_row_with_stalled_glyph() {
        let st = ServiceState {
            phase: Phase::Alarm,
            rss_delta: 0,
            kernel_delta: 0,
            flat_ticks: 60,
            ..base()
        };
        let row = format_service_row(&st);
        assert!(row.contains("ALARM"), "row: {row}");
        assert!(row.contains("stalled"), "row: {row}");
        assert!(!row.contains("progressing"), "row: {row}");
    }

    #[test]
    fn formats_compiling_row_with_kernel_count() {
        let st = ServiceState {
            phase: Phase::Compiling,
            kernel_count: 250,
            kernel_delta: 12,
            rss_delta: 0,
            progress: Some(0.25),
            ..base()
        };
        let row = format_service_row(&st);
        assert!(row.contains("COMPILING"), "row: {row}");
        assert!(row.contains("k=250"), "row: {row}");
        assert!(row.contains("25%"), "row: {row}");
        assert!(row.contains("progressing"), "row: {row}"); // kernel_delta > 0
    }

    #[test]
    fn service_rows_fills_absent_services_as_down() {
        // Empty snapshot (nothing detected this tick) → every known service
        // still appears, all Down.
        let rows = service_rows(&[]);
        assert_eq!(rows.len(), SERVERS.len());
        assert!(rows.iter().all(|r| r.phase == Phase::Down));
        // Order matches SERVERS, not snapshot order.
        assert_eq!(rows[0].key, SERVERS[0].key);
    }

    #[test]
    fn service_rows_uses_live_state_when_present() {
        let live = base(); // key "z-image-turbo", Loading
        let rows = service_rows(std::slice::from_ref(&live));
        let found = rows.iter().find(|r| r.key == "z-image-turbo").unwrap();
        assert_eq!(found.phase, Phase::Loading);
        assert_eq!(found.rss_bytes, live.rss_bytes);
        // Every other known service still present, as Down.
        assert_eq!(rows.len(), SERVERS.len());
        assert!(rows
            .iter()
            .filter(|r| r.key != "z-image-turbo")
            .all(|r| r.phase == Phase::Down));
    }
}
