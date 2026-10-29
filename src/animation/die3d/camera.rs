// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Camera presets, the automatic director and the "chaos" cut transition.

use super::raster::{Iso, PixelBuf};

/// Minimum seconds between two automatic cuts.
pub const COOLDOWN_SECS: f64 = 8.0;
/// Length of the chaos transition: rising to the midpoint, falling after it.
pub const CHAOS_SECS: f64 = 0.8;

/// How steeply the camera looks down: the tile diamond's `th / tw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pitch {
    /// th = 0.3 tw, a low grazing view.
    Low,
    /// th = 0.5 tw, the classic isometric view.
    Iso,
    /// th = 0.8 tw, nearly top-down.
    Top,
}

impl Pitch {
    fn ratio(self) -> f32 {
        match self {
            Pitch::Low => 0.3,
            Pitch::Iso => 0.5,
            Pitch::Top => 0.8,
        }
    }
}

/// One camera preset: a turn about the grid centre and a pitch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camera {
    pub quarter_turns: u8,
    pub pitch: Pitch,
}

/// The four turns at `Iso`, then a top-down and a low view.
pub const PRESETS: [Camera; 6] = [
    Camera {
        quarter_turns: 0,
        pitch: Pitch::Iso,
    },
    Camera {
        quarter_turns: 1,
        pitch: Pitch::Iso,
    },
    Camera {
        quarter_turns: 2,
        pitch: Pitch::Iso,
    },
    Camera {
        quarter_turns: 3,
        pitch: Pitch::Iso,
    },
    Camera {
        quarter_turns: 0,
        pitch: Pitch::Top,
    },
    Camera {
        quarter_turns: 0,
        pitch: Pitch::Low,
    },
];

/// Nearest even integer to `v`, at least 2.
fn even(v: f32) -> i32 {
    (((v / 2.0).round() as i32) * 2).max(2)
}

impl Camera {
    /// The projection for this camera, derived from `base` (the one `fit` or
    /// `fit_super` sized the buffer for).
    ///
    /// A pitch up to 0.5 keeps the tile width and flattens `th`; a steeper
    /// pitch keeps `th` and narrows `tw`. Either way no tile is wider or
    /// taller than in `base`, so the scene stays inside the fitted buffer
    /// (a taller `th` at the same `tw` would clip). The origin is moved so the
    /// grid centre lands on the same pixel as under `base`; `Iso::project`
    /// rotates about that centre, so this holds for every quarter turn.
    pub fn apply(&self, base: &Iso, grid: (usize, usize)) -> Iso {
        let ratio = self.pitch.ratio();
        let (tw, th) = if ratio <= 0.5 {
            (base.tw, even(base.tw as f32 * ratio))
        } else {
            (even(base.th as f32 / ratio), base.th)
        };
        let mut iso = Iso {
            tw,
            th,
            quarter_turns: self.quarter_turns,
            ..*base
        };
        // The centre is a fixed point of the rotation, so its offset from the
        // origin is (cx - cy) tw/2 and (cx + cy) th/2 whatever the turn.
        let cx = (grid.0 as f32 - 1.0) / 2.0;
        let cy = (grid.1 as f32 - 1.0) / 2.0;
        let centre = |i: &Iso| ((cx - cy) * i.tw as f32 / 2.0, (cx + cy) * i.th as f32 / 2.0);
        let (bx, by) = centre(base);
        let (nx, ny) = centre(&iso);
        iso.ox = base.ox + (bx - nx).round() as i32;
        iso.oy = base.oy + (by - ny).round() as i32;
        iso
    }
}

/// Small xorshift64*: enough for picking a camera and noise, no crate needed.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        // xorshift never leaves state 0, so mix the seed and keep it non-zero.
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
}

/// Decides when the camera cuts to another preset.
///
/// Time is passed in (`now`, seconds since the view was created); the director
/// never reads a clock, so tests and replays are exact. A cut chooses the next
/// camera at once, but the caller keeps drawing the old one until
/// [`Director::swapped_at`], while the chaos effect covers the change.
#[derive(Debug, Clone)]
pub struct Director {
    rng: Rng,
    /// Camera shown before the swap of the latest cut.
    from: Camera,
    /// Camera shown after it.
    to: Camera,
    auto: bool,
    /// Start of the latest cut; none yet means the first trigger is allowed.
    last_cut: Option<f64>,
}

impl Director {
    pub fn new(seed: u64) -> Director {
        Director {
            rng: Rng::new(seed),
            from: PRESETS[0],
            to: PRESETS[0],
            auto: true,
            last_cut: None,
        }
    }

    /// The camera after the latest cut (the one in force once it has swapped).
    pub fn camera(&self) -> Camera {
        self.to
    }

    /// The camera to draw at `now`: the old one until the swap, then the new.
    pub fn camera_at(&self, now: f64) -> Camera {
        match self.swapped_at() {
            Some(at) if now < at => self.from,
            _ => self.to,
        }
    }

    pub fn auto(&self) -> bool {
        self.auto
    }

    pub fn set_auto(&mut self, on: bool) {
        self.auto = on;
    }

    /// A cut asked for by the user: ignores the cooldown and the auto setting,
    /// but starts the cooldown. Always returns true.
    pub fn cut_by_hand(&mut self, now: f64) -> bool {
        self.cut(now);
        true
    }

    /// A cut asked for by an event: only when auto is on and at least
    /// [`COOLDOWN_SECS`] have passed since the last cut.
    pub fn on_trigger(&mut self, now: f64) -> bool {
        let ready = self.last_cut.map_or(true, |t| now - t >= COOLDOWN_SECS);
        if self.auto && ready {
            self.cut(now);
            true
        } else {
            false
        }
    }

    fn cut(&mut self, now: f64) {
        // Pick among the other five presets, so the camera always changes.
        let here = PRESETS.iter().position(|c| *c == self.to).unwrap_or(0);
        let pick = self.rng.next() as usize % (PRESETS.len() - 1);
        self.from = self.to;
        self.to = PRESETS[(here + 1 + pick) % PRESETS.len()];
        self.last_cut = Some(now);
    }

    /// Chaos intensity 0..=1 at `now`: rising for the first half of
    /// [`CHAOS_SECS`], falling for the second, `None` outside a transition.
    pub fn chaos(&self, now: f64) -> Option<f32> {
        let p = (now - self.last_cut?) / CHAOS_SECS;
        if !(0.0..1.0).contains(&p) {
            return None;
        }
        Some((1.0 - (2.0 * p - 1.0).abs()) as f32)
    }

    /// When the latest cut swaps cameras (its midpoint); none before any cut.
    pub fn swapped_at(&self) -> Option<f64> {
        self.last_cut.map(|t| t + CHAOS_SECS / 2.0)
    }
}

/// The chaos effect: shift whole rows sideways and add colour noise, both
/// scaled by `intensity` (0..=1). At 0 the buffer is untouched; at 1 nearly
/// every pixel changes. Deterministic for a `seed`.
pub fn scramble(buf: &mut PixelBuf, intensity: f32, seed: u32) {
    let k = intensity.clamp(0.0, 1.0);
    if k <= 0.0 || buf.w == 0 {
        return;
    }
    let mut rng = Rng::new(seed as u64);
    let max_shift = (k * buf.w as f32 / 6.0) as i64;
    let amp = 80.0 * k;
    for y in 0..buf.h {
        let row = &mut buf.rgb[y * buf.w..(y + 1) * buf.w];
        if max_shift > 0 {
            let shift = (rng.next() as i64 % (2 * max_shift + 1)) - max_shift;
            row.rotate_left(shift.rem_euclid(buf.w as i64) as usize);
        }
        for px in row.iter_mut() {
            for c in px.iter_mut() {
                let noise = ((rng.next() % 161) as f32 - 80.0) / 80.0 * amp;
                *c = (*c as f32 + noise).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::die3d::raster::PixelBuf;
    use crate::animation::die3d::scene::{fit, painted_bbox_for, ChipInputs};
    use crate::models::Architecture;

    #[test]
    fn an_automatic_cut_waits_out_the_cooldown() {
        let mut d = Director::new(1);
        assert!(d.on_trigger(1.0));
        assert!(!d.on_trigger(1.0 + 7.9));
        assert!(d.on_trigger(1.0 + 8.0));
    }

    #[test]
    fn auto_off_ignores_triggers_but_not_hand_cuts() {
        let mut d = Director::new(2);
        d.set_auto(false);
        assert!(!d.auto());
        assert!(!d.on_trigger(100.0));
        assert!(d.swapped_at().is_none());
        assert!(d.cut_by_hand(100.0));
        // The hand cut started the cooldown, so even with auto back on a
        // trigger right after is refused.
        d.set_auto(true);
        assert!(!d.on_trigger(101.0));
        // A hand cut inside the cooldown still goes through.
        assert!(d.cut_by_hand(101.0));
    }

    #[test]
    fn a_cut_never_picks_the_same_camera() {
        for seed in 0..20u64 {
            let mut d = Director::new(seed);
            for i in 0..50 {
                let before = d.camera();
                assert!(d.cut_by_hand(i as f64));
                assert_ne!(d.camera(), before, "seed {seed} cut {i}");
            }
        }
    }

    #[test]
    fn chaos_peaks_at_the_midpoint_and_ends_at_none() {
        let mut d = Director::new(3);
        assert!(d.chaos(0.0).is_none());
        d.cut_by_hand(10.0);
        let peak = d.chaos(10.0 + CHAOS_SECS / 2.0).unwrap();
        assert!((peak - 1.0).abs() < 0.05, "peak {peak}");
        let early = d.chaos(10.0 + CHAOS_SECS / 8.0).unwrap();
        let late = d.chaos(10.0 + CHAOS_SECS * 7.0 / 8.0).unwrap();
        assert!(early < 0.6 && late < 0.6, "{early} {late}");
        assert!(d.chaos(10.0 + CHAOS_SECS).is_none());
        assert!(d.chaos(9.0).is_none());
    }

    #[test]
    fn the_camera_swaps_at_the_midpoint() {
        let mut d = Director::new(4);
        let old = d.camera_at(0.0);
        d.cut_by_hand(10.0);
        let at = d.swapped_at().unwrap();
        assert!((at - (10.0 + CHAOS_SECS / 2.0)).abs() < 1e-9);
        assert_eq!(d.camera_at(at - 0.01), old);
        assert_eq!(d.camera_at(at), d.camera());
        assert_ne!(d.camera(), old);
    }

    /// A mid-grey gradient so row shifts and colour noise both show.
    fn gradient() -> PixelBuf {
        let mut b = PixelBuf::new(64, 32);
        for y in 0..32 {
            for x in 0..64 {
                b.put(x, y, [100 + (x % 7) as u8, 100 + (y % 5) as u8, 100]);
            }
        }
        b
    }

    #[test]
    fn scramble_at_zero_leaves_the_buffer_unchanged() {
        let mut b = gradient();
        scramble(&mut b, 0.0, 9);
        assert_eq!(b.rgb, gradient().rgb);
    }

    #[test]
    fn scramble_at_one_changes_most_pixels() {
        let mut b = gradient();
        scramble(&mut b, 1.0, 9);
        let same = b
            .rgb
            .iter()
            .zip(gradient().rgb.iter())
            .filter(|(a, c)| a == c)
            .count();
        assert!(
            same * 4 < b.rgb.len(),
            "{same} of {} unchanged",
            b.rgb.len()
        );
    }

    #[test]
    fn scramble_is_deterministic_for_a_seed() {
        let (mut a, mut b, mut c) = (gradient(), gradient(), gradient());
        scramble(&mut a, 0.7, 5);
        scramble(&mut b, 0.7, 5);
        scramble(&mut c, 0.7, 6);
        assert_eq!(a.rgb, b.rgb);
        assert_ne!(a.rgb, c.rgb);
    }

    fn chip() -> ChipInputs {
        ChipInputs {
            index: 0,
            arch: Architecture::Blackhole,
            load: 1.0,
            pcie_in_bps: Some(40.0e9),
            pcie_out_bps: Some(40.0e9),
            gddr_heat: Some(1.0),
            eth_live: u64::MAX,
            temp_c: None,
            enabled_tensix_col: None,
            tile_flow: None,
        }
    }

    #[test]
    fn every_preset_stays_inside_the_fitted_buffer() {
        let cells = (150usize, 42usize);
        for dies in [1usize, 2] {
            let chips: Vec<ChipInputs> = (0..dies).map(|_| chip()).collect();
            let base = fit(cells.0, cells.1, dies).unwrap();
            let grid = (
                crate::animation::die3d::scene::BOARD_COLS,
                crate::animation::die3d::scene::BOARD_BAND * dies,
            );
            for (i, cam) in PRESETS.iter().enumerate() {
                let mut f = base;
                f.iso = cam.apply(&base.iso, grid);
                let (x0, y0, x1, y1) = painted_bbox_for(&chips, cells, &f);
                assert!(
                    x1 <= cells.0 * 2 && y1 <= cells.1 * 4,
                    "dies={dies} preset={i}: {x0},{y0}..{x1},{y1}"
                );
                // Not vacuous: something big was painted.
                assert!(x1 - x0 > 100 && y1 - y0 > 40, "dies={dies} preset={i}");
            }
        }
    }

    #[test]
    fn apply_keeps_the_projected_grid_centre() {
        let base = fit(150, 42, 2).unwrap().iso;
        let grid = (19usize, 28usize);
        let (cx, cy) = ((grid.0 as f32 - 1.0) / 2.0, (grid.1 as f32 - 1.0) / 2.0);
        let want = base.project(cx, cy, grid);
        for cam in PRESETS.iter() {
            let iso = cam.apply(&base, grid);
            assert_eq!(iso.quarter_turns, cam.quarter_turns);
            let got = iso.project(cx, cy, grid);
            assert!(
                (got.0 - want.0).abs() <= 1 && (got.1 - want.1).abs() <= 1,
                "{got:?} vs {want:?}"
            );
            assert!(iso.tw >= 2 && iso.th >= 2 && iso.tw % 2 == 0 && iso.th % 2 == 0);
        }
    }

    #[test]
    fn pitch_ratios_follow_the_brief() {
        let base = fit(150, 42, 1).unwrap().iso;
        let grid = (19usize, 14usize);
        let low = Camera {
            quarter_turns: 0,
            pitch: Pitch::Low,
        }
        .apply(&base, grid);
        let top = Camera {
            quarter_turns: 0,
            pitch: Pitch::Top,
        }
        .apply(&base, grid);
        let iso = Camera {
            quarter_turns: 0,
            pitch: Pitch::Iso,
        }
        .apply(&base, grid);
        assert_eq!(low.tw, base.tw);
        assert!(low.th < iso.th && iso.th <= top.th);
        assert_eq!(top.th, base.th);
        assert!(top.tw < base.tw);
    }
}
