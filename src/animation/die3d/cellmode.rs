// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Which cell style the picture is drawn in, and when it changes by itself.
//!
//! Pure logic: no clock, no I/O. The view passes the time in.
//!
//! In the default `auto` mode the style follows the hardware:
//!
//! - It starts in a style picked from what the hardware reports when the view
//!   opens (power, temperature, memory heat and links of every chip: see
//!   [`hardware_seed`]), so one run does not always start in the same style.
//! - It changes when something happens on the hardware (an event that cuts the
//!   camera), but not within [`MIN_DWELL_SECS`] of the last change, so it is
//!   not frequent. It moves to the next style in the set.
//! - The change shows at the cut's midpoint, where the transition hides it,
//!   not at the moment the event is seen.
//!
//! `blocks` does the same between the two block styles only (quarter and half
//! blocks), and the other modes hold one style.

use super::cells::CellStyle;
use super::scene::ChipInputs;

/// Least seconds between two automatic style changes.
pub const MIN_DWELL_SECS: f64 = 120.0;

/// Which styles the view may use, and whether it moves between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellMode {
    /// Braille, quarter blocks and half blocks, changing with the hardware.
    Auto,
    /// Quarter blocks and half blocks only, changing with the hardware.
    Blocks,
    /// One style, always.
    Fixed(CellStyle),
}

impl CellMode {
    /// The styles this mode moves through, in order.
    pub fn styles(self) -> &'static [CellStyle] {
        match self {
            CellMode::Auto => &[
                CellStyle::Braille,
                CellStyle::Quadrant,
                CellStyle::HalfBlock,
            ],
            CellMode::Blocks => &[CellStyle::Quadrant, CellStyle::HalfBlock],
            CellMode::Fixed(CellStyle::Braille) => &[CellStyle::Braille],
            CellMode::Fixed(CellStyle::Quadrant) => &[CellStyle::Quadrant],
            CellMode::Fixed(CellStyle::HalfBlock) => &[CellStyle::HalfBlock],
        }
    }

    /// Whether the style changes by itself.
    pub fn is_automatic(self) -> bool {
        !matches!(self, CellMode::Fixed(_))
    }

    /// The mode a `/cells` argument names, or `None`.
    pub fn parse(word: &str) -> Option<CellMode> {
        Some(match word {
            "auto" => CellMode::Auto,
            "blocks" | "block" => CellMode::Blocks,
            "braille" => CellMode::Fixed(CellStyle::Braille),
            "quarter" | "quadrant" => CellMode::Fixed(CellStyle::Quadrant),
            "half" | "half-block" => CellMode::Fixed(CellStyle::HalfBlock),
            _ => return None,
        })
    }

    /// The mode after this one, for a bare `/cells`: auto, braille, blocks,
    /// quarter, half, auto.
    pub fn next(self) -> CellMode {
        match self {
            CellMode::Auto => CellMode::Fixed(CellStyle::Braille),
            CellMode::Fixed(CellStyle::Braille) => CellMode::Blocks,
            CellMode::Blocks => CellMode::Fixed(CellStyle::Quadrant),
            CellMode::Fixed(CellStyle::Quadrant) => CellMode::Fixed(CellStyle::HalfBlock),
            CellMode::Fixed(CellStyle::HalfBlock) => CellMode::Auto,
        }
    }

    /// The footer's words for this mode while it draws in `style`. A fixed
    /// style is its own name; an automatic mode says so and what it is in now.
    pub fn state(self, style: CellStyle) -> &'static str {
        match (self, style) {
            (CellMode::Fixed(s), _) => s.label(),
            (CellMode::Auto, CellStyle::Braille) => "auto: braille",
            (CellMode::Auto, CellStyle::Quadrant) => "auto: quarter",
            (CellMode::Auto, CellStyle::HalfBlock) => "auto: half-block",
            (CellMode::Blocks, CellStyle::Quadrant) => "blocks: quarter",
            (CellMode::Blocks, CellStyle::HalfBlock) => "blocks: half-block",
            // Blocks never holds braille; say what is on screen if it ever does.
            (CellMode::Blocks, CellStyle::Braille) => "blocks: braille",
        }
    }
}

/// A number that depends on what the hardware reports right now: every chip's
/// index, power, temperature, memory heat and live links, mixed (FNV-1a). Two
/// opens of the view on a running box differ in at least the power and
/// temperature readings, so they differ here.
pub fn hardware_seed(chips: &[Vec<ChipInputs>]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for c in chips.iter().flatten() {
        mix(c.index as u64);
        // Tenths of a watt and of a degree; a missing reading is its own value.
        mix(c.power_w.map_or(u64::MAX, |w| (w * 10.0).round() as u64));
        mix(c.temp_c.map_or(u64::MAX, |t| (t * 10.0).round() as u64));
        mix(c
            .gddr_heat
            .map_or(u64::MAX, |g| (g * 1000.0).round() as u64));
        mix(c.eth_live);
    }
    h
}

/// The style of a mode's set that `seed` picks.
pub fn start_style(mode: CellMode, seed: u64) -> CellStyle {
    let styles = mode.styles();
    styles[(seed % styles.len() as u64) as usize]
}

/// The style after `current` in `mode`'s set, wrapping; the set's first when
/// `current` is not in it.
pub fn next_style(mode: CellMode, current: CellStyle) -> CellStyle {
    let styles = mode.styles();
    match styles.iter().position(|&s| s == current) {
        Some(i) => styles[(i + 1) % styles.len()],
        None => styles[0],
    }
}

/// The mode, the style in force, and the one waiting to take over.
#[derive(Debug, Clone, Copy)]
pub struct CellState {
    mode: CellMode,
    style: CellStyle,
    /// A style change decided by an event, and the time it shows from.
    pending: Option<(CellStyle, f64)>,
    /// Time of the latest change, or of the view opening or `/cells auto`.
    since: f64,
    /// Whether the hardware has picked the starting style yet.
    seeded: bool,
}

impl CellState {
    /// A state in `mode`. A fixed mode is its style; an automatic one starts
    /// with its set's first style until the hardware picks.
    pub fn new(mode: CellMode) -> CellState {
        CellState {
            mode,
            style: mode.styles()[0],
            pending: None,
            since: 0.0,
            seeded: !mode.is_automatic(),
        }
    }

    pub fn mode(&self) -> CellMode {
        self.mode
    }

    /// The style to draw at `now`: a pending change counts from its time.
    /// Pure, so `render(&self)` can call it between two updates.
    pub fn style_at(&self, now: f64) -> CellStyle {
        match self.pending {
            Some((style, at)) if now >= at => style,
            _ => self.style,
        }
    }

    /// The footer's words for the mode and style at `now`.
    pub fn state_at(&self, now: f64) -> &'static str {
        self.mode.state(self.style_at(now))
    }

    /// Commit a pending change whose time has passed.
    pub fn settle(&mut self, now: f64) {
        if let Some((style, at)) = self.pending {
            if now >= at {
                self.style = style;
                self.pending = None;
            }
        }
    }

    /// The first reading of the hardware: pick the starting style from `seed`.
    /// Does nothing for a fixed mode, or after the first call. True when the
    /// style changed.
    pub fn seed_once(&mut self, now: f64, seed: u64) -> bool {
        if self.seeded {
            return false;
        }
        self.seeded = true;
        self.since = now;
        let picked = start_style(self.mode, seed);
        let changed = picked != self.style;
        self.style = picked;
        changed
    }

    /// The user chose `mode` at `now`: a fixed style shows at once; an
    /// automatic mode keeps the style on screen if its set has it (and starts
    /// its wait over), else moves to the set's first. Any pending change is
    /// dropped.
    pub fn set_mode(&mut self, mode: CellMode, now: f64) {
        self.mode = mode;
        self.pending = None;
        self.since = now;
        // An explicit choice is not left for the hardware to pick later.
        self.seeded = true;
        let styles = mode.styles();
        if !styles.contains(&self.style) {
            self.style = styles[0];
        }
    }

    /// An event cut the camera at `now` and the cut swaps at `swap_at`: move to
    /// the next style from then, unless the mode is fixed, a change is already
    /// waiting, or the last change was less than [`MIN_DWELL_SECS`] ago. True
    /// when a change was scheduled.
    pub fn on_event_cut(&mut self, now: f64, swap_at: f64) -> bool {
        if !self.mode.is_automatic() || self.pending.is_some() || now - self.since < MIN_DWELL_SECS
        {
            return false;
        }
        self.pending = Some((next_style(self.mode, self.style), swap_at));
        self.since = swap_at;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CellStyle::{Braille, HalfBlock, Quadrant};

    fn chip(index: usize, power: Option<f32>, temp: Option<f32>) -> ChipInputs {
        ChipInputs {
            index,
            label: format!("BH{index}"),
            power_w: power,
            arch: crate::models::Architecture::Blackhole,
            load: 0.0,
            pcie_in_bps: None,
            pcie_out_bps: None,
            gddr_heat: None,
            eth_live: 0,
            eth_known: false,
            temp_c: temp,
            enabled_tensix_col: None,
            tile_flow: None,
        }
    }

    #[test]
    fn modes_name_the_styles_they_move_through() {
        assert_eq!(CellMode::Auto.styles(), [Braille, Quadrant, HalfBlock]);
        assert_eq!(CellMode::Blocks.styles(), [Quadrant, HalfBlock]);
        assert!(
            !CellMode::Blocks.styles().contains(&Braille),
            "blocks has no braille"
        );
        for s in [Braille, Quadrant, HalfBlock] {
            assert_eq!(CellMode::Fixed(s).styles(), [s]);
            assert!(!CellMode::Fixed(s).is_automatic());
        }
        assert!(CellMode::Auto.is_automatic() && CellMode::Blocks.is_automatic());
    }

    #[test]
    fn parse_and_next_cover_every_mode() {
        for (word, mode) in [
            ("auto", CellMode::Auto),
            ("blocks", CellMode::Blocks),
            ("block", CellMode::Blocks),
            ("braille", CellMode::Fixed(Braille)),
            ("quarter", CellMode::Fixed(Quadrant)),
            ("quadrant", CellMode::Fixed(Quadrant)),
            ("half", CellMode::Fixed(HalfBlock)),
            ("half-block", CellMode::Fixed(HalfBlock)),
        ] {
            assert_eq!(CellMode::parse(word), Some(mode), "{word}");
        }
        assert_eq!(CellMode::parse("dots"), None);
        // A bare /cells visits all five modes and comes back.
        let mut seen = vec![CellMode::Auto];
        for _ in 0..5 {
            seen.push(seen.last().unwrap().next());
        }
        assert_eq!(seen[5], CellMode::Auto);
        let mut distinct = seen[..5].to_vec();
        distinct.dedup();
        assert_eq!(distinct.len(), 5, "{seen:?}");
    }

    #[test]
    fn the_hardware_seed_follows_the_readings() {
        let a = vec![vec![
            chip(0, Some(41.2), Some(45.0)),
            chip(1, Some(38.0), Some(44.0)),
        ]];
        assert_eq!(
            hardware_seed(&a),
            hardware_seed(&a.clone()),
            "same readings, same seed"
        );
        let warmer = vec![vec![
            chip(0, Some(41.2), Some(45.1)),
            chip(1, Some(38.0), Some(44.0)),
        ]];
        let busier = vec![vec![
            chip(0, Some(55.0), Some(45.0)),
            chip(1, Some(38.0), Some(44.0)),
        ]];
        let blind = vec![vec![
            chip(0, None, Some(45.0)),
            chip(1, Some(38.0), Some(44.0)),
        ]];
        for other in [&warmer, &busier, &blind] {
            assert_ne!(hardware_seed(&a), hardware_seed(other));
        }
        // Over many plausible readings every style of the set is picked, so the
        // start is not fixed.
        let mut picked = std::collections::HashSet::new();
        for i in 0..200 {
            let c = vec![vec![chip(0, Some(30.0 + i as f32 * 0.37), Some(40.0))]];
            picked.insert(start_style(CellMode::Auto, hardware_seed(&c)));
        }
        assert_eq!(picked.len(), 3, "{picked:?}");
    }

    #[test]
    fn next_style_walks_the_set_and_wraps() {
        assert_eq!(next_style(CellMode::Auto, Braille), Quadrant);
        assert_eq!(next_style(CellMode::Auto, Quadrant), HalfBlock);
        assert_eq!(next_style(CellMode::Auto, HalfBlock), Braille);
        assert_eq!(next_style(CellMode::Blocks, Quadrant), HalfBlock);
        assert_eq!(next_style(CellMode::Blocks, HalfBlock), Quadrant);
        assert_eq!(
            next_style(CellMode::Blocks, Braille),
            Quadrant,
            "not in the set: its first"
        );
    }

    #[test]
    fn the_first_reading_picks_the_start_once() {
        let mut s = CellState::new(CellMode::Auto);
        // Seeds 0, 1, 2 pick the three styles.
        let picks: Vec<_> = (0..3)
            .map(|seed| {
                let mut s = CellState::new(CellMode::Auto);
                s.seed_once(5.0, seed);
                s.style_at(5.0)
            })
            .collect();
        assert_eq!(picks, [Braille, Quadrant, HalfBlock]);
        assert!(s.seed_once(5.0, 2));
        assert!(!s.seed_once(6.0, 1), "only the first reading picks");
        assert_eq!(s.style_at(6.0), HalfBlock);
        // A fixed mode ignores the hardware.
        let mut f = CellState::new(CellMode::Fixed(Braille));
        assert!(!f.seed_once(1.0, 2));
        assert_eq!(f.style_at(1.0), Braille);
    }

    #[test]
    fn an_event_changes_the_style_only_after_the_dwell_and_at_the_swap() {
        let mut s = CellState::new(CellMode::Auto);
        s.seed_once(0.0, 0); // braille from 0 s
                             // An event soon after the start: too soon.
        assert!(!s.on_event_cut(10.0, 10.4));
        assert!(!s.on_event_cut(MIN_DWELL_SECS - 0.1, MIN_DWELL_SECS + 0.3));
        // After the dwell: scheduled for the swap, not the event.
        let now = MIN_DWELL_SECS + 5.0;
        assert!(s.on_event_cut(now, now + 0.45));
        assert_eq!(s.style_at(now), Braille, "not yet");
        assert_eq!(s.style_at(now + 0.44), Braille);
        assert_eq!(s.style_at(now + 0.45), Quadrant, "from the swap");
        assert_eq!(s.state_at(now + 1.0), "auto: quarter");
        // One change at a time, and the dwell restarts from the swap.
        assert!(!s.on_event_cut(now + 1.0, now + 1.5));
        s.settle(now + 1.0);
        assert!(!s.on_event_cut(now + 60.0, now + 60.4));
        // The wait ran from the swap; half a second past it is enough.
        let later = now + 0.45 + MIN_DWELL_SECS + 0.5;
        assert!(s.on_event_cut(later, later + 0.4));
    }

    #[test]
    fn a_fixed_mode_never_changes_and_blocks_never_reaches_braille() {
        let mut f = CellState::new(CellMode::Fixed(HalfBlock));
        assert!(!f.on_event_cut(1000.0, 1000.4));
        assert_eq!(f.style_at(2000.0), HalfBlock);
        let mut b = CellState::new(CellMode::Blocks);
        b.seed_once(0.0, 0);
        let mut seen = vec![b.style_at(0.0)];
        let mut t = 0.0;
        for _ in 0..6 {
            t += MIN_DWELL_SECS + 1.0;
            assert!(b.on_event_cut(t, t + 0.4));
            b.settle(t + 0.4);
            seen.push(b.style_at(t + 0.4));
        }
        assert!(seen.iter().all(|&s| s != Braille), "{seen:?}");
        assert!(seen.contains(&Quadrant) && seen.contains(&HalfBlock));
    }

    #[test]
    fn choosing_a_mode_acts_at_once_and_drops_a_pending_change() {
        let mut s = CellState::new(CellMode::Auto);
        s.seed_once(0.0, 0);
        assert!(s.on_event_cut(MIN_DWELL_SECS + 1.0, MIN_DWELL_SECS + 1.5));
        // The user picks braille before the change shows: it never shows.
        s.set_mode(CellMode::Fixed(Braille), MIN_DWELL_SECS + 1.2);
        assert_eq!(s.style_at(MIN_DWELL_SECS + 5.0), Braille);
        // `blocks` from braille moves to a block style now; from a block style
        // it keeps what is on screen.
        s.set_mode(CellMode::Blocks, 300.0);
        assert_eq!(s.style_at(300.0), Quadrant);
        s.set_mode(CellMode::Fixed(HalfBlock), 301.0);
        s.set_mode(CellMode::Blocks, 302.0);
        assert_eq!(s.style_at(302.0), HalfBlock, "kept: it is in the set");
        // And switching to automatic restarts the wait.
        s.set_mode(CellMode::Auto, 400.0);
        assert!(!s.on_event_cut(400.0 + MIN_DWELL_SECS - 1.0, 400.0 + MIN_DWELL_SECS));
    }

    #[test]
    fn the_footer_words_say_the_mode_and_the_style() {
        assert_eq!(CellMode::Fixed(Braille).state(Braille), "braille");
        assert_eq!(CellMode::Fixed(HalfBlock).state(HalfBlock), "half-block");
        assert_eq!(CellMode::Auto.state(Quadrant), "auto: quarter");
        assert_eq!(CellMode::Blocks.state(HalfBlock), "blocks: half-block");
    }
}
