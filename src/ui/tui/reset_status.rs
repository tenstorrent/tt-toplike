// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure state for the `tt-smi -r` status-bar segment shown by
//! `--tt-smi-reset-behavior inform|dazzle|demo`, the per-scan decision
//! logic that feeds it, and the width-fitting rule for the status bar's
//! left zone. Nothing here touches terminal types or the clock: callers
//! pass `now` and the process snapshot, so tests do not sleep.
//!
//! Lifecycle of the segment text:
//! 1. `⟳ tt-smi -r · {scope} · resetting` while any `tt-smi -r` process is
//!    alive. The scope is that of the most recently started one.
//! 2. `✓ tt-smi -r done` for [`RESET_DONE_VISIBLE`] after the last one ends.
//! 3. Nothing.
//!
//! [`ResetStatus`] tracks every live `tt-smi -r` process on its own. It does
//! not depend on the takeover's [`ResetDetector`]. The detector only records
//! which reset a takeover animation is waiting on, so a second reset that
//! starts while a takeover runs gets no takeover of its own, but it still
//! shows in the segment and is still reported once to HivemindSweeper.

use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthStr;

use crate::animation::takeover::{pick_takeover, takeover_fits, Takeover};
use crate::cli::ResetBehavior;
use crate::models::device::Device;
use crate::workload::reset_detect::{parse_reset_process, ResetDetector, ResetEvent};
use crossterm::event::KeyCode;
use ratatui::layout::Rect;

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

/// One live `tt-smi -r` process that has already been reported.
#[derive(Debug, Clone)]
struct LiveReset {
    /// The cmdline seen when the reset was first detected. The process is
    /// identified by pid and cmdline together, so a pid the kernel reuses
    /// for a different command between two scans counts as a new reset.
    cmdline: String,
    event: ResetEvent,
}

impl LiveReset {
    fn matches(&self, pid: i32, cmdline: &str) -> bool {
        self.event.pid == pid && self.cmdline == cmdline
    }
}

/// Every live `tt-smi -r` process, plus when the last one ended. This is
/// also the set of already-handled resets: a process in `live` is never
/// reported again, and it leaves the set on the first scan it is absent
/// from, so the set stays as small as the number of running resets.
#[derive(Debug, Clone, Default)]
pub struct ResetStatus {
    /// Live resets in the order they were first seen. The last entry is the
    /// most recently started one.
    live: Vec<LiveReset>,
    /// When `live` last became empty. Cleared when a new reset starts.
    finished_at: Option<Instant>,
}

impl ResetStatus {
    /// No reset seen yet: nothing is drawn.
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the tracked set from one process snapshot (`(pid, name,
    /// cmdline)` triples) and returns the resets seen for the first time,
    /// in snapshot order. Resets that have left the snapshot are dropped.
    /// When the last one goes, the "done" countdown starts at `now`.
    pub fn observe(
        &mut self,
        processes: &[(i32, String, String)],
        devices: &[Device],
        now: Instant,
    ) -> Vec<ResetEvent> {
        let had_live = !self.live.is_empty();
        self.live
            .retain(|r| processes.iter().any(|(pid, _, cmd)| r.matches(*pid, cmd)));
        let mut new = Vec::new();
        for (pid, name, cmdline) in processes {
            if self.live.iter().any(|r| r.matches(*pid, cmdline)) {
                continue;
            }
            if let Some(ev) = parse_reset_process(*pid, name, cmdline, devices) {
                self.live.push(LiveReset {
                    cmdline: cmdline.clone(),
                    event: ev.clone(),
                });
                new.push(ev);
            }
        }
        if !self.live.is_empty() {
            self.finished_at = None;
        } else if had_live {
            self.finished_at = Some(now);
        }
        new
    }

    /// The segment to draw at `now`, or `None` when no reset is running and
    /// the "done" text has been visible for [`RESET_DONE_VISIBLE`].
    pub fn text(&self, now: Instant) -> Option<ResetSegment> {
        if let Some(latest) = self.live.last() {
            // Same scope wording as the Quiet Notice takeover.
            let ev = &latest.event;
            let scope = if ev.is_full {
                "all chips".to_string()
            } else {
                format!("{} chip(s)", ev.chip_count)
            };
            return Some(ResetSegment {
                text: format!("⟳ tt-smi -r · {scope} · resetting"),
                short: "⟳ tt-smi -r · resetting".to_string(),
                done: false,
            });
        }
        let finished = self.finished_at?;
        if now.saturating_duration_since(finished) >= RESET_DONE_VISIBLE {
            return None;
        }
        let t = "✓ tt-smi -r done".to_string();
        Some(ResetSegment {
            text: t.clone(),
            short: t,
            done: true,
        })
    }
}

/// What the TUI loop should do after one process scan.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    /// Set when a new reset was detected and a takeover animation should
    /// start for it (`dazzle`/`demo`, never in HivemindSweeper, and only
    /// while no takeover is already waiting on a reset).
    pub takeover_for: Option<ResetEvent>,
    /// Every reset seen for the first time this scan, when the view is
    /// HivemindSweeper. Each becomes one feed event.
    pub feed_events: Vec<ResetEvent>,
    /// True when the reset the running takeover waits on has ended. The
    /// caller passes this on with `Takeover::note_reset_finished`.
    pub takeover_reset_finished: bool,
}

/// One scan of the process list for the reset feature.
///
/// Does nothing, and never calls `snapshot`, unless `behavior.detects()`.
/// Otherwise it takes one snapshot, updates `status` with it, and tells the
/// caller what to do: one feed event per new reset in HivemindSweeper, or a
/// takeover for the newest new reset when the behavior animates and
/// `detector` is free. A takeover marks `detector` as occupied until the
/// caller clears it when the animation ends.
pub fn scan_resets(
    behavior: ResetBehavior,
    hivemind: bool,
    detector: &mut ResetDetector,
    status: &mut ResetStatus,
    snapshot: impl FnOnce() -> Vec<(i32, String, String)>,
    devices: &[Device],
    now: Instant,
) -> ScanOutcome {
    let mut out = ScanOutcome::default();
    if !behavior.detects() {
        return out;
    }
    let processes = snapshot();
    let new = status.observe(&processes, devices, now);
    if hivemind {
        // HivemindSweeper never gets a takeover; each reset is a feed event.
        out.feed_events = new;
    } else if behavior.animates() && !detector.is_active() {
        if let Some(ev) = new.last() {
            detector.begin(ev.clone());
            out.takeover_for = Some(ev.clone());
        }
    }
    out.takeover_reset_finished = detector.is_finished(&processes);
    out
}

/// The takeover half of the loop's hand-off after [`scan_resets`]: starts
/// the takeover for `outcome.takeover_for` (see [`replace_takeover`]) and
/// tells the running takeover when its reset has ended. Feed events are
/// injected by the caller, which owns the Hivemind engine.
pub fn apply_takeover_outcome(
    takeover: &mut Option<Takeover>,
    behavior: ResetBehavior,
    outcome: &ScanOutcome,
) {
    if let Some(ev) = &outcome.takeover_for {
        replace_takeover(takeover, behavior, ev);
    }
    if outcome.takeover_reset_finished {
        if let Some(t) = takeover.as_mut() {
            t.note_reset_finished();
        }
    }
}

/// Advances the running takeover by `dt`. When it is done it is removed and
/// `detector` is cleared, so the next reset can start a takeover. Without
/// the clear, `dazzle` would show one takeover per session.
pub fn tick_takeover(takeover: &mut Option<Takeover>, detector: &mut ResetDetector, dt: Duration) {
    if let Some(t) = takeover.as_mut() {
        t.tick(dt);
        if t.is_done() {
            *takeover = None;
            detector.clear();
        }
    }
}

/// What a keypress does to the active takeover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// End the whole demo (consumed; the app does not quit).
    End,
    /// Skip to the next animation, or finish a single takeover.
    Skip,
}

/// Routes a keypress made while a takeover is on screen. During a demo, Esc
/// and `q`/`Q` end the whole demo and every other key skips to the next
/// animation. A single takeover (`dazzle`) treats every key as a skip,
/// `q` included, as it always has.
pub fn demo_key_action(code: KeyCode, is_demo: bool) -> KeyAction {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') if is_demo => KeyAction::End,
        _ => KeyAction::Skip,
    }
}

/// Applies [`demo_key_action`] to `takeover`.
pub fn apply_key_action(takeover: &mut Takeover, code: KeyCode) {
    match demo_key_action(code, takeover.is_demo()) {
        KeyAction::End => takeover.end(),
        KeyAction::Skip => takeover.skip(),
    }
}

/// True when the boot demo should start: the behavior is `demo` and the
/// first view is not HivemindSweeper (which never gets a takeover).
pub fn should_start_boot_demo(behavior: ResetBehavior, hivemind: bool) -> bool {
    behavior.is_demo() && !hivemind
}

/// The takeover to show at boot, if any. `behavior` must be `demo`,
/// `hivemind` (true when the first view is HivemindSweeper) must be false,
/// and `area` (the whole terminal) must be at least the 8x4 that
/// `render_takeover_frame` draws at. On a smaller terminal the demo would
/// play unseen and swallow the user's keys for up to 56 s, so it does not
/// start. `device_count` is the backend's real device count.
pub fn boot_takeover(
    behavior: ResetBehavior,
    hivemind: bool,
    device_count: usize,
    area: Rect,
) -> Option<Takeover> {
    if should_start_boot_demo(behavior, hivemind) && takeover_fits(area) {
        Some(Takeover::demo_boot(device_count))
    } else {
        None
    }
}

/// The takeover for a detected real reset: the demo sequence under `demo`,
/// otherwise one weighted-random animation.
pub fn takeover_for_event(behavior: ResetBehavior, ev: &ResetEvent) -> Takeover {
    if behavior.is_demo() {
        Takeover::demo_real(ev)
    } else {
        pick_takeover(ev)
    }
}

/// Puts the takeover for a detected real reset into `slot`. It replaces
/// whatever is running, including a boot demo that has not finished.
pub fn replace_takeover(slot: &mut Option<Takeover>, behavior: ResetBehavior, ev: &ResetEvent) {
    *slot = Some(takeover_for_event(behavior, ev));
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

    /// The status text at `now`, or `None` when nothing is drawn.
    fn seg(st: &ResetStatus, now: Instant) -> Option<String> {
        st.text(now).map(|s| s.text)
    }

    const ALL: &str = "⟳ tt-smi -r · all chips · resetting";
    const ONE: &str = "⟳ tt-smi -r · 1 chip(s) · resetting";
    const DONE: &str = "✓ tt-smi -r done";

    // ── ResetStatus: the segment text ──────────────────────────────────

    #[test]
    fn resetting_text_full_and_subset() {
        let t0 = Instant::now();
        let mut st = ResetStatus::new();
        st.observe(&procs(&[(1, "tt-smi -r")]), &[], t0);
        let full = st.text(t0).unwrap();
        assert_eq!(full.text, ALL);
        assert_eq!(full.short, "⟳ tt-smi -r · resetting");
        assert!(!full.done);
        let mut st = ResetStatus::new();
        st.observe(&procs(&[(1, "tt-smi -r 2")]), &[], t0);
        assert_eq!(seg(&st, t0).unwrap(), ONE);
        let mut st = ResetStatus::new();
        st.observe(&procs(&[(1, "tt-smi -r 0,1,2")]), &[], t0);
        assert_eq!(seg(&st, t0).unwrap(), "⟳ tt-smi -r · 3 chip(s) · resetting");
    }

    #[test]
    fn nothing_is_drawn_before_any_reset() {
        let t0 = Instant::now();
        let mut st = ResetStatus::new();
        assert!(st.text(t0).is_none());
        st.observe(&procs(&[(1, "tt-smi -s"), (2, "bash")]), &[], t0);
        assert!(st.text(t0).is_none());
    }

    #[test]
    fn done_is_visible_for_ten_seconds_then_gone() {
        let t0 = Instant::now();
        let mut st = ResetStatus::new();
        st.observe(&procs(&[(1, "tt-smi -r")]), &[], t0);
        let t1 = t0 + Duration::from_secs(3);
        st.observe(&procs(&[]), &[], t1);
        let done = st.text(t1).unwrap();
        assert_eq!((done.text.as_str(), done.short.as_str()), (DONE, DONE));
        assert!(done.done);
        assert!(st.text(t1 + Duration::from_millis(9_900)).is_some());
        assert!(st.text(t1 + RESET_DONE_VISIBLE).is_none());
    }

    #[test]
    fn resetting_never_expires_on_its_own() {
        let t0 = Instant::now();
        let mut st = ResetStatus::new();
        st.observe(&procs(&[(1, "tt-smi -r")]), &[], t0);
        let later = t0 + Duration::from_secs(3600);
        st.observe(&procs(&[(1, "tt-smi -r")]), &[], later);
        assert_eq!(seg(&st, later).unwrap(), ALL);
    }

    #[test]
    fn later_empty_scans_keep_the_first_finish_time() {
        let t0 = Instant::now();
        let mut st = ResetStatus::new();
        st.observe(&procs(&[(1, "tt-smi -r")]), &[], t0);
        st.observe(&procs(&[]), &[], t0);
        st.observe(&procs(&[]), &[], t0 + Duration::from_secs(8));
        assert!(st.text(t0 + Duration::from_secs(10)).is_none());
    }

    #[test]
    fn a_new_reset_during_done_goes_back_to_resetting() {
        let t0 = Instant::now();
        let mut st = ResetStatus::new();
        st.observe(&procs(&[(1, "tt-smi -r")]), &[], t0);
        st.observe(&procs(&[]), &[], t0);
        assert_eq!(seg(&st, t0).unwrap(), DONE);
        let t1 = t0 + Duration::from_secs(4);
        st.observe(&procs(&[(2, "tt-smi -r 1")]), &[], t1);
        assert_eq!(seg(&st, t1).unwrap(), ONE);
        // The old finish time does not expire the new reset.
        assert_eq!(seg(&st, t1 + Duration::from_secs(20)).unwrap(), ONE);
    }

    // ── scan_resets: gating + lifecycle ────────────────────────────────

    fn scan(
        behavior: ResetBehavior,
        hivemind: bool,
        det: &mut ResetDetector,
        status: &mut ResetStatus,
        list: &[(i32, &str)],
        now: Instant,
    ) -> ScanOutcome {
        scan_resets(behavior, hivemind, det, status, || procs(list), &[], now)
    }

    fn feed_pids(out: &ScanOutcome) -> Vec<i32> {
        out.feed_events.iter().map(|e| e.pid).collect()
    }

    #[test]
    fn ignore_never_takes_a_process_snapshot() {
        let called = std::cell::Cell::new(false);
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let out = scan_resets(
            ResetBehavior::Ignore,
            false,
            &mut det,
            &mut st,
            || {
                called.set(true);
                procs(&[(5, "tt-smi -r")])
            },
            &[],
            Instant::now(),
        );
        assert!(!called.get(), "ignore must not read the process list");
        assert!(out.takeover_for.is_none() && out.feed_events.is_empty());
        assert!(st.text(Instant::now()).is_none());
        assert!(!det.is_active());
    }

    #[test]
    fn every_detecting_behavior_takes_the_snapshot() {
        for b in [
            ResetBehavior::Inform,
            ResetBehavior::Dazzle,
            ResetBehavior::Demo,
        ] {
            let called = std::cell::Cell::new(false);
            let mut det = ResetDetector::new();
            let mut st = ResetStatus::new();
            scan_resets(
                b,
                false,
                &mut det,
                &mut st,
                || {
                    called.set(true);
                    vec![]
                },
                &[],
                Instant::now(),
            );
            assert!(called.get(), "{b:?}");
        }
    }

    #[test]
    fn inform_shows_a_segment_without_a_takeover() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let out = scan(
            ResetBehavior::Inform,
            false,
            &mut det,
            &mut st,
            &[(5, "tt-smi -r")],
            t0,
        );
        assert!(out.takeover_for.is_none());
        assert!(out.feed_events.is_empty());
        assert_eq!(seg(&st, t0).unwrap(), ALL);
        assert!(!det.is_active(), "inform never occupies the detector");
    }

    #[test]
    fn dazzle_and_demo_request_a_takeover() {
        let t0 = Instant::now();
        for b in [ResetBehavior::Dazzle, ResetBehavior::Demo] {
            let mut det = ResetDetector::new();
            let mut st = ResetStatus::new();
            let out = scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            assert_eq!(out.takeover_for.as_ref().map(|e| e.pid), Some(5), "{b:?}");
            assert!(out.feed_events.is_empty());
            assert_eq!(seg(&st, t0).unwrap(), ALL);
            assert!(det.is_active());
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
            let mut st = ResetStatus::new();
            // Detected: resetting segment, feed event, no takeover.
            let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            assert!(out.takeover_for.is_none(), "{b:?}");
            assert_eq!(feed_pids(&out), vec![5]);
            assert_eq!(seg(&st, t0).unwrap(), ALL);
            // Still running one scan later: still resetting, no repeat event.
            let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            assert!(out.feed_events.is_empty(), "same pid is not re-injected");
            assert_eq!(seg(&st, t0).unwrap(), ALL);
            // Process ends: done.
            let t1 = t0 + Duration::from_secs(4);
            scan(b, true, &mut det, &mut st, &[], t1);
            assert_eq!(seg(&st, t1).unwrap(), DONE);
            // Ten seconds later: gone.
            let t2 = t1 + RESET_DONE_VISIBLE;
            scan(b, true, &mut det, &mut st, &[], t2);
            assert!(st.text(t2).is_none());
        }
    }

    #[test]
    fn segment_reaches_done_even_if_the_takeover_cleared_the_detector_early() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let b = ResetBehavior::Dazzle;
        scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        det.clear(); // the user skipped the takeover while tt-smi still runs
        let out = scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        assert!(
            out.takeover_for.is_none(),
            "a skipped reset is not replayed"
        );
        assert_eq!(seg(&st, t0).unwrap(), ALL);
        scan(b, false, &mut det, &mut st, &[], t0);
        assert_eq!(seg(&st, t0).unwrap(), DONE);
    }

    #[test]
    fn the_takeover_hears_when_its_own_reset_ends() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let b = ResetBehavior::Dazzle;
        let out = scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        assert!(!out.takeover_reset_finished);
        // A second reset ending does not finish the takeover's reset.
        let out = scan(
            b,
            false,
            &mut det,
            &mut st,
            &[(5, "tt-smi -r"), (6, "tt-smi -r 1")],
            t0,
        );
        assert!(!out.takeover_reset_finished);
        let out = scan(b, false, &mut det, &mut st, &[(6, "tt-smi -r 1")], t0);
        assert!(out.takeover_reset_finished);
    }

    // ── overlapping resets, pid reuse, occupied detector ───────────────

    #[test]
    fn two_overlapping_resets_give_one_feed_event_each_and_a_stable_status() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let b = ResetBehavior::Inform;
        // A (all chips) starts, then B (one chip) starts while A runs.
        let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        assert_eq!(feed_pids(&out), vec![5]);
        let both = [(5, "tt-smi -r"), (6, "tt-smi -r 1")];
        let out = scan(b, true, &mut det, &mut st, &both, t0);
        assert_eq!(feed_pids(&out), vec![6]);
        assert_eq!(seg(&st, t0).unwrap(), ONE, "the newer reset's scope");
        // Both keep running: no repeat events, and the text does not flip.
        for i in 1..=5 {
            let now = t0 + Duration::from_secs(2 * i);
            let out = scan(b, true, &mut det, &mut st, &both, now);
            assert!(
                out.feed_events.is_empty(),
                "scan {i}: {:?}",
                feed_pids(&out)
            );
            assert_eq!(seg(&st, now).unwrap(), ONE, "scan {i}");
        }
        // The same holds outside HivemindSweeper.
        let mut det2 = ResetDetector::new();
        let mut st2 = ResetStatus::new();
        scan(b, false, &mut det2, &mut st2, &[(5, "tt-smi -r")], t0);
        for _ in 0..4 {
            scan(b, false, &mut det2, &mut st2, &both, t0);
            assert_eq!(seg(&st2, t0).unwrap(), ONE);
        }
        // B ends: A is still running, so the text is A's.
        let t1 = t0 + Duration::from_secs(20);
        scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r")], t1);
        assert_eq!(seg(&st, t1).unwrap(), ALL);
        // A ends too: done, then gone.
        scan(b, true, &mut det, &mut st, &[], t1);
        assert_eq!(seg(&st, t1).unwrap(), DONE);
        assert!(st.text(t1 + RESET_DONE_VISIBLE).is_none());
    }

    #[test]
    fn two_resets_first_seen_in_the_same_scan_give_two_feed_events() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let both = [(5, "tt-smi -r"), (6, "tt-smi -r 1")];
        let out = scan(ResetBehavior::Inform, true, &mut det, &mut st, &both, t0);
        assert_eq!(feed_pids(&out), vec![5, 6]);
        // Outside HivemindSweeper the takeover is for the newer one.
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let out = scan(ResetBehavior::Dazzle, false, &mut det, &mut st, &both, t0);
        assert_eq!(out.takeover_for.map(|e| e.pid), Some(6));
        assert_eq!(seg(&st, t0).unwrap(), ONE);
    }

    #[test]
    fn a_second_reset_during_an_occupied_detector_appears_in_the_status() {
        let t0 = Instant::now();
        for b in [ResetBehavior::Dazzle, ResetBehavior::Demo] {
            let mut det = ResetDetector::new();
            let mut st = ResetStatus::new();
            let out = scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            assert!(out.takeover_for.is_some());
            let out = scan(
                b,
                false,
                &mut det,
                &mut st,
                &[(5, "tt-smi -r"), (6, "tt-smi -r 1")],
                t0,
            );
            assert!(out.takeover_for.is_none(), "{b:?}: no second takeover");
            assert_eq!(seg(&st, t0).unwrap(), ONE, "{b:?}");
        }
    }

    #[test]
    fn a_reset_that_runs_entirely_inside_an_occupied_detector_still_shows_done() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let b = ResetBehavior::Demo;
        // A starts a 56 s demo and ends after 2 s. The demo keeps running,
        // so the detector stays occupied.
        scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        let t1 = t0 + Duration::from_secs(2);
        scan(b, false, &mut det, &mut st, &[], t1);
        assert_eq!(seg(&st, t1).unwrap(), DONE);
        // B starts and ends while the demo is still on screen.
        let t2 = t0 + Duration::from_secs(20);
        let out = scan(b, false, &mut det, &mut st, &[(9, "tt-smi -r 3")], t2);
        assert!(out.takeover_for.is_none());
        assert!(det.is_active());
        assert_eq!(seg(&st, t2).unwrap(), ONE);
        let t3 = t2 + Duration::from_secs(2);
        scan(b, false, &mut det, &mut st, &[], t3);
        assert_eq!(seg(&st, t3).unwrap(), DONE);
        assert!(st.text(t3 + Duration::from_millis(9_900)).is_some());
        assert!(st.text(t3 + RESET_DONE_VISIBLE).is_none());
    }

    #[test]
    fn pid_reuse_with_a_different_cmdline_is_a_new_reset() {
        let t0 = Instant::now();
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let b = ResetBehavior::Inform;
        let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        assert_eq!(feed_pids(&out), vec![5]);
        // Between two scans pid 5 exited and was reused by another reset.
        let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r 1")], t0);
        assert_eq!(feed_pids(&out), vec![5], "reported as a new reset");
        assert_eq!(seg(&st, t0).unwrap(), ONE);
        // The same pid and cmdline again is the same reset.
        let out = scan(b, true, &mut det, &mut st, &[(5, "tt-smi -r 1")], t0);
        assert!(out.feed_events.is_empty());
        // The reused pid ends: done (the old command is gone as well).
        scan(b, true, &mut det, &mut st, &[(5, "bash")], t0);
        assert_eq!(seg(&st, t0).unwrap(), DONE);
    }

    #[test]
    fn after_everything_ends_the_next_reset_works() {
        let t0 = Instant::now();
        for hive in [false, true] {
            let mut det = ResetDetector::new();
            let mut st = ResetStatus::new();
            let b = ResetBehavior::Dazzle;
            scan(b, hive, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            scan(
                b,
                hive,
                &mut det,
                &mut st,
                &[(5, "tt-smi -r"), (6, "tt-smi -r 1")],
                t0,
            );
            det.clear(); // the takeover (if any) finished
            scan(b, hive, &mut det, &mut st, &[], t0);
            assert_eq!(seg(&st, t0).unwrap(), DONE);
            // A new reset, even one that reuses pid 5 with the same command,
            // is detected normally.
            let t1 = t0 + Duration::from_secs(30);
            let out = scan(b, hive, &mut det, &mut st, &[(5, "tt-smi -r")], t1);
            assert_eq!(out.takeover_for.is_some(), !hive, "hive={hive}");
            assert_eq!(feed_pids(&out), if hive { vec![5] } else { vec![] });
            assert_eq!(seg(&st, t1).unwrap(), ALL);
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

    #[test]
    fn fit_never_exceeds_the_bar_width() {
        // Left-zone width of a fit, computed the same way the bar draws it.
        let used = |f: &LeftFit, seg: (usize, usize), hk: &[usize], ht: &[usize]| {
            let seg_w = match f.segment {
                SegmentFit::Full => Some(seg.0),
                SegmentFit::Short => Some(seg.1),
                SegmentFit::Hidden => None,
            };
            let ws: Vec<usize> = seg_w
                .into_iter()
                .chain(hk[..f.hotkeys].iter().copied())
                .chain(ht[..f.hints].iter().copied())
                .collect();
            1 + ws.iter().sum::<usize>() + LEFT_SEP_WIDTH * ws.len().saturating_sub(1)
        };
        let hk = [12, 9, 14, 11];
        let ht = [7, 6, 25];
        for right in [0, 25, 60] {
            for seg in [Some((34, 22)), None] {
                for width in 0..=220 {
                    let f = fit_left_zone(width, right, seg, &hk, &ht);
                    let empty = f.segment == SegmentFit::Hidden && f.hotkeys == 0 && f.hints == 0;
                    if empty {
                        continue; // nothing on the left; only the right zone is drawn
                    }
                    let left = used(&f, seg.unwrap_or((0, 0)), &hk, &ht);
                    assert!(
                        left + right <= width,
                        "width {width} right {right} seg {seg:?}: {f:?} uses {left}"
                    );
                }
            }
        }
    }

    // ── demo wiring: keys, boot gating, takeover choice ────────────────

    use crossterm::event::KeyCode;

    #[test]
    fn demo_keys_end_on_esc_and_q_and_skip_on_anything_else() {
        for code in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char('Q')] {
            assert_eq!(demo_key_action(code, true), KeyAction::End, "{code:?}");
        }
        for code in [
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('x'),
            KeyCode::Right,
            KeyCode::Tab,
        ] {
            assert_eq!(demo_key_action(code, true), KeyAction::Skip, "{code:?}");
        }
    }

    #[test]
    fn plain_takeover_keys_always_skip_including_q_and_esc() {
        for code in [
            KeyCode::Esc,
            KeyCode::Char('q'),
            KeyCode::Char('Q'),
            KeyCode::Enter,
            KeyCode::Char('a'),
        ] {
            assert_eq!(demo_key_action(code, false), KeyAction::Skip, "{code:?}");
        }
    }

    #[test]
    fn a_key_action_applied_to_a_takeover_ends_or_skips_it() {
        use crate::animation::takeover::Takeover;
        // End: the whole demo is over, whatever slot it was in.
        let mut t = Takeover::demo_boot(2);
        apply_key_action(&mut t, KeyCode::Char('q'));
        assert!(t.is_done());
        // Skip: one slot forward.
        let mut t = Takeover::demo_boot(2);
        apply_key_action(&mut t, KeyCode::Enter);
        assert!(!t.is_done());
        assert_eq!(t.variant_name(), "BlackholeSwarm");
        // A plain takeover: q only skips it.
        let mut t = crate::animation::takeover::pick_takeover(&ev(1, true, 4));
        apply_key_action(&mut t, KeyCode::Char('q'));
        assert!(t.is_done());
    }

    #[test]
    fn boot_demo_starts_only_for_demo_and_never_in_hivemind() {
        for b in [
            ResetBehavior::Ignore,
            ResetBehavior::Inform,
            ResetBehavior::Dazzle,
        ] {
            assert!(!should_start_boot_demo(b, false), "{b:?}");
            assert!(!should_start_boot_demo(b, true), "{b:?}");
        }
        assert!(should_start_boot_demo(ResetBehavior::Demo, false));
        assert!(!should_start_boot_demo(ResetBehavior::Demo, true));
    }

    // ── tick_takeover + apply_takeover_outcome: the loop's wiring ──────

    #[test]
    fn a_finished_takeover_frees_the_detector_for_the_next_reset() {
        let t0 = Instant::now();
        let b = ResetBehavior::Dazzle;
        // Two ways a takeover ends: the user skips it, or its reset ends
        // and the done tail plays out.
        for skipped in [true, false] {
            let mut det = ResetDetector::new();
            let mut st = ResetStatus::new();
            let mut takeover: Option<Takeover> = None;
            let out = scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
            apply_takeover_outcome(&mut takeover, b, &out);
            assert!(takeover.is_some() && det.is_active());
            if skipped {
                takeover.as_mut().unwrap().skip();
            } else {
                let out = scan(b, false, &mut det, &mut st, &[], t0);
                assert!(out.takeover_reset_finished);
                apply_takeover_outcome(&mut takeover, b, &out);
            }
            // Longer than any variant's done tail.
            tick_takeover(&mut takeover, &mut det, Duration::from_secs(10));
            assert!(takeover.is_none(), "skipped={skipped}: takeover ended");
            assert!(!det.is_active(), "skipped={skipped}: detector freed");
            // A later reset gets a takeover of its own.
            let out = scan(b, false, &mut det, &mut st, &[(9, "tt-smi -r 1")], t0);
            assert_eq!(out.takeover_for.as_ref().map(|e| e.pid), Some(9));
            apply_takeover_outcome(&mut takeover, b, &out);
            assert!(takeover.is_some(), "skipped={skipped}");
        }
    }

    #[test]
    fn a_running_takeover_keeps_the_detector_active() {
        let t0 = Instant::now();
        let b = ResetBehavior::Dazzle;
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let mut takeover: Option<Takeover> = None;
        let out = scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        apply_takeover_outcome(&mut takeover, b, &out);
        for _ in 0..20 {
            tick_takeover(&mut takeover, &mut det, Duration::from_millis(100));
        }
        assert!(takeover.is_some(), "the reset is still running");
        assert!(det.is_active());
        let out = scan(
            b,
            false,
            &mut det,
            &mut st,
            &[(5, "tt-smi -r"), (6, "tt-smi -r 1")],
            t0,
        );
        assert!(out.takeover_for.is_none());
        apply_takeover_outcome(&mut takeover, b, &out);
        tick_takeover(&mut takeover, &mut det, Duration::from_millis(100));
        assert!(takeover.is_some() && det.is_active());
    }

    #[test]
    fn apply_takeover_outcome_passes_the_reset_end_to_the_takeover() {
        let t0 = Instant::now();
        let b = ResetBehavior::Dazzle;
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let mut takeover: Option<Takeover> = None;
        let out = scan(b, false, &mut det, &mut st, &[(5, "tt-smi -r")], t0);
        apply_takeover_outcome(&mut takeover, b, &out);
        // The reset is still running: no amount of time ends the takeover.
        tick_takeover(&mut takeover, &mut det, Duration::from_secs(60));
        assert!(takeover.is_some());
        let out = scan(b, false, &mut det, &mut st, &[], t0);
        apply_takeover_outcome(&mut takeover, b, &out);
        tick_takeover(&mut takeover, &mut det, Duration::from_secs(10));
        assert!(takeover.is_none());
    }

    // ── boot_takeover: the arguments the boot site passes ──────────────

    fn screen(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn boot_takeover_starts_the_demo_only_for_demo_outside_hivemind() {
        let big = screen(134, 40);
        let t = boot_takeover(ResetBehavior::Demo, false, 3, big);
        assert!(t.as_ref().is_some_and(|t| t.is_demo()));
        assert!(
            boot_takeover(ResetBehavior::Demo, true, 3, big).is_none(),
            "HivemindSweeper never gets a takeover"
        );
        for b in [
            ResetBehavior::Ignore,
            ResetBehavior::Inform,
            ResetBehavior::Dazzle,
        ] {
            assert!(boot_takeover(b, false, 3, big).is_none(), "{b:?}");
        }
    }

    #[test]
    fn boot_takeover_uses_the_real_device_count() {
        use crate::animation::takeover::Takeover;
        for n in [1usize, 2, 4] {
            match boot_takeover(ResetBehavior::Demo, false, n, screen(134, 40)) {
                Some(Takeover::Demo(seq)) => {
                    assert_eq!(seq.event().total_devices, n);
                    assert_eq!(seq.event().chip_count, n);
                }
                _ => panic!("expected a demo for {n} devices"),
            }
        }
    }

    #[test]
    fn boot_takeover_skips_a_terminal_too_small_to_draw_the_box() {
        // 8x4 is the smallest size render_takeover_frame draws at.
        assert!(boot_takeover(ResetBehavior::Demo, false, 2, screen(8, 4)).is_some());
        for (w, h) in [(7, 4), (8, 3), (7, 3), (0, 0), (200, 2), (5, 60)] {
            assert!(
                boot_takeover(ResetBehavior::Demo, false, 2, screen(w, h)).is_none(),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn demo_in_hivemind_turns_a_real_reset_into_a_feed_event_only() {
        let mut det = ResetDetector::new();
        let mut st = ResetStatus::new();
        let out = scan_resets(
            ResetBehavior::Demo,
            true,
            &mut det,
            &mut st,
            || procs(&[(5, "tt-smi -r")]),
            &[],
            Instant::now(),
        );
        assert!(out.takeover_for.is_none());
        assert_eq!(out.feed_events.len(), 1);
    }

    #[test]
    fn demo_behavior_picks_the_demo_and_dazzle_never_does() {
        let e = ev(9, true, 4);
        assert!(takeover_for_event(ResetBehavior::Demo, &e).is_demo());
        for _ in 0..50 {
            assert!(!takeover_for_event(ResetBehavior::Dazzle, &e).is_demo());
        }
    }

    #[test]
    fn a_real_reset_preempts_a_running_boot_demo() {
        use crate::animation::takeover::{DemoSource, Takeover};
        let mut slot = Some(Takeover::demo_boot(4));
        // The boot demo has moved on to its third animation.
        slot.as_mut().unwrap().skip();
        slot.as_mut().unwrap().skip();
        replace_takeover(&mut slot, ResetBehavior::Demo, &ev(9, true, 4));
        match slot.as_ref().unwrap() {
            Takeover::Demo(seq) => {
                assert_eq!(seq.source(), DemoSource::RealReset);
                assert_eq!(seq.current_index(), 0, "a fresh sequence starts");
                assert_eq!(seq.event().pid, 9);
            }
            _ => panic!("expected a demo"),
        }
    }
}
