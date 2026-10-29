// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pixel buffer and isometric block rasteriser for the Die 3D view.

/// JavaScript's `Math.round` (ties go up). The mockup relies on it, and Rust's
/// `f32::round` differs for negative halves, so use this to keep pixels identical.
fn js_round(v: f32) -> i32 {
    (v + 0.5).floor() as i32
}

/// Scale a colour by `k`, rounding and clamping like a `Uint8ClampedArray`.
fn shade(c: [u8; 3], k: f32) -> [u8; 3] {
    let f = |v: u8| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2])]
}

/// A plain RGB pixel buffer, row-major, one `[r, g, b]` per pixel.
///
/// The Die 3D scene is drawn here first and converted to terminal cells later,
/// so the rasteriser never depends on ratatui.
///
/// A coverage mask (`cover`, one flag per pixel) records which pixels were
/// painted. An uncovered pixel is transparent: the cell converters leave it to
/// the terminal's own background instead of drawing a colour there. `rgb` of an
/// uncovered pixel is meaningless (black by construction).
#[derive(Debug, Clone)]
pub struct PixelBuf {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<[u8; 3]>,
    /// `true` where a pixel was painted. Same layout as `rgb`.
    pub cover: Vec<bool>,
}

impl PixelBuf {
    /// A `w` x `h` buffer with nothing painted: black, fully transparent.
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            rgb: vec![[0; 3]; w * h],
            cover: vec![false; w * h],
        }
    }

    /// Whether the pixel at (`x`, `y`) was painted. Panics if out of range.
    pub fn covered(&self, x: usize, y: usize) -> bool {
        self.cover[y * self.w + x]
    }

    /// Make every pixel transparent again, keeping the allocations.
    pub fn clear_transparent(&mut self) {
        self.rgb.fill([0; 3]);
        self.cover.fill(false);
    }

    /// Paint every pixel `c` and mark it covered (opaque). The view's opaque
    /// mode (`T` off) uses this with pure black, so the scene stands on a
    /// plain dark background instead of the terminal's own.
    pub fn fill_solid(&mut self, c: [u8; 3]) {
        self.rgb.fill(c);
        self.cover.fill(true);
    }

    /// Set one pixel; coordinates outside the buffer are ignored so callers can
    /// draw blocks that hang off the edge without clipping first. The pixel
    /// becomes covered.
    pub fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.rgb[i] = c;
        self.cover[i] = true;
    }

    /// Read one pixel. Panics if out of range, like slice indexing.
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        self.rgb[y * self.w + x]
    }
}

/// Isometric projection of the tile grid onto the pixel buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Iso {
    /// Tile diamond width in pixels.
    pub tw: i32,
    /// Tile diamond height in pixels (normally `tw / 2`).
    pub th: i32,
    /// Pixel x of grid origin (0, 0).
    pub ox: i32,
    /// Pixel y of grid origin (0, 0).
    pub oy: i32,
    /// Camera orientation, 0..=3 quarter turns about the grid centre.
    pub quarter_turns: u8,
    /// Extra turn in radians on top of `quarter_turns`, same direction (a
    /// spin of `PI / 2` lands where one more quarter turn does). `0.0` for
    /// every still camera; the spin transition sweeps it between two presets.
    ///
    /// Only tile *positions* turn: each block is still drawn as an upright
    /// isometric diamond, so mid-spin neighbouring blocks can leave small gaps
    /// or overlap. `fit` sizes the scene so every angle stays in the buffer.
    pub spin: f32,
}

impl Iso {
    /// Rotate grid coordinates about the grid centre by `quarter_turns` and
    /// then by `spin` radians.
    ///
    /// The centre stays fixed, so a non-square grid is rotated in place and its
    /// rotated footprint may extend past the original bounds; the caller's
    /// `ox`/`oy` decide where that lands on screen.
    ///
    /// The quarter turns are done by swapping coordinates, not with `cos` and
    /// `sin`: `cos(PI / 2)` in `f32` is not exactly 0, and the error would
    /// move a point that sits on a rounding half by a pixel. With `spin` at
    /// `0.0` the result is therefore exactly the pre-spin one.
    fn rotate(&self, gx: f32, gy: f32, grid: (usize, usize)) -> (f32, f32) {
        let cx = (grid.0 as f32 - 1.0) / 2.0;
        let cy = (grid.1 as f32 - 1.0) / 2.0;
        let (mut dx, mut dy) = (gx - cx, gy - cy);
        for _ in 0..(self.quarter_turns % 4) {
            (dx, dy) = (-dy, dx);
        }
        if self.spin != 0.0 {
            // The same sense as a quarter turn: (1, 0) goes toward (0, 1).
            let (s, c) = self.spin.sin_cos();
            (dx, dy) = (dx * c - dy * s, dx * s + dy * c);
        }
        (cx + dx, cy + dy)
    }

    /// Pixel position of the top-face centre of grid tile (`gx`, `gy`),
    /// using the mockup's `OX + (gx - gy) * TW / 2`, `OY + (gx + gy) * TH / 2`.
    pub fn project(&self, gx: f32, gy: f32, grid: (usize, usize)) -> (i32, i32) {
        let (rx, ry) = self.rotate(gx, gy, grid);
        let x = self.ox as f32 + (rx - ry) * self.tw as f32 / 2.0;
        let y = self.oy as f32 + (rx + ry) * self.th as f32 / 2.0;
        (js_round(x), js_round(y))
    }

    /// Every tile of a `grid` (cols, rows), ordered back to front for the
    /// current turn and spin: tiles with a smaller rotated `x + y` are farther
    /// away and are painted first, so nearer blocks cover them.
    pub fn draw_order(&self, grid: (usize, usize)) -> Vec<(usize, usize)> {
        let mut tiles: Vec<(usize, usize)> = (0..grid.1)
            .flat_map(|y| (0..grid.0).map(move |x| (x, y)))
            .collect();
        let key = |&(x, y): &(usize, usize)| {
            let (rx, ry) = self.rotate(x as f32, y as f32, grid);
            (rx + ry, ry)
        };
        tiles.sort_by(|a, b| {
            let (ka, kb) = (key(a), key(b));
            ka.0.total_cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
        });
        tiles
    }
}

/// Draw one isometric block: left face, right face, then the top diamond.
///
/// `(cx, cy)` is the base tile's centre-top; `height` lifts the top face by that
/// many pixels. Ported from `Scene.cube` in
/// `docs/superpowers/specs/assets/2026-10-29-die-3d/die-3d-resolution.html`.
///
/// With `smooth`, the side faces get a vertical gradient (left .70 to .42,
/// right .50 to .28 of `color`) and the top diamond's left and right edge pixels
/// are drawn at 0.72x to outline it. Without it the faces are flat (.62 and .42)
/// and there is no outline, which suits the coarse pixel grids.
#[allow(clippy::too_many_arguments)]
pub fn draw_block(
    buf: &mut PixelBuf,
    cx: i32,
    cy: i32,
    tw: i32,
    th: i32,
    height: i32,
    color: [u8; 3],
    smooth: bool,
) {
    let hw = tw / 2;
    let hh = th / 2;
    let h = height as f32;
    let (hwf, hhf) = (hw as f32, hh as f32);

    // Left face: the slope falls from the left corner toward the bottom vertex.
    for i in 0..hw {
        let yg = cy as f32 + i as f32 * hhf / hwf;
        let y0 = js_round(yg - h);
        let y1 = js_round(yg);
        for y in y0..=y1 {
            let k = if smooth {
                0.70 - 0.28 * (y - y0) as f32 / (h + 1.0)
            } else {
                0.62
            };
            buf.put(cx - hw + i, y + hh, shade(color, k));
        }
    }
    // Right face: mirror image, rising from the bottom vertex to the right corner.
    for i in 0..hw {
        let yg = cy as f32 + hhf - i as f32 * hhf / hwf;
        let y0 = js_round(yg - h - hhf);
        let y1 = js_round(yg - hhf);
        for y in y0..=y1 {
            let k = if smooth {
                0.50 - 0.22 * (y - y0) as f32 / (h + 1.0)
            } else {
                0.42
            };
            buf.put(cx + i, y + hh, shade(color, k));
        }
    }
    // Top diamond, drawn last so it covers the upper edges of both faces.
    for r in 0..th {
        let t = (r as f32 + 0.5) / th as f32;
        let w = (1.0 - (2.0 * t - 1.0).abs()) * hwf;
        let y = cy - height + r;
        let xa = js_round(cx as f32 - w);
        let xb = js_round(cx as f32 + w);
        for x in xa..=xb {
            let edge = smooth && (x == xa || x == xb);
            buf.put(x, y, if edge { shade(color, 0.72) } else { color });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [u8; 3] = [0, 0, 0];

    fn sum(c: [u8; 3]) -> u32 {
        c.iter().map(|&v| v as u32).sum()
    }

    fn painted(buf: &PixelBuf) -> usize {
        buf.rgb.iter().filter(|&&p| p != BLACK).count()
    }

    #[test]
    fn block_top_face_is_the_given_colour_at_the_centre() {
        for smooth in [false, true] {
            let mut buf = PixelBuf::new(40, 40);
            let col = [200, 100, 50];
            // Top diamond centre row is cy - height + th/2.
            draw_block(&mut buf, 20, 20, 8, 4, 8, col, smooth);
            assert_eq!(buf.get(20, 14), col, "smooth={smooth}");
        }
    }

    #[test]
    fn taller_block_paints_more_rows_above_the_base() {
        let mut short = PixelBuf::new(40, 40);
        let mut tall = PixelBuf::new(40, 40);
        draw_block(&mut short, 20, 20, 8, 4, 2, [200, 100, 50], false);
        draw_block(&mut tall, 20, 20, 8, 4, 8, [200, 100, 50], false);
        assert!(painted(&tall) > painted(&short));
    }

    #[test]
    fn side_faces_are_darker_than_the_top() {
        let mut buf = PixelBuf::new(40, 40);
        let col = [200, 100, 50];
        draw_block(&mut buf, 20, 20, 8, 4, 8, col, false);
        let top = buf.get(20, 14);
        let left = buf.get(17, 20);
        let right = buf.get(21, 20);
        assert!(sum(left) < sum(top));
        assert!(sum(right) < sum(left));
    }

    #[test]
    fn put_outside_the_buffer_is_ignored() {
        let mut buf = PixelBuf::new(4, 4);
        buf.put(-1, 0, [255; 3]);
        buf.put(0, -1, [255; 3]);
        buf.put(4, 0, [255; 3]);
        buf.put(0, 4, [255; 3]);
        assert_eq!(painted(&buf), 0);
        buf.put(3, 3, [9, 8, 7]);
        assert_eq!(buf.get(3, 3), [9, 8, 7]);
    }

    #[test]
    fn quarter_turns_move_a_tile_but_keep_the_tile_count() {
        let grid = (6, 4);
        let mut iso = Iso {
            tw: 8,
            th: 4,
            ox: 50,
            oy: 10,
            quarter_turns: 0,
            spin: 0.0,
        };
        let p0 = iso.project(0.0, 0.0, grid);
        iso.quarter_turns = 1;
        let p1 = iso.project(0.0, 0.0, grid);
        assert_ne!(p0, p1);
        for q in 0..4 {
            iso.quarter_turns = q;
            assert_eq!(iso.draw_order(grid).len(), 24);
        }
    }

    #[test]
    fn draw_order_paints_nearer_tiles_later() {
        let iso = Iso {
            tw: 8,
            th: 4,
            ox: 50,
            oy: 10,
            quarter_turns: 0,
            spin: 0.0,
        };
        let order = iso.draw_order((5, 5));
        let pos = |t: (usize, usize)| order.iter().position(|&o| o == t).unwrap();
        assert!(pos((2, 2)) < pos((3, 2)));
        assert!(pos((2, 2)) < pos((2, 3)));
    }

    #[test]
    fn a_new_pixel_buffer_is_fully_transparent() {
        let b = PixelBuf::new(6, 4);
        for y in 0..4 {
            for x in 0..6 {
                assert!(!b.covered(x, y), "{x},{y}");
            }
        }
    }

    #[test]
    fn put_covers_a_pixel_and_fill_solid_covers_all() {
        let mut b = PixelBuf::new(6, 4);
        b.put(2, 1, [9, 9, 9]);
        assert!(b.covered(2, 1));
        assert!(!b.covered(3, 1));
        // An off-buffer put covers nothing.
        b.put(-1, 0, [9, 9, 9]);
        b.put(6, 0, [9, 9, 9]);
        assert_eq!(b.cover.iter().filter(|&&c| c).count(), 1);
        b.fill_solid([20, 30, 40]);
        assert!((0..4).all(|y| (0..6).all(|x| b.covered(x, y))));
        b.clear_transparent();
        assert!((0..4).all(|y| (0..6).all(|x| !b.covered(x, y))));
    }

    #[test]
    fn fill_solid_covers_every_pixel() {
        let mut b = PixelBuf::new(7, 5);
        b.put(3, 2, [9, 9, 9]);
        b.fill_solid([0, 0, 0]);
        // Black is a colour here, not "nothing painted": every pixel is covered.
        assert!(b.cover.iter().all(|&c| c));
        assert!(b.rgb.iter().all(|&p| p == [0, 0, 0]));
        b.fill_solid([1, 2, 3]);
        assert!(b.rgb.iter().all(|&p| p == [1, 2, 3]));
    }

    #[test]
    fn project_at_a_whole_quarter_turn_matches_the_old_result() {
        // Positions worked out by hand from the pre-spin formula (integer
        // quarter turns about the grid centre (10, 6.5), then
        // `OX + (x - y) * TW / 2`, `OY + (x + y) * TH / 2`, ties rounded up).
        // The second case has an odd `th`, so two of its y values are exact
        // halves and pin the rounding too.
        let grid = (21usize, 14usize);
        let cases: [(i32, i32, (f32, f32), [(i32, i32); 4]); 3] = [
            (
                16,
                8,
                (0.0, 0.0),
                [(100, 20), (260, 72), (156, 152), (-4, 100)],
            ),
            (
                16,
                8,
                (5.0, 3.0),
                [(116, 52), (196, 80), (140, 120), (60, 92)],
            ),
            (
                14,
                7,
                (1.0, 0.0),
                [(107, 24), (233, 69), (142, 132), (16, 87)],
            ),
        ];
        for (tw, th, (gx, gy), want) in cases {
            for q in 0..4u8 {
                let iso = Iso {
                    tw,
                    th,
                    ox: 100,
                    oy: 20,
                    quarter_turns: q,
                    spin: 0.0,
                };
                assert_eq!(
                    iso.project(gx, gy, grid),
                    want[q as usize],
                    "tw={tw} ({gx},{gy}) turn {q}"
                );
            }
        }
    }

    #[test]
    fn spin_adds_to_the_quarter_turns() {
        // A quarter turn of spin lands where one more quarter turn does (to
        // within rounding of cos/sin), and the centre never moves.
        let grid = (21usize, 14usize);
        let base = Iso {
            tw: 16,
            th: 8,
            ox: 100,
            oy: 20,
            quarter_turns: 1,
            spin: std::f32::consts::FRAC_PI_2,
        };
        let turned = Iso {
            quarter_turns: 2,
            spin: 0.0,
            ..base
        };
        for (gx, gy) in [(0.0, 0.0), (5.0, 3.0), (20.0, 13.0)] {
            let (a, b) = (base.project(gx, gy, grid), turned.project(gx, gy, grid));
            assert!(
                (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1,
                "{a:?} {b:?}"
            );
        }
        let mid = Iso { spin: 0.3, ..base };
        assert_eq!(
            mid.project(10.0, 6.5, grid),
            turned.project(10.0, 6.5, grid)
        );
        assert_eq!(mid.draw_order(grid).len(), 21 * 14);
    }
}
