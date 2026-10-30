// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Each chip's activity, measured against that chip's own idle.
//!
//! The view first measured load as chip power over the hwmon power limit.
//! On this box `power1_max` is 125 W and idle chips draw 36 to 47 W, so an
//! idle chip already showed a load of 0.30. That is above the trigger
//! detector's re-arm level of 0.25 (`triggers::LOAD_REARM`), so `LoadRise`
//! could never arm, the camera never cut on activity and the tile glow barely
//! moved. Under real token serving (8 concurrent chat clients, 213 tokens per
//! second against the Qwen3-Coder-Next server) the two busy chips drew a mean
//! of 54 and 45 W with a peak of 68 W, only 0.54 of the limit.
//!
//! [`LoadScale`] instead remembers the last ten minutes of one chip's power,
//! one sample per second of view time, and reports where the present draw
//! sits between the lowest (idle) and highest (peak) draw of that window.
//!
//! Pure logic: no clock, no I/O. The caller passes `now` in seconds.

use std::collections::VecDeque;

/// Samples kept: one per second for ten minutes.
pub const WINDOW_SAMPLES: usize = 600;
/// Seconds of view time between two samples, at least.
pub const SAMPLE_INTERVAL_S: f64 = 1.0;
/// Samples needed before the scale reports anything but `0.0`. Until then the
/// view uses the power-over-limit load from `scene::chip_inputs`.
pub const MIN_SAMPLES: usize = 5;
/// The smallest idle-to-peak span, in watts. An idle chip wobbles by a few
/// watts; without a floor that wobble would be scaled up to full load. 25 W
/// is below the 31 W rise measured under real serving (37 W idle to a 68 W
/// peak), so a real burst still reaches full load, and a 3 W wobble stays
/// under 0.13.
pub const MIN_SPAN_W: f32 = 25.0;

/// One chip's power over the last ten minutes, and its load relative to that.
#[derive(Debug, Clone)]
pub struct LoadScale {
    /// The window, oldest first, in watts. Never holds a non-finite value.
    samples: VecDeque<f32>,
    /// View time of the newest sample, `None` before the first.
    last_at: Option<f64>,
    /// What the last call returned; returned again when a call adds no sample.
    last_load: f32,
}

impl Default for LoadScale {
    fn default() -> Self {
        LoadScale::new()
    }
}

impl LoadScale {
    /// An empty scale: it reports `0.0` until it holds [`MIN_SAMPLES`].
    pub fn new() -> LoadScale {
        LoadScale {
            samples: VecDeque::with_capacity(WINDOW_SAMPLES),
            last_at: None,
            last_load: 0.0,
        }
    }

    /// Samples in the window.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// True before the first sample.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// True once the scale holds enough samples for its load to replace the
    /// power-over-limit fallback.
    pub fn ready(&self) -> bool {
        self.samples.len() >= MIN_SAMPLES
    }

    /// The window's lowest draw in watts (the chip's idle), `None` while the
    /// window is empty.
    pub fn idle(&self) -> Option<f32> {
        self.samples.iter().copied().reduce(f32::min)
    }

    /// Feed the chip's power at view time `now` and return its load, 0..1.
    ///
    /// The view calls this every tick, which may be faster than once a
    /// second. A call less than [`SAMPLE_INTERVAL_S`] after the last sample,
    /// a `now` that is not finite or goes backwards, and a power that is
    /// `None`, NaN or infinite all leave the window alone and return the
    /// previous result.
    pub fn observe(&mut self, now: f64, power_w: Option<f32>) -> f32 {
        let Some(power) = power_w.filter(|p| p.is_finite()) else {
            return self.last_load;
        };
        if !now.is_finite() {
            return self.last_load;
        }
        // Backwards time and a tick inside the same second both fail this:
        // `now - last` is then below one second (negative when backwards).
        if let Some(last) = self.last_at {
            if now - last < SAMPLE_INTERVAL_S {
                return self.last_load;
            }
        }
        if self.samples.len() == WINDOW_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(power);
        self.last_at = Some(now);
        self.last_load = if self.ready() {
            relative_load(&self.samples, power)
        } else {
            0.0
        };
        self.last_load
    }
}

/// Where `power` sits between the window's lowest and highest draw, with the
/// span floored at [`MIN_SPAN_W`]. The window is never empty here (the caller
/// has just pushed `power`), and the floor keeps the divisor at 25 W or more,
/// so there is no division by zero.
fn relative_load(window: &VecDeque<f32>, power: f32) -> f32 {
    let idle = window.iter().copied().fold(f32::INFINITY, f32::min);
    let peak = window.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let span = (peak - idle).max(MIN_SPAN_W);
    ((power - idle) / span).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `watts` once a second from `start`, returning the last result and
    /// the time after the last sample.
    fn feed(s: &mut LoadScale, start: f64, watts: &[f32]) -> (f32, f64) {
        let mut last = 0.0;
        let mut t = start;
        for &w in watts {
            last = s.observe(t, Some(w));
            t += 1.0;
        }
        (last, t)
    }

    #[test]
    fn idle_power_reads_as_zero_load() {
        // 37 W is a measured idle draw. On its own it is idle, whatever the
        // power limit: 0.0. As power over the 125 W limit it was 0.30.
        let mut s = LoadScale::new();
        let (load, _) = feed(&mut s, 0.0, &[37.0; 60]);
        assert_eq!(load, 0.0);
        assert!(s.ready());
    }

    #[test]
    fn a_burst_to_its_own_peak_reads_as_full_load() {
        // Measured: 37 W idle, a 68 W peak under serving. The span is 31 W
        // (above the 25 W floor), so 68 W is full load and the ramp in
        // between is linear.
        let mut s = LoadScale::new();
        let (_, t) = feed(&mut s, 0.0, &[37.0; 30]);
        let mut seen = Vec::new();
        for (i, w) in [44.0, 52.0, 60.0, 68.0].into_iter().enumerate() {
            seen.push(s.observe(t + i as f64, Some(w)));
        }
        // On the way up the peak is the present draw, and the span is that
        // rise or the 25 W floor, whichever is larger.
        // 44 W: span max(7, 25) = 25, (44 - 37) / 25 = 0.28.
        assert!((seen[0] - 0.28).abs() < 1e-6, "{seen:?}");
        // 68 W: span 31 W, (68 - 37) / 31 = 1.0.
        assert_eq!(seen[3], 1.0, "{seen:?}");
        // Back to 52.5 W with the 68 W peak still in the window: half way.
        let half = s.observe(t + 4.0, Some(52.5));
        assert!((half - 0.5).abs() < 1e-6, "{half}");
    }

    #[test]
    fn the_span_has_a_25_watt_floor() {
        // An idle chip wobbling by 3 W around 40 W is not busy. Without the
        // floor the top of the wobble would be full load.
        let mut s = LoadScale::new();
        let wobble: Vec<f32> = (0..120)
            .map(|i| 38.5 + 3.0 * ((i % 7) as f32 / 6.0))
            .collect();
        let mut t = 0.0;
        for &w in &wobble {
            let load = s.observe(t, Some(w));
            assert!(load < 0.13, "{w} W at {t} s gave {load}");
            t += 1.0;
        }
        // The wobble really does reach its 3 W top (41.5 W) once the window
        // holds its 38.5 W bottom: 3 / 25 = 0.12.
        let top = s.observe(t, Some(41.5));
        assert!((top - 0.12).abs() < 1e-6, "{top}");
    }

    #[test]
    fn fewer_than_five_samples_give_zero() {
        let mut s = LoadScale::new();
        // Four samples spanning idle to far above it: still nothing to say.
        for (i, w) in [0.0, 100.0, 0.0, 100.0].into_iter().enumerate() {
            assert_eq!(s.observe(i as f64, Some(w)), 0.0);
            assert!(!s.ready());
        }
        // The fifth makes it ready: 100 W against a 0 to 100 W window.
        assert_eq!(s.observe(4.0, Some(100.0)), 1.0);
        assert!(s.ready());
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn none_nan_infinite_and_time_going_backwards_do_not_change_the_window() {
        let mut s = LoadScale::new();
        let (_, t) = feed(&mut s, 0.0, &[37.0; 10]);
        let before = s.observe(t, Some(52.5));
        assert!((before - 0.62).abs() < 1e-6, "{before}");
        assert_eq!(s.len(), 11);
        // Bad power, each a second later: the previous result, nothing added.
        let mut at = t + 1.0;
        for bad in [
            None,
            Some(f32::NAN),
            Some(f32::INFINITY),
            Some(f32::NEG_INFINITY),
        ] {
            assert_eq!(s.observe(at, bad), before, "{bad:?}");
            assert_eq!(s.len(), 11, "{bad:?}");
            at += 1.0;
        }
        // Time going backwards, or not a number: the same.
        assert_eq!(s.observe(t - 5.0, Some(500.0)), before);
        assert_eq!(s.observe(f64::NAN, Some(500.0)), before);
        assert_eq!(s.observe(f64::INFINITY, Some(500.0)), before);
        assert_eq!(s.len(), 11);
        // The window was not touched: a good sample is measured against
        // 37 W idle as before. The refused 500 W never entered the window.
        assert_eq!(s.observe(at, Some(37.0)), 0.0);
        let back = s.observe(at + 1.0, Some(52.5));
        assert!((back - before).abs() < 1e-6, "{back}");
    }

    #[test]
    fn one_sample_per_second_at_most() {
        let mut s = LoadScale::new();
        let (_, t) = feed(&mut s, 0.0, &[37.0; 10]);
        assert_eq!(s.len(), 10);
        // A tick every 0.1 s: the first of each second is sampled, the rest
        // return the previous result even when the power differs.
        let first = s.observe(t, Some(68.0));
        assert_eq!(first, 1.0);
        for k in 1..10 {
            let at = t + k as f64 * 0.1;
            assert_eq!(s.observe(at, Some(37.0)), first, "{at}");
        }
        assert_eq!(s.len(), 11);
        // A full second after the last sample: sampled.
        assert_eq!(s.observe(t + 1.0, Some(37.0)), 0.0);
        assert_eq!(s.len(), 12);
    }

    #[test]
    fn a_long_burst_does_not_re_baseline_within_ten_minutes() {
        let mut s = LoadScale::new();
        let (_, t) = feed(&mut s, 0.0, &[37.0; 60]);
        // A 300 s burst at 68 W: the 37 W idle from before it is still the
        // window's minimum, so the burst stays at full load throughout.
        let (load, t) = feed(&mut s, t, &[68.0; 300]);
        assert_eq!(load, 1.0);
        assert_eq!(s.idle(), Some(37.0));
        // The window holds 600 samples at most. After another 300 s of burst
        // the idle samples have left it, and 68 W becomes the new idle.
        let (load, _) = feed(&mut s, t, &[68.0; 300]);
        assert_eq!(s.len(), WINDOW_SAMPLES);
        assert_eq!(s.idle(), Some(68.0));
        assert_eq!(load, 0.0);
    }
}
