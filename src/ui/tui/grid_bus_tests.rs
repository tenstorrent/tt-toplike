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
        super::super::render_device_panels(f, area, backend, &particles, &baseline, 0, None)
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
