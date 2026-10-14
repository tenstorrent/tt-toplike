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

/// Human-readable label from a model id: strip any HuggingFace org prefix,
/// e.g. `"Qwen/Qwen3-32B"` → `"Qwen3-32B"`. Left unchanged when there's no `/`.
pub fn model_basename(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model).trim()
}

/// The label to show for a model. `served_name` is vLLM's
/// `--served-model-name` when given; `model` is the `--model` / `MODEL` value,
/// which can be a bare id (`Qwen/Qwen3-32B`) or a weights path.
///
/// Order: the served name, else for a weights *path* the nearest segment that
/// says what the model is (skipping directories like `model-dir`, `final`,
/// `checkpoint-1200`, `snapshots` and revision hashes), else the last segment,
/// which is what [`model_basename`] always gave. HuggingFace-style
/// `org--name` and `models--org--name` directory names lose their prefix.
pub fn friendly_model_label(model: &str, served_name: Option<&str>) -> String {
    if let Some(name) = served_name.map(str::trim).filter(|n| !n.is_empty()) {
        return model_basename(name).to_string();
    }
    let model = model.trim();
    let is_path = model.starts_with('/') || model.starts_with("./") || model.starts_with('~');
    if !is_path {
        return model_basename(model).to_string();
    }
    model
        .rsplit('/')
        .filter(|seg| !seg.is_empty() && !is_generic_path_segment(seg))
        .map(strip_hf_dir_prefix)
        .next()
        .unwrap_or_else(|| model_basename(model).to_string())
}

/// A directory name that holds weights or marks a stage but does not name the
/// model: `model-dir`, `final`, `checkpoint-1200`, `snapshots`, a bare number
/// or a revision hash.
fn is_generic_path_segment(seg: &str) -> bool {
    let s = seg.to_lowercase();
    const NAMES: [&str; 17] = [
        "model-dir",
        "model_dir",
        "modeldir",
        "model",
        "models",
        "weights",
        "checkpoint",
        "checkpoints",
        "final",
        "best",
        "latest",
        "hf",
        "huggingface",
        "snapshots",
        "stage",
        "stages",
        "ckpt",
    ];
    if NAMES.contains(&s.as_str()) {
        return true;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // A revision hash (git sha or similar).
    if s.len() >= 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // checkpoint-1200, step_300, stage-2, epoch-3, global_step100.
    ["checkpoint", "step", "global_step", "stage", "epoch"]
        .iter()
        .any(|p| {
            s.strip_prefix(p)
                .map(|rest| {
                    let rest = rest.trim_start_matches(['-', '_']);
                    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
                })
                .unwrap_or(false)
        })
}

/// `models--org--name` and `org--name` (HuggingFace cache and many run
/// directories) to `name`. Anything without `--` is returned as is.
fn strip_hf_dir_prefix(seg: &str) -> String {
    let seg = seg.strip_prefix("models--").unwrap_or(seg);
    seg.rsplit("--").next().unwrap_or(seg).to_string()
}

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
    fn model_basename_strips_org_prefix() {
        assert_eq!(model_basename("Qwen/Qwen3-32B"), "Qwen3-32B");
        assert_eq!(model_basename("meta-llama/Llama-3.1-8B"), "Llama-3.1-8B");
        assert_eq!(model_basename("FLUX.1-schnell"), "FLUX.1-schnell");
    }

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

    // ---- friendly_model_label --------------------------------------------

    /// The launch a user reported: a fine-tune whose weights sit in a generic
    /// `model-dir` two levels below the name that says what it is.
    #[test]
    fn a_generic_weights_dir_is_labelled_by_the_run_it_belongs_to() {
        assert_eq!(
            friendly_model_label(
                "/home/ttuser/orchard-runs/thakicloud--rag-gate-27b/stages/2/model-dir",
                None
            ),
            "rag-gate-27b"
        );
    }

    #[test]
    fn huggingface_cache_and_checkpoint_dirs_are_labelled_by_the_model() {
        let sha = "9a8b7c6d5e4f30211a2b3c4d5e6f708192a3b4c5";
        assert_eq!(
            friendly_model_label(
                &format!("/home/u/.cache/huggingface/hub/models--Qwen--Qwen3-32B/snapshots/{sha}"),
                None
            ),
            "Qwen3-32B"
        );
        assert_eq!(
            friendly_model_label("/data/runs/acme--chat-7b/final", None),
            "chat-7b"
        );
        assert_eq!(
            friendly_model_label("/data/runs/acme--chat-7b/checkpoint-1200", None),
            "chat-7b"
        );
    }

    #[test]
    fn plain_ids_and_informative_dirs_are_unchanged() {
        assert_eq!(friendly_model_label("Qwen/Qwen3-32B", None), "Qwen3-32B");
        assert_eq!(friendly_model_label("Z-Image-Turbo", None), "Z-Image-Turbo");
        assert_eq!(
            friendly_model_label("/models/llama-3.1-8b-instruct", None),
            "llama-3.1-8b-instruct"
        );
    }

    #[test]
    fn a_path_with_nothing_informative_falls_back_to_its_last_segment() {
        assert_eq!(friendly_model_label("/model-dir", None), "model-dir");
        assert_eq!(
            friendly_model_label("/stages/2/model-dir", None),
            "model-dir"
        );
    }

    #[test]
    fn the_served_model_name_wins_over_any_path() {
        assert_eq!(
            friendly_model_label("/x/model-dir", Some("my-llm")),
            "my-llm"
        );
        // An org prefix in a served name is stripped, like any other id.
        assert_eq!(
            friendly_model_label("/x/model-dir", Some("acme/my-llm")),
            "my-llm"
        );
        // An empty served name is no name at all.
        assert_eq!(
            friendly_model_label("/r/acme--chat-7b/model-dir", Some("  ")),
            "chat-7b"
        );
    }
}
