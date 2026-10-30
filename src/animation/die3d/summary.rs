// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The text summary shown where no scene fits.
//!
//! Below the smallest size at which a scene can be drawn, the view lists one
//! line per chip instead: its name, its power and its PCIe rates, as
//! `BH0 44W  in 2.0G  out 1.0M`. The function here is pure: it reads the chips
//! and a size and returns lines, so the layout rules are tested without a
//! view. Devices the view cannot draw are not listed here, because the footer
//! already names them.

use super::scene::ChipInputs;
use super::{chip_label, hint};
use ratatui::text::Line;

/// A PCIe rate in bytes per second, written compactly: `2.0G`, `1.0M`,
/// `12K`, `850`. A missing or non-finite rate, and a negative one, is `-`.
///
/// Units are decimal (1 K = 1000 bytes), as in
/// `backend::pcie_counters::format_bandwidth`. That function adds a unit
/// suffix and spaces, which is too wide for a line in a view this small, so
/// the summary keeps its own shorter form.
fn compact_rate(bps: Option<f64>) -> String {
    let b = match bps {
        Some(b) if b.is_finite() && b >= 0.0 => b,
        _ => return "-".to_string(),
    };
    for (unit, size) in [("G", 1e9), ("M", 1e6), ("K", 1e3)] {
        if b >= size {
            let v = b / size;
            // One decimal below 10 (`2.0G`), whole numbers from 10 up
            // (`12K`). Rounding 9.96 up to `10.0` is avoided by testing the
            // rounded value.
            return if (v * 10.0).round() < 100.0 {
                format!("{v:.1}{unit}")
            } else {
                format!("{v:.0}{unit}")
            };
        }
    }
    format!("{b:.0}")
}

/// One chip's line: `BH0 44W  in 2.0G  out 1.0M`.
fn chip_line(chip: &ChipInputs) -> String {
    format!(
        "{}  in {}  out {}",
        chip_label(chip),
        compact_rate(chip.pcie_in_bps),
        compact_rate(chip.pcie_out_bps)
    )
}

/// `text` cut to its first `cols` characters.
fn cut(text: String, cols: usize) -> String {
    text.chars().take(cols).collect()
}

/// The summary of every chip in `chips` (boards in order, flattened) as at
/// most `rows` lines of at most `cols` characters.
///
/// When all chips fit in `rows`, there is one line each. Otherwise the last
/// row is `+N more`, N being the chips left out, and the rows before it show
/// as many chips as fit. A zero `rows` or `cols` gives no lines.
pub(super) fn summary_lines(
    chips: &[Vec<ChipInputs>],
    cols: usize,
    rows: usize,
) -> Vec<Line<'static>> {
    if rows == 0 || cols == 0 {
        return Vec::new();
    }
    let all: Vec<&ChipInputs> = chips.iter().flatten().collect();
    let shown = if all.len() <= rows {
        all.len()
    } else {
        rows - 1
    };
    let mut lines: Vec<Line<'static>> = all[..shown]
        .iter()
        .map(|c| hint(cut(chip_line(c), cols)))
        .collect();
    if shown < all.len() {
        lines.push(hint(cut(format!("+{} more", all.len() - shown), cols)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Architecture;

    fn chip(i: usize, power: Option<f32>, inn: Option<f64>, out: Option<f64>) -> ChipInputs {
        ChipInputs {
            index: i,
            label: format!("BH{i}"),
            power_w: power,
            arch: Architecture::Blackhole,
            load: 0.0,
            pcie_in_bps: inn,
            pcie_out_bps: out,
            gddr_heat: None,
            eth_live: 0,
            temp_c: None,
            enabled_tensix_col: None,
            tile_flow: None,
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn n_chips(n: usize) -> Vec<Vec<ChipInputs>> {
        vec![(0..n)
            .map(|i| chip(i, Some(44.0), Some(2.0e9), Some(1.0e6)))
            .collect()]
    }

    #[test]
    fn each_chip_gets_one_line() {
        let t = text(&summary_lines(&n_chips(2), 40, 5));
        assert_eq!(
            t,
            vec!["BH0 44W  in 2.0G  out 1.0M", "BH1 44W  in 2.0G  out 1.0M"]
        );
    }

    #[test]
    fn chips_on_every_board_are_listed() {
        let boards = vec![n_chips(1).remove(0), vec![chip(1, None, None, None)]];
        assert_eq!(text(&summary_lines(&boards, 40, 5)).len(), 2);
    }

    #[test]
    fn a_missing_rate_shows_a_dash() {
        let c = vec![vec![chip(0, Some(44.0), None, Some(f64::NAN))]];
        assert_eq!(
            text(&summary_lines(&c, 40, 3)),
            vec!["BH0 44W  in -  out -"]
        );
    }

    #[test]
    fn rates_are_compact() {
        assert_eq!(compact_rate(Some(850.0)), "850");
        assert_eq!(compact_rate(Some(12_000.0)), "12K");
        assert_eq!(compact_rate(Some(9_960.0)), "10K");
        assert_eq!(compact_rate(Some(1.5e9)), "1.5G");
        assert_eq!(compact_rate(Some(-1.0)), "-");
    }

    #[test]
    fn chips_that_do_not_fit_end_in_a_count() {
        let t = text(&summary_lines(&n_chips(5), 40, 3));
        assert_eq!(t.len(), 3);
        assert!(t[0].starts_with("BH0") && t[1].starts_with("BH1"));
        assert_eq!(t[2], "+3 more");
    }

    #[test]
    fn a_line_never_exceeds_the_width() {
        for cols in 0..=60 {
            for l in summary_lines(&n_chips(5), cols, 3) {
                let w: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
                assert!(w <= cols, "{w} > {cols}");
            }
        }
    }

    #[test]
    fn no_rows_or_no_columns_give_no_lines() {
        assert!(summary_lines(&n_chips(2), 40, 0).is_empty());
        assert!(summary_lines(&n_chips(2), 0, 5).is_empty());
        assert!(summary_lines(&[], 40, 5).is_empty());
    }
}
