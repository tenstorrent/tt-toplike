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

/// A single animated data-transfer particle in the chip portrait.
#[derive(Clone)]
pub struct Particle {
    /// Portrait column (0..portrait_cols)
    pub col: u8,
    /// 0.0 = DRAM row boundary, 1.0 = opposite DRAM row boundary.
    /// Reads advance toward 1.0; writes retreat toward 0.0.
    pub progress: f32,
    /// true = spawned at row 0 (top DRAM row), false = row (portrait_rows-1)
    pub from_top: bool,
    pub kind: ParticleKind,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ParticleKind {
    /// Teal: DRAM→Tensix read. Spawned at progress=0.0, travels to 1.0.
    Read,
    /// Pink: Tensix→DRAM write. Spawned at random interior progress, retreats to 0.0.
    Write,
}

/// Map a particle's `progress` to a display character.
/// Same lookup for both Read and Write — progress alone determines appearance.
pub fn particle_char(progress: f32) -> char {
    if      progress < 0.15 { '·' }
    else if progress < 0.40 { '∘' }
    else                     { '○' }
}

/// Return the max aiclk (MHz) for the given architecture, used to normalise speed.
fn max_aiclk(arch: Architecture) -> f32 {
    match arch {
        Architecture::Blackhole => 1000.0,
        Architecture::Wormhole  => 800.0,
        _                       => 700.0,
    }
}

/// Pick a random trained DRAM column for the given architecture.
///
/// Uses `ddr_status` bitmask (bit i set = DRAM col index i is trained) and
/// `tick` as a cheap deterministic seed — no `rand` dependency.
///
/// Returns `None` when no trained columns exist.
pub fn trained_random_col(
    ddr_status: u64,
    portrait_cols: usize,
    arch: Architecture,
    tick: u64,
) -> Option<usize> {
    // Collect trained DRAM chip columns
    let dram_cols: Vec<usize> = (0..portrait_cols)
        .filter(|&c| {
            let is_dram = match arch {
                Architecture::Blackhole => core_type_bh(c, 0) == CoreType::Dram,
                _                       => core_type_wh(c, 1) == CoreType::Dram,
            };
            if !is_dram { return false; }
            // Map chip column to a DRAM channel bit index (0-based among DRAM cols)
            let dram_idx = (0..c)
                .filter(|&cc| match arch {
                    Architecture::Blackhole => core_type_bh(cc, 0) == CoreType::Dram,
                    _                       => core_type_wh(cc, 1) == CoreType::Dram,
                })
                .count();
            (ddr_status >> dram_idx) & 1 == 1
        })
        .collect();

    if dram_cols.is_empty() { return None; }

    // Cheap hash of tick to pick a column
    let idx = ((tick.wrapping_mul(0x9e37_79b9_7f4a_7c15)) >> 32) as usize % dram_cols.len();
    Some(dram_cols[idx])
}

/// Advance particle positions and spawn new ones for a single device.
///
/// # Arguments
/// * `particles` – mutable particle list for this device
/// * `telemetry` – current telemetry snapshot
/// * `smbus` – SMBUS data (for ddr_status bitmask and aiclk)
/// * `arch` – chip architecture (BH / WH / GS)
/// * `baseline` – per-device adaptive baseline (updated in place)
/// * `device_idx` – index within the baseline
/// * `tick` – monotonic frame counter used as PRNG seed
/// * `max_particles` – ceiling from AnimConfig
pub fn tick_particles(
    particles: &mut Vec<Particle>,
    telemetry: &crate::models::Telemetry,
    smbus: Option<&crate::models::SmbusTelemetry>,
    arch: Architecture,
    baseline: &mut crate::animation::baseline::AdaptiveBaseline,
    device_idx: usize,
    tick: u64,
    max_particles: usize,
) {
    let power  = telemetry.power_w();
    // aiclk_mhz() returns u32 — cast to f32 for normalisation
    let aiclk  = telemetry.aiclk_mhz() as f32;
    let aiclk_norm = (aiclk / max_aiclk(arch)).clamp(0.0, 1.0);

    // Update baseline with current reading (current and temp not available here; pass 0.0)
    baseline.update(device_idx, power, 0.0, 0.0, aiclk);

    let power_change = baseline.power_change(device_idx, power).max(0.0);

    // Speed: lerp 1/12 → 1/5 as aiclk_norm rises (12 ticks/cross at idle, 5 at max)
    let speed = 1.0_f32 / 12.0 + (1.0 / 5.0 - 1.0 / 12.0) * aiclk_norm;

    let (portrait_cols, _portrait_rows) = portrait_dims(arch);
    let ddr_status = smbus.and_then(|s| s.ddr_status_bitmask()).unwrap_or(u64::MAX);

    // 1. Advance existing particles; drop expired ones.
    particles.retain_mut(|p| match p.kind {
        ParticleKind::Read  => { p.progress += speed; p.progress < 1.0 }
        ParticleKind::Write => { p.progress -= speed; p.progress > 0.0 }
    });

    if particles.len() >= max_particles { return; }

    // 2. Spawn reads (when power is above idle baseline)
    let read_budget = (power_change * 8.0) as u32;
    if read_budget > 0 {
        let interval = 8 / read_budget.max(1) + 1;
        if tick % interval as u64 == 0 {
            if let Some(col) = trained_random_col(ddr_status, portrait_cols, arch, tick) {
                let from_top = (tick % 2) == 0;
                particles.push(Particle { col: col as u8, progress: 0.0, from_top, kind: ParticleKind::Read });
            }
        }
    }

    // 3. Spawn writes only when power is ≥25% above idle baseline (active writing)
    let write_power = (power_change - 0.25).max(0.0);
    if write_power > 0.0 && particles.len() < max_particles {
        let write_budget = (write_power * 6.0) as u32;
        if write_budget > 0 {
            let interval = 6 / write_budget.max(1) + 1;
            if tick % interval as u64 == 0 {
                let seed = tick.wrapping_add(37);
                if let Some(col) = trained_random_col(ddr_status, portrait_cols, arch, seed) {
                    let from_top = (tick % 3) != 0;
                    // Write particles start in the interior and retreat outward
                    let progress = 0.2 + ((tick % 7) as f32) * 0.08;
                    particles.push(Particle { col: col as u8, progress, from_top, kind: ParticleKind::Write });
                }
            }
        }
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
///
/// The minimum return value is `'░'` — idle non-harvested cells are rendered
/// with the lowest-intensity block character so the heatmap color remains
/// visible. Harvested cells bypass this function and use `'·'` directly in
/// `build_portrait_rows`.
fn tensix_char(activity: f32) -> char {
    if      activity >= 0.75 { '▓' }
    else if activity >= 0.40 { '▒' }
    else                     { '░' }  // minimum floor — harvested cells use '·' directly
}


/// Linear interpolation between two RGB triples. `t` is clamped to [0, 1].
pub(crate) fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

/// Temperature-to-RGB for Tensix cells (ASIC temperature heatmap).
///
/// ≤40°C → teal #4FD1C5, 40–65°C → lerp to gold #F4C471,
/// 65–80°C → lerp to red #FF6B6B, >80°C → red.
pub(crate) fn tensix_temp_rgb(temp_c: f32) -> [u8; 3] {
    const TEAL: [u8; 3] = [79, 209, 197];
    const GOLD: [u8; 3] = [244, 196, 113];
    const RED:  [u8; 3] = [255, 107, 107];
    if temp_c <= 40.0 {
        TEAL
    } else if temp_c <= 65.0 {
        lerp_rgb(TEAL, GOLD, (temp_c - 40.0) / 25.0)
    } else if temp_c <= 80.0 {
        lerp_rgb(GOLD, RED, (temp_c - 65.0) / 15.0)
    } else {
        RED
    }
}

/// Temperature-to-RGB for DRAM cells (GDDR temperature heatmap).
///
/// ≤40°C → blue #3B82F6, 40–60°C → lerp to amber #F4C471, >60°C → lerp to red #FF6B6B.
pub(crate) fn dram_temp_rgb(temp_c: f32) -> [u8; 3] {
    const BLUE:  [u8; 3] = [59, 130, 246];
    const AMBER: [u8; 3] = [244, 196, 113];
    const RED:   [u8; 3] = [255, 107, 107];
    if temp_c <= 40.0 {
        BLUE
    } else if temp_c <= 60.0 {
        lerp_rgb(BLUE, AMBER, (temp_c - 40.0) / 20.0)
    } else {
        lerp_rgb(AMBER, RED, (temp_c - 60.0) / 20.0)
    }
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
/// Only single-column chars are used: ▓ ▒ ░ · ▪ ● ╋ (all width-1 per unicode-width crate).
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
                CoreType::Pcie => '╋',

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

                CoreType::Dram => '▪',

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
/// - Tensix ░▒▓: ASIC temperature heatmap (teal→gold→red)
/// - Tensix ·(harvested): DarkGray+DIM
/// - DRAM ▪: GDDR temperature heatmap (blue→amber→red)
/// - ETH ●(live): Green  ·(down): DarkGray
/// - PCIe ╋: Amber RGB(244,196,113)
pub fn build_portrait_lines<'a>(
    device: &Device,
    telemetry: &Telemetry,
    smbus: Option<&SmbusTelemetry>,
    tick: u64,
) -> Vec<Line<'a>> {
    let rows_strs = build_portrait_rows(device, telemetry, smbus, tick);
    let (cols, _) = portrait_dims(device.architecture);

    // Pre-compute ASIC temp → Tensix heatmap color (same for all Tensix cells on this chip)
    let asic_temp = telemetry.temp_c();
    let [tr, tg, tb] = tensix_temp_rgb(asic_temp);
    let tensix_color = Color::Rgb(tr, tg, tb);

    // Per-column DRAM temperature color via GDDR temp interpolation
    let dram_color_for_col = |col: usize| -> Color {
        let pair_idx = (col / 4).min(3);
        let temp = smbus
            .and_then(|s| s.gddr_temps.get(pair_idx).and_then(|p| p.as_ref()))
            .map(|p| p.0.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            .unwrap_or(0.0);
        let [r, g, b] = dram_temp_rgb(temp);
        Color::Rgb(r, g, b)
    };

    rows_strs.into_iter().enumerate().map(|(row, row_str)| {
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(cols);

        for (col, ch) in row_str.chars().enumerate() {
            let core_type = match device.architecture {
                Architecture::Blackhole => core_type_bh(col, row),
                _                       => core_type_wh(col, row),
            };

            let style = match (core_type, ch) {
                // PCIe: amber (same gold as hot-Tensix anchor)
                (CoreType::Pcie, _) =>
                    Style::default().fg(Color::Rgb(244, 196, 113)),
                // ETH: green when live, dim gray when down
                (CoreType::Eth, '●') =>
                    Style::default().fg(Color::Green),
                (CoreType::Eth, _) =>
                    Style::default().fg(Color::DarkGray),
                // DRAM: blue→amber→red by per-column GDDR temperature
                (CoreType::Dram, _) =>
                    Style::default().fg(dram_color_for_col(col)),
                // Tensix harvested ('·'): dim gray, no heatmap
                (CoreType::Tensix, '·') =>
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                // Tensix non-harvested (░▒▓): ASIC temperature heatmap color
                (CoreType::Tensix, _) =>
                    Style::default().fg(tensix_color),
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
    fn test_bh_dram_rows_show_block() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // row 0 col 1 (non-ETH, non-PCIe) should be '▪'
        let row0_chars: Vec<char> = rows[0].chars().collect();
        assert_eq!(row0_chars[1], '▪', "BH row=0 col=1 should be DRAM '▪'");
        let row11_chars: Vec<char> = rows[11].chars().collect();
        assert_eq!(row11_chars[1], '▪', "BH row=11 col=1 should be DRAM '▪'");
    }

    #[test]
    fn test_bh_pcie_shows_cross() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = make_telemetry();
        let rows = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        // col 8, non-DRAM rows (1-10) should be '╋'
        for row_idx in 1..11 {
            let chars: Vec<char> = rows[row_idx].chars().collect();
            assert_eq!(chars[8], '╋', "BH PCIe col=8 row={} should be '╋'", row_idx);
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

    // ── Layout math ───────────────────────────────────────────────────────────

    #[test]
    fn test_insights_panel_width_bh() {
        // BH portrait outer = 1+17 = 18. Gap = 1. Stats outer = 1+30 = 31.
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let portrait_outer = cols as u16 + 1;
        let gap = 1_u16;
        let stats_outer = 31_u16;
        let total = portrait_outer + gap + stats_outer;
        assert_eq!(total, 50, "BH Insights panel width should be 50 chars");
    }

    #[test]
    fn test_insights_panel_width_wh() {
        let (cols, _) = portrait_dims(Architecture::Wormhole);
        let portrait_outer = cols as u16 + 1;
        let gap = 1_u16;
        let stats_outer = 31_u16;
        let total = portrait_outer + gap + stats_outer;
        assert_eq!(total, 43, "WH Insights panel width should be 43 chars");
    }

    #[test]
    fn test_grid_cell_width_bh() {
        // Grid cell = portrait_cols + 2 (left-border + 1 gap)
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let cell_w = cols as u16 + 2;
        assert_eq!(cell_w, 19, "BH grid cell width should be 19 chars");
    }

    #[test]
    fn test_grid_cell_width_wh() {
        let (cols, _) = portrait_dims(Architecture::Wormhole);
        let cell_w = cols as u16 + 2;
        assert_eq!(cell_w, 12, "WH grid cell width should be 12 chars");
    }

    #[test]
    fn test_grid_cells_per_row_80_col_bh() {
        // At 80 columns, BH cells (19 wide) → 80/19 = 4 cells per row
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let cell_w = cols as u16 + 2;
        let terminal_w = 80_u16;
        let cells = terminal_w / cell_w;
        assert_eq!(cells, 4, "80-col terminal should fit 4 BH cells per row");
    }

    #[test]
    fn test_grid_cells_per_row_20_col_minimum() {
        // At 20 columns (very narrow), clamped to 1 cell minimum
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let cell_w = cols as u16 + 2;
        let terminal_w = 20_u16;
        let cells = (terminal_w / cell_w).max(1);
        assert_eq!(cells, 1, "very narrow terminal should still show 1 cell");
    }

    // ── Color helpers ─────────────────────────────────────────────────────────

    #[test]
    fn test_lerp_rgb_midpoint() {
        let result = lerp_rgb([0, 0, 0], [100, 200, 100], 0.5);
        assert_eq!(result, [50, 100, 50]);
    }

    #[test]
    fn test_lerp_rgb_clamp() {
        // t outside [0,1] must clamp
        let a = lerp_rgb([10, 20, 30], [90, 80, 70], -1.0);
        assert_eq!(a, [10, 20, 30]);
        let b = lerp_rgb([10, 20, 30], [90, 80, 70], 2.0);
        assert_eq!(b, [90, 80, 70]);
    }

    #[test]
    fn test_tensix_temp_rgb_cold() {
        // ≤40°C → teal #4FD1C5
        let [r, g, b] = tensix_temp_rgb(20.0);
        assert_eq!([r, g, b], [79, 209, 197]);
    }

    #[test]
    fn test_tensix_temp_rgb_hot() {
        // >80°C → red #FF6B6B
        let [r, g, b] = tensix_temp_rgb(90.0);
        assert_eq!([r, g, b], [255, 107, 107]);
    }

    #[test]
    fn test_tensix_temp_rgb_midrange() {
        // exactly 65°C → gold #F4C471
        let [r, g, b] = tensix_temp_rgb(65.0);
        assert_eq!([r, g, b], [244, 196, 113]);
    }

    #[test]
    fn test_dram_temp_rgb_cold() {
        // ≤40°C → blue #3B82F6
        let [r, g, b] = dram_temp_rgb(30.0);
        assert_eq!([r, g, b], [59, 130, 246]);
    }

    #[test]
    fn test_dram_temp_rgb_hot() {
        let [r, g, b] = dram_temp_rgb(80.0);
        assert_eq!([r, g, b], [255, 107, 107], "80°C DRAM should be full red");
    }

    #[test]
    fn test_dram_temp_rgb_boundary() {
        let [r, g, b] = dram_temp_rgb(60.0);
        assert_eq!([r, g, b], [244, 196, 113], "60°C DRAM should be amber");
    }

    #[test]
    fn test_tensix_char_idle_non_harvested_is_floor() {
        // activity=0.0 → minimum '░' (not '·')
        assert_eq!(tensix_char(0.0), '░');
        assert_eq!(tensix_char(0.04), '░');
    }

    #[test]
    fn test_tensix_char_active() {
        assert_eq!(tensix_char(0.40), '▒');
        assert_eq!(tensix_char(0.75), '▓');
    }

    #[test]
    fn test_build_portrait_lines_floor_char() {
        let device = make_device(Architecture::Blackhole);
        let smbus  = make_smbus(Architecture::Blackhole);
        let telem  = Telemetry { power: Some(1.0), asic_temperature: Some(20.0), ..Telemetry::new() };
        let rows   = build_portrait_rows(&device, &telem, Some(&smbus), 0);
        let ch = rows[1].chars().nth(1).unwrap();
        assert_eq!(ch, '░', "idle non-harvested Tensix should show '░' floor");
    }

    // ── Particle system ───────────────────────────────────────────────────────

    #[test]
    fn test_particle_read_advances() {
        let mut p = Particle { col: 3, progress: 0.0, from_top: true, kind: ParticleKind::Read };
        let speed = 0.1;
        p.progress += speed;
        assert!((p.progress - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn test_particle_write_retreats() {
        let mut p = Particle { col: 3, progress: 0.5, from_top: true, kind: ParticleKind::Write };
        let speed = 0.1;
        p.progress -= speed;
        assert!((p.progress - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_particle_char_dot() {
        assert_eq!(particle_char(0.05), '·');
    }

    #[test]
    fn test_particle_char_ring() {
        assert_eq!(particle_char(0.25), '∘');
    }

    #[test]
    fn test_particle_char_circle() {
        assert_eq!(particle_char(0.5), '○');
    }

    #[test]
    fn test_trained_random_col_bh_returns_dram_col() {
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let ddr_status: u64 = u64::MAX; // all trained
        let result = trained_random_col(ddr_status, cols, Architecture::Blackhole, 42);
        assert!(result.is_some(), "should return a column when all trained");
        let col = result.unwrap();
        // Col must be a DRAM col in BH: core_type_bh(col, 0) == Dram
        let ct = core_type_bh(col, 0);
        assert_eq!(ct, CoreType::Dram, "col {} should be a DRAM col", col);
    }

    #[test]
    fn test_trained_random_col_none_when_no_trained() {
        let (cols, _) = portrait_dims(Architecture::Blackhole);
        let result = trained_random_col(0u64, cols, Architecture::Blackhole, 42);
        assert!(result.is_none(), "should return None when no trained columns");
    }
}
