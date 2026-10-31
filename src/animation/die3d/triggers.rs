// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Activity trigger detection for the Die 3D camera director.
//!
//! Pure logic: no clock, no I/O, no backend access. The view calls
//! [`TriggerDetector::observe`] once per chip per tick and the returned
//! [`Trigger`]s become camera cuts. Reset, model-load and ECC triggers are
//! deliberately not detected here.
//!
//! Each trigger has a [`Reason`]: something on a chip, in its memory, on its
//! board or in a process that uses it. The reason decides how the camera cuts
//! (`camera::Director`).

use std::collections::{BTreeSet, HashMap};

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

/// GDDR heat (0..1, the hottest memory temperature mapped from 40 to 90 C) at
/// or above which a memory trigger fires: 0.5 is 65 C.
pub const MEMORY_ON: f32 = 0.5;
/// GDDR heat at or below which the memory detector is armed again: 57.5 C.
pub const MEMORY_REARM: f32 = 0.35;
/// Degrees above the chip's own recent temperature at which a thermal trigger
/// fires. Idle Blackhole chips sit at 40 to 50 C and a loaded one a few degrees
/// above its idle, so a fixed limit would be wrong on a cool box and a hot one.
pub const THERMAL_RISE_C: f32 = 4.0;
/// Degrees above the baseline at or below which the thermal detector is armed
/// again.
pub const THERMAL_REARM_C: f32 = 1.5;
/// Time constant, in seconds, of the temperature baseline: a rise slower than
/// this becomes the new normal and does not fire.
pub const THERMAL_BASELINE_SECS: f64 = 60.0;

/// What a trigger is about. It chooses the cut: see `Director::on_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The chip itself: its load, its PCIe traffic or its temperature.
    Chip,
    /// Its memory: the GDDR heating up.
    Memory,
    /// The board: an Ethernet link came up or went down.
    Board,
    /// A process opened or closed the chip's device.
    Process,
}

/// Something interesting happened on a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Load crossed up through [`LOAD_ON`].
    LoadRise { chip: usize },
    /// PCIe traffic crossed up through [`PCIE_ON_BPS`].
    PcieBurst { chip: usize },
    /// The ASIC temperature rose [`THERMAL_RISE_C`] above its recent level.
    ThermalRise { chip: usize },
    /// GDDR heat crossed up through [`MEMORY_ON`].
    MemoryHeat { chip: usize },
    /// The set of live Ethernet links changed.
    EthLink { chip: usize },
    /// A process opened or closed this chip's device.
    Process { chip: usize },
}

impl Trigger {
    /// The chip it happened on (the device index).
    pub fn chip(self) -> usize {
        match self {
            Trigger::LoadRise { chip }
            | Trigger::PcieBurst { chip }
            | Trigger::ThermalRise { chip }
            | Trigger::MemoryHeat { chip }
            | Trigger::EthLink { chip }
            | Trigger::Process { chip } => chip,
        }
    }

    /// What it is about.
    pub fn reason(self) -> Reason {
        match self {
            Trigger::LoadRise { .. } | Trigger::PcieBurst { .. } | Trigger::ThermalRise { .. } => {
                Reason::Chip
            }
            Trigger::MemoryHeat { .. } => Reason::Memory,
            Trigger::EthLink { .. } => Reason::Board,
            Trigger::Process { .. } => Reason::Process,
        }
    }
}

/// Readings beyond load and PCIe that can trigger a cut; each may be missing.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChipExtras {
    /// ASIC temperature, degrees C.
    pub temp_c: Option<f32>,
    /// Hottest GDDR temperature mapped from 40..90 C onto 0..1.
    pub gddr_heat: Option<f32>,
    /// Bit `i` set when Ethernet port `i` has a live link.
    pub eth_live: Option<u64>,
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

/// A temperature trigger against a slowly following baseline of the chip's own
/// temperature. Armed while the temperature is within [`THERMAL_REARM_C`] of the
/// baseline; fires once when it gets [`THERMAL_RISE_C`] above it.
#[derive(Debug, Default, Clone, Copy)]
struct Thermal {
    /// (baseline, time of the last sample); none before the first sample.
    baseline: Option<(f32, f64)>,
    armed: bool,
}

impl Thermal {
    fn step(&mut self, now: f64, temp: f32) -> bool {
        if !temp.is_finite() {
            return false;
        }
        let Some((base, at)) = self.baseline else {
            // No history: the first reading is the baseline, whatever it is.
            self.baseline = Some((temp, now));
            self.armed = true;
            return false;
        };
        let over = temp - base;
        let mut fired = false;
        if over <= THERMAL_REARM_C {
            self.armed = true;
        } else if over >= THERMAL_RISE_C && self.armed {
            self.armed = false;
            fired = true;
        }
        // The baseline follows with a time constant, so it takes the same
        // number of seconds whatever the tick rate. A clock that goes back
        // leaves it as it was.
        let dt = (now - at).max(0.0);
        let k = 1.0 - (-dt / THERMAL_BASELINE_SECS).exp();
        self.baseline = Some((base + over * k as f32, now));
        fired
    }
}

/// Per-chip detector state.
#[derive(Debug, Default, Clone, Copy)]
struct ChipState {
    load: Edge,
    pcie: Edge,
    thermal: Thermal,
    memory: Edge,
    /// The Ethernet link mask seen last.
    eth: Option<u64>,
}

/// Detects [`Trigger`]s from a stream of per-chip samples.
#[derive(Debug, Default)]
pub struct TriggerDetector {
    chips: HashMap<usize, ChipState>,
    /// The (pid, device) pairs seen last; none before the first sample.
    procs: Option<BTreeSet<(i64, usize)>>,
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

    /// Observe a chip's temperature, memory heat and Ethernet links at `now`
    /// seconds. Separate from [`observe`](Self::observe) so a source that has
    /// only load and PCIe needs nothing more.
    pub fn observe_extras(&mut self, now: f64, chip: usize, x: &ChipExtras) -> Vec<Trigger> {
        let state = self.chips.entry(chip).or_default();
        let mut out = Vec::new();
        if let Some(t) = x.temp_c {
            if state.thermal.step(now, t) {
                out.push(Trigger::ThermalRise { chip });
            }
        }
        if let Some(h) = x.gddr_heat {
            if state
                .memory
                .step(f64::from(h), f64::from(MEMORY_ON), f64::from(MEMORY_REARM))
            {
                out.push(Trigger::MemoryHeat { chip });
            }
        }
        if let Some(mask) = x.eth_live {
            // The first mask is the starting point, not a change.
            if state.eth.is_some_and(|prev| prev != mask) {
                out.push(Trigger::EthLink { chip });
            }
            state.eth = Some(mask);
        }
        out
    }

    /// Observe which processes have which devices open: `(pid, device)` pairs.
    /// A trigger for each device where one opened or closed since the last
    /// call. The first call only records the starting set.
    pub fn observe_processes(&mut self, procs: &[(i64, usize)]) -> Vec<Trigger> {
        let now: BTreeSet<(i64, usize)> = procs.iter().copied().collect();
        let Some(before) = self.procs.replace(now.clone()) else {
            return Vec::new();
        };
        before
            .symmetric_difference(&now)
            .map(|&(_, device)| device)
            .collect::<BTreeSet<usize>>()
            .into_iter()
            .map(|chip| Trigger::Process { chip })
            .collect()
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

    fn extras(temp: f32) -> ChipExtras {
        ChipExtras {
            temp_c: Some(temp),
            ..ChipExtras::default()
        }
    }

    #[test]
    fn every_trigger_has_a_chip_and_a_reason() {
        for (t, reason) in [
            (Trigger::LoadRise { chip: 3 }, Reason::Chip),
            (Trigger::PcieBurst { chip: 3 }, Reason::Chip),
            (Trigger::ThermalRise { chip: 3 }, Reason::Chip),
            (Trigger::MemoryHeat { chip: 3 }, Reason::Memory),
            (Trigger::EthLink { chip: 3 }, Reason::Board),
            (Trigger::Process { chip: 3 }, Reason::Process),
        ] {
            assert_eq!((t.chip(), t.reason()), (3, reason), "{t:?}");
        }
    }

    #[test]
    fn a_temperature_rise_fires_once_and_rearms_below_the_band() {
        let mut d = TriggerDetector::new();
        let rise = vec![Trigger::ThermalRise { chip: 0 }];
        assert!(d.observe_extras(0.0, 0, &extras(45.0)).is_empty()); // baseline
        assert!(d.observe_extras(1.0, 0, &extras(48.0)).is_empty()); // +3: under 4
        assert_eq!(d.observe_extras(2.0, 0, &extras(50.0)), rise);
        assert!(
            d.observe_extras(3.0, 0, &extras(55.0)).is_empty(),
            "held high: once"
        );
        // Back to the baseline, then a new rise: fires again.
        assert!(d.observe_extras(4.0, 0, &extras(45.5)).is_empty());
        assert_eq!(d.observe_extras(5.0, 0, &extras(52.0)), rise);
    }

    #[test]
    fn a_slow_warm_up_becomes_the_baseline_and_does_not_fire() {
        let mut d = TriggerDetector::new();
        d.observe_extras(0.0, 0, &extras(45.0));
        // 0.05 C a second for ten minutes: 30 C in all, never 4 C above the
        // baseline that follows it.
        let mut fired = 0;
        for i in 1..=600 {
            fired += d
                .observe_extras(i as f64, 0, &extras(45.0 + 0.05 * i as f32))
                .len();
        }
        assert_eq!(fired, 0, "a slow rise is not an event");
    }

    #[test]
    fn missing_or_non_finite_temperature_never_fires_or_disarms() {
        let mut d = TriggerDetector::new();
        d.observe_extras(0.0, 0, &extras(45.0));
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(d.observe_extras(1.0, 0, &extras(bad)).is_empty(), "{bad}");
        }
        assert!(d.observe_extras(1.5, 0, &ChipExtras::default()).is_empty());
        assert_eq!(
            d.observe_extras(2.0, 0, &extras(51.0)).len(),
            1,
            "still armed"
        );
        // A first reading that is already high is the baseline, not a rise.
        let mut d = TriggerDetector::new();
        assert!(d.observe_extras(0.0, 7, &extras(85.0)).is_empty());
        assert!(d.observe_extras(1.0, 7, &extras(85.5)).is_empty());
    }

    #[test]
    fn memory_heat_fires_at_65_c_after_cooling() {
        let mut d = TriggerDetector::new();
        let heat = |h: f32| ChipExtras {
            gddr_heat: Some(h),
            ..ChipExtras::default()
        };
        let fire = vec![Trigger::MemoryHeat { chip: 1 }];
        assert!(d.observe_extras(0.0, 1, &heat(0.2)).is_empty()); // armed
        assert!(d.observe_extras(1.0, 1, &heat(0.49)).is_empty());
        assert_eq!(d.observe_extras(2.0, 1, &heat(MEMORY_ON)), fire);
        assert!(d.observe_extras(3.0, 1, &heat(0.9)).is_empty());
        assert!(
            d.observe_extras(4.0, 1, &heat(0.4)).is_empty(),
            "between the levels"
        );
        assert!(
            d.observe_extras(5.0, 1, &heat(0.8)).is_empty(),
            "not re-armed yet"
        );
        d.observe_extras(6.0, 1, &heat(MEMORY_REARM));
        assert_eq!(d.observe_extras(7.0, 1, &heat(0.8)), fire);
        // The first reading is never a rise.
        let mut d = TriggerDetector::new();
        assert!(d.observe_extras(0.0, 2, &heat(0.9)).is_empty());
    }

    #[test]
    fn an_ethernet_link_change_fires_but_the_first_mask_does_not() {
        let mut d = TriggerDetector::new();
        let eth = |m: u64| ChipExtras {
            eth_live: Some(m),
            ..ChipExtras::default()
        };
        let fire = vec![Trigger::EthLink { chip: 0 }];
        assert!(d.observe_extras(0.0, 0, &eth(0b0011)).is_empty());
        assert!(
            d.observe_extras(1.0, 0, &eth(0b0011)).is_empty(),
            "unchanged"
        );
        assert_eq!(
            d.observe_extras(2.0, 0, &eth(0b0111)),
            fire,
            "a link came up"
        );
        assert_eq!(
            d.observe_extras(3.0, 0, &eth(0b0101)),
            fire,
            "one went down"
        );
        assert!(d.observe_extras(4.0, 0, &ChipExtras::default()).is_empty());
        assert!(
            d.observe_extras(5.0, 0, &eth(0b0101)).is_empty(),
            "a missing reading changed nothing"
        );
    }

    #[test]
    fn a_process_opening_or_closing_a_device_fires_for_that_device() {
        let mut d = TriggerDetector::new();
        // The first set is the starting point, however many are open.
        assert!(d.observe_processes(&[(100, 0), (101, 1)]).is_empty());
        assert!(
            d.observe_processes(&[(100, 0), (101, 1)]).is_empty(),
            "unchanged"
        );
        // Pid 102 opens device 1: one trigger, for device 1.
        assert_eq!(
            d.observe_processes(&[(100, 0), (101, 1), (102, 1)]),
            vec![Trigger::Process { chip: 1 }]
        );
        // Pid 100 closes device 0 while 103 opens device 1: both devices fire.
        assert_eq!(
            d.observe_processes(&[(101, 1), (102, 1), (103, 1), (104, 2)]),
            vec![
                Trigger::Process { chip: 0 },
                Trigger::Process { chip: 1 },
                Trigger::Process { chip: 2 },
            ]
        );
        // Two processes changing the same device fire once.
        assert_eq!(d.observe_processes(&[(101, 1)]).len(), 2, "devices 1 and 2");
        assert_eq!(
            d.observe_processes(&[(200, 1), (201, 1)]),
            vec![Trigger::Process { chip: 1 }]
        );
    }
}
