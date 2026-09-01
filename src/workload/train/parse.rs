// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Parse tt-train's stdout into structured events. Pure — no I/O.
//!
//! tt-train is C++ and prints with `fmt::print`; these are the only shapes it
//! emits per-run (verbatim from `sources/examples/nano_gpt/main.cpp`):
//!
//! ```text
//! Step: {step}, Loss: {loss}, Time: {ms} ms, cache entries: {n}   (nano_gpt)
//! Step: {step} Loss: {loss}                          (linear_regression)
//! Step: {step} | Average Loss: {loss}                       (mnist_mlp)
//! Max steps {N}
//! Batch size {N}
//! Gradient accumulation steps {N}
//! Scheduler type {name}
//! ```
//!
//! Verified against tt-metal v0.77.0. The three example binaries each spell
//! the per-step line differently, so the step parser matches on the pieces
//! rather than one exact layout. Two shapes an earlier revision of this
//! parser expected — a separate `Full step time …` line, and
//! `Number of parameters: {N}` — are emitted by **no** current binary; they
//! are still accepted (harmless, and other builds may print them) but
//! nothing may depend on them arriving.
//!
//! Anything else (framework logger output, blank lines, a partially-flushed
//! line) yields `None` and is skipped rather than failing the tail.

/// One recognized line of tt-train output.
#[derive(Debug, Clone, PartialEq)]
pub enum TrainEvent {
    Step {
        step: u64,
        loss: f32,
    },
    StepTime {
        ms: f32,
        cache_entries: u32,
    },
    /// The combined per-step line current tt-train emits, carrying the step,
    /// loss, wall time and program-cache count together. Kept distinct from
    /// `Step` + `StepTime` so the parser stays one-event-per-line; the state
    /// expands it into both.
    StepAndTime {
        step: u64,
        loss: f32,
        ms: f32,
        cache_entries: u32,
    },
    MaxSteps(u64),
    BatchSize(u32),
    GradAccum(u32),
    Scheduler(String),
    ParamCount(u64),
    /// Sequence length the run is actually using. Worth its own event
    /// because a harness may *narrow* it below what the model YAML declares
    /// (tt-tnt does), and the run's value is the one tokens/sec must use.
    SeqLen(u32),
    /// A model-topology YAML named in the log rather than on the cmdline.
    /// The monitor resolves it; the parser only reports the name.
    ModelConfigFile(String),
    /// A tqdm progress bar's current state. Several trainers report progress
    /// *only* through a bar — `ttml`'s `SFTTrainer` sets `loss`/`lr` as
    /// postfix and prints no per-step line at all — so without this a run
    /// like that shows an attached process and an empty loss curve.
    BarProgress {
        step: u64,
        max_steps: u64,
        loss: f32,
    },
    /// A Python harness's one-line run summary, carrying several fields at
    /// once. Expanded by the state into the individual events.
    HarnessSummary {
        max_steps: u64,
        batch_size: u32,
        seq_len: u32,
    },
}

/// The integer immediately following `key` in a `key=value` line.
fn kv_num(s: &str, key: &str) -> Option<u64> {
    let i = s.find(key)?;
    let digits: String = s[i + key.len()..]
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Value after `prefix`, trimmed. `None` when the line doesn't start with it.
fn after<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.strip_prefix(prefix).map(|s| s.trim())
}

pub fn parse_train_line(line: &str) -> Option<TrainEvent> {
    let line = line.trim();

    // The per-step line. Each example binary spells it differently, so match
    // on the pieces rather than on one exact layout:
    //
    //   nano_gpt           Step: 2431, Loss: 1.8342, Time: 1124.5 ms, cache entries: 21
    //   linear_regression  Step: 7 Loss: 0.4213
    //   mnist_mlp          Step:    42 | Average Loss: 0.1337
    //
    // Everything after the loss is optional; when the time/cache fields are
    // present (nano_gpt) they arrive on this same line, where older builds
    // put them on a separate "Full step time" line.
    if let Some(rest) = after(line, "Step:") {
        // "Average Loss:" contains "Loss:", so one search covers both.
        let loss_at = rest.find("Loss:")?;
        // The step number is always the leading token, so read it directly
        // rather than trying to unwind whatever separator and "Average "
        // qualifier follow it — those differ per binary and compose in
        // orders that are easy to get subtly wrong.
        let step_s: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let after_loss = &rest[loss_at + "Loss:".len()..];
        // The loss runs to the next comma, or to end of line.
        let (loss_s, tail) = match after_loss.split_once(',') {
            Some((l, t)) => (l, t),
            None => (after_loss, ""),
        };
        let step = step_s.parse().ok()?;
        let loss = loss_s.trim().parse().ok()?;

        // Optional trailing "Time: {} ms" and "cache entries: {}".
        let ms = tail
            .find("Time:")
            .and_then(|i| tail[i + "Time:".len()..].split_once("ms"))
            .and_then(|(v, _)| v.trim().parse::<f32>().ok());
        let cache = tail
            .find("cache entries:")
            .and_then(|i| {
                tail[i + "cache entries:".len()..]
                    .split(',')
                    .next()
                    .map(str::trim)
            })
            .and_then(|v| v.parse::<u32>().ok());

        return Some(match (ms, cache) {
            (Some(ms), Some(cache_entries)) => TrainEvent::StepAndTime {
                step,
                loss,
                ms,
                cache_entries,
            },
            _ => TrainEvent::Step { step, loss },
        });
    }

    // A tqdm progress bar, e.g. from ttml's SFTTrainer:
    //   "SFTTrainer:  12%|#1  | 30/250 [00:45<05:30, 1.50s/it, loss=1.2345, lr=3.00e-04]"
    // The step and budget live in the bar's own "done/total" counter rather
    // than in any printed field, and the loss arrives as a postfix.
    if line.contains("it]") || line.contains("it/s") || line.contains("s/it") {
        // "30/250 [" — anchored on the bracket so a ratio inside the
        // postfix (or a percentage) can't be mistaken for it.
        let counter = line.find(" [").and_then(|br| {
            let head = &line[..br];
            let slash = head.rfind('/')?;
            let done: String = head[..slash]
                .chars()
                .rev()
                .take_while(char::is_ascii_digit)
                .collect();
            let total: String = head[slash + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            Some((
                done.chars().rev().collect::<String>().parse::<u64>().ok()?,
                total.parse::<u64>().ok()?,
            ))
        });
        // `loss=`, but not the tail of `train_loss=` / `val_loss=`, which
        // are different quantities reported by other harnesses.
        let loss = line.match_indices("loss=").find_map(|(i, _)| {
            let prev = line[..i].chars().next_back();
            if matches!(prev, Some(c) if c == '_') {
                return None;
            }
            let v: String = line[i + "loss=".len()..]
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != ',' && *c != ']')
                .collect();
            v.parse::<f32>().ok()
        });
        if let (Some((step, max_steps)), Some(loss)) = (counter, loss) {
            return Some(TrainEvent::BarProgress {
                step,
                max_steps,
                loss,
            });
        }
    }

    // A Python harness's own run summary. tt-tnt prints, on one line:
    //   "tt-tnt training — steps=4000 batch=8 seq_len=512 arch=blackhole"
    // This is the only place a ttml-driven run states its step budget, so
    // without it there is no ETA and no tokens/sec for such a run.
    // Checked before the per-step `step=` shape below, which it cannot be
    // confused with (that one requires `train_loss=`).
    if line.contains("steps=") && line.contains("batch=") {
        if let (Some(max_steps), Some(batch_size)) =
            (kv_num(line, "steps="), kv_num(line, "batch="))
        {
            // seq_len is on the same line for tt-tnt; a harness that omits
            // it still yields the step budget, which is the valuable part.
            let seq_len = kv_num(line, "seq_len=").unwrap_or(0) as u32;
            return Some(TrainEvent::HarnessSummary {
                max_steps,
                batch_size: batch_size as u32,
                seq_len,
            });
        }
    }

    // "  seq_len: 512 (NARROWED from the size's declared ... 2048; ...)"
    // Three variants of the parenthetical exist; only the number matters.
    // Must be read as the run's own value — the model YAML's declared
    // length can be larger, and using that would overstate tokens/sec.
    if let Some(v) = after(line, "seq_len:") {
        let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u32>() {
            return Some(TrainEvent::SeqLen(n));
        }
    }

    // "  model size: 384 (tt-tnt-384.yaml)" — the topology YAML is named in
    // the log rather than passed on the cmdline, because the harness builds
    // its config in Python.
    if let Some(v) = after(line, "model size:") {
        if let Some(open) = v.find('(') {
            let inner = &v[open + 1..];
            if let Some(close) = inner.find(')') {
                let name = inner[..close].trim();
                if name.ends_with(".yaml") || name.ends_with(".yml") {
                    return Some(TrainEvent::ModelConfigFile(name.to_string()));
                }
            }
        }
    }

    // Python harnesses that drive ttml print their own shape rather than
    // tt-train's, because ttml's trainer prints nothing per step. tt-tnt:
    //   "  step=   1234 train_loss=3.3125 val_loss=3.4012 lr=3.000e-04"
    // Only `step=` and `train_loss=` are required; val_loss and lr are
    // optional and the view has nowhere to put them today.
    if let Some(i) = line.find("step=") {
        let rest = &line[i + "step=".len()..];
        let step_s: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Some(j) = rest.find("train_loss=") {
            let loss_s: String = rest[j + "train_loss=".len()..]
                .trim_start()
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if let (Ok(step), Ok(loss)) = (step_s.parse::<u64>(), loss_s.parse::<f32>()) {
                return Some(TrainEvent::Step { step, loss });
            }
        }
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

    /// The three per-step shapes tt-train actually emits, copied from the
    /// `fmt::print` format strings in tt-metal v0.77.0. Each example binary
    /// spells the line differently, and none of them match the two-line
    /// `Step:` + `Full step time` shape this parser was first written
    /// against — a run against any real binary produced an attached view
    /// with a permanently empty loss curve.
    #[test]
    fn parses_the_real_per_step_lines_from_every_example_binary() {
        // nano_gpt — one combined line carrying time and cache entries.
        // main.cpp: "Step: {}, Loss: {}, Time: {} ms, cache entries: {}\n"
        assert_eq!(
            parse_train_line("Step: 2431, Loss: 1.8342, Time: 1124.5 ms, cache entries: 21"),
            Some(TrainEvent::StepAndTime {
                step: 2431,
                loss: 1.8342,
                ms: 1124.5,
                cache_entries: 21,
            }),
        );

        // linear_regression — no comma at all between the two fields.
        // main.cpp: "Step: {} Loss: {}\n"
        assert_eq!(
            parse_train_line("Step: 7 Loss: 0.4213"),
            Some(TrainEvent::Step {
                step: 7,
                loss: 0.4213
            }),
        );

        // mnist_mlp — pipe separator, and "Average Loss" rather than "Loss".
        // main.cpp: "Step: {:5d} | Average Loss: {:.4f}\n"
        assert_eq!(
            parse_train_line("Step:    42 | Average Loss: 0.1337"),
            Some(TrainEvent::Step {
                step: 42,
                loss: 0.1337
            }),
        );
    }

    /// `ttml`'s `SFTTrainer` reports progress *only* through a tqdm bar —
    /// `set_postfix({"loss", "lr"})`, no per-step print at all — so a real
    /// run (tt-tnt's `scripts/train_tool_calling.py`) showed an attached
    /// process and a permanently empty loss curve.
    #[test]
    fn parses_a_tqdm_progress_bar() {
        // Shape built from sft_trainer.py: desc="SFTTrainer", postfix
        // {"loss": "{:.4f}", "lr": "{:.2e}"}.
        assert_eq!(
            parse_train_line(
                "SFTTrainer:  12%|#1        | 30/250 [00:45<05:30,  1.50s/it, loss=1.2345, lr=3.00e-04]"
            ),
            Some(TrainEvent::BarProgress {
                step: 30,
                max_steps: 250,
                loss: 1.2345,
            }),
        );

        // The eval variant adds val_loss; `loss` must still be the one read,
        // not the tail of `val_loss=`.
        assert_eq!(
            parse_train_line(
                "SFTTrainer:  40%|####      | 100/250 [02:30<03:45, 1.5s/it, loss=0.8000, val_loss=0.9500, lr=3.00e-04]"
            ),
            Some(TrainEvent::BarProgress {
                step: 100,
                max_steps: 250,
                loss: 0.8,
            }),
        );

        // Verbatim from a real tt-tnt LoRA run's log (SFTTrainer over 3000
        // steps), block glyphs and all — reconstructing a format by hand is
        // exactly how the per-step shapes came to be wrong.
        assert_eq!(
            parse_train_line(
                "SFTTrainer: 100%|\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2589}| 2998/3000 [04:57<00:00, 10.63it/s, loss=1.5859, lr=2.00e-04]"
            ),
            Some(TrainEvent::BarProgress {
                step: 2998,
                max_steps: 3000,
                loss: 1.5859,
            }),
        );
        assert_eq!(
            parse_train_line(
                "SFTTrainer: 100%|\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}| 3000/3000 [04:58<00:00,  4.73it/s, loss=3.3750, val_loss=1.7712, lr=2.00e-04]"
            ),
            Some(TrainEvent::BarProgress {
                step: 3000,
                max_steps: 3000,
                loss: 3.375,
            }),
        );

        // A bar with no loss postfix yet carries no step data worth showing.
        assert_eq!(
            parse_train_line("SFTTrainer:   0%|          | 0/250 [00:00<?, ?it/s]"),
            None
        );
    }

    /// A bar's step, budget and loss must all reach the state, or the run
    /// has no curve, no progress and no ETA.
    #[test]
    fn a_bar_populates_step_budget_and_loss() {
        let mut st = crate::workload::train::monitor::TrainState::new();
        st.apply_event(
            parse_train_line(
                "SFTTrainer:  12%|#1        | 30/250 [00:45<05:30,  1.50s/it, loss=1.2345, lr=3.00e-04]",
            )
            .expect("the bar must parse"),
        );
        assert_eq!(st.step, 30);
        assert_eq!(st.max_steps, 250);
        assert_eq!(st.loss, Some(1.2345));
        assert_eq!(st.loss_history.len(), 1, "the curve must get a sample");
    }

    /// tt-tnt's shape. `ttml`'s trainer prints nothing per step, so a Python
    /// harness's own logging is the only per-step signal that exists for a
    /// run driven that way.
    #[test]
    fn parses_a_python_harness_step_line() {
        // Verbatim from tt-tnt train/run.py's f-string, including the
        // leading indent and the `:>7` right-alignment of the step.
        assert_eq!(
            parse_train_line("  step=   1234 train_loss=3.3125 val_loss=3.4012 lr=3.000e-04"),
            Some(TrainEvent::Step {
                step: 1234,
                loss: 3.3125
            }),
        );
        // lr is optional (only present for non-constant schedules).
        assert_eq!(
            parse_train_line("  step=      1 train_loss=4.8210 val_loss=4.9001"),
            Some(TrainEvent::Step {
                step: 1,
                loss: 4.8210
            }),
        );
        // A line mentioning steps but carrying no train_loss is not a step.
        assert_eq!(parse_train_line("  step=   12 val_loss=3.1"), None);
    }

    /// Every string here is copied verbatim from the log of the real tt-tnt
    /// run recorded on 4× Blackhole — not retyped from the source, which is
    /// how the per-step shapes came to be wrong in the first place.
    #[test]
    fn parses_a_python_harnesss_run_summary_and_topology_lines() {
        assert_eq!(
            parse_train_line("tt-tnt training — steps=4000 batch=8 seq_len=512 arch=blackhole"),
            Some(TrainEvent::HarnessSummary {
                max_steps: 4000,
                batch_size: 8,
                seq_len: 512,
            }),
            "the run summary is the only place a ttml run states its step budget"
        );

        // The narrowing variant: the run uses 512 even though the model YAML
        // declares 2048. Reading the YAML's number would overstate tokens/sec
        // fourfold, so the run's own value has to win.
        assert_eq!(
            parse_train_line(
                "  seq_len: 512 (NARROWED from the size's declared max_sequence_length 2048; \
                 RoPE cache is read as a prefix)"
            ),
            Some(TrainEvent::SeqLen(512)),
        );
        assert_eq!(
            parse_train_line("  seq_len: 2048 (size's max_sequence_length)"),
            Some(TrainEvent::SeqLen(2048)),
        );

        assert_eq!(
            parse_train_line("  model size: 384 (tt-tnt-384.yaml)"),
            Some(TrainEvent::ModelConfigFile("tt-tnt-384.yaml".to_string())),
        );
        // A parenthetical that isn't a config file must not be taken as one.
        assert_eq!(parse_train_line("  model size: 384 (the big one)"), None);
    }

    /// The summary must not be mistaken for a per-step line, or a run's step
    /// budget would be read as its current step.
    #[test]
    fn the_run_summary_is_not_confused_with_a_step_line() {
        let ev =
            parse_train_line("tt-tnt training — steps=4000 batch=8 seq_len=512 arch=blackhole");
        assert!(
            !matches!(ev, Some(TrainEvent::Step { .. })),
            "`steps=` must not be read as the per-step `step=` shape"
        );
    }

    /// The summary expands into the same fields the tt-train startup lines
    /// set, so ETA and tokens/sec work identically for either kind of run.
    #[test]
    fn a_run_summary_yields_eta_and_tokens_per_sec() {
        let mut st = crate::workload::train::monitor::TrainState::new();
        st.apply_event(
            parse_train_line("tt-tnt training — steps=4000 batch=8 seq_len=512 arch=blackhole")
                .unwrap(),
        );
        assert_eq!(st.max_steps, 4000);
        assert_eq!(st.batch_size, 8);
        assert_eq!(st.config.max_sequence_length, Some(512));

        // With a step time known, both derived figures become available.
        st.apply_event(TrainEvent::Step {
            step: 100,
            loss: 3.0,
        });
        st.step_ms = 500.0;
        assert!(st.eta_secs().is_some(), "ETA needs the step budget");
        assert_eq!(
            st.tokens_per_sec(),
            Some(8.0 * 512.0 * 2.0),
            "batch × seq_len × 1/step_ms, with grad_accum defaulting to 1"
        );
    }

    /// A combined line must feed step time and cache entries through, or the
    /// derived tokens/sec, step/s, ETA and the kernel-cache shimmer all stay
    /// dead on exactly the binary the view is most likely pointed at.
    #[test]
    fn a_combined_step_line_populates_step_time_and_cache_entries() {
        let mut st = crate::workload::train::monitor::TrainState::new();
        let ev = parse_train_line("Step: 10, Loss: 2.5, Time: 800 ms, cache entries: 34")
            .expect("the real nano_gpt line must parse");
        st.apply_event(ev);
        assert_eq!(st.step, 10);
        assert_eq!(st.loss, Some(2.5));
        assert_eq!(st.step_ms, 800.0, "step time must reach the state");
        assert_eq!(st.cache_entries, 34, "cache entries must reach the state");
        assert!(st.steps_per_sec() > 0.0, "step/s must be derivable");
    }

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
