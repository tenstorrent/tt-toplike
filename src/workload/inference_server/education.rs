// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Education copy for the `[i]` loading view: short, phase-synced explanations
//! of what a model load is doing, plus rotating TT facts and per-model taglines.
//! The data is owned in-tree (copied from a tt-studio proof-of-concept, NOT
//! vendored/synced) as plain JSON so it can later graduate to a dedicated facts
//! repo. Parsed once behind `OnceLock`; all lookups are pure.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
