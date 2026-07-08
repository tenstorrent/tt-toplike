// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Education copy for the `[i]` loading view: short, phase-synced explanations
//! of what a model load is doing, plus rotating TT facts and per-model taglines.
//! The data is owned in-tree (copied from a tt-studio proof-of-concept, NOT
//! vendored/synced) as plain JSON so it can later graduate to a dedicated facts
//! repo. Parsed once behind `OnceLock`; all lookups are pure.

use crate::ui::colors;
use crate::workload::inference_server::Phase;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
pub struct Stage {
    pub title: String,
    pub explanation: String,
    pub duration_hint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fact {
    pub headline: String,
    pub body: String,
    pub tag: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeyStats {
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub params: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelOverview {
    pub tagline: String,
    #[serde(default)]
    pub key_stats: KeyStats,
}

#[derive(Deserialize)]
struct FactsFile {
    cards: Vec<Fact>,
}

const STAGES_JSON: &str = include_str!("../../../assets/education/deployment-stages.json");
const FACTS_JSON: &str = include_str!("../../../assets/education/did-you-know.json");
const MODELS_JSON: &str = include_str!("../../../assets/education/model-education.json");

pub fn stages() -> &'static HashMap<String, Stage> {
    static S: OnceLock<HashMap<String, Stage>> = OnceLock::new();
    // A parse failure means a bad in-tree copy — degrade to empty (band suppresses)
    // rather than panicking the whole TUI.
    S.get_or_init(|| serde_json::from_str(STAGES_JSON).unwrap_or_default())
}

pub fn facts() -> &'static [Fact] {
    static F: OnceLock<Vec<Fact>> = OnceLock::new();
    F.get_or_init(|| {
        serde_json::from_str::<FactsFile>(FACTS_JSON)
            .map(|f| f.cards)
            .unwrap_or_default()
    })
}

pub fn models() -> &'static HashMap<String, ModelOverview> {
    static M: OnceLock<HashMap<String, ModelOverview>> = OnceLock::new();
    M.get_or_init(|| serde_json::from_str(MODELS_JSON).unwrap_or_default())
}

/// A stage's display copy, resolved for a phase. Owned (cheap String clones at
/// tick cadence) so `Alarm`'s synthetic entry and the embedded stages share one
/// return type.
#[derive(Debug, Clone)]
pub struct StageInfo {
    pub title: String,
    pub explanation: String,
    pub duration_hint: String,
}

impl From<&Stage> for StageInfo {
    fn from(s: &Stage) -> Self {
        StageInfo {
            title: s.title.clone(),
            explanation: s.explanation.clone(),
            duration_hint: s.duration_hint.clone(),
        }
    }
}

/// The `deployment-stages.json` key each phase maps to. `Down` → `None`
/// (suppress). `Alarm` has no tt-studio stage, so the caller substitutes a
/// synthetic entry (see `stage_for_phase`).
fn stage_key_for_phase(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Down => None,
        Phase::Compiling => Some("container_setup"),
        Phase::Loading => Some("model_preparation"),
        Phase::Ready => Some("complete"),
        Phase::Alarm => None, // handled specially below
    }
}

/// Resolve the display copy for a phase. `None` = suppress the band (`Down`, or a
/// mapped key missing from the data). `Alarm` yields a synthetic stalled entry.
pub fn stage_for_phase(phase: Phase) -> Option<StageInfo> {
    if phase == Phase::Alarm {
        return Some(StageInfo {
            title: "Stalled".to_string(),
            explanation: "No forward progress detected. Check the server logs — \
                          the container may be waiting on a download, a wedged \
                          compile, or an out-of-memory condition."
                .to_string(),
            duration_hint: String::new(),
        });
    }
    let key = stage_key_for_phase(phase)?;
    stages().get(key).map(StageInfo::from)
}

/// Per-model tagline + key stats, by the label tt-toplike shows (e.g.
/// "Llama-3.1-8B-Instruct"). `None` → caller falls back to the raw label.
pub fn model_overview(label: &str) -> Option<&'static ModelOverview> {
    models().get(label)
}

/// Truncate `s` to at most `max` columns (approximated by chars — the copy is
/// ASCII-dominant), appending `…` when cut. Char-boundary-safe: never slices a
/// multibyte codepoint.
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Greedy word-wrap to `width` columns, char-safe, at most `max_lines` lines
/// (the last line truncates with `…` if content remains).
fn wrap_words(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let add = if cur.is_empty() {
            word.chars().count()
        } else {
            cur.chars().count() + 1 + word.chars().count()
        };
        if add > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                break;
            }
        }
        if cur.is_empty() {
            cur = truncate_chars(word, width);
        } else {
            cur.push(' ');
            cur.push_str(word);
        }
    }
    if lines.len() < max_lines && !cur.is_empty() {
        lines.push(truncate_chars(&cur, width));
    }
    lines
}

/// Accent color for the phase marker, matching the synthwave loading palette.
fn phase_accent(phase: Phase) -> Color {
    match phase {
        Phase::Compiling => Color::Rgb(64, 196, 224), // cyan-teal
        Phase::Loading => Color::Rgb(158, 104, 232),  // violet
        Phase::Alarm => Color::Rgb(255, 48, 96),      // hot pink
        _ => Color::Rgb(158, 104, 232),
    }
}

/// Compose the education footer band for the featured loading service. Empty
/// when the phase suppresses the band (`Down`) or the pane is too narrow.
/// As `height_budget` shrinks, lines are dropped by keep-priority (lowest kept
/// longest): phase title > explanation l1 > model header > explanation l2 > fact.
/// The kept lines are then emitted in visual order: header, phase, explanation, fact.
pub fn compose_band(
    model_label: &str,
    phase: Phase,
    width: usize,
    height_budget: usize,
    rotation_idx: usize,
) -> Vec<Line<'static>> {
    if width < 12 || height_budget == 0 {
        return Vec::new();
    }
    let Some(stage) = stage_for_phase(phase) else {
        return Vec::new();
    };
    let dim = Style::default()
        .fg(colors::text_secondary())
        .add_modifier(Modifier::DIM);

    // Phase title (+ trailing duration hint).
    let hint = if stage.duration_hint.is_empty() {
        String::new()
    } else {
        format!("   {}", stage.duration_hint)
    };
    let title = truncate_chars(&stage.title, width.saturating_sub(2 + hint.chars().count()));
    let phase_line = Line::from(vec![
        Span::styled("▸ ".to_string(), Style::default().fg(phase_accent(phase))),
        Span::styled(title, Style::default().fg(colors::text_primary())),
        Span::styled(hint, dim),
    ]);

    // Explanation, wrapped to up to 2 lines.
    let mut expl = wrap_words(&stage.explanation, width, 2)
        .into_iter()
        .map(|l| Line::from(Span::styled(format!("  {l}"), dim)));
    let expl1 = expl.next();
    let expl2 = expl.next();

    // Model header (tagline + stats, or the raw label on a miss).
    let header_text = match model_overview(model_label) {
        Some(ov) => {
            let mut parts: Vec<String> = vec![ov.tagline.clone()];
            if let Some(c) = ov.key_stats.context.as_ref() {
                parts.push(format!("{c} ctx"));
            }
            if let Some(l) = ov.key_stats.license.as_ref() {
                parts.push(l.clone());
            }
            parts.join(" · ")
        }
        None => model_label.to_string(),
    };
    let header = Line::from(Span::styled(truncate_chars(&header_text, width), dim));

    // Rotating fact.
    let fact_line = facts().get(rotation_idx % facts().len().max(1)).map(|f| {
        Line::from(Span::styled(
            truncate_chars(&format!("TT · {}", f.headline), width),
            dim,
        ))
    });

    // Candidates in VISUAL order (header, phase, expl1, expl2, fact), each tagged
    // with a keep-priority (lower = kept longer as height shrinks).
    let mut cand: Vec<(u8, Line<'static>)> = vec![(2, header), (0, phase_line)];
    if let Some(e1) = expl1 {
        cand.push((1, e1));
    }
    if let Some(e2) = expl2 {
        cand.push((3, e2));
    }
    if let Some(f) = fact_line {
        cand.push((4, f));
    }

    // Keep the `height_budget` lowest-priority lines, then restore visual order
    // (ascending original index == the insertion/visual order above).
    let mut ranked: Vec<(u8, usize)> = cand.iter().enumerate().map(|(i, (p, _))| (*p, i)).collect();
    ranked.sort();
    ranked.truncate(height_budget);
    let mut keep: Vec<usize> = ranked.into_iter().map(|(_, i)| i).collect();
    keep.sort_unstable();
    keep.into_iter().map(|i| cand[i].1.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::Phase;

    #[test]
    fn all_assets_parse() {
        // Guards against a bad copy-in: every embedded asset must deserialize.
        assert!(stages().contains_key("model_preparation"));
        assert!(stages().contains_key("container_setup"));
        assert!(stages().contains_key("complete"));
        assert!(!facts().is_empty());
        assert!(!models().is_empty());
        // The subset kept tagline + key_stats — assert against a known model
        // rather than a vacuous `|| true`.
        let llama = models()
            .get("Llama-3.1-8B-Instruct")
            .expect("known model present");
        assert!(!llama.tagline.is_empty(), "subset kept the tagline");
    }

    #[test]
    fn stage_for_phase_is_total_and_maps_correctly() {
        // Down suppresses the band.
        assert!(stage_for_phase(Phase::Down).is_none());
        // Loading phases map to their tt-studio stage titles.
        assert_eq!(
            stage_for_phase(Phase::Compiling).unwrap().title,
            "Compiling execution graphs"
        );
        assert_eq!(
            stage_for_phase(Phase::Loading).unwrap().title,
            "Loading model weights"
        );
        assert_eq!(stage_for_phase(Phase::Ready).unwrap().title, "Ready");
        // Alarm has no tt-studio equivalent → synthetic stalled line.
        let alarm = stage_for_phase(Phase::Alarm).unwrap();
        assert!(alarm.title.to_lowercase().contains("stall"));
        assert!(!alarm.explanation.is_empty());
    }

    #[test]
    fn model_overview_hit_and_miss() {
        // A model present in the education set resolves.
        assert!(model_overview("Llama-3.1-8B-Instruct").is_some());
        // An unknown model misses (caller falls back to the raw name).
        assert!(model_overview("Definitely-Not-A-Real-Model").is_none());
    }

    use ratatui::text::Line;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn compose_band_suppresses_when_down_or_too_narrow() {
        assert!(compose_band("Llama-3.1-8B-Instruct", Phase::Down, 80, 6, 0).is_empty());
        assert!(compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 8, 6, 0).is_empty());
    }

    #[test]
    fn compose_band_full_then_degrades_by_height() {
        let full = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 6, 0);
        assert!(
            full.len() >= 3,
            "roomy band shows header+phase+explanation(+fact)"
        );
        let text = plain(&full).join("\n");
        assert!(text.contains("Loading model weights"));

        // Height 1 keeps the single most important line (the phase title).
        let tiny = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 1, 0);
        assert_eq!(tiny.len(), 1);
        assert!(plain(&tiny)[0].contains("Loading model weights"));

        // Height 0 → nothing.
        assert!(compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 0, 0).is_empty());
    }

    #[test]
    fn compose_band_rotation_is_deterministic_and_wraps() {
        if facts().len() >= 2 {
            let a = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 6, 0);
            let b = compose_band("Llama-3.1-8B-Instruct", Phase::Loading, 80, 6, 1);
            assert_ne!(
                plain(&a),
                plain(&b),
                "different rotation_idx → different fact"
            );
            let wrap = compose_band(
                "Llama-3.1-8B-Instruct",
                Phase::Loading,
                80,
                6,
                facts().len(),
            );
            assert_eq!(
                plain(&a),
                plain(&wrap),
                "rotation_idx wraps modulo facts().len()"
            );
        }
    }

    #[test]
    fn compose_band_is_char_safe_on_multibyte_and_unknown_model() {
        // A multibyte label + narrow width must never panic (no byte slicing).
        let out = compose_band(
            "模型-测试-🚀-very-long-name-that-must-truncate",
            Phase::Loading,
            20,
            6,
            0,
        );
        assert!(!out.is_empty());
        // Unknown model → header falls back to the raw label (no crash, no lookup).
        let unknown = compose_band("Definitely-Not-A-Real-Model", Phase::Loading, 80, 6, 0);
        let text = plain(&unknown).join("\n");
        assert!(text.contains("Definitely-Not-A-Real-Model"));
    }
}
