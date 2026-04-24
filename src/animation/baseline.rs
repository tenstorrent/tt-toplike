// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Adaptive baseline learning system
//!
//! This module implements the adaptive baseline that makes visualizations universally
//! sensitive to hardware activity. By learning each device's idle state over 20 samples,
//! we can show activity **relative to baseline** rather than using fixed thresholds.
//!
//! This solves the fundamental problem: every system has different idle/active ranges.
//! A 10% power increase from 20W→22W generates the same visual response as 50W→55W.

use std::collections::HashMap;

/// Adaptive baseline tracker for a single device
///
/// Learns hardware idle state over initial samples, then provides relative
/// change calculations for all telemetry metrics.
#[derive(Debug, Clone)]
pub struct DeviceBaseline {
    /// Average power consumption during idle state (watts)
    pub power_baseline: f32,

    /// Average current draw during idle state (amps)
    pub current_baseline: f32,

    /// Average temperature during idle state (celsius)
    pub temp_baseline: f32,

    /// Average AICLK frequency during idle state (MHz)
    pub aiclk_baseline: f32,

    /// Number of samples collected so far
    samples_collected: usize,

    /// Sum of power samples (for averaging)
    power_sum: f32,

    /// Sum of current samples (for averaging)
    current_sum: f32,

    /// Sum of temperature samples (for averaging)
    temp_sum: f32,

    /// Sum of AICLK samples (for averaging)
    aiclk_sum: f32,
}

impl DeviceBaseline {
    /// Create a new device baseline tracker
    pub fn new() -> Self {
        Self {
            power_baseline: 0.0,
            current_baseline: 0.0,
            temp_baseline: 0.0,
            aiclk_baseline: 0.0,
            samples_collected: 0,
            power_sum: 0.0,
            current_sum: 0.0,
            temp_sum: 0.0,
            aiclk_sum: 0.0,
        }
    }

    /// Add a sample to the baseline calculation
    ///
    /// Collects samples until 20 are gathered, then computes averages.
    ///
    /// # Arguments
    ///
    /// * `power` - Power consumption in watts
    /// * `current` - Current draw in amps
    /// * `temp` - Temperature in celsius
    /// * `aiclk` - AICLK frequency in MHz
    pub fn add_sample(&mut self, power: f32, current: f32, temp: f32, aiclk: f32) {
        if self.samples_collected < 20 {
            self.power_sum += power;
            self.current_sum += current;
            self.temp_sum += temp;
            self.aiclk_sum += aiclk;
            self.samples_collected += 1;

            // Calculate averages after 20 samples
            if self.samples_collected == 20 {
                self.power_baseline = self.power_sum / 20.0;
                self.current_baseline = self.current_sum / 20.0;
                self.temp_baseline = self.temp_sum / 20.0;
                self.aiclk_baseline = self.aiclk_sum / 20.0;
            }
        }
    }

    /// Check if baseline is established (20 samples collected)
    pub fn is_established(&self) -> bool {
        self.samples_collected >= 20
    }

    /// Get learning progress (0.0 to 1.0)
    pub fn progress(&self) -> f32 {
        (self.samples_collected as f32) / 20.0
    }

    /// Calculate relative change from baseline
    ///
    /// Returns a value where:
    /// - 0.0 = at baseline
    /// - 0.1 = 10% increase from baseline
    /// - 1.0 = 100% increase (double baseline)
    /// - -0.5 = 50% decrease from baseline
    ///
    /// # Arguments
    ///
    /// * `current_value` - Current measurement
    /// * `baseline_value` - Baseline measurement
    ///
    /// # Returns
    ///
    /// Relative change as a ratio (e.g., 0.15 = 15% increase)
    pub fn relative_change(current_value: f32, baseline_value: f32) -> f32 {
        if baseline_value <= 0.0 {
            return 0.0; // Avoid division by zero
        }
        (current_value - baseline_value) / baseline_value
    }

    /// Get relative power change from baseline
    pub fn power_change(&self, current_power: f32) -> f32 {
        if !self.is_established() {
            return 0.0;
        }
        Self::relative_change(current_power, self.power_baseline)
    }

    /// Get relative current change from baseline
    pub fn current_change(&self, current_current: f32) -> f32 {
        if !self.is_established() {
            return 0.0;
        }
        Self::relative_change(current_current, self.current_baseline)
    }

    /// Get relative temperature change from baseline
    pub fn temp_change(&self, current_temp: f32) -> f32 {
        if !self.is_established() {
            return 0.0;
        }
        Self::relative_change(current_temp, self.temp_baseline)
    }

    /// Get relative AICLK change from baseline
    pub fn aiclk_change(&self, current_aiclk: f32) -> f32 {
        if !self.is_established() {
            return 0.0;
        }
        Self::relative_change(current_aiclk, self.aiclk_baseline)
    }
}

impl Default for DeviceBaseline {
    fn default() -> Self {
        Self::new()
    }
}

/// Adaptive baseline system for all devices
///
/// Manages baseline learning for multiple devices, providing a unified
/// interface for relative activity detection.
pub struct AdaptiveBaseline {
    /// Per-device baseline trackers
    device_baselines: HashMap<usize, DeviceBaseline>,

    /// Overall system baseline established flag
    all_established: bool,
}

impl AdaptiveBaseline {
    /// Create a new adaptive baseline system
    pub fn new() -> Self {
        Self {
            device_baselines: HashMap::new(),
            all_established: false,
        }
    }

    /// Update baseline with current telemetry
    ///
    /// Call this every update cycle during the learning phase.
    ///
    /// # Arguments
    ///
    /// * `device_idx` - Device index
    /// * `power` - Current power consumption (watts)
    /// * `current` - Current draw (amps)
    /// * `temp` - Temperature (celsius)
    /// * `aiclk` - AICLK frequency (MHz)
    pub fn update(&mut self, device_idx: usize, power: f32, current: f32, temp: f32, aiclk: f32) {
        let baseline = self.device_baselines
            .entry(device_idx)
            .or_insert_with(DeviceBaseline::new);

        baseline.add_sample(power, current, temp, aiclk);

        // Check if all devices have established baselines
        self.all_established = self.device_baselines
            .values()
            .all(|b| b.is_established());
    }

    /// Check if all device baselines are established
    pub fn is_established(&self) -> bool {
        self.all_established && !self.device_baselines.is_empty()
    }

    /// Get overall learning progress (0.0 to 1.0)
    ///
    /// Returns the minimum progress across all devices.
    pub fn progress(&self) -> f32 {
        if self.device_baselines.is_empty() {
            return 0.0;
        }

        self.device_baselines
            .values()
            .map(|b| b.progress())
            .min_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap_or(0.0)
    }

    /// Get samples collected for a device
    pub fn samples_collected(&self, device_idx: usize) -> usize {
        self.device_baselines
            .get(&device_idx)
            .map(|b| b.samples_collected)
            .unwrap_or(0)
    }

    /// Get baseline for a specific device
    pub fn get_baseline(&self, device_idx: usize) -> Option<&DeviceBaseline> {
        self.device_baselines.get(&device_idx)
    }

    /// Get relative power change for a device
    pub fn power_change(&self, device_idx: usize, current_power: f32) -> f32 {
        self.device_baselines
            .get(&device_idx)
            .map(|b| b.power_change(current_power))
            .unwrap_or(0.0)
    }

    /// Get relative current change for a device
    pub fn current_change(&self, device_idx: usize, current_current: f32) -> f32 {
        self.device_baselines
            .get(&device_idx)
            .map(|b| b.current_change(current_current))
            .unwrap_or(0.0)
    }

    /// Get relative temperature change for a device
    pub fn temp_change(&self, device_idx: usize, current_temp: f32) -> f32 {
        self.device_baselines
            .get(&device_idx)
            .map(|b| b.temp_change(current_temp))
            .unwrap_or(0.0)
    }

    /// Get maximum activity level across all devices
    ///
    /// Returns the highest relative change (power or current) across all devices.
    /// Useful for determining system-wide workload detection.
    pub fn max_activity(&self) -> f32 {
        if !self.is_established() {
            return 0.0;
        }

        // We'll need current telemetry to calculate this properly
        // For now, return 0.0 as placeholder
        0.0
    }

    /// Check if workload is detected (>20% activity increase)
    ///
    /// Returns true if any device shows >20% increase in power or current.
    pub fn workload_detected(&self, device_idx: usize, current_power: f32, current_current: f32) -> bool {
        if !self.is_established() {
            return false;
        }

        let power_change = self.power_change(device_idx, current_power);
        let current_change = self.current_change(device_idx, current_current);

        power_change > 0.20 || current_change > 0.20
    }
}

impl Default for AdaptiveBaseline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_baseline_learning() {
        let mut baseline = DeviceBaseline::new();

        assert!(!baseline.is_established());
        assert_eq!(baseline.progress(), 0.0);

        // Add 20 samples at 50W, 20A, 30°C, 800MHz
        for _ in 0..20 {
            baseline.add_sample(50.0, 20.0, 30.0, 800.0);
        }

        assert!(baseline.is_established());
        assert_eq!(baseline.progress(), 1.0);
        assert_eq!(baseline.power_baseline, 50.0);
        assert_eq!(baseline.current_baseline, 20.0);
        assert_eq!(baseline.temp_baseline, 30.0);
        assert_eq!(baseline.aiclk_baseline, 800.0);
    }

    #[test]
    fn test_relative_change_calculation() {
        let mut baseline = DeviceBaseline::new();

        // Establish baseline at 50W
        for _ in 0..20 {
            baseline.add_sample(50.0, 20.0, 30.0, 800.0);
        }

        // Test 10% increase
        assert!((baseline.power_change(55.0) - 0.10).abs() < 0.01);

        // Test 50% increase
        assert!((baseline.power_change(75.0) - 0.50).abs() < 0.01);

        // Test 100% increase (double)
        assert!((baseline.power_change(100.0) - 1.0).abs() < 0.01);

        // Test decrease
        assert!((baseline.power_change(45.0) + 0.10).abs() < 0.01);
    }

    #[test]
    fn test_adaptive_baseline_system() {
        let mut baseline = AdaptiveBaseline::new();

        assert!(!baseline.is_established());

        // Add samples for device 0
        for _ in 0..20 {
            baseline.update(0, 50.0, 20.0, 30.0, 800.0);
        }

        assert!(baseline.is_established());
        assert_eq!(baseline.samples_collected(0), 20);

        // Test workload detection
        assert!(!baseline.workload_detected(0, 55.0, 22.0)); // 10% increase - below threshold
        assert!(baseline.workload_detected(0, 65.0, 26.0));  // 30% increase - above threshold
    }
}
