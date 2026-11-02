//! Tests for the workload sources. Fake snapshots only: no monitor runs, no
//! hardware is touched.

use super::*;
use crate::backend::mock::MockBackend;
use crate::models::device::Device;
use crate::workload::hivemind::event::Severity;
use crate::workload::inference_server::{MediaCounters, Readiness, VllmCounters};
use crate::workload::train::detect::TrainProcess;
use tt_toplike_bus::{ModelPhase, Record, World};

const A: &str = "0000:01:00.0";
const B: &str = "0000:02:00.0";

fn map_with(devs: &[(usize, &str)]) -> Arc<IdentityMap> {
    let m = Arc::new(IdentityMap::new());
    let list: Vec<Device> = devs
        .iter()
        .map(|(i, b)| Device::new(*i, "n150".into(), (*b).into(), String::new()))
        .collect();
    m.refresh(&list);
    m
}

fn poll(s: &mut dyn Source) -> Vec<Payload> {
    let b = MockBackend::new(1);
    let mut v = Vec::new();
    s.poll(&PollCtx { backend: &b }, Instant::now(), &mut |p| v.push(p));
    v
}

fn serving(completed_total: u64, completed_delta: u32, tps: f32) -> ServingStats {
    let mut s = ServingStats::zeroed();
    s.generation_tps = tps;
    s.requests_running = 2;
    s.requests_waiting = 1;
    s.kv_cache_usage = 0.25;
    s.ttft_avg_s = 0.05;
    s.completed_delta = completed_delta;
    s.counters = VllmCounters {
        requests_succeeded_total: completed_total,
        ..Default::default()
    };
    s
}

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
        last_log: Some("Authorization: Bearer leaked-in-log".into()),
        progress: None,
        flat_ticks: 0,
        serving: None,
        media: None,
        port: Some(8000),
        chips: chips.to_vec(),
    }
}

/// Index of each payload kind in order, as short labels.
fn kinds(v: &[Payload]) -> Vec<String> {
    v.iter()
        .map(|p| match p {
            Payload::Event(Event::ModelLoading { .. }) => "loading".into(),
            Payload::Event(Event::ModelReady { .. }) => "ready".into(),
            Payload::Event(Event::ModelGone { .. }) => "gone".into(),
            Payload::Event(Event::RequestsCompleted { n, .. }) => format!("completed{n}"),
            Payload::Event(_) => "event".into(),
            Payload::Sample(_) => "sample".into(),
            _ => "other".into(),
        })
        .collect()
}

fn events(v: &[Payload]) -> Vec<&Event> {
    v.iter()
        .filter_map(|p| match p {
            Payload::Event(e) => Some(e),
            _ => None,
        })
        .collect()
}

fn sweep<'a>(v: &'a [Payload], e: &EntityKey) -> Option<&'a tt_toplike_bus::Sweep> {
    v.iter().find_map(|p| match p {
        Payload::Sweep(s) if &s.entity == e => Some(s),
        _ => None,
    })
}

fn sample<'a>(v: &'a [Payload], e: &EntityKey, name: &str) -> Option<&'a Sample> {
    v.iter().find_map(|p| match p {
        Payload::Sample(s) if &s.entity == e && s.signal == name => Some(s),
        _ => None,
    })
}

// ---- Inference ------------------------------------------------------------

#[test]
fn first_poll_for_a_new_service_emits_the_event_before_any_sample() {
    let map = map_with(&[(0, A), (1, B)]);
    let mut src = InferenceSource::new(map);
    let mut s = state("svc", Phase::Compiling, &[0, 1]);
    s.progress = Some(0.4);
    src.feed(&[s]);
    let out = poll(&mut src);
    let k = kinds(&out);
    assert_eq!(k[0], "loading", "{k:?}");
    assert!(k[1..].iter().all(|x| x == "sample"), "{k:?}");
    assert!(k.len() > 3);
    match &out[0] {
        Payload::Event(Event::ModelLoading {
            model,
            chips,
            progress,
        }) => {
            assert_eq!(model, &EntityKey::model("svc"));
            assert_eq!(chips, &vec![EntityKey::chip(A), EntityKey::chip(B)]);
            assert_eq!(*progress, Some(0.4));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn compiling_to_loading_to_ready_emits_loading_then_ready_once_each() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    let mut all = Vec::new();
    for ph in [
        Phase::Compiling,
        Phase::Compiling,
        Phase::Loading,
        Phase::Ready,
        Phase::Ready,
    ] {
        src.feed(&[state("svc", ph, &[0])]);
        all.extend(poll(&mut src));
    }
    let ev: Vec<String> = kinds(&all).into_iter().filter(|k| k != "sample").collect();
    assert_eq!(ev, vec!["loading", "ready"]);
    // The phase sample after each event refines Loading to Compiling.
    let m = EntityKey::model("svc");
    assert_eq!(
        sample(&all, &m, bsig::MODEL_PHASE).unwrap().value,
        Value::Text("compiling".into())
    );
}

#[test]
fn alarm_is_a_phase_sample_and_no_event() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    src.feed(&[state("svc", Phase::Loading, &[0])]);
    poll(&mut src);
    src.feed(&[state("svc", Phase::Alarm, &[0])]);
    let out = poll(&mut src);
    assert!(events(&out).is_empty(), "{out:?}");
    assert_eq!(
        sample(&out, &EntityKey::model("svc"), bsig::MODEL_PHASE)
            .unwrap()
            .value,
        Value::Text("alarm".into())
    );
}

#[test]
fn completed_delta_of_three_emits_requests_completed_three_and_first_tick_none() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    let mut s = state("svc", Phase::Ready, &[0]);
    // First tick: the monitor had no previous scrape, so the delta is 0; and
    // even a nonzero delta on the first scrape seen is not emitted.
    s.serving = Some(serving(10, 7, 50.0));
    src.feed(&[s.clone()]);
    let out = poll(&mut src);
    assert!(
        !kinds(&out).iter().any(|k| k.starts_with("completed")),
        "{out:?}"
    );
    s.serving = Some(serving(13, 3, 50.0));
    src.feed(&[s.clone()]);
    let out = poll(&mut src);
    let c: Vec<_> = events(&out)
        .into_iter()
        .filter(|e| matches!(e, Event::RequestsCompleted { .. }))
        .collect();
    assert_eq!(
        c,
        vec![&Event::RequestsCompleted {
            model: EntityKey::model("svc"),
            n: 3
        }]
    );
    // The same scrape fed again is not counted twice.
    src.feed(&[s]);
    let out = poll(&mut src);
    assert!(!kinds(&out).iter().any(|k| k.starts_with("completed")));
}

#[test]
fn a_vanished_service_emits_model_gone_once() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    src.feed(&[
        state("a", Phase::Ready, &[0]),
        state("b", Phase::Ready, &[]),
    ]);
    poll(&mut src);
    src.feed(&[state("b", Phase::Ready, &[])]);
    let out = poll(&mut src);
    assert_eq!(
        events(&out),
        vec![&Event::ModelGone {
            model: EntityKey::model("a")
        }]
    );
    src.feed(&[state("b", Phase::Ready, &[])]);
    assert!(events(&poll(&mut src)).is_empty());
    src.feed(&[]);
    assert_eq!(
        events(&poll(&mut src)),
        vec![&Event::ModelGone {
            model: EntityKey::model("b")
        }]
    );
}

#[test]
fn unresolved_chip_is_held_and_left_out_then_announced_when_it_resolves() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map.clone());
    src.feed(&[state("svc", Phase::Ready, &[0, 5])]);
    let out = poll(&mut src);
    match &out[0] {
        Payload::Event(Event::ModelReady { chips, .. }) => {
            assert_eq!(chips, &vec![EntityKey::chip(A)]);
        }
        other => panic!("{other:?}"),
    }
    // Index 5 was held: it is delivered when a refresh lists it.
    map.refresh(&[
        Device::new(0, "n150".into(), A.into(), String::new()),
        Device::new(5, "n150".into(), B.into(), String::new()),
    ]);
    let held = map.resolve_pending();
    assert_eq!(held.len(), 1, "held once");
    assert_eq!(held[0].key, EntityKey::chip(B));
    // With no new snapshot the source re-announces with both chips.
    let out = poll(&mut src);
    assert_eq!(
        events(&out),
        vec![&Event::ModelReady {
            model: EntityKey::model("svc"),
            chips: vec![EntityKey::chip(A), EntityKey::chip(B)]
        }]
    );
    assert!(poll(&mut src).is_empty());
}

#[test]
fn repeated_snapshot_holds_an_unresolved_index_once() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map.clone());
    for _ in 0..3 {
        src.feed(&[state("svc", Phase::Ready, &[9])]);
        poll(&mut src);
    }
    map.refresh(&[Device::new(0, "n150".into(), A.into(), String::new())]);
    assert_eq!(map.dropped(), 1);
}

#[test]
fn media_fields_are_used_when_serving_is_absent() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    let mut s = state("sky", Phase::Ready, &[0]);
    let mk = |total: u64, delta: u32| MediaStats {
        generations_per_min: 2.0,
        jobs_in_progress: 4,
        completed_delta: delta,
        errored_delta: 0,
        duration_avg_s: 1.0,
        post_avg_s: 0.0,
        pre_avg_s: 0.0,
        inference_avg_s: 0.0,
        warmup_avg_s: 0.0,
        counters: MediaCounters {
            requests_total: total,
            ..Default::default()
        },
    };
    s.media = Some(mk(1, 0));
    src.feed(&[s.clone()]);
    let out = poll(&mut src);
    let m = EntityKey::model("sky");
    assert_eq!(
        sample(&out, &m, sig::MODEL_MEDIA_JOBS).unwrap().value,
        Value::U64(4)
    );
    assert!(sample(&out, &m, bsig::MODEL_TOKENS_PER_S).is_none());
    s.media = Some(mk(3, 2));
    src.feed(&[s]);
    let out = poll(&mut src);
    assert!(kinds(&out).contains(&"completed2".to_string()));
}

#[test]
fn world_fed_the_records_shows_the_model_with_phase_tokens_and_chips() {
    let map = map_with(&[(0, A), (1, B)]);
    let mut src = InferenceSource::new(map);
    let mut w = World::new();
    let mut seq = 0u64;
    let mut apply = |w: &mut World, out: Vec<Payload>| {
        for p in out {
            seq += 1;
            w.apply(&Record {
                seq,
                t_mono_ns: seq * 1_000_000,
                t_wall_ms: seq,
                source: "inference".into(),
                payload: p,
            });
        }
    };
    src.feed(&[state("svc", Phase::Compiling, &[0, 1])]);
    apply(&mut w, poll(&mut src));
    let v = w.models();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].phase, ModelPhase::Compiling);
    assert_eq!(v[0].label, "svc label");

    let mut s = state("svc", Phase::Ready, &[0, 1]);
    s.serving = Some(serving(5, 0, 42.5));
    src.feed(&[s]);
    apply(&mut w, poll(&mut src));
    let v = w.models();
    assert_eq!(v[0].phase, ModelPhase::Ready);
    assert_eq!(v[0].tokens_per_s, Some(42.5));
    assert_eq!(v[0].running, Some(2));
    assert_eq!(v[0].kv_cache, Some(0.25));
    assert_eq!(v[0].chips, vec![EntityKey::chip(A), EntityKey::chip(B)]);
    assert_eq!(
        w.model_on_chip(&EntityKey::chip(B)).unwrap().key,
        EntityKey::model("svc")
    );

    src.feed(&[]);
    apply(&mut w, poll(&mut src));
    assert!(w.models().is_empty());
}

#[test]
fn inference_records_never_carry_the_log_line() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    src.feed(&[state("svc", Phase::Ready, &[0])]);
    let out = poll(&mut src);
    assert!(sample(&out, &EntityKey::model("svc"), bsig::MODEL_LABEL).is_some());
    let text = serde_json::to_string(&out).unwrap();
    assert!(!text.contains("leaked-in-log"));
}

#[test]
fn nothing_fed_means_nothing_emitted() {
    let map = map_with(&[(0, A)]);
    assert!(poll(&mut InferenceSource::new(map.clone())).is_empty());
    assert!(poll(&mut TrainingSource::new(map.clone())).is_empty());
    assert!(poll(&mut HivemindSource::new(map.clone())).is_empty());
    assert!(poll(&mut ProcessSource::new(map)).is_empty());
}

// ---- Training -------------------------------------------------------------

fn train_state() -> TrainState {
    let mut st = TrainState::new();
    st.proc = Some(TrainProcess {
        pid: 4242,
        binary: "nano_gpt".into(),
        config_path: None,
        start_time: Some(99),
    });
    st.step = 120;
    st.max_steps = 1000;
    st.loss = Some(2.5);
    st.step_ms = 310.0;
    st.step_time_source = StepTimeSource::Reported;
    st
}

#[test]
fn training_run_emits_step_loss_and_chips_from_the_process_join() {
    let map = map_with(&[(0, A), (1, B)]);
    let mut src = TrainingSource::new(map);
    let procs = vec![(17u32, vec![0usize]), (4242u32, vec![1usize, 0usize])];
    src.feed(&train_state(), Some(&procs));
    let out = poll(&mut src);
    let run = EntityKey::training_run("nano_gpt:4242@99");
    assert_eq!(
        sample(&out, &run, sig::TRAIN_STEP).unwrap().value,
        Value::U64(120)
    );
    assert_eq!(
        sample(&out, &run, sig::TRAIN_MAX_STEPS).unwrap().value,
        Value::U64(1000)
    );
    assert_eq!(
        sample(&out, &run, sig::TRAIN_LOSS).unwrap().value,
        Value::F64(2.5)
    );
    assert_eq!(
        sample(&out, &run, sig::TRAIN_STEP_MS).unwrap().prov,
        Provenance::Measured
    );
    assert_eq!(
        sample(&out, &run, sig::TRAIN_CHIPS).unwrap().value,
        Value::Text(format!("chip:{B},chip:{A}"))
    );
}

#[test]
fn training_without_a_process_list_has_no_chips_and_nan_loss_is_absent() {
    let map = map_with(&[(0, A)]);
    let mut src = TrainingSource::new(map);
    let mut st = train_state();
    st.loss = Some(f32::NAN);
    src.feed(&st, None);
    let out = poll(&mut src);
    let run = EntityKey::training_run("nano_gpt:4242@99");
    assert!(sample(&out, &run, sig::TRAIN_STEP).is_some());
    assert!(sample(&out, &run, sig::TRAIN_LOSS).is_none());
    assert!(sample(&out, &run, sig::TRAIN_CHIPS).is_none());
}

// ---- Hivemind -------------------------------------------------------------

fn sniff(source: SniffSource, kind: EventKind, device: Option<u8>, text: &str) -> SniffEvent {
    SniffEvent {
        ts: Instant::now(),
        source,
        device,
        severity: Severity::Info,
        kind,
        text: text.into(),
        origin: format!("origin {text}"),
    }
}

#[test]
fn hivemind_events_map_to_compile_reset_and_spark_with_resolved_chips() {
    let map = map_with(&[(0, A), (1, B)]);
    let mut src = HivemindSource::new(map);
    src.feed(&[
        sniff(SniffSource::TtMetal, EventKind::Compile, Some(1), "x"),
        sniff(SniffSource::Driver, EventKind::Reset, Some(0), "x"),
        sniff(SniffSource::Vllm, EventKind::Log, Some(7), "x"),
        sniff(SniffSource::Ttnn, EventKind::Fd, None, "x"),
    ]);
    let out = poll(&mut src);
    assert_eq!(
        events(&out),
        vec![
            &Event::KernelCompiled {
                chip: Some(EntityKey::chip(B)),
                source: "metal".into()
            },
            &Event::Reset {
                chips: vec![EntityKey::chip(A)]
            },
            &Event::Spark {
                chip: None,
                kind: "vLLM.log".into()
            },
            &Event::Spark {
                chip: None,
                kind: "ttnn.fd".into()
            },
        ]
    );
    assert!(poll(&mut src).is_empty(), "each event once");
}

#[test]
fn hivemind_sparks_never_carry_the_raw_log_text() {
    let map = map_with(&[(0, A)]);
    let mut src = HivemindSource::new(map);
    let secret = "HF_TOKEN=hf_sparksecret --api-key sk-sparksecret";
    let all_kinds = [
        EventKind::Log,
        EventKind::Compile,
        EventKind::Process,
        EventKind::Fd,
        EventKind::DriverMsg,
        EventKind::Emission,
        EventKind::Inspector,
        EventKind::Reset,
    ];
    let evs: Vec<SniffEvent> = all_kinds
        .iter()
        .map(|k| sniff(SniffSource::Unknown, *k, Some(0), secret))
        .collect();
    src.feed(&evs);
    let out = poll(&mut src);
    assert_eq!(out.len(), all_kinds.len());
    let text = serde_json::to_string(&out).unwrap();
    assert!(!text.contains("sparksecret"), "{text}");
    assert!(!text.contains("origin"), "{text}");
}

#[test]
fn hivemind_queue_is_bounded() {
    let map = map_with(&[]);
    let mut src = HivemindSource::new(map);
    let e = sniff(SniffSource::Host, EventKind::Log, None, "x");
    let evs = vec![e; HIVEMIND_QUEUE + 10];
    src.feed(&evs);
    assert_eq!(src.dropped(), 10);
    assert_eq!(poll(&mut src).len(), HIVEMIND_QUEUE);
}

// ---- Processes and the log (Review Focus 3) -------------------------------

fn secret_procs() -> Vec<ProcSnapshot> {
    vec![
        ProcSnapshot {
            pid: 100,
            start_ticks: 555,
            name: "python".into(),
            cmdline: "python serve.py --api-key sk-abc HF_TOKEN=hf_xyz --port 8000".into(),
            cpu_pct: 50.0,
            rss_bytes: 2048,
            device_indices: vec![0, 3],
            uses_tt: None,
        },
        ProcSnapshot {
            pid: 101,
            start_ticks: 556,
            name: "curl".into(),
            cmdline: r#"curl -H "Authorization: Bearer tok-bearer-secret" --api-key=sk-eq"#.into(),
            cpu_pct: 0.0,
            rss_bytes: 10,
            device_indices: vec![],
            uses_tt: None,
        },
    ]
}

#[test]
fn process_records_carry_cpu_rss_redacted_cmdline_and_resolved_chips() {
    let map = map_with(&[(0, A)]);
    let mut src = ProcessSource::new(map.clone());
    src.feed(&secret_procs());
    let out = poll(&mut src);
    let e = EntityKey::process(100, 555);
    assert_eq!(
        sample(&out, &e, sig::PROC_CMDLINE).unwrap().value,
        Value::Text("python serve.py --api-key <redacted> HF_TOKEN=<redacted> --port 8000".into())
    );
    assert_eq!(
        sample(&out, &e, sig::PROC_DEVICE).unwrap().value,
        Value::Text(format!("chip:{A}"))
    );
    let sw = sweep(&out, &e).unwrap();
    assert_eq!(sw.signals, vec![sig::PROC_CPU_PCT, sig::PROC_RSS_BYTES]);
    assert_eq!(sw.values, vec![Value::F64(50.0), Value::U64(2048)]);
    assert_eq!(sw.prov, Provenance::Measured);
    // Index 3 did not resolve: it was held and no key was made for it.
    map.refresh(&[Device::new(0, "n150".into(), A.into(), String::new())]);
    assert_eq!(map.dropped(), 1);
}

#[test]
fn a_log_built_from_a_process_list_with_secrets_never_contains_them() {
    use tt_toplike_bus::{Bus, BusConfig};
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::open(BusConfig {
        dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let map = map_with(&[(0, A)]);
    let mut procs = ProcessSource::new(map.clone());
    procs.feed(&secret_procs());
    for p in poll(&mut procs) {
        bus.publish("processes", p);
    }
    // The remote path carries the same command lines.
    #[cfg(feature = "remote")]
    {
        let mut remote = RemoteSource::new(map);
        let rp: Vec<crate::backend::RemoteProc> = secret_procs()
            .into_iter()
            .map(|p| crate::backend::RemoteProc {
                pid: p.pid,
                name: p.name,
                cmd: p.cmdline,
                uses_tt: true,
                cpu_pct: p.cpu_pct,
                mem_bytes: p.rss_bytes,
            })
            .collect();
        remote.feed(None, Some(&rp));
        for p in poll(&mut remote) {
            bus.publish("remote", p);
        }
    }
    assert!(bus.flush(std::time::Duration::from_secs(5)));
    let mut bytes = Vec::new();
    for f in std::fs::read_dir(dir.path()).unwrap() {
        bytes.extend(std::fs::read(f.unwrap().path()).unwrap());
    }
    let text = String::from_utf8_lossy(&bytes);
    // The instrument can see what was written: the redacted line is there.
    assert!(
        text.contains("--port 8000") && text.contains("process:100@555"),
        "log not readable as text: {text}"
    );
    for secret in ["sk-abc", "hf_xyz", "tok-bearer-secret", "sk-eq"] {
        assert!(!text.contains(secret), "{secret} reached the log");
    }
}

// ---- Fix round 1 -------------------------------------------------------------

#[test]
fn second_identical_process_feed_emits_only_the_numeric_sweeps() {
    let map = map_with(&[(0, A)]);
    let mut src = ProcessSource::new(map);
    let procs = secret_procs();
    src.feed(&procs);
    let first = poll(&mut src);
    assert!(first.iter().any(|p| matches!(p, Payload::Sample(_))));
    src.feed(&procs);
    let second = poll(&mut src);
    assert_eq!(second.len(), procs.len(), "{second:?}");
    assert!(second.iter().all(|p| matches!(p, Payload::Sweep(_))));
    assert_eq!(src.redactions(), 2, "one redaction per distinct cmdline");
}

#[test]
fn a_changed_cmdline_is_re_emitted_and_re_redacted() {
    let map = map_with(&[(0, A)]);
    let mut src = ProcessSource::new(map);
    let mut procs = secret_procs();
    src.feed(&procs);
    poll(&mut src);
    procs[0].cmdline = "python serve.py --token tk-changed1 --port 9000".into();
    src.feed(&procs);
    let out = poll(&mut src);
    let e = EntityKey::process(100, 555);
    assert_eq!(
        sample(&out, &e, sig::PROC_CMDLINE).unwrap().value,
        Value::Text("python serve.py --token <redacted> --port 9000".into())
    );
    // Name and device did not change, so they are not sent again.
    assert!(sample(&out, &e, sig::PROC_NAME).is_none());
    assert!(sample(&out, &e, sig::PROC_DEVICE).is_none());
    assert_eq!(src.redactions(), 3);
}

#[test]
fn a_vanished_process_state_is_pruned_and_a_return_is_sent_in_full() {
    let map = map_with(&[(0, A)]);
    let mut src = ProcessSource::new(map);
    let procs = secret_procs();
    src.feed(&procs);
    poll(&mut src);
    assert_eq!(src.tracked(), 2);
    src.feed(&procs[1..]);
    poll(&mut src);
    assert_eq!(src.tracked(), 1);
    src.feed(&procs);
    let out = poll(&mut src);
    let e = EntityKey::process(100, 555);
    assert!(sample(&out, &e, sig::PROC_CMDLINE).is_some());
}

#[test]
fn long_text_is_truncated_after_redaction() {
    let map = map_with(&[(0, A)]);
    let mut src = ProcessSource::new(map);
    let long = format!("python x.py --api-key sk-long1 {}", "a".repeat(5000));
    src.feed(&[ProcSnapshot {
        pid: 1,
        start_ticks: 1,
        name: "n".repeat(900),
        cmdline: long,
        ..Default::default()
    }]);
    let out = poll(&mut src);
    let e = EntityKey::process(1, 1);
    for name in [sig::PROC_CMDLINE, sig::PROC_NAME] {
        match &sample(&out, &e, name).unwrap().value {
            Value::Text(t) => assert_eq!(t.chars().count(), MAX_TEXT, "{name}"),
            v => panic!("{v:?}"),
        }
    }
    let text = serde_json::to_string(&out).unwrap();
    assert!(!text.contains("sk-long1"));
}

#[test]
fn service_label_and_key_are_redacted() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    let mut s = state("svc HF_TOKEN=hf_keysecret1", Phase::Ready, &[0]);
    s.label = format!("serve --api-key sk-labelsecret1 {}", "b".repeat(900));
    src.feed(&[s]);
    let out = poll(&mut src);
    let text = serde_json::to_string(&out).unwrap();
    assert!(!text.contains("keysecret1"), "{text}");
    assert!(!text.contains("labelsecret1"), "{text}");
    let m = EntityKey::model("svc HF_TOKEN=<redacted>");
    assert!(events(&out).contains(&&Event::ModelReady {
        model: m.clone(),
        chips: vec![EntityKey::chip(A)]
    }));
    match &sample(&out, &m, bsig::MODEL_LABEL).unwrap().value {
        Value::Text(t) => assert_eq!(t.chars().count(), MAX_TEXT),
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_service_first_seen_down_or_alarm_sends_nothing_until_announced() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    for ph in [Phase::Down, Phase::Alarm] {
        src.feed(&[state("svc", ph, &[0])]);
        assert!(poll(&mut src).is_empty(), "{ph:?}");
    }
    src.feed(&[state("svc", Phase::Loading, &[0])]);
    let out = poll(&mut src);
    assert_eq!(kinds(&out)[0], "loading");
    assert!(out.len() > 1);
    // Once announced, an Alarm is a phase sample.
    src.feed(&[state("svc", Phase::Alarm, &[0])]);
    let out = poll(&mut src);
    assert!(sample(&out, &EntityKey::model("svc"), bsig::MODEL_PHASE).is_some());
}

#[test]
fn a_service_that_leaves_and_returns_is_announced_again() {
    let map = map_with(&[(0, A)]);
    let mut src = InferenceSource::new(map);
    src.feed(&[state("svc", Phase::Ready, &[0])]);
    poll(&mut src);
    src.feed(&[]);
    assert_eq!(kinds(&poll(&mut src)), vec!["gone"]);
    src.feed(&[state("svc", Phase::Ready, &[0])]);
    let out = poll(&mut src);
    assert_eq!(kinds(&out)[0], "ready");
}

#[test]
fn a_log_built_from_property_command_lines_never_contains_their_secrets() {
    use crate::bus::redact::tests::secret_line;
    use tt_toplike_bus::{Bus, BusConfig};
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::open(BusConfig {
        dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let map = map_with(&[(0, A)]);
    let mut src = ProcessSource::new(map.clone());
    let mut secrets = Vec::new();
    let mut procs = Vec::new();
    for seed in 0..400u64 {
        let (line, s) = secret_line(seed + 10_000);
        secrets.extend(s);
        procs.push(ProcSnapshot {
            pid: seed as u32 + 1,
            start_ticks: 7,
            name: "python".into(),
            cmdline: line,
            ..Default::default()
        });
    }
    src.feed(&procs);
    for p in poll(&mut src) {
        bus.publish("processes", p);
    }
    assert!(bus.flush(std::time::Duration::from_secs(5)));
    let mut bytes = Vec::new();
    for f in std::fs::read_dir(dir.path()).unwrap() {
        bytes.extend(std::fs::read(f.unwrap().path()).unwrap());
    }
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("process:400@7") && text.contains("<redacted>"));
    for s in &secrets {
        assert!(!text.contains(s.as_str()), "{s} reached the log");
    }
}

// ---- Remote -----------------------------------------------------------------

#[cfg(feature = "remote")]
mod remote_tests {
    use super::*;
    use crate::backend::{RemoteInference, RemoteServing};

    fn remote_serving(tps: f32, delta: u32) -> RemoteServing {
        RemoteServing {
            generation_tps: tps,
            prompt_tps: 0.0,
            requests_running: 1,
            requests_waiting: 0,
            kv_cache_usage: 0.5,
            ttft_avg_s: 0.0,
            queue_avg_s: 0.0,
            prefill_avg_s: 0.0,
            decode_avg_s: 0.0,
            tpot_avg_s: 0.0,
            completed_delta: delta,
            errored_delta: 0,
            prefix_hit_rate: 0.0,
            preemptions_delta: 0,
        }
    }

    fn ri(phase: &str, serving: Option<RemoteServing>) -> RemoteInference {
        RemoteInference {
            key: "svc".into(),
            label: "Llama".into(),
            phase: phase.into(),
            progress: None,
            serving,
            media: None,
            port: Some(8000),
            chips: vec![0],
        }
    }

    #[test]
    fn remote_inference_looks_like_local_inference_on_the_bus() {
        let map = map_with(&[(0, A)]);
        let mut src = RemoteSource::new(map);
        src.feed(Some(&[ri("loading", None)]), None);
        let out = poll(&mut src);
        assert_eq!(kinds(&out)[0], "loading");
        src.feed(Some(&[ri("ready", Some(remote_serving(10.0, 0)))]), None);
        let out = poll(&mut src);
        assert_eq!(kinds(&out)[0], "ready");
        let m = EntityKey::model("svc");
        assert_eq!(
            sample(&out, &m, bsig::MODEL_TOKENS_PER_S).unwrap().value,
            Value::F64(10.0)
        );
        // No TTFT reading yet (0 means none): no sample.
        assert!(sample(&out, &m, sig::MODEL_TTFT_S).is_none());
        src.feed(Some(&[ri("ready", Some(remote_serving(11.0, 3)))]), None);
        assert!(kinds(&poll(&mut src)).contains(&"completed3".to_string()));
        // A frame without the inference list declares nothing gone.
        src.feed(None, None);
        assert!(events(&poll(&mut src)).is_empty());
        // An authoritative empty list does.
        src.feed(Some(&[]), None);
        assert_eq!(kinds(&poll(&mut src)), vec!["gone"]);
    }

    #[test]
    fn remote_label_and_key_are_redacted() {
        let map = map_with(&[(0, A)]);
        let mut src = RemoteSource::new(map);
        let mut r = ri("ready", None);
        r.key = "svc --token tk-rkey1".into();
        r.label = "Llama --api-key sk-rlabel1".into();
        src.feed(Some(&[r]), None);
        let text = serde_json::to_string(&poll(&mut src)).unwrap();
        assert!(
            !text.contains("tk-rkey1") && !text.contains("sk-rlabel1"),
            "{text}"
        );
        assert!(text.contains("model:svc --token <redacted>"), "{text}");
    }

    #[test]
    fn unknown_remote_phase_emits_no_event_and_no_phase_sample() {
        let map = map_with(&[(0, A)]);
        let mut src = RemoteSource::new(map);
        src.feed(Some(&[ri("loading", None)]), None);
        poll(&mut src);
        src.feed(Some(&[ri("warming", None)]), None);
        let out = poll(&mut src);
        let m = EntityKey::model("svc");
        assert!(sample(&out, &m, bsig::MODEL_LABEL).is_some(), "{out:?}");
        assert!(events(&out).is_empty());
        assert!(sample(&out, &m, bsig::MODEL_PHASE).is_none());
    }
}
