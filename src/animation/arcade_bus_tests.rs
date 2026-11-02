// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Arcade frames with and without the signal bus.
//!
//! The golden hashes pin the frames the arcade drew before it read the bus.
//! They were taken by running `golden_hashes` against the commit before the
//! bus port (the commit that added this file holds only these tests), so a
//! frame drawn from an empty world must hash to exactly these values.

use super::*;
use crate::models::{Device, SmbusTelemetry, Telemetry};
use ratatui::style::Color;

/// A backend whose readings never change: two p300 chips at fixed power,
/// current, temperature and clock. The mock backend adds random noise, so its
/// frames cannot be pinned.
pub(super) struct Fixed {
    devices: Vec<Device>,
    telem: Vec<Telemetry>,
    smbus: SmbusTelemetry,
}

impl Fixed {
    /// `n` p300 chips with PCI addresses `0000:01:00.0`, `0000:02:00.0`, ...
    pub(super) fn new(n: usize) -> Fixed {
        let devices: Vec<Device> = (0..n)
            .map(|i| {
                Device::new(
                    i,
                    "p300".into(),
                    format!("0000:{:02x}:00.0", i + 1),
                    format!("(0,{i})"),
                )
            })
            .collect();
        let telem = (0..n)
            .map(|i| {
                let mut t = Telemetry::new();
                t.power = Some(45.0 + 9.0 * i as f32);
                t.current = Some(40.0 + 5.0 * i as f32);
                t.asic_temperature = Some(52.0 + 4.0 * i as f32);
                t.aiclk = Some(1000);
                t.voltage = Some(0.85);
                t.heartbeat = Some(7);
                t
            })
            .collect();
        Fixed {
            devices,
            telem,
            smbus: SmbusTelemetry::new(),
        }
    }
}

impl TelemetryBackend for Fixed {
    fn init(&mut self) -> crate::error::BackendResult<()> {
        Ok(())
    }
    fn update(&mut self) -> crate::error::BackendResult<()> {
        Ok(())
    }
    fn devices(&self) -> &[Device] {
        &self.devices
    }
    fn telemetry(&self, i: usize) -> Option<&Telemetry> {
        self.telem.get(i)
    }
    fn smbus_telemetry(&self, _i: usize) -> Option<&SmbusTelemetry> {
        Some(&self.smbus)
    }
    fn backend_info(&self) -> String {
        "fixed".into()
    }
}

/// FNV-1a, 64 bit: the same number on every run and machine.
struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn color(&mut self, c: Option<Color>) {
        match c {
            None => self.bytes(&[0]),
            Some(Color::Reset) => self.bytes(&[1]),
            Some(Color::Rgb(r, g, b)) => self.bytes(&[2, r, g, b]),
            Some(other) => self.bytes(format!("{other:?}").as_bytes()),
        }
    }
}

/// A hash of rendered lines, cell by cell: each character with its colours
/// and modifiers, and a marker at each line end. How cells are grouped into
/// spans does not enter it.
pub(super) fn frame_hash(lines: &[Line<'static>]) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    for line in lines {
        for span in &line.spans {
            let style = line.style.patch(span.style);
            for ch in span.content.chars() {
                h.bytes(&(ch as u32).to_le_bytes());
                h.color(style.fg);
                h.color(style.bg);
                h.bytes(&style.add_modifier.bits().to_le_bytes());
                h.bytes(&style.sub_modifier.bits().to_le_bytes());
            }
        }
        h.bytes(&[0xff, 0xfe]);
    }
    h.0
}

/// The sizes the golden hashes pin: a small terminal and a large one.
const SIZES: [(usize, usize); 2] = [(60, 18), (160, 50)];
/// The updates after which a frame is hashed.
const HASHED_AT: [u32; 3] = [1, 10, 30];

/// Hashes of the frames of an arcade at `w` x `h` over [`Fixed`], after each
/// update in [`HASHED_AT`]. `before_update` runs before every update (the
/// bus tests hand the arcade an empty world there). The colours are pinned
/// through [`tc`]: truecolor on, and the default theme under the theme lock.
pub(super) fn hashes_with(
    w: usize,
    h: usize,
    mut before_update: impl FnMut(&mut ArcadeVisualization, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(2);
        let mut a = ArcadeVisualization::new(w, h);
        a.initialize_from_devices(backend.devices());
        a.initialize_topology(&backend);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut a, &backend, i);
            a.update(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&a.render(&backend)));
            }
        }
        out
    })
}

/// The frames as the arcade drew them before the bus port, at each of
/// [`SIZES`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 3]; 2] = [
    [
        2872948124327350074,
        11781309314807476498,
        4393317451934106095,
    ],
    [
        1981102043556422336,
        3972904484839130755,
        8297947485695072225,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn golden_hashes() {
    for (w, h) in SIZES {
        println!("{w}x{h}: {:?}", hashes_with(w, h, |_, _, _| {}));
    }
}

#[test]
fn the_frames_with_no_bus_are_the_pinned_frames() {
    for (i, (w, h)) in SIZES.into_iter().enumerate() {
        assert_eq!(
            hashes_with(w, h, |_, _, _| {}),
            GOLDEN[i].to_vec(),
            "{w}x{h}"
        );
    }
}

/// The golden hashes and every colour comparison below go through
/// `colors::rgb`, which reads the app-wide theme. Both helpers must pin the
/// default theme under the theme lock, or a test that flips the theme
/// (`colors::tests::theme_state_roundtrips`) can change a frame mid-hash.
#[test]
fn the_colour_helpers_pin_the_default_theme() {
    hashes_with(60, 18, |_, _, _| {
        assert!(
            crate::ui::colors::theme_is_pinned_to_default(),
            "hashes_with"
        )
    });
    tc(|| assert!(crate::ui::colors::theme_is_pinned_to_default(), "tc"));
}

// ---- The bus ---------------------------------------------------------------

use crate::animation::arcade_bus::{HEARTBEAT_FRAMES, MAX_HIT_GLYPHS, TPS_STALE_MS};
use crate::animation::die3d::overlay::{DOWN_GREY, FLASH_NS, LOAD_PURPLE, SERVE_TEAL, SETTLE_NS};
use std::sync::atomic::{AtomicUsize, Ordering};
use tt_toplike_bus::{
    EntityKey, Event, HeaderKind, LocalView, ModelView, Payload, Provenance, Reading, Record,
    Sample, Value, World, WorldRead,
};

const SEC: u64 = 1_000_000_000;
/// The gap between two arcade updates at 30 frames a second.
const TICK: u64 = 33_000_000;
/// The arcade size the bus tests draw at: two slots of 60 columns.
const W: usize = 120;
const H: usize = 40;
/// The gauge row: the starfield's separator.
const GAUGE_ROW: usize = 2;

/// The chip key of device `i` of a [`Fixed`]: its PCI address.
fn chip(i: usize) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i + 1))
}

/// A bus world built record by record, one millisecond apart from 1 s on
/// the bus clock.
#[derive(Clone)]
struct Bus {
    world: World,
    seq: u64,
    t: u64,
}

impl Bus {
    fn new() -> Bus {
        Bus {
            world: World::new(),
            seq: 0,
            t: SEC,
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
    fn sample(&mut self, e: &EntityKey, signal: &str, v: Value, prov: Provenance) -> &mut Self {
        self.push(Payload::Sample(Sample {
            entity: e.clone(),
            signal: signal.into(),
            value: v,
            prov,
        }))
    }
    /// Model `key` ready on `chips`, with a tokens-per-second reading when
    /// `tps` is given.
    fn ready(&mut self, key: &str, chips: &[usize], tps: Option<f64>) -> &mut Self {
        let m = EntityKey::model(key);
        if let Some(t) = tps {
            self.sample(
                &m,
                tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
                Value::F64(t),
                Provenance::Measured,
            );
        }
        self.event(Event::ModelReady {
            model: m,
            chips: chips.iter().map(|&i| chip(i)).collect(),
        })
    }
    fn loading(&mut self, key: &str, chips: &[usize], progress: Option<f32>) -> &mut Self {
        self.event(Event::ModelLoading {
            model: EntityKey::model(key),
            chips: chips.iter().map(|&i| chip(i)).collect(),
            progress,
        })
    }
    fn phase(&mut self, key: &str, phase: &str) -> &mut Self {
        self.sample(
            &EntityKey::model(key),
            tt_toplike_bus::signals::MODEL_PHASE,
            Value::Text(phase.into()),
            Provenance::Measured,
        )
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
            header_kind: HeaderKind::Live,
        }
    }
}

/// An arcade at [`W`] x [`H`] over two fixed chips.
fn arcade() -> (ArcadeVisualization, Fixed) {
    let backend = Fixed::new(2);
    let mut a = ArcadeVisualization::new(W, H);
    a.initialize_from_devices(backend.devices());
    a.initialize_topology(&backend);
    (a, backend)
}

/// One arcade update with `world` at its own clock.
fn step(a: &mut ArcadeVisualization, b: &Fixed, world: &dyn WorldRead) {
    a.set_bus(Some(world), b);
    a.update(b);
}

/// Each row as (character, style) cells.
fn cells(lines: &[Line<'static>]) -> Vec<Vec<(char, Style)>> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
                .collect()
        })
        .collect()
}

fn row_text(lines: &[Line<'static>], row: usize) -> String {
    lines[row]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect()
}

fn fg(c: [u8; 3]) -> Option<Color> {
    Some(crate::ui::colors::rgb(c[0], c[1], c[2]))
}

/// Cells of `row` in columns `cols` whose foreground is `c`.
fn count_fg(
    lines: &[Line<'static>],
    row: usize,
    cols: std::ops::Range<usize>,
    c: [u8; 3],
) -> usize {
    cells(lines)[row]
        .iter()
        .enumerate()
        .filter(|(i, (_, s))| cols.contains(i) && s.fg == fg(c))
        .count()
}

/// Run `f` with truecolor forced, so colours compare as RGB, and with the
/// default theme set under the theme lock, so a test that flips the
/// app-wide theme cannot change the colours mid-test.
fn tc<R>(f: impl FnOnce() -> R) -> R {
    crate::ui::colors::with_theme(crate::ui::colors::Theme::Default, || {
        crate::ui::colors::with_truecolor(true, f)
    })
}

#[test]
fn an_empty_world_draws_the_pinned_frames() {
    // An empty world, and one holding only a reading the arcade does not
    // draw, give exactly the frames the arcade drew before the bus.
    let mut quiet = Bus::new();
    quiet.sample(
        &chip(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    for world in [Bus::new(), quiet] {
        for (i, (w, h)) in SIZES.into_iter().enumerate() {
            let got = hashes_with(w, h, |a, b, n| {
                let v = world.view(5 * SEC + n as u64 * TICK);
                a.set_bus(Some(&v), b);
            });
            assert_eq!(got, GOLDEN[i].to_vec(), "{w}x{h}");
        }
    }
}

#[test]
fn a_request_since_the_last_update_draws_a_hit_and_history_does_not() {
    tc(|| {
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 3);
        let t0 = 5 * SEC;

        // Seen arriving: the frame differs from the quiet one, with teal
        // hit glyphs in it.
        let (mut a, b) = arcade();
        let (mut q, _) = arcade();
        step(&mut a, &b, &base.view(t0));
        step(&mut q, &b, &base.view(t0));
        step(&mut a, &b, &with.view(t0 + TICK));
        step(&mut q, &b, &base.view(t0 + TICK));
        let (fa, fq) = (a.render(&b), q.render(&b));
        assert_ne!(fa, fq, "a new request draws a hit");
        let hits = cells(&fa)
            .iter()
            .flatten()
            .filter(|(c, s)| *c == '✦' && s.fg == fg(SERVE_TEAL))
            .count();
        assert_eq!(hits, 3, "one glyph per request");

        // History: the event was already there at the first read.
        let (mut h, _) = arcade();
        let (mut q, _) = arcade();
        step(&mut h, &b, &with.view(t0));
        step(&mut q, &b, &base.view(t0));
        assert_eq!(h.render(&b), q.render(&b), "history draws nothing");

        // Away: the event landed while the arcade was not updating.
        let (mut w, _) = arcade();
        let (mut q, _) = arcade();
        for i in 0..5 {
            step(&mut w, &b, &base.view(t0 + i * TICK));
            step(&mut q, &b, &base.view(t0 + i * TICK));
        }
        step(&mut w, &b, &with.view(t0 + 10 * SEC));
        step(&mut q, &b, &base.view(t0 + 10 * SEC));
        assert_eq!(w.render(&b), q.render(&b), "an away event draws nothing");
    })
}

#[test]
fn a_hit_has_one_glyph_per_request_up_to_the_cap() {
    tc(|| {
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 50);
        let (mut a, b) = arcade();
        step(&mut a, &b, &base.view(5 * SEC));
        step(&mut a, &b, &with.view(5 * SEC + TICK));
        assert_eq!(a.bus_fx.hits.len(), 1);
        assert_eq!(a.bus_fx.hits[0].glyphs, MAX_HIT_GLYPHS);
        // The hit is gone a second later.
        step(&mut a, &b, &with.view(5 * SEC + TICK + SEC));
        assert!(a.bus_fx.hits.is_empty());
    })
}

/// The gauge-row columns of `a`'s frame, within `cols`, whose character is
/// the same as in the frame of an arcade given the same updates with no bus
/// but whose style differs: the gauge's cells (the label's cells change
/// character and are left out).
fn changed_cols(a: &ArcadeVisualization, b: &Fixed, cols: std::ops::Range<usize>) -> Vec<usize> {
    let (mut plain, _) = arcade();
    for _ in 0..a.frame {
        plain.update(b);
    }
    let p = cells(&plain.render(b))[GAUGE_ROW].clone();
    let c = cells(&a.render(b))[GAUGE_ROW].clone();
    cols.filter(|i| p[*i].0 == c[*i].0 && p[*i].1 != c[*i].1)
        .collect()
}

#[test]
fn loading_at_half_fills_half_the_slot_and_no_progress_sweeps() {
    tc(|| {
        let t0 = 5 * SEC;
        // The model label is right-aligned in the slot; columns 0..40 are
        // clear of it.
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.5));
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(t0));
        let frame = a.render(&b);
        assert_eq!(
            count_fg(&frame, GAUGE_ROW, 0..40, LOAD_PURPLE),
            30,
            "half of a 60-column slot"
        );
        assert!(row_text(&frame, GAUGE_ROW).contains("m loading 50%"));
        // Chip 1's slot has no model and no gauge.
        assert!(changed_cols(&a, &b, 60..120).is_empty());

        // No progress figure: a band narrower than the slot, which moves.
        let mut f = Bus::new();
        f.loading("m", &[0], None);
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(t0));
        let first = changed_cols(&a, &b, 0..60);
        assert!(!first.is_empty(), "the sweep shows");
        assert!(first.len() < 30, "the sweep is a band: {first:?}");
        step(&mut a, &b, &f.view(t0 + SEC / 2));
        let later = changed_cols(&a, &b, 0..60);
        assert!(!later.is_empty() && later != first, "the band moves");
    })
}

#[test]
fn ready_settles_and_a_reannouncement_changes_nothing() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.9));
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(t0));
        f.ready("m", &[0], None);
        step(&mut a, &b, &f.view(t0 + TICK));
        let teal = |a: &ArcadeVisualization| count_fg(&a.render(&b), GAUGE_ROW, 0..40, SERVE_TEAL);
        assert_eq!(teal(&a), 40, "the change to ready lights the slot");
        step(&mut a, &b, &f.view(t0 + TICK + SETTLE_NS));
        assert_eq!(teal(&a), 0, "and settles");

        // A model first seen ready gets no settle.
        let (mut fresh, _) = arcade();
        step(&mut fresh, &b, &f.view(t0));
        assert_eq!(teal(&fresh), 0);

        // A re-announcement with the same phase changes nothing.
        let mut again = f.clone();
        again.ready("m", &[0], None);
        let (mut x, _) = arcade();
        let (mut y, _) = arcade();
        for (i, w) in [&f, &again].into_iter().enumerate() {
            step(&mut x, &b, &w.view(t0 + i as u64 * TICK));
            step(&mut y, &b, &f.view(t0 + i as u64 * TICK));
        }
        assert_eq!(x.render(&b), y.render(&b));
    })
}

#[test]
fn down_greys_the_slot_and_fades() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut g = Bus::new();
        g.ready("m", &[0], None);
        let (mut d, b) = arcade();
        step(&mut d, &b, &g.view(t0));
        g.phase("m", "down");
        step(&mut d, &b, &g.view(t0 + TICK));
        let grey = |a: &ArcadeVisualization| count_fg(&a.render(&b), GAUGE_ROW, 0..40, DOWN_GREY);
        assert_eq!(grey(&d), 40, "the change to down greys the slot");
        step(&mut d, &b, &g.view(t0 + TICK + SETTLE_NS));
        assert_eq!(grey(&d), 0, "and fades");
    })
}

#[test]
fn an_estimated_model_label_starts_with_a_tilde() {
    tc(|| {
        let m = EntityKey::model("mock");
        let mut f = Bus::new();
        f.sample(
            &m,
            tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
            Value::F64(20.0),
            Provenance::Estimated,
        )
        .event(Event::ModelReady {
            model: m,
            chips: vec![chip(0)],
        });
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(5 * SEC));
        let row = row_text(&a.render(&b), GAUGE_ROW);
        assert!(row.contains("~mock ready 20t/s"), "{row}");

        // Measured: no tilde.
        let mut f = Bus::new();
        f.ready("real", &[0], Some(20.0));
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(5 * SEC));
        let row = row_text(&a.render(&b), GAUGE_ROW);
        assert!(
            row.contains("real ready 20t/s") && !row.contains('~'),
            "{row}"
        );
    })
}

#[test]
fn a_world_naming_a_chip_this_tui_lacks_draws_nothing_for_it() {
    tc(|| {
        // Chip 5 is not one of the two fixed chips.
        let mut f = Bus::new();
        f.ready("far", &[5], Some(120.0));
        let t0 = 5 * SEC;
        let (mut a, b) = arcade();
        let (mut q, _) = arcade();
        step(&mut a, &b, &f.view(t0));
        step(&mut q, &b, &Bus::new().view(t0));
        f.requests("far", 4)
            .event(Event::Spark {
                chip: Some(chip(5)),
                kind: "vLLM.log".into(),
            })
            .event(Event::KernelCompiled {
                chip: Some(chip(5)),
                source: "ttnn".into(),
            })
            .event(Event::Reset {
                chips: vec![chip(5)],
            });
        step(&mut a, &b, &f.view(t0 + TICK));
        step(&mut q, &b, &Bus::new().view(t0 + TICK));
        assert_eq!(a.render(&b), q.render(&b));
        assert!(a.bus_fx.is_empty(), "{:?}", a.bus_fx);
    })
}

/// Contract rule 4 through the read path an attached TUI uses: a real
/// `ReadView` limited to this TUI's two chips (`ReadView::only_chips`).
/// The view cuts the model's chip list and the chip-keyed events, and keeps
/// `RequestsCompleted`, which names no chip. A model, requests, a spark, a
/// compile and a reset on the host's own chip draw nothing; the same model
/// once it also runs on chip 0 draws its label and its hits.
#[test]
fn attached_through_only_chips_a_hosts_chip_draws_nothing() {
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    tc(|| {
        let (mut a, b) = arcade();
        let (mut q, _) = arcade();
        let host = BusHost::new(tt_toplike_bus::BusConfig::default(), &b).unwrap();
        let bus = host.bus().clone();
        let view = BusView::Local(Box::new(host));
        let cache = ReadCache::new();
        let local: Arc<BTreeSet<EntityKey>> = Arc::new([chip(0), chip(1)].into_iter().collect());
        let read = || view.read(&cache).only_chips(local.clone());
        let p = |e: Event| {
            bus.publish("test", Payload::Event(e));
        };
        let theirs = EntityKey::model("theirs");
        let foreign = EntityKey::chip("0000:ee:00.0");

        // First sight: an empty world.
        let w = read();
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));

        bus.publish(
            "test",
            Payload::Sample(Sample {
                entity: theirs.clone(),
                signal: tt_toplike_bus::signals::MODEL_TOKENS_PER_S.into(),
                value: Value::F64(120.0),
                prov: Provenance::Measured,
            }),
        );
        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone()],
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 4,
        });
        p(Event::Spark {
            chip: Some(foreign.clone()),
            kind: "vLLM.log".into(),
        });
        p(Event::KernelCompiled {
            chip: Some(foreign.clone()),
            source: "ttnn".into(),
        });
        p(Event::Reset {
            chips: vec![foreign.clone()],
        });
        let w = read();
        // What the view hands the arcade: the model with its chip cut away,
        // and of the events only the requests.
        let models = w.models();
        assert_eq!(models.len(), 1);
        assert!(models[0].chips.is_empty(), "{:?}", models[0].chips);
        let kinds: Vec<&str> = w
            .events_since(0)
            .iter()
            .map(|(_, e)| match e {
                Event::RequestsCompleted { .. } => "requests",
                Event::ModelReady { .. } => "ready",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["requests"]);
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));
        // No hit, no tokens per second, nothing on either chip.
        assert!(a.bus_fx.is_empty(), "{:?}", a.bus_fx);
        assert_eq!(a.render(&b), q.render(&b));

        // The same model also on chip 0: the view keeps chip 0, so the
        // label and the hits show there.
        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone(), chip(0)],
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 2,
        });
        let w = read();
        assert_eq!(w.models()[0].chips, vec![chip(0)]);
        step(&mut a, &b, &w);
        // `update` hands `new_requests` to the duel, so the hit is checked.
        assert_eq!(a.bus_fx.hits.len(), 1);
        assert_eq!(a.bus_fx.hits[0].glyphs, 2);
        assert_eq!(a.bus_fx.chips.len(), 1);
        assert_eq!(a.bus_fx.chips[0].slot, 0);
        assert!(row_text(&a.render(&b), GAUGE_ROW).contains("theirs ready"));
    })
}

/// A world that hands out models whose chip lists are empty, as an attached
/// world does for another machine's models once `ReadView::only_chips` has
/// cut them. Die 3D's `Foreign` double, for the arcade.
struct Foreign(LocalView);

impl WorldRead for Foreign {
    fn seq(&self) -> u64 {
        self.0.seq()
    }
    fn models(&self) -> Vec<ModelView> {
        let mut m = self.0.models();
        m.iter_mut().for_each(|m| m.chips.clear());
        m
    }
    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView> {
        self.0.model_on_chip(chip).map(|mut m| {
            m.chips.clear();
            m
        })
    }
    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading> {
        self.0.reading(entity, signal)
    }
    fn events_since(&self, seq: u64) -> Vec<(u64, Event)> {
        self.0.events_since(seq)
    }
    fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.0.reset_seq(chip)
    }
    fn header_kind(&self) -> HeaderKind {
        self.0.header_kind()
    }
    fn now_mono_ns(&self) -> u64 {
        self.0.now_mono_ns()
    }
}

/// `model_on_chip` can hand back a model whose chip list has been cut to
/// nothing; the arcade draws no label and no gauge for it.
#[test]
fn a_model_whose_chip_list_was_cut_away_draws_no_label() {
    tc(|| {
        let mut f = Bus::new();
        f.loading("far", &[0], Some(0.5));
        let (mut a, b) = arcade();
        let (mut q, _) = arcade();
        step(&mut a, &b, &Foreign(f.view(5 * SEC)));
        step(&mut q, &b, &Bus::new().view(5 * SEC));
        assert!(a.bus_fx.chips.is_empty(), "{:?}", a.bus_fx.chips);
        assert_eq!(a.render(&b), q.render(&b));
    })
}

/// A world that counts every call made through it.
struct Counting {
    inner: LocalView,
    calls: AtomicUsize,
}

impl Counting {
    fn hit(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

impl WorldRead for Counting {
    fn seq(&self) -> u64 {
        self.hit();
        self.inner.seq()
    }
    fn models(&self) -> Vec<ModelView> {
        self.hit();
        self.inner.models()
    }
    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView> {
        self.hit();
        self.inner.model_on_chip(chip)
    }
    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading> {
        self.hit();
        self.inner.reading(entity, signal)
    }
    fn age_ms(&self, r: &Reading, now: u64) -> u64 {
        self.hit();
        self.inner.age_ms(r, now)
    }
    fn events_since(&self, seq: u64) -> Vec<(u64, Event)> {
        self.hit();
        self.inner.events_since(seq)
    }
    fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.hit();
        self.inner.reset_seq(chip)
    }
    fn header_kind(&self) -> HeaderKind {
        self.hit();
        self.inner.header_kind()
    }
    fn now_mono_ns(&self) -> u64 {
        self.hit();
        self.inner.now_mono_ns()
    }
}

#[test]
fn the_frame_path_never_reads_the_world() {
    tc(|| {
        let mut f = Bus::new();
        f.loading("l", &[0], None).ready("m", &[1], Some(80.0));
        let (mut a, b) = arcade();
        let world = |f: &Bus, now| Counting {
            inner: f.view(now),
            calls: AtomicUsize::new(0),
        };
        step(&mut a, &b, &world(&f, 5 * SEC));
        f.requests("m", 4).event(Event::Spark {
            chip: Some(chip(1)),
            kind: "vLLM.log".into(),
        });
        let w = world(&f, 5 * SEC + TICK);
        a.set_bus(Some(&w), &b);
        assert!(w.calls.load(Ordering::Relaxed) > 0, "set_bus reads");
        w.calls.store(0, Ordering::Relaxed);
        a.update(&b);
        for _ in 0..5 {
            let frame = a.render(&b);
            assert!(!a.bus_fx.is_empty() && !frame.is_empty());
        }
        assert_eq!(
            w.calls.load(Ordering::Relaxed),
            0,
            "update and render read nothing"
        );
    })
}

#[test]
fn tokens_per_second_quicken_the_heartbeat_smoothly_and_a_stale_rate_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.ready("m", &[0], Some(100.0));
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(t0));
        assert_eq!(a.bus_fx.tokens_per_s, Some(100.0));
        assert!(a.bus_fx.heartbeat_frames() < HEARTBEAT_FRAMES);
        // A jump is smoothed: one tick later the rate is part of the way.
        let mut jump = f.clone();
        jump.ready("m", &[0], Some(200.0));
        step(&mut a, &b, &jump.view(t0 + TICK));
        let t = a.bus_fx.tokens_per_s.unwrap();
        assert!(t > 100.0 && t < 110.0, "{t}");

        // The heartbeat shows in the frames: the hero's brightness differs
        // from an arcade whose model reports no rate.
        let mut none = Bus::new();
        none.ready("m", &[0], None);
        let (mut x, _) = arcade();
        let (mut y, _) = arcade();
        let mut differ = false;
        for i in 0..20 {
            step(&mut x, &b, &f.view(t0 + i * TICK));
            step(&mut y, &b, &none.view(t0 + i * TICK));
            differ |= x.render(&b) != y.render(&b);
        }
        assert!(differ, "the rate shows in the hero");

        // A reading older than the limit counts as none: the frames are
        // those of a model with no rate, but for the label, which still
        // prints the model's last rate.
        let stale = SEC + (TPS_STALE_MS + 1_000) * 1_000_000;
        let (mut s, _) = arcade();
        let (mut n, _) = arcade();
        for i in 0..20 {
            step(&mut s, &b, &f.view(stale + i * TICK));
            step(&mut n, &b, &none.view(stale + i * TICK));
            let (mut fs, mut fn_) = (s.render(&b), n.render(&b));
            fs.remove(GAUGE_ROW);
            fn_.remove(GAUGE_ROW);
            assert_eq!(fs, fn_);
        }
        assert_eq!(s.bus_fx.tokens_per_s, None);
    })
}

#[test]
fn sparks_and_compiles_rise_in_their_chips_slot() {
    tc(|| {
        let t0 = 5 * SEC;
        let base = Bus::new();
        let mut f = base.clone();
        f.event(Event::Spark {
            chip: Some(chip(1)),
            kind: "vLLM.log".into(),
        })
        .event(Event::KernelCompiled {
            chip: Some(chip(0)),
            source: "ttnn".into(),
        })
        // No chip: nothing, as in Die 3D.
        .event(Event::Spark {
            chip: None,
            kind: "driver".into(),
        });
        let (mut a, b) = arcade();
        step(&mut a, &b, &base.view(t0));
        step(&mut a, &b, &f.view(t0 + TICK));
        let frame = a.render(&b);
        let found = |cols: std::ops::Range<usize>, c: [u8; 3]| {
            cells(&frame)
                .iter()
                .skip(3)
                .flat_map(|row| row.iter().enumerate())
                .filter(|(i, (ch, s))| cols.contains(i) && *ch == '*' && s.fg == fg(c))
                .count()
        };
        assert_eq!(found(60..120, SERVE_TEAL), 1, "the vLLM spark on chip 1");
        assert_eq!(found(0..60, LOAD_PURPLE), 1, "the compile on chip 0");
        let total: usize = a.bus_fx.chips.iter().map(|c| c.particles.len()).sum();
        assert_eq!(total, 2, "the spark with no chip draws nothing");
    })
}

#[test]
fn a_new_reset_flashes_its_chip_and_an_old_one_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let reversed = |a: &ArcadeVisualization, b: &Fixed, cols: std::ops::Range<usize>| {
            cells(&a.render(b))[GAUGE_ROW]
                .iter()
                .enumerate()
                .filter(|(i, (_, s))| {
                    cols.contains(i) && s.add_modifier.contains(Modifier::REVERSED)
                })
                .count()
        };
        let mut f = Bus::new();
        f.event(Event::Reset {
            chips: vec![chip(0)],
        });
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(t0));
        assert_eq!(reversed(&a, &b, 0..120), 0, "a reset seen at first sight");
        f.event(Event::Reset {
            chips: vec![chip(0)],
        });
        step(&mut a, &b, &f.view(t0 + TICK));
        assert_eq!(reversed(&a, &b, 0..60), 60, "a new reset flashes chip 0");
        assert_eq!(reversed(&a, &b, 60..120), 0, "and not chip 1");
        step(&mut a, &b, &f.view(t0 + TICK + FLASH_NS));
        assert_eq!(reversed(&a, &b, 0..120), 0, "the flash is over");
    })
}

#[test]
fn the_legend_and_explain_text_name_what_the_bus_draws() {
    use crate::animation::arcade_bus::{EXPLAIN_TEXT, LEGEND_TEXT};
    let flat = |t: &[&str]| t.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    for (what, text) in [
        ("legend", flat(LEGEND_TEXT)),
        ("explain", flat(EXPLAIN_TEXT)),
    ] {
        for phrase in [
            "STARFIELD",
            "progress",
            "decoration",
            "ready",
            "down",
            "~",
            "estimate",
            "✦",
            "request",
            "compile",
            "reset",
            "tokens/s",
        ] {
            assert!(text.contains(phrase), "{what} lacks {phrase:?}: {text}");
        }
    }
}

#[test]
fn attached_the_duel_serves_the_hosts_stats_and_lunges_on_its_requests() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut base = Bus::new();
        base.loading("idle", &[1], None)
            .ready("m", &[0], Some(42.0));
        // The rows the lanes take from an attached host's world.
        let rows = crate::bus::compat::world_to_service_states(&base.view(t0));
        let stats = crate::animation::arcade_bus::attached_serving(&rows).expect("a ready row");
        assert_eq!(stats.generation_tps, 42.0);
        // Rows built from a world carry no completion delta.
        assert_eq!(stats.completed_delta, 0);

        // A request the bus saw complete makes the snake lunge.
        let mut with = base.clone();
        with.requests("m", 1);
        let (mut a, b) = arcade();
        let (mut q, _) = arcade();
        for x in [&mut a, &mut q] {
            x.set_serving(Some(stats));
            step(x, &b, &base.view(t0));
        }
        step(&mut a, &b, &with.view(t0 + TICK));
        step(&mut q, &b, &base.view(t0 + TICK));
        assert_ne!(
            row_text(&a.render(&b), 1),
            row_text(&q.render(&b), 1),
            "the duel strip shows the lunge"
        );
    })
}

/// Prints an arcade frame with something from the bus on each chip: a model
/// loading at 40% on chip 0, one ready on chip 1 with a hit just begun, a
/// spark and a compile. For a person to look at; asserts nothing. Run with
/// `cargo test --locked --lib --features tui arcade_bus_smoke_frame -- --ignored --nocapture`.
#[test]
#[ignore = "prints a frame for a person to look at"]
fn arcade_bus_smoke_frame() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.loading("load", &[0], Some(0.4))
            .ready("serve", &[1], Some(42.0));
        let (mut a, b) = arcade();
        step(&mut a, &b, &f.view(t0));
        f.requests("serve", 5)
            .event(Event::Spark {
                chip: Some(chip(1)),
                kind: "vLLM.log".into(),
            })
            .event(Event::KernelCompiled {
                chip: Some(chip(0)),
                source: "ttnn".into(),
            });
        step(&mut a, &b, &f.view(t0 + TICK));
        for line in a.render(&b) {
            let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            println!("{s}");
        }
    })
}
