//! Ledger ruling R10: the saga core's Blackhole floor has the shape Die 3D
//! draws.
//!
//! The saga crate cannot depend on this crate, so it copies the Blackhole
//! numbers (see the module comment of `tt_toplike_saga::topology`). This
//! test computes each number from Die 3D's own tables and constants at test
//! time and compares it with the floor `FloorSpec::for_chip` gives, so a
//! change on either side fails here. It is a child of `scene` so it reads
//! the scene's private constants directly.

use super::{gddr_channel, BOARD_BAND, BOARD_COLS, ETH_CAGES, GDDR_ROWS, MARGIN_X};
use crate::animation::die3d::layout::{tensix_mask_to_die_cols, DieLayout};
use tt_toplike_bus::EntityKey;
use tt_toplike_saga::topology::{Arch, FloorSpec};

/// The saga's floor for one Blackhole chip with the given readings.
fn floor(tensix_cols: Option<u64>, eth_live_mask: Option<u64>) -> FloorSpec {
    FloorSpec::for_chip(
        &EntityKey::chip("0000:01:00.0"),
        None,
        tensix_cols,
        eth_live_mask,
        Arch::Blackhole,
    )
}

/// The cages Die 3D lights for `mask`: cage `gx` (1 to `ETH_CAGES`) is lit
/// when bit `gx - 1` is set, as `scene` draws them.
fn lit_cages(mask: u64) -> usize {
    (1..=ETH_CAGES)
        .filter(|gx| (mask >> (gx - 1)) & 1 == 1)
        .count()
}

#[test]
fn the_saga_grid_is_the_die3d_tile_grid() {
    let die = DieLayout::blackhole();
    let f = floor(None, None);
    assert_eq!(
        (usize::from(f.cols), usize::from(f.rows)),
        (die.cols, die.rows),
        "the saga floor is the BH_TILES grid"
    );
    // The board grid adds the GDDR margins on each side and one cage row
    // and one connector row to the same die.
    assert_eq!(usize::from(f.cols), BOARD_COLS - 2 * MARGIN_X);
    assert_eq!(usize::from(f.rows), BOARD_BAND - 2);
}

#[test]
fn the_saga_vaults_are_the_die3d_gddr_split() {
    let rows = GDDR_ROWS.len();
    let west: Vec<usize> = (0..rows).map(|r| gddr_channel(false, r)).collect();
    let east: Vec<usize> = (0..rows).map(|r| gddr_channel(true, r)).collect();
    // Every channel is drawn once, on one side.
    let mut all = [west.clone(), east.clone()].concat();
    all.sort_unstable();
    assert_eq!(all, (0..2 * rows).collect::<Vec<_>>());
    let f = floor(None, None);
    assert_eq!(usize::from(f.west_vaults), west.len());
    assert_eq!(usize::from(f.east_vaults), east.len());
    // The saga's docs put channels 0 to 3 in the west: so does Die 3D.
    assert!(west.iter().all(|&c| c < rows), "{west:?}");
}

#[test]
fn the_saga_portals_are_the_lit_die3d_cages() {
    // No reading: the saga opens every cage Die 3D can draw.
    assert_eq!(usize::from(floor(None, None).portals), ETH_CAGES);
    for mask in [
        0,
        1,
        0x5,
        0xfff,
        0x1000,
        0xaaaa_aaaa_aaaa_aaaa,
        0x0f0f,
        u64::MAX,
    ] {
        assert_eq!(
            usize::from(floor(None, Some(mask)).portals),
            lit_cages(mask),
            "eth mask {mask:#x}"
        );
    }
}

#[test]
fn the_saga_dead_columns_are_the_die3d_harvested_columns() {
    assert!(floor(None, None).dead_cols.is_empty());
    for mask in [0x3fffu32, 0x3edf, 0x3fdd, 0, 0x2aaa, 0x1555, 0x3ffe, 0x2000] {
        assert_eq!(
            floor(Some(u64::from(mask)), None).dead_cols,
            tensix_mask_to_die_cols(mask),
            "tensix mask {mask:#x}"
        );
    }
}
