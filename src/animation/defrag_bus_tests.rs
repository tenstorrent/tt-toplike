// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Defrag frames with and without the signal bus.
//!
//! The golden hashes pin the frames Defrag drew before it read the bus.
//! They were taken by running `golden_hashes` against the commit before the
//! bus port (the commit that added this file holds only these tests), so a
//! frame drawn from an empty world must hash to exactly these values.

use super::*;
use crate::animation::bus_test_support::{frame_hash, tc, Fixed};

/// The layouts the golden hashes pin: (chips, width, height).
const LAYOUTS: [(usize, usize, usize); 3] = [(1, 60, 18), (2, 60, 18), (2, 160, 50)];
/// The updates after which a frame is hashed. Chip 0 loads for a while,
/// so the later frames show its fill grown.
const HASHED_AT: [u32; 4] = [1, 10, 60, 400];

/// Hashes of the frames of a Defrag view in `layout` over [`Fixed`], after
/// each update in [`HASHED_AT`]. `before_update` runs before every update.
pub(super) fn hashes_with(
    (n, w, h): (usize, usize, usize),
    mut before_update: impl FnMut(&mut DefragVis, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(n);
        let mut d = DefragVis::new(w, h);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut d, &backend, i);
            d.update(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&d.render(&backend)));
            }
        }
        out
    })
}

/// The frames as Defrag drew them before the bus port, for each of
/// [`LAYOUTS`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 4]; 3] = [
    [
        6378651105407557510,
        1473458929850560376,
        2672466645569721241,
        6670050726542021795,
    ],
    [
        17050773165888384831,
        11425570284173348045,
        5660999393278833181,
        3652699994344175700,
    ],
    [
        14788702879550107618,
        6915677417392135091,
        15995812354245785336,
        16781590828447983459,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui defrag_golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn defrag_golden_hashes() {
    for l in LAYOUTS {
        println!("{l:?}: {:?}", hashes_with(l, |_, _, _| {}));
    }
}

#[test]
fn the_defrag_frames_with_no_bus_are_the_pinned_frames() {
    for (i, l) in LAYOUTS.into_iter().enumerate() {
        assert_eq!(hashes_with(l, |_, _, _| {}), GOLDEN[i].to_vec(), "{l:?}");
    }
}
