// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tenstorrent model-compatibility catalog: a bundled snapshot (offline floor)
//! plus a background curl-refreshed live copy. Drives the cold-trail starfield
//! screensaver in the Inference Server Monitor.

use arc_swap::ArcSwap;
use serde::Deserialize;
use std::process::Command;
use std::sync::Arc;

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

const CATALOG_URL: &str = "https://d1oi7xemha0dsy.cloudfront.net/data/compatibility.json";
/// Re-fetch at most this often (catalog changes slowly).
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

pub fn catalog_cache_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tt-toplike")
        .join("compatibility.json")
}

/// Background-refreshed model catalog. Seeds from the bundled snapshot (and the
/// on-disk cache if newer/valid), then curls the live URL on a slow cadence.
pub struct CatalogRefresher {
    cache: Arc<ArcSwap<Vec<CatalogModel>>>,
}

impl CatalogRefresher {
    pub fn spawn() -> Self {
        // Seed synchronously: prefer a valid on-disk cache, else the bundled floor.
        let seed = std::fs::read_to_string(catalog_cache_path())
            .ok()
            .map(|s| parse_catalog(&s))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| parse_catalog(bundled()));
        let cache = Arc::new(ArcSwap::from_pointee(seed));
        let cache_bg = Arc::clone(&cache);
        let _ = std::thread::Builder::new()
            .name("tt-catalog-refresh".into())
            .spawn(move || loop {
                if let Some(json) = curl_fetch(CATALOG_URL) {
                    let models = parse_catalog(&json);
                    if !models.is_empty() {
                        // Best-effort cache write (ignore errors).
                        let path = catalog_cache_path();
                        if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        let _ = std::fs::write(&path, &json);
                        cache_bg.store(Arc::new(models));
                    }
                }
                std::thread::sleep(REFRESH_INTERVAL);
            });
        CatalogRefresher { cache }
    }

    pub fn snapshot(&self) -> Arc<Vec<CatalogModel>> {
        self.cache.load_full()
    }
}

/// Fetch a URL via `curl` (dep-free HTTPS). `None` on missing curl / error / non-2xx.
fn curl_fetch(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args(["-fsS", "--max-time", "20", url])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
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

    #[test]
    fn refresher_seeds_from_bundled_immediately() {
        let r = CatalogRefresher::spawn();
        // The bundled catalog is available synchronously at construction —
        // no waiting on the background curl.
        assert!(!r.snapshot().is_empty());
    }

    #[test]
    fn cache_path_is_under_tt_toplike() {
        assert!(catalog_cache_path().ends_with("tt-toplike/compatibility.json"));
    }
}
