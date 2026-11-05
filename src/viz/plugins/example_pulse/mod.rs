// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! `example-pulse`: the example Rust viz plugin. Copy this folder to start
//! your own.
//!
//! A plugin is a folder `src/viz/plugins/<snake_name>/` whose `mod.rs` has the
//! two items below (`ENABLED_BY_DEFAULT` and `make`) and a type that
//! implements [`Viz`]. The build finds the folder by itself; no other file
//! changes. The id must be the folder name with `_` turned into `-`.
//!
//! This plugin draws one row per chip: the chip name, a power sparkline
//! coloured by the chip's temperature, and the latest power and temperature.
//! It uses only what `crate::viz` exposes, plus the shared colour palette.

use crate::ui::colors;
use crate::viz::{SignalSpec, Viz, VizCtx};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tt_toplike_bus::{EntityKey, EntityKind, Value};

#[cfg(test)]
mod tests;

/// Whether the view is in the view cycle before the user chooses. An example
/// stays hidden: enable it with `--viz example-pulse` or `[viz] enable`.
pub const ENABLED_BY_DEFAULT: bool = false;

/// Build one instance. The host calls this once per process.
pub fn make() -> Box<dyn Viz> {
    Box::new(Pulse { hot_c: 85.0 })
}

/// Chip power in watts. This is the fast-lane signal (one value every poll),
/// so the history has a steady series to draw. `toplike.board_power_w` is
/// sent only when it changes and would give a sparse sparkline.
///
/// The name says "tdp", but the value is the power the chip is drawing now.
/// It is not the chip's power limit (see `src/bus/signals.rs`, `TDP_W`).
const POWER: &str = "tt_tdp_w";
/// ASIC temperature in degrees Celsius.
const TEMP: &str = "tt_asic_temperature_celsius";
/// Width of the name column, in cells.
const NAME_W: usize = 12;
/// Width of the numbers to the right of the sparkline: " 123.4 W  85 C".
const NUMBERS_W: usize = 15;
/// Block characters for the sparkline, lowest to highest.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The view's state: only the setting. Everything else is read per frame.
struct Pulse {
    /// `[viz.example-pulse] hot_c`: temperature drawn red. Yellow starts 15
    /// degrees below it.
    hot_c: f64,
}

/// A numeric reading as `f64`, or `None` for text, NaN or infinity.
fn number(v: &Value) -> Option<f64> {
    let x = match v {
        Value::F64(x) => *x,
        Value::U64(x) => *x as f64,
        Value::I64(x) => *x as f64,
        Value::Text(_) => return None,
    };
    x.is_finite().then_some(x)
}

/// A sparkline `width` cells wide from the newest samples, scaled between the
/// smallest and largest value shown. Short series are padded on the left so
/// the newest sample sits at the right edge. A flat series (or one sample)
/// draws the middle block. A NaN sample draws the lowest block and does not
/// move the scale; the host records only finite values, so this is a guard
/// and not a case the view meets.
fn sparkline(series: &[f64], width: usize) -> String {
    let shown = &series[series.len().saturating_sub(width)..];
    let lo = shown.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = shown.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let cells: String = shown
        .iter()
        .map(|v| match hi - lo {
            d if d > 0.0 => BLOCKS[(((v - lo) / d) * 7.0).round() as usize],
            _ => BLOCKS[3], // a flat line sits in the middle
        })
        .collect();
    format!("{cells:>width$}")
}

impl Viz for Pulse {
    /// The id: the folder name `example_pulse` with `_` as `-`.
    fn id(&self) -> &str {
        "example-pulse"
    }

    /// The name in the view cycle and the title bar.
    fn title(&self) -> &str {
        "Pulse"
    }

    /// Below this the host shows "needs 40 x 6" and does not call `render`.
    fn min_size(&self) -> (u16, u16) {
        (40, 6)
    }

    /// The signals this view reads. The host records a history of each, per
    /// chip, and hands it back as `ctx.history`.
    fn signals(&self) -> Vec<SignalSpec> {
        [POWER, TEMP]
            .map(|name| SignalSpec {
                kind: EntityKind::Chip,
                name: name.into(),
            })
            .to_vec()
    }

    /// Draw into `area` of `buf`. Never write outside `area`: the host may
    /// pass a smaller area than the buffer, or an empty one.
    fn render(&mut self, buf: &mut Buffer, area: Rect, ctx: &VizCtx) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let plain = Style::default().fg(colors::text_primary());
        let chips = ctx.chips();
        if chips.is_empty() {
            buf.set_stringn(area.x, area.y, "no chips", area.width as usize, plain);
            return;
        }
        // If the chips do not fit, the last row says how many are left out.
        let rows = area.height as usize;
        let hidden = chips.len().saturating_sub(rows);
        let drawn = if hidden > 0 { rows - 1 } else { chips.len() };
        for (i, chip) in chips.iter().take(drawn).enumerate() {
            self.draw_row(buf, area, area.y + i as u16, chip, ctx);
        }
        if hidden > 0 {
            let more = format!("+{} more", hidden + 1);
            buf.set_stringn(area.x, area.y + drawn as u16, more, area.width as usize, plain);
        }
    }

    /// Lines for the legend panel.
    fn legend(&self) -> Vec<String> {
        let hot = self.hot_c;
        vec![
            format!("green: below {} C", hot - 15.0),
            format!("yellow: {} C up to {} C", hot - 15.0, hot),
            format!("red: {hot} C or more"),
            "the sparkline is power, the colour is temperature".into(),
            "primary colour: the chip has no temperature reading yet".into(),
        ]
    }

    /// What the view shows and where the numbers come from.
    fn explain(&self) -> String {
        "One row per chip. The sparkline is the chip's recent power draw in \
         watts (the tt_tdp_w signal), and its colour follows the ASIC \
         temperature (tt_asic_temperature_celsius). The numbers are the \
         latest power and temperature; -- means the chip has not reported one. A \
         chip with no temperature reading draws its sparkline in the primary \
         colour, because there is nothing to compare with the thresholds."
            .into()
    }

    /// Help text: keys and settings.
    fn help(&self) -> String {
        format!(
            "No keys. Setting: hot_c under [viz.example-pulse] in the config \
             file (now {}). It is the temperature in degrees C where the \
             sparkline turns red.",
            self.hot_c
        )
    }

    /// Read `[viz.example-pulse]`. A value of the wrong type, or one that is
    /// not a positive finite number, is ignored and the old value stays.
    fn configure(&mut self, cfg: &toml_edit::Table) {
        let item = cfg.get("hot_c");
        let value = item.and_then(|i| i.as_float().or(i.as_integer().map(|n| n as f64)));
        if let Some(v) = value.filter(|v| v.is_finite() && *v > 0.0) {
            self.hot_c = v;
        }
    }
}

impl Pulse {
    /// Draw one chip on row `y`: name, sparkline, numbers. Every write is
    /// limited to the width left in `area`, so a narrow area clips the row.
    fn draw_row(&self, buf: &mut Buffer, area: Rect, y: u16, chip: &EntityKey, ctx: &VizCtx) {
        let read = |signal: &str| {
            let r = ctx.world.reading(chip, signal)?;
            number(&r.value)
        };
        let (power, temp) = (read(POWER), read(TEMP));
        // Green below hot - 15, yellow below hot, red at or above hot. A chip
        // with no temperature reading has nothing to compare, so its
        // sparkline draws in the primary colour (the legend says so).
        let colour = match temp {
            None => colors::primary(),
            Some(t) if t >= self.hot_c => colors::error(),
            Some(t) if t >= self.hot_c - 15.0 => colors::warning(),
            Some(_) => colors::success(),
        };
        let plain = Style::default().fg(colors::text_primary());
        let put = |buf: &mut Buffer, col: usize, text: &str, style: Style| {
            if col < area.width as usize {
                let room = area.width as usize - col;
                buf.set_stringn(area.x + col as u16, y, text, room, style);
            }
        };

        // Name: the bus id without the "chip:" prefix, cut to fit.
        let name = chip.as_str().trim_start_matches("chip:");
        let name: String = if name.chars().count() > NAME_W {
            name.chars().take(NAME_W - 1).chain(['…']).collect()
        } else {
            name.to_string()
        };
        put(buf, 0, &name, plain);

        // Sparkline: whatever width is left after the name and the numbers.
        let spark_w = (area.width as usize).saturating_sub(NAME_W + 1 + NUMBERS_W);
        let spark = sparkline(ctx.history.series(chip, POWER), spark_w);
        put(buf, NAME_W + 1, &spark, Style::default().fg(colour));

        // Numbers: "--" stands in for a missing reading.
        let p = power.map_or("--".to_string(), |p| format!("{p:.1} W"));
        let t = temp.map_or("--".to_string(), |t| format!("{t:.0} C"));
        put(buf, NAME_W + 1 + spark_w + 1, &format!("{p:>7} {t:>5}"), plain);
    }
}
