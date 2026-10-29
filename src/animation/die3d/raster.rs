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
#[derive(Debug, Clone)]
pub struct PixelBuf {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<[u8; 3]>,
}

impl PixelBuf {
    /// A black buffer of `w` x `h` pixels.
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            rgb: vec![[0; 3]; w * h],
        }
    }

    /// Fill with `base`, darkened toward the top and bottom edges.
    ///
    /// Same formula as the mockup: factor `1 - 0.25 * |y/h - 0.5|`, so the
    /// middle row keeps the full colour and the edges drop to 87.5%.
    pub fn fill_vignette(&mut self, base: [u8; 3]) {
        for y in 0..self.h {
            let v = 1.0 - 0.25 * ((y as f32 / self.h as f32) - 0.5).abs();
            let row = shade(base, v);
            self.rgb[y * self.w..(y + 1) * self.w].fill(row);
        }
    }

    /// Set one pixel; coordinates outside the buffer are ignored so callers can
    /// draw blocks that hang off the edge without clipping first.
    pub fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        self.rgb[y as usize * self.w + x as usize] = c;
    }

    /// Read one pixel. Panics if out of range, like slice indexing.
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        self.rgb[y * self.w + x]
    }
}

/// Isometric projection of the tile grid onto the pixel buffer.
#[derive(Debug, Clone, Copy)]
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
}

impl Iso {
    /// Rotate grid coordinates about the grid centre by `quarter_turns`.
    ///
    /// The centre stays fixed, so a non-square grid is rotated in place and its
    /// rotated footprint may extend past the original bounds; the caller's
    /// `ox`/`oy` decide where that lands on screen.
    fn rotate(&self, gx: f32, gy: f32, grid: (usize, usize)) -> (f32, f32) {
        let cx = (grid.0 as f32 - 1.0) / 2.0;
        let cy = (grid.1 as f32 - 1.0) / 2.0;
        let (mut dx, mut dy) = (gx - cx, gy - cy);
        for _ in 0..(self.quarter_turns % 4) {
            (dx, dy) = (-dy, dx);
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
    /// current turn: tiles with a smaller rotated `x + y` are farther away and
    /// are painted first, so nearer blocks cover them.
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
        };
        let order = iso.draw_order((5, 5));
        let pos = |t: (usize, usize)| order.iter().position(|&o| o == t).unwrap();
        assert!(pos((2, 2)) < pos((3, 2)));
        assert!(pos((2, 2)) < pos((2, 3)));
    }
}
