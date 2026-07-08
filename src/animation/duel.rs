// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The hero ⚔ snake duel — a telemetry-true tug-of-war strip.
//!
//! When an inference server is *serving*, the Arcade's roguelike hero `@` (the
//! silicon) and the serving snake (the model) meet on a single-row strip and
//! duel. A `⚔` balance marker slides between them, driven by hardware headroom
//! versus serving demand. Lunges fire on **real events only**:
//!
//! - the hero lunges on a real power spike (baseline deviation),
//! - the snake lunges when a request actually completes.
//!
//! Nothing here moves on a scripted timer: every offset traces back to live
//! telemetry. This module is the pure motion + glyph layer; `arcade.rs` owns
//! the color and decides when to show the strip at all.

/// Signed tug-of-war balance in [-1, +1]: positive = hardware headroom is
/// winning (hero side), negative = serving demand dominates (snake side).
/// NaN-safe: non-finite inputs read as 0.
pub fn duel_balance(hw: f32, serve: f32) -> f32 {
    let h = if hw.is_finite() {
        hw.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let s = if serve.is_finite() {
        serve.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (h - s).clamp(-1.0, 1.0)
}

/// How long (in frames) a lunge visibly persists once re-armed by a real event.
const LUNGE_FRAMES: u32 = 12;
/// EMA smoothing factor pulling `balance_smooth` toward the live balance.
const BALANCE_EMA: f32 = 0.15;
/// Number of cells the resting snake body occupies (head + 3 body).
const SNAKE_LEN: i32 = 4;
/// Widths below this drop the snake body and render only hero + marker + head.
const NARROW_WIDTH: usize = 12;

/// Animation state for the hero ⚔ snake duel strip.
///
/// The strip is a horizontal tug-of-war: the hero `@` is anchored near the left
/// edge (the silicon's home), the snake is tail-anchored at the right edge (the
/// model's home), and the `⚔` marker floats between them at a position set by
/// the smoothed balance. Lunges briefly close the distance from whichever side
/// just did real work.
#[derive(Debug, Clone)]
pub struct DuelState {
    /// Frame counter — drives the idle bob (nothing else is time-based).
    frame: u64,
    /// EMA-smoothed balance in [-1, +1]; positive = hero winning.
    balance_smooth: f32,
    /// Frames of hero-lunge remaining (re-armed to `LUNGE_FRAMES` on a spike).
    hero_lunge_left: u32,
    /// Frames of snake-lunge remaining (re-armed on a completed request).
    snake_lunge_left: u32,
    /// KV-cache usage carried for the snake's heat color (arcade reads it).
    pub kv: f32,
    /// Last hardware-headroom signal, retained for idle detection.
    hw: f32,
    /// Last serving-demand signal, retained for idle detection.
    serve: f32,
}

impl DuelState {
    /// Create a fresh, centered, resting duel.
    pub fn new() -> Self {
        Self {
            frame: 0,
            balance_smooth: 0.0,
            hero_lunge_left: 0,
            snake_lunge_left: 0,
            kv: 0.0,
            hw: 0.0,
            serve: 0.0,
        }
    }

    /// Advance one frame.
    ///
    /// * `hw` — hardware headroom signal (roughly 0..1, mean device power / peak).
    /// * `serve` — serving-demand signal (roughly 0..1, blended tps + queue).
    /// * `hero_lunge` — true on a *real* hardware power spike this tick.
    /// * `snake_lunge` — true when a request actually completed this tick.
    /// * `kv` — KV-cache usage in 0..1 (snake heat color).
    ///
    /// All floats are made NaN-safe here so `render_strip` never sees garbage.
    pub fn update(&mut self, hw: f32, serve: f32, hero_lunge: bool, snake_lunge: bool, kv: f32) {
        self.frame = self.frame.wrapping_add(1);
        self.hw = if hw.is_finite() { hw } else { 0.0 };
        self.serve = if serve.is_finite() { serve } else { 0.0 };
        self.kv = if kv.is_finite() {
            kv.clamp(0.0, 1.0)
        } else {
            0.0
        };

        // Smoothly track the live balance so the marker glides rather than jumps.
        let target = duel_balance(hw, serve);
        self.balance_smooth += (target - self.balance_smooth) * BALANCE_EMA;

        // Lunges are re-armed by real events; otherwise they decay one frame/tick.
        self.hero_lunge_left = if hero_lunge {
            LUNGE_FRAMES
        } else {
            self.hero_lunge_left.saturating_sub(1)
        };
        self.snake_lunge_left = if snake_lunge {
            LUNGE_FRAMES
        } else {
            self.snake_lunge_left.saturating_sub(1)
        };
    }

    /// True when both sides are quiet and no lunge is in flight — the strip
    /// shows resting glyphs instead of an active duel.
    fn is_idle(&self) -> bool {
        self.hw.abs() < 1e-3
            && self.serve.abs() < 1e-3
            && self.hero_lunge_left == 0
            && self.snake_lunge_left == 0
    }

    /// Render the duel as a glyph row of **exactly** `width` chars.
    ///
    /// Layout (wide enough, `width >= 12`):
    /// ```text
    ///   @        ⚔        ▶●●●
    ///  hero    marker      snake (tail at right edge)
    /// ```
    /// The hero sits at column 2, lunging +2 toward center on a real spike. The
    /// snake is tail-anchored at the right edge with its head (`▶`) nearest the
    /// hero; the head advances left during its lunge. The `⚔` marker floats at
    /// `center - balance_smooth * span/2` — tug-of-war semantics: the winning
    /// side pulls the marker toward itself, so positive balance (hardware
    /// dominating) drags it LEFT toward the hero. Clamped strictly between the
    /// two actors.
    ///
    /// Idle (both signals ~0, no lunges) → the hero `@` bobs by frame parity,
    /// the snake coils to `◉~` at rest, and the marker rests dim at center.
    ///
    /// Narrow strips (`width < 12`) degrade to hero + marker + a single snake
    /// head. `width == 0` returns `""`. Every index is clamped into `[0, width)`
    /// so no input can panic.
    pub fn render_strip(&self, width: usize) -> String {
        if width == 0 {
            return String::new();
        }

        let w = width as i32;
        let narrow = width < NARROW_WIDTH;
        let idle = self.is_idle();
        let mut buf = vec![' '; width];

        // ── Hero position ──────────────────────────────────────────────────
        // Home column 2; +2 toward center while lunging; a 1-column bob at rest.
        let mut hx = 2;
        if self.hero_lunge_left > 0 {
            hx += 2;
        }
        if idle {
            hx += (self.frame % 2) as i32;
        }
        let hx = hx.clamp(0, w - 1);

        // ── Snake head position ────────────────────────────────────────────
        // Tail anchored at the right edge; head nearest the hero. Head advances
        // 2 cells left on a lunge. Narrow strips keep only the head at the edge.
        let tail_x = w - 1;
        let head_x = if narrow {
            (w - 1).max(0)
        } else {
            let base = w - SNAKE_LEN - if self.snake_lunge_left > 0 { 2 } else { 0 };
            // Never let the head cross past the hero (leave at least 1 gap cell).
            base.clamp((hx + 2).min(w - 1), w - 1)
        };

        // ── Balance marker position ────────────────────────────────────────
        // Float from center by the smoothed balance across the hero↔head span,
        // then clamp strictly between the two actors so it never overlaps them.
        let center = w as f32 / 2.0;
        let span = (head_x - hx).max(0) as f32;
        // Winner pulls the marker toward themselves: positive balance =
        // hardware (hero, left side) dominating → negative offset.
        let raw = center - self.balance_smooth * (span / 2.0);
        let mut mx = raw.round() as i32;
        let lo = (hx + 1).clamp(0, w - 1);
        let hi = (head_x - 1).clamp(0, w - 1);
        mx = if lo <= hi {
            mx.clamp(lo, hi)
        } else {
            center.round() as i32
        };
        let mx = mx.clamp(0, w - 1);

        // ── Paint (snake first, then hero, then marker on top) ─────────────
        let set = |buf: &mut Vec<char>, x: i32, ch: char| {
            if x >= 0 && (x as usize) < buf.len() {
                buf[x as usize] = ch;
            }
        };

        if narrow {
            set(&mut buf, head_x, '▶');
        } else if idle {
            // Coiled snake at rest: `◉~` tucked against the right edge.
            set(&mut buf, tail_x - 1, '◉');
            set(&mut buf, tail_x, '~');
        } else {
            // Active snake: head `▶` nearest the hero, body `●` out to the edge.
            set(&mut buf, head_x, '▶');
            let mut x = head_x + 1;
            while x <= tail_x {
                set(&mut buf, x, '●');
                x += 1;
            }
        }

        set(&mut buf, hx, '@');
        set(&mut buf, mx, '⚔');

        buf.into_iter().collect()
    }
}

impl Default for DuelState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_is_clamped_and_signed() {
        assert!(
            duel_balance(1.0, 0.0) > 0.5,
            "hardware dominance → hero side (+)"
        );
        assert!(
            duel_balance(0.0, 1.0) < -0.5,
            "serving demand → snake side (-)"
        );
        assert_eq!(duel_balance(0.5, 0.5), 0.0);
        assert!(duel_balance(f32::NAN, 0.5).is_finite(), "NaN-safe");
        assert!((-1.0..=1.0).contains(&duel_balance(99.0, -5.0)), "clamped");
    }

    #[test]
    fn strip_stages_hero_snake_and_marker() {
        let mut d = DuelState::new();
        d.update(0.8, 0.3, true, false, 0.1);
        let s = d.render_strip(60);
        assert_eq!(s.chars().count(), 60);
        assert!(s.contains('@'), "hero present");
        assert!(s.contains('⚔'), "balance marker present");
        assert!(s.contains('▶') || s.contains('●'), "snake present");
        assert_eq!(d.render_strip(0), "");
        // Idle both sides → resting glyphs, no lunge offsets.
        let mut idle = DuelState::new();
        idle.update(0.0, 0.0, false, false, 0.0);
        let _ = idle.render_strip(40); // no panic; visual rest is manual-checked
    }

    /// Tug-of-war direction (spec: "slides toward the hero when chip
    /// power/util dominates"): the winner pulls the ⚔ marker toward itself.
    /// Regression for the inverted-sign bug the initial implementation shipped.
    #[test]
    fn marker_pulls_toward_the_winning_side() {
        let width = 80;
        let center = width / 2;
        let marker_at = |hw: f32, serve: f32| -> usize {
            let mut d = DuelState::new();
            // Run the EMA to convergence so balance_smooth ≈ duel_balance.
            for _ in 0..200 {
                d.update(hw, serve, false, false, 0.0);
            }
            d.render_strip(width)
                .chars()
                .position(|c| c == '⚔')
                .expect("marker present")
        };
        // Hardware dominating → marker LEFT of center (toward the hero).
        assert!(
            marker_at(1.0, 0.0) < center,
            "hw dominance must pull the marker toward the hero (left)"
        );
        // Serving demand dominating → marker RIGHT of center (toward the snake).
        assert!(
            marker_at(0.0, 1.0) > center,
            "serving dominance must pull the marker toward the snake (right)"
        );
    }
}
