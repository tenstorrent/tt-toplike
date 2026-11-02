//! Tests for [`world_to_service_states`]: a `ServiceState` goes through the
//! real `InferenceSource` and a real bus (inside a `BusHost`), and comes
//! back out of the world. `MockBackend` only; no hardware.

use super::*;
use crate::backend::mock::MockBackend;
use crate::backend::TelemetryBackend;
use crate::bus::gozer::GozerSource;
use crate::bus::host::BusHost;
use crate::workload::inference_server::{MediaCounters, VllmCounters};
use std::time::Instant;
use tt_toplike_bus::{BusConfig, HeaderKind, LocalView};

/// A two-chip mock backend. Its chips are `0000:01:00.0` (index 0) and
/// `0000:02:00.0` (index 1).
fn mock() -> MockBackend {
    let mut b = MockBackend::new(2);
    b.init().unwrap();
    b.update().unwrap();
    b
}

/// A ServiceState with every field set to something that is not its default,
/// so a field that is dropped on the way shows up.
fn state(key: &str, phase: Phase, chips: &[usize]) -> ServiceState {
    ServiceState {
        key: key.into(),
        label: format!("{key} label"),
        phase,
        cpu_pct: 12.5,
        rss_bytes: 1_000,
        rss_delta: 0,
        kernel_count: 40,
        kernel_delta: 0,
        loaded_count: 3,
        loaded_delta: 0,
        safetensors_fds: 0,
        readiness: Readiness::NotReady,
        top_proc: None,
        last_log: None,
        progress: None,
        flat_ticks: 0,
        serving: None,
        media: None,
        port: Some(8000),
        chips: chips.to_vec(),
    }
}

fn serving(tps: f32, running: u32, waiting: u32, kv: f32) -> ServingStats {
    let mut s = ServingStats::zeroed();
    s.generation_tps = tps;
    s.requests_running = running;
    s.requests_waiting = waiting;
    s.kv_cache_usage = kv;
    s.ttft_avg_s = 0.05;
    s.counters = VllmCounters {
        requests_succeeded_total: 10,
        ..Default::default()
    };
    s
}

fn media(gen_per_min: f32, jobs: u32) -> MediaStats {
    MediaStats {
        generations_per_min: gen_per_min,
        jobs_in_progress: jobs,
        completed_delta: 0,
        errored_delta: 0,
        duration_avg_s: 0.0,
        post_avg_s: 0.0,
        pre_avg_s: 0.0,
        inference_avg_s: 0.0,
        warmup_avg_s: 0.0,
        counters: MediaCounters::default(),
    }
}

/// The world after the host has published `rows` once, as a view reads it.
pub(crate) fn world_from(rows: &[ServiceState], synthetic: bool) -> LocalView {
    world_after(&[rows], synthetic)
}

/// The world after the host has published each snapshot in `feeds`, one
/// tick each, in order.
fn world_after(feeds: &[&[ServiceState]], synthetic: bool) -> LocalView {
    let b = mock();
    // A fed gozer source never runs the `gozer` command.
    let mut host = BusHost::with_gozer(BusConfig::default(), &b, GozerSource::fed()).unwrap();
    host.set_synthetic(synthetic);
    for rows in feeds {
        host.feed_inference(rows);
        host.tick(&b, Instant::now());
    }
    let (seq, world) = host.with_world(|w, seq| (seq, w.clone()));
    LocalView {
        seq,
        world,
        now_mono_ns: 0,
        header_kind: HeaderKind::Live,
    }
}

/// The mock backend's chips as a resolver.
fn mock_index(k: &EntityKey) -> Option<usize> {
    match k.as_str() {
        "chip:0000:01:00.0" => Some(0),
        "chip:0000:02:00.0" => Some(1),
        _ => None,
    }
}

/// The fields the bus carries, side by side.
fn carried(s: &ServiceState) -> String {
    format!(
        "key={} label={} phase={:?} progress={:?} chips={:?} \
         tps={:?} running={:?} waiting={:?} kv={:?} ttft={:?} \
         jobs={:?} gen_per_min={:?} cpu={} rss={} kernels={} shards={}",
        s.key,
        s.label,
        s.phase,
        s.progress,
        s.chips,
        s.serving.as_ref().map(|v| v.generation_tps),
        s.serving.as_ref().map(|v| v.requests_running),
        s.serving.as_ref().map(|v| v.requests_waiting),
        s.serving.as_ref().map(|v| v.kv_cache_usage),
        s.serving.as_ref().map(|v| v.ttft_avg_s),
        s.media.as_ref().map(|m| m.jobs_in_progress),
        s.media.as_ref().map(|m| m.generations_per_min),
        s.cpu_pct,
        s.rss_bytes,
        s.kernel_count,
        s.loaded_count,
    )
}

#[test]
fn a_vllm_service_round_trips_through_the_bus() {
    let mut s = state("vllm-8000", Phase::Ready, &[1, 0]);
    s.serving = Some(serving(42.0, 2, 3, 0.25));
    let w = world_from(std::slice::from_ref(&s), false);
    let back = world_to_service_states_with(&w, &mock_index);
    assert_eq!(back.len(), 1);
    let mut want = s.clone();
    want.chips = vec![0, 1]; // sorted, as the monitor sorts them
    assert_eq!(carried(&back[0]), carried(&want));
    // Not carried: the port, and the counters behind the rates.
    assert_eq!(back[0].port, None);
    assert_eq!(back[0].serving.unwrap().counters, VllmCounters::default());
    assert!(back[0].media.is_none());
    assert_eq!(back[0].readiness, Readiness::Ready { runner: None });
}

#[test]
fn a_media_service_round_trips_through_the_bus() {
    let mut s = state("skyreels-8001", Phase::Ready, &[1]);
    s.media = Some(media(6.0, 2));
    let w = world_from(std::slice::from_ref(&s), false);
    let back = world_to_service_states_with(&w, &mock_index);
    assert_eq!(back.len(), 1);
    assert_eq!(carried(&back[0]), carried(&s));
    assert!(back[0].serving.is_none());
}

#[test]
fn a_loading_service_keeps_its_phase_and_progress() {
    let mut s = state("vllm-8000", Phase::Loading, &[0]);
    s.progress = Some(0.4);
    let w = world_from(std::slice::from_ref(&s), false);
    let back = world_to_service_states_with(&w, &mock_index);
    assert_eq!(carried(&back[0]), carried(&s));
    assert_eq!(back[0].readiness, Readiness::NotReady);
}

#[test]
fn a_compiling_service_round_trips_through_the_bus() {
    let mut s = state("vllm-8000", Phase::Compiling, &[0, 1]);
    s.progress = Some(0.1);
    let w = world_from(std::slice::from_ref(&s), false);
    let back = world_to_service_states_with(&w, &mock_index);
    assert_eq!(back.len(), 1);
    assert_eq!(carried(&back[0]), carried(&s));
    assert_eq!(back[0].readiness, Readiness::NotReady);
}

/// A service is announced only once it compiles, loads or is ready; a
/// later `Down` is a phase sample on the model the world already has.
#[test]
fn a_service_that_goes_down_round_trips_through_the_bus() {
    let mut up = state("vllm-8000", Phase::Ready, &[0]);
    up.serving = Some(serving(42.0, 2, 3, 0.25));
    let mut down = state("vllm-8000", Phase::Down, &[0]);
    down.serving = Some(serving(0.0, 0, 0, 0.0));
    let w = world_after(
        &[std::slice::from_ref(&up), std::slice::from_ref(&down)],
        false,
    );
    let back = world_to_service_states_with(&w, &mock_index);
    assert_eq!(back.len(), 1);
    assert_eq!(carried(&back[0]), carried(&down));
    assert_eq!(back[0].readiness, Readiness::Down);
}

#[test]
fn without_a_resolver_the_chips_are_empty() {
    let mut s = state("vllm-8000", Phase::Ready, &[0, 1]);
    s.serving = Some(serving(42.0, 2, 3, 0.25));
    let w = world_from(&[s], false);
    let back = world_to_service_states(&w);
    assert_eq!(back.len(), 1);
    assert!(back[0].chips.is_empty());
}

#[test]
fn a_world_with_no_models_gives_no_rows() {
    let w = world_from(&[], false);
    assert!(world_to_service_states(&w).is_empty());
}

#[test]
fn estimated_readings_are_reported_per_model() {
    let mut s = state("vllm-8000", Phase::Ready, &[0]);
    s.serving = Some(serving(42.0, 2, 3, 0.25));
    use crate::animation::die3d::overlay::model_text_is_estimated;
    let real = world_from(std::slice::from_ref(&s), false);
    assert!(!model_text_is_estimated(&real, &real.models()[0]));
    let mock = world_from(&[s], true);
    assert!(model_text_is_estimated(&mock, &mock.models()[0]));
}
