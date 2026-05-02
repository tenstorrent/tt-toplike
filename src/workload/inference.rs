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

/// Compute rolling-window statistics for the most recent 10 power samples.
pub fn compute_power_trend(history: &VecDeque<TelemetrySample>) -> PowerTrend {
    let n = history.len().min(10);
    if n < 2 {
        return PowerTrend::default();
    }

    // Take last `n` samples.
    let samples: Vec<f32> = history.iter().rev().take(n).map(|s| s.power).collect();
    // `samples[0]` = most recent, `samples[n-1]` = oldest in window.

    let peak = samples.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    // Standard deviation over last 10 samples.
    let mean = samples.iter().sum::<f32>() / n as f32;
    let variance = (samples.iter().map(|&p| (p - mean).powi(2)).sum::<f32>() / n as f32).sqrt();

    // Slope: simple linear regression across time.
    // x = 0..n-1 (oldest to newest); we reversed above so flip.
    let xs: Vec<f32> = (0..n).map(|i| i as f32).collect();
    let ys: Vec<f32> = samples.iter().rev().cloned().collect(); // oldest first
    let x_mean = xs.iter().sum::<f32>() / n as f32;
    let y_mean = ys.iter().sum::<f32>() / n as f32;
    let num: f32 = xs.iter().zip(ys.iter()).map(|(&x, &y)| (x - x_mean) * (y - y_mean)).sum();
    let den: f32 = xs.iter().map(|&x| (x - x_mean).powi(2)).sum();
    let slope = if den > 0.0 { num / den } else { 0.0 };

    // Monotone checks over last 8 samples.
    let check_n = n.min(8);
    let recent: Vec<f32> = history.iter().rev().take(check_n).map(|s| s.power).collect();
    let is_rising  = recent.windows(2).all(|w| w[0] >= w[1]); // [0]=newer, [1]=older
    let is_falling = recent.windows(2).all(|w| w[0] <= w[1]);

    PowerTrend { slope, variance, is_rising, is_falling, peak }
}

/// Parse a TDP string (decimal watts or hex) into watts.
/// Returns `None` if the string is absent, zero, or unparseable.
pub fn parse_tdp_watts(tdp: Option<&str>) -> Option<f32> {
    let s = tdp?.trim();
    let raw = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()? as f32
    } else {
        s.parse::<f32>().ok()?
    };
    if raw > 0.0 { Some(raw) } else { None }
}

/// Parse all four ARC health counter strings into `[Option<u32>; 4]`.
pub fn parse_arc_health_counters(raw: [Option<String>; 4]) -> [Option<u32>; 4] {
    raw.map(|opt| {
        opt.as_deref().and_then(|s| {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                s.parse().ok()
            }
        })
    })
}

/// True if the throttler register indicates throttling is active.
/// "0", "0x0", "", or None → not throttling.
pub fn is_throttler_active(throttler: Option<&str>) -> bool {
    match throttler {
        None | Some("") | Some("0") | Some("0x0") | Some("0X0") => false,
        Some(_) => true,
    }
}

/// True if an eth_status register indicates link/fabric errors.
/// "0", "0x0", "", or None → no error.
pub fn is_eth_status_error(eth_status: Option<&str>) -> bool {
    match eth_status {
        None | Some("") | Some("0") | Some("0x0") | Some("0X0") => false,
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sample(power: f32) -> TelemetrySample {
        TelemetrySample { power, temp: 50.0, current: 10.0, aiclk: Some(1000), timestamp: Instant::now() }
    }

    #[test]
    fn power_trend_rising() {
        let samples: VecDeque<TelemetrySample> = (0..10)
            .map(|i| make_sample(10.0 + i as f32))
            .collect();
        let trend = compute_power_trend(&samples);
        assert!(trend.is_rising, "monotonically increasing should be rising");
        assert!(!trend.is_falling);
        assert!(trend.slope > 0.0);
    }

    #[test]
    fn power_trend_falling() {
        let samples: VecDeque<TelemetrySample> = (0..10)
            .map(|i| make_sample(20.0 - i as f32))
            .collect();
        let trend = compute_power_trend(&samples);
        assert!(trend.is_falling);
        assert!(!trend.is_rising);
    }

    #[test]
    fn power_trend_variance_stable() {
        let samples: VecDeque<TelemetrySample> = (0..10)
            .map(|_| make_sample(15.0))
            .collect();
        let trend = compute_power_trend(&samples);
        assert!(trend.variance < 0.01, "constant power has near-zero variance");
    }

    #[test]
    fn power_trend_variance_volatile() {
        let samples: VecDeque<TelemetrySample> = (0..10)
            .enumerate()
            .map(|(i, _)| make_sample(if i % 2 == 0 { 10.0 } else { 30.0 }))
            .collect();
        let trend = compute_power_trend(&samples);
        assert!(trend.variance > 5.0, "alternating ±10W should have high variance");
    }

    #[test]
    fn parse_tdp_watts_decimal() {
        assert_eq!(parse_tdp_watts(Some("300")), Some(300.0_f32));
    }

    #[test]
    fn parse_tdp_watts_hex() {
        assert_eq!(parse_tdp_watts(Some("0x12c")), Some(300.0_f32));
    }

    #[test]
    fn parse_tdp_watts_none() {
        assert_eq!(parse_tdp_watts(None), None);
    }

    #[test]
    fn parse_arc_health_zero_is_stalled() {
        let counters = parse_arc_health_counters([
            Some("0".to_string()), None, None, None
        ]);
        assert_eq!(counters[0], Some(0));
    }

    #[test]
    fn parse_arc_health_hex() {
        let counters = parse_arc_health_counters([
            Some("0x10e7a".to_string()), None, None, None
        ]);
        assert_eq!(counters[0], Some(0x10e7a));
    }

    #[test]
    fn throttler_active() {
        assert!(is_throttler_active(Some("0x1")));
        assert!(!is_throttler_active(Some("0")));
        assert!(!is_throttler_active(Some("0x0")));
        assert!(!is_throttler_active(None));
    }

    #[test]
    fn eth_status_error() {
        assert!(is_eth_status_error(Some("0x1")));
        assert!(!is_eth_status_error(Some("0")));
        assert!(!is_eth_status_error(Some("0x0")));
        assert!(!is_eth_status_error(None));
    }
}
