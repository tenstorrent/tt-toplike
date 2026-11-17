// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Size tiers: which parts of the Die 3D view are drawn at a given terminal
//! size.
//!
//! The view draws everything at 100 x 30 cells and up. Below that it gives up
//! features in a fixed order: the cloud layer and the second footer row go
//! first, then the chip flags and the footer. This module only maps a size to
//! those choices. It reads no state, so the thresholds are tested directly.

/// Size at which the footer takes two rows (the keys line plus the status
/// line). Below it, the footer is one row. Both dimensions must reach it.
pub(super) const FOOTER_TWO: (usize, usize) = (100, 30);

/// Size at which the footer is drawn at all. Below it the footer has no rows.
/// Both dimensions must reach it.
pub(super) const FOOTER_ONE: (usize, usize) = (60, 16);

/// Size at which the pin-and-flag chip labels are drawn. Below it there are
/// no flags. Both dimensions must reach it.
pub(super) const FLAGS_MIN: (usize, usize) = (60, 16);

/// Size at which the cloud layer is drawn. Below it there are no clouds. Both
/// dimensions must reach it.
pub(super) const CLOUDS_MIN: (usize, usize) = (100, 30);

/// What the view draws at one terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Tier {
    /// Rows the footer takes: 2, 1 or 0.
    pub footer_rows: usize,
    /// Whether the cloud layer is drawn.
    pub clouds: bool,
    /// Whether the pin-and-flag chip labels are drawn.
    pub flags: bool,
}

/// True when both dimensions reach `min`.
fn reaches(cols: usize, rows: usize, min: (usize, usize)) -> bool {
    cols >= min.0 && rows >= min.1
}

/// The features drawn at `cols` x `rows` terminal cells.
pub(super) fn tier_for(cols: usize, rows: usize) -> Tier {
    let footer_rows = if reaches(cols, rows, FOOTER_TWO) {
        2
    } else if reaches(cols, rows, FOOTER_ONE) {
        1
    } else {
        0
    };
    Tier {
        footer_rows,
        clouds: reaches(cols, rows, CLOUDS_MIN),
        flags: reaches(cols, rows, FLAGS_MIN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_100_by_30_and_up_everything_is_on() {
        let all = Tier {
            footer_rows: 2,
            clouds: true,
            flags: true,
        };
        assert_eq!(tier_for(100, 30), all);
        assert_eq!(tier_for(200, 60), all);
    }

    #[test]
    fn below_100_by_30_clouds_go_and_the_footer_is_one_row() {
        for (c, r) in [(99, 40), (120, 29)] {
            let t = tier_for(c, r);
            assert_eq!(t.footer_rows, 1, "{c}x{r}");
            assert!(!t.clouds, "{c}x{r}");
            assert!(t.flags, "{c}x{r}");
        }
    }

    #[test]
    fn below_60_by_16_flags_and_footer_go() {
        for (c, r) in [(59, 40), (80, 15)] {
            let t = tier_for(c, r);
            assert_eq!(t.footer_rows, 0, "{c}x{r}");
            assert!(!t.flags, "{c}x{r}");
        }
    }

    /// Growing either dimension never turns a feature off or removes footer
    /// rows.
    #[test]
    fn tier_is_monotonic() {
        let ok = |a: Tier, b: Tier| {
            b.footer_rows >= a.footer_rows && (b.clouds || !a.clouds) && (b.flags || !a.flags)
        };
        for c in 0..=200 {
            for r in 0..=60 {
                let here = tier_for(c, r);
                assert!(
                    ok(here, tier_for(c + 1, r)),
                    "cols {c}->{} at rows {r}",
                    c + 1
                );
                assert!(
                    ok(here, tier_for(c, r + 1)),
                    "rows {r}->{} at cols {c}",
                    r + 1
                );
            }
        }
    }
}
