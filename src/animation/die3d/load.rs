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
//! [`LoadScale`] instead reports where the present draw sits between the
//! chip's own idle and its recent peak:
//!
//! - The peak is the highest draw of the last ten minutes, one sample per
//!   second of view time.
//! - The idle is the lowest draw of those ten minutes and of the last day.
//!   For the day the scale keeps the lowest draw of each completed minute,
//!   1440 of them. With the ten-minute window alone, a job at a steady
//!   110 W became its own idle after ten minutes, and the chip read as idle
//!   (load about 0.08) while its label said 110 W. With the day, a chip
//!   that idled at 37 W earlier keeps 37 W as its idle for 24 hours.
//!
//! A limit: a view opened while a job is already running has no idle sample
//! to measure against, so a chip at a constant draw reads 0.0 until its
//! power changes (see the test
//! `a_view_that_opens_during_a_steady_job_reads_zero_until_power_changes`).
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
/// A reading at or below this many watts is not a draw: a powered Blackhole
/// never idles that low, so it is a sensor glitch or a chip reset, and it is
/// ignored like a missing reading.
pub const GLITCH_W: f32 = 1.0;
/// Which draw of a minute stands for it in the idle's memory: the 10th
/// percentile (the sixth lowest of sixty). Its lowest draw would let a single
/// low reading set the idle for a day; a tenth of a minute of low readings is a
/// real low.
const IDLE_QUANTILE: f32 = 0.10;
/// Per-minute idle floors kept for the idle: one day.
pub const DAY_MINUTES: usize = 1440;
/// Most draws kept for the minute in progress, whatever the sample rate.
const MINUTE_SAMPLES_MAX: usize = 120;
/// Seconds of view time in one minute of the idle's memory.
const MINUTE_S: f64 = 60.0;

/// One chip's power over the last ten minutes and its idle over the last
/// day, and its load relative to those.
#[derive(Debug, Clone)]
pub struct LoadScale {
    /// The window, oldest first, in watts. Never holds a non-finite value.
    samples: VecDeque<f32>,
    /// View time of the newest sample, `None` before the first.
    last_at: Option<f64>,
    /// What the last call returned; returned again when a call adds no sample.
    last_load: f32,
    /// The idle floor of each completed minute of view time that had a sample
    /// (the [`IDLE_QUANTILE`] draw of that minute), oldest first, as (minute
    /// number, watts), where minute `m` covers view time `60 m .. 60 (m + 1)`.
    /// Only the last [`DAY_MINUTES`] minutes are kept, so it never holds more
    /// than that many entries.
    minutes: VecDeque<(i64, f32)>,
    /// The minute in progress and its draws so far, `None` before the first
    /// sample. Its floor joins `minutes` when a sample lands in a later minute.
    open_minute: Option<(i64, Vec<f32>)>,
}

/// The [`IDLE_QUANTILE`] draw of `draws`: the lowest when there are fewer than
/// ten, since one low reading among so few is all there is to go on. Never
/// called with none.
fn floor_of(draws: &[f32]) -> f32 {
    let mut sorted = draws.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted[((sorted.len() - 1) as f32 * IDLE_QUANTILE) as usize]
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
            minutes: VecDeque::with_capacity(DAY_MINUTES),
            open_minute: None,
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

    /// The chip's idle in watts: the lowest idle floor of the last day's
    /// minutes, the minute in progress included, `None` before the first sample.
    /// A minute's floor is its [`IDLE_QUANTILE`] draw, so one low reading does not
    /// move it (see [`GLITCH_W`] for the readings that are ignored outright).
    pub fn idle(&self) -> Option<f32> {
        self.open_minute
            .iter()
            .map(|(_, draws)| floor_of(draws))
            .chain(self.minutes.iter().map(|&(_, w)| w))
            .reduce(f32::min)
    }

    /// Fold a sample at `now` into the per-minute floors. A sample in a later
    /// minute closes the open one, whose floor joins the ring, and minutes
    /// more than a day older than the new one leave it.
    fn remember_minute(&mut self, now: f64, power: f32) {
        let minute = (now / MINUTE_S).floor() as i64;
        match self.open_minute.as_mut() {
            Some((m, draws)) if *m == minute => {
                if draws.len() < MINUTE_SAMPLES_MAX {
                    draws.push(power);
                }
            }
            _ => {
                if let Some((closed, draws)) = self.open_minute.take() {
                    self.minutes.push_back((closed, floor_of(&draws)));
                }
                self.open_minute = Some((minute, vec![power]));
                // Keep the minutes `minute - DAY_MINUTES .. minute`: a day of
                // completed minutes before the open one. Minute numbers only
                // grow (time never goes backwards here), so this also bounds
                // the ring at DAY_MINUTES entries.
                while self
                    .minutes
                    .front()
                    .is_some_and(|&(m, _)| minute - m > DAY_MINUTES as i64)
                {
                    self.minutes.pop_front();
                }
            }
        }
    }

    /// Feed the chip's power at view time `now` and return its load, 0..1.
    ///
    /// The view calls this every tick, which may be faster than once a
    /// second. A call less than [`SAMPLE_INTERVAL_S`] after the last sample,
    /// a `now` that is not finite or goes backwards, and a power that is
    /// `None`, NaN, infinite or at most [`GLITCH_W`] all leave the window alone
    /// and return the previous result.
    pub fn observe(&mut self, now: f64, power_w: Option<f32>) -> f32 {
        let Some(power) = power_w.filter(|p| p.is_finite() && *p > GLITCH_W) else {
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
        self.remember_minute(now, power);
        self.last_at = Some(now);
        self.last_load = if self.ready() {
            self.relative_load(power)
        } else {
            0.0
        };
        self.last_load
    }

    /// Where `power` sits between the idle ([`idle`](Self::idle)) and the
    /// window's highest draw, with the span floored at [`MIN_SPAN_W`]. The
    /// window is never empty here (the caller has just pushed `power`), and
    /// the floor keeps the divisor at 25 W or more, so there is no division
    /// by zero. An idle from earlier in the day can sit below the whole
    /// window; the floor and the clamp keep the result in 0..1.
    fn relative_load(&self, power: f32) -> f32 {
        let idle = self.idle().unwrap_or(power);
        let peak = self
            .samples
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let span = (peak - idle).max(MIN_SPAN_W);
        ((power - idle) / span).clamp(0.0, 1.0)
    }
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
        for (i, w) in [20.0, 100.0, 20.0, 100.0].into_iter().enumerate() {
            assert_eq!(s.observe(i as f64, Some(w)), 0.0);
            assert!(!s.ready());
        }
        // The fifth makes it ready: 100 W against a 20 to 100 W window.
        assert_eq!(s.observe(4.0, Some(100.0)), 1.0);
        assert!(s.ready());
        assert_eq!(s.len(), 5);
    }

    /// A reading at or below a watt is a sensor glitch or a reset, not a draw: it
    /// changes nothing, like a missing reading, and the load stays as it was.
    #[test]
    fn a_glitch_reading_is_ignored_like_a_missing_one() {
        let mut s = LoadScale::new();
        let (_, t) = feed(&mut s, 0.0, &[37.0; 10]);
        let before = s.observe(t, Some(52.5));
        let len = s.len();
        let mut at = t + 1.0;
        for glitch in [0.0, 0.5, GLITCH_W, -3.0] {
            assert_eq!(s.observe(at, Some(glitch)), before, "{glitch} W");
            assert_eq!(s.len(), len, "{glitch} W was kept");
            at += 1.0;
        }
        assert_eq!(s.idle(), Some(37.0), "the idle did not move");
        // Just above the glitch level is a draw.
        s.observe(at, Some(GLITCH_W + 0.5));
        assert_eq!(s.len(), len + 1);
    }

    /// The idle is a minute's 10th percentile, not its lowest draw: one low
    /// reading among a minute of steady ones leaves the idle where it was, which
    /// is what stopped a single bad sample becoming a chip's idle for a day.
    #[test]
    fn one_low_reading_does_not_set_the_idle_but_a_real_low_does() {
        // 59 steady seconds at 37 W with one 5 W reading in them, then the next
        // minute opens so the first is closed into the day's memory.
        let mut s = LoadScale::new();
        let mut watts = vec![37.0; 60];
        watts[30] = 5.0;
        let (_, t) = feed(&mut s, 0.0, &watts);
        s.observe(t, Some(37.0));
        assert_eq!(s.idle(), Some(37.0), "one low reading among sixty");
        // A load against that idle is still measured from 37 W.
        let load = s.observe(t + 1.0, Some(62.0));
        assert!(
            (load - 1.0).abs() < 1e-6,
            "62 W is the full 25 W span above 37 W: {load}"
        );

        // Ten low readings in a minute (a sixth of it) are a real low.
        let mut real = LoadScale::new();
        let mut watts = vec![37.0; 60];
        for w in watts.iter_mut().take(10) {
            *w = 20.0;
        }
        let (_, t) = feed(&mut real, 0.0, &watts);
        real.observe(t, Some(37.0));
        assert_eq!(real.idle(), Some(20.0));

        // The minute in progress counts too, so a real low shows at once.
        let mut open = LoadScale::new();
        feed(&mut open, 0.0, &[37.0; 5]);
        assert_eq!(open.idle(), Some(37.0));
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
    fn a_long_burst_does_not_re_baseline() {
        let mut s = LoadScale::new();
        let (_, t) = feed(&mut s, 0.0, &[37.0; 60]);
        // A 300 s burst at 68 W: the 37 W idle from before it is still the
        // window's minimum, so the burst stays at full load throughout.
        let (load, t) = feed(&mut s, t, &[68.0; 300]);
        assert_eq!(load, 1.0);
        assert_eq!(s.idle(), Some(37.0));
        // The window holds 600 samples at most. After another 300 s of burst
        // the idle samples have left it, but the per-minute minima still
        // hold 37 W, so the burst is still full load. (Before the minima,
        // 68 W became the new idle here and the burst read 0.0.)
        let (load, _) = feed(&mut s, t, &[68.0; 300]);
        assert_eq!(s.len(), WINDOW_SAMPLES);
        assert!(s.samples.iter().all(|&w| w == 68.0));
        assert_eq!(s.idle(), Some(37.0));
        assert_eq!(load, 1.0);
    }

    #[test]
    fn a_steady_job_longer_than_ten_minutes_stays_busy() {
        // 37 W idle for five minutes, then a job at a steady 110 W for half
        // an hour. The 600-sample window loses the idle samples after ten
        // minutes of the job; the per-minute minima keep 37 W as the idle,
        // so the job reads full load the whole time.
        let mut s = LoadScale::new();
        let (_, t) = feed(&mut s, 0.0, &[37.0; 300]);
        let mut t = t;
        for i in 0..1800 {
            let load = s.observe(t, Some(110.0));
            assert_eq!(load, 1.0, "{i} s into the job");
            t += 1.0;
        }
        assert_eq!(s.idle(), Some(37.0));
        assert_eq!(s.len(), WINDOW_SAMPLES);
    }

    #[test]
    fn idle_is_remembered_for_a_day_and_then_forgotten() {
        // One sample a minute (half a minute in, so each lands in its own
        // minute) for a day and two minutes, the floor rising by 0.01 W a
        // minute: minute k draws 30 + 0.01 k watts.
        let watts = |k: i64| 30.0 + 0.01 * k as f32;
        let mut s = LoadScale::new();
        for k in 0..=1441i64 {
            s.observe(k as f64 * 60.0 + 30.0, Some(watts(k)));
            if k == 1439 {
                // A day in (minute 1439 is the newest, still open): every
                // completed minute is remembered, minute 0's 30 W included.
                assert_eq!(s.idle(), Some(watts(0)));
            }
        }
        // Minute 1441 is open, so the day of completed minutes is 1 to 1440:
        // minute 0 has dropped out and minute 1 is the idle.
        assert_eq!(s.idle(), Some(watts(1)));
        s.observe(1442.0 * 60.0 + 30.0, Some(watts(1442)));
        assert_eq!(s.idle(), Some(watts(2)));
    }

    #[test]
    fn the_minute_ring_is_bounded() {
        let mut s = LoadScale::new();
        // Three days of one sample every 20 s.
        let mut t = 0.0;
        while t < 3.0 * 86_400.0 {
            s.observe(t, Some(40.0 + (t / 60.0) as f32 % 7.0));
            assert!(s.minutes.len() <= DAY_MINUTES, "{} at {t}", s.minutes.len());
            t += 20.0;
        }
        assert_eq!(s.minutes.len(), DAY_MINUTES);
    }

    #[test]
    fn a_view_that_opens_during_a_steady_job_reads_zero_until_power_changes() {
        // A documented limit: with no earlier idle to measure against, a
        // chip that has drawn 110 W since the view opened is its own idle.
        let mut s = LoadScale::new();
        let (load, t) = feed(&mut s, 0.0, &[110.0; 120]);
        assert_eq!(load, 0.0);
        // Once the job ends the chip shows its real idle, and from then on
        // the next job shows full load.
        let (load, t) = feed(&mut s, t, &[37.0; 30]);
        assert_eq!(load, 0.0);
        assert_eq!(s.observe(t, Some(110.0)), 1.0);
    }
}
