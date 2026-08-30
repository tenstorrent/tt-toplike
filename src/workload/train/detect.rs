// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Recognize a live tt-train process from its name + cmdline. Pure — no I/O.

/// tt-train example binaries we recognize. Matching is on the binary *name*
/// only: the flag sets differ across tt-metal versions (one checkout's
/// `nano_gpt` takes `-c/-n/--multihost`, another's takes `-i/-p/-s/-m`), so
/// keying on flags would silently stop detecting a run after an upgrade.
pub const TRAIN_BINARIES: &[&str] = &["nano_gpt", "mnist_mlp", "linear_regression"];

/// A detected tt-train run.
#[derive(Debug, Clone, PartialEq)]
pub struct TrainProcess {
    pub pid: i32,
    /// Bare binary name (e.g. `nano_gpt`), never the full path.
    pub binary: String,
    /// Value of `-c` / `--config`, when this version's CLI has one.
    pub config_path: Option<String>,
}

/// Extract `-c <path>` / `--config <path>` / `--config=<path>` from an argv.
fn config_arg(toks: &[&str]) -> Option<String> {
    for (i, t) in toks.iter().enumerate() {
        if *t == "-c" || *t == "--config" {
            return toks.get(i + 1).map(|s| s.to_string());
        }
        if let Some(v) = t.strip_prefix("--config=") {
            return Some(v.to_string());
        }
    }
    None
}

/// `Some(TrainProcess)` when this process is a tt-train run.
///
/// The binary is matched by the last path component of either `name` or
/// argv[0], so both `nano_gpt` and `/long/build/path/nano_gpt` resolve.
pub fn parse_train_process(name: &str, cmdline: &str, pid: i32) -> Option<TrainProcess> {
    let toks: Vec<&str> = cmdline.split_whitespace().collect();
    let argv0_base = toks
        .first()
        .and_then(|t| t.rsplit('/').next())
        .unwrap_or("");
    let name_base = name.rsplit('/').next().unwrap_or(name);

    let binary = TRAIN_BINARIES
        .iter()
        .find(|b| **b == argv0_base || **b == name_base)?;

    Some(TrainProcess {
        pid,
        binary: (*binary).to_string(),
        config_path: config_arg(&toks),
    })
}

/// True when `name`/argv[0] is a Python interpreter (`python`, `python3`,
/// `python3.12`, or a venv path ending in one of those).
fn is_python(base: &str) -> bool {
    base == "python"
        || base
            .strip_prefix("python")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_digit()))
}

/// `Some(TrainProcess)` for a Python harness that drives tt-train through
/// `ttml` — tt-metal's own `tt-train/sources/ttml` Python bindings.
///
/// tt-train ships three C++ examples, but real projects here (tt-tnt, for
/// one) train by importing `ttml` from Python instead, so matching only the
/// example binary names misses the runs people actually launch.
///
/// `ttml_loaded` is the gate and must come from the caller's inspection of
/// the process's mapped libraries — a cmdline alone can't distinguish a
/// training harness from any other script called `run.py`, and this view
/// attaching to the wrong process is worse than not attaching. It mirrors
/// how HivemindSweeper classifies interpreter-hosted workloads.
pub fn parse_python_trainer(cmdline: &str, pid: i32, ttml_loaded: bool) -> Option<TrainProcess> {
    if !ttml_loaded {
        return None;
    }
    let toks: Vec<&str> = cmdline.split_whitespace().collect();
    let argv0_base = toks.first().and_then(|t| t.rsplit('/').next())?;
    if !is_python(argv0_base) {
        return None;
    }
    // Name the run after its script, so the header reads `run.py` rather
    // than a bare `python3` that says nothing about which run this is.
    // A bare REPL (no script argument) is not a training run.
    let script = toks
        .iter()
        .skip(1)
        .find(|t| t.ends_with(".py"))
        .and_then(|t| t.rsplit('/').next())?;

    Some(TrainProcess {
        pid,
        binary: script.to_string(),
        config_path: config_arg(&toks),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tt-tnt's real launch shape. The `ttml`-loaded gate is what makes this
    /// safe: without it, every `run.py` on the box would look like training.
    #[test]
    fn recognizes_a_python_harness_that_drives_ttml() {
        let cmd = "python3 train/run.py --size 384 --steps 200 --val-every 1";
        let p = parse_python_trainer(cmd, 9001, true).expect("must detect a ttml python trainer");
        assert_eq!(p.pid, 9001);
        assert_eq!(p.binary, "run.py", "the run is named after its script");

        assert!(
            parse_python_trainer(cmd, 9001, false).is_none(),
            "without ttml mapped this is just some script called run.py"
        );
    }

    #[test]
    fn python_trainer_detection_needs_an_actual_script() {
        // A bare interpreter — or a shell that merely mentions one — is not
        // a training run, however its libraries are mapped.
        assert!(parse_python_trainer("python3", 1, true).is_none());
        assert!(parse_python_trainer("python3 -i", 2, true).is_none());
        assert!(parse_python_trainer("bash run.py", 3, true).is_none());
    }

    #[test]
    fn python_trainer_accepts_versioned_and_venv_interpreters() {
        for argv0 in [
            "python",
            "python3",
            "python3.12",
            "/home/u/.venv/bin/python3",
        ] {
            let cmd = format!("{argv0} train/run.py");
            assert!(
                parse_python_trainer(&cmd, 4, true).is_some(),
                "{argv0} must be recognized as an interpreter"
            );
        }
        // Something merely *starting* with "python" isn't an interpreter.
        assert!(parse_python_trainer("pythonish train/run.py", 5, true).is_none());
    }

    #[test]
    fn python_trainer_picks_up_a_config_flag() {
        let p = parse_python_trainer(
            "python3 train/run.py --config train/configs/nanollama3_bpe_v2.yaml",
            6,
            true,
        )
        .unwrap();
        assert_eq!(
            p.config_path.as_deref(),
            Some("train/configs/nanollama3_bpe_v2.yaml")
        );
    }

    #[test]
    fn recognizes_nano_gpt_by_binary_name_not_flags() {
        // Real shape: an absolute path to a compiled binary in the build tree.
        let p = parse_train_process(
            "nano_gpt",
            "/home/u/tt-metal/tt-train/build/sources/examples/nano_gpt/nano_gpt -c training_shakespeare_nanollama3.yaml",
            48213,
        )
        .expect("must detect nano_gpt");
        assert_eq!(p.pid, 48213);
        assert_eq!(p.binary, "nano_gpt");
        assert_eq!(
            p.config_path.as_deref(),
            Some("training_shakespeare_nanollama3.yaml")
        );
    }

    #[test]
    fn recognizes_an_older_flag_set_because_matching_is_on_binary_name() {
        // A different tt-metal checkout uses `-i -p -s -m` instead of `-c`.
        // Detection must still succeed; only config_path is unknown.
        let p = parse_train_process("mnist_mlp", "./mnist_mlp -i 100 -p mnist_mlp.msgpack", 77)
            .expect("must detect mnist_mlp regardless of flag set");
        assert_eq!(p.binary, "mnist_mlp");
        assert_eq!(p.config_path, None);
    }

    #[test]
    fn accepts_long_form_config_flag() {
        let p = parse_train_process("nano_gpt", "nano_gpt --config /etc/train.yaml", 5).unwrap();
        assert_eq!(p.config_path.as_deref(), Some("/etc/train.yaml"));
    }

    #[test]
    fn rejects_unrelated_processes() {
        assert!(parse_train_process("bash", "bash -c ls", 1).is_none());
        assert!(parse_train_process("python3", "python3 train.py", 2).is_none());
        // A vLLM serve process must not be mistaken for training.
        assert!(parse_train_process("vllm", "vllm serve meta-llama/Llama-3", 3).is_none());
    }
}
