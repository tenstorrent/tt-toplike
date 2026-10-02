// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The `demo` reset behavior: all seven takeovers played in a fixed order.
//!
//! `--tt-smi-reset-behavior demo` plays the whole sequence once at boot and
//! again on every real `tt-smi -r`. [`DemoSequence`] is the sequencer. It
//! lives inside `Takeover::Demo`, so the TUI loop drives it with the same
//! `tick`, `render`, `is_done` and `skip` calls it uses for one animation.
//!
//! # Deliberate exception to the lifecycle rule
//!
//! Every other takeover follows the real reset: `note_reset_finished` fires
//! the tick the real process exits. A demo does not. Each slot is a fixed
//! [`SLOT_LEN`] long and calls `note_reset_finished` on the variant at
//! [`RESOLVE_AT`], and the real reset's own finish is ignored, so all seven
//! animations always play out. Because that timing is staged, the box title
//! carries a `DEMO` tag (see [`DemoSequence::tag`]) so a viewer cannot take a
//! staged animation for a real reset.
//!
//! Time comes only from `tick(dt)`; nothing here reads a clock, so tests
//! drive the whole sequence deterministically.

use super::{
    BbsTakeover, BlackholeSwarmTakeover, FailWhaleTakeover, HatchCountdownTakeover,
    MissileCommandTakeover, QuietNoticeTakeover, Takeover, TrekResetTakeover,
};
use crate::workload::reset_detect::ResetEvent;
use ratatui::layout::Rect;
use ratatui::Frame;
use std::time::Duration;

/// How long each animation holds the screen in a demo. A slot ends once this
/// much time has passed and the animation reports done, so a variant whose
/// own "done" tail runs past the slot finishes that tail first.
pub const SLOT_LEN: Duration = Duration::from_secs(8);

/// When in a slot the variant is told its reset finished, so it plays its
/// "done" beat. Until then it shows its "resetting" beat.
pub const RESOLVE_AT: Duration = Duration::from_secs(5);

/// Number of animations in the sequence.
const SLOT_COUNT: usize = 7;

/// What started a demo run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoSource {
    /// The boot sequence. Nothing real is resetting.
    Boot,
    /// A real `tt-smi -r` was detected while the behavior is `demo`.
    RealReset,
}

/// The seven takeovers in their fixed order, built lazily as each slot
/// begins (so an animation's own clock starts when its slot does).
///
/// This type deliberately bends the project rule that takeovers follow the
/// real reset lifecycle: its timing is fixed and the real reset's finish is
/// ignored. The `DEMO` title tag marks the result as staged (see the module
/// docs).
pub struct DemoSequence {
    source: DemoSource,
    /// Chip scope and labels every slot is built from: the real event for a
    /// real reset, a synthetic full reset for the boot sequence.
    ev: ResetEvent,
    index: usize,
    current: Box<Takeover>,
    slot_elapsed: Duration,
    resolved: bool,
    ended: bool,
}

/// Builds the takeover for slot `index`: Quiet Notice, Blackhole Swarm,
/// Hatch Countdown, BBS, Trek, Fail Whale, Missile Command.
fn build_slot(index: usize, ev: &ResetEvent) -> Takeover {
    match index {
        0 => Takeover::QuietNotice(QuietNoticeTakeover::new(ev)),
        1 => Takeover::BlackholeSwarm(BlackholeSwarmTakeover::new(ev)),
        2 => Takeover::HatchCountdown(HatchCountdownTakeover::new(ev)),
        3 => Takeover::Bbs(BbsTakeover::new(ev)),
        4 => Takeover::TrekReset(TrekResetTakeover::new(ev)),
        5 => Takeover::FailWhale(FailWhaleTakeover::new(ev)),
        _ => Takeover::MissileCommand(MissileCommandTakeover::new(ev)),
    }
}

impl DemoSequence {
    pub fn new(source: DemoSource, ev: ResetEvent) -> Self {
        let current = Box::new(build_slot(0, &ev));
        Self {
            source,
            ev,
            index: 0,
            current,
            slot_elapsed: Duration::ZERO,
            resolved: false,
            ended: false,
        }
    }

    /// The text before the variant title in the box's title row.
    pub fn tag(&self) -> &'static str {
        match self.source {
            DemoSource::Boot => "DEMO",
            DemoSource::RealReset => "DEMO (real reset)",
        }
    }

    pub fn source(&self) -> DemoSource {
        self.source
    }

    /// The event every slot is built from.
    pub fn event(&self) -> &ResetEvent {
        &self.ev
    }

    /// Zero-based position of the running animation.
    pub fn current_index(&self) -> usize {
        self.index
    }

    /// True once the running slot has been told its reset finished.
    pub fn resolved(&self) -> bool {
        self.resolved
    }

    /// The running animation.
    pub fn current(&self) -> &Takeover {
        &self.current
    }

    pub fn current_name(&self) -> &'static str {
        self.current.variant_name()
    }

    pub fn tick(&mut self, dt: Duration) {
        if self.ended {
            return;
        }
        self.current.tick(dt);
        self.slot_elapsed += dt;
        if !self.resolved && self.slot_elapsed >= RESOLVE_AT {
            self.current.note_reset_finished();
            self.resolved = true;
        }
        if self.slot_elapsed >= SLOT_LEN && self.current.is_done() {
            self.advance();
        }
    }

    /// Moves to the next animation; past the last one the sequence ends.
    fn advance(&mut self) {
        if self.index + 1 >= SLOT_COUNT {
            self.ended = true;
            return;
        }
        self.index += 1;
        *self.current = build_slot(self.index, &self.ev);
        self.slot_elapsed = Duration::ZERO;
        self.resolved = false;
    }

    /// A demo ignores the real reset's finish. Each slot resolves at
    /// [`RESOLVE_AT`] and the sequence always runs to the end, so this does
    /// nothing.
    pub fn note_reset_finished(&mut self) {}

    /// Skips to the next animation (one slot, never two).
    pub fn skip(&mut self) {
        if !self.ended {
            self.advance();
        }
    }

    /// Ends the whole demo at once.
    pub fn end(&mut self) {
        self.ended = true;
    }

    pub fn is_done(&self) -> bool {
        self.ended
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        if !self.ended {
            self.current.render_tagged(f, area, self.tag());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{takeover_box, Takeover};
    use super::*;
    use crate::workload::reset_detect::ResetEvent;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Duration;

    const ORDER: [&str; 7] = [
        "QuietNotice",
        "BlackholeSwarm",
        "HatchCountdown",
        "Bbs",
        "TrekReset",
        "FailWhale",
        "MissileCommand",
    ];
    const STEP: Duration = Duration::from_millis(50);

    fn ev(chips: &[u8], total: usize) -> ResetEvent {
        ResetEvent {
            pid: 7,
            is_full: chips.len() == total,
            chip_count: chips.len(),
            total_devices: total,
            device_indices: chips.to_vec(),
            raw_targets: chips.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn boot() -> Takeover {
        Takeover::demo_boot(4)
    }

    /// Ticks in 50 ms steps until `is_done`, recording how long each slot
    /// (by variant name) lasted. Panics if the run exceeds `limit`.
    fn run_to_end(t: &mut Takeover, limit: Duration) -> Vec<(String, Duration)> {
        let mut slots: Vec<(String, Duration)> = vec![];
        let mut total = Duration::ZERO;
        while !t.is_done() {
            let name = t.variant_name().to_string();
            match slots.last_mut() {
                Some((n, d)) if *n == name => *d += STEP,
                _ => slots.push((name, STEP)),
            }
            t.tick(STEP);
            total += STEP;
            assert!(total <= limit, "demo ran past {limit:?}");
        }
        slots
    }

    #[test]
    fn plays_the_seven_takeovers_in_the_fixed_order() {
        let mut t = boot();
        let slots = run_to_end(&mut t, Duration::from_secs(80));
        let names: Vec<&str> = slots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ORDER);
    }

    #[test]
    fn each_slot_lasts_eight_seconds_plus_at_most_its_own_tail() {
        let mut t = boot();
        let slots = run_to_end(&mut t, Duration::from_secs(80));
        for (name, d) in &slots {
            assert!(*d >= Duration::from_secs(8), "{name} ended early at {d:?}");
            assert!(
                *d <= Duration::from_secs(8) + STEP,
                "{name} overran its slot: {d:?}"
            );
        }
        let total: Duration = slots.iter().map(|(_, d)| *d).sum();
        assert_eq!(total, Duration::from_secs(56));
    }

    #[test]
    fn the_slot_ends_at_eight_seconds_and_not_before() {
        let mut seq = DemoSequence::new(DemoSource::Boot, ev(&[0, 1, 2, 3], 4));
        let mut at = Duration::ZERO;
        while seq.current_index() == 0 {
            seq.tick(Duration::from_millis(10));
            at += Duration::from_millis(10);
            assert!(at <= Duration::from_secs(9), "slot never ended");
        }
        assert_eq!(at, Duration::from_secs(8));
    }

    #[test]
    fn resolve_point_is_five_seconds_and_reaches_the_variant() {
        for start in 0..7 {
            let mut seq = DemoSequence::new(DemoSource::Boot, ev(&[0, 1, 2, 3], 4));
            for _ in 0..start {
                seq.skip();
            }
            let mut at = Duration::ZERO;
            while !seq.resolved() {
                seq.tick(Duration::from_millis(10));
                at += Duration::from_millis(10);
                assert!(at <= Duration::from_secs(6), "never resolved");
            }
            assert_eq!(
                at,
                Duration::from_secs(5),
                "slot {start} resolved at {at:?}"
            );
            // The variant now plays its done beat and reports done within
            // its own tail (1.2 s is the longest).
            seq.tick(Duration::from_millis(1300));
            assert!(
                seq.current().is_done(),
                "slot {start}: variant never saw note_reset_finished"
            );
        }
    }

    #[test]
    fn skip_advances_exactly_one_slot_at_a_time() {
        let mut t = boot();
        for expected in 1..7 {
            t.skip();
            assert_eq!(t.variant_name(), ORDER[expected]);
            assert!(!t.is_done());
        }
    }

    #[test]
    fn skip_on_the_last_slot_ends_the_sequence() {
        let mut t = boot();
        for _ in 0..6 {
            t.skip();
        }
        assert!(!t.is_done());
        t.skip();
        assert!(t.is_done());
    }

    #[test]
    fn end_stops_the_sequence_from_any_slot() {
        for start in 0..7 {
            let mut t = boot();
            for _ in 0..start {
                t.skip();
            }
            t.end();
            assert!(t.is_done(), "end() left slot {start} running");
        }
    }

    #[test]
    fn a_real_reset_finishing_early_does_not_shorten_the_sequence() {
        let mut t = Takeover::demo_real(&ev(&[0, 1, 2, 3], 4));
        t.tick(Duration::from_secs(1));
        t.note_reset_finished();
        let slots = run_to_end(&mut t, Duration::from_secs(80));
        let total: Duration =
            slots.iter().map(|(_, d)| *d).sum::<Duration>() + Duration::from_secs(1);
        assert_eq!(slots.len(), 7);
        assert_eq!(total, Duration::from_secs(56));
    }

    /// Text of the box's top row after rendering `t` into a `w` x `h` buffer.
    fn top_row(t: &Takeover, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.render(f, f.area())).unwrap();
        let area = ratatui::layout::Rect::new(0, 0, w, h);
        let b = takeover_box(area);
        let buf = term.backend().buffer();
        (b.x..b.right()).map(|x| buf[(x, b.y)].symbol()).collect()
    }

    fn whole_screen(t: &Takeover, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.render(f, f.area())).unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn boot_title_row_carries_the_demo_tag_in_every_variant_and_size() {
        for (w, h) in [(134, 40), (50, 14)] {
            let mut t = boot();
            for i in 0..7 {
                let row = top_row(&t, w, h);
                assert!(row.contains("DEMO - "), "{} at {w}x{h}: {row:?}", ORDER[i]);
                assert!(!row.contains("real reset"), "{row:?}");
                t.skip();
            }
        }
    }

    #[test]
    fn real_title_row_carries_the_real_reset_tag_in_every_variant_and_size() {
        for (w, h) in [(134, 40), (50, 14)] {
            let mut t = Takeover::demo_real(&ev(&[1, 3], 4));
            for i in 0..7 {
                let row = top_row(&t, w, h);
                assert!(
                    row.contains("DEMO (real reset) - "),
                    "{} at {w}x{h}: {row:?}",
                    ORDER[i]
                );
                t.skip();
            }
        }
    }

    #[test]
    fn plain_takeovers_have_no_demo_tag() {
        use crate::animation::takeover::pick_takeover;
        for roll_ev in [ev(&[0, 1, 2, 3], 4), ev(&[2], 4)] {
            for _ in 0..40 {
                let t = pick_takeover(&roll_ev);
                assert!(!top_row(&t, 134, 40).contains("DEMO"));
            }
        }
    }

    #[test]
    fn subset_event_scopes_the_chip_labels_and_boot_uses_every_device() {
        // Slot 3 is BBS, the variant that prints a `CHIP n` line per
        // targeted chip. Missile Command (slot 6) prints no labels; it draws
        // one lane per device and is covered by the lane test below.
        let mut real = Takeover::demo_real(&ev(&[1, 3], 4));
        let mut bt = boot();
        for _ in 0..3 {
            real.skip();
            bt.skip();
        }
        // Run BBS for 7 s: long enough to type out every chip line, and
        // still inside its 8 s slot.
        for _ in 0..140 {
            real.tick(STEP);
            bt.tick(STEP);
        }
        let r = whole_screen(&real, 134, 40);
        let b = whole_screen(&bt, 134, 40);
        assert!(r.contains("CHIP 1") && r.contains("CHIP 3"), "{r}");
        assert!(!r.contains("CHIP 0") && !r.contains("CHIP 2"), "{r}");
        for c in 0..4 {
            assert!(b.contains(&format!("CHIP {c}")), "boot missing chip {c}");
        }
    }

    #[test]
    fn missile_command_slot_highlights_only_the_targeted_lanes() {
        // Chips 1 and 3 of 4 reset. Slot 6 is Missile Command.
        let mut t = Takeover::demo_real(&ev(&[1, 3], 4));
        for _ in 0..6 {
            t.skip();
        }
        assert_eq!(t.variant_name(), "MissileCommand");
        match &t {
            Takeover::Demo(seq) => assert_eq!(seq.event().device_indices, vec![1, 3]),
            _ => panic!("expected a demo"),
        }
        // Past the 5 s resolve point and inside the 8 s slot, the variant
        // shows its settled frame: a `+` ember on each targeted lane and a
        // `·` idle silo on each other lane, all on the impact row.
        for _ in 0..120 {
            t.tick(STEP);
        }
        assert_eq!(t.variant_name(), "MissileCommand");
        let (w, h) = (134u16, 40u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.render(f, f.area())).unwrap();
        let buf = term.backend().buffer();
        let b = takeover_box(ratatui::layout::Rect::new(0, 0, w, h));
        let mut markers: Vec<(u16, u16, String)> = vec![];
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                let sym = buf[(x, y)].symbol();
                if sym == "+" || sym == "·" {
                    markers.push((x, y, sym.to_string()));
                }
            }
        }
        // One marker per lane, left to right, all on one row.
        assert_eq!(markers.len(), 4, "{markers:?}");
        assert!(markers.iter().all(|m| m.1 == markers[0].1), "{markers:?}");
        markers.sort_by_key(|m| m.0);
        let glyphs: Vec<&str> = markers.iter().map(|m| m.2.as_str()).collect();
        assert_eq!(glyphs, ["·", "+", "·", "+"], "lanes 1 and 3 are targeted");
    }

    #[test]
    fn boot_event_is_a_full_reset_over_the_real_device_count() {
        let t = Takeover::demo_boot(3);
        if let Takeover::Demo(seq) = &t {
            let e = seq.event();
            assert_eq!(e.pid, 0);
            assert!(e.is_full);
            assert_eq!((e.chip_count, e.total_devices), (3, 3));
            assert_eq!(e.device_indices, vec![0, 1, 2]);
            assert!(e.raw_targets.is_empty());
        } else {
            panic!("demo_boot must build a Demo");
        }
    }
}
