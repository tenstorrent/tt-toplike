// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Recognize a TT inference server from a process name + cmdline and extract a
//! structured record. Pure and cross-platform-compilable; only invoked on Linux.

/// Where the server runs. v1 handles Docker; the `Host` trail (non-container
/// installs) slots in behind this enum without changing consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Docker { container: String },
    // Trail: Host { unit_or_pid: String },
}

/// Identity of a detected inference server, parsed from its launch cmdline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceServer {
    pub source: Source,
    pub image: String,
    pub model: Option<String>,
    pub mesh: Option<String>,
    pub arch: Option<String>,
    pub device: Option<String>,
    pub port: Option<u16>,
    pub uses_tt_device: bool,
}

/// True if `image_or_name` names a TT inference server. Central recognizer so
/// new variants (vLLM-native, Triton, …) are a one-line addition here.
fn is_tt_inference_image(token: &str) -> bool {
    let t = token.to_lowercase();
    t.contains("inference-server")
        || t.contains("tt-media-inference")
        || (t.contains("ghcr.io/tenstorrent/tt-") && t.contains("server"))
}

/// Value following a flag token, e.g. `--name X` → `X`. Returns the next token.
fn value_after<'a>(toks: &[&'a str], i: usize) -> Option<&'a str> {
    toks.get(i + 1).copied()
}

/// Parse `-e KEY=VAL` env pairs (KEY is the token after `-e`/`--env`).
fn env_value(toks: &[&str], key: &str) -> Option<String> {
    for (i, t) in toks.iter().enumerate() {
        if *t == "-e" || *t == "--env" {
            if let Some(kv) = value_after(toks, i) {
                if let Some((k, v)) = kv.split_once('=') {
                    if k == key {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Extract the container port from `--publish [host:]ctr[/proto]`, e.g.
/// `--publish 0.0.0.0:8000:8000` → 8000 (last numeric segment before any `/`).
fn published_port(toks: &[&str]) -> Option<u16> {
    for (i, t) in toks.iter().enumerate() {
        if *t == "--publish" || *t == "-p" {
            if let Some(spec) = value_after(toks, i) {
                let spec = spec.split('/').next().unwrap_or(spec);
                if let Some(seg) = spec.rsplit(':').next() {
                    if let Ok(p) = seg.parse::<u16>() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

/// Extract the model from the container's `--model <X>` or `--model=X` arg.
fn model_arg(toks: &[&str]) -> Option<String> {
    for (i, t) in toks.iter().enumerate() {
        if let Some(v) = t.strip_prefix("--model=") {
            return Some(v.to_string());
        }
        if *t == "--model" {
            return value_after(toks, i).map(|s| s.to_string());
        }
    }
    None
}

/// Recognize a TT inference server from `name` + full `cmdline`, or `None`.
/// v1 only recognizes the `docker run` form.
pub fn parse_inference_server(name: &str, cmdline: &str) -> Option<InferenceServer> {
    let toks: Vec<&str> = cmdline.split_whitespace().collect();
    // Must be a docker run invocation.
    let is_docker_run =
        (name == "docker" || toks.first() == Some(&"docker")) && toks.contains(&"run");
    if !is_docker_run {
        return None;
    }
    // Find the image token (recognizer) and the --name. The --name value is
    // excluded from image recognition: container names like
    // `tt-inference-server-<id>` also match `is_tt_inference_image`'s substring
    // checks, which would otherwise shadow the real image token that follows.
    let name_value_idx = toks.iter().position(|t| *t == "--name").map(|i| i + 1);
    let image = toks
        .iter()
        .enumerate()
        .find(|(i, t)| Some(*i) != name_value_idx && is_tt_inference_image(t))
        .map(|(_, t)| t.to_string())?;
    let container = toks
        .iter()
        .position(|t| *t == "--name")
        .and_then(|i| value_after(&toks, i))
        .unwrap_or("unknown")
        .to_string();

    Some(InferenceServer {
        source: Source::Docker { container },
        image,
        // Prefer `-e MODEL=…`; fall back to the container's `--model <X>` (or
        // `--model=X`) arg, which is how vLLM/LLM deployments pass the model
        // (e.g. Qwen/Qwen3-32B) rather than an env var.
        model: env_value(&toks, "MODEL").or_else(|| model_arg(&toks)),
        mesh: env_value(&toks, "MESH_DEVICE"),
        arch: env_value(&toks, "ARCH_NAME"),
        device: env_value(&toks, "DEVICE"),
        port: published_port(&toks),
        uses_tt_device: cmdline.contains("/dev/tenstorrent"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a real `docker run` on the QuietBox (media inference server).
    const DOCKER_RUN: &str = "docker run --rm --name tt-inference-server-5edd00ce \
        --device /dev/tenstorrent:/dev/tenstorrent --publish 0.0.0.0:8000:8000 \
        -e MODEL=FLUX.1-schnell -e MESH_DEVICE=P300x2 -e ARCH_NAME=blackhole \
        -e DEVICE=p300x2 -e NO_AUTH=1 ghcr.io/tenstorrent/tt-media-inference-server:0.17.0-8c48a10";

    #[test]
    fn parses_tt_inference_docker_run() {
        let s = parse_inference_server("docker", DOCKER_RUN).expect("should detect");
        assert!(matches!(s.source, Source::Docker { .. }));
        assert_eq!(s.model.as_deref(), Some("FLUX.1-schnell"));
        assert_eq!(s.mesh.as_deref(), Some("P300x2"));
        assert_eq!(s.arch.as_deref(), Some("blackhole"));
        assert_eq!(s.device.as_deref(), Some("p300x2"));
        assert_eq!(s.port, Some(8000));
        assert!(s.uses_tt_device);
        assert!(s.image.contains("tt-media-inference-server"));
    }

    // A real vLLM/LLM deployment: the model is a container CLI arg
    // (`--model Qwen/Qwen3-32B`), NOT a `-e MODEL=` env var; the only env vars
    // are `MODEL_WEIGHTS_DIR`/`HF_HOME` (must not be mistaken for MODEL).
    const DOCKER_RUN_VLLM: &str = "docker run --rm --name tt-inference-server-2269d4f6 \
        --device /dev/tenstorrent:/dev/tenstorrent --publish 0.0.0.0:8002:8002 \
        -e MODEL_WEIGHTS_DIR=/x -e HF_HOME=/y \
        ghcr.io/tenstorrent/tt-inference-server/vllm-tt-metal-src-release-ubuntu-22.04-amd64:0.14.0 \
        --model Qwen/Qwen3-32B --tt-device p300x2 --no-auth --service-port 8002";

    #[test]
    fn parses_model_from_cli_arg_when_no_env() {
        let s = parse_inference_server("docker", DOCKER_RUN_VLLM).expect("should detect");
        assert_eq!(s.model.as_deref(), Some("Qwen/Qwen3-32B"));
        assert_eq!(s.port, Some(8002));
        assert!(s.image.contains("tt-inference-server"));
        assert!(s.uses_tt_device);
    }

    #[test]
    fn ignores_unrelated_processes() {
        assert!(parse_inference_server("uvicorn", "uvicorn --host 0.0.0.0 main:app").is_none());
        assert!(parse_inference_server("bash", "docker ps").is_none());
        assert!(parse_inference_server("node", "node server.js").is_none());
    }
}
