// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

/// One managed inference service (mirrors tt-local-generator's SERVERS dict).
#[derive(Debug, Clone, Copy)]
pub struct ServiceDef {
    pub key: &'static str,
    pub label: &'static str,
    pub port: u16,
    pub health_path: &'static str,
    /// `runner_in_use` value reported by /tt-liveness when this model is loaded.
    pub runner_key: Option<&'static str>,
    /// Exact `MODEL` env value this service runs (for matching in `service_for`).
    pub model_match: &'static str,
}

/// Known services. Trail: later read from a shared config so the two tools can't drift.
pub const SERVERS: &[ServiceDef] = &[
    ServiceDef {
        key: "wan2.2",
        label: "Wan2.2-T2V-A14B (P300X2)",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-wan2.2"),
        model_match: "Wan2.2-T2V-A14B-Diffusers",
    },
    ServiceDef {
        key: "mochi",
        label: "Mochi-1",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-mochi-1"),
        model_match: "mochi-1-preview",
    },
    ServiceDef {
        key: "flux",
        label: "FLUX.1-schnell",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-flux.1-schnell"),
        model_match: "FLUX.1-schnell",
    },
    ServiceDef {
        key: "sdxl",
        label: "SDXL (cpp_server)",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-sdxl-generate"),
        model_match: "stable-diffusion-xl-base-1.0",
    },
    ServiceDef {
        key: "z-image-turbo",
        label: "Z-Image-Turbo (P150X4)",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-z-image-turbo"),
        model_match: "Z-Image-Turbo",
    },
    ServiceDef {
        key: "motif",
        label: "Motif-Image-6B-Preview (P300X2)",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-motif-image-6b-preview"),
        model_match: "Motif-Image-6B-Preview",
    },
    ServiceDef {
        key: "animate",
        label: "Wan2.2-Animate-14B",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-wan2.2-animate"),
        model_match: "Wan2.2-Animate-14B-Diffusers",
    },
    ServiceDef {
        key: "skyreels",
        label: "SkyReels-V2-I2V-14B-540P (Blackhole)",
        port: 8000,
        health_path: "/tt-liveness",
        runner_key: Some("tt-skyreels-v2-i2v"),
        model_match: "SkyReels-V2-I2V-14B-540P",
    },
    // Launched via plain `python3`, not `docker run` — container detector never sees it;
    // appears in panel only as a known service in the Down state; model_match unreachable.
    ServiceDef {
        key: "prompt-server",
        label: "Prompt Generator (Qwen3-0.6B)",
        port: 8001,
        health_path: "/health",
        runner_key: None,
        model_match: "Qwen3-0.6B",
    },
];

/// Normalize a model name for comparison: lowercase; `.`, `_`, whitespace → `-`.
fn norm(s: &str) -> String {
    s.trim().to_lowercase().replace(['.', '_', ' '], "-")
}

/// Map a container's `MODEL` env value to its service by exact (normalized) match.
/// Returns `None` for unknown/absent models — never guesses, so an unexpected
/// model name is shown unmatched rather than misattributed.
pub fn service_for(model: Option<&str>) -> Option<&'static ServiceDef> {
    let m = norm(model?);
    SERVERS.iter().find(|d| norm(d.model_match) == m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_each_model_exactly_no_false_positives() {
        assert_eq!(
            service_for(Some("Z-Image-Turbo")).unwrap().key,
            "z-image-turbo"
        );
        assert_eq!(service_for(Some("FLUX.1-schnell")).unwrap().key, "flux");
        assert!(service_for(Some("Influx-Model")).is_none()); // no substring FP
        assert!(service_for(None).is_none());
        // endpoint still carried
        let p = SERVERS.iter().find(|d| d.key == "prompt-server").unwrap();
        assert_eq!((p.port, p.health_path), (8001, "/health"));
        // regression: verify corrected model_match values
        assert_eq!(
            service_for(Some("Wan2.2-T2V-A14B-Diffusers")).unwrap().key,
            "wan2.2"
        );
        assert_eq!(
            service_for(Some("Wan2.2-Animate-14B-Diffusers"))
                .unwrap()
                .key,
            "animate"
        );
        assert_eq!(service_for(Some("mochi-1-preview")).unwrap().key, "mochi");
        assert_eq!(
            service_for(Some("stable-diffusion-xl-base-1.0"))
                .unwrap()
                .key,
            "sdxl"
        );
    }
}
