// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The terminal traffic of each backdrop on a blank view. A measurement, so
//! it is ignored by default and asserts nothing about time (the box it runs
//! on may be loaded). Run with
//! `cargo test --release --locked --lib --features tui backdrop_blank_view_traffic -- --ignored --nocapture`.

use crate::animation::backdrop::{BackdropKind, Choice};
use crate::animation::backdrop_pins::scene_rosters;
use crate::animation::bus_test_support::{tc, Fixed};
use crate::backend::pcie_counters::PcieBandwidth;
use crate::backend::TelemetryBackend;
use crate::models::{Device, SmbusTelemetry, Telemetry};
use ratatui::layout::Rect;

/// [`Fixed`] with a PCIe rate on every chip, so the clouds drift.
struct WithPcie(Fixed, Option<PcieBandwidth>);

impl TelemetryBackend for WithPcie {
    fn init(&mut self) -> crate::error::BackendResult<()> {
        Ok(())
    }
    fn update(&mut self) -> crate::error::BackendResult<()> {
        Ok(())
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
        "fixed".into()
    }
    fn pcie_bandwidth(&self, _i: usize) -> Option<PcieBandwidth> {
        self.1
    }
}

/// A writer that only counts the bytes written to it.
struct Counter(std::rc::Rc<std::cell::Cell<usize>>);

impl std::io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.set(self.0.get() + buf.len());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Bytes a second the terminal receives for the backdrop alone on a blank
/// `w` x `h` view at `fps` frames a second, the view updated (and the
/// backdrop fed) every frame, over `secs` seconds of view time. Frames go
/// through ratatui's diff and the crossterm backend into a byte count, as
/// they would to a terminal. Busy: a serving model, 4 GB/s of PCIe and a
/// loaded chip. A fixed clock: frame `i` is at `i / fps` seconds.
pub(crate) fn blank_view_bytes_per_s(kind: Choice, busy: bool, fps: u32, secs: u32) -> f64 {
    use crate::animation::backdrop::{Backdrop, BackdropSignals};
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    let (w, h) = (200u16, 60u16);
    let backend = WithPcie(
        Fixed::new(2),
        busy.then_some(PcieBandwidth {
            rx_bytes_per_sec: 3.0e9,
            tx_bytes_per_sec: 1.0e9,
        }),
    );
    let (_, rows) = scene_rosters()
        .into_iter()
        .find(|(n, _)| *n == "aurora")
        .unwrap();
    let mut b = Backdrop::new(kind);
    let count = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let mut term = Terminal::with_options(
        CrosstermBackend::new(Counter(count.clone())),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, w, h)),
        },
    )
    .expect("terminal");
    let settle = fps as usize; // the first second is left out of the count
    let frames = (fps * secs) as usize;
    let mut counted_from = 0usize;
    for i in 0..settle + frames {
        let now_s = i as f64 / fps as f64;
        let services = if busy { rows(i as u64) } else { Vec::new() };
        let mut s = BackdropSignals::gather(None, &backend, services, now_s, false);
        if !busy {
            s.pcie_bps = None;
        }
        b.observe(&s);
        term.draw(|f| {
            let area = f.area();
            b.layer_buffer(f.buffer_mut(), area);
        })
        .expect("draw");
        if i + 1 == settle {
            counted_from = count.get();
        }
    }
    let total = count.get() - counted_from;
    total as f64 / secs as f64
}

#[test]
#[ignore = "a measurement; prints a table"]
fn backdrop_blank_view_traffic() {
    tc(|| {
        println!("| backdrop | signals | fps | bytes per second |");
        println!("|---|---|---|---|");
        for kind in [
            Some(BackdropKind::Scene),
            Some(BackdropKind::Sky),
            Some(BackdropKind::Clouds),
        ] {
            for busy in [false, true] {
                for fps in [10, 60] {
                    let bps = blank_view_bytes_per_s(kind, busy, fps, 10);
                    println!(
                        "| {} | {} | {fps} | {bps:.0} |",
                        crate::animation::backdrop::choice_name(kind),
                        if busy { "busy" } else { "idle" }
                    );
                }
            }
        }
    })
}
