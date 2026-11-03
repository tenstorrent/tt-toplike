// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! What the arcade writes to the terminal, by panel: a measurement, run by
//! hand (`cargo test --release --lib --features tui arcade_traffic --
//! --ignored --nocapture`). It changes nothing in the arcade.
//!
//! It draws the default arcade layout over the mock backend for
//! [`FRAMES`] frames at 60 frames per second of arcade time (the backend
//! updates every sixth frame, 10 Hz, as in the TUI), through ratatui's
//! crossterm backend into a byte buffer. For each frame it counts the cells
//! ratatui's diff sends, by region (title, Row 1, each rule, each panel, the
//! dividers, the footer), and the bytes crossterm writes for them. It also
//! counts, in the starfield, the changed cells whose character stayed the
//! same and whose colours moved by at most [`SMALL_RGB`] per channel: the
//! cells a "skip small colour changes" filter would leave unsent.
//!
//! Compiled for tests only (`#[cfg(test)]` on the `mod` line).

use crate::animation::arcade_layout::{default_rows, layout, View};
use crate::animation::ArcadeVisualization;
use crate::backend::mock::MockBackend;
use crate::backend::TelemetryBackend;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use std::collections::BTreeMap;

/// Frames measured, after [`WARMUP`] frames that are drawn and not counted.
const FRAMES: usize = 600;
const WARMUP: usize = 60;
/// A colour change at most this large on every channel counts as small.
const SMALL_RGB: i32 = 8;

fn rgb(c: Color) -> Option<(i32, i32, i32)> {
    match c {
        Color::Rgb(r, g, b) => Some((r as i32, g as i32, b as i32)),
        _ => None,
    }
}

fn small_change(a: Color, b: Color) -> bool {
    match (rgb(a), rgb(b)) {
        (Some(x), Some(y)) => {
            (x.0 - y.0).abs() <= SMALL_RGB
                && (x.1 - y.1).abs() <= SMALL_RGB
                && (x.2 - y.2).abs() <= SMALL_RGB
        }
        _ => a == b,
    }
}

/// A byte sink shared with the terminal, so each frame's bytes can be read.
#[derive(Clone, Default)]
struct Sink(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The region a cell belongs to.
fn region(lay: &crate::animation::arcade_layout::Layout, x: usize, y: usize, h: usize) -> String {
    if y == 0 {
        return "title row".into();
    }
    if y == 1 {
        return "row 1 (telemetry)".into();
    }
    if y + 1 == h {
        return "footer / status".into();
    }
    for row in &lay.rows {
        if y == row.rule_y {
            return "rules".into();
        }
        for p in &row.panels {
            if p.rect.rows().contains(&y) && p.rect.cols().contains(&x) {
                return format!("{:?}", p.view).to_lowercase();
            }
        }
    }
    "dividers / blank".into()
}

/// Bytes written per frame and changed cells per frame by region.
fn measure(w: u16, h: u16) -> (f64, BTreeMap<String, f64>, f64, f64) {
    let mut backend = MockBackend::new(4);
    backend.init().unwrap();
    backend.update().unwrap();
    let mut arc = ArcadeVisualization::with_config(
        w as usize,
        h as usize,
        &crate::animation::arcade_layout::ArcadeConfig {
            rows: default_rows(),
        },
    );
    arc.initialize_from_devices(backend.devices());
    arc.initialize_topology(&backend);
    let lay = layout(&default_rows(), w as usize, h as usize);

    let sink = Sink::default();
    // A fixed viewport: the test has no terminal to ask for a size.
    let mut term = Terminal::with_options(
        CrosstermBackend::new(sink.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, w, h)),
        },
    )
    .unwrap();
    let mut prev: Option<Buffer> = None;
    let mut bytes = 0usize;
    let mut by_region: BTreeMap<String, usize> = BTreeMap::new();
    let (mut star_changed, mut star_small) = (0usize, 0usize);
    for frame in 0..WARMUP + FRAMES {
        if frame % 6 == 0 {
            backend.update().unwrap();
        }
        arc.update(&backend);
        sink.0.borrow_mut().clear();
        let done = term
            .draw(|f| super::ui_arcade(f, &arc, &backend))
            .unwrap()
            .buffer
            .clone();
        let written = sink.0.borrow().len();
        if let (Some(p), true) = (&prev, frame >= WARMUP) {
            bytes += written;
            for y in 0..h {
                for x in 0..w {
                    let (a, b) = (&p[(x, y)], &done[(x, y)]);
                    if a == b {
                        continue;
                    }
                    let r = region(&lay, x as usize, y as usize, h as usize);
                    if r == format!("{:?}", View::Starfield).to_lowercase() {
                        star_changed += 1;
                        if a.symbol() == b.symbol()
                            && small_change(a.fg, b.fg)
                            && small_change(a.bg, b.bg)
                        {
                            star_small += 1;
                        }
                    }
                    *by_region.entry(r).or_default() += 1;
                }
            }
        }
        prev = Some(done);
    }
    let n = FRAMES as f64;
    (
        bytes as f64 / n,
        by_region
            .into_iter()
            .map(|(k, v)| (k, v as f64 / n))
            .collect(),
        star_changed as f64 / n,
        star_small as f64 / n,
    )
}

#[test]
#[ignore = "a measurement for the fast-start report; run by hand"]
fn arcade_traffic_by_panel() {
    for (w, h) in [(140u16, 40u16), (200, 60)] {
        let (bytes, regions, star, small) =
            crate::ui::colors::with_truecolor(true, || measure(w, h));
        let total: f64 = regions.values().sum();
        println!(
            "== {w}x{h}: {bytes:.0} bytes/frame, {total:.0} cells changed/frame of {}",
            w as usize * h as usize
        );
        for (k, v) in &regions {
            println!("   {k:<20} {v:>7.1} cells/frame");
        }
        println!(
            "   starfield: {star:.1} changed, {small:.1} of them same glyph with every colour channel within {SMALL_RGB}"
        );
    }
}
