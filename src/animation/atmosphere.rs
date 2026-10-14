//! The Inference view's atmosphere: a backdrop drawn *behind* whatever the view
//! already shows, so loading, thinking and generating have a feel.
//!
//! The backdrop is a canvas of optional cells. [`composite`] lays it under the
//! existing renderer's lines and fills blank cells only, so the snake, the
//! dashboards and the roster always win. Every effect maps to a real signal in a
//! [`ServiceState`]; see the spec for the table. Everything here is deterministic:
//! no randomness, positions come from a spatial hash, motion from a frame counter.

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
}
