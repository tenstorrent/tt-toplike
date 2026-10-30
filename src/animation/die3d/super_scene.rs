// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Die 3D super scene: every board in one view.
//!
//! Boards stand in a row along the grid x axis and wrap to a second row after
//! [`BOARDS_PER_ROW`]. A 150 x 42 cell view then holds up to four boards, so
//! each die gets a fraction of the area a lone board has. To stay legible the
//! dies are drawn at reduced detail: `Detail::Blocks(2)`, where each 2 x 2
//! group of Tensix tiles is one block (see [`block_heat`]). A lone board has
//! the room for full detail and is drawn exactly like `draw_board_scene`.

use super::palette;
use super::raster::PixelBuf;
use super::scene::{
    draw_die_tile, draw_slab, fit_grid, harvested_columns, ChipInputs, Detail, DieSlot, DrawCtx,
    SceneFit, BOARD_BAND, BOARD_COLS,
};
use std::collections::HashMap;

/// Boards in one row of the super scene.
const BOARDS_PER_ROW: usize = 3;

/// Heat of a block of tiles: the mean of the tiles' heats, at most 1.0.
pub fn block_heat(tiles: &[f32; 4]) -> f32 {
    mean_heat(tiles)
}

/// [`block_heat`] for a group with fewer than four tiles, such as the single
/// column left at the die edge by 17 columns of 2 x 2 groups. An empty group
/// is 0.0.
pub(super) fn mean_heat(tiles: &[f32]) -> f32 {
    if tiles.is_empty() {
        return 0.0;
    }
    (tiles.iter().sum::<f32>() / tiles.len() as f32).min(1.0)
}

/// Grid size (columns, rows) of the super scene and the height in rows of one
/// board row, for boards of the given die counts. Every board gets a cell as
/// wide as a board and as tall as the tallest board, so the boards line up.
pub(super) fn super_grid(boards: &[usize]) -> ((usize, usize), usize) {
    let row_rows = BOARD_BAND * boards.iter().copied().max().unwrap_or(1).max(1);
    let cols = boards.len().clamp(1, BOARDS_PER_ROW) * BOARD_COLS;
    let rows = boards.len().max(1).div_ceil(BOARDS_PER_ROW);
    ((cols, rows * row_rows), row_rows)
}

/// Grid position of the band of die `die` on board `board` in the super
/// scene, whose board rows are `row_rows` tall (from [`super_grid`]): boards
/// fill a row of [`BOARDS_PER_ROW`] along the grid x axis, then wrap, and a
/// board's dies stack along the grid y axis as on a lone board.
pub(super) fn band_origin(board: usize, die: usize, row_rows: usize) -> (usize, usize) {
    (
        board % BOARDS_PER_ROW * BOARD_COLS,
        board / BOARDS_PER_ROW * row_rows + die * BOARD_BAND,
    )
}

/// [`fit`](super::scene::fit) for the super scene: `boards` holds the number
/// of dies on each board. One origin serves all four quarter turns. `None`
/// below 100 x 30 cells, and when even the smallest tile (4 pixels wide) does
/// not hold every board.
pub fn fit_super(cells_w: usize, cells_h: usize, boards: &[usize]) -> Option<SceneFit> {
    fit_grid(cells_w, cells_h, super_grid(boards).0)
}

/// Draw every board in one view.
///
/// `fit` must come from [`fit_super`] with the die count of each board. One
/// board is drawn at full detail, exactly as `draw_board_scene` would; more
/// boards use `Detail::Blocks(2)`. The whole grid is projected and ordered
/// together, so a board in front covers the one behind it.
#[cfg(test)]
pub fn draw_super_scene(buf: &mut PixelBuf, boards: &[Vec<ChipInputs>], fit: &SceneFit, t: f32) {
    draw_super_scene_in(palette::GRAYSKULL, buf, boards, fit, t)
}

/// [`draw_super_scene`] painted in `pal`.
pub fn draw_super_scene_in(
    pal: palette::Palette,
    buf: &mut PixelBuf,
    boards: &[Vec<ChipInputs>],
    fit: &SceneFit,
    t: f32,
) {
    let dies: Vec<usize> = boards.iter().map(Vec::len).collect();
    let (grid, row_rows) = super_grid(&dies);
    let detail = if boards.len() <= 1 {
        Detail::Full
    } else {
        Detail::Blocks(2)
    };
    let ctx = DrawCtx::in_palette(fit, t, detail, pal);
    let harvested: Vec<Vec<Vec<u8>>> = boards.iter().map(|b| harvested_columns(b)).collect();
    let mut visits = HashMap::new();
    for (bx, by) in ctx.iso.draw_order(grid) {
        let at = ctx.iso.project(bx as f32, by as f32, grid);
        draw_slab(buf, &ctx, (bx, by), at);
        // Which board, which die of it, and where in that die's band.
        let idx = by / row_rows * BOARDS_PER_ROW + bx / BOARD_COLS;
        let in_row = by % row_rows;
        let die = in_row / BOARD_BAND;
        let Some(chip) = boards.get(idx).and_then(|b| b.get(die)) else {
            continue;
        };
        let slot = DieSlot {
            chip,
            harvested: &harvested[idx][die],
            origin: band_origin(idx, die, row_rows),
            // Dies are numbered across boards; no board has more than
            // `row_rows` bands, so this is unique.
            slot: idx * row_rows + die,
        };
        let local = (bx % BOARD_COLS, in_row % BOARD_BAND);
        draw_die_tile(buf, &ctx, &slot, grid, local, at, &mut visits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::die3d::scene::{draw_board_scene, fit};
    use crate::models::Architecture;

    fn chip(load: f32) -> ChipInputs {
        ChipInputs {
            index: 0,
            label: "BH0".to_string(),
            power_w: Some(load * 150.0),
            arch: Architecture::Blackhole,
            load,
            pcie_in_bps: Some(40.0e9),
            pcie_out_bps: Some(40.0e9),
            gddr_heat: Some(1.0),
            eth_live: u64::MAX,
            eth_known: false,
            temp_c: None,
            enabled_tensix_col: None,
            tile_flow: None,
        }
    }

    fn boards(dies: &[usize]) -> Vec<Vec<ChipInputs>> {
        dies.iter()
            .map(|&d| (0..d).map(|_| chip(1.0)).collect())
            .collect()
    }

    #[test]
    fn one_board_super_scene_equals_the_board_scene() {
        for dies in [1usize, 2] {
            let b = boards(&[dies]);
            for turn in 0..4 {
                let mut f = fit(150, 42, dies).unwrap();
                f.iso.quarter_turns = turn;
                let mut want = PixelBuf::new(300, 168);
                draw_board_scene(&mut want, &b[0], &f, 1.7, Detail::Full);
                let mut fs = fit_super(150, 42, &[dies]).unwrap();
                fs.iso.quarter_turns = turn;
                let mut got = PixelBuf::new(300, 168);
                draw_super_scene(&mut got, &b, &fs, 1.7);
                assert_eq!(got.rgb, want.rgb, "dies={dies} turn={turn}");
            }
        }
    }

    #[test]
    fn blocks_detail_lights_a_block_by_the_sum_of_its_tiles() {
        // The mean of four tiles: two hot ones and two cold ones are half lit.
        assert_eq!(block_heat(&[1.0, 1.0, 0.0, 0.0]), 0.5);
        assert_eq!(block_heat(&[0.0; 4]), 0.0);
        assert_eq!(block_heat(&[0.4; 4]), 0.4);
        // Capped at 1.0 even when the inputs are above it.
        assert_eq!(block_heat(&[2.0, 2.0, 2.0, 2.0]), 1.0);
        // A short group at the die edge is the mean of what it has.
        assert_eq!(mean_heat(&[1.0, 0.0]), 0.5);
        assert_eq!(mean_heat(&[]), 0.0);
    }

    /// Each board is drawn from its own chips: cooling the second board's chip
    /// changes the picture.
    #[test]
    fn each_board_is_drawn_from_its_own_chips() {
        let mut f = fit_super(150, 42, &[1, 1]).unwrap();
        f.iso.quarter_turns = 0;
        let ctx_boards = boards(&[1, 1]);
        let mut coarse = PixelBuf::new(300, 168);
        draw_super_scene(&mut coarse, &ctx_boards, &f, 1.7);
        let mut hot = ctx_boards.clone();
        hot[1][0].load = 0.0;
        let mut other = PixelBuf::new(300, 168);
        draw_super_scene(&mut other, &hot, &f, 1.7);
        // Load changes the picture (the blocks are lit from the load estimate).
        assert_ne!(coarse.rgb, other.rgb);
    }

    /// Everything the scene painted, as a bounding box `(x0, y0, x1, y1)` with
    /// an exclusive end, found by drawing into a buffer with `PAD` spare pixels
    /// on every side (`PixelBuf::put` silently drops what falls outside).
    fn painted_bbox(
        b: &[Vec<ChipInputs>],
        dies: &[usize],
        cells: (usize, usize),
        turn: u8,
    ) -> (usize, usize, usize, usize) {
        painted_bbox_spun(b, dies, cells, turn, 0.0)
    }

    /// [`painted_bbox`] with `spin` radians on top of the quarter turns.
    fn painted_bbox_spun(
        b: &[Vec<ChipInputs>],
        dies: &[usize],
        cells: (usize, usize),
        turn: u8,
        spin: f32,
    ) -> (usize, usize, usize, usize) {
        const PAD: usize = 80;
        let mut f = fit_super(cells.0, cells.1, dies).unwrap();
        f.iso.quarter_turns = turn;
        f.iso.spin = spin;
        f.iso.ox += PAD as i32;
        f.iso.oy += PAD as i32;
        let (w, h) = (cells.0 * 2 + 2 * PAD, cells.1 * 4 + 2 * PAD);
        let mut drawn = PixelBuf::new(w, h);
        draw_super_scene(&mut drawn, b, &f, 1.3);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if drawn.covered(x, y) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        assert!(
            x0 >= PAD && y0 >= PAD,
            "clipped on the left or top: {x0},{y0}"
        );
        (x0 - PAD, y0 - PAD, x1 - PAD, y1 - PAD)
    }

    #[test]
    fn four_boards_do_not_overflow_the_reference_buffer() {
        // Four boards of two dies: three in the first row, one in the second.
        let dies = [2usize, 2, 2, 2];
        let b = boards(&dies);
        for turn in 0..4 {
            let (x0, y0, x1, y1) = painted_bbox(&b, &dies, (150, 42), turn);
            assert!(x1 <= 300 && y1 <= 168, "turn={turn}: {x0},{y0}..{x1},{y1}");
            // Not vacuous: the scene fills a good part of the buffer.
            assert!(
                x1 - x0 > 150 && y1 - y0 > 60,
                "turn={turn}: {x0},{y0}..{x1},{y1}"
            );
        }
    }

    #[test]
    fn boards_wrap_to_a_second_row_after_three() {
        let (grid3, row3) = super_grid(&[2, 2, 2]);
        let (grid4, row4) = super_grid(&[2, 2, 2, 2]);
        assert_eq!(grid3, (3 * BOARD_COLS, 2 * BOARD_BAND));
        assert_eq!(grid4, (3 * BOARD_COLS, 2 * 2 * BOARD_BAND));
        assert_eq!((row3, row4), (2 * BOARD_BAND, 2 * BOARD_BAND));
        // One board is exactly the board grid, so fits agree.
        assert_eq!(super_grid(&[1]).0, (BOARD_COLS, BOARD_BAND));
    }

    #[test]
    fn four_boards_get_a_smaller_tile_than_one() {
        let one = fit_super(150, 42, &[2]).unwrap();
        let four = fit_super(150, 42, &[2, 2, 2, 2]).unwrap();
        assert!(four.iso.tw < one.iso.tw);
        // Below the smallest terminal there is no fit, as for one board.
        assert!(fit_super(99, 42, &[2]).is_none());
    }

    #[test]
    fn every_spin_angle_stays_inside_the_buffer() {
        use crate::animation::die3d::scene::painted_bbox_for;
        let cells = (150usize, 42usize);
        let (pw, ph) = (cells.0 * 2, cells.1 * 4);
        let inside = |what: &str, deg: i32, (x0, y0, x1, y1): (usize, usize, usize, usize)| {
            assert!(
                x1 <= pw && y1 <= ph,
                "{what} at {deg} degrees: {x0},{y0}..{x1},{y1} in {pw} x {ph}"
            );
        };
        // Real pictures every 15 degrees. A fit that samples only the four
        // quarter turns overflows the two-die board by a pixel at 94 to 103
        // and 167 to 176 degrees (and the mirror angles), which these steps
        // pass over; `scene::tests::fit_holds_every_degree_of_spin_without_drawing`
        // checks every degree without drawing and catches that fit. Drawing
        // every degree took about 4 s.
        for deg in (0..360).step_by(15) {
            let spin = (deg as f32).to_radians();
            // One board of one and of two dies.
            for dies in [1usize, 2] {
                let chips: Vec<ChipInputs> = (0..dies).map(|_| chip(1.0)).collect();
                let mut f = fit(cells.0, cells.1, dies).unwrap();
                f.iso.spin = spin;
                // `painted_bbox_for` asserts the left and top edges itself.
                inside(
                    &format!("{dies} dies"),
                    deg,
                    painted_bbox_for(&chips, cells, &f),
                );
            }
            // The super scene of four boards of two dies each.
            let dies = [2usize, 2, 2, 2];
            let b = boards(&dies);
            inside(
                "4 x 2 super scene",
                deg,
                painted_bbox_spun(&b, &dies, cells, 0, spin),
            );
        }
    }
}
