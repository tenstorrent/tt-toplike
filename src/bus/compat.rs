//! Compatibility with the shapes the TUI used before the bus: the
//! tt-station `/telemetry` frame, and the inference monitor's
//! [`ServiceState`] list, both rebuilt from the world.
//!
//! # The `/telemetry` frame
//!
//! Today a frame is the newest `tt-smi -s` snapshot with the `tt_toplike`
//! extension added (see `crate::backend::inject_extension`). The bus keeps
//! that snapshot verbatim as a `Raw` record from the source named
//! [`crate::bus::signals::RAW_SOURCE_TT_SMI`] (`"tt-smi"`, published by
//! `BackendSource`). These helpers rebuild the same frame from a [`World`].
//!
//! Both broadcast sites use them: the headless loop
//! (`crate::bus::headless`) and the TUI's `/serve` block, through
//! `crate::bus::tui_bus::serve_frame`.
//!
//! Both frame helpers need the `remote` feature, because the extension
//! types live behind it.
//!
//! # The inference monitor's rows
//!
//! [`world_to_service_states`] turns the world's models back into the
//! `ServiceState` list the `i` view draws. The TUI uses it when it is
//! attached to a headless host (see `ui::tui::inference_bus`). It needs no
//! feature.

#[cfg(feature = "remote")]
use super::signals::RAW_SOURCE_TT_SMI;
#[cfg(feature = "remote")]
use crate::backend::{inject_extension, TtToplikeExt};
use crate::bus::signals as sig;
use crate::workload::inference_server::{
    MediaCounters, MediaStats, Phase, Readiness, ServiceState, ServingStats, VllmCounters,
};
use tt_toplike_bus::signals as bsig;
#[cfg(feature = "remote")]
use tt_toplike_bus::World;
use tt_toplike_bus::{EntityKey, ModelPhase, ModelView, Provenance, Value, WorldRead};

/// The frame to broadcast for a box that reads its own chips.
///
/// Returns exactly `inject_extension(&raw.body, ext)` for the newest `tt-smi`
/// raw record in `world`, or `None` when the world has no such record yet.
/// A body that `inject_extension` cannot parse or that is not a JSON object
/// comes back unchanged, as it does there.
///
/// Relay rule that every call site keeps: a `--remote` box
/// (`backend_type == BackendType::Remote`) re-broadcasts the origin's frame
/// verbatim and never adds its own extension. That call site must use
/// [`relay_frame`] and must not call this function.
#[cfg(feature = "remote")]
pub fn telemetry_frame(world: &World, ext: &TtToplikeExt) -> Option<String> {
    let raw = world.latest_raw(RAW_SOURCE_TT_SMI)?;
    Some(inject_extension(&raw.body, ext))
}

/// The frame to re-broadcast for a `--remote` box: the newest `tt-smi` raw
/// body, byte for byte. No extension is added, so a `tt_toplike` key in the
/// result is the origin's own.
#[cfg(feature = "remote")]
pub fn relay_frame(world: &World) -> Option<String> {
    world
        .latest_raw(RAW_SOURCE_TT_SMI)
        .map(|raw| raw.body.clone())
}

// ---------------------------------------------------------------------------
// The inference monitor's rows from the world
// ---------------------------------------------------------------------------

/// The `i` view's service rows, built from the world's models, with no chip
/// indexes: [`world_to_service_states_with`] with a resolver that knows no
/// chip, so every row's `chips` is empty. The world names chips by PCI
/// address and carries no `/dev/tenstorrent/N` index.
pub fn world_to_service_states(world: &dyn WorldRead) -> Vec<ServiceState> {
    world_to_service_states_with(world, &|_| None)
}

/// The `i` view's service rows, one per model in the world, in the world's
/// key order. `chip_index` turns a chip key into a `/dev/tenstorrent/N`
/// index; the TUI builds it from its own backend's device list.
///
/// What each field comes from:
///
/// * `key`: the model entity key without its `model:` prefix. This is the
///   key the inference source wrote, which is the monitor's key after
///   `safe_key` (redacted and clipped; a key that needed that carries a
///   hash suffix and does not match the monitor's raw key).
/// * `label`, `phase`, `progress`: the [`ModelView`]. The label is as
///   written: the estimate marker `~` is added by the caller (see
///   [`model_is_estimated`]).
/// * `chips`: each chip key through `chip_index`, sorted, without repeats.
///   A key that does not resolve is left out. A chip that a read view cut
///   (`ReadView::only_chips`) is not in the model view and cannot come back.
/// * `serving`: present when the model has any of tokens/s, running,
///   waiting or KV cache. `generation_tps`, `requests_running`,
///   `requests_waiting` and `kv_cache_usage` come from the model view (the
///   same fields Die 3D's flag reads), and `ttft_avg_s` from the
///   [`sig::MODEL_TTFT_S`] reading. A missing one is 0.
/// * `media`: present only without `serving`, when the model has a
///   [`sig::MODEL_MEDIA_JOBS`] or [`sig::MODEL_GENERATIONS_PER_MIN`]
///   reading. `jobs_in_progress` and `generations_per_min` come from them.
/// * `cpu_pct`, `rss_bytes`, `kernel_count`, `loaded_count`: the
///   [`sig::MODEL_CPU_PCT`], [`sig::MODEL_RSS_BYTES`],
///   [`sig::MODEL_KERNEL_COUNT`] and [`sig::MODEL_SHARD_COUNT`] readings,
///   0 when absent.
/// * `readiness`: derived from the phase, as the `--remote` mapping does:
///   `Ready { runner: None }` for ready, `Down` for down, otherwise
///   `NotReady`.
///
/// Fields the bus does not carry, and the neutral value each takes:
/// `last_log` and `top_proc` (`None`), the runner name in `readiness`
/// (`None`), `rss_delta`, `kernel_delta`, `loaded_delta`, `flat_ticks` and
/// `safetensors_fds` (0), `port` (`None`: no source writes it), the serving
/// fields other than the five above (`prompt_tps`, `completed_delta`,
/// `errored_delta`, `queue_avg_s`, `prefill_avg_s`, `decode_avg_s`,
/// `tpot_avg_s`, `prefix_hit_rate`, `preemptions_delta`: 0) and the vLLM
/// counters (`Default`), and the media fields other than the two above
/// (0, counters `Default`). The views that read these show a dash, a zero
/// or nothing for them.
pub fn world_to_service_states_with(
    world: &dyn WorldRead,
    chip_index: &dyn Fn(&EntityKey) -> Option<usize>,
) -> Vec<ServiceState> {
    world
        .models()
        .iter()
        .map(|m| service_state_of(world, m, chip_index))
        .collect()
}

/// Whether the model's readings that the `i` view and Die 3D's flag show
/// as text (label, phase, progress, tokens/s, running, waiting, KV cache)
/// include one marked `Estimated` or `Decoration`. Every reading of a
/// synthetic (`--mock`) host is; on a real host the progress figure is (the
/// inference source writes it as `Estimated`). Die 3D's flag starts with
/// `~` then, and so does the `i` view's label for an attached world. This
/// checks Die 3D's signals plus running and KV cache, which the `i` view
/// also prints; no source marks those two differently from the rest.
pub fn model_is_estimated(world: &dyn WorldRead, model: &EntityKey) -> bool {
    [
        bsig::MODEL_LABEL,
        bsig::MODEL_PHASE,
        bsig::MODEL_PROGRESS,
        bsig::MODEL_TOKENS_PER_S,
        bsig::MODEL_RUNNING,
        bsig::MODEL_WAITING,
        bsig::MODEL_KV_CACHE,
    ]
    .iter()
    .filter_map(|s| world.reading(model, s))
    .any(|r| matches!(r.prov, Provenance::Estimated | Provenance::Decoration))
}

/// A reading as a number, when it is one.
fn number_of(world: &dyn WorldRead, model: &EntityKey, signal: &str) -> Option<f64> {
    match world.reading(model, signal)?.value {
        Value::F64(v) => Some(v),
        Value::U64(v) => Some(v as f64),
        Value::I64(v) => Some(v as f64),
        Value::Text(_) => None,
    }
}

fn phase_of(p: ModelPhase) -> Phase {
    match p {
        ModelPhase::Down => Phase::Down,
        ModelPhase::Compiling => Phase::Compiling,
        ModelPhase::Loading => Phase::Loading,
        ModelPhase::Ready => Phase::Ready,
        ModelPhase::Alarm => Phase::Alarm,
    }
}

fn service_state_of(
    world: &dyn WorldRead,
    m: &ModelView,
    chip_index: &dyn Fn(&EntityKey) -> Option<usize>,
) -> ServiceState {
    let num = |signal: &str| number_of(world, &m.key, signal);
    let phase = phase_of(m.phase);
    let readiness = match phase {
        Phase::Ready => Readiness::Ready { runner: None },
        Phase::Down => Readiness::Down,
        Phase::Compiling | Phase::Loading | Phase::Alarm => Readiness::NotReady,
    };
    let has_serving = m.tokens_per_s.is_some()
        || m.running.is_some()
        || m.waiting.is_some()
        || m.kv_cache.is_some();
    let serving = has_serving.then(|| ServingStats {
        generation_tps: m.tokens_per_s.unwrap_or(0.0) as f32,
        prompt_tps: 0.0,
        completed_delta: 0,
        errored_delta: 0,
        requests_running: m.running.unwrap_or(0),
        requests_waiting: m.waiting.unwrap_or(0),
        kv_cache_usage: m.kv_cache.unwrap_or(0.0) as f32,
        ttft_avg_s: num(sig::MODEL_TTFT_S).unwrap_or(0.0) as f32,
        queue_avg_s: 0.0,
        prefill_avg_s: 0.0,
        decode_avg_s: 0.0,
        tpot_avg_s: 0.0,
        prefix_hit_rate: 0.0,
        preemptions_delta: 0,
        counters: VllmCounters::default(),
    });
    let jobs = num(sig::MODEL_MEDIA_JOBS);
    let gen_per_min = num(sig::MODEL_GENERATIONS_PER_MIN);
    let media =
        (serving.is_none() && (jobs.is_some() || gen_per_min.is_some())).then(|| MediaStats {
            generations_per_min: gen_per_min.unwrap_or(0.0) as f32,
            jobs_in_progress: jobs.unwrap_or(0.0) as u32,
            completed_delta: 0,
            errored_delta: 0,
            duration_avg_s: 0.0,
            post_avg_s: 0.0,
            pre_avg_s: 0.0,
            inference_avg_s: 0.0,
            warmup_avg_s: 0.0,
            counters: MediaCounters::default(),
        });
    let mut chips: Vec<usize> = m.chips.iter().filter_map(chip_index).collect();
    chips.sort_unstable();
    chips.dedup();
    let key = m.key.as_str();
    ServiceState {
        key: key.strip_prefix("model:").unwrap_or(key).to_string(),
        label: m.label.clone(),
        phase,
        cpu_pct: num(sig::MODEL_CPU_PCT).unwrap_or(0.0) as f32,
        rss_bytes: num(sig::MODEL_RSS_BYTES).unwrap_or(0.0) as u64,
        rss_delta: 0,
        kernel_count: num(sig::MODEL_KERNEL_COUNT).unwrap_or(0.0) as usize,
        kernel_delta: 0,
        loaded_count: num(sig::MODEL_SHARD_COUNT).unwrap_or(0.0) as usize,
        loaded_delta: 0,
        safetensors_fds: 0,
        readiness,
        top_proc: None,
        last_log: None,
        progress: m.progress,
        flat_ticks: 0,
        serving,
        media,
        port: None,
        chips,
    }
}

#[cfg(all(test, feature = "remote"))]
#[path = "compat_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "compat_inference_tests.rs"]
mod inference_tests;
