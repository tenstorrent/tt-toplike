// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Chip Portrait — character-art core-grid visualization for Blackhole and Wormhole.
//!
//! Each chip is rendered as an exact W×H character grid where every character is
//! exactly one terminal column wide. The render function guarantees that no output
//! line exceeds `portrait_cols(arch)` characters.

use crate::models::{Architecture, Device, SmbusTelemetry, Telemetry};
use crate::models::telemetry::tensix_col_harvested;

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

#[cfg(feature = "tui")]
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Map chip-level power fraction to a Tensix activity character.
fn tensix_char(activity: f32) -> char {
    if      activity >= 0.75 { '▓' }
    else if activity >= 0.40 { '▒' }
    else if activity >= 0.05 { '░' }
    else                     { '·' }
}

#[cfg(feature = "tui")]
/// Temperature-based color for DRAM cells.
fn gddr_color(temp_c: f32) -> Color {
    if      temp_c > 70.0 { Color::Red }
    else if temp_c > 50.0 { Color::Yellow }
    else                  { Color::Cyan }
}

/// Map chip column → 0-indexed Tensix column for Blackhole harvesting mask.
/// BH Tensix columns occupy chip cols 1-7 (→ tensix cols 0-6) and 9-15 (→ tensix cols 7-13).
/// Returns None for ETH (col 0, 16) and PCIe (col 8) columns.
fn bh_tensix_col_index(chip_col: usize) -> Option<usize> {
    match chip_col {
        1..=7  => Some(chip_col - 1),   // tensix cols 0-6
        9..=15 => Some(chip_col - 2),   // tensix cols 7-13
        _      => None,
    }
}

/// ETH port index for a BH cell (col 0 or 16, any row).
/// Col 0 → ports 0-11 (row = port), col 16 → ports 12-23.
fn bh_eth_port(col: usize, row: usize) -> usize {
    if col == 0 { row } else { 12 + row }
}

/// ETH port index for a WH cell (row 0 or 6, any col).
/// Row 0 → ports 0-9 (col = port), row 6 → ports 10-19.
fn wh_eth_port(col: usize, row: usize) -> usize {
    if row == 0 { col } else { 10 + col }
}

/// Build portrait rows as plain Strings (for testing and width validation).
///
/// # Width guarantee
/// Each returned String is exactly `portrait_dims(arch).0` Unicode characters wide.
/// Only single-column chars are used: ▓ ▒ ░ · D ● P (all width-1 per unicode-width crate).
/// The inner loop pushes exactly one char per column — no more, no less.
pub fn build_portrait_rows(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    _tick: u64,
) -> Vec<String> {
    let (cols, rows) = portrait_dims(device.architecture);

    // Chip-level activity: power fraction of 300W TDP
    let activity = (telemetry.power_w() / 300.0).min(1.0).max(0.0);

    let mut result = Vec::with_capacity(rows);

    for row in 0..rows {
        let mut line = String::with_capacity(cols);

        for col in 0..cols {
            let core_type = match device.architecture {
                Architecture::Blackhole => core_type_bh(col, row),
                _                       => core_type_wh(col, row),
            };

            let ch = match core_type {
                CoreType::Pcie => 'P',

                CoreType::Eth => {
                    let port = match device.architecture {
                        Architecture::Blackhole => bh_eth_port(col, row),
                        _                       => wh_eth_port(col, row),
                    };
                    let live = smbus
                        .and_then(|s| s.eth_live_status)
                        .map(|mask| (mask >> port) & 1 == 1)
                        .unwrap_or(false);
                    if live { '●' } else { '·' }
                }

                CoreType::Dram => 'D',

                CoreType::Tensix => {
                    // BH only: check harvesting mask via tensix column index
                    let harvested = if device.architecture == Architecture::Blackhole {
                        bh_tensix_col_index(col)
                            .and_then(|tc| {
                                smbus.and_then(|s| s.enabled_tensix_col)
                                    .map(|mask| tensix_col_harvested(mask, tc))
                            })
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    if harvested { '·' } else { tensix_char(activity) }
                }
            };

            line.push(ch);
        }

        // Width invariant: exactly `cols` terminal display columns pushed above.
        debug_assert_eq!(
            unicode_width::UnicodeWidthStr::width(line.as_str()), cols,
            "portrait row {} terminal width {} ≠ expected {}", row,
            unicode_width::UnicodeWidthStr::width(line.as_str()), cols
        );

        result.push(line);
    }

    result
}

#[cfg(feature = "tui")]
/// Build portrait rows as styled Ratatui Lines for actual rendering.
///
/// Color scheme:
/// - Tensix ▓: Cyan  ▒: Yellow  ░: DarkGray  ·(harvested): DarkGray+DIM
/// - DRAM D: Cyan/Yellow/Red by GDDR temperature
/// - ETH ●(live): Green  ·(down): DarkGray
/// - PCIe P: Blue
pub fn build_portrait_lines<'a>(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Vec<Line<'a>> {
    let rows_strs = build_portrait_rows(device, telemetry, smbus, tick);
    let (cols, _) = portrait_dims(device.architecture);

    // Per-column DRAM temperature color (BH: index into gddr_temps array)
    let dram_color_for_col = |col: usize| -> Color {
        let pair_idx = (col / 4).min(3);
        let temp = smbus
            .and_then(|s| s.gddr_temps.get(pair_idx).and_then(|p| p.as_ref()))
            .map(|p| p.0.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            .unwrap_or(0.0);
        gddr_color(temp)
    };

    rows_strs.into_iter().enumerate().map(|(row, row_str)| {
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(cols);

        for (col, ch) in row_str.chars().enumerate() {
            let core_type = match device.architecture {
                Architecture::Blackhole => core_type_bh(col, row),
                _                       => core_type_wh(col, row),
            };

            let style = match (core_type, ch) {
                (CoreType::Pcie, _) =>
                    Style::default().fg(Color::Blue),
                (CoreType::Eth, '●') =>
                    Style::default().fg(Color::Green),
                (CoreType::Eth, _) =>
                    Style::default().fg(Color::DarkGray),
                (CoreType::Dram, _) =>
                    Style::default().fg(dram_color_for_col(col)),
                (CoreType::Tensix, '·') =>
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                (CoreType::Tensix, '▓') =>
                    Style::default().fg(Color::Cyan),
                (CoreType::Tensix, '▒') =>
                    Style::default().fg(Color::Yellow),
                (CoreType::Tensix, _) =>
                    Style::default().fg(Color::DarkGray),
            };

            spans.push(Span::styled(ch.to_string(), style));
        }

        Line::from(spans)
    }).collect()
}

#[cfg(feature = "tui")]
/// Render a chip portrait into the given Rect.
///
/// Returns the sub-Rect actually used (`portrait_cols × portrait_rows`).
/// The caller must allocate at least `portrait_dims(arch)` space in the area.
pub fn render_chip_portrait(
    f: &mut Frame,
    area: Rect,
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Rect {
    let (cols, rows) = portrait_dims(device.architecture);
    let used = Rect {
        x: area.x,
        y: area.y,
        width:  (cols as u16).min(area.width),
        height: (rows as u16).min(area.height),
    };

    let lines = build_portrait_lines(device, telemetry, smbus, tick);
    let para = Paragraph::new(lines);
    f.render_widget(para, used);

    used
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

    use crate::models::{Device, SmbusTelemetry, Telemetry};
    use crate::models::telemetry::GddrTempPair;

    fn make_smbus(_arch: Architecture) -> SmbusTelemetry {
        let mut s = SmbusTelemetry::new();
        s.enabled_tensix_col = Some(0x3FFF); // no harvesting
        s.eth_live_status = Some(0xFFFFFFFFFFFFFFFF); // all live
        s.gddr_temps = [
            Some(GddrTempPair([38.0, 40.0, 42.0, 44.0])),
            Some(GddrTempPair([38.0, 40.0, 42.0, 44.0])),
            Some(GddrTempPair([38.0, 40.0, 42.0, 44.0])),
            Some(GddrTempPair([38.0, 40.0, 42.0, 44.0])),
        ];
        s
    }

    fn make_device(arch: Architecture) -> Device {
        let board = match arch {
            Architecture::Blackhole => "p150a",
            Architecture::Wormhole  => "n150",
            _                       => "e150",
        };
        Device::new(0, board.to_string(), "0000:01:00.0".to_string(), "(0,0)".to_string())
    }

    fn make_telemetry() -> Telemetry {
        Telemetry { power: Some(100.0), ..Telemetry::new() }
    }

    // ── Width contract ────────────────────────────────────────────────────────

    #[test]
    fn test_bh_portrait_row_width_exact() {
        use unicode_width::UnicodeWidthStr;
        let device  = make_device(Architecture::Blackhole);
        let smbus   = make_smbus(Architecture::Blackhole);
        let telem   = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        assert_eq!(rows.len(), 12, "BH must have 12 rows");
        for (i, row) in rows.iter().enumerate() {
            let w = UnicodeWidthStr::width(row.as_str());
            assert_eq!(w, 17, "BH row {} width is {} not 17: {:?}", i, w, row);
        }
    }

    #[test]
    fn test_wh_portrait_row_width_exact() {
        use unicode_width::UnicodeWidthStr;
        let device  = make_device(Architecture::Wormhole);
        let smbus   = make_smbus(Architecture::Wormhole);
        let telem   = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        assert_eq!(rows.len(), 12, "WH must have 12 rows");
        for (i, row) in rows.iter().enumerate() {
            let w = UnicodeWidthStr::width(row.as_str());
            assert_eq!(w, 10, "WH row {} width is {} not 10: {:?}", i, w, row);
        }
    }

    #[test]
    fn test_bh_portrait_no_overflow_no_smbus() {
        use unicode_width::UnicodeWidthStr;
        let device = make_device(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, None, 0);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(UnicodeWidthStr::width(row.as_str()), 17,
                "BH row {} overflowed without smbus", i);
        }
    }

    // ── Content correctness ───────────────────────────────────────────────────

    #[test]
    fn test_bh_eth_cols_show_eth_char() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // col 0 should be '●' (ETH live) in all rows
        for row in &rows {
            let chars: Vec<char> = row.chars().collect();
            assert!(chars[0] == '●' || chars[0] == '·',
                "BH col=0 should be ETH char, got '{}'", chars[0]);
        }
    }

    #[test]
    fn test_bh_dram_rows_show_d() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // row 0 col 1 (non-ETH, non-PCIe) should be 'D'
        let row0_chars: Vec<char> = rows[0].chars().collect();
        assert_eq!(row0_chars[1], 'D', "BH row=0 col=1 should be DRAM 'D'");
        let row11_chars: Vec<char> = rows[11].chars().collect();
        assert_eq!(row11_chars[1], 'D', "BH row=11 col=1 should be DRAM 'D'");
    }

    #[test]
    fn test_bh_pcie_shows_p() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // col 8, non-DRAM rows (1-10) should be 'P'
        for row_idx in 1..11 {
            let chars: Vec<char> = rows[row_idx].chars().collect();
            assert_eq!(chars[8], 'P', "BH PCIe col=8 row={} should be 'P'", row_idx);
        }
    }

    #[test]
    fn test_harvested_col_shows_dot() {
        let device = make_device(Architecture::Blackhole);
        let mut smbus = make_smbus(Architecture::Blackhole);
        smbus.enabled_tensix_col = Some(0x3FFE); // bit 0 clear → Tensix col 0 harvested
        // BH Tensix col 0 = chip col 1 (first non-ETH col)
        let telem = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // chip col 1, row 1 (Tensix area) should be '·' (harvested)
        let chars: Vec<char> = rows[1].chars().collect();
        assert_eq!(chars[1], '·', "harvested col should show '·', got '{}'", chars[1]);
    }

    #[test]
    fn test_eth_down_shows_dot() {
        let device = make_device(Architecture::Blackhole);
        let mut smbus = make_smbus(Architecture::Blackhole);
        smbus.eth_live_status = Some(0x0); // all ETH down
        let telem = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        let chars: Vec<char> = rows[0].chars().collect();
        assert_eq!(chars[0], '·', "ETH down should show '·'");
    }
}
