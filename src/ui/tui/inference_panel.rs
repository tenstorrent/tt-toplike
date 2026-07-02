// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure helpers for the Inference Servers views: the dedicated full-screen
//! `DisplayMode::InferenceMonitor` (entered via `i`/`I`; see
//! `render_inference_monitor` below and its caller
//! `render_inference_monitor_view` in `mod.rs`).
//!
//! Kept separate from `mod.rs`'s ratatui rendering so the text layout is
//! unit-testable without a terminal: `format_service_row` turns one
//! `ServiceState` into a single status line, `service_rows` merges the live
//! snapshot with the static `SERVERS` table so every known service is always
//! shown (a service with no running container renders as `Down` rather than
//! vanishing from the list — see the design doc's "services registry"
//! section), and `render_inference_monitor` turns those rows into styled,
//! cursor-aware `Line`s. All pure — no I/O — so they're exercised directly in
//! tests below.

use crate::workload::inference_server::{Phase, Readiness, ServiceState, SERVERS};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

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

/// Render the dedicated, full-screen "Inference Servers" monitor view: one
/// line per `service_rows(snapshot)` entry (so every known `SERVERS` service
/// always appears, even with an empty/off-Linux snapshot — see
/// `service_rows`), colored by phase, with the `service_cursor`-th row
/// marked for keyboard navigation (the cursor itself is wired up in a later
/// task; this function just renders whatever index it's given, clamping
/// nothing — an out-of-range cursor simply matches no row).
///
/// Pure — returns styled `Line`s rather than drawing directly, so the layout
/// is unit-testable without a terminal. The caller (`mod.rs`) wraps the
/// result in a `Paragraph`/`Block` and renders it to a full-screen `Rect`.
pub fn render_inference_monitor(
    snapshot: &[ServiceState],
    service_cursor: usize,
) -> Vec<Line<'static>> {
    service_rows(snapshot)
        .iter()
        .enumerate()
        .map(|(i, row)| {
            // green Ready, yellow Compiling/Loading, red Alarm, gray Down.
            let color = match row.phase {
                Phase::Ready => Color::Green,
                Phase::Compiling | Phase::Loading => Color::Yellow,
                Phase::Alarm => Color::Red,
                Phase::Down => Color::DarkGray,
            };
            let selected = i == service_cursor;
            let marker = if selected { "> " } else { "  " };
            let mut style = Style::default().fg(color);
            if selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::from(vec![
                Span::raw(marker),
                Span::styled(format_service_row(row), style),
            ])
        })
        .collect()
}

/// Detect a model-unload edge between two consecutive inference-monitor
/// snapshots: a service that was loaded (`Ready`) previously and is now `Down` or
/// absent from the snapshot. Used to trigger the Defrag EVICT animation (the
/// power-EMA heuristic can't reliably detect unload on Blackhole). Pure.
///
/// A `Loading`→gone transition is deliberately NOT an unload — the model never
/// finished loading, so there's nothing to evict.
pub fn model_unloaded(prev: &[ServiceState], cur: &[ServiceState]) -> bool {
    prev.iter().any(|p| {
        p.phase == Phase::Ready
            && match cur.iter().find(|c| c.key == p.key) {
                Some(c) => c.phase == Phase::Down,
                None => true, // container disappeared entirely
            }
    })
}

/// The service to feature in the full-screen loading view: the first one that
/// is Compiling, Loading, or Alarm (a stalled load stays featured so the
/// snake can show the red alarm state), in snapshot order. `None` when
/// nothing is loading (the view then falls to the starfield when cold, else
/// the live list).
///
/// Wired into `mod.rs`'s `DisplayMode::InferenceMonitor` render path: when this
/// returns `Some`, the boxed-snake loading view takes over the `[i]` screen.
pub fn featured_loading(snapshot: &[ServiceState]) -> Option<&ServiceState> {
    snapshot
        .iter()
        .find(|s| matches!(s.phase, Phase::Compiling | Phase::Loading | Phase::Alarm))
}

/// The inference "trail" is cold when no service is doing anything — used to
/// swap the [i] view to the model-starfield screensaver. Compiling/Loading/Ready
/// means a model is present, so the live list is shown instead.
pub fn trail_is_cold(snapshot: &[ServiceState]) -> bool {
    !snapshot
        .iter()
        .any(|s| matches!(s.phase, Phase::Compiling | Phase::Loading | Phase::Ready))
}

/// Best-effort: map a selected process row to a known inference service key so the
/// monitor can pre-focus it. ProcRow carries no cmdline/model, so we only match its
/// inference label against a SERVERS key; anything else → None (monitor opens
/// unfocused). Precise PID→container linking is out of scope (spec #1).
pub fn service_for_selected_process(row: &crate::workload::ProcRow) -> Option<&'static str> {
    let label = row.inference?;
    SERVERS.iter().find(|d| d.key == label).map(|d| d.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: the brief for this task illustrated the resolver with `"vllm"` as
    // an example "SERVERS key", but `SERVERS` (see `services.rs`) only holds
    // TT container services (`wan2.2`, `mochi`, `flux`, `sdxl`,
    // `z-image-turbo`, `motif`, `animate`, `skyreels`, `prompt-server`) —
    // `vllm` is a host-runtime label from `inference_match`, never a
    // container-service key. Swapped the example to `"flux"` (an actual
    // `SERVERS` key) so the match-succeeds branch is real; `"ollama"` still
    // stands in for a matched-but-not-a-container-service label.
    #[test]
    fn service_for_selected_process_matches_label_else_none() {
        let mut r = crate::workload::ProcRow {
            pid: 1,
            name: "python3".into(),
            cpu_pct: 1.0,
            mem_bytes: 0,
            inference: Some("flux"),
            active: true,
            tt: None,
        };
        assert_eq!(service_for_selected_process(&r), Some("flux")); // label is a SERVERS key
        r.inference = Some("ollama"); // not a container service
        assert_eq!(service_for_selected_process(&r), None);
        r.inference = None;
        assert_eq!(service_for_selected_process(&r), None);
    }

    #[test]
    fn featured_loading_picks_first_compiling_or_loading() {
        let mut down = base();
        down.phase = Phase::Down;
        let mut ready = base();
        ready.phase = Phase::Ready;
        let mut loading = base();
        loading.phase = Phase::Loading;
        loading.key = "b".into();
        let mut compiling = base();
        compiling.phase = Phase::Compiling;
        compiling.key = "a".into();
        // Nothing loading → None.
        assert!(featured_loading(&[down.clone(), ready.clone()]).is_none());
        assert!(featured_loading(&[]).is_none());
        // First Compiling/Loading in slice order wins; Down/Ready skipped.
        let rows = vec![down, ready, compiling.clone(), loading];
        assert_eq!(featured_loading(&rows).map(|s| s.key.as_str()), Some("a"));
    }

    #[test]
    fn featured_loading_includes_alarm() {
        let mut alarm = base();
        alarm.phase = Phase::Alarm;
        alarm.key = "z".into();
        assert_eq!(featured_loading(&[alarm]).map(|s| s.key.as_str()), Some("z"));
    }

    #[test]
    fn trail_is_cold_only_when_nothing_running() {
        let mut down = base();
        down.phase = Phase::Down;
        assert!(trail_is_cold(&[down.clone()]));
        assert!(trail_is_cold(&[]));
        let mut loading = base();
        loading.phase = Phase::Loading;
        assert!(!trail_is_cold(&[down, loading]));
        let mut ready = base();
        ready.phase = Phase::Ready;
        assert!(!trail_is_cold(&[ready]));
    }

    #[test]
    fn model_unloaded_detects_ready_to_gone_only() {
        let mut ready = base();
        ready.key = "flux".into();
        ready.phase = Phase::Ready;
        let mut down = base();
        down.key = "flux".into();
        down.phase = Phase::Down;

        assert!(
            model_unloaded(&[ready.clone()], &[down]),
            "Ready → Down is an unload"
        );
        assert!(
            model_unloaded(&[ready.clone()], &[]),
            "Ready → absent is an unload"
        );
        assert!(
            !model_unloaded(&[ready.clone()], &[ready.clone()]),
            "still Ready → no unload"
        );

        let mut loading = base();
        loading.key = "flux".into();
        loading.phase = Phase::Loading;
        assert!(
            !model_unloaded(&[loading], &[]),
            "Loading → gone is not an unload"
        );
    }

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

    /// Flatten a rendered `Line`'s spans back into plain text for assertions
    /// — mirrors the pattern used by other TUI render tests (e.g.
    /// `animation::arcade`'s `line_text`).
    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn render_inference_monitor_lists_all_servers_and_marks_cursor() {
        // Empty snapshot (no live monitor / off-Linux) still renders one row
        // per known service, all Down.
        let lines = render_inference_monitor(&[], 1);
        assert_eq!(lines.len(), SERVERS.len());

        for def in SERVERS {
            assert!(
                lines.iter().any(|l| line_text(l).contains(def.label)),
                "expected a row for {}",
                def.label
            );
        }

        // The cursor row (index 1) is marked with the "> " prefix; every
        // other row keeps the unmarked "  " prefix.
        for (i, line) in lines.iter().enumerate() {
            let text = line_text(line);
            if i == 1 {
                assert!(text.starts_with("> "), "cursor row not marked: {text}");
            } else {
                assert!(
                    !text.starts_with("> "),
                    "non-cursor row {i} unexpectedly marked: {text}"
                );
            }
        }
    }

    #[test]
    fn render_inference_monitor_uses_live_state() {
        let live = base(); // key "z-image-turbo", Loading, progress 68%
        let lines = render_inference_monitor(std::slice::from_ref(&live), 0);
        assert_eq!(lines.len(), SERVERS.len());
        assert!(
            lines.iter().any(|l| {
                let t = line_text(l);
                t.contains("Z-Image-Turbo") && t.contains("LOADING")
            }),
            "expected the live Loading row to appear"
        );
    }
}
