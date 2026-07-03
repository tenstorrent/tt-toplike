// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The Feeding behavior of the unified serving snake: a calm living creature
//! whose form encodes live vLLM serving signals directly. Near-still at rest;
//! animates only in proportion to real throughput/requests.
//!
//! Every motion maps to a real signal:
//! - body **length** grows with `requests_running` (in-flight concurrency),
//! - wave **amplitude/speed** scale with `generation_tps` (0 tps → flat = calm),
//! - **color-heat** tracks `kv_cache_usage` (cool blue → hot red),
//! - a bright **pulse** glyph fires when a request completes this tick,
//! - a **frayed** `ERROR`-tinted tail appears when a request errors,
//! - dim **queued pellets** trail to the right, one per `requests_waiting`.
//!
//! Shape mirrors [`crate::animation::inference_load::LoadSnake`] /
//! [`crate::animation::model_starfield::ModelStarfield`]: a `Vec<Vec<(char,
//! Color)>>` canvas guarded by non-zero dims, run-length grouped into owned
//! `Line<'static>` rows. Returns `vec![]` on a zero-sized view.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::animation::hsv_to_rgb;
use crate::animation::inference_load::fmt_elapsed;
use crate::ui::colors;
use crate::workload::inference_server::ServingStats;

/// Frames a completion "pulse" glyph stays lit after `completed_delta > 0`.
const PULSE_FRAMES: u32 = 6;
/// Frames the tail stays frayed after `errored_delta > 0`.
const FRAY_FRAMES: u32 = 8;
/// `generation_tps` at/above which the wave reaches full amplitude/speed.
const GEN_TPS_REF: f32 = 200.0;

/// Body length (cells) for `running` in-flight requests, clamped to `max`.
/// A small base so the creature is always visible; grows with concurrency.
pub fn body_len(running: u32, max: usize) -> usize {
    (4 + running as usize * 3).min(max.max(1))
}

/// Whether the server is doing work right now (drives motion vs. breathing).
pub fn is_active(s: &ServingStats) -> bool {
    s.generation_tps > 0.5 || s.requests_running > 0 || s.requests_waiting > 0
}

/// The Feeding creature: a horizontal serving snake driven by live vLLM stats.
pub struct ServingCreature {
    /// Monotonic frame counter driving the wave phase.
    frame: u64,
    /// Model / service label shown in the title.
    label: String,
    /// Server uptime in seconds (formatted into the title).
    uptime_secs: u64,
    /// Latest serving stats; `None` = ready but no metrics yet (calm state).
    serving: Option<ServingStats>,
    /// Frames left on the completion pulse (a bright head glyph).
    pulse_left: u32,
    /// Frames left on the frayed error tail.
    fray_left: u32,
}

impl Default for ServingCreature {
    fn default() -> Self {
        Self::new()
    }
}

impl ServingCreature {
    /// Create a calm, unstarted creature.
    pub fn new() -> Self {
        Self {
            frame: 0,
            label: String::new(),
            uptime_secs: 0,
            serving: None,
            pulse_left: 0,
            fray_left: 0,
        }
    }

    /// Fold this tick's signals into the creature's state. Pulse/fray are set as
    /// short frame-countdowns so a single completion/error tick leaves a brief
    /// visible mark that decays on its own over the next few frames.
    pub fn update(&mut self, label: &str, uptime_secs: u64, serving: Option<&ServingStats>) {
        self.frame = self.frame.wrapping_add(1);
        self.label = label.to_string();
        self.uptime_secs = uptime_secs;
        self.serving = serving.copied();

        // Decay any live effect, then re-arm it if this tick earned it.
        self.pulse_left = self.pulse_left.saturating_sub(1);
        self.fray_left = self.fray_left.saturating_sub(1);
        if let Some(s) = serving {
            if s.completed_delta > 0 {
                self.pulse_left = PULSE_FRAMES;
            }
            if s.errored_delta > 0 {
                self.fray_left = FRAY_FRAMES;
            }
        }
    }

    /// Render the creature to owned Ratatui lines: a title, the serpentine body
    /// canvas, and a footer (live stats when serving, else "serving · ready").
    /// Returns `vec![]` on a zero-sized view; never panics on narrow/short grids.
    pub fn render(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        if width == 0 || height == 0 {
            return vec![];
        }

        let bg = colors::rgb(0, 0, 0);
        let dim = colors::rgb(90, 90, 90);
        let mut lines: Vec<Line<'static>> = Vec::new();

        // --- Title: "{label}  serving · up {elapsed}" (char-safe truncation) ---
        let title_full = format!(
            "{}  serving · up {}",
            self.label,
            fmt_elapsed(self.uptime_secs)
        );
        let title: String = title_full.chars().take(width).collect();
        lines.push(Line::from(Span::styled(
            title,
            Style::default()
                .fg(colors::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        )));

        // --- Body canvas (title + footer reserve one row each) ---
        let body_rows = height.saturating_sub(2);
        if body_rows > 0 {
            let mut canvas: Vec<Vec<(char, Color)>> = vec![vec![(' ', bg); width]; body_rows];

            // Signals (all zero/calm when serving is None).
            let (gen_tps, kv, running, waiting) = match &self.serving {
                Some(s) => (
                    s.generation_tps,
                    s.kv_cache_usage,
                    s.requests_running,
                    s.requests_waiting,
                ),
                None => (0.0, 0.0, 0, 0),
            };

            // Body length grows with in-flight concurrency, clamped to the arena.
            let max_len = width.saturating_sub(2);
            let len = body_len(running, max_len);

            // Wave amplitude/speed scale with throughput; 0 tps → flat + still.
            let amp_norm = (gen_tps / GEN_TPS_REF).clamp(0.0, 1.0);
            let max_amp = (body_rows.saturating_sub(1)) as f32 / 2.0;
            let amp = amp_norm * max_amp;
            let center = (body_rows as f32 - 1.0) / 2.0;
            let speed = 0.12 + 0.18 * amp_norm;

            // Color-heat: cool blue (idle KV) → hot red (full KV cache).
            let heat = hsv_to_rgb(240.0 * (1.0 - kv.clamp(0.0, 1.0)), 0.85, 0.95);

            let last_row = body_rows - 1;
            for i in 0..len {
                let x = 1 + i;
                if x >= width {
                    break;
                }
                let phase = self.frame as f32 * speed + i as f32 * 0.6;
                let y = (center + amp * phase.sin()).round();
                let y = (y.max(0.0) as usize).min(last_row);

                let is_head = i + 1 == len;
                let is_tail = i == 0;

                let (glyph, color) = if is_tail && self.fray_left > 0 {
                    // Frayed, error-tinted tail after a request errors this tick.
                    ('✗', colors::ERROR)
                } else if is_head && self.pulse_left > 0 {
                    // Bright completion pulse riding the head.
                    ('✦', colors::TEXT_PRIMARY)
                } else if is_head {
                    ('▶', heat)
                } else {
                    ('●', heat)
                };
                canvas[y][x] = (glyph, color);
            }

            // Dim queued pellets trail to the right, one per waiting request.
            let pellet_row = (center.round().max(0.0) as usize).min(last_row);
            let mut px = 1 + len + 1;
            for _ in 0..waiting {
                if px >= width {
                    break;
                }
                canvas[pellet_row][px] = ('·', dim);
                px += 1;
            }

            for row in canvas {
                lines.push(row_to_line(row, bg));
            }
        }

        // --- Footer: live stats when serving, else the calm ready state ---
        let footer_full = match &self.serving {
            Some(s) => format!(
                "{:.0} tok/s · {} running · {} queued · KV {:.0}% · TTFT {:.2}s · {} served",
                s.generation_tps,
                s.requests_running,
                s.requests_waiting,
                s.kv_cache_usage * 100.0,
                s.ttft_avg_s,
                s.counters.requests_succeeded_total,
            ),
            None => "serving · ready".to_string(),
        };
        let footer: String = footer_full.chars().take(width).collect();
        lines.push(Line::from(Span::styled(
            footer,
            Style::default().fg(colors::TEXT_SECONDARY),
        )));

        lines
    }
}

/// Run-length group one canvas row (cells of `(char, Color)`) into a `Line`,
/// coalescing adjacent same-color cells into a single owned-String span.
fn row_to_line(row: Vec<(char, Color)>, bg: Color) -> Line<'static> {
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
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::{ServingStats, VllmCounters};

    fn stats(running: u32, tps: f32, completed: u32, errored: u32) -> ServingStats {
        ServingStats {
            generation_tps: tps,
            prompt_tps: 0.0,
            completed_delta: completed,
            errored_delta: errored,
            requests_running: running,
            requests_waiting: 0,
            kv_cache_usage: 0.0,
            ttft_avg_s: 0.0,
            counters: VllmCounters::default(),
        }
    }

    #[test]
    fn body_length_tracks_in_flight_requests() {
        assert!(
            body_len(0, 40) < body_len(3, 40),
            "more in-flight → longer body"
        );
        assert!(body_len(1000, 40) <= 40, "clamped to arena width");
    }

    #[test]
    fn idle_is_active_false_active_true() {
        assert!(!is_active(&stats(0, 0.0, 0, 0)), "0 tps + 0 running = idle");
        assert!(is_active(&stats(0, 120.0, 0, 0)), "tokens flowing = active");
        assert!(
            is_active(&stats(2, 0.0, 0, 0)),
            "requests in flight = active"
        );
    }

    #[test]
    fn render_shows_stats_line_and_no_panic_on_zero_dims() {
        let mut c = ServingCreature::new();
        c.update("Qwen3-32B", 1454, Some(&stats(1, 842.0, 0, 0)));
        let text: String = c
            .render(70, 12)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("Qwen3-32B"));
        assert!(text.contains("842") && text.contains("tok/s"));
        assert!(text.contains("1 running"));
        let _ = c.render(0, 0); // must not panic
    }

    #[test]
    fn no_metrics_renders_calm_alive_state() {
        let mut c = ServingCreature::new();
        c.update("Qwen3-32B", 60, None); // serving=None
        let text: String = c
            .render(70, 12)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("Qwen3-32B"));
        assert!(text.to_lowercase().contains("serving") || text.to_lowercase().contains("ready"));
        assert!(!text.contains("tok/s"), "no rate shown without metrics");
    }
}
