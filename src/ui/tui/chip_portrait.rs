// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Chip Portrait — character-art core-grid visualization for Blackhole and Wormhole.
//!
//! Each chip is rendered as an exact W×H character grid where every character is
//! exactly one terminal column wide. The render function guarantees that no output
//! line exceeds `portrait_cols(arch)` characters.

use crate::models::Architecture;

/// Full chip grid dimensions (cols × rows) for each architecture.
/// These are the COMPLETE chip grids including ETH, DRAM, and PCIe cells.
/// Source: tensix-viz chip.js coreType() — authoritative.
pub fn portrait_dims(arch: Architecture) -> (usize, usize) {
    match arch {
        Architecture::Blackhole => (17, 12),
        Architecture::Wormhole  => (10, 12),
        Architecture::Grayskull => (10, 12), // rendered as all-Tensix
        Architecture::Unknown   => (10, 12),
    }
}

/// Classification of a single core cell in the chip grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoreType {
    Tensix,
    Dram,
    Eth,
    Pcie,
}

/// Return the CoreType for cell (col, row) on a Blackhole chip.
/// BH: 17 cols × 12 rows. ETH at col 0 or 16. PCIe at col 8.
/// DRAM at row 0 or 11. Tensix everywhere else.
pub fn core_type_bh(col: usize, row: usize) -> CoreType {
    if col == 0 || col == 16    { CoreType::Eth }
    else if row == 0 || row == 11 { CoreType::Dram }
    else if col == 8            { CoreType::Pcie }
    else                        { CoreType::Tensix }
}

/// Return the CoreType for cell (col, row) on a Wormhole chip.
/// WH: 10 cols × 12 rows. ETH at row 0 or 6. DRAM at col 0 or 5.
/// Tensix everywhere else (no PCIe special cell).
pub fn core_type_wh(col: usize, row: usize) -> CoreType {
    if row == 0 || row == 6  { CoreType::Eth }
    else if col == 0 || col == 5 { CoreType::Dram }
    else                     { CoreType::Tensix }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── portrait_dims ────────────────────────────────────────────────────────

    #[test]
    fn test_bh_portrait_dims() {
        assert_eq!(portrait_dims(Architecture::Blackhole), (17, 12));
    }

    #[test]
    fn test_wh_portrait_dims() {
        assert_eq!(portrait_dims(Architecture::Wormhole), (10, 12));
    }

    // ── BH core type exhaustive ───────────────────────────────────────────────

    #[test]
    fn test_bh_eth_cols() {
        for row in 0..12 {
            assert_eq!(core_type_bh(0,  row), CoreType::Eth,  "BH col=0  row={}", row);
            assert_eq!(core_type_bh(16, row), CoreType::Eth,  "BH col=16 row={}", row);
        }
    }

    #[test]
    fn test_bh_dram_rows() {
        // DRAM rows only apply to non-ETH columns
        for col in 1..16_usize {
            if col == 8 { continue; } // PCIe col
            assert_eq!(core_type_bh(col, 0),  CoreType::Dram, "BH col={} row=0",  col);
            assert_eq!(core_type_bh(col, 11), CoreType::Dram, "BH col={} row=11", col);
        }
    }

    #[test]
    fn test_bh_pcie_col() {
        for row in 1..11 {
            assert_eq!(core_type_bh(8, row), CoreType::Pcie, "BH PCIe row={}", row);
        }
    }

    #[test]
    fn test_bh_tensix_interior() {
        // Sample interior cells that are definitely Tensix
        for &(col, row) in &[(1,1), (4,5), (15,10), (7,6), (9,3)] {
            assert_eq!(core_type_bh(col, row), CoreType::Tensix,
                "expected Tensix at BH ({},{})", col, row);
        }
    }

    #[test]
    fn test_bh_dram_row_takes_precedence_over_pcie() {
        // col=8, row=0 → ETH col wins over DRAM, but col 8 is interior; row 0 should be DRAM
        // col=0 row=0 → ETH wins (ETH check first)
        assert_eq!(core_type_bh(0, 0),   CoreType::Eth);  // ETH col
        assert_eq!(core_type_bh(8, 0),   CoreType::Dram); // PCIe col but row 0 → DRAM
        assert_eq!(core_type_bh(8, 11),  CoreType::Dram); // same
        assert_eq!(core_type_bh(8, 1),   CoreType::Pcie); // PCIe non-DRAM row
    }

    // ── WH core type exhaustive ───────────────────────────────────────────────

    #[test]
    fn test_wh_eth_rows() {
        for col in 0..10 {
            assert_eq!(core_type_wh(col, 0), CoreType::Eth, "WH col={} row=0", col);
            assert_eq!(core_type_wh(col, 6), CoreType::Eth, "WH col={} row=6", col);
        }
    }

    #[test]
    fn test_wh_dram_cols() {
        // DRAM only for non-ETH rows
        for row in [1,2,3,4,5,7,8,9,10,11] {
            assert_eq!(core_type_wh(0, row), CoreType::Dram, "WH DRAM col=0 row={}", row);
            assert_eq!(core_type_wh(5, row), CoreType::Dram, "WH DRAM col=5 row={}", row);
        }
    }

    #[test]
    fn test_wh_tensix_interior() {
        for &(col, row) in &[(1,1), (3,5), (9,11), (6,8), (4,4)] {
            assert_eq!(core_type_wh(col, row), CoreType::Tensix,
                "expected Tensix at WH ({},{})", col, row);
        }
    }

    #[test]
    fn test_wh_eth_row_wins_over_dram_col() {
        // col=0 row=0 → ETH row wins (ETH check is first in core_type_wh)
        assert_eq!(core_type_wh(0, 0), CoreType::Eth);
        assert_eq!(core_type_wh(5, 6), CoreType::Eth);
    }
}
