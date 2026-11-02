//! Tests for the TUI's bus. `MockBackend` (wrapped where a raw `tt-smi`
//! frame is needed), temp directories and sockets in them. No hardware, no
//! gozer command, no terminal.

use super::*;
use crate::backend::mock::MockBackend;
use crate::bus::signals as sig;
use crate::cli::{BackendType, Cli};
use crate::error::BackendResult;
use crate::models::device::Device;
use crate::models::telemetry::{SmbusTelemetry, Telemetry};
use clap::Parser;
use std::collections::BTreeSet;
use tt_toplike_bus::{EntityKey, Event, HeaderKind, Replay, World, WorldRead};

const A: &str = "0000:01:00.0";
const B: &str = "0000:02:00.0";

fn mock(n: usize) -> MockBackend {
    let mut b = MockBackend::new(n);
    b.init().unwrap();
    b.update().unwrap();
    b
}

fn cli(args: &[&str]) -> Cli {
    let mut v = vec!["tt-toplike-tui"];
    v.extend_from_slice(args);
    Cli::try_parse_from(v).unwrap()
}

/// Options for a mock TUI (`--mock` plus `args`).
fn mock_opts(args: &[&str]) -> TuiBusOptions {
    let mut v = vec!["--mock", "2"];
    v.extend_from_slice(args);
    TuiBusOptions::from_cli(&cli(&v), BackendType::Mock, "Mock (2 devices)")
}

/// A local TUI bus on `backend`, memory only, no probe, no gozer.
fn local_bus(backend: &dyn TelemetryBackend) -> TuiBus {
    TuiBus::start(&mock_opts(&[]), backend).unwrap()
}

/// The `ViewChanged` events in `w`, in order.
fn view_events(w: &World) -> Vec<String> {
    w.events()
        .iter()
        .filter_map(|(_, e)| match e {
            Event::ViewChanged { view } => Some(view.clone()),
            _ => None,
        })
        .collect()
}

// ---- the data tick ----------------------------------------------------------

/// The loop's shape: a render frame every 16 ms, and the data tick checked
/// on every pass. The host is ticked once per data tick, never per frame.
/// Returns (frames, data ticks, host ticks, bus head).
fn run_loop(frames: u32, interval_ms: u64) -> (u32, u64, u64, u64) {
    let mut b = mock(2);
    let mut bus = local_bus(&b);
    let start = Instant::now();
    let mut last = start;
    let mut data_ticks = 0;
    for f in 1..=frames {
        let now = start + Duration::from_millis(16 * u64::from(f));
        // A render frame: reads the bus once, as a view will.
        std::hint::black_box(bus.read().seq());
        if run_data_tick(
            &mut last,
            Duration::from_millis(interval_ms),
            now,
            &mut b,
            Some(&mut bus),
        ) {
            data_ticks += 1;
        }
    }
    let head = bus.host().unwrap().bus().head();
    (frames, data_ticks, bus.ticks(), head)
}

#[test]
fn the_host_ticks_on_the_data_tick_and_not_per_render_frame() {
    let (frames, data_ticks, host_ticks, head) = run_loop(120, 100);
    // A data tick is due on the first frame at least 100 ms after the last
    // one: every 7th frame (112 ms), so 17 in 120 frames.
    assert_eq!(data_ticks, 17);
    assert_eq!(host_ticks, data_ticks, "host ticks follow data ticks");
    assert!(host_ticks < u64::from(frames));
    assert!(head > 0, "the ticks published");
}

/// The host's sweeps reach the world through the data tick.
#[test]
fn a_data_tick_publishes_every_chip() {
    let mut b = mock(2);
    let mut bus = local_bus(&b);
    let mut last = Instant::now() - Duration::from_secs(1);
    assert!(run_data_tick(
        &mut last,
        Duration::from_millis(100),
        Instant::now(),
        &mut b,
        Some(&mut bus)
    ));
    let v = bus.read();
    for c in [A, B] {
        assert!(
            v.reading(&EntityKey::chip(c), sig::TDP_W).is_some(),
            "no reading for {c}"
        );
    }
}

// ---- ViewChanged ---------------------------------------------------------------

#[test]
fn view_changed_is_published_once_per_change_and_not_on_a_redraw() {
    let b = mock(2);
    let mut bus = local_bus(&b);
    for v in [
        "insights", "insights", "insights", "grid", "grid", "die3d", "die3d", "insights",
    ] {
        bus.note_view(v);
    }
    let w = bus.host().unwrap().world();
    assert_eq!(view_events(&w), ["insights", "grid", "die3d", "insights"]);
    assert_eq!(bus.view_changes(), 4);
}

// ---- --record, --bus-socket ---------------------------------------------------------

/// `--record DIR` writes a segment that `Replay` reads back to the same
/// world the TUI held.
#[test]
fn record_writes_a_segment_replay_reads_back() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bus");
    let opts = mock_opts(&["--record", dir.to_str().unwrap()]);
    assert_eq!(opts.attach, Attach::Off);
    let mut b = mock(2);
    let mut bus = TuiBus::start(&opts, &b).unwrap();
    let mut last = Instant::now() - Duration::from_secs(1);
    let t0 = Instant::now();
    for i in 0..3 {
        let now = t0 + Duration::from_millis(200 * i);
        last = last.min(now - Duration::from_millis(100));
        assert!(run_data_tick(
            &mut last,
            Duration::from_millis(100),
            now,
            &mut b,
            Some(&mut bus)
        ));
    }
    bus.note_view("insights");
    let host = bus.host().unwrap();
    assert!(host.bus().flush(Duration::from_secs(5)));
    let live = host.world();
    drop(bus);

    let mut replay = Replay::open(&dir).unwrap();
    let header = replay.header().cloned().unwrap();
    assert_eq!(header.kind, HeaderKind::Live);
    assert_eq!(
        header.tool_versions.get("mode").map(String::as_str),
        Some("mock")
    );
    let mut w = World::new();
    for r in replay.records() {
        w.apply(&r);
    }
    assert_eq!(w, live, "the recorded log replays to the TUI's world");
    assert_eq!(view_events(&w), ["insights"]);
}

/// `--bus-socket PATH` serves the TUI's bus: a client reads a snapshot
/// with both chips.
#[cfg(unix)]
#[test]
fn bus_socket_serves_a_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let opts = mock_opts(&["--bus-socket", sock.to_str().unwrap()]);
    assert_eq!(opts.attach, Attach::Off, "--bus-socket hosts its own bus");
    let mut b = mock(2);
    let mut bus = TuiBus::start(&opts, &b).unwrap();
    let mut last = Instant::now() - Duration::from_secs(1);
    run_data_tick(
        &mut last,
        Duration::from_millis(100),
        Instant::now(),
        &mut b,
        Some(&mut bus),
    );
    let snap = tt_toplike_bus::SocketClient::connect(&sock)
        .unwrap()
        .snapshot()
        .unwrap();
    for c in [A, B] {
        assert!(snap
            .world
            .reading(&EntityKey::chip(c), sig::TDP_W)
            .is_some());
    }
    assert_eq!(bus.bus_label(), "bus: local");
    drop(bus);
    assert!(!sock.exists(), "the socket file was removed");
}

// ---- attach -------------------------------------------------------------------------

/// A headless-style live host (`mode live`) serving `sock`, ticked once
/// against `backend`.
#[cfg(unix)]
fn serving_host(
    sock: &Path,
    backend: &dyn TelemetryBackend,
) -> (BusHost, tt_toplike_bus::SocketServer) {
    serving_host_with(sock, backend, "live", HeaderKind::Live)
}

/// [`serving_host`] with the header's `mode` tag and kind.
#[cfg(unix)]
fn serving_host_with(
    sock: &Path,
    backend: &dyn TelemetryBackend,
    mode: &str,
    kind: HeaderKind,
) -> (BusHost, tt_toplike_bus::SocketServer) {
    let mut cfg = BusConfig {
        header_kind: kind,
        ..BusConfig::default()
    };
    cfg.header_tags.insert("mode".into(), mode.into());
    cfg.header_tags.insert("backend".into(), "test".into());
    let mut host = BusHost::with_gozer(cfg, backend, GozerSource::fed()).unwrap();
    host.tick(backend, Instant::now());
    let server = tt_toplike_bus::SocketServer::spawn(host.bus().clone(), sock).unwrap();
    (host, server)
}

fn probe_opts(attach: Attach) -> TuiBusOptions {
    TuiBusOptions {
        attach,
        ..mock_opts(&[])
    }
}

/// With a live host at the probed path, the TUI attaches, hosts nothing,
/// says so, and reads the host's world.
#[cfg(unix)]
#[test]
fn a_tui_attaches_to_a_live_host_at_the_probed_path() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_host, _server) = serving_host(&sock, &b);

    let bus = TuiBus::start(&probe_opts(Attach::Probe(sock.clone())), &b).unwrap();
    assert!(bus.is_attached());
    assert_eq!(bus.bus_label(), format!("bus: {}", sock.display()));
    let v = bus.read();
    assert!(v.seq() > 0);
    assert!(v
        .reading(&EntityKey::chip(A), sig::TDP_W)
        .is_some_and(|r| r.prov == tt_toplike_bus::Provenance::Measured));
}

/// With nothing at the probed path, the TUI hosts its own bus, quickly.
#[cfg(unix)]
#[test]
fn a_probe_that_finds_nothing_hosts_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let b = mock(2);
    let t = Instant::now();
    let bus = TuiBus::start(&probe_opts(Attach::Probe(tmp.path().join("none.sock"))), &b).unwrap();
    assert!(!bus.is_attached());
    assert_eq!(bus.bus_label(), "bus: local");
    assert!(t.elapsed() < Duration::from_secs(1));
}

/// `--no-bus-attach` (and every flag that makes the TUI host its own bus)
/// turns the probe off, even with a live host at the default path; a real
/// backend with no such flag probes the default path.
#[cfg(unix)]
#[test]
fn no_bus_attach_skips_the_probe() {
    let o = TuiBusOptions::from_cli(&cli(&["--no-bus-attach"]), BackendType::Json, "JSON");
    assert_eq!(o.attach, Attach::Off);
    let o = TuiBusOptions::from_cli(&cli(&[]), BackendType::Json, "JSON");
    assert_eq!(
        o.attach,
        Attach::Probe(crate::cli::default_bus_socket()),
        "a real backend probes the default path"
    );
    for args in [
        &["--bus-socket", "/x/s"][..],
        &["--record", "/r"],
        &["--bus-dir", "/d"],
    ] {
        let o = TuiBusOptions::from_cli(&cli(args), BackendType::Json, "JSON");
        assert_eq!(o.attach, Attach::Off, "{args:?}");
    }
    // A mock or remote backend's chips are not a local host's chips.
    assert_eq!(mock_opts(&[]).attach, Attach::Off);
    let o = TuiBusOptions::from_cli(&cli(&[]), BackendType::Auto, "Mock (3 devices)");
    assert_eq!(o.attach, Attach::Off, "auto that fell back to mock");

    // Off means off: a live host at the path is not used.
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_host, _server) = serving_host(&sock, &b);
    let bus = TuiBus::start(&probe_opts(Attach::Off), &b).unwrap();
    assert!(!bus.is_attached());
}

/// `--bus-attach` to a path where nothing answers fails with a message that
/// names the path and what to do.
#[cfg(unix)]
#[test]
fn bus_attach_to_a_dead_path_fails_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let dead = tmp.path().join("dead.sock");
    let o = TuiBusOptions::from_cli(
        &cli(&["--bus-attach", dead.to_str().unwrap()]),
        BackendType::Mock,
        "Mock",
    );
    assert_eq!(o.attach, Attach::Required(dead.clone()));
    let b = mock(1);
    let err = match TuiBus::start(&o, &b) {
        Ok(_) => panic!("attached to nothing"),
        Err(e) => e,
    };
    assert!(err.contains(&dead.display().to_string()), "{err}");
    assert!(err.contains("no bus host answered"), "{err}");
    assert!(err.contains("--headless"), "{err}");
}

/// `--bus-attach` to a live host attaches.
#[cfg(unix)]
#[test]
fn bus_attach_to_a_live_host_attaches() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_host, _server) = serving_host(&sock, &b);
    let bus = TuiBus::start(&probe_opts(Attach::Required(sock.clone())), &b).unwrap();
    assert!(bus.is_attached());
}

// ---- the read cache -------------------------------------------------------------------

/// Reading twice with no record in between does not copy the world; a new
/// record does, once. The clock is read on every call.
#[test]
fn read_copies_the_world_only_when_it_changed() {
    let mut b = mock(2);
    let mut bus = local_bus(&b);
    let mut last = Instant::now() - Duration::from_secs(1);
    run_data_tick(
        &mut last,
        Duration::from_millis(100),
        Instant::now(),
        &mut b,
        Some(&mut bus),
    );
    let first = bus.read();
    let n = bus.read_rebuilds();
    for _ in 0..100 {
        let v = bus.read();
        assert_eq!(v.seq(), first.seq());
    }
    assert_eq!(bus.read_rebuilds(), n, "no copy while nothing changed");
    bus.note_view("grid");
    let after = bus.read();
    assert_eq!(after.seq(), first.seq() + 1);
    assert_eq!(bus.read_rebuilds(), n + 1);
    assert!(after.now_mono_ns() >= first.now_mono_ns());
}

/// Mean time of one call of `f`, best of fifteen batches of `n`.
fn best_mean(n: u32, mut f: impl FnMut()) -> Duration {
    (0..15)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..n {
                f();
            }
            t.elapsed() / n
        })
        .min()
        .unwrap()
}

/// The per-frame cost of `read()`: near zero when nothing changed, one
/// world copy when it did. Prints both; asserts a loose bound that holds in
/// a debug build on a busy machine.
#[test]
fn read_cost_when_unchanged_is_near_zero() {
    let mut b = mock(4);
    let mut bus = local_bus(&b);
    let mut last = Instant::now() - Duration::from_secs(1);
    run_data_tick(
        &mut last,
        Duration::from_millis(100),
        Instant::now(),
        &mut b,
        Some(&mut bus),
    );
    let unchanged = best_mean(1000, || {
        std::hint::black_box(bus.read());
    });
    let changed = best_mean(50, || {
        bus.note_view(if bus.view_changes() % 2 == 0 {
            "a"
        } else {
            "b"
        });
        std::hint::black_box(bus.read());
    });
    println!("read(): unchanged {unchanged:?}, changed {changed:?}");
    assert!(unchanged < Duration::from_micros(50), "{unchanged:?}");
}

// ---- render cost --------------------------------------------------------------------------

/// The render path with the host ticking is within 0.3 ms per frame of the
/// same path without a bus. A frame is a Die 3D update and render at the
/// app's size plus one `read()`; the data tick runs on its own schedule
/// (every 100 ms, a frame every 16 ms), as in the loop.
#[test]
fn render_cost_with_the_host_ticking_is_within_a_third_of_a_millisecond() {
    const FRAMES: u32 = 60;
    let frame_cost = |with_bus: bool| -> Duration {
        let mut b = mock(2);
        let mut bus = with_bus.then(|| local_bus(&b));
        let mut view = crate::animation::Die3dView::new(100, 30);
        (0..7)
            .map(|_| {
                let start = Instant::now();
                let mut last = start;
                for f in 1..=FRAMES {
                    let now = start + Duration::from_millis(16 * u64::from(f));
                    run_data_tick(
                        &mut last,
                        Duration::from_millis(100),
                        now,
                        &mut b,
                        bus.as_mut(),
                    );
                    // With a bus, Die 3D reads it as the app does.
                    let world = bus.as_ref().map(|bus| bus.read());
                    let world = world.as_ref().map(|w| w as &dyn WorldRead);
                    let t = f64::from(f) * 0.016;
                    view.update(&b, world, t);
                    std::hint::black_box(view.render(world, t));
                }
                start.elapsed() / FRAMES
            })
            .min()
            .unwrap()
    };
    let without = frame_cost(false);
    let with = frame_cost(true);
    println!("per frame: without bus {without:?}, with bus {with:?}");
    assert!(
        with.saturating_sub(without) < Duration::from_micros(300),
        "with {with:?}, without {without:?}"
    );
}

// ---- /serve frames ---------------------------------------------------------------------

#[cfg(feature = "remote")]
struct WithRaw(MockBackend, String);

#[cfg(feature = "remote")]
impl TelemetryBackend for WithRaw {
    fn init(&mut self) -> BackendResult<()> {
        self.0.init()
    }
    fn update(&mut self) -> BackendResult<()> {
        self.0.update()
    }
    fn devices(&self) -> &[Device] {
        self.0.devices()
    }
    fn telemetry(&self, i: usize) -> Option<&Telemetry> {
        self.0.telemetry(i)
    }
    fn smbus_telemetry(&self, i: usize) -> Option<&SmbusTelemetry> {
        self.0.smbus_telemetry(i)
    }
    fn backend_info(&self) -> String {
        "with-raw".into()
    }
    fn snapshot_json(&self) -> Option<String> {
        Some(self.1.clone())
    }
}

#[cfg(feature = "remote")]
const TT_SMI_FRAME: &str = include_str!("fixtures/tt_smi_frame.json");

#[cfg(feature = "remote")]
fn one_proc() -> Vec<ProcRow> {
    vec![ProcRow {
        pid: 1,
        name: "init".into(),
        cpu_pct: 0.5,
        mem_bytes: 4096,
        inference: None,
        active: false,
        tt: None,
    }]
}

/// The `/serve` frame from the bus equals the old `inject_extension` frame
/// byte for byte, and is built from the world: the backend frame passed in
/// is stale on purpose and must not be used.
#[cfg(feature = "remote")]
#[test]
fn serve_frames_from_the_bus_equal_the_injected_frame() {
    let mut b = WithRaw(mock(2), TT_SMI_FRAME.to_string());
    let mut bus = local_bus(&b);
    let mut last = Instant::now() - Duration::from_secs(1);
    run_data_tick(
        &mut last,
        Duration::from_millis(100),
        Instant::now(),
        &mut b,
        Some(&mut bus),
    );
    let ext = || crate::backend::build_extension(&one_proc(), &[]);
    let want = crate::backend::inject_extension(TT_SMI_FRAME, &ext());
    let got = serve_frame(Some(&bus), Some("stale".into()), false, ext).unwrap();
    assert_eq!(got, want);
    assert!(got.contains("tt_toplike"));

    // Without a bus the old path gives the same bytes.
    let old = serve_frame(None, Some(TT_SMI_FRAME.into()), false, ext).unwrap();
    assert_eq!(old, want);
    // No backend frame: nothing to serve, as before.
    assert!(serve_frame(Some(&bus), None, false, ext).is_none());
}

/// The relay branch passes the origin's frame verbatim, from the world, and
/// never builds the local extension.
#[cfg(feature = "remote")]
#[test]
fn relay_serve_frames_are_the_origin_frame_verbatim() {
    let mut b = WithRaw(mock(2), TT_SMI_FRAME.to_string());
    let mut bus = local_bus(&b);
    let mut last = Instant::now() - Duration::from_secs(1);
    run_data_tick(
        &mut last,
        Duration::from_millis(100),
        Instant::now(),
        &mut b,
        Some(&mut bus),
    );
    let ext = || -> crate::backend::TtToplikeExt { panic!("a relay built the local extension") };
    let got = serve_frame(Some(&bus), Some("stale".into()), true, ext).unwrap();
    assert_eq!(got, TT_SMI_FRAME);
    let old = serve_frame(None, Some(TT_SMI_FRAME.into()), true, ext).unwrap();
    assert_eq!(old, TT_SMI_FRAME);
}

// ---- workload feeds ---------------------------------------------------------------------

/// Hivemind events are fed once each, and only new ones.
#[test]
fn hivemind_events_are_fed_once() {
    use crate::workload::hivemind::event::{EventKind, Severity, SniffEvent, Source};
    let b = mock(2);
    let mut bus = local_bus(&b);
    let mut hm = crate::workload::hivemind::Hivemind::new();
    let ev = |n: u8| SniffEvent {
        ts: Instant::now(),
        source: Source::TtMetal,
        kind: EventKind::Compile,
        device: Some(n % 2),
        severity: Severity::Info,
        text: "x".into(),
        origin: "test".into(),
    };
    hm.push_for_test(ev(0));
    bus.feed_hivemind(&hm);
    bus.feed_hivemind(&hm);
    hm.push_for_test(ev(1));
    bus.feed_hivemind(&hm);
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut b = b;
    run_data_tick(
        &mut last,
        Duration::from_millis(100),
        Instant::now(),
        &mut b,
        Some(&mut bus),
    );
    let w = bus.host().unwrap().world();
    let compiled = w
        .events()
        .iter()
        .filter(|(_, e)| matches!(e, Event::KernelCompiled { .. }))
        .count();
    assert_eq!(compiled, 2);
}

/// Process rows are fed only for a box whose processes belong on the bus,
/// and only TT-relevant rows.
#[test]
fn only_tt_relevant_rows_are_fed_and_not_for_a_mock_run() {
    let run = |opts: TuiBusOptions| -> usize {
        let mut b = mock(2);
        let mut bus = TuiBus::start(&opts, &b).unwrap();
        let mut rows = vec![ProcRow {
            pid: 4242,
            name: "busy-but-not-tt".into(),
            cpu_pct: 90.0,
            mem_bytes: 1,
            inference: None,
            active: false,
            tt: None,
        }];
        rows.push(ProcRow {
            pid: 4243,
            name: "vllm".into(),
            inference: Some("vllm"),
            ..rows[0].clone()
        });
        bus.feed_workloads(&rows, None, None);
        let mut last = Instant::now() - Duration::from_secs(1);
        run_data_tick(
            &mut last,
            Duration::from_millis(100),
            Instant::now(),
            &mut b,
            Some(&mut bus),
        );
        let w = bus.host().unwrap().world();
        let n = w.entities(tt_toplike_bus::EntityKind::Process).count();
        n
    };
    assert_eq!(run(mock_opts(&[])), 0, "a mock run records no real process");
    let real = TuiBusOptions {
        local_workloads: true,
        ..mock_opts(&[])
    };
    assert_eq!(run(real), 1, "only the inference row");
}

// ---- fix round 1: attach only to a matching live host ------------------------------

/// A backend whose devices have the given PCI addresses and no telemetry.
#[derive(Default)]
struct Devices(Vec<Device>);

impl Devices {
    fn with(ids: &[&str]) -> Devices {
        Devices(
            ids.iter()
                .enumerate()
                .map(|(i, b)| Device::new(i, "p150".into(), (*b).into(), String::new()))
                .collect(),
        )
    }
}

impl TelemetryBackend for Devices {
    fn init(&mut self) -> BackendResult<()> {
        Ok(())
    }
    fn update(&mut self) -> BackendResult<()> {
        Ok(())
    }
    fn devices(&self) -> &[Device] {
        &self.0
    }
    fn telemetry(&self, _: usize) -> Option<&Telemetry> {
        None
    }
    fn smbus_telemetry(&self, _: usize) -> Option<&SmbusTelemetry> {
        None
    }
    fn backend_info(&self) -> String {
        "devices".into()
    }
}

/// Probe a host serving `sock` from a TUI on `local`, as this machine.
#[cfg(unix)]
fn probe_from(sock: &Path, local: &dyn TelemetryBackend) -> TuiBus {
    TuiBus::start(&probe_opts(Attach::Probe(sock.to_path_buf())), local).unwrap()
}

#[cfg(unix)]
#[test]
fn the_probe_refuses_a_mock_host() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_h, _s) = serving_host_with(&sock, &b, "mock", HeaderKind::Live);
    assert!(!probe_from(&sock, &b).is_attached());
}

#[cfg(unix)]
#[test]
fn the_probe_refuses_a_replay_host() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_h, _s) = serving_host_with(&sock, &b, "replay", HeaderKind::Replay);
    assert!(!probe_from(&sock, &b).is_attached());
}

#[cfg(unix)]
#[test]
fn the_probe_refuses_a_host_with_another_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_h, _s) = serving_host(&sock, &b);
    let opts = TuiBusOptions {
        local_host: "another-box/0123456789abcdef".into(),
        ..probe_opts(Attach::Probe(sock.clone()))
    };
    assert!(!TuiBus::start(&opts, &b).unwrap().is_attached());
}

#[cfg(unix)]
#[test]
fn the_probe_refuses_a_live_host_with_no_chip_in_common() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_h, _s) = serving_host(&sock, &b);
    let local = Devices::with(&["0000:41:00.0", "0000:42:00.0"]);
    assert!(!probe_from(&sock, &local).is_attached());
}

#[cfg(unix)]
#[test]
fn the_probe_attaches_to_a_live_same_host_world_with_a_chip_in_common() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_h, _s) = serving_host(&sock, &b);
    // One chip in common is enough.
    let local = Devices::with(&[A, "0000:42:00.0"]);
    let bus = probe_from(&sock, &local);
    assert!(bus.is_attached());
    assert_eq!(bus.bus_label(), format!("bus: {}", sock.display()));
}

/// The mismatch rule itself, case by case.
#[test]
fn origin_mismatch_checks_kind_mode_host_and_chips() {
    let local: BTreeSet<EntityKey> = [EntityKey::chip(A)].into_iter().collect();
    let o = |kind, host: &str, mode: &str, chips: &[&str]| Origin {
        kind,
        host: host.into(),
        mode: mode.into(),
        chips: chips.iter().map(|c| EntityKey::chip(c)).collect(),
    };
    let live = HeaderKind::Live;
    assert_eq!(
        origin_mismatch(&o(live, "me", "live", &[A]), "me", &local),
        None
    );
    assert_eq!(
        origin_mismatch(&o(HeaderKind::Replay, "me", "live", &[A]), "me", &local),
        Some(Mismatch::Replay)
    );
    assert_eq!(
        origin_mismatch(&o(live, "me", "mock", &[A]), "me", &local),
        Some(Mismatch::Mock)
    );
    assert_eq!(
        origin_mismatch(&o(live, "you", "live", &[A]), "me", &local),
        Some(Mismatch::OtherHost)
    );
    assert_eq!(
        origin_mismatch(&o(live, "me", "", &[A]), "me", &local),
        Some(Mismatch::UnknownMode),
        "an older host that sends no mode"
    );
    assert_eq!(
        origin_mismatch(&o(live, "me", "live", &[B]), "me", &local),
        Some(Mismatch::NoMatchingChips)
    );
}

/// An explicit `--bus-attach` attaches to a mismatching host, and the label
/// says why.
#[cfg(unix)]
#[test]
fn bus_attach_to_a_mismatching_host_warns_in_the_label() {
    let tmp = tempfile::tempdir().unwrap();
    let b = mock(2);
    let cases: [(&str, HeaderKind, &str, &str); 3] = [
        ("mock", HeaderKind::Live, "me", "(mock)"),
        ("replay", HeaderKind::Replay, "me", "(replay)"),
        ("live", HeaderKind::Live, "other", "(other host)"),
    ];
    for (i, (mode, kind, who, want)) in cases.into_iter().enumerate() {
        let sock = tmp.path().join(format!("run{i}")).join("bus.sock");
        let (_h, _s) = serving_host_with(&sock, &b, mode, kind);
        let local_host = if who == "me" {
            tt_toplike_bus::host_identity().to_string()
        } else {
            "elsewhere".to_string()
        };
        let opts = TuiBusOptions {
            local_host,
            ..probe_opts(Attach::Required(sock.clone()))
        };
        let bus = TuiBus::start(&opts, &b).unwrap();
        assert!(bus.is_attached());
        assert_eq!(bus.bus_label(), format!("bus: {} {want}", sock.display()));
    }
    // No chip in common.
    let sock = tmp.path().join("run9").join("bus.sock");
    let (_h, _s) = serving_host(&sock, &b);
    let local = Devices::with(&["0000:41:00.0"]);
    let bus = TuiBus::start(&probe_opts(Attach::Required(sock.clone())), &local).unwrap();
    assert_eq!(
        bus.bus_label(),
        format!("bus: {} (no matching chips)", sock.display())
    );
}

/// An attached world answers only for local chips: a reading of a remote
/// chip that is not local (or of its DRAM channel) is `None`.
#[cfg(unix)]
#[test]
fn an_attached_world_is_read_only_for_local_chips() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock(2);
    let (_h, _s) = serving_host(&sock, &b);
    let local = Devices::with(&[A]);
    let bus = TuiBus::start(&probe_opts(Attach::Required(sock.clone())), &local).unwrap();
    let v = bus.read();
    let (a, bb) = (EntityKey::chip(A), EntityKey::chip(B));
    assert!(v.reading(&a, sig::TDP_W).is_some());
    assert!(
        v.reading(&bb, sig::TDP_W).is_none(),
        "a chip that is not local"
    );
    assert!(v
        .reading(&EntityKey::dram_channel(&bb, 0), "dram.temp_c")
        .is_none());
    assert!(v.reset_seq(&bb).is_none());
    assert_eq!(bus.local_chips().len(), 1);
}

/// A dead host shows in the label: reconnecting, then disconnected.
#[cfg(unix)]
#[test]
fn the_label_says_when_the_attached_host_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let mut b = mock(2);
    let (host, server) = serving_host(&sock, &b);
    let mut bus = TuiBus::start(&probe_opts(Attach::Required(sock.clone())), &b).unwrap();
    assert_eq!(bus.bus_label(), format!("bus: {}", sock.display()));
    drop(server);
    drop(host);
    let end = Instant::now() + Duration::from_secs(6);
    let mut seen = Vec::new();
    while Instant::now() < end {
        bus.data_tick(&b, Instant::now());
        let l = bus.bus_label();
        if seen.last() != Some(&l) {
            seen.push(l.clone());
        }
        if l.ends_with("(disconnected)") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        b.update().unwrap();
    }
    assert!(
        seen.iter().any(|l| l.ends_with("(reconnecting)")),
        "{seen:?}"
    );
    assert!(seen.last().unwrap().ends_with("(disconnected)"), "{seen:?}");
}

// ---- fix round 1: synthetic data is Estimated ------------------------------------

/// Every reading in `w` and its provenance.
fn provenances(w: &World) -> Vec<(String, tt_toplike_bus::Provenance)> {
    w.readings()
        .map(|(e, s, r)| (format!("{e} {s}"), r.prov))
        .collect()
}

/// A TUI on `opts` fed the mock roster and training run, ticked twice.
fn fed_world(opts: TuiBusOptions) -> World {
    let mut b = mock(2);
    let mut bus = TuiBus::start(&opts, &b).unwrap();
    let services = crate::workload::inference_server::mock::mock_services(30.0);
    let train = crate::workload::train::MockTrainRun::new().state_at(30.0);
    let t0 = Instant::now();
    for i in 0..2u64 {
        bus.feed_workloads(&[], Some(&services), Some(&train));
        bus.data_tick(&b, t0 + Duration::from_secs(i));
        b.update().unwrap();
    }
    bus.host().unwrap().world()
}

#[test]
fn a_mock_tui_world_has_no_measured_reading() {
    let w = fed_world(mock_opts(&[]));
    let p = provenances(&w);
    assert!(p.iter().any(|(k, _)| k.starts_with("model:")), "{p:?}");
    assert!(p.iter().any(|(k, _)| k.starts_with("run:")), "{p:?}");
    let measured: Vec<_> = p
        .iter()
        .filter(|(_, v)| {
            matches!(
                v,
                tt_toplike_bus::Provenance::Measured | tt_toplike_bus::Provenance::Derived
            )
        })
        .collect();
    assert!(measured.is_empty(), "{measured:?}");
}

#[test]
fn a_real_run_keeps_measured_readings() {
    let opts = TuiBusOptions {
        synthetic: false,
        ..mock_opts(&[])
    };
    let w = fed_world(opts);
    assert!(provenances(&w)
        .iter()
        .any(|(k, v)| k.starts_with("model:") && *v == tt_toplike_bus::Provenance::Measured));
}

// ---- fix round 1: backend switches ------------------------------------------------

#[test]
fn a_switch_while_recording_or_serving_is_refused_with_a_message() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bus");
    let b = mock(2);
    let bus = TuiBus::start(&mock_opts(&["--record", dir.to_str().unwrap()]), &b).unwrap();
    let why = bus.switch_refusal().expect("refused while recording");
    assert!(
        why.contains("refused") && why.contains(&dir.display().to_string()),
        "{why}"
    );
    assert!(!why.contains('\n'));

    #[cfg(unix)]
    {
        let sock = tmp.path().join("run").join("bus.sock");
        let bus = TuiBus::start(&mock_opts(&["--bus-socket", sock.to_str().unwrap()]), &b).unwrap();
        assert!(bus
            .switch_refusal()
            .is_some_and(|w| w.contains("--bus-socket")));
    }
    assert_eq!(
        local_bus(&b).switch_refusal(),
        None,
        "a memory-only bus follows"
    );
}

/// In a memory-only bus, a switch into mock makes the next readings
/// `Estimated`, and a switch back makes them `Measured` again.
#[test]
fn a_switch_in_a_memory_bus_retags_the_next_readings() {
    let mut b = mock(2);
    let opts = TuiBusOptions {
        synthetic: false,
        ..mock_opts(&[])
    };
    let mut bus = TuiBus::start(&opts, &b).unwrap();
    let chip = EntityKey::chip(A);
    let t0 = Instant::now();
    bus.data_tick(&b, t0);
    let prov = |bus: &TuiBus| bus.read().reading(&chip, sig::TDP_W).unwrap().prov;
    assert_eq!(prov(&bus), tt_toplike_bus::Provenance::Measured);

    bus.backend_switched(BackendType::Mock, "Mock (2 devices)", false);
    assert!(bus.synthetic());
    b.update().unwrap();
    bus.data_tick(&b, t0 + Duration::from_secs(1));
    assert_eq!(prov(&bus), tt_toplike_bus::Provenance::Estimated);

    bus.backend_switched(BackendType::Json, "JSON", false);
    b.update().unwrap();
    bus.data_tick(&b, t0 + Duration::from_secs(2));
    assert_eq!(prov(&bus), tt_toplike_bus::Provenance::Measured);
}

/// Right after a switch, `/serve` never sends the old backend's frame: the
/// new backend's frame is used until the bus has the new raw frame.
#[cfg(feature = "remote")]
#[test]
fn serve_frame_after_a_switch_never_returns_the_old_frame() {
    let old = TT_SMI_FRAME.to_string();
    let new = TT_SMI_FRAME
        .replace("\"", "\"")
        .replacen('{', "{\"switched\":1,", 1);
    assert_ne!(old, new);
    let mut b = WithRaw(mock(2), old.clone());
    let mut bus = local_bus(&b);
    bus.data_tick(&b, Instant::now());
    let ext = || crate::backend::build_extension(&one_proc(), &[]);

    // Swap: the backend now serves `new`; the bus has not ticked yet.
    b.1 = new.clone();
    bus.backend_switched(BackendType::Json, "JSON", false);
    let got = serve_frame(Some(&bus), b.snapshot_json(), false, ext).unwrap();
    assert_eq!(got, crate::backend::inject_extension(&new, &ext()));
    let got = serve_frame(Some(&bus), b.snapshot_json(), true, ext).unwrap();
    assert_eq!(got, new, "relay");

    // After the next tick the world's frame is the new one.
    bus.data_tick(&b, Instant::now());
    assert!(bus.host().unwrap().raw_is_current());
    let got = serve_frame(Some(&bus), Some("stale".into()), false, ext).unwrap();
    assert_eq!(got, crate::backend::inject_extension(&new, &ext()));
}

/// A backend with a `--remote` frame extension: one ready model.
#[cfg(feature = "remote")]
struct RemoteBox(MockBackend);

#[cfg(feature = "remote")]
impl TelemetryBackend for RemoteBox {
    fn init(&mut self) -> BackendResult<()> {
        self.0.init()
    }
    fn update(&mut self) -> BackendResult<()> {
        self.0.update()
    }
    fn devices(&self) -> &[Device] {
        self.0.devices()
    }
    fn telemetry(&self, i: usize) -> Option<&Telemetry> {
        self.0.telemetry(i)
    }
    fn smbus_telemetry(&self, i: usize) -> Option<&SmbusTelemetry> {
        self.0.smbus_telemetry(i)
    }
    fn backend_info(&self) -> String {
        "Remote (ws)".into()
    }
    fn remote_inference(&self) -> Option<Vec<crate::backend::RemoteInference>> {
        Some(vec![crate::backend::RemoteInference {
            key: "box-llama".into(),
            label: "Llama".into(),
            phase: "ready".into(),
            progress: None,
            serving: None,
            media: None,
            port: None,
            chips: vec![0],
        }])
    }
}

/// `/remote` mid-session: the bus starts feeding the remote frame's
/// extension.
#[cfg(feature = "remote")]
#[test]
fn a_switch_to_remote_starts_remote_feeding() {
    let mut b = RemoteBox(mock(2));
    let opts = TuiBusOptions {
        synthetic: false,
        ..mock_opts(&[])
    };
    let mut bus = TuiBus::start(&opts, &b).unwrap();
    bus.data_tick(&b, Instant::now());
    assert!(bus.read().models().is_empty(), "not fed before the switch");
    bus.backend_switched(BackendType::Remote, "Remote (ws)", false);
    assert!(bus.relays());
    b.update().unwrap();
    bus.data_tick(&b, Instant::now());
    assert!(
        bus.read()
            .models()
            .iter()
            .any(|m| m.label.contains("Llama")),
        "{:?}",
        bus.read().models()
    );
}
