// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Focused parser for a vLLM Prometheus `/metrics` scrape, plus per-tick
//! `ServingStats` (rates from counter deltas). Drives the Feeding behavior of
//! the unified serving snake. Tolerant: any non-vLLM text yields `None`.

/// Raw cumulative + gauge values from one `/metrics` scrape.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VllmCounters {
    pub generation_tokens_total: u64,
    pub prompt_tokens_total: u64,
    pub requests_succeeded_total: u64, // stop + length
    pub requests_errored_total: u64,   // error + abort
    pub requests_running: u32,
    pub requests_waiting: u32,
    pub kv_cache_usage: f32, // 0..1
    pub ttft_sum: f64,
    pub ttft_count: u64,
}

/// The numeric value at the end of a Prometheus sample line (after the last
/// space). vLLM formats integers as floats (`826.0`), so parse as f64.
fn line_value(line: &str) -> Option<f64> {
    line.rsplit(' ').next()?.trim().parse::<f64>().ok()
}

/// True if `line`'s metric name (before any `{labels}` or space) equals `name`.
fn is_metric(line: &str, name: &str) -> bool {
    let head = line.split(['{', ' ']).next().unwrap_or("");
    head == name
}

/// Parse the vLLM counters we render. `None` if the text carries no `vllm:`
/// metric lines at all (e.g. a non-vLLM server).
pub fn parse_vllm_metrics(text: &str) -> Option<VllmCounters> {
    let mut c = VllmCounters::default();
    let mut saw_vllm = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with("vllm:") {
            continue;
        }
        saw_vllm = true;
        let Some(v) = line_value(line) else { continue };
        if is_metric(line, "vllm:generation_tokens_total") {
            c.generation_tokens_total = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:prompt_tokens_total") {
            c.prompt_tokens_total = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:num_requests_running") {
            c.requests_running = v.max(0.0) as u32;
        } else if is_metric(line, "vllm:num_requests_waiting") {
            c.requests_waiting = v.max(0.0) as u32;
        } else if is_metric(line, "vllm:kv_cache_usage_perc") {
            c.kv_cache_usage = (v as f32).clamp(0.0, 1.0);
        } else if is_metric(line, "vllm:time_to_first_token_seconds_sum") {
            c.ttft_sum = v.max(0.0);
        } else if is_metric(line, "vllm:time_to_first_token_seconds_count") {
            c.ttft_count = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:request_success_total") {
            // Sum the labelled variants by finished_reason.
            let n = v.max(0.0) as u64;
            if line.contains("finished_reason=\"error\"")
                || line.contains("finished_reason=\"abort\"")
            {
                c.requests_errored_total += n;
            } else if line.contains("finished_reason=") {
                c.requests_succeeded_total += n; // stop, length, others
            }
        }
    }
    saw_vllm.then_some(c)
}

/// Display-ready serving stats folded from the previous tick's counters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServingStats {
    pub generation_tps: f32,
    pub prompt_tps: f32,
    pub completed_delta: u32,
    pub errored_delta: u32,
    pub requests_running: u32,
    pub requests_waiting: u32,
    pub kv_cache_usage: f32,
    pub ttft_avg_s: f32,
    /// Carried so the next tick can compute deltas.
    pub counters: VllmCounters,
}

impl ServingStats {
    /// Fold `cur` against the previous tick's counters over `cadence_secs`.
    /// A counter reset (cur < prev, e.g. server restart) clamps that
    /// rate/delta to 0. Without `prev`, rates/deltas are 0 but gauges hold.
    pub fn fold(
        prev: Option<&VllmCounters>,
        cur: &VllmCounters,
        cadence_secs: u32,
    ) -> ServingStats {
        let secs = cadence_secs.max(1) as f32;
        let rate = |c: u64, p: u64| -> f32 { c.saturating_sub(p) as f32 / secs };
        let delta = |c: u64, p: u64| -> u32 { c.saturating_sub(p) as u32 };
        let (gen_tps, prompt_tps, completed, errored) = match prev {
            Some(p) => (
                rate(cur.generation_tokens_total, p.generation_tokens_total),
                rate(cur.prompt_tokens_total, p.prompt_tokens_total),
                delta(cur.requests_succeeded_total, p.requests_succeeded_total),
                delta(cur.requests_errored_total, p.requests_errored_total),
            ),
            None => (0.0, 0.0, 0, 0),
        };
        let ttft_avg_s = if cur.ttft_count > 0 {
            (cur.ttft_sum / cur.ttft_count as f64) as f32
        } else {
            0.0
        };
        ServingStats {
            generation_tps: gen_tps,
            prompt_tps,
            completed_delta: completed,
            errored_delta: errored,
            requests_running: cur.requests_running,
            requests_waiting: cur.requests_waiting,
            kv_cache_usage: cur.kv_cache_usage,
            ttft_avg_s,
            counters: *cur,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a real vLLM /metrics scrape (model_name normalized to "M").
    const SAMPLE: &str = "\
# HELP vllm:num_requests_running Number of requests in model execution batches.
vllm:num_requests_running{engine=\"0\",model_name=\"M\"} 0.0
vllm:num_requests_waiting{engine=\"0\",model_name=\"M\"} 0.0
vllm:kv_cache_usage_perc{engine=\"0\",model_name=\"M\"} 0.0
vllm:prompt_tokens_total{engine=\"0\",model_name=\"M\"} 343.0
vllm:generation_tokens_total{engine=\"0\",model_name=\"M\"} 826.0
vllm:request_success_total{engine=\"0\",finished_reason=\"stop\",model_name=\"M\"} 3.0
vllm:request_success_total{engine=\"0\",finished_reason=\"length\",model_name=\"M\"} 1.0
vllm:request_success_total{engine=\"0\",finished_reason=\"abort\",model_name=\"M\"} 0.0
vllm:request_success_total{engine=\"0\",finished_reason=\"error\",model_name=\"M\"} 2.0
vllm:time_to_first_token_seconds_count{engine=\"0\",model_name=\"M\"} 4.0
vllm:time_to_first_token_seconds_sum{engine=\"0\",model_name=\"M\"} 0.88
";

    #[test]
    fn parses_the_vllm_counters() {
        let c = parse_vllm_metrics(SAMPLE).expect("has vllm metrics");
        assert_eq!(c.generation_tokens_total, 826);
        assert_eq!(c.prompt_tokens_total, 343);
        assert_eq!(c.requests_succeeded_total, 4); // stop(3)+length(1)
        assert_eq!(c.requests_errored_total, 2); // error(2)+abort(0)
        assert_eq!(c.requests_running, 0);
        assert_eq!(c.requests_waiting, 0);
        assert_eq!(c.ttft_count, 4);
        assert!((c.ttft_sum - 0.88).abs() < 1e-6);
    }

    #[test]
    fn none_when_no_vllm_metrics() {
        assert!(parse_vllm_metrics("# nothing here\nother_metric 5\n").is_none());
        assert!(parse_vllm_metrics("").is_none());
    }

    #[test]
    fn fold_computes_rates_from_deltas() {
        let prev = VllmCounters {
            generation_tokens_total: 826,
            prompt_tokens_total: 343,
            requests_succeeded_total: 4,
            requests_errored_total: 2,
            ..Default::default()
        };
        let cur = VllmCounters {
            generation_tokens_total: 826 + 4210,
            prompt_tokens_total: 343 + 100,
            requests_succeeded_total: 6,
            requests_errored_total: 2,
            requests_running: 1,
            requests_waiting: 2,
            kv_cache_usage: 0.04,
            ttft_sum: 1.10,
            ttft_count: 6,
        };
        let s = ServingStats::fold(Some(&prev), &cur, 5);
        assert!(
            (s.generation_tps - 842.0).abs() < 0.5,
            "4210 gen tokens / 5s ≈ 842"
        );
        assert_eq!(s.completed_delta, 2); // 6-4
        assert_eq!(s.errored_delta, 0);
        assert_eq!(s.requests_running, 1);
        assert_eq!(s.requests_waiting, 2);
        assert!((s.ttft_avg_s - (1.10 / 6.0) as f32).abs() < 1e-4);
    }

    #[test]
    fn fold_clamps_counter_reset_to_zero() {
        // Server restarted: cur < prev → no negative rates/deltas.
        let prev = VllmCounters {
            generation_tokens_total: 9000,
            requests_succeeded_total: 50,
            ..Default::default()
        };
        let cur = VllmCounters {
            generation_tokens_total: 10,
            requests_succeeded_total: 1,
            ..Default::default()
        };
        let s = ServingStats::fold(Some(&prev), &cur, 5);
        assert_eq!(s.generation_tps, 0.0);
        assert_eq!(s.completed_delta, 0);
    }

    #[test]
    fn fold_without_prev_is_zero_rates_but_keeps_gauges() {
        let cur = VllmCounters {
            requests_running: 3,
            kv_cache_usage: 0.5,
            ttft_sum: 2.0,
            ttft_count: 4,
            ..Default::default()
        };
        let s = ServingStats::fold(None, &cur, 5);
        assert_eq!(s.generation_tps, 0.0);
        assert_eq!(s.requests_running, 3);
        assert!((s.kv_cache_usage - 0.5).abs() < 1e-6);
        assert!((s.ttft_avg_s - 0.5).abs() < 1e-4); // 2.0/4
    }
}
