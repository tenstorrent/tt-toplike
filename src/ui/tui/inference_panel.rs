// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure snapshot selectors backing the unified serving snake that owns the
//! `[i]` `DisplayMode::InferenceMonitor` screen (rendered by `mod.rs`'s
//! `render_snake_view` over a `crate::animation::Snake`). These are the small,
//! I/O-free decisions the snake coordinator makes each tick, kept here so they
//! stay unit-testable without a terminal:
//!
//! - `featured_loading` picks the one service the Growing (loading) view should
//!   feature — the first Compiling/Loading/Alarm service in snapshot order.
//!
//! Defrag's EVICT animation used to be triggered here, from a Ready→Down/gone
//! edge between two snapshots. It now comes from the signal bus
//! (`animation::defrag_bus`), which sees the same services.
//!
//! The older text-list render path (`format_service_row`/`service_rows`/
//! `render_inference_monitor`) that this file used to host has been retired
//! along with the strip it drew — the snake is now the only `[i]` view.

use crate::workload::inference_server::{Phase, ServiceState};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::Readiness;

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
        assert_eq!(
            featured_loading(&[alarm]).map(|s| s.key.as_str()),
            Some("z")
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
            loaded_count: 1400,
            loaded_delta: 0,
            safetensors_fds: 3,
            readiness: Readiness::NotReady,
            top_proc: Some("python3".to_string()),
            last_log: None,
            progress: Some(0.68),
            flat_ticks: 0,
            serving: None,
            media: None,
            port: None,
            chips: Vec::new(),
        }
    }
}
