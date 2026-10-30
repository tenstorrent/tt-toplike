// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Clouds drifting behind the die.
//!
//! The clouds are decoration. They are still tied to the hardware, as
//! everything that moves in this view is: their drift speed follows the total
//! PCIe traffic of the chips drawn ([`Clouds::drift_px_per_s`]) and their
//! brightness follows chip load ([`Clouds::intensity`]). The legend and the
//! `i` text say so.
//!
//! The picture is two octaves of tiling value noise. Each octave is a 64 x 32
//! lattice of random values from a seed, wrapping at its edges and sampled
//! with bilinear interpolation. The second octave has half the lattice
//! spacing and drifts at half the speed of the first, so the clouds change
//! shape slowly while they drift. The noise is evaluated on a half-resolution
//! grid and each value covers 2 x 2 pixels, which keeps the cost far below the
//! 1 ms per frame budget.
//!
//! A pixel whose density is above [`DENSITY_THRESHOLD`] is painted with
//! `palette.cloud`, scaled by its density band and by the intensity. Every
//! other pixel is left untouched: in transparent mode clear sky stays
//! uncovered and the terminal's own background shows through it.
//!
//! The cost that matters most here is terminal traffic. Task 16 cut
//! the cells that change per frame by more than half, and a cloud field that
//! changed every cell it covers on every frame would give that back. So:
//!
//! - the drift is integrated on a clock that steps 4 times a second
//!   ([`CloudClock`]), and the offset the picture uses is the integral rounded
//!   down to a whole pixel, so the sky changes only when the drift crosses a
//!   pixel boundary (at most once per 1.25 s at the 0.8 px/s floor);
//! - the colour comes in three flat density bands, so a
//!   whole-pixel shift changes only the cells along band edges, and the
//!   inside of a band stays one flat colour from frame to frame;
//! - the intensity moves in steps of 0.05 with hysteresis, so a load that
//!   wobbles at a step's edge does not recolour every cloud cell each tick.
//!
//! Pure logic: no clock, no I/O. The caller passes time in seconds.

use super::palette::GRAYSKULL;
use super::raster::PixelBuf;

/// Drift speed with no PCIe reading, or at or below 1 kB/s: the sky is never
/// frozen.
pub const DRIFT_FLOOR_PX_PER_S: f32 = 0.8;
/// Drift speed at 10 GB/s and above.
pub const DRIFT_MAX_PX_PER_S: f32 = 8.0;
/// Base-10 logarithm of the PCIe rate (bytes per second) where the drift
/// starts rising from the floor: 1 kB/s.
const DRIFT_FROM_LOG10: f64 = 3.0;
/// Base-10 logarithm of the PCIe rate where the drift reaches its maximum:
/// 10 GB/s.
const DRIFT_TO_LOG10: f64 = 10.0;
/// Intensity at load 0.
pub const INTENSITY_MIN: f32 = 0.25;
/// Intensity at load 1.
pub const INTENSITY_MAX: f32 = 0.75;
/// The shown intensity moves in steps of 1 / this (0.05; see
/// [`CloudClock`]). Kept as a count so a step is an exact division.
const INTENSITY_STEPS: f32 = 20.0;
/// One step of the shown intensity.
const INTENSITY_STEP: f32 = 1.0 / INTENSITY_STEPS;
/// How far the load's intensity must be from the shown one before the shown
/// one moves: three quarters of a step, so a value sitting on the edge
/// between two steps does not flip between them.
const INTENSITY_HYSTERESIS: f32 = 0.75 * INTENSITY_STEP;
/// Pixels with a density at or below this stay untouched (clear sky).
pub const DENSITY_THRESHOLD: f32 = 0.45;
/// The cloud clock's steps per second (see [`CloudClock`]).
pub const CLOCK_STEPS_PER_SEC: f64 = 4.0;
/// Lattice size of one noise octave: 64 columns by 32 rows, wrapping.
const TILE_W: usize = 64;
const TILE_H: usize = 32;

/// One octave of the noise.
struct Octave {
    /// Lattice spacing in pixels, horizontal and vertical. Wider than tall,
    /// so the clouds are long, flat banks.
    spacing: (f64, f64),
    /// Weight in the density; the weights sum to 1.
    weight: f32,
    /// Share of the drift offset this octave moves by.
    speed: f64,
    /// Where on the lattice this octave starts, in lattice cells, so the two
    /// octaves do not read the same values in step.
    shift: (f64, f64),
}

/// The two octaves. The second has half the spacing and half the speed.
const OCTAVES: [Octave; 2] = [
    Octave {
        spacing: (48.0, 16.0),
        weight: 0.7,
        speed: 1.0,
        shift: (0.0, 0.0),
    },
    Octave {
        spacing: (24.0, 8.0),
        weight: 0.3,
        speed: 0.5,
        shift: (29.0, 13.0),
    },
];

/// The density bands above the threshold and the share of the cloud colour
/// each is painted with: thin cloud dimmest, thick cloud full. A band holds
/// densities up to its first value (the last band takes everything above).
const BANDS: [(f32, f32); 3] = [(0.55, 0.6), (0.65, 0.8), (f32::INFINITY, 1.0)];

/// The cloud field of one view: the noise lattice made from a seed.
#[derive(Debug, Clone)]
pub struct Clouds {
    /// `TILE_W * TILE_H` values in 0..1, row-major.
    lattice: Vec<f32>,
}

/// Scale a colour by `k`, rounded and clamped like `raster`'s `shade`.
fn scale(c: [u8; 3], k: f32) -> [u8; 3] {
    let f = |v: u8| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2])]
}

impl Clouds {
    /// The cloud field for `seed`. The same seed always gives the same sky.
    pub fn new(seed: u64) -> Clouds {
        // SplitMix64: a small, well-mixed generator; the top 24 bits of each
        // output become one lattice value in 0..1.
        let mut state = seed;
        let lattice = (0..TILE_W * TILE_H)
            .map(|_| {
                state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                z ^= z >> 31;
                (z >> 40) as f32 / (1u64 << 24) as f32
            })
            .collect();
        Clouds { lattice }
    }

    /// Drift speed in pixels per second for a total PCIe rate in bytes per
    /// second: [`DRIFT_FLOOR_PX_PER_S`] up to 1 kB/s, then rising with the
    /// base-10 logarithm of the rate to [`DRIFT_MAX_PX_PER_S`] at 10 GB/s,
    /// and no faster above it. `None`, NaN, infinity and negative rates give
    /// the floor: with no reading the clouds still move, slowly.
    ///
    /// A logarithm, because the rates that matter span seven decades: chips
    /// serving tokens on this box moved tens of MB/s, a weight load moves
    /// GB/s, an idle chip a few kB/s.
    pub fn drift_px_per_s(pcie_total_bps: Option<f64>) -> f32 {
        let Some(bps) = pcie_total_bps.filter(|b| b.is_finite() && *b > 0.0) else {
            return DRIFT_FLOOR_PX_PER_S;
        };
        let along = ((bps.log10() - DRIFT_FROM_LOG10) / (DRIFT_TO_LOG10 - DRIFT_FROM_LOG10))
            .clamp(0.0, 1.0) as f32;
        DRIFT_FLOOR_PX_PER_S + (DRIFT_MAX_PX_PER_S - DRIFT_FLOOR_PX_PER_S) * along
    }

    /// Brightness of the clouds for a chip load in 0..1: [`INTENSITY_MIN`] at
    /// load 0, rising linearly to [`INTENSITY_MAX`] at load 1. A load outside
    /// 0..1 is clamped, and NaN counts as 0, so a missing reading leaves dim
    /// clouds.
    pub fn intensity(load: f32) -> f32 {
        let load = if load.is_nan() {
            0.0
        } else {
            load.clamp(0.0, 1.0)
        };
        INTENSITY_MIN + (INTENSITY_MAX - INTENSITY_MIN) * load
    }

    /// Paint the clouds into `buf`, shifted right by `offset_px` pixels (the
    /// view passes [`CloudClock::offset_px`], a whole number), at
    /// `intensity` (see [`Clouds::intensity`]). Pixels of clear sky are not
    /// touched. The caller draws the scene over the result.
    ///
    /// The noise is evaluated once per 2 x 2 block of pixels, at the centre
    /// of the block's top-left pixel (`2 * bx + 0.5`, `2 * by + 0.5`), and
    /// each value covers its block: a block takes the value its first pixel
    /// would have on its own.
    pub fn draw(&self, buf: &mut PixelBuf, offset_px: f64, intensity: f32) {
        let intensity = if intensity.is_nan() {
            INTENSITY_MIN
        } else {
            intensity.clamp(0.0, 1.0)
        };
        // One colour per band: the cloud colour times the band's share times
        // the intensity. The thickest band at the top intensity (0.75) is
        // about as bright as an idle tile.
        let colours = BANDS.map(|(_, share)| scale(GRAYSKULL.cloud, share * intensity));
        let half_w = buf.w.div_ceil(2);
        // The band of each block in one row of blocks: 0 for clear sky, else
        // the band number plus one. A few hundred bytes per frame.
        let mut row = vec![0u8; half_w];
        for by in 0..buf.h.div_ceil(2) {
            let y = (2 * by) as f64 + 0.5;
            for (bx, band) in row.iter_mut().enumerate() {
                let x = (2 * bx) as f64 + 0.5;
                *band = band_of(self.density(x, y, offset_px));
            }
            for py in 2 * by..(2 * by + 2).min(buf.h) {
                for px in 0..buf.w {
                    let band = row[px / 2];
                    if band > 0 {
                        buf.put(px as i32, py as i32, colours[band as usize - 1]);
                    }
                }
            }
        }
    }

    /// The cloud density at pixel (`x`, `y`) with the field shifted right by
    /// `offset_px`: the octaves' weighted sum, in 0..1.
    fn density(&self, x: f64, y: f64, offset_px: f64) -> f32 {
        OCTAVES
            .iter()
            .map(|o| o.weight * self.octave(o, x, y, offset_px))
            .sum()
    }

    /// One octave's noise at pixel (`x`, `y`), in 0..1, with the field
    /// shifted right by `offset_px` times the octave's speed.
    fn octave(&self, o: &Octave, x: f64, y: f64, offset_px: f64) -> f32 {
        // Lattice coordinates, wrapped in f64 before narrowing so a view
        // that has run for days keeps its precision.
        let u = ((x - offset_px * o.speed) / o.spacing.0 + o.shift.0).rem_euclid(TILE_W as f64);
        let v = (y / o.spacing.1 + o.shift.1).rem_euclid(TILE_H as f64);
        self.sample(u, v)
    }

    /// The lattice at (`u`, `v`), each in `0..TILE_W` and `0..TILE_H`,
    /// bilinearly interpolated between the four nearest values, wrapping at
    /// the edges.
    fn sample(&self, u: f64, v: f64) -> f32 {
        let (i0, j0) = (u.floor() as usize % TILE_W, v.floor() as usize % TILE_H);
        let (i1, j1) = ((i0 + 1) % TILE_W, (j0 + 1) % TILE_H);
        let (fu, fv) = (u.fract() as f32, v.fract() as f32);
        let at = |i: usize, j: usize| self.lattice[j * TILE_W + i];
        let top = at(i0, j0) + (at(i1, j0) - at(i0, j0)) * fu;
        let bottom = at(i0, j1) + (at(i1, j1) - at(i0, j1)) * fu;
        top + (bottom - top) * fv
    }
}

/// The band of a density: 0 at or below [`DENSITY_THRESHOLD`] (clear sky),
/// else 1, 2 or 3 from thin to thick.
fn band_of(density: f32) -> u8 {
    if density <= DENSITY_THRESHOLD {
        return 0;
    }
    BANDS
        .iter()
        .position(|(upto, _)| density <= *upto)
        .map_or(BANDS.len() as u8, |i| i as u8 + 1)
}

/// The drift integrated over view time, on a clock that steps
/// [`CLOCK_STEPS_PER_SEC`] times a second, and the intensity shown.
///
/// The view keeps one and advances it with every `update` and `render`. The
/// position is the integral of the speed: each step adds the speed at that
/// moment times the step's length, so a change of speed changes how fast the
/// clouds go from where they are, and never moves them at once.
///
/// The picture changes only on a step, and then only when the integral
/// crosses a whole pixel ([`offset_px`](Self::offset_px)) or the intensity
/// moves by a step of 0.05: this is what keeps the clouds from repainting
/// cells every frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CloudClock {
    /// The last step reached (view time times the step rate, rounded down);
    /// `None` until the first call.
    step: Option<i64>,
    /// Pixels drifted so far, unrounded.
    drift_px: f64,
    /// The intensity shown; `None` until the first call.
    intensity: Option<f32>,
}

impl CloudClock {
    /// Move the clock to view time `now`. Each whole step passed since the
    /// last call adds `speed_px_per_s` for a quarter second to the drift, and
    /// takes up `intensity` if it differs from the one shown by more than
    /// three quarters of a 0.05 step. The first call only starts the clock.
    ///
    /// A `now` before the last step (a new clock) moves nothing: the clock
    /// restarts from there, so the clouds neither jump back nor wait for the
    /// old time to come round again. A `now` that is not finite, or too large
    /// to count in steps, is ignored.
    ///
    /// The restart has a cost when time only jitters: a `now` that steps
    /// back across a step boundary and then forward again counts that step
    /// twice, moving the clouds one step's drift (at most 2 px at the 8 px/s
    /// top speed) further than the time passed. The view's callers read one
    /// monotonic clock, so this does not happen in the TUI; it is left
    /// unguarded because guarding it would mean waiting out an old clock
    /// after a real restart.
    pub fn advance(&mut self, now: f64, speed_px_per_s: f32, intensity: f32) {
        let steps = now * CLOCK_STEPS_PER_SEC;
        if !steps.is_finite() || steps.abs() >= i64::MAX as f64 / 2.0 {
            return;
        }
        let step = steps.floor() as i64;
        match self.step {
            Some(last) if step > last => {
                let speed = if speed_px_per_s.is_finite() {
                    speed_px_per_s.max(0.0) as f64
                } else {
                    DRIFT_FLOOR_PX_PER_S as f64
                };
                self.drift_px += speed * (step - last) as f64 / CLOCK_STEPS_PER_SEC;
                self.take_intensity(intensity);
            }
            Some(_) => {}
            None => self.take_intensity(intensity),
        }
        self.step = Some(step);
    }

    /// Show `intensity`, rounded to a 0.05 step, unless the one shown is
    /// within the hysteresis of it.
    fn take_intensity(&mut self, intensity: f32) {
        let want = if intensity.is_nan() {
            INTENSITY_MIN
        } else {
            intensity.clamp(INTENSITY_MIN, INTENSITY_MAX)
        };
        let keep = self
            .intensity
            .is_some_and(|shown| (want - shown).abs() <= INTENSITY_HYSTERESIS);
        if !keep {
            self.intensity = Some((want * INTENSITY_STEPS).round() / INTENSITY_STEPS);
        }
    }

    /// The offset the picture is drawn at: the drift rounded down to a whole
    /// pixel.
    pub fn offset_px(&self) -> f64 {
        self.drift_px.floor()
    }

    /// The intensity the picture is drawn at; [`INTENSITY_MIN`] before the
    /// first call.
    pub fn intensity(&self) -> f32 {
        self.intensity.unwrap_or(INTENSITY_MIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Braille pixels of a 150 x 40 cell scene area.
    const W: usize = 300;
    const H: usize = 160;

    /// A clear buffer with the clouds of seed 7 drawn at `offset`.
    fn sky(offset: f64) -> PixelBuf {
        let mut buf = PixelBuf::new(W, H);
        Clouds::new(7).draw(&mut buf, offset, 0.5);
        buf
    }

    /// The share of `buf`'s pixels that `a` and `b`, compared at `a`'s
    /// (x, y) and `b`'s (x + dx, y), agree on, coverage and colour. Only the
    /// columns both buffers have are compared.
    fn agreement(a: &PixelBuf, b: &PixelBuf, dx: i64) -> f64 {
        let (mut same, mut n) = (0usize, 0usize);
        for y in 0..a.h {
            for x in 0..a.w {
                let bx = x as i64 + dx;
                if !(0..b.w as i64).contains(&bx) {
                    continue;
                }
                let bx = bx as usize;
                n += 1;
                same +=
                    (a.covered(x, y) == b.covered(bx, y) && a.get(x, y) == b.get(bx, y)) as usize;
            }
        }
        same as f64 / n as f64
    }

    #[test]
    fn clouds_are_deterministic_for_a_seed_and_time() {
        let (a, b) = (sky(37.0), sky(37.0));
        assert_eq!(a.rgb, b.rgb);
        assert_eq!(a.cover, b.cover);
        // Another seed is another sky.
        let mut other = PixelBuf::new(W, H);
        Clouds::new(8).draw(&mut other, 37.0, 0.5);
        assert_ne!(a.cover, other.cover);
    }

    #[test]
    fn clouds_drift_over_time() {
        // Driven by the clock at 5 px/s, as the view drives it.
        let offset_at = |t: f64| {
            let mut clock = CloudClock::default();
            clock.advance(0.0, 5.0, 0.5);
            clock.advance(t, 5.0, 0.5);
            clock.offset_px()
        };
        let start = sky(offset_at(0.0));
        assert_ne!(start.cover, sky(offset_at(10.0)).cover, "no motion in 10 s");
        let c = Clouds::new(7);
        for t in [2.0, 4.0, 10.0] {
            let d = offset_at(t);
            assert_eq!(d, 5.0 * t);
            // The cloud bank (the first octave) at t is the bank at 0
            // shifted right by exactly 5 t pixels; the detail (the second
            // octave) by half that.
            for (o, shift) in OCTAVES.iter().zip([d, d / 2.0]) {
                for y in (0..H).step_by(7) {
                    for x in (0..W).step_by(5) {
                        let (x, y) = (x as f64 + 0.5, y as f64 + 0.5);
                        let now = c.octave(o, x + shift, y, d);
                        let then = c.octave(o, x, y, 0.0);
                        assert!((now - then).abs() < 1e-4, "t {t}, ({x}, {y})");
                    }
                }
            }
            // The whole picture, the sum of the two, therefore matches the
            // old one best at a shift between the two octaves' shifts. (Not
            // at exactly 5 t: the detail moving at half speed changes the
            // shapes as they drift, as intended.)
            let now = sky(d);
            let best = (0..=60)
                .max_by(|&a, &b| agreement(&start, &now, a).total_cmp(&agreement(&start, &now, b)))
                .unwrap() as f64;
            assert!(
                (d / 2.0..=d).contains(&best) && best > 0.0,
                "at {t} s the best shift is {best} px, want {} to {d}",
                d / 2.0
            );
        }
    }

    #[test]
    fn drift_speed_has_a_floor_and_follows_the_log_of_pcie() {
        for bps in [
            None,
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(100.0),
            Some(-5.0),
        ] {
            assert_eq!(Clouds::drift_px_per_s(bps), 0.8, "{bps:?}");
        }
        assert_eq!(Clouds::drift_px_per_s(Some(1e3)), 0.8);
        let gb = Clouds::drift_px_per_s(Some(1e9));
        assert!(gb > 0.8 && gb < 8.0, "{gb}");
        assert!((Clouds::drift_px_per_s(Some(1e10)) - 8.0).abs() < 1e-5);
        assert_eq!(Clouds::drift_px_per_s(Some(1e12)), 8.0);
        // Measured on this box: chips serving tokens moved a mean of about
        // 23 MB/s and peaked near 131 MB/s. 100 MB/s is 5/7 of the way up
        // the log scale: 0.8 + 7.2 * 5 / 7 = 5.94 px/s.
        let serving = Clouds::drift_px_per_s(Some(1e8));
        assert!((serving - 5.943).abs() < 0.01, "{serving}");
        // Monotonic over the whole range.
        let mut prev = 0.0;
        for k in 0..=130 {
            let s = Clouds::drift_px_per_s(Some(10f64.powf(k as f64 / 10.0)));
            assert!(s >= prev, "at 1e{} B/s", k as f64 / 10.0);
            prev = s;
        }
    }

    #[test]
    fn intensity_follows_load_and_survives_nan() {
        assert_eq!(Clouds::intensity(0.0), 0.25);
        assert_eq!(Clouds::intensity(1.0), 0.75);
        assert!((Clouds::intensity(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(Clouds::intensity(f32::NAN), 0.25);
        assert_eq!(Clouds::intensity(-1.0), 0.25);
        assert_eq!(Clouds::intensity(7.0), 0.75);
        // Brighter clouds for a higher intensity, same shape.
        let mut dim = PixelBuf::new(W, H);
        let mut bright = PixelBuf::new(W, H);
        let c = Clouds::new(7);
        c.draw(&mut dim, 0.0, 0.25);
        c.draw(&mut bright, 0.0, 0.75);
        assert_eq!(dim.cover, bright.cover);
        let lum = |b: &PixelBuf| b.rgb.iter().map(|p| p[2] as u64).sum::<u64>();
        assert!(lum(&bright) > 2 * lum(&dim));
    }

    #[test]
    fn clear_sky_stays_uncovered_in_transparent_mode() {
        for offset in [0.0, 13.0, 400.0] {
            let buf = sky(offset);
            let clear = buf.cover.iter().filter(|c| !**c).count() as f64 / buf.cover.len() as f64;
            assert!((0.2..=0.7).contains(&clear), "{clear} clear at {offset}");
            // Every covered pixel is the cloud colour scaled down.
            for (i, c) in buf.rgb.iter().enumerate() {
                if buf.cover[i] {
                    assert!(c[2] > 0 && c[2] <= GRAYSKULL.cloud[2], "{c:?}");
                } else {
                    assert_eq!(*c, [0; 3]);
                }
            }
        }
    }

    #[test]
    fn the_clock_integrates_speed_in_whole_pixels_at_four_steps_a_second() {
        let mut clock = CloudClock::default();
        clock.advance(10.0, 3.0, 0.5);
        assert_eq!(clock.offset_px(), 0.0, "the first call only starts it");
        // Within one quarter second nothing moves, whatever the speed.
        clock.advance(10.2, 8.0, 0.5);
        assert_eq!(clock.offset_px(), 0.0);
        // Ten steps of 0.25 s at 1 px/s: 2.5 px, shown as 2.
        let mut t = 10.0;
        let mut seen = Vec::new();
        for _ in 0..40 {
            t += 1.0 / 40.0 * 2.5;
            clock.advance(t, 1.0, 0.5);
            seen.push(clock.offset_px());
        }
        assert!(seen.iter().all(|o| o.fract() == 0.0), "{seen:?}");
        assert_eq!(clock.offset_px(), 2.0);
        // A clock going backwards never moves the clouds back, nor forward
        // for the time it went back.
        let before = clock.offset_px();
        clock.advance(3.0, 8.0, 0.5);
        assert_eq!(clock.offset_px(), before);
        clock.advance(3.1, 8.0, 0.5);
        assert_eq!(clock.offset_px(), before);
        clock.advance(4.0, 8.0, 0.5);
        assert_eq!(clock.offset_px(), before + 8.0);
        // Nonsense times do not panic or move anything.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300] {
            let mut c = clock;
            c.advance(bad, 8.0, 0.5);
            assert!(c.offset_px().is_finite());
        }
    }

    #[test]
    fn the_shown_intensity_moves_in_steps_with_hysteresis() {
        let mut clock = CloudClock::default();
        clock.advance(0.0, 1.0, 0.5);
        assert_eq!(clock.intensity(), 0.5);
        // A load wobbling around the edge between two steps (0.525) does not
        // flip the shown intensity at every clock step.
        for (i, x) in [0.52, 0.53, 0.52, 0.53, 0.52].into_iter().enumerate() {
            clock.advance(0.25 * (i + 1) as f64, 1.0, x);
            assert_eq!(clock.intensity(), 0.5, "{x}");
        }
        // A real change is taken at the next step, rounded to 0.05.
        clock.advance(2.0, 1.0, 0.71);
        assert!(
            (clock.intensity() - 0.7).abs() < 1e-6,
            "{}",
            clock.intensity()
        );
        // Not between steps.
        clock.advance(2.1, 1.0, 0.3);
        assert!((clock.intensity() - 0.7).abs() < 1e-6);
    }
}
