// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Memory Castle frames with and without the signal bus.
//!
//! The golden hashes pin the frames the castle drew before it read the bus.
//! They were taken by running `golden_hashes` against the commit before the
//! bus port (the commit that added this file holds only these tests), so a
//! frame drawn from an empty world must hash to exactly these values.

use super::*;
use crate::animation::bus_test_support::{frame_hash, tc, Fixed};

/// The layouts the golden hashes pin: (chips, width, height). One chip; two
/// chips in the full, compact and fleet-grid tiers.
const LAYOUTS: [(usize, usize, usize); 4] = [(1, 100, 30), (2, 120, 40), (2, 30, 30), (2, 14, 20)];
/// The updates after which a frame is hashed.
const HASHED_AT: [u32; 3] = [1, 10, 30];

/// Hashes of the frames of a castle in `layout` over [`Fixed`], after each
/// update in [`HASHED_AT`]. `before_update` runs before every update.
pub(super) fn hashes_with(
    (n, w, h): (usize, usize, usize),
    mut before_update: impl FnMut(&mut MemoryCastle, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(n);
        let mut c = MemoryCastle::new(w, h);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut c, &backend, i);
            c.update(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&c.render(&backend)));
            }
        }
        out
    })
}

/// The frames as the castle drew them before the bus port, for each of
/// [`LAYOUTS`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 3]; 4] = [
    [
        3227054172396918020,
        7077085263160370286,
        6484174506412191991,
    ],
    [
        630315234471104202,
        10500481099453121099,
        6438625630473264859,
    ],
    [
        15622758398212693181,
        306673472684778459,
        1072057634338145074,
    ],
    [
        18019069934949340965,
        18019069934949340965,
        18019069934949340965,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui castle_golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn castle_golden_hashes() {
    for l in LAYOUTS {
        println!("{l:?}: {:?}", hashes_with(l, |_, _, _| {}));
    }
}

#[test]
fn the_castle_frames_with_no_bus_are_the_pinned_frames() {
    for (i, l) in LAYOUTS.into_iter().enumerate() {
        assert_eq!(hashes_with(l, |_, _, _| {}), GOLDEN[i].to_vec(), "{l:?}");
    }
}
