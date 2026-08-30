// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Training view's nightscape: drifting aurora bands and a twinkling
//! starfield that fill the empty sky above the loss mountains.
//!
//! Because the mountains descend as the model converges, this negative space
//! grows over a run — the sky opening up *is* a progress signal.
//!
//! Star positions come from a spatial hash rather than a random generator so
//! they are stable frame to frame (a re-rolled star field would shimmer into
//! noise); only their brightness animates.

use crate::animation::common::hsv_to_rgb_bytes;
use ratatui::style::Color;

/// A star occupies a cell when its hash exceeds this. Tuned so the field
/// reads as scattered rather than dense — the mountains are the subject.
pub const STAR_THRESHOLD: f32 = 0.9715;

/// One rendered sky cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyCell {
    pub ch: char,
    pub color: Color,
}

/// Stable 2-D hash → `[0, 1)`. Same coords always give the same value.
pub fn star_hash(x: usize, y: usize) -> f32 {
    let mut h = (x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663);
    h ^= h >> 13;
    h = h.wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h as f32) / (u32::MAX as f32)
}

/// Aurora intensity and hue at a cell. `rel_y` is 0.0 at the top of the sky
/// band and 1.0 at the mountain line.
fn aurora(x: usize, rel_y: f32, frame: u64) -> (f32, f32) {
    let fx = x as f32;
    let ff = frame as f32;
    let mut best = 0.0f32;
    let mut hue = 0.0f32;
    for band in 0..2u32 {
        let speed = if band == 0 { 0.0085 } else { -0.0061 };
        let wob =
            (fx * 0.048 + ff * speed).sin() * 0.16 + (fx * 0.019 - ff * speed * 1.7).sin() * 0.11;
        let centre = if band == 0 { 0.30 } else { 0.55 } + wob;
        let dist = (rel_y - centre).abs();
        if dist < 0.20 {
            let mut v = 1.0 - dist / 0.20;
            v *= 0.55 + 0.45 * (fx * 0.07 + ff * 0.02 + band as f32 * 2.0).sin();
            if v > best {
                best = v;
                hue = if band == 0 { 152.0 } else { 292.0 } + (fx * 0.03 + ff * 0.006).sin() * 40.0;
            }
        }
    }
    (best, hue)
}

/// The character to draw at a sky cell, if any.
pub fn sky_cell(x: usize, y: usize, rel_y: f32, frame: u64) -> Option<SkyCell> {
    // Stars sit on top of the aurora.
    let sv = star_hash(x, y);
    if sv > STAR_THRESHOLD {
        let phase = star_hash(x + 31, y + 17) * std::f32::consts::TAU;
        let speed = 0.035 + star_hash(x + 7, y + 3) * 0.075;
        let tw = 0.5 + 0.5 * (frame as f32 * speed + phase).sin();
        // A minority of stars are vividly colored; the rest are cool white.
        let vivid = star_hash(x + 91, y + 53) > 0.82;
        let (h, s) = if vivid {
            (158.0 + star_hash(x, y + 11) * 190.0, 0.78)
        } else {
            (205.0, 0.22)
        };
        let (ch, v) = if tw > 0.88 {
            ('✦', 0.74)
        } else if tw > 0.66 {
            ('∙', 0.56)
        } else if tw > 0.36 {
            ('·', 0.38)
        } else {
            ('·', 0.22)
        };
        let (r, g, b) = hsv_to_rgb_bytes(h, s, v);
        return Some(SkyCell {
            ch,
            color: Color::Rgb(r, g, b),
        });
    }

    let (lit, hue) = aurora(x, rel_y, frame);
    if lit > 0.14 {
        let ch = if lit > 0.62 { '▒' } else { '░' };
        let (r, g, b) = hsv_to_rgb_bytes(hue, 0.62, 0.10 + lit * 0.15);
        return Some(SkyCell {
            ch,
            color: Color::Rgb(r, g, b),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_hash_is_deterministic_and_in_range() {
        // Stars must not jitter between frames: same coords → same value.
        for (x, y) in [(0usize, 0usize), (17, 3), (129, 40), (7, 11)] {
            let a = star_hash(x, y);
            let b = star_hash(x, y);
            assert_eq!(a, b, "hash must be stable for ({x},{y})");
            assert!((0.0..=1.0).contains(&a), "hash out of range: {a}");
        }
    }

    #[test]
    fn star_hash_varies_across_neighbours() {
        // A weak hash that returns near-identical values for adjacent cells
        // would produce visible banding instead of scattered stars.
        let a = star_hash(10, 10);
        let b = star_hash(11, 10);
        let c = star_hash(10, 11);
        assert!((a - b).abs() > 0.001, "x-neighbours too similar");
        assert!((a - c).abs() > 0.001, "y-neighbours too similar");
    }

    #[test]
    fn a_star_keeps_its_position_across_frames() {
        // Find a cell that is a star at frame 0, then confirm it is still a
        // star (maybe a different brightness) many frames later.
        let mut found = None;
        for x in 0..200usize {
            for y in 0..12usize {
                if sky_cell(x, y, 0.5, 0).is_some() && star_hash(x, y) > STAR_THRESHOLD {
                    found = Some((x, y));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (sx, sy) = found.expect("some cell in a 200x12 field must hold a star");
        for f in [1u64, 7, 60, 999] {
            assert!(
                sky_cell(sx, sy, 0.5, f).is_some(),
                "star at ({sx},{sy}) vanished at frame {f}"
            );
        }
    }

    #[test]
    fn sky_is_mostly_empty_so_the_mountains_stay_readable() {
        let mut filled = 0usize;
        let total = 128 * 11;
        for x in 0..128usize {
            for y in 0..11usize {
                if sky_cell(x, y, y as f32 / 11.0, 0).is_some() {
                    filled += 1;
                }
            }
        }
        let frac = filled as f32 / total as f32;
        assert!(
            frac < 0.5,
            "sky fill {frac:.2} is too dense — aurora/stars must stay subtle"
        );
    }
}
