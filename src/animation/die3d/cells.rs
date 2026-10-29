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
pub fn to_braille_lines(buf: &PixelBuf) -> Vec<Line<'static>> {
    debug_assert!(buf.w % 2 == 0 && buf.h % 4 == 0, "braille needs w%2==0, h%4==0");
    let mut lines = Vec::with_capacity(buf.h / 4);
    for cy in 0..buf.h / 4 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for cx in 0..buf.w / 2 {
            // Gather the block as (row, col, pixel, luma, covered).
            let mut cell = [(0usize, 0usize, [0u8; 3], 0f32, false); 8];
            let mut n_covered = 0;
            for r in 0..4 {
                for c in 0..2 {
                    let (x, y) = (cx * 2 + c, cy * 4 + r);
                    let covered = buf.covered(x, y);
                    let p = buf.get(x, y);
                    n_covered += covered as usize;
                    cell[r * 2 + c] = (r, c, p, if covered { luma(p) } else { 0.0 }, covered);
                }
            }
            if n_covered == 0 {
                push_cell(&mut spans, ' ', transparent_style());
                continue;
            }
            let mean = cell.iter().map(|t| t.3).sum::<f32>() / 8.0;
            let min = cell.iter().map(|t| t.3).fold(f32::MAX, f32::min);
            let max = cell.iter().map(|t| t.3).fold(f32::MIN, f32::max);

            if n_covered == 8 && max - min <= UNIFORM_LUMA_SPREAD {
                let all: Vec<[u8; 3]> = cell.iter().map(|t| t.2).collect();
                push_cell(&mut spans, ' ', Style::default().bg(color(mean_rgb(&all))));
                continue;
            }
            let (mut lit, mut dark) = (Vec::new(), Vec::new());
            let mut mask = 0u8;
            for &(r, c, p, l, covered) in &cell {
                if covered && l > mean {
                    lit.push(p);
                    mask |= DOT_BIT[r][c];
                } else if covered {
                    dark.push(p);
                }
            }
            if lit.is_empty() {
                // Only reachable when the covered pixels are all pure black
                // beside uncovered ones: nothing is above the mean. Draw them
                // all as dots rather than fabricate a split.
                for &(r, c, p, _, covered) in &cell {
                    if covered {
                        lit.push(p);
                        mask |= DOT_BIT[r][c];
                    }
                }
                dark.clear();
            }
            // Fully covered: the spread exceeds 14, so some pixel is above the
            // mean and some is not, and `dark` is non-empty. Partly covered:
            // `dark` is empty exactly when every non-dot pixel is uncovered.
            let bg = if dark.is_empty() {
                Color::Reset
            } else {
                color(mean_rgb(&dark))
            };
            let style = Style::default().fg(color(mean_rgb(&lit))).bg(bg);
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
/// An uncovered half is left to the terminal: with only the upper half covered
/// the cell is `▀` on `Color::Reset`, with only the lower half covered it is
/// `▄` (the foreground is the lower half) on `Color::Reset`, and with neither
/// it is a space on `Color::Reset`.
pub fn to_halfblock_lines(buf: &PixelBuf) -> Vec<Line<'static>> {
    debug_assert!(buf.h % 2 == 0, "half-block needs h%2==0");
    let mut lines = Vec::with_capacity(buf.h / 2);
    for cy in 0..buf.h / 2 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for x in 0..buf.w {
            let (up, down) = (cy * 2, cy * 2 + 1);
            match (buf.covered(x, up), buf.covered(x, down)) {
                (true, true) => {
                    let style = Style::default()
                        .fg(color(buf.get(x, up)))
                        .bg(color(buf.get(x, down)));
                    push_cell(&mut spans, '▀', style);
                }
                (true, false) => push_cell(
                    &mut spans,
                    '▀',
                    Style::default().fg(color(buf.get(x, up))).bg(Color::Reset),
                ),
                (false, true) => push_cell(
                    &mut spans,
                    '▄',
                    Style::default()
                        .fg(color(buf.get(x, down)))
                        .bg(Color::Reset),
                ),
                (false, false) => push_cell(&mut spans, ' ', transparent_style()),
            }
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
}
