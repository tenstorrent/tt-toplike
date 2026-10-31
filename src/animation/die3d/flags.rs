// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pin-and-flag labels: where each die's name goes so that no text lies over
//! the picture.
//!
//! A flag is a pole of `│` cells standing on a pin (`●`) at the rim of its
//! die, with the label at the other end of the pole: `┌ BH0 44W` for a flag
//! flying to the right of its pole, `BH0 44W ┐` to the left. The text is
//! always on open space, outside the outline of everything the scene draws
//! (the board's slab covers the whole grid, not only the dies). The pole is a
//! one-cell line that may cross the board on its way out, as the pin has to
//! stand on the die; it stops at the nearest open space above or below.
//!
//! The view works in terminal cells. The outline is the convex hull of the
//! board grid's corner blocks as the frame was projected (so it follows
//! quarter turns and any spin), plus the blocks' own size and a margin.

use std::collections::HashSet;

/// Where the pin and the text of one flag go. Cells are (column, row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Flag {
    /// The pin: the die's rim.
    pub pin: (i32, i32),
    /// Rows from the pin to the text row. Positive: the pole stands up from
    /// the pin (the text is above it); negative: it hangs down.
    pub rise: i32,
    /// The text flies to the left of the pole when set, else to the right.
    pub left: bool,
    pub text: String,
}

impl Flag {
    /// Every cell of the flag with the character that goes in it: the pin,
    /// the pole, the corner, one space of padding on each side of the text
    /// and the text.
    pub(super) fn cells(&self) -> Vec<((i32, i32), char)> {
        let (pc, pr) = self.pin;
        let text_row = pr - self.rise;
        let mut out = vec![((pc, pr), '●')];
        for k in 1..self.rise.abs() {
            out.push(((pc, pr - self.rise.signum() * k), '│'));
        }
        let corner = match (self.rise > 0, self.left) {
            (true, false) => '┌',
            (true, true) => '┐',
            (false, false) => '└',
            (false, true) => '┘',
        };
        out.push(((pc, text_row), corner));
        let n = self.text.chars().count() as i32;
        for (i, ch) in self.text.chars().enumerate() {
            let i = i as i32;
            let col = if self.left {
                pc - 1 - n + i
            } else {
                pc + 2 + i
            };
            out.push(((col, text_row), ch));
        }
        // The cell next to the pole and one blank beyond the far end of the
        // text are part of the flag: they keep the text off whatever is there.
        let (pad, far) = if self.left {
            (pc - 1, pc - 2 - n)
        } else {
            (pc + 1, pc + 2 + n)
        };
        out.push(((pad, text_row), ' '));
        out.push(((far, text_row), ' '));
        out
    }
}

/// The outline of what the scene draws: a convex polygon in cell units
/// (column, row as fractions of a cell).
#[derive(Debug, Clone)]
pub(super) struct Hull {
    points: Vec<(f32, f32)>,
}

impl Hull {
    /// The convex hull of `points` (Andrew's monotone chain).
    pub(super) fn of(points: &[(f32, f32)]) -> Self {
        let mut p: Vec<(f32, f32)> = points.to_vec();
        p.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        p.dedup();
        if p.len() < 3 {
            return Self { points: p };
        }
        let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
            (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
        };
        let mut hull: Vec<(f32, f32)> = Vec::with_capacity(p.len() * 2);
        for &q in &p {
            while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0 {
                hull.pop();
            }
            hull.push(q);
        }
        let lower = hull.len() + 1;
        for &q in p.iter().rev().skip(1) {
            while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0
            {
                hull.pop();
            }
            hull.push(q);
        }
        hull.pop();
        Self { points: hull }
    }

    /// Whether the cell (`col`, `row`) has its centre inside the hull.
    pub(super) fn covers(&self, col: i32, row: i32) -> bool {
        let n = self.points.len();
        if n < 3 {
            return false;
        }
        let (px, py) = (col as f32 + 0.5, row as f32 + 0.5);
        (0..n).all(|i| {
            let (ax, ay) = self.points[i];
            let (bx, by) = self.points[(i + 1) % n];
            (bx - ax) * (py - ay) - (by - ay) * (px - ax) >= 0.0
        })
    }
}

/// One die to flag: its text candidates (the full label first, then a
/// shorter one) and the two cells a pin can stand on, the die's top corner
/// for a pole that goes up and its bottom corner for one that goes down.
#[derive(Debug, Clone)]
pub(super) struct DieFlag {
    pub texts: Vec<String>,
    pub top: (i32, i32),
    pub bottom: (i32, i32),
}

/// Place one flag for every die on a `cols` x `rows` area of cells, in the
/// order given; no two flags share a cell.
///
/// For each die the shortest pole wins, up or down, text to the right or the
/// left, with the full text before the short one at every length. The text,
/// the corner and the padding must be on screen, outside `hull` and on cells
/// no earlier flag uses; the pole only has to be on screen and free, so it
/// may cross the board. A die with no such place gets no flag.
pub(super) fn place(dies: &[DieFlag], hull: &Hull, cols: usize, rows: usize) -> Vec<Option<Flag>> {
    let mut used: HashSet<(i32, i32)> = HashSet::new();
    let mut flags = Vec::with_capacity(dies.len());
    for die in dies {
        let found = (2..=rows as i32).find_map(|len| {
            die.texts.iter().find_map(|text| {
                [(len, die.top), (-len, die.bottom)]
                    .into_iter()
                    .flat_map(|(rise, pin)| [false, true].map(move |left| (rise, pin, left)))
                    .map(|(rise, pin, left)| Flag {
                        pin,
                        rise,
                        left,
                        text: text.clone(),
                    })
                    .find(|f| fits(f, hull, &used, cols, rows))
            })
        });
        if let Some(f) = &found {
            used.extend(f.cells().into_iter().map(|(at, _)| at));
        }
        flags.push(found);
    }
    flags
}

/// Whether every cell of `flag` is on screen and unused, and every cell but
/// the pin and the pole is outside `hull`.
fn fits(flag: &Flag, hull: &Hull, used: &HashSet<(i32, i32)>, cols: usize, rows: usize) -> bool {
    flag.cells().into_iter().all(|((c, r), ch)| {
        let on_screen = (0..cols as i32).contains(&c) && (0..rows as i32).contains(&r);
        if !on_screen || used.contains(&(c, r)) {
            return false;
        }
        matches!(ch, '●' | '│') || !hull.covers(c, r)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A diamond hull centred at (`cx`, `cy`), `w` wide and `h` tall.
    fn diamond(cx: f32, cy: f32, w: f32, h: f32) -> Hull {
        Hull::of(&[
            (cx, cy - h / 2.0),
            (cx + w / 2.0, cy),
            (cx, cy + h / 2.0),
            (cx - w / 2.0, cy),
        ])
    }

    fn die(text: &str, top: (i32, i32), bottom: (i32, i32)) -> DieFlag {
        DieFlag {
            texts: vec![text.to_string()],
            top,
            bottom,
        }
    }

    #[test]
    fn a_hull_covers_its_middle_and_not_a_far_cell() {
        let d = diamond(20.0, 10.0, 20.0, 10.0);
        assert!(d.covers(20, 10));
        assert!(!d.covers(2, 10));
        assert!(!d.covers(20, 0));
        // The corners of the bounding box are outside a diamond.
        assert!(!d.covers(11, 5));
    }

    #[test]
    fn the_hull_of_extra_inner_points_is_the_same_hull() {
        let outer = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let mut more = outer.to_vec();
        more.extend([(5.0, 5.0), (2.0, 7.0), (9.0, 1.0)]);
        let (a, b) = (Hull::of(&outer), Hull::of(&more));
        for c in -2..14 {
            for r in -2..14 {
                assert_eq!(a.covers(c, r), b.covers(c, r), "{c},{r}");
            }
        }
    }

    #[test]
    fn a_flag_has_its_text_outside_the_board() {
        let hull = diamond(40.0, 20.0, 40.0, 16.0);
        // The die's top corner is inside the hull, 3 rows below its edge.
        let top = (40, 15);
        let flags = place(&[die("BH0 44W", top, (40, 25))], &hull, 100, 40);
        let f = flags[0].as_ref().expect("a place outside the board");
        assert!(f.pin == top || f.pin == (40, 25), "{f:?}");
        for ((c, r), ch) in f.cells() {
            if !matches!(ch, '●' | '│') {
                assert!(!hull.covers(c, r), "{ch} at ({c},{r}) is on the board");
            }
        }
    }

    #[test]
    fn the_shortest_pole_wins() {
        let hull = diamond(40.0, 20.0, 40.0, 16.0);
        // The hull spans rows 12 to 28. From a top pin at row 24 the pole must
        // climb 12+ rows; from a bottom pin at row 25 only a few.
        let flags = place(&[die("BH0", (40, 24), (40, 25))], &hull, 100, 40);
        let f = flags[0].as_ref().unwrap();
        assert!(f.rise < 0, "down is shorter here: {f:?}");
    }

    #[test]
    fn a_flag_hangs_below_when_there_is_no_room_above() {
        let hull = diamond(40.0, 7.0, 30.0, 12.0);
        let flags = place(&[die("BH0", (40, 3), (40, 11))], &hull, 100, 40);
        let f = flags[0].as_ref().expect("room below");
        assert!(f.rise < 0, "{f:?}");
    }

    #[test]
    fn flags_never_share_a_cell() {
        let hull = diamond(40.0, 20.0, 40.0, 16.0);
        let flags = place(
            &[
                die("BH0 44W", (40, 15), (40, 25)),
                die("BH1 44W", (40, 15), (40, 25)),
                die("BH2 44W", (41, 15), (41, 25)),
            ],
            &hull,
            100,
            40,
        );
        let mut seen = HashSet::new();
        for f in flags.iter().flatten() {
            for (at, _) in f.cells() {
                assert!(seen.insert(at), "{at:?} used twice");
            }
        }
        assert_eq!(flags.iter().flatten().count(), 3);
    }

    #[test]
    fn the_short_text_is_used_only_when_the_long_one_cannot_be() {
        let hull = diamond(6.0, 20.0, 4.0, 4.0);
        let both = DieFlag {
            texts: vec!["BH0 44W".into(), "BH0".into()],
            top: (6, 18),
            bottom: (6, 22),
        };
        // Wide enough for the long text beside the pole.
        let flags = place(&[both.clone()], &hull, 20, 40);
        assert_eq!(flags[0].as_ref().unwrap().text, "BH0 44W");
        // 8 columns: neither side has room for the long text.
        let flags = place(&[both], &hull, 8, 40);
        assert_eq!(flags[0].as_ref().unwrap().text, "BH0");
    }

    #[test]
    fn nothing_is_placed_when_nothing_is_free() {
        // A hull as large as the screen leaves no free cell for the text.
        let hull = diamond(10.0, 10.0, 200.0, 200.0);
        let flags = place(&[die("BH0", (10, 8), (10, 12))], &hull, 20, 20);
        assert!(flags[0].is_none());
    }

    #[test]
    fn a_flag_has_a_pin_a_pole_a_corner_and_its_text() {
        let f = Flag {
            pin: (10, 10),
            rise: 3,
            left: false,
            text: "BH0".into(),
        };
        let at = |f: &Flag, c: i32, r: i32| {
            f.cells()
                .into_iter()
                .find(|(p, _)| *p == (c, r))
                .map(|(_, ch)| ch)
        };
        assert_eq!(at(&f, 10, 10), Some('●'));
        assert_eq!(at(&f, 10, 9), Some('│'));
        assert_eq!(at(&f, 10, 8), Some('│'));
        assert_eq!(at(&f, 10, 7), Some('┌'));
        assert_eq!(at(&f, 12, 7), Some('B'));
        assert_eq!(at(&f, 15, 7), Some(' '), "a blank after the text");
        let left = Flag {
            left: true,
            ..f.clone()
        };
        let text: String = (6..=8).map(|c| at(&left, c, 7).unwrap()).collect();
        assert_eq!(text, "BH0");
        assert_eq!(at(&left, 10, 7), Some('┐'));
        // Hanging down: the pole runs below the pin.
        let down = Flag { rise: -3, ..f };
        assert_eq!(at(&down, 10, 11), Some('│'));
        assert_eq!(at(&down, 10, 13), Some('└'));
    }
}
