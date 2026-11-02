// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! MemoryFlow frames with and without the signal bus.
//!
//! The golden hashes pin the frames MemoryFlow drew before it read the bus.
//! They were taken by running `golden_hashes` against the commit that added
//! this file, which holds only these tests, so a frame drawn from an empty
//! world must hash to exactly these values.

use crate::animation::starfield_bus::tests::{frame_hash, tc, Fixed, HASHED_AT, SIZES};
use crate::animation::MemoryFlowVis;

/// Hashes of the frames of a MemoryFlow view at `w` x `h` over two
/// [`Fixed`] chips, after each update in [`HASHED_AT`]. `before_update`
/// runs before every update (the bus tests hand the view a world there).
pub(super) fn hashes_with(
    w: usize,
    h: usize,
    mut before_update: impl FnMut(&mut MemoryFlowVis, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(2);
        let mut mf = MemoryFlowVis::new(w, h);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut mf, &backend, i);
            mf.update(&backend);
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&mf.render(&backend)));
            }
        }
        out
    })
}

/// The frames as MemoryFlow drew them before the bus port, at each of
/// [`SIZES`]. Taken from the commit that added this file.
const GOLDEN: [[u64; 3]; 2] = [
    [
        16119804873617930179,
        2949916930769929029,
        9339828239280807536,
    ],
    [
        9868299539168236138,
        16623803713248559811,
        10993199208549131537,
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui memory_flow_bus::tests::golden_hashes -- --ignored --nocapture`.
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
