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
            // Current tt-train packs all four fields onto one line; expand it
            // so both halves take exactly the paths the split shape does.
            TrainEvent::StepAndTime {
                step,
                loss,
                ms,
                cache_entries,
            } => {
                self.apply_event(TrainEvent::Step { step, loss });
                self.apply_event(TrainEvent::StepTime { ms, cache_entries });
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
        let accum = if self.grad_accum == 0 {
            1
        } else {
            self.grad_accum
        } as f32;
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
    /// Whether the checkpoint already existed when we attached. A file that
    /// did NOT exist yet has its first appearance treated as a real save —
    /// otherwise the first checkpoint of a fresh run is swallowed as the
    /// baseline. A file that DID exist is a leftover from a previous run and
    /// must not announce itself as fresh, which is what the baseline is for.
    existed_at_start: bool,
}

impl CheckpointWatch {
    pub fn new(path: PathBuf) -> Self {
        let existed_at_start = std::fs::metadata(&path).is_ok();
        Self {
            path,
            last: None,
            existed_at_start,
        }
    }

    /// `true` exactly once per observed save. If the checkpoint already
    /// existed when we attached, the first call only establishes a baseline
    /// — a leftover from a previous run must not announce itself as fresh.
    /// If it did NOT exist yet, its first successful read means the file was
    /// just created, which is itself a genuine save and pulses immediately.
    pub fn poll(&mut self) -> bool {
        let Ok(m) = std::fs::metadata(&self.path).and_then(|m| m.modified()) else {
            return false;
        };
        match self.last {
            None => {
                self.last = Some(m);
                !self.existed_at_start
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
                Ok(b) => String::from_utf8_lossy(&b)
                    .replace('\0', " ")
                    .trim()
                    .to_string(),
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
    /// Resolve a possibly-relative path the way the *training process* would.
    ///
    /// tt-train is normally launched from inside its own run directory
    /// (`./nano_gpt -c train.yaml`), so its config path is relative to that
    /// cwd — not to wherever the user happened to start tt-toplike. Reading
    /// it directly therefore fails and the whole model card silently goes
    /// unknown. `/proc/<pid>/cwd` is a symlink to the process's working
    /// directory, which lets us resolve it exactly as the trainer would.
    ///
    /// Absolute paths pass through untouched. A pid that has exited (no
    /// `/proc/<pid>/cwd`) yields the original relative path, which then
    /// simply fails to open — the caller degrades to an empty config, never
    /// an error.
    #[cfg(target_os = "linux")]
    fn resolve_for_pid(pid: i32, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            return p;
        }
        match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(cwd) => cwd.join(p),
            Err(_) => p,
        }
    }

    /// Non-Linux: no `/proc`, so a relative path can only be taken as-is.
    #[cfg(not(target_os = "linux"))]
    fn resolve_for_pid(_pid: i32, path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    /// Build the checkpoint watcher for a detected run.
    ///
    /// `model_path` in tt-train's own sample configs is a bare filename
    /// (`transformer.msgpack`), and the trainer writes it through a plain
    /// relative `fopen` — so it lands in *the trainer's* working directory,
    /// which has nothing to do with ours. Resolving it against our own cwd
    /// yields a path that simply never exists, and since `poll()` reports
    /// `false` whenever the file can't be stat'd, the checkpoint pulse would
    /// stay silent for the entire run instead of failing loudly.
    fn checkpoint_watch_for(p: &TrainProcess, cfg: &TrainConfig) -> Option<CheckpointWatch> {
        let mp = cfg.model_save_path.as_ref()?;
        Some(CheckpointWatch::new(Self::resolve_for_pid(p.pid, mp)))
    }

    fn load_config(p: &TrainProcess) -> TrainConfig {
        let Some(cfg_path) = p.config_path.as_ref() else {
            return TrainConfig::default();
        };
        // Resolve against the trainer's cwd, since `-c train.yaml` is the
        // usual invocation and our cwd is unrelated to the run's.
        let cfg_path = Self::resolve_for_pid(p.pid, cfg_path);
        let Ok(text) = std::fs::read_to_string(&cfg_path) else {
            return TrainConfig::default();
        };
        let mut cfg = parse_train_yaml(&text);
        if let Some(mp) = cfg.model_config_path.clone() {
            // `transformer_config` is itself usually relative — try it
            // beside the training config first (the shape tt-train's own
            // sample configs use), then as the trainer's cwd would see it.
            let base = cfg_path
                .parent()
                .map(|b| b.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            let text2 = std::fs::read_to_string(base.join(&mp))
                .or_else(|_| std::fs::read_to_string(Self::resolve_for_pid(p.pid, &mp)));
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
                self.ckpt = Self::checkpoint_watch_for(&p, &cfg);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A relative `-c config.yaml` is the common way tt-train is launched
    /// (from inside the run's own directory), but tt-toplike's cwd is
    /// wherever the *user* started it — so resolving the path directly
    /// silently fails and the whole model card goes unknown. It has to be
    /// resolved against the training process's cwd, which `/proc/<pid>/cwd`
    /// exposes. Uses a real spawned process because that symlink is the
    /// mechanism under test; a mock would prove nothing.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_relative_config_path_resolves_against_the_process_cwd() {
        let dir = std::env::temp_dir().join(format!("ttrelcfg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("train.yaml"),
            "training_config:\n  model_path: \"transformer.msgpack\"\n  transformer_config: \"model.yaml\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("model.yaml"),
            "transformer_config:\n  num_blocks: 12\n  num_heads: 8\n  embedding_dim: 512\n",
        )
        .unwrap();

        // A real process whose cwd is `dir`, exactly like a trainer launched
        // from its own run directory.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .current_dir(&dir)
            .spawn()
            .expect("spawn test child");
        let pid = child.id() as i32;

        let p = TrainProcess {
            pid,
            binary: "nano_gpt".into(),
            // Relative, as the real CLI is almost always invoked.
            config_path: Some("train.yaml".into()),
        };
        let cfg = TrainMonitor::load_config(&p);

        child.kill().ok();
        child.wait().ok();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            cfg.num_blocks,
            Some(12),
            "a relative config path must resolve against the trainer's cwd"
        );
        assert_eq!(cfg.num_heads, Some(8));
        assert_eq!(
            cfg.model_save_path.as_deref(),
            Some("transformer.msgpack"),
            "the training yaml itself must have been read"
        );
    }

    /// The checkpoint watcher must observe the file the *trainer* writes.
    ///
    /// `model_path` is a bare filename in tt-train's own configs, so building
    /// the watcher from it verbatim points at our cwd, where nothing exists —
    /// and a watcher on a nonexistent path reports "no save" forever, so the
    /// checkpoint pulse never fires for the whole run.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_checkpoint_watcher_follows_the_trainers_cwd_not_ours() {
        // Distinct from the other checkpoint tests' directories: they all run
        // in one process, so a shared `ttckpt_<pid>` name would have each
        // test's cleanup deleting a sibling's files mid-run.
        let dir = std::env::temp_dir().join(format!("ttckptcwd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ckpt = dir.join("transformer.msgpack");
        std::fs::write(&ckpt, b"v1").unwrap();

        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .current_dir(&dir)
            .spawn()
            .expect("spawn test child");
        let pid = child.id() as i32;

        let p = TrainProcess {
            pid,
            binary: "nano_gpt".into(),
            config_path: Some("train.yaml".into()),
        };
        let cfg = TrainConfig {
            // Relative, exactly as tt-train's sample configs ship it.
            model_save_path: Some("transformer.msgpack".into()),
            ..TrainConfig::default()
        };

        let mut w = TrainMonitor::checkpoint_watch_for(&p, &cfg)
            .expect("a config naming model_path must yield a watcher");

        // The checkpoint already existed at attach, so the first poll only
        // establishes the baseline.
        let baseline = w.poll();
        // A later save bumps the mtime; SystemTime comparison needs the write
        // to land strictly after the baseline reading.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&ckpt, b"v2").unwrap();
        let after_save = w.poll();

        child.kill().ok();
        child.wait().ok();
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            !baseline,
            "a checkpoint left over from a previous run must not pulse on attach"
        );
        assert!(
            after_save,
            "a save in the trainer's own cwd must pulse — a watcher built \
             from the raw relative path watches our cwd and stays silent forever"
        );
    }

    /// An absolute path must keep working untouched, and a dead pid (no
    /// `/proc/<pid>/cwd` to resolve against) must degrade to an empty config
    /// rather than panicking.
    #[test]
    #[cfg(target_os = "linux")]
    fn absolute_config_paths_still_work_and_a_dead_pid_degrades() {
        let dir = std::env::temp_dir().join(format!("ttabscfg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("train.yaml");
        std::fs::write(
            &cfg_path,
            "training_config:\n  model_path: \"ckpt.msgpack\"\n  transformer_config: \"model.yaml\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("model.yaml"),
            "transformer_config:\n  num_blocks: 4\n  num_heads: 2\n",
        )
        .unwrap();

        // Absolute path + a pid that does not exist: resolution must fall
        // through to the literal path and still succeed.
        let p = TrainProcess {
            pid: 999_999_998,
            binary: "nano_gpt".into(),
            config_path: Some(cfg_path.to_string_lossy().into_owned()),
        };
        let cfg = TrainMonitor::load_config(&p);
        assert_eq!(cfg.num_blocks, Some(4), "absolute paths must be unaffected");
        assert_eq!(cfg.num_heads, Some(2));

        // Relative path + dead pid: nothing to resolve against, so empty.
        let p2 = TrainProcess {
            pid: 999_999_998,
            binary: "nano_gpt".into(),
            config_path: Some("train.yaml".into()),
        };
        let cfg2 = TrainMonitor::load_config(&p2);
        assert_eq!(cfg2.num_blocks, None);
        assert_eq!(cfg2.model_save_path, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn events_fold_into_state() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::MaxSteps(50000));
        st.apply_event(TrainEvent::BatchSize(64));
        st.apply_event(TrainEvent::GradAccum(4));
        st.apply_event(TrainEvent::ParamCount(11_200_000));
        st.apply_event(TrainEvent::Scheduler("cosine".into()));
        st.apply_event(TrainEvent::Step { step: 1, loss: 4.5 });
        st.apply_event(TrainEvent::StepTime {
            ms: 1000.0,
            cache_entries: 7,
        });

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
            st.apply_event(TrainEvent::Step {
                step: i as u64,
                loss: 1.0,
            });
        }
        assert_eq!(st.loss_history.len(), LOSS_HISTORY);
    }

    #[test]
    fn derived_rates_need_real_inputs_and_never_divide_by_zero() {
        let mut st = TrainState::new();
        assert_eq!(st.steps_per_sec(), 0.0, "no step time yet");
        assert_eq!(st.tokens_per_sec(), None, "no batch/seq_len yet");
        assert_eq!(st.eta_secs(), None, "no max_steps yet");

        st.apply_event(TrainEvent::StepTime {
            ms: 1000.0,
            cache_entries: 1,
        });
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

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
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

    #[test]
    fn checkpoint_created_after_attach_pulses_on_its_first_appearance() {
        let dir = std::env::temp_dir().join(format!("ttckpt_new_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.msgpack");
        // Deliberately do NOT create the file before attaching — this is the
        // fresh-run case where the checkpoint doesn't exist yet.
        assert!(!path.exists());

        let mut w = CheckpointWatch::new(path.clone());
        assert!(!w.poll(), "no file yet — nothing to pulse");

        std::fs::write(&path, b"a").unwrap();
        assert!(
            w.poll(),
            "the checkpoint's first appearance after attach is a genuine save, not a baseline"
        );
        assert!(!w.poll(), "and only once");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, b"bb").unwrap();
        assert!(w.poll(), "a later real mtime bump still pulses");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn checkpoint_that_never_appears_never_pulses() {
        let dir = std::env::temp_dir().join(format!("ttckpt_absent_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("never.msgpack");
        assert!(!path.exists());

        let mut w = CheckpointWatch::new(path.clone());
        for _ in 0..5 {
            assert!(!w.poll(), "an absent checkpoint never pulses, never panics");
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
