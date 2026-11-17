// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Starfield frames with and without the signal bus.
//!
//! The golden hashes pin the frames the Starfield drew before it read the
//! bus. They were taken by running `golden_hashes` against the commit that
//! added this file, which holds only these tests, so a frame drawn from an
//! empty world must hash to exactly these values.
//!
//! The fixed backend, the frame hash and the colour lock here are shared
//! with the MemoryFlow tests.

use crate::animation::HardwareStarfield;
use crate::backend::TelemetryBackend;
use crate::models::{Device, SmbusTelemetry, Telemetry};
use ratatui::style::Color;
use ratatui::text::Line;

/// A backend whose readings never change: `n` p300 chips at fixed power,
/// current, temperature and clock. The mock backend adds random noise, so
/// its frames cannot be pinned.
pub(crate) struct Fixed {
    devices: Vec<Device>,
    telem: Vec<Telemetry>,
    smbus: SmbusTelemetry,
}

impl Fixed {
    /// `n` p300 chips with PCI addresses `0000:01:00.0`, `0000:02:00.0`, ...
    pub(crate) fn new(n: usize) -> Fixed {
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
pub(crate) fn frame_hash(lines: &[Line<'static>]) -> u64 {
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

/// Run `f` with truecolor forced, so colours compare as RGB, and with the
/// default theme set under the theme lock, so a test that flips the
/// app-wide theme cannot change the colours mid-test.
pub(crate) fn tc<R>(f: impl FnOnce() -> R) -> R {
    crate::ui::colors::with_theme(crate::ui::colors::Theme::Default, || {
        crate::ui::colors::with_truecolor(true, f)
    })
}

/// The sizes the golden hashes pin: a small field and a large one.
pub(crate) const SIZES: [(usize, usize); 2] = [(60, 18), (160, 50)];
/// The updates after which a frame is hashed.
pub(crate) const HASHED_AT: [u32; 3] = [1, 10, 30];

/// Hashes of the frames of a Starfield at `w` x `h` over two [`Fixed`]
/// chips, after each update in [`HASHED_AT`]. `before_update` runs before
/// every update (the bus tests hand the field a world there).
pub(crate) fn hashes_with(
    w: usize,
    h: usize,
    mut before_update: impl FnMut(&mut HardwareStarfield, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(2);
        let mut sf = HardwareStarfield::new(w, h);
        sf.initialize_from_devices(backend.devices());
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut sf, &backend, i);
            sf.update_from_telemetry(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&sf.render()));
            }
        }
        out
    })
}

/// The frames as the Starfield drew them before the bus port, at each of
/// [`SIZES`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 3]; 2] = [
    [
        16316922878991444544,
        10762525961434439164,
        5386681435767523541,
    ],
    [
        18074279486178955259,
        14384572491133398039,
        9376491155689480051,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui starfield_bus::tests::golden_hashes -- --ignored --nocapture`.
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
/// default theme under the theme lock.
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

use crate::animation::arcade_bus::TPS_STALE_MS;
use crate::animation::die3d::overlay::{ALARM_RED, FLASH_NS};
use crate::animation::starfield_bus::{COMET_NS, EXPLAIN_TEXT, LEGEND_TEXT};
use tt_toplike_bus::{
    EntityKey, Event, HeaderKind, LocalView, Payload, Provenance, Record, Sample, Value, World,
    WorldRead,
};

pub(crate) const SEC: u64 = 1_000_000_000;
/// The gap between two updates at 30 a second.
pub(crate) const TICK: u64 = 33_000_000;
/// The field size the bus tests draw at: two slots of 60 columns.
const W: usize = 120;
const H: usize = 30;

/// The chip key of device `i` of a [`Fixed`]: its PCI address.
pub(crate) fn chip(i: usize) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i + 1))
}

/// A bus world built record by record, one millisecond apart from 1 s on
/// the bus clock.
#[derive(Clone)]
pub(crate) struct Bus {
    world: World,
    seq: u64,
    t: u64,
}

impl Bus {
    pub(crate) fn new() -> Bus {
        Bus {
            world: World::new(),
            seq: 0,
            t: SEC,
        }
    }
    pub(crate) fn push(&mut self, payload: Payload) -> &mut Self {
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
    pub(crate) fn event(&mut self, e: Event) -> &mut Self {
        self.push(Payload::Event(e))
    }
    pub(crate) fn sample(
        &mut self,
        e: &EntityKey,
        signal: &str,
        v: Value,
        prov: Provenance,
    ) -> &mut Self {
        self.push(Payload::Sample(Sample {
            entity: e.clone(),
            signal: signal.into(),
            value: v,
            prov,
        }))
    }
    /// Model `key` ready on `chips`, with a measured tokens-per-second
    /// reading when `tps` is given.
    pub(crate) fn ready(&mut self, key: &str, chips: &[usize], tps: Option<f64>) -> &mut Self {
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
    pub(crate) fn loading(
        &mut self,
        key: &str,
        chips: &[usize],
        progress: Option<f32>,
    ) -> &mut Self {
        self.event(Event::ModelLoading {
            model: EntityKey::model(key),
            chips: chips.iter().map(|&i| chip(i)).collect(),
            progress,
        })
    }
    pub(crate) fn requests(&mut self, key: &str, n: u32) -> &mut Self {
        self.event(Event::RequestsCompleted {
            model: EntityKey::model(key),
            n,
        })
    }
    /// Set the corrected-read ECC count of channel `ch` of chip `i`.
    pub(crate) fn ecc(&mut self, i: usize, ch: u8, v: u64) -> &mut Self {
        self.sample(
            &EntityKey::dram_channel(&chip(i), ch),
            crate::bus::signals::DRAM_ECC_CORR_RD,
            Value::U64(v),
            Provenance::Measured,
        )
    }
    pub(crate) fn view(&self, now: u64) -> LocalView {
        LocalView {
            seq: self.world.seq(),
            world: self.world.clone(),
            now_mono_ns: now,
            header_kind: HeaderKind::Live,
        }
    }
}

/// The fixed-clock wrapper the attached tests read through. It lives in
/// `bus_test_support` so every mode's attached test can share it.
pub(crate) use crate::animation::bus_test_support::Clocked;

/// A field at [`W`] x [`H`] over two fixed chips.
fn field() -> (HardwareStarfield, Fixed) {
    let backend = Fixed::new(2);
    let mut sf = HardwareStarfield::new(W, H);
    sf.initialize_from_devices(backend.devices());
    (sf, backend)
}

/// One update with `world`.
fn step(sf: &mut HardwareStarfield, b: &Fixed, world: &dyn WorldRead) {
    sf.set_bus(Some(world), b.devices());
    sf.update_from_telemetry(b);
}

/// Each row as (character, foreground) cells.
pub(crate) fn cells(lines: &[Line<'static>]) -> Vec<Vec<(char, Option<Color>)>> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, s.style.fg)))
                .collect()
        })
        .collect()
}

pub(crate) fn row_text(lines: &[Line<'static>], row: usize) -> String {
    lines[row]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect()
}

/// The cells, within columns `cols`, where `a` and `b` differ.
fn diff(
    a: &[Line<'static>],
    b: &[Line<'static>],
    cols: std::ops::Range<usize>,
) -> Vec<(usize, usize)> {
    let (ca, cb) = (cells(a), cells(b));
    let mut out = Vec::new();
    for (y, (ra, rb)) in ca.iter().zip(&cb).enumerate() {
        for x in cols.clone() {
            if ra.get(x) != rb.get(x) {
                out.push((x, y));
            }
        }
    }
    out
}

/// A field fed `worlds` one update apart from `t0`, and a twin fed empty
/// worlds at the same times. Their frames after the last update.
fn pair(worlds: &[(&Bus, u64)]) -> (Vec<Line<'static>>, Vec<Line<'static>>, HardwareStarfield) {
    let (mut a, b) = field();
    let (mut q, _) = field();
    for (w, t) in worlds {
        step(&mut a, &b, &w.view(*t));
        step(&mut q, &b, &Bus::new().view(*t));
    }
    (a.render(), q.render(), a)
}

#[test]
fn an_empty_world_draws_the_pinned_frames() {
    // An empty world, and one holding only a reading the field does not
    // draw, give exactly the frames the field drew before the bus.
    let mut quiet = Bus::new();
    quiet.sample(
        &chip(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    for world in [Bus::new(), quiet] {
        for (i, (w, h)) in SIZES.into_iter().enumerate() {
            let got = hashes_with(w, h, |sf, b, n| {
                let v = world.view(5 * SEC + n as u64 * TICK);
                sf.set_bus(Some(&v), b.devices());
            });
            assert_eq!(got, GOLDEN[i].to_vec(), "{w}x{h}");
        }
    }
}

#[test]
fn loading_lights_a_cluster_that_grows_with_progress() {
    tc(|| {
        let t0 = 5 * SEC;
        let lit = |p: f32| {
            let mut f = Bus::new();
            f.loading("m", &[0], Some(p));
            let (a, q, _) = pair(&[(&f, t0)]);
            // Row 0 holds the label; the cluster is below it.
            let d: Vec<_> = diff(&a, &q, 0..60)
                .into_iter()
                .filter(|c| c.1 > 0)
                .collect();
            let purple_balls = d.iter().filter(|(x, y)| cells(&a)[*y][*x].0 == '●').count();
            assert_eq!(purple_balls, d.len(), "every changed star is a ●");
            assert!(diff(&a, &q, 60..120).is_empty(), "chip 1 has nothing");
            assert!(row_text(&a, 0).contains(&format!("m loading {}%", (p * 100.0) as u32)));
            d.len()
        };
        let (quarter, half, full) = (lit(0.25), lit(0.5), lit(1.0));
        assert!(
            0 < quarter && quarter < half && half < full,
            "{quarter} {half} {full}"
        );
        // At full progress every star of the chip's 14 x 16 grid is lit
        // (less any a planet covers).
        assert!(full > 200, "{full}");
    })
}

#[test]
fn loading_with_no_progress_sweeps_a_ring() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.loading("m", &[0], None);
        let (mut a, b) = field();
        let (mut q, _) = field();
        step(&mut a, &b, &f.view(t0));
        step(&mut q, &b, &Bus::new().view(t0));
        let first: Vec<_> = diff(&a.render(), &q.render(), 0..60)
            .into_iter()
            .filter(|c| c.1 > 0)
            .collect();
        step(&mut a, &b, &f.view(t0 + SEC));
        step(&mut q, &b, &Bus::new().view(t0 + SEC));
        let later: Vec<_> = diff(&a.render(), &q.render(), 0..60)
            .into_iter()
            .filter(|c| c.1 > 0)
            .collect();
        assert!(!first.is_empty() && !later.is_empty());
        assert!(
            first.len() < 150,
            "the ring covers part of the grid: {}",
            first.len()
        );
        assert_ne!(first, later, "the ring moves");
    })
}

#[test]
fn ready_settles_teal_and_then_draws_only_the_label() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.5));
        let mut g = f.clone();
        g.ready("m", &[0], None);
        let (a, q, _) = pair(&[(&f, t0), (&g, t0 + TICK)]);
        let d: Vec<_> = diff(&a, &q, 0..60)
            .into_iter()
            .filter(|c| c.1 > 0)
            .collect();
        assert!(d.len() > 200, "the whole chip settles: {}", d.len());
        // Past the settle only the label row differs.
        let (a, q, _) = pair(&[(&f, t0), (&g, t0 + TICK), (&g, t0 + TICK + 3 * SEC)]);
        let d = diff(&a, &q, 0..120);
        assert!(d.iter().all(|c| c.1 == 0), "{d:?}");
        assert!(row_text(&a, 0).contains("m ready"));
    })
}

#[test]
fn a_request_since_the_last_update_sends_a_comet_and_history_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut base = Bus::new();
        base.ready("m", &[1], None);
        let mut with = base.clone();
        with.requests("m", 3);

        // Seen arriving: a teal comet head and a tail of three cells.
        let (a, q, sf) = pair(&[
            (&base, t0),
            (&with, t0 + TICK),
            (&with, t0 + TICK + SEC / 2),
        ]);
        assert_eq!(sf.bus_fx().comets.len(), 1);
        assert_eq!(sf.bus_fx().comets[0].tail, 3);
        assert_eq!(sf.bus_fx().comets[0].slot, 1, "it leaves from chip 1");
        let d: Vec<_> = diff(&a, &q, 0..W).into_iter().filter(|c| c.1 > 0).collect();
        let heads = d.iter().filter(|(x, y)| cells(&a)[*y][*x].0 == '✦').count();
        assert_eq!(heads, 1, "{d:?}");
        assert_eq!(d.len(), 4, "a head and three tail cells: {d:?}");

        // History: the event was already there at the first read.
        let (a, q, sf) = pair(&[(&with, t0), (&with, t0 + TICK)]);
        assert!(sf.bus_fx().comets.is_empty());
        assert!(diff(&a, &q, 0..W).iter().all(|c| c.1 == 0));

        // Away: the event landed while the field was not updating.
        let mut ws: Vec<(&Bus, u64)> = (0..5).map(|i| (&base, t0 + i * TICK)).collect();
        ws.push((&with, t0 + 10 * SEC));
        let (_, _, sf) = pair(&ws);
        assert!(sf.bus_fx().comets.is_empty(), "an away event draws nothing");

        // The comet is gone once it has crossed.
        let (_, _, sf) = pair(&[
            (&base, t0),
            (&with, t0 + TICK),
            (&with, t0 + TICK + COMET_NS),
        ]);
        assert!(sf.bus_fx().comets.is_empty());
    })
}

#[test]
fn a_comets_tail_is_one_cell_per_request_up_to_eight() {
    tc(|| {
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 50);
        let (_, _, sf) = pair(&[(&base, 5 * SEC), (&with, 5 * SEC + TICK)]);
        assert_eq!(sf.bus_fx().comets[0].tail, 8);
        // A count of 0 completed nothing.
        let mut zero = base.clone();
        zero.requests("m", 0);
        let (_, _, sf) = pair(&[(&base, 5 * SEC), (&zero, 5 * SEC + TICK)]);
        assert!(sf.bus_fx().comets.is_empty());
    })
}

#[test]
fn tokens_per_second_speed_the_twinkle_only_while_fresh() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut f = Bus::new();
        f.ready("m", &[0], Some(200.0));
        // Ten updates, a tick apart: the ready model's stars twinkle faster,
        // so chip 0's frame drifts from the quiet one below the label row.
        let ws: Vec<(&Bus, u64)> = (0..10).map(|i| (&f, t0 + i * TICK)).collect();
        let (a, q, sf) = pair(&ws);
        assert!(sf.bus_fx().chips[0].tokens_per_s.is_some());
        let d: Vec<_> = diff(&a, &q, 0..60)
            .into_iter()
            .filter(|c| c.1 > 0)
            .collect();
        assert!(!d.is_empty(), "the twinkle changes");
        assert!(diff(&a, &q, 60..120).is_empty(), "chip 1 has no model");

        // The same reading, older than the stale limit: no rate, no change.
        let late = t0 + (TPS_STALE_MS + 1_000) * 1_000_000;
        let ws: Vec<(&Bus, u64)> = (0..10).map(|i| (&f, late + i * TICK)).collect();
        let (a, q, sf) = pair(&ws);
        assert!(sf.bus_fx().chips[0].tokens_per_s.is_none());
        let d: Vec<_> = diff(&a, &q, 0..60)
            .into_iter()
            .filter(|c| c.1 > 0)
            .collect();
        assert!(d.is_empty(), "{d:?}");
    })
}

#[test]
fn an_ecc_rise_flares_a_ddr_planet_and_the_first_sight_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut a1 = Bus::new();
        a1.ecc(0, 3, 5);
        let mut a2 = a1.clone();
        a2.ecc(0, 3, 6);
        let (a, q, sf) = pair(&[(&a1, t0), (&a2, t0 + TICK)]);
        assert_eq!(sf.bus_fx().chips.len(), 1);
        assert_eq!(sf.bus_fx().chips[0].ecc.len(), 1);
        assert_eq!(sf.bus_fx().chips[0].ecc[0].channel, 3);
        let red = crate::ui::colors::rgb(ALARM_RED[0], ALARM_RED[1], ALARM_RED[2]);
        let flares = cells(&a)
            .iter()
            .flatten()
            .filter(|(c, fg)| *c == '✸' && *fg == Some(red))
            .count();
        assert_eq!(flares, 1);
        assert!(!diff(&a, &q, 0..60).is_empty());

        // First sight of a count, however high, flares nothing.
        let (_, _, sf) = pair(&[(&a2, t0), (&a2, t0 + TICK)]);
        assert!(sf.bus_fx().is_empty(), "{:?}", sf.bus_fx());
        // Nor does a count that first appears after the first update.
        let (_, _, sf) = pair(&[(&Bus::new(), t0), (&a2, t0 + TICK)]);
        assert!(sf.bus_fx().is_empty(), "{:?}", sf.bus_fx());
        // The flare ends.
        let (_, _, sf) = pair(&[(&a1, t0), (&a2, t0 + TICK), (&a2, t0 + TICK + FLASH_NS)]);
        assert!(sf.bus_fx().is_empty());

        // An `EccTick` event flares too.
        let mut e = Bus::new();
        e.ecc(1, 0, 1);
        let mut e2 = e.clone();
        e2.event(Event::EccTick {
            chip: chip(1),
            channel: 2,
        });
        let (_, _, sf) = pair(&[(&e, t0), (&e2, t0 + TICK)]);
        assert_eq!(sf.bus_fx().chips[0].slot, 1);
        assert_eq!(sf.bus_fx().chips[0].ecc[0].channel, 2);
    })
}

#[test]
fn a_reset_flashes_the_chips_stars_and_the_first_sight_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut r1 = Bus::new();
        r1.event(Event::Reset {
            chips: vec![chip(1)],
        });
        let mut r2 = r1.clone();
        r2.event(Event::Reset {
            chips: vec![chip(1)],
        });
        let (a, q, sf) = pair(&[(&r1, t0), (&r2, t0 + TICK)]);
        assert!(sf.bus_fx().chips[0].flash_ns.is_some());
        assert!(diff(&a, &q, 0..60).is_empty(), "chip 0 is not reset");
        assert!(diff(&a, &q, 60..120).len() > 200, "chip 1's stars flash");
        let (_, _, sf) = pair(&[(&r2, t0), (&r2, t0 + TICK)]);
        assert!(sf.bus_fx().is_empty(), "a reset already there is history");
    })
}

#[test]
fn a_spark_twinkles_one_star_of_its_chip() {
    tc(|| {
        let t0 = 5 * SEC;
        let base = Bus::new();
        let mut s = base.clone();
        s.event(Event::Spark {
            chip: Some(chip(0)),
            kind: "vLLM.log".into(),
        })
        .event(Event::Spark {
            chip: None,
            kind: "vLLM.log".into(),
        })
        .event(Event::KernelCompiled {
            chip: Some(chip(1)),
            source: "ttnn".into(),
        });
        let (a, q, sf) = pair(&[(&base, t0), (&s, t0 + TICK)]);
        assert_eq!(sf.bus_fx().chips.len(), 2);
        let d0 = diff(&a, &q, 0..60);
        let d1 = diff(&a, &q, 60..120);
        assert_eq!(d0.len(), 1, "{d0:?}");
        assert_eq!(d1.len(), 1, "{d1:?}");
        assert_eq!(cells(&a)[d0[0].1][d0[0].0].0, '✦');
    })
}

#[test]
fn an_estimated_model_has_a_tilde_and_a_measured_one_does_not() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut m = Bus::new();
        m.ready("m", &[0], Some(12.0));
        let mut e = Bus::new();
        e.sample(
            &EntityKey::model("m"),
            tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
            Value::F64(12.0),
            Provenance::Estimated,
        )
        .event(Event::ModelReady {
            model: EntityKey::model("m"),
            chips: vec![chip(0)],
        });
        let (a, _, _) = pair(&[(&m, t0)]);
        let (b, _, _) = pair(&[(&e, t0)]);
        assert!(
            row_text(&a, 0).trim_start().starts_with("m ready"),
            "{}",
            row_text(&a, 0)
        );
        assert!(
            row_text(&b, 0).trim_start().starts_with("~m ready"),
            "{}",
            row_text(&b, 0)
        );
    })
}

/// Contract rule 4 through the read path an attached TUI uses: a real
/// `ReadView` limited to this TUI's two chips. A model, requests, a spark,
/// a reset and an ECC rise on the host's own chip draw nothing; the same
/// model once it also runs on chip 0 draws its label and its comet.
#[test]
fn attached_through_only_chips_a_hosts_chip_draws_nothing() {
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    tc(|| {
        let (mut a, b) = field();
        let (mut q, _) = field();
        let host = BusHost::new(tt_toplike_bus::BusConfig::default(), &b).unwrap();
        let bus = host.bus().clone();
        let view = BusView::Local(Box::new(host));
        let cache = ReadCache::new();
        let local: Arc<BTreeSet<EntityKey>> = Arc::new([chip(0), chip(1)].into_iter().collect());
        let read = || view.read(&cache).only_chips(local.clone());
        let p = |e: Event| {
            bus.publish("test", Payload::Event(e));
        };
        let ecc = |v: u64| {
            bus.publish(
                "test",
                Payload::Sample(Sample {
                    entity: EntityKey::dram_channel(&EntityKey::chip("0000:ee:00.0"), 1),
                    signal: crate::bus::signals::DRAM_ECC_CORR_RD.into(),
                    value: Value::U64(v),
                    prov: Provenance::Measured,
                }),
            );
        };
        let theirs = EntityKey::model("theirs");
        let foreign = EntityKey::chip("0000:ee:00.0");

        ecc(1);
        let r = read();
        let w = Clocked(&r, 5 * SEC + 1 * TICK);
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));

        p(Event::ModelLoading {
            model: theirs.clone(),
            chips: vec![foreign.clone()],
            progress: Some(0.5),
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 4,
        });
        p(Event::Spark {
            chip: Some(foreign.clone()),
            kind: "vLLM.log".into(),
        });
        p(Event::Reset {
            chips: vec![foreign.clone()],
        });
        p(Event::EccTick {
            chip: foreign.clone(),
            channel: 1,
        });
        ecc(2);
        let r = read();
        let w = Clocked(&r, 5 * SEC + 2 * TICK);
        step(&mut a, &b, &w);
        step(&mut q, &b, &Bus::new().view(w.now_mono_ns()));
        assert!(a.bus_fx().is_empty(), "{:?}", a.bus_fx());
        assert_eq!(a.render(), q.render());

        // The same model also on chip 0: its label and its comet show.
        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone(), chip(0)],
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 2,
        });
        let r = read();
        let w = Clocked(&r, 5 * SEC + 3 * TICK);
        step(&mut a, &b, &w);
        assert_eq!(a.bus_fx().comets.len(), 1);
        assert_eq!(a.bus_fx().chips.len(), 1);
        assert_eq!(a.bus_fx().chips[0].slot, 0);
        assert!(row_text(&a.render(), 0).contains("theirs ready"));
    })
}

#[test]
fn set_bus_none_clears_the_effects_and_the_next_world_is_a_first_sight() {
    tc(|| {
        let t0 = 5 * SEC;
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 2);
        let (mut a, b) = field();
        step(&mut a, &b, &base.view(t0));
        a.set_bus(None, b.devices());
        assert!(a.bus_fx().is_empty());
        // The request is in the world at the next first sight: history.
        step(&mut a, &b, &with.view(t0 + TICK));
        assert!(a.bus_fx().comets.is_empty());
    })
}

#[test]
fn the_legend_and_explain_text_have_no_right_border() {
    for s in LEGEND_TEXT.iter().chain(EXPLAIN_TEXT) {
        assert!(!s.ends_with('║') && !s.ends_with('│'), "{s}");
    }
}
