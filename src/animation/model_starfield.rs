// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! `ModelStarfield` — an After-Dark-style screensaver that renders the
//! Tenstorrent model catalog as drifting, labeled "stars".
//!
//! Each catalog model becomes one star that drifts slowly across the field and
//! wraps at the edges. Models compatible with the detected hardware render
//! bright and colored by their support level (Supported → teal-green,
//! Experimental → amber); incompatible models render as dim gray. A footer line
//! summarizes how many of the catalog's models run on the current chip set.
//!
//! Shape mirrors [`crate::animation::starfield::HardwareStarfield`]:
//! `new(width, height)` / `update(..)` / `render() -> Vec<Line<'static>>`, plus a
//! private [`ModelStar`] struct. Positions/velocities/phases are seeded with
//! `rand` and advanced deterministically each `update`.

use rand::Rng;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::colors;
use crate::workload::model_catalog::{model_support, CatalogModel, Support};

/// One drifting model "star".
struct ModelStar {
    /// Position in float cells (so drift is smooth); clamped/wrapped to the grid.
    x: f32,
    y: f32,
    /// Per-frame velocity in cells (the slow "incoming star" drift).
    vx: f32,
    vy: f32,
    /// Twinkle phase offset so stars don't all pulse in lockstep.
    phase: f32,
    /// Label drawn at the star's cell (`display_name`, plus ` model_size` if set).
    label: String,
    /// Support level of this model on the current chip set.
    support: Support,
}

/// Count models whose support on `chip_set` is Supported OR Experimental.
///
/// Pure function (no starfield state) so callers can compute the "N of M"
/// headline without building an animation.
pub fn compatible_count(models: &[CatalogModel], chip_set: &str) -> usize {
    models
        .iter()
        .filter(|m| !matches!(model_support(m, chip_set), Support::No))
        .count()
}

/// After-Dark-style drifting starfield of the model catalog.
pub struct ModelStarfield {
    width: usize,
    height: usize,
    /// Monotonic frame counter driving the twinkle brightness.
    frame: u64,
    stars: Vec<ModelStar>,
    /// Cached headline numbers, recomputed each `update`.
    compatible: usize,
    total: usize,
    /// Chip set the last `update` classified against (for the footer text).
    chip_set: String,
}

impl ModelStarfield {
    /// Create an empty starfield sized to the given cell grid.
    ///
    /// Guards against a zero-sized view (the UI may call `new(0, 0)` before its
    /// first resize): rendering simply yields only the footer in that case.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            frame: 0,
            stars: Vec::new(),
            compatible: 0,
            total: 0,
            chip_set: String::new(),
        }
    }

    /// Number of active stars (one per catalog model).
    pub fn star_count(&self) -> usize {
        self.stars.len()
    }

    /// Advance the animation one frame against the current catalog + hardware.
    ///
    /// Re-seeds the star field whenever the model set changes size (a catalog
    /// refresh), otherwise advances each star's drift and refreshes its label +
    /// support classification in place. Safe on empty catalogs and zero-sized
    /// grids.
    pub fn update(&mut self, models: &[CatalogModel], chip_set: &str) {
        self.frame = self.frame.wrapping_add(1);
        self.chip_set = chip_set.to_string();
        self.total = models.len();
        self.compatible = compatible_count(models, chip_set);

        // Usable spans for seeding/wrapping. Never let these hit zero so the
        // rand range and modulo below stay valid on a 0-sized view.
        let w = self.width.max(1) as f32;
        let h = self.height.max(1) as f32;

        // Re-seed the whole field if the catalog size changed (first update or a
        // background refresh added/removed models). Keeps stars index-aligned to
        // `models` so labels/support can be updated in place afterwards.
        if self.stars.len() != models.len() {
            let mut rng = rand::rng();
            self.stars = models
                .iter()
                .map(|_| ModelStar {
                    x: rng.random_range(0.0..w),
                    y: rng.random_range(0.0..h),
                    // Slow drift, biased toward the classic left-down "incoming"
                    // motion but with a little variety per star.
                    vx: rng.random_range(-0.30..-0.05),
                    vy: rng.random_range(-0.10..0.10),
                    phase: rng.random_range(0.0..std::f32::consts::TAU),
                    label: String::new(),
                    support: Support::No,
                })
                .collect();
        }

        // Advance drift + refresh per-model fields.
        for (star, m) in self.stars.iter_mut().zip(models.iter()) {
            // Move and wrap at the edges (toroidal field).
            star.x += star.vx;
            star.y += star.vy;
            star.x = star.x.rem_euclid(w);
            star.y = star.y.rem_euclid(h);

            let mut label = m.display_name.clone();
            if !m.model_size.is_empty() {
                label.push(' ');
                label.push_str(&m.model_size);
            }
            star.label = label;
            star.support = model_support(m, chip_set);
        }
    }

    /// Render the field to owned Ratatui lines. The final line is always the
    /// footer: `"{compatible} of {total} models run on your {chip_set}"`.
    pub fn render(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        if self.width > 0 && self.height > 0 {
            // Blank canvas; each cell is (char, color). Black background floor.
            let bg = colors::rgb(0, 0, 0);
            let mut canvas: Vec<Vec<(char, Color)>> =
                vec![vec![(' ', bg); self.width]; self.height];

            for star in &self.stars {
                // Twinkle: brightness pulses with frame + per-star phase.
                let twinkle = (self.frame as f32 * 0.12 + star.phase).sin();
                let (color, bold) = star_style(star.support, twinkle);

                // Round the float drift position to a cell and clamp to grid.
                let sx = (star.x as usize).min(self.width - 1);
                let sy = (star.y as usize).min(self.height - 1);

                // Draw the label starting at the star cell; stop at the right edge.
                for (i, ch) in star.label.chars().enumerate() {
                    let col = sx + i;
                    if col >= self.width {
                        break;
                    }
                    canvas[sy][col] = (ch, color);
                }
                // A bare star always leaves a visible mark, even for empty labels.
                if star.label.is_empty() {
                    let mark = if bold { '*' } else { '·' };
                    canvas[sy][sx] = (mark, color);
                }
            }

            // Convert canvas rows to Lines, run-length grouping by color.
            for row in canvas {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut current_text = String::new();
                let mut current_color = bg;
                for (ch, color) in row {
                    if color != current_color {
                        if !current_text.is_empty() {
                            spans.push(Span::styled(
                                std::mem::take(&mut current_text),
                                Style::default().fg(current_color),
                            ));
                        }
                        current_color = color;
                    }
                    current_text.push(ch);
                }
                if !current_text.is_empty() {
                    spans.push(Span::styled(
                        current_text,
                        Style::default().fg(current_color),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }

        // Footer — the test depends on this exact phrasing.
        lines.push(Line::from(Span::styled(
            format!(
                "{} of {} models run on your {}",
                self.compatible, self.total, self.chip_set
            ),
            Style::default()
                .fg(colors::TEXT_SECONDARY)
                .add_modifier(Modifier::BOLD),
        )));

        lines
    }
}

/// Pick a star's foreground color + bold flag from its support level and a
/// twinkle factor in `[-1.0, 1.0]`.
///
/// Compatible stars pulse in brightness (scaled base color, bold on peaks);
/// incompatible stars stay a steady dim gray so they read as background.
fn star_style(support: Support, twinkle: f32) -> (Color, bool) {
    match support {
        Support::Supported => (scale(colors::SUCCESS, 0.7 + 0.3 * twinkle), twinkle > 0.4),
        Support::Experimental => (scale(colors::WARNING, 0.7 + 0.3 * twinkle), twinkle > 0.4),
        Support::No => (colors::rgb(90, 90, 90), false),
    }
}

/// Multiply an RGB color's channels by `factor` (clamped), for twinkle dimming.
/// Non-RGB colors pass through unchanged.
fn scale(color: Color, factor: f32) -> Color {
    let f = factor.clamp(0.0, 1.0);
    if let Color::Rgb(r, g, b) = color {
        Color::Rgb(
            (r as f32 * f) as u8,
            (g as f32 * f) as u8,
            (b as f32 * f) as u8,
        )
    } else {
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::model_catalog::{CatalogModel, Compat};

    fn model(name: &str, chip: &str, status: &str) -> CatalogModel {
        CatalogModel {
            display_name: name.into(), family: String::new(), model_size: String::new(),
            tasks: vec![], compatibility: vec![Compat { chip_set: chip.into(), hardware: String::new(), status: status.into() }],
        }
    }
    #[test]
    fn footer_counts_compatible_and_stars_track_models() {
        let models = vec![
            model("A", "Blackhole", "Supported"),
            model("B", "Blackhole", "Experimental"),
            model("C", "Wormhole", "Supported"),  // not Blackhole
        ];
        assert_eq!(compatible_count(&models, "Blackhole"), 2);
        let mut sf = ModelStarfield::new(80, 24);
        sf.update(&models, "Blackhole");
        assert_eq!(sf.star_count(), 3, "one star per model");
        let text: String = sf.render().iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        assert!(text.contains("2 of 3"), "footer shows compatible/total");
        assert!(text.contains("Blackhole"));
    }

    #[test]
    fn compatible_count_ignores_unsupported_and_other_chips() {
        let models = vec![
            model("A", "Blackhole", "Supported"),
            model("B", "Blackhole", "Experimental"),
            model("C", "Blackhole", "Not Supported"),
            model("D", "Wormhole", "Supported"),
        ];
        assert_eq!(compatible_count(&models, "Blackhole"), 2);
        assert_eq!(compatible_count(&models, "Wormhole"), 1);
        assert_eq!(compatible_count(&[], "Blackhole"), 0);
    }

    #[test]
    fn empty_catalog_renders_zero_footer_without_panic() {
        let mut sf = ModelStarfield::new(80, 24);
        sf.update(&[], "Blackhole");
        assert_eq!(sf.star_count(), 0);
        let text: String = sf
            .render()
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("0 of 0"));
    }

    #[test]
    fn zero_sized_grid_does_not_panic() {
        let models = vec![model("A", "Blackhole", "Supported")];
        let mut sf = ModelStarfield::new(0, 0);
        // Several updates to exercise drift/wrap math on a degenerate grid.
        for _ in 0..5 {
            sf.update(&models, "Blackhole");
        }
        assert_eq!(sf.star_count(), 1);
        // Only the footer is produced when there is no drawable area.
        let lines = sf.render();
        assert_eq!(lines.len(), 1);
    }
}
