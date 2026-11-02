//! Tests for the `i` view's rows from the bus: the attached/local choice,
//! the frames drawn from world rows, the `~` marker, and the coherence of
//! the numbers with Die 3D's flag. `MockBackend`, a real `BusHost`, sockets
//! in temp directories and a ratatui `TestBackend`. No hardware.

use super::*;
use crate::animation::die3d::overlay::{Overlay, OverlayState};
use crate::animation::{Snake, SnakeWorld};
use crate::backend::mock::MockBackend;
use crate::bus::compat::world_to_service_states_with;
use crate::bus::gozer::GozerSource;
use crate::bus::host::BusHost;
use crate::bus::tui_bus::{Attach, TuiBusOptions};
use crate::workload::inference_server::{
    Phase, Readiness, ServingStats, VllmCounters, CADENCE_SECS,
};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
#[cfg(unix)]
use std::path::Path;
use std::time::Instant;
use tt_toplike_bus::{BusConfig, HeaderKind, LocalView};

const A: &str = "0000:01:00.0";
const B: &str = "0000:02:00.0";

fn mock() -> MockBackend {
    let mut b = MockBackend::new(2);
    b.init().unwrap();
    b.update().unwrap();
    b
}

/// A service whose every field the bus carries, and nothing it does not:
/// no port, no log line, no process name, zero deltas. Drawn from the
/// local list or from the world, it must look the same.
fn state(key: &str, phase: Phase, chips: &[usize]) -> ServiceState {
    ServiceState {
        key: key.into(),
        label: format!("{key}-model"),
        phase,
        cpu_pct: 12.5,
        rss_bytes: 1 << 30,
        rss_delta: 0,
        kernel_count: 40,
        kernel_delta: 0,
        loaded_count: 3,
        loaded_delta: 0,
        safetensors_fds: 0,
        readiness: match phase {
            Phase::Ready => Readiness::Ready { runner: None },
            Phase::Down => Readiness::Down,
            _ => Readiness::NotReady,
        },
        top_proc: None,
        last_log: None,
        progress: None,
        flat_ticks: 0,
        serving: None,
        media: None,
        port: None,
        chips: chips.to_vec(),
    }
}

fn serving(tps: f32, running: u32, waiting: u32, kv: f32) -> ServingStats {
    ServingStats {
        generation_tps: tps,
        prompt_tps: 0.0,
        completed_delta: 0,
        errored_delta: 0,
        requests_running: running,
        requests_waiting: waiting,
        kv_cache_usage: kv,
        ttft_avg_s: 0.0,
        queue_avg_s: 0.0,
        prefill_avg_s: 0.0,
        decode_avg_s: 0.0,
        tpot_avg_s: 0.0,
        prefix_hit_rate: 0.0,
        preemptions_delta: 0,
        counters: VllmCounters::default(),
    }
}

/// The fixture list: one model serving on chip 0, one loading on chip 1.
fn fixture() -> Vec<ServiceState> {
    let mut a = state("vllm-8000", Phase::Ready, &[0]);
    a.serving = Some(serving(42.0, 2, 3, 0.25));
    let mut b = state("vllm-8001", Phase::Loading, &[1]);
    b.progress = Some(0.4);
    vec![a, b]
}

/// A host that published `rows` once; `synthetic` marks it a mock run.
fn host_with(b: &MockBackend, rows: &[ServiceState], synthetic: bool) -> BusHost {
    let mut host = BusHost::with_gozer(BusConfig::default(), b, GozerSource::fed()).unwrap();
    host.set_synthetic(synthetic);
    host.feed_inference(rows);
    host.tick(b, Instant::now());
    host
}

/// The host's world as a view reads it.
fn view_of(host: &BusHost) -> LocalView {
    let (seq, world) = host.with_world(|w, seq| (seq, w.clone()));
    LocalView {
        seq,
        world,
        now_mono_ns: 0,
        header_kind: HeaderKind::Live,
    }
}

/// The mock backend's chips.
fn mock_index(k: &EntityKey) -> Option<usize> {
    [(EntityKey::chip(A), 0), (EntityKey::chip(B), 1)]
        .into_iter()
        .find(|(c, _)| c == k)
        .map(|(_, i)| i)
}

/// The `i` view for `rows` at `w`x`h`, drawn as the TUI loop draws it
/// (roster or single-service line, the snake, `render_snake_view`), as
/// one string per row. The education band is left out; it is built from
/// the same rows.
fn render_i(rows: &[ServiceState], w: u16, h: u16) -> Vec<String> {
    let mut roster = super::super::inference_roster_lines(rows, w as usize);
    if roster.is_empty() {
        roster = super::super::inference_single_service_line(rows, w as usize);
    }
    let mut snake = Snake::new();
    snake.update(&SnakeWorld {
        rows,
        catalog: &[],
        arch: "Blackhole",
        chips: &[],
        cadence_secs: CADENCE_SECS,
        width: w as usize,
        height: (h as usize).saturating_sub(1 + roster.len()),
    });
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| super::super::render_snake_view(f, &snake, &roster, &[]))
        .unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect()
}

/// The fixture with both models ready, the first one featured.
fn ready_fixture() -> Vec<ServiceState> {
    let mut rows = fixture();
    rows[1] = state("vllm-8001", Phase::Ready, &[1]);
    rows[1].serving = Some(serving(7.5, 1, 0, 0.1));
    rows
}

/// The fields the bus carries, side by side.
fn carried(s: &ServiceState) -> String {
    format!(
        "{} {} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
        s.key,
        s.label,
        s.phase,
        s.progress,
        s.chips,
        s.serving.as_ref().map(|v| v.generation_tps),
        s.serving.as_ref().map(|v| v.requests_running),
        s.serving.as_ref().map(|v| v.requests_waiting),
        s.serving.as_ref().map(|v| v.kv_cache_usage),
    )
}

fn carried_all(rows: &[ServiceState]) -> Vec<String> {
    rows.iter().map(carried).collect()
}

// ---- the frames ---------------------------------------------------------------

/// The `i` view drawn from the world's rows is the frame drawn from the
/// local list, for each fixture and at sizes with and without the roster,
/// the swimlanes and the request group. The rows are compared before the
/// `~` marker: the loading model's progress is an estimate, which the
/// marker test covers.
#[test]
fn world_rows_draw_the_same_i_frames_as_the_local_list() {
    let b = mock();
    let single = vec![fixture()[0].clone()];
    for rows in [fixture(), ready_fixture(), single] {
        let world = view_of(&host_with(&b, &rows, false));
        let from_world = world_to_service_states_with(&world, &mock_index);
        for (w, h) in [(120u16, 40u16), (66, 40), (80, 24)] {
            assert_eq!(
                render_i(&from_world, w, h),
                render_i(&rows, w, h),
                "{w}x{h}, rows {:?}",
                carried_all(&rows)
            );
        }
    }
}

/// A world with no models draws the `i` view's empty state, the same frame
/// as no services today.
#[test]
fn a_world_with_no_models_draws_the_empty_state() {
    let b = mock();
    let world = view_of(&host_with(&b, &[], false));
    let rows = rows_from_world(&world, &mock_index);
    assert!(rows.is_empty());
    for (w, h) in [(120u16, 40u16), (80, 24)] {
        assert_eq!(render_i(&rows, w, h), render_i(&[], w, h), "{w}x{h}");
    }
}

// ---- the estimate marker ------------------------------------------------------

/// A synthetic host's models carry `~` in the `i` view, in the roster line,
/// as Die 3D's flag does. A real host's serving models do not; its loading
/// model with an estimated progress does, in both views.
#[test]
fn estimated_models_are_marked_like_die3d_flags() {
    let b = mock();
    let rows = ready_fixture();
    let mock_world = view_of(&host_with(&b, &rows, true));
    let marked = rows_from_world(&mock_world, &mock_index);
    assert!(
        marked.iter().all(|s| s.label.starts_with('~')),
        "{marked:?}"
    );
    let text = render_i(&marked, 66, 40).join("\n");
    assert!(text.contains("serving  ~vllm-8000-model"), "{text}");
    let flag = die3d_flag(&mock_world);
    assert!(flag.starts_with("~vllm-8000-model"), "{flag}");

    let real_world = view_of(&host_with(&b, &rows, false));
    let plain = rows_from_world(&real_world, &mock_index);
    assert!(plain.iter().all(|s| !s.label.starts_with('~')), "{plain:?}");
    assert!(!die3d_flag(&real_world).starts_with('~'));

    // A real host's loading model with a progress figure: the progress is
    // the monitor's estimate, so Die 3D's flag starts with `~`, and so does
    // the `i` view's label for it.
    let loading = view_of(&host_with(&b, &fixture(), false));
    let rows = rows_from_world(&loading, &mock_index);
    assert_eq!(rows[1].label, "~vllm-8001-model");
    assert!(!rows[0].label.starts_with('~'));
    let flag = die3d_flag_on(&loading, B, 1);
    assert!(flag.starts_with("~vllm-8001-model loading 40%"), "{flag}");
}

// ---- coherence with Die 3D ----------------------------------------------------

/// The longest text of Die 3D's flag on chip A, built by the overlay.
fn die3d_flag(world: &dyn WorldRead) -> String {
    die3d_flag_on(world, A, 0)
}

/// The longest text of Die 3D's flag on the chip `bus_id`, drawn as device
/// `index`.
fn die3d_flag_on(world: &dyn WorldRead, bus_id: &str, index: usize) -> String {
    let mut st = OverlayState::default();
    let ov = Overlay::from_world(world, &[(index, EntityKey::chip(bus_id))], 0, &mut st);
    ov.flag(index).expect("a flag on the chip").texts()[0].clone()
}

/// The first number in `s`, skipping anything that is not a digit.
fn first_number(s: &str) -> Option<f64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let tail = &s[start..];
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(tail.len());
    tail[..end].trim_end_matches('.').parse().ok()
}

/// The first number after `word` on the first line of `lines` that has it.
fn number_after(lines: &[String], word: &str) -> Option<f64> {
    lines.iter().find_map(|l| {
        let at = l.find(word)?;
        first_number(&l[at + word.len()..])
    })
}

/// The phase, tokens/s, running, waiting and KV cache that Die 3D's flag
/// and the `i` view show for the same model in the same world are equal.
///
/// Die 3D's flag text carries the phase, tokens/s and waiting count. It
/// does not print running or KV cache, so those two are checked against
/// the `ModelView` the flag is built from (Die 3D's source for the same
/// model). The `i` view's numbers are read from its rendered text: the
/// creature's title (phase) and its stats panel.
#[test]
fn die3d_and_i_show_the_same_numbers_for_one_world() {
    let b = mock();
    let world = view_of(&host_with(&b, &ready_fixture(), false));

    // Die 3D.
    let flag = die3d_flag(&world);
    let d_phase = flag.split_whitespace().nth(1).unwrap().to_string();
    let d_tps = flag
        .split_whitespace()
        .find(|w| w.ends_with("t/s"))
        .and_then(first_number)
        .unwrap();
    let d_waiting = number_after(std::slice::from_ref(&flag), "t/s").unwrap();
    let model = world
        .model_on_chip(&EntityKey::chip(A))
        .expect("a model on chip A");
    let d_running = f64::from(model.running.unwrap());
    let d_kv_pct = (model.kv_cache.unwrap() * 100.0).round();

    // The `i` view, rendered from the world's rows. 66 columns: no
    // swimlanes, so the panel prints the request group.
    let rows = rows_from_world(&world, &mock_index);
    let text = render_i(&rows, 66, 40);
    let title = text
        .iter()
        .find(|l| l.starts_with("vllm-8000-model  serving"))
        .expect("the creature's title");
    let i_phase = title.split(" · ").nth(1).unwrap().to_lowercase();
    let i_tps = number_after(&text, "decode").unwrap();
    let i_running = number_after(&text, "running").unwrap();
    let i_waiting = number_after(&text, "queued").unwrap();
    let i_kv_pct = number_after(&text, "cache").unwrap();

    let all = text.join("\n");
    assert_eq!(i_phase, d_phase, "phase\n{flag}\n{all}");
    assert_eq!(i_tps, d_tps, "tokens/s\n{flag}\n{all}");
    assert_eq!(i_running, d_running, "running\n{all}");
    assert_eq!(i_waiting, d_waiting, "waiting\n{flag}\n{all}");
    assert_eq!(i_kv_pct, d_kv_pct, "KV cache %\n{all}");
    // And they are the fixture's numbers: two zero defaults would also match.
    assert_eq!(
        (d_phase.as_str(), d_tps, d_running, d_waiting, d_kv_pct),
        ("ready", 42.0, 2.0, 3.0, 25.0)
    );
}

// ---- the attached/local choice ------------------------------------------------

/// Options for a TUI on this machine that attaches as `attach` says.
fn opts(attach: Attach) -> TuiBusOptions {
    TuiBusOptions {
        bus: BusConfig::default(),
        serve_socket: None,
        attach,
        synthetic: false,
        relay: false,
        gozer: false,
        local_workloads: false,
        local_host: tt_toplike_bus::host_identity().to_string(),
    }
}

/// A live headless-style host serving `sock`, which published `rows` once.
#[cfg(unix)]
fn serving_host(
    sock: &Path,
    b: &MockBackend,
    rows: &[ServiceState],
) -> (BusHost, tt_toplike_bus::SocketServer) {
    let mut cfg = BusConfig::default();
    cfg.header_tags.insert("mode".into(), "live".into());
    cfg.header_tags.insert("backend".into(), "test".into());
    let mut host = BusHost::with_gozer(cfg, b, GozerSource::fed()).unwrap();
    host.feed_inference(rows);
    host.tick(b, Instant::now());
    let server = tt_toplike_bus::SocketServer::spawn(host.bus().clone(), sock).unwrap();
    (host, server)
}

/// A TUI that hosts its own bus keeps the local monitor's rows, even with
/// models in its own world.
#[test]
fn a_local_bus_keeps_the_local_monitor() {
    let b = mock();
    let mut bus = TuiBus::start(&opts(Attach::Off), &b).unwrap();
    bus.feed_workloads(&[], Some(&fixture()), None);
    bus.data_tick(&b, Instant::now());
    assert!(!bus.is_attached());
    assert_eq!(
        bus.read().models().len(),
        2,
        "the local world has the models"
    );
    assert!(attached_rows(&bus, &b).is_none());
}

/// A TUI attached by the probe draws the host's models, with chip indexes
/// from its own backend, and lists a model on a chip it does not have.
#[cfg(unix)]
#[test]
fn an_attached_tui_draws_the_hosts_models() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let host_backend = mock();
    let (_h, _s) = serving_host(&sock, &host_backend, &fixture());

    // The same machine with both chips.
    let bus = TuiBus::start(&opts(Attach::Probe(sock.clone())), &host_backend).unwrap();
    assert!(bus.is_attached());
    assert!(bus.origin_is_local());
    let rows = attached_rows(&bus, &host_backend).expect("attached: rows from the world");
    // The loading model's progress is an estimate, so its label is marked.
    let mut want = fixture();
    want[1].label.insert(0, '~');
    assert_eq!(carried_all(&rows), carried_all(&want));

    // A TUI whose backend has only chip A (index 0): Die 3D's read drops
    // chip B, the `i` rows still list the model on it, with no chip index.
    let mut one = MockBackend::new(1);
    one.init().unwrap();
    one.update().unwrap();
    let bus = TuiBus::start(&opts(Attach::Probe(sock.clone())), &one).unwrap();
    assert!(bus.is_attached());
    let rows = attached_rows(&bus, &one).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].chips, vec![0]);
    assert_eq!(rows[1].key, "vllm-8001");
    assert!(rows[1].chips.is_empty());
    let cut = bus.read().models();
    assert!(
        cut[1].chips.is_empty(),
        "Die 3D's read keeps the chip limit"
    );
}

/// An explicit attach to a host on another machine lists its models with no
/// chip indexes: its PCI addresses are not this machine's.
#[cfg(unix)]
#[test]
fn another_hosts_models_get_no_local_chip_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock();
    let (_h, _s) = serving_host(&sock, &b, &fixture());
    let o = TuiBusOptions {
        local_host: "another-box/0123456789abcdef".into(),
        ..opts(Attach::Required(sock.clone()))
    };
    let bus = TuiBus::start(&o, &b).unwrap();
    assert!(bus.is_attached());
    assert!(!bus.origin_is_local());
    let rows = attached_rows(&bus, &b).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|s| s.chips.is_empty()),
        "{:?}",
        carried_all(&rows)
    );
}

// ---- fix round 1: the rotation ------------------------------------------------

/// An attached TUI on a box with no models of its own: the rotation sees
/// the host's live model, and does not ask the local monitor.
#[cfg(unix)]
#[test]
fn the_rotation_sees_an_attached_hosts_live_model() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock();
    let (_h, _s) = serving_host(&sock, &b, &ready_fixture());
    let bus = TuiBus::start(&opts(Attach::Probe(sock.clone())), &b).unwrap();
    assert!(bus.is_attached());
    let mut asked = false;
    assert!(inference_active(&bus, &b, || {
        asked = true;
        false
    }));
    assert!(!asked, "an attached TUI does not ask the local monitor");
}

/// An attached host with no models is idle, even when the local monitor
/// would say otherwise.
#[cfg(unix)]
#[test]
fn the_rotation_sees_an_attached_host_without_models_as_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let b = mock();
    let (_h, _s) = serving_host(&sock, &b, &[]);
    let bus = TuiBus::start(&opts(Attach::Probe(sock.clone())), &b).unwrap();
    assert!(bus.is_attached());
    assert!(!inference_active(&bus, &b, || true));
}

/// A TUI that hosts its own bus asks the local monitor, whatever its own
/// world holds.
#[test]
fn the_rotation_with_a_local_bus_asks_the_local_monitor() {
    let b = mock();
    let mut bus = TuiBus::start(&opts(Attach::Off), &b).unwrap();
    bus.feed_workloads(&[], Some(&ready_fixture()), None);
    bus.data_tick(&b, Instant::now());
    let mut asked = false;
    assert!(!inference_active(&bus, &b, || {
        asked = true;
        false
    }));
    assert!(asked);
    assert!(inference_active(&bus, &b, || true));
}

// ---- fix round 1: a lost host ---------------------------------------------------

/// The note for each link state.
#[cfg(unix)]
#[test]
fn a_lost_link_gets_a_note() {
    use tt_toplike_bus::Link;
    assert_eq!(stale_note_for(Link::Connected), None);
    assert_eq!(stale_note_for(Link::Reconnecting), Some("(reconnecting)"));
    assert_eq!(
        stale_note_for(Link::Disconnected),
        Some("(host disconnected)")
    );
}

/// The text of `line`.
fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// A stale roster gets the note on top and every line drawn dim; a lone
/// service with no roster still gets the note.
#[test]
fn a_stale_roster_is_dimmed_under_the_note() {
    use ratatui::style::Modifier;
    let rows = ready_fixture();
    let mut roster = super::super::inference_roster_lines(&rows, 80);
    let n = roster.len();
    assert!(n > 1);
    mark_stale(&mut roster, "(host disconnected)");
    assert_eq!(roster.len(), n + 1);
    assert_eq!(line_text(&roster[0]), "(host disconnected)");
    for l in &roster {
        for s in &l.spans {
            assert!(
                s.style.add_modifier.contains(Modifier::DIM),
                "{:?} in {:?}",
                s.content,
                line_text(l)
            );
        }
    }
    // The model names are still there.
    assert!(line_text(&roster[2]).contains("vllm-8000-model"));

    let mut none: Vec<ratatui::text::Line<'static>> = Vec::new();
    mark_stale(&mut none, "(reconnecting)");
    assert_eq!(none.len(), 1);
    assert_eq!(line_text(&none[0]), "(reconnecting)");
}

/// The real path: the host goes away, and the note goes from reconnecting
/// to host disconnected. A local bus never has a note.
#[cfg(unix)]
#[test]
fn a_dead_host_marks_the_i_view() {
    use std::time::Duration;
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("bus.sock");
    let mut b = mock();
    let (host, server) = serving_host(&sock, &b, &ready_fixture());
    let mut bus = TuiBus::start(&opts(Attach::Required(sock.clone())), &b).unwrap();
    assert_eq!(stale_note(&bus), None);
    drop(server);
    drop(host);
    let end = Instant::now() + Duration::from_secs(6);
    let mut seen = Vec::new();
    while Instant::now() < end {
        bus.data_tick(&b, Instant::now());
        let n = stale_note(&bus);
        if seen.last() != Some(&n) {
            seen.push(n);
        }
        if n == Some("(host disconnected)") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        b.update().unwrap();
    }
    assert!(seen.contains(&Some("(reconnecting)")), "{seen:?}");
    assert_eq!(seen.last(), Some(&Some("(host disconnected)")), "{seen:?}");
    // The models stay listed, from the last world received.
    assert_eq!(attached_rows(&bus, &b).unwrap().len(), 2);

    let local = TuiBus::start(&opts(Attach::Off), &b).unwrap();
    assert_eq!(stale_note(&local), None);
}

// ---- fix round 1: one estimate helper --------------------------------------------

/// Every model signal the world reads into a `ModelView`.
const MODEL_SIGNALS: [&str; 7] = [
    tt_toplike_bus::signals::MODEL_LABEL,
    tt_toplike_bus::signals::MODEL_PHASE,
    tt_toplike_bus::signals::MODEL_PROGRESS,
    tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
    tt_toplike_bus::signals::MODEL_RUNNING,
    tt_toplike_bus::signals::MODEL_WAITING,
    tt_toplike_bus::signals::MODEL_KV_CACHE,
];

/// The value of model signal `sig`: the base one, or another when `alt`.
fn signal_value(sig: &str, alt: bool) -> tt_toplike_bus::Value {
    use tt_toplike_bus::signals as s;
    use tt_toplike_bus::Value;
    let pick = |a: f64, b: f64| if alt { b } else { a };
    if sig == s::MODEL_LABEL {
        Value::Text(if alt { "other" } else { "m-model" }.into())
    } else if sig == s::MODEL_PHASE {
        Value::Text(if alt { "compiling" } else { "loading" }.into())
    } else if sig == s::MODEL_PROGRESS {
        Value::F64(pick(0.4, 0.7))
    } else if sig == s::MODEL_TOKENS_PER_S {
        Value::F64(pick(42.0, 7.0))
    } else if sig == s::MODEL_RUNNING {
        Value::U64(if alt { 5 } else { 2 })
    } else if sig == s::MODEL_WAITING {
        Value::U64(if alt { 9 } else { 3 })
    } else {
        Value::F64(pick(0.25, 0.9))
    }
}

/// A loading model on chip A with every model signal set, built from
/// records on a real bus. The signal `estimated` names is marked
/// `Estimated` and the rest `Measured`. The signal `changed` names gets
/// another value.
fn signal_world(estimated: Option<&str>, changed: Option<&str>) -> LocalView {
    use tt_toplike_bus::{Bus, Event, Payload, Provenance, Sample};
    let bus = Bus::open_with_world(BusConfig::default()).unwrap();
    let model = EntityKey::model("m");
    bus.publish(
        "test",
        Payload::Event(Event::ModelLoading {
            model: model.clone(),
            chips: vec![EntityKey::chip(A)],
            progress: Some(0.4),
        }),
    );
    for sig in MODEL_SIGNALS {
        let prov = if estimated == Some(sig) {
            Provenance::Estimated
        } else {
            Provenance::Measured
        };
        bus.publish(
            "test",
            Payload::Sample(Sample {
                entity: model.clone(),
                signal: sig.into(),
                value: signal_value(sig, changed == Some(sig)),
                prov,
            }),
        );
    }
    let snap = bus.snapshot().unwrap();
    LocalView {
        seq: bus.head(),
        world: snap.world,
        now_mono_ns: 0,
        header_kind: HeaderKind::Live,
    }
}

/// For each model signal marked `Estimated` alone, and for none: Die 3D's
/// flag, the `i` view's label and the shared helper all agree on `~`, and
/// the marker is set exactly when the signal feeds the flag text.
#[test]
fn die3d_i_and_the_helper_agree_on_the_estimate_marker() {
    use crate::animation::die3d::overlay::{model_text_is_estimated, FLAG_TEXT_SIGNALS};
    let cases: Vec<Option<&str>> = std::iter::once(None)
        .chain(MODEL_SIGNALS.iter().map(|s| Some(*s)))
        .collect();
    for est in cases {
        let w = signal_world(est, None);
        let m = w.models().remove(0);
        let helper = model_text_is_estimated(&w, &m);
        let flag = die3d_flag(&w).starts_with('~');
        let i = rows_from_world(&w, &mock_index)[0].label.starts_with('~');
        assert!(
            helper == flag && flag == i,
            "{est:?}: helper {helper}, Die 3D {flag}, i {i}"
        );
        let want = est.is_some_and(|s| FLAG_TEXT_SIGNALS.contains(&s));
        assert_eq!(helper, want, "{est:?}");
    }
}

/// The helper's signal list is exactly the signals that change Die 3D's
/// flag text: change one signal's value and the text changes if and only
/// if the signal is on the list.
#[test]
fn the_estimate_list_is_the_flag_texts_inputs() {
    use crate::animation::die3d::overlay::FLAG_TEXT_SIGNALS;
    let base = die3d_flag(&signal_world(None, None));
    for sig in MODEL_SIGNALS {
        let changed = die3d_flag(&signal_world(None, Some(sig)));
        assert_eq!(
            changed != base,
            FLAG_TEXT_SIGNALS.contains(&sig),
            "{sig}: {base:?} -> {changed:?}"
        );
    }
}
