//! The Inference view's atmosphere: a backdrop drawn *behind* whatever the view
//! already shows, so loading, thinking and generating have a feel.
//!
//! The backdrop is a canvas of optional cells. [`composite`] lays it under the
//! existing renderer's lines and fills blank cells only, so the snake, the
//! dashboards and the roster always win. Every effect maps to a real signal in a
//! [`ServiceState`]; see the spec for the table. Everything here is deterministic:
//! no randomness, positions come from a spatial hash, motion from a frame counter.

use crate::animation::hsv_to_rgb;
use crate::animation::train_sky::{aurora, sky_cell, star_hash, STAR_THRESHOLD};
use crate::workload::inference_server::{Phase, ServiceState};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// One backdrop cell: a glyph and its colour, or nothing.
pub type Cell = Option<(char, Color)>;
/// A backdrop, `height` rows of `width` cells.
pub type Canvas = Vec<Vec<Cell>>;

/// What the featured service is doing, which decides the backdrop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// Nothing featured: the view is left exactly as it is.
    None,
    /// Compiling or loading.
    Forge,
    /// Requests are in but no token has come out yet (prefill: reading).
    Storm,
    /// Tokens are flowing.
    Aurora,
    /// Ready and idle.
    Dream,
    /// A diffusion or video server has a job in flight.
    Forming,
    /// No progress for five minutes.
    Stalled,
}

/// The service the backdrop follows: a loading (or stalled) one wins, else the
/// first ready one. The same rule the snake uses to pick its behaviour.
pub fn featured(rows: &[ServiceState]) -> Option<&ServiceState> {
    rows.iter()
        .find(|s| matches!(s.phase, Phase::Compiling | Phase::Loading | Phase::Alarm))
        .or_else(|| rows.iter().find(|s| s.phase == Phase::Ready))
}

/// The scene for a featured service.
pub fn scene_of(row: Option<&ServiceState>) -> Scene {
    let Some(s) = row else {
        return Scene::None;
    };
    match s.phase {
        Phase::Down => Scene::None,
        Phase::Compiling | Phase::Loading => Scene::Forge,
        Phase::Alarm => Scene::Stalled,
        Phase::Ready => {
            if s.media.as_ref().is_some_and(|m| m.jobs_in_progress > 0) {
                return Scene::Forming;
            }
            match &s.serving {
                Some(sv) if sv.generation_tps >= 1.0 => Scene::Aurora,
                Some(sv) if sv.requests_running > 0 || sv.requests_waiting > 0 => Scene::Storm,
                _ => Scene::Dream,
            }
        }
    }
}

/// One output character of a composited row, with its style.
struct Col {
    ch: char,
    style: Style,
}

/// Lay `canvas` under `lines`, filling blank cells only. A cell the foreground
/// draws is never replaced: not a non-space character, and not a space that has
/// its own background (a panel). A wide character keeps its width so later
/// columns stay aligned. A short line is padded with backdrop cells up to the
/// row's last lit cell (and no further than `width`). A row with no backdrop, or
/// a zero `width`, is returned unchanged.
pub fn composite(canvas: &Canvas, lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }
    let mut out = Vec::with_capacity(lines.len());
    for (y, line) in lines.into_iter().enumerate() {
        let Some(row) = canvas.get(y) else {
            out.push(line);
            continue;
        };
        let Some(last) = row.iter().rposition(|c| c.is_some()) else {
            out.push(line);
            continue;
        };
        let mut cols: Vec<Col> = Vec::with_capacity(width);
        let mut x = 0usize;
        for span in &line.spans {
            for c in span.content.chars() {
                let w = c.width().unwrap_or(0);
                if w == 0 {
                    // A combining mark: it attaches to the previous column.
                    cols.push(Col {
                        ch: c,
                        style: span.style,
                    });
                    continue;
                }
                let blank = c == ' ' && span.style.bg.is_none();
                match row.get(x).copied().flatten() {
                    Some((ch, color)) if blank && w == 1 => cols.push(Col {
                        ch,
                        style: Style::default().fg(color),
                    }),
                    _ => cols.push(Col {
                        ch: c,
                        style: span.style,
                    }),
                }
                x += w;
            }
        }
        while x <= last && x < width {
            match row.get(x).copied().flatten() {
                Some((ch, color)) => cols.push(Col {
                    ch,
                    style: Style::default().fg(color),
                }),
                None => cols.push(Col {
                    ch: ' ',
                    style: Style::default(),
                }),
            }
            x += 1;
        }
        // Group runs of one style back into spans.
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_style = Style::default();
        for col in cols.iter() {
            if !run.is_empty() && col.style != run_style {
                spans.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            run_style = col.style;
            run.push(col.ch);
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, run_style));
        }
        out.push(Line { spans, ..line });
    }
    out
}

/// Cumulative failed-request count of a service (vLLM errors and aborts, or a
/// media server's failed generations).
fn errors_total(row: &ServiceState) -> u64 {
    if let Some(sv) = &row.serving {
        sv.counters.requests_errored_total
    } else if let Some(m) = &row.media {
        m.counters.errored_total
    } else {
        0
    }
}

/// Cumulative preemptions of a vLLM service.
fn preemptions_total(row: &ServiceState) -> u64 {
    row.serving
        .as_ref()
        .map(|s| s.counters.preemptions_total)
        .unwrap_or(0)
}

/// Aurora cells fainter than this are left empty, which keeps the aurora from
/// covering a third of the screen at full signal.
const AURORA_MIN: f32 = 0.20;

/// Smallest area the backdrop is drawn in.
const MIN_W: usize = 4;
const MIN_H: usize = 3;

/// Per-update smoothing of the gauges (a fraction of the gap closed each call).
const SMOOTH: f32 = 0.15;
/// Per-update decay of a gauge's remembered peak, so it adapts to any hardware.
const PEAK_DECAY: f32 = 0.999;

/// A rate scaled against the largest value seen recently, so a slow box and a
/// fast one both fill the 0..1 range.
#[derive(Debug, Clone, Copy)]
struct Gauge {
    peak: f32,
    value: f32,
}

impl Gauge {
    fn new() -> Self {
        Self {
            peak: 1.0,
            value: 0.0,
        }
    }

    fn feed(&mut self, rate: f32) {
        let rate = rate.max(0.0);
        self.peak = (self.peak * PEAK_DECAY).max(rate).max(1.0);
        let target = (rate / self.peak).clamp(0.0, 1.0);
        self.value += (target - self.value) * SMOOTH;
    }
}

/// The Inference atmosphere: observe the roster each update, then paint a
/// backdrop for the featured service's scene.
#[derive(Debug, Clone)]
pub struct Atmosphere {
    frame: u64,
    scene: Scene,
    /// Generation tokens per second, normalised.
    energy: Gauge,
    /// KV-cache use, 0..1, smoothed.
    kv: f32,
    /// Prompt tokens per second, normalised.
    reading: Gauge,
    running: u32,
    waiting: u32,
    /// The service the counters below belong to. A different service, or none,
    /// starts a fresh baseline.
    key: Option<String>,
    seen: Seen,
    /// Birth frame and index of each live completion flare.
    flares: Vec<(u64, u32)>,
    static_until: u64,
    splash_until: u64,
}

/// Cumulative counters from the last observed scrape. The monitor snapshot
/// repeats for several seconds while the render loop reads it every frame, so
/// every event is a *rise* of one of these, never a per-tick delta.
#[derive(Debug, Clone, Copy, Default)]
struct Seen {
    done: Option<u64>,
    errored: Option<u64>,
    preempted: Option<u64>,
}

/// How much a counter rose since last seen. The first sight of a counter is a
/// baseline (0), and a counter that went down (a restarted server) is a reset
/// (0), never a burst.
fn rise(prev: &mut Option<u64>, now: u64) -> u64 {
    let r = prev.map(|p| now.saturating_sub(p)).unwrap_or(0);
    *prev = Some(now);
    r
}

const FLARE_FRAMES: u64 = 24;
const MAX_FLARES: usize = 8;
const STATIC_FRAMES: u64 = 40;
const SPLASH_FRAMES: u64 = 30;

impl Default for Atmosphere {
    fn default() -> Self {
        Self::new()
    }
}

impl Atmosphere {
    pub fn new() -> Self {
        Self {
            frame: 0,
            scene: Scene::None,
            energy: Gauge::new(),
            kv: 0.0,
            reading: Gauge::new(),
            running: 0,
            waiting: 0,
            key: None,
            seen: Seen::default(),
            flares: Vec::new(),
            static_until: 0,
            splash_until: 0,
        }
    }

    /// Take in this update's roster. Cheap: a few float operations.
    pub fn observe(&mut self, rows: &[ServiceState]) {
        self.frame = self.frame.wrapping_add(1);
        let row = featured(rows);
        self.scene = scene_of(row);
        let serving = row.and_then(|r| r.serving.as_ref());
        self.energy
            .feed(serving.map(|s| s.generation_tps).unwrap_or(0.0));
        self.reading
            .feed(serving.map(|s| s.prompt_tps).unwrap_or(0.0));
        let kv = serving
            .map(|s| s.kv_cache_usage)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        self.kv += (kv - self.kv) * SMOOTH;
        self.running = serving.map(|s| s.requests_running).unwrap_or(0);
        self.waiting = serving.map(|s| s.requests_waiting).unwrap_or(0);

        // Events are rises of the cumulative counters of the featured service.
        let key = row.map(|r| r.key.clone());
        if key != self.key {
            self.seen = Seen::default();
            self.key = key;
        }
        let Some(row) = row else {
            return;
        };
        let done = rise(
            &mut self.seen.done,
            crate::animation::activity::pulse_total(row),
        );
        let errored = rise(&mut self.seen.errored, errors_total(row));
        let preempted = rise(&mut self.seen.preempted, preemptions_total(row));
        for i in 0..done.min(3) as u32 {
            if self.flares.len() >= MAX_FLARES {
                self.flares.remove(0);
            }
            self.flares.push((self.frame, i));
        }
        let frame = self.frame;
        self.flares.retain(|(born, _)| frame - born < FLARE_FRAMES);
        if errored > 0 {
            self.static_until = self.frame + STATIC_FRAMES;
        }
        if preempted > 0 {
            self.splash_until = self.frame + SPLASH_FRAMES;
        }
    }

    #[cfg(test)]
    pub(crate) fn live_flares(&self) -> usize {
        self.flares.len()
    }

    /// True when a service is featured, so the backdrop should be drawn.
    pub fn active(&self) -> bool {
        self.scene != Scene::None
    }

    /// The backdrop for `w` by `h` cells. Pure: the same state gives the same
    /// canvas. Nothing is drawn below the minimum size or with no scene.
    pub fn render_backdrop(&self, w: usize, h: usize) -> Canvas {
        let mut canvas: Canvas = vec![vec![None; w]; h];
        if self.scene == Scene::None || w < MIN_W || h < MIN_H {
            return canvas;
        }
        self.paint_sky(&mut canvas, w, h);
        if matches!(self.scene, Scene::Aurora | Scene::Storm | Scene::Dream) {
            self.paint_tide(&mut canvas, w, h);
        }
        if self.scene == Scene::Storm {
            self.paint_storm(&mut canvas, w, h);
        }
        if matches!(self.scene, Scene::Aurora | Scene::Storm) {
            self.paint_rain(&mut canvas, w, h);
            self.paint_horizon(&mut canvas, w, h);
            self.paint_comets(&mut canvas, w, h);
        }
        if self.scene == Scene::Storm {
            self.paint_heartbeat(&mut canvas, w);
        }
        self.paint_flares(&mut canvas, w, h);
        self.paint_static(&mut canvas, w, h);
        canvas
    }

    /// Stars everywhere in the upper sky, and the aurora where the scene has
    /// one: bright and moving with tokens per second while generating, faint
    /// when idle, none elsewhere.
    fn paint_sky(&self, canvas: &mut Canvas, w: usize, h: usize) {
        let aurora_energy = match self.scene {
            Scene::Aurora => 0.25 + 0.75 * self.energy.value,
            Scene::Dream => 0.15,
            _ => 0.0,
        };
        let sky_h = ((h as f32) * 0.7).ceil() as usize;
        for y in 0..sky_h.min(h) {
            let rel_y = y as f32 / sky_h.max(1) as f32;
            for x in 0..w {
                if star_hash(x, y) > STAR_THRESHOLD {
                    if let Some(c) = sky_cell(x, y, rel_y, self.frame) {
                        canvas[y][x] = Some((c.ch, c.color));
                    }
                    continue;
                }
                if aurora_energy > 0.0 {
                    let (lit, hue) = aurora(x, rel_y, self.frame);
                    let l = lit * aurora_energy;
                    if l > AURORA_MIN {
                        let ch = if l > 0.62 { '▒' } else { '░' };
                        let hue = (hue + self.kv * 120.0) % 360.0;
                        canvas[y][x] = Some((ch, hsv_to_rgb(hue, 0.62, 0.10 + l * 0.2)));
                    }
                }
            }
        }
    }

    /// Falling dim glyphs in proportion to prompt tokens per second ("reading"),
    /// and, while tokens flow, rising gold braille in proportion to generation
    /// tokens per second ("writing"). A column is a drop when its hash is under
    /// the density, so the same inputs always draw the same columns.
    fn paint_rain(&self, canvas: &mut Canvas, w: usize, h: usize) {
        let reading = self.reading.value;
        let writing = if self.scene == Scene::Aurora {
            self.energy.value
        } else {
            0.0
        };
        for x in 0..w {
            if star_hash(x, 7) < 0.10 * reading {
                let speed = 0.25 + star_hash(x, 11) * 0.5;
                let cycle = (h + 4) as f32;
                let head = ((self.frame as f32 * speed + star_hash(x, 5) * cycle * 7.0) % cycle
                    - 2.0) as i32;
                for t in 0..4i32 {
                    let y = head - t;
                    if y >= 0 && (y as usize) < h {
                        let (ch, v) = if t == 0 {
                            (':', 0.5)
                        } else {
                            ('.', 0.3 - 0.05 * t as f32)
                        };
                        canvas[y as usize][x] = Some((ch, hsv_to_rgb(190.0, 0.45, v)));
                    }
                }
            }
            if star_hash(x, 13) < 0.12 * writing {
                let speed = 0.2 + star_hash(x, 17) * 0.4;
                let cycle = (h + 2) as f32;
                let up =
                    ((self.frame as f32 * speed + star_hash(x, 19) * cycle * 5.0) % cycle) as i32;
                let y = h as i32 - 1 - up;
                if y >= 0 && (y as usize) < h {
                    let glyph = ['⠁', '⠃', '⠇', '⡇'][((self.frame / 4) as usize + x) % 4];
                    canvas[y as usize][x] = Some((glyph, hsv_to_rgb(45.0, 0.8, 0.85)));
                }
            }
        }
    }

    /// Prefill: requests are in and the model is reading. A cloud band builds
    /// across the top, heavier while requests are running than while they only
    /// wait, with lightning every ninety frames while they run.
    fn paint_storm(&self, canvas: &mut Canvas, w: usize, h: usize) {
        let intensity = if self.running > 0 { 1.0 } else { 0.5 };
        let band = ((h as f32) * 0.35) as usize;
        let f = self.frame as f32;
        let threshold = 1.0 - 0.6 * intensity;
        for y in 0..band.min(h) {
            for x in 0..w {
                let (xf, yf) = (x as f32, y as f32);
                let v = 0.5
                    + 0.25 * (xf * 0.07 + f * 0.01).sin()
                    + 0.3 * (xf * 0.19 - f * 0.017 + yf).sin();
                if v > threshold {
                    let ch = if v > 0.9 {
                        '▓'
                    } else if v > 0.7 {
                        '▒'
                    } else {
                        '░'
                    };
                    let lum = 0.2 + 0.25 * (v - threshold).clamp(0.0, 1.0);
                    canvas[y][x] = Some((ch, hsv_to_rgb(220.0, 0.25, lum)));
                }
            }
        }
        if intensity >= 1.0 && self.frame >= 90 && self.frame % 90 < 3 {
            let x0 = (star_hash((self.frame / 90) as usize, 5) * w as f32) as usize;
            for dy in 0..5usize {
                let x = x0 + dy % 2;
                if dy < h && x < w {
                    let ch = if dy % 2 == 0 { '╲' } else { '╱' };
                    canvas[dy][x] = Some((ch, hsv_to_rgb(210.0, 0.2, 1.0)));
                }
            }
        }
    }

    /// Three dots at the top centre, one brighter in turn: the model thinking.
    fn paint_heartbeat(&self, canvas: &mut Canvas, w: usize) {
        if w < 7 || canvas.len() < 2 {
            return;
        }
        let cx = w / 2;
        let lit = ((self.frame / 20) % 3) as usize;
        for (i, x) in [cx - 3, cx, cx + 3].into_iter().enumerate() {
            let v = if i == lit { 1.0 } else { 0.4 };
            canvas[1][x] = Some(('◦', hsv_to_rgb(200.0, 0.3, v)));
        }
    }

    /// One dim star on the horizon per waiting request, at most twelve.
    fn paint_horizon(&self, canvas: &mut Canvas, w: usize, h: usize) {
        let n = self.waiting.min(12) as usize;
        let y = ((h as f32) * 0.8) as usize;
        for i in 0..n {
            let x = (i + 1) * w / (n + 1);
            if x < w && y < h {
                let v = 0.35 + 0.25 * (self.frame as f32 * 0.05 + i as f32).sin();
                canvas[y][x] = Some(('⋅', hsv_to_rgb(200.0, 0.2, v)));
            }
        }
    }

    /// One comet per running request, at most six, orbiting the centre with a
    /// short fading trail.
    fn paint_comets(&self, canvas: &mut Canvas, w: usize, h: usize) {
        let n = self.running.min(6) as usize;
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let (rx, ry) = (w as f32 * 0.40, h as f32 * 0.35);
        for i in 0..n {
            let ang = self.frame as f32 * 0.02 * (1.0 + 0.15 * i as f32)
                + i as f32 * std::f32::consts::TAU / n as f32;
            for (t, ch) in ['◉', '●', '•', '∘', '˙'].iter().enumerate() {
                let a = ang - t as f32 * 0.12;
                let x = (cx + rx * a.cos()).round() as i32;
                let y = (cy + ry * a.sin()).round() as i32;
                if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                    let v = 1.0 - 0.17 * t as f32;
                    canvas[y as usize][x as usize] = Some((*ch, hsv_to_rgb(175.0, 0.5, v)));
                }
            }
        }
    }

    /// An expanding ring for each recent completion.
    fn paint_flares(&self, canvas: &mut Canvas, w: usize, h: usize) {
        for (born, idx) in &self.flares {
            let age = self.frame - born;
            if age >= FLARE_FRAMES {
                continue;
            }
            let seed = (*born as usize).wrapping_mul(7).wrapping_add(*idx as usize);
            let x0 = (star_hash(seed, 41) * w as f32) as i32;
            let y0 = (star_hash(seed, 43) * h as f32 * 0.6) as i32;
            let r = (age / 3 + 1) as i32;
            let glyph = match age {
                0..=7 => '○',
                8..=15 => '◌',
                _ => '˙',
            };
            let v = 1.0 - age as f32 / FLARE_FRAMES as f32;
            let color = hsv_to_rgb(175.0, 0.4, 0.4 + 0.6 * v);
            for (dx, dy) in [(r * 2, 0), (-r * 2, 0), (0, r), (0, -r)] {
                let (x, y) = (x0 + dx, y0 + dy);
                if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                    canvas[y as usize][x as usize] = Some((glyph, color));
                }
            }
        }
    }

    /// A row of dim red static for a moment after a request errors.
    fn paint_static(&self, canvas: &mut Canvas, w: usize, h: usize) {
        if self.frame >= self.static_until {
            return;
        }
        let step = (self.frame / 2) as usize;
        let y = (star_hash(step, 3) * h as f32) as usize;
        if y >= h {
            return;
        }
        for x in 0..w {
            if star_hash(x, y + step) < 0.3 {
                canvas[y][x] = Some(('▚', hsv_to_rgb(0.0, 0.6, 0.35)));
            }
        }
    }

    /// A liquid level along the bottom: as high as the KV cache is full, blue,
    /// then amber above half, then red above 80%.
    fn paint_tide(&self, canvas: &mut Canvas, w: usize, h: usize) {
        let rows = ((self.kv * 3.0).round() as usize).min(3).min(h / 10);
        let hue = if self.kv < 0.5 {
            200.0
        } else if self.kv < 0.8 {
            40.0
        } else {
            5.0
        };
        for r in 0..rows {
            let y = h - 1 - r;
            for x in 0..w {
                let phase = (x + (self.frame / 6) as usize + r * 2) % 4;
                let glyph = if phase < 2 { '≈' } else { '~' };
                let v = 0.35 + 0.15 * (x as f32 * 0.4 + self.frame as f32 * 0.08).sin();
                canvas[y][x] = Some((glyph, hsv_to_rgb(hue, 0.7, v)));
                // A preemption splashes the top of the tide.
                if r + 1 == rows && self.frame < self.splash_until && x % 3 == 0 {
                    canvas[y][x] = Some(('^', hsv_to_rgb(hue, 0.3, 0.9)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::metrics::{MediaStats, ServingStats};
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    pub(super) fn svc(phase: Phase) -> ServiceState {
        ServiceState {
            key: "m".into(),
            label: "m".into(),
            phase,
            cpu_pct: 0.0,
            rss_bytes: 0,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            loaded_count: 0,
            loaded_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::Down,
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving: None,
            media: None,
            port: None,
            chips: vec![],
        }
    }

    pub(super) fn ready_serving(f: impl FnOnce(&mut ServingStats)) -> ServiceState {
        let mut s = svc(Phase::Ready);
        let mut sv = ServingStats::zeroed();
        f(&mut sv);
        s.serving = Some(sv);
        s
    }

    fn media(jobs: u32) -> MediaStats {
        MediaStats {
            generations_per_min: 0.0,
            jobs_in_progress: jobs,
            completed_delta: 0,
            errored_delta: 0,
            duration_avg_s: 0.0,
            post_avg_s: 0.0,
            pre_avg_s: 0.0,
            inference_avg_s: 0.0,
            warmup_avg_s: 0.0,
            counters: Default::default(),
        }
    }

    #[test]
    fn scene_follows_the_phase_and_the_stats() {
        assert_eq!(scene_of(None), Scene::None);
        assert_eq!(scene_of(Some(&svc(Phase::Down))), Scene::None);
        assert_eq!(scene_of(Some(&svc(Phase::Compiling))), Scene::Forge);
        assert_eq!(scene_of(Some(&svc(Phase::Loading))), Scene::Forge);
        assert_eq!(scene_of(Some(&svc(Phase::Alarm))), Scene::Stalled);
        assert_eq!(scene_of(Some(&svc(Phase::Ready))), Scene::Dream);
        assert_eq!(
            scene_of(Some(&ready_serving(|s| s.generation_tps = 40.0))),
            Scene::Aurora
        );
        // Prefill: requests are in but no token has come out.
        assert_eq!(
            scene_of(Some(&ready_serving(|s| s.requests_running = 2))),
            Scene::Storm
        );
        assert_eq!(
            scene_of(Some(&ready_serving(|s| s.requests_waiting = 3))),
            Scene::Storm
        );
        // Tokens flowing wins over a queue.
        assert_eq!(
            scene_of(Some(&ready_serving(|s| {
                s.generation_tps = 12.0;
                s.requests_waiting = 3;
            }))),
            Scene::Aurora
        );
        let mut diffusion = svc(Phase::Ready);
        diffusion.media = Some(media(1));
        assert_eq!(scene_of(Some(&diffusion)), Scene::Forming);
        diffusion.media = Some(media(0));
        assert_eq!(scene_of(Some(&diffusion)), Scene::Dream);
    }

    #[test]
    fn a_loading_service_is_featured_over_a_ready_one() {
        let rows = vec![svc(Phase::Ready), svc(Phase::Loading)];
        assert_eq!(featured(&rows).unwrap().phase, Phase::Loading);
        let rows = vec![svc(Phase::Down), svc(Phase::Ready)];
        assert_eq!(featured(&rows).unwrap().phase, Phase::Ready);
        assert!(featured(&[svc(Phase::Down)]).is_none());
        assert!(featured(&[]).is_none());
    }

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn row_of(w: usize, cell: Cell) -> Vec<Cell> {
        vec![cell; w]
    }

    #[test]
    fn composite_fills_blank_cells_and_never_replaces_foreground() {
        let red = Color::Rgb(255, 0, 0);
        let canvas: Canvas = vec![row_of(4, Some(('x', red)))];
        let out = composite(&canvas, vec![Line::from("ab c")], 4);
        assert_eq!(text(&out[0]), "abxc");
        let x = out[0]
            .spans
            .iter()
            .find(|s| s.content.contains('x'))
            .unwrap();
        assert_eq!(x.style.fg, Some(red));
    }

    #[test]
    fn a_wide_foreground_character_keeps_later_columns_aligned() {
        let canvas: Canvas = vec![vec![
            Some(('1', Color::Red)),
            Some(('2', Color::Red)),
            Some(('q', Color::Red)),
            Some(('4', Color::Red)),
        ]];
        // `日` is two columns wide: columns 0 and 1. The space is column 2.
        let out = composite(&canvas, vec![Line::from("日 a")], 4);
        assert_eq!(text(&out[0]), "日qa");
    }

    #[test]
    fn a_short_line_is_padded_with_backdrop_up_to_its_last_cell() {
        let canvas: Canvas = vec![vec![
            None,
            Some(('x', Color::Red)),
            Some(('y', Color::Red)),
            None,
            None,
        ]];
        let out = composite(&canvas, vec![Line::from("a")], 5);
        assert_eq!(text(&out[0]), "axy");
    }

    #[test]
    fn rows_without_backdrop_and_a_zero_width_are_returned_unchanged() {
        let canvas: Canvas = vec![row_of(4, None)];
        let line = Line::from(vec![Span::styled("ab c", Style::default().fg(Color::Blue))]);
        let out = composite(&canvas, vec![line.clone()], 4);
        assert_eq!(out[0], line);
        let busy: Canvas = vec![row_of(4, Some(('x', Color::Red)))];
        let out = composite(&busy, vec![line.clone()], 0);
        assert_eq!(out[0], line);
        // More lines than canvas rows: the extra lines pass through.
        let out = composite(&busy, vec![Line::from("   "), line.clone()], 4);
        assert_eq!(out[1], line);
    }

    #[test]
    fn a_blank_cell_with_its_own_background_belongs_to_the_foreground() {
        let canvas: Canvas = vec![row_of(3, Some(('x', Color::Red)))];
        let panel = Line::from(vec![Span::styled("   ", Style::default().bg(Color::Blue))]);
        let out = composite(&canvas, vec![panel.clone()], 3);
        assert_eq!(text(&out[0]), "   ");
    }

    // ---- Task 2: the sky and the tide -----------------------------------

    fn lit(c: &Canvas) -> usize {
        c.iter().flatten().filter(|x| x.is_some()).count()
    }

    fn observed(rows: &[ServiceState], times: usize) -> Atmosphere {
        let mut a = Atmosphere::new();
        for _ in 0..times {
            a.observe(rows);
        }
        a
    }

    #[test]
    fn the_backdrop_is_deterministic() {
        let rows = vec![ready_serving(|s| {
            s.generation_tps = 30.0;
            s.kv_cache_usage = 0.4;
        })];
        let a = observed(&rows, 20);
        let b = observed(&rows, 20);
        assert_eq!(a.render_backdrop(80, 30), b.render_backdrop(80, 30));
        // And the same atmosphere asked twice.
        assert_eq!(a.render_backdrop(80, 30), a.render_backdrop(80, 30));
    }

    #[test]
    fn nothing_featured_means_no_backdrop_and_not_active() {
        let a = observed(&[], 10);
        assert!(!a.active());
        assert_eq!(lit(&a.render_backdrop(80, 30)), 0);
        let down = observed(&[svc(Phase::Down)], 10);
        assert!(!down.active());
    }

    #[test]
    fn an_idle_sky_is_quiet_and_a_generating_sky_is_brighter() {
        let idle = observed(&[svc(Phase::Ready)], 60);
        assert!(idle.active());
        let busy = observed(&[ready_serving(|s| s.generation_tps = 100.0)], 120);
        let (i, b) = (
            lit(&idle.render_backdrop(100, 30)),
            lit(&busy.render_backdrop(100, 30)),
        );
        assert!(i > 0, "stars at least");
        assert!(b > i, "aurora lights more cells: idle {i}, generating {b}");
    }

    #[test]
    fn the_tide_height_follows_the_kv_cache() {
        let bottom_lit = |kv: f32| {
            let a = observed(
                &[ready_serving(|s| {
                    s.generation_tps = 20.0;
                    s.kv_cache_usage = kv;
                })],
                200,
            );
            let c = a.render_backdrop(60, 30);
            (0..3)
                .filter(|r| c[29 - r].iter().any(|x| x.is_some()))
                .count()
        };
        assert_eq!(bottom_lit(0.0), 0);
        assert_eq!(bottom_lit(0.34), 1);
        assert_eq!(bottom_lit(0.95), 3);
    }

    #[test]
    fn the_tide_warms_from_blue_to_amber_to_red() {
        let colour_at = |kv: f32| {
            let a = observed(
                &[ready_serving(|s| {
                    s.generation_tps = 20.0;
                    s.kv_cache_usage = kv;
                })],
                200,
            );
            a.render_backdrop(60, 20)[19]
                .iter()
                .flatten()
                .next()
                .unwrap()
                .1
        };
        let (low, mid, high) = (colour_at(0.4), colour_at(0.7), colour_at(0.95));
        assert!(
            low != mid && mid != high && low != high,
            "{low:?} {mid:?} {high:?}"
        );
    }

    #[test]
    fn any_size_is_safe_and_returns_the_asked_dimensions() {
        let a = observed(&[ready_serving(|s| s.generation_tps = 50.0)], 30);
        for (w, h) in [(0, 0), (1, 1), (3, 2), (4, 3), (5, 3), (40, 12), (160, 50)] {
            let c = a.render_backdrop(w, h);
            assert_eq!(c.len(), h, "{w}x{h}");
            assert!(c.iter().all(|r| r.len() == w), "{w}x{h}");
        }
        // Below the minimum nothing is drawn.
        assert_eq!(lit(&a.render_backdrop(3, 2)), 0);
    }

    // ---- Task 3: rain, comets, horizon stars, flares, static -------------

    /// Glyphs unique to each layer, so a test can count what a layer drew.
    const FALL: [char; 2] = [':', '.'];
    const RISE: [char; 4] = ['⠁', '⠃', '⠇', '⡇'];
    const COMET_HEAD: char = '◉';
    const HORIZON: char = '⋅';
    const RING: char = '○';
    const STATIC: char = '▚';

    fn count(c: &Canvas, set: &[char]) -> usize {
        c.iter()
            .flatten()
            .filter(|x| x.is_some_and(|(ch, _)| set.contains(&ch)))
            .count()
    }

    fn serving_row(f: impl FnOnce(&mut ServingStats)) -> ServiceState {
        ready_serving(f)
    }

    #[test]
    fn no_rain_without_rates() {
        let a = observed(&[serving_row(|s| s.requests_running = 2)], 100);
        let c = a.render_backdrop(100, 30);
        assert_eq!(count(&c, &FALL), 0);
        assert_eq!(count(&c, &RISE), 0);
    }

    /// A gauge is scaled against the peak it has seen, so feed a high rate
    /// first and then compare a held-high rate with one that dropped to a tenth.
    fn held_vs_dropped(f: impl Fn(&mut ServingStats, f32)) -> (Canvas, Canvas) {
        let hi = |rate: f32| {
            serving_row(|s| {
                s.generation_tps = 50.0;
                f(s, rate)
            })
        };
        let mut held = Atmosphere::new();
        for _ in 0..200 {
            held.observe(&[hi(500.0)]);
        }
        let mut dropped = Atmosphere::new();
        dropped.observe(&[hi(500.0)]);
        for _ in 0..200 {
            dropped.observe(&[hi(50.0)]);
        }
        (
            held.render_backdrop(120, 30),
            dropped.render_backdrop(120, 30),
        )
    }

    #[test]
    fn falling_glyphs_follow_the_prompt_rate() {
        let (held, dropped) = held_vs_dropped(|s, r| s.prompt_tps = r);
        assert!(
            count(&held, &FALL) > count(&dropped, &FALL),
            "reading density"
        );
        assert!(count(&held, &FALL) > 0);
    }

    #[test]
    fn rising_glyphs_follow_the_generation_rate() {
        let (held, dropped) = held_vs_dropped(|s, r| s.generation_tps = r);
        assert!(
            count(&held, &RISE) > count(&dropped, &RISE),
            "writing density"
        );
        assert!(count(&held, &RISE) > 0);
    }

    #[test]
    fn comets_are_the_running_requests_up_to_six() {
        let heads = |running: u32| {
            let a = observed(
                &[serving_row(|s| {
                    s.generation_tps = 20.0;
                    s.requests_running = running;
                })],
                30,
            );
            count(&a.render_backdrop(100, 30), &[COMET_HEAD])
        };
        assert_eq!(heads(0), 0);
        assert_eq!(heads(3), 3);
        assert_eq!(heads(40), 6);
    }

    #[test]
    fn horizon_stars_are_the_waiting_requests_up_to_twelve() {
        let stars = |waiting: u32| {
            let a = observed(
                &[serving_row(|s| {
                    s.generation_tps = 20.0;
                    s.requests_waiting = waiting;
                })],
                30,
            );
            count(&a.render_backdrop(100, 30), &[HORIZON])
        };
        assert_eq!(stars(0), 0);
        assert_eq!(stars(4), 4);
        assert_eq!(stars(50), 12);
    }

    fn with_done(total: u64) -> ServiceState {
        serving_row(|s| {
            s.generation_tps = 20.0;
            s.counters.requests_succeeded_total = total;
        })
    }

    #[test]
    fn each_finished_request_flares_once_and_the_flare_expires() {
        let mut a = Atmosphere::new();
        a.observe(&[with_done(10)]); // first sight: a baseline, not a burst
        assert_eq!(a.live_flares(), 0);
        a.observe(&[with_done(12)]);
        assert_eq!(a.live_flares(), 2);
        assert!(count(&a.render_backdrop(100, 30), &[RING]) > 0);
        // The same snapshot read on later frames adds nothing.
        for _ in 0..5 {
            a.observe(&[with_done(12)]);
        }
        assert_eq!(a.live_flares(), 2);
        for _ in 0..40 {
            a.observe(&[with_done(12)]);
        }
        assert_eq!(a.live_flares(), 0, "flares expire");
        assert_eq!(count(&a.render_backdrop(100, 30), &[RING]), 0);
    }

    #[test]
    fn flares_are_capped_per_tick_and_overall() {
        let mut a = Atmosphere::new();
        a.observe(&[with_done(0)]);
        a.observe(&[with_done(1000)]);
        assert_eq!(a.live_flares(), 3, "at most three per observe");
        for i in 2..20u64 {
            a.observe(&[with_done(i * 1000)]);
        }
        assert!(a.live_flares() <= 8, "{}", a.live_flares());
        // A counter that drops (a restarted server) is a reset, not a burst.
        let mut b = Atmosphere::new();
        b.observe(&[with_done(500)]);
        b.observe(&[with_done(3)]);
        assert_eq!(b.live_flares(), 0);
    }

    #[test]
    fn an_error_draws_static_that_clears() {
        let with_errors = |n: u64| {
            serving_row(|s| {
                s.generation_tps = 20.0;
                s.counters.requests_errored_total = n;
            })
        };
        let mut a = Atmosphere::new();
        a.observe(&[with_errors(0)]);
        assert_eq!(count(&a.render_backdrop(100, 30), &[STATIC]), 0);
        a.observe(&[with_errors(1)]);
        assert!(count(&a.render_backdrop(100, 30), &[STATIC]) > 0);
        for _ in 0..60 {
            a.observe(&[with_errors(1)]);
        }
        assert_eq!(count(&a.render_backdrop(100, 30), &[STATIC]), 0);
    }

    #[test]
    fn at_full_signal_the_backdrop_stays_readable() {
        let mut a = Atmosphere::new();
        let row = |done: u64| {
            serving_row(|s| {
                s.generation_tps = 500.0;
                s.prompt_tps = 5000.0;
                s.requests_running = 8;
                s.requests_waiting = 30;
                s.kv_cache_usage = 0.95;
                s.counters.requests_succeeded_total = done;
            })
        };
        for i in 0..300u64 {
            a.observe(&[row(i * 5)]);
        }
        for (w, h) in [(80, 24), (100, 30), (160, 50)] {
            let c = a.render_backdrop(w, h);
            let frac = lit(&c) as f32 / (w * h) as f32;
            assert!(frac <= 0.45, "{w}x{h}: {:.0}% lit", frac * 100.0);
        }
    }

    #[test]
    fn a_preemption_splashes_the_tide() {
        let row = |n: u64| {
            serving_row(|s| {
                s.generation_tps = 20.0;
                s.kv_cache_usage = 0.6;
                s.counters.preemptions_total = n;
            })
        };
        let mut a = Atmosphere::new();
        for _ in 0..80 {
            a.observe(&[row(0)]);
        }
        assert_eq!(count(&a.render_backdrop(60, 20), &['^']), 0);
        a.observe(&[row(1)]);
        assert!(count(&a.render_backdrop(60, 20), &['^']) > 0);
        for _ in 0..40 {
            a.observe(&[row(1)]);
        }
        assert_eq!(count(&a.render_backdrop(60, 20), &['^']), 0);
    }

    // ---- Task 4: the storm ----------------------------------------------

    const CLOUD: [char; 3] = ['░', '▒', '▓'];
    const BOLT: [char; 2] = ['╲', '╱'];
    const BEAT: char = '◦';

    fn storm(running: u32, waiting: u32, frames: usize) -> Atmosphere {
        observed(
            &[serving_row(|s| {
                s.requests_running = running;
                s.requests_waiting = waiting;
            })],
            frames,
        )
    }

    #[test]
    fn a_storm_is_a_cloud_band_across_the_top_and_nothing_low() {
        let a = storm(2, 0, 30);
        assert_eq!(a.scene, Scene::Storm);
        let c = a.render_backdrop(100, 30);
        let top: usize = c[..11]
            .iter()
            .map(|r| count(&vec![r.clone()], &CLOUD))
            .sum();
        let low: usize = c[20..]
            .iter()
            .map(|r| count(&vec![r.clone()], &CLOUD))
            .sum();
        assert!(top > 100, "a band of cloud: {top}");
        assert_eq!(low, 0, "no cloud in the lower third");
    }

    #[test]
    fn running_requests_make_a_heavier_storm_than_waiting_ones() {
        let heavy = count(&storm(2, 0, 30).render_backdrop(100, 30), &CLOUD);
        let light = count(&storm(0, 3, 30).render_backdrop(100, 30), &CLOUD);
        assert!(heavy > light && light > 0, "heavy {heavy}, light {light}");
    }

    #[test]
    fn lightning_strikes_on_a_schedule_while_requests_run() {
        let bolts = |frames: usize| count(&storm(2, 0, frames).render_backdrop(100, 30), &BOLT);
        assert_eq!(bolts(40), 0);
        assert!(bolts(90) > 0, "a strike at frame 90");
        assert_eq!(bolts(95), 0, "it is over after three frames");
        // A waiting-only storm has no lightning.
        assert_eq!(count(&storm(0, 3, 90).render_backdrop(100, 30), &BOLT), 0);
    }

    #[test]
    fn the_storm_has_a_heartbeat_and_only_the_storm_does() {
        assert_eq!(count(&storm(2, 0, 30).render_backdrop(100, 30), &[BEAT]), 3);
        let aurora = observed(&[serving_row(|s| s.generation_tps = 50.0)], 30);
        let c = aurora.render_backdrop(100, 30);
        assert_eq!(count(&c, &[BEAT]), 0);
        assert_eq!(count(&c, &BOLT), 0);
    }
}
