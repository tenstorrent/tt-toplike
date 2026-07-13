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

use crate::animation::inference_load::{fmt_bytes, fmt_elapsed, group_thousands};
use crate::animation::{
    exhaust_count, format_chip, hsv_to_rgb, lane_segments, sparkline, temp_to_hue,
    value_to_char_intensity, ChipReading, BLOCK_CHARS,
};
use crate::ui::colors;
use crate::workload::inference_server::{MediaStats, ServiceState, ServingStats};
use std::time::Instant;

/// Frames a completion "pulse" glyph stays lit after `completed_delta > 0`.
const PULSE_FRAMES: u32 = 6;
/// Frames the tail stays frayed after `errored_delta > 0`.
const FRAY_FRAMES: u32 = 8;
/// `generation_tps` at/above which the wave reaches full amplitude/speed.
const GEN_TPS_REF: f32 = 200.0;
/// Media/diffusion analog of [`GEN_TPS_REF`]: generations-per-minute at/above
/// which the wave reaches full amplitude. Diffusion throughput is orders of
/// magnitude lower than token throughput (SkyReels ≈2 clips/min per device), so
/// a busy multi-device box lands near full tilt without pinning a single one.
const MEDIA_GEN_REF: f32 = 6.0;
/// Max samples kept in the bounded throughput-history ring (the timeline the
/// compositor sparklines). Oldest samples are dropped once this is exceeded.
const HISTORY_CAP: usize = 64;
/// Minimum wall-clock spacing between throughput-history samples. Decoupled
/// from the render FPS so the timeline spans a meaningful window (~64 × this)
/// regardless of how fast the view redraws — the monitor only refreshes data
/// every few seconds anyway, so sampling faster would just flatten the line.
const HISTORY_SAMPLE_MS: u128 = 1000;

/// Body length (cells) for `running` in-flight requests, clamped to `max`.
/// A small base so the creature is always visible; grows with concurrency.
pub fn body_len(running: u32, max: usize) -> usize {
    (4 + running as usize * 3).min(max.max(1))
}

/// Whether the server is doing work right now (drives motion vs. breathing).
pub fn is_active(s: &ServingStats) -> bool {
    s.generation_tps > 0.5 || s.requests_running > 0 || s.requests_waiting > 0
}

/// Workload-agnostic drive signals the animated snake reads, so the render
/// path is single across vLLM and media/diffusion workloads. Built by
/// [`ServingCreature::drive`]; see there for the per-workload mapping.
#[derive(Default)]
struct CreatureDrive {
    /// Timeline headline number and its unit label ("tok/s" | "gen/min").
    timeline_value: f32,
    timeline_label: &'static str,
    /// Decimal places to render `timeline_value` with (tok/s is integer-ish,
    /// gen/min wants one decimal).
    value_decimals: usize,
    /// Wave amplitude/speed normalization in `[0, 1]` (throughput vs a
    /// per-workload reference).
    amp_norm: f32,
    /// Particle-exhaust glyph count trailing the head.
    exhaust: usize,
    /// Snake colour heat in `[0, 1]` (cool blue → hot red).
    heat: f32,
    /// In-flight count (snake body length): vLLM `requests_running`, or the
    /// media `jobs_in_progress` gauge. `waiting` (queued pellets) is
    /// vLLM-only — media has no queue gauge, so it's 0 there.
    running: u32,
    waiting: u32,
    /// Completions this tick (fires the head pulse).
    completed: u32,
}

impl CreatureDrive {
    fn timeline_label_text(&self) -> String {
        format!(
            "{} {:.*} ",
            self.timeline_label, self.value_decimals, self.timeline_value
        )
    }
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
    /// Latest media/diffusion stats (tt-media-inference-server, e.g. SkyReels).
    /// Mutually exclusive with `serving` — a service exposes one namespace or
    /// the other — and drives the same snake via a workload-agnostic `drive()`.
    media: Option<MediaStats>,
    /// Frames left on the completion pulse (a bright head glyph).
    pulse_left: u32,
    /// Frames left on the frayed error tail.
    fray_left: u32,
    /// Latest per-chip telemetry readings, refreshed each `update` (silicon strip).
    chips: Vec<ChipReading>,
    /// Bounded ring of past `generation_tps` samples for the compositor timeline.
    tps_history: Vec<f32>,
    /// When the last history sample was taken, to pace sampling by wall-clock
    /// (`HISTORY_SAMPLE_MS`) independently of the render FPS.
    last_history_push: Option<Instant>,
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
            media: None,
            pulse_left: 0,
            fray_left: 0,
            chips: Vec::new(),
            tps_history: Vec::new(),
            last_history_push: None,
        }
    }

    /// Push one throughput sample onto the bounded history ring, dropping the
    /// oldest when it overflows. Split out from the wall-clock throttle in
    /// [`update`](Self::update) so the ring's bound is unit-testable directly.
    fn push_history(&mut self, tps: f32) {
        self.tps_history.push(tps);
        if self.tps_history.len() > HISTORY_CAP {
            self.tps_history.remove(0);
        }
    }

    /// The latest per-chip telemetry readings folded in by [`update`](Self::update).
    pub fn chips(&self) -> &[ChipReading] {
        &self.chips
    }

    /// The bounded throughput-history ring (oldest→newest `generation_tps`).
    pub fn tps_history(&self) -> &[f32] {
        &self.tps_history
    }

    /// Fold this tick's Ready `ServiceState` (+ its `serving` stats) into the
    /// creature. Pulse/fray are set as short frame-countdowns so a single
    /// completion/error tick leaves a brief visible mark that decays on its own.
    pub fn update(&mut self, svc: &ServiceState, uptime_secs: u64, chips: &[ChipReading]) {
        self.frame = self.frame.wrapping_add(1);
        self.label = svc.label.clone();
        self.uptime_secs = uptime_secs;
        self.cpu_pct = svc.cpu_pct;
        self.rss_bytes = svc.rss_bytes;
        self.serving = svc.serving;
        self.media = svc.media;

        // Refresh the silicon strip every tick; sample the throughput history
        // on a wall-clock cadence (not every frame) so the timeline spans a
        // useful window at 60 FPS instead of collapsing to a flat ~1s of the
        // same value.
        self.chips = chips.to_vec();
        let now = Instant::now();
        let due = self
            .last_history_push
            .map(|t| now.duration_since(t).as_millis() >= HISTORY_SAMPLE_MS)
            .unwrap_or(true);
        if due {
            // History tracks whichever throughput is live (tok/s or gen/min).
            self.push_history(self.drive().timeline_value);
            self.last_history_push = Some(now);
        }

        // Decay any live effect, then re-arm it if this tick earned it. The
        // completion/error deltas come from whichever workload is live so a
        // finished generation pulses the head just like a finished request.
        self.pulse_left = self.pulse_left.saturating_sub(1);
        self.fray_left = self.fray_left.saturating_sub(1);
        let (completed, errored) = svc
            .serving
            .map(|s| (s.completed_delta, s.errored_delta))
            .or_else(|| svc.media.map(|m| (m.completed_delta, m.errored_delta)))
            .unwrap_or((0, 0));
        if completed > 0 {
            self.pulse_left = PULSE_FRAMES;
        }
        if errored > 0 {
            self.fray_left = FRAY_FRAMES;
        }
    }

    /// Collapse whichever workload is live (vLLM `serving` or media) into the
    /// workload-agnostic signals the animated snake reads, so the render path
    /// stays single. Returns all-zero calm defaults when neither is present.
    ///
    /// For media, `running` is driven by the `jobs_in_progress` gauge (so the
    /// snake body grows with in-flight generations, just like vLLM
    /// `requests_running`); there is no queue gauge, so `waiting` stays 0.
    /// Throughput is generations-per-minute rather than tok/s, and "heat" tracks
    /// activity since diffusion servers expose no KV-cache gauge.
    fn drive(&self) -> CreatureDrive {
        if let Some(s) = &self.serving {
            CreatureDrive {
                timeline_value: s.generation_tps,
                timeline_label: "tok/s",
                value_decimals: 0,
                amp_norm: (s.generation_tps / GEN_TPS_REF).clamp(0.0, 1.0),
                exhaust: exhaust_count(s.generation_tps),
                heat: s.kv_cache_usage.clamp(0.0, 1.0),
                running: s.requests_running,
                waiting: s.requests_waiting,
                completed: s.completed_delta,
            }
        } else if let Some(m) = &self.media {
            // Diffusion throughput (gen/min) is tiny and completions are rare
            // (a clip can take minutes), so activity is driven mainly by the
            // in-flight `jobs_in_progress` gauge — that's the immediate "it's
            // working on my load" signal. A busy box (jobs running) animates
            // even between completions; the generation rate adds a little more.
            let rate_amp = (m.generations_per_min / MEDIA_GEN_REF).clamp(0.0, 1.0);
            let busy_amp = if m.jobs_in_progress > 0 { 0.55 } else { 0.0 };
            let amp = rate_amp.max(busy_amp);
            CreatureDrive {
                timeline_value: m.generations_per_min,
                timeline_label: "gen/min",
                value_decimals: 1,
                amp_norm: amp,
                // A short exhaust plume while work is in flight; a completion
                // this tick also fires the head pulse (see `update`).
                exhaust: if m.jobs_in_progress > 0 {
                    (m.jobs_in_progress as usize).clamp(1, 4)
                } else {
                    0
                },
                // No KV gauge on media servers — warm the creature with activity
                // so a box actively generating still reads "hot".
                heat: (0.15 + amp * 0.55).clamp(0.0, 1.0),
                // Body length grows with in-flight generations (the acking
                // signal), exactly as it grows with vLLM `requests_running`.
                running: m.jobs_in_progress,
                waiting: 0,
                completed: m.completed_delta,
            }
        } else {
            CreatureDrive::default()
        }
    }

    /// Build the media/diffusion info panel — the counterpart to
    /// [`panel_rows`](Self::panel_rows) for servers where tok/s is meaningless.
    /// Headline throughput is generations/min plus seconds-per-generation, then
    /// generation tallies, the pipeline stage times
    /// (warmup→preprocess→inference→postprocess), and the container group.
    fn media_panel_rows(&self, m: &MediaStats) -> Vec<(String, Color)> {
        let header = colors::rgb(120, 180, 200);
        let val = colors::text_primary();
        let mut rows: Vec<(String, Color)> = Vec::new();
        let hdr = |rows: &mut Vec<(String, Color)>, t: &str| rows.push((t.to_string(), header));
        // Seconds formatter: sub-minute in seconds, longer warmups in minutes.
        let secs = |s: f32| -> String {
            if s >= 90.0 {
                format!("{:.1} min", s / 60.0)
            } else {
                format!("{:.1} s", s)
            }
        };
        hdr(&mut rows, "generations");
        // In-flight first — the live "it's working on my load" signal.
        rows.push((format!("  in flight {}", m.jobs_in_progress), val));
        rows.push((
            format!(
                "  done     {}",
                group_thousands(m.counters.requests_total as usize)
            ),
            val,
        ));
        if m.counters.errored_total > 0 {
            rows.push((
                format!(
                    "  errors   {}",
                    group_thousands(m.counters.errored_total as usize)
                ),
                val,
            ));
        }
        rows.push((String::new(), val));
        hdr(&mut rows, "throughput");
        rows.push((format!("  gen/min  {:.1}", m.generations_per_min), val));
        rows.push((format!("  per gen  {}", secs(m.duration_avg_s)), val));
        rows.push((String::new(), val));
        // Pipeline stages: show only the ones this server actually reports
        // (post is always present on the current build; the others appear only
        // if the runner emits them, so we don't print misleading 0.0s rows).
        let stages: [(&str, f32); 4] = [
            ("warmup", m.warmup_avg_s),
            ("pre", m.pre_avg_s),
            ("infer", m.inference_avg_s),
            ("post", m.post_avg_s),
        ];
        if stages.iter().any(|(_, v)| *v > 0.0) {
            hdr(&mut rows, "stages");
            for (name, v) in stages {
                if v > 0.0 {
                    rows.push((format!("  {name:<8} {}", secs(v)), val));
                }
            }
            rows.push((String::new(), val));
        }
        hdr(&mut rows, "container");
        rows.push((format!("  CPU      {:.1}%", self.cpu_pct), val));
        rows.push((format!("  RSS      {}", fmt_bytes(self.rss_bytes)), val));
        rows
    }

    /// Build the verbose info panel: grouped key/value rows drawn beside the
    /// snake. `(text, color)` per row; blank rows separate groups. Values come
    /// from the live `serving` stats (or `—` when metrics are absent) plus the
    /// container CPU/RSS. Kept pure/allocating so it's unit-testable.
    ///
    /// `include_requests` gates the "requests" group (running/queued/served/
    /// errors): when the swimlane region is drawn it already shows per-request
    /// state, so the caller passes `false` to avoid repeating it in the panel.
    fn panel_rows(&self, include_requests: bool) -> Vec<(String, Color)> {
        let header = colors::rgb(120, 180, 200); // muted teal group headers
        let val = colors::text_primary();
        let mut rows: Vec<(String, Color)> = Vec::new();
        let hdr = |rows: &mut Vec<(String, Color)>, t: &str| {
            rows.push((t.to_string(), header));
        };
        // Metric getters degrade to "—" when there are no serving stats.
        let (dec, pre, run, wait, served, errs, kv, ttft, prefix, gen_tot, prompt_tot) =
            match &self.serving {
                Some(s) => (
                    format!("{:.0} tok/s", s.generation_tps),
                    format!("{:.0} tok/s", s.prompt_tps),
                    s.requests_running.to_string(),
                    s.requests_waiting.to_string(),
                    group_thousands(s.counters.requests_succeeded_total as usize),
                    group_thousands(s.counters.requests_errored_total as usize),
                    format!("{:.0}%", s.kv_cache_usage * 100.0),
                    format!("{:.2}s", s.ttft_avg_s),
                    format!("{:.0}%", s.prefix_hit_rate * 100.0),
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
                    "—".into(),
                ),
            };

        hdr(&mut rows, "throughput");
        rows.push((format!("  decode   {dec}"), val));
        rows.push((format!("  prefill  {pre}"), val));
        rows.push((String::new(), val));
        // The swimlane region (when drawn) owns per-request state, so this group
        // is emitted only when the caller isn't already showing swimlanes.
        if include_requests {
            hdr(&mut rows, "requests");
            rows.push((format!("  running  {run}"), val));
            rows.push((format!("  queued   {wait}"), val));
            rows.push((format!("  served   {served}"), val));
            rows.push((format!("  errors   {errs}"), val));
            rows.push((String::new(), val));
        }
        hdr(&mut rows, "cache · latency");
        rows.push((format!("  KV cache {kv}"), val));
        rows.push((format!("  TTFT avg {ttft}"), val));
        rows.push((format!("  prefix   {prefix}"), val));
        rows.push((String::new(), val));
        hdr(&mut rows, "container");
        rows.push((format!("  CPU      {:.1}%", self.cpu_pct), val));
        rows.push((format!("  RSS      {}", fmt_bytes(self.rss_bytes)), val));
        rows.push((format!("  tokens   {gen_tot} gen · {prompt_tot} in"), val));
        rows
    }

    /// Render the creature as an adaptive full-screen dashboard composed onto a
    /// single `(char, Color)` canvas: a title row, a throughput **timeline** band
    /// at the top, the animated **snake + token-exhaust** (center-left), request
    /// **swimlanes** + the verbose stats panel (center-right), and a **silicon
    /// strip** across the bottom. Regions are gated on width/height and dropped
    /// in the order timeline → swimlanes → chip strip → (snake + stats always
    /// survive); below `width < 44` it collapses to the snake-only fallback.
    /// Returns `vec![]` on a zero-sized view and never panics on narrow/short
    /// grids — every region is gated on its min size and all indices are bounded.
    pub fn render(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        if width == 0 || height == 0 {
            return vec![];
        }

        let bg = colors::rgb(0, 0, 0);
        let dim = colors::rgb(90, 90, 90);
        let mut lines: Vec<Line<'static>> = Vec::new();

        // --- Title: "{label}  serving · READY · up {elapsed} · {arch}" ---
        // The architecture suffix is appended only when a chip reports a known
        // (non-empty) arch, so the title stays honest when telemetry is absent.
        let arch = self
            .chips
            .iter()
            .map(|c| c.arch)
            .find(|a| !a.is_empty() && *a != "Unknown");
        // Title is three spans so "READY" reads in green while the rest stays
        // primary: "{label}  serving · " + green "READY" + " · up {elapsed}[· {arch}]".
        // The label is budgeted to what's left after the fixed parts so the line
        // stays within `width` (char-safe, no multibyte slicing); ratatui clips
        // any residual overflow at the pane edge.
        let mid = "  serving · ";
        let ready = "READY";
        let suffix = match arch {
            Some(a) => format!(" · up {} · {}", fmt_elapsed(self.uptime_secs), a),
            None => format!(" · up {}", fmt_elapsed(self.uptime_secs)),
        };
        let fixed = mid.chars().count() + ready.chars().count() + suffix.chars().count();
        let label_budget = width.saturating_sub(fixed);
        let label: String = self.label.chars().take(label_budget).collect();
        let primary = Style::default()
            .fg(colors::text_primary())
            .add_modifier(Modifier::BOLD);
        lines.push(Line::from(vec![
            Span::styled(format!("{label}{mid}"), primary),
            Span::styled(
                ready.to_string(),
                Style::default()
                    .fg(colors::success())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(suffix, primary),
        ]));

        // --- Body canvas (only the title row is reserved) ---
        let body_rows = height.saturating_sub(1);
        if body_rows == 0 {
            return lines;
        }
        let mut canvas: Vec<Vec<(char, Color)>> = vec![vec![(' ', bg); width]; body_rows];

        // Workload-agnostic drive (tok/s or gen/min); all zero when calm.
        let drive = self.drive();
        let running = drive.running;
        let waiting = drive.waiting;
        let completed = drive.completed;
        // Snake heat: KV-cache for vLLM, activity for media (see `drive`).
        let kv = drive.heat;
        // vLLM-only swimlane inputs (queue/prefill/decode stage times + served
        // count). All zero for a media workload, so its per-request swimlane
        // region simply never appears — the stage times show in the panel.
        let (q_avg, p_avg, d_avg, served) = match &self.serving {
            Some(s) => (
                s.queue_avg_s,
                s.prefill_avg_s,
                s.decode_avg_s,
                s.counters.requests_succeeded_total,
            ),
            None => (0.0, 0.0, 0.0, 0),
        };

        // Column split: a fixed-ish info panel on the right when there's room,
        // else snake-only. `panel_x` is the first panel column; the left region
        // ends one column before it (a divider at `panel_x - 1`).
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

        // --- Region budget (collapse order: timeline → chip strip → middle) ---
        // Full-width throughput timeline (2 rows) at the top, only with room and
        // live metrics (so the "tok/s" label never appears in a calm no-metrics
        // view). Full-width silicon strip (1 row) at the bottom.
        let has_workload = self.serving.is_some() || self.media.is_some();
        let has_timeline = has_workload && body_rows >= 10 && width >= 60;
        let timeline_h = if has_timeline { 2 } else { 0 };
        let has_strip = body_rows >= timeline_h + 5;
        let strip_h = if has_strip { 1 } else { 0 };
        let mid_top = timeline_h;
        let mid_bot = body_rows - strip_h; // exclusive
        let mid_rows = mid_bot - mid_top; // >= 1 (strip only claimed when room)

        // --- Timeline band: sparkline of throughput history + numeric headline ---
        // The top rows sit above the snake/panel split (no divider up here), so
        // the band spans the FULL width rather than clipping to `snake_right`.
        // Label + units come from `drive` ("tok/s" for vLLM, "gen/min" for media).
        if has_timeline {
            let label = drive.timeline_label_text();
            put_str(&mut canvas, 0, 0, &label, colors::info(), width);
            let spark_x = label.chars().count().min(width);
            let spark_w = width.saturating_sub(spark_x);
            if spark_w > 0 {
                let spark = sparkline(self.tps_history(), spark_w);
                put_str(&mut canvas, 0, spark_x, &spark, colors::success(), width);
                // Second row: a dimmer full-width continuation of the same trace.
                let wide = sparkline(self.tps_history(), width);
                put_str(&mut canvas, 1, 0, &wide, dim, width);
            }
        }

        // --- Swimlanes (center-right, when wide) claim the bottom of the middle
        // left region; the snake gets the rows above them. ---
        // Swimlanes are a vLLM-only concept (per-request queue→prefill→decode).
        // A media workload has `running = jobs_in_progress` but no such stage
        // times, so gate on `serving` — otherwise a diffusion box with jobs in
        // flight would draw a bogus "requests"/"#1" region with zero-width bars.
        let want_swim = self.serving.is_some() && width >= 70 && mid_rows >= 6 && snake_right >= 8;
        let max_lanes = if want_swim {
            5usize.min(mid_rows.saturating_sub(2)).max(1)
        } else {
            0
        };
        let lane_w = snake_right.saturating_sub(4);
        // Idle-but-served: no requests in flight, but we've served some and have
        // stage-time averages — show ONE dim "avg" lane (the typical request
        // shape) so the region stays informative instead of going blank.
        let idle_avg = want_swim && running == 0 && served > 0 && (q_avg + p_avg + d_avg) > 0.0;
        let lanes = if !want_swim {
            Vec::new()
        } else if running > 0 {
            lane_segments(running, q_avg, p_avg, d_avg, lane_w, max_lanes)
        } else if idle_avg {
            lane_segments(1, q_avg, p_avg, d_avg, lane_w, 1)
        } else {
            Vec::new()
        };
        // +1 header row ("requests"); clamp so the snake keeps at least one row.
        let swim_h = if want_swim {
            (lanes.len() + 1).min(mid_rows.saturating_sub(1))
        } else {
            0
        };
        let snake_rows = mid_rows - swim_h;

        // --- Snake + token exhaust (center-left) ---
        let snake_top = mid_top;
        let snake_last = snake_top + snake_rows - 1; // snake_rows >= 1
        let max_len = snake_right.saturating_sub(1);
        let len = body_len(running, max_len);
        let amp_norm = drive.amp_norm;
        let max_amp = (snake_rows.saturating_sub(1)) as f32 / 2.0;
        // A small always-on idle amplitude so the creature gently breathes even
        // at rest (never fully frozen), rising with throughput. The y-write is
        // clamped to the snake rows, so the +idle can't overflow the region.
        let amp = 0.45 + amp_norm * max_amp;
        let center = snake_top as f32 + (snake_rows as f32 - 1.0) / 2.0;
        // Per-frame phase step, tuned for the 60 FPS anim cadence (≈2–5 rad/s):
        // gentle undulation at rest, livelier under load. Not the old 10 FPS
        // value (which would whip around 6× too fast at 60 FPS).
        let speed = 0.03 + 0.05 * amp_norm;
        let heat = hsv_to_rgb(240.0 * (1.0 - kv.clamp(0.0, 1.0)), 0.85, 0.95);
        let (mut head_x, mut head_y) = (0usize, snake_top);
        for i in 0..len {
            let x = 1 + i;
            if x >= snake_right {
                break;
            }
            let phase = self.frame as f32 * speed + i as f32 * 0.6;
            let y = (center + amp * phase.sin()).round();
            let y = ((y.max(0.0) as usize).max(snake_top)).min(snake_last);
            let is_head = i + 1 == len;
            let is_tail = i == 0;
            let (glyph, color) = if is_tail && self.fray_left > 0 {
                ('✗', colors::error())
            } else if is_head && self.pulse_left > 0 {
                ('✦', colors::text_primary())
            } else if is_head {
                ('▶', heat)
            } else {
                ('●', heat)
            };
            canvas[y][x] = (glyph, color);
            if is_head {
                head_x = x;
                head_y = y;
            }
        }
        // Throughput exhaust: `drive.exhaust` dim glyphs drifting right of the
        // head, frame-indexed so they animate; none at rest. Bounds-clamped.
        if head_x > 0 {
            let n = drive.exhaust;
            // Slow the drift cycle for the 60 FPS cadence so particles stream
            // rather than strobe.
            let drift = (self.frame as usize / 4) % 3;
            for k in 0..n {
                let ex = head_x + 1 + k + drift;
                if ex >= snake_right {
                    break;
                }
                let ch = if k % 2 == 0 { '»' } else { '·' };
                canvas[head_y][ex] = (ch, dim);
            }
        }
        // Dim queued pellets trail one per waiting req, on the snake center row.
        let pellet_row = (center.round().max(0.0) as usize).clamp(snake_top, snake_last);
        let mut px = 1 + len + 1;
        for _ in 0..waiting {
            if px >= snake_right {
                break;
            }
            canvas[pellet_row][px] = ('·', dim);
            px += 1;
        }

        // --- Swimlanes: one shaded [queue|prefill|decode] bar per running req ---
        if swim_h > 0 {
            let swim_top = snake_top + snake_rows;
            // Header carries the cumulative served count so the region reads as
            // informative even when idle (no live lanes).
            let header = if served > 0 {
                format!("requests · {} served", group_thousands(served as usize))
            } else {
                "requests".to_string()
            };
            put_str(
                &mut canvas,
                swim_top,
                1,
                &header,
                colors::info(),
                snake_right,
            );
            // Completion marker: a bright pulse glyph beside the header,
            // lit while a completion is fresh (this tick, or during its pulse).
            if completed > 0 || self.pulse_left > 0 {
                let mx = 1 + header.chars().count() + 1;
                if mx < snake_right {
                    canvas[swim_top][mx] = ('✦', colors::success());
                }
            }
            let q_col = colors::rgb(90, 110, 130); // queue: dim blue-grey
            let p_col = colors::rgb(120, 170, 200); // prefill: mid teal
            let d_col = colors::success(); // decode: bright teal
            for (li, seg) in lanes.iter().enumerate() {
                let row = swim_top + 1 + li;
                if row > snake_last + swim_h || row >= mid_bot {
                    break;
                }
                // Live lanes are numbered; the idle representative lane is "avg".
                let label = if idle_avg {
                    "avg ".to_string()
                } else {
                    format!("#{} ", li + 1)
                };
                put_str(&mut canvas, row, 1, &label, dim, snake_right);
                let mut x = 1 + label.chars().count();
                let stages = [
                    (seg[0], 0.3_f32, q_col),
                    (seg[1], 0.6, p_col),
                    (seg[2], 1.0, d_col),
                ];
                for (w, intensity, color) in stages {
                    let ch = value_to_char_intensity(intensity, &BLOCK_CHARS);
                    for _ in 0..w {
                        if x >= snake_right {
                            break;
                        }
                        canvas[row][x] = (ch, color);
                        x += 1;
                    }
                }
            }
        }

        // --- Info / stats panel (right region) + divider ---
        if panel_w > 0 {
            let div = panel_x - 1;
            for row in canvas.iter_mut().take(mid_bot).skip(mid_top) {
                row[div] = ('│', dim);
            }
            // Media workloads get the diffusion panel (gen/min, per-gen time,
            // stage durations); vLLM workloads get the token/request panel. For
            // vLLM, when the swimlane region is drawn it already carries
            // per-request state, so the panel omits its "requests" group to
            // avoid duplication (info never fully disappears).
            let rows = match &self.media {
                Some(m) => self.media_panel_rows(m),
                None => self.panel_rows(swim_h == 0),
            };
            for (r, (text, color)) in rows.into_iter().enumerate() {
                let row = mid_top + r;
                if row >= mid_bot {
                    break;
                }
                put_str(&mut canvas, row, panel_x, &text, color, width);
            }
        }

        // --- Silicon strip (bottom row): one shaded cell per chip ---
        if strip_h > 0 {
            let row = mid_bot; // strip occupies [mid_bot, body_rows)
            if self.chips.is_empty() {
                put_str(&mut canvas, row, 0, "no device telemetry", dim, width);
            } else {
                let mut x = 0usize;
                for chip in &self.chips {
                    if x >= width {
                        break;
                    }
                    let cell = format_chip(chip);
                    let hue = temp_to_hue(chip.temp_c.unwrap_or(0.0));
                    let color = hsv_to_rgb(hue, 0.75, 0.95);
                    put_str(&mut canvas, row, x, &cell, color, width);
                    x += cell.chars().count() + 3; // gap between chip cells
                }
            }
        }

        for row in canvas {
            lines.push(row_to_line(row, bg));
        }
        lines
    }
}

/// Write `s` onto canvas `row` starting at column `col`, one char per cell,
/// clipped at `max_x` and at the row length. A no-op when `row` is off-canvas.
/// Every index is bounds-checked so callers never panic on a tight grid.
fn put_str(
    canvas: &mut [Vec<(char, Color)>],
    row: usize,
    col: usize,
    s: &str,
    color: Color,
    max_x: usize,
) {
    if row >= canvas.len() {
        return;
    }
    let line = &mut canvas[row];
    for (i, ch) in s.chars().enumerate() {
        let x = col + i;
        if x >= max_x || x >= line.len() {
            break;
        }
        line[x] = (ch, color);
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
    use crate::animation::ChipReading;
    use crate::workload::inference_server::{
        MediaCounters, MediaStats, Phase, Readiness, ServiceState, ServingStats, VllmCounters,
    };

    /// A media/diffusion stats sample (SkyReels-shaped): a modest generations
    /// rate, `jobs` in flight, `done` completed, a long end-to-end per-gen time.
    fn media_stats(gen_per_min: f32, done: u64, jobs: u32, completed: u32) -> MediaStats {
        MediaStats {
            generations_per_min: gen_per_min,
            jobs_in_progress: jobs,
            completed_delta: completed,
            errored_delta: 0,
            duration_avg_s: 612.0,
            post_avg_s: 0.32,
            pre_avg_s: 0.0,
            inference_avg_s: 0.0,
            warmup_avg_s: 0.0,
            counters: MediaCounters {
                requests_total: done,
                ..Default::default()
            },
        }
    }

    /// A Ready `ServiceState` carrying media stats instead of vLLM serving.
    fn ready_media_svc(label: &str, media: MediaStats) -> ServiceState {
        ServiceState {
            media: Some(media),
            ..ready_svc(label, 12.0, 40 * 1024 * 1024 * 1024, None)
        }
    }

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
            queue_avg_s: 0.0,
            prefill_avg_s: 0.0,
            decode_avg_s: 0.0,
            tpot_avg_s: 0.0,
            prefix_hit_rate: 0.0,
            preemptions_delta: 0,
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
            loaded_count: 0,
            loaded_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::Ready { runner: None },
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving,
            media: None,
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
        c.update(&svc, 1454, &[]);
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
        // At 80x24 the swimlane region draws (width >= 70), so it owns per-request
        // state and the panel omits its "requests" group to avoid duplication.
        // Prove the request info is present exactly once — in the swimlane region,
        // via its "requests" header and a "#1" lane label — not in the panel.
        assert!(text.contains("requests"), "swimlane requests header");
        assert!(
            text.contains("#1"),
            "swimlane lane label (request shown once)"
        );
        assert!(
            !text.contains("running"),
            "panel must not repeat the requests group when swimlanes draw"
        );
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
    fn media_workload_shows_diffusion_panel_not_tokens() {
        // Regression: a media/diffusion server (SkyReels) reports the
        // `tt_media_server_*` namespace, not vLLM tokens — the panel must show
        // generations + stage durations, and NEVER "tok/s".
        let mut c = ServingCreature::new();
        // 2 jobs in flight, 43 done — the "acking" case the user hit.
        let svc = ready_media_svc("SkyReels-V2-I2V", media_stats(2.1, 43, 2, 1));
        c.update(&svc, 720, &[]);
        let text: String = c
            .render(80, 24)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("SkyReels-V2-I2V"), "title label");
        // The in-flight gauge must surface — that's the acking signal.
        assert!(
            text.contains("in flight") && text.contains("generations"),
            "in-flight generations group"
        );
        // Headline throughput as generations, not tokens.
        assert!(text.contains("gen/min"), "generations-per-minute headline");
        assert!(text.contains("2.1"), "the actual gen/min value");
        // Only stages the server reports appear (post here; no misleading 0.0s
        // warmup/pre/infer rows on this build).
        assert!(text.contains("per gen"), "end-to-end per-generation time");
        assert!(
            !text.contains("tok/s"),
            "tok/s is meaningless for diffusion and must not appear"
        );
        assert!(
            !text.contains("KV cache"),
            "no KV-cache group for a media workload"
        );
        // Swimlanes are vLLM-only (per-request queue→prefill→decode). Even with
        // jobs in flight at a wide size, a media workload must NOT draw the
        // "requests" swimlane header or numbered "#1" lanes — those are the
        // stat-panel's job here (regression guard for the gating fix).
        assert!(
            !text.contains("#1") && !text.contains("requests"),
            "media workload must not render vLLM request swimlanes:\n{text}"
        );
        // Must not panic across a zero and a narrow grid either.
        let _ = c.render(0, 0);
        let _ = c.render(20, 8);
    }

    #[test]
    fn media_timeline_history_tracks_generations() {
        // The throughput history ring should track gen/min for a media workload
        // (so the timeline sparkline is meaningful), seeded on the first update.
        let mut c = ServingCreature::new();
        let svc = ready_media_svc("SkyReels", media_stats(3.0, 10, 1, 1));
        c.update(&svc, 60, &[]);
        assert_eq!(c.tps_history().len(), 1, "first update seeds history");
        assert!(
            (c.tps_history()[0] - 3.0).abs() < 1e-4,
            "history sample is the gen/min rate, got {}",
            c.tps_history()[0]
        );
    }

    #[test]
    fn media_snake_body_grows_with_in_flight_jobs() {
        // The snake body length must track in-flight jobs (the acking signal),
        // so sending load visibly lengthens/animates the creature.
        let mut idle = ServingCreature::new();
        idle.update(
            &ready_media_svc("SkyReels", media_stats(0.0, 5, 0, 0)),
            60,
            &[],
        );
        let mut busy = ServingCreature::new();
        busy.update(
            &ready_media_svc("SkyReels", media_stats(0.0, 5, 3, 0)),
            60,
            &[],
        );
        assert_eq!(idle.drive().running, 0, "no jobs → base body");
        assert_eq!(busy.drive().running, 3, "3 jobs in flight → longer body");
        assert!(
            busy.drive().amp_norm > idle.drive().amp_norm,
            "in-flight work animates the creature even at 0 gen/min"
        );
    }

    #[test]
    fn update_records_chips_and_throughput_history() {
        let mut c = ServingCreature::new();
        let chips = [ChipReading {
            index: 0,
            arch: "Blackhole",
            power_w: Some(90.0),
            temp_c: Some(70.0),
            aiclk_mhz: Some(1350),
        }];
        let svc = ready_svc("M", 2.0, 1024, Some(stats(1, 842.0, 0, 0)));
        c.update(&svc, 10, &chips);
        assert_eq!(c.chips().len(), 1, "chips stored");
        assert!(
            c.tps_history().len() >= 1,
            "first update seeds a history sample"
        );
        // History sampling is wall-clock throttled (HISTORY_SAMPLE_MS), so rapid
        // updates don't spam the ring — it stays tiny under a burst of updates.
        for _ in 0..500 {
            c.update(&svc, 12, &chips);
        }
        assert!(
            c.tps_history().len() <= 3,
            "wall-clock throttle keeps rapid updates from flooding history"
        );
        // The ring's bound itself: pushing past the cap drops the oldest.
        for i in 0..(HISTORY_CAP + 50) {
            c.push_history(i as f32);
        }
        assert_eq!(c.tps_history().len(), HISTORY_CAP, "ring bounded to cap");
    }

    #[test]
    fn idle_swimlanes_show_avg_lane_and_served_count() {
        // Idle (0 in-flight) but has served history + stage averages: the
        // swimlane region should show a representative "avg" lane and the
        // cumulative served count, not go blank.
        let mut c = ServingCreature::new();
        let mut s = stats(0, 0.0, 0, 0); // 0 running, 0 tps → idle
        s.queue_avg_s = 0.01;
        s.prefill_avg_s = 20.0;
        s.decode_avg_s = 0.4;
        s.counters.requests_succeeded_total = 11;
        let svc = ready_svc("Llama-3.3-70B", 3.0, 1024, Some(s));
        c.update(&svc, 100, &[]);
        let text: String = c
            .render(100, 30)
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string()))
            .collect();
        assert!(
            text.contains("11 served"),
            "header shows served count when idle"
        );
        assert!(text.contains("avg"), "idle shows a representative avg lane");
    }

    #[test]
    fn dashboard_composes_regions_at_large_size_and_degrades() {
        let mut c = ServingCreature::new();
        let chips = [ChipReading {
            index: 0,
            arch: "Blackhole",
            power_w: Some(92.0),
            temp_c: Some(78.0),
            aiclk_mhz: Some(1350),
        }];
        let mut svc = ready_svc(
            "Qwen3-32B",
            2.8,
            70 * 1024 * 1024 * 1024,
            Some(stats(2, 842.0, 0, 0)),
        );
        // give swimlanes some stage times
        if let Some(s) = svc.serving.as_mut() {
            s.queue_avg_s = 0.5;
            s.prefill_avg_s = 1.0;
            s.decode_avg_s = 3.0;
        }
        // Two ticks so the throughput ring has samples for the timeline sparkline.
        c.update(&svc, 1453, &chips);
        c.update(&svc, 1454, &chips);
        let big: String = c
            .render(100, 30)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(big.contains("Qwen3-32B"), "title"); // title
        assert!(big.contains("tok/s"), "timeline / stats"); // timeline / stats
        assert!(big.contains("requests"), "swimlane header"); // swimlane header
        assert!(big.contains("BH0"), "chip strip"); // chip strip
                                                    // Pin the timeline band: a sparkline glyph must be present (not satisfied
                                                    // by the stats panel alone).
        assert!(
            big.chars().any(|ch| "▁▂▃▄▅▆▇█".contains(ch)),
            "timeline sparkline glyph"
        );
        // Pin the swimlane region: a "#1" lane label (also panel-independent).
        assert!(big.contains("#1"), "swimlane lane label");
        // Degrade: tiny sizes must not panic and must still show the label.
        for (w, h) in [(100, 6), (60, 10), (30, 8), (10, 4), (0, 0)] {
            let _ = c.render(w, h);
        }
    }

    #[test]
    fn no_metrics_shows_dashes_not_rates() {
        let mut c = ServingCreature::new();
        let svc = ready_svc("Qwen3-32B", 0.0, 0, None); // serving=None
        c.update(&svc, 60, &[]);
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

    #[test]
    fn ready_word_renders_green() {
        // The title's "READY" token is styled with the success (green) color,
        // distinct from the primary-colored rest of the title.
        let mut c = ServingCreature::new();
        let svc = ready_svc("Qwen3-32B", 10.0, 8_000_000_000, None);
        c.update(&svc, 60, &[]);
        let ready_green = c
            .render(80, 24)
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| {
                s.content.contains("READY") && s.style.fg == Some(crate::ui::colors::success())
            });
        assert!(ready_green, "READY renders in the success/green color");
    }
}
