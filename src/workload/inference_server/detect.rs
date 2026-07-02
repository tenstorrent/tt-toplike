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
        model: env_value(&toks, "MODEL"),
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

    #[test]
    fn ignores_unrelated_processes() {
        assert!(parse_inference_server("uvicorn", "uvicorn --host 0.0.0.0 main:app").is_none());
        assert!(parse_inference_server("bash", "docker ps").is_none());
        assert!(parse_inference_server("node", "node server.js").is_none());
    }
}
