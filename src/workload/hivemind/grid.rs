// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Decaying (source × column) activity heat. `bump` on each event, `decay`
//! once per frame so idle cells fade to cold.

use super::event::Source;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub const DECAY: f32 = 0.9;

/// Width of a cell's recent-activity sparkline, in 1-second buckets. Kept
/// small (a few seconds of history) since board cells are narrow — see
/// `hivemind_view::render_hivemind`'s degrade-gracefully handling on tight
/// terminals.
pub const SPARK_W: usize = 4;

/// Cadence at which a cell's spark buckets roll, mirrored from
/// `feed::BUCKETS`'s cadence so the board and the feed pulse in lockstep.
const SPARK_BUCKET_DUR: Duration = Duration::from_secs(1);

/// How recently a cell must have been bumped to still read as "pulsing" in
/// the board (see `is_pulsing`) — a UI notion of "something just happened
/// here", not a data-retention window.
const PULSE_WINDOW: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    Device(u8),
    Host,
}

/// Per-cell recent-activity tracking: a short ring of per-second bump counts
/// (for the sparkline) plus the timestamp of the most recent bump (for the
/// pulse-on-new-event effect). Separate from `heat` (which is a decaying
/// scalar, not a time series) so the two can evolve independently — heat
/// answers "how hot is this cell lately", spark/pulse answer "is there
/// activity happening *right now*".
#[derive(Default)]
struct CellActivity {
    /// Per-second bump counts, oldest first, rolled by `HeatGrid::tick`.
    buckets: VecDeque<u32>,
    /// Bumps since the last roll — becomes the newest bucket once rolled.
    current_bucket: u32,
    last_active: Option<Instant>,
}

pub struct HeatGrid {
    heat: HashMap<(Source, Column), f32>,
    order: Vec<Source>, // sources in first-seen order
    max_device: Option<u8>,
    /// Per-cell sparkline/pulse state, fed by `bump`/`bump_at` alongside
    /// `heat`, rolled once per second by `tick`.
    activity: HashMap<(Source, Column), CellActivity>,
    last_roll: Option<Instant>,
}

impl HeatGrid {
    pub fn new() -> Self {
        Self {
            heat: HashMap::new(),
            order: Vec::new(),
            max_device: None,
            activity: HashMap::new(),
            last_roll: None,
        }
    }

    /// Bump a cell "now" (`Instant::now()`). Kept as the stable, no-clock
    /// entry point for existing callers/tests (`Hivemind::bump_grid_for_test`
    /// and this module's own pre-existing tests) that don't care about
    /// controlling the timestamp; anything that wants deterministic spark/
    /// pulse behavior (real `poll()`, and this module's spark/pulse tests)
    /// should use `bump_at` instead.
    pub fn bump(&mut self, source: Source, device: Option<u8>) {
        self.bump_at(source, device, Instant::now());
    }

    /// Same as `bump`, but with an explicit timestamp — used by `poll()` (one
    /// `Instant::now()` captured per poll, shared across every drained event
    /// and the grid/feed `tick()` calls) and by tests that need reproducible
    /// spark/pulse timing.
    pub fn bump_at(&mut self, source: Source, device: Option<u8>, now: Instant) {
        let col = match device {
            Some(d) => {
                self.max_device = Some(self.max_device.map_or(d, |m| m.max(d)));
                Column::Device(d)
            }
            None => Column::Host,
        };
        if !self.order.contains(&source) {
            self.order.push(source);
        }
        let e = self.heat.entry((source, col)).or_insert(0.0);
        *e = (*e + 1.0).min(8.0);

        let act = self.activity.entry((source, col)).or_default();
        act.current_bucket = act.current_bucket.saturating_add(1);
        act.last_active = Some(now);
    }

    pub fn decay(&mut self) {
        for v in self.heat.values_mut() {
            *v *= DECAY;
        }
    }

    /// Roll every cell's spark bucket on a ~1s cadence — a no-op if less than
    /// `SPARK_BUCKET_DUR` has elapsed since the last roll. Call once per
    /// `poll()`, alongside `decay()` (which runs on every poll regardless;
    /// this only actually rolls roughly once a second). Mirrors
    /// `feed::FeedAgg::tick`'s bucket-rolling logic.
    pub fn tick(&mut self, now: Instant) {
        let elapsed_secs = match self.last_roll {
            None => 1,
            Some(lr) => {
                let secs = now.saturating_duration_since(lr).as_secs_f32()
                    / SPARK_BUCKET_DUR.as_secs_f32();
                if secs < 1.0 {
                    return;
                }
                (secs.floor() as u64).min(SPARK_W as u64 + 1)
            }
        };

        for act in self.activity.values_mut() {
            for i in 0..elapsed_secs {
                let count = if i == 0 { act.current_bucket } else { 0 };
                act.buckets.push_back(count);
                if act.buckets.len() > SPARK_W {
                    act.buckets.pop_front();
                }
            }
            act.current_bucket = 0;
        }
        self.last_roll = Some(now);
    }

    pub fn heat(&self, source: Source, col: Column) -> f32 {
        *self.heat.get(&(source, col)).unwrap_or(&0.0)
    }

    /// Recent per-second bump counts for one cell, oldest first, zero-padded
    /// at the front until `SPARK_W` seconds of history has accumulated.
    /// Always `SPARK_W` wide regardless of how much history exists, so
    /// callers can render a fixed-width sparkline unconditionally.
    pub fn spark(&self, source: Source, col: Column) -> [u8; SPARK_W] {
        let mut out = [0u8; SPARK_W];
        if let Some(act) = self.activity.get(&(source, col)) {
            let offset = SPARK_W.saturating_sub(act.buckets.len());
            for (i, &v) in act.buckets.iter().enumerate() {
                if offset + i < SPARK_W {
                    out[offset + i] = v.min(u8::MAX as u32) as u8;
                }
            }
        }
        out
    }

    /// Whether this cell was bumped within the last `PULSE_WINDOW` — drives
    /// the board's brighter/bold "just happened" rendering.
    pub fn is_pulsing(&self, source: Source, col: Column, now: Instant) -> bool {
        self.activity
            .get(&(source, col))
            .and_then(|a| a.last_active)
            .is_some_and(|t| now.saturating_duration_since(t) <= PULSE_WINDOW)
    }

    pub fn active_sources(&self) -> Vec<Source> {
        self.order.clone()
    }

    pub fn max_device(&self) -> Option<u8> {
        self.max_device
    }
}

impl Default for HeatGrid {
    fn default() -> Self {
        Self::new()
    }
}

/// Map decayed heat (0..~8) to a block glyph. Normalizes to 0..1 for the ramp.
pub fn heat_char(heat: f32) -> char {
    crate::animation::common::value_to_block_char((heat / 8.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_tracks_column_and_first_seen_order() {
        let mut g = HeatGrid::new();
        g.bump(Source::TtMetal, Some(0));
        g.bump(Source::Ttnn, None);
        g.bump(Source::TtMetal, Some(3));
        assert_eq!(g.active_sources(), vec![Source::TtMetal, Source::Ttnn]);
        assert_eq!(g.max_device(), Some(3));
        assert!(g.heat(Source::TtMetal, Column::Device(0)) > 0.0);
        assert!(g.heat(Source::Ttnn, Column::Host) > 0.0);
    }

    #[test]
    fn decay_cools_idle_cells() {
        let mut g = HeatGrid::new();
        g.bump(Source::TtMetal, Some(0));
        let before = g.heat(Source::TtMetal, Column::Device(0));
        g.decay();
        let after = g.heat(Source::TtMetal, Column::Device(0));
        assert!(after < before);
        assert!((after - before * DECAY).abs() < 1e-6);
    }

    /// Spark bucket rolling: two `bump_at` calls a second apart should land
    /// in two distinct buckets once `tick` has rolled past each of them, and
    /// `spark` should always report exactly `SPARK_W` values regardless of
    /// how little history exists yet.
    #[test]
    fn spark_rolls_buckets_and_reports_expected_width() {
        let mut g = HeatGrid::new();
        let t0 = Instant::now();

        g.bump_at(Source::TtMetal, Some(0), t0);
        g.tick(t0 + Duration::from_secs(1)); // rolls the first bucket (1 bump)

        g.bump_at(Source::TtMetal, Some(0), t0 + Duration::from_secs(1));
        g.bump_at(Source::TtMetal, Some(0), t0 + Duration::from_secs(1));
        g.tick(t0 + Duration::from_secs(2)); // rolls the second bucket (2 bumps)

        let spark = g.spark(Source::TtMetal, Column::Device(0));
        assert_eq!(spark.len(), SPARK_W);
        assert_eq!(spark[SPARK_W - 1], 2, "most recent bucket had 2 bumps");
        assert_eq!(spark[SPARK_W - 2], 1, "previous bucket had 1 bump");
    }

    /// A cell with no activity at all reports an all-zero spark of the right
    /// width — never panics on a cell that's never been touched.
    #[test]
    fn spark_on_untouched_cell_is_all_zero() {
        let g = HeatGrid::new();
        assert_eq!(g.spark(Source::Host, Column::Host), [0u8; SPARK_W]);
    }

    #[test]
    fn is_pulsing_true_right_after_activity_then_false_after_window() {
        let mut g = HeatGrid::new();
        let t0 = Instant::now();
        g.bump_at(Source::Vllm, None, t0);

        assert!(g.is_pulsing(Source::Vllm, Column::Host, t0 + Duration::from_millis(50)));
        assert!(!g.is_pulsing(Source::Vllm, Column::Host, t0 + Duration::from_millis(500)));
    }

    #[test]
    fn is_pulsing_false_for_a_cell_never_bumped() {
        let g = HeatGrid::new();
        assert!(!g.is_pulsing(Source::Host, Column::Host, Instant::now()));
    }
}
