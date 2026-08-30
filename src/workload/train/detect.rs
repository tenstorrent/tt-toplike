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

#[cfg(test)]
mod tests {
    use super::*;

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
