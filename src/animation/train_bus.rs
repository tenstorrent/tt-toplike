// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What the Training view draws from the signal bus: a training run seen
//! through the world, a pulse when a step lands, the sky's pace from the step
//! time, a mark on the chips the run uses, and a strip naming a model that
//! serves on the same chips.
//!
//! # Two steps
//!
//! The shape is Die 3D's overlay (`die3d::overlay`) and the arcade's
//! (`arcade_bus`):
//!
//! 1. [`TrainFx::from_world`] runs in `TrainView::set_bus`, once per view
//!    update. It reads the world through [`WorldRead`], compares it with what
//!    the view saw last ([`TrainBusState`]) and returns a [`TrainFx`]. All the
//!    reading, matching and bookkeeping happens here, including the run's
//!    history of successive reads.
//! 2. `TrainView::render` (the frame path) draws the `TrainFx` with
//!    arithmetic only. It never reads the world.
//!
//! An empty world (no training run, no model) gives an empty `TrainFx` and no
//! run state, and the view then draws exactly what it drew before the bus.
//!
//! # The run
//!
//! The world names a training run `run:<binary>:<pid>@<start>`, or `run:local`
//! when only a log is known (`TrainingSource` in `bus::workload_sources`). The
//! view shows the run whose `toplike.train.step` reading is the newest, as
//! long as that reading is at most [`RUN_STALE_MS`] old.
//!
//! The world keeps only the latest reading of each signal, so this module
//! keeps the run's history itself: each read in which the step went up adds
//! one loss point and, when a step time is known, one step sample. Several
//! steps that land between two reads give one point, carrying the latest
//! loss and step time. The history becomes a `TrainState` (see
//! [`TrainBusState::world_state`]) that the view draws with its usual look
//! when this machine has no trainer of its own, for example when the TUI is
//! attached to a headless host that sees one. A local trainer always wins:
//! its state has more in it (the log path, the config, checkpoints). The
//! effects below describe the bus run, so a local trainer's view draws them
//! only when the bus run is that trainer ([`TrainBusState::is_run_of`]).
//!
//! # What drives what
//!
//! * **Pulses.** A read in which the step went up is a step landing. It draws
//!   a pulse on the newest columns of the step band. When the loss fell by
//!   more than [`LOSS_PULSE_FRAC`] of its last value the pulse is brighter
//!   (mint); when it rose by more than that it is dimmer (coral); otherwise it
//!   is a plain step pulse in the training amber. The pulse's width follows
//!   the measured size of the loss change, up to [`MAX_PULSE_COLS`] columns.
//! * **Pace.** A fresh `toplike.train.step_ms` reading sets how fast the sky
//!   moves (aurora drift and star twinkle): faster for short steps, slower
//!   for long ones, between half and twice the usual speed. With no reading
//!   the sky moves as before.
//! * **The run's chips.** `toplike.train.chips` names the run's chips. The
//!   ones this TUI has get a `▸` before their tag in the CHIPS row. Chips this
//!   TUI does not have (an attached host's) are never drawn.
//! * **Neighbours.** A model announced on one of the run's chips (and not
//!   `Down`) is named in a `SAME CHIPS` strip under the CHIPS row, with its
//!   phase and tokens per second, in its phase colour, so the two workloads
//!   on one board show together.
//!
//! A step already in the world at the first read is history and makes no
//! pulse. So does a step that landed while the view was away (off screen or
//! another mode shown): an update more than `PULSE_NS` after the last one,
//! and more than four times the usual gap between updates, records the step
//! and draws nothing for it. This is Die 3D's rule, with its constants.
//!
//! # Provenance
//!
//! The run's numbers are what its source measured, except a step time timed
//! from a progress bar, which the source marks `Derived` and the view's title
//! discloses as "from bar". A run whose readings are `Estimated` (a mock
//! backend) or `Decoration` gets `~` in the view's title (`TRAINING · ~bus`),
//! and a neighbour model's text gets `~` through `model_text_is_estimated`.
//! The pulse's shape, colour and fade, and the mapping from step time to sky
//! speed, are decoration. The pulse's width is derived from the measured loss
//! change.

use crate::animation::die3d::overlay::{flag_of, ModelFlag, MAX_PULSES, PULSE_NS};
use crate::bus::signals as sig;
use crate::workload::train::{
    StepSample, StepTimeSource, TrainProcess, TrainState, LOSS_HISTORY, STEP_HISTORY,
};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use tt_toplike_bus::{EntityKey, EntityKind, ModelPhase, Provenance, Value, WorldRead};

/// A run whose step reading is older than this is not shown. The host feeds
/// the run once per process scan (every 2 s), so a live run is never near it.
pub const RUN_STALE_MS: u64 = 30_000;
/// A loss change larger than this fraction of the last loss makes a brighter
/// (fall) or dimmer (rise) pulse.
pub const LOSS_PULSE_FRAC: f32 = 0.01;
/// The widest pulse, in data columns.
pub const MAX_PULSE_COLS: usize = 6;
/// Pulse columns per unit of relative loss change: one column per percent.
const PULSE_COLS_PER_FRAC: f32 = 100.0;
/// The step time at which the sky moves at its usual speed, in ms. Shorter
/// steps make it faster, longer ones slower. Decoration.
pub const PACE_REF_MS: f32 = 500.0;
/// The slowest and fastest sky, as a multiple of its usual speed.
pub const PACE_MIN: f32 = 0.5;
pub const PACE_MAX: f32 = 2.0;

/// What a step landing looked like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PulseKind {
    /// The loss moved by less than [`LOSS_PULSE_FRAC`], or is unknown.
    Step,
    /// The loss fell by more than [`LOSS_PULSE_FRAC`].
    LossFall,
    /// The loss rose by more than [`LOSS_PULSE_FRAC`].
    LossRise,
}

/// One step landing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepPulse {
    pub start_ns: u64,
    pub kind: PulseKind,
    /// The loss change as a fraction of the last loss, 0 for a plain step.
    pub size: f32,
}

impl StepPulse {
    /// How many data columns the pulse covers: 1 for a plain step, one more
    /// per percent of loss change, up to [`MAX_PULSE_COLS`].
    pub fn cols(&self) -> usize {
        match self.kind {
            PulseKind::Step => 1,
            _ => (1 + (self.size * PULSE_COLS_PER_FRAC).round() as usize).min(MAX_PULSE_COLS),
        }
    }
}

/// A model announced on one of the run's chips.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    pub flag: ModelFlag,
    /// The flag's text forms (`ModelFlag::texts`), longest first, worked out
    /// here so the frame path builds no strings for them.
    pub texts: Vec<String>,
    /// The device indexes of the chips it shares with the run.
    pub devices: Vec<usize>,
    /// ` dev0,1`: the shared chips, worked out here for the same reason.
    pub devices_text: String,
}

/// What the bus puts on the Training view. See the module doc.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrainFx {
    /// Step landings still fading, oldest first.
    pub pulses: Vec<StepPulse>,
    /// The run's fresh step time in ms, which sets the sky's pace.
    pub pace_ms: Option<f32>,
    /// The device indexes of the run's chips that this TUI has.
    pub run_devices: Vec<usize>,
    /// Models on the run's chips, in key order.
    pub neighbours: Vec<Neighbour>,
}

impl TrainFx {
    /// Whether nothing is drawn from the bus.
    pub fn is_empty(&self) -> bool {
        self.pulses.is_empty()
            && self.pace_ms.is_none()
            && self.run_devices.is_empty()
            && self.neighbours.is_empty()
    }

    /// How fast the sky moves, as a multiple of its usual speed: 1 with no
    /// step time. Decoration.
    pub fn sky_pace(&self) -> f32 {
        match self.pace_ms {
            Some(ms) if ms.is_finite() && ms > 0.0 => (PACE_REF_MS / ms).clamp(PACE_MIN, PACE_MAX),
            _ => 1.0,
        }
    }
}

/// What the view remembers between two [`TrainFx::from_world`] calls. Start
/// with the default; set a new default when the view stops reading a world,
/// so the next world is a first sight.
#[derive(Debug, Clone, Default)]
pub struct TrainBusState {
    /// Bus time of the last update.
    last_now: Option<u64>,
    /// The time between the last two updates.
    last_gap: Option<u64>,
    /// The run shown, by key.
    run: Option<EntityKey>,
    /// The step of the last read.
    last_step: Option<u64>,
    /// The run as the view draws it, built from successive reads.
    state: Option<TrainState>,
    pulses: Vec<StepPulse>,
}

impl TrainBusState {
    /// The run seen through the world as a `TrainState`, or `None` when the
    /// world has no fresh run. See the module doc.
    pub fn world_state(&self) -> Option<&TrainState> {
        self.state.as_ref()
    }

    /// Whether the run shown from the bus is the trainer `local`: the same
    /// pid and start time, as `TrainingSource` names it in the run key
    /// (`run:<binary>:<pid>@<start>`, with 0 for an unknown start). No run on
    /// the bus is no match.
    pub fn is_run_of(&self, local: &TrainProcess) -> bool {
        self.run.as_ref().is_some_and(|k| {
            let p = proc_of_key(k);
            p.pid == local.pid
                && p.pid != 0
                && p.start_time.unwrap_or(0) == local.start_time.unwrap_or(0)
        })
    }
}

/// A reading's value as a number.
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) => Some(*x),
        Value::U64(x) => Some(*x as f64),
        Value::I64(x) => Some(*x as f64),
        Value::Text(_) => None,
    }
}

/// Every training-run signal the view shows.
const RUN_SIGNALS: [&str; 7] = [
    sig::TRAIN_STEP,
    sig::TRAIN_MAX_STEPS,
    sig::TRAIN_LOSS,
    sig::TRAIN_STEP_MS,
    sig::TRAIN_CPU_PCT,
    sig::TRAIN_RSS_BYTES,
    sig::TRAIN_CHIPS,
];

/// Whether any of the run's readings is `Estimated` or `Decoration`. The
/// training counterpart of `model_text_is_estimated`: the view's title then
/// carries `~`.
pub(crate) fn run_text_is_estimated(world: &dyn WorldRead, run: &EntityKey) -> bool {
    RUN_SIGNALS
        .iter()
        .filter_map(|s| world.reading(run, s))
        .any(|r| matches!(r.prov, Provenance::Estimated | Provenance::Decoration))
}

/// The trainer process a run key names: `run:<binary>:<pid>@<start>`. A key
/// without a pid (`run:local`) gives the binary `training` and pid 0.
pub fn proc_of_key(run: &EntityKey) -> TrainProcess {
    let id = run.as_str().strip_prefix("run:").unwrap_or(run.as_str());
    let parsed = id.rsplit_once(':').and_then(|(bin, rest)| {
        let (pid, start) = rest.split_once('@').unwrap_or((rest, ""));
        Some((bin, pid.parse::<i32>().ok()?, start.parse::<u64>().ok()))
    });
    match parsed {
        Some((bin, pid, start)) if !bin.is_empty() => TrainProcess {
            pid,
            binary: bin.to_string(),
            config_path: None,
            start_time: start,
        },
        _ => TrainProcess {
            pid: 0,
            binary: "training".to_string(),
            config_path: None,
            start_time: None,
        },
    }
}

/// The run the view shows: the run whose `toplike.train.step` reading is
/// the newest, as long as that reading is at most [`RUN_STALE_MS`] old at
/// `now_mono_ns`. Gives the run's key and its step.
fn newest_run(world: &dyn WorldRead, now_mono_ns: u64) -> Option<(EntityKey, u64)> {
    world
        .entities(EntityKind::TrainingRun)
        .into_iter()
        .filter_map(|k| {
            let r = world.reading(&k, sig::TRAIN_STEP)?;
            let step = number(&r.value)?;
            (world.age_ms(&r, now_mono_ns) <= RUN_STALE_MS && step >= 0.0).then_some((
                r.seq,
                k,
                step as u64,
            ))
        })
        .max_by_key(|(seq, _, _)| *seq)
        .map(|(_, k, step)| (k, step))
}

/// Whether `world` holds a run the Training view would draw, at its own
/// clock. The `v` rotation asks this while another view is on screen, so an
/// attached TUI whose host sees a trainer still visits Training. It is the
/// same choice [`TrainFx::from_world`] makes, without the bookkeeping.
pub(crate) fn run_on_bus(world: &dyn WorldRead) -> bool {
    newest_run(world, world.now_mono_ns()).is_some()
}

impl TrainFx {
    /// The effects for the chips `chips` (each a device index and its chip
    /// key, `EntityKey::chip(bus_id)`) from `world` at bus time
    /// `now_mono_ns`, updating `prev`. See the module doc.
    pub fn from_world(
        world: &dyn WorldRead,
        chips: &[(usize, EntityKey)],
        now_mono_ns: u64,
        prev: &mut TrainBusState,
    ) -> TrainFx {
        let now = now_mono_ns;
        // Die 3D's away rule: an update long after the last one, and well
        // past the usual gap, records what it finds and pulses for nothing.
        let gap = prev.last_now.map(|t| now.saturating_sub(t));
        let away = gap.is_some_and(|g| {
            g > PULSE_NS
                && prev
                    .last_gap
                    .is_none_or(|usual| g > usual.saturating_mul(4))
        });
        prev.last_now = Some(now);
        if gap.is_some() {
            prev.last_gap = gap;
        }
        prev.pulses
            .retain(|p| now.saturating_sub(p.start_ns) < PULSE_NS);

        let Some((key, step)) = newest_run(world, now) else {
            // No run: nothing is kept.
            prev.run = None;
            prev.last_step = None;
            prev.state = None;
            prev.pulses.clear();
            return TrainFx::default();
        };

        let read = |s: &str| world.reading(&key, s);
        let read_num = |s: &str| read(s).and_then(|r| number(&r.value));
        let loss = read_num(sig::TRAIN_LOSS)
            .filter(|l| l.is_finite())
            .map(|l| l as f32);
        let step_ms_reading =
            read(sig::TRAIN_STEP_MS).filter(|r| world.age_ms(r, now) <= RUN_STALE_MS);
        let step_ms = step_ms_reading
            .as_ref()
            .and_then(|r| number(&r.value))
            .filter(|m| m.is_finite() && *m > 0.0)
            .map(|m| m as f32);

        // A new run, or the step going back (the same key restarted), is a
        // first sight: the history starts again and nothing pulses.
        let first = prev.run.as_ref() != Some(&key)
            || prev.state.is_none()
            || prev.last_step.is_some_and(|s| step < s);
        if first {
            prev.run = Some(key.clone());
            prev.last_step = None;
            prev.pulses.clear();
            let mut fresh = TrainState::new();
            fresh.proc = Some(proc_of_key(&key));
            fresh.seen_on_bus = true;
            prev.state = Some(fresh);
        }
        let st = prev
            .state
            .as_mut()
            .expect("the run state was just set for a first sight");

        let landed = prev.last_step.is_none_or(|s| step > s);
        if landed {
            let old = st.loss;
            if let Some(l) = loss {
                if old.is_some() {
                    st.prev_loss = old;
                }
                st.loss = Some(l);
                st.loss_history.push(l);
                if st.loss_history.len() > LOSS_HISTORY {
                    st.loss_history.remove(0);
                }
            }
            if let Some(ms) = step_ms {
                st.step_seq += 1;
                st.step_history.push(StepSample {
                    step,
                    seq: st.step_seq,
                    ms,
                    cache_delta: 0,
                    checkpoint: false,
                });
                if st.step_history.len() > STEP_HISTORY {
                    st.step_history.remove(0);
                }
            }
            if !first && !away {
                let kind_size = match (old, loss) {
                    (Some(o), Some(n)) if o.abs() > f32::EPSILON => {
                        let rel = (n - o) / o.abs();
                        if rel <= -LOSS_PULSE_FRAC {
                            (PulseKind::LossFall, -rel)
                        } else if rel >= LOSS_PULSE_FRAC {
                            (PulseKind::LossRise, rel)
                        } else {
                            (PulseKind::Step, 0.0)
                        }
                    }
                    _ => (PulseKind::Step, 0.0),
                };
                add_pulse(&mut prev.pulses, now, kind_size.0, kind_size.1);
            }
            prev.last_step = Some(step);
        }

        // The latest readings, every read.
        st.step = step;
        st.max_steps = read_num(sig::TRAIN_MAX_STEPS)
            .filter(|m| *m >= 0.0)
            .map_or(0, |m| m as u64);
        if let Some(ms) = step_ms {
            st.step_ms = ms;
            st.step_time_source = match step_ms_reading.as_ref().map(|r| r.prov) {
                Some(Provenance::Derived) => StepTimeSource::Observed,
                _ => StepTimeSource::Reported,
            };
        }
        st.host_cpu_pct = read_num(sig::TRAIN_CPU_PCT)
            .filter(|v| v.is_finite())
            .map(|v| v as f32);
        st.host_rss_bytes = read_num(sig::TRAIN_RSS_BYTES)
            .filter(|v| *v >= 0.0)
            .map(|v| v as u64);
        st.is_mock = run_text_is_estimated(world, &key);

        // The run's chips that this TUI has, in device order.
        let run_chips: Vec<&(usize, EntityKey)> = match read(sig::TRAIN_CHIPS).map(|r| r.value) {
            Some(Value::Text(t)) => {
                let named: Vec<&str> = t.split(',').map(str::trim).collect();
                chips
                    .iter()
                    .filter(|(_, k)| named.contains(&k.as_str()))
                    .collect()
            }
            _ => Vec::new(),
        };
        let mut run_devices: Vec<usize> = run_chips.iter().map(|(i, _)| *i).collect();
        run_devices.sort_unstable();
        run_devices.dedup();

        // Models on the run's chips. `models()` of an attached world already
        // has its chip lists cut to this TUI's chips.
        let neighbours: Vec<Neighbour> = if run_chips.is_empty() {
            Vec::new()
        } else {
            world
                .models()
                .into_iter()
                .filter(|m| m.phase != ModelPhase::Down)
                .filter_map(|m| {
                    let mut devices: Vec<usize> = run_chips
                        .iter()
                        .filter(|(_, k)| m.chips.contains(k))
                        .map(|(i, _)| *i)
                        .collect();
                    if devices.is_empty() {
                        return None;
                    }
                    devices.sort_unstable();
                    devices.dedup();
                    let flag = flag_of(world, &m);
                    let list: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
                    Some(Neighbour {
                        texts: flag.texts(),
                        flag,
                        devices_text: format!(" dev{}", list.join(",")),
                        devices,
                    })
                })
                .collect()
        };

        TrainFx {
            pulses: prev.pulses.clone(),
            pace_ms: step_ms,
            run_devices,
            neighbours,
        }
    }
}

/// Add a pulse at `now`, keeping at most [`MAX_PULSES`]: past that the newest
/// takes this one's place, as Die 3D merges pulses.
fn add_pulse(list: &mut Vec<StepPulse>, now: u64, kind: PulseKind, size: f32) {
    let p = StepPulse {
        start_ns: now,
        kind,
        size,
    };
    if list.len() < MAX_PULSES {
        list.push(p);
    } else if let Some(last) = list.last_mut() {
        *last = p;
    }
}

// ---- Legend and explain text --------------------------------------------------

/// The Training legend's section on what the bus draws.
pub const LEGEND_TEXT: &[&str] = &[
    "Signal bus:",
    "  ┊ pulse  a step landed: amber; mint and wider when",
    "           loss fell over 1%, dim coral when it rose",
    "  sky      faster for short steps (decoration)",
    "  ▸ dev    a chip the training run uses",
    "  SAME CHIPS  a model served on the run's chips,",
    "           phase and tokens/s; '~' marks an estimate",
    "  bus      in the title: the run comes from the bus",
    "           (an attached host); '~bus' is an estimate",
];

/// The Training explain text's paragraph on what the bus draws.
pub const EXPLAIN_TEXT: &[&str] = &[
    "",
    "Signal bus: each step pulses ┊, wider when the loss fell",
    "over 1% and dimmer when it rose. The sky's speed follows",
    "step time (decoration). ▸ marks the run's chips; SAME",
    "CHIPS names a model served on them, with tokens/s. A run",
    "seen on the bus has 'bus' in the title; '~' marks an",
    "estimate.",
];

/// The legend lines for [`LEGEND_TEXT`], with the overlay's left bar.
pub fn legend_lines(bar: Color, bg: Color, dim: Color) -> Vec<Line<'static>> {
    LEGEND_TEXT
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled("║ ", Style::default().fg(bar).bg(bg)),
                Span::styled(*s, Style::default().fg(dim)),
            ])
        })
        .collect()
}
