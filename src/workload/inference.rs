// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Inference state classification for Tenstorrent hardware.
//!
//! Maintains a rolling 30-sample history per device and classifies each
//! device's hardware state into one of 13 evocative states using power
//! variance, temperature, AICLK, throttler registers, and ARC health.

use crate::animation::baseline::AdaptiveBaseline;
use ratatui::style::Color;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// One sample in the rolling history kept per device.
#[derive(Debug, Clone)]
pub struct TelemetrySample {
    pub power:     f32,
    pub temp:      f32,
    pub current:   f32,
    pub aiclk:     Option<u32>,
    pub timestamp: Instant,
}

/// Rolling-window power statistics over the last N samples.
#[derive(Debug, Clone, Default)]
pub struct PowerTrend {
    /// W/s (positive = rising). Zero when fewer than 2 samples.
    pub slope:       f32,
    /// Standard deviation (not raw variance) of power in last 10 samples.
    pub variance:    f32,
    /// True when last 8 samples are monotonically rising.
    pub is_rising:   bool,
    /// True when last 8 samples are monotonically falling.
    pub is_falling:  bool,
    /// Max power in the window.
    pub peak:        f32,
}

/// Classified hardware activity state (13 states, priority-ordered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceInferenceState {
    Stalled,         // ARC counters frozen or heartbeat == 0
    HardwareFault,   // faults register non-empty
    ThermalStress,   // temp > 82°C
    Throttling,      // throttler set, or AICLK dropped >20% while power medium-high
    FabricDegraded,  // eth_status error and device_count > 1
    MaxedOut,        // power > 85% TDP, or power_change > 1.5
    WarmingUp,       // rising trend, was near baseline 20 samples ago
    CoolingDown,     // falling from high peak
    Thinking,        // active, high-variance (prefill / attention)
    Generating,      // active, low-variance, high current:power (decode)
    MovingData,      // current_change >> power_change
    Idle,            // all within 15% of baseline
    Unclear,         // active but no pattern
}

/// Confidence in the classified state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// Classification result for one device, ready for rendering.
#[derive(Debug, Clone)]
pub struct InferenceResult {
    pub state:              DeviceInferenceState,
    pub confidence:         Confidence,
    /// Bold ALL-CAPS label, max 16 chars.
    pub label:              &'static str,
    /// Ratatui color for the label.
    pub label_color:        Color,
    /// Fixed-format metrics line: `"18.0W   62.1°C  1100MHz   +50%"`
    pub headline:           String,
    /// Fixed-format interpretation line: `"humming   rivers of data   24% TDP"`
    pub detail:             String,
    /// Fill fraction for power bar: `Some(0.24)` if TDP known.
    pub power_pct_of_tdp:   Option<f32>,
    /// Fill fraction fallback: power / (2 * baseline_power).
    pub power_pct_baseline: f32,
}

/// State and label table (matches spec § State display).
const STATE_META: &[(DeviceInferenceState, &str, Color)] = &[
    (DeviceInferenceState::Stalled,        "GONE SILENT",     Color::Red),
    (DeviceInferenceState::HardwareFault,  "IN DISTRESS",     Color::Red),
    (DeviceInferenceState::ThermalStress,  "SCORCHING",       Color::Rgb(255, 80, 0)),
    (DeviceInferenceState::Throttling,     "REINED IN",       Color::Rgb(255, 140, 0)),
    (DeviceInferenceState::FabricDegraded, "LOST THE THREAD", Color::Rgb(255, 140, 0)),
    (DeviceInferenceState::MaxedOut,       "BURNING BRIGHT",  Color::Rgb(220, 60, 0)),
    (DeviceInferenceState::WarmingUp,      "AWAKENING",       Color::Yellow),
    (DeviceInferenceState::CoolingDown,    "EXHALING",        Color::Cyan),
    (DeviceInferenceState::Thinking,       "DREAMING DEEP",   Color::Rgb(160, 80, 255)),
    (DeviceInferenceState::Generating,     "CONJURING",       Color::Green),
    (DeviceInferenceState::MovingData,     "IN FULL FLOW",    Color::Blue),
    (DeviceInferenceState::Idle,           "BREATHING",       Color::DarkGray),
    (DeviceInferenceState::Unclear,        "INSCRUTABLE",     Color::Rgb(160, 160, 40)),
];

/// Return the ALL-CAPS label string for a given state.
pub fn state_label(state: DeviceInferenceState) -> &'static str {
    STATE_META.iter()
        .find(|(s, _, _)| *s == state)
        .map(|(_, l, _)| *l)
        .unwrap_or("UNKNOWN")
}

/// Return the ratatui display color for a given state.
pub fn state_color(state: DeviceInferenceState) -> Color {
    STATE_META.iter()
        .find(|(s, _, _)| *s == state)
        .map(|(_, _, c)| *c)
        .unwrap_or(Color::White)
}

/// Maintains rolling history and classifies hardware state per device.
pub struct InferenceEngine {
    /// 30-sample rolling window per device index.
    pub history:  HashMap<usize, VecDeque<TelemetrySample>>,
    /// Last seen ARC health counters for stall detection.
    /// Updated by `ingest()` after reading SMBUS arc health values;
    /// compared against current values in `classify()` to detect frozen counters.
    pub prev_arc: HashMap<usize, [Option<u32>; 4]>,
    /// Shared adaptive baseline (learns idle power/current/temp).
    pub baseline: AdaptiveBaseline,
}

impl InferenceEngine {
    /// Create a new engine with empty history.
    pub fn new() -> Self {
        Self {
            history:  HashMap::new(),
            prev_arc: HashMap::new(),
            baseline: AdaptiveBaseline::new(),
        }
    }
}

impl Default for InferenceEngine {
    fn default() -> Self { Self::new() }
}
