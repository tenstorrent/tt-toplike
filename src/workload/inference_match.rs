// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Heuristic classifier: does a process look like an ML/inference runtime?
//!
//! Pure, cross-platform name/cmdline matching against a curated, easily-extended
//! list. Returns a stable runtime label for display, or `None`. This is a "might
//! be doing inference" hint, not proof — it does not attribute device usage.

/// (label, needles) — if any needle appears (case-insensitive) in the process
/// name or full cmdline, the process is tagged with `label`. Ordered most- to
/// least-specific so the first hit wins.
const RUNTIMES: &[(&str, &[&str])] = &[
    ("ollama", &["ollama"]),
    ("vllm", &["vllm"]),
    (
        "llama.cpp",
        &[
            "llama-server",
            "llama-cli",
            "llama.cpp",
            "llama_cpp",
            "llamacpp",
        ],
    ),
    ("mlx", &["mlx_lm", "mlx-lm", "mlx.server"]),
    ("ComfyUI", &["comfyui"]),
    (
        "stable-diffusion",
        &["stable-diffusion", "sdwebui", "automatic1111"],
    ),
    ("koboldcpp", &["koboldcpp", "koboldai"]),
    (
        "whisper",
        &["whisper.cpp", "whisper-server", "faster-whisper"],
    ),
    ("lm-studio", &["lm studio", "lmstudio", "lms server"]),
    (
        "text-generation",
        &["text-generation-inference", "text-generation-webui"],
    ),
    (
        "torch",
        &["torchrun", "torch.distributed", "pytorch", " torch "],
    ),
    (
        "transformers",
        &["transformers", "huggingface", "accelerate launch"],
    ),
    ("diffusers", &["diffusers"]),
    (
        "jax",
        &["jax.numpy", "import jax", " jax ", "/jax", "flax.linen"],
    ),
    ("tensorflow", &["tensorflow", "tf.keras"]),
    ("triton", &["tritonserver", "triton_python"]),
];

/// Classify a process by name + cmdline. Returns the matched runtime label, or
/// `None` if nothing matches.
pub fn inference_match(name: &str, cmdline: &str) -> Option<&'static str> {
    if name.is_empty() && cmdline.is_empty() {
        return None;
    }
    // Pad both ends with spaces so the space-bounded needles (" torch ", " jax ")
    // also match a bare process name like `torch` or `jax` — without the padding
    // the label wouldn't tag a process whose name is exactly that label. The
    // bounded needles stay tight (e.g. " torch " won't match `torchlight`), which
    // is why we pad rather than add bare substrings that would over-match.
    let hay = format!(" {} {} ", name, cmdline).to_lowercase();
    for (label, needles) in RUNTIMES {
        for needle in *needles {
            if hay.contains(&needle.to_lowercase()) {
                return Some(label);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_runtimes() {
        assert_eq!(inference_match("ollama", "ollama serve"), Some("ollama"));
        assert_eq!(
            inference_match("python3", "python3 -m vllm.entrypoints.openai.api_server"),
            Some("vllm")
        );
        assert_eq!(
            inference_match("python", "python train.py --use torch"),
            Some("torch")
        );
        assert_eq!(
            inference_match("llama-server", "/usr/bin/llama-server -m m.gguf"),
            Some("llama.cpp")
        );
        assert_eq!(
            inference_match("mlx_lm.server", "mlx_lm.server --model x"),
            Some("mlx")
        );
    }

    #[test]
    fn ignores_non_inference() {
        assert_eq!(
            inference_match("WindowServer", "/System/.../WindowServer"),
            None
        );
        assert_eq!(inference_match("node", "node server.js"), None);
        assert_eq!(inference_match("", ""), None);
    }

    #[test]
    fn name_or_cmdline_either_matches() {
        // matches on cmdline even when the process name is a generic interpreter
        assert_eq!(
            inference_match("python3.11", "comfyui/main.py"),
            Some("ComfyUI")
        );
    }
}
