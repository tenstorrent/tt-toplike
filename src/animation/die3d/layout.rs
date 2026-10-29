// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Blackhole die layout: which kind of tile sits at each die coordinate, and
//! the NoC0 coordinate it answers to.
//!
//! The NoC is a folded torus. Tiles are placed on the die in an interleaved
//! order so the torus wrap-around wires stay physically short; die column `d`
//! therefore has NoC0 index `fold(d, n)`. The tile table itself is generated
//! from hardware by `scripts/gen_die_layout.py` (see `bh_layout_data.rs`).

use super::bh_layout_data::BH_TILES;
use std::sync::OnceLock;

/// What a tile does. `Mgmt` groups ARC, PCIe, L2CPU and security tiles, which
/// the view draws the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileKind {
    Tensix,
    Dram,
    Eth,
    Mgmt,
}

impl TileKind {
    /// Decode the kind code written by `scripts/gen_die_layout.py`.
    fn from_code(code: u8) -> TileKind {
        match code {
            0 => TileKind::Tensix,
            1 => TileKind::Dram,
            2 => TileKind::Eth,
            _ => TileKind::Mgmt,
        }
    }
}

/// One tile: its physical (die) position, its NoC0 position, and its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    pub die: (u8, u8),
    pub noc0: (u8, u8),
    pub kind: TileKind,
}

/// The full tile grid of one die.
#[derive(Debug)]
pub struct DieLayout {
    pub cols: usize,
    pub rows: usize,
    pub tiles: Vec<Tile>,
}

impl DieLayout {
    /// The Blackhole layout, built once from the generated `BH_TILES` table.
    /// The table is the union over several chips, so harvested tiles that are
    /// missing on one chip are still present here.
    pub fn blackhole() -> &'static DieLayout {
        static LAYOUT: OnceLock<DieLayout> = OnceLock::new();
        LAYOUT.get_or_init(|| {
            let tiles: Vec<Tile> = BH_TILES
                .iter()
                .map(|&(dx, dy, nx, ny, k)| Tile {
                    die: (dx, dy),
                    noc0: (nx, ny),
                    kind: TileKind::from_code(k),
                })
                .collect();
            let cols = tiles.iter().map(|t| t.die.0 as usize + 1).max().unwrap_or(0);
            let rows = tiles.iter().map(|t| t.die.1 as usize + 1).max().unwrap_or(0);
            DieLayout { cols, rows, tiles }
        })
    }

    /// The tile at a die coordinate, if the table has one there.
    pub fn tile_at(&self, die: (u8, u8)) -> Option<&Tile> {
        self.tiles.iter().find(|t| t.die == die)
    }
}

/// NoC0 index of die index `d` on an axis of `n` tiles.
///
/// The die order interleaves the two ends of the torus: 0, 1, n-1, 2, n-2, ...
/// so die 0 is noc0 0, odd `d` is `(d + 1) / 2`, even `d > 0` is `n - d / 2`.
pub fn fold(d: usize, n: usize) -> usize {
    if d == 0 {
        0
    } else if d % 2 == 1 {
        (d + 1) / 2
    } else {
        n - d / 2
    }
}

/// Hop count between two NoC0 coordinates on a `cols` x `rows` torus
/// (toroidal Manhattan distance).
pub fn torus_hops(a: (u8, u8), b: (u8, u8), cols: usize, rows: usize) -> usize {
    let dx = (a.0 as isize - b.0 as isize).unsigned_abs();
    let dy = (a.1 as isize - b.1 as isize).unsigned_abs();
    dx.min(cols - dx) + dy.min(rows - dy)
}

/// Die columns harvested for an `enabled_tensix_col` mask, ascending.
///
/// Rule: die column = bit index + 1 for each CLEAR bit among the low 14 bits
/// (Tensix lives in die columns 1..=14). Measured on two chips only: mask
/// 0x3edf gave missing die columns [6, 9] and 0x3fdd gave [2, 6], which
/// exercises bits 1, 5 and 8. The other bits follow by assumption.
pub fn tensix_mask_to_die_cols(mask: u32) -> Vec<u8> {
    (0..14u8).filter(|bit| mask & (1 << bit) == 0).map(|bit| bit + 1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_matches_the_interleave_for_17_columns() {
        let want = [0, 1, 16, 2, 15, 3, 14, 4, 13, 5, 12, 6, 11, 7, 10, 8, 9];
        for d in 0..17 {
            assert_eq!(fold(d, 17), want[d], "die column {d}");
        }
    }

    #[test]
    fn torus_hops_wraps_around() {
        assert_eq!(torus_hops((0, 0), (16, 0), 17, 12), 1);
    }

    #[test]
    fn blackhole_layout_is_17_by_12() {
        let l = DieLayout::blackhole();
        assert_eq!((l.cols, l.rows), (17, 12));
    }

    #[test]
    fn blackhole_layout_has_24_dram_and_13_eth_tiles() {
        // One chip has 12 Ethernet tiles; the two measured chips each lack a
        // different one, so their union holds 13. Die column 9 had none on
        // either chip and is not filled.
        let l = DieLayout::blackhole();
        let n = |k| l.tiles.iter().filter(|t| t.kind == k).count();
        assert_eq!(n(TileKind::Dram), 24);
        assert_eq!(n(TileKind::Eth), 13);
    }

    #[test]
    fn die_coordinates_are_unique() {
        let l = DieLayout::blackhole();
        let mut seen = std::collections::HashSet::new();
        for t in &l.tiles {
            assert!(seen.insert(t.die), "duplicate die coordinate {:?}", t.die);
        }
    }

    #[test]
    fn every_tile_noc0_equals_fold_of_its_die_coordinate() {
        let l = DieLayout::blackhole();
        for t in &l.tiles {
            assert_eq!(t.noc0.0 as usize, fold(t.die.0 as usize, l.cols), "x of {t:?}");
            assert_eq!(t.noc0.1 as usize, fold(t.die.1 as usize, l.rows), "y of {t:?}");
        }
    }

    #[test]
    fn harvest_mask_matches_the_missing_die_columns() {
        // Measured on board A (chips 0 and 1).
        assert_eq!(tensix_mask_to_die_cols(0x3edf), vec![6, 9]);
        assert_eq!(tensix_mask_to_die_cols(0x3fdd), vec![2, 6]);
    }

    #[test]
    fn harvest_hole_columns_are_filled_with_tensix() {
        // Die column 6 was harvested on both measured chips; the generator
        // fills it from its neighbours. It must have Tensix wherever column 5
        // and column 7 both do.
        let l = DieLayout::blackhole();
        let is_tensix = |x: u8, y: u8| l.tile_at((x, y)).map_or(false, |t| t.kind == TileKind::Tensix);
        let mut checked = 0;
        for y in 0..l.rows as u8 {
            if is_tensix(5, y) && is_tensix(7, y) {
                assert!(is_tensix(6, y), "no Tensix at die (6, {y})");
                checked += 1;
            }
        }
        assert!(checked > 0);
    }
}
