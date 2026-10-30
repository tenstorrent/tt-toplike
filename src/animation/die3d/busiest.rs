// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Which board the inset shows when every board does not fit: the busiest
//! one, held for a while before it changes.
//!
//! The inset on the Insights screen has room for one board scene at most of
//! its sizes. It shows the board with the highest load, and changes to
//! another board only after that board has been the highest for
//! [`BUSIEST_HOLD_SECS`] without a break. Two boards that take turns at the
//! top every few seconds therefore never move the picture.
//!
//! Nothing here reads a clock: `now` is passed in, as for `Switcher`.

/// Seconds a board must stay strictly the busiest, without a break, before
/// the inset changes to it.
pub(super) const BUSIEST_HOLD_SECS: f64 = 10.0;

/// The busiest board, by mean chip load, with a hold before it changes.
#[derive(Debug, Clone, Default)]
pub(super) struct BusiestBoard {
    /// The board shown. Starts at 0. It can be past the end of the board
    /// list after the list shrinks; the caller clamps it.
    board: usize,
    /// Another board that is strictly the busiest now, and the view time it
    /// became so. Cleared when it stops being the busiest, when the current
    /// board is the busiest again, and by an empty reading.
    candidate: Option<(usize, f64)>,
}

impl BusiestBoard {
    /// Start on board 0, with no candidate.
    pub(super) fn new() -> BusiestBoard {
        BusiestBoard::default()
    }

    /// The board to show.
    pub(super) fn board(&self) -> usize {
        self.board
    }

    /// Feed each board's load at view time `now` (`board_loads[i]` is board
    /// `i`'s mean chip load). A NaN load counts as 0.
    ///
    /// The board with the strictly highest load becomes the candidate. It
    /// takes over once it has stayed the candidate for [`BUSIEST_HOLD_SECS`].
    /// When the highest load is shared by two or more boards, no board is
    /// strictly the highest, so there is no candidate and the current board
    /// stays: ties keep the current board. An empty slice keeps the current
    /// board and drops the candidate.
    pub(super) fn observe(&mut self, now: f64, board_loads: &[f32]) {
        let load = |l: f32| if l.is_nan() { 0.0 } else { l };
        let Some(top) = strictly_highest(board_loads, load) else {
            self.candidate = None;
            return;
        };
        if top == self.board {
            self.candidate = None;
            return;
        }
        let since = match self.candidate {
            Some((b, since)) if b == top => since,
            _ => now,
        };
        if now - since >= BUSIEST_HOLD_SECS {
            self.board = top;
            self.candidate = None;
        } else {
            self.candidate = Some((top, since));
        }
    }
}

/// The index of the one highest value in `loads` after `load` maps it, or
/// `None` when `loads` is empty or the highest value is shared.
fn strictly_highest(loads: &[f32], load: impl Fn(f32) -> f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    let mut shared = false;
    for (i, &l) in loads.iter().enumerate() {
        let l = load(l);
        match best {
            Some((_, b)) if l < b => {}
            Some((_, b)) if l == b => shared = true,
            _ => {
                best = Some((i, l));
                shared = false;
            }
        }
    }
    best.filter(|_| !shared).map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `loads` once a second from `from` to `to` inclusive.
    fn feed(b: &mut BusiestBoard, from: u32, to: u32, loads: &[f32]) {
        for s in from..=to {
            b.observe(s as f64, loads);
        }
    }

    #[test]
    fn a_board_higher_for_9_s_does_not_take_over_and_at_10_s_it_does() {
        let mut b = BusiestBoard::new();
        feed(&mut b, 0, 9, &[0.1, 0.8]);
        assert_eq!(b.board(), 0);
        b.observe(10.0, &[0.1, 0.8]);
        assert_eq!(b.board(), 1);
    }

    #[test]
    fn a_break_in_the_lead_restarts_the_hold() {
        let mut b = BusiestBoard::new();
        // Higher for 6 s, lower once, then higher again for 6 s.
        feed(&mut b, 0, 6, &[0.1, 0.8]);
        b.observe(7.0, &[0.8, 0.1]);
        feed(&mut b, 8, 14, &[0.1, 0.8]);
        assert_eq!(b.board(), 0);
    }

    #[test]
    fn equal_loads_keep_the_current_board() {
        let mut b = BusiestBoard::new();
        feed(&mut b, 0, 30, &[0.5, 0.5]);
        assert_eq!(b.board(), 0);
        // Two other boards tied above the current one: neither is strictly
        // the highest, so neither takes over.
        feed(&mut b, 31, 60, &[0.1, 0.7, 0.7]);
        assert_eq!(b.board(), 0);
    }

    #[test]
    fn empty_input_keeps_the_board_and_drops_the_candidate() {
        let mut b = BusiestBoard::new();
        feed(&mut b, 0, 10, &[0.1, 0.8]);
        assert_eq!(b.board(), 1);
        b.observe(11.0, &[]);
        assert_eq!(b.board(), 1);
        // Board 0 leads from 12 s; an empty reading at 18 s drops it, so
        // at 22 s (10 s after it first led) it has not taken over.
        feed(&mut b, 12, 17, &[0.9, 0.1]);
        b.observe(18.0, &[]);
        feed(&mut b, 19, 22, &[0.9, 0.1]);
        assert_eq!(b.board(), 1);
    }

    #[test]
    fn a_nan_load_counts_as_zero() {
        let mut b = BusiestBoard::new();
        feed(&mut b, 0, 10, &[f32::NAN, 0.2]);
        assert_eq!(b.board(), 1);
        // NaN against 0: a tie, so the current board stays.
        let mut b = BusiestBoard::new();
        feed(&mut b, 0, 20, &[0.0, f32::NAN]);
        assert_eq!(b.board(), 0);
    }
}
