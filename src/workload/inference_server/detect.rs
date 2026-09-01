// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Recognize a TT inference server from a process name + cmdline and extract a
//! structured record. Pure and cross-platform-compilable; only invoked on Linux.

use crate::workload::inference_server::probe::parse_env_var;

/// Where the server runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Docker {
        container: String,
    },
    /// A bare (non-Docker) process, e.g. a direct `vllm serve`/
    /// `server_example_tt.py` launch — see `parse_direct_vllm`.
    Host {
        pid: i32,
    },
}

/// Prefix of the identity key `service_key` derives for a `Source::Host` —
/// also used by `probe::SystemProbe` to recognize a host-keyed call.
pub(crate) const HOST_KEY_PREFIX: &str = "host-vllm-";

/// Stable identity key for a detected server, used for prev-state lookup,
/// dedup, and the monitor's change-signature. Docker keys by container name
/// (survives the monitor's own restarts, stable across ticks); a bare host
/// process has no such name, so it keys by pid — a restart gets a fresh key
/// and starts from `fresh_state`, which is correct: the old process's
/// kernel/RSS history is not the new process's history.
pub fn service_key(source: &Source) -> String {
    match source {
        Source::Docker { container } => container.clone(),
        Source::Host { pid } => format!("{HOST_KEY_PREFIX}{pid}"),
    }
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
pub(crate) fn is_tt_inference_image(token: &str) -> bool {
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

/// Extract the HOST port from `--publish [ip:][hostPort:]containerPort[/proto]`,
/// e.g. `--publish 0.0.0.0:8000:8001` → 8000. The monitor probes
/// `127.0.0.1:<port>` — the host side of the mapping — so we take the host port,
/// which is the second-from-last `:`-segment when one is given. With only a
/// container port (`--publish 8000`) Docker assigns a random host port we can't
/// read from the cmdline, so we fall back to that segment as a best effort
/// (`parse_inspect`'s HostPort is authoritative when the inspect path resolves it).
fn published_port(toks: &[&str]) -> Option<u16> {
    for (i, t) in toks.iter().enumerate() {
        if *t == "--publish" || *t == "-p" {
            if let Some(spec) = value_after(toks, i) {
                let spec = spec.split('/').next().unwrap_or(spec);
                let segs: Vec<&str> = spec.split(':').collect();
                let host_seg = if segs.len() >= 2 {
                    segs[segs.len() - 2]
                } else {
                    segs[0]
                };
                if let Ok(p) = host_seg.parse::<u16>() {
                    return Some(p);
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

/// Parse `docker inspect <container>` JSON (a one-element array) into an
/// [`InferenceServer`]. This is the path for **detached** containers (`docker
/// run -d`, `docker compose`, systemd) that have no foreground `docker run`
/// host process for [`parse_inference_server`] to see — the monitor enumerates
/// running containers with `docker ps` and inspects each. Returns `None` on
/// malformed JSON or a non-TT image. Pure (no I/O), so it's unit-tested.
pub fn parse_inspect(json: &str) -> Option<InferenceServer> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_array()?.first()?;
    let config = obj.get("Config")?;
    let image = config.get("Image")?.as_str()?.to_string();
    if !is_tt_inference_image(&image) {
        return None;
    }
    let container = obj
        .get("Name")
        .and_then(|n| n.as_str())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Config.Env is ["KEY=VALUE", …]; Config.Cmd is the container's argv.
    let env: Vec<&str> = config
        .get("Env")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let env_val = |key: &str| -> Option<String> {
        env.iter().find_map(|kv| {
            let (k, val) = kv.split_once('=')?;
            (k == key).then(|| val.to_string())
        })
    };
    let cmd: Vec<&str> = config
        .get("Cmd")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();

    // uses_tt_device: any HostConfig.Devices entry mapping /dev/tenstorrent.
    let uses_tt_device = obj
        .get("HostConfig")
        .and_then(|h| h.get("Devices"))
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter().any(|dev| {
                dev.get("PathOnHost")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| p.contains("/dev/tenstorrent"))
            })
        })
        .unwrap_or(false);

    // Published port: HostConfig.PortBindings maps "8002/tcp" → [{HostPort:"8002"}].
    // We probe localhost, so the HostPort is what matters.
    let port = obj
        .get("HostConfig")
        .and_then(|h| h.get("PortBindings"))
        .and_then(|pb| pb.as_object())
        .and_then(|map| {
            map.values().find_map(|binds| {
                binds
                    .as_array()?
                    .first()?
                    .get("HostPort")?
                    .as_str()?
                    .parse::<u16>()
                    .ok()
            })
        });

    Some(InferenceServer {
        source: Source::Docker { container },
        image,
        // Prefer `-e MODEL=`; fall back to the container's `--model` arg (vLLM).
        model: env_val("MODEL").or_else(|| model_arg(&cmd)),
        mesh: env_val("MESH_DEVICE"),
        arch: env_val("ARCH_NAME"),
        device: env_val("DEVICE"),
        port,
        uses_tt_device,
    })
}

/// Recognize a direct (non-Docker) vLLM-on-TT launch from `name` + `cmdline`
/// + the process's own `environ` (`KEY=VALUE`-per-line text — same shape
/// `probe::parse_env_var` already expects), building a `Source::Host { pid }`
/// record. Two real shapes (from `tt-tnt`'s `tt-model serve`, which execs one
/// of these directly): `vllm serve <model> ...`, or a cmdline containing
/// `server_example_tt.py` with a `--model <X>` arg.
///
/// Requires `MESH_DEVICE` or `TT_METAL_HOME` present in `environ` to confirm
/// this is genuinely TT-backed — mirrors `parse_inference_server`'s
/// `uses_tt_device` check, which has no `/dev/tenstorrent` cmdline
/// equivalent to key off for a bare host process. Without this gate, plain
/// upstream vLLM (e.g. running against a GPU on the same box for comparison)
/// would be misclassified as a TT inference server.
pub fn parse_direct_vllm(
    name: &str,
    cmdline: &str,
    environ: &str,
    pid: i32,
) -> Option<InferenceServer> {
    let _ = name; // recognized from cmdline shape alone; kept for API symmetry with parse_inference_server
    let toks: Vec<&str> = cmdline.split_whitespace().collect();

    // Match by `ends_with("vllm")`, not `== "vllm"`: a venv console-script's
    // argv[0] is commonly a full path (e.g.
    // `/home/ttuser/venv-vllm-standalone/bin/vllm`), not the bare token.
    // `vllm serve <model> ...`: positional model argument. If that shape
    // isn't present (e.g. `vllm serve --model X` — flag form instead of
    // positional), fall back to `model_arg`, the same `--model`/`--model=X`
    // helper the `server_example_tt.py` shape below uses — otherwise a
    // flag-form `vllm serve` launch goes entirely undetected.
    let model = toks
        .iter()
        .position(|t| t.ends_with("vllm"))
        .filter(|&i| toks.get(i + 1) == Some(&"serve"))
        .and_then(|i| {
            toks.get(i + 2)
                .filter(|t| !t.starts_with('-'))
                .map(|s| s.to_string())
                .or_else(|| model_arg(&toks))
        })
        .or_else(|| {
            cmdline
                .contains("server_example_tt.py")
                .then(|| model_arg(&toks))
                .flatten()
        })?;

    let mesh = parse_env_var(environ, "MESH_DEVICE");
    let tt_metal_home = parse_env_var(environ, "TT_METAL_HOME");
    if mesh.is_none() && tt_metal_home.is_none() {
        return None;
    }

    // vLLM uses argparse, so both `--port <N>` and `--port=<N>` are valid.
    let port = toks
        .iter()
        .position(|t| *t == "--port")
        .and_then(|i| toks.get(i + 1))
        .and_then(|p| p.parse::<u16>().ok())
        .or_else(|| {
            toks.iter()
                .find_map(|t| t.strip_prefix("--port="))
                .and_then(|p| p.parse::<u16>().ok())
        })
        .unwrap_or(8000);

    Some(InferenceServer {
        source: Source::Host { pid },
        // No container image for a bare process; a stable label is enough —
        // only used for the monitor's display/signature formatting.
        image: "vllm-direct".to_string(),
        model: Some(model),
        mesh,
        arch: None,
        device: None,
        port: Some(port),
        uses_tt_device: true,
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
    fn published_port_takes_host_side_of_mapping() {
        // ip:hostPort:containerPort → the HOST port (middle segment), which is
        // what the monitor probes at 127.0.0.1 — not the container port (8001).
        assert_eq!(
            published_port(&["--publish", "0.0.0.0:8000:8001"]),
            Some(8000)
        );
        // hostPort:containerPort → host port (first segment).
        assert_eq!(published_port(&["-p", "9000:8000"]), Some(9000));
        // container port only → best-effort that segment (Docker picks a random
        // host port we can't read from the cmdline).
        assert_eq!(published_port(&["--publish", "8000"]), Some(8000));
        // /proto suffix is stripped.
        assert_eq!(published_port(&["-p", "0.0.0.0:8000:8001/tcp"]), Some(8000));
        assert_eq!(published_port(&["run", "--rm"]), None);
    }

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

    // A detached vLLM container as `docker inspect` reports it: model is in
    // Config.Cmd (not an env var), device + port in HostConfig.
    const INSPECT_VLLM: &str = r#"[{
        "Name": "/tt-inference-server-2269d4f6",
        "Config": {
            "Image": "ghcr.io/tenstorrent/tt-inference-server/vllm-tt-metal-src-release-ubuntu-22.04-amd64:0.14.0",
            "Env": ["MODEL_WEIGHTS_DIR=/x", "HF_HOME=/y", "ARCH_NAME=blackhole", "PATH=/usr/bin"],
            "Cmd": ["--model", "Qwen/Qwen3-32B", "--tt-device", "p300x2", "--no-auth"]
        },
        "HostConfig": {
            "Devices": [{"PathOnHost": "/dev/tenstorrent", "PathInContainer": "/dev/tenstorrent"}],
            "PortBindings": {"8002/tcp": [{"HostIp": "0.0.0.0", "HostPort": "8002"}]}
        }
    }]"#;

    #[test]
    fn parses_detached_container_from_inspect() {
        let s = parse_inspect(INSPECT_VLLM).expect("detached vLLM container should parse");
        assert_eq!(
            s.source,
            Source::Docker {
                container: "tt-inference-server-2269d4f6".into()
            }
        );
        assert_eq!(s.model.as_deref(), Some("Qwen/Qwen3-32B")); // from Cmd
        assert_eq!(s.arch.as_deref(), Some("blackhole"));
        assert_eq!(s.port, Some(8002)); // from PortBindings HostPort
        assert!(s.uses_tt_device);
        assert!(s.image.contains("tt-inference-server"));
    }

    // A media server inspected: model is an env var, no Cmd model arg.
    const INSPECT_MEDIA: &str = r#"[{
        "Name": "/tt-inference-server-5edd00ce",
        "Config": {
            "Image": "ghcr.io/tenstorrent/tt-media-inference-server:0.17.0-8c48a10",
            "Env": ["MODEL=FLUX.1-schnell", "MESH_DEVICE=P300x2", "NO_AUTH=1"],
            "Cmd": null
        },
        "HostConfig": {
            "Devices": [{"PathOnHost": "/dev/tenstorrent"}],
            "PortBindings": {"8000/tcp": [{"HostPort": "8000"}]}
        }
    }]"#;

    #[test]
    fn parses_detached_media_server_model_from_env() {
        let s = parse_inspect(INSPECT_MEDIA).expect("media container should parse");
        assert_eq!(s.model.as_deref(), Some("FLUX.1-schnell")); // from -e MODEL=
        assert_eq!(s.mesh.as_deref(), Some("P300x2"));
        assert_eq!(s.port, Some(8000));
        assert!(s.uses_tt_device);
    }

    #[test]
    fn inspect_rejects_non_tt_and_malformed() {
        // A random container image is not a TT inference server.
        let other = r#"[{"Name":"/pg","Config":{"Image":"postgres:16","Env":[],"Cmd":null},"HostConfig":{}}]"#;
        assert!(parse_inspect(other).is_none());
        // Malformed / empty JSON → None, never a panic.
        assert!(parse_inspect("").is_none());
        assert!(parse_inspect("not json").is_none());
        assert!(parse_inspect("[]").is_none());
        assert!(parse_inspect("{}").is_none());
    }

    #[test]
    fn service_key_docker_uses_container_name_host_uses_pid() {
        let docker = Source::Docker {
            container: "tt-inference-server-abc123".into(),
        };
        assert_eq!(service_key(&docker), "tt-inference-server-abc123");

        let host = Source::Host { pid: 4242 };
        assert_eq!(service_key(&host), "host-vllm-4242");
    }

    // Real shapes from tt-tnt's docs/serving-with-tt-kernel.md and AUTOFIX.md,
    // trimmed to the parts parse_direct_vllm reads.
    const CMDLINE_VLLM_SERVE: &str = "vllm serve episod/tt-tnt-1024 --max_model_len 512 \
        --max_num_seqs 32 --port 8000 --additional-config {\"tt\":{\"fabric_config\":\"FABRIC_2D_TORUS_XY\"}}";
    const ENVIRON_VLLM_SERVE: &str = "TT_METAL_HOME=/home/ttuser/tt-metal-src-vllm-home\n\
        MESH_DEVICE=P300x2\nHF_MODEL=episod/tt-tnt-1024\nPATH=/usr/bin\n";

    const CMDLINE_EXAMPLE_SCRIPT: &str =
        "python3 server_example_tt.py --model episod/tt-tnt --max_model_len 2048 --max_num_seqs 8";
    const ENVIRON_EXAMPLE_SCRIPT: &str =
        "MESH_DEVICE=P150\nHF_MODEL=episod/tt-tnt\nVLLM_USE_V1=1\n";

    #[test]
    fn parses_vllm_serve_shape() {
        let s = parse_direct_vllm("vllm", CMDLINE_VLLM_SERVE, ENVIRON_VLLM_SERVE, 4242)
            .expect("should detect vllm serve shape");
        assert_eq!(s.source, Source::Host { pid: 4242 });
        assert_eq!(s.model.as_deref(), Some("episod/tt-tnt-1024"));
        assert_eq!(s.mesh.as_deref(), Some("P300x2"));
        assert_eq!(s.port, Some(8000));
        assert!(s.uses_tt_device);
    }

    #[test]
    fn parses_example_script_shape() {
        let s = parse_direct_vllm(
            "python3",
            CMDLINE_EXAMPLE_SCRIPT,
            ENVIRON_EXAMPLE_SCRIPT,
            99,
        )
        .expect("should detect server_example_tt.py shape");
        assert_eq!(s.source, Source::Host { pid: 99 });
        assert_eq!(s.model.as_deref(), Some("episod/tt-tnt"));
        assert_eq!(s.mesh.as_deref(), Some("P150"));
        // no --port in this shape → default
        assert_eq!(s.port, Some(8000));
    }

    #[test]
    fn rejects_vllm_serve_without_tt_evidence() {
        // Same cmdline, but neither MESH_DEVICE nor TT_METAL_HOME set — must not
        // be misclassified as TT-backed (could be plain upstream vLLM on GPU).
        let no_tt_env = "HOME=/root\nPATH=/usr/bin\n";
        assert!(parse_direct_vllm("vllm", CMDLINE_VLLM_SERVE, no_tt_env, 1).is_none());
    }

    #[test]
    fn rejects_unrelated_processes() {
        assert!(parse_direct_vllm("bash", "bash -c ls", ENVIRON_VLLM_SERVE, 1).is_none());
        assert!(parse_direct_vllm("python3", "python3 train.py", ENVIRON_VLLM_SERVE, 1).is_none());
    }

    #[test]
    fn parses_explicit_port_override() {
        let cmd = "vllm serve some/model --port 9001";
        let s = parse_direct_vllm("vllm", cmd, ENVIRON_VLLM_SERVE, 1).unwrap();
        assert_eq!(s.port, Some(9001));
    }

    #[test]
    fn parses_port_equals_form() {
        // argparse accepts `--port=<N>` as well as `--port <N>`.
        let cmd = "vllm serve some/model --port=9001";
        let s = parse_direct_vllm("vllm", cmd, ENVIRON_VLLM_SERVE, 1).unwrap();
        assert_eq!(s.port, Some(9001));
    }

    #[test]
    fn parses_vllm_serve_with_flag_form_model_arg() {
        // `vllm serve --model X` (flag form) instead of `vllm serve X`
        // (positional) must still be recognized.
        let cmd = "vllm serve --model episod/tt-tnt-1024 --port 8000";
        let s = parse_direct_vllm("vllm", cmd, ENVIRON_VLLM_SERVE, 1)
            .expect("flag-form --model must still be detected");
        assert_eq!(s.model.as_deref(), Some("episod/tt-tnt-1024"));
        assert_eq!(s.port, Some(8000));
    }

    #[test]
    fn recognizes_a_full_path_vllm_binary_not_just_the_bare_name() {
        // A venv console-script's argv[0] is commonly a full path
        // (e.g. `/home/ttuser/venv-vllm-standalone/bin/vllm serve ...`), not the
        // bare token "vllm" — real deployments look like this, not like the
        // other tests' bare-name fixtures.
        let cmd = "/home/ttuser/venv-vllm-standalone/bin/vllm serve episod/tt-tnt-1024 --port 8000";
        let s = parse_direct_vllm("vllm", cmd, ENVIRON_VLLM_SERVE, 1)
            .expect("full-path vllm binary must still be recognized");
        assert_eq!(s.model.as_deref(), Some("episod/tt-tnt-1024"));
    }
}
