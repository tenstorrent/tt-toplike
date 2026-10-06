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
use super::detect::{parse_python_trainer, parse_train_process, TrainProcess};
use super::logsrc::{discover_log, LogSource};
use super::parse::{parse_train_line, TrainEvent};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Loss samples retained for the mountain range.
pub const LOSS_HISTORY: usize = 512;

/// Per-step samples retained for the Training view's step history. The
/// starfield puts two steps in each terminal column, so 160 fills a band
/// about 80 columns wide. The chip, PCIe and host-CPU rings in `ChipHistory`
/// keep the same number so the weave lines up with the stars.
pub const STEP_HISTORY: usize = 160;

/// Where the step history's times came from. The chart title discloses
/// `Observed`, so a time measured by polling a progress bar is never taken
/// for one the trainer printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepTimeSource {
    /// No per-step times yet.
    #[default]
    Unknown,
    /// The trainer printed the time (`Time: N ms`).
    Reported,
    /// Timed from the gap between progress-bar updates, to within one poll
    /// interval.
    Observed,
}

/// One step's wall time, either as the trainer reported it or as observed from
/// a progress bar (see `StepTimeSource`).
///
/// A sample is only recorded when it is a single step's own time. An
/// observed time needs exactly one step between two polls
/// (`note_step_progress`): a poll that read several steps measures an average,
/// and storing that as a per-step bar would invent resolution. A trainer
/// faster than one step per poll therefore has no history, and the view says
/// so. Observed samples carry `cache_delta: 0`, which means unknown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepSample {
    /// The trainer's own step number. Informational only: a progress-bar
    /// harness restarts its bar every chunk, so this can go backwards.
    pub step: u64,
    /// The run's own sequence number for this sample. It only rises within a
    /// run, whatever the trainer's step counter does (a progress-bar harness
    /// restarts its bar every chunk), so it is the key chip samples are joined
    /// on. `step` stays the trainer's own number.
    pub seq: u64,
    /// Wall time for this step in milliseconds, reported or observed.
    pub ms: f32,
    /// How much the program cache grew on this step. Growth means kernels were
    /// compiled, which is the usual reason one step is far slower than the rest.
    /// 0 also stands for unknown (observed samples never see the cache).
    pub cache_delta: u32,
    /// A checkpoint was written while this was the latest step.
    pub checkpoint: bool,
}

/// How long a checkpoint pulse stays lit, in poll ticks.
const CKPT_PULSE_TICKS: u8 = 40;

/// Re-scan for a training process at most this often when none is attached.
const RESCAN_EVERY: Duration = Duration::from_secs(2);

/// If more than this passes between two calls of `note_step_progress`, the
/// monitor was not being polled (the Training view was off screen) and the
/// call is treated as a fresh baseline. While the view is up the monitor is
/// polled every frame, tens of milliseconds apart, so 5 s is far above normal.
const POLL_GAP_REBASE: Duration = Duration::from_secs(5);

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
    /// The last [`STEP_HISTORY`] per-step times (reported or observed, see
    /// `step_time_source`), oldest first. Empty when neither a printed time nor
    /// a one-step-per-poll bar is available.
    pub step_history: Vec<StepSample>,
    /// Samples recorded this run. Rises by one per new sample, so it never goes
    /// backwards when a progress bar restarts. Chip lanes are keyed on it.
    pub step_seq: u64,
    /// Where `step_history`'s times came from.
    pub step_time_source: StepTimeSource,
    /// The progress bar restarted (its step went down), so the bar counts a
    /// chunk and its total is a chunk size. The run's step budget is a
    /// different, larger number. Set by `apply_event` when a bar step is lower
    /// than the previous bar step, and also by the monitor's cadence
    /// derivation when the parsed step goes down.
    pub chunked_bar: bool,
    /// The step shown by the last progress-bar update, `None` before the
    /// first one. `apply_event` compares each bar step with it to detect a
    /// restart on its own, so a trainer that also prints its own step times
    /// (which turns the cadence derivation off) still gets `chunked_bar`.
    pub last_bar_step: Option<u64>,
    /// The step budget a `MaxSteps` or `HarnessSummary` line stated, 0 when
    /// none has. Kept apart from `max_steps` so a chunked bar's total cannot
    /// replace it.
    pub stated_max_steps: u64,
    /// The absolute step this process started from, `Some` once a resume line
    /// has been read. `None` for a run that began at step 0 or never said.
    pub resume_start: Option<u64>,
    /// The run's absolute step at the start of the current bar chunk. `None`
    /// when the log has given no absolute position.
    ///
    /// A bar that counts the current chunk (tt-tnt's restarts at 1 every
    /// chunk) shows only the chunk-local step, so the run's step is
    /// `abs_base + local`. Set to 0 by a run header (a fresh run starts at
    /// step 0), to the start step by a resume line, and to the printed step
    /// by any absolute step line (a `step=` validation line or a `Step:`
    /// line), which is authoritative. A bar restart with no absolute line
    /// before it adds the finished chunk's length. With no base the bar's
    /// step is used as it is, as it always was.
    pub abs_base: Option<u64>,
    /// An absolute line set `abs_base` after the last bar frame, so the next
    /// bar restart starts on top of it and adds nothing.
    base_from_line: bool,
    /// The bar's step jumped a chunk boundary (an absolute line arrived while
    /// a bar is the step source, or the bar restarted on a base). The gap
    /// since the previous frame spans checkpoint, validation and the next
    /// chunk's warm-up, so the monitor re-baselines its cadence anchor on its
    /// next check and times nothing across it. Consumed by
    /// `TrainMonitor::note_step_progress`.
    cadence_rebase: bool,
    /// The total shown by the last progress-bar update, 0 before the first.
    pub last_bar_total: u64,
    /// A line that carries both a step and a step time (`StepAndMs`,
    /// `StepAndTime`) has been read. Its step is the trainer's own global
    /// counter, so a bar no longer sets the step or adds a loss entry. A
    /// time-only line (`StepTime`) does not set this: it has no step, so the
    /// bar stays the step source.
    pub step_from_reported_line: bool,
    pub step_ms: f32,
    pub cache_entries: u32,
    pub batch_size: u32,
    pub grad_accum: u32,
    pub scheduler: Option<String>,
    pub param_count: u64,
    pub checkpoint_step: u64,
    pub checkpoint_pulse: u8,
    pub first_seen: Option<Instant>,
    /// The trainer process's own host-side cost: CPU percent (100 = one
    /// core saturated) and resident set size.
    ///
    /// Training is not only a device workload — tokenisation, the data
    /// pipeline, kernel compilation and any CPU-resident model all burn host
    /// cycles, and a run can be entirely CPU-bound with the accelerators
    /// idle. Without this the view showed cold chips and offered no clue
    /// that the machine was flat out.
    pub host_cpu_pct: Option<f32>,
    pub host_rss_bytes: Option<u64>,
    /// Whether tt-train's own extension module is mapped into the process,
    /// i.e. this run is device-backed rather than pure host compute.
    pub device_backed: bool,
    /// This run is fabricated by `--mock`, not read from a real trainer.
    /// Surfaced in the header: this tool's premise is that every pixel maps
    /// to a real signal, so synthetic data has to say so.
    pub is_mock: bool,
}

impl TrainState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a step timed by watching a progress bar. `ms` is the gap between
    /// the two polls that saw `step - 1` and `step`.
    pub fn record_observed_step(&mut self, step: u64, ms: f32) {
        self.step_seq += 1;
        self.step_time_source = StepTimeSource::Observed;
        self.step_history.push(StepSample {
            step,
            seq: self.step_seq,
            ms,
            cache_delta: 0,
            checkpoint: false,
        });
        if self.step_history.len() > STEP_HISTORY {
            self.step_history.remove(0);
        }
    }

    /// Record a trainer-reported step time against the current step. With a
    /// cache count, growth is measured against the previous count; without
    /// one, growth is unknown (`cache_delta` 0) and `cache_entries` keeps
    /// its value.
    fn note_reported_time(&mut self, ms: f32, cache_entries: Option<u32>) {
        // Growth is measured against the previous sample, so the first
        // sample has no baseline. Without this guard, attaching to a
        // run whose cache is already full would paint its first bar
        // as a compile.
        let cache_delta = if self.step_history.is_empty() {
            0
        } else {
            match cache_entries {
                Some(n) => n.saturating_sub(self.cache_entries),
                // No count reported: growth is unknown, never a compile.
                None => 0,
            }
        };
        self.step_ms = ms;
        if let Some(n) = cache_entries {
            self.cache_entries = n;
        }
        self.step_time_source = StepTimeSource::Reported;
        let sample = StepSample {
            step: self.step,
            // Used only when this is a new sample; the replace branch
            // below keeps the first line's seq.
            seq: self.step_seq + 1,
            ms,
            cache_delta,
            checkpoint: false,
        };
        match self.step_history.last_mut() {
            // A second time line for the same step replaces the first
            // and keeps what the first already established.
            Some(last) if last.step == sample.step => {
                *last = StepSample {
                    cache_delta: last.cache_delta.max(sample.cache_delta),
                    checkpoint: last.checkpoint,
                    seq: last.seq,
                    ..sample
                };
            }
            _ => {
                self.step_seq += 1;
                self.step_history.push(sample);
                if self.step_history.len() > STEP_HISTORY {
                    self.step_history.remove(0);
                }
            }
        }
    }

    /// True when the state already holds data from a run, so a new run header
    /// means a second run was appended to the same log. Any of a step, a loss
    /// sample, a step sample or a stated budget qualifies. The first header of
    /// a fresh state has none of them, so it resets nothing.
    fn holds_run_data(&self) -> bool {
        self.step > 0
            || !self.loss_history.is_empty()
            || !self.step_history.is_empty()
            || self.stated_max_steps > 0
    }

    /// Clear the per-run data when a new run header follows an earlier run in
    /// the same log (a resumed run appends to the file the first run wrote).
    ///
    /// Cleared: step, losses, step history and its source, bar tracking, the
    /// budget, checkpoint markers, scheduler and grad accumulation, all of
    /// which belong to the old run. Kept: `proc`, `log` and `first_seen`
    /// (the attachment is the same), `config` (the model and YAML do not change
    /// with a restart), `is_mock`, and the host cost fields, which the monitor
    /// samples from the live process on every poll. `batch_size` and
    /// `param_count` are kept too: the header restates the batch size, and the
    /// parameter count is a property of the model.
    pub fn begin_new_run(&mut self) {
        self.step = 0;
        self.loss = None;
        self.prev_loss = None;
        self.loss_history.clear();
        self.step_history.clear();
        self.step_seq = 0;
        self.step_ms = 0.0;
        self.cache_entries = 0;
        self.max_steps = 0;
        self.stated_max_steps = 0;
        self.last_bar_step = None;
        self.last_bar_total = 0;
        self.chunked_bar = false;
        self.step_from_reported_line = false;
        self.step_time_source = StepTimeSource::Unknown;
        self.checkpoint_step = 0;
        self.checkpoint_pulse = 0;
        self.resume_start = None;
        self.abs_base = None;
        self.base_from_line = false;
        self.cadence_rebase = false;
        self.scheduler = None;
        self.grad_accum = 0;
    }

    /// Set the step and fold the loss into the history. Shared by every event
    /// that carries a step; touches neither `abs_base` nor the bar tracking.
    fn apply_step_and_loss(&mut self, step: u64, loss: f32) {
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

    /// An absolute step printed by the trainer itself (a `step=` validation
    /// line, or a `Step:` line with or without a time). It is the run's real
    /// position, so it becomes the base the next bar chunk counts from. When
    /// a bar is the step source this line marks a chunk boundary, and the
    /// monitor re-baselines its cadence so the boundary gap is never timed.
    /// A trainer with no bar keeps its cadence: a per-step `Step:` trainer
    /// would otherwise never be timed, and a validation-only log would lose
    /// its chunk-to-chunk rate.
    fn note_absolute_step(&mut self, step: u64) {
        self.abs_base = Some(step);
        self.base_from_line = true;
        if self.last_bar_step.is_some() {
            self.cadence_rebase = true;
        }
    }

    pub fn apply_event(&mut self, ev: TrainEvent) {
        match ev {
            // Only the parser produces this event (the bar arm below calls
            // `apply_step_and_loss` directly), so its step is always one the
            // trainer printed as absolute.
            TrainEvent::Step { step, loss } => {
                self.note_absolute_step(step);
                self.apply_step_and_loss(step, loss);
            }
            TrainEvent::StepTime { ms, cache_entries } => {
                self.note_reported_time(ms, Some(cache_entries));
            }
            // A time with no cache count: record the step, then the time with
            // growth unknown and the cache count left as it was.
            TrainEvent::StepAndMs { step, loss, ms } => {
                self.step_from_reported_line = true;
                self.apply_event(TrainEvent::Step { step, loss });
                self.note_reported_time(ms, None);
            }
            // Current tt-train packs all four fields onto one line; expand it
            // so both halves take exactly the paths the split shape does.
            TrainEvent::StepAndTime {
                step,
                loss,
                ms,
                cache_entries,
            } => {
                self.step_from_reported_line = true;
                self.apply_event(TrainEvent::Step { step, loss });
                self.apply_event(TrainEvent::StepTime { ms, cache_entries });
            }
            // The run's own sequence length, which may be narrower than the
            // model YAML's declared maximum. Whichever the log states last
            // wins, and the harness prints its summary after naming the
            // YAML, so a narrowed run is not overstated.
            TrainEvent::SeqLen(v) => {
                if v > 0 {
                    self.config.max_sequence_length = Some(v);
                }
            }
            // Resolved by the monitor, which has the pid needed to find the
            // file; nothing to record in the state itself.
            TrainEvent::ModelConfigFile(_) => {}
            // A bar carries the step, its total and the loss together.
            //
            // Rules that keep a bar from contradicting better sources:
            // * A bar step lower than the previous bar step is a restart for a
            //   new chunk, so `chunked_bar` is set here. The monitor's cadence
            //   derivation also detects it, but that derivation stops once the
            //   trainer prints its own step times.
            // * Once a line carrying both a step and a time has been read
            //   (`step_from_reported_line`), that line is the step counter and
            //   the loss source. The bar may count steps within a chunk, so
            //   letting it set `step` would make the step jump between the
            //   global and the chunk-local number, and its loss would add a
            //   second entry per step to `loss_history`. The bar's total is
            //   then used as the budget only when no budget was stated, the
            //   bar has not restarted, and the total is at least the current
            //   step. Otherwise the stated budget is kept, or the budget is 0
            //   (unknown) when none was stated. A bar update read before the
            //   first such line still sets the step and adds one loss entry;
            //   the line then sets the step again.
            // * Otherwise the bar is the step and loss source, as it always
            //   was for bar-only trainers. A time-only `StepTime` line does
            //   not change this, because it carries no step. The bar's total
            //   is the budget unless the step is chunk-local (see
            //   `step_is_chunk_local`) and a budget was stated, in which case
            //   the stated budget is kept. The view and `eta_secs` show no
            //   budget for a chunk-local step either way.
            // * With a base (`abs_base`), the bar's step is chunk-local and
            //   the run's step is `base + local`, so it is global and never
            //   goes backwards at a restart. The stated budget (the header's
            //   `steps=` for a fresh run, the resume line's end for a resumed
            //   one) is kept; the bar's total is a chunk size. With no stated
            //   budget the bar's total is used only while the bar has not
            //   restarted and the total covers the step, as for reported
            //   steps above.
            TrainEvent::BarProgress {
                step,
                max_steps,
                loss,
            } => {
                let restarted = self.last_bar_step.is_some_and(|prev| step < prev);
                if restarted {
                    self.chunked_bar = true;
                    // A restart with no absolute line since the last frame
                    // (a checkpoint-only boundary prints none): the finished
                    // chunk's length moves the base on, so the step stays
                    // global. The boundary gap is not a step either.
                    if !self.base_from_line {
                        if let (Some(base), Some(prev)) = (self.abs_base, self.last_bar_step) {
                            self.abs_base = Some(base + prev);
                            self.cadence_rebase = true;
                        }
                    }
                }
                self.base_from_line = false;
                self.last_bar_step = Some(step);
                self.last_bar_total = max_steps;
                if self.step_from_reported_line {
                    self.max_steps = if self.stated_max_steps > 0 {
                        self.stated_max_steps
                    } else if !self.chunked_bar && max_steps >= self.step {
                        max_steps
                    } else {
                        0
                    };
                } else if let Some(base) = self.abs_base {
                    let global = base + step;
                    self.max_steps = if self.stated_max_steps > 0 {
                        self.stated_max_steps
                    } else if !self.chunked_bar && max_steps >= global {
                        max_steps
                    } else {
                        0
                    };
                    self.apply_step_and_loss(global, loss);
                } else {
                    self.max_steps = if self.stated_max_steps > 0 && self.step_is_chunk_local() {
                        self.stated_max_steps
                    } else {
                        max_steps
                    };
                    self.apply_step_and_loss(step, loss);
                }
            }
            TrainEvent::HarnessSummary {
                max_steps,
                batch_size,
                seq_len,
            } => {
                if self.holds_run_data() {
                    self.begin_new_run();
                }
                // A fresh run starts at step 0. A resume line, which follows
                // the header, replaces this with its start step.
                if self.abs_base.is_none() {
                    self.abs_base = Some(0);
                }
                self.apply_event(TrainEvent::MaxSteps(max_steps));
                self.apply_event(TrainEvent::BatchSize(batch_size));
                self.apply_event(TrainEvent::SeqLen(seq_len));
            }
            // The run's absolute budget. It follows the header, whose `steps=`
            // counts only this process's steps, so it overrides it.
            TrainEvent::Resumed {
                start_step,
                end_step,
            } => {
                self.resume_start = Some(start_step);
                // The bar of a resumed process counts from its start step. A
                // bar frame left from before this line must not be read as a
                // restart that adds a chunk on top of it.
                self.abs_base = Some(start_step);
                self.base_from_line = true;
                // The process starts at the start step, so that is the first
                // position it can claim. Without it the header shows 0% of a
                // run that is partly done until the first val line arrives
                // (about 3195 steps later). `max` keeps a step that is
                // already further along. `holds_run_data` stays correct:
                // the budget is already stated here, so it was true before.
                self.step = self.step.max(start_step);
                self.max_steps = end_step;
                self.stated_max_steps = end_step;
            }
            TrainEvent::MaxSteps(v) => {
                self.max_steps = v;
                self.stated_max_steps = v;
            }
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

    /// True when `step` counts steps within a chunk, so it cannot be set
    /// against the run's budget: no percentage, no `step / budget` and no ETA
    /// apply to it. That is the case when the step comes from a bar (or a
    /// plain `Step:` line) and either the bar has restarted (`chunked_bar`)
    /// or the bar's total is smaller than a stated budget. The second
    /// condition makes the step chunk-local from the first bar, so the
    /// display does not change at the first restart. A step taken from a
    /// line that carries a step and a time (`step_from_reported_line`) is
    /// the trainer's global counter and is never chunk-local.
    ///
    /// A resumed run whose bar counts only the remaining steps also has a
    /// bar total below the budget, and is shown without the budget too.
    ///
    /// Never true once `abs_base` is known: the bar's step is then rebuilt
    /// as `base + local`, which is the run's global step.
    pub fn step_is_chunk_local(&self) -> bool {
        self.abs_base.is_none()
            && !self.step_from_reported_line
            && (self.chunked_bar
                || (self.stated_max_steps > 0
                    && self.last_bar_total > 0
                    && self.last_bar_total < self.stated_max_steps))
    }

    /// Seconds left in the run. `None` when the budget or step time is
    /// unknown, or when the step is chunk-local (a chunk-relative ETA is not
    /// the run's).
    pub fn eta_secs(&self) -> Option<f32> {
        if self.step_is_chunk_local() {
            return None;
        }
        if self.max_steps == 0 || self.step_ms <= 0.0 || self.step >= self.max_steps {
            return None;
        }
        Some((self.max_steps - self.step) as f32 * (self.step_ms / 1000.0))
    }

    /// A checkpoint was just written: start the pulse, remember the step, and
    /// flag the latest step bar so the chart can mark it.
    pub fn mark_checkpoint(&mut self) {
        self.checkpoint_pulse = CKPT_PULSE_TICKS;
        self.checkpoint_step = self.step;
        if let Some(last) = self.step_history.last_mut() {
            last.checkpoint = true;
        }
    }
}

/// Incremental line reader that only ever returns newly-appended lines.
pub struct Tailer {
    path: PathBuf,
    offset: u64,
}

/// True for every event that carries a trainer-printed step time, whether or
/// not a cache count came with it. `poll` uses this to stop deriving or
/// bar-observing step times once the trainer reports its own.
fn is_reported_step_time(ev: &TrainEvent) -> bool {
    matches!(
        ev,
        TrainEvent::StepTime { .. } | TrainEvent::StepAndTime { .. } | TrainEvent::StepAndMs { .. }
    )
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
                        // A progress bar redraws itself with carriage
                        // returns rather than newlines, so a "line" here can
                        // hold many bar frames. Split them out and keep the
                        // last, which is the bar's current state — otherwise
                        // a tqdm-only trainer (ttml's SFTTrainer reports
                        // loss *solely* as bar postfix) yields nothing until
                        // the run ends.
                        for seg in line.trim_end().split('\r') {
                            if !seg.trim().is_empty() {
                                out.push(seg.to_string());
                            }
                        }
                    } else if let Some(last_cr) = line.rfind('\r') {
                        // No newline — but a carriage return ENDS a progress
                        // frame just as definitively as a newline ends a
                        // line, so everything before the final '\r' is
                        // settled and can be emitted now.
                        //
                        // Without this the newline requirement above defeats
                        // the '\r' handling below it. A trainer that reports
                        // loss only as bar postfix emits no newline at all
                        // between its startup banner and its final summary:
                        // measured against ttml's SFTTrainer, ~300 seconds
                        // and 3000 steps of one unterminated line. The whole
                        // run buffered here and arrived in a single burst at
                        // the end, so the loss mountains stayed empty, the
                        // model/topology fields never resolved, and the LIVE
                        // block froze a few seconds after attach.
                        //
                        // Consume up to and including that '\r'; the tail
                        // after it is the bar's in-flight frame, which is
                        // re-read whole on the next poll.
                        consumed += (last_cr + 1) as u64;
                        for seg in line[..last_cr].split('\r') {
                            if !seg.trim().is_empty() {
                                out.push(seg.to_string());
                            }
                        }
                        break;
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
    /// When the checkpoint was last written.
    ///
    /// tt-train's rolling checkpoint is a *directory* of per-tensor
    /// `.tensorbin` files, rewritten in place on every save. A directory's
    /// own mtime only moves when entries are added or removed, so it stays
    /// frozen for the whole run — measured on a live run, the directory sat
    /// at one timestamp while the files inside advanced every ten seconds.
    /// Taking the directory's own mtime therefore means the pulse never
    /// fires on a real run; the newest entry is the real save time.
    fn last_written(path: &std::path::Path) -> Option<SystemTime> {
        let md = std::fs::metadata(path).ok()?;
        if !md.is_dir() {
            return md.modified().ok();
        }
        let mut newest: Option<SystemTime> = None;
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            if let Ok(m) = entry.metadata().and_then(|m| m.modified()) {
                newest = Some(newest.map_or(m, |n| n.max(m)));
            }
        }
        // An empty checkpoint directory falls back to its own mtime rather
        // than reporting "unreadable", which would look like no checkpoint.
        newest.or_else(|| md.modified().ok())
    }

    pub fn poll(&mut self) -> bool {
        let Some(m) = Self::last_written(&self.path) else {
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
    /// True once the log has stated a step time itself (tt-train does, on
    /// its combined per-step line, and the tt-tnt harness does with no cache
    /// count; see `is_reported_step_time`). While false, step time is derived from
    /// how fast step lines arrive — see `note_step_progress`.
    saw_reported_step_time: bool,
    /// `(step number, when we saw it)` for the last poll that advanced the
    /// step. The gap to the next one is what the derivation measures.
    last_step_seen: Option<(u64, Instant)>,
    /// True while `last_step_seen` is a BASELINE: the first step seen after
    /// attach or after a new-run reset. The viewer chose when to look, so the
    /// step it shows may have started long before. False once the anchor is
    /// an OBSERVED CHANGE (a poll that saw the step rise or restart). Only an
    /// observed change can be the start of a measurement.
    anchor_is_baseline: bool,
    /// When `note_step_progress` last ran on a parsed step. A gap longer than
    /// `POLL_GAP_REBASE` since then means the view was away and the anchor
    /// is stale.
    last_note_at: Option<Instant>,
    /// `(cumulative CPU ticks, when read)` for the trainer, so CPU percent
    /// is a rate between polls rather than the process's lifetime average —
    /// the latter would understate a run that has only just got busy.
    last_cpu: Option<(u64, Instant)>,
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
            saw_reported_step_time: false,
            last_step_seen: None,
            anchor_is_baseline: false,
            last_note_at: None,
            last_cpu: None,
        }
    }

    /// Sample the trainer's CPU percent and RSS from `/proc/<pid>`.
    ///
    /// CPU is a rate between polls: `utime + stime` are cumulative counters,
    /// so reporting them against process age would show a long run that has
    /// only just become busy as almost idle. 100.0 means one core saturated,
    /// matching `top`, so a value above 100 is normal on a threaded loader.
    ///
    /// This is the process's own CPU, threads included but child processes
    /// not. That matches the common case — a Python trainer parallelises
    /// with threads — but a harness that forks worker processes will read
    /// low here, and its workers are separate pids we do not attribute.
    #[cfg(target_os = "linux")]
    fn sample_host_cost(&mut self, pid: i32, now: Instant) {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return;
        };
        // Fields after the comm field, which may itself contain spaces and
        // is parenthesised — split there rather than on whitespace.
        let Some(rest) = stat.rsplit_once(')') else {
            return;
        };
        let f: Vec<&str> = rest.1.split_whitespace().collect();
        // utime and stime are fields 14 and 15 (1-based) of the whole line;
        // after the comm split they are index 11 and 12.
        let (Some(ut), Some(st)) = (
            f.get(11).and_then(|v| v.parse::<u64>().ok()),
            f.get(12).and_then(|v| v.parse::<u64>().ok()),
        ) else {
            return;
        };
        let ticks = ut + st;
        let hz = 100.0; // USER_HZ is 100 on every Linux target we support.
        if let Some((prev_ticks, prev_at)) = self.last_cpu {
            let dt = now.duration_since(prev_at).as_secs_f32();
            if dt > 0.05 && ticks >= prev_ticks {
                let pct = (ticks - prev_ticks) as f32 / hz / dt * 100.0;
                self.state.host_cpu_pct = Some(pct);
            }
        }
        self.last_cpu = Some((ticks, now));

        if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
            for line in status.lines() {
                if let Some(v) = line.strip_prefix("VmRSS:") {
                    if let Some(kb) = v
                        .split_whitespace()
                        .next()
                        .and_then(|k| k.parse::<u64>().ok())
                    {
                        self.state.host_rss_bytes = Some(kb * 1024);
                    }
                    break;
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn sample_host_cost(&mut self, _pid: i32, _now: Instant) {}

    /// Derive a per-step wall time from how fast step lines arrive.
    ///
    /// A harness that drives `ttml` from Python prints no step time at all,
    /// so without this its run shows no step/s, no tokens/sec and no ETA.
    /// The measurement divides by the *step delta*, not by one, so a harness
    /// that logs every Nth step still yields a per-step figure.
    ///
    /// Only used when the log never states a step time itself — a reported
    /// number is the trainer's own measurement of compute and always beats
    /// our measurement of its logging. Deliberately skipped for the first
    /// observation (nothing to measure against) and for implausible gaps, so
    /// the burst of backlog read at attach can't be mistaken for one step.
    ///
    /// Three further behaviours, all for progress-bar harnesses:
    /// * When exactly one step was seen since the previous poll, the gap is
    ///   that step's own time (to within one poll interval) and is recorded as
    ///   an observed sample. A poll that saw several steps measures an average,
    ///   so it updates the rate but records no sample.
    /// * When the step goes backwards the bar restarted for a new chunk. The
    ///   monitor re-anchors without measuring (the gap spans validation and
    ///   checkpoint work, not a step) and flags `chunked_bar`. Before this, the
    ///   derivation froze until the bar passed its old position.
    /// * `chunked_bar` also tells the view the bar's total is a chunk size.
    ///
    /// A measurement needs both ends to be observed step changes. The first
    /// step seen after attach (or after a new-run reset) is only a baseline:
    /// the step may have begun at any time before the viewer looked. The
    /// first increase after a baseline therefore re-anchors at that poll and
    /// measures nothing, and records no observed sample. This matters most for
    /// a trainer that prints one step line per chunk (the tt-tnt harness
    /// prints an absolute validation line every 3195 steps). Attached 166 s
    /// into a chunk, the first 3195-step jump used to be divided by 166 s,
    /// which gave 52 ms/step and 630k tokens/s against a true 300 ms/step.
    /// A jump that began before the anchor must not be timed from the anchor.
    ///
    /// Consequences: a trainer that advances one step at a time loses one
    /// step of cadence after attach. A chunk-jump trainer gets its first
    /// `step_ms` at the second chunk end after attach, and that value spans
    /// train, checkpoint and validation time for a chunk, so it is an
    /// effective throughput slightly below pure training speed. Until then
    /// `step_ms` is 0 and `tokens_per_sec()` is `None`.
    ///
    /// The monitor is polled only while the Training view is on screen. After
    /// a pause longer than `POLL_GAP_REBASE` the anchor may be minutes old, and
    /// a chunk that ended during the pause would be timed from it (too fast
    /// if the anchor is old and the end is seen late in the next window, too
    /// slow in the other order). So the first call after a pause re-baselines:
    /// it re-anchors at the current step, measures nothing and records no
    /// sample, whatever changed. Returning to the view therefore costs one
    /// more observed change before a rate appears.
    ///
    /// A bar restart is an observed event with a known time, so the
    /// regression arm anchors as an observed change. The next step is timed
    /// from the restart poll and includes that chunk's first-step warm-up,
    /// which this code has always accepted. This applies to a bar with no
    /// base (`TrainState::abs_base`), whose step really goes down.
    ///
    /// A bar rebuilt on a base never goes down: the next chunk's first frame
    /// is `base + 1`, one more than the last. Without a guard that +1 would
    /// be timed across checkpoint save, validation and the first step's
    /// warm-up, which is one huge step: a spike in `step_ms`, a giant star
    /// and a poisoned average. So a chunk boundary in such a run (an absolute
    /// step line beside a bar, or a restart that moved the base) re-baselines
    /// the anchor as a pause does. The step after the boundary only
    /// re-anchors, and timing resumes from the step after that.
    fn note_step_progress(&mut self, now: Instant) {
        if self.saw_reported_step_time {
            return;
        }
        // No step line has been parsed yet: no loss has been seen (the parser
        // drops a `0/N` bar, which has no loss). `step` may still be non-zero,
        // because a resume line sets it to the start step. Anchoring on that
        // placeholder would time the first real step from an arbitrary poll,
        // so the gap (model load, data load, compile) would be recorded as
        // step time. The first parsed step only anchors.
        if self.state.loss.is_none() && (self.state.step == 0 || self.state.resume_start.is_some())
        {
            return;
        }
        let step = self.state.step;
        let paused = self
            .last_note_at
            .is_some_and(|t| now.saturating_duration_since(t) > POLL_GAP_REBASE);
        self.last_note_at = Some(now);
        // A chunk boundary in a bar run (see `TrainState::cadence_rebase`)
        // re-baselines exactly as a pause does. The step is global there, so
        // the regression arm below no longer sees the boundary, and the first
        // step after it would be timed across checkpoint, validation and
        // warm-up (about a minute on a live tt-tnt run).
        let boundary = std::mem::take(&mut self.state.cadence_rebase);
        if paused || boundary {
            if let Some((prev_step, _)) = self.last_step_seen {
                if step < prev_step {
                    self.state.chunked_bar = true;
                }
                self.last_step_seen = Some((step, now));
                self.anchor_is_baseline = true;
                return;
            }
        }
        match self.last_step_seen {
            Some((prev_step, _)) if step < prev_step => {
                self.state.chunked_bar = true;
                self.last_step_seen = Some((step, now));
                self.anchor_is_baseline = false;
                return;
            }
            Some((prev_step, _)) if step > prev_step && self.anchor_is_baseline => {
                // First increase after the baseline: re-anchor, measure nothing.
                self.last_step_seen = Some((step, now));
                self.anchor_is_baseline = false;
                return;
            }
            Some((prev_step, prev_at)) if step > prev_step => {
                let dt_ms = now.duration_since(prev_at).as_secs_f32() * 1000.0;
                let dsteps = (step - prev_step) as f32;
                let per_step = dt_ms / dsteps;
                // Under 1 ms is backlog being replayed, not training; over
                // two minutes a step means the run stalled or we were
                // descheduled, and either way it is not a rate worth showing.
                if (1.0..=120_000.0).contains(&per_step) {
                    // Smoothed: a single slow step (a checkpoint save, a
                    // scheduler hiccup) shouldn't visibly jerk the readout.
                    self.state.step_ms = if self.state.step_ms > 0.0 {
                        self.state.step_ms * 0.7 + per_step * 0.3
                    } else {
                        per_step
                    };
                    // Exactly one step since the last poll: the gap is that
                    // step's own time, to within one poll interval. More than
                    // one is an average, which is not recorded as a sample.
                    if step - prev_step == 1 {
                        self.state.record_observed_step(step, per_step);
                    }
                }
            }
            _ => {}
        }
        if self.last_step_seen.is_none() {
            // The first step seen is a baseline and starts no measurement.
            self.last_step_seen = Some((step, now));
            self.anchor_is_baseline = true;
        } else if self.last_step_seen.map(|(s, _)| step > s).unwrap_or(false) {
            self.last_step_seen = Some((step, now));
            self.anchor_is_baseline = false;
        }
    }

    /// Find a model-topology YAML the log named but the cmdline didn't pass.
    ///
    /// A harness that assembles its config in Python has no config file to
    /// point at, but it does print which topology it chose. Searched beneath
    /// the trainer's own working directory and depth-bounded, so this stays
    /// a quick look near the run rather than a filesystem crawl.
    #[cfg(target_os = "linux")]
    fn find_named_config(pid: i32, name: &str) -> Option<PathBuf> {
        const MAX_DEPTH: usize = 4;
        let root = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
        let mut queue = vec![(root, 0usize)];
        while let Some((dir, depth)) = queue.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.file_name().is_some_and(|f| f == name) {
                    return Some(path);
                }
                if depth < MAX_DEPTH && e.file_type().is_ok_and(|t| t.is_dir()) {
                    // Skip the usual large, uninteresting trees rather than
                    // descending into a venv or a build directory.
                    let skip = path.file_name().is_some_and(|f| {
                        matches!(
                            f.to_string_lossy().as_ref(),
                            ".git" | "target" | "build" | "node_modules" | "__pycache__" | ".venv"
                        )
                    });
                    if !skip {
                        queue.push((path, depth + 1));
                    }
                }
            }
        }
        None
    }

    #[cfg(not(target_os = "linux"))]
    fn find_named_config(_pid: i32, _name: &str) -> Option<PathBuf> {
        None
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
            // A compiled example is the cheap case; a Python harness driving
            // ttml costs a `maps` read, so only reach for it when the first
            // match failed. The gate is that tt-train's own extension module
            // is actually mapped — see `parse_python_trainer`.
            if let Some(p) = parse_python_trainer(&cmdline, pid, Self::maps_ttml(pid)) {
                return Some(p);
            }
        }
        None
    }

    /// Whether tt-train's `ttml` extension module is mapped into `pid`.
    ///
    /// Reading `/proc/<pid>/maps` needs no more privilege than reading the
    /// cmdline for our own processes, and fails closed (`false`) for
    /// anything we can't read, so a foreign process is never claimed.
    #[cfg(target_os = "linux")]
    fn maps_ttml(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/maps"))
            .map(|m| {
                m.lines().any(|l| {
                    // The compiled binding (`_ttml*.so`) or the package
                    // directory tt-train installs it under.
                    l.contains("_ttml") || l.contains("/ttml/")
                })
            })
            .unwrap_or(false)
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
            // tt-train's own configs write this path with a `${...}` prefix
            // and resolve it two directories above the training config
            // (`configs/training_configs/x.yaml` -> the tt-train root), so
            // reproduce that rule first and keep the looser guesses as
            // fallbacks for hand-written layouts.
            let mp = Self::expand_env_for_pid(p.pid, &mp);
            let dir = cfg_path
                .parent()
                .map(|b| b.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            let tt_train_root = dir.parent().and_then(|d| d.parent());
            let candidates = [
                tt_train_root.map(|r| r.join(&mp)),
                Some(dir.join(&mp)),
                Some(Self::resolve_for_pid(p.pid, &mp)),
            ];
            for cand in candidates.into_iter().flatten() {
                if let Ok(t) = std::fs::read_to_string(&cand) {
                    merge_model_yaml(&mut cfg, &t);
                    break;
                }
            }
        }
        cfg
    }

    /// Expand `${VAR}` using the *trainer's* environment.
    ///
    /// tt-train's sample configs point at their model YAML through
    /// `${TT_METAL_RUNTIME_ROOT}`, which is set for the training process and
    /// says nothing about ours. An unset or unreadable variable expands to
    /// nothing, leaving a relative path the fallbacks below can still find.
    #[cfg(target_os = "linux")]
    fn expand_env_for_pid(pid: i32, path: &str) -> String {
        if !path.contains("${") {
            return path.to_string();
        }
        let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        let text = String::from_utf8_lossy(&environ);
        let mut out = path.to_string();
        for entry in text.split('\0') {
            if let Some((k, v)) = entry.split_once('=') {
                out = out.replace(&format!("${{{k}}}"), v);
            }
        }
        // Anything still unexpanded would only produce a path that cannot
        // exist; drop the marker so the relative remainder stays usable.
        while let Some(start) = out.find("${") {
            match out[start..].find('}') {
                Some(end) => out.replace_range(start..start + end + 1, ""),
                None => break,
            }
        }
        out.trim_start_matches('/').to_string()
    }

    #[cfg(not(target_os = "linux"))]
    fn expand_env_for_pid(_pid: i32, path: &str) -> String {
        path.to_string()
    }

    /// True while the attached process is still alive.
    fn still_alive(pid: i32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    /// Forget everything the monitor itself remembers about the previous run.
    ///
    /// `poll` replaces `self.state` at detach and again at attach, but these
    /// fields live on the monitor, not the state. Left alone, the old run's
    /// step anchor makes a new run's first step look like a bar restart (so
    /// `chunked_bar` is set on a fresh run), a previous run's printed step
    /// times stop a bar harness from ever being timed, and the old process's
    /// CPU ticks would be differenced against the new process's. Both sites
    /// call this so they cannot drift apart.
    fn reset_run_anchors(&mut self) {
        self.last_step_seen = None;
        self.anchor_is_baseline = false;
        self.last_note_at = None;
        self.saw_reported_step_time = false;
        self.last_cpu = None;
    }

    /// One tick: attach if needed, drain new log lines, check the checkpoint.
    pub fn poll(&mut self) {
        // Detach if the run ended.
        if let Some(p) = self.state.proc.as_ref() {
            if !Self::still_alive(p.pid) {
                self.state = TrainState::new();
                self.reset_run_anchors();
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
                self.reset_run_anchors();
                self.state.first_seen = Some(Instant::now());
                self.state.config = cfg;
                self.state.log = Some(log);
                self.state.proc = Some(p);
            }
            return;
        }

        // Drain newly-appended log lines.
        if let Some(t) = self.tailer.as_mut() {
            let lines = t.read_new();
            self.ingest_lines(lines);
        }
        // Measure the step cadence after the batch, so a poll that read many
        // lines counts as one observation rather than many.
        let now = Instant::now();
        self.note_step_progress(now);
        // Host-side cost of the run, sampled every poll — this is the only
        // signal a CPU-bound run produces, and it stays useful for a
        // device-backed one (the data pipeline and compiler live here too).
        if let Some(pid) = self.state.proc.as_ref().map(|p| p.pid) {
            self.sample_host_cost(pid, now);
        }

        // Checkpoint pulse.
        if self.state.checkpoint_pulse > 0 {
            self.state.checkpoint_pulse -= 1;
        }
        if let Some(w) = self.ckpt.as_mut() {
            if w.poll() {
                self.state.mark_checkpoint();
            }
        }
    }

    /// Parse one poll's batch of log lines and fold them into the state.
    /// Split out of `poll` so tests can replay a log through exactly the
    /// path a live poll takes, with injected times for the cadence check.
    fn ingest_lines(&mut self, lines: Vec<String>) {
        let pid = self.state.proc.as_ref().map(|p| p.pid);
        for line in lines {
            let Some(ev) = parse_train_line(&line) else {
                continue;
            };
            match ev {
                // The trainer stated its own step time, so stop deriving
                // one from log cadence.
                ref e if is_reported_step_time(e) => {
                    self.saw_reported_step_time = true;
                    self.state.apply_event(ev);
                }
                // A topology YAML named in the log: locate it near the
                // run and merge it, the same as a `--config`-supplied
                // one. Any `SeqLen` later in the log still wins, since
                // the harness prints its summary after this line.
                TrainEvent::ModelConfigFile(ref name) => {
                    if let Some(pid) = pid {
                        if let Some(path) = Self::find_named_config(pid, name) {
                            if let Ok(text) = std::fs::read_to_string(&path) {
                                // The YAML states the topology's *declared*
                                // maximum sequence length; a run may narrow
                                // it (tt-tnt runs 512 against a declared
                                // 2048). Taking the YAML's number would
                                // overstate tokens/sec fourfold, so a
                                // length the run already told us survives
                                // the merge. In practice the log names the
                                // YAML before stating its length, so this
                                // guards the other ordering — but tokens
                                // /sec being wrong by a factor of four is
                                // not something to leave resting on the
                                // order two lines happen to be printed in.
                                let run_seq = self.state.config.max_sequence_length;
                                merge_model_yaml(&mut self.state.config, &text);
                                if run_seq.is_some() {
                                    self.state.config.max_sequence_length = run_seq;
                                }
                            }
                        }
                    }
                }
                // A run header after an earlier run in the same log starts
                // a new run (the state resets itself on it). The monitor's
                // step anchor and reported-time flag belong to the old run
                // too: a second run may be a bar-only trainer after a
                // reported one. `last_cpu` stays, because it differences
                // ticks of the attached process, which has not changed.
                TrainEvent::HarnessSummary { .. } => {
                    if self.state.holds_run_data() {
                        self.last_step_seen = None;
                        self.anchor_is_baseline = false;
                        self.last_note_at = None;
                        self.saw_reported_step_time = false;
                    }
                    self.state.apply_event(ev);
                }
                _ => self.state.apply_event(ev),
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

    /// Host cost is the only signal a CPU-bound run produces, so it has to
    /// come off a real process rather than a fixture — and CPU has to be a
    /// *rate*, not the lifetime average, or a long run that has just become
    /// busy reads as idle.
    #[test]
    #[cfg(target_os = "linux")]
    fn host_cpu_and_rss_are_sampled_from_a_real_process() {
        // A child that burns CPU *in its own process*: shell builtins only,
        // no forks. A loop calling `date` spends its cycles in short-lived
        // children instead, which `utime`/`stime` rightly exclude — the
        // first version of this test did that and measured 13%.
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("i=0; while [ $i -lt 100000000 ]; do i=$((i+1)); done")
            .spawn()
            .expect("spawn busy child");
        let pid = child.id() as i32;

        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        // First sample only establishes the baseline counter.
        m.sample_host_cost(pid, t0);
        assert_eq!(m.state.host_cpu_pct, None, "one reading cannot be a rate");

        std::thread::sleep(Duration::from_millis(600));
        m.sample_host_cost(pid, Instant::now());

        child.kill().ok();
        child.wait().ok();

        let cpu = m
            .state
            .host_cpu_pct
            .expect("a busy process must report CPU");
        assert!(
            cpu > 20.0,
            "a spin loop should show substantial CPU, got {cpu:.1}%"
        );
        assert!(
            m.state.host_rss_bytes.is_some_and(|r| r > 0),
            "RSS must be read"
        );
    }

    /// A pid that has exited must degrade quietly — a monitoring view keeps
    /// drawing rather than propagating an error.
    #[test]
    #[cfg(target_os = "linux")]
    fn host_cost_of_a_dead_pid_is_simply_absent() {
        let mut m = TrainMonitor::new();
        m.sample_host_cost(999_999, Instant::now());
        assert_eq!(m.state.host_cpu_pct, None);
        assert_eq!(m.state.host_rss_bytes, None);
    }

    /// A progress bar redraws with carriage returns, so many bar frames
    /// arrive inside one newline-terminated chunk. Handing that chunk over
    /// whole means the parser sees a single mangled line and the run's only
    /// source of loss — `ttml`'s SFTTrainer reports it solely as bar postfix
    /// — never lands.
    #[test]
    fn the_tailer_splits_carriage_return_progress_frames() {
        let dir = std::env::temp_dir().join(format!("ttcr_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("train.log");
        std::fs::write(
            &path,
            "SFTTrainer: 1/9 [00:01<00:09, 1.0it/s, loss=2.0]\r             SFTTrainer: 2/9 [00:02<00:08, 1.0it/s, loss=1.9]\r             SFTTrainer: 3/9 [00:03<00:07, 1.0it/s, loss=1.8]\n",
        )
        .unwrap();

        let got = Tailer::new(path.clone()).read_new();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(got.len(), 3, "each bar frame is its own line, got {got:?}");
        assert!(got[2].contains("loss=1.8"), "the newest frame must survive");
    }

    /// A tqdm-only trainer emits NO newline between its banner and its final
    /// summary — ttml's SFTTrainer reports loss solely as bar postfix, so a
    /// 3000-step run is one unterminated line for ~300 seconds. Requiring a
    /// newline buffers the entire run and delivers it in one burst at the end,
    /// which is exactly the failure the '\r' splitting exists to prevent.
    #[test]
    fn carriage_return_frames_are_emitted_without_a_trailing_newline() {
        let dir = std::env::temp_dir().join(format!("ttcrnl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("train.log");

        // No trailing newline anywhere — the shape tqdm actually writes.
        std::fs::write(
            &path,
            "SFTTrainer: 1/9 [00:01, 1.0it/s, loss=2.0]\rSFTTrainer: 2/9 [00:02, 1.0it/s, loss=1.9]\rSFTTrainer: 3/9 [00:03, 1.0it/s, loss=1.8]",
        )
        .unwrap();

        let mut t = Tailer::new(path.clone());
        let got = t.read_new();
        assert_eq!(
            got.len(),
            2,
            "the two completed frames must land; the third is still in flight, got {got:?}"
        );
        assert!(
            got[1].contains("loss=1.9"),
            "newest COMPLETE frame, got {got:?}"
        );

        // The in-flight frame is not consumed, so it arrives once terminated.
        std::fs::write(
            &path,
            "SFTTrainer: 1/9 [00:01, 1.0it/s, loss=2.0]\rSFTTrainer: 2/9 [00:02, 1.0it/s, loss=1.9]\rSFTTrainer: 3/9 [00:03, 1.0it/s, loss=1.8]\rSFTTrainer: 4/9 [00:04, 1.0it/s, loss=1.7]",
        )
        .unwrap();
        let got2 = t.read_new();
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            got2.iter().any(|l| l.contains("loss=1.8")),
            "the previously-partial frame must not be lost, got {got2:?}"
        );
    }

    /// tt-train's rolling checkpoint is a directory of per-tensor files that
    /// it rewrites in place. Watching the directory's own mtime sees nothing
    /// — on a live run it stayed frozen while the files inside advanced
    /// every ten seconds — so the pulse never fires and the comet never
    /// crosses the sky.
    #[test]
    fn a_checkpoint_directory_pulses_when_its_contents_are_rewritten() {
        let dir = std::env::temp_dir().join(format!("ttckptdir_{}", std::process::id()));
        let ckpt = dir.join("transformer.msgpack");
        std::fs::create_dir_all(&ckpt).unwrap();
        std::fs::write(ckpt.join("block_0.tensorbin"), b"v1").unwrap();

        let dir_mtime_before = std::fs::metadata(&ckpt).unwrap().modified().unwrap();

        let mut w = CheckpointWatch::new(ckpt.clone());
        assert!(!w.poll(), "first observation establishes a baseline");

        // Rewrite an existing entry, exactly as a save does — no entry is
        // added or removed, so the directory's own mtime does not move.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ckpt.join("block_0.tensorbin"), b"v2").unwrap();
        let pulsed = w.poll();
        let dir_mtime_after = std::fs::metadata(&ckpt).unwrap().modified().unwrap();

        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            dir_mtime_before, dir_mtime_after,
            "precondition: rewriting an entry must not move the directory's \
             own mtime, or this test proves nothing"
        );
        assert!(
            pulsed,
            "a save that rewrites files in place must still pulse"
        );
    }

    /// The run's own sequence length must beat the topology YAML's declared
    /// maximum, whichever order they arrive in. tt-tnt runs 512 against a
    /// declared 2048; taking the YAML's number overstates tokens/sec 4×.
    #[test]
    fn a_narrowed_run_sequence_length_survives_the_topology_merge() {
        // The real tt-tnt-384.yaml's relevant lines.
        let yaml = "transformer_config:\n  num_heads: 6\n  embedding_dim: 384\n  \
                    num_blocks: 6\n  vocab_size: 32000\n  max_sequence_length: 2048\n";

        // Order as the log actually prints it: YAML named first, run's own
        // length stated after.
        let mut cfg = TrainConfig::default();
        merge_model_yaml(&mut cfg, yaml);
        let mut st = TrainState::new();
        st.config = cfg;
        st.apply_event(TrainEvent::SeqLen(512));
        assert_eq!(st.config.max_sequence_length, Some(512));

        // The other order — the guard in the merge arm, expressed directly.
        let mut cfg2 = TrainConfig {
            max_sequence_length: Some(512),
            ..TrainConfig::default()
        };
        let run_seq = cfg2.max_sequence_length;
        merge_model_yaml(&mut cfg2, yaml);
        if run_seq.is_some() {
            cfg2.max_sequence_length = run_seq;
        }
        assert_eq!(
            cfg2.max_sequence_length,
            Some(512),
            "the topology's declared maximum must not overwrite the run's own length"
        );
        // The rest of the topology must still come through.
        assert_eq!(cfg2.num_blocks, Some(6));
        assert_eq!(cfg2.embedding_dim, Some(384));
    }

    /// A harness that builds its config in Python has no config file to
    /// pass on the cmdline, but it does print which topology YAML it chose.
    /// Finding it is what turns `topology unknown` into a real model card.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_config_named_in_the_log_is_found_beneath_the_trainers_cwd() {
        let dir = std::env::temp_dir().join(format!("ttfindcfg_{}", std::process::id()));
        let nested = dir.join("train").join("configs").join("model");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("tt-tnt-384.yaml"), "transformer_config:\n").unwrap();
        // A tree that must be skipped rather than descended into.
        let skipped = dir.join(".venv").join("deep");
        std::fs::create_dir_all(&skipped).unwrap();
        std::fs::write(skipped.join("decoy.yaml"), "x").unwrap();

        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .current_dir(&dir)
            .spawn()
            .expect("spawn test child");
        let pid = child.id() as i32;

        let found = TrainMonitor::find_named_config(pid, "tt-tnt-384.yaml");
        let decoy = TrainMonitor::find_named_config(pid, "decoy.yaml");
        let absent = TrainMonitor::find_named_config(pid, "nothing-here.yaml");

        child.kill().ok();
        child.wait().ok();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            found.as_deref(),
            Some(nested.join("tt-tnt-384.yaml").as_path()),
            "a config nested under the trainer's cwd must be found"
        );
        assert!(
            decoy.is_none(),
            "trees like .venv must be skipped, not crawled"
        );
        assert!(absent.is_none(), "a name that isn't there must not match");
    }

    /// A ttml-driven harness prints no step time, so without deriving one
    /// from log cadence its run shows no step/s, no tokens/sec and no ETA.
    #[test]
    fn step_time_is_derived_from_log_cadence_when_the_log_never_states_one() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();

        // First observation only establishes a baseline — there is nothing
        // to measure a gap against yet.
        m.state.step = 10;
        m.note_step_progress(t0);
        assert_eq!(m.state.step_ms, 0.0, "one observation cannot be a rate");

        // The first increase only re-anchors (the baseline is not a step
        // boundary), so it measures nothing.
        m.state.step = 11;
        m.note_step_progress(t0 + Duration::from_millis(100));
        assert_eq!(m.state.step_ms, 0.0, "the first increase is not measured");

        // 400 ms later, 2 steps on: 200 ms per step.
        m.state.step = 13;
        m.note_step_progress(t0 + Duration::from_millis(500));
        assert!(
            (m.state.step_ms - 200.0).abs() < 1.0,
            "expected ~200ms/step from a 400ms gap over 2 steps, got {}",
            m.state.step_ms
        );
        assert!(m.state.steps_per_sec() > 0.0);
    }

    /// The trainer's own measurement of its compute always beats our
    /// measurement of how fast it writes to a file.
    #[test]
    fn a_reported_step_time_is_never_overwritten_by_the_derived_one() {
        let mut m = TrainMonitor::new();
        m.saw_reported_step_time = true;
        m.state.step_ms = 1124.5;

        let t0 = Instant::now();
        m.state.step = 1;
        m.note_step_progress(t0);
        m.state.step = 2;
        m.note_step_progress(t0 + Duration::from_millis(50));
        m.state.step = 3;
        m.note_step_progress(t0 + Duration::from_millis(100));

        assert_eq!(
            m.state.step_ms, 1124.5,
            "a log that states its own step time must not be second-guessed"
        );
    }

    /// At attach the tailer replays the whole existing log at once, which
    /// would otherwise look like thousands of steps in a single instant.
    #[test]
    fn replayed_backlog_does_not_produce_an_absurd_step_rate() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();

        // One poll jumps from nothing to step 4000 — the backlog.
        m.state.step = 4000;
        m.note_step_progress(t0);
        // The first increase only anchors (it is not measured at all).
        m.state.step = 4001;
        m.note_step_progress(t0 + Duration::from_millis(100));
        // The very next poll, microseconds later, advances one step. This
        // increase is measured, and the 1 ms lower bound rejects it.
        m.state.step = 4002;
        m.note_step_progress(t0 + Duration::from_millis(100) + Duration::from_micros(200));
        assert_eq!(
            m.state.step_ms, 0.0,
            "a sub-millisecond gap is replay, not a training step"
        );

        // A genuine gap afterwards is still measured.
        m.state.step = 4003;
        m.note_step_progress(t0 + Duration::from_millis(400));
        assert!(
            m.state.step_ms > 0.0,
            "real cadence must still be picked up"
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

#[cfg(test)]
mod step_history_tests {
    use super::*;

    fn timed(step: u64, ms: f32, cache: u32) -> TrainEvent {
        TrainEvent::StepAndTime {
            step,
            loss: 2.0,
            ms,
            cache_entries: cache,
        }
    }

    #[test]
    fn step_time_events_build_a_per_step_history() {
        let mut st = TrainState::new();
        st.apply_event(timed(1, 410.0, 8));
        st.apply_event(timed(2, 395.0, 8));
        st.apply_event(timed(3, 402.0, 8));
        let got: Vec<(u64, f32)> = st.step_history.iter().map(|s| (s.step, s.ms)).collect();
        assert_eq!(got, vec![(1, 410.0), (2, 395.0), (3, 402.0)]);
    }

    /// Attaching mid-run to a trainer whose program cache is
    /// already full must not paint the first sample as a compile: there is no
    /// earlier sample to measure growth against.
    #[test]
    fn the_first_sample_has_no_cache_baseline_so_it_is_never_a_compile() {
        let mut st = TrainState::new();
        st.apply_event(timed(900, 400.0, 64));
        st.apply_event(timed(901, 400.0, 70));
        st.apply_event(timed(902, 400.0, 70));
        let deltas: Vec<u32> = st.step_history.iter().map(|s| s.cache_delta).collect();
        assert_eq!(deltas, vec![0, 6, 0]);
    }

    #[test]
    fn the_history_is_bounded() {
        let mut st = TrainState::new();
        for i in 1..=200u64 {
            st.apply_event(timed(i, 400.0, 8));
        }
        assert_eq!(st.step_history.len(), STEP_HISTORY);
        assert_eq!(st.step_history.last().unwrap().step, 200);
        assert_eq!(
            st.step_history.first().unwrap().step,
            200 - STEP_HISTORY as u64 + 1
        );
    }

    #[test]
    fn a_second_time_line_for_the_same_step_replaces_rather_than_duplicates() {
        let mut st = TrainState::new();
        st.apply_event(timed(5, 400.0, 8));
        st.apply_event(TrainEvent::StepTime {
            ms: 450.0,
            cache_entries: 8,
        });
        assert_eq!(st.step_history.len(), 1);
        assert_eq!(st.step_history[0].ms, 450.0);
    }

    /// A trainer that prints only loss (bar-only harnesses,
    /// or cadence derived from log timing) has no per-step times. The history
    /// must stay empty rather than be filled from the derived average.
    #[test]
    fn loss_only_events_leave_the_history_empty() {
        let mut st = TrainState::new();
        for i in 1..=10u64 {
            st.apply_event(TrainEvent::Step { step: i, loss: 2.0 });
        }
        assert!(st.step_history.is_empty());
    }

    #[test]
    fn a_checkpoint_flags_the_latest_sample_and_starts_the_pulse() {
        let mut st = TrainState::new();
        st.apply_event(timed(7, 400.0, 8));
        st.apply_event(timed(8, 400.0, 8));
        st.mark_checkpoint();
        assert!(!st.step_history[0].checkpoint);
        assert!(st.step_history[1].checkpoint);
        assert_eq!(st.checkpoint_step, 8);
        assert_eq!(st.checkpoint_pulse, CKPT_PULSE_TICKS);
    }

    #[test]
    fn a_checkpoint_with_no_history_still_pulses() {
        let mut st = TrainState::new();
        st.mark_checkpoint();
        assert_eq!(st.checkpoint_pulse, CKPT_PULSE_TICKS);
    }
}

/// Test helper: poll once a second at the current step from `from` (exclusive)
/// up to `to` (exclusive), as the Training view does while it is on screen.
/// Without it a long jump in injected time would look like a pause in polling.
#[cfg(test)]
fn keep_polling(m: &mut TrainMonitor, from: Instant, to: Instant) {
    let mut t = from + Duration::from_secs(1);
    while t < to {
        m.note_step_progress(t);
        t += Duration::from_secs(1);
    }
}

#[cfg(test)]
mod observed_step_tests {
    use super::*;
    use std::time::Duration;

    fn bar_state_at(m: &mut TrainMonitor, step: u64, now: Instant) {
        m.state.step = step;
        m.note_step_progress(now);
    }

    #[test]
    fn one_step_between_polls_is_timed_from_the_gap() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        // The first increase after attach only re-anchors.
        bar_state_at(&mut m, 11, t0 + Duration::from_millis(100));
        assert!(m.state.step_history.is_empty());
        bar_state_at(&mut m, 12, t0 + Duration::from_millis(400));
        assert_eq!(m.state.step_history.len(), 1);
        let s = m.state.step_history[0];
        assert!((s.ms - 300.0).abs() < 1.0, "{}", s.ms);
        assert_eq!(s.step, 12);
        assert_eq!(s.seq, 1);
        assert_eq!(s.cache_delta, 0, "unknown growth is not a compile");
        assert_eq!(m.state.step_time_source, StepTimeSource::Observed);
    }

    /// A poll that read several steps measures an average, which would give
    /// the chart resolution it does not have.
    #[test]
    fn several_steps_in_one_poll_record_no_sample_but_still_update_the_rate() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        bar_state_at(&mut m, 11, t0 + Duration::from_millis(100));
        bar_state_at(&mut m, 14, t0 + Duration::from_millis(1000));
        assert!(m.state.step_history.is_empty());
        assert!((m.state.step_ms - 300.0).abs() < 1.0, "{}", m.state.step_ms);
    }

    /// The existing freeze bug: after a bar restart the cadence derivation
    /// must resume. The gap across the restart is validation work, not a step.
    #[test]
    fn a_bar_restart_records_no_sample_flags_the_bar_and_resumes_timing() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 3193, t0);
        bar_state_at(&mut m, 3194, t0 + Duration::from_millis(300));
        bar_state_at(&mut m, 3195, t0 + Duration::from_millis(600));
        assert_eq!(m.state.step_history.len(), 1);
        // Validation and a checkpoint take 40 s, then the bar restarts at 1.
        keep_polling(
            &mut m,
            t0 + Duration::from_millis(600),
            t0 + Duration::from_millis(40_600),
        );
        bar_state_at(&mut m, 1, t0 + Duration::from_millis(40_600));
        assert_eq!(
            m.state.step_history.len(),
            1,
            "no sample for the restart gap"
        );
        assert!(m.state.chunked_bar);
        // Timing resumes straight away, far below the old step number.
        let before = m.state.step_ms;
        bar_state_at(&mut m, 2, t0 + Duration::from_millis(40_850));
        assert_eq!(m.state.step_history.len(), 2);
        assert!((m.state.step_history[1].ms - 250.0).abs() < 1.0);
        assert_ne!(
            m.state.step_ms, before,
            "the derived rate must update again"
        );
        // seq keeps rising across the restart even though step went back.
        assert_eq!(m.state.step_history[1].seq, 2);
        assert!(m.state.step_history[1].step < m.state.step_history[0].step);
    }

    #[test]
    fn a_trainer_that_reports_its_own_step_time_is_left_alone() {
        let mut m = TrainMonitor::new();
        m.saw_reported_step_time = true;
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        bar_state_at(&mut m, 11, t0 + Duration::from_millis(300));
        bar_state_at(&mut m, 12, t0 + Duration::from_millis(600));
        assert!(m.state.step_history.is_empty());
        assert_eq!(m.state.step_time_source, StepTimeSource::Unknown);
    }

    #[test]
    fn an_implausible_gap_records_no_sample() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        bar_state_at(&mut m, 10, t0);
        bar_state_at(&mut m, 11, t0 + Duration::from_millis(100));
        bar_state_at(&mut m, 12, t0 + Duration::from_secs(300));
        assert!(m.state.step_history.is_empty());
    }

    #[test]
    fn reported_samples_carry_a_rising_seq_and_say_they_are_reported() {
        let mut st = TrainState::new();
        for i in [900u64, 901, 902] {
            st.apply_event(TrainEvent::StepAndTime {
                step: i,
                loss: 2.0,
                ms: 400.0,
                cache_entries: 8,
            });
        }
        let seqs: Vec<u64> = st.step_history.iter().map(|s| s.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(st.step_time_source, StepTimeSource::Reported);
        assert_eq!(st.step_seq, 3);
    }

    #[test]
    fn a_second_time_line_for_the_same_step_keeps_the_first_seq() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::StepAndTime {
            step: 5,
            loss: 2.0,
            ms: 400.0,
            cache_entries: 8,
        });
        st.apply_event(TrainEvent::StepTime {
            ms: 450.0,
            cache_entries: 8,
        });
        assert_eq!(st.step_history.len(), 1);
        assert_eq!(st.step_history[0].seq, 1);
        assert_eq!(st.step_seq, 1);
    }
}

#[cfg(test)]
mod run_anchor_tests {
    use super::*;
    use std::time::Duration;

    /// Drive one poll's worth of progress: set the parsed step (and loss, as a
    /// real `Step` event does) then run the cadence derivation.
    fn parsed_step_at(m: &mut TrainMonitor, step: u64, now: Instant) {
        m.state.step = step;
        m.state.loss = Some(2.0);
        m.note_step_progress(now);
    }

    /// The anchors are monitor-level, so a replaced `TrainState` does not
    /// clear them; `reset_run_anchors` is what detach and attach both call.
    #[test]
    fn a_fresh_run_after_a_previous_one_is_not_flagged_chunked_and_is_timed() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        // The previous run printed its own times and ended at step 500.
        m.saw_reported_step_time = true;
        m.last_step_seen = Some((500, t0));
        // What detach/attach do: a new state plus the shared reset.
        m.state = TrainState::new();
        m.reset_run_anchors();
        parsed_step_at(&mut m, 1, t0 + Duration::from_secs(1));
        parsed_step_at(&mut m, 2, t0 + Duration::from_millis(1300));
        parsed_step_at(&mut m, 3, t0 + Duration::from_millis(1600));
        assert!(!m.state.chunked_bar, "a new run is not a bar restart");
        assert_eq!(m.state.step_history.len(), 1, "a bar run is timed again");
        assert_eq!(m.state.step_history[0].step, 3);
    }

    /// Attaching during model load: the first polls see the default step 0
    /// (the parser drops the `0/N` bar). The first parsed step only anchors.
    #[test]
    fn the_gap_from_an_unparsed_step_zero_is_never_a_step_time() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        m.note_step_progress(t0); // step is still the default 0, no loss
        m.note_step_progress(t0 + Duration::from_secs(5));
        parsed_step_at(&mut m, 1, t0 + Duration::from_secs(60));
        assert!(m.state.step_history.is_empty(), "load time is not step 1");
        assert_eq!(m.state.step_ms, 0.0);
        parsed_step_at(&mut m, 2, t0 + Duration::from_millis(60_400));
        assert!(
            m.state.step_history.is_empty(),
            "first increase only anchors"
        );
        parsed_step_at(&mut m, 3, t0 + Duration::from_millis(60_800));
        assert_eq!(m.state.step_history.len(), 1);
        assert_eq!(m.state.step_history[0].step, 3);
        assert!((m.state.step_history[0].ms - 400.0).abs() < 1.0);
    }
}

#[cfg(test)]
mod step_and_ms_tests {
    use super::*;

    fn line(step: u64, ms: f32) -> TrainEvent {
        TrainEvent::StepAndMs {
            step,
            loss: 2.0,
            ms,
        }
    }

    #[test]
    fn a_time_with_no_cache_count_is_a_reported_sample_with_unknown_growth() {
        let mut st = TrainState::new();
        st.cache_entries = 7; // an earlier reading must survive
        st.apply_event(line(100, 285.0));
        st.apply_event(line(101, 290.0));
        assert_eq!(st.step, 101);
        assert_eq!(st.step_ms, 290.0);
        assert_eq!(st.cache_entries, 7, "no cache count was reported");
        assert_eq!(st.step_time_source, StepTimeSource::Reported);
        let got: Vec<(u64, u64, f32, u32)> = st
            .step_history
            .iter()
            .map(|s| (s.step, s.seq, s.ms, s.cache_delta))
            .collect();
        assert_eq!(got, vec![(100, 1, 285.0, 0), (101, 2, 290.0, 0)]);
    }

    #[test]
    fn a_trainer_that_prints_time_and_cache_still_measures_growth() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::StepAndTime {
            step: 1,
            loss: 2.0,
            ms: 300.0,
            cache_entries: 8,
        });
        st.apply_event(TrainEvent::StepAndTime {
            step: 2,
            loss: 2.0,
            ms: 300.0,
            cache_entries: 12,
        });
        assert_eq!(st.step_history[1].cache_delta, 4);
    }

    #[test]
    fn every_step_time_shape_counts_as_a_reported_time() {
        assert!(is_reported_step_time(&line(1, 1.0)));
        assert!(is_reported_step_time(&TrainEvent::StepTime {
            ms: 1.0,
            cache_entries: 1
        }));
        assert!(is_reported_step_time(&TrainEvent::StepAndTime {
            step: 1,
            loss: 1.0,
            ms: 1.0,
            cache_entries: 1
        }));
        assert!(!is_reported_step_time(&TrainEvent::Step {
            step: 1,
            loss: 1.0
        }));
        assert!(!is_reported_step_time(&TrainEvent::MaxSteps(5)));
    }

    fn bar(step: u64, max_steps: u64) -> TrainEvent {
        TrainEvent::BarProgress {
            step,
            max_steps,
            loss: 2.0,
        }
    }

    /// A trainer that prints a global `Step: N, ... Time: T ms` line and also
    /// a tqdm bar that restarts every chunk. The bar's restart is seen by the
    /// state itself, the bar's chunk size does not replace the stated budget,
    /// and the bar never moves the step or adds a loss entry.
    #[test]
    fn a_chunked_bar_beside_reported_steps_is_flagged_and_counts_nothing_twice() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::MaxSteps(63906));
        st.apply_event(line(25565, 285.0));
        st.apply_event(bar(630, 3195));
        assert_eq!(st.step, 25565, "the chunk-local bar step is not the step");
        st.apply_event(bar(1, 3195));
        assert!(st.chunked_bar, "the bar restarted");
        assert_eq!(st.max_steps, 63906, "the chunk size is not the budget");
        assert_eq!(st.step, 25565);
        st.apply_event(line(25566, 290.0));
        st.apply_event(bar(2, 3195));
        assert_eq!(st.step, 25566);
        assert_eq!(st.max_steps, 63906);
        assert_eq!(st.loss_history.len(), 2, "one loss entry per step");
        assert_eq!(st.step_history.len(), 2, "one sample per step");
    }

    /// The same, with the budget stated after the bar restart.
    #[test]
    fn a_budget_stated_after_the_bar_restart_still_wins() {
        let mut st = TrainState::new();
        st.apply_event(bar(3195, 3195));
        st.apply_event(bar(1, 3195));
        assert!(st.chunked_bar);
        st.apply_event(TrainEvent::MaxSteps(63906));
        st.apply_event(bar(2, 3195));
        assert_eq!(st.max_steps, 63906);
    }

    /// A trainer that reports progress only through a chunked bar keeps its
    /// old behaviour: the bar sets the step, the loss and the (chunk) total.
    #[test]
    fn a_bar_only_trainer_is_unchanged_apart_from_the_restart_flag() {
        let mut st = TrainState::new();
        for s in [3194u64, 3195, 1, 2] {
            st.apply_event(bar(s, 3195));
        }
        assert!(st.chunked_bar);
        assert_eq!(st.step, 2);
        assert_eq!(st.max_steps, 3195);
        assert_eq!(st.loss_history.len(), 4);
    }

    fn summary(max_steps: u64) -> TrainEvent {
        TrainEvent::HarnessSummary {
            max_steps,
            batch_size: 8,
            seq_len: 512,
        }
    }

    /// A `Max steps` line states the run budget, and a bar counts steps
    /// within a chunk and restarts every chunk. With no run header and no
    /// absolute step line there is no base to rebuild the global step from,
    /// so after the restart the step is chunk-local and no budget or ETA
    /// applies to it. (A tt-tnt run header sets a base of 0; see
    /// `abs_base_tests` for that case.)
    #[test]
    fn a_bar_only_chunked_trainer_with_a_stated_budget_has_no_eta() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::MaxSteps(63906));
        st.apply_event(bar(3195, 3195));
        st.apply_event(bar(630, 3195));
        st.step_ms = 285.0;
        assert!(st.chunked_bar);
        assert_eq!(st.step, 630, "the bar is the step source");
        assert!(st.step_is_chunk_local());
        assert_eq!(st.eta_secs(), None);
    }

    /// Before the first restart, a bar total smaller than the stated budget
    /// already shows the bar counts something smaller than the run, so the
    /// step is chunk-local from the first bar and the display does not
    /// change at the restart. Stated by a `Max steps` line, which sets no
    /// base.
    #[test]
    fn a_bar_smaller_than_the_stated_budget_is_chunk_local_before_any_restart() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::MaxSteps(63906));
        st.apply_event(bar(630, 3195));
        st.step_ms = 285.0;
        assert!(!st.chunked_bar);
        assert!(st.step_is_chunk_local());
        assert_eq!(st.eta_secs(), None);
    }

    /// A bar whose total matches the stated budget (or with no budget
    /// stated) is the run's own counter until it restarts: unchanged.
    #[test]
    fn a_bar_matching_the_budget_keeps_its_total_and_eta() {
        for stated in [None, Some(3195u64)] {
            let mut st = TrainState::new();
            if let Some(v) = stated {
                st.apply_event(summary(v));
            }
            st.apply_event(bar(630, 3195));
            st.step_ms = 285.0;
            assert!(!st.step_is_chunk_local(), "{stated:?}");
            assert_eq!(st.max_steps, 3195);
            assert!(st.eta_secs().is_some(), "{stated:?}");
        }
    }

    /// The split `Full step time ... cache entries` line sets `Reported` but
    /// carries no step or loss. With a bar beside it, the bar stays the step
    /// and loss source.
    #[test]
    fn a_time_only_line_beside_a_bar_leaves_the_bar_as_the_step_source() {
        let mut st = TrainState::new();
        st.apply_event(bar(10, 100));
        st.apply_event(TrainEvent::StepTime {
            ms: 300.0,
            cache_entries: 4,
        });
        st.apply_event(bar(11, 100));
        st.apply_event(TrainEvent::StepTime {
            ms: 300.0,
            cache_entries: 4,
        });
        assert_eq!(st.step_time_source, StepTimeSource::Reported);
        assert_eq!(st.step, 11, "the bar still advances the step");
        assert_eq!(st.loss_history.len(), 2);
        assert_eq!(st.step_history.len(), 2, "one sample per step");
    }

    /// Reported global steps, a chunked bar, and no stated budget: the
    /// bar's total is a chunk size, so no budget is known.
    #[test]
    fn reported_steps_beside_a_chunk_bar_with_no_stated_budget_have_no_budget() {
        let mut st = TrainState::new();
        st.apply_event(line(25565, 285.0));
        st.apply_event(bar(630, 3195));
        assert_eq!(st.max_steps, 0, "a bar total below the step is no budget");
        st.apply_event(bar(1, 3195));
        assert!(st.chunked_bar);
        assert_eq!(st.max_steps, 0);
        assert_eq!(st.step, 25565);
        assert_eq!(st.eta_secs(), None);
    }

    fn resumed(start_step: u64, end_step: u64) -> TrainEvent {
        TrainEvent::Resumed {
            start_step,
            end_step,
        }
    }

    fn val(step: u64) -> TrainEvent {
        TrainEvent::Step { step, loss: 3.0 }
    }

    /// The resume line states the absolute budget and overrides the header's
    /// relative `steps=`, so a later absolute val step stays inside it.
    #[test]
    fn the_resume_line_sets_the_absolute_budget() {
        let mut st = TrainState::new();
        st.apply_event(summary(25560));
        assert_eq!(st.max_steps, 25560);
        st.apply_event(resumed(38346, 63906));
        assert_eq!(st.max_steps, 63906);
        assert_eq!(st.stated_max_steps, 63906);
        assert_eq!(st.resume_start, Some(38346));
        assert_eq!(st.step, 38346, "the run starts at its start step");
        st.apply_event(val(41541));
        assert!(st.step <= st.max_steps);
    }

    /// A miniature of the user's appended two-run log.
    #[test]
    fn a_second_run_header_resets_the_first_runs_data() {
        use crate::workload::train::TrainProcess;
        let mut st = TrainState::new();
        st.proc = Some(TrainProcess {
            pid: 7,
            binary: "python".into(),
            config_path: None,
        });
        st.log = Some(LogSource::NotRedirected);
        st.first_seen = Some(Instant::now());
        st.is_mock = true;
        st.host_cpu_pct = Some(80.0);
        st.host_rss_bytes = Some(1 << 30);
        st.device_backed = true;
        st.config.max_sequence_length = Some(512);
        st.apply_event(summary(63906));
        st.apply_event(resumed(100, 63906));
        for s in [3195u64, 6390, 38340] {
            st.apply_event(TrainEvent::StepAndTime {
                step: s,
                loss: 3.0,
                ms: 285.0,
                cache_entries: 9,
            });
        }
        st.apply_event(bar(5, 3195));
        st.apply_event(bar(2, 3195));
        st.mark_checkpoint();
        st.scheduler = Some("cosine".into());
        st.grad_accum = 4;
        // Every per-run field now holds first-run data.
        assert!(st.step > 0 && st.step_seq > 0 && st.cache_entries > 0);
        assert!(st.last_bar_step.is_some() && st.last_bar_total > 0 && st.chunked_bar);
        assert!(st.step_from_reported_line && st.checkpoint_pulse > 0);
        assert!(st.step_ms > 0.0 && st.loss.is_some() && st.prev_loss.is_some());
        assert_eq!(st.loss_history.len(), 3);

        st.apply_event(summary(25560));
        assert_eq!(st.step, 0);
        assert_eq!(st.loss, None);
        assert_eq!(st.prev_loss, None);
        assert!(st.loss_history.is_empty() && st.step_history.is_empty());
        assert_eq!(st.step_seq, 0);
        assert_eq!(st.step_time_source, StepTimeSource::Unknown);
        assert_eq!(st.step_ms, 0.0);
        assert_eq!(st.cache_entries, 0);
        assert_eq!(st.last_bar_step, None);
        assert_eq!(st.last_bar_total, 0);
        assert!(!st.chunked_bar && !st.step_from_reported_line);
        assert_eq!(st.checkpoint_step, 0);
        assert_eq!(st.checkpoint_pulse, 0);
        assert_eq!(st.resume_start, None);
        assert_eq!(st.scheduler, None);
        assert_eq!(st.grad_accum, 0);
        assert_eq!(st.max_steps, 25560, "the new header's own budget");
        assert_eq!(st.stated_max_steps, 25560);
        // Kept.
        assert_eq!(st.proc.as_ref().map(|p| p.pid), Some(7));
        assert!(st.log.is_some() && st.first_seen.is_some() && st.is_mock);
        assert_eq!(st.host_cpu_pct, Some(80.0));
        assert_eq!(st.host_rss_bytes, Some(1 << 30));
        assert!(st.device_backed);
        assert_eq!(st.config.max_sequence_length, Some(512));

        st.apply_event(resumed(38346, 63906));
        st.apply_event(val(41541));
        assert_eq!(st.max_steps, 63906);
        assert_eq!(st.step, 41541);
        assert_eq!(st.loss_history.len(), 1);
    }

    /// The resume line puts the process at its start step, so a resumed run
    /// does not claim 0% until the first val line. A step already further
    /// along is kept, and no ETA exists until a step time does.
    #[test]
    fn the_resume_line_places_the_run_at_its_start_step() {
        let mut st = TrainState::new();
        st.apply_event(summary(25560));
        st.apply_event(resumed(38346, 63906));
        assert_eq!(st.step, 38346);
        assert_eq!(st.eta_secs(), None);
        st.step_ms = 285.0;
        assert!(st.eta_secs().is_some());
        st.apply_event(val(41541));
        st.apply_event(resumed(100, 63906));
        assert_eq!(st.step, 41541, "never moved backwards");
        // A header after that is a new run and resets, as before.
        st.apply_event(summary(10));
        assert_eq!(st.step, 0);
    }

    /// The start step is a placeholder with no measurement behind it. The monitor does
    /// not anchor its cadence on it, so the first val line only anchors,
    /// the second re-anchors and the third gives a rate.
    #[test]
    fn a_resume_start_step_is_not_a_cadence_anchor() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        m.state.apply_event(summary(25560));
        m.state.apply_event(resumed(38346, 63906));
        m.note_step_progress(t0);
        assert_eq!(m.last_step_seen, None);
        keep_polling(&mut m, t0, t0 + Duration::from_secs(800));
        m.state.apply_event(val(41541));
        m.note_step_progress(t0 + Duration::from_secs(800));
        assert_eq!(m.state.step_ms, 0.0, "one observation is no rate");
        let (a, b) = (
            t0 + Duration::from_secs(800),
            t0 + Duration::from_secs(1600),
        );
        keep_polling(&mut m, a, b);
        m.state.apply_event(val(44736));
        m.note_step_progress(b);
        assert_eq!(m.state.step_ms, 0.0, "the first jump only re-anchors");
        keep_polling(&mut m, b, t0 + Duration::from_secs(2400));
        m.state.apply_event(val(47931));
        m.note_step_progress(t0 + Duration::from_secs(2400));
        assert!((m.state.step_ms - 250.4).abs() < 1.0, "{}", m.state.step_ms);
    }

    /// The first header of a fresh state resets nothing.
    #[test]
    fn the_first_header_resets_nothing() {
        let mut st = TrainState::new();
        st.config.max_sequence_length = Some(512);
        st.scheduler = Some("cosine".into());
        st.grad_accum = 4;
        st.apply_event(summary(100));
        assert_eq!(st.scheduler.as_deref(), Some("cosine"));
        assert_eq!(st.grad_accum, 4);
        assert_eq!(st.max_steps, 100);
    }
}

/// The attach baseline is not a step boundary, so no measurement starts from
/// it. These tests drive `note_step_progress` with injected `Instant`s.
#[cfg(test)]
mod baseline_anchor_tests {
    use super::*;
    use std::time::Duration;

    /// Apply a parsed log line to the state, as `poll` does for a plain event.
    fn feed(m: &mut TrainMonitor, line: &str) {
        let ev = parse_train_line(line).unwrap_or_else(|| panic!("line did not parse: {line}"));
        m.state.apply_event(ev);
    }

    /// A tt-tnt monitor: batch 64, sequence 512, no step time in the log.
    fn tnt_monitor() -> TrainMonitor {
        let mut m = TrainMonitor::new();
        m.state.apply_event(TrainEvent::HarnessSummary {
            max_steps: 63906,
            batch_size: 64,
            seq_len: 512,
        });
        m
    }

    fn val_line(step: u64) -> String {
        format!("  step={step:>7} train_loss=2.5000 val_loss=3.1000")
    }

    fn at_step(m: &mut TrainMonitor, step: u64, now: Instant) {
        m.state.step = step;
        m.state.loss = Some(2.0);
        m.note_step_progress(now);
    }

    /// The reported run: attached at step 60711, the chunk ended 166 s later
    /// and the old code divided 3195 steps by 166 s.
    #[test]
    fn the_first_validation_jump_after_attach_is_never_timed() {
        let mut m = tnt_monitor();
        let t0 = Instant::now();
        m.state.step = 60711;
        m.state.loss = Some(2.0);
        m.note_step_progress(t0);
        assert_eq!(m.state.step_ms, 0.0);

        keep_polling(&mut m, t0, t0 + Duration::from_secs(166));
        feed(&mut m, &val_line(63906));
        assert_eq!(m.state.step, 63906);
        m.note_step_progress(t0 + Duration::from_secs(166));
        assert_eq!(m.state.step_ms, 0.0, "the jump began before the attach");
        assert_eq!(m.state.tokens_per_sec(), None);

        // The next chunk end is measured chunk to chunk: 780 s / 3195.
        keep_polling(
            &mut m,
            t0 + Duration::from_secs(166),
            t0 + Duration::from_secs(166 + 780),
        );
        feed(&mut m, &val_line(67101));
        m.note_step_progress(t0 + Duration::from_secs(166 + 780));
        assert!((m.state.step_ms - 244.1).abs() < 0.5, "{}", m.state.step_ms);
        let tps = m.state.tokens_per_sec().expect("a rate now exists");
        assert!((tps - 134_000.0).abs() < 1_500.0, "{tps}");
    }

    /// A per-step trainer loses one step of cadence after attach, and then
    /// every step is timed from the previous one.
    #[test]
    fn a_per_step_trainer_is_timed_from_the_second_increase() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        at_step(&mut m, 1, t0);
        at_step(&mut m, 2, t0 + Duration::from_millis(300));
        assert!(m.state.step_history.is_empty(), "no sample yet");
        assert_eq!(m.state.step_ms, 0.0);
        at_step(&mut m, 3, t0 + Duration::from_millis(600));
        at_step(&mut m, 4, t0 + Duration::from_millis(900));
        assert_eq!(m.state.step_history.len(), 2);
        for s in &m.state.step_history {
            assert!((s.ms - 300.0).abs() < 1.0, "{}", s.ms);
        }
        assert!((m.state.step_ms - 300.0).abs() < 1.0);
    }

    /// A bar restart is an observed event with a known time, so the restart
    /// poll is a valid start for the next measurement and timing resumes at
    /// once (the restart gap itself records nothing).
    #[test]
    fn a_bar_restart_anchors_as_an_observed_change() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        at_step(&mut m, 100, t0);
        at_step(&mut m, 101, t0 + Duration::from_millis(300));
        assert!(!m.anchor_is_baseline);
        keep_polling(
            &mut m,
            t0 + Duration::from_millis(300),
            t0 + Duration::from_secs(40),
        );
        at_step(&mut m, 1, t0 + Duration::from_secs(40));
        assert!(m.state.chunked_bar);
        assert!(!m.anchor_is_baseline, "the restart is an observed event");
        assert!(
            m.state.step_history.is_empty(),
            "the restart gap is no step"
        );
        at_step(&mut m, 2, t0 + Duration::from_millis(40_300));
        assert_eq!(m.state.step_history.len(), 1);
        assert!((m.state.step_history[0].ms - 300.0).abs() < 1.0);
    }

    /// A new run resets the baseline flag, so its first increase only anchors.
    #[test]
    fn a_new_run_makes_the_next_increase_only_anchor() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        at_step(&mut m, 10, t0);
        at_step(&mut m, 11, t0 + Duration::from_millis(300));
        assert!(!m.anchor_is_baseline);
        m.state = TrainState::new();
        m.reset_run_anchors();
        assert_eq!(m.last_step_seen, None);
        at_step(&mut m, 1, t0 + Duration::from_secs(5));
        assert!(
            m.anchor_is_baseline,
            "the first step of a run is a baseline"
        );
        at_step(&mut m, 2, t0 + Duration::from_millis(5300));
        assert_eq!(m.state.step_ms, 0.0);
        assert!(m.state.step_history.is_empty());
        assert!(!m.anchor_is_baseline);
        at_step(&mut m, 3, t0 + Duration::from_millis(5600));
        assert_eq!(m.state.step_history.len(), 1);
    }

    /// A trainer that reports its own time never reaches the derivation.
    #[test]
    fn a_reported_step_time_ignores_the_baseline_rule() {
        let mut m = TrainMonitor::new();
        m.saw_reported_step_time = true;
        m.state.step_ms = 1124.5;
        let t0 = Instant::now();
        for i in 1..=4u64 {
            at_step(&mut m, i, t0 + Duration::from_millis(300 * i));
        }
        assert_eq!(m.state.step_ms, 1124.5);
        assert!(m.state.step_history.is_empty());
        assert_eq!(m.last_step_seen, None, "the derivation never ran");
    }

    /// For every jump size and attach gap, the first increase measures
    /// nothing and the second equals its own gap divided by its own jump.
    #[test]
    fn the_first_increase_never_inflates_the_rate_for_any_jump_or_gap() {
        for &jump in &[1u64, 10, 3195] {
            for &gap1 in &[1u64, 5, 30, 166, 400, 1000] {
                let mut m = TrainMonitor::new();
                let t0 = Instant::now();
                at_step(&mut m, 1000, t0);
                keep_polling(&mut m, t0, t0 + Duration::from_secs(gap1));
                at_step(&mut m, 1000 + jump, t0 + Duration::from_secs(gap1));
                assert_eq!(m.state.step_ms, 0.0, "jump {jump} gap {gap1}");
                assert!(m.state.step_history.is_empty(), "jump {jump} gap {gap1}");
                // A second gap long enough for a plausible per-step time.
                let gap2_ms = jump * 250;
                keep_polling(
                    &mut m,
                    t0 + Duration::from_secs(gap1),
                    t0 + Duration::from_secs(gap1) + Duration::from_millis(gap2_ms),
                );
                at_step(
                    &mut m,
                    1000 + 2 * jump,
                    t0 + Duration::from_secs(gap1) + Duration::from_millis(gap2_ms),
                );
                let want = gap2_ms as f32 / jump as f32;
                assert!(
                    (m.state.step_ms - want).abs() < 0.5,
                    "jump {jump} gap {gap1}: {} vs {want}",
                    m.state.step_ms
                );
            }
        }
    }

    fn secs(t0: Instant, s: f32) -> Instant {
        t0 + Duration::from_secs_f32(s)
    }

    /// Chunks of 13 minutes (780 s, 3195 steps). The viewer attaches, leaves,
    /// a chunk ends while it is away, and it returns. All times in minutes.
    #[test]
    fn returning_to_the_view_never_times_a_chunk_from_a_stale_anchor() {
        let min = 60.0;
        let mut m = tnt_monitor();
        let t0 = Instant::now();
        at_step(&mut m, 60711, t0); // attach, baseline
        keep_polling(&mut m, t0, secs(t0, 1.0 * min)); // polled until leaving
                                                       // The chunk ends at 5 min, unseen. The viewer returns at 12 min.
        at_step(&mut m, 63906, secs(t0, 12.0 * min));
        assert_eq!(m.state.step_ms, 0.0, "the return poll only re-anchors");
        // Polling continues. The chunk end at 18 min is the first observed
        // change after the re-baseline, so it only anchors.
        keep_polling(&mut m, secs(t0, 12.0 * min), secs(t0, 18.0 * min));
        at_step(&mut m, 67101, secs(t0, 18.0 * min));
        assert_eq!(m.state.step_ms, 0.0, "6 min / 3195 would be 113 ms");
        // The next jump is timed chunk to chunk.
        keep_polling(&mut m, secs(t0, 18.0 * min), secs(t0, 31.0 * min));
        at_step(&mut m, 70296, secs(t0, 31.0 * min));
        assert!((m.state.step_ms - 244.1).abs() < 0.5, "{}", m.state.step_ms);
    }

    /// Returning 6 s before a chunk end would otherwise time 3195 steps over
    /// 6 s (1.9 ms each), which passes the plausibility range.
    #[test]
    fn returning_just_before_a_chunk_end_produces_no_value() {
        let min = 60.0;
        let mut m = tnt_monitor();
        let t0 = Instant::now();
        at_step(&mut m, 60711, t0);
        keep_polling(&mut m, t0, secs(t0, 1.0 * min));
        // The chunk ended at 5 min, unseen. The return poll at 17.9 min sees
        // 63906, and the next chunk ends 6 s later.
        at_step(&mut m, 63906, secs(t0, 17.9 * min));
        keep_polling(&mut m, secs(t0, 17.9 * min), secs(t0, 17.9 * min + 6.0));
        at_step(&mut m, 67101, secs(t0, 17.9 * min + 6.0));
        assert_eq!(m.state.step_ms, 0.0);
        assert_eq!(m.state.tokens_per_sec(), None);
    }

    /// A pause under the limit changes nothing.
    #[test]
    fn a_short_pause_is_still_timed() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        at_step(&mut m, 1, t0);
        at_step(&mut m, 2, secs(t0, 0.3));
        at_step(&mut m, 3, secs(t0, 0.6));
        // 4 s without a poll, under the 5 s limit.
        at_step(&mut m, 4, secs(t0, 4.6));
        assert_eq!(m.state.step_history.len(), 2);
        assert!((m.state.step_history[1].ms - 4000.0).abs() < 1.0);
    }

    /// A per-step trainer that pauses for 10 s and carries on: the first
    /// increase after the pause only anchors.
    #[test]
    fn a_per_step_trainer_re_anchors_after_a_pause() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        at_step(&mut m, 1, t0);
        at_step(&mut m, 2, secs(t0, 0.3));
        at_step(&mut m, 3, secs(t0, 0.6));
        assert_eq!(m.state.step_history.len(), 1);
        at_step(&mut m, 40, secs(t0, 10.6)); // returns, 37 steps later
        assert_eq!(m.state.step_history.len(), 1, "no sample for the return");
        at_step(&mut m, 41, secs(t0, 10.9));
        assert_eq!(m.state.step_history.len(), 1, "first increase only anchors");
        at_step(&mut m, 42, secs(t0, 11.2));
        assert_eq!(m.state.step_history.len(), 2);
        assert!((m.state.step_history[1].ms - 300.0).abs() < 1.0);
    }

    /// Frame-rate polling, 50 ms apart, never trips the pause rule.
    #[test]
    fn polls_50ms_apart_are_unaffected() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        // A step every 300 ms, a poll every 50 ms for 20 s.
        for i in 0..400u64 {
            at_step(&mut m, 1 + i / 6, t0 + Duration::from_millis(50 * i));
        }
        assert!(m.state.step_history.len() > 50);
        assert!(
            (m.state.step_ms - 300.0).abs() < 60.0,
            "{}",
            m.state.step_ms
        );
    }

    /// A second run header in the log clears every monitor anchor, including
    /// the pause clock. The test process is alive, so `poll` does not detach.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_second_run_header_in_poll_clears_the_anchors() {
        use crate::workload::train::TrainProcess;
        let dir = std::env::temp_dir().join(format!("ttanchor_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("train.log");
        std::fs::write(
            &path,
            "tt-tnt training - steps=4000 batch=8 seq_len=512 arch=blackhole\n",
        )
        .unwrap();
        let mut m = TrainMonitor::new();
        m.state.proc = Some(TrainProcess {
            pid: std::process::id() as i32,
            binary: "test".into(),
            config_path: None,
        });
        m.tailer = Some(Tailer::new(path.clone()));
        // Run data from an earlier run, and live anchors.
        m.state.step = 100;
        m.state.loss = Some(2.0);
        let t0 = Instant::now();
        m.last_step_seen = Some((100, t0));
        m.anchor_is_baseline = true;
        m.last_note_at = Some(t0);
        m.poll();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(m.last_step_seen, None);
        assert!(!m.anchor_is_baseline);
        assert_eq!(m.last_note_at, None);
    }
}

/// Test helper: one tt-tnt bar frame in the real shape (see the verbatim
/// frames in `parse::tests::parses_tt_tnt_bar_frames_that_carry_train_loss`),
/// at chunk-local step `local` of a `total`-step chunk.
#[cfg(test)]
pub(crate) fn tnt_frame(local: u64, total: u64, loss: f32) -> String {
    let pct = local * 100 / total.max(1);
    format!(
        "{pct:>3}%|\u{2588}         | {local}/{total} [00:19<30:41,  3.43it/s, train_loss={loss:.4}, val_loss={loss:.4}]"
    )
}

#[cfg(test)]
impl TrainMonitor {
    /// Test helper: one poll at an injected time. Folds `lines` into the state
    /// through the same path `poll` uses, then runs the cadence check.
    pub(crate) fn replay_poll(&mut self, lines: &[String], now: Instant) {
        self.ingest_lines(lines.to_vec());
        self.note_step_progress(now);
    }
}

/// The run's global step rebuilt from a chunk-local bar (`abs_base`), and the
/// cadence guard at the chunk boundary.
#[cfg(test)]
mod abs_base_tests {
    use super::*;
    use std::time::Duration;

    fn header(steps: u64) -> TrainEvent {
        parse_train_line(&format!(
            "tt-tnt training \u{2014} steps={steps} batch=64 seq_len=512 arch=blackhole"
        ))
        .expect("the header parses")
    }

    fn resume(start: u64, more: u64) -> TrainEvent {
        parse_train_line(&format!(
            "  resumed from artifacts/x/tt_tnt_step{start:08}.pkl at step {start} (created_at=2026-10-02T12:00:00+00:00); running {more} more steps to step {}",
            start + more
        ))
        .expect("the resume line parses")
    }

    fn frame(local: u64) -> TrainEvent {
        parse_train_line(&tnt_frame(local, 6391, 3.1)).expect("the bar frame parses")
    }

    fn val(step: u64) -> TrainEvent {
        parse_train_line(&format!(
            "  step={step:>7} train_loss=2.9570 val_loss=3.1508 lr=5.274e-05"
        ))
        .expect("the val line parses")
    }

    #[test]
    fn a_run_header_sets_the_base_to_zero() {
        let mut st = TrainState::new();
        assert_eq!(st.abs_base, None);
        st.apply_event(header(31951));
        assert_eq!(st.abs_base, Some(0));
    }

    #[test]
    fn a_resume_line_overrides_the_headers_base() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(resume(31955, 31951));
        assert_eq!(st.abs_base, Some(31955));
        assert_eq!(st.step, 31955);
        assert_eq!(st.max_steps, 63906);
    }

    #[test]
    fn an_absolute_step_line_sets_the_base() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(val(38346));
        assert_eq!(st.abs_base, Some(38346));
        let mut st = TrainState::new();
        st.apply_event(parse_train_line("Step: 25565, Loss: 3.1367, Time: 285.0 ms").unwrap());
        assert_eq!(st.abs_base, Some(25565));
        let mut st = TrainState::new();
        st.apply_event(parse_train_line("Step: 7 Loss: 0.4213").unwrap());
        assert_eq!(st.abs_base, Some(7));
    }

    /// The bar's step is chunk-local, so it never becomes the base itself.
    #[test]
    fn a_bar_frame_never_sets_the_base() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        for local in [1u64, 2, 3, 200] {
            st.apply_event(frame(local));
            assert_eq!(st.abs_base, Some(0), "after frame {local}");
        }
        assert_eq!(st.step, 200, "a fresh run's base is 0");
    }

    #[test]
    fn a_new_run_clears_the_base() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(resume(31955, 31951));
        st.apply_event(frame(10));
        st.begin_new_run();
        assert_eq!(st.abs_base, None);
        // A second header after run data resets, then states its own base.
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(resume(31955, 31951));
        st.apply_event(frame(10));
        st.apply_event(header(4000));
        assert_eq!(st.abs_base, Some(0));
        st.apply_event(frame(3));
        assert_eq!(st.step, 3);
    }

    /// A trainer with no header and no absolute line keeps the bar's own step.
    #[test]
    fn with_no_base_the_bar_step_is_used_as_it_is() {
        let mut st = TrainState::new();
        st.apply_event(frame(200));
        assert_eq!(st.abs_base, None);
        assert_eq!(st.step, 200);
        assert_eq!(st.max_steps, 6391);
    }

    #[test]
    fn a_resumed_run_shows_its_global_step_and_keeps_its_budget() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(resume(31955, 31951));
        st.apply_event(frame(200));
        assert_eq!(st.step, 32155);
        assert_eq!(st.max_steps, 63906, "the chunk size is not the budget");
        assert_eq!(st.stated_max_steps, 63906);
        assert!(!st.step_is_chunk_local());
        st.step_ms = 300.0;
        let eta = st.eta_secs().expect("a global step has an ETA");
        assert!((eta - (63906 - 32155) as f32 * 0.3).abs() < 1.0, "{eta}");
    }

    #[test]
    fn a_fresh_run_counts_globally_across_chunks() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(frame(6390));
        st.apply_event(frame(6391));
        assert_eq!(st.step, 6391);
        st.apply_event(val(6391));
        st.apply_event(frame(1));
        assert_eq!(
            st.step, 6392,
            "the next chunk starts on top of the val line"
        );
        assert_eq!(st.max_steps, 31951);
        assert!(!st.step_is_chunk_local());
        assert!(st.chunked_bar, "the bar still restarted");
    }

    /// The harness's absolute line is authoritative even when it disagrees
    /// with the base plus the bar's step.
    #[test]
    fn an_absolute_line_that_disagrees_with_the_bar_wins() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(resume(31955, 31951));
        st.apply_event(frame(6391));
        assert_eq!(st.step, 38346);
        st.apply_event(val(40000));
        assert_eq!(st.step, 40000);
        st.apply_event(frame(1));
        assert_eq!(st.step, 40001);
    }

    /// tt-tnt ends a chunk at a checkpoint boundary too, which prints no
    /// validation line when `--save-every` is shorter than `--val-every`. A
    /// restart with no absolute line before it adds the finished chunk.
    #[test]
    fn a_restart_with_no_absolute_line_adds_the_finished_chunk() {
        let mut st = TrainState::new();
        st.apply_event(header(31951));
        st.apply_event(frame(3000));
        st.apply_event(frame(3195));
        st.apply_event(frame(1));
        assert_eq!(st.abs_base, Some(3195));
        assert_eq!(st.step, 3196);
        assert!(st.cadence_rebase, "the boundary gap is never timed");
    }

    /// Without a base the old chunk-local rule is unchanged: a stated budget
    /// with no header (a `Max steps` line) and a smaller bar total.
    #[test]
    fn without_a_base_the_chunk_local_rule_is_unchanged() {
        let mut st = TrainState::new();
        st.apply_event(TrainEvent::MaxSteps(63906));
        st.apply_event(frame(630));
        assert_eq!(st.abs_base, None);
        assert!(st.step_is_chunk_local());
        assert_eq!(st.max_steps, 63906);
    }

    /// The validation line ends a chunk. Checkpoint save, validation and the
    /// first step's warm-up fall between the chunk's last frame and the next
    /// chunk's first, so the first step after the boundary is never timed.
    #[test]
    fn the_chunk_boundary_gap_is_never_timed() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        m.replay_poll(
            &[
                "tt-tnt training \u{2014} steps=31951 batch=64 seq_len=512 arch=blackhole".into(),
                tnt_frame(6388, 6391, 3.1),
            ],
            ms(0),
        );
        for (i, local) in (6389..=6391u64).enumerate() {
            m.replay_poll(&[tnt_frame(local, 6391, 3.1)], ms(300 * (i as u64 + 1)));
        }
        let samples = m.state.step_history.len();
        assert!(samples >= 1, "the chunk's last steps are timed");
        // 60 s of checkpoint and validation, polled every second.
        keep_polling(&mut m, ms(900), ms(60_900));
        m.replay_poll(
            &["  step=   6391 train_loss=2.9570 val_loss=3.1508 lr=5.274e-05".into()],
            ms(60_900),
        );
        assert!(m.anchor_is_baseline, "the val line re-baselines");
        // The first frame of the next chunk arrives after the warm-up.
        m.replay_poll(&[tnt_frame(1, 6391, 3.0)], ms(62_000));
        assert_eq!(m.state.step, 6392, "global: one more than the last frame");
        assert!(m.state.chunked_bar, "the bar restarted");
        assert!(!m.state.step_is_chunk_local());
        assert_eq!(m.state.step_history.len(), samples, "no sample for the gap");
        assert!(m.state.step_ms < 400.0, "no spike: {}", m.state.step_ms);
        // The first step after the boundary only re-anchors; the next two are
        // timed from their own gaps.
        m.replay_poll(&[tnt_frame(2, 6391, 3.0)], ms(62_300));
        m.replay_poll(&[tnt_frame(3, 6391, 3.0)], ms(62_600));
        assert_eq!(m.state.step_history.len(), samples + 2);
        let last = m.state.step_history.last().unwrap();
        assert_eq!(last.step, 6394);
        assert!((last.ms - 300.0).abs() < 1.0, "{}", last.ms);
        assert!(
            m.state.step_history.iter().all(|s| s.ms < 400.0),
            "no spike in the history"
        );
    }

    /// The same guard for a restart with no validation line in front of it.
    #[test]
    fn a_restart_with_no_val_line_is_never_timed_either() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        m.replay_poll(
            &[
                "tt-tnt training \u{2014} steps=31951 batch=64 seq_len=512 arch=blackhole".into(),
                tnt_frame(3192, 3195, 3.1),
            ],
            ms(0),
        );
        for (i, local) in (3193..=3195u64).enumerate() {
            m.replay_poll(&[tnt_frame(local, 3195, 3.1)], ms(300 * (i as u64 + 1)));
        }
        let samples = m.state.step_history.len();
        keep_polling(&mut m, ms(900), ms(20_900));
        m.replay_poll(&[tnt_frame(1, 3195, 3.0)], ms(20_900));
        assert_eq!(m.state.step, 3196);
        m.replay_poll(&[tnt_frame(2, 3195, 3.0)], ms(21_200));
        assert_eq!(m.state.step_history.len(), samples, "no sample for the gap");
        assert!(m.state.step_ms < 400.0, "no spike: {}", m.state.step_ms);
    }

    /// A trainer that prints a plain `Step:` line every step, with no bar,
    /// keeps its cadence: an absolute line re-baselines only beside a bar.
    #[test]
    fn a_per_step_line_trainer_with_no_bar_is_still_timed() {
        let mut m = TrainMonitor::new();
        let t0 = Instant::now();
        for i in 1..=4u64 {
            m.replay_poll(
                &[format!("Step: {i} Loss: 0.4213")],
                t0 + Duration::from_millis(300 * i),
            );
        }
        assert_eq!(m.state.step_history.len(), 2);
        assert!((m.state.step_ms - 300.0).abs() < 1.0, "{}", m.state.step_ms);
    }
}
