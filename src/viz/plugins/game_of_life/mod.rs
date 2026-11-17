// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! `game-of-life`: Conway's Game of Life that the chips feed.
//!
//! The board is split into one vertical band per chip. Each band is seeded
//! from that chip's power draw: the busier the chip is compared with its own
//! range so far, the more new live cells fall into its band every generation,
//! and a sudden rise in power drops a glider into the band. A chip at rest
//! adds nothing, so its band settles into still lifes and blinkers. Cell colour
//! follows the chip's temperature (green, yellow, red, as in `example-pulse`).
//!
//! The rules are the standard ones (B3/S23) on a torus, so a glider leaving one
//! edge returns on the opposite edge and can cross into the next chip's band.
//! One generation runs every 125 ms of the bus clock, at most four per frame.
//! The random numbers come from a fixed-seed generator, so the same readings
//! at the same times draw the same board (the tests depend on it).
//!
//! Like every Rust plugin it reads only what `crate::viz` exposes.

use crate::ui::colors;
use crate::viz::{SignalSpec, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use std::collections::HashMap;
use tt_toplike_bus::{EntityKey, EntityKind, Value};

#[cfg(test)]
mod tests;

/// Hidden until asked for: `--viz game-of-life` or `[viz] enable`.
pub const ENABLED_BY_DEFAULT: bool = false;

/// Build one instance.
pub fn make() -> Box<dyn Viz> {
    Box::new(Life::new())
}

/// Chip power in watts, sent on every poll.
const POWER: &str = "tt_tdp_w";
/// ASIC temperature in degrees Celsius.
const TEMP: &str = "tt_asic_temperature_celsius";
/// Bus-clock time between generations.
const STEP_NS: u64 = 125_000_000;
/// Generations run in one frame at most, so a long pause does not stall the draw.
const MAX_STEPS_PER_FRAME: u64 = 4;
/// Fraction of a band's cells that get a new live cell per generation at full
/// activity.
const NOISE_RATE: f64 = 0.004;
/// A power rise of this fraction of the chip's range in one frame drops a glider.
const SURGE: f64 = 0.15;
/// Power ranges narrower than this many watts count as no activity.
const MIN_SPAN_W: f64 = 5.0;
/// The glider, as (dx, dy) offsets.
const GLIDER: [(usize, usize); 5] = [(1, 0), (2, 1), (0, 2), (1, 2), (2, 2)];

/// What the view remembers about one chip's power.
#[derive(Clone, Copy)]
struct Power {
    min: f64,
    max: f64,
    last: f64,
}

/// A small fixed-seed generator (xorshift64*), so no dependency is needed and
/// a run can be repeated.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// A number in `0.0..1.0`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// A number in `0..n` (`n` of 0 gives 0).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// The view's state.
pub(super) struct Life {
    /// `[viz.game-of-life] hot_c`: temperature drawn red; yellow starts 15 below.
    pub(super) hot_c: f64,
    w: usize,
    h: usize,
    cells: Vec<bool>,
    generation: u64,
    last_step_ns: u64,
    rng: Rng,
    power: HashMap<EntityKey, Power>,
}

impl Life {
    pub(super) fn new() -> Self {
        Life {
            hot_c: 85.0,
            w: 0,
            h: 0,
            cells: Vec::new(),
            generation: 0,
            last_step_ns: 0,
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            power: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn population(&self) -> usize {
        self.cells.iter().filter(|c| **c).count()
    }

    pub(super) fn alive(&self, x: usize, y: usize) -> bool {
        x < self.w && y < self.h && self.cells[y * self.w + x]
    }

    /// Put a live cell at (x, y); out of range is ignored.
    pub(super) fn set(&mut self, x: usize, y: usize) {
        if x < self.w && y < self.h {
            self.cells[y * self.w + x] = true;
        }
    }

    /// Make the board `w` x `h`, keeping the cells that still fit.
    pub(super) fn resize(&mut self, w: usize, h: usize) {
        if (w, h) == (self.w, self.h) {
            return;
        }
        let mut cells = vec![false; w * h];
        for y in 0..self.h.min(h) {
            for x in 0..self.w.min(w) {
                cells[y * w + x] = self.cells[y * self.w + x];
            }
        }
        self.cells = cells;
        self.w = w;
        self.h = h;
    }

    /// One generation of B3/S23 on a torus.
    pub(super) fn step(&mut self) {
        let (w, h) = (self.w, self.h);
        if w == 0 || h == 0 {
            return;
        }
        let mut next = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut n = 0;
                for dy in [h - 1, 0, 1] {
                    for dx in [w - 1, 0, 1] {
                        if (dx, dy) != (0, 0) && self.cells[((y + dy) % h) * w + (x + dx) % w] {
                            n += 1;
                        }
                    }
                }
                let on = self.cells[y * w + x];
                next[y * w + x] = matches!((on, n), (true, 2) | (_, 3));
            }
        }
        self.cells = next;
        self.generation += 1;
    }

    /// Drop a glider with its top-left corner at (x, y), wrapping at the edges.
    pub(super) fn glider(&mut self, x: usize, y: usize) {
        if self.w == 0 || self.h == 0 {
            return;
        }
        for (dx, dy) in GLIDER {
            let (cx, cy) = ((x + dx) % self.w, (y + dy) % self.h);
            self.cells[cy * self.w + cx] = true;
        }
    }

    /// The band (x range) of chip `i` of `n`.
    pub(super) fn band(&self, i: usize, n: usize) -> (usize, usize) {
        (self.w * i / n.max(1), self.w * (i + 1) / n.max(1))
    }

    /// Feed band `i` of `n` from a chip's activity (0 to 1), and drop a glider
    /// when `surge` is set.
    pub(super) fn feed(&mut self, i: usize, n: usize, activity: f64, surge: bool) {
        let (x0, x1) = self.band(i, n);
        let width = x1 - x0;
        if width == 0 || self.h == 0 {
            return;
        }
        // The expected number of new cells, with the fraction rolled as a chance.
        let want = activity.clamp(0.0, 1.0) * NOISE_RATE * (width * self.h) as f64;
        let mut count = want.floor() as usize;
        if self.rng.unit() < want.fract() {
            count += 1;
        }
        for _ in 0..count {
            let (x, y) = (x0 + self.rng.below(width), self.rng.below(self.h));
            self.set(x, y);
        }
        if surge {
            let y = self.rng.below(self.h);
            self.glider(x0, y);
        }
    }

    /// Read each chip's power, update the ranges, run the due generations
    /// and feed the bands. `chips` is in band order.
    pub(super) fn advance(&mut self, chips: &[EntityKey], ctx: &VizCtx) {
        let now = ctx.now_mono_ns;
        if self.last_step_ns == 0 || now < self.last_step_ns {
            self.last_step_ns = now;
        }
        let due = ((now - self.last_step_ns) / STEP_NS).min(MAX_STEPS_PER_FRAME);
        if due == 0 {
            return;
        }
        // A long pause is not made up: restart the clock from now.
        self.last_step_ns = if now - self.last_step_ns > STEP_NS * MAX_STEPS_PER_FRAME {
            now
        } else {
            self.last_step_ns + due * STEP_NS
        };
        let mut activity = Vec::with_capacity(chips.len());
        for chip in chips {
            activity.push(self.track(chip, ctx));
        }
        for k in 0..due {
            self.step();
            for (i, (a, surge)) in activity.iter().enumerate() {
                // A surge drops one glider per frame, on the frame's first generation.
                self.feed(i, chips.len(), *a, *surge && k == 0);
            }
        }
    }

    /// Update the range for `chip` and return its activity and whether power
    /// just surged. A chip with no reading, or a flat range, has no activity.
    fn track(&mut self, chip: &EntityKey, ctx: &VizCtx) -> (f64, bool) {
        let Some(p) = number(ctx, chip, POWER) else {
            return (0.0, false);
        };
        let r = self.power.entry(chip.clone()).or_insert(Power { min: p, max: p, last: p });
        // The range relaxes slowly toward the current value, so an old peak
        // does not hold the activity down for ever.
        r.min += (p - r.min) * 0.002;
        r.max += (p - r.max) * 0.002;
        r.min = r.min.min(p);
        r.max = r.max.max(p);
        let span = r.max - r.min;
        let surge = span >= MIN_SPAN_W && p - r.last > SURGE * span;
        r.last = p;
        if span < MIN_SPAN_W {
            (0.0, false)
        } else {
            ((p - r.min) / span, surge)
        }
    }
}

/// A finite numeric reading as `f64`.
fn number(ctx: &VizCtx, chip: &EntityKey, signal: &str) -> Option<f64> {
    let x = match &ctx.world.reading(chip, signal)?.value {
        Value::F64(x) => *x,
        Value::U64(x) => *x as f64,
        Value::I64(x) => *x as f64,
        Value::Text(_) => return None,
    };
    x.is_finite().then_some(x)
}

impl Viz for Life {
    fn id(&self) -> &str {
        "game-of-life"
    }

    fn title(&self) -> &str {
        "Life"
    }

    fn min_size(&self) -> (u16, u16) {
        (20, 6)
    }

    fn signals(&self) -> Vec<SignalSpec> {
        [POWER, TEMP]
            .map(|name| SignalSpec { kind: EntityKind::Chip, name: name.into() })
            .to_vec()
    }

    /// The first row is a status line; the rest is the board, two cells per
    /// terminal row (upper half block, lower half block or full block).
    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &VizCtx) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        let chips = ctx.chips();
        self.resize(area.width as usize, (area.height as usize - 1) * 2);
        self.advance(&chips, ctx);

        let plain = Style::default().fg(colors::text_primary());
        let status = if chips.is_empty() {
            format!("no chips: nothing feeds the board   gen {}  alive {}", self.generation, self.population())
        } else {
            format!("gen {}  alive {}  {} chips feed it", self.generation, self.population(), chips.len())
        };
        buf.set_stringn(area.x, area.y, status, area.width as usize, plain);

        for row in 0..(area.height as usize - 1) {
            for x in 0..area.width as usize {
                let (top, bottom) = (self.alive(x, row * 2), self.alive(x, row * 2 + 1));
                let glyph = match (top, bottom) {
                    (false, false) => continue,
                    (true, false) => "▀",
                    (false, true) => "▄",
                    (true, true) => "█",
                };
                let band = if chips.is_empty() { 0 } else { (x * chips.len() / self.w).min(chips.len() - 1) };
                let colour = chips.get(band).map_or(colors::primary(), |c| self.colour(number(ctx, c, TEMP)));
                buf.set_stringn(area.x + x as u16, area.y + 1 + row as u16, glyph, 1, Style::default().fg(colour));
            }
        }
    }

    fn legend(&self) -> Vec<String> {
        let hot = self.hot_c;
        vec![
            "one vertical band per chip, left to right in chip order".into(),
            format!("green: below {} C", hot - 15.0),
            format!("yellow: {} C up to {} C", hot - 15.0, hot),
            format!("red: {hot} C or more"),
            "primary colour: the chip has no temperature reading".into(),
        ]
    }

    fn explain(&self) -> String {
        "Conway's Game of Life on a wrapping board, one band per chip. Each chip's \
         power draw (tt_tdp_w) is compared with the lowest and highest it has drawn so \
         far. The nearer the top of that range, the more new live cells fall into its \
         band each generation, and a sharp rise in power drops a glider. A chip at \
         rest adds nothing and its band settles. Cell colour follows the chip's \
         temperature (tt_asic_temperature_celsius)."
            .into()
    }

    fn help(&self) -> String {
        format!(
            "No keys. Setting: hot_c under [viz.game-of-life] in the config file \
             (now {}), the temperature in degrees C where cells turn red.",
            self.hot_c
        )
    }

    fn configure(&mut self, cfg: &toml_edit::Table) {
        let item = cfg.get("hot_c");
        let value = item.and_then(|i| i.as_float().or(i.as_integer().map(|n| n as f64)));
        if let Some(v) = value.filter(|v| v.is_finite() && *v > 0.0) {
            self.hot_c = v;
        }
    }
}

impl Life {
    /// The cell colour for a band whose chip reads `temp`.
    pub(super) fn colour(&self, temp: Option<f64>) -> ratatui::style::Color {
        match temp {
            None => colors::primary(),
            Some(t) if t >= self.hot_c => colors::error(),
            Some(t) if t >= self.hot_c - 15.0 => colors::warning(),
            Some(_) => colors::success(),
        }
    }
}
