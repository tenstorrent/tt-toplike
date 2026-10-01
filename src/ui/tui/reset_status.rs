// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure state for the `tt-smi -r` status-bar segment shown by
//! `--tt-smi-reset-behavior inform|dazzle|demo`, the per-scan decision
//! logic that feeds it, and the width-fitting rule for the status bar's
//! left zone. Nothing here touches terminal types or the clock: callers
//! pass `now`, so tests do not sleep.
//!
//! Lifecycle of the segment text:
//! 1. `⟳ tt-smi -r · {scope} · resetting` from the moment the reset is
//!    detected until the real `tt-smi -r` pid is no longer in the process
//!    list.
//! 2. `✓ tt-smi -r done` for [`RESET_DONE_VISIBLE`] after that.
//! 3. Nothing.
//!
//! The segment follows the pid itself.
//! A takeover can be skipped (and the detector cleared) while
//! `tt-smi` is still running, and HivemindSweeper clears the detector right
//! after injecting its feed event. Neither changes what the text says.

use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthStr;

use crate::cli::ResetBehavior;
use crate::models::device::Device;
use crate::workload::reset_detect::{ResetDetector, ResetEvent};

/// How long `✓ tt-smi -r done` stays in the status bar after the reset ends.
pub const RESET_DONE_VISIBLE: Duration = Duration::from_secs(10);

/// What the status bar draws for the reset segment this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetSegment {
    /// Full text, including the scope clause while resetting.
    pub text: String,
    /// Same text with the scope clause dropped, for narrow terminals.
    pub short: String,
    /// True once the reset has finished (drawn in the "done" colour).
    pub done: bool,
}

/// One tracked `tt-smi -r` invocation as the status bar sees it.
#[derive(Debug, Clone)]
pub struct ResetStatus {
    pid: i32,
    chip_count: usize,
    is_full: bool,
    finished_at: Option<Instant>,
}

impl ResetStatus {
    /// Starts in the "resetting" state for a freshly detected reset.
    pub fn from_event(ev: &ResetEvent) -> Self {
        Self {
            pid: ev.pid,
            chip_count: ev.chip_count,
            is_full: ev.is_full,
            finished_at: None,
        }
    }

    /// The pid of the `tt-smi -r` process being followed (used by tests).
    #[cfg(test)]
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Marks the reset finished at `now`. The first call wins.
    pub fn note_finished(&mut self, now: Instant) {
        self.finished_at.get_or_insert(now);
    }

    /// Marks the reset finished when its pid is absent from `processes`
    /// (the same test `ResetDetector::is_finished` uses).
    pub fn observe_processes(&mut self, processes: &[(i32, String, String)], now: Instant) {
        if !processes.iter().any(|(pid, _, _)| *pid == self.pid) {
            self.note_finished(now);
        }
    }

    /// True once the "done" text has been visible for [`RESET_DONE_VISIBLE`].
    pub fn is_expired(&self, now: Instant) -> bool {
        self.finished_at
            .is_some_and(|t| now.saturating_duration_since(t) >= RESET_DONE_VISIBLE)
    }

    /// The segment to draw at `now`, or `None` once it has expired.
    pub fn text(&self, now: Instant) -> Option<ResetSegment> {
        if self.is_expired(now) {
            return None;
        }
        if self.finished_at.is_some() {
            let t = "✓ tt-smi -r done".to_string();
            return Some(ResetSegment {
                text: t.clone(),
                short: t,
                done: true,
            });
        }
        // Same scope wording as the Quiet Notice takeover.
        let scope = if self.is_full {
            "all chips".to_string()
        } else {
            format!("{} chip(s)", self.chip_count)
        };
        Some(ResetSegment {
            text: format!("⟳ tt-smi -r · {scope} · resetting"),
            short: "⟳ tt-smi -r · resetting".to_string(),
            done: false,
        })
    }
}

/// What the TUI loop should do after one process scan.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    /// Set when a new reset was detected and a takeover animation should
    /// start for it (`dazzle`/`demo`, never in HivemindSweeper).
    pub takeover_for: Option<ResetEvent>,
    /// Set when a new reset was detected in HivemindSweeper and should
    /// become a real feed event (`inform`/`dazzle`/`demo`).
    pub feed_event_for: Option<ResetEvent>,
}

/// One scan of the process list for the reset feature. Runs only when
/// `behavior.detects()`. Updates `status` (creating it on a new reset,
/// marking it finished when the pid disappears, dropping it once the done
/// text has expired) and tells the caller whether to start a takeover or
/// inject a feed event.
///
/// The detector stays occupied only while a takeover is waiting on it. In
/// every other case it is cleared at once, so the next reset is detected
/// normally and the still-running pid is not reported twice.
pub fn scan_resets(
    behavior: ResetBehavior,
    hivemind: bool,
    detector: &mut ResetDetector,
    status: &mut Option<ResetStatus>,
    processes: &[(i32, String, String)],
    devices: &[Device],
    now: Instant,
) -> ScanOutcome {
    let mut out = ScanOutcome::default();
    if !behavior.detects() {
        return out;
    }
    if let Some(ev) = detector.observe(processes, devices).cloned() {
        *status = Some(ResetStatus::from_event(&ev));
        if hivemind {
            // HivemindSweeper never gets a takeover; the reset is a feed event.
            out.feed_event_for = Some(ev);
            detector.clear();
        } else if behavior.animates() {
            out.takeover_for = Some(ev);
        } else {
            detector.clear();
        }
    }
    if let Some(st) = status.as_mut() {
        st.observe_processes(processes, now);
        if st.is_expired(now) {
            *status = None;
        }
    }
    out
}

/// How much of the reset segment the status bar draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentFit {
    /// The full text, scope clause included.
    Full,
    /// The short text (scope clause dropped).
    Short,
    /// Nothing; no form of it fits.
    Hidden,
}

/// The outcome of [`fit_left_zone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeftFit {
    pub segment: SegmentFit,
    /// How many hotkey groups to keep, counted from the left.
    pub hotkeys: usize,
    /// How many hint groups (perf, throttle, serve) to keep, from the left.
    pub hints: usize,
}

/// Width of the ` │ ` separator drawn between left-zone items.
pub const LEFT_SEP_WIDTH: usize = 5;

/// Display width of `s`.
pub fn text_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Decides what fits in the status bar's left zone beside `right_w` columns
/// of chip telemetry in a bar `width` columns wide.
///
/// The zone is a leading space, then items joined by [`LEFT_SEP_WIDTH`]
/// separators: the segment (if any), the hotkey groups, the hint groups.
/// `segment` is `(full width, short width)`. Whole groups are dropped from
/// the right (hotkeys first, then hints) until the zone fits. The segment is
/// never cut: when it cannot fit even alone, the short form is tried, and
/// when that cannot fit either it is hidden.
pub fn fit_left_zone(
    width: usize,
    right_w: usize,
    segment: Option<(usize, usize)>,
    hotkey_ws: &[usize],
    hint_ws: &[usize],
) -> LeftFit {
    let candidates: Vec<(SegmentFit, Option<usize>)> = match segment {
        Some((full, short)) => vec![
            (SegmentFit::Full, Some(full)),
            (SegmentFit::Short, Some(short)),
            (SegmentFit::Hidden, None),
        ],
        None => vec![(SegmentFit::Hidden, None)],
    };
    for (kind, seg_w) in candidates {
        let mut hk = hotkey_ws.len();
        let mut ht = hint_ws.len();
        loop {
            let widths: Vec<usize> = seg_w
                .into_iter()
                .chain(hotkey_ws[..hk].iter().copied())
                .chain(hint_ws[..ht].iter().copied())
                .collect();
            let items = widths.len();
            let left_w =
                1 + widths.iter().sum::<usize>() + LEFT_SEP_WIDTH * items.saturating_sub(1);
            if left_w + right_w <= width {
                return LeftFit {
                    segment: kind,
                    hotkeys: hk,
                    hints: ht,
                };
            }
            if hk > 0 {
                hk -= 1;
            } else if ht > 0 {
                ht -= 1;
            } else {
                break;
            }
        }
    }
    // Not even the right zone fits by itself. Draw no left items.
    LeftFit {
        segment: SegmentFit::Hidden,
        hotkeys: 0,
        hints: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ResetBehavior;
    use crate::workload::reset_detect::{ResetDetector, ResetEvent};
    use std::time::{Duration, Instant};

    fn ev(pid: i32, is_full: bool, chip_count: usize) -> ResetEvent {
        ResetEvent {
            pid,
            is_full,
            chip_count,
            total_devices: 4,
            device_indices: vec![],
            raw_targets: vec![],
        }
    }

    fn procs(list: &[(i32, &str)]) -> Vec<(i32, String, String)> {
        list.iter()
            .map(|(p, c)| (*p, "tt-smi".to_string(), c.to_string()))
            .collect()
    }

    #[test]
    fn resetting_text_full_and_subset() {
        let t0 = Instant::now();
        let full = ResetStatus::from_event(&ev(1, true, 4)).text(t0).unwrap();
        assert_eq!(full.text, "⟳ tt-smi -r · all chips · resetting");
        assert_eq!(full.short, "⟳ tt-smi -r · resetting");
        assert!(!full.done);
        let one = ResetStatus::from_event(&ev(1, false, 1)).text(t0).unwrap();
        assert_eq!(one.text, "⟳ tt-smi -r · 1 chip(s) · resetting");
        let many = ResetStatus::from_event(&ev(1, false, 3)).text(t0).unwrap();
        assert_eq!(many.text, "⟳ tt-smi -r · 3 chip(s) · resetting");
    }

    #[test]
    fn done_text_after_note_finished() {
        let t0 = Instant::now();
        let mut s = ResetStatus::from_event(&ev(1, true, 4));
        s.note_finished(t0);
        let seg = s.text(t0).unwrap();
        assert_eq!(seg.text, "✓ tt-smi -r done");
        assert_eq!(seg.short, "✓ tt-smi -r done");
        assert!(seg.done);
    }

    #[test]
    fn done_is_visible_for_ten_seconds_then_gone() {
        let t0 = Instant::now();
        let mut s = ResetStatus::from_event(&ev(1, true, 4));
        s.note_finished(t0);
        assert!(s.text(t0 + Duration::from_millis(9_900)).is_some());
        assert!(!s.is_expired(t0 + Duration::from_millis(9_900)));
        assert!(s.text(t0 + RESET_DONE_VISIBLE).is_none());
        assert!(s.is_expired(t0 + RESET_DONE_VISIBLE));
    }

    #[test]
    fn resetting_never_expires_on_its_own() {
        let t0 = Instant::now();
        let s = ResetStatus::from_event(&ev(1, true, 4));
        let later = t0 + Duration::from_secs(3600);
        assert!(s.text(later).is_some());
        assert!(!s.is_expired(later));
    }

    #[test]
    fn note_finished_keeps_the_first_finish_time() {
        let t0 = Instant::now();
        let mut s = ResetStatus::from_event(&ev(1, true, 4));
        s.note_finished(t0);
        s.note_finished(t0 + Duration::from_secs(8));
        assert!(s.text(t0 + Duration::from_secs(10)).is_none());
    }

    #[test]
    fn observe_processes_finishes_only_when_the_pid_is_gone() {
        let t0 = Instant::now();
        let mut s = ResetStatus::from_event(&ev(7, true, 4));
        s.observe_processes(&procs(&[(7, "tt-smi -r")]), t0);
        assert!(!s.text(t0).unwrap().done, "pid still alive");
        s.observe_processes(&procs(&[(8, "bash")]), t0);
        assert!(s.text(t0).unwrap().done, "pid gone");
    }

    // ── scan_resets: gating + lifecycle ────────────────────────────────

    fn scan(
        behavior: ResetBehavior,
        hivemind: bool,
        det: &mut ResetDetector,
        status: &mut Option<ResetStatus>,
        list: &[(i32, &str)],
        now: Instant,
    ) -> ScanOutcome {
        scan_resets(behavior, hivemind, det, status, &procs(list), &[], now)
    }

    #[test]
    fn ignore_never_observes_a_reset() {
        let mut det = ResetDetector::new();
        let mut st = None;
        let out = scan(
            ResetBehavior::Ignore,
            false,
            &mut det,
            &mut st,
            &[(5, "tt-smi -r")],
            Instant::now(),
        );
        assert!(out.takeover_for.is_none() && out.feed_event_for.is_none());
        assert!(st.is_none());
        // The detector was never fed, so it still has nothing to finish.
        assert!(!det.is_finished(&procs(&[])));
    }

    #[test]
    fn inform_shows_a_segment_without_a_takeover() {
        let mut det = ResetDetector::new();
        let mut st = None;
        let out = scan(
            ResetBehavior::Inform,
            false,
            &mut det,
            &mut st,
            &[(5, "tt-smi -r")],
            Instant::now(),
        );
        assert!(out.takeover_for.is_none());
        assert!(out.feed_event_for.is_none());
        assert!(st.is_some());
    }

    #[test]
    fn dazzle_and_demo_request_a_takeover() {
        for b in [ResetBehavior::Dazzle, ResetBehavior::Demo] {
            let mut det = ResetDetector::new();
            let mut st = None;
            let out = scan(
                b,
                false,
                &mut det,
                &mut st,
                &[(5, "tt-smi -r")],
                Instant::now(),
            );
            assert_eq!(out.takeover_for.as_ref().map(|e| e.pid), Some(5), "{b:?}");
            assert!(out.feed_event_for.is_none());
            assert!(st.is_some());
        }
    }

    #[test]
    fn hivemind_injects_a_feed_event_never_a_takeover_and_the_segment_runs() {
        let t0 = Instant::now();
        for b in [
            ResetBehavior::Inform,
            ResetBehavior::Dazzle,
            ResetBehavior::Demo,
        ] {
            let mut det = ResetDetector::new();
            let mut st = None;
            // Detected: resetting segment, feed event, no takeover.
            let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            assert!(out.takeover_for.is_none(), "{b:?}");
            assert_eq!(out.feed_event_for.as_ref().map(|e| e.pid), Some(5));
            assert!(!st.as_ref().unwrap().text(t0).unwrap().done);
            // Still running one scan later: still resetting, no repeat event.
            let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            assert!(out.feed_event_for.is_none(), "same pid is not re-injected");
            assert!(!st.as_ref().unwrap().text(t0).unwrap().done);
            // Process ends: done.
            let t1 = t0 + Duration::from_secs(4);
            scan(b, true, &mut det, &mut st, &[], t1);
            assert!(st.as_ref().unwrap().text(t1).unwrap().done);
            // Ten seconds later: gone.
            let t2 = t1 + RESET_DONE_VISIBLE;
            scan(b, true, &mut det, &mut st, &[], t2);
            assert!(st.is_none());
        }
    }

    #[test]
    fn segment_reaches_done_even_if_the_takeover_cleared_the_detector_early() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = None;
        scan(
            ResetBehavior::Dazzle,
            false,
            &mut det,
            &mut st,
            &[(5, "tt-smi -r")],
            t0,
        );
        det.clear(); // the user skipped the takeover while tt-smi still runs
        scan(
            ResetBehavior::Dazzle,
            false,
            &mut det,
            &mut st,
            &[(5, "tt-smi -r")],
            t0,
        );
        assert!(!st.as_ref().unwrap().text(t0).unwrap().done);
        scan(ResetBehavior::Dazzle, false, &mut det, &mut st, &[], t0);
        assert!(st.as_ref().unwrap().text(t0).unwrap().done);
    }

    #[test]
    fn detector_is_free_for_the_next_reset_after_inform_and_hivemind() {
        let t0 = Instant::now();
        for hive in [false, true] {
            let mut det = ResetDetector::new();
            let mut st = None;
            scan(
                ResetBehavior::Inform,
                hive,
                &mut det,
                &mut st,
                &[(5, "tt-smi -r")],
                t0,
            );
            scan(ResetBehavior::Inform, hive, &mut det, &mut st, &[], t0);
            let out = scan(
                ResetBehavior::Dazzle,
                hive,
                &mut det,
                &mut st,
                &[(9, "tt-smi -r")],
                t0,
            );
            assert_eq!(st.as_ref().unwrap().pid(), 9, "hive={hive}");
            assert_eq!(out.takeover_for.is_some(), !hive);
            assert_eq!(out.feed_event_for.is_some(), hive);
        }
    }

    // ── status-bar fit ─────────────────────────────────────────────────

    #[test]
    fn fit_keeps_everything_when_it_all_fits() {
        let f = fit_left_zone(200, 25, Some((30, 20)), &[10, 10, 10], &[8]);
        assert_eq!(
            f,
            LeftFit {
                segment: SegmentFit::Full,
                hotkeys: 3,
                hints: 1
            }
        );
    }

    #[test]
    fn fit_drops_hotkey_groups_from_the_right_before_touching_the_segment() {
        // 1 lead + 30 seg + (5+10)*3 hotkeys = 76, + 25 right = 101.
        let f = fit_left_zone(90, 25, Some((30, 20)), &[10, 10, 10], &[]);
        assert_eq!(f.segment, SegmentFit::Full);
        assert_eq!(f.hotkeys, 2);
    }

    #[test]
    fn fit_shortens_then_hides_the_segment() {
        // Full segment (30) + right (25) + lead (1) = 56 > 50, short (20) fits.
        let f = fit_left_zone(50, 25, Some((30, 20)), &[10], &[]);
        assert_eq!((f.segment, f.hotkeys), (SegmentFit::Short, 0));
        let f = fit_left_zone(40, 25, Some((30, 20)), &[10], &[]);
        assert_eq!(f.segment, SegmentFit::Hidden);
    }

    #[test]
    fn fit_hotkey_widths_with_no_segment() {
        let f = fit_left_zone(60, 25, None, &[10, 10, 10], &[]);
        assert_eq!(f.segment, SegmentFit::Hidden);
        // 1 + 10 + 15 + 15 = 41 + 25 = 66 > 60, so two groups: 26 + 25 = 51.
        assert_eq!(f.hotkeys, 2);
    }

    #[test]
    fn fit_drops_hints_only_after_every_hotkey_group_is_gone() {
        // Segment 30 + hotkeys 3x10 + hints 2x8, right 25.
        // All 7 items need 1 + 30+30+16 + 5*6 = 107 + 25 = 132. At 110 one
        // hotkey group has to go, both hints stay.
        let f = fit_left_zone(110, 25, Some((30, 20)), &[10, 10, 10], &[8, 8]);
        assert_eq!((f.hotkeys, f.hints), (1, 2), "hotkeys go first");
        // Width 82: no hotkeys keeps 1+30+16+5*2 = 57 + 25 = 82, both hints.
        let f = fit_left_zone(82, 25, Some((30, 20)), &[10, 10, 10], &[8, 8]);
        assert_eq!((f.hotkeys, f.hints), (0, 2));
        // Width 70: a hint has to go, from the right.
        let f = fit_left_zone(70, 25, Some((30, 20)), &[10, 10, 10], &[8, 8]);
        assert_eq!((f.hotkeys, f.hints), (0, 1));
    }
}
