// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Activity trigger detection for the Die 3D camera director.
//!
//! Pure logic: no clock, no I/O, no backend access. The view calls
//! [`TriggerDetector::observe`] once per chip per tick and the returned
//! [`Trigger`]s become camera cuts. Reset, model-load and ECC triggers are
//! deliberately not detected here.

use std::collections::HashMap;

/// Load (0..1) at or above which a rise fires. The view passes load relative
/// to the chip's own idle (`load::LoadScale`), so 0.5 is half way from idle
/// to the chip's recent peak. As power over the 125 W limit it could never
/// fire: idle chips measured 36 to 47 W, which is 0.30 and above
/// [`LOAD_REARM`], so the detector never armed.
pub const LOAD_ON: f32 = 0.5;
/// Load at or below which the load detector is armed again.
pub const LOAD_REARM: f32 = 0.25;
/// PCIe rx+tx rate (bytes/s) at or above which a burst fires: 50 MB/s.
///
/// Measured from the tt-kmd counters while 8 chat clients drove a coder
/// model at 213 tokens per second: the two busy chips moved a mean of
/// 23 MB/s into the chip (95th percentile 115 MB/s, maximum 131 MB/s) and
/// 13 MB/s out (maximum 53 MB/s); idle chips were near 0. The earlier
/// 2 GB/s was about 15 times above anything token serving moves. 50 MB/s is
/// above the serving mean and below its bursts.
pub const PCIE_ON_BPS: f64 = 50.0e6;
/// PCIe rate (bytes/s) at or below which the burst detector is armed again:
/// 10 MB/s, below the serving mean, so a burst fires again only after the
/// traffic has dropped back towards idle.
pub const PCIE_REARM_BPS: f64 = 10.0e6;

/// Something interesting happened on a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Load crossed up through [`LOAD_ON`].
    LoadRise { chip: usize },
    /// PCIe traffic crossed up through [`PCIE_ON_BPS`].
    PcieBurst { chip: usize },
}

/// Schmitt-trigger style edge detector.
///
/// Armed after a sample at or below the re-arm level. While armed, the
/// first sample at or above the on level fires and disarms. Samples between
/// the two levels change nothing. No history means unarmed, so a first sample
/// that is already high never fires. Non-finite samples are ignored entirely.
#[derive(Debug, Default, Clone, Copy)]
struct Edge {
    armed: bool,
}

impl Edge {
    /// Feed one sample; returns true when the edge fires.
    fn step(&mut self, v: f64, on: f64, rearm: f64) -> bool {
        if !v.is_finite() {
            return false;
        }
        if v <= rearm {
            self.armed = true;
            false
        } else if v >= on && self.armed {
            self.armed = false;
            true
        } else {
            false
        }
    }
}

/// Per-chip detector state.
#[derive(Debug, Default, Clone, Copy)]
struct ChipState {
    load: Edge,
    pcie: Edge,
}

/// Detects [`Trigger`]s from a stream of per-chip samples.
#[derive(Debug, Default)]
pub struct TriggerDetector {
    chips: HashMap<usize, ChipState>,
}

impl TriggerDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe one chip's sample. `load` is 0..1; `pcie_total_bps` is
    /// `Some(rx + tx)` or `None` when unavailable.
    pub fn observe(&mut self, chip: usize, load: f32, pcie_total_bps: Option<f64>) -> Vec<Trigger> {
        let state = self.chips.entry(chip).or_default();
        let mut out = Vec::new();
        if state
            .load
            .step(f64::from(load), f64::from(LOAD_ON), f64::from(LOAD_REARM))
        {
            out.push(Trigger::LoadRise { chip });
        }
        // A missing rate is the same as a non-finite one: ignored.
        if let Some(bps) = pcie_total_bps {
            if state.pcie.step(bps, PCIE_ON_BPS, PCIE_REARM_BPS) {
                out.push(Trigger::PcieBurst { chip });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loads(d: &mut TriggerDetector, chip: usize, seq: &[f32]) -> Vec<usize> {
        seq.iter()
            .enumerate()
            .filter(|(_, &l)| {
                d.observe(chip, l, None)
                    .contains(&Trigger::LoadRise { chip })
            })
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn load_rise_fires_once_per_crossing() {
        let mut d = TriggerDetector::new();
        // Fires at index 2 only; staying high must not refire.
        assert_eq!(loads(&mut d, 0, &[0.1, 0.4, 0.6, 0.9, 0.9]), vec![2]);
    }

    #[test]
    fn load_rise_needs_a_drop_below_rearm_before_firing_again() {
        let mut d = TriggerDetector::new();
        // 0.3 is between the levels: it does not re-arm, so index 4 is silent.
        // 0.1 re-arms, so index 7 fires.
        let seq = [0.1, 0.6, 0.3, 0.4, 0.6, 0.1, 0.4, 0.6];
        assert_eq!(loads(&mut d, 0, &seq), vec![1, 7]);
    }

    #[test]
    fn load_boundaries_on_and_rearm_are_both_inclusive() {
        let mut d = TriggerDetector::new();
        // Exactly LOAD_ON fires when armed.
        assert_eq!(loads(&mut d, 0, &[0.0, LOAD_ON]), vec![1]);
        // Exactly LOAD_REARM re-arms.
        let mut d = TriggerDetector::new();
        assert_eq!(loads(&mut d, 0, &[0.0, 0.9, LOAD_REARM, 0.9]), vec![1, 3]);
        // Just above LOAD_REARM does not.
        let mut d = TriggerDetector::new();
        let just_over = LOAD_REARM + 0.001;
        assert_eq!(loads(&mut d, 0, &[0.0, 0.9, just_over, 0.9]), vec![1]);
    }

    #[test]
    fn a_first_sample_already_high_does_not_fire() {
        let mut d = TriggerDetector::new();
        assert!(d.observe(0, 0.9, Some(5.0e9)).is_empty());
        // Still high: no crossing ever happened.
        assert!(d.observe(0, 0.95, Some(6.0e9)).is_empty());
        // A first sample between the levels is not armed either.
        let mut d = TriggerDetector::new();
        assert!(loads(&mut d, 1, &[0.3, 0.6]).is_empty());
    }

    #[test]
    fn pcie_burst_fires_at_50_mb_per_second_after_idle() {
        // Rates from real token serving: idle near 0, a mean of 23 MB/s in
        // and 13 MB/s out, a 95th percentile of 115 MB/s and a maximum of
        // 131 MB/s in.
        let mut d = TriggerDetector::new();
        let burst = Trigger::PcieBurst { chip: 0 };
        assert!(d.observe(0, 0.0, Some(0.0)).is_empty()); // idle: armed
        assert!(d.observe(0, 0.0, Some(49.0e6)).is_empty());
        assert_eq!(d.observe(0, 0.0, Some(50.0e6)), vec![burst]);
        // Held high, then the serving mean (36 MB/s in and out), which stays
        // above re-arm: silent.
        assert!(d.observe(0, 0.0, Some(131.0e6)).is_empty());
        assert!(d.observe(0, 0.0, Some(36.0e6)).is_empty());
        assert!(d.observe(0, 0.0, Some(115.0e6)).is_empty());
        // At the re-arm level, then up again: fires.
        assert!(d.observe(0, 0.0, Some(PCIE_REARM_BPS)).is_empty());
        assert_eq!(d.observe(0, 0.0, Some(115.0e6)), vec![burst]);
        // The constants are the measured ones.
        assert_eq!((PCIE_ON_BPS, PCIE_REARM_BPS), (50.0e6, 10.0e6));
        // PCIe and load are independent edges.
        let mut d = TriggerDetector::new();
        d.observe(0, 0.0, Some(0.0));
        assert_eq!(
            d.observe(0, 0.9, Some(3.0e9)),
            vec![Trigger::LoadRise { chip: 0 }, burst]
        );
    }

    #[test]
    fn non_finite_or_missing_pcie_never_fires() {
        let mut d = TriggerDetector::new();
        d.observe(0, 0.0, Some(0.0)); // armed
        for bad in [None, Some(f64::NAN), Some(f64::INFINITY)] {
            assert!(d.observe(0, 0.0, bad).is_empty(), "{bad:?}");
        }
        // Still armed: bad readings did not disarm either.
        assert_eq!(
            d.observe(0, 0.0, Some(3.0e9)),
            vec![Trigger::PcieBurst { chip: 0 }]
        );
        // Bad readings must not re-arm a disarmed detector (NaN < x is false,
        // -inf < x is true, so both need an explicit guard).
        for bad in [None, Some(f64::NAN), Some(f64::NEG_INFINITY)] {
            d.observe(0, 0.0, bad);
        }
        assert!(d.observe(0, 0.0, Some(3.0e9)).is_empty());
        // A first sample of infinity is not "high with history" either.
        let mut d = TriggerDetector::new();
        assert!(d.observe(0, 0.0, Some(f64::INFINITY)).is_empty());
        assert!(d.observe(0, 0.0, Some(3.0e9)).is_empty());
    }

    #[test]
    fn non_finite_load_behaves_like_a_missing_reading() {
        let mut d = TriggerDetector::new();
        assert!(loads(&mut d, 0, &[0.0]).is_empty()); // armed
        assert!(loads(&mut d, 0, &[f32::NAN, f32::INFINITY]).is_empty());
        // Still armed after bad readings.
        assert_eq!(loads(&mut d, 0, &[0.9]), vec![0]);
        // Disarmed: bad readings (including -inf) must not re-arm.
        assert!(loads(&mut d, 0, &[f32::NAN, f32::NEG_INFINITY, 0.9]).is_empty());
    }

    #[test]
    fn chips_are_tracked_independently() {
        let mut d = TriggerDetector::new();
        d.observe(0, 0.0, None);
        d.observe(1, 0.0, None);
        assert_eq!(d.observe(0, 0.9, None), vec![Trigger::LoadRise { chip: 0 }]);
        // Chip 0 firing leaves chip 1 armed, and chip 1 is not affected by 0's
        // disarmed state.
        assert_eq!(d.observe(1, 0.9, None), vec![Trigger::LoadRise { chip: 1 }]);
        assert!(d.observe(0, 0.9, None).is_empty());
        // Re-arming chip 1 does not re-arm chip 0.
        d.observe(1, 0.0, None);
        assert!(d.observe(0, 0.9, None).is_empty());
        assert_eq!(d.observe(1, 0.9, None), vec![Trigger::LoadRise { chip: 1 }]);
    }
}
