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
use crate::animation::inference_load::{fmt_bytes, fmt_elapsed, group_thousands};
use crate::ui::colors;
use crate::workload::inference_server::{ServiceState, ServingStats};

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

/// The Feeding creature: a horizontal serving snake driven by live vLLM stats,
/// beside a verbose info panel.
pub struct ServingCreature {
    /// Monotonic frame counter driving the wave phase.
    frame: u64,
    /// Model / service label shown in the title.
    label: String,
    /// Server uptime in seconds (formatted into the title).
    uptime_secs: u64,
    /// Container CPU% (from `ServiceState`) shown in the panel.
    cpu_pct: f32,
    /// Container RSS bytes (from `ServiceState`) shown in the panel.
    rss_bytes: u64,
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
            cpu_pct: 0.0,
            rss_bytes: 0,
            serving: None,
            pulse_left: 0,
            fray_left: 0,
        }
    }

    /// Fold this tick's Ready `ServiceState` (+ its `serving` stats) into the
    /// creature. Pulse/fray are set as short frame-countdowns so a single
    /// completion/error tick leaves a brief visible mark that decays on its own.
    pub fn update(&mut self, svc: &ServiceState, uptime_secs: u64) {
        self.frame = self.frame.wrapping_add(1);
        self.label = svc.label.clone();
        self.uptime_secs = uptime_secs;
        self.cpu_pct = svc.cpu_pct;
        self.rss_bytes = svc.rss_bytes;
        self.serving = svc.serving;

        // Decay any live effect, then re-arm it if this tick earned it.
        self.pulse_left = self.pulse_left.saturating_sub(1);
        self.fray_left = self.fray_left.saturating_sub(1);
        if let Some(s) = &svc.serving {
            if s.completed_delta > 0 {
                self.pulse_left = PULSE_FRAMES;
            }
            if s.errored_delta > 0 {
                self.fray_left = FRAY_FRAMES;
            }
        }
    }

    /// Build the verbose info panel: grouped key/value rows drawn beside the
    /// snake. `(text, color)` per row; blank rows separate groups. Values come
    /// from the live `serving` stats (or `—` when metrics are absent) plus the
    /// container CPU/RSS. Kept pure/allocating so it's unit-testable.
    fn panel_rows(&self) -> Vec<(String, Color)> {
        let header = colors::rgb(120, 180, 200); // muted teal group headers
        let val = colors::TEXT_PRIMARY;
        let mut rows: Vec<(String, Color)> = Vec::new();
        let hdr = |rows: &mut Vec<(String, Color)>, t: &str| {
            rows.push((t.to_string(), header));
        };
        // Metric getters degrade to "—" when there are no serving stats.
        let (dec, pre, run, wait, served, errs, kv, ttft, gen_tot, prompt_tot) = match &self.serving
        {
            Some(s) => (
                format!("{:.0} tok/s", s.generation_tps),
                format!("{:.0} tok/s", s.prompt_tps),
                s.requests_running.to_string(),
                s.requests_waiting.to_string(),
                group_thousands(s.counters.requests_succeeded_total as usize),
                group_thousands(s.counters.requests_errored_total as usize),
                format!("{:.0}%", s.kv_cache_usage * 100.0),
                format!("{:.2}s", s.ttft_avg_s),
                group_thousands(s.counters.generation_tokens_total as usize),
                group_thousands(s.counters.prompt_tokens_total as usize),
            ),
            None => (
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
            ),
        };

        hdr(&mut rows, "throughput");
        rows.push((format!("  decode   {dec}"), val));
        rows.push((format!("  prefill  {pre}"), val));
        rows.push((String::new(), val));
        hdr(&mut rows, "requests");
        rows.push((format!("  running  {run}"), val));
        rows.push((format!("  queued   {wait}"), val));
        rows.push((format!("  served   {served}"), val));
        rows.push((format!("  errors   {errs}"), val));
        rows.push((String::new(), val));
        hdr(&mut rows, "cache · latency");
        rows.push((format!("  KV cache {kv}"), val));
        rows.push((format!("  TTFT avg {ttft}"), val));
        rows.push((String::new(), val));
        hdr(&mut rows, "container");
        rows.push((format!("  CPU      {:.1}%", self.cpu_pct), val));
        rows.push((format!("  RSS      {}", fmt_bytes(self.rss_bytes)), val));
        rows.push((format!("  tokens   {gen_tot} gen · {prompt_tot} in"), val));
        rows
    }

    /// Render the creature to owned Ratatui lines: a title row, then a body that
    /// splits into the animated snake (left) and a verbose info panel (right,
    /// when the view is wide enough). Returns `vec![]` on a zero-sized view;
    /// never panics on narrow/short grids (falls back to snake-only when narrow).
    pub fn render(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        if width == 0 || height == 0 {
            return vec![];
        }

        let bg = colors::rgb(0, 0, 0);
        let dim = colors::rgb(90, 90, 90);
        let mut lines: Vec<Line<'static>> = Vec::new();

        // --- Title: "{label}  serving · READY · up {elapsed}" (char-safe) ---
        let title_full = format!(
            "{}  serving · READY · up {}",
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

        // --- Body canvas (only the title row is reserved; the panel lives in
        // the body beside the snake, so there's no separate footer row) ---
        let body_rows = height.saturating_sub(1);
        if body_rows == 0 {
            return lines;
        }
        let mut canvas: Vec<Vec<(char, Color)>> = vec![vec![(' ', bg); width]; body_rows];

        // Column split: a fixed-ish info panel on the right when there's room,
        // else snake-only. `panel_x` is the first panel column; the snake draws
        // left of a one-column divider at `panel_x - 1`.
        let panel_w = if width >= 44 {
            30usize.min(width / 2)
        } else {
            0
        };
        let panel_x = width - panel_w;
        let snake_right = if panel_w > 0 {
            panel_x.saturating_sub(1)
        } else {
            width
        };

        // --- Snake (left region) ---
        let (gen_tps, kv, running, waiting) = match &self.serving {
            Some(s) => (
                s.generation_tps,
                s.kv_cache_usage,
                s.requests_running,
                s.requests_waiting,
            ),
            None => (0.0, 0.0, 0, 0),
        };
        let max_len = snake_right.saturating_sub(1);
        let len = body_len(running, max_len);
        // Wave amplitude/speed scale with throughput; 0 tps → flat + still.
        let amp_norm = (gen_tps / GEN_TPS_REF).clamp(0.0, 1.0);
        let max_amp = (body_rows.saturating_sub(1)) as f32 / 2.0;
        let amp = amp_norm * max_amp;
        let center = (body_rows as f32 - 1.0) / 2.0;
        let speed = 0.12 + 0.18 * amp_norm;
        let heat = hsv_to_rgb(240.0 * (1.0 - kv.clamp(0.0, 1.0)), 0.85, 0.95);
        let last_row = body_rows - 1;
        for i in 0..len {
            let x = 1 + i;
            if x >= snake_right {
                break;
            }
            let phase = self.frame as f32 * speed + i as f32 * 0.6;
            let y = (center + amp * phase.sin()).round();
            let y = (y.max(0.0) as usize).min(last_row);
            let is_head = i + 1 == len;
            let is_tail = i == 0;
            let (glyph, color) = if is_tail && self.fray_left > 0 {
                ('✗', colors::ERROR)
            } else if is_head && self.pulse_left > 0 {
                ('✦', colors::TEXT_PRIMARY)
            } else if is_head {
                ('▶', heat)
            } else {
                ('●', heat)
            };
            canvas[y][x] = (glyph, color);
        }
        // Dim queued pellets trail to the right of the snake, one per waiting req.
        let pellet_row = (center.round().max(0.0) as usize).min(last_row);
        let mut px = 1 + len + 1;
        for _ in 0..waiting {
            if px >= snake_right {
                break;
            }
            canvas[pellet_row][px] = ('·', dim);
            px += 1;
        }

        // --- Info panel (right region) + divider ---
        if panel_w > 0 {
            let div = panel_x - 1;
            for row in canvas.iter_mut() {
                row[div] = ('│', dim);
            }
            for (r, (text, color)) in self.panel_rows().into_iter().enumerate() {
                if r >= body_rows {
                    break;
                }
                for (c, ch) in text.chars().enumerate() {
                    let x = panel_x + c;
                    if x >= width {
                        break;
                    }
                    canvas[r][x] = (ch, color);
                }
            }
        }

        for row in canvas {
            lines.push(row_to_line(row, bg));
        }
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
    use crate::workload::inference_server::{
        Phase, Readiness, ServiceState, ServingStats, VllmCounters,
    };

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

    /// A Ready `ServiceState` for the Feeding creature, with optional serving.
    fn ready_svc(label: &str, cpu: f32, rss: u64, serving: Option<ServingStats>) -> ServiceState {
        ServiceState {
            key: label.into(),
            label: label.into(),
            phase: Phase::Ready,
            cpu_pct: cpu,
            rss_bytes: rss,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::Ready { runner: None },
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving,
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
    fn verbose_panel_shows_grouped_stats_and_no_panic_on_zero_dims() {
        let mut c = ServingCreature::new();
        let svc = ready_svc(
            "Qwen3-32B",
            2.8,
            70 * 1024 * 1024 * 1024,
            Some(stats(1, 842.0, 0, 0)),
        );
        c.update(&svc, 1454);
        // Tall + wide so the whole panel fits beside the snake.
        let text: String = c
            .render(80, 24)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("Qwen3-32B"), "title label");
        // Grouped, verbose panel content:
        assert!(
            text.contains("throughput") && text.contains("decode"),
            "throughput group"
        );
        assert!(text.contains("842 tok/s"), "decode rate");
        assert!(
            text.contains("requests") && text.contains("running"),
            "requests group"
        );
        assert!(text.contains("served"), "served row");
        assert!(
            text.contains("container") && text.contains("CPU"),
            "container group"
        );
        assert!(
            text.contains("RSS") && text.contains("70.0G"),
            "RSS from ServiceState"
        );
        let _ = c.render(0, 0); // must not panic
        let _ = c.render(20, 8); // narrow (snake-only fallback) must not panic
    }

    #[test]
    fn no_metrics_shows_dashes_not_rates() {
        let mut c = ServingCreature::new();
        let svc = ready_svc("Qwen3-32B", 0.0, 0, None); // serving=None
        c.update(&svc, 60);
        let text: String = c
            .render(80, 24)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("Qwen3-32B"));
        assert!(text.contains("READY"), "title shows readiness");
        assert!(!text.contains("tok/s"), "no rate shown without metrics");
        assert!(text.contains('—'), "metric rows show em-dash placeholders");
    }
}
