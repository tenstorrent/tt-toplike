// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Opt-in animation throttling decisions (pure logic).
//!
//! Modulates the animated render interval: full speed by default, dropped to
//! 30 FPS under heavy host load (when `--throttle`/`/throttle` is on, with
//! hysteresis), or 2 FPS when the terminal is unfocused (when
//! `--idle-on-blur`/`/idle-on-blur` is on). Focus-idle beats load-throttle.
//! Never returns an interval faster than the supplied base.

use std::time::Duration;

const IDLE_FPS: u64 = 2;
const LOAD_FPS: u64 = 30;
const LOAD_HI: f32 = 85.0; // engage load throttle at/above this host CPU %
const LOAD_LO: f32 = 80.0; // disengage below this (hysteresis band 80–85)

fn fps_interval(fps: u64) -> Duration {
    Duration::from_secs_f64(1.0 / fps as f64)
}

/// Runtime throttle flags + derived load state. Cheap to copy semantics; lives
/// in the run loop.
#[derive(Debug, Clone)]
pub struct ThrottleState {
    pub throttle_on: bool,
    pub idle_on_blur: bool,
    pub focused: bool,
    load_throttling: bool,
}

impl ThrottleState {
    pub fn new(throttle_on: bool, idle_on_blur: bool) -> Self {
        ThrottleState {
            throttle_on,
            idle_on_blur,
            focused: true,
            load_throttling: false,
        }
    }

    /// Update the load-throttle latch from a host CPU% sample, with hysteresis.
    pub fn update_load(&mut self, host_cpu_pct: f32) {
        if host_cpu_pct >= LOAD_HI {
            self.load_throttling = true;
        } else if host_cpu_pct < LOAD_LO {
            self.load_throttling = false;
        }
        // 80–85% band: leave self.load_throttling unchanged.
    }

    /// Animated render interval given the user's base (e.g. from `/fps`).
    /// Result is never faster (smaller) than `base`.
    pub fn effective_anim_interval(&self, base: Duration) -> Duration {
        let chosen = if self.idle_on_blur && !self.focused {
            fps_interval(IDLE_FPS)
        } else if self.throttle_on && self.load_throttling {
            fps_interval(LOAD_FPS)
        } else {
            base
        };
        chosen.max(base)
    }

    /// Short status-bar hint when actively throttling, else `None`.
    pub fn status_hint(&self) -> Option<&'static str> {
        if self.idle_on_blur && !self.focused {
            Some("⏸2")
        } else if self.throttle_on && self.load_throttling {
            Some("⏷30")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_millis(16); // ~60 FPS

    #[test]
    fn full_speed_when_off_and_focused() {
        let s = ThrottleState::new(false, false);
        assert_eq!(s.effective_anim_interval(BASE), BASE);
        assert_eq!(s.status_hint(), None);
    }

    #[test]
    fn blur_idle_only_when_enabled_and_unfocused() {
        let mut s = ThrottleState::new(false, true);
        s.focused = true;
        assert_eq!(s.effective_anim_interval(BASE), BASE); // focused → full
        s.focused = false;
        assert_eq!(s.effective_anim_interval(BASE), fps_interval(IDLE_FPS)); // 2 FPS
        assert_eq!(s.status_hint(), Some("⏸2"));
    }

    #[test]
    fn load_throttle_drops_to_30_with_hysteresis() {
        let mut s = ThrottleState::new(true, false);
        s.update_load(90.0); // ≥85 engages
        assert_eq!(s.effective_anim_interval(BASE), fps_interval(LOAD_FPS)); // ~30 FPS
        assert_eq!(s.status_hint(), Some("⏷30"));
        s.update_load(82.0); // 80–85 band holds prior (still throttling)
        assert_eq!(s.effective_anim_interval(BASE), fps_interval(LOAD_FPS));
        s.update_load(78.0); // <80 disengages
        assert_eq!(s.effective_anim_interval(BASE), BASE);
    }

    #[test]
    fn load_throttle_ignored_when_toggle_off() {
        let mut s = ThrottleState::new(false, false);
        s.update_load(99.0);
        assert_eq!(s.effective_anim_interval(BASE), BASE);
    }

    #[test]
    fn blur_idle_beats_load_throttle() {
        let mut s = ThrottleState::new(true, true);
        s.update_load(99.0);
        s.focused = false;
        assert_eq!(s.effective_anim_interval(BASE), fps_interval(IDLE_FPS)); // blur wins
    }

    #[test]
    fn never_faster_than_base() {
        // If /fps set a slow base (e.g. 10 FPS = 100ms), throttle's 30 FPS must not speed it up.
        let mut s = ThrottleState::new(true, false);
        s.update_load(99.0);
        let slow = Duration::from_millis(100);
        assert_eq!(s.effective_anim_interval(slow), slow);
    }
}
