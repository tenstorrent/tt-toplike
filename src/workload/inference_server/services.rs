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
}

/// Known services. Trail: later read from a shared config so the two tools can't drift.
pub const SERVERS: &[ServiceDef] = &[
    ServiceDef { key: "wan2.2", label: "Wan2.2-T2V-A14B (P300X2)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-wan2.2") },
    ServiceDef { key: "mochi", label: "Mochi-1", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-mochi-1") },
    ServiceDef { key: "flux", label: "FLUX.1-schnell", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-flux.1-schnell") },
    ServiceDef { key: "sdxl", label: "SDXL (cpp_server)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-sdxl-generate") },
    ServiceDef { key: "z-image-turbo", label: "Z-Image-Turbo (P150X4)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-z-image-turbo") },
    ServiceDef { key: "motif", label: "Motif-Image-6B-Preview (P300X2)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-motif-image-6b-preview") },
    ServiceDef { key: "animate", label: "Wan2.2-Animate-14B", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-wan2.2-animate") },
    ServiceDef { key: "skyreels", label: "SkyReels-V2-I2V-14B-540P (Blackhole)", port: 8000, health_path: "/tt-liveness", runner_key: Some("tt-skyreels-v2-i2v") },
    ServiceDef { key: "prompt-server", label: "Prompt Generator (Qwen3-0.6B)", port: 8001, health_path: "/health", runner_key: None },
];

/// Map a detected container's MODEL env (preferred) or image to a service.
/// Case-insensitive contains match on the model against each key's tokens.
pub fn service_for(model: Option<&str>, _image: &str) -> Option<&'static ServiceDef> {
    let m = model?.to_lowercase();
    SERVERS.iter().find(|d| {
        // e.g. model "Z-Image-Turbo" → key "z-image-turbo"
        m.replace(['.', '_'], "-").contains(d.key) || d.key.contains(&m.replace(['.', '_'], "-"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_model_to_service_and_carries_endpoint() {
        let s = service_for(Some("Z-Image-Turbo"), "ghcr.io/tenstorrent/tt-media-inference-server:x").expect("known");
        assert_eq!(s.key, "z-image-turbo");
        assert_eq!(s.port, 8000);
        assert_eq!(s.health_path, "/tt-liveness");
        assert_eq!(s.runner_key, Some("tt-z-image-turbo"));
        // prompt-server uses a different port + path
        let p = SERVERS.iter().find(|d| d.key == "prompt-server").unwrap();
        assert_eq!((p.port, p.health_path), (8001, "/health"));
    }
}
