// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Animation configuration: profiles and user-tunable thresholds.

use serde::Deserialize;
use std::path::PathBuf;

/// Named animation sensitivity presets.
///
/// Controls how responsive all visualizations are to hardware activity changes.
/// Applies universally to Memory Castle, Starfield, Memory Flow, and chip portrait particles.
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum AnimationProfile {
    /// Only extreme spikes trigger visible activity (5× power increase needed)
    AnomaliesOnly,
    /// Gentle response; light workloads appear nearly still
    Relaxed,
    /// Balanced default — mirrors observed idle vs. load contrast
    Normal,
    /// Heightened — moderate workloads produce vigorous animation
    Hyper,
    /// Maximum sensitivity; every fluctuation is visible
    Paranoid,
}

impl Default for AnimationProfile {
    fn default() -> Self {
        Self::Normal
    }
}

/// Resolved animation configuration applied to all viz objects.
#[derive(Debug, Clone)]
pub struct AnimConfig {
    /// Multiplier applied to every `AdaptiveBaseline::power_change()` result.
    /// 1.0 = Normal; 0.2 = AnomaliesOnly; 5.0 = Paranoid.
    pub sensitivity: f32,

    /// Scale factor for particle caps (multiplied into base counts).
    /// 1.0 = Normal; 3.0 = Paranoid.
    pub max_particles_scale: f32,
}

impl AnimConfig {
    /// Build a config from a named preset.
    pub fn from_profile(profile: AnimationProfile) -> Self {
        match profile {
            AnimationProfile::AnomaliesOnly => Self {
                sensitivity: 0.2,
                max_particles_scale: 0.5,
            },
            AnimationProfile::Relaxed => Self {
                sensitivity: 0.5,
                max_particles_scale: 0.75,
            },
            AnimationProfile::Normal => Self {
                sensitivity: 1.0,
                max_particles_scale: 1.0,
            },
            AnimationProfile::Hyper => Self {
                sensitivity: 2.0,
                max_particles_scale: 2.0,
            },
            AnimationProfile::Paranoid => Self {
                sensitivity: 5.0,
                max_particles_scale: 3.0,
            },
        }
    }

    /// Memory Castle max_particles for this config (base: 600).
    pub fn castle_max_particles(&self) -> usize {
        ((600.0 * self.max_particles_scale) as usize).max(20)
    }

    /// Memory Flow max_particles for this config (base: 200).
    pub fn flow_max_particles(&self) -> usize {
        ((200.0 * self.max_particles_scale) as usize).max(10)
    }

    /// Portrait max_particles per device for this config (base: 32).
    pub fn portrait_max_particles(&self) -> usize {
        ((32.0 * self.max_particles_scale) as usize).max(4)
    }

    /// Apply user-provided TOML overrides on top of profile defaults.
    pub fn merge(mut self, overrides: AnimConfigOverrides) -> Self {
        if let Some(v) = overrides.sensitivity {
            self.sensitivity = v;
        }
        if let Some(v) = overrides.max_particles_scale {
            self.max_particles_scale = v;
        }
        self
    }
}

/// Partial overrides from `~/.config/tt-toplike/config.toml`.
/// All fields optional — missing = keep profile default.
#[derive(Debug, Default, Deserialize)]
pub struct AnimConfigOverrides {
    pub sensitivity: Option<f32>,
    pub max_particles_scale: Option<f32>,
}

/// Load config overrides from `~/.config/tt-toplike/config.toml`.
/// Returns `Default` silently if the file is absent or unreadable.
pub fn load_config_overrides() -> AnimConfigOverrides {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("tt-toplike").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        assert!((cfg.sensitivity - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_paranoid_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        assert!((cfg.sensitivity - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_anomalies_only_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::AnomaliesOnly);
        assert!((cfg.sensitivity - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn test_max_particles_normal() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        // Normal: 600 castle, 200 flow
        assert_eq!(cfg.castle_max_particles(), 600);
        assert_eq!(cfg.flow_max_particles(), 200);
    }

    #[test]
    fn test_max_particles_paranoid() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        // 3× scale → 1800 castle, 600 flow
        assert_eq!(cfg.castle_max_particles(), 1800);
        assert_eq!(cfg.flow_max_particles(), 600);
    }

    #[test]
    fn test_toml_override_sensitivity() {
        let toml = r#"sensitivity = 2.5"#;
        let overrides: AnimConfigOverrides = toml::from_str(toml).unwrap();
        let base = AnimConfig::from_profile(AnimationProfile::Normal);
        let merged = base.merge(overrides);
        assert!((merged.sensitivity - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_portrait_max_particles_normal() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        // 32 * 1.0 = 32
        assert_eq!(cfg.portrait_max_particles(), 32);
    }

    #[test]
    fn test_portrait_max_particles_paranoid() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        // 32 * 3.0 = 96
        assert_eq!(cfg.portrait_max_particles(), 96);
    }

    #[test]
    fn test_toml_override_max_particles_scale() {
        let toml = r#"max_particles_scale = 1.5"#;
        let overrides: AnimConfigOverrides = toml::from_str(toml).unwrap();
        let base = AnimConfig::from_profile(AnimationProfile::Normal);
        let merged = base.merge(overrides);
        // 600 * 1.5 = 900
        assert_eq!(merged.castle_max_particles(), 900);
        assert!((merged.max_particles_scale - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_anomalies_only_castle_particles() {
        let cfg = AnimConfig::from_profile(AnimationProfile::AnomaliesOnly);
        // 600 * 0.5 = 300
        assert_eq!(cfg.castle_max_particles(), 300);
    }

    #[test]
    fn test_hyper_flow_particles() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Hyper);
        // 200 * 2.0 = 400
        assert_eq!(cfg.flow_max_particles(), 400);
    }

    #[test]
    fn test_relaxed_portrait_particles() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Relaxed);
        // 32 * 0.75 = 24
        assert_eq!(cfg.portrait_max_particles(), 24);
    }
}
