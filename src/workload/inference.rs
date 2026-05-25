// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Inference state classification for Tenstorrent hardware.
//!
//! Maintains a rolling 30-sample history per device and classifies each
//! device's hardware state into one of 13 evocative states using power
//! variance, temperature, AICLK, throttler registers, and ARC health.

use crate::animation::baseline::AdaptiveBaseline;
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
    /// RGB color for the label.  Convert to `ratatui::style::Color::Rgb(r, g, b)`
    /// in the TUI layer; kept as a plain tuple here to avoid an unconditional
    /// ratatui dependency in the workload crate.
    pub label_color:        (u8, u8, u8),
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
/// Colors are stored as `(r, g, b)` tuples; convert to `Color::Rgb(r,g,b)` in the TUI layer.
const STATE_META: &[(DeviceInferenceState, &str, (u8, u8, u8))] = &[
    (DeviceInferenceState::Stalled,        "GONE SILENT",     (205,   0,   0)),
    (DeviceInferenceState::HardwareFault,  "IN DISTRESS",     (205,   0,   0)),
    (DeviceInferenceState::ThermalStress,  "SCORCHING",       (255,  80,   0)),
    (DeviceInferenceState::Throttling,     "REINED IN",       (255, 140,   0)),
    (DeviceInferenceState::FabricDegraded, "LOST THE THREAD", (255, 140,   0)),
    (DeviceInferenceState::MaxedOut,       "BURNING BRIGHT",  (220,  60,   0)),
    (DeviceInferenceState::WarmingUp,      "AWAKENING",       (205, 205,   0)),
    (DeviceInferenceState::CoolingDown,    "EXHALING",        (  0, 205, 205)),
    (DeviceInferenceState::Thinking,       "DREAMING DEEP",   (160,  80, 255)),
    (DeviceInferenceState::Generating,     "CONJURING",       (  0, 205,   0)),
    (DeviceInferenceState::MovingData,     "IN FULL FLOW",    (  0,   0, 205)),
    (DeviceInferenceState::Idle,           "BREATHING",       ( 85,  85,  85)),
    (DeviceInferenceState::Unclear,        "INSCRUTABLE",     (160, 160,  40)),
];

/// Return the ALL-CAPS label string for a given state.
pub fn state_label(state: DeviceInferenceState) -> &'static str {
    STATE_META.iter()
        .find(|(s, _, _)| *s == state)
        .map(|(_, l, _)| *l)
        .unwrap_or("UNKNOWN")
}

/// Return the RGB color `(r, g, b)` for a given state.
///
/// In the TUI layer, convert with `ratatui::style::Color::Rgb(r, g, b)`.
pub fn state_color(state: DeviceInferenceState) -> (u8, u8, u8) {
    STATE_META.iter()
        .find(|(s, _, _)| *s == state)
        .map(|(_, _, c)| *c)
        .unwrap_or((205, 205, 205))
}

/// Maintains rolling history and classifies hardware state per device.
pub struct InferenceEngine {
    /// 30-sample rolling window per device index.
    pub history:  HashMap<usize, VecDeque<TelemetrySample>>,
    /// Shared adaptive baseline (learns idle power/current/temp).
    pub baseline: AdaptiveBaseline,
    /// Last observed ARC health counter values per device.
    arc_snapshots: HashMap<usize, [Option<u32>; 4]>,
    /// How many consecutive frames the ARC counters have been identical.
    /// Resets to 0 when any counter changes. Stall declared above ~30 frames.
    arc_stall_frames: HashMap<usize, u32>,
}

impl InferenceEngine {
    /// Create a new engine with empty history.
    pub fn new() -> Self {
        Self {
            history:          HashMap::new(),
            baseline:         AdaptiveBaseline::new(),
            arc_snapshots:    HashMap::new(),
            arc_stall_frames: HashMap::new(),
        }
    }

    /// Push a new telemetry sample for `device_idx` into the rolling history.
    ///
    /// Also updates the shared `AdaptiveBaseline` and ARC stall tracking for this device.
    ///
    /// `arc_health` — current parsed ARC counter values, or `None` if not available.
    /// The engine compares them across frames; stall is only declared after ~30
    /// consecutive frames of identical counters (~3 s at 10 Hz).
    pub fn ingest(
        &mut self,
        device_idx: usize,
        power: f32,
        temp: f32,
        current: f32,
        aiclk: Option<u32>,
        arc_health: Option<[Option<u32>; 4]>,
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

        // Track ARC counter stall state across frames.
        // ARC firmware (TIMER_HEARTBEAT) updates at ~1 Hz; we poll at ~10 Hz.
        // Comparing consecutive frames would always look "frozen", so we only
        // declare a real stall after 30 consecutive frames with the same value.
        match arc_health {
            Some(current_counts) => {
                let stall_count = self.arc_stall_frames.entry(device_idx).or_insert(0);
                if let Some(prev) = self.arc_snapshots.get(&device_idx) {
                    // A true stall means ALL counters are frozen (none changed).
                    // Using .any() would trigger on normal slow-updating ARC counters;
                    // .all() only fires when every counter has been stuck for this frame.
                    let all_counters_frozen = prev.iter().zip(current_counts.iter()).all(|(p, c)| {
                        matches!((p, c), (Some(pv), Some(cv)) if pv == cv)
                    });
                    if all_counters_frozen {
                        *stall_count = stall_count.saturating_add(1);
                    } else {
                        *stall_count = 0;
                    }
                } else {
                    // First time we see this device's counters — not stalled yet.
                    *stall_count = 0;
                }
                self.arc_snapshots.insert(device_idx, current_counts);
            }
            None => {
                // No ARC data available — clear stall counter so we don't false-positive.
                self.arc_stall_frames.insert(device_idx, 0);
            }
        }
    }

    /// Classify the current state of `device_idx`.
    ///
    /// # Arguments
    /// * `device_idx` — device index (0-based)
    /// * `smbus` — optional SMBUS telemetry for richer classification
    /// * `device_count` — total number of devices (for FabricDegraded check)
    ///
    /// ARC stall detection uses internal cross-frame tracking updated via `ingest()`,
    /// not a caller-supplied snapshot, to avoid false positives from slow counter updates.
    pub fn classify(
        &self,
        device_idx: usize,
        smbus: Option<&crate::models::telemetry::SmbusTelemetry>,
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
            matches!(s.faults.as_deref(), Some(f) if f != "0" && f != "0x0" && f != "0X0" && !f.is_empty())
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

        // 1. Stalled — ARC health counters unchanged for ≥30 consecutive frames (~3 s).
        // The counter (TIMER_HEARTBEAT) updates at ~1 Hz; at 10 Hz polling it is naturally
        // identical across ~10 consecutive frames.  We only declare a real stall after
        // 30 frames so brief polling coincidences don't trigger this.
        let arc_stall_frames = self.arc_stall_frames.get(&device_idx).copied().unwrap_or(0);
        if arc_stall_frames >= 30 {
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
        }).unwrap_or_else(|| " --- TDP".to_string());
        let detail = format!("{:<8}  {:<14}  {}",
            trend_word, pattern_word, tdp_field);

        InferenceResult {
            state, confidence, label, label_color,
            headline, detail,
            power_pct_of_tdp, power_pct_baseline,
        }
    }

    fn unknown_result(&self) -> InferenceResult {
        // Headline must match the 32-char width produced by build_result:
        //   power_str(6) + "   "(3) + temp_str(7) + "  "(2) + clk_str(7) + "  "(2) + delta_str(5)
        //   "  ---W"      + "   "   + "  ---°C"   + "  "   + "  ---  "   + "  "   + "  ---"
        let headline = "  ---W     ---\u{00B0}C    ---      ---".to_string();
        debug_assert_eq!(headline.chars().count(), 32, "unknown_result headline must be 32 chars");
        InferenceResult {
            state: DeviceInferenceState::Unclear,
            confidence: Confidence::Low,
            label: "INSCRUTABLE",
            label_color: (85, 85, 85),
            headline,
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

    // Single pass over `n` most-recent samples (index 0 = most recent).
    // Accumulate sum, sum-of-squares, peak, and Σ(x*y) for linear regression.
    // x = 0..n-1 where 0 is the OLDEST sample (reversed order vs iteration).
    let nf = n as f32;
    let x_mean = (nf - 1.0) / 2.0; // closed-form mean of 0..n-1
    let mut sum = 0.0_f32;
    let mut sum_sq = 0.0_f32;
    let mut peak = f32::NEG_INFINITY;
    let mut xy_cov = 0.0_f32; // Σ (x - x_mean)(y - y_mean)  — built after mean is known

    for (rev_i, sample) in history.iter().rev().take(n).enumerate() {
        let p = sample.power;
        sum += p;
        sum_sq += p * p;
        peak = peak.max(p);
        // x for this sample in oldest-first order is (n-1) - rev_i
        let x = (n - 1 - rev_i) as f32;
        xy_cov += (x - x_mean) * p; // y_mean subtracted below after computing mean
    }

    let mean = sum / nf;
    let variance = ((sum_sq / nf) - mean * mean).max(0.0).sqrt();

    // Σ(x - x_mean)*y: since Σ(x - x_mean) = 0, the y_mean cross-term vanishes,
    // so xy_cov is already the correct OLS numerator.
    // Denominator is closed-form: Σ(i - (n-1)/2)² for i=0..n-1 = n(n²-1)/12.
    let den = nf * (nf * nf - 1.0) / 12.0;
    let slope = if den > 0.0 { xy_cov / den } else { 0.0 };

    // Monotone checks — iterate directly, no allocation.
    let check_n = n.min(8);
    let mut prev = f32::NAN;
    let mut is_rising  = true;
    let mut is_falling = true;
    for sample in history.iter().rev().take(check_n) {
        if !prev.is_nan() {
            // sample.power is older; prev is newer
            if sample.power > prev { is_rising  = false; }
            if sample.power < prev { is_falling = false; }
        }
        prev = sample.power;
    }

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

/// Produce a `width`-character power bar using `█` (filled) and `░` (empty).
///
/// `fraction` — 0.0..=1.0 fill fraction (clamped).
pub fn render_power_bar(fraction: f32, width: usize) -> String {
    let filled = (fraction.clamp(0.0, 1.0) * width as f32).round() as usize;
    let empty  = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
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
        let result = eng.classify(0, None, 1);
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
        let result = eng.classify(0, None, 1);
        assert_eq!(result.state, DeviceInferenceState::ThermalStress);
    }

    #[test]
    fn classify_thinking_high_variance() {
        // Alternating 10W/40W → high variance, high power_change
        let powers: Vec<f32> = (0..30)
            .map(|i| if i % 2 == 0 { 10.0 } else { 40.0 })
            .collect();
        let eng = engine_with_history(&powers, 55.0, 15.0);
        let result = eng.classify(0, None, 1);
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

    #[test]
    fn power_bar_full_tdp() {
        let bar = render_power_bar(1.0, 20);
        assert_eq!(bar.chars().count(), 20);
        assert!(bar.chars().all(|c| c == '█'));
    }

    #[test]
    fn power_bar_half() {
        let bar = render_power_bar(0.5, 20);
        assert_eq!(bar.chars().count(), 20);
        let filled: usize = bar.chars().filter(|&c| c == '█').count();
        assert_eq!(filled, 10);
    }

    #[test]
    fn power_bar_zero() {
        let bar = render_power_bar(0.0, 20);
        assert!(bar.chars().all(|c| c == '░'));
    }
}
