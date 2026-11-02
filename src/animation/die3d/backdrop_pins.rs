// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pinned Die 3D frames with its clouds on (the default), idle and busy, at
//! several sizes, taken before the shared backdrop existed (`what-if` at
//! d6560a27). The shared backdrop must leave them byte-identical.
//! `die3d_backdrop_pin_hashes` prints the table.

use super::test_backend::Fake;
use super::*;
use crate::animation::bus_test_support::{frame_hash, tc};
use crate::backend::pcie_counters::PcieBandwidth;

/// Two dies of one board, idle or busy (power near the TDP and gigabytes a
/// second over PCIe, so the clouds are bright and fast).
fn board(busy: bool) -> Fake {
    let mut b = Fake::boards(&["p300"; 2], Some("100"));
    if busy {
        b.set_power(0, 95.0);
        b.set_power(1, 80.0);
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 3.0e9,
            tx_bytes_per_sec: 1.0e9,
        });
    } else {
        b.set_power(0, 20.0);
        b.set_power(1, 20.0);
    }
    b
}

/// View times, in seconds, at which a frame is hashed.
const AT: [u32; 3] = [60, 180, 360];

/// The frames of a Die 3D view at `w` x `h`, updated 60 times a second.
/// `setup` runs once after construction.
pub(crate) fn die3d_hashes_with(
    busy: bool,
    w: usize,
    h: usize,
    setup: impl FnOnce(&mut Die3dView),
) -> Vec<u64> {
    let fake = board(busy);
    let mut v = Die3dView::new(w, h);
    setup(&mut v);
    let mut out = Vec::new();
    for i in 0..=*AT.last().unwrap() {
        let t = i as f64 / 60.0;
        v.update(&fake, None, t);
        if AT.contains(&i) {
            out.push(frame_hash(&v.render(None, t)));
        }
    }
    out
}

const SIZES: [(usize, usize); 3] = [(80, 24), (120, 40), (200, 60)];

fn all_hashes() -> Vec<(String, Vec<u64>)> {
    tc(|| {
        let mut out = Vec::new();
        for busy in [false, true] {
            for (w, h) in SIZES {
                out.push((
                    format!("die3d busy {busy} {w}x{h}"),
                    die3d_hashes_with(busy, w, h, |_| {}),
                ));
            }
        }
        out
    })
}

/// Prints the table for [`PINS`]. Run with
/// `cargo test --locked --lib --features tui die3d_backdrop_pin_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn die3d_backdrop_pin_hashes() {
    for (name, h) in all_hashes() {
        println!("    ({name:?}, {h:?}),");
    }
}

#[test]
fn the_die3d_clouds_draw_their_pinned_frames() {
    let got = all_hashes();
    let mut bad = Vec::new();
    for (name, h) in &got {
        match PINS.iter().find(|(n, _)| n == name) {
            Some((_, want)) if want.as_slice() == h.as_slice() => {}
            Some((_, want)) => bad.push(format!("{name}: {h:?}, pinned {want:?}")),
            None => bad.push(format!("{name}: not pinned")),
        }
    }
    assert_eq!(got.len(), PINS.len(), "pins added or lost");
    assert!(bad.is_empty(), "frames changed:\n{}", bad.join("\n"));
}

/// The frames at d6560a27, before the shared backdrop.
const PINS: &[(&str, [u64; 3])] = &[
    (
        "die3d busy false 80x24",
        [
            6154786996226261436,
            13209881315875946633,
            8864526934766754834,
        ],
    ),
    (
        "die3d busy false 120x40",
        [
            11017125499313513356,
            1948922558097837657,
            4271645045449597719,
        ],
    ),
    (
        "die3d busy false 200x60",
        [
            1529569435153072444,
            2610336210384477905,
            10158580177772670879,
        ],
    ),
    (
        "die3d busy true 80x24",
        [4244172633710885458, 9306680801247810329, 260760363979418375],
    ),
    (
        "die3d busy true 120x40",
        [
            16092328997930957063,
            9549522429039058630,
            4183203228796688840,
        ],
    ),
    (
        "die3d busy true 200x60",
        [2223325318754879055, 7534626186001702092, 298940819006023881],
    ),
];

/// The pins would catch a change to the clouds: without them every pinned
/// frame is different at 120 x 40 and 200 x 60. (At 80 x 24 no cloud pixel
/// shows in these frames, so that size pins the rest of the view only.)
#[test]
fn the_pins_see_the_clouds() {
    tc(|| {
        for busy in [false, true] {
            for (w, h) in SIZES.into_iter().filter(|(w, _)| *w >= 120) {
                let off =
                    die3d_hashes_with(busy, w, h, |v| v.handle_key(Die3dKey::ToggleClouds, 0.0));
                let name = format!("die3d busy {busy} {w}x{h}");
                let pinned = PINS.iter().find(|(n, _)| *n == name).unwrap().1;
                for (a, b) in off.iter().zip(pinned) {
                    assert_ne!(*a, b, "{name}");
                }
            }
        }
    })
}
