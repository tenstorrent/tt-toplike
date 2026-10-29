// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Convert a `PixelBuf` into terminal cells: braille (2x4 pixels per cell) or
//! half-block (1x2 pixels per cell). Both emit truecolour ratatui spans.

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
fn push_cell(spans: &mut Vec<Span<'static>>, text: char, style: Style) {
    if let Some(last) = spans.last_mut() {
        if last.style == style {
            last.content.to_mut().push(text);
            return;
        }
    }
    spans.push(Span::styled(text.to_string(), style));
}

/// Braille lines, one per 4 pixel rows. Needs `w % 2 == 0` and `h % 4 == 0`.
///
/// Each 2x4 block is one cell with two colours. Pixels brighter than the block
/// mean become dots in the foreground colour; the rest show as background.
/// A near-flat block (luma spread <= 14) is a space on the mean colour.
pub fn to_braille_lines(buf: &PixelBuf) -> Vec<Line<'static>> {
    debug_assert!(buf.w % 2 == 0 && buf.h % 4 == 0, "braille needs w%2==0, h%4==0");
    let mut lines = Vec::with_capacity(buf.h / 4);
    for cy in 0..buf.h / 4 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for cx in 0..buf.w / 2 {
            // Gather the block as (row, col, pixel, luma).
            let mut cell = [(0usize, 0usize, [0u8; 3], 0f32); 8];
            for r in 0..4 {
                for c in 0..2 {
                    let p = buf.get(cx * 2 + c, cy * 4 + r);
                    cell[r * 2 + c] = (r, c, p, luma(p));
                }
            }
            let mean = cell.iter().map(|t| t.3).sum::<f32>() / 8.0;
            let min = cell.iter().map(|t| t.3).fold(f32::MAX, f32::min);
            let max = cell.iter().map(|t| t.3).fold(f32::MIN, f32::max);

            if max - min <= UNIFORM_LUMA_SPREAD {
                let all: Vec<[u8; 3]> = cell.iter().map(|t| t.2).collect();
                push_cell(&mut spans, ' ', Style::default().bg(color(mean_rgb(&all))));
                continue;
            }
            let (mut lit, mut dark) = (Vec::new(), Vec::new());
            let mut mask = 0u8;
            for &(r, c, p, l) in &cell {
                if l > mean {
                    lit.push(p);
                    mask |= DOT_BIT[r][c];
                } else {
                    dark.push(p);
                }
            }
            // The spread exceeds 14, so some pixel is above the mean and some
            // is not; both sets are non-empty.
            let style = Style::default().fg(color(mean_rgb(&lit))).bg(color(mean_rgb(&dark)));
            push_cell(&mut spans, braille_char(mask), style);
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Half-block lines, one per 2 pixel rows. Needs `h % 2 == 0`.
///
/// `▀` paints its upper half in the foreground and the lower half shows the
/// background, so foreground = upper pixel and background = lower pixel.
pub fn to_halfblock_lines(buf: &PixelBuf) -> Vec<Line<'static>> {
    debug_assert!(buf.h % 2 == 0, "half-block needs h%2==0");
    let mut lines = Vec::with_capacity(buf.h / 2);
    for cy in 0..buf.h / 2 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for x in 0..buf.w {
            let style = Style::default()
                .fg(color(buf.get(x, cy * 2)))
                .bg(color(buf.get(x, cy * 2 + 1)));
            push_cell(&mut spans, '▀', style);
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
    fn left_column_lit_sets_dots_1_2_3_7() {
        assert_eq!(braille_char(0x47), '\u{2847}');
    }

    #[test]
    fn a_uniform_block_is_a_space_on_the_mean_background() {
        let mut b = PixelBuf::new(2, 4);
        // Luma spread of 10 (<= 14), so the cell counts as uniform.
        for y in 0..4 {
            for x in 0..2 {
                b.put(x, y, if (x + y) % 2 == 0 { [100, 100, 100] } else { [110, 110, 110] });
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
}
