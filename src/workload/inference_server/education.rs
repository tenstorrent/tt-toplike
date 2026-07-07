// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Education copy for the `[i]` loading view: short, phase-synced explanations
//! of what a model load is doing, plus rotating TT facts and per-model taglines.
//! The data is owned in-tree (copied from a tt-studio proof-of-concept, NOT
//! vendored/synced) as plain JSON so it can later graduate to a dedicated facts
//! repo. Parsed once behind `OnceLock`; all lookups are pure.

use crate::workload::inference_server::Phase;
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
}
