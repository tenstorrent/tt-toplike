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
}

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
        let kv = serving
            .map(|s| s.kv_cache_usage)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        self.kv += (kv - self.kv) * SMOOTH;
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
                    if l > 0.14 {
                        let ch = if l > 0.62 { '▒' } else { '░' };
                        let hue = (hue + self.kv * 120.0) % 360.0;
                        canvas[y][x] = Some((ch, hsv_to_rgb(hue, 0.62, 0.10 + l * 0.2)));
                    }
                }
            }
        }
    }

    /// A liquid level along the bottom: as high as the KV cache is full, blue,
    /// then amber above half, then red above 80%.
    fn paint_tide(&self, canvas: &mut Canvas, w: usize, h: usize) {
        let rows = ((self.kv * 3.0).round() as usize).min(3).min(h / 4);
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
            let c = a.render_backdrop(60, 20);
            (0..3)
                .filter(|r| c[19 - r].iter().any(|x| x.is_some()))
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
}
