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
    pub queue_time_sum: f64,
    pub queue_time_count: u64,
    pub prefill_time_sum: f64,
    pub prefill_time_count: u64,
    pub decode_time_sum: f64,
    pub decode_time_count: u64,
    pub tpot_sum: f64,
    pub tpot_count: u64,
    pub prefix_queries_total: u64,
    pub prefix_hits_total: u64,
    pub preemptions_total: u64,
}

/// The numeric value of a Prometheus sample line — the field right after the
/// `metric{labels}` head, NOT the last field. The exposition format allows an
/// optional trailing timestamp (`metric{..} 5.0 1739000000000`); taking the
/// last field would parse that timestamp as the value. Splitting after the
/// closing `}` (when labels are present) also tolerates spaces inside quoted
/// label values. vLLM formats integers as floats (`826.0`), so parse as f64.
fn line_value(line: &str) -> Option<f64> {
    let value_field = match line.rfind('}') {
        Some(i) => line[i + 1..].split_whitespace().next(),
        None => line.split_whitespace().nth(1),
    };
    value_field?.parse::<f64>().ok()
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
        } else if is_metric(line, "vllm:kv_cache_usage_perc")
            || is_metric(line, "vllm:gpu_cache_usage_perc")
        {
            // vLLM renamed gpu_cache_usage_perc → kv_cache_usage_perc; TT
            // builds in the field have shipped both. Accept either.
            c.kv_cache_usage = (v as f32).clamp(0.0, 1.0);
        } else if is_metric(line, "vllm:time_to_first_token_seconds_sum") {
            c.ttft_sum = v.max(0.0);
        } else if is_metric(line, "vllm:time_to_first_token_seconds_count") {
            c.ttft_count = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:request_queue_time_seconds_sum") {
            c.queue_time_sum = v.max(0.0);
        } else if is_metric(line, "vllm:request_queue_time_seconds_count") {
            c.queue_time_count = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:request_prefill_time_seconds_sum") {
            c.prefill_time_sum = v.max(0.0);
        } else if is_metric(line, "vllm:request_prefill_time_seconds_count") {
            c.prefill_time_count = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:request_decode_time_seconds_sum") {
            c.decode_time_sum = v.max(0.0);
        } else if is_metric(line, "vllm:request_decode_time_seconds_count") {
            c.decode_time_count = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:time_per_output_token_seconds_sum") {
            c.tpot_sum = v.max(0.0);
        } else if is_metric(line, "vllm:time_per_output_token_seconds_count") {
            c.tpot_count = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:prefix_cache_queries_total") {
            c.prefix_queries_total = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:prefix_cache_hits_total") {
            c.prefix_hits_total = v.max(0.0) as u64;
        } else if is_metric(line, "vllm:num_preemptions_total") {
            c.preemptions_total = v.max(0.0) as u64;
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

// ── Media / diffusion server (tt-media-inference-server) ──────────────────────
//
// Diffusion/video models (SkyReels, SDXL, z-image) run under
// tt-media-inference-server, which exposes a *different* Prometheus namespace —
// `tt_media_server_*` — on the same `/metrics` endpoint. vLLM's token counters
// don't exist there (and tokens/sec is meaningless for image/video generation),
// so those servers need their own parser + stats. See `parse_media_metrics`.

/// Raw cumulative counters + a gauge + histogram sums/counts from one
/// media-server `/metrics` scrape, summed across label sets (model_type,
/// device_id) so the display sees one fleet figure per family.
///
/// Metric names verified against a live `tt-media-inference-server` 0.15.0
/// SkyReels scrape — the earlier doc-derived names (`model_inference_total`,
/// `pre_processing`, `device_warmup`) don't exist on that build; the real
/// signals are `requests_base_total`, `jobs_in_progress`, the
/// `requests_base_duration_seconds_total` histogram, and `post_processing`.
/// The absent families are still parsed best-effort (other model runners may
/// emit them) and simply stay 0 here.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MediaCounters {
    /// `tt_media_server_requests_base_total` — completed generations (the
    /// duration histogram observes on completion, so this tracks finished work).
    pub requests_total: u64,
    /// `tt_media_server_requests_base_total{status="error"|"failed"}`, if the
    /// server labels failures (this build does not, so it stays 0).
    pub errored_total: u64,
    /// `tt_media_server_jobs_in_progress` — in-flight generations right now.
    /// The live "is it working on my load" gauge; drives the snake body length.
    pub jobs_in_progress: u32,
    /// `tt_media_server_requests_base_duration_seconds_total_{sum,count}` — the
    /// end-to-end per-generation wall time (queue + compute + post).
    pub duration_sum: f64,
    pub duration_count: u64,
    /// `tt_media_server_post_processing_duration_seconds_{sum,count}`.
    pub post_sum: f64,
    pub post_count: u64,
    // ── Best-effort extras: not emitted by 0.15.0 SkyReels, but other runners
    //    (or future versions) may. Stay 0 when absent; shown only if non-zero.
    /// `tt_media_server_pre_processing_duration_seconds_{sum,count}`.
    pub pre_sum: f64,
    pub pre_count: u64,
    /// `tt_media_server_model_inference_duration_seconds_{sum,count}`.
    pub inference_sum: f64,
    pub inference_count: u64,
    /// `tt_media_server_device_warmup_duration_seconds_{sum,count}`.
    pub warmup_sum: f64,
    pub warmup_count: u64,
}

/// Parse the media-server counters we render. `None` if the text carries no
/// `tt_media_server_` metric lines at all (e.g. a vLLM or non-media server),
/// which folds to `media: None` downstream — mirroring `parse_vllm_metrics`.
pub fn parse_media_metrics(text: &str) -> Option<MediaCounters> {
    let mut c = MediaCounters::default();
    let mut saw_media = false;
    // The live server (prometheus multiprocess mode) emits each series *twice* —
    // byte-identical `name{labels}` lines. Since we sum across label sets, those
    // duplicates would double every value (jobs_in_progress 2→4, etc.), so we
    // count each distinct series (name + full label block) at most once. Genuine
    // multi-device/model_type series have different label blocks → still summed.
    //
    // The key is the exact `name{labels}` byte substring, so this assumes the
    // exporter emits a given series with a *stable label order* (true for the
    // prometheus client's multiprocess output). If a series were re-emitted with
    // its labels reordered, the two forms would key differently and double-count
    // — not a concern for this exporter, but noted since fleet totals rest on it.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with("tt_media_server_") {
            continue;
        }
        saw_media = true;
        let Some(v) = line_value(line) else { continue };
        // Series identity = everything up to the value: `name{labels}` when
        // labelled, else the bare metric name. A repeat of the same series in
        // one scrape is a duplicate to skip, not another label set to add.
        let series_key = match line.rfind('}') {
            Some(i) => &line[..=i],
            None => line.split_whitespace().next().unwrap_or(line),
        };
        if !seen.insert(series_key) {
            continue;
        }
        // Counters are summed across their (distinct) label sets (device_id,
        // model_type), so multi-device servers report a fleet total.
        // `is_metric` compares the exact metric name, so histogram `_bucket` /
        // `_created` companion lines never collide with the `_sum`/`_count` /
        // base-counter names below.
        if is_metric(line, "tt_media_server_requests_base_total") {
            let n = v.max(0.0) as u64;
            if line.contains("status=\"error\"")
                || line.contains("status=\"failed\"")
                || line.contains("status=\"failure\"")
            {
                c.errored_total += n;
            } else {
                c.requests_total += n;
            }
        } else if is_metric(line, "tt_media_server_jobs_in_progress") {
            // A gauge (not cumulative); summed across model_type for a fleet total.
            c.jobs_in_progress += v.max(0.0) as u32;
        } else if is_metric(
            line,
            "tt_media_server_requests_base_duration_seconds_total_sum",
        ) {
            c.duration_sum += v.max(0.0);
        } else if is_metric(
            line,
            "tt_media_server_requests_base_duration_seconds_total_count",
        ) {
            c.duration_count += v.max(0.0) as u64;
        } else if is_metric(line, "tt_media_server_post_processing_duration_seconds_sum") {
            c.post_sum += v.max(0.0);
        } else if is_metric(
            line,
            "tt_media_server_post_processing_duration_seconds_count",
        ) {
            c.post_count += v.max(0.0) as u64;
        } else if is_metric(line, "tt_media_server_pre_processing_duration_seconds_sum") {
            c.pre_sum += v.max(0.0);
        } else if is_metric(
            line,
            "tt_media_server_pre_processing_duration_seconds_count",
        ) {
            c.pre_count += v.max(0.0) as u64;
        } else if is_metric(line, "tt_media_server_model_inference_duration_seconds_sum") {
            c.inference_sum += v.max(0.0);
        } else if is_metric(
            line,
            "tt_media_server_model_inference_duration_seconds_count",
        ) {
            c.inference_count += v.max(0.0) as u64;
        } else if is_metric(line, "tt_media_server_device_warmup_duration_seconds_sum") {
            c.warmup_sum += v.max(0.0);
        } else if is_metric(line, "tt_media_server_device_warmup_duration_seconds_count") {
            c.warmup_count += v.max(0.0) as u64;
        }
    }
    saw_media.then_some(c)
}

/// Display-ready media/diffusion stats, folded from the previous tick's
/// counters (rates from deltas). The counterpart to [`ServingStats`] for
/// servers where "tokens/sec" is meaningless — throughput is generations,
/// the live-work signal is `jobs_in_progress`, and timing is the end-to-end
/// per-generation duration plus whichever pipeline stages the server exposes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaStats {
    /// Completed generations per minute — the headline throughput.
    pub generations_per_min: f32,
    /// In-flight generations right now (the `jobs_in_progress` gauge).
    pub jobs_in_progress: u32,
    /// Generations completed / errored this tick (for pulse + fray effects).
    pub completed_delta: u32,
    pub errored_delta: u32,
    /// Windowed mean end-to-end per-generation wall time (seconds), same fold
    /// rule as [`ServingStats`]'s latencies: this tick's completed work when any
    /// completed, else the lifetime mean, else 0.
    pub duration_avg_s: f32,
    /// Windowed mean per-stage durations. `post_avg_s` is always present on this
    /// build; the others (`pre`/`inference`/`warmup`) are 0 unless the runner
    /// emits them, and callers show a stage row only when its value is non-zero.
    pub post_avg_s: f32,
    pub pre_avg_s: f32,
    pub inference_avg_s: f32,
    pub warmup_avg_s: f32,
    /// Carried so the next tick can compute deltas.
    pub counters: MediaCounters,
}

impl MediaStats {
    /// Fold `cur` against the previous tick's counters over `cadence_secs`.
    /// A counter reset (cur < prev, e.g. server restart) clamps rates/deltas to
    /// 0. Without `prev`, rates/deltas are 0 but the gauges/averages still hold.
    pub fn fold(
        prev: Option<&MediaCounters>,
        cur: &MediaCounters,
        cadence_secs: u32,
    ) -> MediaStats {
        let secs = cadence_secs.max(1) as f32;
        let delta = |c: u64, p: u64| -> u32 { c.saturating_sub(p) as u32 };
        let (gen_per_min, completed, errored) = match prev {
            Some(p) => {
                let per_sec = cur.requests_total.saturating_sub(p.requests_total) as f32 / secs;
                (
                    per_sec * 60.0,
                    delta(cur.requests_total, p.requests_total),
                    delta(cur.errored_total, p.errored_total),
                )
            }
            None => (0.0, 0, 0),
        };
        // Windowed mean over just this tick's completed work (cur − prev), so a
        // slow generation moves the number instead of being drowned by the
        // lifetime mean; falls back to lifetime mean when nothing completed this
        // window, so the display holds its last steady value rather than dropping
        // to 0. Identical rule to `ServingStats::fold`'s `wavg`.
        let wavg = |cur_sum: f64, cur_count: u64, prev_sum: f64, prev_count: u64| -> f32 {
            let d_count = cur_count.saturating_sub(prev_count);
            if d_count > 0 {
                ((cur_sum - prev_sum).max(0.0) / d_count as f64) as f32
            } else if cur_count > 0 {
                (cur_sum / cur_count as f64) as f32
            } else {
                0.0
            }
        };
        let avg = |cur_sum: f64,
                   cur_count: u64,
                   ps: fn(&MediaCounters) -> f64,
                   pc: fn(&MediaCounters) -> u64|
         -> f32 {
            wavg(cur_sum, cur_count, prev.map_or(0.0, ps), prev.map_or(0, pc))
        };
        MediaStats {
            generations_per_min: gen_per_min.max(0.0),
            jobs_in_progress: cur.jobs_in_progress,
            completed_delta: completed,
            errored_delta: errored,
            duration_avg_s: avg(
                cur.duration_sum,
                cur.duration_count,
                |c| c.duration_sum,
                |c| c.duration_count,
            ),
            post_avg_s: avg(
                cur.post_sum,
                cur.post_count,
                |c| c.post_sum,
                |c| c.post_count,
            ),
            pre_avg_s: avg(cur.pre_sum, cur.pre_count, |c| c.pre_sum, |c| c.pre_count),
            inference_avg_s: avg(
                cur.inference_sum,
                cur.inference_count,
                |c| c.inference_sum,
                |c| c.inference_count,
            ),
            warmup_avg_s: avg(
                cur.warmup_sum,
                cur.warmup_count,
                |c| c.warmup_sum,
                |c| c.warmup_count,
            ),
            counters: *cur,
        }
    }
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
    pub queue_avg_s: f32,
    pub prefill_avg_s: f32,
    pub decode_avg_s: f32,
    pub tpot_avg_s: f32,
    pub prefix_hit_rate: f32,
    pub preemptions_delta: u32,
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
        // Windowed latency average: mean over just this tick's completed
        // requests (cur − prev), so a slow request actually moves the number
        // instead of being drowned by the lifetime mean of every request since
        // server start. Falls back to the lifetime mean when nothing completed
        // this window (idle) or on the first tick, so the display holds the last
        // steady value rather than dropping to 0.
        let wavg = |cur_sum: f64, cur_count: u64, prev_sum: f64, prev_count: u64| -> f32 {
            let d_count = cur_count.saturating_sub(prev_count);
            if d_count > 0 {
                ((cur_sum - prev_sum).max(0.0) / d_count as f64) as f32
            } else if cur_count > 0 {
                (cur_sum / cur_count as f64) as f32
            } else {
                0.0
            }
        };
        let avg = |cur_sum: f64,
                   cur_count: u64,
                   ps: fn(&VllmCounters) -> f64,
                   pc: fn(&VllmCounters) -> u64|
         -> f32 {
            wavg(cur_sum, cur_count, prev.map_or(0.0, ps), prev.map_or(0, pc))
        };
        let ttft_avg_s = avg(
            cur.ttft_sum,
            cur.ttft_count,
            |c| c.ttft_sum,
            |c| c.ttft_count,
        );
        let prefix_hit_rate = if cur.prefix_queries_total > 0 {
            (cur.prefix_hits_total as f64 / cur.prefix_queries_total as f64) as f32
        } else {
            0.0
        };
        let preemptions_delta = match prev {
            Some(p) => cur.preemptions_total.saturating_sub(p.preemptions_total) as u32,
            None => 0,
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
            queue_avg_s: avg(
                cur.queue_time_sum,
                cur.queue_time_count,
                |c| c.queue_time_sum,
                |c| c.queue_time_count,
            ),
            prefill_avg_s: avg(
                cur.prefill_time_sum,
                cur.prefill_time_count,
                |c| c.prefill_time_sum,
                |c| c.prefill_time_count,
            ),
            decode_avg_s: avg(
                cur.decode_time_sum,
                cur.decode_time_count,
                |c| c.decode_time_sum,
                |c| c.decode_time_count,
            ),
            tpot_avg_s: avg(
                cur.tpot_sum,
                cur.tpot_count,
                |c| c.tpot_sum,
                |c| c.tpot_count,
            ),
            prefix_hit_rate,
            preemptions_delta,
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
    fn line_value_takes_value_not_trailing_timestamp() {
        // An optional Prometheus timestamp after the value must be ignored, not
        // parsed as the value.
        assert_eq!(line_value("vllm:x{a=\"b\"} 5.0 1739000000000"), Some(5.0));
        // Labels present, no timestamp.
        assert_eq!(line_value("vllm:x{a=\"b\"} 42.0"), Some(42.0));
        // No labels at all.
        assert_eq!(line_value("some_metric 7"), Some(7.0));
        // A quoted label value containing a space doesn't derail the split.
        assert_eq!(line_value("m{who=\"a b\"} 9.0"), Some(9.0));
    }

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
    fn kv_cache_accepts_both_metric_names() {
        // vLLM renamed gpu_cache_usage_perc → kv_cache_usage_perc; TT builds
        // in the field have shipped both. Accept either.
        let old_name = "vllm:gpu_cache_usage_perc{engine=\"0\",model_name=\"M\"} 0.42\n";
        let c = parse_vllm_metrics(old_name).expect("has vllm metrics");
        assert_eq!(c.kv_cache_usage, 0.42);

        let new_name = "vllm:kv_cache_usage_perc{engine=\"0\",model_name=\"M\"} 0.37\n";
        let c = parse_vllm_metrics(new_name).expect("has vllm metrics");
        assert_eq!(c.kv_cache_usage, 0.37);
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
            ..Default::default()
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
    fn latency_avg_is_windowed_not_lifetime() {
        // Lifetime mean is a calm 0.1s over 10 requests; this window has ONE new
        // request that took 2.0s. A windowed average must surface the 2.0s spike,
        // not drown it in the lifetime mean (which would read ~0.27s).
        let prev = VllmCounters {
            ttft_sum: 1.0,
            ttft_count: 10,
            ..Default::default()
        };
        let cur = VllmCounters {
            ttft_sum: 3.0, // +2.0s for the one new request
            ttft_count: 11,
            ..Default::default()
        };
        let s = ServingStats::fold(Some(&prev), &cur, 5);
        assert!(
            (s.ttft_avg_s - 2.0).abs() < 1e-4,
            "windowed TTFT should reflect this tick's one 2.0s request, got {}",
            s.ttft_avg_s
        );
        // Idle tick (no new requests): hold the last steady lifetime mean, not 0.
        let idle = ServingStats::fold(Some(&cur), &cur, 5);
        assert!(
            (idle.ttft_avg_s - (3.0 / 11.0) as f32).abs() < 1e-4,
            "idle window should fall back to lifetime mean, got {}",
            idle.ttft_avg_s
        );
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

    const SAMPLE2: &str = "\
vllm:generation_tokens_total{m=\"M\"} 100.0
vllm:request_queue_time_seconds_sum{m=\"M\"} 2.0
vllm:request_queue_time_seconds_count{m=\"M\"} 4.0
vllm:request_prefill_time_seconds_sum{m=\"M\"} 4.0
vllm:request_prefill_time_seconds_count{m=\"M\"} 4.0
vllm:request_decode_time_seconds_sum{m=\"M\"} 12.0
vllm:request_decode_time_seconds_count{m=\"M\"} 4.0
vllm:time_per_output_token_seconds_sum{m=\"M\"} 0.8
vllm:time_per_output_token_seconds_count{m=\"M\"} 40.0
vllm:prefix_cache_queries_total{m=\"M\"} 200.0
vllm:prefix_cache_hits_total{m=\"M\"} 150.0
vllm:num_preemptions_total{m=\"M\"} 3.0
";
    #[test]
    fn parses_stage_prefix_and_preemption_metrics() {
        let c = parse_vllm_metrics(SAMPLE2).expect("vllm present");
        assert_eq!(c.queue_time_count, 4);
        assert!((c.queue_time_sum - 2.0).abs() < 1e-6);
        assert_eq!(c.decode_time_count, 4);
        assert!((c.decode_time_sum - 12.0).abs() < 1e-6);
        assert_eq!(c.prefix_queries_total, 200);
        assert_eq!(c.prefix_hits_total, 150);
        assert_eq!(c.preemptions_total, 3);
    }
    #[test]
    fn fold_derives_stage_averages_and_rates() {
        let cur = VllmCounters {
            queue_time_sum: 2.0,
            queue_time_count: 4,
            prefill_time_sum: 4.0,
            prefill_time_count: 4,
            decode_time_sum: 12.0,
            decode_time_count: 4,
            tpot_sum: 0.8,
            tpot_count: 40,
            prefix_queries_total: 200,
            prefix_hits_total: 150,
            preemptions_total: 5,
            ..Default::default()
        };
        let prev = VllmCounters {
            preemptions_total: 3,
            ..Default::default()
        };
        let s = ServingStats::fold(Some(&prev), &cur, 5);
        assert!((s.queue_avg_s - 0.5).abs() < 1e-4); // 2/4
        assert!((s.prefill_avg_s - 1.0).abs() < 1e-4); // 4/4
        assert!((s.decode_avg_s - 3.0).abs() < 1e-4); // 12/4
        assert!((s.tpot_avg_s - 0.02).abs() < 1e-4); // 0.8/40
        assert!((s.prefix_hit_rate - 0.75).abs() < 1e-4); // 150/200
        assert_eq!(s.preemptions_delta, 2); // 5-3
    }

    // Trimmed verbatim from a live tt-media-inference-server 0.15.0 SkyReels
    // scrape (one request completed, two still in progress). Includes the
    // histogram `_bucket`/`_created` companion lines the parser must ignore AND
    // the byte-identical duplicate series the server emits (prometheus
    // multiprocess mode) — each real series appears twice and must count once.
    const MEDIA_SAMPLE: &str = "\
# HELP tt_media_server_requests_base_total Total base requests.
# TYPE tt_media_server_requests_base_total counter
tt_media_server_requests_base_total{model_type=\"tt-skyreels-v2-i2v\"} 1.0
tt_media_server_requests_base_created{model_type=\"tt-skyreels-v2-i2v\"} 1.7838079607483413e+09
tt_media_server_requests_base_duration_seconds_total_bucket{le=\"5.0\",model_type=\"tt-skyreels-v2-i2v\"} 0.0
tt_media_server_requests_base_duration_seconds_total_bucket{le=\"+Inf\",model_type=\"tt-skyreels-v2-i2v\"} 1.0
tt_media_server_requests_base_duration_seconds_total_count{model_type=\"tt-skyreels-v2-i2v\"} 1.0
tt_media_server_requests_base_duration_seconds_total_sum{model_type=\"tt-skyreels-v2-i2v\"} 612.0247645378113
tt_media_server_post_processing_duration_seconds_bucket{le=\"0.5\",model_type=\"tt-skyreels-v2-i2v\",post_processing_enabled=\"True\"} 1.0
tt_media_server_post_processing_duration_seconds_count{model_type=\"tt-skyreels-v2-i2v\",post_processing_enabled=\"True\"} 1.0
tt_media_server_post_processing_duration_seconds_sum{model_type=\"tt-skyreels-v2-i2v\",post_processing_enabled=\"True\"} 0.3206624984741211
tt_media_server_jobs_in_progress{model_type=\"tt-skyreels-v2-i2v\"} 2.0
tt_media_server_info_info{model_runner=\"tt-skyreels-v2-i2v\",version=\"1.0.0\"} 1.0
tt_media_server_post_processing_duration_seconds_sum{model_type=\"tt-skyreels-v2-i2v\",post_processing_enabled=\"True\"} 0.3206624984741211
tt_media_server_requests_base_duration_seconds_total_sum{model_type=\"tt-skyreels-v2-i2v\"} 612.0247645378113
tt_media_server_requests_base_duration_seconds_total_count{model_type=\"tt-skyreels-v2-i2v\"} 1.0
tt_media_server_requests_base_total{model_type=\"tt-skyreels-v2-i2v\"} 1.0
tt_media_server_jobs_in_progress{model_type=\"tt-skyreels-v2-i2v\"} 2.0
";

    #[test]
    fn parses_real_media_scrape_deduping_duplicate_series() {
        let c = parse_media_metrics(MEDIA_SAMPLE).expect("has media metrics");
        // Each series appears twice in the fixture; values must NOT double.
        assert_eq!(c.requests_total, 1, "one completed generation (not 2)");
        assert_eq!(c.jobs_in_progress, 2, "two in-flight, not 4 (dedup)");
        assert_eq!(c.errored_total, 0);
        // End-to-end per-request duration histogram (not the _bucket lines,
        // not doubled by the duplicate _sum line).
        assert!((c.duration_sum - 612.0247645378113).abs() < 1e-6);
        assert_eq!(c.duration_count, 1);
        assert!((c.post_sum - 0.3206624984741211).abs() < 1e-6);
        assert_eq!(c.post_count, 1);
        // Families this build doesn't emit stay at zero.
        assert_eq!(c.inference_count, 0);
        assert_eq!(c.warmup_count, 0);
    }

    #[test]
    fn media_sums_distinct_label_sets_but_not_duplicates() {
        // Two genuinely different device_id series → summed; a byte-identical
        // repeat of one of them → counted once.
        let text = "\
tt_media_server_jobs_in_progress{model_type=\"m\",device_id=\"2\"} 1.0
tt_media_server_jobs_in_progress{model_type=\"m\",device_id=\"3\"} 2.0
tt_media_server_jobs_in_progress{model_type=\"m\",device_id=\"2\"} 1.0
";
        let c = parse_media_metrics(text).unwrap();
        assert_eq!(
            c.jobs_in_progress, 3,
            "1 (dev2) + 2 (dev3); dup dev2 ignored"
        );
    }

    #[test]
    fn media_ignores_bucket_and_created_companion_lines() {
        // The histogram `_bucket` and `_created` lines must NOT be mistaken for
        // the `_sum`/`_count`/base-counter families (exact-name matching).
        let c = parse_media_metrics(MEDIA_SAMPLE).unwrap();
        // If `_bucket` leaked into duration_count it would be >1; if `_created`
        // (a ~1.78e9 unix timestamp) leaked into any sum it would be enormous.
        assert_eq!(c.duration_count, 1);
        assert!(c.duration_sum < 1_000.0, "no _created timestamp leaked in");
        assert!(
            c.requests_total < 10,
            "no _created leaked into requests_total"
        );
    }

    #[test]
    fn media_and_vllm_parsers_are_mutually_exclusive() {
        // A vLLM scrape has no media lines, and vice-versa — so a server is only
        // ever one or the other, never both.
        assert!(parse_media_metrics(SAMPLE).is_none());
        assert!(parse_vllm_metrics(MEDIA_SAMPLE).is_none());
        assert!(parse_media_metrics("").is_none());
    }

    #[test]
    fn media_fold_computes_rate_inflight_and_duration() {
        let prev = MediaCounters {
            requests_total: 43,
            ..Default::default()
        };
        // +2 completed over 5s = 0.4/s = 24/min; 3 now in flight.
        let cur = MediaCounters {
            requests_total: 45,
            jobs_in_progress: 3,
            duration_sum: 55.6, // +55.6s over the 2 that completed → 27.8s each
            duration_count: 2,
            post_sum: 0.64,
            post_count: 2,
            ..Default::default()
        };
        let s = MediaStats::fold(Some(&prev), &cur, 5);
        assert!(
            (s.generations_per_min - 24.0).abs() < 0.5,
            "2 gens / 5s = 24/min, got {}",
            s.generations_per_min
        );
        assert_eq!(s.jobs_in_progress, 3, "in-flight carried straight through");
        assert_eq!(s.completed_delta, 2);
        assert!(
            (s.duration_avg_s - 27.8).abs() < 0.1,
            "55.6s / 2 completed = 27.8s, got {}",
            s.duration_avg_s
        );
        assert!((s.post_avg_s - 0.32).abs() < 0.01);
    }

    #[test]
    fn media_fold_clamps_counter_reset_and_holds_gauges_without_prev() {
        // Restart: cur < prev → no negative rate.
        let prev = MediaCounters {
            requests_total: 900,
            ..Default::default()
        };
        let cur = MediaCounters {
            requests_total: 3,
            jobs_in_progress: 1,
            duration_sum: 90.0,
            duration_count: 3,
            ..Default::default()
        };
        let reset = MediaStats::fold(Some(&prev), &cur, 5);
        assert_eq!(reset.generations_per_min, 0.0);
        assert_eq!(reset.completed_delta, 0);
        assert_eq!(reset.jobs_in_progress, 1, "gauge still surfaces on reset");
        // No prev: rate 0, but the lifetime-mean duration still holds (90/3=30s).
        let first = MediaStats::fold(None, &cur, 5);
        assert_eq!(first.generations_per_min, 0.0);
        assert!((first.duration_avg_s - 30.0).abs() < 1e-4);
    }
}
