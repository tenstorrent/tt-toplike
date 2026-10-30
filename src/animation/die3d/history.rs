// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The history panel: the last minute of measured telemetry, as sparklines.
//!
//! Taylor asked for "some kind of display small showing the data it's
//! monitoring changing over time". The scene shows the present; this panel
//! shows how each Blackhole chip's power and temperature and the box's PCIe
//! traffic moved over the last 60 seconds. Every value is a reading from the
//! backend, never an estimate.
//!
//! [`History`] keeps one sample per second of view time: per chip its power
//! and temperature, and for the box the PCIe rates in and out summed over the
//! chips the view draws. [`spark`] turns a window of samples into a row of
//! block characters. `Die3dView` samples in `update` and writes
//! [`History::panel_rows`] over the top-left of the scene.
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
/// Cells of each sparkline.
pub const SPARK_WIDTH: usize = 24;
/// The panel's title row.
pub const TITLE: &str = "history 60 s";

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
    /// (`▲`).
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
                spark(&s.power, SPARK_WIDTH),
                latest(&s.power).map_or("-".into(), |w| format!("{w:.0}W")),
                latest(&s.temp).map_or("-".into(), |c| format!("{c:.0}\u{b0}")),
            ));
        }
        for (label, ring) in [
            ("PCIe \u{25bc}", &self.pcie_in),
            ("     \u{25b2}", &self.pcie_out),
        ] {
            // The sparkline scales each window to its own range, so f32 is
            // precise enough for any rate a link can carry.
            let values: Vec<Option<f32>> = ring.iter().map(|v| v.map(|b| b as f32)).collect();
            rows.push(format!(
                "{label} {} {}",
                spark(&values, SPARK_WIDTH),
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

/// The last `width` of `values` as a sparkline of `width` characters.
///
/// Each value maps onto the eight blocks `▁▂▃▄▅▆▇█`, scaled between the
/// lowest and highest value in the window, so the line shows how a reading
/// moved within the minute, whatever its size. A window whose values are all
/// equal, or that holds one value, draws `▄`. A `None` sample is a space, a
/// gap where there was no reading; NaN and infinite values count as `None`
/// and do not enter the scale. A window shorter than `width` is padded with
/// spaces on the left, so the newest sample is always the last character.
pub fn spark(values: &[Option<f32>], width: usize) -> String {
    let window = &values[values.len().saturating_sub(width)..];
    let finite = |v: &Option<f32>| v.filter(|x| x.is_finite());
    let (lo, hi) = window
        .iter()
        .filter_map(finite)
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| {
            (lo.min(x), hi.max(x))
        });
    let span = hi - lo;
    let mut out = String::with_capacity(width * 3);
    out.extend(std::iter::repeat_n(' ', width - window.len()));
    for v in window {
        out.push(match finite(v) {
            None => ' ',
            // Flat or a single point. `span` is also NaN or 0 here; checking
            // `> 0` with a finite span covers both.
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
        // blocks, and only the last `width` values count.
        let values = [Some(1000.0), Some(-4.0), Some(9.0), Some(-1.0)];
        assert_eq!(spark(&values, 3), "\u{2581}\u{2588}\u{2583}");
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
}
