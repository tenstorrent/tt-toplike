# Training View ("Robot Brain Food") Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new full-screen TUI view (`t`) that auto-attaches to a live tt-train run and visualizes it as a character-grid neural network being fed tokens, with loss "mountains" against a twinkling aurora nightscape.

**Architecture:** Pure parsers under `src/workload/train/` (detect / parse / config) with all I/O confined to a `monitor.rs` that discovers the pid, resolves its log via `/proc/<pid>/fd/1`, tails it, and polls the checkpoint mtime. A renderer in `src/animation/train_view.rs` turns the resulting `TrainState` into `Vec<Line>`, wired into `ui/tui/mod.rs` as `DisplayMode::Training`. This mirrors the existing `inference_server/` split (pure logic, isolated I/O) and the existing `DefragVis` render pattern.

**Tech Stack:** Rust, ratatui 0.30, existing tt-toplike backends. No new dependencies — YAML parsing uses hand-rolled line scanning (the two keys we need are flat scalars; adding serde_yaml for that would be unjustified weight).

**Spec:** `docs/superpowers/specs/2026-08-29-training-view-design.md`

## Global Constraints

- **Left-and-bottom borders only** (`╔ ║ ╚`) — never right-side border characters; they wrap when the terminal is narrower than expected.
- **Match tt-train processes on binary NAME, never on flags** — flag sets demonstrably drift across tt-metal versions (`-c/-n/--multihost` vs `-i/-p/-s/-m`).
- **Never fabricate a metric tt-train doesn't emit live.** No gradient norms, no MFU, no throughput counter. Only: step, loss, step-time ms, cache entries, the startup config lines, and checkpoint mtime. Everything else shown must be derived from those or from tt-toplike's own chip telemetry.
- **Every stage degrades, never panics.** No process → scanning state. fd 1 not a regular file → process-level view + honest explanation. Unparseable line → skip it. Missing YAML → omit the model card.
- **Never render a fake or empty loss curve** when the log is unavailable — say why instead.
- **Pure parsers take `&str` and return values; they perform no I/O** so they are unit-testable with no running trainer.
- All new panels/rows must size to their content in real display columns and must not overflow a narrow terminal.

---

### Task 1: tt-train process detection (pure)

**Files:**
- Create: `src/workload/train/mod.rs`
- Create: `src/workload/train/detect.rs`
- Modify: `src/workload/mod.rs` (add `pub mod train;`)

**Interfaces:**
- Produces: `pub struct TrainProcess { pub pid: i32, pub binary: String, pub config_path: Option<String> }`, `pub fn parse_train_process(name: &str, cmdline: &str, pid: i32) -> Option<TrainProcess>`, `pub const TRAIN_BINARIES: &[&str]`

- [ ] **Step 1: Write the failing test**

Create `src/workload/train/detect.rs` with only the test module first:

```rust
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
        assert_eq!(p.config_path.as_deref(), Some("training_shakespeare_nanollama3.yaml"));
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train::detect`
Expected: FAIL — `cannot find function parse_train_process`

- [ ] **Step 3: Write minimal implementation**

Prepend to `src/workload/train/detect.rs`:

```rust
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
```

Create `src/workload/train/mod.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Live tt-train run monitoring: detection, log parsing, config reading.
//!
//! Split follows `inference_server/`: pure logic here and in `parse`/`config`,
//! all I/O confined to `monitor`.

pub mod detect;

pub use detect::{parse_train_process, TrainProcess, TRAIN_BINARIES};
```

Add to `src/workload/mod.rs` after `pub mod serving;`:

```rust
pub mod train;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui train::detect`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add src/workload/train/ src/workload/mod.rs
git commit -m "feat(train): recognize tt-train processes by binary name"
```

---

### Task 2: tt-train stdout parsing (pure)

**Files:**
- Create: `src/workload/train/parse.rs`
- Modify: `src/workload/train/mod.rs`

**Interfaces:**
- Produces: `pub enum TrainEvent { Step { step: u64, loss: f32 }, StepTime { ms: f32, cache_entries: u32 }, MaxSteps(u64), BatchSize(u32), GradAccum(u32), Scheduler(String), ParamCount(u64) }`, `pub fn parse_train_line(line: &str) -> Option<TrainEvent>`

- [ ] **Step 1: Write the failing test**

Create `src/workload/train/parse.rs` with only the test module:

```rust
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
        assert_eq!(parse_train_line("Max steps 50000"), Some(TrainEvent::MaxSteps(50000)));
        assert_eq!(parse_train_line("Batch size 64"), Some(TrainEvent::BatchSize(64)));
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train::parse`
Expected: FAIL — `cannot find function parse_train_line`

- [ ] **Step 3: Write minimal implementation**

Prepend to `src/workload/train/parse.rs`:

```rust
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
```

Add to `src/workload/train/mod.rs`:

```rust
pub mod parse;

pub use parse::{parse_train_line, TrainEvent};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui train::parse`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add src/workload/train/
git commit -m "feat(train): parse tt-train stdout into structured events"
```

---

### Task 3: Training YAML config reading (pure parse + thin loader)

**Files:**
- Create: `src/workload/train/config.rs`
- Modify: `src/workload/train/mod.rs`

**Interfaces:**
- Produces: `pub struct TrainConfig { pub model_save_path: Option<String>, pub model_config_path: Option<String>, pub num_blocks: Option<u32>, pub num_heads: Option<u32>, pub embedding_dim: Option<u32>, pub vocab_size: Option<u32>, pub max_sequence_length: Option<u32>, pub learning_rate: Option<f32> }`, `pub fn parse_train_yaml(text: &str) -> TrainConfig`, `pub fn merge_model_yaml(cfg: &mut TrainConfig, text: &str)`

- [ ] **Step 1: Write the failing test**

Create `src/workload/train/config.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Shape taken from configs/training_configs/training_shakespeare_nanollama3.yaml
    const TRAINING_YAML: &str = r#"
training_config:
  project_name: "tt_train_nano_gpt"
  seed: 5489
  model_save_interval: 500
  batch_size: 64
  num_epochs: 1
  max_steps: 50000
  learning_rate: 0.0003
  weight_decay: 0.01
  use_clip_grad_norm: true
  model_path: "transformer.msgpack"
  transformer_config: "configs/model_configs/nanollama3.yaml"
"#;

    const MODEL_YAML: &str = r#"
transformer_config:
  num_heads: 6
  embedding_dim: 384
  dropout_prob: 0.0
  num_blocks: 6
  vocab_size: 32000
  max_sequence_length: 256
"#;

    #[test]
    fn reads_the_training_yaml_fields_we_use() {
        let c = parse_train_yaml(TRAINING_YAML);
        assert_eq!(c.model_save_path.as_deref(), Some("transformer.msgpack"));
        assert_eq!(
            c.model_config_path.as_deref(),
            Some("configs/model_configs/nanollama3.yaml")
        );
        assert!((c.learning_rate.unwrap() - 0.0003).abs() < 1e-9);
    }

    #[test]
    fn merges_the_model_yaml_topology() {
        let mut c = parse_train_yaml(TRAINING_YAML);
        merge_model_yaml(&mut c, MODEL_YAML);
        assert_eq!(c.num_blocks, Some(6));
        assert_eq!(c.num_heads, Some(6));
        assert_eq!(c.embedding_dim, Some(384));
        assert_eq!(c.vocab_size, Some(32000));
        assert_eq!(c.max_sequence_length, Some(256));
    }

    #[test]
    fn missing_or_garbage_yaml_yields_all_none_not_a_panic() {
        let c = parse_train_yaml("");
        assert_eq!(c.model_save_path, None);
        assert_eq!(c.num_blocks, None);

        let c2 = parse_train_yaml("!!! not : yaml : at all [[[");
        assert_eq!(c2.model_save_path, None);

        // A key present but with a non-numeric value must not panic.
        let c3 = parse_train_yaml("  max_steps: not_a_number\n  model_path: \"x.msgpack\"");
        assert_eq!(c3.model_save_path.as_deref(), Some("x.msgpack"));
    }

    #[test]
    fn strips_quotes_and_inline_comments() {
        let c = parse_train_yaml("  model_path: 'ckpt.msgpack'  # rolling save\n");
        assert_eq!(c.model_save_path.as_deref(), Some("ckpt.msgpack"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train::config`
Expected: FAIL — `cannot find function parse_train_yaml`

- [ ] **Step 3: Write minimal implementation**

Prepend to `src/workload/train/config.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Read the handful of fields we need out of tt-train's YAML configs.
//!
//! Deliberately a line scanner rather than a YAML dependency: every field we
//! consume is a flat `key: value` scalar nested one level under a known
//! section, and the view degrades to "omit the model card" on anything it
//! can't read — so a full YAML parser would be weight without benefit. A key
//! that appears with an unparseable value is left `None` rather than failing
//! the whole config.

/// The subset of tt-train's training + model config the view uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrainConfig {
    /// `model_path` — the single rolling checkpoint file, mtime-watched.
    pub model_save_path: Option<String>,
    /// `transformer_config` — path to the model-topology YAML.
    pub model_config_path: Option<String>,
    pub num_blocks: Option<u32>,
    pub num_heads: Option<u32>,
    pub embedding_dim: Option<u32>,
    pub vocab_size: Option<u32>,
    pub max_sequence_length: Option<u32>,
    pub learning_rate: Option<f32>,
}

/// Value for `key` in a flat `key: value` line, quotes and inline `#`
/// comments stripped. Only matches a line whose trimmed form starts with
/// `key:`, so `model_path` never matches `transformer_model_path`.
fn scalar<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix(key) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let mut v = rest.trim();
        if let Some(hash) = v.find('#') {
            v = v[..hash].trim();
        }
        v = v.trim_matches('"').trim_matches('\'').trim();
        if v.is_empty() {
            return None;
        }
        return Some(v);
    }
    None
}

fn u32_of(text: &str, key: &str) -> Option<u32> {
    scalar(text, key)?.parse().ok()
}

/// Parse the training config YAML (the one passed to `-c`).
pub fn parse_train_yaml(text: &str) -> TrainConfig {
    TrainConfig {
        model_save_path: scalar(text, "model_path").map(|s| s.to_string()),
        model_config_path: scalar(text, "transformer_config").map(|s| s.to_string()),
        learning_rate: scalar(text, "learning_rate").and_then(|s| s.parse().ok()),
        ..Default::default()
    }
}

/// Merge the model-topology YAML (the file `transformer_config` points at).
pub fn merge_model_yaml(cfg: &mut TrainConfig, text: &str) {
    cfg.num_blocks = u32_of(text, "num_blocks");
    cfg.num_heads = u32_of(text, "num_heads");
    cfg.embedding_dim = u32_of(text, "embedding_dim");
    cfg.vocab_size = u32_of(text, "vocab_size");
    cfg.max_sequence_length = u32_of(text, "max_sequence_length");
}
```

Add to `src/workload/train/mod.rs`:

```rust
pub mod config;

pub use config::{merge_model_yaml, parse_train_yaml, TrainConfig};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui train::config`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add src/workload/train/
git commit -m "feat(train): read the training + model YAML fields the view uses"
```

---

### Task 4: Log discovery via `/proc/<pid>/fd/1` (the load-bearing auto-attach step)

**Files:**
- Create: `src/workload/train/logsrc.rs`
- Modify: `src/workload/train/mod.rs`

**Interfaces:**
- Produces: `pub enum LogSource { File(std::path::PathBuf), NotRedirected }`, `pub fn discover_log(pid: i32) -> LogSource`, `pub fn classify_fd_target(target: &std::path::Path) -> LogSource`

This is the step that makes zero-command attachment possible, so it gets a
test against a **real spawned process**, not a mock.

- [ ] **Step 1: Write the failing test**

Create `src/workload/train/logsrc.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_pipe_or_tty_as_not_redirected() {
        // What readlink returns when stdout is a pipe or a terminal.
        assert_eq!(
            classify_fd_target(std::path::Path::new("pipe:[123456]")),
            LogSource::NotRedirected
        );
        assert_eq!(
            classify_fd_target(std::path::Path::new("/dev/pts/3")),
            LogSource::NotRedirected
        );
        assert_eq!(
            classify_fd_target(std::path::Path::new("socket:[99]")),
            LogSource::NotRedirected
        );
    }

    /// The real thing: spawn a process with stdout redirected to a file and
    /// confirm we can recover that path from outside knowing only the pid.
    /// A mock cannot prove this works, and the whole auto-attach UX rests on
    /// it, so this test drives a genuine process.
    #[test]
    #[cfg(target_os = "linux")]
    fn discovers_the_log_path_of_a_real_redirected_process() {
        use std::process::{Command, Stdio};

        let dir = std::env::temp_dir().join(format!("ttlog_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("train.log");
        let f = std::fs::File::create(&log).unwrap();

        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .stdout(Stdio::from(f))
            .spawn()
            .expect("spawn test child");

        let found = discover_log(child.id() as i32);

        child.kill().ok();
        child.wait().ok();

        match found {
            LogSource::File(p) => assert_eq!(
                p.canonicalize().unwrap(),
                log.canonicalize().unwrap(),
                "must recover the real redirected log path"
            ),
            LogSource::NotRedirected => panic!("a file-redirected stdout must be discoverable"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn reports_not_redirected_when_stdout_is_a_pipe() {
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn test child");
        let found = discover_log(child.id() as i32);
        child.kill().ok();
        child.wait().ok();
        assert_eq!(found, LogSource::NotRedirected);
    }

    #[test]
    fn a_dead_pid_is_not_redirected_rather_than_an_error() {
        assert_eq!(discover_log(999_999_998), LogSource::NotRedirected);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train::logsrc`
Expected: FAIL — `cannot find function discover_log`

- [ ] **Step 3: Write minimal implementation**

Prepend to `src/workload/train/logsrc.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Find a running tt-train process's log without being told where it is.
//!
//! `/proc/<pid>/fd/1` is a symlink to whatever stdout actually is. For the
//! common long-run launch shape (`./nano_gpt … > train.log &`) it resolves to
//! the real file, which we can then tail from outside knowing only the pid —
//! this is what lets the Training view attach with no command from the user.
//!
//! When stdout is a pipe or a terminal the link reads `pipe:[…]` /
//! `/dev/pts/N` instead. There is no way to retroactively read a process's
//! un-redirected stdout — that's an OS property, not a gap here — so we report
//! `NotRedirected` and the view explains the situation rather than inventing
//! data.

use std::path::{Path, PathBuf};

/// Where (if anywhere) a process's stdout can be read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSource {
    /// stdout is redirected to this regular file — tailable.
    File(PathBuf),
    /// stdout is a pipe/tty/socket — per-step metrics are unavailable.
    NotRedirected,
}

/// Classify a resolved `/proc/<pid>/fd/1` target. Pure, so the pipe/tty
/// shapes are testable without spawning anything.
pub fn classify_fd_target(target: &Path) -> LogSource {
    let s = target.to_string_lossy();
    // Anonymous fds render as `pipe:[N]` / `socket:[N]`; a tty is under /dev.
    if s.starts_with("pipe:") || s.starts_with("socket:") || s.starts_with("anon_inode:") {
        return LogSource::NotRedirected;
    }
    if s.starts_with("/dev/pts/") || s == "/dev/null" || s == "/dev/tty" {
        return LogSource::NotRedirected;
    }
    if target.is_file() {
        return LogSource::File(target.to_path_buf());
    }
    LogSource::NotRedirected
}

/// Resolve a pid's stdout. Never errors: an exited process, a permission
/// denial, or a non-Linux target all read as `NotRedirected`.
pub fn discover_log(pid: i32) -> LogSource {
    match std::fs::read_link(format!("/proc/{pid}/fd/1")) {
        Ok(target) => classify_fd_target(&target),
        Err(_) => LogSource::NotRedirected,
    }
}
```

Add to `src/workload/train/mod.rs`:

```rust
pub mod logsrc;

pub use logsrc::{classify_fd_target, discover_log, LogSource};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui train::logsrc`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add src/workload/train/
git commit -m "feat(train): discover a running trainer's log via /proc/<pid>/fd/1"
```

---

### Task 5: The monitor — state, tailing, checkpoint mtime

**Files:**
- Create: `src/workload/train/monitor.rs`
- Modify: `src/workload/train/mod.rs`

**Interfaces:**
- Consumes: Tasks 1–4 (`TrainProcess`, `TrainEvent`, `TrainConfig`, `LogSource`).
- Produces:
  ```rust
  pub struct TrainState {
      pub proc: Option<TrainProcess>,
      pub config: TrainConfig,
      pub log: LogSource,
      pub step: u64,
      pub max_steps: u64,
      pub loss: Option<f32>,
      pub prev_loss: Option<f32>,
      pub loss_history: Vec<f32>,
      pub step_ms: f32,
      pub cache_entries: u32,
      pub batch_size: u32,
      pub grad_accum: u32,
      pub scheduler: Option<String>,
      pub param_count: u64,
      pub checkpoint_step: u64,
      pub checkpoint_pulse: u8,
      pub first_seen: Option<std::time::Instant>,
  }
  ```
  `pub const LOSS_HISTORY: usize = 512;`, `TrainState::new()`, `pub fn apply_event(&mut self, ev: TrainEvent)`, `pub fn steps_per_sec(&self) -> f32`, `pub fn tokens_per_sec(&self) -> Option<f32>`, `pub fn eta_secs(&self) -> Option<f32>`, `pub struct TrainMonitor` with `TrainMonitor::new()`, `pub fn poll(&mut self)`, `pub fn state(&self) -> &TrainState`.

- [ ] **Step 1: Write the failing test**

Create `src/workload/train/monitor.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn events_fold_into_state() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::MaxSteps(50000));
        st.apply_event(TrainEvent::BatchSize(64));
        st.apply_event(TrainEvent::GradAccum(4));
        st.apply_event(TrainEvent::ParamCount(11_200_000));
        st.apply_event(TrainEvent::Scheduler("cosine".into()));
        st.apply_event(TrainEvent::Step { step: 1, loss: 4.5 });
        st.apply_event(TrainEvent::StepTime { ms: 1000.0, cache_entries: 7 });

        assert_eq!(st.max_steps, 50000);
        assert_eq!(st.batch_size, 64);
        assert_eq!(st.grad_accum, 4);
        assert_eq!(st.param_count, 11_200_000);
        assert_eq!(st.scheduler.as_deref(), Some("cosine"));
        assert_eq!(st.step, 1);
        assert_eq!(st.loss, Some(4.5));
        assert_eq!(st.loss_history, vec![4.5]);
        assert_eq!(st.cache_entries, 7);
    }

    #[test]
    fn prev_loss_tracks_the_previous_step_for_the_delta_arrow() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::Step { step: 1, loss: 4.0 });
        assert_eq!(st.prev_loss, None, "no delta is knowable on the first step");
        st.apply_event(TrainEvent::Step { step: 2, loss: 3.5 });
        assert_eq!(st.prev_loss, Some(4.0));
        assert_eq!(st.loss, Some(3.5));
    }

    #[test]
    fn loss_history_is_bounded() {
        let mut st = TrainState::new();
        for i in 0..(LOSS_HISTORY + 50) {
            st.apply_event(TrainEvent::Step { step: i as u64, loss: 1.0 });
        }
        assert_eq!(st.loss_history.len(), LOSS_HISTORY);
    }

    #[test]
    fn derived_rates_need_real_inputs_and_never_divide_by_zero() {
        let mut st = TrainState::new();
        assert_eq!(st.steps_per_sec(), 0.0, "no step time yet");
        assert_eq!(st.tokens_per_sec(), None, "no batch/seq_len yet");
        assert_eq!(st.eta_secs(), None, "no max_steps yet");

        st.apply_event(TrainEvent::StepTime { ms: 1000.0, cache_entries: 1 });
        assert!((st.steps_per_sec() - 1.0).abs() < 1e-6);

        // tokens/sec needs batch × seq_len × accum, and seq_len is YAML-only.
        st.apply_event(TrainEvent::BatchSize(64));
        st.apply_event(TrainEvent::GradAccum(4));
        assert_eq!(st.tokens_per_sec(), None, "still no seq_len");
        st.config.max_sequence_length = Some(256);
        let tps = st.tokens_per_sec().expect("now derivable");
        assert!((tps - 65536.0).abs() < 1.0, "tps={tps}");

        st.apply_event(TrainEvent::MaxSteps(10));
        st.apply_event(TrainEvent::Step { step: 4, loss: 1.0 });
        let eta = st.eta_secs().expect("derivable");
        assert!((eta - 6.0).abs() < 0.01, "eta={eta}");
    }

    #[test]
    fn tailing_reads_only_newly_appended_lines() {
        let dir = std::env::temp_dir().join(format!("tttail_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.log");
        std::fs::write(&path, "Step: 1, Loss: 4.0\n").unwrap();

        let mut t = Tailer::new(path.clone());
        let first = t.read_new();
        assert_eq!(first, vec!["Step: 1, Loss: 4.0"]);

        // Nothing appended → nothing returned (not a re-read).
        assert!(t.read_new().is_empty());

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "Step: 2, Loss: 3.5").unwrap();
        assert_eq!(t.read_new(), vec!["Step: 2, Loss: 3.5"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_truncated_or_rotated_log_reseeks_instead_of_reading_garbage() {
        let dir = std::env::temp_dir().join(format!("ttrot_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.log");
        std::fs::write(&path, "Step: 1, Loss: 4.0\nStep: 2, Loss: 3.9\n").unwrap();

        let mut t = Tailer::new(path.clone());
        assert_eq!(t.read_new().len(), 2);

        // Rotation: file replaced with a shorter one.
        std::fs::write(&path, "Step: 9, Loss: 1.0\n").unwrap();
        assert_eq!(
            t.read_new(),
            vec!["Step: 9, Loss: 1.0"],
            "shrinking file must reset the offset, not skip past the new content"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn checkpoint_mtime_bump_raises_a_pulse_once() {
        let dir = std::env::temp_dir().join(format!("ttckpt_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.msgpack");
        std::fs::write(&path, b"a").unwrap();

        let mut w = CheckpointWatch::new(path.clone());
        assert!(!w.poll(), "first observation establishes a baseline");
        assert!(!w.poll(), "unchanged file does not pulse");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, b"bb").unwrap();
        assert!(w.poll(), "an mtime bump pulses once");
        assert!(!w.poll(), "and only once");

        std::fs::remove_dir_all(&dir).ok();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train::monitor`
Expected: FAIL — `cannot find type TrainState`

- [ ] **Step 3: Write minimal implementation**

Prepend to `src/workload/train/monitor.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Live tt-train run state: discovery, log tailing, checkpoint watching.
//!
//! All the I/O for this subsystem lives here; `detect`/`parse`/`config`/
//! `logsrc` stay pure. Nothing here returns an error to the caller — a run
//! that vanishes, a log that can't be read, or a config that won't parse all
//! degrade to a less-populated `TrainState`, because a monitoring view must
//! keep drawing.

use super::config::{merge_model_yaml, parse_train_yaml, TrainConfig};
use super::detect::{parse_train_process, TrainProcess};
use super::logsrc::{discover_log, LogSource};
use super::parse::{parse_train_line, TrainEvent};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Loss samples retained for the mountain range.
pub const LOSS_HISTORY: usize = 512;

/// How long a checkpoint pulse stays lit, in poll ticks.
const CKPT_PULSE_TICKS: u8 = 40;

/// Re-scan for a training process at most this often when none is attached.
const RESCAN_EVERY: Duration = Duration::from_secs(2);

/// Everything the view draws.
#[derive(Debug, Clone, Default)]
pub struct TrainState {
    pub proc: Option<TrainProcess>,
    pub config: TrainConfig,
    pub log: Option<LogSource>,
    pub step: u64,
    pub max_steps: u64,
    pub loss: Option<f32>,
    pub prev_loss: Option<f32>,
    pub loss_history: Vec<f32>,
    pub step_ms: f32,
    pub cache_entries: u32,
    pub batch_size: u32,
    pub grad_accum: u32,
    pub scheduler: Option<String>,
    pub param_count: u64,
    pub checkpoint_step: u64,
    pub checkpoint_pulse: u8,
    pub first_seen: Option<Instant>,
}

impl TrainState {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when a run is attached and its per-step stream is readable.
    pub fn has_metrics(&self) -> bool {
        matches!(self.log, Some(LogSource::File(_))) && self.step > 0
    }

    pub fn apply_event(&mut self, ev: TrainEvent) {
        match ev {
            TrainEvent::Step { step, loss } => {
                if self.loss.is_some() {
                    self.prev_loss = self.loss;
                }
                self.step = step;
                self.loss = Some(loss);
                self.loss_history.push(loss);
                if self.loss_history.len() > LOSS_HISTORY {
                    self.loss_history.remove(0);
                }
            }
            TrainEvent::StepTime { ms, cache_entries } => {
                self.step_ms = ms;
                self.cache_entries = cache_entries;
            }
            TrainEvent::MaxSteps(v) => self.max_steps = v,
            TrainEvent::BatchSize(v) => self.batch_size = v,
            TrainEvent::GradAccum(v) => self.grad_accum = v,
            TrainEvent::Scheduler(s) => self.scheduler = Some(s),
            TrainEvent::ParamCount(v) => self.param_count = v,
        }
    }

    pub fn steps_per_sec(&self) -> f32 {
        if self.step_ms <= 0.0 {
            0.0
        } else {
            1000.0 / self.step_ms
        }
    }

    /// batch × seq_len × grad_accum × steps/sec. `None` until every factor is
    /// known — `max_sequence_length` comes only from the model YAML, so this
    /// stays `None` rather than inventing a number when the config is absent.
    pub fn tokens_per_sec(&self) -> Option<f32> {
        let seq = self.config.max_sequence_length? as f32;
        if self.batch_size == 0 || self.step_ms <= 0.0 {
            return None;
        }
        let accum = if self.grad_accum == 0 { 1 } else { self.grad_accum } as f32;
        Some(self.batch_size as f32 * seq * accum * self.steps_per_sec())
    }

    pub fn eta_secs(&self) -> Option<f32> {
        if self.max_steps == 0 || self.step_ms <= 0.0 || self.step >= self.max_steps {
            return None;
        }
        Some((self.max_steps - self.step) as f32 * (self.step_ms / 1000.0))
    }
}

/// Incremental line reader that only ever returns newly-appended lines.
pub struct Tailer {
    path: PathBuf,
    offset: u64,
}

impl Tailer {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0 }
    }

    /// Lines appended since the last call. A file that shrank (rotated or
    /// truncated) resets the offset so we resume from its new start instead
    /// of seeking past the end and reading nothing forever.
    pub fn read_new(&mut self) -> Vec<String> {
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut reader = BufReader::new(&mut f);
        let mut consumed = self.offset;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => {
                    // Only accept a complete line; a partial final write is
                    // left for the next poll rather than mis-parsed.
                    if line.ends_with('\n') {
                        consumed += n as u64;
                        out.push(line.trim_end().to_string());
                    } else {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        self.offset = consumed;
        out
    }
}

/// Pulses when a checkpoint file's mtime advances.
pub struct CheckpointWatch {
    path: PathBuf,
    last: Option<SystemTime>,
}

impl CheckpointWatch {
    pub fn new(path: PathBuf) -> Self {
        Self { path, last: None }
    }

    /// `true` exactly once per observed save. The first call only establishes
    /// a baseline — an already-existing checkpoint from a previous run must
    /// not announce itself as a fresh save.
    pub fn poll(&mut self) -> bool {
        let Ok(m) = std::fs::metadata(&self.path).and_then(|m| m.modified()) else {
            return false;
        };
        match self.last {
            None => {
                self.last = Some(m);
                false
            }
            Some(prev) => {
                if m > prev {
                    self.last = Some(m);
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// Owns discovery + polling for the Training view.
pub struct TrainMonitor {
    state: TrainState,
    tailer: Option<Tailer>,
    ckpt: Option<CheckpointWatch>,
    last_scan: Option<Instant>,
}

impl Default for TrainMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl TrainMonitor {
    pub fn new() -> Self {
        Self {
            state: TrainState::new(),
            tailer: None,
            ckpt: None,
            last_scan: None,
        }
    }

    pub fn state(&self) -> &TrainState {
        &self.state
    }

    /// Scan `/proc` for a tt-train process. Linux-only; other targets simply
    /// never find one (the whole subsystem is /proc-based).
    #[cfg(target_os = "linux")]
    fn scan_for_process() -> Option<TrainProcess> {
        let entries = std::fs::read_dir("/proc").ok()?;
        for e in entries.flatten() {
            let name = e.file_name();
            let pid: i32 = match name.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(b) => String::from_utf8_lossy(&b).replace('\0', " ").trim().to_string(),
                Err(_) => continue,
            };
            if cmdline.is_empty() {
                continue;
            }
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .unwrap_or_default()
                .trim()
                .to_string();
            if let Some(p) = parse_train_process(&comm, &cmdline, pid) {
                return Some(p);
            }
        }
        None
    }

    #[cfg(not(target_os = "linux"))]
    fn scan_for_process() -> Option<TrainProcess> {
        None
    }

    /// Load the run's YAML config, following `transformer_config` to the
    /// model topology. Silently leaves fields unset when unreadable.
    fn load_config(p: &TrainProcess) -> TrainConfig {
        let Some(cfg_path) = p.config_path.as_ref() else {
            return TrainConfig::default();
        };
        let Ok(text) = std::fs::read_to_string(cfg_path) else {
            return TrainConfig::default();
        };
        let mut cfg = parse_train_yaml(&text);
        if let Some(mp) = cfg.model_config_path.clone() {
            // The model path may be relative to the training config's dir.
            let direct = std::fs::read_to_string(&mp);
            let text2 = direct.or_else(|_| {
                let base = std::path::Path::new(cfg_path).parent().unwrap_or(std::path::Path::new("."));
                std::fs::read_to_string(base.join(&mp))
            });
            if let Ok(t) = text2 {
                merge_model_yaml(&mut cfg, &t);
            }
        }
        cfg
    }

    /// True while the attached process is still alive.
    fn still_alive(pid: i32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    /// One tick: attach if needed, drain new log lines, check the checkpoint.
    pub fn poll(&mut self) {
        // Detach if the run ended.
        if let Some(p) = self.state.proc.as_ref() {
            if !Self::still_alive(p.pid) {
                self.state = TrainState::new();
                self.tailer = None;
                self.ckpt = None;
            }
        }

        // Attach (rate-limited so a bare /proc walk isn't done every frame).
        if self.state.proc.is_none() {
            let due = self
                .last_scan
                .map(|t| t.elapsed() >= RESCAN_EVERY)
                .unwrap_or(true);
            if !due {
                return;
            }
            self.last_scan = Some(Instant::now());
            if let Some(p) = Self::scan_for_process() {
                let cfg = Self::load_config(&p);
                let log = discover_log(p.pid);
                if let LogSource::File(ref path) = log {
                    self.tailer = Some(Tailer::new(path.clone()));
                }
                if let Some(ref mp) = cfg.model_save_path {
                    self.ckpt = Some(CheckpointWatch::new(PathBuf::from(mp)));
                }
                self.state = TrainState::new();
                self.state.first_seen = Some(Instant::now());
                self.state.config = cfg;
                self.state.log = Some(log);
                self.state.proc = Some(p);
            }
            return;
        }

        // Drain newly-appended log lines.
        if let Some(t) = self.tailer.as_mut() {
            for line in t.read_new() {
                if let Some(ev) = parse_train_line(&line) {
                    self.state.apply_event(ev);
                }
            }
        }

        // Checkpoint pulse.
        if self.state.checkpoint_pulse > 0 {
            self.state.checkpoint_pulse -= 1;
        }
        if let Some(w) = self.ckpt.as_mut() {
            if w.poll() {
                self.state.checkpoint_pulse = CKPT_PULSE_TICKS;
                self.state.checkpoint_step = self.state.step;
            }
        }
    }
}
```

Add to `src/workload/train/mod.rs`:

```rust
pub mod monitor;

pub use monitor::{CheckpointWatch, Tailer, TrainMonitor, TrainState, LOSS_HISTORY};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui train::monitor`
Expected: PASS (7 tests)

- [ ] **Step 5: Commit**

```bash
git add src/workload/train/
git commit -m "feat(train): monitor with log tailing and checkpoint watching"
```

---

### Task 6: The nightscape (aurora + deterministic starfield)

**Files:**
- Create: `src/animation/train_sky.rs`
- Modify: `src/animation/mod.rs`

Split from the main renderer because it is self-contained, has its own
determinism property worth testing in isolation, and would otherwise push
`train_view.rs` past the size where it stays reviewable.

**Interfaces:**
- Produces: `pub struct SkyCell { pub ch: char, pub color: ratatui::style::Color }`, `pub fn star_hash(x: usize, y: usize) -> f32`, `pub fn sky_cell(x: usize, y: usize, rel_y: f32, frame: u64) -> Option<SkyCell>`

- [ ] **Step 1: Write the failing test**

Create `src/animation/train_sky.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_hash_is_deterministic_and_in_range() {
        // Stars must not jitter between frames: same coords → same value.
        for (x, y) in [(0usize, 0usize), (17, 3), (129, 40), (7, 11)] {
            let a = star_hash(x, y);
            let b = star_hash(x, y);
            assert_eq!(a, b, "hash must be stable for ({x},{y})");
            assert!((0.0..=1.0).contains(&a), "hash out of range: {a}");
        }
    }

    #[test]
    fn star_hash_varies_across_neighbours() {
        // A weak hash that returns near-identical values for adjacent cells
        // would produce visible banding instead of scattered stars.
        let a = star_hash(10, 10);
        let b = star_hash(11, 10);
        let c = star_hash(10, 11);
        assert!((a - b).abs() > 0.001, "x-neighbours too similar");
        assert!((a - c).abs() > 0.001, "y-neighbours too similar");
    }

    #[test]
    fn a_star_keeps_its_position_across_frames() {
        // Find a cell that is a star at frame 0, then confirm it is still a
        // star (maybe a different brightness) many frames later.
        let mut found = None;
        for x in 0..200usize {
            for y in 0..12usize {
                if sky_cell(x, y, 0.5, 0).is_some() && star_hash(x, y) > STAR_THRESHOLD {
                    found = Some((x, y));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (sx, sy) = found.expect("some cell in a 200x12 field must hold a star");
        for f in [1u64, 7, 60, 999] {
            assert!(
                sky_cell(sx, sy, 0.5, f).is_some(),
                "star at ({sx},{sy}) vanished at frame {f}"
            );
        }
    }

    #[test]
    fn sky_is_mostly_empty_so_the_mountains_stay_readable() {
        let mut filled = 0usize;
        let total = 128 * 11;
        for x in 0..128usize {
            for y in 0..11usize {
                if sky_cell(x, y, y as f32 / 11.0, 0).is_some() {
                    filled += 1;
                }
            }
        }
        let frac = filled as f32 / total as f32;
        assert!(
            frac < 0.5,
            "sky fill {frac:.2} is too dense — aurora/stars must stay subtle"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train_sky`
Expected: FAIL — `cannot find function star_hash`

- [ ] **Step 3: Write minimal implementation**

Prepend to `src/animation/train_sky.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Training view's nightscape: drifting aurora bands and a twinkling
//! starfield that fill the empty sky above the loss mountains.
//!
//! Because the mountains descend as the model converges, this negative space
//! grows over a run — the sky opening up *is* a progress signal.
//!
//! Star positions come from a spatial hash rather than a random generator so
//! they are stable frame to frame (a re-rolled star field would shimmer into
//! noise); only their brightness animates.

use crate::animation::common::hsv_to_rgb_bytes;
use ratatui::style::Color;

/// A star occupies a cell when its hash exceeds this. Tuned so the field
/// reads as scattered rather than dense — the mountains are the subject.
pub const STAR_THRESHOLD: f32 = 0.9715;

/// One rendered sky cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyCell {
    pub ch: char,
    pub color: Color,
}

/// Stable 2-D hash → `[0, 1)`. Same coords always give the same value.
pub fn star_hash(x: usize, y: usize) -> f32 {
    let mut h = (x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663);
    h ^= h >> 13;
    h = h.wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h as f32) / (u32::MAX as f32)
}

/// Aurora intensity and hue at a cell. `rel_y` is 0.0 at the top of the sky
/// band and 1.0 at the mountain line.
fn aurora(x: usize, rel_y: f32, frame: u64) -> (f32, f32) {
    let fx = x as f32;
    let ff = frame as f32;
    let mut best = 0.0f32;
    let mut hue = 0.0f32;
    for band in 0..2u32 {
        let speed = if band == 0 { 0.0085 } else { -0.0061 };
        let wob = (fx * 0.048 + ff * speed).sin() * 0.16
            + (fx * 0.019 - ff * speed * 1.7).sin() * 0.11;
        let centre = if band == 0 { 0.30 } else { 0.55 } + wob;
        let dist = (rel_y - centre).abs();
        if dist < 0.20 {
            let mut v = 1.0 - dist / 0.20;
            v *= 0.55 + 0.45 * (fx * 0.07 + ff * 0.02 + band as f32 * 2.0).sin();
            if v > best {
                best = v;
                hue = if band == 0 { 152.0 } else { 292.0 }
                    + (fx * 0.03 + ff * 0.006).sin() * 40.0;
            }
        }
    }
    (best, hue)
}

/// The character to draw at a sky cell, if any.
pub fn sky_cell(x: usize, y: usize, rel_y: f32, frame: u64) -> Option<SkyCell> {
    // Stars sit on top of the aurora.
    let sv = star_hash(x, y);
    if sv > STAR_THRESHOLD {
        let phase = star_hash(x + 31, y + 17) * std::f32::consts::TAU;
        let speed = 0.035 + star_hash(x + 7, y + 3) * 0.075;
        let tw = 0.5 + 0.5 * (frame as f32 * speed + phase).sin();
        // A minority of stars are vividly colored; the rest are cool white.
        let vivid = star_hash(x + 91, y + 53) > 0.82;
        let (h, s) = if vivid {
            (158.0 + star_hash(x, y + 11) * 190.0, 0.78)
        } else {
            (205.0, 0.22)
        };
        let (ch, v) = if tw > 0.88 {
            ('✦', 0.74)
        } else if tw > 0.66 {
            ('∙', 0.56)
        } else if tw > 0.36 {
            ('·', 0.38)
        } else {
            ('·', 0.22)
        };
        let (r, g, b) = hsv_to_rgb_bytes(h, s, v);
        return Some(SkyCell {
            ch,
            color: Color::Rgb(r, g, b),
        });
    }

    let (lit, hue) = aurora(x, rel_y, frame);
    if lit > 0.14 {
        let ch = if lit > 0.62 { '▒' } else { '░' };
        let (r, g, b) = hsv_to_rgb_bytes(hue, 0.62, 0.10 + lit * 0.15);
        return Some(SkyCell {
            ch,
            color: Color::Rgb(r, g, b),
        });
    }
    None
}
```

Note: `hsv_to_rgb_bytes(h, s, v)` already exists in `animation/common.rs` and
returns `(u8, u8, u8)` — use it directly rather than unwrapping the `Color`
that `hsv_to_rgb` returns.

Add to `src/animation/mod.rs` alongside the other `pub mod` lines:

```rust
pub mod train_sky;
```

and alongside the other `pub use` lines:

```rust
pub use train_sky::{sky_cell, star_hash, SkyCell, STAR_THRESHOLD};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui train_sky`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add src/animation/train_sky.rs src/animation/mod.rs
git commit -m "feat(train): aurora + deterministic twinkling starfield"
```

---

### Task 7: The Training renderer

**Files:**
- Create: `src/animation/train_view.rs`
- Modify: `src/animation/mod.rs`

**Interfaces:**
- Consumes: `TrainState` (Task 5), `sky_cell` (Task 6).
- Produces: `pub struct TrainView` with `TrainView::new(width: usize, height: usize)`, `pub fn update(&mut self)`, `pub fn render(&self, st: &TrainState, backend: &dyn TelemetryBackend) -> Vec<Line<'static>>`, `pub fn loss_hue(loss: f32) -> f32`

- [ ] **Step 1: Write the failing test**

Create `src/animation/train_view.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::MockBackend;
    use crate::backend::TelemetryBackend;
    use crate::workload::train::TrainState;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn loss_hue_runs_magenta_when_high_to_teal_when_converged() {
        let hot = loss_hue(4.5);
        let cool = loss_hue(0.35);
        assert!(hot > 300.0, "high loss should be magenta-ish, got {hot}");
        assert!(
            (150.0..180.0).contains(&cool),
            "converged loss should be teal-ish, got {cool}"
        );
        // Clamped outside the observed range rather than wrapping around.
        assert!((loss_hue(99.0) - loss_hue(4.6)).abs() < 1.0);
        assert!((loss_hue(-1.0) - loss_hue(0.30)).abs() < 1.0);
    }

    #[test]
    fn with_no_process_it_says_it_is_scanning_and_draws_no_metrics() {
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let v = TrainView::new(120, 40);
        let out = text_of(&v.render(&TrainState::new(), &b));
        assert!(out.contains("SCANNING"), "expected a scanning state:\n{out}");
        assert!(
            !out.contains("LOSS  "),
            "must not draw a loss panel with no data:\n{out}"
        );
    }

    #[test]
    fn an_unredirected_stdout_explains_itself_instead_of_faking_a_curve() {
        use crate::workload::train::{LogSource, TrainProcess};
        let mut b = MockBackend::new(1);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 4242,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::NotRedirected);

        let v = TrainView::new(120, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(out.contains("nano_gpt"), "should still name the run:\n{out}");
        assert!(
            out.to_lowercase().contains("redirect"),
            "must explain why per-step metrics are missing:\n{out}"
        );
    }

    #[test]
    fn a_live_run_draws_its_numbers() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 48213,
            binary: "nano_gpt".into(),
            config_path: Some("t.yaml".into()),
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        st.apply_event(TrainEvent::MaxSteps(50000));
        st.apply_event(TrainEvent::BatchSize(64));
        for i in 1..=60u64 {
            st.apply_event(TrainEvent::Step {
                step: i,
                loss: 4.5 - (i as f32) * 0.05,
            });
        }
        st.apply_event(TrainEvent::StepTime {
            ms: 1000.0,
            cache_entries: 21,
        });

        let v = TrainView::new(134, 40);
        let out = text_of(&v.render(&st, &b));
        assert!(out.contains("nano_gpt"));
        assert!(out.contains("48213"), "pid should be shown:\n{out}");
        assert!(out.contains("50,000"), "max steps should be shown:\n{out}");
        assert!(out.contains("LOSS"), "loss panel should be present:\n{out}");
    }

    #[test]
    fn never_emits_a_right_side_border_and_fits_the_width() {
        use crate::workload::train::{LogSource, TrainEvent, TrainProcess};
        let mut b = MockBackend::new(4);
        b.init().unwrap();
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 1,
            binary: "nano_gpt".into(),
            config_path: None,
        });
        st.log = Some(LogSource::File("/tmp/x.log".into()));
        for i in 1..=200u64 {
            st.apply_event(TrainEvent::Step { step: i, loss: 2.0 });
        }

        // Narrow terminals are where wrapping bugs show up.
        for w in [60usize, 80, 100, 134] {
            let v = TrainView::new(w, 30);
            for line in v.render(&st, &b) {
                let s: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
                assert!(
                    !s.contains('╗') && !s.contains('╝') && !s.contains('║').then(|| s.ends_with('║')).unwrap_or(false),
                    "right-side border characters are forbidden (w={w}): {s:?}"
                );
                let cols = unicode_width::UnicodeWidthStr::width(s.as_str());
                assert!(cols <= w, "line is {cols} cols at width {w}: {s:?}");
            }
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train_view`
Expected: FAIL — `cannot find type TrainView`

- [ ] **Step 3: Write minimal implementation**

Read `src/animation/defrag.rs`'s `render_inner` first — it is the closest
existing model for "build `Vec<Line>` over a full-screen character grid" and
this implementation should follow its shape (a cell buffer, then one `Line`
per row with adjacent same-style cells coalesced into a `Span`).

Prepend to `src/animation/train_view.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Training view — a live tt-train run drawn as a character-grid network
//! being fed tokens, with loss "mountains" against an aurora nightscape.
//!
//! ## The colour language
//!
//! Each channel carries a different real signal — nothing is decorative:
//!
//! | Channel | Encodes |
//! |---|---|
//! | node/mountain hue magenta→teal | loss magnitude |
//! | amber sweep left→right | forward pass |
//! | violet sweep right→left | backward pass / gradients |
//! | per-column river hue | the run's history (each column keeps its own loss's hue) |
//! | mint ▼ / coral ▲ | loss delta direction |
//! | cyan→green→amber→red | chip temperature (the app's existing ramp) |
//! | bar density █▓▒░· | chip power draw |
//! | violet shimmer → dim | kernel cache compiling → steady |
//! | mint burst + comet | checkpoint saved |
//!
//! Everything here is driven by what tt-train actually prints plus
//! tt-toplike's own chip telemetry; no metric is invented.

use crate::animation::common::hsv_to_rgb;
use crate::animation::train_sky::sky_cell;
use crate::backend::TelemetryBackend;
use crate::ui::colors;
use crate::workload::train::{LogSource, TrainState};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Loss → hue: 158° (teal, converged) … 325° (magenta, chaotic).
pub fn loss_hue(loss: f32) -> f32 {
    let t = ((loss - 0.30) / 4.3).clamp(0.0, 1.0);
    158.0 + t * 167.0
}

const FWD_HUE: f32 = 42.0;
const BWD_HUE: f32 = 268.0;

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    fg: Color,
    bold: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Rgb(22, 29, 38),
            bold: false,
        }
    }
}

pub struct TrainView {
    width: usize,
    height: usize,
    frame: u64,
}

impl TrainView {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width: width.max(20),
            height: height.max(10),
            frame: 0,
        }
    }

    pub fn update(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn render(&self, st: &TrainState, backend: &dyn TelemetryBackend) -> Vec<Line<'static>> {
        let mut buf = vec![vec![Cell::default(); self.width]; self.height];
        self.draw_frame(&mut buf);

        match st.proc.as_ref() {
            None => self.draw_scanning(&mut buf),
            Some(_) => {
                self.draw_header(&mut buf, st);
                match st.log.as_ref() {
                    Some(LogSource::File(_)) => {
                        self.draw_model_card(&mut buf, st);
                        self.draw_network(&mut buf, st);
                        self.draw_live_stats(&mut buf, st);
                        self.draw_river(&mut buf, st);
                    }
                    _ => self.draw_no_log_notice(&mut buf),
                }
                self.draw_chips(&mut buf, backend);
                self.draw_legend(&mut buf, st);
            }
        }
        self.to_lines(buf)
    }

    // ── the pieces (see defrag.rs for the same buffer→Line shape) ──
    // NOTE TO IMPLEMENTER: each `draw_*` writes into `buf`; keep every write
    // bounds-checked via `put`, and never emit ╗ or ╝ (left+bottom borders
    // only). The reference mockup for exact glyphs and layout is
    // docs/superpowers/specs/2026-08-29-training-view-design.md.

    fn put(&self, buf: &mut [Vec<Cell>], x: usize, y: usize, ch: char, fg: Color, bold: bool) {
        if y < self.height && x < self.width {
            buf[y][x] = Cell { ch, fg, bold };
        }
    }

    fn text(&self, buf: &mut [Vec<Cell>], x: usize, y: usize, s: &str, fg: Color, bold: bool) {
        for (i, ch) in s.chars().enumerate() {
            self.put(buf, x + i, y, ch, fg, bold);
        }
    }

    fn to_lines(&self, buf: Vec<Vec<Cell>>) -> Vec<Line<'static>> {
        buf.into_iter()
            .map(|row| {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut run = String::new();
                let mut cur: Option<(Color, bool)> = None;
                for c in row {
                    let key = (c.fg, c.bold);
                    if Some(key) != cur {
                        if !run.is_empty() {
                            let (fg, b) = cur.unwrap();
                            let mut st = Style::default().fg(fg);
                            if b {
                                st = st.add_modifier(Modifier::BOLD);
                            }
                            spans.push(Span::styled(std::mem::take(&mut run), st));
                        }
                        cur = Some(key);
                    }
                    run.push(c.ch);
                }
                if !run.is_empty() {
                    let (fg, b) = cur.unwrap_or((Color::Reset, false));
                    let mut st = Style::default().fg(fg);
                    if b {
                        st = st.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(run, st));
                }
                Line::from(spans)
            })
            .collect()
    }
}
```

Then implement each `draw_*` method against the layout in the spec:

- `draw_frame` — `║` down the left edge, `╚═══` along the bottom. **No `╗`/`╝`.**
- `draw_scanning` — centered "SCANNING FOR TRAINING" plus the discovery
  checklist (device-fd holders → tt-train binaries → `/proc/<pid>/fd/1` → log
  tail), with an idle nightscape behind it.
- `draw_header` — mode tag, binary name, pid, chip count; right side shows
  loss (hue-coloured), the `▼`/`▲` delta from `prev_loss`, and step/max_steps.
- `draw_no_log_notice` — names the run and states plainly that per-step
  metrics need the trainer's stdout redirected to a file, e.g.
  `stdout is not redirected to a file — relaunch with '> train.log' for per-step metrics`.
  **Draws no loss panel.**
- `draw_model_card` — params/blocks/heads/d_model/vocab/seq_len from
  `st.config` and `st.param_count`; omit any field that is `None`.
- `draw_network` — `st.config.num_blocks` columns × `num_heads` rows (default
  6×6 when unknown), `●◉○◇·` nodes, `─╱╲` synapses, an amber forward sweep and
  a violet backward sweep driven by `self.frame`.
- `draw_live_stats` — tok/s, step/s, step ms, cache (violet while
  `cache_entries` is still climbing, dim once steady), elapsed/ETA, checkpoint
  step + file, mint while `st.checkpoint_pulse > 0`.
- `draw_river` — measure the skyline from `st.loss_history` first, fill the
  space above it with `sky_cell(...)`, then draw `▁▂▃▄▅▆▇█` bars at 8×
  sub-cell resolution with **each column coloured by its own loss's hue**.
- `draw_chips` — one entry per `backend.devices()`: temp via
  `colors::temp_color`, power as a `█▓▒░·` bar.
- `draw_legend` — the nine channels as `glyph + label` pairs.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui train_view`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add src/animation/train_view.rs src/animation/mod.rs
git commit -m "feat(train): the Training view renderer"
```

---

### Task 8: Wire into the TUI (`t` key, mode, update/render loop)

**Files:**
- Modify: `src/ui/tui/mod.rs`

**Interfaces:**
- Consumes: `TrainView` (Task 7), `TrainMonitor` (Task 5).

- [ ] **Step 1: Write the failing test**

Add to the existing test module in `src/ui/tui/mod.rs`:

```rust
#[test]
fn t_key_enters_training_mode_and_esc_leaves_it() {
    // Mirrors the existing mode-transition tests: `t` is a dedicated entry
    // key like `~`/`i`, not part of the `v` cycle.
    let mut mode = DisplayMode::Insights;
    let prev = mode;
    mode = DisplayMode::Training;
    assert_eq!(mode, DisplayMode::Training);
    mode = prev;
    assert_eq!(mode, DisplayMode::Insights);
}

#[test]
fn training_is_excluded_from_the_v_cycle() {
    // Walk the whole `v` rotation; Training must never appear (it is
    // entered with `t` only, like InferenceMonitor and HivemindSweeper).
    let mut m = DisplayMode::Insights;
    for _ in 0..12 {
        m = next_display_mode(m);
        assert_ne!(m, DisplayMode::Training, "`v` must not cycle into Training");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui t_key_enters_training`
Expected: FAIL — `no variant named Training` / `cannot find function next_display_mode`

- [ ] **Step 3: Write minimal implementation**

1. Add the variant to `DisplayMode` (after `HivemindSweeper`):

```rust
    /// Training view — a live tt-train run. Entered via `t` only
    /// (deliberately excluded from the `v`-cycle rotation, like
    /// `InferenceMonitor` and `HivemindSweeper`); `t` toggles back out to
    /// `prev_mode`, as does Esc.
    Training,
```

2. Extract the existing `v`-cycle `match` into a testable free function so
   the exclusion is assertable:

```rust
/// The `v` rotation. `InferenceMonitor`, `HivemindSweeper` and `Training`
/// are entry-key-only modes and are never cycled into; a `v` pressed while
/// one of them is active leaves to `Insights`.
fn next_display_mode(m: DisplayMode) -> DisplayMode {
    match m {
        DisplayMode::Insights => DisplayMode::Grid,
        DisplayMode::Grid => DisplayMode::Starfield,
        DisplayMode::Starfield => DisplayMode::MemoryCastle,
        DisplayMode::MemoryCastle => DisplayMode::MemoryFlow,
        DisplayMode::MemoryFlow => DisplayMode::Arcade,
        DisplayMode::Arcade => DisplayMode::Defrag,
        DisplayMode::Defrag => DisplayMode::Insights,
        DisplayMode::InferenceMonitor
        | DisplayMode::HivemindSweeper
        | DisplayMode::Training => DisplayMode::Insights,
    }
}
```

   Replace the inline `v`-handler match with a call to it. Preserve the
   existing behaviour for every arm that already existed — read the current
   match and carry each mapping across unchanged.

3. Declare the state next to the other visualizations (near `let mut defrag:
   Option<DefragVis> = None;`):

```rust
    let mut train_view: Option<TrainView> = None;
    let mut train_monitor: Option<TrainMonitor> = None;
```

4. Update arm, alongside `DisplayMode::Defrag`:

```rust
                DisplayMode::Training => {
                    if train_view.is_none() {
                        train_view = Some(TrainView::new(size.width as usize, size.height as usize));
                    }
                    if train_monitor.is_none() {
                        train_monitor = Some(TrainMonitor::new());
                    }
                    if let Some(ref mut tm) = train_monitor {
                        tm.poll();
                    }
                    if let Some(ref mut tv) = train_view {
                        tv.update();
                    }
                }
```

5. Render arm, alongside the `DisplayMode::Defrag` render arm:

```rust
                        DisplayMode::Training => {
                            if let (Some(ref tv), Some(ref tm)) = (&train_view, &train_monitor) {
                                ui_train(f, tv, tm.state(), backend);
                            }
                        }
```

6. The render helper, next to `ui_defrag`:

```rust
/// Render the Training view — full-screen character grid, same shape as
/// `ui_defrag`.
fn ui_train(
    f: &mut Frame,
    tv: &TrainView,
    st: &crate::workload::train::TrainState,
    backend: &dyn TelemetryBackend,
) {
    let lines = tv.render(st, backend);
    let widget = Paragraph::new(lines).style(Style::default().bg(colors::rgb(0, 0, 0)));
    f.render_widget(widget, f.area());
}
```

7. The `t` key handler, following the `~` handler's shape (remember
   `prev_mode`, toggle back out). Add it **before** any unguarded global
   single-letter arm so it isn't shadowed:

```rust
                            KeyCode::Char('t') => {
                                if display_mode == DisplayMode::Training {
                                    display_mode = prev_mode;
                                } else {
                                    prev_mode = display_mode;
                                    display_mode = DisplayMode::Training;
                                }
                            }
```

8. Add the imports: `use crate::animation::TrainView;` and
   `use crate::workload::train::TrainMonitor;` (match the file's existing
   import style).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui`
Expected: PASS — all tests, including the two new ones.

Then confirm it builds and runs:

```bash
cargo build --release --features tui
```

- [ ] **Step 5: Commit**

```bash
git add src/ui/tui/mod.rs src/animation/mod.rs
git commit -m "feat(train): wire the Training view into the TUI on `t`"
```

---

### Task 9: Legend + explain overlays

**Files:**
- Modify: `src/ui/tui/mod.rs`

**Interfaces:**
- Produces: `fn train_legend_lines(bar: Color, bg: Color, dim: Color) -> Vec<Line<'static>>`

- [ ] **Step 1: Write the failing test**

Add to the existing test module in `src/ui/tui/mod.rs`:

```rust
#[test]
fn train_legend_documents_every_colour_channel() {
    let lines = train_legend_lines(
        colors::rgb(80, 80, 80),
        colors::rgb(0, 0, 0),
        colors::rgb(120, 120, 120),
    );
    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    for needle in [
        "loss", "forward", "gradient", "temp", "checkpoint", "aurora", "cache",
    ] {
        assert!(text.contains(needle), "legend is missing {needle:?}: {text}");
    }
}

#[test]
fn training_overlays_render_without_truncating() {
    // The overlay panel sizes to its widest line; this guards the new mode's
    // entries the same way the existing overlay test does.
    for panel in [OverlayPanel::Legend, OverlayPanel::Explain] {
        let lines = overlay_lines(panel, DisplayMode::Training);
        assert!(!lines.is_empty(), "{panel:?} must render for Training");
    }
}
```

If `overlay_lines(panel, mode)` is not already a standalone function in this
file, extract the existing `match` used by `render_overlay_panel` into one
with that signature first, leaving its behaviour for every existing
`(panel, mode)` pair unchanged.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib --features tui train_legend`
Expected: FAIL — `cannot find function train_legend_lines`

- [ ] **Step 3: Write minimal implementation**

Add the legend builder next to `defrag_legend_lines`:

```rust
/// Legend for the Training view: the nine colour channels, each of which
/// carries a different live signal.
fn train_legend_lines(
    bar: ratatui::style::Color,
    bg: ratatui::style::Color,
    dim: ratatui::style::Color,
) -> Vec<Line<'static>> {
    macro_rules! ln {
        ($spans:expr) => {{
            let mut v: Vec<Span<'static>> =
                vec![Span::styled("║ ", Style::default().fg(bar).bg(bg))];
            v.extend($spans);
            Line::from(v)
        }};
    }
    vec![
        ln!(vec![
            Span::styled("●", Style::default().fg(colors::rgb(214, 92, 208))),
            Span::styled(" → ", Style::default().fg(dim)),
            Span::styled("●", Style::default().fg(colors::rgb(79, 209, 197))),
            Span::styled("  loss: magenta chaos → teal converged", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("─", Style::default().fg(colors::rgb(242, 180, 62))),
            Span::styled(" forward pass (left→right)", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("∙", Style::default().fg(colors::rgb(150, 120, 240))),
            Span::styled(" gradients (right→left)", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("▁▄█", Style::default().fg(colors::rgb(180, 120, 200))),
            Span::styled(" loss river — each column keeps its own value's hue", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("▼", Style::default().fg(colors::rgb(124, 242, 156))),
            Span::styled(" improving   ", Style::default().fg(dim)),
            Span::styled("▲", Style::default().fg(colors::rgb(255, 138, 107))),
            Span::styled(" regressing", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("█", Style::default().fg(colors::rgb(246, 188, 66))),
            Span::styled(" chip temp + power draw", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("░▒", Style::default().fg(colors::rgb(96, 70, 130))),
            Span::styled(" aurora + stars — sky opens as loss falls", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("◆", Style::default().fg(colors::rgb(170, 120, 245))),
            Span::styled(" kernel cache compiling → steady", Style::default().fg(dim)),
        ]),
        ln!(vec![
            Span::styled("✦", Style::default().fg(colors::rgb(124, 242, 156))),
            Span::styled(" checkpoint saved", Style::default().fg(dim)),
        ]),
    ]
}
```

Add a `DisplayMode::Training` arm to the `OverlayPanel::Legend` match calling
`train_legend_lines(bar, bg, dim)`, and an `OverlayPanel::Explain` arm using
`explain_lines(...)` with this copy:

```
"Training — Robot Brain Food",
"",
"A live tt-train run, drawn as the model",
"itself: one column per transformer block,",
"one node per attention head.",
"",
"Each step feeds tokens in from the left",
"(amber), then gradients flow back out to",
"the right (violet). The loss mountains",
"below are coloured by their own value, so",
"the range is the whole run's history —",
"magenta chaos resolving to teal calm.",
"",
"The view attaches by itself: it finds a",
"process holding /dev/tenstorrent whose",
"binary is a tt-train example, then reads",
"/proc/<pid>/fd/1 to locate its log.",
"",
"tt-train prints step, loss, step time and",
"kernel-cache size; tokens/sec and ETA are",
"derived from those plus the run's YAML.",
"Nothing here is invented — gradient norms",
"and MFU aren't emitted live, so they",
"aren't shown.",
"",
"If stdout wasn't redirected to a file, the",
"per-step stream can't be read after the",
"fact — relaunch with '> train.log' and the",
"view picks it up automatically.",
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib --features tui`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/ui/tui/mod.rs
git commit -m "feat(train): legend and explain overlays for the Training view"
```

---

### Task 10: Docs, website copy, release

**Files:**
- Modify: `README.md`, `QUICK_START.md`, `site/index.html`, `AGENTS.md`,
  `CHANGELOG.md`, `debian/changelog`, `Cargo.toml` (via script)

- [ ] **Step 1: Bump the version**

```bash
./scripts/bump-version.sh 0.11.0
```

The script rewrites only the version *token* in `debian/changelog` — it does
not open a new stanza. Restore the previous stanza's header to its own
version, then prepend a complete new `0.11.0` stanza above it (newest first;
CI's version-consistency job reads the top stanza).

- [ ] **Step 2: Write the changelog entries**

`debian/changelog` (new top stanza) and a matching `CHANGELOG.md` section:
the Training view, its auto-attach mechanism, what tt-train emits, and the
honest caveat that per-step metrics require redirected stdout.

- [ ] **Step 3: Website + docs copy**

Add a feature entry to `site/index.html` matching the existing feature-block
markup, and a line to `README.md` + `QUICK_START.md`'s mode lists describing
`t`. Suggested site copy:

> **Training** — Watch a model learn. tt-toplike finds a running tt-train
> job on its own and draws it as the network it is: tokens streaming in,
> gradients washing back, and a range of loss mountains under an aurora that
> widens as the model converges.

- [ ] **Step 4: AGENTS.md phase entry**

Add a phase entry covering: what tt-train emits (with the verbatim line
shapes), the `/proc/<pid>/fd/1` auto-attach technique and its pipe/tty
limitation, the nine-channel colour language, and the hardware-verification
status.

- [ ] **Step 5: Verify and commit**

```bash
cargo test --lib --features tui
cargo fmt --check
cargo clippy --lib --bin tt-toplike-tui --features tui -- -D warnings
git add -u
git commit -m "release: bump to v0.11.0 — Training view"
```

---

## Manual verification (hardware)

1. Launch a real run with stdout redirected:
   `./build/sources/examples/nano_gpt/nano_gpt -c configs/training_configs/training_shakespeare_nanollama3.yaml > run/shakespeare.log 2>&1 &`
2. Start `tt-toplike-tui`, press `t`. It should attach with no further input
   and begin drawing within ~2 s.
3. Confirm: loss mountains advance and cool from magenta toward teal; the
   forward/backward sweeps fire once per step; the cache counter reads
   "compiling" early and "steady" later; a checkpoint save (every
   `model_save_interval` steps) pulses mint and releases a comet.
4. Re-launch **without** redirecting stdout and confirm the view names the run
   and explains the redirect requirement rather than drawing an empty curve.
5. Kill the trainer; the view should return to scanning.
6. Record the outcome in `AGENTS.md`, matching how prior telemetry work
   records its verification status.
