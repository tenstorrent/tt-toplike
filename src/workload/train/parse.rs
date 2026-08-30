// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Parse tt-train's stdout into structured events. Pure — no I/O.
//!
//! tt-train is C++ and prints with `fmt::print`; these are the only shapes it
//! emits per-run (verbatim from `sources/examples/nano_gpt/main.cpp`):
//!
//! ```text
//! Step: {global_step}, Loss: {average_loss}
//! Full step time {duration} ms, cache entries: {n}
//! Max steps {N}
//! Batch size {N}
//! Gradient accumulation steps {N}
//! Scheduler type {name}
//! Number of parameters: {N}
//! ```
//!
//! Anything else (framework logger output, blank lines, a partially-flushed
//! line) yields `None` and is skipped rather than failing the tail.

/// One recognized line of tt-train output.
#[derive(Debug, Clone, PartialEq)]
pub enum TrainEvent {
    Step { step: u64, loss: f32 },
    StepTime { ms: f32, cache_entries: u32 },
    MaxSteps(u64),
    BatchSize(u32),
    GradAccum(u32),
    Scheduler(String),
    ParamCount(u64),
}

/// Value after `prefix`, trimmed. `None` when the line doesn't start with it.
fn after<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.strip_prefix(prefix).map(|s| s.trim())
}

pub fn parse_train_line(line: &str) -> Option<TrainEvent> {
    let line = line.trim();

    // "Step: 2431, Loss: 1.8342" — both halves required.
    if let Some(rest) = after(line, "Step:") {
        let (step_s, loss_s) = rest.split_once(", Loss:")?;
        return Some(TrainEvent::Step {
            step: step_s.trim().parse().ok()?,
            loss: loss_s.trim().parse().ok()?,
        });
    }

    // "Full step time 1124.5 ms, cache entries: 21"
    if let Some(rest) = after(line, "Full step time") {
        let (ms_s, cache_s) = rest.split_once("ms, cache entries:")?;
        return Some(TrainEvent::StepTime {
            ms: ms_s.trim().parse().ok()?,
            cache_entries: cache_s.trim().parse().ok()?,
        });
    }

    if let Some(v) = after(line, "Number of parameters:") {
        return Some(TrainEvent::ParamCount(v.parse().ok()?));
    }
    if let Some(v) = after(line, "Gradient accumulation steps") {
        return Some(TrainEvent::GradAccum(v.parse().ok()?));
    }
    if let Some(v) = after(line, "Max steps") {
        return Some(TrainEvent::MaxSteps(v.parse().ok()?));
    }
    if let Some(v) = after(line, "Batch size") {
        return Some(TrainEvent::BatchSize(v.parse().ok()?));
    }
    if let Some(v) = after(line, "Scheduler type") {
        if v.is_empty() {
            return None;
        }
        return Some(TrainEvent::Scheduler(v.to_string()));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every string below is the verbatim shape tt-train's fmt::print emits
    // (sources/examples/nano_gpt/main.cpp) — do not "tidy" them.
    #[test]
    fn parses_the_per_step_line() {
        match parse_train_line("Step: 2431, Loss: 1.8342") {
            Some(TrainEvent::Step { step, loss }) => {
                assert_eq!(step, 2431);
                assert!((loss - 1.8342).abs() < 1e-6, "loss={loss}");
            }
            other => panic!("expected Step, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_step_time_line() {
        match parse_train_line("Full step time 1124.5 ms, cache entries: 21") {
            Some(TrainEvent::StepTime { ms, cache_entries }) => {
                assert!((ms - 1124.5).abs() < 1e-3);
                assert_eq!(cache_entries, 21);
            }
            other => panic!("expected StepTime, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_startup_config_lines() {
        assert_eq!(
            parse_train_line("Max steps 50000"),
            Some(TrainEvent::MaxSteps(50000))
        );
        assert_eq!(
            parse_train_line("Batch size 64"),
            Some(TrainEvent::BatchSize(64))
        );
        assert_eq!(
            parse_train_line("Gradient accumulation steps 4"),
            Some(TrainEvent::GradAccum(4))
        );
        assert_eq!(
            parse_train_line("Number of parameters: 11200000"),
            Some(TrainEvent::ParamCount(11_200_000))
        );
        match parse_train_line("Scheduler type cosine") {
            Some(TrainEvent::Scheduler(s)) => assert_eq!(s, "cosine"),
            other => panic!("expected Scheduler, got {other:?}"),
        }
    }

    #[test]
    fn ignores_noise_without_panicking() {
        // Framework logger lines, blank lines, MPI rank prefixes, partial writes.
        assert!(parse_train_line("").is_none());
        assert!(parse_train_line("[info] some framework chatter").is_none());
        assert!(parse_train_line("Step: notanumber, Loss: 1.0").is_none());
        assert!(parse_train_line("Step: 5").is_none());
        assert!(parse_train_line("Loss: 1.0").is_none());
    }

    #[test]
    fn tolerates_scientific_notation_and_integer_loss() {
        match parse_train_line("Step: 1, Loss: 4") {
            Some(TrainEvent::Step { loss, .. }) => assert!((loss - 4.0).abs() < 1e-6),
            other => panic!("expected Step, got {other:?}"),
        }
        match parse_train_line("Step: 2, Loss: 1.2e-1") {
            Some(TrainEvent::Step { loss, .. }) => assert!((loss - 0.12).abs() < 1e-6),
            other => panic!("expected Step, got {other:?}"),
        }
    }
}
