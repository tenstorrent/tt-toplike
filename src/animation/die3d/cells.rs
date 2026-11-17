// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Convert a `PixelBuf` into terminal cells: braille (2x4 pixels per cell),
//! quarter blocks (the same 2x4 pixels, merged to 2x2) or half blocks (1x2
//! pixels per cell). All three emit truecolour ratatui spans.

use super::raster::PixelBuf;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Braille dot bit for each pixel of a 2x4 cell, indexed `[row][column]`.
///
/// Unicode numbers the dots 1..8 down column 0 then column 1 for the first
/// three rows, and puts dots 7 and 8 on the bottom row, hence 0x40 and 0x80.
const DOT_BIT: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Cells whose luma spread is at most this are drawn as one flat colour. Below
/// it the two-colour split would only show quantisation noise as speckle.
const UNIFORM_LUMA_SPREAD: f32 = 14.0;

/// The braille character for a dot mask (U+2800 + mask).
pub fn braille_char(mask: u8) -> char {
    // 0x2800 + any u8 is inside the braille block, so this cannot fail.
    char::from_u32(0x2800 + mask as u32).expect("braille block is valid unicode")
}

/// The UTF-8 bytes of all 256 braille characters, three bytes each, built at
/// compile time. U+2800..=U+28FF all encode as `E2 A0..A3 80..BF`.
static BRAILLE_UTF8: [u8; 256 * 3] = {
    let mut out = [0u8; 256 * 3];
    let mut m = 0;
    while m < 256 {
        let cp = 0x2800 + m as u32;
        out[3 * m] = 0xE0 | (cp >> 12) as u8;
        out[3 * m + 1] = 0x80 | ((cp >> 6) & 0x3F) as u8;
        out[3 * m + 2] = 0x80 | (cp & 0x3F) as u8;
        m += 1;
    }
    out
};

/// [`braille_char`] as a `&'static str`, so a cell's span can borrow its
/// text instead of allocating a `String` for one character.
pub fn braille_str(mask: u8) -> &'static str {
    let i = mask as usize * 3;
    std::str::from_utf8(&BRAILLE_UTF8[i..i + 3]).expect("the table holds valid UTF-8")
}

/// Perceptual brightness, same weights as the mockup.
fn luma(c: [u8; 3]) -> f32 {
    0.3 * c[0] as f32 + 0.59 * c[1] as f32 + 0.11 * c[2] as f32
}

fn color(c: [u8; 3]) -> Color {
    Color::Rgb(c[0], c[1], c[2])
}

/// Mean of a set of pixels, truncated per channel like the mockup's `| 0`.
fn mean_rgb(px: &[[u8; 3]]) -> [u8; 3] {
    let n = px.len().max(1) as u32;
    let sum = |k: usize| px.iter().map(|p| p[k] as u32).sum::<u32>() / n;
    [sum(0) as u8, sum(1) as u8, sum(2) as u8]
}

/// Append a cell, extending the previous span when the style is identical so a
/// flat region becomes one span rather than one per cell.
///
/// A new span borrows `text` (no allocation). Only a run of two or more cells
/// of one style gets an owned `String`, once, when its second cell arrives.
fn push_cell(spans: &mut Vec<Span<'static>>, text: &'static str, style: Style) {
    if let Some(last) = spans.last_mut() {
        if last.style == style {
            last.content.to_mut().push_str(text);
            return;
        }
    }
    spans.push(Span::styled(text, style));
}

/// The transparent cell: a space on the terminal's own background, with no
/// foreground colour either.
fn transparent_style() -> Style {
    Style::default().bg(Color::Reset)
}

/// Braille lines, one per 4 pixel rows. Needs `w % 2 == 0` and `h % 4 == 0`.
///
/// Each 2x4 block is one cell with two colours. Pixels brighter than the block
/// mean become dots in the foreground colour; the rest show as background.
/// A near-flat block (luma spread <= 14) is a space on the mean colour.
///
/// Uncovered pixels are transparent. A block with none covered is a space on
/// `Color::Reset`. In a partly covered block an uncovered pixel counts as
/// black for the threshold (so the covered pixels become the dots) but never
/// enters a colour mean; the background is `Color::Reset` when every non-dot
/// pixel is uncovered, and the mean of the covered non-dot pixels otherwise.
/// A partly covered block always takes the dot form, because a flat fill would
/// paint its uncovered pixels.
///
/// A cell's pixels are gathered into fixed arrays, so the conversion
/// allocates per line (the span list), never per cell.
pub fn to_braille_lines(buf: &PixelBuf) -> Vec<Line<'static>> {
    debug_assert!(
        buf.w % 2 == 0 && buf.h % 4 == 0,
        "braille needs w%2==0, h%4==0"
    );
    let mut lines = Vec::with_capacity(buf.h / 4);
    for cy in 0..buf.h / 4 {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(buf.w / 2);
        for cx in 0..buf.w / 2 {
            // The block's pixels in dot order (row, then column): colour,
            // luma (0 when uncovered) and coverage.
            let mut px = [[0u8; 3]; 8];
            let mut lum = [0f32; 8];
            let mut cov = [false; 8];
            let mut n_covered = 0;
            for r in 0..4 {
                let row = (cy * 4 + r) * buf.w + cx * 2;
                for c in 0..2 {
                    let i = r * 2 + c;
                    px[i] = buf.rgb[row + c];
                    cov[i] = buf.cover[row + c];
                    n_covered += cov[i] as usize;
                    lum[i] = if cov[i] { luma(px[i]) } else { 0.0 };
                }
            }
            if n_covered == 0 {
                push_cell(&mut spans, " ", transparent_style());
                continue;
            }
            let mean = lum.iter().sum::<f32>() / 8.0;
            let min = lum.iter().copied().fold(f32::MAX, f32::min);
            let max = lum.iter().copied().fold(f32::MIN, f32::max);

            if n_covered == 8 && max - min <= UNIFORM_LUMA_SPREAD {
                push_cell(&mut spans, " ", Style::default().bg(color(mean_rgb(&px))));
                continue;
            }
            let (mut lit, mut n_lit) = ([[0u8; 3]; 8], 0);
            let (mut dark, mut n_dark) = ([[0u8; 3]; 8], 0);
            let mut mask = 0u8;
            for i in 0..8 {
                if cov[i] && lum[i] > mean {
                    lit[n_lit] = px[i];
                    n_lit += 1;
                    mask |= DOT_BIT[i / 2][i % 2];
                } else if cov[i] {
                    dark[n_dark] = px[i];
                    n_dark += 1;
                }
            }
            if n_lit == 0 {
                // Only reachable when the covered pixels are all pure black
                // beside uncovered ones: nothing is above the mean. Draw them
                // all as dots rather than fabricate a split.
                for i in 0..8 {
                    if cov[i] {
                        lit[n_lit] = px[i];
                        n_lit += 1;
                        mask |= DOT_BIT[i / 2][i % 2];
                    }
                }
                n_dark = 0;
            }
            // Fully covered: the spread exceeds 14, so some pixel is above the
            // mean and some is not, and `dark` is non-empty. Partly covered:
            // `dark` is empty exactly when every non-dot pixel is uncovered.
            let bg = if n_dark == 0 {
                Color::Reset
            } else {
                color(mean_rgb(&dark[..n_dark]))
            };
            let style = Style::default().fg(color(mean_rgb(&lit[..n_lit]))).bg(bg);
            push_cell(&mut spans, braille_str(mask), style);
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Half-block lines, one per 2 pixel rows. Needs `h % 2 == 0`.
///
/// `▀` paints its upper half in the foreground and the lower half shows the
/// background, so foreground = upper pixel and background = lower pixel.
/// An uncovered half is left to the terminal: with only the upper half covered
/// the cell is `▀` on `Color::Reset`, with only the lower half covered it is
/// `▄` (the foreground is the lower half) on `Color::Reset`, and with neither
/// it is a space on `Color::Reset`.
pub fn to_halfblock_lines(buf: &PixelBuf) -> Vec<Line<'static>> {
    debug_assert!(buf.h % 2 == 0, "half-block needs h%2==0");
    let mut lines = Vec::with_capacity(buf.h / 2);
    for cy in 0..buf.h / 2 {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(buf.w);
        for x in 0..buf.w {
            let (up, down) = (cy * 2, cy * 2 + 1);
            match (buf.covered(x, up), buf.covered(x, down)) {
                (true, true) => {
                    let style = Style::default()
                        .fg(color(buf.get(x, up)))
                        .bg(color(buf.get(x, down)));
                    push_cell(&mut spans, "▀", style);
                }
                (true, false) => push_cell(
                    &mut spans,
                    "▀",
                    Style::default().fg(color(buf.get(x, up))).bg(Color::Reset),
                ),
                (false, true) => push_cell(
                    &mut spans,
                    "▄",
                    Style::default()
                        .fg(color(buf.get(x, down)))
                        .bg(Color::Reset),
                ),
                (false, false) => push_cell(&mut spans, " ", transparent_style()),
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// How the picture is turned into terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellStyle {
    /// Braille dots: 2 x 4 pixels a cell, the finest.
    Braille,
    /// Quarter blocks (`▘▝▀▖▌▞▛▗▚▐▜▄▙▟█`): the same 2 x 4 pixels, each pair of
    /// rows merged, so a cell shows 2 x 2. The picture is the same size and
    /// shape as braille, only chunkier. A block form, so it draws in any font
    /// that has the block elements, which braille needs more of.
    Quadrant,
    /// Half blocks (`▀▄`): 1 x 2 pixels a cell.
    HalfBlock,
}

impl CellStyle {
    /// The pixels a cell covers in the buffer the scene is drawn into: (wide,
    /// tall). Quarter blocks use the braille grid so the picture keeps its
    /// proportions (a 2 x 2 grid would make the pixels twice as tall as wide).
    pub fn px_per_cell(self) -> (usize, usize) {
        match self {
            CellStyle::Braille | CellStyle::Quadrant => (2, 4),
            CellStyle::HalfBlock => (1, 2),
        }
    }

    /// Whether this is one of the block forms (not braille).
    pub fn is_block(self) -> bool {
        !matches!(self, CellStyle::Braille)
    }

    /// The word the footer and `/cells` use.
    pub fn label(self) -> &'static str {
        match self {
            CellStyle::Braille => "braille",
            CellStyle::Quadrant => "quarter",
            CellStyle::HalfBlock => "half-block",
        }
    }

    /// `buf` as terminal lines in this style.
    pub fn lines(self, buf: &PixelBuf) -> Vec<Line<'static>> {
        match self {
            CellStyle::Braille => to_braille_lines(buf),
            CellStyle::Quadrant => to_quadrant_lines(buf),
            CellStyle::HalfBlock => to_halfblock_lines(buf),
        }
    }
}

/// The quarter-block character for a mask of lit quarters: 1 upper left, 2
/// upper right, 4 lower left, 8 lower right.
const QUADRANT: [&str; 16] = [
    " ", "\u{2598}", "\u{259D}", "\u{2580}", "\u{2596}", "\u{258C}", "\u{259E}", "\u{259B}",
    "\u{2597}", "\u{259A}", "\u{2590}", "\u{259C}", "\u{2584}", "\u{2599}", "\u{259F}", "\u{2588}",
];

/// Quarter-block lines, one per 4 pixel rows. Needs `w % 2 == 0` and
/// `h % 4 == 0`: the buffer is the braille grid, and each cell's four quarters
/// are the 1 x 2 pixel groups of its 2 x 4 block.
///
/// The rule is the braille one on four values instead of eight: quarters
/// brighter than the cell's mean become the foreground block glyph, the rest
/// the background; a flat, fully covered cell is a space on the mean colour;
/// uncovered quarters are transparent (a cell with none covered is a space on
/// `Color::Reset`, and a partly covered one takes the glyph form so its
/// uncovered quarters are not painted). A quarter is covered when either of its
/// pixels is, and is the mean of the covered ones.
pub fn to_quadrant_lines(buf: &PixelBuf) -> Vec<Line<'static>> {
    debug_assert!(
        buf.w % 2 == 0 && buf.h % 4 == 0,
        "quarter blocks need w%2==0, h%4==0"
    );
    let mut lines = Vec::with_capacity(buf.h / 4);
    for cy in 0..buf.h / 4 {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(buf.w / 2);
        for cx in 0..buf.w / 2 {
            // Quarters in mask order: UL, UR, LL, LR.
            let mut px = [[0u8; 3]; 4];
            let mut lum = [0f32; 4];
            let mut cov = [false; 4];
            let mut n_covered = 0;
            for q in 0..4 {
                let (qx, qy) = (q % 2, q / 2);
                let mut group = [[0u8; 3]; 2];
                let mut n = 0;
                for r in 0..2 {
                    let (x, y) = (cx * 2 + qx, cy * 4 + qy * 2 + r);
                    if buf.covered(x, y) {
                        group[n] = buf.get(x, y);
                        n += 1;
                    }
                }
                if n > 0 {
                    cov[q] = true;
                    n_covered += 1;
                    px[q] = mean_rgb(&group[..n]);
                    lum[q] = luma(px[q]);
                }
            }
            if n_covered == 0 {
                push_cell(&mut spans, " ", transparent_style());
                continue;
            }
            let mean = lum.iter().sum::<f32>() / 4.0;
            let min = lum.iter().copied().fold(f32::MAX, f32::min);
            let max = lum.iter().copied().fold(f32::MIN, f32::max);
            if n_covered == 4 && max - min <= UNIFORM_LUMA_SPREAD {
                push_cell(&mut spans, " ", Style::default().bg(color(mean_rgb(&px))));
                continue;
            }
            let (mut lit, mut n_lit) = ([[0u8; 3]; 4], 0);
            let (mut dark, mut n_dark) = ([[0u8; 3]; 4], 0);
            let mut mask = 0usize;
            for q in 0..4 {
                if cov[q] && lum[q] > mean {
                    lit[n_lit] = px[q];
                    n_lit += 1;
                    mask |= 1 << q;
                } else if cov[q] {
                    dark[n_dark] = px[q];
                    n_dark += 1;
                }
            }
            if n_lit == 0 {
                // Only reachable when the covered quarters are all pure black
                // beside uncovered ones: draw them all as the glyph; there is no split to make.
                for q in 0..4 {
                    if cov[q] {
                        lit[n_lit] = px[q];
                        n_lit += 1;
                        mask |= 1 << q;
                    }
                }
                n_dark = 0;
            }
            let bg = if n_dark == 0 {
                Color::Reset
            } else {
                color(mean_rgb(&dark[..n_dark]))
            };
            let style = Style::default().fg(color(mean_rgb(&lit[..n_lit]))).bg(bg);
            push_cell(&mut spans, QUADRANT[mask], style);
        }
        lines.push(Line::from(spans));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: [u8; 3]) -> Color {
        Color::Rgb(c[0], c[1], c[2])
    }

    #[test]
    fn braille_char_covers_the_empty_and_full_patterns() {
        assert_eq!(braille_char(0x00), '\u{2800}');
        assert_eq!(braille_char(0xFF), '⣿');
    }

    #[test]
    fn braille_str_is_braille_char_for_every_mask() {
        for m in 0..=255u8 {
            assert_eq!(braille_str(m), braille_char(m).to_string(), "{m:#x}");
        }
    }

    #[test]
    fn left_column_lit_sets_dots_1_2_3_7() {
        assert_eq!(braille_char(0x47), '\u{2847}');
    }

    #[test]
    fn a_uniform_block_is_a_space_on_the_mean_background() {
        let mut b = PixelBuf::new(2, 4);
        // Luma spread of 10 (<= 14), so the cell counts as uniform.
        for y in 0..4 {
            for x in 0..2 {
                b.put(
                    x,
                    y,
                    if (x + y) % 2 == 0 {
                        [100, 100, 100]
                    } else {
                        [110, 110, 110]
                    },
                );
            }
        }
        let lines = to_braille_lines(&b);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " ");
        assert_eq!(lines[0].spans[0].style.bg, Some(rgb([105, 105, 105])));
    }

    #[test]
    fn a_split_block_uses_a_bright_foreground_and_a_dark_background() {
        let mut b = PixelBuf::new(2, 4);
        // Left column bright, right column dark.
        for y in 0..4 {
            b.put(0, y, [200, 180, 160]);
            b.put(1, y, [10, 20, 30]);
        }
        let lines = to_braille_lines(&b);
        let s = &lines[0].spans[0];
        // Left column rows 0..3 = 0x01 | 0x02 | 0x04 | 0x40.
        assert_eq!(s.content.as_ref(), "\u{2847}");
        assert_eq!(s.style.fg, Some(rgb([200, 180, 160])));
        assert_eq!(s.style.bg, Some(rgb([10, 20, 30])));
    }

    #[test]
    fn braille_lines_have_w_over_2_by_h_over_4_cells() {
        let mut b = PixelBuf::new(8, 8);
        // Vary pixels so adjacent cells differ and spans are not merged away.
        for y in 0..8 {
            for x in 0..8 {
                b.put(x as i32, y as i32, [(x * 30) as u8, (y * 30) as u8, 0]);
            }
        }
        let lines = to_braille_lines(&b);
        assert_eq!(lines.len(), 2);
        for l in &lines {
            let n: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(n, 4);
        }
    }

    #[test]
    fn halfblock_uses_upper_pixel_as_foreground() {
        let mut b = PixelBuf::new(1, 2);
        b.put(0, 0, [1, 2, 3]);
        b.put(0, 1, [4, 5, 6]);
        let lines = to_halfblock_lines(&b);
        assert_eq!(lines.len(), 1);
        let s = &lines[0].spans[0];
        assert_eq!(s.content.as_ref(), "▀");
        assert_eq!(s.style.fg, Some(rgb([1, 2, 3])));
        assert_eq!(s.style.bg, Some(rgb([4, 5, 6])));
    }

    fn text_of(lines: &[Line<'static>]) -> String {
        lines[0].spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn an_uncovered_braille_cell_is_a_reset_background_space() {
        let b = PixelBuf::new(2, 4);
        let lines = to_braille_lines(&b);
        assert_eq!(text_of(&lines), " ");
        let st = lines[0].spans[0].style;
        assert_eq!(st.bg, Some(Color::Reset));
        assert_eq!(st.fg, None);
    }

    #[test]
    fn a_braille_cell_with_only_lit_pixels_covered_has_a_reset_background() {
        let mut b = PixelBuf::new(2, 4);
        // Left column bright and covered, right column left uncovered.
        for y in 0..4 {
            b.put(0, y, [200, 180, 160]);
        }
        let lines = to_braille_lines(&b);
        let s = &lines[0].spans[0];
        assert_eq!(s.style.fg, Some(rgb([200, 180, 160])));
        assert_eq!(s.style.bg, Some(Color::Reset));
        // Only the covered pixels are lit; the uncovered ones are not dots.
        assert_eq!(s.content.as_ref(), "\u{2847}");
    }

    #[test]
    fn uncovered_pixels_do_not_enter_the_cell_means() {
        let mut b = PixelBuf::new(2, 4);
        for y in 0..4 {
            b.put(0, y, [200, 180, 160]);
            b.put(1, y, [10, 20, 30]);
        }
        // Make two dark pixels uncovered but stuffed with loud colour data: if
        // they entered the mean the background would shift.
        b.rgb[1] = [250, 0, 250];
        b.cover[1] = false;
        b.rgb[3] = [250, 0, 250];
        b.cover[3] = false;
        let lines = to_braille_lines(&b);
        let s = &lines[0].spans[0];
        assert_eq!(s.style.fg, Some(rgb([200, 180, 160])));
        assert_eq!(s.style.bg, Some(rgb([10, 20, 30])));
    }

    #[test]
    fn half_block_uses_reset_for_an_uncovered_half() {
        // Both halves uncovered.
        let b = PixelBuf::new(1, 2);
        let s = &to_halfblock_lines(&b)[0].spans[0];
        assert_eq!(s.content.as_ref(), " ");
        assert_eq!((s.style.fg, s.style.bg), (None, Some(Color::Reset)));
        // Only the upper half covered.
        let mut b = PixelBuf::new(1, 2);
        b.put(0, 0, [1, 2, 3]);
        let s = &to_halfblock_lines(&b)[0].spans[0];
        assert_eq!(s.content.as_ref(), "▀");
        assert_eq!(s.style.fg, Some(rgb([1, 2, 3])));
        assert_eq!(s.style.bg, Some(Color::Reset));
        // Only the lower half covered.
        let mut b = PixelBuf::new(1, 2);
        b.put(0, 1, [4, 5, 6]);
        let s = &to_halfblock_lines(&b)[0].spans[0];
        assert_eq!(s.content.as_ref(), "▄");
        assert_eq!(s.style.fg, Some(rgb([4, 5, 6])));
        assert_eq!(s.style.bg, Some(Color::Reset));
    }

    /// A 2 x 4 buffer with the given quarters lit white (UL, UR, LL, LR order
    /// = mask bits 1, 2, 4, 8) on black, every pixel covered.
    fn quarters(mask: usize) -> PixelBuf {
        let mut b = PixelBuf::new(2, 4);
        for q in 0..4 {
            let on = mask >> q & 1 == 1;
            for r in 0..2 {
                b.put(
                    (q % 2) as i32,
                    ((q / 2) * 2 + r) as i32,
                    if on { [250, 250, 250] } else { [0, 0, 0] },
                );
            }
        }
        b
    }

    fn glyph(buf: &PixelBuf) -> String {
        to_quadrant_lines(buf)[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn each_of_the_sixteen_quarter_patterns_draws_its_block_glyph() {
        // 0 is all black and flat: a space on black. 15 is all white and flat:
        // a space on white. The other fourteen are real glyphs.
        let want = [
            " ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", " ",
        ];
        for (mask, w) in want.iter().enumerate() {
            assert_eq!(glyph(&quarters(mask)), *w, "mask {mask:#06b}");
        }
        // The flat cells differ by their background.
        let bg = |m| to_quadrant_lines(&quarters(m))[0].spans[0].style.bg;
        assert_eq!(bg(0), Some(rgb([0, 0, 0])));
        assert_eq!(bg(15), Some(rgb([250, 250, 250])));
        // And a lit quarter is the foreground over the dark background.
        let s = &to_quadrant_lines(&quarters(0b0011))[0].spans[0];
        assert_eq!(
            (s.content.as_ref(), s.style.fg, s.style.bg),
            ("▀", Some(rgb([250, 250, 250])), Some(rgb([0, 0, 0])))
        );
    }

    #[test]
    fn a_quarter_is_the_mean_of_its_two_pixel_rows() {
        let mut b = PixelBuf::new(2, 4);
        // Upper left: 200 and 100 average to 150; the other quarters are dark.
        b.put(0, 0, [200, 200, 200]);
        b.put(0, 1, [100, 100, 100]);
        for (x, y) in [(1, 0), (1, 1), (0, 2), (0, 3), (1, 2), (1, 3)] {
            b.put(x, y, [0, 0, 0]);
        }
        let s = &to_quadrant_lines(&b)[0].spans[0];
        assert_eq!(s.content.as_ref(), "▘");
        assert_eq!(s.style.fg, Some(rgb([150, 150, 150])));
    }

    #[test]
    fn uncovered_quarters_are_transparent() {
        // Nothing covered: a space on the terminal's own background.
        let s = &to_quadrant_lines(&PixelBuf::new(2, 4))[0].spans[0];
        assert_eq!((s.content.as_ref(), s.style.bg), (" ", Some(Color::Reset)));
        // Only the lower right covered: that block, background left to the
        // terminal, never painted black.
        let mut b = PixelBuf::new(2, 4);
        b.put(1, 2, [90, 120, 200]);
        let s = &to_quadrant_lines(&b)[0].spans[0];
        assert_eq!(s.content.as_ref(), "▗");
        assert_eq!(s.style.fg, Some(rgb([90, 120, 200])));
        assert_eq!(s.style.bg, Some(Color::Reset));
        // One covered pixel of a pair is enough to cover that quarter.
        let mut b = PixelBuf::new(2, 4);
        b.put(0, 1, [40, 40, 40]);
        assert_eq!(glyph(&b), "▘");
    }

    #[test]
    fn quarter_lines_have_w_over_2_by_h_over_4_cells_and_the_style_table_agrees() {
        let b = PixelBuf::new(8, 12);
        let lines = to_quadrant_lines(&b);
        assert_eq!(lines.len(), 3);
        for l in &lines {
            assert_eq!(
                l.spans
                    .iter()
                    .map(|s| s.content.chars().count())
                    .sum::<usize>(),
                4
            );
        }
        for style in [
            CellStyle::Braille,
            CellStyle::Quadrant,
            CellStyle::HalfBlock,
        ] {
            let (pw, ph) = style.px_per_cell();
            let buf = PixelBuf::new(pw * 5, ph * 3);
            let lines = style.lines(&buf);
            assert_eq!(lines.len(), 3, "{style:?}");
            assert_eq!(style.is_block(), style != CellStyle::Braille);
        }
        assert_eq!(
            CellStyle::Quadrant.px_per_cell(),
            CellStyle::Braille.px_per_cell()
        );
    }
}
