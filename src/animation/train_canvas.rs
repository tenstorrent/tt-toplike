// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure geometry behind the Training view's starfield and signal weave.
//!
//! The starfield draws one star per training step on a braille canvas. A
//! terminal cell holds 2 dots across and 4 dots down, so two steps share a
//! column and a cell row spans four dot rows. The weave draws one-row strips
//! that line up with the starfield column for column.
//!
//! Everything here works on plain numbers and `StepSample`s. Nothing touches a
//! terminal, so each rule can be tested on its own. The renderer only turns
//! the returned cells into glyphs and colours.
//!
//! ## Alignment
//!
//! Both [`place_stars`] and [`weave_columns`] right-align their input through
//! [`dot_column`]. Sample `i` of `n` (oldest first) lands in dot column
//! `cols * 2 - n + i`, so the newest sample always sits in the last dot
//! column. When `n > cols * 2` the oldest samples are dropped. They are never
//! wrapped round to the other side.

use crate::animation::train_tapestry::{cause_of, StepCause};
use crate::workload::train::StepSample;

/// Dots across one terminal cell.
pub const DOTS_X: usize = 2;
/// Dots down one terminal cell.
pub const DOTS_Y: usize = 4;

/// The value range the starfield's y axis covers, as `(lo, hi)`.
///
/// Only finite, positive values count; others are ignored, and `None` comes
/// back when none are left. Otherwise `lo` sits 10% of the span below the
/// smallest value (never below zero) and `hi` sits 5% of the span above the
/// largest, so the extremes do not land on the edge of the canvas. When every
/// value is equal the span is zero, and the range is `value * 0.9` to
/// `value * 1.1`.
pub fn y_range(values: &[f32]) -> Option<(f32, f32)> {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &v in values {
        if v.is_finite() && v > 0.0 {
            min = min.min(v);
            max = max.max(v);
        }
    }
    if min > max {
        return None;
    }
    let span = max - min;
    if span == 0.0 {
        return Some((min * 0.9, min * 1.1));
    }
    Some(((min - 0.10 * span).max(0.0), max + 0.05 * span))
}

/// True when `lo..hi` can be mapped onto a canvas: both finite and `hi > lo`.
fn usable_range(lo: f32, hi: f32) -> bool {
    lo.is_finite() && hi.is_finite() && hi > lo
}

/// The dot row for `value` on a canvas `rows` cells tall, in `0..rows * 4`.
///
/// Row 0 is the top, so larger values sit nearer the top. `lo` maps to the
/// bottom dot row and `hi` to the top one. Values outside the range clamp to
/// the nearest edge. A NaN value, a non-finite bound, or a range with
/// `hi <= lo` clamps to the bottom. With `rows == 0` the result is 0.
pub fn dot_y(value: f32, lo: f32, hi: f32, rows: usize) -> usize {
    let dots = rows * DOTS_Y;
    if dots == 0 {
        return 0;
    }
    let bottom = dots - 1;
    let frac = (value - lo) / (hi - lo);
    if !usable_range(lo, hi) || frac.is_nan() || frac <= 0.0 {
        return bottom;
    }
    let up = (frac.min(1.0) * bottom as f32).round() as usize;
    bottom - up.min(bottom)
}

/// The braille bit for the dot at column `dx` (0 or 1) and row `dy` (0 to 3,
/// top to bottom) inside one cell. Out-of-range positions give 0.
pub const fn braille_bit(dx: usize, dy: usize) -> u8 {
    match (dx, dy) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x40,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (1, 3) => 0x80,
        _ => 0,
    }
}

/// The braille glyph (U+2800 block) for a set of dot bits. 0 is the blank
/// braille cell (U+2800), which is a different character from a space.
pub fn braille_char(bits: u8) -> char {
    // 0x2800 + any u8 stays inside the braille block, so this cannot fail.
    char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
}

/// What kind of star a step draws as. Same rule as [`cause_of`]: a compile
/// outranks a checkpoint, which outranks a normal step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StarKind {
    /// An ordinary step: braille dots in the normal colour.
    #[default]
    Normal,
    /// The program cache grew on this step: a compile marker.
    Compile,
    /// A checkpoint was written during this step: a checkpoint marker.
    Checkpoint,
}

impl StarKind {
    /// The kind for one step sample, through [`cause_of`].
    pub fn of(sample: &StepSample) -> Self {
        match cause_of(sample) {
            StepCause::Normal => Self::Normal,
            StepCause::Compile => Self::Compile,
            StepCause::Checkpoint => Self::Checkpoint,
        }
    }

    /// Higher wins when several stars share a cell.
    fn priority(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Checkpoint => 1,
            Self::Compile => 2,
        }
    }
}

/// One terminal cell of the starfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StarCell {
    /// Braille dot bits of every star in the cell, whatever its kind. A cell
    /// with no star has 0, so `bits == 0` means empty. A renderer that draws
    /// a marker for a compile or checkpoint cell ignores the bits.
    pub bits: u8,
    /// The highest-priority kind among the stars in the cell: compile, then
    /// checkpoint, then normal. An empty cell has the default, `Normal`.
    pub kind: StarKind,
    /// How many of the stars in this cell are among the newest `newest_n`
    /// samples passed to [`place_stars`]. The renderer brightens or swells
    /// the cell by this count.
    pub newest: usize,
}

impl StarCell {
    /// True when no star landed in this cell.
    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }
}

/// The dot column for sample `i` of `n` samples (oldest first) on a canvas
/// `cols` cells wide, or `None` when the sample is older than the canvas can
/// hold. Right-aligned: sample `n - 1` always lands in the last dot column.
pub fn dot_column(i: usize, n: usize, cols: usize) -> Option<usize> {
    let width = cols * DOTS_X;
    // Samples before `n - width` are the dropped, oldest ones.
    if i >= n || i < n.saturating_sub(width) {
        return None;
    }
    // The check above guarantees `i + width >= n`, so this cannot underflow
    // even when `n > width`.
    Some(i + width - n)
}

/// Place one star per sample on a `rows` x `cols` grid of cells, indexed
/// `grid[row][col]`, row 0 at the top.
///
/// `lo` and `hi` are the y range (see [`y_range`]). Samples are right-aligned
/// through [`dot_column`]; with more than `cols * 2` samples the oldest are
/// dropped. Each star sets one braille dot. A cell holding several stars
/// keeps all their dots, takes the highest-priority [`StarKind`], and counts
/// how many of its stars are among the newest `newest_n` samples.
pub fn place_stars(
    samples: &[StepSample],
    cols: usize,
    rows: usize,
    lo: f32,
    hi: f32,
    newest_n: usize,
) -> Vec<Vec<StarCell>> {
    let mut grid = vec![vec![StarCell::default(); cols]; rows];
    let n = samples.len();
    let first_newest = n.saturating_sub(newest_n);
    for (i, s) in samples.iter().enumerate() {
        let Some(dx) = dot_column(i, n, cols) else {
            continue;
        };
        if rows == 0 {
            continue;
        }
        let dy = dot_y(s.ms, lo, hi, rows);
        let cell = &mut grid[dy / DOTS_Y][dx / DOTS_X];
        cell.bits |= braille_bit(dx % DOTS_X, dy % DOTS_Y);
        let kind = StarKind::of(s);
        if kind.priority() > cell.kind.priority() {
            cell.kind = kind;
        }
        if i >= first_newest {
            cell.newest += 1;
        }
    }
    grid
}

/// The cell row (0 at the top) the median horizon falls in, or `None` when
/// the median is non-finite, outside `lo..=hi`, or the range is unusable.
pub fn median_cell_row(median: f32, lo: f32, hi: f32, rows: usize) -> Option<usize> {
    if rows == 0 || !median.is_finite() || !usable_range(lo, hi) || median < lo || median > hi {
        return None;
    }
    Some(dot_y(median, lo, hi, rows) / DOTS_Y)
}

/// Resample a signal to `cols` columns so it lines up with [`place_stars`].
///
/// Sample `i` of `n` goes to the same dot column as star `i`, and two dot
/// columns share one output column. A column takes the larger of its present
/// samples and is `None` when neither is present. A `None` or non-finite
/// sample counts as absent. Samples older than `cols * 2` are dropped.
pub fn weave_columns(values: &[Option<f32>], cols: usize) -> Vec<Option<f32>> {
    let mut out: Vec<Option<f32>> = vec![None; cols];
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let (Some(dx), Some(v)) = (dot_column(i, n, cols), *v) else {
            continue;
        };
        if !v.is_finite() {
            continue;
        }
        let slot = &mut out[dx / DOTS_X];
        *slot = Some(slot.map_or(v, |old| old.max(v)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn star(seq: u64, ms: f32) -> StepSample {
        StepSample {
            step: seq,
            seq,
            ms,
            cache_delta: 0,
            checkpoint: false,
        }
    }

    // ---- y_range ----

    #[test]
    fn y_range_pads_below_by_ten_percent_and_above_by_five() {
        // min 100, max 200, span 100: lo = 100 - 10 = 90, hi = 200 + 5 = 205.
        let (lo, hi) = y_range(&[100.0, 150.0, 200.0]).unwrap();
        assert!((lo - 90.0).abs() < 1e-4, "lo {lo}");
        assert!((hi - 205.0).abs() < 1e-4, "hi {hi}");
    }

    #[test]
    fn y_range_never_goes_below_zero() {
        // min 1, max 101, span 100: 1 - 10 would be negative, so lo is 0.
        let (lo, _) = y_range(&[1.0, 101.0]).unwrap();
        assert_eq!(lo, 0.0);
    }

    #[test]
    fn y_range_of_equal_values_is_ten_percent_either_side() {
        let (lo, hi) = y_range(&[300.0, 300.0]).unwrap();
        assert!((lo - 270.0).abs() < 1e-3 && (hi - 330.0).abs() < 1e-3);
        let (lo, hi) = y_range(&[50.0]).unwrap();
        assert!((lo - 45.0).abs() < 1e-4 && (hi - 55.0).abs() < 1e-4);
    }

    #[test]
    fn y_range_ignores_non_finite_and_non_positive_values() {
        assert_eq!(y_range(&[]), None);
        assert_eq!(y_range(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]), None);
        assert_eq!(y_range(&[0.0, -5.0]), None);
        // Only the 100 and 200 count.
        let (lo, hi) = y_range(&[f32::NAN, 100.0, f32::INFINITY, -3.0, 200.0, 0.0]).unwrap();
        assert!((lo - 90.0).abs() < 1e-4 && (hi - 205.0).abs() < 1e-4);
    }

    // ---- dot_y ----

    #[test]
    fn dot_y_puts_the_highest_value_at_the_top_row() {
        // 2 cell rows = 8 dot rows. hi is dot row 0, lo is dot row 7.
        assert_eq!(dot_y(200.0, 100.0, 200.0, 2), 0);
        assert_eq!(dot_y(100.0, 100.0, 200.0, 2), 7);
        // Midpoint: frac 0.5 * 7 = 3.5 rounds to 4, so row 7 - 4 = 3.
        assert_eq!(dot_y(150.0, 100.0, 200.0, 2), 3);
    }

    #[test]
    fn dot_y_is_monotone_higher_value_means_smaller_row() {
        let mut prev = usize::MAX;
        for v in (100..=200).step_by(5) {
            let y = dot_y(v as f32, 100.0, 200.0, 3);
            assert!(y <= prev, "value {v} went down the canvas");
            prev = y;
        }
    }

    #[test]
    fn dot_y_clamps_and_survives_bad_input() {
        assert_eq!(dot_y(500.0, 100.0, 200.0, 2), 0);
        assert_eq!(dot_y(f32::INFINITY, 100.0, 200.0, 2), 0);
        assert_eq!(dot_y(10.0, 100.0, 200.0, 2), 7);
        assert_eq!(dot_y(f32::NEG_INFINITY, 100.0, 200.0, 2), 7);
        assert_eq!(dot_y(f32::NAN, 100.0, 200.0, 2), 7);
        assert_eq!(dot_y(150.0, 200.0, 100.0, 2), 7, "inverted range");
        assert_eq!(dot_y(150.0, 150.0, 150.0, 2), 7, "empty range");
        assert_eq!(dot_y(150.0, f32::NAN, 200.0, 2), 7);
        assert_eq!(dot_y(150.0, 100.0, 200.0, 0), 0, "no rows");
        assert_eq!(dot_y(150.0, 100.0, 200.0, 1), 1);
    }

    // ---- braille ----

    #[test]
    fn braille_bits_follow_the_standard_layout() {
        assert_eq!(braille_bit(0, 0), 0x01);
        assert_eq!(braille_bit(0, 1), 0x02);
        assert_eq!(braille_bit(0, 2), 0x04);
        assert_eq!(braille_bit(0, 3), 0x40);
        assert_eq!(braille_bit(1, 0), 0x08);
        assert_eq!(braille_bit(1, 1), 0x10);
        assert_eq!(braille_bit(1, 2), 0x20);
        assert_eq!(braille_bit(1, 3), 0x80);
        assert_eq!(braille_bit(2, 0), 0);
        assert_eq!(braille_bit(0, 4), 0);
        // All eight bits are distinct and together fill a byte.
        let all = (0..2)
            .flat_map(|x| (0..4).map(move |y| braille_bit(x, y)))
            .fold(0u8, |a, b| a | b);
        assert_eq!(all, 0xFF);
    }

    #[test]
    fn braille_char_maps_into_the_braille_block() {
        assert_eq!(braille_char(0), '\u{2800}');
        assert_eq!(braille_char(0x01), '⠁');
        assert_eq!(braille_char(0x40), '⡀');
        assert_eq!(braille_char(0xFF), '⣿');
        assert_eq!(braille_char(braille_bit(1, 3) | braille_bit(0, 0)), '⢁');
    }

    // ---- StarKind ----

    #[test]
    fn star_kind_follows_cause_of() {
        let mut s = star(1, 100.0);
        assert_eq!(StarKind::of(&s), StarKind::Normal);
        s.checkpoint = true;
        assert_eq!(StarKind::of(&s), StarKind::Checkpoint);
        s.cache_delta = 3;
        assert_eq!(
            StarKind::of(&s),
            StarKind::Compile,
            "compile outranks checkpoint"
        );
        s.checkpoint = false;
        assert_eq!(StarKind::of(&s), StarKind::Compile);
    }

    // ---- dot_column ----

    #[test]
    fn dot_column_right_aligns_and_drops_the_oldest() {
        // 3 cols = 6 dot columns.
        assert_eq!(dot_column(0, 1, 3), Some(5));
        assert_eq!(dot_column(0, 2, 3), Some(4));
        assert_eq!(dot_column(1, 2, 3), Some(5));
        assert_eq!(
            dot_column(0, 6, 3),
            Some(0),
            "n == cols * 2 fills the canvas"
        );
        assert_eq!(dot_column(5, 6, 3), Some(5));
        // n = 8: samples 0 and 1 are dropped, sample 2 is the first dot column.
        assert_eq!(dot_column(0, 8, 3), None);
        assert_eq!(dot_column(1, 8, 3), None);
        assert_eq!(dot_column(2, 8, 3), Some(0));
        assert_eq!(dot_column(7, 8, 3), Some(5));
        assert_eq!(dot_column(3, 3, 3), None, "i out of range");
        assert_eq!(dot_column(0, 1, 0), None, "no columns");
    }

    // ---- place_stars ----

    #[test]
    fn no_samples_or_no_canvas_gives_an_empty_grid() {
        let g = place_stars(&[], 4, 2, 0.0, 10.0, 3);
        assert_eq!(g.len(), 2);
        assert!(g
            .iter()
            .all(|r| r.len() == 4 && r.iter().all(StarCell::is_empty)));
        assert!(place_stars(&[star(1, 5.0)], 4, 0, 0.0, 10.0, 1).is_empty());
        let g = place_stars(&[star(1, 5.0)], 0, 2, 0.0, 10.0, 1);
        assert!(g.iter().all(|r| r.is_empty()));
    }

    #[test]
    fn one_sample_lands_in_the_last_dot_column() {
        // 3 cols x 1 row, range 0..=9 so dot_y(9) = 0 and dot_y(0) = 3.
        let g = place_stars(&[star(1, 9.0)], 3, 1, 0.0, 9.0, 1);
        // Last dot column is 5: cell 2, sub-column 1; top dot row 0.
        assert_eq!(g[0][2].bits, braille_bit(1, 0));
        assert_eq!(g[0][2].newest, 1);
        assert!(g[0][0].is_empty() && g[0][1].is_empty());
    }

    #[test]
    fn the_newest_star_is_in_the_last_dot_column_for_odd_and_even_counts() {
        for n in 1..=9usize {
            let samples: Vec<_> = (1..=n as u64).map(|i| star(i, 9.0)).collect();
            let g = place_stars(&samples, 3, 1, 0.0, 9.0, 1);
            // Newest star: dot column 5, so the right-hand dot of cell 2.
            assert_eq!(
                g[0][2].bits & braille_bit(1, 0),
                braille_bit(1, 0),
                "n = {n}"
            );
            let dots: u32 = g[0].iter().map(|c| c.bits.count_ones()).sum();
            assert_eq!(dots as usize, n.min(6), "n = {n}");
        }
    }

    #[test]
    fn an_odd_count_leaves_the_oldest_cell_half_filled() {
        // n = 3 on 2 cols (4 dot columns): dot columns 1, 2, 3.
        let samples: Vec<_> = (1..=3u64).map(|i| star(i, 9.0)).collect();
        let g = place_stars(&samples, 2, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].bits, braille_bit(1, 0));
        assert_eq!(g[0][1].bits, braille_bit(0, 0) | braille_bit(1, 0));
    }

    #[test]
    fn exactly_cols_times_two_samples_fill_every_dot_column() {
        let samples: Vec<_> = (1..=6u64).map(|i| star(i, 9.0)).collect();
        let g = place_stars(&samples, 3, 1, 0.0, 9.0, 0);
        for cell in &g[0] {
            assert_eq!(cell.bits, braille_bit(0, 0) | braille_bit(1, 0));
        }
    }

    #[test]
    fn more_samples_than_dot_columns_drop_the_oldest_and_never_wrap() {
        // 8 samples on 3 cols: samples 0 and 1 are dropped. Make the dropped
        // ones the only low values so a wrap would show at the bottom row.
        let mut samples: Vec<_> = (1..=8u64).map(|i| star(i, 9.0)).collect();
        samples[0].ms = 0.0;
        samples[1].ms = 0.0;
        let g = place_stars(&samples, 3, 1, 0.0, 9.0, 0);
        for cell in &g[0] {
            assert_eq!(cell.bits, braille_bit(0, 0) | braille_bit(1, 0));
        }
        // The first kept sample (index 2) is dot column 0.
        let mut samples: Vec<_> = (1..=8u64).map(|i| star(i, 9.0)).collect();
        samples[2].ms = 0.0;
        let g = place_stars(&samples, 3, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].bits, braille_bit(0, 3) | braille_bit(1, 0));
    }

    #[test]
    fn the_highest_value_is_at_the_top_row_and_the_lowest_at_the_bottom() {
        // 2 cell rows, range 0..=7. The two stars share a cell column.
        let samples = [star(1, 7.0), star(2, 0.0)];
        let g = place_stars(&samples, 1, 2, 0.0, 7.0, 0);
        assert_eq!(g[0][0].bits, braille_bit(0, 0), "high value, top dot");
        assert_eq!(g[1][0].bits, braille_bit(1, 3), "low value, bottom dot");
    }

    #[test]
    fn a_cell_with_several_stars_keeps_all_their_dots() {
        // Both stars in cell (0, 0): dot columns 0 and 1, values at dot rows 0 and 3.
        let samples = [star(1, 9.0), star(2, 0.0)];
        let g = place_stars(&samples, 1, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].bits, braille_bit(0, 0) | braille_bit(1, 3));
        assert_eq!(g[0][0].kind, StarKind::Normal);
    }

    #[test]
    fn compile_beats_checkpoint_beats_normal_in_a_shared_cell() {
        let normal = star(1, 5.0);
        let mut ckpt = star(2, 5.0);
        ckpt.checkpoint = true;
        let mut comp = star(3, 5.0);
        comp.cache_delta = 2;
        // On a 1-column canvas both samples share a single cell.
        let g = place_stars(&[normal, ckpt], 1, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].kind, StarKind::Checkpoint);
        let g = place_stars(&[ckpt, normal], 1, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].kind, StarKind::Checkpoint, "order does not matter");
        let g = place_stars(&[comp, ckpt], 1, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].kind, StarKind::Compile);
        let g = place_stars(&[ckpt, comp], 1, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].kind, StarKind::Compile);
        let g = place_stars(&[normal, comp], 1, 1, 0.0, 9.0, 0);
        assert_eq!(g[0][0].kind, StarKind::Compile);
        // A marker star still sets its dot.
        assert_ne!(g[0][0].bits, 0);
    }

    #[test]
    fn newest_counts_the_newest_stars_in_each_cell() {
        // 4 samples on 2 cols (cells hold two stars each). newest_n = 3 marks
        // samples 1, 2, 3: cell 0 holds samples 0 and 1, cell 1 holds 2 and 3.
        let samples: Vec<_> = (1..=4u64).map(|i| star(i, 9.0)).collect();
        let g = place_stars(&samples, 2, 1, 0.0, 9.0, 3);
        assert_eq!(g[0][0].newest, 1);
        assert_eq!(g[0][1].newest, 2);
        // newest_n larger than n marks everything; 0 marks nothing.
        let g = place_stars(&samples, 2, 1, 0.0, 9.0, 99);
        assert_eq!((g[0][0].newest, g[0][1].newest), (2, 2));
        let g = place_stars(&samples, 2, 1, 0.0, 9.0, 0);
        assert_eq!((g[0][0].newest, g[0][1].newest), (0, 0));
    }

    #[test]
    fn dropped_samples_do_not_count_as_newest() {
        // 6 samples on 1 col: samples 0..4 dropped; newest_n 6 only counts the 2 kept.
        let samples: Vec<_> = (1..=6u64).map(|i| star(i, 9.0)).collect();
        let g = place_stars(&samples, 1, 1, 0.0, 9.0, 6);
        assert_eq!(g[0][0].newest, 2);
    }

    #[test]
    fn non_finite_step_times_sit_at_the_bottom_and_do_not_panic() {
        let samples = [star(1, f32::NAN), star(2, f32::INFINITY)];
        let g = place_stars(&samples, 1, 1, 0.0, 9.0, 0);
        // NaN: dot column 0, bottom row 3. Infinity: dot column 1, top row 0.
        assert_eq!(g[0][0].bits, braille_bit(0, 3) | braille_bit(1, 0));
    }

    // ---- median_cell_row ----

    #[test]
    fn the_median_row_is_the_cell_row_of_its_dot_row() {
        // 2 cell rows over 0..=7: value 7 -> dot 0 (cell 0), value 0 -> dot 7 (cell 1).
        assert_eq!(median_cell_row(7.0, 0.0, 7.0, 2), Some(0));
        assert_eq!(median_cell_row(0.0, 0.0, 7.0, 2), Some(1));
        // 4.0: frac 4/7 * 7 = 4, dot 3, cell 0. 3.0: dot 4, cell 1.
        assert_eq!(median_cell_row(4.0, 0.0, 7.0, 2), Some(0));
        assert_eq!(median_cell_row(3.0, 0.0, 7.0, 2), Some(1));
        // Always agrees with dot_y.
        for v in 0..=70 {
            let v = v as f32 / 10.0;
            assert_eq!(
                median_cell_row(v, 0.0, 7.0, 3),
                Some(dot_y(v, 0.0, 7.0, 3) / 4)
            );
        }
    }

    #[test]
    fn the_median_row_is_none_outside_the_range_or_for_bad_input() {
        assert_eq!(median_cell_row(8.0, 0.0, 7.0, 2), None);
        assert_eq!(median_cell_row(-1.0, 0.0, 7.0, 2), None);
        assert_eq!(median_cell_row(f32::NAN, 0.0, 7.0, 2), None);
        assert_eq!(median_cell_row(f32::INFINITY, 0.0, 7.0, 2), None);
        assert_eq!(median_cell_row(3.0, 7.0, 0.0, 2), None);
        assert_eq!(median_cell_row(3.0, 3.0, 3.0, 2), None);
        assert_eq!(median_cell_row(3.0, 0.0, 7.0, 0), None);
    }

    // ---- weave_columns ----

    #[test]
    fn a_weave_column_takes_the_larger_of_its_two_samples() {
        let v = [Some(1.0), Some(5.0), Some(7.0), Some(2.0)];
        assert_eq!(weave_columns(&v, 2), vec![Some(5.0), Some(7.0)]);
    }

    #[test]
    fn a_weave_column_with_one_present_sample_takes_it() {
        let v = [Some(1.0), None, None, Some(2.0)];
        assert_eq!(weave_columns(&v, 2), vec![Some(1.0), Some(2.0)]);
        let v = [None, None, Some(3.0), None];
        assert_eq!(weave_columns(&v, 2), vec![None, Some(3.0)]);
    }

    #[test]
    fn weave_columns_right_align_an_odd_count() {
        // n = 3 on 2 cols: dot columns 1, 2, 3, so column 0 holds only sample 0.
        let v = [Some(4.0), Some(1.0), Some(2.0)];
        assert_eq!(weave_columns(&v, 2), vec![Some(4.0), Some(2.0)]);
        // n = 1: only the last column.
        assert_eq!(weave_columns(&[Some(9.0)], 3), vec![None, None, Some(9.0)]);
    }

    #[test]
    fn weave_columns_drop_the_oldest_beyond_the_canvas() {
        // n = 5 on 2 cols (4 dot columns): sample 0 is dropped.
        let v = [Some(99.0), Some(1.0), Some(2.0), Some(3.0), Some(4.0)];
        assert_eq!(weave_columns(&v, 2), vec![Some(2.0), Some(4.0)]);
        // n == cols * 2 keeps everything.
        let v = [Some(1.0), Some(2.0), Some(3.0), Some(4.0)];
        assert_eq!(weave_columns(&v, 2), vec![Some(2.0), Some(4.0)]);
    }

    #[test]
    fn weave_columns_handle_empty_and_non_finite_input() {
        assert_eq!(weave_columns(&[], 3), vec![None, None, None]);
        assert!(weave_columns(&[Some(1.0)], 0).is_empty());
        let v = [Some(f32::NAN), Some(2.0), Some(f32::INFINITY), None];
        assert_eq!(weave_columns(&v, 2), vec![Some(2.0), None]);
    }

    /// The starfield and the weave must put sample `i` of `n` in the same
    /// terminal column, or the strips would not line up under the stars.
    #[test]
    fn weave_and_starfield_use_the_same_column_for_the_same_sample() {
        for cols in 1..=5usize {
            for n in 0..=(cols * 2 + 3) {
                for i in 0..n {
                    // A star at sample i alone (others far below, at the bottom
                    // row), and a weave value at sample i alone.
                    let mut samples: Vec<_> = (0..n as u64).map(|k| star(k, 1.0)).collect();
                    samples[i].ms = 9.0;
                    let g = place_stars(&samples, cols, 1, 1.0, 9.0, 0);
                    // Top dot row holds only the high star.
                    let star_col = g[0]
                        .iter()
                        .position(|c| c.bits & (braille_bit(0, 0) | braille_bit(1, 0)) != 0);
                    let mut vals = vec![None; n];
                    vals[i] = Some(1.0);
                    let weave_col = weave_columns(&vals, cols).iter().position(Option::is_some);
                    assert_eq!(star_col, weave_col, "cols {cols} n {n} i {i}");
                }
            }
        }
    }
}
