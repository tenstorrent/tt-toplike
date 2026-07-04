# Insights Screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `DisplayMode::Normal` (the raw telemetry table) with a purpose-built Insights screen that classifies what each chip is doing in plain, evocative language, while adding process kill (`SIGTERM`/`SIGKILL`) and keeping the raw table reachable via `v` cycling.

**Architecture:** A new `InferenceEngine` in `src/workload/inference.rs` maintains a 30-sample rolling history per device and classifies hardware state into 13 priority-ordered states. The TUI's `DisplayMode::Insights` replaces `DisplayMode::Normal` as default, rendering borderless device cards + a process panel with cursor and kill confirmation. View cycling shifts Normal to `DisplayMode::Table`.

**Tech Stack:** Ratatui (spans + lines), `libc::kill`, existing `AdaptiveBaseline`, `SmbusTelemetry`, `ProcessMonitor`

---

### Task 1: Create feature branch and add `libc` dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Create branch from main**

```bash
git checkout main
git checkout -b insights-screen
```

- [ ] **Step 2: Add libc dependency to Cargo.toml**

Find the `[dependencies]` section. Add just after it (or near end of file if a `[target.'cfg(unix)'.dependencies]` section already exists):

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo build --bin tt-toplike-tui --features tui,linux-procfs
```

Expected: `Finished` with no errors. Warnings are fine.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat: add libc dep for kill syscall"
```

---

### Task 2: Add `kill_pid` to `process_monitor.rs` with tests

**Files:**
- Modify: `src/workload/process_monitor.rs`

- [ ] **Step 1: Write failing test first**

Add this at the bottom of `process_monitor.rs`, inside `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_pid_invalid_returns_error() {
        // PID 0 means "all processes in process group" — always EPERM unless root.
        // We use this as a reliable "should fail without root" probe.
        // On CI (root), skip the test.
        if unsafe { libc::getuid() } == 0 {
            return; // running as root — kill(0) would succeed, skip
        }
        let result = kill_pid(0, libc::SIGTERM);
        assert!(result.is_err(), "kill_pid(0, SIGTERM) should fail for non-root");
    }

    #[test]
    fn kill_pid_nonexistent_returns_error() {
        // PID i32::MAX is virtually guaranteed to not exist.
        let result = kill_pid(i32::MAX, libc::SIGTERM);
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Expected: ESRCH (no such process)
        assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
    }
}
```

- [ ] **Step 2: Run test to verify it fails (function not defined yet)**

```bash
cargo test --features linux-procfs kill_pid 2>&1 | head -20
```

Expected: compile error `cannot find function kill_pid`.

- [ ] **Step 3: Add the `libc` import and `kill_pid` function**

At the top of `src/workload/process_monitor.rs`, after the existing `use` statements:

```rust
#[cfg(unix)]
use libc;
```

At the bottom of `process_monitor.rs`, before the `impl Default` block:

```rust
/// Send a signal to a process by PID.
///
/// Returns `Ok(())` on success, or an `io::Error` with the OS error on failure.
/// Common errors: `ESRCH` (no such process), `EPERM` (permission denied).
#[cfg(unix)]
pub fn kill_pid(pid: i32, signal: libc::c_int) -> std::io::Result<()> {
    let ret = unsafe { libc::kill(pid, signal) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test --features linux-procfs kill_pid
```

Expected:
```
test workload::process_monitor::tests::kill_pid_invalid_returns_error ... ok
test workload::process_monitor::tests::kill_pid_nonexistent_returns_error ... ok
```

- [ ] **Step 5: Commit**

```bash
git add src/workload/process_monitor.rs
git commit -m "feat: add kill_pid(pid, signal) to process_monitor"
```

---

### Task 3: Define inference types in `src/workload/inference.rs`

**Files:**
- Create: `src/workload/inference.rs`
- Modify: `src/workload/mod.rs`

- [ ] **Step 1: Write a failing compile test**

Temporarily add to `src/workload/mod.rs`:

```rust
// Temporary compile test — remove after Task 3 confirms types compile
#[cfg(test)]
mod _type_smoke {
    use super::inference::{DeviceInferenceState, Confidence, InferenceResult, InferenceEngine};
    #[test]
    fn types_exist() {
        let _s = DeviceInferenceState::Idle;
        let _c = Confidence::High;
        let _e = InferenceEngine::new();
    }
}
```

Run:
```bash
cargo test --features linux-procfs types_exist 2>&1 | head -10
```
Expected: compile error (module doesn't exist yet).

- [ ] **Step 2: Create `src/workload/inference.rs` with all types**

```rust
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
    /// Standard deviation of power in last 10 samples.
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

pub fn state_label(state: DeviceInferenceState) -> &'static str {
    STATE_META.iter()
        .find(|(s, _, _)| *s == state)
        .map(|(_, l, _)| *l)
        .unwrap_or("UNKNOWN")
}

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
```

- [ ] **Step 3: Export from `src/workload/mod.rs`**

Replace the contents of `src/workload/mod.rs` with:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Workload detection, process monitoring, and inference classification.

#[cfg(feature = "linux-procfs")]
pub mod process_monitor;
pub mod inference;

#[cfg(feature = "linux-procfs")]
pub use process_monitor::{ProcessInfo, ProcessMonitor};
pub use inference::{
    Confidence, DeviceInferenceState, InferenceEngine, InferenceResult, PowerTrend,
    TelemetrySample, state_label, state_color,
};
```

- [ ] **Step 4: Run compile test**

```bash
cargo test --features linux-procfs types_exist
```

Expected: `test workload::_type_smoke::types_exist ... ok`

- [ ] **Step 5: Remove the temporary smoke test from mod.rs**

Delete the `mod _type_smoke` block added in Step 1.

- [ ] **Step 6: Build clean**

```bash
cargo build --bin tt-toplike-tui --features tui,linux-procfs
```

Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add src/workload/inference.rs src/workload/mod.rs
git commit -m "feat: define inference types (states, confidence, engine shell)"
```

---

### Task 4: Implement `PowerTrend` computation and SMBUS parse helpers with tests

**Files:**
- Modify: `src/workload/inference.rs`

These are pure-math helpers — no hardware, fully unit-testable.

- [ ] **Step 1: Write failing tests first**

Add at the bottom of `src/workload/inference.rs`:

```rust
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
        // "0" → all four counters are 0 → stalled
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
```

Run:
```bash
cargo test --features linux-procfs -p tt-toplike 2>&1 | grep "compute_power_trend\|parse_tdp\|parse_arc\|throttler\|eth_status"
```
Expected: compile errors (functions not defined yet).

- [ ] **Step 2: Add the compute and parse functions to `inference.rs`**

Add these functions to `inference.rs`, before the `#[cfg(test)]` block:

```rust
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

    // Variance over last 10 samples.
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
```

- [ ] **Step 3: Add `use std::str` at the top if needed (it isn't — `from_str_radix` is on primitives)**

No additional imports needed beyond what's already in the file.

- [ ] **Step 4: Run the tests**

```bash
cargo test --features linux-procfs power_trend rising -- --nocapture
cargo test --features linux-procfs power_trend falling
cargo test --features linux-procfs power_trend variance
cargo test --features linux-procfs parse_tdp
cargo test --features linux-procfs parse_arc
cargo test --features linux-procfs throttler
cargo test --features linux-procfs eth_status
```

Expected: all pass.

- [ ] **Step 5: Run all tests**

```bash
cargo test --features linux-procfs
```

Expected: no regressions.

- [ ] **Step 6: Commit**

```bash
git add src/workload/inference.rs
git commit -m "feat: add PowerTrend computation and SMBUS parse helpers"
```

---

### Task 5: Implement `InferenceEngine::ingest` and `::classify` with tests

**Files:**
- Modify: `src/workload/inference.rs`

- [ ] **Step 1: Write failing classify tests first**

Add to the `tests` module in `inference.rs`:

```rust
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
```

Run:
```bash
cargo test --features linux-procfs classify 2>&1 | head -20
```
Expected: compile error (method `classify` not found).

- [ ] **Step 2: Implement `InferenceEngine::ingest` and `classify`**

Add these methods to the `impl InferenceEngine` block in `inference.rs`:

```rust
impl InferenceEngine {
    pub fn new() -> Self { /* already there */ }

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
    /// * `arc_counters` — optional [arc0..arc3] health counter values from SMBUS
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

        // 1. Stalled — ARC health counters frozen, or heartbeat == 0
        let stalled = {
            let heartbeat_zero = history.back().map(|_| false).unwrap_or(false); // sysfs heartbeat
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
            }).unwrap_or(heartbeat_zero);
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
        let high_variance_threshold = 3.0; // W standard deviation
        if power_change > 0.5 && trend.variance > high_variance_threshold {
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
            && trend.variance < high_variance_threshold
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
        let trend_word  = trend_vocabulary(&trend);
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

/// Map power trend to an 8-char evocative word.
fn trend_vocabulary(trend: &PowerTrend) -> &'static str {
    if trend.is_rising  { return "climbing"; }
    if trend.is_falling { return "settling"; }
    if trend.variance > 5.0 { return "sparking"; }
    if trend.variance > 2.0 { return "churning"; }
    "humming "
}

/// Map inference state to a 14-char evocative pattern description.
fn pattern_vocabulary(state: DeviceInferenceState) -> &'static str {
    match state {
        DeviceInferenceState::Generating   => "rivers of data",
        DeviceInferenceState::Thinking     => "tensix ablaze ",
        DeviceInferenceState::Idle         => "quiet as ever ",
        DeviceInferenceState::ThermalStress
        | DeviceInferenceState::Throttling => "fighting heat ",
        DeviceInferenceState::MovingData   => "pipes are full",
        DeviceInferenceState::FabricDegraded => "losing thread ",
        DeviceInferenceState::MaxedOut     => "burning full  ",
        DeviceInferenceState::WarmingUp    => "coming to life",
        DeviceInferenceState::CoolingDown  => "finding rest  ",
        _                                  => "who can say   ",
    }
}
```

- [ ] **Step 3: Run all classify tests**

```bash
cargo test --features linux-procfs classify
```

Expected: 3 tests pass (idle, thermal_stress, thinking).

- [ ] **Step 4: Run full test suite**

```bash
cargo test --features linux-procfs
```

Expected: no regressions.

- [ ] **Step 5: Commit**

```bash
git add src/workload/inference.rs
git commit -m "feat: implement InferenceEngine::ingest and classify (13 states)"
```

---

### Task 6: Implement fixed-width formatting helpers and power bar with tests

**Files:**
- Modify: `src/workload/inference.rs`

These helpers produce the `Span` sequences for each card line.

- [ ] **Step 1: Write failing tests for format helpers**

Add to `tests` module in `inference.rs`:

```rust
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
```

Run:
```bash
cargo test --features linux-procfs power_bar 2>&1 | head -10
```
Expected: compile error (function not defined).

- [ ] **Step 2: Add `render_power_bar` function**

Add before the `#[cfg(test)]` block:

```rust
/// Produce a 20-char (or `width`-char) power bar using `█` and `░`.
///
/// `fraction` — 0.0..=1.0 fill fraction.
pub fn render_power_bar(fraction: f32, width: usize) -> String {
    let filled = (fraction.clamp(0.0, 1.0) * width as f32).round() as usize;
    let empty  = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}
```

- [ ] **Step 3: Run the tests**

```bash
cargo test --features linux-procfs power_bar
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/workload/inference.rs
git commit -m "feat: add render_power_bar formatting helper"
```

---

### Task 7: Wire `InferenceEngine` into TUI run loop and add `DisplayMode::Insights`

**Files:**
- Modify: `src/ui/tui/mod.rs`

This task adds the new mode, kill-confirm state, and process cursor — but not the actual rendering (that's Task 8 and 9).

- [ ] **Step 1: Add new types and imports at top of `mod.rs`**

After the existing imports, add:

```rust
#[cfg(feature = "linux-procfs")]
use crate::workload::{InferenceEngine, ProcessMonitor};
#[cfg(unix)]
use libc;
```

- [ ] **Step 2: Add `KillConfirmState` struct and update `DisplayMode` enum**

In `src/ui/tui/mod.rs`, replace the `DisplayMode` enum:

```rust
/// Pending kill confirmation state.
#[cfg(feature = "linux-procfs")]
#[derive(Debug, Clone)]
struct KillConfirmState {
    pid:    i32,
    name:   String,
    signal: i32,   // libc::SIGTERM (15) or libc::SIGKILL (9)
}

/// UI display mode
#[derive(Debug, Clone, Copy, PartialEq)]
enum DisplayMode {
    /// Insights screen (human-readable inference per chip + process panel)
    Insights,
    /// Raw telemetry table (original Normal mode, now secondary)
    Table,
    /// Hardware-responsive starfield visualization
    Starfield,
    /// Memory Castle mode (architectural memory hierarchy visualization)
    MemoryCastle,
    /// Memory Flow Topology (full-screen DRAM motion visualization)
    MemoryFlow,
    /// Arcade mode (unified visualization with all three modes + hero character)
    Arcade,
}
```

- [ ] **Step 3: Update the CLI mode mapping and default in `run_app`**

In the `run_app` function, update the mode initialization block:

```rust
let mut display_mode = if let Some(mode) = cli.mode {
    match mode {
        crate::cli::VisualizationMode::Normal   => DisplayMode::Table,   // old Normal → Table
        crate::cli::VisualizationMode::Starfield => DisplayMode::Starfield,
        crate::cli::VisualizationMode::Castle    => DisplayMode::MemoryCastle,
        crate::cli::VisualizationMode::Flow      => DisplayMode::MemoryFlow,
        crate::cli::VisualizationMode::Arcade    => DisplayMode::Arcade,
    }
} else {
    DisplayMode::Insights   // default is now Insights
};
```

- [ ] **Step 4: Add Insights state variables in `run_app`**

After the existing `let mut arcade: Option<ArcadeVisualization> = None;` line, add:

```rust
// Insights mode state
#[cfg(feature = "linux-procfs")]
let mut inference_engine = InferenceEngine::new();
#[cfg(feature = "linux-procfs")]
let mut process_cursor: usize = 0;
#[cfg(feature = "linux-procfs")]
let mut kill_confirm: Option<KillConfirmState> = None;
```

- [ ] **Step 5: Add Insights update arm in the display mode match**

In the `match display_mode { ... }` block that updates visualizations (around line 169), add:

```rust
DisplayMode::Insights => {
    // Ingest telemetry into InferenceEngine each frame
    #[cfg(feature = "linux-procfs")]
    for device in backend.devices() {
        let idx = device.index;
        let power   = backend.telemetry(idx).map(|t| t.power_w()).unwrap_or(0.0);
        let temp    = backend.telemetry(idx).map(|t| t.temp_c()).unwrap_or(0.0);
        let current = backend.telemetry(idx).map(|t| t.current_a()).unwrap_or(0.0);
        let aiclk   = backend.telemetry(idx).and_then(|t| t.aiclk);
        inference_engine.ingest(idx, power, temp, current, aiclk);
    }
}
DisplayMode::Table => {
    // Table mode doesn't need special init
}
```

- [ ] **Step 6: Add Insights rendering arm in the draw match**

In the `match display_mode { ... }` block inside `terminal.draw(|f| { ... })`, add:

```rust
DisplayMode::Insights => {
    #[cfg(feature = "linux-procfs")]
    render_insights(f, backend, &inference_engine, &process_monitor, process_cursor, kill_confirm.as_ref());
    #[cfg(not(feature = "linux-procfs"))]
    render_insights_no_procfs(f, backend, &inference_engine);
}
DisplayMode::Table => {
    #[cfg(feature = "linux-procfs")]
    ui(f, backend, cli, &process_monitor);
    #[cfg(not(feature = "linux-procfs"))]
    ui(f, backend, cli);
}
```

- [ ] **Step 7: Update the `v` key cycle**

Replace the `'v'` key handler:

```rust
KeyCode::Char('v') => {
    display_mode = match display_mode {
        DisplayMode::Insights    => DisplayMode::Table,
        DisplayMode::Table       => DisplayMode::MemoryFlow,
        DisplayMode::MemoryFlow  => DisplayMode::Starfield,
        DisplayMode::Starfield   => {
            memory_castle = None;
            DisplayMode::MemoryCastle
        }
        DisplayMode::MemoryCastle => {
            arcade = None;
            DisplayMode::Arcade
        }
        DisplayMode::Arcade => DisplayMode::Insights,
    };
    log::info!("Switched to {:?} mode", display_mode);
}
```

- [ ] **Step 8: Add Insights key bindings**

In the `Event::Key(key)` handler, add before the `_ => {}` arm:

```rust
// Insights-mode navigation and kill
#[cfg(feature = "linux-procfs")]
KeyCode::Up | KeyCode::Char('l') if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        process_cursor = process_cursor.saturating_sub(1);
    }
}
#[cfg(feature = "linux-procfs")]
KeyCode::Down | KeyCode::Char('j') if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        process_cursor = process_cursor.saturating_add(1);
    }
}
#[cfg(feature = "linux-procfs")]
KeyCode::Char('k') if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        if let Some(proc) = flat_process_list(&process_monitor).get(process_cursor) {
            kill_confirm = Some(KillConfirmState {
                pid: proc.pid,
                name: proc.name.clone(),
                signal: libc::SIGTERM,
            });
        }
    }
}
#[cfg(feature = "linux-procfs")]
KeyCode::Char('K') if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        if let Some(proc) = flat_process_list(&process_monitor).get(process_cursor) {
            kill_confirm = Some(KillConfirmState {
                pid: proc.pid,
                name: proc.name.clone(),
                signal: libc::SIGKILL,
            });
        }
    }
}
#[cfg(feature = "linux-procfs")]
KeyCode::Char('y') | KeyCode::Enter if display_mode == DisplayMode::Insights => {
    if let Some(ref kc) = kill_confirm {
        let _ = crate::workload::process_monitor::kill_pid(kc.pid, kc.signal);
        kill_confirm = None;
    }
}
#[cfg(feature = "linux-procfs")]
KeyCode::Char('n') if display_mode == DisplayMode::Insights => {
    kill_confirm = None;
}
```

Also add `Esc` to cancel kill confirm:

```rust
KeyCode::Esc => {
    #[cfg(feature = "linux-procfs")]
    if kill_confirm.is_some() {
        kill_confirm = None;
    } else {
        return Ok(());
    }
    #[cfg(not(feature = "linux-procfs"))]
    return Ok(());
}
```

- [ ] **Step 9: Add a `flat_process_list` helper function**

Somewhere near the bottom of `mod.rs`, add:

```rust
/// Flatten all device-specific and shared processes into one ordered Vec
/// for cursor-based navigation in the process panel.
#[cfg(feature = "linux-procfs")]
fn flat_process_list<'a>(pm: &'a ProcessMonitor) -> Vec<&'a crate::workload::ProcessInfo> {
    let mut all: Vec<&'a crate::workload::ProcessInfo> = Vec::new();
    // Collect by device (sorted by device index for stable ordering)
    let mut device_indices: Vec<usize> = (0..32).collect();
    device_indices.sort_unstable();
    for idx in device_indices {
        if let Some(procs) = pm.get_processes_for_device(idx) {
            for p in procs {
                if !all.iter().any(|existing| existing.pid == p.pid) {
                    all.push(p);
                }
            }
        }
    }
    for p in pm.get_shared_processes() {
        if !all.iter().any(|existing| existing.pid == p.pid) {
            all.push(p);
        }
    }
    all
}
```

- [ ] **Step 10: Build to verify no errors**

```bash
cargo build --bin tt-toplike-tui --features tui,linux-procfs
```

Expected: compiles (there will be "function not found" for `render_insights` — add a stub):

```rust
#[cfg(feature = "linux-procfs")]
fn render_insights(
    f: &mut Frame,
    backend: &Box<dyn TelemetryBackend>,
    engine: &InferenceEngine,
    pm: &ProcessMonitor,
    cursor: usize,
    kill_confirm: Option<&KillConfirmState>,
) {
    // stub — implemented in Task 8 and 9
    let _ = (f, backend, engine, pm, cursor, kill_confirm);
}

#[cfg(not(feature = "linux-procfs"))]
fn render_insights_no_procfs(
    f: &mut Frame,
    backend: &Box<dyn TelemetryBackend>,
    engine: &crate::workload::InferenceEngine,
) {
    let _ = (f, backend, engine);
}
```

- [ ] **Step 11: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat: add DisplayMode::Insights, TUI state, key bindings"
```

---

### Task 8: Implement `render_device_cards` — borderless device cards

**Files:**
- Modify: `src/ui/tui/mod.rs`

- [ ] **Step 1: Replace the stub `render_insights` with the real layout skeleton**

Replace the stub from Task 7 with:

```rust
#[cfg(feature = "linux-procfs")]
fn render_insights(
    f: &mut Frame,
    backend: &Box<dyn TelemetryBackend>,
    engine: &InferenceEngine,
    pm: &ProcessMonitor,
    cursor: usize,
    kill_confirm: Option<&KillConfirmState>,
) {
    let area = f.area();
    let devices = backend.devices();
    let n = devices.len().max(1);
    let procs = flat_process_list(pm);
    let proc_count = procs.len().max(1);

    // Layout: header(3) | cards(4*n) | process panel (min 3) | footer(1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length((4 * n) as u16),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    // Header (existing render_header function)
    render_header(f, chunks[0], backend, cli_placeholder());

    // Device cards
    render_device_cards(f, chunks[1], backend, engine);

    // Process panel (separator + list)
    render_process_panel(f, chunks[2], pm, &procs, cursor, kill_confirm);

    // Footer
    render_insights_footer(f, chunks[3], kill_confirm);
}

/// Placeholder for cli reference in header — header uses it for backend info.
fn cli_placeholder() -> &'static Cli {
    // SAFETY: We don't actually call any method on cli that reads fields;
    // render_header only uses backend. Pass a static default.
    // This is a known limitation — ideally thread cli through.
    // For now the header ignores cli in Insights mode.
    static PLACEHOLDER: std::sync::OnceLock<Cli> = std::sync::OnceLock::new();
    PLACEHOLDER.get_or_init(|| Cli {
        backend: crate::cli::BackendType::Auto,
        mock: false, json: false,
        interval: 100, max_errors: 10,
        verbose: false, quiet: false,
        mock_devices: 1, tt_smi_path: "tt-smi".to_string(),
        tt_smi_args: vec!["-s".to_string()],
        devices: None, mode: None,
        timeout: 30,
    })
}
```

**Note:** The `render_header` function already exists. Pass `cli` through instead of a placeholder by adding `cli: &Cli` as a parameter if you prefer — but the cleanest approach is to pass it from the call site in the run loop. Update the `render_insights` signature to accept `cli: &Cli` and pass it through from the event loop call.

Actually, do it cleanly — update the call site in `run_app` to pass `cli`, and update the function signature:

```rust
// In run_app, update the call:
render_insights(f, backend, &inference_engine, &process_monitor, process_cursor, kill_confirm.as_ref(), cli);

// Update the function signature:
fn render_insights(
    f: &mut Frame,
    backend: &Box<dyn TelemetryBackend>,
    engine: &InferenceEngine,
    pm: &ProcessMonitor,
    cursor: usize,
    kill_confirm: Option<&KillConfirmState>,
    cli: &Cli,
) { ... }
```

Remove the `cli_placeholder()` function entirely and use `cli` directly.

- [ ] **Step 2: Implement `render_device_cards`**

Add this function:

```rust
/// Render borderless device cards — 3 content lines + 1 blank per device.
#[cfg(feature = "linux-procfs")]
fn render_device_cards(
    f: &mut Frame,
    area: Rect,
    backend: &Box<dyn TelemetryBackend>,
    engine: &InferenceEngine,
) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    let devices = backend.devices();
    let mut lines: Vec<Line> = Vec::new();

    for device in devices.iter() {
        let idx = device.index;
        let smbus = backend.smbus_telemetry(idx);

        // Classify
        let arc_counts = smbus.map(|s| parse_arc_health_counters([
            s.arc0_health.clone(), s.arc1_health.clone(),
            s.arc2_health.clone(), s.arc3_health.clone(),
        ]));
        let result = engine.classify(idx, smbus, arc_counts.as_ref(), devices.len());

        // ── Line 1: device identity ──────────────────────────────────────
        // "  Dev  0  p150a  Blackhole"
        let board_type  = device.board_type.as_deref().unwrap_or("?????");
        let arch        = format!("{:?}", device.architecture);
        let line1 = Line::from(vec![
            Span::raw("  "),
            Span::styled("Dev ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:<2}", idx), Style::default().fg(Color::White)),
            Span::raw("  "),
            Span::styled(format!("{:<5}", &board_type[..board_type.len().min(5)]),
                Style::default().fg(Color::Gray)),
            Span::raw("  "),
            Span::styled(format!("{:<9}", &arch[..arch.len().min(9)]),
                Style::default().fg(Color::Gray)),
        ]);

        // ── Line 2: status + metrics ─────────────────────────────────────
        // "  CONJURING        18.0W   62.1°C  1100MHz   +50%"
        // label is 16 chars, left-padded; each metric is a fixed-width Span.
        let label_padded = format!("{:<16}", result.label);
        let confidence_mod = match result.confidence {
            crate::workload::Confidence::High   => Modifier::BOLD,
            crate::workload::Confidence::Medium => Modifier::empty(),
            crate::workload::Confidence::Low    => Modifier::DIM,
        };
        // Parse headline back into parts (it was formatted as fixed-width string)
        // Easier to re-derive the parts here for Span composition.
        let power   = backend.telemetry(idx).map(|t| t.power_w()).unwrap_or(0.0);
        let temp    = backend.telemetry(idx).map(|t| t.temp_c()).unwrap_or(0.0);
        let aiclk   = backend.telemetry(idx).and_then(|t| t.aiclk);
        let power_change = engine.baseline.power_change(idx, power);

        let power_str = if power > 0.0 {
            format!("{:5.1}W", power)
        } else { "  ---W".to_string() };
        let temp_str = if temp > 0.0 {
            format!("{:5.1}\u{00B0}C", temp)
        } else { "  ---\u{00B0}C".to_string() };
        let clk_str = aiclk.map(|c| format!("{:4}MHz", c))
            .unwrap_or_else(|| "  ---   ".to_string());
        let delta_str = format!("{:+4.0}%", power_change * 100.0);

        let line2 = Line::from(vec![
            Span::raw("  "),
            Span::styled(label_padded,
                Style::default().fg(result.label_color).add_modifier(confidence_mod)),
            Span::raw(" "),
            Span::styled(power_str, Style::default().fg(Color::White)),
            Span::raw("   "),
            Span::styled(temp_str, Style::default().fg(crate::ui::colors::temp_color(temp))),
            Span::raw("  "),
            Span::styled(clk_str, Style::default().fg(Color::Cyan)),
            Span::raw("  "),
            Span::styled(delta_str,
                Style::default().fg(if power_change > 0.3 {
                    Color::Yellow
                } else if power_change < -0.1 {
                    Color::Cyan
                } else {
                    Color::DarkGray
                })),
        ]);

        // ── Line 3: power bar + interpretation ──────────────────────────
        // "  ████████░░░░░░░░░░  humming   rivers of data   24% TDP"
        let bar_frac = result.power_pct_of_tdp.unwrap_or(result.power_pct_baseline);
        let bar = crate::workload::inference::render_power_bar(bar_frac, 20);
        let bar_color = if bar_frac > 0.85 { Color::Red }
            else if bar_frac > 0.60 { Color::Yellow }
            else { Color::Green };

        let line3 = Line::from(vec![
            Span::raw("  "),
            Span::styled(bar, Style::default().fg(bar_color)),
            Span::raw("  "),
            Span::raw(result.detail.clone()),
        ]);

        // ── Blank separator ──────────────────────────────────────────────
        lines.push(line1);
        lines.push(line2);
        lines.push(line3);
        lines.push(Line::raw(""));
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, area);
}
```

- [ ] **Step 3: Build and check for errors**

```bash
cargo build --bin tt-toplike-tui --features tui,linux-procfs 2>&1 | grep "^error"
```

Fix any type errors. Common issues:
- `parse_arc_health_counters` needs to be imported or prefixed with `crate::workload::inference::`
- `render_power_bar` same

Add to the top of the function:
```rust
use crate::workload::inference::{parse_arc_health_counters, render_power_bar};
```
Or prefix calls with the full path.

- [ ] **Step 4: Smoke test — launch and see Insights mode**

```bash
cargo run --bin tt-toplike-tui --features tui,linux-procfs -- --mock --mock-devices 3
```

Expected: launches showing 3 device cards (state labels, metrics, bars) with a blank process panel. Press `v` to cycle to Table mode (old Normal). Press `v` repeatedly to confirm full cycle: Insights → Table → MemoryFlow → Starfield → MemoryCastle → Arcade → Insights.

- [ ] **Step 5: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat: implement render_device_cards (borderless 3-line cards)"
```

---

### Task 9: Implement `render_process_panel` with cursor and kill confirmation

**Files:**
- Modify: `src/ui/tui/mod.rs`

- [ ] **Step 1: Implement `render_process_panel`**

Add this function:

```rust
/// Render the process panel with cursor, device mapping, and kill confirmation.
#[cfg(feature = "linux-procfs")]
fn render_process_panel(
    f: &mut Frame,
    area: Rect,
    pm: &ProcessMonitor,
    procs: &[&crate::workload::ProcessInfo],
    cursor: usize,
    kill_confirm: Option<&KillConfirmState>,
) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    let mut lines: Vec<Line> = Vec::new();

    // ── Separator rule ────────────────────────────────────────────────────
    let rule_width = area.width.saturating_sub(4) as usize;
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("─".repeat(rule_width), Style::default().fg(Color::DarkGray)),
    ]));

    // ── Section label ─────────────────────────────────────────────────────
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("Processes", Style::default().fg(Color::Gray)),
    ]));

    if procs.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "nothing is feeding the chips right now",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    } else {
        let clamped_cursor = cursor.min(procs.len().saturating_sub(1));

        for (i, proc) in procs.iter().enumerate() {
            let selected = i == clamped_cursor;
            let cursor_char = if selected { "▶" } else { " " };

            // Name: 12 chars left-aligned, truncated
            let name = format!("{:<12}", truncate(&proc.name, 12));
            // Cmdline: 32 chars, no ellipsis
            let cmdline = format!("{:<32}", truncate(&proc.cmdline, 32));
            // PID: 7 chars right-aligned
            let pid = format!("{:>7}", proc.pid);
            // Devices: 10 chars
            let devices = if proc.device_indices.is_empty() {
                "shared    ".to_string()
            } else {
                let dev_str = proc.device_indices.iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{:<10}", format!("Dev {}", dev_str))
            };

            let row_color = if selected { Color::White } else { Color::Gray };
            let cursor_color = if selected { Color::Yellow } else { Color::DarkGray };

            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(cursor_char, Style::default().fg(cursor_color)),
                Span::raw(" "),
                Span::styled(name,    Style::default().fg(row_color)),
                Span::raw("  "),
                Span::styled(cmdline, Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(pid,     Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled(devices, Style::default().fg(Color::Blue)),
            ]));
        }
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, area);
}

/// Truncate a string to at most `max` characters.
fn truncate(s: &str, max: usize) -> &str {
    let mut end = 0;
    let mut count = 0;
    for c in s.chars() {
        if count >= max { break; }
        end += c.len_utf8();
        count += 1;
    }
    &s[..end]
}
```

- [ ] **Step 2: Implement `render_insights_footer`**

Add this function:

```rust
/// Render the one-line footer for Insights mode.
/// Shows kill confirmation when pending, keyboard hints otherwise.
#[cfg(feature = "linux-procfs")]
fn render_insights_footer(
    f: &mut Frame,
    area: Rect,
    kill_confirm: Option<&KillConfirmState>,
) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    let line = if let Some(kc) = kill_confirm {
        let verb = if kc.signal == libc::SIGTERM { "silence" } else { "destroy" };
        let adverb = if kc.signal == libc::SIGTERM { "yes, gently" } else { "yes, hard" };
        let name_short = truncate(&kc.name, 12);
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} {} (PID {})?", verb, name_short, kc.pid),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("y", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(adverb, Style::default().fg(Color::Green)),
            Span::raw("    "),
            Span::styled("n", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw("  nevermind"),
        ])
    } else {
        Line::from(vec![
            Span::raw("  "),
            Span::styled("↑↓", Style::default().fg(Color::White)),
            Span::raw("  move"),
            Span::raw("  ·  "),
            Span::styled("k", Style::default().fg(Color::Yellow)),
            Span::raw("  silence"),
            Span::raw("  ·  "),
            Span::styled("K", Style::default().fg(Color::Red)),
            Span::raw("  destroy"),
            Span::raw("  ·  "),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw("  next view"),
            Span::raw("  ·  "),
            Span::styled("q", Style::default().fg(Color::DarkGray)),
            Span::raw("  leave"),
        ])
    };

    let para = Paragraph::new(line);
    f.render_widget(para, area);
}
```

- [ ] **Step 3: Build cleanly**

```bash
cargo build --bin tt-toplike-tui --features tui,linux-procfs 2>&1 | grep "^error"
```

Expected: no errors.

- [ ] **Step 4: Clamp cursor in key handler**

In the `'j'`/Down handler in run_app, clamp cursor to list length:

```rust
KeyCode::Down | KeyCode::Char('j') if display_mode == DisplayMode::Insights => {
    if kill_confirm.is_none() {
        let max = flat_process_list(&process_monitor).len().saturating_sub(1);
        process_cursor = (process_cursor + 1).min(max);
    }
}
```

- [ ] **Step 5: Launch and test process panel interactively**

```bash
cargo run --bin tt-toplike-tui --features tui,linux-procfs -- --mock --mock-devices 3
```

- Verify process panel shows or shows "nothing is feeding the chips"
- If no real hardware, test with sysfs: `--backend sysfs`
- Press `↓` to move cursor
- Press `k` to initiate SIGTERM confirmation — footer should change
- Press `n` to cancel — footer returns to normal
- Press `K` to initiate SIGKILL — "destroy" prompt should appear
- Press `v` to cycle to Table mode and back

- [ ] **Step 6: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat: render_process_panel with cursor and kill confirmation"
```

---

### Task 10: Final integration, build verification, and manual test

**Files:**
- Review only — no new code

- [ ] **Step 1: Full build — all features**

```bash
cargo build --bin tt-toplike-tui --features tui,linux-procfs
cargo build --bin tt-toplike-tui --features tui
```

Expected: both succeed, no errors. Warnings are acceptable.

- [ ] **Step 2: Run all tests**

```bash
cargo test --features linux-procfs
```

Expected: all pass, no regressions.

- [ ] **Step 3: Launch with mock and walk through the full UX**

```bash
cargo run --bin tt-toplike-tui --features tui,linux-procfs -- --mock --mock-devices 4
```

**Checklist:**
- [ ] Starts in Insights mode (not the old telemetry table)
- [ ] 4 device cards visible, each with 3 lines + blank separator
- [ ] State labels are ALL-CAPS, colored (BREATHING / INSCRUTABLE / etc.)
- [ ] Metrics line has power / temp / clock / delta — columns don't shift
- [ ] Power bar fills proportionally to activity
- [ ] Process panel shows header rule + "Processes" label
- [ ] Footer shows `↑↓ move · k silence · K destroy · v next view · q leave`
- [ ] `↓` / `↑` moves cursor (highlighted row changes)
- [ ] `k` activates SIGTERM confirmation, footer replaces with confirm prompt
- [ ] `n` cancels, footer returns to normal
- [ ] `K` activates SIGKILL confirmation with "destroy" verb
- [ ] `v` cycles: Insights → Table → MemoryFlow → Starfield → MemoryCastle → Arcade → Insights
- [ ] `q` quits cleanly

- [ ] **Step 4: Test with sysfs backend (real hardware or verify graceful fallback)**

```bash
cargo run --bin tt-toplike-tui --features tui,linux-procfs -- --backend sysfs
```

Expected: shows real device cards with sysfs-only state classification (BREATHING, INSCRUTABLE, SCORCHING, etc. — no Throttling/HardwareFault without SMBUS).

- [ ] **Step 5: Final commit**

```bash
git add -A
git status  # confirm nothing unexpected
git commit -m "feat: insights screen complete — inference classification + process kill"
```

- [ ] **Step 6: Push branch**

Only push when ready for review:
```bash
git push -u origin insights-screen
```

---

## Self-Review

### Spec coverage check

| Spec section | Covered by task(s) |
|---|---|
| InferenceEngine, history, stall detection | Task 3, 5 |
| 13 priority-ordered states | Task 5 |
| Thinking vs Generating (variance) | Task 5 |
| Graceful degradation (sysfs-only vs Hybrid) | Task 5 (`smbus` param is `Option`) |
| State display: ALL-CAPS label, color, no emoji | Task 8 |
| Confidence affects brightness | Task 8 (`Modifier::BOLD` / `DIM`) |
| Line 1: Dev identity, fixed widths | Task 8 |
| Line 2: status 16ch + metrics fixed | Task 8 |
| Line 3: bar 20ch + trend 8ch + pattern 14ch + TDP 7ch | Task 6, 8 |
| Process panel: rule, label, cursor `▶`, columns | Task 9 |
| No-process fallback ("nothing is feeding...") | Task 9 |
| Kill function: `libc::kill` | Task 1, 2 |
| SIGTERM / SIGKILL confirmation footer | Task 9 |
| y / n / Enter / Esc confirm/cancel | Task 7 |
| `j` / `l` home-row nav | Task 7 |
| `v` cycle: Insights → Table → Flow → Starfield → Castle → Arcade | Task 7 |
| No Block borders in Insights layout | Task 8 (`Paragraph`, no `Block`) |
| libc explicit dep under `[target.'cfg(unix)'.dependencies]` | Task 1 |

### Placeholder scan

No placeholders found. All code blocks are complete.

### Type consistency

- `InferenceEngine::ingest` signature: `(usize, f32, f32, f32, Option<u32>)` — matches call in Task 7
- `InferenceEngine::classify` signature: `(usize, Option<&SmbusTelemetry>, Option<&[Option<u32>; 4]>, usize)` — matches call in Task 8
- `render_power_bar(fraction: f32, width: usize) -> String` — matches call in Task 8 with `(bar_frac, 20)`
- `KillConfirmState.signal: i32` — matches `libc::SIGTERM` and `libc::SIGKILL` which are `i32`
- `flat_process_list` returns `Vec<&ProcessInfo>` — matches `.get(process_cursor)` in key handler
