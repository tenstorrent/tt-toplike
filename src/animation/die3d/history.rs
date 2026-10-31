// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The history panel: the last minute of measured telemetry, as sparklines.
//!
//! The scene shows the present. This panel shows how the readings moved: it
//! plots each Blackhole chip's power and the box's PCIe traffic in each
//! direction over the last 60 seconds, and prints each chip's latest power
//! and temperature next to its sparkline. Temperature is not plotted. Every
//! value is a reading from the backend, never an estimate.
//!
//! [`History`] keeps one sample per second of view time: per chip its power
//! and temperature, and for the box the PCIe rates in and out summed over the
//! chips the view draws. [`spark_scaled`] turns the 60 samples of a series
//! into a row of 24 block characters, each the mean of two or three samples.
//! `Die3dView` samples in `update` and writes [`History::panel_rows`] over
//! the top-left of the scene.
//!
//! Pure logic: no clock, no I/O. The caller passes `now` in seconds.

use super::scene::ChipInputs;
use crate::backend::pcie_counters::format_bandwidth;

/// Samples kept per series: one per second for a minute.
pub const SAMPLES: usize = 60;
/// Seconds of view time between two samples, at least.
pub const SAMPLE_INTERVAL_S: f64 = 1.0;
/// Chips the panel lists at most. Eight chips would make the panel eleven
/// rows tall and cover too much of the scene.
pub const MAX_CHIPS: usize = 6;
/// Cells of each sparkline. The [`SAMPLES`] samples of a series are
/// bucketed into them (see [`spark_scaled`]), so the line covers the whole
/// minute.
pub const SPARK_WIDTH: usize = 24;
/// The panel's title row. The sparklines cover all 60 samples.
pub const TITLE: &str = "history 60 s";
/// The smallest scale a power sparkline is drawn against, in watts. A chip
/// whose draw wobbles between 13 and 16 W was drawn as full-height bars
/// when each window was scaled to its own range; against 5 W a wobble of a
/// watt or two stays in the lowest blocks.
pub const POWER_MIN_SPAN_W: f32 = 5.0;
/// The smallest scale a PCIe sparkline is drawn against: 10 MB/s, in bytes
/// per second. Below it a link's background chatter would fill the row.
pub const PCIE_MIN_SPAN_BPS: f32 = 10.0e6;

/// The eight block characters, lowest first.
const BLOCKS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];
/// The block a flat window (or a single point) is drawn with: the middle one,
/// so a steady reading neither looks like zero nor like a peak.
const FLAT: char = '\u{2584}';
/// Width of the label column, `PCIe ▼` being the widest entry. Each row's
/// sparkline starts one cell after it, so the sparklines line up in time.
const LABEL_W: usize = 6;
/// Cells of every panel row: the label column, the sparkline and the values
/// (` 44W  56°` for a chip, at most 9 for a rate from `format_bandwidth`),
/// each after a space. Every row is padded to it, so the panel is one even
/// block over the scene.
pub const PANEL_WIDTH: usize = LABEL_W + 1 + SPARK_WIDTH + 1 + 9;

/// The last [`SAMPLES`] values of one series, oldest first.
fn push<T>(ring: &mut Vec<Option<T>>, v: Option<T>) {
    if ring.len() == SAMPLES {
        ring.remove(0);
    }
    ring.push(v);
}

/// One chip's series.
#[derive(Debug, Clone)]
struct ChipSeries {
    /// Device index: the key the series is kept under.
    index: usize,
    /// The chip's label at the latest sample (`BH0`).
    label: String,
    /// Watts, oldest first; `None` where the chip gave no reading.
    power: Vec<Option<f32>>,
    /// Degrees C, oldest first; `None` where the chip gave no reading.
    temp: Vec<Option<f32>>,
}

/// The last minute of measured telemetry for the chips the view draws.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// One entry per chip of the latest sample, in the order the chips were
    /// passed. A chip that leaves loses its series; one that arrives starts
    /// with an empty one, which [`spark`] pads on the left so its newest
    /// sample still lines up with every other row's.
    chips: Vec<ChipSeries>,
    /// Bytes per second into the chips, summed, oldest first.
    pcie_in: Vec<Option<f64>>,
    /// Bytes per second out of the chips, summed, oldest first.
    pcie_out: Vec<Option<f64>>,
    /// View time of the latest sample, `None` before the first.
    last_at: Option<f64>,
    /// Samples taken so far. The view's redraw throttle keys on it, so a new
    /// sample is drawn at once.
    revision: u64,
}

impl History {
    /// An empty history.
    pub fn new() -> History {
        History::default()
    }

    /// Whether [`sample`](Self::sample) at `now` would take a sample: `now` is
    /// finite and at least [`SAMPLE_INTERVAL_S`] after the last sample. A
    /// `now` before the last sample (a new clock) is not: the history waits
    /// rather than storing samples out of order.
    pub fn due(&self, now: f64) -> bool {
        now.is_finite()
            && self
                .last_at
                .is_none_or(|last| now - last >= SAMPLE_INTERVAL_S)
    }

    /// Take one sample of `chips` at view time `now`, unless it is not
    /// [`due`](Self::due). Returns whether a sample was taken.
    ///
    /// The chips' order is kept for the panel. The box's PCIe rate in each
    /// direction is the sum over the chips with a finite reading, `None` when
    /// none has one. Power and temperature that are NaN or infinite are
    /// stored as `None`.
    pub fn sample(&mut self, now: f64, chips: &[ChipInputs]) -> bool {
        if !self.due(now) {
            return false;
        }
        let mut series = Vec::with_capacity(chips.len());
        for chip in chips {
            let mut s = match self.chips.iter().position(|s| s.index == chip.index) {
                Some(i) => self.chips.swap_remove(i),
                None => ChipSeries {
                    index: chip.index,
                    label: String::new(),
                    power: Vec::with_capacity(SAMPLES),
                    temp: Vec::with_capacity(SAMPLES),
                },
            };
            s.label.clone_from(&chip.label);
            push(&mut s.power, chip.power_w.filter(|w| w.is_finite()));
            push(&mut s.temp, chip.temp_c.filter(|c| c.is_finite()));
            series.push(s);
        }
        self.chips = series;
        let sum = |rate: fn(&ChipInputs) -> Option<f64>| {
            chips
                .iter()
                .filter_map(rate)
                .filter(|b| b.is_finite())
                .fold(None, |sum: Option<f64>, b| Some(sum.unwrap_or(0.0) + b))
        };
        push(&mut self.pcie_in, sum(|c| c.pcie_in_bps));
        push(&mut self.pcie_out, sum(|c| c.pcie_out_bps));
        self.last_at = Some(now);
        self.revision += 1;
        true
    }

    /// Samples taken so far; changes exactly when a sample is taken.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Power samples of device `index`, oldest first.
    pub fn power(&self, index: usize) -> Option<&[Option<f32>]> {
        self.series(index).map(|s| s.power.as_slice())
    }

    /// Temperature samples of device `index`, oldest first.
    pub fn temp(&self, index: usize) -> Option<&[Option<f32>]> {
        self.series(index).map(|s| s.temp.as_slice())
    }

    /// The box's PCIe rate into the chips, oldest first.
    pub fn pcie_in(&self) -> &[Option<f64>] {
        &self.pcie_in
    }

    /// The box's PCIe rate out of the chips, oldest first.
    pub fn pcie_out(&self) -> &[Option<f64>] {
        &self.pcie_out
    }

    fn series(&self, index: usize) -> Option<&ChipSeries> {
        self.chips.iter().find(|s| s.index == index)
    }

    /// The panel's text, one string per row: the title, one row per chip (at
    /// most [`MAX_CHIPS`]) and two PCIe rows, in to the chips (`▼`) and out
    /// (`▲`). A chip's row plots its power over the minute and ends with its
    /// latest power and temperature; a PCIe row plots that direction's rate
    /// and ends with the latest one.
    ///
    /// ```text
    /// history 60 s
    /// BH0    ▁▂▃▅▆▇▅▃▂▁▂▃▅▆▇▆▅▃▂▁▂▃▅▆  44W  56°
    /// PCIe ▼ ▁▂▃▅▆▇▅▃▂▁▂▃▅▆▇▆▅▃▂▁▂▃▅▆ 1.23 GB/s
    ///      ▲ ▁▂▃▅▆▇▅▃▂▁▂▃▅▆▇▆▅▃▂▁▂▃▅▆ 310 MB/s
    /// ```
    ///
    /// The values at the right are the latest sample's, `-` when it had no
    /// reading. Every row is exactly [`PANEL_WIDTH`] characters: padded with
    /// spaces, so the panel covers an even block of the scene, and cut, so
    /// an unexpected value can never widen it.
    pub fn panel_rows(&self) -> Vec<String> {
        let mut rows = Vec::with_capacity(MAX_CHIPS + 3);
        rows.push(TITLE.to_string());
        for s in self.chips.iter().take(MAX_CHIPS) {
            let label: String = s.label.chars().take(LABEL_W).collect();
            rows.push(format!(
                "{label:<LABEL_W$} {} {:>4} {:>4}",
                spark_scaled(&s.power, SPARK_WIDTH, POWER_MIN_SPAN_W),
                latest(&s.power).map_or("-".into(), |w| format!("{w:.0}W")),
                latest(&s.temp).map_or("-".into(), |c| format!("{c:.0}\u{b0}")),
            ));
        }
        for (label, ring) in [
            ("PCIe \u{25bc}", &self.pcie_in),
            ("     \u{25b2}", &self.pcie_out),
        ] {
            // A sparkline has eight levels, so f32 is precise enough for
            // any rate a link can carry.
            let values: Vec<Option<f32>> = ring.iter().map(|v| v.map(|b| b as f32)).collect();
            rows.push(format!(
                "{label} {} {}",
                spark_scaled(&values, SPARK_WIDTH, PCIE_MIN_SPAN_BPS),
                latest(ring).map_or("-".into(), format_bandwidth),
            ));
        }
        rows.into_iter()
            .map(|r| {
                let n = r.chars().count();
                if n >= PANEL_WIDTH {
                    r.chars().take(PANEL_WIDTH).collect()
                } else {
                    format!("{r}{}", " ".repeat(PANEL_WIDTH - n))
                }
            })
            .collect()
    }
}

/// The newest sample of `ring`, `None` when it is empty or had no reading.
fn latest<T: Copy>(ring: &[Option<T>]) -> Option<T> {
    ring.last().copied().flatten()
}

/// `values` as a sparkline of `width` characters, scaled to the window's own
/// range: [`spark_scaled`] with no minimum span. A window whose finite cells
/// are all equal, or that has one, draws `▄` everywhere it has a reading.
pub fn spark(values: &[Option<f32>], width: usize) -> String {
    spark_scaled(values, width, 0.0)
}

/// The cells a sparkline of `width` draws for `values`, oldest first.
///
/// Up to `width` values, each value is one cell, and a shorter series is
/// left for the caller to pad on the left. More values than that are split
/// into `width` buckets with integer edges `i * n / width .. (i + 1) * n /
/// width`, so every sample counts once: the oldest bucket is the leftmost
/// cell and the newest bucket, which holds the newest sample, the rightmost.
/// Sixty samples over 24 cells make buckets of two and three samples. A
/// cell is the mean of its bucket's finite values, `None` (a gap) when it
/// has none: NaN, infinite and `None` samples are left out.
fn cells(values: &[Option<f32>], width: usize) -> Vec<Option<f32>> {
    let finite = |v: &Option<f32>| v.filter(|x| x.is_finite());
    let n = values.len();
    if n <= width {
        return values.iter().map(finite).collect();
    }
    (0..width)
        .map(|i| {
            let bucket = &values[i * n / width..(i + 1) * n / width];
            // Summed in f64: a PCIe rate near 1e10 B/s loses nothing that
            // shows in eight levels, but there is no reason to risk it.
            let (sum, count) = bucket
                .iter()
                .filter_map(finite)
                .fold((0.0f64, 0usize), |(s, c), x| (s + x as f64, c + 1));
            (count > 0).then(|| (sum / count as f64) as f32)
        })
        .collect()
}

/// `values` as a sparkline of `width` characters, the scale at least
/// `min_span` tall.
///
/// [`cells`] reduces the values to at most `width` cells (each the mean of
/// its bucket once there are more values than cells), and each cell maps
/// onto the eight blocks `▁▂▃▄▅▆▇█`, scaled from the lowest cell up. The top
/// of the scale is the highest cell, or the lowest plus `min_span` when
/// that is higher. So a reading that swings by more than `min_span` uses
/// the whole height, and one that only wobbles (a chip at 13 W give or take
/// a watt, against the 5 W [`POWER_MIN_SPAN_W`]) stays near the bottom. A
/// constant reading sits on the lowest block.
///
/// With `min_span` zero (or less, or not finite) the scale is the cells' own
/// range, and a window whose cells are all equal, or that has one, draws
/// `▄`: with no scale to place it on, the middle block says "a steady
/// reading" without looking like zero or a peak.
///
/// A gap cell is a space. A series shorter than `width` is padded with
/// spaces on the left, so the newest sample is always the last character.
pub fn spark_scaled(values: &[Option<f32>], width: usize, min_span: f32) -> String {
    let cells = cells(values, width);
    let (lo, hi) = cells
        .iter()
        .flatten()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &x| {
            (lo.min(x), hi.max(x))
        });
    let min_span = if min_span.is_finite() {
        min_span.max(0.0)
    } else {
        0.0
    };
    // NaN or negative infinity when there is no finite cell; then every
    // cell is a gap and the span is never used.
    let span = (hi - lo).max(min_span);
    let mut out = String::with_capacity(width * 3);
    out.extend(std::iter::repeat_n(' ', width - cells.len()));
    for cell in cells {
        out.push(match cell {
            None => ' ',
            // Flat or a single cell with no minimum span. `span` is 0 here;
            // checking `> 0` with a finite span covers it and NaN alike.
            Some(_) if !(span.is_finite() && span > 0.0) => FLAT,
            Some(x) => {
                let step = ((x - lo) / span * (BLOCKS.len() - 1) as f32).round();
                BLOCKS[(step as usize).min(BLOCKS.len() - 1)]
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Architecture;

    /// A Blackhole chip with the given readings and nothing else.
    fn chip(index: usize, power: Option<f32>, temp: Option<f32>) -> ChipInputs {
        ChipInputs {
            index,
            label: format!("BH{index}"),
            power_w: power,
            arch: Architecture::Blackhole,
            load: 0.0,
            pcie_in_bps: Some(1.0e6 * index as f64),
            pcie_out_bps: None,
            gddr_heat: None,
            eth_live: 0,
            eth_known: false,
            temp_c: temp,
            enabled_tensix_col: None,
            tile_flow: None,
        }
    }

    #[test]
    fn the_ring_keeps_the_last_sixty_samples_in_order() {
        let mut h = History::new();
        for t in 0..70 {
            let chips = [
                chip(0, Some(t as f32), Some(40.0 + t as f32)),
                chip(1, None, None),
            ];
            assert!(h.sample(t as f64, &chips), "sample at {t}");
        }
        let want: Vec<Option<f32>> = (10..70).map(|t| Some(t as f32)).collect();
        assert_eq!(h.power(0).unwrap(), &want[..]);
        let want: Vec<Option<f32>> = (10..70).map(|t| Some(40.0 + t as f32)).collect();
        assert_eq!(h.temp(0).unwrap(), &want[..]);
        assert_eq!(h.power(1).unwrap(), &[None; SAMPLES][..]);
        // The box's PCIe in is the sum over the chips (0 + 1 MB/s); out has
        // no reading on any chip.
        assert_eq!(h.pcie_in(), &[Some(1.0e6); SAMPLES][..]);
        assert_eq!(h.pcie_out(), &[None; SAMPLES][..]);
        assert_eq!(h.revision(), 70);
        // A chip that leaves loses its series; the order follows the chips.
        h.sample(70.0, &[chip(1, Some(5.0), None)]);
        assert!(h.power(0).is_none());
        assert_eq!(h.power(1).unwrap().last(), Some(&Some(5.0)));
    }

    #[test]
    fn sampling_ignores_calls_within_a_second_and_time_going_backwards() {
        let mut h = History::new();
        let chips = [chip(0, Some(1.0), None)];
        let mut taken = Vec::new();
        for now in [
            0.0,
            0.5,
            0.999,
            1.0,
            1.7,
            0.5,
            -3.0,
            2.0,
            f64::NAN,
            f64::INFINITY,
            2.9,
            3.05,
        ] {
            if h.sample(now, &chips) {
                taken.push(now);
            }
        }
        assert_eq!(taken, [0.0, 1.0, 2.0, 3.05]);
        assert_eq!(h.power(0).unwrap().len(), 4);
        assert_eq!(h.revision(), 4);
        // A call that takes no sample changes nothing, revision included.
        let before = h.revision();
        assert!(!h.sample(3.5, &chips));
        assert_eq!(h.revision(), before);
    }

    #[test]
    fn spark_maps_min_to_the_lowest_and_max_to_the_highest_block() {
        let values: Vec<Option<f32>> = (0..8).map(|i| Some(10.0 + 5.0 * i as f32)).collect();
        assert_eq!(
            spark(&values, 8),
            "\u{2581}\u{2582}\u{2583}\u{2584}\u{2585}\u{2586}\u{2587}\u{2588}"
        );
        // Negative and unordered values: the extremes still take the end
        // blocks.
        let values = [Some(1000.0), Some(-4.0), Some(9.0), Some(-11.0)];
        assert_eq!(spark(&values, 4), "\u{2588}\u{2581}\u{2581}\u{2581}");
        let values = [Some(-4.0), Some(1000.0), Some(9.0), Some(-11.0)];
        assert_eq!(spark(&values, 4), "\u{2581}\u{2588}\u{2581}\u{2581}");
        // More values than cells: every value counts, bucketed (edges
        // i * 4 / 3: [1000], [-4], [9, -11] whose mean is -1).
        let values = [Some(1000.0), Some(-4.0), Some(9.0), Some(-11.0)];
        assert_eq!(spark(&values, 3), "\u{2588}\u{2581}\u{2581}");
    }

    #[test]
    fn spark_handles_flat_single_none_nan_and_short_windows() {
        // Flat: the middle block everywhere.
        assert_eq!(spark(&[Some(44.0); 5], 5), "\u{2584}".repeat(5));
        // One point, padded on the left to the width.
        assert_eq!(spark(&[Some(3.0)], 4), "   \u{2584}");
        // None is a gap; NaN and infinity are gaps too and do not stretch
        // the scale (0 and 7 stay the end blocks).
        let values = [
            Some(0.0),
            None,
            Some(f32::NAN),
            Some(7.0),
            Some(f32::INFINITY),
            Some(f32::NEG_INFINITY),
        ];
        assert_eq!(spark(&values, 6), "\u{2581}  \u{2588}  ");
        // Nothing but gaps, and nothing at all.
        assert_eq!(spark(&[None, None], 3), "   ");
        assert_eq!(spark(&[], 3), "   ");
        assert_eq!(spark(&[Some(1.0)], 0), "");
        // A short window keeps its newest value last.
        assert_eq!(spark(&[Some(1.0), Some(2.0)], 4), "  \u{2581}\u{2588}");
    }

    /// The block index (0 lowest .. 7 highest) of each cell of a sparkline,
    /// `None` for a gap.
    fn levels(line: &str) -> Vec<Option<usize>> {
        line.chars()
            .map(|c| BLOCKS.iter().position(|&b| b == c))
            .collect()
    }

    #[test]
    fn sixty_samples_are_bucketed_into_twenty_four_cells() {
        // A ramp: the oldest sample is the lowest, the newest the highest.
        // Every sample must count, so the oldest bucket draws the lowest
        // block in the leftmost cell and the newest the highest block in
        // the rightmost cell.
        for n in [60usize, 120] {
            let values: Vec<Option<f32>> = (0..n).map(|i| Some(i as f32)).collect();
            let line = spark(&values, SPARK_WIDTH);
            assert_eq!(line.chars().count(), SPARK_WIDTH, "{n}: {line}");
            let l = levels(&line);
            assert_eq!(l[0], Some(0), "{n}: oldest bucket leftmost: {line}");
            assert_eq!(l[SPARK_WIDTH - 1], Some(7), "{n}: newest rightmost: {line}");
            // A ramp stays a ramp: never lower than the cell before it.
            for w in l.windows(2) {
                assert!(w[0] <= w[1], "{n}: {line}");
            }
        }
        // A spike in the oldest sample only shows in the leftmost cell, and
        // one in the newest sample only in the rightmost.
        let mut values = vec![Some(0.0f32); 60];
        values[0] = Some(100.0);
        let l = levels(&spark(&values, SPARK_WIDTH));
        assert!(l[0] > Some(0), "{l:?}");
        assert!(l[1..].iter().all(|&c| c == Some(0)), "{l:?}");
        let mut values = vec![Some(0.0f32); 60];
        values[59] = Some(100.0);
        let l = levels(&spark(&values, SPARK_WIDTH));
        assert!(l[SPARK_WIDTH - 1] > Some(0), "{l:?}");
        assert!(l[..SPARK_WIDTH - 1].iter().all(|&c| c == Some(0)), "{l:?}");
        // Each cell is the mean of its bucket: 60 samples into 24 cells
        // gives buckets of 2 and 3 samples (edges i * 60 / 24). Cell 1
        // holds samples 2, 3 and 4, here 0, 7 and 14: its mean is 7, the
        // same as the last cell (samples 57 to 59, all 7), the window's top.
        let mut values = vec![Some(0.0f32); 60];
        values[3] = Some(7.0);
        values[4] = Some(14.0);
        values[59] = Some(7.0);
        values[58] = Some(7.0);
        values[57] = Some(7.0);
        let l = levels(&spark(&values, SPARK_WIDTH));
        assert_eq!(l[1], l[SPARK_WIDTH - 1], "{l:?}");
        assert_eq!(l[1], Some(7), "{l:?}");
    }

    #[test]
    fn a_bucket_without_a_finite_sample_is_a_gap() {
        // Cell 2 holds samples 5 and 6 (edges 5 and 7): with neither
        // readable it is a gap. Cell 3 (samples 7, 8, 9) with one NaN and
        // one None still has a reading and is drawn.
        let mut values: Vec<Option<f32>> = (0..60).map(|i| Some(i as f32)).collect();
        values[5] = None;
        values[6] = Some(f32::NAN);
        values[7] = None;
        values[8] = Some(f32::INFINITY);
        let line = spark(&values, SPARK_WIDTH);
        let cells: Vec<char> = line.chars().collect();
        assert_eq!(cells[2], ' ', "{line}");
        assert_ne!(cells[3], ' ', "{line}");
        assert_eq!(line.chars().filter(|&c| c == ' ').count(), 1, "{line}");
    }

    #[test]
    fn a_one_watt_wobble_stays_at_the_bottom_of_the_scale() {
        // 13 W with +-1 W of noise per sample, as the product owner saw on
        // a real board (13 to 16 W chips drawn as full-height bars). Against
        // a 5 W minimum span the cells, each the mean of two or three
        // samples, stay in the lowest two blocks.
        let noise = [0.0f32, 1.0, 0.0, -1.0];
        let values: Vec<Option<f32>> = (0..60).map(|i| Some(13.0 + noise[i % 4])).collect();
        let line = spark_scaled(&values, SPARK_WIDTH, POWER_MIN_SPAN_W);
        let l = levels(&line);
        assert!(l.iter().all(|c| matches!(c, Some(0) | Some(1))), "{line}");
        assert!(l.contains(&Some(0)), "{line}");
        // A constant reading sits on the lowest block.
        let line = spark_scaled(&[Some(13.0); 60], SPARK_WIDTH, POWER_MIN_SPAN_W);
        assert_eq!(line, BLOCKS[0].to_string().repeat(SPARK_WIDTH));
        // PCIe: a 3 MB/s wobble against the 10 MB/s floor stays low too.
        let values: Vec<Option<f32>> = (0..60)
            .map(|i| Some(5.0e6 + 3.0e6 * (i % 2) as f32))
            .collect();
        let l = levels(&spark_scaled(&values, SPARK_WIDTH, PCIE_MIN_SPAN_BPS));
        assert!(l.iter().all(|&c| c <= Some(3)), "{l:?}");
    }

    #[test]
    fn a_20_watt_swing_still_uses_the_full_range() {
        // 30 W idle, then 50 W: above the 5 W minimum span, the window's own
        // range is the scale, so both end blocks show.
        let values: Vec<Option<f32>> = (0..60)
            .map(|i| Some(if i < 30 { 30.0 } else { 50.0 }))
            .collect();
        let line = spark_scaled(&values, SPARK_WIDTH, POWER_MIN_SPAN_W);
        let l = levels(&line);
        assert_eq!(l[0], Some(0), "{line}");
        assert_eq!(l[SPARK_WIDTH - 1], Some(7), "{line}");
    }

    #[test]
    fn the_panel_draws_each_row_against_its_minimum_span() {
        // The panel's own rows, so the wiring is checked as well as
        // `spark_scaled`: a minute of a chip at 13 W give or take a watt
        // and of PCIe wobbling by 3 MB/s draws low rows.
        let noise = [0.0f32, 1.0, 0.0, -1.0];
        let mut h = History::new();
        for k in 0..SAMPLES {
            let mut c = chip(0, Some(13.0 + noise[k % 4]), Some(50.0));
            c.pcie_in_bps = Some(5.0e6 + 3.0e6 * (k % 2) as f64);
            c.pcie_out_bps = Some(5.0e6 + 3.0e6 * (k % 2) as f64);
            assert!(h.sample(k as f64, &[c]));
        }
        let rows = h.panel_rows();
        let spark_of =
            |row: &str| -> String { row.chars().skip(LABEL_W + 1).take(SPARK_WIDTH).collect() };
        let power = levels(&spark_of(&rows[1]));
        assert!(power.iter().all(|&c| c <= Some(1)), "{rows:#?}");
        for row in &rows[2..4] {
            let pcie = levels(&spark_of(row));
            assert!(pcie.iter().all(|&c| c <= Some(3)), "{rows:#?}");
        }
    }
}
