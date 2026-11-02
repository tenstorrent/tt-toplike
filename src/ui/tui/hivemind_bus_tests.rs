// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests of what the `~` view draws from the bus: the strip, the bus rows in
//! the feed, the KITT scanner's bus input, and the rendered view with an
//! empty world (pinned against the view before the bus existed).

use super::*;
use crate::ui::tui::hivemind_view::{kitt_activity_norm, render_hivemind, KittScanner};
use crate::workload::hivemind::{event::*, Hivemind};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use std::time::Duration;
use tt_toplike_bus::{LocalView, Payload, Provenance, Record, Sample, Value, World};

const SEC_NS: u64 = 1_000_000_000;
/// The data tick: 100 ms between updates.
const TICK_NS: u64 = 100_000_000;

/// The chip key of device `i`: its PCI address, as the mock backend sets
/// `bus_id`.
fn chip_key(i: usize) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i + 1))
}

/// Two local chips, device indices 0 and 1.
fn two_chips() -> Vec<(usize, EntityKey)> {
    vec![(0, chip_key(0)), (1, chip_key(1))]
}

/// A world built record by record (one millisecond apart), read through a
/// `LocalView` at a clock the test sets.
struct Fx {
    world: World,
    seq: u64,
    t: u64,
}

impl Fx {
    fn new() -> Fx {
        Fx {
            world: World::new(),
            seq: 0,
            t: SEC_NS,
        }
    }
    fn push(&mut self, payload: Payload) -> &mut Self {
        self.seq += 1;
        self.t += 1_000_000;
        self.world.apply(&Record {
            seq: self.seq,
            t_mono_ns: self.t,
            t_wall_ms: 1_790_000_000_000 + self.seq,
            source: "test".into(),
            payload,
        });
        self
    }
    fn event(&mut self, e: Event) -> &mut Self {
        self.push(Payload::Event(e))
    }
    fn sample(&mut self, e: &EntityKey, s: &str, v: Value, p: Provenance) -> &mut Self {
        self.push(Payload::Sample(Sample {
            entity: e.clone(),
            signal: s.into(),
            value: v,
            prov: p,
        }))
    }
    fn label(&mut self, key: &str, label: &str, p: Provenance) -> &mut Self {
        self.sample(
            &EntityKey::model(key),
            tt_toplike_bus::signals::MODEL_LABEL,
            Value::Text(label.into()),
            p,
        )
    }
    fn tps(&mut self, key: &str, tps: f64) -> &mut Self {
        self.sample(
            &EntityKey::model(key),
            tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
            Value::F64(tps),
            Provenance::Measured,
        )
    }
    fn loading(&mut self, key: &str, chips: &[usize], progress: Option<f32>) -> &mut Self {
        if let Some(p) = progress {
            self.sample(
                &EntityKey::model(key),
                tt_toplike_bus::signals::MODEL_PROGRESS,
                Value::F64(p as f64),
                Provenance::Measured,
            );
        }
        self.event(Event::ModelLoading {
            model: EntityKey::model(key),
            chips: chips.iter().map(|&i| chip_key(i)).collect(),
            progress,
        })
    }
    fn ready(&mut self, key: &str, chips: &[usize]) -> &mut Self {
        self.event(Event::ModelReady {
            model: EntityKey::model(key),
            chips: chips.iter().map(|&i| chip_key(i)).collect(),
        })
    }
    fn requests(&mut self, key: &str, n: u32) -> &mut Self {
        self.event(Event::RequestsCompleted {
            model: EntityKey::model(key),
            n,
        })
    }
    fn view(&self, now: u64) -> LocalView {
        LocalView {
            seq: self.world.seq(),
            world: self.world.clone(),
            now_mono_ns: now,
            header_kind: tt_toplike_bus::HeaderKind::Live,
        }
    }
}

/// A view memory that has read `f` once at `now` (so later events are not
/// history), as an attached TUI (`local = false`).
fn primed(f: &Fx, now: u64, at: Instant) -> HivemindBus {
    let mut b = HivemindBus::default();
    b.read_world(&f.view(now), &two_chips(), false, now, at);
    b
}

// ---- The rendered view ------------------------------------------------------

/// FNV-1a over every cell: symbol, colours and modifiers.
fn buffer_hash(buf: &Buffer) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut put = |b: &[u8]| {
        for x in b {
            h ^= *x as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for c in buf.content() {
        put(c.symbol().as_bytes());
        put(format!("{:?}{:?}{:?}", c.fg, c.bg, c.modifier).as_bytes());
        put(&[0xff]);
    }
    h
}

/// The rendered view of `hive` and `bus` at `w`x`h`.
fn render(
    hive: &Hivemind,
    bus: &HivemindBus,
    w: u16,
    h: u16,
    unified: bool,
    sev: Severity,
) -> Buffer {
    let mut kitt = KittScanner::new();
    kitt.pos = 0.37;
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| {
        let a = f.area();
        render_hivemind(f, a, hive, bus, (0, 0), true, unified, sev, &kitt);
    })
    .unwrap();
    t.backend().buffer().clone()
}

/// Each row of `buf` as text. The cell after a double-width glyph holds a
/// filler, which is skipped.
fn lines(buf: &Buffer) -> Vec<String> {
    let w = buf.area.width as usize;
    buf.content()
        .chunks(w)
        .map(|row| {
            let mut s = String::new();
            let mut skip = 0usize;
            for c in row {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                s.push_str(c.symbol());
                skip = unicode_width::UnicodeWidthStr::width(c.symbol()).saturating_sub(1);
            }
            s
        })
        .collect()
}

/// The engine states the golden hashes were taken with: an empty engine,
/// and one with board heat, a reset row and a ring event. The active one
/// waits out the board's 300 ms pulse so its cells are drawn steady.
fn golden_hives() -> (Hivemind, Hivemind) {
    let empty = Hivemind::new();
    let mut active = Hivemind::new();
    active.bump_grid_for_test(Source::TtMetal, Some(0));
    active.bump_grid_for_test(Source::Ttnn, None);
    active.inject_reset("tt-smi -r 1 detected".into(), &[1]);
    active.push_for_test(SniffEvent {
        ts: Instant::now(),
        source: Source::TtMetal,
        device: Some(0),
        severity: Severity::Warn,
        kind: EventKind::Compile,
        text: "brisc.cpp.o compiled".into(),
        origin: "cache_watch".into(),
    });
    std::thread::sleep(Duration::from_millis(350));
    (empty, active)
}

/// Contract rule 1. With an empty world the view draws exactly what it drew
/// before the bus: these hashes were taken from `render_hivemind` at the
/// base commit (31d4747d) with the same engines, sizes and scanner position.
/// The bus memory has read the empty world, so this is the wired path.
#[test]
fn an_empty_world_draws_the_view_as_before_the_bus() {
    let (empty, active) = golden_hives();
    let f = Fx::new();
    let mut bus = HivemindBus::default();
    for k in 0..3 {
        let now = 5 * SEC_NS + k * TICK_NS;
        bus.read_world(&f.view(now), &two_chips(), false, now, Instant::now());
    }
    assert!(bus.strip().is_empty());
    let cases = [
        (
            "empty_120x40",
            &empty,
            120u16,
            40u16,
            false,
            0xc5e979cf38d66cc2u64,
        ),
        ("empty_60x20", &empty, 60, 20, false, 0x18127313d02ba013),
        (
            "active_80x24_unified",
            &active,
            80,
            24,
            true,
            0xa7ad9db4f5174c32,
        ),
        (
            "active_80x24_cell",
            &active,
            80,
            24,
            false,
            0x513f932162d75c93,
        ),
    ];
    for (name, hive, w, h, unified, want) in cases {
        let got = buffer_hash(&render(hive, &bus, w, h, unified, Severity::Trace));
        assert_eq!(got, want, "{name}: {got:#018x}");
    }
}

// ---- The strip --------------------------------------------------------------

#[test]
fn a_loading_model_shows_its_phase_word_and_a_bar_that_follows_progress() {
    let bar_of = |p: f32| {
        let mut f = Fx::new();
        f.label("llama", "Llama", Provenance::Measured)
            .loading("llama", &[0], Some(p));
        let b = primed(&f, 5 * SEC_NS, Instant::now());
        assert_eq!(b.strip().len(), 1, "one cell: chip 0 runs the only model");
        let line = b.strip_line(200).expect("a strip");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("D0 Llama loading"), "{text}");
        text.chars().filter(|c| *c == '█').count()
    };
    let quarter = bar_of(0.25);
    let most = bar_of(0.75);
    assert_eq!(quarter, 2);
    assert_eq!(most, 6);
}

#[test]
fn a_ready_model_says_ready_and_an_estimated_one_carries_the_mark() {
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured)
        .tps("llama", 42.0)
        .ready("llama", &[1]);
    let b = primed(&f, 5 * SEC_NS, Instant::now());
    let text: String = b
        .strip_line(200)
        .unwrap()
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect();
    assert!(text.starts_with("D1 Llama ready 42t/s"), "{text}");

    let mut f = Fx::new();
    f.label("mock", "Mock", Provenance::Estimated)
        .ready("mock", &[0]);
    let b = primed(&f, 5 * SEC_NS, Instant::now());
    let text: String = b
        .strip_line(200)
        .unwrap()
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect();
    assert!(text.starts_with("D0 ~Mock ready"), "{text}");
}

#[test]
fn the_strip_never_overflows_its_row() {
    let mut f = Fx::new();
    f.label(
        "big",
        "A model with a very long name indeed",
        Provenance::Estimated,
    )
    .loading("big", &[0, 1], Some(0.4));
    let chips: Vec<(usize, EntityKey)> = (0..2).map(|i| (i, chip_key(i))).collect();
    let mut b = HivemindBus::default();
    b.read_world(
        &f.view(5 * SEC_NS),
        &chips,
        false,
        5 * SEC_NS,
        Instant::now(),
    );
    for w in 0..140usize {
        let width = b
            .strip_line(w)
            .map(|l| l.width())
            .expect("a strip at every width");
        assert!(width <= w, "width {w}: strip is {width} wide");
    }
}

// ---- Bus rows in the feed ---------------------------------------------------

/// One world with one event of each kind after the first read, read by an
/// attached TUI.
fn one_of_each() -> HivemindBus {
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured);
    f.label("old", "Old", Provenance::Measured)
        .ready("old", &[1]);
    let at = Instant::now();
    let mut b = primed(&f, 5 * SEC_NS, at);
    f.loading("llama", &[0], Some(0.5))
        .ready("llama", &[0])
        .requests("llama", 3)
        .event(Event::ModelGone {
            model: EntityKey::model("old"),
        })
        .event(Event::KernelCompiled {
            chip: Some(chip_key(0)),
            source: "metal".into(),
        })
        .event(Event::Spark {
            chip: Some(chip_key(1)),
            kind: "vLLM.log".into(),
        })
        .event(Event::Reset {
            chips: vec![chip_key(1)],
        });
    let now = 5 * SEC_NS + TICK_NS;
    b.read_world(&f.view(now), &two_chips(), false, now, at);
    b
}

#[test]
fn each_bus_event_kind_gives_one_row_with_its_documented_severity() {
    let b = one_of_each();
    let all = b.rows(None, Severity::Trace);
    // The `old` model's announcement came before the first read: one
    // history row, listed after every live row.
    assert_eq!(all.iter().filter(|r| r.history).count(), 1);
    assert!(all.last().unwrap().history && all.last().unwrap().kind == BusKind::ModelReady);
    let rows: Vec<&BusRow> = all.into_iter().filter(|r| !r.history).collect();
    let mut kinds: Vec<(BusKind, Severity)> = rows.iter().map(|r| (r.kind, r.severity)).collect();
    kinds.sort_by_key(|(k, _)| *k as u8);
    assert_eq!(
        kinds,
        vec![
            (BusKind::ModelLoading, Severity::Notice),
            (BusKind::ModelReady, Severity::Notice),
            (BusKind::ModelGone, Severity::Warn),
            (BusKind::Requests, Severity::Info),
            (BusKind::Kernel, Severity::Info),
            (BusKind::Spark, Severity::Info),
            (BusKind::Reset, Severity::Warn),
        ]
    );
    assert!(rows.iter().all(|r| !r.history));
    let req = rows.iter().find(|r| r.kind == BusKind::Requests).unwrap();
    assert_eq!(req.count, 3, "a requests row counts requests");
    assert_eq!(req.device, Some(0));
    let spark = rows.iter().find(|r| r.kind == BusKind::Spark).unwrap();
    assert_eq!(spark.source, Some(Source::Vllm));
    assert_eq!(spark.device, Some(1));
}

#[test]
fn the_severity_floor_hides_bus_rows_below_it() {
    let b = one_of_each();
    let notice: Vec<BusKind> = b
        .rows(None, Severity::Notice)
        .iter()
        .filter(|r| !r.history)
        .map(|r| r.kind)
        .collect();
    assert_eq!(notice.len(), 4, "{notice:?}");
    assert!(!notice.contains(&BusKind::Requests));
    assert!(!notice.contains(&BusKind::Spark));
    let warn = b.rows(None, Severity::Warn);
    assert_eq!(warn.len(), 2);
    assert!(b.rows(None, Severity::Error).is_empty());
}

#[test]
fn the_unified_feed_shows_bus_rows_and_a_cell_feed_shows_only_its_own() {
    let b = one_of_each();
    let hive = Hivemind::new();
    let unified = lines(&render(&hive, &b, 120, 40, true, Severity::Trace)).join("\n");
    for want in [
        "bus: Llama ready",
        "bus: Llama loading 50%",
        "bus: Llama requests completed",
        "bus: Old gone",
        "bus: kernel compiled",
        "bus: vLLM.log",
        "bus: chip reset",
    ] {
        assert!(unified.contains(want), "{want} missing:\n{unified}");
    }
    // A cell filter keeps the rows of that source and device only.
    let cell = b.rows(Some((Source::Vllm, Some(1))), Severity::Trace);
    assert_eq!(cell.len(), 1);
    assert_eq!(cell[0].kind, BusKind::Spark);
    // The drawn cell feed: the focus is on the board's only cell, metal on
    // D0, so the bus's metal kernel compile on D0 shows and nothing else.
    let mut busy = Hivemind::new();
    busy.bump_grid_for_test(Source::TtMetal, Some(0));
    let per_cell = lines(&render(&busy, &b, 120, 40, false, Severity::Trace)).join("\n");
    assert!(per_cell.contains("bus: kernel compiled"), "{per_cell}");
    assert_eq!(per_cell.matches("bus: ").count(), 1, "{per_cell}");
    // The severity floor applies to the drawn feed too.
    let warn = lines(&render(&hive, &b, 120, 40, true, Severity::Warn)).join("\n");
    assert!(warn.contains("bus: Old gone"));
    assert!(!warn.contains("bus: Llama ready"));
}

#[test]
fn history_is_dimmed_and_a_later_event_is_full_strength() {
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured)
        .ready("llama", &[0])
        .requests("llama", 4);
    let at = Instant::now();
    let mut b = primed(&f, 5 * SEC_NS, at);
    let rows = b.rows(None, Severity::Trace);
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|r| r.history),
        "events before the first read are history"
    );
    assert_eq!(b.kitt_activity(0.0, at), 0.0, "history makes no surge");

    // Drawn dim.
    let hive = Hivemind::new();
    let buf = render(&hive, &b, 120, 40, true, Severity::Trace);
    let text = lines(&buf);
    let y = text
        .iter()
        .position(|l| l.contains("bus: Llama ready"))
        .unwrap();
    let x = text[y].find("bus:").unwrap();
    let col = text[y][..x].chars().count() as u16;
    assert!(buf[(col, y as u16)].modifier.contains(Modifier::DIM));

    // A later event is live and drawn without DIM, above the history.
    f.event(Event::ModelGone {
        model: EntityKey::model("llama"),
    });
    let now = 5 * SEC_NS + TICK_NS;
    b.read_world(&f.view(now), &two_chips(), false, now, at);
    let rows = b.rows(None, Severity::Trace);
    assert_eq!(rows[0].kind, BusKind::ModelGone);
    assert!(!rows[0].history);
    let buf = render(&hive, &b, 120, 40, true, Severity::Trace);
    let text = lines(&buf);
    let y = text
        .iter()
        .position(|l| l.contains("bus: Llama gone"))
        .unwrap();
    let x = text[y].find("bus:").unwrap();
    let col = text[y][..x].chars().count() as u16;
    assert!(!buf[(col, y as u16)].modifier.contains(Modifier::DIM));
}

/// Die 3D's rule: an update more than a pulse lifetime after the last one,
/// and more than four times the usual gap, reads what arrived meanwhile as
/// history.
#[test]
fn events_that_arrived_while_the_view_was_away_are_history() {
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured);
    let at = Instant::now();
    let mut b = HivemindBus::default();
    for k in 0..3 {
        let now = 5 * SEC_NS + k * TICK_NS;
        b.read_world(&f.view(now), &two_chips(), false, now, at);
    }
    f.ready("llama", &[0]).requests("llama", 2);
    let now = 5 * SEC_NS + 2 * TICK_NS + 30 * SEC_NS;
    b.read_world(&f.view(now), &two_chips(), false, now, at);
    let rows = b.rows(None, Severity::Trace);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.history));
    assert_eq!(b.kitt_activity(0.0, at), 0.0);
    // The update after that is a normal one again: a new event is live.
    f.requests("llama", 1);
    let now = now + TICK_NS;
    b.read_world(&f.view(now), &two_chips(), false, now, at);
    let req = b.rows(None, Severity::Trace);
    assert!(!req[0].history && req[0].kind == BusKind::Requests);
}

/// The host re-announces every model every five minutes. An announcement
/// with the same phase and chips adds nothing.
#[test]
fn a_re_announcement_adds_no_row() {
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured);
    let at = Instant::now();
    let mut b = primed(&f, 5 * SEC_NS, at);
    f.ready("llama", &[0]);
    b.read_world(
        &f.view(5 * SEC_NS + TICK_NS),
        &two_chips(),
        false,
        5 * SEC_NS + TICK_NS,
        at,
    );
    f.ready("llama", &[0]);
    b.read_world(
        &f.view(5 * SEC_NS + 2 * TICK_NS),
        &two_chips(),
        false,
        5 * SEC_NS + 2 * TICK_NS,
        at,
    );
    let rows = b.rows(None, Severity::Trace);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].count, 1);
}

/// On a local bus this sweeper's own events are already on the board and in
/// the feed; the bus copies of them are not shown twice.
#[test]
fn a_local_bus_does_not_repeat_the_sweepers_own_events() {
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured);
    let at = Instant::now();
    let mut b = HivemindBus::default();
    b.read_world(&f.view(5 * SEC_NS), &two_chips(), true, 5 * SEC_NS, at);
    f.ready("llama", &[0])
        .event(Event::Spark {
            chip: Some(chip_key(0)),
            kind: "metal.log".into(),
        })
        .event(Event::KernelCompiled {
            chip: None,
            source: "ttnn".into(),
        })
        .event(Event::Reset {
            chips: vec![chip_key(0)],
        });
    let now = 5 * SEC_NS + TICK_NS;
    b.read_world(&f.view(now), &two_chips(), true, now, at);
    let kinds: Vec<BusKind> = b
        .rows(None, Severity::Trace)
        .iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(kinds, vec![BusKind::ModelReady]);
}

/// Contract rule 4, through the real attached read path: a world limited to
/// the local chips (`ReadView::only_chips`). A model, sparks and a reset on
/// a chip this TUI does not have give no strip cell and no feed row.
#[test]
fn a_chip_this_tui_does_not_have_gets_no_strip_cell_and_no_row() {
    use crate::backend::mock::MockBackend;
    use crate::backend::TelemetryBackend as _;
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    let mut backend = MockBackend::new(1);
    backend.init().unwrap();
    backend.update().unwrap();
    let host = BusHost::new(tt_toplike_bus::BusConfig::default(), &backend).unwrap();
    let foreign = EntityKey::chip("0000:ee:00.0");
    let view = BusView::Local(Box::new(host));
    let cache = ReadCache::new();
    let local: Arc<BTreeSet<EntityKey>> = Arc::new([chip_key(0)].into_iter().collect());
    let chips = vec![(0usize, chip_key(0))];
    let read = |v: &BusView| v.read(&cache).only_chips(local.clone());

    let mut b = HivemindBus::default();
    let w = read(&view);
    let now = w.now_mono_ns();
    b.read_world(&w, &chips, false, now, Instant::now());

    #[allow(irrefutable_let_patterns)]
    let BusView::Local(h) = &view
    else {
        unreachable!()
    };
    let p = |e: Event| {
        h.bus().publish("test", Payload::Event(e));
    };
    p(Event::ModelReady {
        model: EntityKey::model("theirs"),
        chips: vec![foreign.clone()],
    });
    p(Event::RequestsCompleted {
        model: EntityKey::model("theirs"),
        n: 9,
    });
    p(Event::Spark {
        chip: Some(foreign.clone()),
        kind: "metal.log".into(),
    });
    p(Event::KernelCompiled {
        chip: Some(foreign.clone()),
        source: "metal".into(),
    });
    p(Event::Reset {
        chips: vec![foreign.clone()],
    });
    let w = read(&view);
    b.read_world(&w, &chips, false, now + TICK_NS, Instant::now());
    assert!(b.strip().is_empty());
    assert!(
        b.rows(None, Severity::Trace).is_empty(),
        "{:?}",
        b.rows(None, Severity::Trace)
    );
    assert_eq!(b.kitt_activity(0.0, Instant::now()), 0.0);

    // The same events on the local chip do show.
    p(Event::ModelReady {
        model: EntityKey::model("ours"),
        chips: vec![chip_key(0)],
    });
    p(Event::Spark {
        chip: Some(chip_key(0)),
        kind: "metal.log".into(),
    });
    let w = read(&view);
    b.read_world(&w, &chips, false, now + 2 * TICK_NS, Instant::now());
    assert_eq!(b.strip().len(), 1);
    assert_eq!(b.rows(None, Severity::Trace).len(), 2);
}

/// Attached to a host on another machine (`TuiBus::origin_is_local` is
/// false), no chip is local: `update` passes no chips. `ReadView` would let
/// through events on a foreign chip whose PCI address happens to equal a
/// local one, so this view's own check is what keeps them off screen. A
/// spark with no chip is the host's and still shows.
#[test]
fn attached_to_another_machine_no_chip_is_ours() {
    let mut f = Fx::new();
    f.label("theirs", "Theirs", Provenance::Measured);
    let at = Instant::now();
    let mut b = HivemindBus::default();
    b.read_world(&f.view(5 * SEC_NS), &[], false, 5 * SEC_NS, at);
    f.tps("theirs", 500.0)
        .ready("theirs", &[0])
        .requests("theirs", 7)
        .event(Event::Spark {
            chip: Some(chip_key(0)),
            kind: "metal.log".into(),
        })
        .event(Event::KernelCompiled {
            chip: Some(chip_key(0)),
            source: "metal".into(),
        })
        .event(Event::Reset {
            chips: vec![chip_key(0)],
        })
        .event(Event::Spark {
            chip: None,
            kind: "driver.driver".into(),
        });
    let now = 5 * SEC_NS + TICK_NS;
    b.read_world(&f.view(now), &[], false, now, at);
    assert!(b.strip().is_empty());
    let kinds: Vec<(BusKind, Option<u8>)> = b
        .rows(None, Severity::Trace)
        .iter()
        .map(|r| (r.kind, r.device))
        .collect();
    assert_eq!(kinds, vec![(BusKind::Spark, None)]);
    assert_eq!(
        b.kitt_activity(0.0, at),
        0.0,
        "their traffic moves nothing here"
    );
}

// ---- The KITT scanner -------------------------------------------------------

#[test]
fn with_no_bus_signal_the_scanner_reads_the_board_alone() {
    let b = HivemindBus::default();
    let at = Instant::now();
    for rate in [0.0f32, 0.5, 3.0, 12.0, 40.0] {
        assert_eq!(b.kitt_activity(rate, at), kitt_activity_norm(rate));
        assert_eq!(
            KittScanner::speed(b.kitt_activity(rate, at)),
            KittScanner::speed(kitt_activity_norm(rate))
        );
    }
    // A world with a model and no traffic adds nothing either.
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured)
        .ready("llama", &[0]);
    let b = primed(&f, 5 * SEC_NS, at);
    assert_eq!(b.kitt_activity(3.0, at), kitt_activity_norm(3.0));
}

#[test]
fn a_request_surge_raises_the_activity_and_decays() {
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured)
        .ready("llama", &[0]);
    let at = Instant::now();
    let mut b = primed(&f, 5 * SEC_NS, at);
    f.requests("llama", 5);
    let now = 5 * SEC_NS + TICK_NS;
    b.read_world(&f.view(now), &two_chips(), false, now, at);
    let peak = b.kitt_activity(0.0, at);
    assert!(peak > 0.3, "a surge: {peak}");
    assert!(KittScanner::speed(peak) < KittScanner::speed(0.0));
    let later = b.kitt_activity(0.0, at + Duration::from_millis(500));
    assert!(later < peak && later > 0.0, "{later} after {peak}");
    // The surge is over after a pulse lifetime; the smoothed rate stays,
    // and decays with quiet updates.
    let after = b.kitt_activity(0.0, at + Duration::from_millis(1100));
    assert!(after < later);
    let mut quiet = after;
    for k in 2..60 {
        let now = 5 * SEC_NS + k * TICK_NS;
        b.read_world(&f.view(now), &two_chips(), false, now, at);
        let a = b.kitt_activity(0.0, at + Duration::from_secs(10));
        assert!(a <= quiet + 1e-6);
        quiet = a;
    }
    assert!(quiet < after / 2.0, "{quiet} vs {after}");
}

#[test]
fn tokens_per_second_raise_the_floor() {
    let at = Instant::now();
    let floor_at = |tps: f64| {
        let mut f = Fx::new();
        f.label("llama", "Llama", Provenance::Measured)
            .tps("llama", tps)
            .ready("llama", &[0]);
        primed(&f, 5 * SEC_NS, at).kitt_activity(0.0, at)
    };
    let low = floor_at(10.0);
    let high = floor_at(80.0);
    assert!(low > 0.0);
    assert!(high > low);
    assert!((floor_at(10_000.0) - 1.0).abs() < 1e-6);
    // The board still wins when it is busier.
    let mut f = Fx::new();
    f.label("llama", "Llama", Provenance::Measured)
        .tps("llama", 10.0)
        .ready("llama", &[0]);
    let b = primed(&f, 5 * SEC_NS, at);
    assert_eq!(b.kitt_activity(12.0, at), 1.0);
}

// ---- Sizes ------------------------------------------------------------------

/// The size sweep of `hivemind_view`'s smoke test, with a busy bus: a strip
/// on two chips, live and history rows, and labels longer than the board's
/// source column. Nothing panics, and where the FOCUS pane is drawn its
/// divider stays in one column on every row, which it would not if a strip
/// or bus row ran past the left column.
#[test]
fn the_view_with_a_busy_bus_draws_at_every_size() {
    let mut b = one_of_each();
    let mut f = Fx::new();
    f.label(
        "w",
        "A model whose name is far too long for any strip cell",
        Provenance::Estimated,
    )
    .loading("w", &[0, 1], Some(0.3));
    let at = Instant::now();
    b.read_world(&f.view(9 * SEC_NS), &two_chips(), false, 9 * SEC_NS, at);
    f.event(Event::Spark {
        chip: None,
        kind: "workload.process".into(),
    });
    b.read_world(
        &f.view(9 * SEC_NS + TICK_NS),
        &two_chips(),
        false,
        9 * SEC_NS + TICK_NS,
        at,
    );
    let (_, active) = golden_hives();
    for (w, h) in [
        (160u16, 50u16),
        (120, 40),
        (98, 30),
        (60, 20),
        (20, 8),
        (12, 6),
        (5, 4),
    ] {
        for unified in [false, true] {
            let text = lines(&render(&active, &b, w, h, unified, Severity::Trace));
            let cols: Vec<usize> = text
                .iter()
                .filter_map(|l| {
                    l.find('│')
                        .map(|i| unicode_width::UnicodeWidthStr::width(&l[..i]))
                })
                .collect();
            if w >= 98 {
                assert!(cols.len() > 5, "{w}x{h}: the FOCUS pane is drawn");
            }
            assert!(
                cols.windows(2).all(|p| p[0] == p[1]),
                "{w}x{h} unified={unified}: divider moved {cols:?}\n{}",
                text.join("\n")
            );
        }
    }
}
