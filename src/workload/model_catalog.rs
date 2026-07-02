// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tenstorrent model-compatibility catalog: a bundled snapshot (offline floor)
//! plus a background curl-refreshed live copy. Drives the cold-trail starfield
//! screensaver in the Inference Server Monitor.

use serde::Deserialize;

/// One hardware-compatibility entry for a model.
#[derive(Debug, Clone, Deserialize)]
pub struct Compat {
    #[serde(default)]
    pub chip_set: String, // "Blackhole" | "Wormhole" | ...
    #[serde(default)]
    pub hardware: String, // "p150" | "n300" | "Quietbox" | ...
    #[serde(default)]
    pub status: String, // "Supported" | "Experimental" | "Not Supported"
}

/// One catalog model (only the fields the screensaver renders).
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogModel {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub model_size: String,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub compatibility: Vec<Compat>,
}

#[derive(Deserialize)]
struct Catalog {
    #[serde(default)]
    models: Vec<CatalogModel>,
}

/// Parse `compatibility.json`. Tolerant: any parse failure → empty vec (never panics).
pub fn parse_catalog(json: &str) -> Vec<CatalogModel> {
    serde_json::from_str::<Catalog>(json)
        .map(|c| c.models)
        .unwrap_or_default()
}

/// The compiled-in snapshot — the always-available offline floor.
pub fn bundled() -> &'static str {
    include_str!("../../assets/compatibility.snapshot.json")
}

/// Support level of a model on a given chip_set (best across its entries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Supported,
    Experimental,
    No,
}

pub fn model_support(m: &CatalogModel, chip_set: &str) -> Support {
    let mut best = Support::No;
    for c in &m.compatibility {
        if c.chip_set.eq_ignore_ascii_case(chip_set) {
            match c.status.as_str() {
                "Supported" => return Support::Supported,
                "Experimental" => best = Support::Experimental,
                _ => {}
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_bundled_catalog_and_matches_hardware() {
        let models = parse_catalog(bundled());
        assert!(models.len() > 20, "bundled catalog should have many models");
        // A known model with a Blackhole compatibility entry.
        let flux = models
            .iter()
            .find(|m| m.display_name.contains("FLUX.1 [schnell]"))
            .expect("FLUX.1 [schnell] present in snapshot");
        assert!(matches!(
            model_support(flux, "Blackhole"),
            Support::Supported | Support::Experimental
        ));
        // Malformed JSON → empty, never panics.
        assert!(parse_catalog("{ not json").is_empty());
        assert!(parse_catalog("{\"models\":[{}]}").len() == 1); // tolerant of missing fields
    }
}
