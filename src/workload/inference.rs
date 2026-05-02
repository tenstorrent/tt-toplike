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

    /// Push a new telemetry sample for `device_idx` into the rolling history.
    ///
    /// Also updates the shared `AdaptiveBaseline` for this device.
    pub fn ingest(
        &mut self,
        device_idx: usize,
        power: f32,
        temp: f32,
        current: f32,
        aiclk: Option<u32>,
    ) {
        // Update baseline (learns idle state over first 20 samples)
        self.baseline.update(device_idx, power, current, temp, aiclk.unwrap_or(0) as f32);

        let sample = TelemetrySample {
            power, temp, current, aiclk,
            timestamp: Instant::now(),
        };
        let deque = self.history.entry(device_idx).or_insert_with(VecDeque::new);
        deque.push_back(sample);
        while deque.len() > 30 {
            deque.pop_front();
        }
    }

    /// Classify the current state of `device_idx`.
    ///
    /// # Arguments
    /// * `device_idx` — device index (0-based)
    /// * `smbus` — optional SMBUS telemetry for richer classification
    /// * `arc_counters` — optional prev [arc0..arc3] health counter values (for stall detection)
    /// * `device_count` — total number of devices (for FabricDegraded check)
    pub fn classify(
        &self,
        device_idx: usize,
        smbus: Option<&crate::models::telemetry::SmbusTelemetry>,
        arc_counters: Option<&[Option<u32>; 4]>,
        device_count: usize,
    ) -> InferenceResult {
        let history = match self.history.get(&device_idx) {
            Some(h) if !h.is_empty() => h,
            _ => return self.unknown_result(),
        };

        let latest = history.back().unwrap();
        let power   = latest.power;
        let temp    = latest.temp;
        let current = latest.current;
        let aiclk   = latest.aiclk;

        let power_change   = self.baseline.power_change(device_idx, power);
        let current_change = self.baseline.current_change(device_idx, current);
        let aiclk_change   = self.baseline.get_baseline(device_idx)
            .map(|b| b.aiclk_change(aiclk.unwrap_or(0) as f32))
            .unwrap_or(0.0);

        let trend = compute_power_trend(history);

        // Parse SMBUS fields
        let tdp_w = smbus.and_then(|s| parse_tdp_watts(s.tdp.as_deref()));
        let throttler_on = smbus.map(|s| is_throttler_active(s.throttler.as_deref())).unwrap_or(false);
        let eth0_err = smbus.map(|s| is_eth_status_error(s.eth_status0.as_deref())).unwrap_or(false);
        let eth1_err = smbus.map(|s| is_eth_status_error(s.eth_status1.as_deref())).unwrap_or(false);
        let has_faults = smbus.map(|s| {
            matches!(s.faults.as_deref(), Some(f) if f != "0" && f != "0x0" && !f.is_empty())
        }).unwrap_or(false);

        // Baseline power (for bar fallback)
        let baseline_power = self.baseline.get_baseline(device_idx)
            .map(|b| b.power_baseline)
            .unwrap_or(1.0)
            .max(1.0);

        // Power bar fraction
        let power_pct_of_tdp = tdp_w.map(|tdp| (power / tdp).min(1.0));
        let power_pct_baseline = (power / (2.0 * baseline_power)).min(1.0);

        // ── Priority-ordered classification ──────────────────────────────

        // 1. Stalled — ARC health counters frozen
        let stalled = {
            let arc_frozen = arc_counters.map(|prev| {
                let current_counts = smbus.map(|s| parse_arc_health_counters([
                    s.arc0_health.clone(), s.arc1_health.clone(),
                    s.arc2_health.clone(), s.arc3_health.clone(),
                ]));
                match current_counts {
                    Some(cur) => {
                        // Stalled if ANY counter that previously existed hasn't moved
                        prev.iter().zip(cur.iter()).any(|(p, c)| {
                            matches!((p, c), (Some(pv), Some(cv)) if pv == cv)
                        })
                    }
                    None => false,
                }
            }).unwrap_or(false);
            arc_frozen
        };
        if stalled {
            return self.build_result(
                DeviceInferenceState::Stalled, Confidence::High,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 2. HardwareFault
        if has_faults {
            return self.build_result(
                DeviceInferenceState::HardwareFault, Confidence::High,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 3. ThermalStress
        if temp > 82.0 {
            return self.build_result(
                DeviceInferenceState::ThermalStress, Confidence::High,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 4. Throttling — direct register OR AICLK dropped >20% while power medium-high
        let aiclk_throttled = aiclk_change < -0.20 && power_change > 0.3;
        if throttler_on || aiclk_throttled {
            return self.build_result(
                DeviceInferenceState::Throttling, Confidence::High,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 5. FabricDegraded — eth error AND multi-chip system
        if (eth0_err || eth1_err) && device_count > 1 {
            return self.build_result(
                DeviceInferenceState::FabricDegraded, Confidence::Medium,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 6. MaxedOut — power > 85% TDP, or power_change > 1.5
        let maxed_by_tdp = power_pct_of_tdp.map(|p| p > 0.85).unwrap_or(false);
        if maxed_by_tdp || power_change > 1.5 {
            return self.build_result(
                DeviceInferenceState::MaxedOut, Confidence::High,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 7. WarmingUp — rising AND recently near baseline
        let recently_near_baseline = history.len() >= 20 && {
            let old_idx = history.len().saturating_sub(20);
            let old_power = history.iter().nth(old_idx).map(|s| s.power).unwrap_or(power);
            let baseline_power_val = self.baseline.get_baseline(device_idx)
                .map(|b| b.power_baseline).unwrap_or(0.0);
            baseline_power_val > 0.0 && (old_power / baseline_power_val - 1.0).abs() < 0.15
        };
        if trend.is_rising && power_change > 0.2 && recently_near_baseline {
            return self.build_result(
                DeviceInferenceState::WarmingUp, Confidence::Medium,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 8. CoolingDown — falling AND peak was high
        let baseline_power_val = self.baseline.get_baseline(device_idx)
            .map(|b| b.power_baseline).unwrap_or(0.0);
        let peak_was_high = baseline_power_val > 0.0
            && trend.peak > baseline_power_val * 1.5;
        if trend.is_falling && peak_was_high {
            return self.build_result(
                DeviceInferenceState::CoolingDown, Confidence::Medium,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 9. Thinking — active, high variance (prefill / attention)
        // high_variance_threshold is W std dev (PowerTrend.variance stores std dev, not raw variance)
        let high_variance_threshold = 3.0; // W std dev
        if power_change > 0.5 && trend.variance > high_variance_threshold { // W std dev
            return self.build_result(
                DeviceInferenceState::Thinking, Confidence::Medium,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 10. Generating — active, low variance, elevated current:power ratio
        let current_power_ratio = if power > 0.1 { current / power } else { 0.0 };
        let baseline_cp_ratio = {
            let b = self.baseline.get_baseline(device_idx);
            let bp = b.map(|x| x.power_baseline).unwrap_or(1.0).max(1.0);
            let bc = b.map(|x| x.current_baseline).unwrap_or(0.0);
            if bp > 0.0 { bc / bp } else { 0.0 }
        };
        let ratio_elevated = current_power_ratio > baseline_cp_ratio * 1.2;
        if power_change > 0.2 && power_change < 0.8
            && trend.variance < high_variance_threshold // W std dev
            && ratio_elevated
        {
            return self.build_result(
                DeviceInferenceState::Generating, Confidence::Medium,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 11. MovingData — current_change significantly exceeds power_change
        if current_change > power_change + 0.3 && power_change > 0.1 {
            return self.build_result(
                DeviceInferenceState::MovingData, Confidence::Medium,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 12. Idle — within 15% of baseline
        if power_change.abs() < 0.15 && current_change.abs() < 0.15 {
            return self.build_result(
                DeviceInferenceState::Idle, Confidence::High,
                power, temp, aiclk, power_change,
                trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
            );
        }

        // 13. Unclear
        self.build_result(
            DeviceInferenceState::Unclear, Confidence::Low,
            power, temp, aiclk, power_change,
            trend, tdp_w, power_pct_of_tdp, power_pct_baseline,
        )
    }

    /// Build an `InferenceResult` with formatted headline and detail fields.
    fn build_result(
        &self,
        state: DeviceInferenceState,
        confidence: Confidence,
        power: f32,
        temp: f32,
        aiclk: Option<u32>,
        power_change: f32,
        trend: PowerTrend,
        tdp_w: Option<f32>,
        power_pct_of_tdp: Option<f32>,
        power_pct_baseline: f32,
    ) -> InferenceResult {
        let label = state_label(state);
        let label_color = state_color(state);

        // Headline: "18.0W   62.1°C  1100MHz   +50%"
        let power_str = format!("{:5.1}W", power);
        let temp_str  = format!("{:5.1}\u{00B0}C", temp);
        let clk_str   = aiclk.map(|c| format!("{:4}MHz", c))
                             .unwrap_or_else(|| "  ---  ".to_string());
        let delta_str = format!("{:+4.0}%", power_change * 100.0);
        let headline = format!("{}   {}  {}  {}",
            power_str, temp_str, clk_str, delta_str);

        // Detail: "humming   rivers of data   24% TDP"
        let trend_word   = trend_vocabulary(&trend);
        let pattern_word = pattern_vocabulary(state);
        let tdp_field = tdp_w.map(|tdp| {
            let pct = ((power / tdp) * 100.0).round() as u32;
            format!("{:3}% TDP", pct)
        }).unwrap_or_else(|| "--- TDP".to_string());
        let detail = format!("{:<8}  {:<14}  {}",
            trend_word, pattern_word, tdp_field);

        InferenceResult {
            state, confidence, label, label_color,
            headline, detail,
            power_pct_of_tdp, power_pct_baseline,
        }
    }

    fn unknown_result(&self) -> InferenceResult {
        InferenceResult {
            state: DeviceInferenceState::Unclear,
            confidence: Confidence::Low,
            label: "INSCRUTABLE",
            label_color: Color::DarkGray,
            headline: "  ---W     ---\u{00B0}C    ---    ---".to_string(),
            detail: "--------  who can say      --- TDP".to_string(),
            power_pct_of_tdp: None,
            power_pct_baseline: 0.0,
        }
    }
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

/// Map power trend to an 8-char evocative word.
///
/// Used in the detail line of `InferenceResult`. Each string is exactly 8 chars
/// (including any trailing space) so the detail line stays fixed-width.
fn trend_vocabulary(trend: &PowerTrend) -> &'static str {
    if trend.is_rising  { return "climbing"; }
    if trend.is_falling { return "settling"; }
    if trend.variance > 5.0 { return "sparking"; }
    if trend.variance > 2.0 { return "churning"; }
    "humming "
}

/// Map inference state to a 14-char evocative pattern description.
///
/// Used in the detail line of `InferenceResult`. Each string is exactly 14 chars
/// (including any trailing spaces) so the detail line stays fixed-width.
fn pattern_vocabulary(state: DeviceInferenceState) -> &'static str {
    match state {
        DeviceInferenceState::Generating     => "rivers of data",
        DeviceInferenceState::Thinking       => "tensix ablaze ",
        DeviceInferenceState::Idle           => "quiet as ever ",
        DeviceInferenceState::ThermalStress
        | DeviceInferenceState::Throttling   => "fighting heat ",
        DeviceInferenceState::MovingData     => "pipes are full",
        DeviceInferenceState::FabricDegraded => "losing thread ",
        DeviceInferenceState::MaxedOut       => "burning full  ",
        DeviceInferenceState::WarmingUp      => "coming to life",
        DeviceInferenceState::CoolingDown    => "finding rest  ",
        _                                    => "who can say   ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sample(power: f32) -> TelemetrySample {
        TelemetrySample { power, temp: 50.0, current: 10.0, aiclk: Some(1000), timestamp: Instant::now() }
    }

    // Helper: build an engine with a full 30-sample history for device 0
    fn engine_with_history(powers: &[f32], temp: f32, current: f32) -> InferenceEngine {
        let mut eng = InferenceEngine::new();
        // Feed 20 baseline samples first
        for &p in powers.iter().take(20) {
            eng.baseline.update(0, p, current, temp, 1000.0);
        }
        // Then add all samples to history
        for &p in powers {
            let sample = TelemetrySample {
                power: p, temp, current, aiclk: Some(1000),
                timestamp: Instant::now(),
            };
            eng.history.entry(0).or_insert_with(VecDeque::new).push_back(sample);
        }
        eng
    }

    #[test]
    fn classify_idle_when_stable_near_baseline() {
        // 30 samples all at 15W, 50°C, current 10A
        let powers = vec![15.0_f32; 30];
        let eng = engine_with_history(&powers, 50.0, 10.0);
        let result = eng.classify(0, None, None, 1);
        assert_eq!(result.state, DeviceInferenceState::Idle);
    }

    #[test]
    fn classify_thermal_stress_above_82c() {
        let powers = vec![30.0_f32; 30];
        let mut eng = engine_with_history(&powers, 83.0, 10.0);
        // Update temp in history
        for s in eng.history.get_mut(&0).unwrap().iter_mut() {
            s.temp = 83.5;
        }
        let result = eng.classify(0, None, None, 1);
        assert_eq!(result.state, DeviceInferenceState::ThermalStress);
    }

    #[test]
    fn classify_thinking_high_variance() {
        // Alternating 10W/40W → high variance, high power_change
        let powers: Vec<f32> = (0..30)
            .map(|i| if i % 2 == 0 { 10.0 } else { 40.0 })
            .collect();
        let eng = engine_with_history(&powers, 55.0, 15.0);
        let result = eng.classify(0, None, None, 1);
        // Should match Thinking (high variance, active)
        assert_eq!(result.state, DeviceInferenceState::Thinking);
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
