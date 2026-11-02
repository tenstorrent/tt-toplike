//! Workload sources: inference servers, training runs, hivemind events,
//! processes and the `--remote` extension.
//!
//! # How the host drives them
//!
//! Every source here uses the same pattern. The source does not own a
//! monitor. The host reads the monitor it already runs and hands the result
//! to the source with `feed(...)`. The source stores it, and the next
//! [`Source::poll`] turns it into payloads. A poll with nothing fed since the
//! last poll emits nothing (the inference and remote sources may still emit a
//! chip update, see below). `ctx` is ignored.
//!
//! | Source | Feed call |
//! |---|---|
//! | [`InferenceSource`] | `feed(&[ServiceState])`: the monitor's latest snapshot |
//! | [`TrainingSource`] | `feed(&TrainState, Option<&[(u32, Vec<usize>)]>)`: the run plus each pid's device indices |
//! | [`HivemindSource`] | `feed(&[SniffEvent])`: events not fed before |
//! | [`ProcessSource`] | `feed(&[ProcSnapshot])`: the current process list |
//! | `RemoteSource` | `feed(Option<&[RemoteInference]>, Option<&[RemoteProc]>)`: the remote frame's extension |
//!
//! For snapshot sources a later `feed` replaces an earlier one not yet
//! polled. Hivemind events queue up (capped at [`HIVEMIND_QUEUE`]). Feeding
//! the same snapshot twice re-emits its samples but never repeats an event
//! or a completed-request count (see [`InferenceSource`]).
//!
//! These sources take plain snapshot values, so they build on every
//! platform, including where the monitors themselves are Linux-only.
//!
//! # Chips
//!
//! Workload snapshots name chips by `/dev/tenstorrent/N` index. Each index is
//! looked up in the shared [`IdentityMap`]. A source never makes a chip key
//! itself. An index that does not resolve is left out of the record and
//! passed to [`IdentityMap::hold_unresolved`] once (as a `Gap` naming the
//! entity), so the map counts it if it never resolves. The inference and
//! remote sources look again on every poll and re-announce a model when its
//! chip list grows.
//!
//! # Secrets
//!
//! Process names and command lines go through
//! [`redact_cmdline`](crate::bus::redact::redact_cmdline). Hivemind log text
//! never goes on the bus: a `Spark` carries only a fixed kind string.

use crate::bus::identity::IdentityMap;
use crate::bus::redact::redact_cmdline;
use crate::bus::signals as sig;
use crate::bus::source::{PollCtx, Source};
use crate::workload::hivemind::event::{EventKind, SniffEvent, Source as SniffSource};
use crate::workload::host_processes::ProcRow;
use crate::workload::inference_server::{MediaStats, Phase, ServiceState, ServingStats};
use crate::workload::train::monitor::{StepTimeSource, TrainState};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tt_toplike_bus::signals as bsig;
use tt_toplike_bus::{EntityKey, Event, Gap, Payload, Provenance, Sample, Sweep, Value};

/// Most hivemind events queued between polls. When full, the oldest is
/// dropped and counted in [`HivemindSource::dropped`].
pub const HIVEMIND_QUEUE: usize = 4096;

/// Most characters in a free-text sample (command line, name, label),
/// counted after redaction.
pub const MAX_TEXT: usize = 512;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Emit one sample.
fn put(out: &mut dyn FnMut(Payload), e: &EntityKey, signal: &str, v: Value, prov: Provenance) {
    // A non-finite reading is a missing reading: no sample.
    if !v.is_finite() {
        return;
    }
    out(Payload::Sample(Sample {
        entity: e.clone(),
        signal: signal.to_string(),
        value: v,
        prov,
    }));
}

/// Resolve chip indexes through the map. Returns the keys (in index order,
/// without repeats) and the indexes that did not resolve.
fn resolve_chips(map: &IdentityMap, indices: &[usize]) -> (Vec<EntityKey>, Vec<usize>) {
    let mut keys: Vec<EntityKey> = Vec::new();
    let mut missing = Vec::new();
    for &i in indices {
        match map.key_for_index(i) {
            Some(k) => {
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
            None => {
                if !missing.contains(&i) {
                    missing.push(i);
                }
            }
        }
    }
    (keys, missing)
}

/// Hold each newly unresolved index once per entity. `held` remembers which
/// indexes were held, so a snapshot repeated every tick does not hold (and
/// later count as dropped) the same index again and again. Indexes that now
/// resolve leave `held`, so a chip that disappears again is held again.
fn hold_new(
    map: &IdentityMap,
    source: &str,
    entity: &EntityKey,
    missing: &[usize],
    held: &mut BTreeSet<usize>,
) {
    held.retain(|i| missing.contains(i));
    for &i in missing {
        if held.insert(i) {
            map.hold_unresolved(
                i,
                Payload::Gap(Gap {
                    reason: format!("{source}: chip index {i} of {entity} was not resolved"),
                }),
            );
        }
    }
}

/// Chip keys as one text value: `chip:A,chip:B`.
fn chip_text(keys: &[EntityKey]) -> String {
    keys.iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Cut free text to [`MAX_TEXT`] characters. Applied after redaction.
fn clip(mut s: String) -> String {
    if let Some((i, _)) = s.char_indices().nth(MAX_TEXT) {
        s.truncate(i);
    }
    s
}

/// The phase as the world spells it.
fn phase_text(p: Phase) -> &'static str {
    match p {
        Phase::Down => "down",
        Phase::Compiling => "compiling",
        Phase::Loading => "loading",
        Phase::Ready => "ready",
        Phase::Alarm => "alarm",
    }
}

// ---------------------------------------------------------------------------
// Model tracking (shared by the inference and remote sources)
// ---------------------------------------------------------------------------

/// Serving numbers, from a local `ServingStats` or a remote `RemoteServing`.
#[derive(Debug, Clone, PartialEq)]
struct ServeSnap {
    tokens_per_s: f32,
    running: u32,
    waiting: u32,
    kv_cache: f32,
    ttft_s: f32,
}

/// Media numbers, from a local `MediaStats` or a remote `RemoteMedia`.
#[derive(Debug, Clone, PartialEq)]
struct MediaSnap {
    generations_per_min: f32,
    jobs: u32,
}

/// One service, normalised from either a local or a remote snapshot.
#[derive(Debug, Clone)]
struct ModelSnap {
    key: String,
    label: String,
    /// `None` when a remote peer sent a phase this build does not know.
    phase: Option<Phase>,
    progress: Option<f32>,
    cpu_pct: Option<f32>,
    rss_bytes: Option<u64>,
    kernels: Option<u64>,
    shards: Option<u64>,
    chips: Vec<usize>,
    serving: Option<ServeSnap>,
    media: Option<MediaSnap>,
    /// This scrape's completed-request delta and a fingerprint of the stats
    /// it came from. A repeated fingerprint is the same scrape fed again.
    completed: Option<(u32, String)>,
}

/// The announcing event last sent for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Announced {
    Loading,
    Ready,
}

#[derive(Debug, Default)]
struct Tracked {
    announced: Option<Announced>,
    /// Chip keys in the last announcing event.
    chips: Vec<EntityKey>,
    /// Chip indexes from the last snapshot.
    indices: Vec<usize>,
    /// Indexes passed to `hold_unresolved` and not resolved since.
    held: BTreeSet<usize>,
    /// Progress in the last snapshot, reused when only the chips change.
    progress: Option<f32>,
    /// Fingerprint of the last scrape whose completed delta was considered.
    last_fp: Option<String>,
}

/// Turns service snapshots into model events and samples.
struct ModelTracker {
    source: &'static str,
    map: Arc<IdentityMap>,
    tracked: HashMap<String, Tracked>,
}

impl ModelTracker {
    fn new(source: &'static str, map: Arc<IdentityMap>) -> Self {
        Self {
            source,
            map,
            tracked: HashMap::new(),
        }
    }

    fn announce(
        model: &EntityKey,
        a: Announced,
        chips: &[EntityKey],
        progress: Option<f32>,
        out: &mut dyn FnMut(Payload),
    ) {
        let e = match a {
            Announced::Loading => Event::ModelLoading {
                model: model.clone(),
                chips: chips.to_vec(),
                progress,
            },
            Announced::Ready => Event::ModelReady {
                model: model.clone(),
                chips: chips.to_vec(),
            },
        };
        out(Payload::Event(e));
    }

    /// Process one full snapshot. Order per service: the announcing event
    /// (when the phase or chips changed), then samples, then
    /// `RequestsCompleted`. Then `ModelGone` for announced services that
    /// left. A service that was never announced sends nothing.
    fn process(&mut self, snaps: &[ModelSnap], out: &mut dyn FnMut(Payload)) {
        let mut present: HashSet<&str> = HashSet::new();
        for s in snaps {
            if !present.insert(s.key.as_str()) {
                continue; // a repeated key in one snapshot: the first wins
            }
            let model = EntityKey::model(&s.key);
            let (chips, missing) = resolve_chips(&self.map, &s.chips);
            let t = self.tracked.entry(s.key.clone()).or_default();
            hold_new(&self.map, self.source, &model, &missing, &mut t.held);
            t.indices = s.chips.clone();
            t.progress = s.progress;

            // 1. The announcing event, before any sample of this model.
            let want = match s.phase {
                Some(Phase::Compiling) | Some(Phase::Loading) => Some(Announced::Loading),
                Some(Phase::Ready) => Some(Announced::Ready),
                // Down, Alarm and unknown phases: a sample only, no event.
                _ => None,
            };
            if let Some(w) = want {
                if t.announced != Some(w) || t.chips != chips {
                    Self::announce(&model, w, &chips, s.progress, out);
                    t.announced = Some(w);
                    t.chips = chips;
                }
            }

            // A service never announced (first seen Down, Alarm or in an
            // unknown phase) sends nothing: the world has no view for it,
            // and its samples would only sit there unread.
            if t.announced.is_none() {
                continue;
            }

            // 2. Samples.
            Self::samples(&model, s, out);

            // 3. Completed requests. Nothing on the first scrape seen, and
            // nothing when the same scrape is fed again.
            if let Some((n, fp)) = &s.completed {
                let new_scrape = t.last_fp.as_ref() != Some(fp);
                if new_scrape && t.last_fp.is_some() && *n > 0 {
                    out(Payload::Event(Event::RequestsCompleted {
                        model: model.clone(),
                        n: *n,
                    }));
                }
                t.last_fp = Some(fp.clone());
            }
        }
        let gone: Vec<String> = self
            .tracked
            .keys()
            .filter(|k| !present.contains(k.as_str()))
            .cloned()
            .collect();
        for k in gone {
            let announced = self
                .tracked
                .remove(&k)
                .is_some_and(|t| t.announced.is_some());
            if !announced {
                continue; // nothing was ever sent for it
            }
            out(Payload::Event(Event::ModelGone {
                model: EntityKey::model(&k),
            }));
        }
    }

    fn samples(model: &EntityKey, s: &ModelSnap, out: &mut dyn FnMut(Payload)) {
        use Provenance::{Derived, Estimated, Measured};
        put(
            out,
            model,
            bsig::MODEL_LABEL,
            Value::Text(s.label.clone()),
            Measured,
        );
        if let Some(p) = s.phase {
            // The monitor derives the phase from deltas and readiness.
            put(
                out,
                model,
                bsig::MODEL_PHASE,
                Value::Text(phase_text(p).into()),
                Derived,
            );
        }
        if let Some(p) = s.progress {
            // Progress is the monitor's best-effort estimate.
            put(
                out,
                model,
                bsig::MODEL_PROGRESS,
                Value::F64(p as f64),
                Estimated,
            );
        }
        if let Some(v) = s.cpu_pct {
            put(
                out,
                model,
                sig::MODEL_CPU_PCT,
                Value::F64(v as f64),
                Measured,
            );
        }
        if let Some(v) = s.rss_bytes {
            put(out, model, sig::MODEL_RSS_BYTES, Value::U64(v), Measured);
        }
        if let Some(v) = s.kernels {
            put(out, model, sig::MODEL_KERNEL_COUNT, Value::U64(v), Measured);
        }
        if let Some(v) = s.shards {
            put(out, model, sig::MODEL_SHARD_COUNT, Value::U64(v), Measured);
        }
        if let Some(v) = &s.serving {
            put(
                out,
                model,
                bsig::MODEL_TOKENS_PER_S,
                Value::F64(v.tokens_per_s as f64),
                Derived,
            );
            put(
                out,
                model,
                bsig::MODEL_RUNNING,
                Value::U64(v.running as u64),
                Measured,
            );
            put(
                out,
                model,
                bsig::MODEL_WAITING,
                Value::U64(v.waiting as u64),
                Measured,
            );
            put(
                out,
                model,
                bsig::MODEL_KV_CACHE,
                Value::F64(v.kv_cache as f64),
                Measured,
            );
            // 0 means no request has finished yet: no reading.
            if v.ttft_s > 0.0 {
                put(
                    out,
                    model,
                    sig::MODEL_TTFT_S,
                    Value::F64(v.ttft_s as f64),
                    Derived,
                );
            }
        } else if let Some(m) = &s.media {
            put(
                out,
                model,
                sig::MODEL_MEDIA_JOBS,
                Value::U64(m.jobs as u64),
                Measured,
            );
            put(
                out,
                model,
                sig::MODEL_GENERATIONS_PER_MIN,
                Value::F64(m.generations_per_min as f64),
                Derived,
            );
        }
    }

    /// With no new snapshot: look up held chip indexes again and re-announce
    /// a model whose chip list changed.
    fn recheck(&mut self, out: &mut dyn FnMut(Payload)) {
        for (key, t) in self.tracked.iter_mut() {
            if t.held.is_empty() {
                continue;
            }
            let model = EntityKey::model(key);
            let (chips, missing) = resolve_chips(&self.map, &t.indices);
            hold_new(&self.map, self.source, &model, &missing, &mut t.held);
            if let Some(a) = t.announced {
                if chips != t.chips {
                    Self::announce(&model, a, &chips, t.progress, out);
                    t.chips = chips;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Inference
// ---------------------------------------------------------------------------

/// Turns the inference monitor's `ServiceState` snapshots into model records.
///
/// Per service, on entity `EntityKey::model(key)`, where the key and the
/// label are passed through `redact_cmdline` and cut to [`MAX_TEXT`]
/// characters:
/// * `ModelLoading{chips, progress}` when the phase becomes Compiling or
///   Loading, `ModelReady{chips}` when it becomes Ready, each once per
///   change. Always emitted before the model's samples, because the world
///   creates a model view only on one of these events.
/// * A service first seen Down, Alarm or in an unknown phase sends nothing
///   until it is announced.
/// * Samples: label, phase (Alarm and Down are a phase sample and no event),
///   progress, CPU, RSS, kernel and shard counts, and either the serving
///   fields (tokens/s, running, waiting, KV cache, TTFT) or the media fields
///   (jobs in progress, generations per minute).
/// * `RequestsCompleted{n}` from the scrape's completed delta. Not on the
///   first scrape seen, and not when the same scrape is fed again (the
///   stats, counters included, compare equal).
/// * `ModelGone` once when an announced service is missing from a
///   snapshot. If it returns, it is announced again.
pub struct InferenceSource {
    tracker: ModelTracker,
    pending: Option<Vec<ModelSnap>>,
}

impl InferenceSource {
    pub fn new(map: Arc<IdentityMap>) -> Self {
        Self {
            tracker: ModelTracker::new("inference", map),
            pending: None,
        }
    }

    /// Hand over the monitor's latest snapshot (all services).
    pub fn feed(&mut self, snapshot: &[ServiceState]) {
        self.pending = Some(snapshot.iter().map(snap_from_state).collect());
    }
}

fn snap_from_state(s: &ServiceState) -> ModelSnap {
    let serving = s.serving.as_ref().map(serve_from_stats);
    let media = if s.serving.is_none() {
        s.media.as_ref().map(media_from_stats)
    } else {
        None
    };
    let completed = match (&s.serving, &s.media) {
        (Some(st), _) => Some((st.completed_delta, format!("{st:?}"))),
        (None, Some(m)) => Some((m.completed_delta, format!("{m:?}"))),
        _ => None,
    };
    ModelSnap {
        key: clip(redact_cmdline(&s.key)),
        label: clip(redact_cmdline(&s.label)),
        phase: Some(s.phase),
        progress: s.progress,
        cpu_pct: Some(s.cpu_pct),
        rss_bytes: Some(s.rss_bytes),
        kernels: Some(s.kernel_count as u64),
        shards: Some(s.loaded_count as u64),
        chips: s.chips.clone(),
        serving,
        media,
        completed,
    }
}

fn serve_from_stats(s: &ServingStats) -> ServeSnap {
    ServeSnap {
        tokens_per_s: s.generation_tps,
        running: s.requests_running,
        waiting: s.requests_waiting,
        kv_cache: s.kv_cache_usage,
        ttft_s: s.ttft_avg_s,
    }
}

fn media_from_stats(m: &MediaStats) -> MediaSnap {
    MediaSnap {
        generations_per_min: m.generations_per_min,
        jobs: m.jobs_in_progress,
    }
}

impl Source for InferenceSource {
    fn name(&self) -> &'static str {
        "inference"
    }
    fn poll(&mut self, _ctx: &PollCtx<'_>, _now: Instant, out: &mut dyn FnMut(Payload)) {
        match self.pending.take() {
            Some(snaps) => self.tracker.process(&snaps, out),
            None => self.tracker.recheck(out),
        }
    }
}

// ---------------------------------------------------------------------------
// Training
// ---------------------------------------------------------------------------

/// Turns the training monitor's `TrainState` into samples on a training-run
/// entity.
///
/// The run key is `<binary>:<pid>@<start time>` when the trainer process is
/// known (a new process is a new run) and `local` when only a log is.
/// Signals: step, step budget (when known), loss (finite only), step time,
/// host CPU and RSS, and the chips as text. The chips come from the process
/// list the host passes with each feed: the trainer's pid is looked up there
/// and its device indexes resolved through the map. With no list, or no
/// entry for the pid, no chips sample is sent.
pub struct TrainingSource {
    map: Arc<IdentityMap>,
    pending: Option<(TrainState, Option<Vec<(u32, Vec<usize>)>>)>,
    held: BTreeSet<usize>,
}

impl TrainingSource {
    pub fn new(map: Arc<IdentityMap>) -> Self {
        Self {
            map,
            pending: None,
            held: BTreeSet::new(),
        }
    }

    /// Hand over the run state and, when known, each process's device
    /// indexes as `(pid, indexes)`.
    pub fn feed(&mut self, state: &TrainState, procs: Option<&[(u32, Vec<usize>)]>) {
        self.pending = Some((state.clone(), procs.map(|p| p.to_vec())));
    }

    fn emit(
        &mut self,
        st: &TrainState,
        procs: Option<&[(u32, Vec<usize>)]>,
        out: &mut dyn FnMut(Payload),
    ) {
        use Provenance::{Derived, Measured};
        if st.proc.is_none() && st.step == 0 && st.loss.is_none() {
            return; // no run attached
        }
        let key = match &st.proc {
            Some(p) => format!("{}:{}@{}", p.binary, p.pid, p.start_time.unwrap_or(0)),
            None => "local".to_string(),
        };
        let run = EntityKey::training_run(&key);
        put(out, &run, sig::TRAIN_STEP, Value::U64(st.step), Measured);
        if st.max_steps > 0 {
            put(
                out,
                &run,
                sig::TRAIN_MAX_STEPS,
                Value::U64(st.max_steps),
                Measured,
            );
        }
        if let Some(l) = st.loss.filter(|l| l.is_finite()) {
            put(out, &run, sig::TRAIN_LOSS, Value::F64(l as f64), Measured);
        }
        if st.step_ms > 0.0 {
            let prov = match st.step_time_source {
                StepTimeSource::Observed => Derived,
                _ => Measured,
            };
            put(
                out,
                &run,
                sig::TRAIN_STEP_MS,
                Value::F64(st.step_ms as f64),
                prov,
            );
        }
        if let Some(v) = st.host_cpu_pct {
            put(
                out,
                &run,
                sig::TRAIN_CPU_PCT,
                Value::F64(v as f64),
                Measured,
            );
        }
        if let Some(v) = st.host_rss_bytes {
            put(out, &run, sig::TRAIN_RSS_BYTES, Value::U64(v), Measured);
        }
        let indices: Vec<usize> = match (&st.proc, procs) {
            (Some(p), Some(list)) => list
                .iter()
                .filter(|(pid, _)| i64::from(*pid) == i64::from(p.pid))
                .flat_map(|(_, d)| d.iter().copied())
                .collect(),
            _ => Vec::new(),
        };
        let (chips, missing) = resolve_chips(&self.map, &indices);
        hold_new(&self.map, "training", &run, &missing, &mut self.held);
        if !chips.is_empty() {
            put(
                out,
                &run,
                sig::TRAIN_CHIPS,
                Value::Text(chip_text(&chips)),
                Measured,
            );
        }
    }
}

impl Source for TrainingSource {
    fn name(&self) -> &'static str {
        "training"
    }
    fn poll(&mut self, _ctx: &PollCtx<'_>, _now: Instant, out: &mut dyn FnMut(Payload)) {
        if let Some((st, procs)) = self.pending.take() {
            self.emit(&st, procs.as_deref(), out);
        }
    }
}

// ---------------------------------------------------------------------------
// Hivemind
// ---------------------------------------------------------------------------

/// Turns hivemind `SniffEvent`s into events.
///
/// * `Compile` becomes `KernelCompiled{chip, source}` with the source label.
/// * `Reset` becomes `Reset{chips}` (empty when the chip is not known).
/// * Everything else becomes `Spark{chip, kind}`, where `kind` is
///   `<source>.<event kind>` built from fixed strings (`metal.log`).
///
/// The chip is the event's `device` index resolved through the map, `None`
/// when absent or unresolved. The event's text and origin are never copied:
/// log lines can hold secrets.
pub struct HivemindSource {
    map: Arc<IdentityMap>,
    queue: VecDeque<(SniffSource, EventKind, Option<u8>)>,
    dropped: u64,
}

impl HivemindSource {
    pub fn new(map: Arc<IdentityMap>) -> Self {
        Self {
            map,
            queue: VecDeque::new(),
            dropped: 0,
        }
    }

    /// Queue events. Feed each event once.
    pub fn feed(&mut self, events: &[SniffEvent]) {
        for e in events {
            if self.queue.len() >= HIVEMIND_QUEUE {
                self.queue.pop_front();
                self.dropped += 1;
            }
            // Keep only the typed fields; `text` and `origin` stay behind.
            self.queue.push_back((e.source, e.kind, e.device));
        }
    }

    /// Events dropped because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Fixed name of an event kind.
fn kind_text(k: EventKind) -> &'static str {
    match k {
        EventKind::Log => "log",
        EventKind::Compile => "compile",
        EventKind::Process => "process",
        EventKind::Fd => "fd",
        EventKind::DriverMsg => "driver",
        EventKind::Emission => "emission",
        EventKind::Inspector => "inspector",
        EventKind::Reset => "reset",
    }
}

impl Source for HivemindSource {
    fn name(&self) -> &'static str {
        "hivemind"
    }
    fn poll(&mut self, _ctx: &PollCtx<'_>, _now: Instant, out: &mut dyn FnMut(Payload)) {
        while let Some((src, kind, device)) = self.queue.pop_front() {
            let chip = device.and_then(|d| self.map.key_for_index(d as usize));
            let e = match kind {
                EventKind::Compile => Event::KernelCompiled {
                    chip,
                    source: src.label().to_string(),
                },
                EventKind::Reset => Event::Reset {
                    chips: chip.into_iter().collect(),
                },
                k => Event::Spark {
                    chip,
                    kind: format!("{}.{}", src.label(), kind_text(k)),
                },
            };
            out(Payload::Event(e));
        }
    }
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

/// One process, as the host hands it to [`ProcessSource`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcSnapshot {
    pub pid: u32,
    /// Start time in clock ticks since boot (`/proc/<pid>/stat` field 22),
    /// so a recycled pid is a different entity. 0 when unknown.
    pub start_ticks: u64,
    pub name: String,
    /// The full command line. Empty when unknown. Redacted before use.
    pub cmdline: String,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    /// `/dev/tenstorrent/N` indexes the process holds open.
    pub device_indices: Vec<usize>,
    /// Whether the process uses Tenstorrent hardware, when a remote host
    /// says so. `None` for local rows.
    pub uses_tt: Option<bool>,
}

impl From<&ProcRow> for ProcSnapshot {
    /// `ProcRow` has no start time and no command line, so both stay empty.
    fn from(r: &ProcRow) -> Self {
        ProcSnapshot {
            pid: r.pid.max(0) as u32,
            start_ticks: 0,
            name: r.name.clone(),
            cmdline: String::new(),
            cpu_pct: r.cpu_pct,
            rss_bytes: r.mem_bytes,
            device_indices: r
                .tt
                .as_ref()
                .map(|t| t.device_indices.clone())
                .unwrap_or_default(),
            uses_tt: None,
        }
    }
}

/// What [`ProcessSource`] remembers about one process between feeds.
#[derive(Debug, Default)]
struct ProcState {
    /// Hash of the raw command line last redacted, and its redacted text.
    /// The raw text itself is not kept.
    cmd_hash: Option<u64>,
    /// Hash of the raw name last sent.
    name_hash: Option<u64>,
    /// Chips text last sent. `None` while no chip resolves.
    device: Option<String>,
    /// Indexes passed to `hold_unresolved` and not resolved since.
    held: BTreeSet<usize>,
}

fn hash_of(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Turns the process list into records on process entities
/// (`EntityKey::process(pid, start_ticks)`).
///
/// Per feed and process: one `Sweep` of the numbers (CPU, RSS, and
/// `uses_tt` from a remote). The text signals (name, command line, chips)
/// are sent only when they changed since the last feed for that process,
/// so a steady process list costs one record per process per feed. Name
/// and command line are redacted and cut to [`MAX_TEXT`] characters;
/// `redact_cmdline` runs once per distinct command line. State for a
/// process missing from a feed is dropped, so a process that returns is
/// sent in full.
pub struct ProcessSource {
    map: Arc<IdentityMap>,
    source: &'static str,
    pending: Option<Vec<ProcSnapshot>>,
    state: HashMap<EntityKey, ProcState>,
    redactions: u64,
}

impl ProcessSource {
    pub fn new(map: Arc<IdentityMap>) -> Self {
        Self::named("processes", map)
    }

    fn named(source: &'static str, map: Arc<IdentityMap>) -> Self {
        Self {
            map,
            source,
            pending: None,
            state: HashMap::new(),
            redactions: 0,
        }
    }

    /// Command lines redacted so far (one per distinct command line seen).
    pub fn redactions(&self) -> u64 {
        self.redactions
    }

    /// Processes with remembered state.
    pub fn tracked(&self) -> usize {
        self.state.len()
    }

    /// Hand over the current process list.
    pub fn feed(&mut self, procs: &[ProcSnapshot]) {
        self.pending = Some(procs.to_vec());
    }

    fn emit(&mut self, procs: &[ProcSnapshot], out: &mut dyn FnMut(Payload)) {
        use Provenance::Measured;
        let mut present = HashSet::new();
        for p in procs {
            let e = EntityKey::process(p.pid, p.start_ticks);
            present.insert(e.clone());
            let st = self.state.entry(e.clone()).or_default();

            // Numbers: one sweep.
            let mut signals = Vec::new();
            let mut values = Vec::new();
            if p.cpu_pct.is_finite() {
                signals.push(sig::PROC_CPU_PCT.to_string());
                values.push(Value::F64(p.cpu_pct as f64));
            }
            signals.push(sig::PROC_RSS_BYTES.to_string());
            values.push(Value::U64(p.rss_bytes));
            if let Some(u) = p.uses_tt {
                signals.push(sig::PROC_USES_TT.to_string());
                values.push(Value::U64(u as u64));
            }
            out(Payload::Sweep(Sweep {
                entity: e.clone(),
                signals,
                values,
                prov: Measured,
            }));

            // Text: only when it changed.
            if !p.name.is_empty() {
                let h = hash_of(&p.name);
                if st.name_hash != Some(h) {
                    st.name_hash = Some(h);
                    let text = clip(redact_cmdline(&p.name));
                    put(out, &e, sig::PROC_NAME, Value::Text(text), Measured);
                }
            }
            if !p.cmdline.is_empty() {
                let h = hash_of(&p.cmdline);
                if st.cmd_hash != Some(h) {
                    st.cmd_hash = Some(h);
                    self.redactions += 1;
                    let text = clip(redact_cmdline(&p.cmdline));
                    put(out, &e, sig::PROC_CMDLINE, Value::Text(text), Measured);
                }
            }
            let (chips, missing) = resolve_chips(&self.map, &p.device_indices);
            hold_new(&self.map, self.source, &e, &missing, &mut st.held);
            if chips.is_empty() {
                st.device = None;
            } else {
                let text = chip_text(&chips);
                if st.device.as_deref() != Some(text.as_str()) {
                    put(
                        out,
                        &e,
                        sig::PROC_DEVICE,
                        Value::Text(text.clone()),
                        Measured,
                    );
                    st.device = Some(text);
                }
            }
        }
        self.state.retain(|k, _| present.contains(k));
    }
}

impl Source for ProcessSource {
    fn name(&self) -> &'static str {
        self.source
    }
    fn poll(&mut self, _ctx: &PollCtx<'_>, _now: Instant, out: &mut dyn FnMut(Payload)) {
        if let Some(p) = self.pending.take() {
            self.emit(&p, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Remote
// ---------------------------------------------------------------------------

#[cfg(feature = "remote")]
pub use remote::RemoteSource;

#[cfg(feature = "remote")]
mod remote {
    use super::*;
    use crate::backend::{RemoteInference, RemoteProc};

    /// Turns the `--remote` frame extension into the same records as the
    /// local inference and process sources, so a remote host looks the same
    /// on the bus.
    ///
    /// A `None` list means the remote did not stream it this frame: nothing
    /// is emitted for it and no model is declared gone. `Some(&[])` is an
    /// authoritative empty list.
    ///
    /// Chip indexes are the remote host's; the map must be built
    /// from the remote backend's device list. Remote processes carry no
    /// start time, so they are keyed with start time 0, and no device list,
    /// so they carry no chips. A remote has no CPU, RSS, kernel or shard
    /// counts per model, so those samples are not sent.
    pub struct RemoteSource {
        models: ModelTracker,
        procs: ProcessSource,
        pending_models: Option<Vec<ModelSnap>>,
    }

    impl RemoteSource {
        pub fn new(map: Arc<IdentityMap>) -> Self {
            Self {
                models: ModelTracker::new("remote", map.clone()),
                procs: ProcessSource::named("remote", map),
                pending_models: None,
            }
        }

        /// Hand over one frame's extension lists.
        pub fn feed(
            &mut self,
            inference: Option<&[RemoteInference]>,
            procs: Option<&[RemoteProc]>,
        ) {
            if let Some(list) = inference {
                self.pending_models = Some(list.iter().map(snap_from_remote).collect());
            }
            if let Some(list) = procs {
                let rows: Vec<ProcSnapshot> = list
                    .iter()
                    .map(|p| ProcSnapshot {
                        pid: p.pid,
                        start_ticks: 0,
                        name: p.name.clone(),
                        cmdline: p.cmd.clone(),
                        cpu_pct: p.cpu_pct,
                        rss_bytes: p.mem_bytes,
                        device_indices: Vec::new(),
                        uses_tt: Some(p.uses_tt),
                    })
                    .collect();
                self.procs.feed(&rows);
            }
        }
    }

    fn parse_phase(s: &str) -> Option<Phase> {
        match s {
            "down" => Some(Phase::Down),
            "compiling" => Some(Phase::Compiling),
            "loading" => Some(Phase::Loading),
            "ready" => Some(Phase::Ready),
            "alarm" => Some(Phase::Alarm),
            _ => None,
        }
    }

    fn snap_from_remote(r: &RemoteInference) -> ModelSnap {
        let serving = r.serving.as_ref().map(|s| ServeSnap {
            tokens_per_s: s.generation_tps,
            running: s.requests_running,
            waiting: s.requests_waiting,
            kv_cache: s.kv_cache_usage,
            ttft_s: s.ttft_avg_s,
        });
        let media = if r.serving.is_none() {
            r.media.as_ref().map(|m| MediaSnap {
                generations_per_min: m.generations_per_min,
                jobs: m.jobs_in_progress,
            })
        } else {
            None
        };
        // RemoteServing has no cumulative counters, so two scrapes with every
        // field equal look like one. RemoteMedia carries completed_total.
        let completed = match (&r.serving, &r.media) {
            (Some(s), _) => Some((s.completed_delta, format!("{s:?}"))),
            (None, Some(m)) => Some((m.completed_delta, format!("{m:?}"))),
            _ => None,
        };
        ModelSnap {
            key: clip(redact_cmdline(&r.key)),
            label: clip(redact_cmdline(&r.label)),
            phase: parse_phase(&r.phase),
            progress: r.progress,
            cpu_pct: None,
            rss_bytes: None,
            kernels: None,
            shards: None,
            chips: r.chips.clone(),
            serving,
            media,
            completed,
        }
    }

    impl Source for RemoteSource {
        fn name(&self) -> &'static str {
            "remote"
        }
        fn poll(&mut self, ctx: &PollCtx<'_>, now: Instant, out: &mut dyn FnMut(Payload)) {
            match self.pending_models.take() {
                Some(snaps) => self.models.process(&snaps, out),
                None => self.models.recheck(out),
            }
            self.procs.poll(ctx, now, out);
        }
    }
}

#[cfg(test)]
#[path = "workload_sources_tests.rs"]
mod tests;
