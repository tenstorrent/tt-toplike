// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The cost of each backdrop on the arcade at 200 x 60: cells changed per
//! frame and frame time. A measurement, so it is ignored by default and
//! asserts nothing about time (the box it runs on may be loaded). Run with
//! `cargo test --release --locked --lib --features tui backdrop_cost -- --ignored --nocapture`.

use crate::animation::backdrop::{BackdropKind, Choice};
use crate::animation::backdrop_pins::scene_rosters;
use crate::animation::bus_test_support::{tc, Fixed};
use crate::animation::ArcadeVisualization;
use crate::backend::pcie_counters::PcieBandwidth;
use crate::backend::TelemetryBackend;
use crate::models::{Device, SmbusTelemetry, Telemetry};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

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

/// Mean cells changed per frame, mean and 95th percentile frame time (ms)
/// of the default arcade at `w` x `h` over `frames` frames at 60 a second.
pub(crate) fn measure(kind: Choice, busy: bool, w: u16, h: u16, frames: usize) -> (f64, f64, f64) {
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
    let mut a = ArcadeVisualization::new(w as usize, h as usize);
    a.initialize_from_devices(backend.devices());
    a.initialize_topology(&backend);
    a.set_backdrop(kind);
    let area = Rect::new(0, 0, w, h);
    let mut prev = Buffer::empty(area);
    let (mut changed, mut times) = (0usize, Vec::with_capacity(frames));
    for i in 0..frames + 30 {
        let start = std::time::Instant::now();
        a.set_services(if busy { rows(i as u64) } else { Vec::new() });
        a.set_clock(i as f64 / 60.0);
        a.update(&backend);
        let mut buf = Buffer::empty(area);
        Paragraph::new(a.render(&backend)).render(area, &mut buf);
        let t = start.elapsed().as_secs_f64() * 1e3;
        // The first 30 frames settle the arcade; they are not counted.
        if i >= 30 {
            changed += prev.diff(&buf).len();
            times.push(t);
        }
        prev = buf;
    }
    times.sort_by(f64::total_cmp);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
    (changed as f64 / frames as f64, mean, p95)
}

#[test]
#[ignore = "a measurement; prints a table"]
fn backdrop_cost() {
    tc(|| {
        println!("| backdrop | signals | cells changed per frame | mean ms | p95 ms |");
        println!("|---|---|---|---|---|");
        for kind in [
            None,
            Some(BackdropKind::Scene),
            Some(BackdropKind::Sky),
            Some(BackdropKind::Clouds),
        ] {
            for busy in [false, true] {
                let (c, mean, p95) = measure(kind, busy, 200, 60, 300);
                println!(
                    "| {} | {} | {c:.0} | {mean:.2} | {p95:.2} |",
                    crate::animation::backdrop::choice_name(kind),
                    if busy { "busy" } else { "idle" }
                );
            }
        }
    })
}

/// The backdrop's own cost on a busy arcade frame at 200 x 60, best of 7
/// batches of 50 (the least disturbed by other load): drawing the canvas
/// afresh, and laying it (`layer`, with the sky's and the clouds' canvas
/// cached between steps, as between frames).
#[test]
#[ignore = "a measurement; prints a table"]
fn backdrop_layer_cost() {
    tc(|| {
        let backend = WithPcie(
            Fixed::new(2),
            Some(PcieBandwidth {
                rx_bytes_per_sec: 3.0e9,
                tx_bytes_per_sec: 1.0e9,
            }),
        );
        let (_, rows) = scene_rosters()
            .into_iter()
            .find(|(n, _)| *n == "aurora")
            .unwrap();
        let (w, h) = (200usize, 60usize);
        println!("| backdrop | canvas, ms | layer, ms |");
        println!("|---|---|---|");
        for kind in [
            Some(BackdropKind::Scene),
            Some(BackdropKind::Sky),
            Some(BackdropKind::Clouds),
        ] {
            let mut a = ArcadeVisualization::new(w, h);
            a.initialize_from_devices(backend.devices());
            a.initialize_topology(&backend);
            for i in 0..120u64 {
                a.set_services(rows(i));
                a.set_clock(i as f64 / 60.0);
                a.update(&backend);
            }
            a.set_backdrop(None);
            let lines = a.render(&backend);
            a.set_backdrop(kind);
            let b = a.backdrop().clone();
            let best = |f: &dyn Fn()| {
                (0..7)
                    .map(|_| {
                        let t = std::time::Instant::now();
                        for _ in 0..50 {
                            f();
                        }
                        t.elapsed().as_secs_f64() * 1e3 / 50.0
                    })
                    .fold(f64::INFINITY, f64::min)
            };
            let canvas = best(&|| {
                b.clear_cache();
                std::hint::black_box(b.canvas(w, h));
            });
            let layer = best(&|| {
                std::hint::black_box(b.layer(lines.clone(), w, h));
            });
            println!(
                "| {} | {canvas:.3} | {layer:.3} |",
                crate::animation::backdrop::choice_name(kind)
            );
        }
    })
}
