// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! A synthetic training run for `--mock`.
//!
//! `--mock` already fabricates chip telemetry so the app can be driven with no
//! Tenstorrent hardware present. Without this, two of its views stayed empty
//! anyway: the Training view scans `/proc` for a real trainer and the
//! Inference monitor probes real containers, neither of which a mock backend
//! conjures. That made the app impossible to demo, screenshot, or develop
//! against end to end.
//!
//! Everything here is derived from elapsed time by a closed-form function —
//! no randomness — so a screenshot taken at t seconds always looks the same,
//! and a test can assert on any instant without sleeping.
//!
//! **The view labels this run as mock.** This tool's entire premise is that
//! every pixel maps to a real signal, so synthetic data that is
//! indistinguishable from a real run would be a genuine hazard rather than a
//! convenience. `TrainState::is_mock` carries that through to the header.

use super::config::TrainConfig;
use super::detect::TrainProcess;
use super::logsrc::LogSource;
use super::monitor::{TrainState, LOSS_HISTORY};

/// Wall-clock seconds for one simulated step.
const STEP_SECS: f32 = 1.0 / 12.0;
/// Steps between checkpoint saves — about every 10s at the rate above.
const SAVE_EVERY: u64 = 120;
/// The run's step budget, so ETA and the progress bar have a denominator.
const MAX_STEPS: u64 = 24_000;

/// Loss after `step` steps: an exponential decay to a floor, plus a smooth
/// wobble so the mountain range has the texture a real run has. Deterministic
/// in `step` — the same step always yields the same loss.
fn loss_at(step: u64) -> f32 {
    let p = (step as f32 / 2_400.0).min(1.0);
    let base = 4.6 * (-3.1 * p).exp() + 0.35;
    // Two incommensurate sine terms read as noise without being random.
    let wobble = (step as f32 * 0.7).sin() * 0.06 + (step as f32 * 0.13).sin() * 0.04;
    (base + wobble).max(0.05)
}

/// A deterministic stand-in for a live tt-train run.
pub struct MockTrainRun {
    started: std::time::Instant,
}

impl Default for MockTrainRun {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTrainRun {
    pub fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
        }
    }

    /// The state this run would be in now.
    pub fn state(&self) -> TrainState {
        self.state_at(self.started.elapsed().as_secs_f32())
    }

    /// The state at `elapsed` seconds — the seam the tests drive, so they
    /// never have to sleep to reach a later point in the run.
    pub fn state_at(&self, elapsed: f32) -> TrainState {
        let step = ((elapsed / STEP_SECS) as u64).min(MAX_STEPS).max(1);

        let mut st = TrainState::new();
        st.is_mock = true;
        st.proc = Some(TrainProcess {
            // A plausible shape, but named so nobody mistakes it for a real
            // process: there is no such pid and the header says `mock`.
            pid: 0,
            binary: "nano_gpt".into(),
            config_path: Some("train.yaml".into()),
        });
        st.config = TrainConfig {
            num_blocks: Some(6),
            num_heads: Some(6),
            embedding_dim: Some(384),
            vocab_size: Some(32_000),
            max_sequence_length: Some(256),
            learning_rate: Some(3.0e-4),
            model_save_path: Some("transformer.msgpack".into()),
            model_config_path: Some("configs/model_configs/nanollama3_char.yaml".into()),
        };
        // A log source, or the view draws its "stdout wasn't redirected"
        // degraded card instead of the run — that path is for a real trainer
        // whose output is genuinely unreachable, which is not this.
        st.log = Some(LogSource::File(std::path::PathBuf::from(
            "/mock/tt-train/train.log",
        )));
        st.max_steps = MAX_STEPS;
        st.batch_size = 64;
        st.grad_accum = 1;
        st.step_ms = STEP_SECS * 1000.0;
        st.scheduler = Some("cosine".into());
        // Kernel cache fills during the opening steps then goes quiet, which
        // is what drives the compiling -> steady shimmer.
        st.cache_entries = 64.min(8 + step as u32 / 6);
        st.step = step;

        // Only the tail of the run is retained, exactly as the live path does.
        let first = step.saturating_sub(LOSS_HISTORY as u64 - 1).max(1);
        st.loss_history = (first..=step).map(loss_at).collect();
        st.loss = Some(loss_at(step));
        st.prev_loss = (step > 1).then(|| loss_at(step - 1));

        st.checkpoint_step = step - (step % SAVE_EVERY);
        // Pulse for the handful of steps right after a save, so the comet
        // crosses the sky at the same cadence a real run's would.
        let since_save = step % SAVE_EVERY;
        st.checkpoint_pulse = if step >= SAVE_EVERY && since_save < 5 {
            40 - (since_save as u8 * 8)
        } else {
            0
        };
        st.first_seen = Some(self.started);
        st
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of `--mock` is a demoable app with no hardware and no
    /// workload, so the state has to be complete enough to draw every panel.
    #[test]
    fn a_mock_run_populates_every_panel_the_view_draws() {
        let st = MockTrainRun::new().state_at(30.0);
        assert!(st.proc.is_some(), "the header needs an attached run");
        assert!(
            matches!(st.log, Some(LogSource::File(_))),
            "without a log source the view draws its degraded card instead"
        );
        assert_eq!(st.config.num_blocks, Some(6));
        assert_eq!(st.config.num_heads, Some(6));
        assert!(st.loss.is_some());
        assert!(!st.loss_history.is_empty(), "the mountains need history");
        assert!(st.step > 0 && st.step < st.max_steps);
        assert!(st.steps_per_sec() > 0.0, "LIVE needs a step rate");
        assert!(st.tokens_per_sec().is_some(), "LIVE needs tokens/sec");
        assert!(st.eta_secs().is_some(), "LIVE needs an ETA");
    }

    /// Synthetic data that looks exactly like a real run would undermine the
    /// one guarantee this tool makes about its own display.
    #[test]
    fn a_mock_run_is_marked_as_mock() {
        assert!(MockTrainRun::new().state_at(5.0).is_mock);
        // ...and a real run is not, or the marker would be meaningless.
        assert!(!TrainState::new().is_mock);
    }

    #[test]
    fn the_run_advances_and_the_loss_descends() {
        let r = MockTrainRun::new();
        let early = r.state_at(5.0);
        let late = r.state_at(120.0);
        assert!(late.step > early.step, "steps must advance with time");
        assert!(
            late.loss.unwrap() < early.loss.unwrap(),
            "loss must descend: {:?} -> {:?}",
            early.loss,
            late.loss
        );
    }

    /// Determinism is what makes a screenshot reproducible and lets these
    /// tests assert on a later instant without sleeping.
    #[test]
    fn the_same_instant_always_yields_the_same_run() {
        let r = MockTrainRun::new();
        assert_eq!(r.state_at(42.0).loss, r.state_at(42.0).loss);
        assert_eq!(r.state_at(42.0).step, r.state_at(42.0).step);
    }

    #[test]
    fn checkpoints_pulse_periodically_rather_than_never_or_always() {
        let r = MockTrainRun::new();
        let secs_per_save = SAVE_EVERY as f32 * STEP_SECS;
        // Sample a couple of save periods; some instants must pulse and some
        // must not, or the comet would either never fire or never stop.
        let samples: Vec<bool> = (0..200)
            .map(|i| {
                r.state_at(secs_per_save * 2.0 * i as f32 / 200.0)
                    .checkpoint_pulse
                    > 0
            })
            .collect();
        assert!(samples.iter().any(|p| *p), "a checkpoint must pulse");
        assert!(samples.iter().any(|p| !*p), "it must not pulse constantly");
    }

    /// History is bounded the same way the live path bounds it, so the mock
    /// cannot grow without limit in a long-running kiosk session.
    #[test]
    fn loss_history_stays_bounded() {
        let st = MockTrainRun::new().state_at(10_000.0);
        assert!(st.loss_history.len() <= LOSS_HISTORY);
    }
}
