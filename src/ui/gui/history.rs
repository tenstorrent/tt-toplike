// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Historical telemetry tracking for GUI charts
//!
//! This module provides efficient circular buffers for tracking telemetry
//! history, used for rendering line charts and responsive visualizations.

use std::collections::VecDeque;
use crate::models::Telemetry;

/// Maximum number of historical samples to keep per device
const MAX_HISTORY_SAMPLES: usize = 300; // 30 seconds at 100ms interval

/// Historical telemetry data for a single device
#[derive(Debug, Clone)]
pub struct TelemetryHistory {
    /// Device index
    pub device_idx: usize,

    /// Power history (watts)
    pub power: VecDeque<f32>,

    /// Temperature history (celsius)
    pub temperature: VecDeque<f32>,

    /// Current history (amps)
    pub current: VecDeque<f32>,

    /// Voltage history (volts)
    pub voltage: VecDeque<f32>,

    /// AICLK history (MHz)
    pub aiclk: VecDeque<u32>,

    /// Timestamps (for X-axis)
    pub timestamps: VecDeque<f64>,
}

impl TelemetryHistory {
    /// Create a new history tracker for a device
    pub fn new(device_idx: usize) -> Self {
        Self {
            device_idx,
            power: VecDeque::with_capacity(MAX_HISTORY_SAMPLES),
            temperature: VecDeque::with_capacity(MAX_HISTORY_SAMPLES),
            current: VecDeque::with_capacity(MAX_HISTORY_SAMPLES),
            voltage: VecDeque::with_capacity(MAX_HISTORY_SAMPLES),
            aiclk: VecDeque::with_capacity(MAX_HISTORY_SAMPLES),
            timestamps: VecDeque::with_capacity(MAX_HISTORY_SAMPLES),
        }
    }

    /// Add a new telemetry sample
    pub fn push(&mut self, telem: &Telemetry, timestamp: f64) {
        // Add new values
        self.power.push_back(telem.power.unwrap_or(0.0));
        self.temperature.push_back(telem.asic_temperature.unwrap_or(0.0));
        self.current.push_back(telem.current.unwrap_or(0.0));
        self.voltage.push_back(telem.voltage.unwrap_or(0.0));
        self.aiclk.push_back(telem.aiclk.unwrap_or(0));
        self.timestamps.push_back(timestamp);

        // Remove old values if we exceed max capacity
        if self.power.len() > MAX_HISTORY_SAMPLES {
            self.power.pop_front();
            self.temperature.pop_front();
            self.current.pop_front();
            self.voltage.pop_front();
            self.aiclk.pop_front();
            self.timestamps.pop_front();
        }
    }

    /// Get the latest value for a metric
    pub fn latest_power(&self) -> f32 {
        self.power.back().copied().unwrap_or(0.0)
    }

    pub fn latest_temp(&self) -> f32 {
        self.temperature.back().copied().unwrap_or(0.0)
    }

    pub fn latest_current(&self) -> f32 {
        self.current.back().copied().unwrap_or(0.0)
    }

    /// Get min/max for a metric (for chart scaling)
    pub fn power_range(&self) -> (f32, f32) {
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for &val in &self.power {
            if val < min { min = val; }
            if val > max { max = val; }
        }
        (min, max)
    }

    pub fn temp_range(&self) -> (f32, f32) {
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for &val in &self.temperature {
            if val < min { min = val; }
            if val > max { max = val; }
        }
        (min, max)
    }

    /// Get the number of samples
    pub fn len(&self) -> usize {
        self.power.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.power.is_empty()
    }

    /// Clear all history
    pub fn clear(&mut self) {
        self.power.clear();
        self.temperature.clear();
        self.current.clear();
        self.voltage.clear();
        self.aiclk.clear();
        self.timestamps.clear();
    }
}

/// Manager for all device histories
#[derive(Debug, Clone)]
pub struct HistoryManager {
    /// Histories for all devices (indexed by device index)
    histories: Vec<TelemetryHistory>,

    /// Start time (for relative timestamps)
    start_time: std::time::Instant,
}

impl HistoryManager {
    /// Create a new history manager
    pub fn new() -> Self {
        Self {
            histories: Vec::new(),
            start_time: std::time::Instant::now(),
        }
    }

    /// Ensure we have history trackers for N devices
    pub fn ensure_capacity(&mut self, device_count: usize) {
        while self.histories.len() < device_count {
            let idx = self.histories.len();
            self.histories.push(TelemetryHistory::new(idx));
        }
    }

    /// Add telemetry for a device
    pub fn push(&mut self, device_idx: usize, telem: &Telemetry) {
        // Ensure we have a history for this device
        while self.histories.len() <= device_idx {
            let idx = self.histories.len();
            self.histories.push(TelemetryHistory::new(idx));
        }

        // Calculate relative timestamp in seconds
        let timestamp = self.start_time.elapsed().as_secs_f64();

        // Push to history
        self.histories[device_idx].push(telem, timestamp);
    }

    /// Get history for a device
    pub fn get(&self, device_idx: usize) -> Option<&TelemetryHistory> {
        self.histories.get(device_idx)
    }

    /// Get mutable history for a device
    pub fn get_mut(&mut self, device_idx: usize) -> Option<&mut TelemetryHistory> {
        self.histories.get_mut(device_idx)
    }

    /// Clear all histories
    pub fn clear(&mut self) {
        for history in &mut self.histories {
            history.clear();
        }
        self.start_time = std::time::Instant::now();
    }
}

impl Default for HistoryManager {
    fn default() -> Self {
        Self::new()
    }
}
