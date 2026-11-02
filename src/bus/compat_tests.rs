//! Tests for the tt-station frame helpers. The world is always fed through a
//! real bus, so the tests cover the `Raw` record path end to end.

use super::*;
use crate::backend::{build_extension, parse_extension};
use crate::workload::host_processes::ProcRow;
use crate::workload::inference_server::{Phase, Readiness, ServiceState};
use tt_toplike_bus::{Bus, BusConfig, Payload, RawFrame};

/// A two-chip `tt-smi -s` frame in the shape the existing `serve.rs` and
/// `ws.rs` tests use. Bus ids are placeholders; there are no serials or
/// hostnames. Not captured from hardware.
const FIXTURE: &str = include_str!("fixtures/tt_smi_frame.json");

fn bus() -> Bus {
    Bus::open_with_world(BusConfig::default()).unwrap()
}

fn publish_raw(bus: &Bus, source: &str, body: &str) {
    bus.publish(
        source,
        Payload::Raw(RawFrame {
            source: source.into(),
            body: body.into(),
        }),
    );
}

fn world_of(bus: &Bus) -> World {
    bus.snapshot().unwrap().world
}

fn ext() -> TtToplikeExt {
    let procs = vec![ProcRow {
        pid: 4242,
        name: "vllm".into(),
        cpu_pct: 87.5,
        mem_bytes: 1 << 30,
        inference: Some("vllm"),
        active: true,
        tt: None,
    }];
    let svc = ServiceState {
        key: "vllm-8000".into(),
        label: "vLLM".into(),
        phase: Phase::Ready,
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
        chips: vec![0, 1],
    };
    build_extension(&procs, &[svc])
}

#[test]
fn telemetry_frame_equals_inject_extension_byte_for_byte() {
    let b = bus();
    publish_raw(&b, "tt-smi", FIXTURE);
    let ext = ext();
    let got = telemetry_frame(&world_of(&b), &ext).unwrap();
    assert_eq!(got, inject_extension(FIXTURE, &ext));
    // The extension is really in there, so equality is not two plain copies.
    assert_eq!(parse_extension(&got).unwrap(), ext);
}

#[test]
fn no_raw_record_gives_none() {
    let b = bus();
    assert!(telemetry_frame(&world_of(&b), &ext()).is_none());
    assert!(relay_frame(&world_of(&b)).is_none());
    // A raw record from another source does not count.
    publish_raw(&b, "other", FIXTURE);
    assert!(telemetry_frame(&world_of(&b), &ext()).is_none());
    assert!(relay_frame(&world_of(&b)).is_none());
}

#[test]
fn newest_of_two_raw_records_wins() {
    let b = bus();
    let old = r#"{"device_info":[],"n":1}"#;
    let new = r#"{"device_info":[],"n":2}"#;
    publish_raw(&b, "tt-smi", old);
    publish_raw(&b, "tt-smi", new);
    let ext = ext();
    let w = world_of(&b);
    assert_eq!(
        telemetry_frame(&w, &ext).unwrap(),
        inject_extension(new, &ext)
    );
    assert_eq!(relay_frame(&w).unwrap(), new);
}

#[test]
fn awkward_bodies_survive_unchanged() {
    let ext = ext();
    let big_pad = "x".repeat(1_000_000);
    let bodies = [
        // Trailing newline, as a process that prints a line would leave it.
        format!("{}\n", FIXTURE.trim_end()),
        // Non-ASCII text inside a string value.
        r#"{"device_info":[],"note":"température 52°C – 温度"}"#.to_string(),
        // About 1 MB.
        format!(r#"{{"device_info":[],"pad":"{big_pad}"}}"#),
        // Non-JSON and non-object bodies, which injection leaves alone.
        "not json\n".to_string(),
        "[1,2,3]".to_string(),
        "  \n".to_string(),
    ];
    for body in bodies {
        let b = bus();
        publish_raw(&b, "tt-smi", &body);
        let w = world_of(&b);
        assert_eq!(
            telemetry_frame(&w, &ext).unwrap(),
            inject_extension(&body, &ext),
            "telemetry_frame for a {}-byte body",
            body.len()
        );
        assert_eq!(relay_frame(&w).unwrap(), body);
    }
}

/// Bodies with an embedded NUL byte, and with a lone-surrogate JSON escape
/// (the six characters `\ud800`: valid text, but not a valid character once
/// decoded).
fn nul_and_surrogate_bodies() -> Vec<String> {
    vec![
        "{\"device_info\":[],\"note\":\"a\0b\"}\0".to_string(),
        r#"{"device_info":[],"s":"\ud800","t":"x\udfff"}"#.to_string(),
    ]
}

#[test]
fn nul_and_lone_surrogate_bodies_survive_publish_and_snapshot() {
    let ext = ext();
    for body in nul_and_surrogate_bodies() {
        let b = bus();
        publish_raw(&b, "tt-smi", &body);
        let w = world_of(&b);
        assert_eq!(relay_frame(&w).unwrap(), body, "{body:?}");
        assert_eq!(
            telemetry_frame(&w, &ext).unwrap(),
            inject_extension(&body, &ext),
            "{body:?}"
        );
    }
}

/// The same bodies through the on-disk path: written to a log directory,
/// read back by `Replay`, published again into a fresh bus.
#[test]
fn awkward_bodies_survive_the_on_disk_path() {
    let ext = ext();
    let big_pad = "x".repeat(100_000);
    let mut bodies = nul_and_surrogate_bodies();
    bodies.push(format!("{}\n", FIXTURE.trim_end()));
    bodies.push(r#"{"device_info":[],"note":"température 52°C – 温度"}"#.to_string());
    bodies.push(format!(r#"{{"device_info":[],"pad":"{big_pad}"}}"#));
    bodies.push("not json\n".to_string());
    for body in bodies {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bus");
        {
            let disk = Bus::open_with_world(BusConfig {
                dir: Some(dir.clone()),
                ..BusConfig::default()
            })
            .unwrap();
            publish_raw(&disk, "tt-smi", &body);
            assert!(disk.flush(std::time::Duration::from_secs(5)));
        }
        let again = bus();
        for r in tt_toplike_bus::Replay::open(&dir).unwrap().records() {
            again.publish(&r.source, r.payload);
        }
        let w = world_of(&again);
        assert_eq!(relay_frame(&w).unwrap(), body, "{} bytes", body.len());
        assert_eq!(
            telemetry_frame(&w, &ext).unwrap(),
            inject_extension(&body, &ext)
        );
    }
}

#[test]
fn rejected_frames_come_back_as_inject_extension_returns_them() {
    let ext = ext();
    for body in ["[1,2,3]", "42", "\"text\"", "not json", "null"] {
        let b = bus();
        publish_raw(&b, "tt-smi", body);
        let got = telemetry_frame(&world_of(&b), &ext).unwrap();
        // inject_extension returns these unchanged; the helper must too.
        assert_eq!(got, body);
    }
}

#[test]
fn relay_frame_is_verbatim_and_adds_no_extension() {
    let b = bus();
    publish_raw(&b, "tt-smi", FIXTURE);
    let got = relay_frame(&world_of(&b)).unwrap();
    assert_eq!(got, FIXTURE);
    assert!(!got.contains("tt_toplike"));
    assert!(parse_extension(&got).is_none());

    // A relayed frame that already carries the origin's extension keeps it,
    // untouched, and gains nothing from the local one.
    let origin = inject_extension(FIXTURE, &ext());
    let b = bus();
    publish_raw(&b, "tt-smi", &origin);
    assert_eq!(relay_frame(&world_of(&b)).unwrap(), origin);
}
