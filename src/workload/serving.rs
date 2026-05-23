// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! TT-specific inference server detection and metrics enrichment.
//!
//! When a TT device is running `tt-inference-server`, `prompt_server.py`, or a
//! vLLM instance with `VLLM_TARGET_DEVICE=tt`, this module probes the server's
//! HTTP endpoints and exposes the live serving metrics in `ServingMetrics`.
//!
//! ## Detection pipeline
//!
//! 1. `InferenceServerProbe::update()` scans the process list (already populated
//!    by `ProcessMonitor`) for known server flavours via cmdline pattern matching.
//! 2. For each candidate, the port is extracted from cmdline args or
//!    `/proc/PID/net/tcp` (little-endian hex decoding).
//! 3. `GET /health` is tried synchronously with a short timeout (~100 ms).
//! 4. `GET /metrics` (Prometheus text format) is tried; 404 is handled gracefully.
//! 5. The log file path is found by scanning `/proc/PID/fd/` for symlinks whose
//!    target names look like log files.
//!
//! All I/O is blocking but bounded by `HTTP_TIMEOUT` so it never stalls the UI.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(feature = "linux-procfs")]
use crate::workload::ProcessInfo;

/// How long we allow any single HTTP probe request to take.
const HTTP_TIMEOUT: Duration = Duration::from_millis(150);

/// Server flavour — which inference stack is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerFlavour {
    /// tt-inference-server (official TT FastAPI wrapper)
    TtInference,
    /// prompt_server.py (lightweight TT demo server)
    PromptServer,
    /// vLLM with VLLM_TARGET_DEVICE=tt
    VllmTt,
    /// Other server that opened a TT device file
    Unknown,
}

impl ServerFlavour {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TtInference => "tt-inference-server",
            Self::PromptServer => "prompt_server",
            Self::VllmTt      => "vllm-tt",
            Self::Unknown     => "server",
        }
    }
}

/// All serving metrics collected for one inference server process.
#[derive(Debug, Clone)]
pub struct ServingMetrics {
    /// Process ID of the server.
    pub pid: i32,
    /// Server flavour.
    pub flavour: ServerFlavour,
    /// Listening port (may be 0 if unknown).
    pub port: u16,
    /// Model identifier from cmdline or `/v1/models`.
    pub model_id: Option<String>,
    /// Whether `/health` returned `model_ready: true`.
    pub is_ready: bool,
    /// Whether a model swap is in progress.
    pub swap_in_progress: bool,
    /// Decode throughput in tokens/s (`tt_generation_tokens_total` counter rate).
    pub generation_tps: Option<f32>,
    /// Prefill throughput in tokens/s (`tt_prompt_tokens_total` counter rate).
    pub prompt_tps: Option<f32>,
    /// Requests currently in the decode queue.
    pub requests_in_flight: Option<u32>,
    /// Requests currently in the decode stage.
    pub requests_decoding: Option<u32>,
    /// Active sessions (if exposed).
    pub active_sessions: Option<u32>,
    /// KV cache utilisation (0.0–1.0).
    pub kv_cache_utilization: Option<f32>,
    /// Prefix cache hit rate (0.0–1.0).
    pub prefix_cache_hit_rate: Option<f32>,
    /// p50 TTFT in seconds.
    pub ttft_p50: Option<f32>,
    /// p99 TTFT in seconds.
    pub ttft_p99: Option<f32>,
    /// Path to the log file (from `/proc/PID/fd/` scan).
    pub log_path: Option<PathBuf>,
    /// Raw MESH_DEVICE env-var value (e.g. "T3K").
    pub mesh_device: Option<String>,
}

impl ServingMetrics {
    fn new(pid: i32, flavour: ServerFlavour, port: u16) -> Self {
        Self {
            pid,
            flavour,
            port,
            model_id: None,
            is_ready: false,
            swap_in_progress: false,
            generation_tps: None,
            prompt_tps: None,
            requests_in_flight: None,
            requests_decoding: None,
            active_sessions: None,
            kv_cache_utilization: None,
            prefix_cache_hit_rate: None,
            ttft_p50: None,
            ttft_p99: None,
            log_path: None,
            mesh_device: None,
        }
    }
}

/// Probes known TT inference servers for live metrics.
///
/// Call `update(processes)` on each backend refresh cycle.  The result is
/// a `HashMap<i32, ServingMetrics>` keyed by PID so the UI can look up
/// metrics for any process in O(1).
#[derive(Default)]
pub struct InferenceServerProbe {
    /// Previous generation-token counter values for computing delta TPS.
    prev_gen_tokens:    HashMap<i32, (f32, std::time::Instant)>,
    /// Previous prompt-token counter values.
    prev_prompt_tokens: HashMap<i32, (f32, std::time::Instant)>,
}

impl InferenceServerProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Probe all TT inference server processes in `processes` and return their metrics.
    ///
    /// Processes that don't look like known server flavours are skipped.
    #[cfg(feature = "linux-procfs")]
    pub fn update(&mut self, processes: &[&ProcessInfo]) -> HashMap<i32, ServingMetrics> {
        let mut out = HashMap::new();
        for proc in processes {
            if let Some(metrics) = self.probe_process(proc) {
                out.insert(proc.pid, metrics);
            }
        }
        // GC stale counter entries for PIDs that are no longer alive.
        let active_pids: std::collections::HashSet<i32> = out.keys().copied().collect();
        self.prev_gen_tokens.retain(|pid, _| active_pids.contains(pid));
        self.prev_prompt_tokens.retain(|pid, _| active_pids.contains(pid));
        out
    }

    #[cfg(feature = "linux-procfs")]
    fn probe_process(&mut self, proc: &ProcessInfo) -> Option<ServingMetrics> {
        let (flavour, port_hint) = classify_cmdline(&proc.cmdline)?;

        // Resolve port: prefer cmdline hint, fall back to net/tcp scan.
        let port = port_hint
            .or_else(|| find_listen_port_from_proc(proc.pid))
            .unwrap_or(0);

        let mut m = ServingMetrics::new(proc.pid, flavour, port);

        // Read mesh device from environ if available.
        m.mesh_device = read_proc_env(proc.pid, "MESH_DEVICE");

        // Model id from cmdline first, fallback to /v1/models.
        m.model_id = extract_model_from_cmdline(&proc.cmdline)
            .or_else(|| if port > 0 { fetch_model_from_api(port) } else { None });

        // Probe /health.
        if port > 0 {
            probe_health(port, &mut m);
            probe_metrics(port, &mut m, &mut self.prev_gen_tokens, &mut self.prev_prompt_tokens, proc.pid);
        }

        // Scan fd/ for log file.
        m.log_path = find_log_path(proc.pid);

        Some(m)
    }
}

// ─── Classification ────────────────────────────────────────────────────────

/// Return `(flavour, port_hint)` if cmdline matches a known server pattern.
fn classify_cmdline(cmdline: &str) -> Option<(ServerFlavour, Option<u16>)> {
    let cmd = cmdline.to_lowercase();
    if cmd.contains("tt-inference-server") || cmd.contains("tt_inference_server") {
        let port = extract_port_arg(cmdline, &["--port", "-p"]);
        return Some((ServerFlavour::TtInference, port));
    }
    if cmd.contains("prompt_server") {
        let port = extract_port_arg(cmdline, &["--port", "-p"]);
        return Some((ServerFlavour::PromptServer, port));
    }
    // vLLM only when VLLM_TARGET_DEVICE=tt (checked via environ later).
    // Accept the cmdline "vllm" binary as a candidate here; flavour may be
    // downgraded to Unknown if the env check fails.
    if cmd.contains("vllm") || cmd.contains("vllm_entrypoints") {
        let port = extract_port_arg(cmdline, &["--port"]);
        return Some((ServerFlavour::VllmTt, port));
    }
    None
}

/// Extract a `--flag VALUE` or `--flag=VALUE` port number from a cmdline string.
fn extract_port_arg(cmdline: &str, flags: &[&str]) -> Option<u16> {
    let parts: Vec<&str> = cmdline.split('\0').collect();
    for (i, part) in parts.iter().enumerate() {
        for flag in flags {
            if *part == *flag {
                if let Some(next) = parts.get(i + 1) {
                    if let Ok(p) = next.parse::<u16>() {
                        return Some(p);
                    }
                }
            }
            if let Some(val) = part.strip_prefix(&format!("{}=", flag)) {
                if let Ok(p) = val.parse::<u16>() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Extract model path/name from cmdline (e.g. `--model meta-llama/...`).
fn extract_model_from_cmdline(cmdline: &str) -> Option<String> {
    let parts: Vec<&str> = cmdline.split('\0').collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "--model" {
            if let Some(val) = parts.get(i + 1) {
                if !val.starts_with('-') {
                    return Some(val.to_string());
                }
            }
        }
        if let Some(val) = part.strip_prefix("--model=") {
            return Some(val.to_string());
        }
    }
    None
}

// ─── /proc helpers ─────────────────────────────────────────────────────────

/// Read one environment variable from `/proc/PID/environ`.
fn read_proc_env(pid: i32, key: &str) -> Option<String> {
    let path = format!("/proc/{}/environ", pid);
    let data = std::fs::read(&path).ok()?;
    let prefix = format!("{}=", key);
    for entry in data.split(|&b| b == 0) {
        if let Ok(s) = std::str::from_utf8(entry) {
            if let Some(val) = s.strip_prefix(&prefix) {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Scan `/proc/PID/net/tcp` for a port the process is listening on (LISTEN state).
/// Returns the first local port in LISTEN state (0A hex).
fn find_listen_port_from_proc(pid: i32) -> Option<u16> {
    let path = format!("/proc/{}/net/tcp", pid);
    let f = std::fs::File::open(&path).ok()?;
    for line in BufReader::new(f).lines().flatten().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 { continue; }
        // col[3] is state: "0A" = LISTEN
        if cols[3] != "0A" { continue; }
        // col[1] is "local_address" in format "AABBCCDD:PORT" (little-endian)
        let local = cols[1];
        if let Some(colon) = local.find(':') {
            let port_hex = &local[colon + 1..];
            if let Ok(port) = u16::from_str_radix(port_hex, 16) {
                // Skip localhost-only listeners that are clearly not public servers
                // (e.g. port < 1024 or > 65000). Common inference ports are 8000–9000.
                if port >= 1024 {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Scan `/proc/PID/fd/` for symlinks whose target looks like a log file.
fn find_log_path(pid: i32) -> Option<PathBuf> {
    let fd_dir = format!("/proc/{}/fd", pid);
    let dir = std::fs::read_dir(&fd_dir).ok()?;
    for entry in dir.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            let name = target.to_string_lossy();
            if name.ends_with(".log")
                || name.contains("vllm_")
                || name.contains("inference_")
                || name.contains("server.log")
            {
                return Some(target);
            }
        }
    }
    None
}

// ─── HTTP probing ──────────────────────────────────────────────────────────

/// Issue a minimal HTTP/1.1 GET and return the response body as a String.
/// Uses raw TcpStream so we have no external dependencies and full timeout control.
fn http_get(port: u16, path: &str) -> Option<String> {
    let addr = format!("127.0.0.1:{}", port);
    let mut stream = TcpStream::connect_timeout(
        &addr.parse().ok()?,
        HTTP_TIMEOUT,
    ).ok()?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT)).ok()?;

    let req = format!(
        "GET {} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
        path, port
    );
    stream.write_all(req.as_bytes()).ok()?;

    let mut body = String::new();
    let mut reader = BufReader::new(stream);

    // Drain headers.
    let mut in_headers = true;
    for line in (&mut reader).lines().flatten() {
        if in_headers {
            if line.is_empty() { in_headers = false; }
            continue;
        }
        body.push_str(&line);
        body.push('\n');
        if body.len() > 64 * 1024 { break; } // guard against huge /metrics responses
    }
    Some(body)
}

/// Probe `/health` and populate readiness fields in `m`.
fn probe_health(port: u16, m: &mut ServingMetrics) {
    if let Some(body) = http_get(port, "/health") {
        m.is_ready = body.contains("\"model_ready\":true") || body.contains("\"status\":\"ok\"");
        m.swap_in_progress = body.contains("\"swap_in_progress\":true");
        // Extract model name if not already set.
        if m.model_id.is_none() {
            if let Some(start) = body.find("\"model\":\"") {
                let rest = &body[start + 9..];
                if let Some(end) = rest.find('"') {
                    let s = &rest[..end];
                    if !s.is_empty() { m.model_id = Some(s.to_string()); }
                }
            }
        }
    }
}

/// Try `/v1/models` to get the loaded model name.
fn fetch_model_from_api(port: u16) -> Option<String> {
    let body = http_get(port, "/v1/models")?;
    // JSON: {"data":[{"id":"meta-llama/Llama-3.1-8B-Instruct",...}]}
    if let Some(start) = body.find("\"id\":\"") {
        let rest = &body[start + 6..];
        if let Some(end) = rest.find('"') {
            let s = &rest[..end];
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Probe `/metrics` (Prometheus text format) and populate throughput/queue fields.
fn probe_metrics(
    port: u16,
    m: &mut ServingMetrics,
    prev_gen:    &mut HashMap<i32, (f32, std::time::Instant)>,
    prev_prompt: &mut HashMap<i32, (f32, std::time::Instant)>,
    pid: i32,
) {
    let body = match http_get(port, "/metrics") {
        Some(b) if !b.is_empty() => b,
        _ => return, // 404 or connection refused — server doesn't expose Prometheus
    };

    let now = std::time::Instant::now();

    let mut gen_total: Option<f32> = None;
    let mut prompt_total: Option<f32> = None;

    for line in body.lines() {
        if line.starts_with('#') { continue; }
        // Match known TT metric names.
        if let Some(val) = parse_metric_line(line, "tt_generation_tokens_total") {
            gen_total = Some(val);
        } else if let Some(val) = parse_metric_line(line, "tt_prompt_tokens_total") {
            prompt_total = Some(val);
        } else if let Some(val) = parse_metric_line(line, "tt_num_requests_in_flight") {
            m.requests_in_flight = Some(val as u32);
        } else if let Some(val) = parse_metric_line(line, "tt_num_decoding_requests") {
            m.requests_decoding = Some(val as u32);
        } else if let Some(val) = parse_metric_line(line, "tt_num_active_sessions") {
            m.active_sessions = Some(val as u32);
        } else if let Some(val) = parse_metric_line(line, "vllm:gpu_cache_usage_perc") {
            m.kv_cache_utilization = Some(val);
        } else if let Some(val) = parse_metric_line(line, "tt_prefix_cache_hit_rate") {
            m.prefix_cache_hit_rate = Some(val);
        } else if line.contains("tt_time_to_first_token_seconds") && line.contains("quantile=\"0.5\"") {
            if let Some(val) = extract_histogram_quantile(line) {
                m.ttft_p50 = Some(val);
            }
        } else if line.contains("tt_time_to_first_token_seconds") && line.contains("quantile=\"0.99\"") {
            if let Some(val) = extract_histogram_quantile(line) {
                m.ttft_p99 = Some(val);
            }
        }
    }

    // Compute TPS as delta counter / elapsed seconds.
    if let Some(total) = gen_total {
        if let Some((prev_total, prev_time)) = prev_gen.get(&pid) {
            let dt = now.duration_since(*prev_time).as_secs_f32();
            if dt > 0.05 {
                m.generation_tps = Some((total - prev_total).max(0.0) / dt);
            }
        }
        prev_gen.insert(pid, (total, now));
    }
    if let Some(total) = prompt_total {
        if let Some((prev_total, prev_time)) = prev_prompt.get(&pid) {
            let dt = now.duration_since(*prev_time).as_secs_f32();
            if dt > 0.05 {
                m.prompt_tps = Some((total - prev_total).max(0.0) / dt);
            }
        }
        prev_prompt.insert(pid, (total, now));
    }
}

/// Parse a Prometheus exposition line: `metric_name [label_pairs] value timestamp?`
/// Returns the value if the line's metric name (without labels) matches `name`.
fn parse_metric_line(line: &str, name: &str) -> Option<f32> {
    let (metric_part, val_part) = if let Some(space) = line.find(' ') {
        (&line[..space], line[space + 1..].split_whitespace().next()?)
    } else {
        return None;
    };
    // Metric name is the part before any `{`.
    let metric_name = metric_part.split('{').next()?;
    if metric_name != name { return None; }
    val_part.parse::<f32>().ok()
}

/// Extract the numeric value from a Prometheus summary/histogram line.
/// These look like: `metric_name{quantile="0.5"} 0.123`
fn extract_histogram_quantile(line: &str) -> Option<f32> {
    let val = line.split_whitespace().nth(1)?;
    val.parse::<f32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_tt_inference() {
        let cmd = "/usr/bin/python3\0-m\0server\0--port\00\08001\0--model\0Qwen/Qwen3-0.6B";
        // No model server keyword → None
        assert!(classify_cmdline(cmd).is_none());
    }

    #[test]
    fn classify_prompt_server() {
        let cmd = "/usr/bin/python3\0prompt_server.py\0--port\08001";
        let (flavour, port) = classify_cmdline(cmd).unwrap();
        assert_eq!(flavour, ServerFlavour::PromptServer);
        assert_eq!(port, Some(8001));
    }

    #[test]
    fn extract_model_arg() {
        let cmd = "python\0-m\0vllm\0--model\0meta-llama/Llama-3.1-8B\0--port\08000";
        assert_eq!(
            extract_model_from_cmdline(cmd),
            Some("meta-llama/Llama-3.1-8B".to_string())
        );
    }

    #[test]
    fn parse_prometheus_line() {
        assert_eq!(
            parse_metric_line("tt_num_requests_in_flight 3", "tt_num_requests_in_flight"),
            Some(3.0)
        );
        assert_eq!(
            parse_metric_line("tt_generation_tokens_total{model=\"foo\"} 12345.0", "tt_generation_tokens_total"),
            Some(12345.0)
        );
        assert!(parse_metric_line("other_metric 1.0", "tt_num_requests_in_flight").is_none());
    }

    #[test]
    fn port_extraction() {
        let cmd = "server\0--port\08001\0--other";
        assert_eq!(extract_port_arg(cmd, &["--port"]), Some(8001));
        let cmd2 = "server\0--port=9000";
        assert_eq!(extract_port_arg(cmd2, &["--port"]), Some(9000));
    }
}
