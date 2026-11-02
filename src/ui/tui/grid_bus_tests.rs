// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Grid and Insights chip panels with and without the signal bus.
//!
//! The golden hashes pin the screens as they were drawn before the bus port.
//! They were taken by running `golden_hashes` against the commit that added
//! this file, which holds only these tests, so a screen drawn from an empty
//! world must hash to exactly these values.

use crate::backend::TelemetryBackend;
use crate::models::{Device, SmbusTelemetry, Telemetry};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;

/// A backend whose readings never change. The mock backend adds random
/// noise, so its screens cannot be pinned.
pub(super) struct Fixed {
    devices: Vec<Device>,
    telem: Vec<Telemetry>,
    smbus: SmbusTelemetry,
}

impl Fixed {
    /// `n` chips of board `board` with PCI addresses `0000:01:00.0`,
    /// `0000:02:00.0`, ...
    pub(super) fn new(n: usize, board: &str) -> Fixed {
        let devices: Vec<Device> = (0..n)
            .map(|i| {
                Device::new(
                    i,
                    board.into(),
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
    fn color(&mut self, c: Color) {
        match c {
            Color::Reset => self.bytes(&[1]),
            Color::Rgb(r, g, b) => self.bytes(&[2, r, g, b]),
            other => self.bytes(format!("{other:?}").as_bytes()),
        }
    }
}

/// A hash of a buffer, cell by cell: each symbol with its colours and
/// modifiers, and a marker at each row end.
pub(super) fn buffer_hash(buf: &Buffer) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    let a = buf.area;
    for y in a.y..a.y + a.height {
        for x in a.x..a.x + a.width {
            let c = &buf[(x, y)];
            h.bytes(c.symbol().as_bytes());
            h.color(c.fg);
            h.color(c.bg);
            h.bytes(&c.modifier.bits().to_le_bytes());
        }
        h.bytes(&[0xff, 0xfe]);
    }
    h.0
}

/// Run `f` with truecolor forced and the default theme set under the theme
/// lock, so a test that flips the app-wide theme cannot change a colour
/// mid-test.
pub(super) fn tc<R>(f: impl FnOnce() -> R) -> R {
    crate::ui::colors::with_theme(crate::ui::colors::Theme::Default, || {
        crate::ui::colors::with_truecolor(true, f)
    })
}

/// The fixtures the golden hashes pin: board, chip count.
pub(super) const FLEETS: [(&str, usize); 2] = [("p300", 3), ("n300", 2)];
/// The terminal sizes the golden hashes pin. They include the sizes the
/// existing Grid tests draw at.
pub(super) const SIZES: [(u16, u16); 4] = [(40, 16), (80, 24), (120, 40), (200, 60)];

/// The Grid screen of `backend` at `w` x `h`.
pub(super) fn grid_buffer(backend: &Fixed, w: u16, h: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| super::super::render_grid_mode(f, backend))
        .unwrap();
    term.backend().buffer().clone()
}

/// The Insights chip panels of `backend` drawn into a `w` x `h` screen.
pub(super) fn panels_buffer(backend: &Fixed, w: u16, h: u16) -> Buffer {
    let particles = std::collections::HashMap::new();
    let baseline = crate::animation::baseline::AdaptiveBaseline::new();
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let area = f.area();
        super::super::render_device_panels(
            f,
            area,
            backend,
            &particles,
            &baseline,
            0,
            None,
            &super::ChipFlags::default(),
        )
    })
    .unwrap();
    term.backend().buffer().clone()
}

/// Every hash: for each fleet, for each size, the Grid then the panels.
pub(super) fn all_hashes(
    grid: impl Fn(&Fixed, u16, u16) -> Buffer,
    panels: impl Fn(&Fixed, u16, u16) -> Buffer,
) -> Vec<u64> {
    tc(|| {
        let mut out = Vec::new();
        for (board, n) in FLEETS {
            let b = Fixed::new(n, board);
            for (w, h) in SIZES {
                out.push(buffer_hash(&grid(&b, w, h)));
                out.push(buffer_hash(&panels(&b, w, h)));
            }
        }
        out
    })
}

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --lib --features tui grid_bus::tests::golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn golden_hashes() {
    println!("{:?}", all_hashes(grid_buffer, panels_buffer));
}

/// The screens as drawn before the bus port, in [`all_hashes`] order. Taken
/// from the commit that added this file.
pub(super) const GOLDEN: [u64; 16] = [
    4627677985046369938,
    132552608772644955,
    9334011028018469006,
    3625453620629595416,
    6527148699475993382,
    17616149948087944249,
    12539039274425627486,
    5992414636549882561,
    2502912525261110251,
    11235147078355310986,
    5141699003594082149,
    5283918007088733431,
    4023280515739292541,
    8460433051788075242,
    9915331039150203509,
    11466176651688754866,
];

#[test]
fn the_screens_with_no_bus_are_the_pinned_screens() {
    assert_eq!(all_hashes(grid_buffer, panels_buffer), GOLDEN.to_vec());
}

// ---- The bus -----------------------------------------------------------------

use super::{
    grid_cells, text_forms, CellGeom, ChipFlags, GridBus, GridFx, GRID_EXPLAIN_TEXT,
    GRID_LEGEND_TEXT, INSIGHTS_EXPLAIN_TEXT, INSIGHTS_LEGEND_TEXT,
};
use crate::animation::die3d::overlay::{
    ModelFlag, DOWN_GREY, FLASH_NS, LOAD_PURPLE, PULSE_NS, SERVE_TEAL, SETTLE_NS,
};
use ratatui::style::Modifier;
use tt_toplike_bus::{
    EntityKey, Event, HeaderKind, LocalView, ModelPhase, Payload, Provenance, Record, Sample,
    Value, World,
};

const SEC: u64 = 1_000_000_000;
/// The gap between two Grid updates at the 10 per second data rate.
const TICK: u64 = 100_000_000;
/// The size the bus tests draw the Grid at: four Blackhole cells a row.
const W: u16 = 80;
const H: u16 = 24;

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
    /// Model `key` ready on `chips`, with a tokens-per-second reading of
    /// provenance `prov` when `tps` is given.
    fn ready_as(
        &mut self,
        key: &str,
        chips: &[usize],
        tps: Option<f64>,
        prov: Provenance,
    ) -> &mut Self {
        let m = EntityKey::model(key);
        if let Some(t) = tps {
            self.sample(
                &m,
                tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
                Value::F64(t),
                prov,
            );
        }
        self.event(Event::ModelReady {
            model: m,
            chips: chips.iter().map(|&i| chip(i)).collect(),
        })
    }
    fn ready(&mut self, key: &str, chips: &[usize], tps: Option<f64>) -> &mut Self {
        self.ready_as(key, chips, tps, Provenance::Measured)
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

/// The Grid screen of `b` at `w` x `h` with the bus effects of `g` on it.
fn grid_with(b: &Fixed, g: &GridBus, w: u16, h: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        super::super::render_grid_mode(f, b);
        let area = f.area();
        super::draw_grid(f.buffer_mut(), area, b.devices(), g.fx());
    })
    .unwrap();
    term.backend().buffer().clone()
}

/// The Insights chip panels of `b` at `w` x `h` with the model rules of
/// `flags`.
fn panels_with(b: &Fixed, flags: &ChipFlags, w: u16, h: u16) -> Buffer {
    let particles = std::collections::HashMap::new();
    let baseline = crate::animation::baseline::AdaptiveBaseline::new();
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let area = f.area();
        super::super::render_device_panels(f, area, b, &particles, &baseline, 0, None, flags)
    })
    .unwrap();
    term.backend().buffer().clone()
}

/// Row `y` of `buf` as text.
fn row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
}

/// The text of the top row of `g`, the cell's own columns only.
fn top(buf: &Buffer, g: &CellGeom) -> String {
    (g.x..g.x + 1 + g.cols)
        .map(|x| buf[(x, g.y)].symbol())
        .collect()
}

/// The text of the bottom rule of `g`, the cell's own columns only.
fn rule(buf: &Buffer, g: &CellGeom) -> String {
    let y = g.y + g.h - 1;
    (g.x..g.x + 1 + g.cols)
        .map(|x| buf[(x, y)].symbol())
        .collect()
}

/// Cell `i` of `b`'s Grid at [`W`] x [`H`].
fn geom(b: &Fixed, i: usize) -> CellGeom {
    grid_cells(Rect::new(0, 0, W, H), b.devices())
        .nth(i)
        .unwrap()
}

fn fg(c: [u8; 3]) -> Color {
    crate::ui::colors::rgb(c[0], c[1], c[2])
}

/// The left bar's cells of `g`, bottom first.
fn bar(buf: &Buffer, g: &CellGeom) -> Vec<(String, Color)> {
    (0..g.h - 2)
        .map(|i| {
            let c = &buf[(g.x, g.y + g.h - 2 - i)];
            (c.symbol().to_string(), c.fg)
        })
        .collect()
}

use ratatui::layout::Rect;

#[test]
fn an_empty_world_draws_the_pinned_screens() {
    // An empty world, and one holding only a reading these views do not
    // draw, give exactly the screens drawn before the bus. Several updates
    // run first, so any memory the Grid keeps has had a chance to act.
    let mut quiet = Bus::new();
    quiet.sample(
        &chip(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    for world in [Bus::new(), quiet] {
        let got = all_hashes(
            |b, w, h| {
                let mut g = GridBus::default();
                for n in 0..5 {
                    g.update(&world.view(5 * SEC + n * TICK), b.devices());
                }
                assert_eq!(g.fx().chips, vec![]);
                grid_with(b, &g, w, h)
            },
            |b, w, h| {
                let flags = ChipFlags::from_world(&world.view(5 * SEC), b.devices());
                panels_with(b, &flags, w, h)
            },
        );
        assert_eq!(got, GOLDEN.to_vec());
    }
}

/// `grid_cells` repeats `render_grid_mode`'s layout. Where a cell's corners
/// are on screen, `render_grid_mode` drew `╔` and `╚` there.
#[test]
fn grid_cells_match_what_render_grid_mode_draws() {
    for (board, n) in FLEETS {
        let b = Fixed::new(n, board);
        for (w, h) in SIZES.into_iter().chain([(19, 16), (12, 8), (5, 6)]) {
            let buf = grid_buffer(&b, w, h);
            let cells: Vec<CellGeom> = grid_cells(Rect::new(0, 0, w, h), b.devices()).collect();
            assert!(!cells.is_empty(), "{board} {w}x{h}");
            let mut checked = 0;
            for g in cells {
                if g.x < w && g.y + g.h - 1 < h {
                    assert_eq!(buf[(g.x, g.y)].symbol(), "╔", "{board} {w}x{h} {g:?}");
                    assert_eq!(buf[(g.x, g.y + g.h - 1)].symbol(), "╚", "{board} {w}x{h}");
                    assert_eq!(
                        buf[(g.x + g.cols, g.y + g.h - 1)].symbol(),
                        "═",
                        "{board} {w}x{h}"
                    );
                    checked += 1;
                }
            }
            if h >= 16 && w >= 19 {
                assert!(checked > 0, "{board} {w}x{h}");
            }
        }
    }
}

#[test]
fn the_model_text_is_on_its_chips_bottom_rule() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let mut f = Bus::new();
        f.ready("m", &[0], Some(80.0));
        let mut g = GridBus::default();
        g.update(&f.view(5 * SEC), b.devices());
        let buf = grid_with(&b, &g, W, H);
        let r0 = rule(&buf, &geom(&b, 0));
        assert_eq!(r0, "╚═ready 80t/s═════", "{r0}");
        let g0 = geom(&b, 0);
        assert_eq!(buf[(g0.x + 2, g0.y + g0.h - 1)].fg, fg(SERVE_TEAL));
        // The label is on the top row, after the device label.
        assert_eq!(top(&buf, &g0), "╔BH p300 m        ");
        assert_eq!(buf[(g0.x + 9, g0.y)].fg, fg(SERVE_TEAL));
        // Chip 1 has no model: its rule is as before.
        let plain = grid_buffer(&b, W, H);
        assert_eq!(rule(&buf, &geom(&b, 1)), rule(&plain, &geom(&b, 1)));
        assert_eq!(top(&buf, &geom(&b, 1)), top(&plain, &geom(&b, 1)));
    })
}

/// A label longer than the room left on the top row is cut with `…`, and
/// the device label stays whole.
#[test]
fn a_long_label_is_cut_to_the_top_row() {
    tc(|| {
        for (board, want) in [("p300", "╔BH p300 Llama-3.…"), ("n300", "╔WH n300 L…")] {
            let b = Fixed::new(2, board);
            let mut f = Bus::new();
            f.loading("Llama-3.1-70B", &[0], Some(0.25));
            let mut g = GridBus::default();
            g.update(&f.view(5 * SEC), b.devices());
            let buf = grid_with(&b, &g, W, H);
            assert_eq!(top(&buf, &geom(&b, 0)), want, "{board}");
        }
    })
}

#[test]
fn an_estimated_model_text_starts_with_a_tilde() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let mut f = Bus::new();
        f.ready_as("m", &[0], Some(20.0), Provenance::Estimated);
        let mut g = GridBus::default();
        g.update(&f.view(5 * SEC), b.devices());
        let r0 = rule(&grid_with(&b, &g, W, H), &geom(&b, 0));
        assert_eq!(r0, "╚═~ready 20t/s════", "{r0}");
        // Measured: no tilde.
        let mut f2 = Bus::new();
        f2.ready("m", &[0], Some(20.0));
        let mut g = GridBus::default();
        g.update(&f2.view(5 * SEC), b.devices());
        let r0 = rule(&grid_with(&b, &g, W, H), &geom(&b, 0));
        assert_eq!(r0, "╚═ready 20t/s═════", "{r0}");

        // A Wormhole cell is too narrow for that form: the phase stays, and
        // so does the `~`.
        let b = Fixed::new(2, "n300");
        let mut g = GridBus::default();
        g.update(&f.view(5 * SEC), b.devices());
        let r0 = rule(&grid_with(&b, &g, W, H), &geom(&b, 0));
        assert!(r0.starts_with("╚═~"), "{r0}");
        assert!(r0.contains("ready"), "{r0}");
    })
}

/// The text stays inside the cell's own rule at every size and on both
/// architectures, with one `═` left at each end, and nothing the bus draws
/// lands outside the chips' cells.
#[test]
fn the_bus_keeps_every_portrait_inside_its_cell() {
    tc(|| {
        for (board, n) in FLEETS {
            let b = Fixed::new(n, board);
            let mut f = Bus::new();
            f.ready_as(
                "Llama-3.1-70B-Instruct-with-a-long-name",
                &(0..n).collect::<Vec<_>>(),
                Some(1234.5),
                Provenance::Estimated,
            );
            let mut g = GridBus::default();
            g.update(&f.view(5 * SEC), b.devices());
            f.requests("Llama-3.1-70B-Instruct-with-a-long-name", 3)
                .event(Event::Reset {
                    chips: vec![chip(0)],
                })
                .event(Event::Spark {
                    chip: Some(chip(0)),
                    kind: "vLLM.log".into(),
                });
            g.update(&f.view(5 * SEC + TICK), b.devices());
            assert!(!g.fx().chips.is_empty());
            for (w, h) in SIZES.into_iter().chain([(19, 16), (12, 8), (5, 6)]) {
                let with = grid_with(&b, &g, w, h);
                let plain = grid_buffer(&b, w, h);
                let cells: Vec<CellGeom> = grid_cells(Rect::new(0, 0, w, h), b.devices()).collect();
                for y in 0..h {
                    for x in 0..w {
                        if with[(x, y)] == plain[(x, y)] {
                            continue;
                        }
                        let inside = cells
                            .iter()
                            .any(|c| x >= c.x && x <= c.x + c.cols && y >= c.y && y < c.y + c.h);
                        assert!(inside, "{board} {w}x{h}: ({x},{y}) changed outside a cell");
                    }
                }
                for c in &cells {
                    let y = c.y + c.h - 1;
                    if y < h && c.x + c.cols < w {
                        assert_eq!(with[(c.x + 1, y)].symbol(), "═", "{board} {w}x{h}");
                        assert_eq!(with[(c.x + c.cols, y)].symbol(), "═", "{board} {w}x{h}");
                    }
                }
            }
        }
    })
}

#[test]
fn loading_fills_the_bar_to_its_progress_and_no_progress_sweeps() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let g0 = geom(&b, 0);
        let mut f = Bus::new();
        f.loading("l", &[0], Some(0.5));
        let mut g = GridBus::default();
        g.update(&f.view(5 * SEC), b.devices());
        let cells = bar(&grid_with(&b, &g, W, H), &g0);
        assert_eq!(cells.len(), 12);
        let filled: Vec<bool> = cells
            .iter()
            .map(|(s, c)| s == "┃" && *c == fg(LOAD_PURPLE))
            .collect();
        assert_eq!(filled, [vec![true; 6], vec![false; 6]].concat());
        // Chip 1's bar is as before.
        let plain = grid_buffer(&b, W, H);
        assert_eq!(
            bar(&grid_with(&b, &g, W, H), &geom(&b, 1)),
            bar(&plain, &geom(&b, 1))
        );

        // No progress figure: a band climbs the bar, drawn as a tint over
        // the plain bar's characters, and it moves.
        let mut f = Bus::new();
        f.loading("l", &[0], None);
        let mut g = GridBus::default();
        let mut seen = Vec::new();
        for k in 0..30 {
            g.update(&f.view(5 * SEC + k * TICK), b.devices());
            let cells = bar(&grid_with(&b, &g, W, H), &g0);
            assert!(cells.iter().all(|(s, _)| s == "║"));
            let lit: Vec<usize> = (0..cells.len())
                .filter(|&i| cells[i].1 != bar(&plain, &g0)[i].1)
                .collect();
            seen.push(lit);
        }
        assert!(seen.iter().any(|l| !l.is_empty()), "the band shows");
        seen.dedup();
        assert!(seen.len() > 3, "the band moves: {seen:?}");
    })
}

#[test]
fn ready_settles_the_bar_and_down_greys_it() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let g0 = geom(&b, 0);
        let plain = bar(&grid_buffer(&b, W, H), &g0);
        let mut f = Bus::new();
        f.loading("m", &[0], Some(0.9));
        let mut g = GridBus::default();
        let t0 = 5 * SEC;
        g.update(&f.view(t0), b.devices());
        f.ready("m", &[0], None);
        g.update(&f.view(t0 + TICK), b.devices());
        let lit = bar(&grid_with(&b, &g, W, H), &g0);
        assert!(lit.iter().all(|(s, _)| s == "║"));
        assert_eq!(lit[0].1, fg(SERVE_TEAL), "fully teal at the change");
        g.update(&f.view(t0 + TICK + SETTLE_NS), b.devices());
        assert_eq!(bar(&grid_with(&b, &g, W, H), &g0), plain, "settled");
        // A re-announcement with the same phase changes nothing.
        f.ready("m", &[0], None);
        g.update(&f.view(t0 + 2 * TICK + SETTLE_NS), b.devices());
        assert_eq!(bar(&grid_with(&b, &g, W, H), &g0), plain);

        f.phase("m", "down");
        g.update(&f.view(t0 + 3 * TICK + SETTLE_NS), b.devices());
        let grey = bar(&grid_with(&b, &g, W, H), &g0);
        assert_eq!(grey[0].1, fg(DOWN_GREY), "grey at the change");

        // First sight of a ready model: no settle.
        let mut f = Bus::new();
        f.ready("m", &[0], None);
        let mut g = GridBus::default();
        g.update(&f.view(t0), b.devices());
        assert_eq!(bar(&grid_with(&b, &g, W, H), &g0), plain);
    })
}

/// The PCIe spine cells of `b`'s cell 0, bottom first, with their colour.
fn spine(buf: &Buffer, g: &CellGeom) -> Vec<Color> {
    (0..10).map(|i| buf[(g.x + 9, g.y + 11 - i)].fg).collect()
}

#[test]
fn a_request_since_the_last_update_pulses_the_pcie_spine_and_history_does_not() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let g0 = geom(&b, 0);
        let plain = grid_buffer(&b, W, H);
        assert!((0..10).all(|i| plain[(g0.x + 9, g0.y + 11 - i)].symbol() == "╋"));
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 3);
        let t0 = 5 * SEC;

        // Seen arriving: the bottom of the spine lights teal, and the light
        // climbs.
        let mut g = GridBus::default();
        g.update(&base.view(t0), b.devices());
        g.update(&with.view(t0 + TICK), b.devices());
        let s = spine(&grid_with(&b, &g, W, H), &g0);
        assert_eq!(s[0], fg(SERVE_TEAL), "{s:?}");
        g.update(&with.view(t0 + TICK + PULSE_NS / 2), b.devices());
        let s = spine(&grid_with(&b, &g, W, H), &g0);
        assert_eq!(s[5], fg(SERVE_TEAL), "halfway up: {s:?}");
        assert_ne!(s[0], fg(SERVE_TEAL));
        g.update(&with.view(t0 + TICK + PULSE_NS), b.devices());
        assert_eq!(
            grid_with(&b, &g, W, H),
            grid_with(&b, &quiet(&b, &base), W, H)
        );

        // History: the event was already there at the first read.
        let mut h = GridBus::default();
        h.update(&with.view(t0), b.devices());
        assert!(h.fx().chips.iter().all(|c| c.pulses.is_empty()));

        // Away: the event landed while the Grid was not updating.
        let mut a = GridBus::default();
        for i in 0..5 {
            a.update(&base.view(t0 + i * TICK), b.devices());
        }
        a.update(&with.view(t0 + 10 * SEC), b.devices());
        assert!(a.fx().chips.iter().all(|c| c.pulses.is_empty()));
    })
}

/// A Grid that has seen only `world`, once.
fn quiet(b: &Fixed, world: &Bus) -> GridBus {
    let mut g = GridBus::default();
    g.update(&world.view(5 * SEC), b.devices());
    g
}

#[test]
fn on_a_wormhole_the_pulse_climbs_the_left_bar() {
    tc(|| {
        let b = Fixed::new(2, "n300");
        let g0 = geom(&b, 0);
        let mut base = Bus::new();
        base.ready("m", &[0], None);
        let mut with = base.clone();
        with.requests("m", 1);
        let mut g = GridBus::default();
        g.update(&base.view(5 * SEC), b.devices());
        g.update(&with.view(5 * SEC + TICK), b.devices());
        let cells = bar(&grid_with(&b, &g, W, H), &g0);
        assert_eq!(cells[0], ("║".to_string(), fg(SERVE_TEAL)));
    })
}

#[test]
fn a_request_count_of_zero_and_a_model_on_another_chip_pulse_nothing() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let mut base = Bus::new();
        base.ready("m", &[0], None).ready("o", &[1], None);
        let mut with = base.clone();
        with.requests("m", 0).requests("o", 2);
        let mut g = GridBus::default();
        g.update(&base.view(5 * SEC), b.devices());
        g.update(&with.view(5 * SEC + TICK), b.devices());
        let pulses: Vec<(usize, usize)> = g
            .fx()
            .chips
            .iter()
            .map(|c| (c.cell, c.pulses.len()))
            .collect();
        assert_eq!(pulses, vec![(0, 0), (1, 1)]);
    })
}

#[test]
fn a_new_reset_flashes_its_portrait_and_an_old_one_does_not() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let reversed = |buf: &Buffer, g: &CellGeom| {
            (g.y + 1..g.y + 1 + g.rows)
                .flat_map(|y| (g.x + 1..g.x + 1 + g.cols).map(move |x| (x, y)))
                .filter(|p| buf[*p].modifier.contains(Modifier::REVERSED))
                .count()
        };
        let (g0, g1) = (geom(&b, 0), geom(&b, 1));
        let mut f = Bus::new();
        f.event(Event::Reset {
            chips: vec![chip(0)],
        });
        let t0 = 5 * SEC;
        let mut g = GridBus::default();
        g.update(&f.view(t0), b.devices());
        assert_eq!(reversed(&grid_with(&b, &g, W, H), &g0), 0, "first sight");
        f.event(Event::Reset {
            chips: vec![chip(0)],
        });
        g.update(&f.view(t0 + TICK), b.devices());
        let buf = grid_with(&b, &g, W, H);
        assert_eq!(reversed(&buf, &g0), 17 * 12, "a new reset flashes chip 0");
        assert_eq!(reversed(&buf, &g1), 0, "and not chip 1");
        g.update(&f.view(t0 + TICK + FLASH_NS), b.devices());
        assert_eq!(
            reversed(&grid_with(&b, &g, W, H), &g0),
            0,
            "the flash is over"
        );

        // A reset while the Grid was away: no flash on return.
        let mut g = GridBus::default();
        for i in 0..5 {
            g.update(&f.view(t0 + i * TICK), b.devices());
        }
        f.event(Event::Reset {
            chips: vec![chip(0)],
        });
        g.update(&f.view(t0 + 10 * SEC), b.devices());
        assert_eq!(reversed(&grid_with(&b, &g, W, H), &g0), 0, "away");
    })
}

#[test]
fn sparks_and_compiles_light_a_tensix_cell_of_their_chip() {
    tc(|| {
        for board in ["p300", "n300"] {
            let b = Fixed::new(2, board);
            let plain = grid_buffer(&b, W, H);
            let mut f = Bus::new();
            let mut g = GridBus::default();
            g.update(&f.view(5 * SEC), b.devices());
            for k in 0..6u64 {
                f.event(Event::Spark {
                    chip: Some(chip(1)),
                    kind: "vLLM.log".into(),
                })
                .event(Event::KernelCompiled {
                    chip: Some(chip(1)),
                    source: "ttnn".into(),
                });
                g.update(&f.view(5 * SEC + (k + 1) * TICK / 4), b.devices());
            }
            let buf = grid_with(&b, &g, W, H);
            let g1 = geom(&b, 1);
            let stars: Vec<(u16, u16)> = (g1.y + 1..g1.y + 1 + g1.rows)
                .flat_map(|y| (g1.x + 1..g1.x + 1 + g1.cols).map(move |x| (x, y)))
                .filter(|p| buf[*p].symbol() == "*")
                .collect();
            assert!(!stars.is_empty(), "{board}");
            for (x, y) in stars {
                // Only on cells the plain portrait drew as Tensix.
                let s = plain[(x, y)].symbol();
                assert!(
                    ["░", "▒", "▓", "·"].contains(&s),
                    "{board} ({x},{y}) was {s}"
                );
            }
            // Chip 0's portrait is untouched.
            let g0 = geom(&b, 0);
            for y in g0.y..g0.y + g0.h {
                for x in g0.x..=g0.x + g0.cols {
                    assert_eq!(buf[(x, y)], plain[(x, y)], "{board}");
                }
            }
        }
    })
}

/// `ReadView` limited to this TUI's chips (`ReadView::only_chips`), as
/// `TuiBus::read` hands it when attached. A model, requests, a spark and a
/// reset on the host's own chip draw nothing on the Grid and put no rule on
/// the Insights panels; the same model once it also runs on chip 0 shows on
/// chip 0.
#[test]
fn attached_through_only_chips_a_hosts_chip_draws_nothing() {
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    tc(|| {
        let b = Fixed::new(2, "p300");
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

        let mut g = GridBus::default();
        g.update(&read(), b.devices());
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
        p(Event::Reset {
            chips: vec![foreign.clone()],
        });
        let w = read();
        g.update(&w, b.devices());
        assert_eq!(g.fx().chips, vec![], "nothing on the Grid");
        assert_eq!(
            buffer_hash(&grid_with(&b, &g, W, H)),
            buffer_hash(&grid_buffer(&b, W, H))
        );
        let flags = ChipFlags::from_world(&w, b.devices());
        assert_eq!(
            buffer_hash(&panels_with(&b, &flags, 120, 40)),
            buffer_hash(&panels_buffer(&b, 120, 40)),
            "no rule on the panels"
        );

        p(Event::ModelReady {
            model: theirs.clone(),
            chips: vec![foreign.clone(), chip(0)],
        });
        p(Event::RequestsCompleted {
            model: theirs.clone(),
            n: 2,
        });
        let w = read();
        g.update(&w, b.devices());
        let cells: Vec<(usize, usize)> = g
            .fx()
            .chips
            .iter()
            .map(|c| (c.cell, c.pulses.len()))
            .collect();
        assert_eq!(cells, vec![(0, 1)]);
        assert!(top(&grid_with(&b, &g, W, H), &geom(&b, 0)).contains("theirs"));
        let flags = ChipFlags::from_world(&w, b.devices());
        let buf = panels_with(&b, &flags, 120, 40);
        let rows: Vec<String> = (0..40).map(|y| row(&buf, y)).collect();
        assert_eq!(
            rows.iter()
                .filter(|r| r.contains("── theirs ready"))
                .count(),
            1,
            "{rows:#?}"
        );
    })
}

#[test]
fn the_insights_rule_names_the_model_and_fills_to_its_progress() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let mut f = Bus::new();
        f.ready_as("mock", &[0], Some(20.0), Provenance::Estimated)
            .loading("big", &[1], Some(0.5));
        let flags = ChipFlags::from_world(&f.view(5 * SEC), b.devices());
        let buf = panels_with(&b, &flags, 120, 40);
        let rows: Vec<(u16, String)> = (0..40).map(|y| (y, row(&buf, y))).collect();
        let (y0, r0) = rows
            .iter()
            .find(|(_, r)| r.contains("── ~mock ready 20t/s ─"))
            .expect("chip 0's rule");
        let (y1, r1) = rows
            .iter()
            .find(|(_, r)| r.contains("── big loading 50% ─"))
            .expect("chip 1's rule");
        assert_eq!(y0, y1, "both panels' footers are on one row");
        let _ = r0;
        // Chip 1's panel starts after chip 0's. The dashes after its text
        // are half purple, then dark grey.
        let at = r1.find("── big").unwrap();
        let start = r1[..at].chars().count() as u16;
        let after = start + "── big loading 50% ".chars().count() as u16;
        let dash: Vec<Color> = (after..buf.area.width)
            .take_while(|&x| buf[(x, *y1)].symbol() == "─")
            .map(|x| buf[(x, *y1)].fg)
            .collect();
        let purple = dash.iter().filter(|c| **c == fg(LOAD_PURPLE)).count();
        assert!(dash.len() > 20, "{}", dash.len());
        assert!(
            purple.abs_diff(dash.len() / 2) <= 1,
            "{purple} of {} purple",
            dash.len()
        );
        assert!(dash[..purple].iter().all(|c| *c == fg(LOAD_PURPLE)));
        assert!(dash[purple..].iter().all(|c| *c == Color::DarkGray));
        // Chip 0 is ready: no purple on its rule.
        assert!((0..start).all(|x| buf[(x, *y0)].fg != fg(LOAD_PURPLE)));
    })
}

/// The Insights process table of `rows` at 120 x 12 with the model flags
/// `flags`, and no local serving metrics.
fn process_panel_with(rows: &[crate::workload::ProcRow], flags: &ChipFlags) -> Vec<String> {
    let mut term = Terminal::new(TestBackend::new(120, 12)).unwrap();
    term.draw(|f| {
        let area = f.area();
        super::super::render_process_panel(
            f,
            area,
            rows,
            0,
            None,
            10.0,
            1 << 30,
            4 << 30,
            false,
            None,
            flags,
            #[cfg(all(target_os = "linux", feature = "linux-procfs"))]
            &std::collections::HashMap::new(),
        )
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    (0..12).map(|y| row(&buf, y)).collect()
}

/// Group C S1: attached to a headless host that serves a model on chip 0,
/// the process on chip 0 gets a serving row from the world's model (its
/// phase, label with `~` for an estimate, and tokens per second). On a
/// local bus the row is the local monitor's, so nothing is drawn from the
/// world, and a process on a chip with no model gets no row.
#[test]
fn an_attached_process_row_shows_the_worlds_model_on_its_chips() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let mut f = Bus::new();
        f.ready_as("llama", &[0], Some(42.0), Provenance::Estimated);
        let proc_on = |dev: usize| crate::workload::ProcRow {
            pid: 4242,
            name: "vllm".into(),
            cpu_pct: 90.0,
            mem_bytes: 8 << 30,
            inference: Some("vLLM"),
            active: true,
            tt: Some(crate::workload::host_processes::TtProcInfo {
                device_indices: vec![dev],
                hugepages_1g: 0,
                hugepages_2m: 0,
            }),
        };
        let flags = ChipFlags::from_world(&f.view(5 * SEC), b.devices());
        let attached = flags.clone().with_attached(true);

        let text = process_panel_with(&[proc_on(0)], &attached).join("\n");
        assert!(text.contains("[bus] ready"), "{text}");
        assert!(text.contains("model=~llama"), "{text}");
        assert!(text.contains("42tok/s"), "{text}");

        // A process on chip 1, which serves nothing: no row.
        let text = process_panel_with(&[proc_on(1)], &attached).join("\n");
        assert!(!text.contains("[bus]"), "{text}");
        // A local bus: the local monitor's row, so nothing from the world.
        let text = process_panel_with(&[proc_on(0)], &flags).join("\n");
        assert!(!text.contains("[bus]"), "{text}");
    })
}

#[test]
fn text_forms_end_with_the_phase_alone() {
    let flag = ModelFlag {
        label: "Llama-3.1-70B-Instruct".into(),
        phase: ModelPhase::Ready,
        colour: SERVE_TEAL,
        tokens_per_s: Some(812.0),
        waiting: Some(3),
        progress: None,
        estimated: true,
    };
    let forms = text_forms(&flag);
    assert_eq!(
        forms,
        vec![
            "~Llama-3.1-70B-Instruct ready 812t/s 3 waiting",
            "~Llama-3.1-70B-Instruct ready 812t/s",
            "~Llama-3.1-7… ready 812t/s",
            "~ready 812t/s",
            "~ready",
        ]
    );
}

/// The frame path takes no world: `draw_grid` is handed a `GridFx` and the
/// footer a `ChipFlags`, both built at the data rate. The type signatures
/// are the guard; this test pins that drawing twice gives the same screen,
/// so nothing on the frame path keeps state.
#[test]
fn drawing_twice_gives_the_same_screen() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let mut f = Bus::new();
        f.loading("l", &[0], None).ready("m", &[1], Some(80.0));
        let mut g = GridBus::default();
        g.update(&f.view(5 * SEC), b.devices());
        f.requests("m", 4);
        g.update(&f.view(5 * SEC + TICK), b.devices());
        let fx: &GridFx = g.fx();
        assert!(!fx.chips.is_empty());
        assert_eq!(grid_with(&b, &g, W, H), grid_with(&b, &g, W, H));
    })
}

#[test]
fn the_legend_and_explain_text_name_what_the_bus_draws() {
    let grid = GRID_LEGEND_TEXT.join(" ") + &GRID_EXPLAIN_TEXT.join(" ");
    for word in [
        "model",
        "progress",
        "PCIe",
        "reset",
        "'~'",
        "decoration",
        "compile",
    ] {
        assert!(
            grid.to_lowercase().contains(&word.to_lowercase()),
            "grid: {word}"
        );
    }
    let ins = INSIGHTS_LEGEND_TEXT.join(" ") + &INSIGHTS_EXPLAIN_TEXT.join(" ");
    for word in ["model", "progress", "tokens/s", "'~'", "purple"] {
        assert!(ins.contains(word), "insights: {word}");
    }
    // The overlay is about 52 columns wide inside its bar.
    for line in GRID_LEGEND_TEXT.iter().chain(INSIGHTS_LEGEND_TEXT) {
        assert!(line.chars().count() <= 50, "{line}");
    }
}

/// Prints a Grid screen and the Insights panels with something from the bus
/// on each chip: a model loading at 40% on chip 0, an estimated one ready on
/// chip 1 with a pulse just begun, a spark. For a person to look at;
/// asserts nothing. Run with
/// `cargo test --lib --features tui grid_bus_smoke_screen -- --ignored --nocapture`.
#[test]
#[ignore = "prints a screen for a person to look at"]
fn grid_bus_smoke_screen() {
    tc(|| {
        let b = Fixed::new(3, "p300");
        let mut f = Bus::new();
        f.loading("Llama-3.1-8B", &[0], Some(0.4)).ready_as(
            "Qwen3-32B",
            &[1, 2],
            Some(42.0),
            Provenance::Estimated,
        );
        let mut g = GridBus::default();
        g.update(&f.view(5 * SEC), b.devices());
        f.requests("Qwen3-32B", 5).event(Event::Spark {
            chip: Some(chip(1)),
            kind: "vLLM.log".into(),
        });
        g.update(&f.view(5 * SEC + TICK), b.devices());
        let buf = grid_with(&b, &g, W, H);
        for y in 0..H {
            println!("{}", row(&buf, y));
        }
        let flags = ChipFlags::from_world(&f.view(5 * SEC), b.devices());
        let buf = panels_with(&b, &flags, 160, 40);
        for y in 0..40 {
            println!("{}", row(&buf, y).trim_end());
        }
    })
}

/// A world that hands out models whose chip lists are empty, as an attached
/// world does for another machine's models once `ReadView::only_chips` has
/// cut them. Die 3D's `Foreign` double, for Grid and Insights.
struct Foreign(LocalView);

impl tt_toplike_bus::WorldRead for Foreign {
    fn seq(&self) -> u64 {
        self.0.seq()
    }
    fn models(&self) -> Vec<tt_toplike_bus::ModelView> {
        let mut m = self.0.models();
        m.iter_mut().for_each(|m| m.chips.clear());
        m
    }
    fn model_on_chip(&self, chip: &EntityKey) -> Option<tt_toplike_bus::ModelView> {
        self.0.model_on_chip(chip).map(|mut m| {
            m.chips.clear();
            m
        })
    }
    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<tt_toplike_bus::Reading> {
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
/// nothing. Grid draws nothing for it, and the Insights rule stays plain.
#[test]
fn a_model_whose_chip_list_was_cut_away_draws_nothing() {
    tc(|| {
        let b = Fixed::new(2, "p300");
        let mut f = Bus::new();
        f.loading("far", &[0], Some(0.5))
            .ready("other", &[1], Some(9.0));
        let w = Foreign(f.view(5 * SEC));
        let mut g = GridBus::default();
        g.update(&w, b.devices());
        assert_eq!(g.fx().chips, vec![]);
        let flags = ChipFlags::from_world(&w, b.devices());
        assert_eq!(
            buffer_hash(&panels_with(&b, &flags, 120, 40)),
            buffer_hash(&panels_buffer(&b, 120, 40))
        );
    })
}
