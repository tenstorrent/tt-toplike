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
/// bus tests hand the arcade an empty world there). Truecolor is forced so
/// the colours do not depend on the terminal running the tests.
pub(super) fn hashes_with(
    w: usize,
    h: usize,
    mut before_update: impl FnMut(&mut ArcadeVisualization, &Fixed, u32),
) -> Vec<u64> {
    crate::ui::colors::with_truecolor(true, || {
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
