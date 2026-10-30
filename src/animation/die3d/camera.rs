// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Camera presets, the automatic director and the cut transitions: chaos
//! noise, a dissolve and an animated spin.

use super::raster::{Iso, PixelBuf};
use crate::animation::common::ease_in_out;
use std::f32::consts::FRAC_PI_2;

/// Minimum seconds between two automatic cuts.
pub const COOLDOWN_SECS: f64 = 8.0;
/// Length of the chaos transition: rising to the midpoint, falling after it.
pub const CHAOS_SECS: f64 = 0.8;
/// Length of the dissolve transition.
pub const DISSOLVE_SECS: f64 = 0.9;
/// Length of the spin transition. Longer than the others: the camera visibly
/// turns, and a quick turn would look like a jump.
pub const SPIN_SECS: f64 = 1.6;

/// How a cut changes from the old camera to the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    /// Rows shift and colours flicker, rising to the midpoint, where the
    /// camera swaps, and falling after it (see [`scramble`]).
    Chaos,
    /// The new picture replaces the old one pixel by pixel in a random
    /// order (see [`dissolve`]).
    Dissolve,
    /// The camera turns from the old angle to the new one (see
    /// [`Camera::lerp_view`]). Only for a cut that keeps the scene: there is
    /// nothing to turn between two different scenes.
    Spin,
}

impl TransitionKind {
    /// How long the transition runs, in seconds.
    pub fn secs(self) -> f64 {
        match self {
            TransitionKind::Chaos => CHAOS_SECS,
            TransitionKind::Dissolve => DISSOLVE_SECS,
            TransitionKind::Spin => SPIN_SECS,
        }
    }
}

/// Shuffle weights: how often each kind is drawn, out of their sum, before
/// the kinds a cut cannot use are left out.
const SHUFFLE_WEIGHTS: [(TransitionKind, u32); 3] = [
    (TransitionKind::Chaos, 40),
    (TransitionKind::Dissolve, 35),
    (TransitionKind::Spin, 25),
];

/// Which transitions the director uses. The view's `X` key cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransitionMode {
    /// A weighted draw for each cut, never the same kind twice in a row.
    #[default]
    Shuffle,
    /// Always chaos.
    Chaos,
    /// Always a dissolve.
    Dissolve,
    /// Always a spin, except that a cut that changes the scene dissolves.
    Spin,
}

impl TransitionMode {
    /// The mode after this one, in the order `X` steps through them.
    pub fn next(self) -> TransitionMode {
        match self {
            TransitionMode::Shuffle => TransitionMode::Chaos,
            TransitionMode::Chaos => TransitionMode::Dissolve,
            TransitionMode::Dissolve => TransitionMode::Spin,
            TransitionMode::Spin => TransitionMode::Shuffle,
        }
    }

    /// The footer's word for the mode.
    pub fn label(self) -> &'static str {
        match self {
            TransitionMode::Shuffle => "shuffle",
            TransitionMode::Chaos => "chaos",
            TransitionMode::Dissolve => "dissolve",
            TransitionMode::Spin => "spin",
        }
    }
}

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

/// Move `iso`'s origin so the grid centre lands on the pixel `base` puts it.
///
/// The centre is a fixed point of the rotation (quarter turns and spin
/// alike), so its offset from the origin is (cx - cy) tw/2 and (cx + cy) th/2
/// whatever the angle; only `tw` and `th` move it.
fn recentre(iso: &mut Iso, base: &Iso, grid: (usize, usize)) {
    let cx = (grid.0 as f32 - 1.0) / 2.0;
    let cy = (grid.1 as f32 - 1.0) / 2.0;
    let centre = |i: &Iso| ((cx - cy) * i.tw as f32 / 2.0, (cx + cy) * i.th as f32 / 2.0);
    let (bx, by) = centre(base);
    let (nx, ny) = centre(iso);
    iso.ox = base.ox + (bx - nx).round() as i32;
    iso.oy = base.oy + (by - ny).round() as i32;
}

impl Camera {
    /// The projection for this camera, derived from `base` (the one `fit` or
    /// `fit_super` sized the buffer for).
    ///
    /// `Iso` pitch is the base geometry as it is: same `tw`, `th`, `ox`, `oy`,
    /// only the turn changes. `fit` may produce an odd `th` (tw 14 gives 7), so
    /// rounding it to an even number would make the tiles taller than the
    /// buffer was sized for. `Low` keeps `tw` and takes a flatter `th`, never
    /// above `base.th`. `Top` keeps `th` and narrows `tw`. So no preset has a
    /// taller or wider tile than `base`, and the scene stays inside the fitted
    /// buffer. Where the geometry changes the origin is moved so the grid
    /// centre lands on the same pixel as under `base`; `Iso::project` rotates
    /// about that centre, so this holds for every quarter turn.
    pub fn apply(&self, base: &Iso, grid: (usize, usize)) -> Iso {
        let ratio = self.pitch.ratio();
        if ratio == 0.5 {
            return Iso {
                quarter_turns: self.quarter_turns,
                ..*base
            };
        }
        let (tw, th) = if ratio < 0.5 {
            (base.tw, even(base.tw as f32 * ratio).min(base.th).max(2))
        } else {
            (even(base.th as f32 / ratio), base.th)
        };
        let mut iso = Iso {
            tw,
            th,
            quarter_turns: self.quarter_turns,
            ..*base
        };
        recentre(&mut iso, base, grid);
        iso
    }

    /// The projection part way from camera `from` to camera `to`, for the
    /// spin transition. `t` runs 0..1 and is clamped, then eased with
    /// [`ease_in_out`] so the turn starts and stops gently.
    ///
    /// The yaw goes the shortest way round: one quarter turn back from turn 0
    /// to turn 3, not three forward; a half turn goes the positive way. It is
    /// carried as `from`'s `quarter_turns` plus a `spin`. The tile width and
    /// height (so the pitch ratio too) move linearly between `from.apply` and
    /// `to.apply`; the origin is then placed so the projected grid centre
    /// stays on the same pixel, which moves `ox` and `oy` linearly as well
    /// (up to rounding). Both endpoints are at most the base size, so every
    /// step between them is too: the base is the size the scene was fitted
    /// to the view at, so no frame of the turn can outgrow the view. At the
    /// ends the result is exactly `from.apply` and `to.apply`, with no spin
    /// left over.
    pub fn lerp_view(from: &Camera, to: &Camera, t: f32, base: &Iso, grid: (usize, usize)) -> Iso {
        let a = from.apply(base, grid);
        let b = to.apply(base, grid);
        let e = ease_in_out(t);
        if e <= 0.0 {
            return a;
        }
        if e >= 1.0 {
            return b;
        }
        let turns = match (to.quarter_turns as i32 - from.quarter_turns as i32).rem_euclid(4) {
            3 => -1,
            d => d,
        };
        let lerp = |x: i32, y: i32| (x as f32 + (y - x) as f32 * e).round() as i32;
        let mut iso = Iso {
            tw: lerp(a.tw, b.tw),
            th: lerp(a.th, b.th),
            quarter_turns: a.quarter_turns,
            spin: turns as f32 * e * FRAC_PI_2,
            ..a
        };
        recentre(&mut iso, base, grid);
        iso
    }

    /// The board corner nearest the viewer, for the footer caption: plain
    /// text such as `"front-right"`, or `"top-down, front-right"` for the
    /// `Top` pitch.
    ///
    /// "Front" is the PCIe edge side of the board (grid y high, die row 12),
    /// "back" the Ethernet edge side; "right" is the GDDR edge on the grid x
    /// high side, "left" the one on the low side. The nearest corner is the
    /// one that projects lowest on screen: the largest rotated `gx + gy`.
    /// That corner is unique on any board shape, square or not, so the name
    /// says where the viewer stands. At turn 0 it is front-right, and each
    /// quarter turn brings the next corner round: back-right, back-left,
    /// front-left. The test `side_names_name_the_corner_nearest_the_viewer`
    /// derives this from `Iso::project` on three grid shapes. The pitch only
    /// changes how steeply the camera looks down, so every pitch shares the
    /// corner names.
    pub fn side_name(&self) -> &'static str {
        let corner = self.quarter_turns % 4;
        match (self.pitch, corner) {
            (Pitch::Top, 0) => "top-down, front-right",
            (Pitch::Top, 1) => "top-down, back-right",
            (Pitch::Top, 2) => "top-down, back-left",
            (Pitch::Top, _) => "top-down, front-left",
            (_, 0) => "front-right",
            (_, 1) => "back-right",
            (_, 2) => "back-left",
            _ => "front-left",
        }
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

/// Decides when the camera cuts to another preset, and how the cut looks.
///
/// Time is passed in (`now`, seconds since the view was created); the director
/// never reads a clock, so tests and replays are exact. A cut chooses the next
/// camera and the transition kind at once, but the caller keeps drawing the
/// old camera until [`Director::swapped_at`]: the midpoint for chaos and
/// dissolve, the end for a spin (whose in-between angles the caller derives
/// from [`Director::progress`] with [`Camera::lerp_view`]).
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
    mode: TransitionMode,
    /// Transition of the latest cut; none before the first.
    kind: Option<TransitionKind>,
}

impl Director {
    pub fn new(seed: u64) -> Director {
        Director {
            rng: Rng::new(seed),
            from: PRESETS[0],
            to: PRESETS[0],
            auto: true,
            last_cut: None,
            mode: TransitionMode::Shuffle,
            kind: None,
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

    pub fn transition_mode(&self) -> TransitionMode {
        self.mode
    }

    /// Choose the transitions of later cuts; a cut in progress keeps its own.
    pub fn set_transition_mode(&mut self, m: TransitionMode) {
        self.mode = m;
    }

    /// Step to the next mode: shuffle, chaos, dissolve, spin, shuffle.
    pub fn cycle_transition_mode(&mut self) {
        self.mode = self.mode.next();
    }

    /// The transition of the latest cut, which is the one in progress while
    /// [`progress`](Self::progress) is `Some`. `None` before any cut.
    pub fn transition_kind(&self) -> Option<TransitionKind> {
        self.kind
    }

    /// The two cameras of the latest cut: the one shown before it and the one
    /// shown after it.
    pub fn cameras(&self) -> (Camera, Camera) {
        (self.from, self.to)
    }

    /// When the latest cut started; `None` before any cut.
    pub fn cut_at(&self) -> Option<f64> {
        self.last_cut
    }

    /// A cut asked for by the user that changes the camera only: see
    /// [`cut_by_hand_with_scene`](Self::cut_by_hand_with_scene).
    pub fn cut_by_hand(&mut self, now: f64) -> bool {
        self.cut_by_hand_with_scene(now, false)
    }

    /// A cut asked for by the user: ignores the cooldown and the auto setting,
    /// but starts the cooldown. Always returns true. `changes_scene` says the
    /// scene will change at this cut's swap, as when the cut takes over a
    /// scene switch still waiting for the midpoint of the cut it replaces;
    /// then the transition must be one that can cover a scene change (not a
    /// spin), as for [`on_trigger_with_scene`](Self::on_trigger_with_scene).
    pub fn cut_by_hand_with_scene(&mut self, now: f64, changes_scene: bool) -> bool {
        self.cut(now, changes_scene);
        true
    }

    /// A cut asked for by an event that changes the camera only: see
    /// [`on_trigger_with_scene`](Self::on_trigger_with_scene).
    pub fn on_trigger(&mut self, now: f64) -> bool {
        self.on_trigger_with_scene(now, false)
    }

    /// A cut asked for by an event: only when auto is on and at least
    /// [`COOLDOWN_SECS`] have passed since the last cut. `changes_scene` says
    /// the caller will switch scenes at the cut's swap if it goes through, so
    /// the transition must be one that can cover a scene change (not a spin).
    pub fn on_trigger_with_scene(&mut self, now: f64, changes_scene: bool) -> bool {
        let ready = self.last_cut.map_or(true, |t| now - t >= COOLDOWN_SECS);
        if self.auto && ready {
            self.cut(now, changes_scene);
            true
        } else {
            false
        }
    }

    fn cut(&mut self, now: f64, changes_scene: bool) {
        // Pick among the other five presets, so the camera always changes.
        let here = PRESETS.iter().position(|c| *c == self.to).unwrap_or(0);
        let pick = self.rng.next() as usize % (PRESETS.len() - 1);
        self.from = self.to;
        self.to = PRESETS[(here + 1 + pick) % PRESETS.len()];
        self.last_cut = Some(now);
        self.kind = Some(self.pick_kind(changes_scene));
    }

    /// The transition for a new cut. A spin turns one scene's camera, so a
    /// cut that changes the scene never spins: the fixed `Spin` mode falls
    /// back to a dissolve, and the shuffle leaves `Spin` out of the draw.
    fn pick_kind(&mut self, changes_scene: bool) -> TransitionKind {
        match self.mode {
            TransitionMode::Chaos => TransitionKind::Chaos,
            TransitionMode::Dissolve => TransitionKind::Dissolve,
            TransitionMode::Spin if changes_scene => TransitionKind::Dissolve,
            TransitionMode::Spin => TransitionKind::Spin,
            TransitionMode::Shuffle => {
                // Never the previous kind again. At most two kinds are left
                // out (the previous one and Spin), so one is always left.
                let options: Vec<(TransitionKind, u32)> = SHUFFLE_WEIGHTS
                    .iter()
                    .copied()
                    .filter(|&(k, _)| Some(k) != self.kind)
                    .filter(|&(k, _)| !(changes_scene && k == TransitionKind::Spin))
                    .collect();
                let total: u32 = options.iter().map(|&(_, w)| w).sum();
                let mut r = self.rng.next() % total;
                for &(k, w) in &options {
                    if r < w {
                        return k;
                    }
                    r -= w;
                }
                unreachable!("r < total, so some option takes it")
            }
        }
    }

    /// How far the latest cut's transition has run at `now`: 0 at its start,
    /// rising to just under 1 at its end, whatever its kind. `None` before
    /// the first cut and outside the transition.
    pub fn progress(&self, now: f64) -> Option<f32> {
        let (start, secs) = (self.last_cut?, self.kind?.secs());
        // The end is tested as `now >= start + secs`, the same sum
        // `swapped_at` gives a spin, so "progress has run out" and "the spin's
        // camera has swapped" agree to the last bit of the float.
        if now < start || now >= start + secs {
            return None;
        }
        let p = (now - start) / secs;
        // A p just under 1 can round up to 1.0 as an f32; keep it below 1 so
        // "in progress" and "p < 1" stay the same thing.
        Some((p as f32).min(1.0 - f32::EPSILON))
    }

    /// Chaos intensity 0..=1 at `now`: rising for the first half of
    /// [`CHAOS_SECS`], falling for the second. `None` outside a transition
    /// and when the transition in progress is not chaos.
    pub fn chaos(&self, now: f64) -> Option<f32> {
        if self.kind != Some(TransitionKind::Chaos) {
            return None;
        }
        let p = (now - self.last_cut?) / CHAOS_SECS;
        if !(0.0..1.0).contains(&p) {
            return None;
        }
        Some((1.0 - (2.0 * p - 1.0).abs()) as f32)
    }

    /// When the latest cut swaps cameras: its midpoint for chaos and
    /// dissolve, its end for a spin (which turns the old camera all the way
    /// to the new one first). `None` before any cut.
    pub fn swapped_at(&self) -> Option<f64> {
        let (t, kind) = (self.last_cut?, self.kind?);
        Some(match kind {
            TransitionKind::Spin => t + kind.secs(),
            _ => t + kind.secs() / 2.0,
        })
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
        let span = y * buf.w..(y + 1) * buf.w;
        let row = &mut buf.rgb[span.clone()];
        let cover = &mut buf.cover[span];
        if max_shift > 0 {
            let shift = (rng.next() as i64 % (2 * max_shift + 1)) - max_shift;
            let by = shift.rem_euclid(buf.w as i64) as usize;
            // The mask moves with the pixels it describes.
            row.rotate_left(by);
            cover.rotate_left(by);
        }
        for (px, &covered) in row.iter_mut().zip(cover.iter()) {
            // Noise colours painted pixels only; an uncovered one stays
            // transparent and black.
            if !covered {
                continue;
            }
            for c in px.iter_mut() {
                let noise = ((rng.next() % 161) as f32 - 80.0) / 80.0 * amp;
                *c = (*c as f32 + noise).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// A per-pixel value in `[0, 1)` from `x`, `y` and `seed`: an integer hash
/// (the murmur3 finaliser over a mix of the three), so the order in which a
/// dissolve turns pixels looks random but is the same on every frame.
fn pixel_rank(x: usize, y: usize, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1)
        ^ (y as u32).wrapping_mul(0x85EB_CA77)
        ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    // The top 24 bits fit an f32 exactly, so the result is never 1.0.
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// The dissolve: each pixel, colour and coverage alike, comes from `new`
/// where its rank (see `pixel_rank`) is below `progress`, else from `old`. At
/// 0 the result is `old`, at 1 it is `new`, and a pixel that has turned stays
/// turned as `progress` grows. Deterministic for a `seed`.
///
/// Panics if the two buffers differ in size: the view draws both frames into
/// one area, so they never do.
pub fn dissolve(old: &PixelBuf, new: &PixelBuf, progress: f32, seed: u32) -> PixelBuf {
    let mut out = old.clone();
    dissolve_into(&mut out, new, progress, seed);
    out
}

/// [`dissolve`] written over `old` itself, for the view, which has no further
/// use for the old frame and so saves copying it.
pub fn dissolve_into(old: &mut PixelBuf, new: &PixelBuf, progress: f32, seed: u32) {
    assert_eq!(
        (old.w, old.h),
        (new.w, new.h),
        "dissolve needs two frames of one size"
    );
    for y in 0..old.h {
        for x in 0..old.w {
            if pixel_rank(x, y, seed) < progress {
                let i = y * old.w + x;
                old.rgb[i] = new.rgb[i];
                old.cover[i] = new.cover[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::die3d::raster::PixelBuf;
    use crate::animation::die3d::scene::{
        fit, painted_bbox_for, ChipInputs, SceneFit, BOARD_BAND, BOARD_COLS,
    };
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
        d.set_transition_mode(TransitionMode::Chaos);
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
        d.set_transition_mode(TransitionMode::Chaos);
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
            label: "BH0".to_string(),
            power_w: Some(150.0),
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
            let grid = (BOARD_COLS, BOARD_BAND * dies);
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
            assert!(iso.tw >= 2 && iso.th >= 2);
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

    #[test]
    fn iso_pitch_is_an_identity_for_an_odd_base_th() {
        let base = Iso {
            tw: 14,
            th: 7,
            ox: 123,
            oy: 45,
            quarter_turns: 0,
            spin: 0.0,
        };
        for q in 0..4 {
            let iso = Camera {
                quarter_turns: q,
                pitch: Pitch::Iso,
            }
            .apply(&base, (19, 28));
            assert_eq!((iso.tw, iso.th, iso.ox, iso.oy), (14, 7, 123, 45));
            assert_eq!(iso.quarter_turns, q);
        }
    }

    #[test]
    fn no_preset_is_taller_than_the_base() {
        for tw in [8, 10, 12, 14, 18, 22, 30] {
            let base = Iso {
                tw,
                th: tw / 2,
                ox: 0,
                oy: 0,
                quarter_turns: 0,
                spin: 0.0,
            };
            for cam in PRESETS.iter() {
                let iso = cam.apply(&base, (19, 14));
                assert!(iso.th <= base.th, "tw={tw} {cam:?}: th {}", iso.th);
                assert!(iso.tw <= base.tw, "tw={tw} {cam:?}: tw {}", iso.tw);
            }
        }
    }

    #[test]
    fn every_preset_fits_a_base_with_tw_two_mod_four() {
        let chips = [chip()];
        let grid = (BOARD_COLS, BOARD_BAND);
        // A base like `fit` could pick on a tightly packed terminal: odd th.
        let mut base = SceneFit {
            iso: Iso {
                tw: 14,
                th: 7,
                ox: 150,
                oy: 90,
                quarter_turns: 0,
                spin: 0.0,
            },
            scale: 2,
        };
        // Measure the base scene in a roomy buffer, then size the terminal
        // area to just hold it (cells are 2 x 4 pixels, so slack is at most
        // 1 and 3 pixels). A taller `th` would then clip; a roomy buffer
        // would hide that.
        let (x0, y0, x1, y1) = painted_bbox_for(&chips, (150, 42), &base);
        let cells = ((x1 - x0).div_ceil(2), (y1 - y0).div_ceil(4));
        base.iso.ox += (cells.0 as i32 * 2 - (x1 - x0) as i32) / 2 - x0 as i32;
        base.iso.oy += (cells.1 as i32 * 4 - (y1 - y0) as i32) / 2 - y0 as i32;
        let (_, _, x1, y1) = painted_bbox_for(&chips, cells, &base);
        assert!(x1 <= cells.0 * 2 && y1 <= cells.1 * 4, "base: {x1},{y1}");
        for (i, cam) in PRESETS.iter().enumerate() {
            let mut f = base;
            f.iso = cam.apply(&base.iso, grid);
            let (_, _, x1, y1) = painted_bbox_for(&chips, cells, &f);
            assert!(
                x1 <= cells.0 * 2 && y1 <= cells.1 * 4,
                "preset {i}: {x1},{y1}"
            );
        }
    }

    #[test]
    fn scramble_moves_coverage_with_the_pixels_and_never_colours_an_uncovered_one() {
        // Cover only the left third of each row; mark covered pixels by blue 200.
        let mut b = PixelBuf::new(64, 32);
        for y in 0..32 {
            for x in 0..21 {
                b.put(x, y, [100, 100, 200]);
            }
        }
        let uncovered_rgb = [0u8; 3];
        let before = b.cover.iter().filter(|&&c| c).count();
        scramble(&mut b, 1.0, 11);
        // Row shifts only permute a row, so the covered count is unchanged ...
        assert_eq!(b.cover.iter().filter(|&&c| c).count(), before);
        // ... the mask actually moved in some row ...
        assert!((0..32).any(|y| !(0..21).all(|x| b.covered(x, y))));
        // ... and noise reached covered pixels only.
        for i in 0..b.rgb.len() {
            if !b.cover[i] {
                assert_eq!(b.rgb[i], uncovered_rgb, "pixel {i}");
            }
        }
        assert!(b
            .rgb
            .iter()
            .zip(&b.cover)
            .any(|(p, &c)| c && *p != [100, 100, 200]));
    }

    /// Every ordered pair of presets, the same one twice included.
    fn preset_pairs() -> impl Iterator<Item = (Camera, Camera)> {
        PRESETS
            .iter()
            .flat_map(|a| PRESETS.iter().map(move |b| (*a, *b)))
    }

    fn same_iso(a: &Iso, b: &Iso) -> bool {
        (a.tw, a.th, a.ox, a.oy, a.quarter_turns, a.spin)
            == (b.tw, b.th, b.ox, b.oy, b.quarter_turns, b.spin)
    }

    #[test]
    fn lerp_view_endpoints_equal_the_cameras() {
        let base = fit(150, 42, 1).unwrap().iso;
        let grid = (BOARD_COLS, BOARD_BAND);
        for (from, to) in preset_pairs() {
            let a = Camera::lerp_view(&from, &to, 0.0, &base, grid);
            let b = Camera::lerp_view(&from, &to, 1.0, &base, grid);
            assert!(same_iso(&a, &from.apply(&base, grid)), "{from:?}: {a:?}");
            assert!(same_iso(&b, &to.apply(&base, grid)), "{to:?}: {b:?}");
            // Out-of-range t is clamped to the endpoints.
            let a = Camera::lerp_view(&from, &to, -3.0, &base, grid);
            let b = Camera::lerp_view(&from, &to, 7.0, &base, grid);
            assert!(same_iso(&a, &from.apply(&base, grid)));
            assert!(same_iso(&b, &to.apply(&base, grid)));
        }
    }

    #[test]
    fn lerp_view_keeps_the_grid_centre_fixed() {
        let base = fit(150, 42, 2).unwrap().iso;
        let grid = (BOARD_COLS, 2 * BOARD_BAND);
        let (cx, cy) = ((grid.0 as f32 - 1.0) / 2.0, (grid.1 as f32 - 1.0) / 2.0);
        let want = base.project(cx, cy, grid);
        for (from, to) in preset_pairs() {
            for i in 0..=20 {
                let iso = Camera::lerp_view(&from, &to, i as f32 / 20.0, &base, grid);
                let got = iso.project(cx, cy, grid);
                assert!(
                    (got.0 - want.0).abs() <= 1 && (got.1 - want.1).abs() <= 1,
                    "{from:?} -> {to:?} at {i}/20: {got:?} vs {want:?}"
                );
                // Never a tile larger than the fitted base, so the turn
                // stays inside the area the scene was fitted to.
                assert!(iso.tw <= base.tw && iso.th <= base.th, "{iso:?}");
            }
        }
    }

    /// Total yaw of an `Iso` in quarter turns.
    fn yaw(iso: &Iso) -> f32 {
        iso.quarter_turns as f32 + iso.spin / std::f32::consts::FRAC_PI_2
    }

    #[test]
    fn lerp_view_takes_the_shortest_way_round() {
        let base = fit(150, 42, 1).unwrap().iso;
        let grid = (BOARD_COLS, BOARD_BAND);
        let cam = |q| Camera {
            quarter_turns: q,
            pitch: Pitch::Iso,
        };
        // Turn 3 to turn 0 is one quarter turn forward (to 4, which is 0), not
        // three back through 2 and 1.
        let mut last = yaw(&Camera::lerp_view(&cam(3), &cam(0), 0.0, &base, grid));
        assert!((last - 3.0).abs() < 1e-4);
        for i in 1..20 {
            let y = yaw(&Camera::lerp_view(
                &cam(3),
                &cam(0),
                i as f32 / 20.0,
                &base,
                grid,
            ));
            assert!(y >= last - 1e-4 && y <= 4.0 + 1e-4, "step {i}: yaw {y}");
            last = y;
        }
        // Turn 0 to turn 3 is one quarter turn back.
        let mid = yaw(&Camera::lerp_view(&cam(0), &cam(3), 0.5, &base, grid));
        assert!(
            (mid + 0.5).abs() < 1e-3 || (mid - 3.5).abs() < 1e-3,
            "{mid}"
        );
        // Two quarter turns go the positive way.
        let mid = yaw(&Camera::lerp_view(&cam(1), &cam(3), 0.5, &base, grid));
        assert!((mid - 2.0).abs() < 1e-3, "{mid}");
    }

    #[test]
    fn transition_durations_are_0_8_0_9_and_1_6_seconds() {
        assert_eq!(TransitionKind::Chaos.secs(), CHAOS_SECS);
        assert_eq!(CHAOS_SECS, 0.8);
        assert_eq!(TransitionKind::Dissolve.secs(), 0.9);
        assert_eq!(TransitionKind::Spin.secs(), 1.6);
        // And the director uses them: progress runs out at that length.
        for (mode, secs) in [
            (TransitionMode::Chaos, 0.8),
            (TransitionMode::Dissolve, 0.9),
            (TransitionMode::Spin, 1.6),
        ] {
            let mut d = Director::new(7);
            d.set_transition_mode(mode);
            d.cut_by_hand(10.0);
            assert!(d.progress(10.0 + secs - 0.01).is_some(), "{mode:?}");
            assert!(d.progress(10.0 + secs).is_none(), "{mode:?}");
        }
    }

    /// The kind of each of `n` camera-only cuts of a shuffling director.
    fn shuffled_kinds(seed: u64, n: usize, changes_scene: bool) -> Vec<TransitionKind> {
        let mut d = Director::new(seed);
        assert_eq!(d.transition_mode(), TransitionMode::Shuffle);
        (0..n)
            .map(|i| {
                // 10 s apart: past the cooldown, so every trigger cuts.
                assert!(d.on_trigger_with_scene(i as f64 * 10.0, changes_scene));
                d.transition_kind().unwrap()
            })
            .collect()
    }

    #[test]
    fn shuffle_never_repeats_a_kind_twice_in_a_row() {
        for seed in 0..10u64 {
            for changes_scene in [false, true] {
                let kinds = shuffled_kinds(seed, 300, changes_scene);
                for w in kinds.windows(2) {
                    assert_ne!(w[0], w[1], "seed {seed} scene={changes_scene}");
                }
            }
        }
        // Hand cuts follow the same rule.
        let mut d = Director::new(1);
        let mut last = None;
        for i in 0..200 {
            d.cut_by_hand(i as f64);
            assert_ne!(d.transition_kind(), last, "cut {i}");
            last = d.transition_kind();
        }
    }

    #[test]
    fn shuffle_splits_follow_the_weights_after_each_kind() {
        // The previous kind is left out, so after each kind the other two
        // share the draw in the ratio of their weights (40, 35, 25):
        //   after Chaos:    Dissolve 35/60, Spin 25/60
        //   after Dissolve: Chaos 40/65,    Spin 25/65
        //   after Spin:     Chaos 40/75,    Dissolve 35/75
        // The closest split to an even one is after Spin, 53.3 / 46.7, so a
        // uniform draw misses it by 3.3 points. The tolerance is 2 points,
        // and each split here rests on well over 10 000 draws (a standard
        // error under 0.5 points), so weights drawn uniformly fail.
        use TransitionKind as K;
        let kinds = shuffled_kinds(0x5EED, 60_000, false);
        for (prev, first, w_first, w_second) in [
            (K::Chaos, K::Dissolve, 35.0, 25.0),
            (K::Dissolve, K::Chaos, 40.0, 25.0),
            (K::Spin, K::Chaos, 40.0, 35.0),
        ] {
            let after: Vec<K> = kinds
                .windows(2)
                .filter(|w| w[0] == prev)
                .map(|w| w[1])
                .collect();
            assert!(after.len() > 10_000, "{prev:?}: {} draws", after.len());
            let got =
                100.0 * after.iter().filter(|&&k| k == first).count() as f64 / after.len() as f64;
            let want = 100.0 * w_first / (w_first + w_second);
            assert!(
                (got - want).abs() <= 2.0,
                "after {prev:?}: {first:?} {got:.1}% (want {want:.1}%)"
            );
        }
    }

    /// The four grid corners of a `grid`, by the corner name `side_name`
    /// uses: front is grid y high (the PCIe edge side), right is grid x high.
    fn corners(grid: (usize, usize)) -> [(&'static str, (f32, f32)); 4] {
        let (x, y) = ((grid.0 - 1) as f32, (grid.1 - 1) as f32);
        [
            ("front-right", (x, y)),
            ("front-left", (0.0, y)),
            ("back-left", (0.0, 0.0)),
            ("back-right", (x, 0.0)),
        ]
    }

    #[test]
    fn side_names_name_the_corner_nearest_the_viewer() {
        // A wide, a deep and a square grid: the nearest corner must not
        // depend on the board's shape.
        use crate::animation::die3d::scene::fit_pixels;
        for grid in [(17usize, 12usize), (17, 26), (17, 17)] {
            let base = fit_pixels(300, 160, grid).unwrap().iso;
            let mut order = Vec::new();
            for pitch in [Pitch::Iso, Pitch::Low, Pitch::Top] {
                for q in 0..4 {
                    let cam = Camera {
                        quarter_turns: q,
                        pitch,
                    };
                    let iso = cam.apply(&base, grid);
                    let name = cam.side_name();
                    let corner = name.trim_start_matches("top-down, ");
                    assert_eq!(name.starts_with("top-down, "), pitch == Pitch::Top);
                    let projected: Vec<(&str, i32)> = corners(grid)
                        .iter()
                        .map(|&(n, (x, y))| (n, iso.project(x, y, grid).1))
                        .collect();
                    let named = projected.iter().find(|p| p.0 == corner).unwrap().1;
                    for other in projected.iter().filter(|p| p.0 != corner) {
                        assert!(
                            named > other.1,
                            "{grid:?} {cam:?}: {name} at y {named}, {other:?}"
                        );
                    }
                    if pitch == Pitch::Iso {
                        order.push(corner);
                    }
                }
            }
            // The order the projection gives, pinned once checked.
            assert_eq!(
                order,
                ["front-right", "back-right", "back-left", "front-left"],
                "{grid:?}"
            );
        }
    }

    #[test]
    fn top_pitch_is_named_top_down() {
        for q in 0..4 {
            let top = Camera {
                quarter_turns: q,
                pitch: Pitch::Top,
            };
            let iso = Camera {
                quarter_turns: q,
                pitch: Pitch::Iso,
            };
            assert_eq!(top.side_name(), format!("top-down, {}", iso.side_name()));
        }
        // Every preset has a plain-text name.
        for cam in PRESETS {
            assert!(cam.side_name().is_ascii() && !cam.side_name().is_empty());
        }
    }

    #[test]
    fn a_scene_change_never_draws_spin() {
        for seed in 0..20u64 {
            let kinds = shuffled_kinds(seed, 200, true);
            assert!(!kinds.contains(&TransitionKind::Spin), "seed {seed}");
            // Both of the others do come up.
            assert!(kinds.contains(&TransitionKind::Chaos));
            assert!(kinds.contains(&TransitionKind::Dissolve));
        }
        // Control: without a scene change Spin does come up.
        assert!(shuffled_kinds(0, 200, false).contains(&TransitionKind::Spin));
    }

    #[test]
    fn a_fixed_mode_forces_its_kind_and_spin_falls_back_to_dissolve_for_a_scene_change() {
        use TransitionKind as K;
        use TransitionMode as M;
        for (mode, camera_only, scene_change) in [
            (M::Chaos, K::Chaos, K::Chaos),
            (M::Dissolve, K::Dissolve, K::Dissolve),
            (M::Spin, K::Spin, K::Dissolve),
        ] {
            let mut d = Director::new(9);
            d.set_transition_mode(mode);
            assert_eq!(d.transition_mode(), mode);
            for i in 0..20 {
                let scene = i % 3 == 0;
                assert!(d.on_trigger_with_scene(i as f64 * 10.0, scene));
                let want = if scene { scene_change } else { camera_only };
                assert_eq!(d.transition_kind(), Some(want), "{mode:?} cut {i}");
            }
        }
    }

    #[test]
    fn transition_mode_cycles_shuffle_chaos_dissolve_spin() {
        let mut d = Director::new(1);
        let mut seen = vec![d.transition_mode()];
        for _ in 0..4 {
            d.cycle_transition_mode();
            seen.push(d.transition_mode());
        }
        use TransitionMode as M;
        assert_eq!(
            seen,
            [M::Shuffle, M::Chaos, M::Dissolve, M::Spin, M::Shuffle]
        );
        assert_eq!(TransitionMode::default(), M::Shuffle);
    }

    #[test]
    fn progress_is_monotonic_and_none_outside_the_transition() {
        for mode in [
            TransitionMode::Chaos,
            TransitionMode::Dissolve,
            TransitionMode::Spin,
        ] {
            let mut d = Director::new(5);
            d.set_transition_mode(mode);
            assert!(d.progress(0.0).is_none(), "before any cut");
            assert!(d.transition_kind().is_none());
            d.cut_by_hand(10.0);
            let secs = d.transition_kind().unwrap().secs();
            assert!(d.progress(9.99).is_none());
            assert_eq!(d.progress(10.0), Some(0.0));
            let mut last = 0.0;
            for i in 1..100 {
                let p = d.progress(10.0 + secs * i as f64 / 100.0).unwrap();
                assert!(p >= last && p < 1.0, "{mode:?} {i}: {p}");
                last = p;
            }
            // Not a constant: half way at the midpoint, nearly 1 at the end.
            let half = d.progress(10.0 + secs / 2.0).unwrap();
            assert!((half - 0.5).abs() < 1e-3, "{mode:?}: {half}");
            let end = d.progress(10.0 + secs - 1e-6).unwrap();
            assert!((end - 1.0).abs() < 1e-3, "{mode:?}: {end}");
            assert!(d.progress(10.0 + secs).is_none());
            assert!(d.progress(100.0).is_none());
            // Chaos intensity only for a chaos cut.
            let mid = 10.0 + secs / 2.0;
            assert_eq!(
                d.chaos(mid).is_some(),
                mode == TransitionMode::Chaos,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_spin_keeps_the_old_camera_until_it_ends() {
        let mut d = Director::new(5);
        d.set_transition_mode(TransitionMode::Spin);
        let old = d.camera();
        d.cut_by_hand(10.0);
        assert_eq!(d.camera_at(10.0 + 1.59), old);
        assert_eq!(d.camera_at(10.0 + 1.6), d.camera());
        assert_eq!(d.swapped_at(), Some(10.0 + 1.6));
        // Dissolve swaps at its midpoint, like chaos.
        d.set_transition_mode(TransitionMode::Dissolve);
        let old = d.camera();
        d.cut_by_hand(20.0);
        assert_eq!(d.camera_at(20.0 + 0.44), old);
        assert_eq!(d.camera_at(20.0 + 0.45), d.camera());
    }

    /// A buffer covered with one colour.
    fn solid(w: usize, h: usize, c: [u8; 3]) -> PixelBuf {
        let mut b = PixelBuf::new(w, h);
        b.fill_solid(c);
        b
    }

    #[test]
    fn dissolve_at_zero_is_old_and_at_one_is_new() {
        let old = gradient();
        let mut new = PixelBuf::new(64, 32);
        // Half of the new frame is uncovered, so coverage must come along too.
        for y in 0..16 {
            for x in 0..64 {
                new.put(x, y, [10, 200, 30]);
            }
        }
        let a = dissolve(&old, &new, 0.0, 3);
        assert_eq!((a.rgb, a.cover), (old.rgb.clone(), old.cover.clone()));
        let b = dissolve(&old, &new, 1.0, 3);
        assert_eq!((b.rgb, b.cover), (new.rgb.clone(), new.cover.clone()));
    }

    #[test]
    fn dissolve_at_half_mixes_about_half_the_pixels() {
        let (old, new) = (solid(64, 32, [1, 1, 1]), solid(64, 32, [2, 2, 2]));
        let mix = dissolve(&old, &new, 0.5, 11);
        let n = mix.rgb.iter().filter(|&&p| p == [2, 2, 2]).count();
        let total = mix.rgb.len();
        assert!(n * 10 > total * 4 && n * 10 < total * 6, "{n} of {total}");
        assert!(mix.rgb.iter().all(|&p| p == [1, 1, 1] || p == [2, 2, 2]));
        // A pixel that has turned stays turned as progress grows.
        let later = dissolve(&old, &new, 0.7, 11);
        for (a, b) in mix.rgb.iter().zip(&later.rgb) {
            assert!(*a != [2, 2, 2] || *b == [2, 2, 2]);
        }
    }

    #[test]
    fn dissolve_is_deterministic_for_a_seed() {
        let (old, new) = (solid(64, 32, [1, 1, 1]), solid(64, 32, [2, 2, 2]));
        let a = dissolve(&old, &new, 0.5, 4);
        let b = dissolve(&old, &new, 0.5, 4);
        let c = dissolve(&old, &new, 0.5, 5);
        assert_eq!(a.rgb, b.rgb);
        assert_ne!(a.rgb, c.rgb);
    }
}
